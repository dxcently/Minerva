//! One adapter for OpenAI-compatible chat-completions endpoints.
//!
//! Translates the canonical (Anthropic-shaped) request into
//! `POST {base}/chat/completions` with `stream: true`, and the streamed
//! `choices[0].delta` chunks back into [`StreamEvent`]s. Tool-call
//! arguments arrive index-accumulated (`delta.tool_calls[i].function
//! .arguments` fragments); each new index opens a tool block, so a consumer's
//! assembler sees the same bracket structure the native wire gives it.
//!
//! What is lost in translation, on purpose: thinking blocks are not sent
//! back (the endpoint has no signature to verify), and `budget_tokens` is
//! dropped. This wire has no *standard* cache control — the endpoints that
//! cache do it on their own account, on the request prefix, so the saving
//! is bought by keeping that prefix byte-identical rather than by a field;
//! a gateway that does accept Anthropic-shaped `cache_control` blocks is a
//! per-endpoint quirk and belongs in the consumer's compatibility layer,
//! not here. What the endpoint then *charges* is read back in
//! [`usage_from`]. `reasoning_content` and `reasoning` deltas are surfaced
//! as `ThinkingDelta`; consumers can journal them as unsigned thinking.
//! Sending that thinking back remains an endpoint-specific compatibility knob.
//!
//! The stream honours the one-`Stop` contract: the `finish_reason` chunk is
//! held until the trailing usage chunk (`stream_options.include_usage`)
//! arrives, so the single `Stop` carries both; a provider that never sends
//! usage gets the `Stop` at end of stream with zero usage.

use anyhow::{Context, bail};
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::credentials::TokenSource;
use super::message::{ContentBlock, Message, Role, StopReason, Usage};
use super::provider::{ChatRequest, EventStream, Provider, StreamEvent, ToolChoice};

pub struct OpenAiCompat {
    name: String,
    base_url: String,
    token: TokenSource,
    client: reqwest::Client,
}

impl OpenAiCompat {
    pub fn new(name: impl Into<String>, base_url: impl Into<String>, token: TokenSource) -> Self {
        let client = super::http::streaming_client_builder().build().expect("reqwest client");
        OpenAiCompat { name: name.into(), base_url: base_url.into().trim_end_matches('/').to_string(), token, client }
    }

    pub fn request_body(req: &ChatRequest) -> Value {
        let mut messages: Vec<Value> = Vec::new();
        if let Some(s) = &req.system {
            messages.push(json!({ "role": "system", "content": s }));
        }
        for m in req.wire_messages().iter() {
            messages.extend(message_to_wire(m));
        }
        let mut body = json!({
            "model": req.model,
            "messages": messages,
            "stream": true,
            "stream_options": { "include_usage": true },
            "max_tokens": req.max_tokens,
        });
        if !req.tools.is_empty() {
            body["tools"] = Value::Array(
                req.tools
                    .iter()
                    .map(|t| {
                        json!({ "type": "function", "function": {
                            "name": t.name, "description": t.description, "parameters": t.input_schema } })
                    })
                    .collect(),
            );
            // Only with tools to choose from: the field over an empty list
            // is a `400` on every endpoint that reads it at all.
            if req.tool_choice == ToolChoice::Required {
                body["tool_choice"] = json!("required");
            }
        }
        // Not alongside a reasoning request: the models that take one reject
        // any temperature but their own default.
        if let Some(t) = req.temperature
            && req.thinking.is_none()
        {
            body["temperature"] = json!(t);
        }
        body
    }

    /// Send the request and check the status, cancellably (see
    /// `Anthropic::open` for why this is not inline in the stream).
    async fn open(&self, req: &ChatRequest, cancel: &CancellationToken) -> anyhow::Result<reqwest::Response> {
        let mut rb = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .header("accept", "text/event-stream")
            .json(&Self::request_body(req));
        if let Some(t) = self.token.read()? {
            rb = rb.bearer_auth(t);
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

/// One canonical message may become several wire messages: tool results
/// are `role: tool` messages of their own.
pub fn message_to_wire(m: &Message) -> Vec<Value> {
    match m.role {
        Role::User => {
            let mut out = Vec::new();
            let mut text = String::new();
            // Images make `content` a *list of parts* rather than a string.
            // Built only when there is an image to put in it: the string
            // form is what every image-free turn has always sent, and this
            // wire's prompt cache is a prefix cache, so quietly restating
            // the same words in a richer shape would miss on every message
            // of every session that never attaches anything.
            let mut parts: Vec<Value> = Vec::new();
            let imaged = m.has_images();
            for b in &m.content {
                match b {
                    ContentBlock::Text { text: t } => {
                        if imaged {
                            parts.push(json!({ "type": "text", "text": t }));
                        } else {
                            text.push_str(t);
                        }
                    }
                    ContentBlock::Image { source, .. } => {
                        parts.push(json!({ "type": "image_url", "image_url": { "url": source.data_uri() } }));
                    }

                    ContentBlock::ToolResult { tool_use_id, content, is_error } => {
                        let content = if *is_error { format!("ERROR: {content}") } else { content.clone() };
                        out.push(json!({ "role": "tool", "tool_call_id": tool_use_id, "content": content }));
                    }
                    _ => {}
                }
            }
            if !parts.is_empty() {
                out.push(json!({ "role": "user", "content": parts }));
            } else if !text.is_empty() {
                out.push(json!({ "role": "user", "content": text }));
            }
            out
        }
        Role::Assistant => {
            let mut text = String::new();
            let mut calls = Vec::new();
            for b in &m.content {
                match b {
                    ContentBlock::Text { text: t } => text.push_str(t),
                    ContentBlock::ToolUse { id, name, input } => calls.push(json!({
                        "id": id, "type": "function",
                        "function": { "name": name, "arguments": input.0 }
                    })),
                    _ => {}
                }
            }
            let mut v = json!({ "role": "assistant" });
            v["content"] = if text.is_empty() { Value::Null } else { Value::String(text) };
            if !calls.is_empty() {
                v["tool_calls"] = Value::Array(calls);
            }
            vec![v]
        }
    }
}

/// The usage chunk, read across the three places an OpenAI-compatible
/// endpoint puts its cache counters.
///
/// `prompt_tokens` is the *whole* prompt on this wire — cache hits
/// included — so the cached share has to be found and subtracted, or a
/// cache-warm turn is priced as though nothing was cached at all. The
/// providers worth caching on are exactly the ones that disagree about
/// where to say it: OpenAI and OpenRouter use
/// `prompt_tokens_details.cached_tokens`, DeepSeek uses
/// `prompt_cache_hit_tokens` beside its `prompt_cache_miss_tokens`, and
/// Moonshot puts a bare `cached_tokens` on the final chunk. Reading only
/// the first meant a DeepSeek session — the one whose cache reads cost a
/// thirtieth of its fresh ones — reported every token as fresh.
///
/// Writes are separate where they are reported at all
/// (`prompt_tokens_details.cache_write_tokens`, an OpenRouter extension)
/// and are *not* subtracted from the read count: a provider that follows
/// the documented semantics would be under-reported if they were.
fn usage_from(u: &Value) -> Usage {
    let g = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
    let cache_read = u
        .pointer("/prompt_tokens_details/cached_tokens")
        .or_else(|| u.get("prompt_cache_hit_tokens"))
        .or_else(|| u.get("cached_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cache_write = u.pointer("/prompt_tokens_details/cache_write_tokens").and_then(Value::as_u64).unwrap_or(0);
    Usage {
        input_tokens: g("prompt_tokens").saturating_sub(cache_read).saturating_sub(cache_write),
        output_tokens: g("completion_tokens"),
        cache_read_input_tokens: cache_read,
        cache_creation_input_tokens: cache_write,
    }
}

/// Per-stream state for turning chunks into events.
#[derive(Default)]
pub struct ChunkMapper {
    open_tool: Option<u64>,
    text_open: bool,
    /// The `finish_reason` seen so far, held back until usage arrives (or the
    /// stream ends) so exactly one `Stop` is emitted.
    finished: Option<StopReason>,
    stopped: bool,
}

impl ChunkMapper {
    pub fn map(&mut self, chunk: &Value) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        if let Some(choice) = chunk.pointer("/choices/0") {
            let delta = &choice["delta"];
            // Prefer the established spelling if a gateway supplies both;
            // an empty/null placeholder must not hide Ollama's actual delta.
            let reasoning = ["reasoning_content", "reasoning"].into_iter()
                .find_map(|key| delta.get(key).and_then(Value::as_str).filter(|s| !s.is_empty()));
            if let Some(r) = reasoning {
                out.push(StreamEvent::ThinkingDelta(r.to_string()));
            }
            if let Some(t) = delta.get("content").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                if self.open_tool.take().is_some() {
                    out.push(StreamEvent::BlockStop);
                }
                self.text_open = true;
                out.push(StreamEvent::TextDelta(t.to_string()));
            }
            if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for c in calls {
                    let idx = c.get("index").and_then(Value::as_u64).unwrap_or(0);
                    if self.open_tool != Some(idx) {
                        if self.open_tool.is_some() || self.text_open {
                            out.push(StreamEvent::BlockStop);
                            self.text_open = false;
                        }
                        self.open_tool = Some(idx);
                        out.push(StreamEvent::ToolUseStart {
                            id: c.get("id").and_then(Value::as_str).unwrap_or(&format!("call_{idx}")).to_string(),
                            name: c.pointer("/function/name").and_then(Value::as_str).unwrap_or_default().to_string(),
                        });
                    }
                    if let Some(a) = c.pointer("/function/arguments").and_then(Value::as_str).filter(|s| !s.is_empty()) {
                        out.push(StreamEvent::ToolInputDelta(a.to_string()));
                    }
                }
            }
            if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                if self.open_tool.take().is_some() || std::mem::take(&mut self.text_open) {
                    out.push(StreamEvent::BlockStop);
                }
                self.finished = Some(match reason {
                    "stop" => StopReason::EndTurn,
                    "tool_calls" | "function_call" => StopReason::ToolUse,
                    "length" => StopReason::MaxTokens,
                    "content_filter" => StopReason::Refusal,
                    _ => StopReason::Other,
                });
            }
        }
        if let Some(u) = chunk.get("usage").filter(|u| !u.is_null()) {
            out.extend(self.stop(usage_from(u)));
        }
        out
    }

    /// The single terminal event, if it hasn't been emitted yet. Called by
    /// the stream at `[DONE]`/EOF for a provider that never sent usage.
    pub fn finish(&mut self) -> Option<StreamEvent> {
        self.stop(Usage::default())
    }

    fn stop(&mut self, usage: Usage) -> Option<StreamEvent> {
        if self.stopped {
            return None;
        }
        self.stopped = true;
        Some(StreamEvent::Stop { stop_reason: self.finished.take().unwrap_or(StopReason::Other), usage })
    }
}

impl Provider for OpenAiCompat {
    fn name(&self) -> &str {
        &self.name
    }

    fn stream<'a>(&'a self, req: ChatRequest, cancel: CancellationToken) -> EventStream<'a> {
        Box::pin(async_stream::try_stream! {
            let resp = self.open(&req, &cancel).await?;
            yield StreamEvent::Start { usage: Usage::default() };
            let mut events = std::pin::pin!(super::sse::events(resp.bytes_stream()));
            let mut mapper = ChunkMapper::default();
            let mut cancelled = false;
            loop {
                let next = tokio::select! {
                    n = events.next() => n,
                    _ = cancel.cancelled() => { cancelled = true; break }
                };
                let Some(ev) = next else { break };
                let ev = ev?;
                if ev.data.trim() == "[DONE]" { break; }
                if ev.data.is_empty() { continue; }
                let chunk: Value = serde_json::from_str(&ev.data)?;
                if let Some(err) = chunk.get("error") {
                    Err(anyhow::anyhow!("{}: {}", self.name, err))?;
                }
                for out in mapper.map(&chunk) {
                    yield out;
                }
            }
            if !cancelled && let Some(stop) = mapper.finish() {
                yield stop;
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::message::Json;
    use crate::llm::{ThinkingConfig, ToolDef};

    fn treq() -> ChatRequest {
        ChatRequest {
            model: "m".into(),
            system: None,
            messages: vec![Message::user_text("hi")],
            tools: vec![ToolDef { name: "peers".into(), description: "p".into(), input_schema: json!({"type":"object"}) }],
            max_tokens: 10,
            thinking: None,
            cache: Default::default(),
            vision: None,
            temperature: None,
            tool_choice: ToolChoice::Auto,
        }
    }

    /// Nobody said, so nothing is sent — an endpoint that has never heard of
    /// either field is not made to hear of it for the ordinary turn.
    #[test]
    fn neither_knob_appears_unasked() {
        let b = OpenAiCompat::request_body(&treq());
        assert!(b.get("temperature").is_none());
        assert!(b.get("tool_choice").is_none());
    }

    #[test]
    fn a_named_temperature_reaches_the_wire() {
        let b = OpenAiCompat::request_body(&ChatRequest { temperature: Some(0.2), ..treq() });
        assert_eq!(b["temperature"], json!(0.2));
    }

    /// The reasoning models reject any temperature but their own default, so
    /// a thinking request never carries one.
    #[test]
    fn thinking_suppresses_the_temperature() {
        let req = ChatRequest { temperature: Some(0.2), thinking: Some(ThinkingConfig { budget_tokens: 1024 }), ..treq() };
        assert!(OpenAiCompat::request_body(&req).get("temperature").is_none());
    }

    #[test]
    fn forcing_the_channel_sends_required() {
        let b = OpenAiCompat::request_body(&ChatRequest { tool_choice: ToolChoice::Required, ..treq() });
        assert_eq!(b["tool_choice"], json!("required"));
    }

    /// `tool_choice` over an empty tool list is a `400` on every endpoint
    /// that reads the field at all.
    #[test]
    fn forcing_with_no_tools_sends_nothing() {
        let req = ChatRequest { tool_choice: ToolChoice::Required, tools: vec![], ..treq() };
        assert!(OpenAiCompat::request_body(&req).get("tool_choice").is_none());
    }

    #[test]
    fn reasoning_spellings_preserve_thinking_before_text_and_tools() {
        for key in ["reasoning_content", "reasoning"] {
            let mut m = ChunkMapper::default();
            let mut delta = json!({"content":"checking", "tool_calls":[
                {"index":0,"id":"c1","function":{"name":"read","arguments":"{}"}}
            ]});
            delta[key] = json!("look first");
            let mut all = m.map(&json!({"choices":[{"delta":delta}]}));
            all.extend(m.map(&json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]})));
            all.extend(m.finish());
            assert_eq!(all, vec![
                StreamEvent::ThinkingDelta("look first".into()),
                StreamEvent::TextDelta("checking".into()),
                StreamEvent::BlockStop,
                StreamEvent::ToolUseStart { id: "c1".into(), name: "read".into() },
                StreamEvent::ToolInputDelta("{}".into()),
                StreamEvent::BlockStop,
                StreamEvent::Stop { stop_reason: StopReason::ToolUse, usage: Usage::default() },
            ]);
            assert_eq!(m.finish(), None);
        }
    }

    #[test]
    fn reasoning_aliases_do_not_duplicate_or_hide_deltas() {
        for primary in [Value::Null, json!(""), json!(false), json!({})] {
            let mut m = ChunkMapper::default();
            assert_eq!(m.map(&json!({"choices":[{"delta":{
                "reasoning_content":primary,"reasoning":"thought"
            }}]})), vec![StreamEvent::ThinkingDelta("thought".into())]);
        }
        let mut m = ChunkMapper::default();
        assert_eq!(m.map(&json!({"choices":[{"delta":{
            "reasoning_content":"primary","reasoning":"alias"
        }}]})), vec![StreamEvent::ThinkingDelta("primary".into())]);
        for delta in [json!({}), json!({"reasoning":""}), json!({"reasoning":null}), json!({"reasoning":{}})] {
            assert!(m.map(&json!({"choices":[{"delta":delta}]})).is_empty());
        }
    }

    #[test]
    fn tool_call_chunks_open_blocks_and_stop_once_with_usage() {
        let mut m = ChunkMapper::default();
        let c1: Value = json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"read","arguments":""}}]}}]});
        let c2: Value = json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"p\":1}"}}]}}]});
        let c3: Value = json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]});
        let c4: Value = json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"prompt_tokens_details":{"cached_tokens":4}}});
        let mut all = m.map(&c1);
        all.extend(m.map(&c2));
        all.extend(m.map(&c3));
        all.extend(m.map(&c4));
        assert_eq!(m.finish(), None, "usage already closed the stream");
        assert_eq!(
            all,
            vec![
                StreamEvent::ToolUseStart { id: "c1".into(), name: "read".into() },
                StreamEvent::ToolInputDelta("{\"p\":1}".into()),
                StreamEvent::BlockStop,
                StreamEvent::Stop {
                    stop_reason: StopReason::ToolUse,
                    usage: Usage { input_tokens: 6, output_tokens: 5, cache_read_input_tokens: 4, cache_creation_input_tokens: 0 },
                },
            ]
        );
    }

    #[test]
    fn a_provider_without_usage_still_stops_exactly_once() {
        let mut m = ChunkMapper::default();
        let c1: Value = json!({"choices":[{"delta":{"content":"hi"}}]});
        let c2: Value = json!({"choices":[{"delta":{},"finish_reason":"stop"}]});
        let mut all = m.map(&c1);
        all.extend(m.map(&c2));
        assert_eq!(all, vec![StreamEvent::TextDelta("hi".into()), StreamEvent::BlockStop]);
        assert_eq!(m.finish(), Some(StreamEvent::Stop { stop_reason: StopReason::EndTurn, usage: Usage::default() }));
        assert_eq!(m.finish(), None);
    }

    /// DeepSeek's own shape: the hit count is a top-level field, and
    /// `prompt_tokens` is hits plus misses. Reading only OpenAI's
    /// `prompt_tokens_details` priced all 8000 of these as fresh.
    #[test]
    fn deepseeks_cache_hits_are_not_counted_as_fresh_input() {
        let u = json!({"prompt_tokens": 8192, "completion_tokens": 40, "prompt_cache_hit_tokens": 8000, "prompt_cache_miss_tokens": 192});
        assert_eq!(
            usage_from(&u),
            Usage { input_tokens: 192, output_tokens: 40, cache_read_input_tokens: 8000, cache_creation_input_tokens: 0 }
        );
    }

    /// Moonshot's placement, and OpenRouter's separate write count. A write
    /// is not subtracted from the read count — they are different tokens.
    #[test]
    fn the_other_two_placements_are_read_too() {
        let u = json!({"prompt_tokens": 500, "completion_tokens": 1, "cached_tokens": 400});
        assert_eq!(usage_from(&u).cache_read_input_tokens, 400);
        assert_eq!(usage_from(&u).input_tokens, 100);
        let u = json!({"prompt_tokens": 500, "completion_tokens": 1,
                       "prompt_tokens_details": {"cached_tokens": 300, "cache_write_tokens": 150}});
        assert_eq!(
            usage_from(&u),
            Usage { input_tokens: 50, output_tokens: 1, cache_read_input_tokens: 300, cache_creation_input_tokens: 150 }
        );
    }

    /// An endpoint that reports no cache counters at all is unchanged.
    #[test]
    fn an_uncached_endpoint_reports_plain_input() {
        let u = json!({"prompt_tokens": 120, "completion_tokens": 7});
        assert_eq!(usage_from(&u), Usage { input_tokens: 120, output_tokens: 7, ..Default::default() });
    }

    #[test]
    fn tool_results_become_role_tool_messages() {
        let m = Message::user(vec![
            ContentBlock::ToolResult { tool_use_id: "c1".into(), content: "out".into(), is_error: true },
            ContentBlock::text("and then"),
        ]);
        let w = message_to_wire(&m);
        assert_eq!(w[0]["role"], "tool");
        assert_eq!(w[0]["content"], "ERROR: out");
        assert_eq!(w[1]["role"], "user");
        let a = Message::assistant(vec![ContentBlock::ToolUse { id: "c1".into(), name: "read".into(), input: Json("{}".into()) }]);
        let w = message_to_wire(&a);
        assert_eq!(w[0]["content"], Value::Null);
        assert_eq!(w[0]["tool_calls"][0]["function"]["arguments"], "{}");
    }
}
