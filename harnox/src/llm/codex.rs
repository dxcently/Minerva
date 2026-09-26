//! The ChatGPT Codex backend — the subscription's own Responses surface,
//! spoken natively (feature `llm`).
//!
//! `https://chatgpt.com/backend-api/codex/responses` is the private endpoint
//! the Codex CLI itself talks to, authenticated by a ChatGPT OAuth token
//! ([`TokenSource::CodexRefresh`](super::credentials::TokenSource::CodexRefresh))
//! rather than a platform API key. Billing is the subscription's message
//! quota, not tokens. The request shape is OpenAI's Responses API with a
//! Codex flavour: `instructions` instead of a system message, a flat `input`
//! array of items, `store: false` always, and a client masquerade
//! (originator/User-Agent) that the backend expects.
//!
//! What this module does, and only this: builds one `POST
//! {base}/responses` with `stream: true`, maps the SSE events it knows to
//! [`StreamEvent`], and ignores the rest — the same contract
//! [`anthropic`](super::anthropic) holds. Unknown event types are skipped;
//! `response.failed` and `error` fail the stream.
//!
//! Reasoning round-trips through the canonical thinking block's *signature*:
//! summary deltas arrive as `ThinkingDelta`, the item's `encrypted_content`
//! as `ThinkingSignature`, and on replay a thinking block with a signature
//! becomes a `{type:"reasoning"}` input item carrying that `encrypted_content`
//! back. That is exactly the role the signature plays on the Anthropic wire —
//! an opaque blob the backend validates against the reasoning it accompanies —
//! so the journal round-trips it for free and a tool loop hands the model its
//! own reasoning back with the results, as the Responses API expects.

use anyhow::Context;
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::credentials::TokenSource;
use super::message::{ContentBlock, Role, StopReason, Usage};
use super::provider::{ChatRequest, EventStream, Provider, StreamEvent, ToolChoice, ToolDef};

pub struct Codex {
    name: String,
    base_url: String,
    token: TokenSource,
    client: reqwest::Client,
    session_id: String,
}

impl Codex {
    pub fn new(name: impl Into<String>, base_url: impl Into<String>, token: TokenSource) -> Self {
        let client = super::http::streaming_client_builder().build().expect("reqwest client");
        Codex {
            name: name.into(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            token,
            client,
            session_id: new_session_id(),
        }
    }

    /// The responses endpoint under a base configured to the codex root,
    /// e.g. `https://chatgpt.com/backend-api/codex` → `…/codex/responses`.
    pub fn endpoint(base: &str) -> String {
        format!("{}/responses", base.trim_end_matches('/'))
    }

    /// The outgoing body. Public so a consumer that wraps its own HTTP
    /// (headers, compat, hooks) can reuse the translation.
    pub fn request_body(req: &ChatRequest) -> Value {
        let mut input: Vec<Value> = Vec::new();
        // Consecutive text/image blocks of one message group into a single
        // message item; tool calls and reasoning are standalone items in
        // place, so the turn's block order survives.
        let mut pending: Option<Value> = None;
        let flush = |pending: &mut Option<Value>, input: &mut Vec<Value>| {
            if let Some(item) = pending.take() {
                input.push(item);
            }
        };
        for m in req.wire_messages().iter() {
            let role = if m.role == Role::Assistant { "assistant" } else { "user" };
            for b in &m.content {
                match b {
                    ContentBlock::Text { text } => {
                        let part = if m.role == Role::Assistant {
                            json!({ "type": "output_text", "text": text })
                        } else {
                            json!({ "type": "input_text", "text": text })
                        };
                        pending.get_or_insert_with(|| json!({ "role": role, "content": [] }))
                            ["content"].as_array_mut().expect("content array").push(part);
                    }
                    ContentBlock::Image { source, .. } => {
                        let url = source.data_uri();
                        pending.get_or_insert_with(|| json!({ "role": role, "content": [] }))
                            ["content"].as_array_mut().expect("content array")
                            .push(json!({ "type": "input_image", "image_url": url }));
                    }
                    ContentBlock::ToolUse { id, name, input: args_json } => {
                        flush(&mut pending, &mut input);
                        let arguments = args_json.to_value().to_string();
                        input.push(json!({ "type": "function_call", "call_id": id, "name": name, "arguments": arguments }));
                    }
                    ContentBlock::ToolResult { tool_use_id, content, .. } => {
                        flush(&mut pending, &mut input);
                        input.push(json!({ "type": "function_call_output", "call_id": tool_use_id, "output": content }));
                    }
                    // A thinking block is this wire's own reasoning item,
                    // round-tripped through the signature; one without a
                    // signature (a model-switch carry-over from the Anthropic
                    // wire) has nothing the backend would accept back.
                    ContentBlock::Thinking { thinking, signature } if !signature.is_empty() => {
                        flush(&mut pending, &mut input);
                        input.push(json!({
                            "type": "reasoning",
                            "summary": [{ "type": "summary_text", "text": thinking }],
                            "encrypted_content": signature,
                        }));
                    }
                    ContentBlock::Thinking { .. } | ContentBlock::RedactedThinking { .. } => {}
                }
            }
            flush(&mut pending, &mut input);
        }

        let mut body = json!({
            "model": req.model,
            "instructions": req.system.as_deref().unwrap_or(""),
            "input": input,
            "stream": true,
            "store": false,
            // The encrypted reasoning items this asks for are what makes the
            // signature round-trip above possible.
            "include": ["reasoning.encrypted_content"],
        });
        if !req.tools.is_empty() {
            body["tools"] = Value::Array(req.tools.iter().map(tool_to_wire).collect());
            if req.tool_choice == ToolChoice::Required {
                body["tool_choice"] = json!("required");
            }
        }
        if let Some(t) = &req.thinking {
            body["reasoning"] = json!({ "effort": effort_for(t.budget_tokens), "summary": "auto" });
        }
        // A prefix cache behind a load balancer needs the session's name to
        // find the machine holding the entry the last request wrote.
        if !req.cache.retention.is_off()
            && let Some(key) = &req.cache.key
        {
            body["prompt_cache_key"] = json!(key);
        }
        body
    }

    async fn open(&self, body: &Value, cancel: &CancellationToken) -> anyhow::Result<reqwest::Response> {
        let mut rb = self
            .client
            .post(Self::endpoint(&self.base_url))
            .header("accept", "text/event-stream")
            .json(body);
        if let Some(t) = self.token.read()? {
            rb = rb.bearer_auth(t);
        }
        let account = match &self.token {
            TokenSource::CodexRefresh { path } => super::credentials::codex_account_id(path),
            _ => None,
        };
        for (k, v) in client_headers(account.as_deref(), &self.session_id) {
            rb = rb.header(k, v);
        }
        let resp = tokio::select! {
            r = rb.send() => r.with_context(|| format!("{}: sending request", self.name))?,
            _ = cancel.cancelled() => anyhow::bail!("cancelled"),
        };
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("{} returned HTTP {status}: {}", self.name, text.chars().take(2000).collect::<String>());
        }
        Ok(resp)
    }
}

/// A session-scoped opaque id, the way the CLI identifies a conversation.
pub fn new_session_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// The account id out of a credential file, for the header below.
pub use super::credentials::codex_account_id;

/// The client masquerade the backend expects. `session_id` should be stable
/// for a provider instance's lifetime.
pub fn client_headers(account_id: Option<&str>, session_id: &str) -> Vec<(&'static str, String)> {
    let mut out = vec![
        ("originator", "codex_cli_rs".to_string()),
        ("User-Agent", "codex_cli_rs/0.55.0 (Ubuntu 26.04; x86_64) Linux".to_string()),
        ("session_id", session_id.to_string()),
    ];
    if let Some(id) = account_id {
        out.push(("ChatGPT-Account-Id", id.to_string()));
    }
    out
}

fn tool_to_wire(t: &ToolDef) -> Value {
    json!({ "type": "function", "name": t.name, "description": t.description, "parameters": t.input_schema })
}

/// The Anthropic-shaped thinking budget, translated to the discrete efforts
/// this backend offers. The bands are the ones the Codex-compatible proxies
/// converged on; a budget nobody mapped is `medium`, the backend's own
/// default.
fn effort_for(budget: u32) -> &'static str {
    match budget {
        0..=1024 => "low",
        1025..=8192 => "medium",
        8193..=24576 => "high",
        24577..=98304 => "xhigh",
        _ => "max",
    }
}

fn usage_from(v: &Value) -> Usage {
    let g = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
    Usage {
        input_tokens: g("input_tokens"),
        output_tokens: g("output_tokens"),
        // This backend reports cache hits flat; the public Responses API
        // nests them. Either way they are *not* inside input_tokens.
        cache_read_input_tokens: g("cached_tokens")
            + v.pointer("/input_tokens_details/cached_tokens").and_then(Value::as_u64).unwrap_or(0),
        cache_creation_input_tokens: 0,
    }
}

/// Maps the upstream SSE stream onto [`StreamEvent`]s, one instance per
/// response. Holds only what the terminal event needs: whether a tool call
/// was seen (the stop reason) and which argument streams already delivered.
#[derive(Default)]
pub struct Mapper {
    started: bool,
    saw_tool_call: bool,
    stopped: bool,
    args_seen: Vec<String>,
}

impl Mapper {
    fn start(&mut self) -> Vec<StreamEvent> {
        if self.started {
            return vec![];
        }
        self.started = true;
        vec![StreamEvent::Start { usage: Usage::default() }]
    }

    pub fn map(&mut self, ty: &str, data: &Value) -> anyhow::Result<Vec<StreamEvent>> {
        if self.stopped {
            return Ok(vec![]);
        }
        let mut out = match ty {
            "response.created" => self.start(),
            "response.output_item.added" => {
                let item = &data["item"];
                if let Some("function_call" | "custom_tool_call") = item.get("type").and_then(Value::as_str) {
                    let mut out = self.start();
                    self.saw_tool_call = true;
                    let id = item.get("call_id").and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .or_else(|| item.get("id").and_then(Value::as_str))
                        .unwrap_or_default();
                    let item_id = item.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
                    self.args_seen.retain(|i| i != &item_id);
                    out.push(StreamEvent::ToolUseStart { id: id.to_string(), name: item["name"].as_str().unwrap_or_default().to_string() });
                    out
                } else {
                    vec![]
                }
            }
            "response.output_text.delta" => {
                let mut out = self.start();
                out.push(StreamEvent::TextDelta(data["delta"].as_str().unwrap_or_default().to_string()));
                out
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                let mut out = self.start();
                out.push(StreamEvent::ThinkingDelta(data["delta"].as_str().unwrap_or_default().to_string()));
                out
            }
            "response.function_call_arguments.delta" | "response.custom_tool_call_input.delta" => {
                let mut out = self.start();
                if let Some(id) = data.get("item_id").and_then(Value::as_str) {
                    self.args_seen.push(id.to_string());
                }
                out.push(StreamEvent::ToolInputDelta(data["delta"].as_str().unwrap_or_default().to_string()));
                out
            }
            // The complete arguments, for an endpoint that sends no deltas.
            "response.function_call_arguments.done" => {
                let item_id = data.get("item_id").and_then(Value::as_str).unwrap_or_default().to_string();
                if !item_id.is_empty() && !self.args_seen.contains(&item_id) {
                    let args = data["arguments"].as_str().unwrap_or_default();
                    if !args.is_empty() {
                        return Ok(vec![StreamEvent::ToolInputDelta(args.to_string())]);
                    }
                }
                vec![]
            }
            "response.output_item.done" => {
                let item = &data["item"];
                match item.get("type").and_then(Value::as_str) {
                    Some("reasoning") => {
                        let mut out = vec![];
                        if let Some(enc) = item.get("encrypted_content").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                            out.push(StreamEvent::ThinkingSignature(enc.to_string()));
                        }
                        out.push(StreamEvent::BlockStop);
                        out
                    }
                    Some("message" | "function_call" | "custom_tool_call") => vec![StreamEvent::BlockStop],
                    _ => vec![],
                }
            }
            "response.completed" | "response.incomplete" => {
                let resp = &data["response"];
                let status = resp.get("status").and_then(Value::as_str).unwrap_or("completed");
                let stop_reason = if self.saw_tool_call {
                    StopReason::ToolUse
                } else if status == "incomplete" {
                    match resp.pointer("/incomplete_details/reason").and_then(Value::as_str) {
                        Some("max_output_tokens") => StopReason::MaxTokens,
                        _ => StopReason::Other,
                    }
                } else {
                    StopReason::EndTurn
                };
                self.stopped = true;
                vec![StreamEvent::Stop { stop_reason, usage: resp.get("usage").map(usage_from).unwrap_or_default() }]
            }
            "response.failed" | "error" => {
                let e = &data["error"];
                let msg = ["message", "detail"]
                    .iter().find_map(|k| e.get(k).and_then(Value::as_str))
                    .or_else(|| data.get("message").and_then(Value::as_str))
                    .unwrap_or("unknown error");
                let code = e.get("code").and_then(Value::as_str).unwrap_or("");
                anyhow::bail!("{}{msg}", if code.is_empty() { String::new() } else { format!("{code}: ") });
            }
            // Quota telemetry the proxy family consumes for its own gauges.
            _ => vec![],
        };
        if !self.started && !out.is_empty() {
            let mut start = self.start();
            start.append(&mut out);
            out = start;
        }
        Ok(out)
    }
}

impl Provider for Codex {
    fn name(&self) -> &str {
        &self.name
    }

    fn stream<'a>(&'a self, req: ChatRequest, cancel: CancellationToken) -> EventStream<'a> {
        Box::pin(async_stream::try_stream! {
            let body = Self::request_body(&req);
            let resp = self.open(&body, &cancel).await?;
            let mut events = std::pin::pin!(super::sse::events(resp.bytes_stream()));
            let mut mapper = Mapper::default();
            loop {
                let next = tokio::select! {
                    n = events.next() => n,
                    _ = cancel.cancelled() => break,
                };
                let Some(ev) = next else { break };
                let ev = ev?;
                if ev.data.trim() == "[DONE]" { continue; }
                if ev.data.is_empty() { continue; }
                let data: Value = serde_json::from_str(&ev.data)?;
                let ty = ev.event.as_deref()
                    .or_else(|| data.get("type").and_then(Value::as_str))
                    .unwrap_or("");
                for out in mapper.map(ty, &data)? {
                    yield out;
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::message::{Json, Message};
    use crate::llm::provider::{CacheOptions, CacheRetention, ThinkingConfig};

    fn req(messages: Vec<Message>) -> ChatRequest {
        ChatRequest {
            model: "gpt-6-astra".into(),
            system: Some("be brief".into()),
            messages,
            tools: vec![ToolDef { name: "read".into(), description: "Read a file".into(), input_schema: json!({"type":"object"}) }],
            max_tokens: 1024,
            thinking: Some(ThinkingConfig { budget_tokens: 8192 }),
            cache: CacheOptions::default(),
            vision: None,
            temperature: None,
            tool_choice: ToolChoice::Auto,
        }
    }

    /// A full tool-loop conversation becomes the flat item array the backend
    /// expects, order preserved: user text, the reasoning item, the call, the
    /// call's output, the answer.
    #[test]
    fn a_tool_loop_becomes_items_in_order() {
        let body = Codex::request_body(&req(vec![
            Message::user_text("read a"),
            Message::assistant(vec![
                ContentBlock::Thinking { thinking: "looking".into(), signature: "enc_1".into() },
                ContentBlock::ToolUse { id: "call_1".into(), name: "read".into(), input: Json(r#"{"path":"a"}"#.into()) },
            ]),
            Message::user(vec![ContentBlock::ToolResult { tool_use_id: "call_1".into(), content: "the file".into(), is_error: false }]),
            Message::assistant(vec![ContentBlock::text("it says hi")]),
        ]));
        let input = body["input"].as_array().unwrap();
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"][0]["type"], "input_text");
        assert_eq!(input[1]["type"], "reasoning");
        assert_eq!(input[1]["encrypted_content"], "enc_1");
        assert_eq!(input[1]["summary"][0]["text"], "looking");
        assert_eq!(input[2]["type"], "function_call");
        assert_eq!(input[2]["call_id"], "call_1");
        assert_eq!(input[2]["arguments"], r#"{"path":"a"}"#);
        assert_eq!(input[3]["type"], "function_call_output");
        assert_eq!(input[3]["output"], "the file");
        assert_eq!(input[4]["role"], "assistant");
        assert_eq!(input[4]["content"][0]["type"], "output_text");
        assert_eq!(body["instructions"], "be brief");
        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], true);
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["reasoning"]["effort"], "medium");
    }

    /// A thinking block without a signature — an Anthropic-wire model's
    /// carry-over on a shared branch — is dropped, not sent as a reasoning
    /// item the backend would reject.
    #[test]
    fn unsigned_thinking_is_dropped() {
        let body = Codex::request_body(&req(vec![
            Message::assistant(vec![ContentBlock::Thinking { thinking: "hmm".into(), signature: String::new() }]),
        ]));
        assert!(body["input"].as_array().unwrap().is_empty());
    }

    #[test]
    fn an_image_becomes_an_input_image_part() {
        let body = Codex::request_body(&req(vec![Message::user(vec![
            ContentBlock::text("what is this?"),
            ContentBlock::image("image/png", "AAAA", Some("shot.png".into())),
        ])]));
        assert_eq!(body["input"][0]["content"][1]["type"], "input_image");
        assert_eq!(body["input"][0]["content"][1]["image_url"], "data:image/png;base64,AAAA");
    }

    #[test]
    fn the_cache_key_travels_as_prompt_cache_key() {
        let mut r = req(vec![Message::user_text("hi")]);
        r.cache = CacheOptions { retention: CacheRetention::Short, key: Some("session-1".into()) };
        assert_eq!(Codex::request_body(&r)["prompt_cache_key"], "session-1");
        r.cache = CacheOptions::off();
        assert!(Codex::request_body(&r).get("prompt_cache_key").is_none());
    }

    /// Effort bands, and the two tool-choice spellings.
    #[test]
    fn effort_bands_and_tool_choice() {
        for (budget, effort) in [(512u32, "low"), (8192, "medium"), (20000, "high"), (50000, "xhigh"), (150000, "max")] {
            let mut r = req(vec![Message::user_text("hi")]);
            r.thinking = Some(ThinkingConfig { budget_tokens: budget });
            assert_eq!(Codex::request_body(&r)["reasoning"]["effort"], effort);
        }
        let mut r = req(vec![Message::user_text("hi")]);
        r.thinking = None;
        assert!(Codex::request_body(&r).get("reasoning").is_none());
        r.tool_choice = ToolChoice::Required;
        assert_eq!(Codex::request_body(&r)["tool_choice"], "required");
        r.tools = vec![];
        assert!(Codex::request_body(&r).get("tool_choice").is_none());
    }

    /// A streamed tool call, end to end: created → item added → argument
    /// deltas → item done → completed with `tool_use`.
    #[test]
    fn a_streamed_tool_call_maps_end_to_end() {
        let mut m = Mapper::default();
        let ev = |s: &str| serde_json::from_str::<Value>(s).unwrap();
        let mut all = vec![];
        for (ty, d) in [
            ("response.created", r#"{"type":"response.created","response":{"id":"r1"}}"#),
            ("response.output_item.added", r#"{"item":{"id":"fc_1","call_id":"call_1","type":"function_call","name":"read"}}"#),
            ("response.function_call_arguments.delta", r#"{"item_id":"fc_1","call_id":"call_1","delta":"{\"path\"" }"#),
            ("response.function_call_arguments.delta", r#"{"item_id":"fc_1","delta":":\"a\"}"}"#),
            ("response.output_item.done", r#"{"item":{"id":"fc_1","call_id":"call_1","type":"function_call","status":"completed"}}"#),
            ("response.completed", r#"{"response":{"status":"completed","usage":{"input_tokens":100,"output_tokens":5,"cached_tokens":40}}}"#),
        ] {
            all.extend(m.map(ty, &ev(d)).unwrap());
        }
        assert_eq!(
            all,
            vec![
                StreamEvent::Start { usage: Usage::default() },
                StreamEvent::ToolUseStart { id: "call_1".into(), name: "read".into() },
                StreamEvent::ToolInputDelta("{\"path\"".into()),
                StreamEvent::ToolInputDelta(":\"a\"}".into()),
                StreamEvent::BlockStop,
                StreamEvent::Stop {
                    stop_reason: StopReason::ToolUse,
                    usage: Usage { input_tokens: 100, output_tokens: 5, cache_read_input_tokens: 40, ..Default::default() },
                },
            ]
        );
    }

    /// Reasoning streams as summary deltas and lands its encrypted content
    /// as the signature, so the next request round-trips it.
    #[test]
    fn reasoning_round_trips_through_the_signature() {
        let mut m = Mapper::default();
        let ev = |s: &str| serde_json::from_str::<Value>(s).unwrap();
        let mut all = vec![];
        for (ty, d) in [
            ("response.created", r#"{"type":"response.created","response":{"id":"r1"}}"#),
            ("response.output_item.added", r#"{"item":{"id":"rs_1","type":"reasoning"}}"#),
            ("response.reasoning_summary_text.delta", r#"{"delta":"thinking"}"#),
            ("response.output_item.done", r#"{"item":{"id":"rs_1","type":"reasoning","encrypted_content":"enc_9"}}"#),
            ("response.completed", r#"{"response":{"status":"completed","usage":{}}}"#),
        ] {
            all.extend(m.map(ty, &ev(d)).unwrap());
        }
        assert_eq!(all[1], StreamEvent::ThinkingDelta("thinking".into()));
        assert_eq!(all[2], StreamEvent::ThinkingSignature("enc_9".into()));
        assert_eq!(all[3], StreamEvent::BlockStop);
        assert!(matches!(all[4], StreamEvent::Stop { stop_reason: StopReason::EndTurn, .. }));
    }

    /// An endpoint that sends no argument deltas still delivers the call.
    #[test]
    fn arguments_that_never_streamed_arrive_on_done() {
        let mut m = Mapper::default();
        let ev = |s: &str| serde_json::from_str::<Value>(s).unwrap();
        m.map("response.output_item.added", &ev(r#"{"item":{"id":"fc_2","call_id":"call_2","type":"function_call","name":"read"}}"#)).unwrap();
        let out = m.map(
            "response.function_call_arguments.done",
            &ev(r#"{"item_id":"fc_2","arguments":"{\"path\":\"b\"}"}"#),
        ).unwrap();
        assert_eq!(out, vec![StreamEvent::ToolInputDelta("{\"path\":\"b\"}".into())]);
    }

    #[test]
    fn failure_events_fail_the_stream() {
        let mut m = Mapper::default();
        let e = m.map("response.failed", &serde_json::from_str::<Value>(r#"{"error":{"code":"quota_exhausted","message":"out of messages"}}"#).unwrap()).unwrap_err();
        assert_eq!(e.to_string(), "quota_exhausted: out of messages");
    }

    /// A missed `response.created` does not leave the stream block-less: the
    /// first mapped event implies the start.
    #[test]
    fn a_missing_created_is_implied() {
        let mut m = Mapper::default();
        let out = m.map("response.output_text.delta", &serde_json::from_str::<Value>(r#"{"delta":"hi"}"#).unwrap()).unwrap();
        assert_eq!(out, vec![StreamEvent::Start { usage: Usage::default() }, StreamEvent::TextDelta("hi".into())]);
    }

    /// Truncation reads as truncation, not as a finished answer.
    #[test]
    fn an_incomplete_response_says_max_tokens() {
        let mut m = Mapper::default();
        let ev = |s: &str| serde_json::from_str::<Value>(s).unwrap();
        m.map("response.created", &ev(r#"{"response":{"id":"r1"}}"#)).unwrap();
        let out = m.map(
            "response.completed",
            &ev(r#"{"response":{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"usage":{}}}"#),
        ).unwrap();
        assert!(matches!(out[0], StreamEvent::Stop { stop_reason: StopReason::MaxTokens, .. }));
    }
}
