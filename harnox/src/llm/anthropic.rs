//! The Anthropic Messages API, streamed — the native wire.
//!
//! What this module does, and only this (a wrapper's docs must describe its
//! own subset, never read like the protocol spec): builds one `POST
//! {base}/v1/messages` with `stream: true`, maps the SSE events it knows to
//! [`StreamEvent`], and ignores the rest. Unknown `stop_reason` values
//! become `StopReason::Other`; unknown event types are skipped.
//!
//! Auth is one of two headers, chosen by [`AuthStyle`]: `x-api-key` for an
//! API key, or `Authorization: Bearer` plus the OAuth beta header for a
//! `claude setup-token` bearer. Kimi's Anthropic-compatible endpoint is
//! this same code with a different base URL and an API key.
//!
//! Prompt caching: three breakpoints, which is what an agent loop needs and
//! one under the wire's limit of four. Two are the stable prefix — the
//! system prompt and the last tool definition — and the third *rolls*: the
//! last cacheable block of the last message, so the conversation itself is
//! cached and not just the header above it.
//!
//! The rolling one is the whole saving. A turn that calls tools sends the
//! entire transcript again on every iteration, and the transcript is the
//! large part: without a breakpoint below the tools, every one of those
//! requests re-reads the whole conversation at the full input rate. With
//! one, each request writes only what was added since the last and reads
//! the rest from the cache at a tenth of it or less. The endpoint matches
//! the longest cached prefix at or before a breakpoint, so the entry the
//! previous request wrote is found without this one naming its position.
//!
//! Which block it lands on matters: a `thinking` block cannot carry
//! `cache_control`, so the search walks back to the last block that can.
//! [`CacheRetention`] chooses the entry's lifetime, and `None` places no
//! breakpoints at all — a cache write is a surcharge (1.25× the base rate,
//! 2× for the hour-long entry), so a request whose prefix nothing will
//! share should not pay it.

use anyhow::{Context, bail};
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::credentials::TokenSource;
use super::message::{ContentBlock, Message, StopReason, Usage};
use super::provider::{CacheRetention, ChatRequest, EventStream, Provider, StreamEvent, ToolChoice, ToolDef};
use super::spec::AuthStyle;

pub const API_VERSION: &str = "2023-06-01";
const OAUTH_BETA: &str = "oauth-2025-04-20";

pub struct Anthropic {
    name: String,
    base_url: String,
    token: TokenSource,
    auth: AuthStyle,
    client: reqwest::Client,
}

impl Anthropic {
    pub fn new(name: impl Into<String>, base_url: impl Into<String>, token: TokenSource, auth: AuthStyle) -> Self {
        let client = super::http::streaming_client_builder().build().expect("reqwest client");
        Anthropic { name: name.into(), base_url: base_url.into().trim_end_matches('/').to_string(), token, auth, client }
    }

    pub fn request_body(req: &ChatRequest) -> Value {
        let mark = cache_control(req.cache.retention);
        let mut messages: Vec<Value> = req.wire_messages().iter().map(message_to_wire).collect();
        // The rolling breakpoint: the end of the conversation as it stands.
        if let Some(mark) = &mark
            && let Some(block) = messages.last_mut().and_then(last_cacheable)
        {
            block["cache_control"] = mark.clone();
        }
        let mut body = json!({
            "model": req.model,
            "max_tokens": req.max_tokens,
            "stream": true,
            "messages": messages,
        });
        if let Some(system) = &req.system {
            let mut block = json!({ "type": "text", "text": system });
            if let Some(mark) = &mark {
                block["cache_control"] = mark.clone();
            }
            body["system"] = json!([block]);
        }
        if !req.tools.is_empty() {
            let mut tools: Vec<Value> = req.tools.iter().map(tool_to_wire).collect();
            if let Some(mark) = &mark
                && let Some(last) = tools.last_mut()
            {
                last["cache_control"] = mark.clone();
            }
            body["tools"] = Value::Array(tools);
        }
        if let Some(t) = &req.thinking {
            body["thinking"] = json!({ "type": "enabled", "budget_tokens": t.budget_tokens });
        }
        // Not alongside thinking, which requires the default and answers
        // `400` to anything else.
        if let Some(t) = req.temperature
            && req.thinking.is_none()
        {
            body["temperature"] = json!(t);
        }
        // Only with tools to choose from: `tool_choice` over an empty list
        // is a `400` on every endpoint that reads the field at all.
        if req.tool_choice == ToolChoice::Required && !req.tools.is_empty() {
            body["tool_choice"] = json!({ "type": "any" });
        }
        body
    }

    /// Send the request and check the status, cancellably. Separated from the
    /// stream so early exits are plain `?`/`bail!` rather than `Err(..)?`
    /// inside `try_stream!` (which cannot infer the `Ok` type).
    async fn open(&self, req: &ChatRequest, cancel: &CancellationToken) -> anyhow::Result<reqwest::Response> {
        let mut rb = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("anthropic-version", API_VERSION)
            .header("accept", "text/event-stream")
            .json(&Self::request_body(req));
        match (self.auth, self.token.read()?) {
            (AuthStyle::ApiKey, Some(t)) => rb = rb.header("x-api-key", t),
            (AuthStyle::Bearer, Some(t)) => rb = rb.bearer_auth(t).header("anthropic-beta", OAUTH_BETA),
            (_, None) => {}
        }
        let resp = tokio::select! {
            r = rb.send() => r.with_context(|| format!("{}: sending request", self.name))?,
            _ = cancel.cancelled() => bail!("cancelled"),
        };
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            bail!("{} returned HTTP {status}: {}", self.name, text.chars().take(2000).collect::<String>());
        }
        Ok(resp)
    }
}

/// The `cache_control` a breakpoint carries, or `None` when this request
/// wants no caching at all.
fn cache_control(retention: CacheRetention) -> Option<Value> {
    match retention {
        CacheRetention::None => None,
        CacheRetention::Short => Some(json!({ "type": "ephemeral" })),
        CacheRetention::Long => Some(json!({ "type": "ephemeral", "ttl": "1h" })),
    }
}

/// The last block of a wire message that may carry a breakpoint.
///
/// `thinking` and `redacted_thinking` may not — the endpoint rejects the
/// request — and an assistant turn that reasoned and then stopped ends on
/// one, so taking the last block unconditionally would fail exactly the
/// turns worth caching. `None` when the message has no cacheable block,
/// which is not an error: the breakpoints above it still stand.
fn last_cacheable(m: &mut Value) -> Option<&mut Value> {
    m.get_mut("content")?
        .as_array_mut()?
        .iter_mut()
        .rev()
        .find(|b| matches!(b.get("type").and_then(Value::as_str), Some("text" | "image" | "tool_use" | "tool_result" | "document")))
}

/// Wire form of a tool definition.
fn tool_to_wire(t: &ToolDef) -> Value {
    json!({ "name": t.name, "description": t.description, "input_schema": t.input_schema })
}

/// Wire form of a message. Differs from the model's own serde only in that
/// a tool input is a JSON object here and JSON *text* in the log.
pub fn message_to_wire(m: &Message) -> Value {
    let content: Vec<Value> = m
        .content
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => json!({ "type": "text", "text": text }),
            ContentBlock::Thinking { thinking, signature } => {
                json!({ "type": "thinking", "thinking": thinking, "signature": signature })
            }
            ContentBlock::RedactedThinking { data } => json!({ "type": "redacted_thinking", "data": data }),

            ContentBlock::ToolUse { id, name, input } => {
                json!({ "type": "tool_use", "id": id, "name": name, "input": input.to_value() })
            }
            ContentBlock::ToolResult { tool_use_id, content, is_error } => {
                let mut v = json!({ "type": "tool_result", "tool_use_id": tool_use_id, "content": content });
                if *is_error {
                    v["is_error"] = json!(true);
                }
                v
            }
            // `alt` is ours and stays ours: the wire has no field for it,
            // and `source` is already the wire's own union.
            ContentBlock::Image { source, .. } => json!({ "type": "image", "source": source }),

        })
        .collect();
    json!({ "role": m.role, "content": content })
}

fn usage_from(v: &Value) -> Usage {
    let g = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
    Usage {
        input_tokens: g("input_tokens"),
        output_tokens: g("output_tokens"),
        cache_creation_input_tokens: g("cache_creation_input_tokens"),
        cache_read_input_tokens: g("cache_read_input_tokens"),
    }
}

fn stop_reason(s: Option<&str>) -> StopReason {
    match s {
        Some("end_turn") => StopReason::EndTurn,
        Some("tool_use") => StopReason::ToolUse,
        Some("max_tokens") => StopReason::MaxTokens,
        Some("stop_sequence") => StopReason::StopSequence,
        Some("refusal") => StopReason::Refusal,
        _ => StopReason::Other,
    }
}

/// Map one parsed SSE event to zero or more stream events.
pub fn map_event(event: Option<&str>, data: &Value) -> anyhow::Result<Vec<StreamEvent>> {
    let ty = event.or_else(|| data.get("type").and_then(Value::as_str)).unwrap_or("");
    Ok(match ty {
        "message_start" => {
            let usage = data.pointer("/message/usage").map(usage_from).unwrap_or_default();
            vec![StreamEvent::Start { usage }]
        }
        "content_block_start" => {
            let block = &data["content_block"];
            match block.get("type").and_then(Value::as_str) {
                Some("tool_use") => vec![StreamEvent::ToolUseStart {
                    id: block["id"].as_str().unwrap_or_default().to_string(),
                    name: block["name"].as_str().unwrap_or_default().to_string(),
                }],
                Some("redacted_thinking") => {
                    vec![StreamEvent::RedactedThinking(block["data"].as_str().unwrap_or_default().to_string())]
                }
                Some("text") => {
                    let t = block["text"].as_str().unwrap_or_default();
                    if t.is_empty() { vec![] } else { vec![StreamEvent::TextDelta(t.to_string())] }
                }
                _ => vec![],
            }
        }
        "content_block_delta" => {
            let delta = &data["delta"];
            match delta.get("type").and_then(Value::as_str) {
                Some("text_delta") => vec![StreamEvent::TextDelta(delta["text"].as_str().unwrap_or_default().to_string())],
                Some("input_json_delta") => {
                    vec![StreamEvent::ToolInputDelta(delta["partial_json"].as_str().unwrap_or_default().to_string())]
                }
                Some("thinking_delta") => {
                    vec![StreamEvent::ThinkingDelta(delta["thinking"].as_str().unwrap_or_default().to_string())]
                }
                Some("signature_delta") => {
                    vec![StreamEvent::ThinkingSignature(delta["signature"].as_str().unwrap_or_default().to_string())]
                }
                _ => vec![],
            }
        }
        "content_block_stop" => vec![StreamEvent::BlockStop],
        "message_delta" => {
            let sr = stop_reason(data.pointer("/delta/stop_reason").and_then(Value::as_str));
            let usage = data.get("usage").map(usage_from).unwrap_or_default();
            vec![StreamEvent::Stop { stop_reason: sr, usage }]
        }
        "error" => {
            let msg = data.pointer("/error/message").and_then(Value::as_str).unwrap_or("unknown error");
            let kind = data.pointer("/error/type").and_then(Value::as_str).unwrap_or("error");
            anyhow::bail!("{kind}: {msg}");
        }
        _ => vec![],
    })
}

impl Provider for Anthropic {
    fn name(&self) -> &str {
        &self.name
    }

    fn stream<'a>(&'a self, req: ChatRequest, cancel: CancellationToken) -> EventStream<'a> {
        Box::pin(async_stream::try_stream! {
            let resp = self.open(&req, &cancel).await?;
            let mut events = std::pin::pin!(super::sse::events(resp.bytes_stream()));
            loop {
                let next = tokio::select! {
                    n = events.next() => n,
                    _ = cancel.cancelled() => break,
                };
                let Some(ev) = next else { break };
                let ev = ev?;
                if ev.data.is_empty() { continue; }
                let data: Value = serde_json::from_str(&ev.data)?;
                for out in map_event(ev.event.as_deref(), &data)? {
                    yield out;
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::message::Json;
    use crate::llm::provider::CacheOptions;
    use crate::llm::ThinkingConfig;

    #[test]
    fn tool_input_is_an_object_on_the_wire() {
        let m = Message::assistant(vec![ContentBlock::ToolUse {
            id: "t1".into(),
            name: "read".into(),
            input: Json("{\"path\":\"a\"}".into()),
        }]);
        let w = message_to_wire(&m);
        assert_eq!(w["content"][0]["input"]["path"], "a");
    }

    /// Nobody said, so nothing is sent.
    #[test]
    fn neither_knob_appears_unasked() {
        let b = Anthropic::request_body(&req(vec![Message::user_text("hi")], CacheOptions::off()));
        assert!(b.get("temperature").is_none());
        assert!(b.get("tool_choice").is_none());
    }

    /// The shared `req` helper enables thinking, so this one turns it off:
    /// the two are never sent together.
    #[test]
    fn a_named_temperature_reaches_the_wire() {
        let base = req(vec![Message::user_text("hi")], CacheOptions::off());
        let r = ChatRequest { temperature: Some(0.2), thinking: None, ..base };
        assert_eq!(Anthropic::request_body(&r)["temperature"], serde_json::json!(0.2));
    }

    /// Extended thinking requires the default temperature and answers `400`
    /// to anything else, so the two are never sent together.
    #[test]
    fn thinking_suppresses_the_temperature() {
        let base = req(vec![Message::user_text("hi")], CacheOptions::off());
        let r = ChatRequest { temperature: Some(0.2), thinking: Some(ThinkingConfig { budget_tokens: 1024 }), ..base };
        assert!(Anthropic::request_body(&r).get("temperature").is_none());
    }

    /// The wire spells "you must call something" as `any`.
    #[test]
    fn forcing_the_channel_sends_any() {
        let r = ChatRequest { tool_choice: ToolChoice::Required, ..req(vec![Message::user_text("hi")], CacheOptions::off()) };
        assert_eq!(Anthropic::request_body(&r)["tool_choice"], serde_json::json!({ "type": "any" }));
    }

    #[test]
    fn forcing_with_no_tools_sends_nothing() {
        let base = req(vec![Message::user_text("hi")], CacheOptions::off());
        let r = ChatRequest { tool_choice: ToolChoice::Required, tools: vec![], ..base };
        assert!(Anthropic::request_body(&r).get("tool_choice").is_none());
    }

    fn req(messages: Vec<Message>, cache: CacheOptions) -> ChatRequest {
        ChatRequest {
            model: "claude-sonnet-5".into(),
            system: Some("be brief".into()),
            messages,
            tools: vec![
                ToolDef { name: "a".into(), description: "A".into(), input_schema: json!({"type":"object"}) },
                ToolDef { name: "b".into(), description: "B".into(), input_schema: json!({"type":"object"}) },
            ],
            max_tokens: 100,
            thinking: Some(super::super::provider::ThinkingConfig { budget_tokens: 1024 }),
            cache,
            vision: None,
            temperature: None,
            tool_choice: ToolChoice::Auto,
        }
    }

    #[test]
    fn request_body_caches_system_and_last_tool() {
        let body = Anthropic::request_body(&req(vec![Message::user_text("hi")], CacheOptions::default()));
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        assert!(body["tools"][0].get("cache_control").is_none());
        assert_eq!(body["tools"][1]["cache_control"]["type"], "ephemeral");
        assert_eq!(body["tools"][1]["input_schema"]["type"], "object");
        assert_eq!(body["thinking"]["budget_tokens"], 1024);
        assert_eq!(body["stream"], true);
    }

    /// The rolling breakpoint: the end of the conversation, not just the
    /// header above it. Without it every iteration of a tool loop re-reads
    /// the whole transcript at the full input rate.
    #[test]
    fn the_conversation_carries_a_rolling_breakpoint() {
        let messages = vec![
            Message::user_text("hi"),
            Message::assistant(vec![ContentBlock::ToolUse { id: "t1".into(), name: "read".into(), input: Json("{}".into()) }]),
            Message::user(vec![ContentBlock::ToolResult { tool_use_id: "t1".into(), content: "out".into(), is_error: false }]),
        ];
        let body = Anthropic::request_body(&req(messages, CacheOptions::default()));
        // Only the last message, and only its last block.
        assert!(body["messages"][0]["content"][0].get("cache_control").is_none());
        assert!(body["messages"][1]["content"][0].get("cache_control").is_none());
        assert_eq!(body["messages"][2]["content"][0]["cache_control"]["type"], "ephemeral");
    }

    /// A turn that reasoned and then stopped ends on a `thinking` block,
    /// which may not carry `cache_control` — so the breakpoint walks back
    /// to the block that can, rather than failing the request.
    #[test]
    fn a_breakpoint_never_lands_on_a_thinking_block() {
        let tail = Message::assistant(vec![
            ContentBlock::text("here goes"),
            ContentBlock::Thinking { thinking: "hmm".into(), signature: "sig".into() },
        ]);
        let body = Anthropic::request_body(&req(vec![Message::user_text("hi"), tail], CacheOptions::default()));
        assert_eq!(body["messages"][1]["content"][0]["cache_control"]["type"], "ephemeral");
        assert!(body["messages"][1]["content"][1].get("cache_control").is_none());
    }

    /// A message with nothing cacheable in it is not an error; the system
    /// and tool breakpoints above it still stand.
    #[test]
    fn a_message_with_no_cacheable_block_is_skipped() {
        let tail = Message::assistant(vec![ContentBlock::Thinking { thinking: "hmm".into(), signature: "sig".into() }]);
        let body = Anthropic::request_body(&req(vec![tail], CacheOptions::default()));
        assert!(body["messages"][0]["content"][0].get("cache_control").is_none());
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn long_retention_asks_for_the_hour_long_entry() {
        let cache = CacheOptions { retention: CacheRetention::Long, key: None };
        let body = Anthropic::request_body(&req(vec![Message::user_text("hi")], cache));
        assert_eq!(body["system"][0]["cache_control"], json!({ "type": "ephemeral", "ttl": "1h" }));
        assert_eq!(body["tools"][1]["cache_control"]["ttl"], "1h");
        assert_eq!(body["messages"][0]["content"][0]["cache_control"]["ttl"], "1h");
    }

    /// A write is a surcharge, so a request whose prefix nothing will ever
    /// share places no breakpoints at all.
    #[test]
    fn retention_none_places_no_breakpoints() {
        let body = Anthropic::request_body(&req(vec![Message::user_text("hi")], CacheOptions::off()));
        assert!(body["system"][0].get("cache_control").is_none());
        assert!(body["tools"][1].get("cache_control").is_none());
        assert!(body["messages"][0]["content"][0].get("cache_control").is_none());
    }

    #[test]
    fn maps_deltas() {
        let d: Value = json!({"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"{\"a\""}});
        assert_eq!(map_event(None, &d).unwrap(), vec![StreamEvent::ToolInputDelta("{\"a\"".into())]);
        let d: Value = json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":5}});
        assert!(matches!(map_event(None, &d).unwrap()[0], StreamEvent::Stop { stop_reason: StopReason::ToolUse, .. }));
        let d: Value = json!({"type":"message_start","message":{"usage":{"input_tokens":7,"cache_read_input_tokens":3}}});
        assert_eq!(
            map_event(Some("message_start"), &d).unwrap(),
            vec![StreamEvent::Start { usage: Usage { input_tokens: 7, cache_read_input_tokens: 3, ..Default::default() } }]
        );
        let d: Value = json!({"type":"error","error":{"type":"overloaded_error","message":"busy"}});
        assert_eq!(map_event(None, &d).unwrap_err().to_string(), "overloaded_error: busy");
    }
}
