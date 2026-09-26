//! A `Provider` built from a [`ProviderDef`]: harnox's wire code plus the
//! things a definition can vary.

use std::sync::Arc;

use anyhow::Context as _;
use futures_util::StreamExt;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use harnox::llm::{
    AuthStyle, CacheRetention, ChatRequest, EventStream, Provider, StreamEvent, ToolDef, Usage,
    Wire,
};

use crate::def::ProviderDef;
use crate::script::ProviderScript;

pub struct HttpProvider {
    def: ProviderDef,
    script: Option<Arc<ProviderScript>>,
    store: Option<Arc<harnox::secrets::SecretStore>>,
    client: reqwest::Client,
    /// The Codex backend identifies a conversation by a `session_id` header;
    /// stable for the provider's lifetime, like the CLI's own.
    session_id: String,
}

impl HttpProvider {
    pub fn new(def: ProviderDef, script: Option<Arc<ProviderScript>>) -> Self {
        Self::with_store(def, script, None)
    }

    pub fn with_store(
        def: ProviderDef,
        script: Option<Arc<ProviderScript>>,
        store: Option<Arc<harnox::secrets::SecretStore>>,
    ) -> Self {
        let client = harnox::llm::http::streaming_client_builder()
            .build()
            .expect("reqwest client");
        HttpProvider {
            def,
            script,
            store,
            client,
            session_id: harnox::llm::codex::new_session_id(),
        }
    }

    pub fn def(&self) -> &ProviderDef {
        &self.def
    }

    /// Whether a model of this provider's can be shown an image, as its
    /// definition declares it. `None` where nothing says — including for an
    /// id this provider does not list, which happens whenever a bare id was
    /// resolved against a gateway's discovered models.
    pub fn vision_of(&self, model: &str) -> Option<bool> {
        self.def
            .models
            .iter()
            .find(|m| m.id == model)
            .and_then(|m| m.vision)
    }

    /// The temperature to sample this model at, as its definition declares
    /// it: the model's own, else the provider's, else nobody said.
    ///
    /// The provider-wide fallback is what makes this reach anything on a
    /// gateway. `models()` discovers ids the definition never lists — 65 of
    /// them on `fau` — and a per-model knob alone would have no row to sit
    /// on for any of them.
    pub fn temperature_of(&self, model: &str) -> Option<f64> {
        self.def
            .models
            .iter()
            .find(|m| m.id == model)
            .and_then(|m| m.temperature)
            .or(self.def.temperature)
    }

    /// The outgoing body: harnox's builder, then compat, then the script.
    ///
    /// Whether the model can see is answered *here*, and this is the only
    /// place that can answer it: the agent loop holds no catalog, so its
    /// request leaves core with `vision: None`, and the provider serving
    /// the model is the thing that knows. Filling it in per request rather
    /// than per session is also what makes a mid-session `:model` switch
    /// correct for free — the next request reads the new id.
    pub fn body(&self, req: &ChatRequest) -> anyhow::Result<Value> {
        // Two questions only this side can answer — whether the model can
        // see, and what to sample it at — and one clone between them. Every
        // other answer leaves the caller's request untouched, so the
        // ordinary turn does not copy its whole transcript to say nothing.
        let declared;
        let vision = match (req.vision, self.vision_of(&req.model)) {
            (None, Some(false)) => Some(Some(false)),
            _ => None,
        };
        let temperature = match req.temperature {
            None => self.temperature_of(&req.model).map(Some),
            Some(_) => None,
        };
        let req = match (vision, temperature) {
            (None, None) => req,
            (v, t) => {
                declared = ChatRequest {
                    vision: v.unwrap_or(req.vision),
                    temperature: t.unwrap_or(req.temperature),
                    ..req.clone()
                };
                &declared
            }
        };
        let mut body = match self.def.wire {
            Wire::Anthropic => harnox::llm::anthropic::Anthropic::request_body(req),
            Wire::OpenAi => harnox::llm::openai::OpenAiCompat::request_body(req),
            Wire::Gemini => harnox::llm::gemini::Gemini::request_body(req.clone(), self.def.project.clone()),
            Wire::Codex => harnox::llm::codex::Codex::request_body(req),
        };
        if self.def.wire == Wire::OpenAi {
            let c = &self.def.compat;
            if c.max_completion_tokens
                && let Some(v) = body.as_object_mut().and_then(|o| o.remove("max_tokens"))
            {
                body["max_completion_tokens"] = v;
            }
            if !c.stream_options
                && let Some(o) = body.as_object_mut()
            {
                o.remove("stream_options");
            }
            if c.reasoning_effort
                && req.thinking.is_some()
                && let Some(o) = body.as_object_mut()
            {
                o.insert("reasoning_effort".into(), Value::String("medium".into()));
            }
            if c.cache_control
                && let Some(mark) = cache_mark(req.cache.retention)
            {
                mark_prefix(&mut body, &mark);
            }
            if c.prompt_cache_key
                && !req.cache.retention.is_off()
                && let Some(key) = &req.cache.key
            {
                body["prompt_cache_key"] = Value::String(key.clone());
                if req.cache.retention == CacheRetention::Long {
                    body["prompt_cache_retention"] = Value::String("24h".into());
                }
            }
            if c.reasoning_content {
                send_reasoning(&mut body, &req.wire_messages());
            }
        }
        match &self.script {
            Some(s) => s.rewrite(body),
            None => Ok(body),
        }
    }

    fn url(&self) -> String {
        let base = self.def.base_url.trim_end_matches('/');
        match self.def.wire {
            Wire::Anthropic => format!("{base}/v1/messages"),
            Wire::OpenAi => format!("{base}/chat/completions"),
            Wire::Gemini => harnox::llm::gemini::Gemini::endpoint(base),
            Wire::Codex => harnox::llm::codex::Codex::endpoint(base),
        }
    }

    async fn open(
        &self,
        body: &Value,
        cancel: &CancellationToken,
    ) -> anyhow::Result<reqwest::Response> {
        let mut rb = self
            .client
            .post(self.url())
            .header("accept", "text/event-stream")
            .json(body);
        for (k, v) in &self.def.headers {
            rb = rb.header(k, v);
        }
        // The one place a credential touches a request.
        match (
            self.def.wire,
            self.def.auth,
            self.def.token(self.store.as_deref())?,
        ) {
            (Wire::Anthropic, AuthStyle::ApiKey, Some(t)) => {
                rb = rb
                    .header("x-api-key", t.as_str())
                    .header("anthropic-version", harnox::llm::anthropic::API_VERSION);
            }
            (Wire::Anthropic, AuthStyle::Bearer, Some(t)) => {
                rb = rb
                    .bearer_auth(t.as_str())
                    .header("anthropic-version", harnox::llm::anthropic::API_VERSION)
                    .header("anthropic-beta", "oauth-2025-04-20");
            }
            (Wire::Anthropic, _, None) => {
                rb = rb.header("anthropic-version", harnox::llm::anthropic::API_VERSION);
            }
            (Wire::OpenAi, _, Some(t)) => rb = rb.bearer_auth(t.as_str()),
            (Wire::OpenAi, _, None) => {}
            (Wire::Gemini, _, Some(key)) => {
                rb = rb.bearer_auth(key.as_str());
                if self.def.project.is_some() {
                    // The Code Assist surface gates on the stated client
                    // identity, not only the credential — see
                    // `gemini::code_assist_headers`.
                    for (k, v) in harnox::llm::gemini::code_assist_headers() {
                        rb = rb.header(k, v);
                    }
                }
            }
            (Wire::Gemini, _, None) => {}
            // The Codex backend expects the CLI's own client identity
            // beside the bearer — see `codex::client_headers`. A header the
            // definition names itself wins over the masquerade's default,
            // so an operator's override is sent once, not twice.
            (Wire::Codex, _, Some(t)) => {
                rb = rb.bearer_auth(t.as_str());
                let account = self
                    .def
                    .token_file
                    .as_deref()
                    .and_then(harnox::llm::codex::codex_account_id);
                for (k, v) in harnox::llm::codex::client_headers(account.as_deref(), &self.session_id) {
                    if !self.def.headers.contains_key(k) {
                        rb = rb.header(k, v);
                    }
                }
            }
            (Wire::Codex, _, None) => {
                for (k, v) in harnox::llm::codex::client_headers(None, &self.session_id) {
                    if !self.def.headers.contains_key(k) {
                        rb = rb.header(k, v);
                    }
                }
            }
        }
        let resp = tokio::select! {
            r = rb.send() => r.with_context(|| format!("POST {}", self.url()))?,
            _ = cancel.cancelled() => anyhow::bail!("cancelled"),
        };
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!(
                "{} returned HTTP {status}: {}",
                self.def.name,
                text.chars().take(2000).collect::<String>()
            );
        }
        Ok(resp)
    }
}

/// Put each assistant message's thinking back on it as `reasoning_content`
/// — see `Compat::reasoning_content` for who asks for it and why.
///
/// The body's assistant messages are walked beside the canonical ones:
/// harnox's `message_to_wire` yields exactly one assistant message per
/// canonical assistant message, while a user message may become several
/// (one `tool` message per result), which is why the walk is by role and
/// not by index. Several thinking blocks on one message are one string on
/// the wire, in order, since the field is a string. A message that thought
/// nothing gets no field rather than an empty one.
fn send_reasoning(body: &mut Value, messages: &[harnox::llm::message::Message]) {
    use harnox::llm::message::{ContentBlock, Role};
    let Some(wire) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    let mut canon = messages.iter().filter(|m| m.role == Role::Assistant);
    for m in wire.iter_mut().filter(|m| m["role"] == "assistant") {
        let Some(c) = canon.next() else { break };
        let thinking: String = c
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Thinking { thinking, .. } => Some(thinking.as_str()),
                _ => None,
            })
            .collect();
        if !thinking.is_empty() {
            m["reasoning_content"] = Value::String(thinking);
        }
    }
}

/// The Anthropic `cache_control` value a breakpoint carries on this wire,
/// or `None` when the request wants no caching.
fn cache_mark(retention: CacheRetention) -> Option<Value> {
    match retention {
        CacheRetention::None => None,
        CacheRetention::Short => Some(serde_json::json!({ "type": "ephemeral" })),
        CacheRetention::Long => Some(serde_json::json!({ "type": "ephemeral", "ttl": "1h" })),
    }
}

/// Mark a chat-completions body's stable prefix, the way a gateway that
/// forwards to Anthropic expects: the instruction message, the last tool,
/// and — rolling — the last message of the conversation.
///
/// The three places are the same three the Anthropic wire uses, for the
/// same reason: the header is stable and the conversation grows, so the
/// third breakpoint is what keeps a tool loop from re-reading the whole
/// transcript at the fresh rate on every iteration.
fn mark_prefix(body: &mut Value, mark: &Value) {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    if let Some(m) = messages
        .iter_mut()
        .find(|m| matches!(role(m), Some("system" | "developer")))
    {
        mark_text(m, mark);
    }
    // Walk back to the last message with text to hang it on: an assistant
    // turn that only called tools has `content: null` and cannot carry it.
    for m in messages.iter_mut().rev() {
        if matches!(role(m), Some("user" | "assistant" | "tool")) && mark_text(m, mark) {
            break;
        }
    }
    if let Some(last) = body
        .get_mut("tools")
        .and_then(Value::as_array_mut)
        .and_then(|t| t.last_mut())
    {
        last["cache_control"] = mark.clone();
    }
}

fn role(m: &Value) -> Option<&str> {
    m.get("role").and_then(Value::as_str)
}

/// Hang the mark on a message's last text part, promoting a plain string
/// to the one-part array the field needs. `false` when there was no text
/// to mark, so the caller can keep walking.
fn mark_text(m: &mut Value, mark: &Value) -> bool {
    match m.get_mut("content") {
        Some(Value::String(t)) if !t.is_empty() => {
            let text = std::mem::take(t);
            m["content"] = serde_json::json!([{ "type": "text", "text": text, "cache_control": mark.clone() }]);
            true
        }
        Some(Value::Array(parts)) => match parts
            .iter_mut()
            .rev()
            .find(|p| p.get("type").and_then(Value::as_str) == Some("text"))
        {
            Some(part) => {
                part["cache_control"] = mark.clone();
                true
            }
            None => false,
        },
        _ => false,
    }
}

/// Recovers tool calls that the endpoint left in the assistant's text.
///
/// Mistral-family models emit tool calls as `[TOOL_CALLS]name[ARGS]{json}`
/// in their own chat template. litellm's ollama path parses some of those
/// back into `delta.tool_calls` and lets the rest through as `delta.content`
/// — reliably the first one, when a turn makes several. The call is not
/// malformed, just delivered on the wrong channel, so it is recoverable
/// exactly.
///
/// Text is held back only while it could still turn out to be such a call:
/// a tool name has no spaces, so ordinary prose fails the test within a
/// token or two and streams through untouched. Held text that turns out to
/// be prose after all is flushed unchanged when the stream ends. This is why
/// the recovered call is not simply appended after the text — the raw
/// template would otherwise be journaled, replayed on every later request,
/// and read back by the model as though it were its own prose.
///
/// **A call that takes no arguments has no `[ARGS]` to find.** It arrives as
/// a bare name, which the shape test alone cannot tell from a one-word
/// reply, so that case is decided by the only thing that can decide it: the
/// names this very request offered. A buffer that is *entirely* the name of
/// an offered tool with no required parameters becomes that call with `{}`;
/// anything else stays prose. Both halves of the guard matter. Without the
/// exhaustive match a reply of "done" would be eaten, and without the
/// required-parameter test a bare `write` would become a `write` with no
/// path — a call that can only fail, invented out of a word.
///
/// The narrowness is affordable because of where this runs: only on an
/// endpoint whose definition already says `tool_calls_in_text`, i.e. one
/// known to put tool calls on this channel. Before this, no zero-argument
/// call could *ever* be recovered — `peers` and `sessions` were unreachable
/// through the text path by construction.
#[derive(Default)]
struct TextToolCalls {
    enabled: bool,
    buf: String,
    /// Set once the buffer cannot be a tool call; text then streams straight through.
    passthrough: bool,
    next_id: u64,
    /// Offered tools that would be valid with no arguments — the only names
    /// a bare word is allowed to become.
    nullary: Vec<String>,
}

/// What a partial buffer could still become.
#[derive(PartialEq, Debug)]
enum Shape {
    /// Not a call, and cannot become one.
    No,
    /// Could still become a call if more text arrives.
    Prefix,
    /// `name[ARGS]` has been seen; the JSON may still be incomplete.
    Call,
}

impl TextToolCalls {
    fn new(enabled: bool, tools: &[ToolDef]) -> Self {
        // A tool is callable bare when its schema demands nothing. An absent
        // or empty `required` both mean that; anything listed there makes a
        // bare name an invalid call rather than a recovered one.
        let nullary = tools
            .iter()
            .filter(|t| {
                t.input_schema
                    .get("required")
                    .and_then(Value::as_array)
                    .is_none_or(|r| r.is_empty())
            })
            .map(|t| t.name.clone())
            .collect();
        TextToolCalls {
            enabled,
            nullary,
            ..Default::default()
        }
    }

    /// The whole buffer as the name of an offered no-argument tool.
    ///
    /// Deliberately an equality test on the entire held text, not a search
    /// within it: a mention of a tool in a sentence is prose, and only a
    /// reply that is nothing but the name is the mangled call this recovers.
    fn nullary_call(&self, buf: &str) -> Option<String> {
        let t = buf.trim();
        let t = t.strip_prefix("[TOOL_CALLS]").unwrap_or(t).trim();
        self.nullary.iter().find(|n| n.as_str() == t).cloned()
    }

    /// One recovered call, as the three events a tool block is made of.
    fn emit(&mut self, name: String, args: String) -> Vec<StreamEvent> {
        self.next_id += 1;
        tracing::warn!(tool = %name, "recovered a tool call the endpoint delivered as text");
        vec![
            StreamEvent::ToolUseStart {
                id: format!("text_call_{}", self.next_id),
                name,
            },
            StreamEvent::ToolInputDelta(args),
            StreamEvent::BlockStop,
        ]
    }

    /// Handle one text delta, returning what should be yielded now.
    fn text(&mut self, delta: &str) -> Vec<StreamEvent> {
        if !self.enabled || self.passthrough {
            return vec![StreamEvent::TextDelta(delta.to_string())];
        }
        self.buf.push_str(delta);
        match shape(&self.buf) {
            Shape::No => {
                // Ordinary prose: release everything held and stop looking.
                self.passthrough = true;
                let out = std::mem::take(&mut self.buf);
                if out.is_empty() {
                    vec![]
                } else {
                    vec![StreamEvent::TextDelta(out)]
                }
            }
            Shape::Prefix | Shape::Call => vec![],
        }
    }

    /// Everything still held, as the events it turned out to be. Called
    /// before any non-text event and at end of stream.
    fn flush(&mut self) -> Vec<StreamEvent> {
        if self.buf.is_empty() {
            return vec![];
        }
        let buf = std::mem::take(&mut self.buf);
        let calls = extract_calls(&buf);
        if calls.is_empty() {
            // No `[ARGS]` anywhere. Either a no-argument call whose template
            // form is just the name, or genuine prose.
            return match self.nullary_call(&buf) {
                Some(name) => self.emit(name, "{}".into()),
                None => vec![StreamEvent::TextDelta(buf)],
            };
        }
        let mut out = Vec::new();
        for (name, args) in calls {
            out.extend(self.emit(name, args));
        }
        out
    }
}

/// Classify a partial buffer against `[TOOL_CALLS]?name[ARGS]…`.
fn shape(s: &str) -> Shape {
    let t = s.trim_start();
    if t.is_empty() {
        return Shape::Prefix;
    }
    const MARK: &str = "[TOOL_CALLS]";
    let rest = match t.strip_prefix(MARK) {
        Some(r) => r,
        // A partially-arrived marker is still a candidate.
        None if MARK.starts_with(t) => return Shape::Prefix,
        None => t,
    };
    let name_len = rest
        .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-'))
        .unwrap_or(rest.len());
    let after = &rest[name_len..];
    if after.is_empty() {
        // All name so far, nothing to disqualify it yet.
        return if name_len == 0 {
            Shape::No
        } else {
            Shape::Prefix
        };
    }
    if name_len == 0 {
        return Shape::No;
    }
    const ARGS: &str = "[ARGS]";
    if after.starts_with(ARGS) {
        Shape::Call
    } else if ARGS.starts_with(after) {
        Shape::Prefix
    } else {
        Shape::No
    }
}

/// Pull every `name[ARGS]{json}` out of a completed buffer.
fn extract_calls(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = s.trim_start();
    loop {
        rest = rest.trim_start();
        let r = rest.strip_prefix("[TOOL_CALLS]").unwrap_or(rest);
        let name_len = r
            .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-'))
            .unwrap_or(r.len());
        if name_len == 0 {
            break;
        }
        let (name, after) = r.split_at(name_len);
        let Some(json) = after.strip_prefix("[ARGS]") else {
            break;
        };
        // One complete JSON value; the remainder may hold the next call.
        let mut de = serde_json::Deserializer::from_str(json).into_iter::<Value>();
        let Some(Ok(v)) = de.next() else { break };
        out.push((name.to_string(), v.to_string()));
        rest = &json[de.byte_offset()..];
    }
    out
}

/// Re-index streamed tool calls so each distinct call gets its own block.
///
/// The OpenAI wire identifies a streamed tool call by its position in
/// `delta.tool_calls[].index`, and harnox's `ChunkMapper` opens one block
/// per index — correct for a well-behaved endpoint. Some gateways are not
/// well behaved: litellm's ollama path reports *every* call a Mistral-family
/// model makes at `index: 0`, changing only the `id` and `function.name`.
/// The mapper then sees one continuing block, so the second call is dropped
/// and its arguments are concatenated onto the first, producing a
/// `tool_use` whose input is two JSON objects run together.
///
/// This rewrites the index before the mapper sees it, keying a block on
/// `(index, id or name)` rather than `index` alone. Continuation deltas —
/// which carry neither id nor name — stay on whichever block that index
/// last opened, so an endpoint that streams correctly is unaffected.
#[derive(Default)]
struct ToolCallDemux {
    /// `(wire index, discriminator)` → the index we report.
    assigned: Vec<((u64, String), u64)>,
    /// Last index reported for a given wire index, for continuation deltas.
    last: Vec<(u64, u64)>,
    next: u64,
}

impl ToolCallDemux {
    /// Rewrite `chunk` in place. A chunk with no tool calls is untouched.
    fn rewrite(&mut self, chunk: &mut Value) {
        let Some(calls) = chunk
            .pointer_mut("/choices/0/delta/tool_calls")
            .and_then(Value::as_array_mut)
        else {
            return;
        };
        for c in calls.iter_mut() {
            let wire = c.get("index").and_then(Value::as_u64).unwrap_or(0);
            let disc = c
                .get("id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .or_else(|| {
                    c.pointer("/function/name")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                })
                .unwrap_or_default()
                .to_string();

            let out = if disc.is_empty() {
                // Pure argument continuation: whatever this wire index last opened.
                self.last
                    .iter()
                    .find(|(w, _)| *w == wire)
                    .map(|(_, i)| *i)
                    .unwrap_or(wire)
            } else if let Some((_, i)) = self
                .assigned
                .iter()
                .find(|((w, d), _)| *w == wire && *d == disc)
            {
                *i
            } else {
                let i = self.next;
                self.next += 1;
                self.assigned.push(((wire, disc), i));
                i
            };

            match self.last.iter_mut().find(|(w, _)| *w == wire) {
                Some(e) => e.1 = out,
                None => self.last.push((wire, out)),
            }
            c["index"] = Value::from(out);
        }
    }
}

/// Raw-wire tracing. Set `EIDOLON_WIRE_LOG=/path/to/file` to append every
/// SSE `data:` line, plus the outgoing request body, to that file. This is
/// the tool of first resort when a gateway speaks the wire imperfectly:
/// the harness's own view of a stream is already an interpretation, and
/// the interpretation is usually what is in doubt.
fn wire_log(tag: &str, text: &str) {
    use std::io::Write as _;
    let Some(path) = std::env::var_os("EIDOLON_WIRE_LOG") else {
        return;
    };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "[{tag}] {text}");
    }
}

impl Provider for HttpProvider {
    fn name(&self) -> &str {
        &self.def.name
    }

    fn stream<'a>(&'a self, req: ChatRequest, cancel: CancellationToken) -> EventStream<'a> {
        Box::pin(async_stream::try_stream! {
            let body = self.body(&req)?;
            wire_log("request", &body.to_string());
            let resp = self.open(&body, &cancel).await?;
            match self.def.wire {
                Wire::Anthropic => {
                    let mut events = std::pin::pin!(harnox::llm::sse::events(resp.bytes_stream()));
                    loop {
                        let next = tokio::select! {
                            n = events.next() => n,
                            _ = cancel.cancelled() => break,
                        };
                        let Some(ev) = next else { break };
                        let ev = ev?;
                        wire_log("sse", &ev.data);
                        if ev.data.is_empty() { continue; }
                        let data: Value = serde_json::from_str(&ev.data)?;
                        for out in harnox::llm::anthropic::map_event(ev.event.as_deref(), &data)? {
                            yield out;
                        }
                    }
                }
                Wire::OpenAi => {
                    let mut events = std::pin::pin!(harnox::llm::sse::events(resp.bytes_stream()));
                    yield StreamEvent::Start { usage: Usage::default() };
                    let mut mapper = harnox::llm::openai::ChunkMapper::default();
                    let mut demux = ToolCallDemux::default();
                    let mut recover = TextToolCalls::new(self.def.compat.tool_calls_in_text, &req.tools);
                    loop {
                        let next = tokio::select! {
                            n = events.next() => n,
                            _ = cancel.cancelled() => break,
                        };
                        let Some(ev) = next else { break };
                        let ev = ev?;
                        wire_log("sse", &ev.data);
                        if ev.data.trim() == "[DONE]" { break; }
                        if ev.data.is_empty() { continue; }
                        let mut chunk: Value = serde_json::from_str(&ev.data)?;
                        if let Some(err) = chunk.get("error") {
                            Err(anyhow::anyhow!("{}: {}", self.def.name, err))?;
                        }
                        demux.rewrite(&mut chunk);
                        for out in mapper.map(&chunk) {
                            match out {
                                StreamEvent::TextDelta(t) => {
                                    for e in recover.text(&t) { yield e; }
                                }
                                other => {
                                    // Held text resolves before anything that
                                    // closes or follows it, so block order and
                                    // the single terminal `Stop` are preserved.
                                    for e in recover.flush() { yield e; }
                                    yield other;
                                }
                            }
                        }
                    }
                    for e in recover.flush() { yield e; }
                    if let Some(stop) = mapper.finish() {
                        yield stop;
                    }
                }
                Wire::Codex => {
                    let mut events = std::pin::pin!(harnox::llm::sse::events(resp.bytes_stream()));
                    let mut mapper = harnox::llm::codex::Mapper::default();
                    loop {
                        let next = tokio::select! {
                            n = events.next() => n,
                            _ = cancel.cancelled() => break,
                        };
                        let Some(ev) = next else { break };
                        let ev = ev?;
                        wire_log("sse", &ev.data);
                        if ev.data.trim() == "[DONE]" { break; }
                        if ev.data.is_empty() { continue; }
                        let data: Value = serde_json::from_str(&ev.data)?;
                        let ty = ev
                            .event
                            .as_deref()
                            .or_else(|| data.get("type").and_then(Value::as_str))
                            .unwrap_or("");
                        for out in mapper.map(ty, &data)? {
                            yield out;
                        }
                    }
                }
                Wire::Gemini => {
                    use futures_util::StreamExt;
                    let mut events = std::pin::pin!(harnox::llm::gemini::Gemini::stream_events(resp.bytes_stream()));
                    loop {
                        let next = tokio::select! {
                            n = events.next() => n,
                            _ = cancel.cancelled() => break,
                        };
                        let Some(ev) = next else { break };
                        match ev? {
                            harnox::llm::gemini::Event::Delta(d) => yield StreamEvent::TextDelta(d),
                            harnox::llm::gemini::Event::ThinkingDelta(d) => yield StreamEvent::ThinkingDelta(d),
                            harnox::llm::gemini::Event::ThinkingSignature(s) => yield StreamEvent::ThinkingSignature(s),
                            harnox::llm::gemini::Event::ToolUseStart { id, name } => yield StreamEvent::ToolUseStart { id, name },
                            harnox::llm::gemini::Event::ToolInputDelta(d) => yield StreamEvent::ToolInputDelta(d),
                            harnox::llm::gemini::Event::BlockStop => yield StreamEvent::BlockStop,
                            harnox::llm::gemini::Event::Stop { stop_reason, usage } => yield StreamEvent::Stop { stop_reason, usage },
                            harnox::llm::gemini::Event::Start { usage } => yield StreamEvent::Start { usage },
                        }
                    }
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::def::Compat;
    use harnox::llm::Message;

    #[test]
    fn ollama_cloud_uses_anthropic_and_replays_unsigned_thinking() {
        let (def, script) = ProviderScript::load("ollama", include_str!("../builtin/ollama.rn")).unwrap();
        assert_eq!(def.wire, Wire::Anthropic);
        assert_eq!(def.auth, AuthStyle::Bearer);
        assert_eq!(def.token_secret.as_deref(), Some("ollama"));
        assert_eq!(def.token_env.as_deref(), Some("OLLAMA_API_KEY"));
        let p = HttpProvider::new(def, Some(script));
        assert_eq!(p.url(), "https://ollama.com/v1/messages");
        let body = p.body(&req(thoughtful(), Default::default())).unwrap();
        let blocks = &body["messages"][1]["content"];
        assert_eq!(blocks[0], serde_json::json!({"type":"thinking","thinking":"look first","signature":""}));
        assert_eq!(blocks[1]["type"], "tool_use");
        assert_eq!(blocks[1]["input"], serde_json::json!({}));
        assert_eq!(body["messages"][2]["content"][0]["type"], "tool_result");
        assert!(body.get("stream_options").is_none());
    }

    #[test]
    fn a_tool_call_delivered_as_text_is_recovered() {
        let mut r = TextToolCalls::new(true, &[]);
        // Streamed the way litellm actually chunks it.
        let mut got = Vec::new();
        for d in [
            "write[ARGS]{\"path\": ",
            "\"index.html\", ",
            "\"content\": ",
            "\"<h1>hi</h1>\"}",
        ] {
            got.extend(r.text(d));
        }
        assert!(
            got.is_empty(),
            "nothing should stream while it might be a call: {got:?}"
        );
        let out = r.flush();
        assert_eq!(
            out,
            vec![
                StreamEvent::ToolUseStart {
                    id: "text_call_1".into(),
                    name: "write".into()
                },
                StreamEvent::ToolInputDelta(
                    r#"{"content":"<h1>hi</h1>","path":"index.html"}"#.into()
                ),
                StreamEvent::BlockStop,
            ]
        );
    }

    #[test]
    fn the_tool_calls_marker_is_accepted() {
        let mut r = TextToolCalls::new(true, &[]);
        r.text("[TOOL_CALLS]read[ARGS]{\"path\":\"a\"}");
        let out = r.flush();
        assert!(
            matches!(&out[0], StreamEvent::ToolUseStart { name, .. } if name == "read"),
            "{out:?}"
        );
    }

    #[test]
    fn several_calls_in_one_blob_all_come_back() {
        let mut r = TextToolCalls::new(true, &[]);
        r.text("write[ARGS]{\"path\":\"a\"}write[ARGS]{\"path\":\"b\"}");
        let out = r.flush();
        assert_eq!(
            out.iter()
                .filter(|e| matches!(e, StreamEvent::ToolUseStart { .. }))
                .count(),
            2,
            "{out:?}"
        );
    }

    #[test]
    fn ordinary_prose_streams_through_promptly() {
        let mut r = TextToolCalls::new(true, &[]);
        // Held while it is still a bare identifier, released as soon as a
        // space proves it is prose — one token of latency, not a whole turn.
        assert!(r.text("Sure").is_empty());
        assert_eq!(
            r.text(", I will do that."),
            vec![StreamEvent::TextDelta("Sure, I will do that.".into())]
        );
        assert_eq!(
            r.text(" More."),
            vec![StreamEvent::TextDelta(" More.".into())]
        );
        assert!(r.flush().is_empty());
    }

    #[test]
    fn a_bare_word_reply_is_not_swallowed() {
        let mut r = TextToolCalls::new(true, &[]);
        assert!(r.text("done").is_empty());
        assert_eq!(r.flush(), vec![StreamEvent::TextDelta("done".into())]);
    }

    /// A tool as the model is offered it. `required` is the field the bare-name
    /// recovery turns on, so it is the only one these tests vary.
    fn tool(name: &str, required: &[&str]) -> ToolDef {
        ToolDef {
            name: name.into(),
            description: String::new(),
            input_schema: serde_json::json!({ "type": "object", "properties": {}, "required": required }),
        }
    }

    /// The `peers` case. A call taking no arguments has no `[ARGS]` section,
    /// so it reaches us as a bare name and used to flush as prose — which is
    /// how "How many other sessions are there" got the one-word reply `peers`
    /// instead of a listing (session 1788581044652, 2026-09-05).
    #[test]
    fn a_no_argument_call_delivered_as_a_bare_name_is_recovered() {
        let mut r = TextToolCalls::new(true, &[tool("peers", &[])]);
        assert!(
            r.text("peers").is_empty(),
            "a bare name is still a candidate"
        );
        assert_eq!(
            r.flush(),
            vec![
                StreamEvent::ToolUseStart {
                    id: "text_call_1".into(),
                    name: "peers".into()
                },
                StreamEvent::ToolInputDelta("{}".into()),
                StreamEvent::BlockStop,
            ]
        );
    }

    /// The same, wearing the template's marker.
    #[test]
    fn a_marked_no_argument_call_is_recovered() {
        let mut r = TextToolCalls::new(true, &[tool("peers", &[])]);
        r.text("[TOOL_CALLS]peers");
        let out = r.flush();
        assert!(
            matches!(&out[0], StreamEvent::ToolUseStart { name, .. } if name == "peers"),
            "{out:?}"
        );
    }

    /// A tool that demands arguments is never conjured out of its own name:
    /// `write` with no path is a call that can only fail, and the word was
    /// far likelier to be prose.
    #[test]
    fn a_bare_name_of_a_tool_that_needs_arguments_stays_prose() {
        let mut r = TextToolCalls::new(true, &[tool("write", &["path", "content"])]);
        r.text("write");
        assert_eq!(r.flush(), vec![StreamEvent::TextDelta("write".into())]);
    }

    /// Only the tools *this request* offered. A name the model invented, or
    /// one from a registry it is not being given, is prose.
    #[test]
    fn a_bare_word_that_is_not_an_offered_tool_stays_prose() {
        let mut r = TextToolCalls::new(true, &[tool("peers", &[])]);
        r.text("done");
        assert_eq!(r.flush(), vec![StreamEvent::TextDelta("done".into())]);
    }

    /// The match is on the whole buffer, so a tool named in a sentence is
    /// still a sentence.
    #[test]
    fn a_tool_named_inside_prose_is_not_a_call() {
        let mut r = TextToolCalls::new(true, &[tool("peers", &[])]);
        let mut got = Vec::new();
        for d in ["peers", " lists the other sessions."] {
            got.extend(r.text(d));
        }
        got.extend(r.flush());
        assert_eq!(
            got,
            vec![StreamEvent::TextDelta(
                "peers lists the other sessions.".into()
            )]
        );
    }

    /// A schema with no `required` key at all is the ordinary way to write
    /// "takes nothing", and `peers` writes it that way.
    #[test]
    fn an_absent_required_key_counts_as_no_arguments() {
        let schema = serde_json::json!({ "type": "object", "properties": {} });
        let t = ToolDef {
            name: "peers".into(),
            description: String::new(),
            input_schema: schema,
        };
        let mut r = TextToolCalls::new(true, &[t]);
        r.text("peers");
        assert!(matches!(&r.flush()[0], StreamEvent::ToolUseStart { .. }));
    }

    /// The recovery is the broken gateway's alone: with the flag off, a bare
    /// name is text like anything else.
    #[test]
    fn the_flag_off_leaves_a_bare_tool_name_as_text() {
        let mut r = TextToolCalls::new(false, &[tool("peers", &[])]);
        assert_eq!(
            r.text("peers"),
            vec![StreamEvent::TextDelta("peers".into())]
        );
        assert!(r.flush().is_empty());
    }

    #[test]
    fn the_flag_off_means_no_buffering_at_all() {
        let mut r = TextToolCalls::new(false, &[]);
        assert_eq!(
            r.text("write[ARGS]{\"path\":\"a\"}"),
            vec![StreamEvent::TextDelta("write[ARGS]{\"path\":\"a\"}".into())]
        );
        assert!(r.flush().is_empty());
    }

    #[test]
    fn prose_that_mentions_the_template_is_not_eaten() {
        let mut r = TextToolCalls::new(true, &[]);
        let mut got = Vec::new();
        for d in ["The ", "model emits write[ARGS]{\"x\":1} as text."] {
            got.extend(r.text(d));
        }
        got.extend(r.flush());
        let text: String = got
            .iter()
            .map(|e| match e {
                StreamEvent::TextDelta(t) => t.clone(),
                other => panic!("expected only text, got {other:?}"),
            })
            .collect();
        assert_eq!(text, "The model emits write[ARGS]{\"x\":1} as text.");
    }

    #[test]
    fn shape_classifies_partial_buffers() {
        assert_eq!(shape(""), Shape::Prefix);
        assert_eq!(shape("wri"), Shape::Prefix);
        assert_eq!(shape("write["), Shape::Prefix);
        assert_eq!(shape("write[ARGS]"), Shape::Call);
        assert_eq!(shape("[TOOL"), Shape::Prefix);
        assert_eq!(shape("Sure, "), Shape::No);
        assert_eq!(shape("write{"), Shape::No);
        assert_eq!(shape("[ARGS]{}"), Shape::No);
    }

    /// The exact shape litellm produced for ministral-3:14b: two distinct
    /// calls, both announced at `index: 0`.
    #[test]
    fn collapsed_tool_calls_are_split_onto_separate_blocks() {
        let mut d = ToolCallDemux::default();
        let mut a = serde_json::json!({"choices":[{"delta":{"tool_calls":[
            {"index":0,"id":"A","type":"function","function":{"name":"write","arguments":"{\"path\":\"a\"}"}}]}}]});
        let mut b = serde_json::json!({"choices":[{"delta":{"tool_calls":[
            {"index":0,"id":"B","type":"function","function":{"name":"read","arguments":"{\"path\":\"a\"}"}}]}}]});
        d.rewrite(&mut a);
        d.rewrite(&mut b);
        assert_eq!(a.pointer("/choices/0/delta/tool_calls/0/index").unwrap(), 0);
        assert_eq!(b.pointer("/choices/0/delta/tool_calls/0/index").unwrap(), 1);
    }

    #[test]
    fn a_well_behaved_stream_is_unchanged() {
        let mut d = ToolCallDemux::default();
        // Open index 0, then stream argument fragments with no id or name,
        // then open a genuine second call at index 1.
        let mut open = serde_json::json!({"choices":[{"delta":{"tool_calls":[
            {"index":0,"id":"A","function":{"name":"read","arguments":""}}]}}]});
        let mut frag = serde_json::json!({"choices":[{"delta":{"tool_calls":[
            {"index":0,"function":{"arguments":"{\"path\":"}}]}}]});
        let mut frag2 = serde_json::json!({"choices":[{"delta":{"tool_calls":[
            {"index":0,"function":{"arguments":"\"a\"}"}}]}}]});
        let mut second = serde_json::json!({"choices":[{"delta":{"tool_calls":[
            {"index":1,"id":"B","function":{"name":"grep","arguments":"{}"}}]}}]});
        for c in [&mut open, &mut frag, &mut frag2, &mut second] {
            d.rewrite(c);
        }
        assert_eq!(
            open.pointer("/choices/0/delta/tool_calls/0/index").unwrap(),
            0
        );
        assert_eq!(
            frag.pointer("/choices/0/delta/tool_calls/0/index").unwrap(),
            0
        );
        assert_eq!(
            frag2
                .pointer("/choices/0/delta/tool_calls/0/index")
                .unwrap(),
            0
        );
        assert_eq!(
            second
                .pointer("/choices/0/delta/tool_calls/0/index")
                .unwrap(),
            1
        );
    }

    #[test]
    fn repeating_the_same_id_keeps_one_block() {
        let mut d = ToolCallDemux::default();
        for _ in 0..3 {
            let mut c = serde_json::json!({"choices":[{"delta":{"tool_calls":[
                {"index":0,"id":"A","function":{"name":"read","arguments":"x"}}]}}]});
            d.rewrite(&mut c);
            assert_eq!(c.pointer("/choices/0/delta/tool_calls/0/index").unwrap(), 0);
        }
    }

    #[test]
    fn chunks_without_tool_calls_are_left_alone() {
        let mut d = ToolCallDemux::default();
        let mut c = serde_json::json!({"choices":[{"delta":{"content":"hi"}}]});
        let before = c.clone();
        d.rewrite(&mut c);
        assert_eq!(c, before);
    }

    fn openai(compat: Compat) -> HttpProvider {
        HttpProvider::new(
            ProviderDef {
                name: "t".into(),
                wire: Wire::OpenAi,
                base_url: "http://x".into(),
                compat,
                ..Default::default()
            },
            None,
        )
    }

    fn conversation() -> Vec<Message> {
        use harnox::llm::{ContentBlock, Json};
        vec![
            Message::user_text("hi"),
            Message::assistant(vec![ContentBlock::ToolUse {
                id: "c1".into(),
                name: "read".into(),
                input: Json("{}".into()),
            }]),
            Message::user(vec![ContentBlock::ToolResult {
                tool_use_id: "c1".into(),
                content: "out".into(),
                is_error: false,
            }]),
        ]
    }

    fn req(messages: Vec<Message>, cache: harnox::llm::CacheOptions) -> ChatRequest {
        ChatRequest {
            model: "m".into(),
            system: Some("be brief".into()),
            messages,
            tools: vec![
                harnox::llm::ToolDef {
                    name: "a".into(),
                    description: "A".into(),
                    input_schema: serde_json::json!({}),
                },
                harnox::llm::ToolDef {
                    name: "b".into(),
                    description: "B".into(),
                    input_schema: serde_json::json!({}),
                },
            ],
            max_tokens: 5,
            thinking: None,
            cache,
            vision: None,
            temperature: None,
            tool_choice: harnox::llm::ToolChoice::Auto,
        }
    }

    /// A gateway that forwards to Anthropic over this wire caches nothing
    /// unless the request says so, so the same three breakpoints go on by
    /// declaration: the instruction message, the last tool, and — rolling
    /// — the end of the conversation.
    #[test]
    fn a_gateway_that_takes_cache_control_gets_the_same_three_breakpoints() {
        let p = openai(Compat {
            cache_control: true,
            ..Default::default()
        });
        let b = p.body(&req(conversation(), Default::default())).unwrap();
        assert_eq!(
            b["messages"][0]["content"][0]["cache_control"]["type"],
            "ephemeral"
        );
        assert_eq!(b["messages"][0]["content"][0]["text"], "be brief");
        assert_eq!(b["tools"][1]["cache_control"]["type"], "ephemeral");
        assert!(b["tools"][0].get("cache_control").is_none());
        // The tool result is the last message; the `tool_calls`-only
        // assistant turn before it has no text to hang a mark on.
        let last = b["messages"].as_array().unwrap().last().unwrap();
        assert_eq!(last["role"], "tool");
        assert_eq!(last["content"][0]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn long_retention_reaches_both_wires_dialects() {
        let cache = harnox::llm::CacheOptions {
            retention: CacheRetention::Long,
            key: Some("s1".into()),
        };
        let p = openai(Compat {
            cache_control: true,
            prompt_cache_key: true,
            ..Default::default()
        });
        let b = p.body(&req(conversation(), cache)).unwrap();
        assert_eq!(b["messages"][0]["content"][0]["cache_control"]["ttl"], "1h");
        assert_eq!(b["prompt_cache_key"], "s1");
        assert_eq!(b["prompt_cache_retention"], "24h");
    }

    /// Both flags are claims about one endpoint, so an ordinary
    /// OpenAI-compatible body is untouched by either.
    #[test]
    fn neither_field_appears_unasked() {
        let cache = harnox::llm::CacheOptions {
            retention: CacheRetention::Long,
            key: Some("s1".into()),
        };
        let b = openai(Compat::default())
            .body(&req(conversation(), cache))
            .unwrap();
        assert_eq!(b["messages"][0]["content"], "be brief");
        assert!(b.get("prompt_cache_key").is_none());
        assert!(b.get("prompt_cache_retention").is_none());
        assert!(b["tools"][1].get("cache_control").is_none());
    }

    /// A conversation the way a thinking model on the chat-completions
    /// wire leaves it on the log: reasoning before the call, none before
    /// the plain answer, two blocks before the last.
    fn thoughtful() -> Vec<Message> {
        use harnox::llm::{ContentBlock, Json};
        vec![
            Message::user_text("hi"),
            Message::assistant(vec![
                ContentBlock::Thinking {
                    thinking: "look first".into(),
                    signature: String::new(),
                },
                ContentBlock::ToolUse {
                    id: "c1".into(),
                    name: "read".into(),
                    input: Json("{}".into()),
                },
            ]),
            Message::user(vec![ContentBlock::ToolResult {
                tool_use_id: "c1".into(),
                content: "out".into(),
                is_error: false,
            }]),
            Message::assistant(vec![ContentBlock::text("done")]),
            Message::user_text("and?"),
            Message::assistant(vec![
                ContentBlock::Thinking {
                    thinking: "so ".into(),
                    signature: String::new(),
                },
                ContentBlock::Thinking {
                    thinking: "then".into(),
                    signature: String::new(),
                },
                ContentBlock::text("more"),
            ]),
        ]
    }

    /// z.ai and DeepSeek want the reasoning back on every assistant
    /// message, tool results between them or not: the walk is by role, so
    /// the `tool` message a result became does not shift it onto the
    /// wrong message.
    #[test]
    fn reasoning_goes_back_on_the_message_that_thought_it() {
        let p = openai(Compat {
            reasoning_content: true,
            ..Default::default()
        });
        let b = p.body(&req(thoughtful(), Default::default())).unwrap();
        let m = b["messages"].as_array().unwrap();
        // system, user, assistant (the call), tool, assistant, user, assistant.
        assert_eq!(m[2]["role"], "assistant");
        assert_eq!(m[2]["reasoning_content"], "look first");
        assert_eq!(m[3]["role"], "tool");
        assert_eq!(m[4]["role"], "assistant");
        assert!(
            m[4].get("reasoning_content").is_none(),
            "nothing thought, nothing sent"
        );
        assert_eq!(m[6]["reasoning_content"], "so then");
    }

    /// Off by default: OpenAI's own endpoint has no such field.
    #[test]
    fn reasoning_stays_home_unasked() {
        let b = openai(Compat::default())
            .body(&req(thoughtful(), Default::default()))
            .unwrap();
        assert!(
            b["messages"]
                .as_array()
                .unwrap()
                .iter()
                .all(|m| m.get("reasoning_content").is_none())
        );
    }

    /// Retention `none` means no breakpoints and no key, whatever the
    /// definition declares — the request said not to cache.
    #[test]
    fn retention_none_overrides_the_flags() {
        let p = openai(Compat {
            cache_control: true,
            prompt_cache_key: true,
            ..Default::default()
        });
        let b = p
            .body(&req(conversation(), harnox::llm::CacheOptions::off()))
            .unwrap();
        assert_eq!(b["messages"][0]["content"], "be brief");
        assert!(b.get("prompt_cache_key").is_none());
    }

    fn with_models(models: Vec<crate::def::ModelDef>) -> HttpProvider {
        let def = ProviderDef {
            name: "t".into(),
            wire: Wire::OpenAi,
            base_url: "http://x".into(),
            token_file: None,
            token_env: None,
            token: None,
            token_secret: None,
            token_secrets: vec![],
            auth: AuthStyle::default(),
            project: None,
            headers: Default::default(),
            compat: Default::default(),
            models,
            model_globs: vec![],
            temperature: None,
        };
        HttpProvider::new(def, None)
    }

    fn model_at(id: &str, temperature: Option<f64>) -> crate::def::ModelDef {
        crate::def::ModelDef {
            id: id.into(),
            name: None,
            context: None,
            reasoning: false,
            vision: None,
            temperature,
            cost: None,
        }
    }

    /// A model's own temperature reaches the body, filled in here because
    /// the agent loop holds no catalog and leaves the field unanswered.
    #[test]
    fn a_models_temperature_is_filled_in_from_its_definition() {
        let p = with_models(vec![model_at("m", Some(0.2))]);
        let b = p.body(&req(conversation(), Default::default())).unwrap();
        assert_eq!(b["temperature"], serde_json::json!(0.2));
    }

    /// A gateway lists most of what it serves without a `[[models]]` row, so
    /// the provider-wide value is the one that actually reaches them.
    #[test]
    fn a_discovered_model_takes_the_providers_temperature() {
        let mut p = with_models(vec![]);
        p.def.temperature = Some(0.3);
        let b = p.body(&req(conversation(), Default::default())).unwrap();
        assert_eq!(b["temperature"], serde_json::json!(0.3));
    }

    /// The row wins over the provider's default.
    #[test]
    fn a_models_own_temperature_beats_the_providers() {
        let mut p = with_models(vec![model_at("m", Some(0.1))]);
        p.def.temperature = Some(0.9);
        let b = p.body(&req(conversation(), Default::default())).unwrap();
        assert_eq!(b["temperature"], serde_json::json!(0.1));
    }

    /// Nobody said, so nothing is sent and the endpoint's own default stands
    /// — which is what every request did before the knob existed.
    #[test]
    fn no_declared_temperature_sends_no_field() {
        let p = with_models(vec![model_at("m", None)]);
        let b = p.body(&req(conversation(), Default::default())).unwrap();
        assert!(b.get("temperature").is_none());
    }

    /// A caller that has already answered is not overridden — the fill-in is
    /// for the `None` that means nobody said.
    #[test]
    fn an_explicit_request_temperature_is_left_alone() {
        let mut p = with_models(vec![model_at("m", Some(0.2))]);
        p.def.temperature = Some(0.9);
        let r = ChatRequest {
            temperature: Some(0.7),
            ..req(conversation(), Default::default())
        };
        assert_eq!(p.body(&r).unwrap()["temperature"], serde_json::json!(0.7));
    }

    fn model(id: &str, vision: Option<bool>) -> crate::def::ModelDef {
        crate::def::ModelDef {
            id: id.into(),
            name: None,
            context: None,
            reasoning: false,
            vision,
            temperature: None,
            cost: None,
        }
    }

    fn with_image() -> Vec<Message> {
        use harnox::llm::ContentBlock;
        vec![Message::user(vec![
            ContentBlock::text("what is this?"),
            ContentBlock::image("image/png", "AAAA", Some("shot.png".into())),
        ])]
    }

    /// A model whose definition says it cannot see is not sent the image.
    /// The endpoint would reject the whole request, and a rejected request
    /// is not one bad turn — it is every turn after it on that branch,
    /// because the image stays on the transcript.
    #[test]
    fn a_declared_blind_model_is_told_about_the_image_instead_of_shown_it() {
        let p = with_models(vec![model("m", Some(false))]);
        let b = p.body(&req(with_image(), Default::default())).unwrap();
        let content = &b["messages"][1]["content"];
        // Degraded to a string message, not a parts array.
        let text = content.as_str().expect("degraded content is plain text");
        assert!(text.contains("what is this?"), "{text}");
        assert!(text.contains("shot.png"), "{text}");
        assert!(!b.to_string().contains("image_url"));
    }

    /// Nothing declared is *not* a declaration of blindness. A gateway's
    /// discovered model list carries no such flag and most of what is
    /// behind one can see, so the image goes; an endpoint that cannot read
    /// it says so in an error, which is a thing the operator can act on.
    /// Silently dropping it is not — it looks exactly like a model that
    /// looked at the screenshot and ignored it.
    #[test]
    fn an_undeclared_model_is_still_shown_the_image() {
        for p in [with_models(vec![model("m", None)]), with_models(vec![])] {
            let b = p.body(&req(with_image(), Default::default())).unwrap();
            let parts = b["messages"][1]["content"]
                .as_array()
                .expect("content is a parts array");
            assert_eq!(parts[0]["type"], "text");
            assert_eq!(parts[1]["type"], "image_url");
            assert_eq!(parts[1]["image_url"]["url"], "data:image/png;base64,AAAA");
        }
    }

    /// A model that *is* declared sighted takes the same path as one that
    /// says nothing — the flag exists to withhold, never to permit.
    #[test]
    fn a_declared_sighted_model_is_shown_the_image() {
        let p = with_models(vec![model("m", Some(true))]);
        let b = p.body(&req(with_image(), Default::default())).unwrap();
        assert_eq!(b["messages"][1]["content"][1]["type"], "image_url");
    }

    /// An image-free turn keeps the plain-string content it has always
    /// sent. This wire's prompt cache is a prefix cache, so restating the
    /// same words in the richer parts shape would miss on every message of
    /// every session that never attaches anything.
    #[test]
    fn a_turn_with_no_image_still_sends_a_plain_string() {
        let p = with_models(vec![model("m", None)]);
        let b = p.body(&req(conversation(), Default::default())).unwrap();
        assert!(b["messages"][1]["content"].is_string());
    }

    #[test]
    fn compat_renames_max_tokens() {
        let def = ProviderDef {
            name: "t".into(),
            wire: Wire::OpenAi,
            base_url: "http://x".into(),
            token_file: None,
            token_env: None,
            token: None,
            token_secret: None,
            token_secrets: vec![],
            auth: AuthStyle::default(),
            project: None,
            headers: Default::default(),
            compat: Compat {
                max_completion_tokens: true,
                stream_options: false,
                ..Default::default()
            },
            models: vec![],
            model_globs: vec![],
            temperature: None,
        };
        let p = HttpProvider::new(def, None);
        let req = ChatRequest {
            model: "m".into(),
            system: None,
            messages: vec![Message::user_text("hi")],
            tools: vec![],
            max_tokens: 5,
            thinking: None,
            cache: Default::default(),
            vision: None,
            temperature: None,
            tool_choice: harnox::llm::ToolChoice::Auto,
        };
        let b = p.body(&req).unwrap();
        assert_eq!(b["max_completion_tokens"], 5);
        assert!(b.get("max_tokens").is_none());
        assert!(b.get("stream_options").is_none());
    }
}
