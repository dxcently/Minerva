//! The wire projection: `core::Event` as the JSON a browser sees on
//! `/api/events`.
//!
//! A projection, not a mirror: thinking signatures, redacted thinking and
//! image bytes never cross; the log is the one place they live. `record` is
//! the `RecordId` where the event has one.
//!
//! [`project`] maps bus events and [`replay`] maps a walked branch to the same
//! shapes, so the page never knows which it is drawing. The door's own frames
//! (`hello`, `caught-up`, `turn-state`, `ask`, `ask-settled`, `lagged`,
//! `goodbye`) are built by the server, not here.

use eidolon_core::event::Event;
use eidolon_core::message::{ContentBlock, Message, StopReason};
use eidolon_core::session::{PolicyOutcome, Record, RecordKind};
use eidolon_core::tool::CallOrigin;
use serde::Serialize;

/// One `data:` line's payload.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Wire {
    MessageStart {},
    TextDelta { text: String },
    ThinkingDelta { text: String },
    ToolUseStart { id: String, name: String },
    ToolInputDelta { id: String, partial_json: String },
    AssistantMessage {
        record: u64,
        text: String,
        thinking: String,
        redacted: usize,
        tool_uses: Vec<ToolUse>,
    },
    UserMessage {
        record: u64,
        text: String,
        images: usize,
    },
    ToolCallStarted {
        id: String,
        name: String,
        input: serde_json::Value,
        origin: Origin,
    },
    ToolCallFinished {
        record: Option<u64>,
        id: String,
        name: String,
        output: String,
        is_error: bool,
    },
    AskUser { prompt: String },
    PolicyVerdict {
        tool: String,
        reason: String,
        outcome: Outcome,
        #[serde(skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    ContextSize { tokens: u64 },
    TurnSettled {
        stop_reason: Stop,
        usage: Usage,
        #[serde(skip_serializing_if = "Option::is_none")]
        timing: Option<Timing>,
    },
    Compacted { summary: String, replaced_messages: u32 },
    PeerMessage {
        record: u64,
        from: String,
        from_cwd: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        channel: Option<String>,
        text: String,
        external: bool,
    },
    TurnBudget { record: u64, calls_left: u32 },
    CommandResults { record: u64, lines: Vec<String> },
    Cancelled { usage: Usage, calls: u32 },
    Error { text: String },
    Queued { waiting: usize },
    Quiesced { record: u64, destination: String },
    /// A park resolved. `outcome` is `eidolon_core::wait::Outcome::words`;
    /// `call_id` is the `wait_for` call that armed it, so a live view can
    /// settle the block that call drew.
    TriggerFired {
        record: u64,
        condition: String,
        outcome: String,
        call_id: String,
    },
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct ToolUse {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Origin {
    Model,
    Script,
    User,
}

impl From<CallOrigin> for Origin {
    fn from(o: CallOrigin) -> Self {
        match o {
            CallOrigin::Model => Origin::Model,
            CallOrigin::Script => Origin::Script,
            CallOrigin::User => Origin::User,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    Refused,
    Approved,
    Declined,
    Judged,
    Yolo,
}

impl From<&PolicyOutcome> for Outcome {
    fn from(o: &PolicyOutcome) -> Self {
        match o {
            PolicyOutcome::Refused => Outcome::Refused,
            PolicyOutcome::Approved => Outcome::Approved,
            PolicyOutcome::Declined => Outcome::Declined,
            PolicyOutcome::Judged => Outcome::Judged,
            PolicyOutcome::Yolo => Outcome::Yolo,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Stop {
    EndTurn,
    ToolUse,
    MaxTokens,
    StopSequence,
    Refusal,
    Cancelled,
    Other,
    /// Parked on a wait (`eidolon_core::wait`). Not `Other`: a park is a
    /// decision, not an unrecognised stop.
    Waiting,
    /// The operator left a question unanswered; likewise not `Other`.
    Dismissed,
}

impl From<&StopReason> for Stop {
    fn from(s: &StopReason) -> Self {
        match s {
            StopReason::EndTurn => Stop::EndTurn,
            StopReason::ToolUse => Stop::ToolUse,
            StopReason::MaxTokens => Stop::MaxTokens,
            StopReason::StopSequence => Stop::StopSequence,
            StopReason::Refusal => Stop::Refusal,
            StopReason::Cancelled => Stop::Cancelled,
            StopReason::Other => Stop::Other,
            StopReason::Waiting => Stop::Waiting,
            StopReason::Dismissed => Stop::Dismissed,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
}

impl From<&eidolon_core::message::Usage> for Usage {
    fn from(u: &eidolon_core::message::Usage) -> Self {
        Usage {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cache_creation_input_tokens: u.cache_creation_input_tokens,
            cache_read_input_tokens: u.cache_read_input_tokens,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
pub struct Timing {
    pub calls: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_token_ms: Option<u64>,
    pub decode_ms: u64,
    pub total_ms: u64,
}

impl From<&eidolon_core::usage::TurnTiming> for Timing {
    fn from(t: &eidolon_core::usage::TurnTiming) -> Self {
        Timing {
            calls: t.calls,
            first_token_ms: t.first_token_ms,
            decode_ms: t.decode_ms,
            total_ms: t.total_ms,
        }
    }
}

/// Each kind of block joined: the page draws a transcript row, not a block list.
fn split(message: &Message) -> (String, String, usize, Vec<ToolUse>) {
    let mut text = String::new();
    let mut thinking = String::new();
    let mut redacted = 0usize;
    let mut tool_uses = Vec::new();
    for block in &message.content {
        match block {
            ContentBlock::Text { text: t } => {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(t);
            }
            ContentBlock::Thinking { thinking: t, .. } => {
                if !thinking.is_empty() {
                    thinking.push('\n');
                }
                thinking.push_str(t);
            }
            ContentBlock::RedactedThinking { .. } => redacted += 1,
            ContentBlock::ToolUse { id, name, input } => tool_uses.push(ToolUse {
                id: id.clone(),
                name: name.clone(),
                input: serde_json::from_str(&input.0).unwrap_or(serde_json::Value::Null),
            }),
            // Tool results replay from their own `ToolResult` record.
            ContentBlock::ToolResult { .. } | ContentBlock::Image { .. } => {}
        }
    }
    (text, thinking, redacted, tool_uses)
}

fn image_count(message: &Message) -> usize {
    message
        .content
        .iter()
        .filter(|b| matches!(b, ContentBlock::Image { .. }))
        .count()
}

/// One bus [`Event`] as its wire frame, or `None` for one the page has no row
/// for (none today).
pub fn project(event: &Event) -> Option<Wire> {
    Some(match event {
        Event::MessageStart => Wire::MessageStart {},
        Event::TextDelta(text) => Wire::TextDelta { text: text.clone() },
        Event::ThinkingDelta(text) => Wire::ThinkingDelta { text: text.clone() },
        Event::ToolUseStart { id, name } => Wire::ToolUseStart {
            id: id.clone(),
            name: name.clone(),
        },
        Event::ToolInputDelta { id, partial_json } => Wire::ToolInputDelta {
            id: id.clone(),
            partial_json: partial_json.clone(),
        },
        Event::AssistantMessage { record, message } => {
            let (text, thinking, redacted, tool_uses) = split(message);
            Wire::AssistantMessage {
                record: *record,
                text,
                thinking,
                redacted,
                tool_uses,
            }
        }
        Event::UserMessage { record, message } => {
            let (text, _thinking, _redacted, _tool_uses) = split(message);
            Wire::UserMessage {
                record: *record,
                text,
                images: image_count(message),
            }
        }
        Event::ToolCallStarted(call) => Wire::ToolCallStarted {
            id: call.id.clone(),
            name: call.name.clone(),
            input: call.input.clone(),
            origin: call.origin.into(),
        },
        Event::ToolCallFinished { record, call, output } => Wire::ToolCallFinished {
            record: *record,
            id: call.id.clone(),
            name: call.name.clone(),
            output: output.content.clone(),
            is_error: output.is_error,
        },
        Event::AskUser { prompt } => Wire::AskUser { prompt: prompt.clone() },
        Event::PolicyVerdict { tool, reason, outcome, note } => Wire::PolicyVerdict {
            tool: tool.clone(),
            reason: reason.clone(),
            outcome: outcome.into(),
            note: note.clone(),
        },
        Event::ContextSize { tokens } => Wire::ContextSize { tokens: *tokens },
        Event::TurnSettled { stop_reason, usage, timing } => Wire::TurnSettled {
            stop_reason: stop_reason.into(),
            usage: usage.into(),
            timing: timing.as_ref().map(Timing::from),
        },
        Event::Compacted { summary, replaced_messages } => Wire::Compacted {
            summary: summary.clone(),
            replaced_messages: *replaced_messages,
        },
        Event::PeerMessage { record, from, from_cwd, channel, text, external } => Wire::PeerMessage {
            record: *record,
            from: from.clone(),
            from_cwd: from_cwd.clone(),
            channel: channel.clone(),
            text: text.clone(),
            external: *external,
        },
        Event::TurnBudget { record, calls_left } => Wire::TurnBudget {
            record: *record,
            calls_left: *calls_left,
        },
        Event::CommandResults { record, lines } => Wire::CommandResults {
            record: *record,
            lines: lines.clone(),
        },
        Event::Cancelled { usage, calls } => Wire::Cancelled {
            usage: usage.into(),
            calls: *calls,
        },
        Event::Error(text) => Wire::Error { text: text.clone() },
        Event::Queued { waiting } => Wire::Queued { waiting: *waiting },
        Event::Quiesced { record, destination } => Wire::Quiesced {
            record: *record,
            destination: destination.clone(),
        },
        Event::TriggerFired { record, condition, outcome, call_id } => Wire::TriggerFired {
            record: *record,
            condition: condition.clone(),
            outcome: outcome.clone(),
            call_id: call_id.clone(),
        },
    })
}

/// A walked branch as the frames a live session would have published. Kinds
/// with no drawn row are skipped; `ModelChanged` rides `hello.model` instead.
pub fn replay(records: &[&Record]) -> Vec<Wire> {
    let mut out = Vec::new();
    for r in records {
        match &r.kind {
            RecordKind::UserMessage(message) => {
                let (text, _t, _rd, _tu) = split(message);
                out.push(Wire::UserMessage {
                    record: r.id,
                    text,
                    images: image_count(message),
                });
            }
            RecordKind::AssistantMessage(message) => {
                let (text, thinking, redacted, tool_uses) = split(message);
                out.push(Wire::AssistantMessage {
                    record: r.id,
                    text,
                    thinking,
                    redacted,
                    tool_uses: tool_uses.clone(),
                });
                for tu in tool_uses {
                    out.push(Wire::ToolCallStarted {
                        id: tu.id,
                        name: tu.name,
                        input: tu.input,
                        origin: Origin::Model,
                    });
                }
            }
            RecordKind::ToolResult { tool_use_id, content, is_error } => {
                // Name and input are on the paired `tool-call-started`, keyed by id.
                out.push(Wire::ToolCallFinished {
                    record: Some(r.id),
                    id: tool_use_id.clone(),
                    name: String::new(),
                    output: content.clone(),
                    is_error: *is_error,
                });
            }
            RecordKind::PolicyVerdict { tool, reason, outcome, note, .. } => {
                out.push(Wire::PolicyVerdict {
                    tool: tool.clone(),
                    reason: reason.clone(),
                    outcome: outcome.into(),
                    note: note.clone(),
                });
            }
            RecordKind::TurnSettled { stop_reason, usage } => {
                out.push(Wire::TurnSettled {
                    stop_reason: stop_reason.into(),
                    usage: usage.into(),
                    // Timing is on the `TurnPace` record before this; replay
                    // does not walk back for it.
                    timing: None,
                });
            }
            RecordKind::Compacted { summary, replaced_messages } => out.push(Wire::Compacted {
                summary: summary.clone(),
                replaced_messages: *replaced_messages,
            }),
            RecordKind::PeerMessage { from, from_cwd, channel, text } => out.push(Wire::PeerMessage {
                record: r.id,
                from: from.clone(),
                from_cwd: from_cwd.clone(),
                channel: channel.clone(),
                text: text.clone(),
                external: false,
            }),
            RecordKind::TurnBudget { calls_left } => out.push(Wire::TurnBudget {
                record: r.id,
                calls_left: *calls_left,
            }),
            RecordKind::CommandResults { lines } => out.push(Wire::CommandResults {
                record: r.id,
                lines: lines.clone(),
            }),
            RecordKind::Cancelled => out.push(Wire::Cancelled {
                usage: Usage::default(),
                calls: 0,
            }),
            RecordKind::ContextSize { tokens } => out.push(Wire::ContextSize { tokens: *tokens }),
            // No `quiesced`: its marker is a skipped `Note`, so it arrives only live.
            RecordKind::ExternalMessage { from, channel, text } => out.push(Wire::PeerMessage {
                record: r.id,
                from: from.clone(),
                // An outside caller (`eidolon send`) has no cwd.
                from_cwd: String::new(),
                channel: channel.clone(),
                text: text.clone(),
                external: true,
            }),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use eidolon_core::message::{Json, Usage as CoreUsage};
    use eidolon_core::tool::{CallOrigin, ToolCall, ToolOutput};

    fn wire_json(w: &Wire) -> serde_json::Value {
        serde_json::to_value(w).unwrap()
    }

    #[test]
    fn message_start() {
        assert_eq!(project(&Event::MessageStart), Some(Wire::MessageStart {}));
        assert_eq!(wire_json(&Wire::MessageStart {})["type"], "message-start");
    }

    #[test]
    fn text_delta() {
        let w = project(&Event::TextDelta("hi".into())).unwrap();
        assert_eq!(wire_json(&w), serde_json::json!({"type": "text-delta", "text": "hi"}));
    }

    #[test]
    fn thinking_delta() {
        let w = project(&Event::ThinkingDelta("hmm".into())).unwrap();
        assert_eq!(wire_json(&w), serde_json::json!({"type": "thinking-delta", "text": "hmm"}));
    }

    #[test]
    fn tool_use_start() {
        let w = project(&Event::ToolUseStart { id: "a".into(), name: "bash".into() }).unwrap();
        assert_eq!(
            wire_json(&w),
            serde_json::json!({"type": "tool-use-start", "id": "a", "name": "bash"})
        );
    }

    #[test]
    fn tool_input_delta() {
        let w = project(&Event::ToolInputDelta { id: "a".into(), partial_json: "{\"x\":".into() }).unwrap();
        assert_eq!(
            wire_json(&w),
            serde_json::json!({"type": "tool-input-delta", "id": "a", "partial_json": "{\"x\":"})
        );
    }

    #[test]
    fn assistant_message() {
        let msg = Message::assistant(vec![
            ContentBlock::text("hello"),
            ContentBlock::ToolUse { id: "t1".into(), name: "read".into(), input: Json("{\"path\":\"x\"}".into()) },
        ]);
        let w = project(&Event::AssistantMessage { record: 5, message: msg }).unwrap();
        assert_eq!(
            wire_json(&w),
            serde_json::json!({
                "type": "assistant-message",
                "record": 5,
                "text": "hello",
                "thinking": "",
                "redacted": 0,
                "tool_uses": [{"id": "t1", "name": "read", "input": {"path": "x"}}],
            })
        );
    }

    #[test]
    fn user_message_with_image() {
        let msg = Message::user(vec![ContentBlock::text("look"), ContentBlock::image("image/png", "AA", None)]);
        let w = project(&Event::UserMessage { record: 2, message: msg }).unwrap();
        assert_eq!(
            wire_json(&w),
            serde_json::json!({"type": "user-message", "record": 2, "text": "look", "images": 1})
        );
    }

    #[test]
    fn tool_call_started() {
        let call = ToolCall {
            id: "t1".into(),
            name: "bash".into(),
            input: serde_json::json!({"cmd": "ls"}),
            origin: CallOrigin::Script,
        };
        let w = project(&Event::ToolCallStarted(call)).unwrap();
        assert_eq!(
            wire_json(&w),
            serde_json::json!({
                "type": "tool-call-started", "id": "t1", "name": "bash",
                "input": {"cmd": "ls"}, "origin": "script",
            })
        );
    }

    #[test]
    fn tool_call_finished_no_record() {
        let call = ToolCall { id: "t1".into(), name: "bash".into(), input: serde_json::Value::Null, origin: CallOrigin::Model };
        let out = ToolOutput::error("nope");
        let w = project(&Event::ToolCallFinished { record: None, call, output: out }).unwrap();
        assert_eq!(
            wire_json(&w),
            serde_json::json!({
                "type": "tool-call-finished", "record": null, "id": "t1", "name": "bash",
                "output": "nope", "is_error": true,
            })
        );
    }

    #[test]
    fn ask_user() {
        let w = project(&Event::AskUser { prompt: "run rm -rf?".into() }).unwrap();
        assert_eq!(wire_json(&w), serde_json::json!({"type": "ask-user", "prompt": "run rm -rf?"}));
    }

    #[test]
    fn policy_verdict_judged() {
        let w = project(&Event::PolicyVerdict {
            tool: "bash".into(),
            reason: "subshell".into(),
            outcome: PolicyOutcome::Judged,
            note: Some("safe: read-only listing".into()),
        })
        .unwrap();
        assert_eq!(
            wire_json(&w),
            serde_json::json!({
                "type": "policy-verdict", "tool": "bash", "reason": "subshell",
                "outcome": "judged", "note": "safe: read-only listing",
            })
        );
    }

    /// Not folded into `judged`: the compiler catches a missing arm, not a wrong one.
    #[test]
    fn policy_verdict_yolo() {
        let w = project(&Event::PolicyVerdict {
            tool: "bash".into(),
            reason: "outside-cwd".into(),
            outcome: PolicyOutcome::Yolo,
            note: None,
        })
        .unwrap();
        assert_eq!(
            wire_json(&w),
            serde_json::json!({
                "type": "policy-verdict", "tool": "bash", "reason": "outside-cwd", "outcome": "yolo",
            })
        );
    }

    #[test]
    fn policy_verdict_no_note_omits_field() {
        let w = project(&Event::PolicyVerdict {
            tool: "bash".into(),
            reason: "outside-cwd".into(),
            outcome: PolicyOutcome::Refused,
            note: None,
        })
        .unwrap();
        let j = wire_json(&w);
        assert!(j.get("note").is_none());
    }

    #[test]
    fn context_size() {
        let w = project(&Event::ContextSize { tokens: 4096 }).unwrap();
        assert_eq!(wire_json(&w), serde_json::json!({"type": "context-size", "tokens": 4096}));
    }

    #[test]
    fn turn_settled_with_timing() {
        let usage = CoreUsage {
            input_tokens: 10,
            output_tokens: 20,
            cache_creation_input_tokens: 1,
            cache_read_input_tokens: 2,
        };
        let timing = eidolon_core::usage::TurnTiming { calls: 1, first_token_ms: Some(120), decode_ms: 300, total_ms: 420 };
        let w = project(&Event::TurnSettled { stop_reason: StopReason::EndTurn, usage, timing: Some(timing) }).unwrap();
        assert_eq!(
            wire_json(&w),
            serde_json::json!({
                "type": "turn-settled",
                "stop_reason": "end-turn",
                "usage": {"input_tokens": 10, "output_tokens": 20, "cache_creation_input_tokens": 1, "cache_read_input_tokens": 2},
                "timing": {"calls": 1, "first_token_ms": 120, "decode_ms": 300, "total_ms": 420},
            })
        );
    }

    #[test]
    fn turn_settled_no_timing_omits_field() {
        let usage = CoreUsage::default();
        let w = project(&Event::TurnSettled { stop_reason: StopReason::Cancelled, usage, timing: None }).unwrap();
        assert!(wire_json(&w).get("timing").is_none());
    }

    #[test]
    fn harness_settles_are_not_other() {
        for (reason, wire) in [(StopReason::Waiting, "waiting"), (StopReason::Dismissed, "dismissed")] {
            let w = project(&Event::TurnSettled { stop_reason: reason, usage: CoreUsage::default(), timing: None }).unwrap();
            assert_eq!(serde_json::to_value(Stop::from(&reason)).unwrap(), serde_json::json!(wire));
            assert_eq!(wire_json(&w)["stop_reason"], wire);
        }
    }

    #[test]
    fn compacted() {
        let w = project(&Event::Compacted { summary: "s".into(), replaced_messages: 12 }).unwrap();
        assert_eq!(
            wire_json(&w),
            serde_json::json!({"type": "compacted", "summary": "s", "replaced_messages": 12})
        );
    }

    #[test]
    fn peer_message_external() {
        let w = project(&Event::PeerMessage {
            record: 9,
            from: "eidolon send".into(),
            from_cwd: "/tmp".into(),
            channel: None,
            text: "hi".into(),
            external: true,
        })
        .unwrap();
        assert_eq!(
            wire_json(&w),
            serde_json::json!({
                "type": "peer-message", "record": 9, "from": "eidolon send", "from_cwd": "/tmp",
                "text": "hi", "external": true,
            })
        );
    }

    #[test]
    fn turn_budget() {
        let w = project(&Event::TurnBudget { record: 3, calls_left: 2 }).unwrap();
        assert_eq!(wire_json(&w), serde_json::json!({"type": "turn-budget", "record": 3, "calls_left": 2}));
    }

    #[test]
    fn command_results() {
        let w = project(&Event::CommandResults { record: 7, lines: vec!["ok".into()] }).unwrap();
        assert_eq!(
            wire_json(&w),
            serde_json::json!({"type": "command-results", "record": 7, "lines": ["ok"]})
        );
    }

    #[test]
    fn cancelled() {
        let usage = CoreUsage { input_tokens: 5, output_tokens: 0, cache_creation_input_tokens: 0, cache_read_input_tokens: 0 };
        let w = project(&Event::Cancelled { usage, calls: 3 }).unwrap();
        assert_eq!(
            wire_json(&w),
            serde_json::json!({
                "type": "cancelled", "calls": 3,
                "usage": {"input_tokens": 5, "output_tokens": 0, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0},
            })
        );
    }

    #[test]
    fn error() {
        let w = project(&Event::Error("append failed".into())).unwrap();
        assert_eq!(wire_json(&w), serde_json::json!({"type": "error", "text": "append failed"}));
    }

    #[test]
    fn queued() {
        let w = project(&Event::Queued { waiting: 2 }).unwrap();
        assert_eq!(wire_json(&w), serde_json::json!({"type": "queued", "waiting": 2}));
    }

    #[test]
    fn quiesced() {
        let w = project(&Event::Quiesced { record: 11, destination: "log 42".into() }).unwrap();
        assert_eq!(
            wire_json(&w),
            serde_json::json!({"type": "quiesced", "record": 11, "destination": "log 42"})
        );
    }

    #[test]
    fn trigger_fired() {
        let w = project(&Event::TriggerFired {
            record: 8,
            condition: "the build log says green".into(),
            outcome: "it fired".into(),
            call_id: "tu1".into(),
        })
        .unwrap();
        assert_eq!(
            wire_json(&w),
            serde_json::json!({
                "type": "trigger-fired", "record": 8, "condition": "the build log says green",
                "outcome": "it fired", "call_id": "tu1",
            })
        );
    }
}
