//! Turn the CLI's `stream-json` lines into journal records and bus events.

use serde_json::Value;

use eidolon_core::UsageExt;
use eidolon_core::agent::{Agent, TurnOutcome};
use eidolon_core::event::Event;
use eidolon_core::message::{ContentBlock, Json, Message, StopReason, Usage};
use eidolon_core::session::RecordKind;
use eidolon_core::tool::{CallOrigin, ToolCall, ToolOutput};

pub struct Sink<'a> {
    agent: &'a Agent,
    known_session: Option<String>,
    /// Blocks of the assistant message being assembled, keyed by message id.
    current: Option<(String, Vec<ContentBlock>)>,
    /// tool_use id → (name, input) for pairing results with calls.
    calls: std::collections::HashMap<String, (String, Value)>,
    result: Option<(StopReason, Usage, bool, String)>,
    /// The input of the most recent model call, for `RecordKind::ContextSize`.
    ///
    /// The `result` event's usage is the whole invocation's total — the CLI
    /// runs its own tool loop, so that is every call it made summed, and it
    /// grows with the length of the turn rather than the size of the
    /// context. Each `assistant` event carries the usage of *one* call, and
    /// the last of them is the only one that saw the whole transcript.
    last_input: Option<u64>,
    started: bool,
    /// Whether the CLI reported a session for this turn. Until it does,
    /// nothing we sent is in any session of its own.
    saw_init: bool,
    /// Model calls the CLI made this turn: one per assistant message it
    /// reported, which is what `TurnOutcome::Settled::calls` counts on the
    /// provider loop too.
    model_calls: u32,
}

impl<'a> Sink<'a> {
    pub fn new(agent: &'a Agent, known_session: Option<String>) -> Self {
        Sink {
            agent,
            known_session,
            current: None,
            calls: Default::default(),
            result: None,
            last_input: None,
            started: false,
            saw_init: false,
            model_calls: 0,
        }
    }

    pub fn saw_result(&self) -> bool {
        self.result.is_some()
    }

    pub async fn line(&mut self, line: &str) -> anyhow::Result<()> {
        let line = line.trim();
        if line.is_empty() {
            return Ok(());
        }
        let ev: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                tracing::debug!(line, "non-JSON line from claude");
                return Ok(());
            }
        };
        match ev.get("type").and_then(Value::as_str).unwrap_or("") {
            "system" => {
                if ev.get("subtype").and_then(Value::as_str) == Some("init")
                    && let Some(id) = ev.get("session_id").and_then(Value::as_str)
                {
                    self.saw_init = true;
                    // Journaled as soon as it is known, so a turn that dies
                    // mid-stream can still be resumed.
                    if self.known_session.as_deref() != Some(id) {
                        self.known_session = Some(id.to_string());
                        self.agent
                            .session()
                            .lock()
                            .await
                            .append(RecordKind::BackendSession {
                                backend: crate::BACKEND.into(),
                                id: id.into(),
                            })?;
                    }
                }
            }
            "stream_event" => self.stream_event(&ev["event"]).await?,
            "assistant" => {
                let msg = &ev["message"];
                // Overwritten by every call, so what survives is the last
                // one's — see `last_input`.
                if let Some(t) = context_of(msg) {
                    self.last_input = Some(t);
                }
                let id = msg
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let blocks = blocks_of(
                    msg.get("content")
                        .and_then(Value::as_array)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]),
                );
                for b in &blocks {
                    if let ContentBlock::ToolUse { id, name, input } = b {
                        self.calls
                            .insert(id.clone(), (name.clone(), input.to_value()));
                    }
                }
                match &mut self.current {
                    Some((cur, acc)) if *cur == id => acc.extend(blocks),
                    _ => {
                        self.flush().await?;
                        self.current = Some((id, blocks));
                    }
                }
            }
            "user" => {
                self.flush().await?;
                let content = ev["message"].get("content");
                if let Some(items) = content.and_then(Value::as_array) {
                    for item in items {
                        if item.get("type").and_then(Value::as_str) == Some("tool_result") {
                            let tid = item
                                .get("tool_use_id")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            let text = result_text(item.get("content"));
                            let is_error = item
                                .get("is_error")
                                .and_then(Value::as_bool)
                                .unwrap_or(false);
                            let record = self.agent.session().lock().await.append(
                                RecordKind::ToolResult {
                                    tool_use_id: tid.clone(),
                                    content: text.clone(),
                                    is_error,
                                },
                            )?;
                            let (name, input) = self
                                .calls
                                .get(&tid)
                                .cloned()
                                .unwrap_or_else(|| ("tool".into(), Value::Null));
                            self.agent.bus().publish(Event::ToolCallFinished {
                                record: Some(record),
                                call: ToolCall {
                                    id: tid,
                                    name,
                                    input,
                                    origin: CallOrigin::Model,
                                },
                                output: ToolOutput {
                                    content: text,
                                    is_error,
                                    ends_turn: false,
                                },
                            });
                        }
                    }
                }
            }
            "result" => {
                self.flush().await?;
                let usage = ev.get("usage").map(usage_of).unwrap_or_default();
                let stop = match ev.get("stop_reason").and_then(Value::as_str) {
                    Some("end_turn") | None => StopReason::EndTurn,
                    Some("max_tokens") => StopReason::MaxTokens,
                    Some("refusal") => StopReason::Refusal,
                    Some(_) => StopReason::Other,
                };
                let is_error = ev.get("is_error").and_then(Value::as_bool).unwrap_or(false);
                let text = ev
                    .get("result")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                self.result = Some((stop, usage, is_error, text));
            }
            _ => {}
        }
        Ok(())
    }

    async fn stream_event(&mut self, e: &Value) -> anyhow::Result<()> {
        let bus = self.agent.bus();
        match e.get("type").and_then(Value::as_str).unwrap_or("") {
            "message_start" => {
                if !self.started {
                    self.started = true;
                }
                bus.publish(Event::MessageStart);
            }
            "content_block_start" => {
                let b = &e["content_block"];
                if b.get("type").and_then(Value::as_str) == Some("tool_use") {
                    bus.publish(Event::ToolUseStart {
                        id: b["id"].as_str().unwrap_or_default().to_string(),
                        name: b["name"].as_str().unwrap_or_default().to_string(),
                    });
                }
            }
            "content_block_delta" => {
                let d = &e["delta"];
                match d.get("type").and_then(Value::as_str) {
                    Some("text_delta") => bus.publish(Event::TextDelta(
                        d["text"].as_str().unwrap_or_default().to_string(),
                    )),
                    Some("thinking_delta") => {
                        let t = d["thinking"].as_str().unwrap_or_default();
                        // The CLI streams thinking as empty deltas with a token estimate.
                        let shown = if t.is_empty() {
                            "·".to_string()
                        } else {
                            t.to_string()
                        };
                        bus.publish(Event::ThinkingDelta(shown));
                    }
                    Some("input_json_delta") => bus.publish(Event::ToolInputDelta {
                        id: String::new(),
                        partial_json: d["partial_json"].as_str().unwrap_or_default().to_string(),
                    }),
                    _ => {}
                }
            }
            "message_stop" => self.flush().await?,
            _ => {}
        }
        Ok(())
    }

    /// Journal that the CLI's session now holds the branch up to this
    /// point: a `BackendSession` record's *position* is the high-water mark
    /// a later turn replays from, so one is written at the end of every
    /// turn, not only when the id is new. Without it every turn would
    /// re-send the one before it as a transcript.
    ///
    /// Nothing is written when the CLI never reported a session (it failed
    /// to start, or the resume id was stale): claiming it heard us when it
    /// did not is how context goes missing.
    pub async fn caught_up(&self) -> anyhow::Result<()> {
        if let (true, Some(id)) = (self.saw_init, self.known_session.clone()) {
            self.agent
                .session()
                .lock()
                .await
                .append(RecordKind::BackendSession {
                    backend: crate::BACKEND.into(),
                    id,
                })?;
        }
        Ok(())
    }

    /// Journal and publish the assistant message being assembled, if any.
    async fn flush(&mut self) -> anyhow::Result<()> {
        if let Some((_, blocks)) = self.current.take() {
            let blocks: Vec<ContentBlock> = blocks
                .into_iter()
                .filter(|b| !matches!(b, ContentBlock::Thinking { thinking, signature } if thinking.is_empty() && signature.is_empty()))
                .collect();
            if blocks.is_empty() {
                return Ok(());
            }
            let m = Message::assistant(blocks);
            let record = self
                .agent
                .session()
                .lock()
                .await
                .append(RecordKind::AssistantMessage(m.clone()))?;
            self.model_calls += 1;
            self.agent
                .bus()
                .publish(Event::AssistantMessage { record, message: m });
        }
        Ok(())
    }

    /// Model calls made so far — what a *cancelled* turn on this backend
    /// can say about itself. The CLI reports usage in one final `result`
    /// event and a killed process never sends it, so the count is known
    /// and the cost is not; see `Agent::cancelled`, which declines to
    /// journal a zero rather than pricing the turn at nothing.
    pub fn model_calls(&self) -> u32 {
        self.model_calls
    }

    /// Journal and publish the context this turn last measured, when a call
    /// reported one at all.
    ///
    /// Before the settle it belongs to, and before the *cancellation* it
    /// belongs to as well: a killed process sends no `result` event, so the
    /// number cannot travel on `TurnOutcome::Cancelled` — that carries the
    /// turn's spend, and a spend is not a context. What each `assistant`
    /// event carries is one call's input, and the last of those is the last
    /// call that saw the whole transcript. A turn that reported nothing (a
    /// process killed before it answered) writes nothing, which leaves the
    /// context unknown rather than replacing it with a zero.
    pub async fn journal_context(&self) -> anyhow::Result<()> {
        if let Some(tokens) = self.last_input {
            self.agent
                .session()
                .lock()
                .await
                .append(RecordKind::ContextSize { tokens })?;
            self.agent.bus().publish(Event::ContextSize { tokens });
        }
        Ok(())
    }

    /// End of stream: settle the turn from the `result` event.
    pub async fn finish(&mut self) -> anyhow::Result<TurnOutcome> {
        self.flush().await?;
        let Some((stop, usage, is_error, text)) = self.result.take() else {
            anyhow::bail!("claude ended without a result event");
        };
        if is_error {
            self.agent
                .bus()
                .publish(Event::Error(format!("claude: {text}")));
            anyhow::bail!("claude reported an error turn: {text}");
        }
        // The CLI answers dangling tool uses itself, so unanswered ones here
        // mean the stream was cut; give them a synthetic error so the branch
        // stays well-formed.
        let dangling = self.agent.session().lock().await.unanswered_tool_uses();
        for (id, _, _) in dangling {
            self.agent
                .session()
                .lock()
                .await
                .append(RecordKind::ToolResult {
                    tool_use_id: id,
                    content: "no result reported".into(),
                    is_error: true,
                })?;
        }
        // Before the settle, as in the agent loop, and only when a call
        // actually reported one: a turn whose assistant events carried no
        // usage leaves the context unknown rather than claiming zero.
        self.journal_context().await?;
        self.agent
            .session()
            .lock()
            .await
            .append(RecordKind::TurnSettled {
                stop_reason: stop,
                usage,
            })?;
        self.agent.bus().publish(Event::TurnSettled {
            stop_reason: stop,
            usage,
            // Nobody timed this driver's calls: `duration_ms` in the CLI's
            // own result covers the whole subprocess — tool execution
            // included — so it is not a call's pace, and passing it here
            // would price the model's decode against the time a `cargo
            // test` took. `None` is the honest answer.
            timing: None,
        });
        Ok(TurnOutcome::Settled {
            stop_reason: stop,
            usage,
            calls: self.model_calls,
        })
    }
}

fn blocks_of(items: &[Value]) -> Vec<ContentBlock> {
    items
        .iter()
        .filter_map(|b| match b.get("type").and_then(Value::as_str) {
            Some("text") => Some(ContentBlock::Text {
                text: b["text"].as_str().unwrap_or_default().to_string(),
            }),
            Some("thinking") => Some(ContentBlock::Thinking {
                thinking: b["thinking"].as_str().unwrap_or_default().to_string(),
                signature: b["signature"].as_str().unwrap_or_default().to_string(),
            }),
            Some("redacted_thinking") => Some(ContentBlock::RedactedThinking {
                data: b["data"].as_str().unwrap_or_default().to_string(),
            }),
            Some("tool_use") => Some(ContentBlock::ToolUse {
                id: b["id"].as_str().unwrap_or_default().to_string(),
                name: b["name"].as_str().unwrap_or_default().to_string(),
                input: Json::from_value(b.get("input").unwrap_or(&Value::Null)),
            }),
            _ => None,
        })
        .collect()
}

fn result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|i| i.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// How full the context was at one model call, from that call's
/// `assistant` message.
///
/// `total_input` and never `input_tokens`: the CLI caches aggressively, so
/// a call that read 205k tokens of transcript reports `input_tokens: 12`
/// with the other 205,571 under `cache_read_input_tokens`. Reading the
/// first alone would draw a gauge at nearly empty on a context nearly
/// full — the same trap `eidolon_core::usage` documents for the cost
/// figures.
fn context_of(msg: &Value) -> Option<u64> {
    Some(usage_of(msg.get("usage")?).total_input())
}

fn usage_of(u: &Value) -> Usage {
    let g = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
    Usage {
        input_tokens: g("input_tokens"),
        output_tokens: g("output_tokens"),
        cache_creation_input_tokens: g("cache_creation_input_tokens"),
        cache_read_input_tokens: g("cache_read_input_tokens"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The context is one call's input, read whole.
    ///
    /// This backend used to journal no context size at all, so a session on
    /// the Claude CLI drew `ctx —` for ever rather than only until its next
    /// turn: the `result` event's usage is the whole invocation's total —
    /// the CLI runs its own tool loop — and that is the number
    /// `RecordKind::ContextSize` exists to not be.
    #[test]
    fn the_context_is_a_calls_whole_input_and_not_its_fresh_tokens() {
        let msg = serde_json::json!({
            "usage": { "input_tokens": 12, "output_tokens": 3282, "cache_creation_input_tokens": 18_474, "cache_read_input_tokens": 187_097 }
        });
        // Not 12. A cache-warm call has read everything it is charged a
        // discount for.
        assert_eq!(context_of(&msg), Some(205_583));
    }

    /// A call that reports no usage leaves the context unknown, which the
    /// gauge draws as a dash. Claiming zero would be claiming an empty
    /// context on a session that may be nearly full.
    #[test]
    fn a_call_that_reports_no_usage_says_nothing_rather_than_zero() {
        assert_eq!(context_of(&serde_json::json!({ "id": "msg_1" })), None);
        assert_eq!(
            context_of(&serde_json::json!({ "usage": {} })),
            Some(0),
            "a usage that is present but empty is a real zero"
        );
    }

    /// An agent over a session log, for the tests that need somewhere to
    /// journal. Nothing here runs a model: the sink is handed lines.
    fn harness(
        dir: &std::path::Path,
    ) -> (
        eidolon_core::Agent,
        std::sync::Arc<tokio::sync::Mutex<eidolon_core::Session>>,
    ) {
        use crate::BACKEND;
        use eidolon_core::AgentConfig;
        use eidolon_core::Session;
        use eidolon_core::policy::AllowAll;
        use eidolon_core::testing::{ScriptedProvider, ScriptedUser};
        let session = Session::create(&dir.join("s.eid"), BACKEND, dir, None).unwrap();
        let session = std::sync::Arc::new(tokio::sync::Mutex::new(session));
        let d = std::sync::Arc::new(eidolon_core::Dispatcher::new(
            eidolon_core::ToolRegistry::new(),
            std::sync::Arc::new(AllowAll),
            ScriptedUser::new(true),
            eidolon_core::EventBus::default(),
            session.clone(),
            dir.to_path_buf(),
        ));
        let agent = eidolon_core::Agent::new(
            ScriptedProvider::new(Vec::new()),
            d,
            AgentConfig {
                model: "sonnet".into(),
                ..Default::default()
            },
        );
        (agent, session)
    }

    /// A killed process still says how full the context was, when it
    /// managed to say anything.
    ///
    /// The cancel path returned `TurnOutcome::Cancelled` and journaled
    /// nothing at all, so a stopped turn on this backend left every
    /// consumer drawing the previous turn's number: stale by one turn of
    /// ordinary work, and wrong by an order of magnitude on a branch that
    /// had just been compacted, where the number standing there is the
    /// summary's own size. The measurement is in hand — every `assistant`
    /// event carries one call's input — and `TurnOutcome::Cancelled` has
    /// no room for it, being the turn's spend.
    #[tokio::test]
    async fn a_stopped_turn_journals_the_context_the_cli_reported() {
        let dir = tempfile::tempdir().unwrap();
        let (agent, session) = harness(dir.path());
        let mut sink = Sink::new(&agent, None);
        let mut events = agent.bus().subscribe();
        // One call, read whole: 12 fresh beside 187,097 served from cache.
        sink.line(
            &serde_json::json!({
                "type": "assistant",
                "message": {
                    "id": "msg_1",
                    "content": [{ "type": "text", "text": "working" }],
                    "usage": { "input_tokens": 12, "output_tokens": 30, "cache_read_input_tokens": 187_097 }
                }
            })
            .to_string(),
        )
        .await
        .unwrap();
        sink.journal_context().await.unwrap();

        {
            let s = session.lock().await;
            assert_eq!(
                s.last_input_tokens(),
                Some(187_109),
                "the gauge and the inspector read this"
            );
        }
        let mut seen = Vec::new();
        while let Ok(e) = events.try_recv() {
            if let Event::ContextSize { tokens } = e {
                seen.push(tokens);
            }
        }
        assert_eq!(seen, [187_109], "and a live consumer is told");

        // A process killed before it answered anything reported no context:
        // the branch is left without one rather than with a zero, which
        // would read as an empty context on a session that may be full.
        Sink::new(&agent, None).journal_context().await.unwrap();
        let s = session.lock().await;
        assert_eq!(
            s.branch()
                .iter()
                .filter(|r| matches!(r.kind, RecordKind::ContextSize { .. }))
                .count(),
            1,
            "nothing measured, so nothing written"
        );
    }
}
