//! One JSON fixture per wire message, pinned from outside the crate as
//! `crates/core/tests/pins.rs` pins `RecordKind`: a changed shape has to
//! break here as well as in `src/wire.rs`.

use eidolon_core::event::Event;
use eidolon_core::message::{ContentBlock, Json, Message, StopReason, Usage};
use eidolon_core::session::PolicyOutcome;
use eidolon_core::tool::{CallOrigin, ToolCall, ToolOutput};
use eidolon_web::wire::project;

fn j(event: &Event) -> serde_json::Value {
    serde_json::to_value(project(event).expect("every Event variant projects to a Wire message")).unwrap()
}

#[test]
fn one_fixture_per_wire_message() {
    assert_eq!(j(&Event::MessageStart), serde_json::json!({"type": "message-start"}));

    assert_eq!(
        j(&Event::TextDelta("partial".into())),
        serde_json::json!({"type": "text-delta", "text": "partial"})
    );

    assert_eq!(
        j(&Event::ThinkingDelta("reasoning".into())),
        serde_json::json!({"type": "thinking-delta", "text": "reasoning"})
    );

    assert_eq!(
        j(&Event::ToolUseStart { id: "tu1".into(), name: "bash".into() }),
        serde_json::json!({"type": "tool-use-start", "id": "tu1", "name": "bash"})
    );

    assert_eq!(
        j(&Event::ToolInputDelta { id: "tu1".into(), partial_json: "{\"cmd\":".into() }),
        serde_json::json!({"type": "tool-input-delta", "id": "tu1", "partial_json": "{\"cmd\":"})
    );

    assert_eq!(
        j(&Event::AssistantMessage {
            record: 1,
            message: Message::assistant(vec![ContentBlock::text("done")]),
        }),
        serde_json::json!({
            "type": "assistant-message", "record": 1, "text": "done",
            "thinking": "", "redacted": 0, "tool_uses": [],
        })
    );

    assert_eq!(
        j(&Event::UserMessage { record: 2, message: Message::user(vec![ContentBlock::text("hi")]) }),
        serde_json::json!({"type": "user-message", "record": 2, "text": "hi", "images": 0})
    );

    assert_eq!(
        j(&Event::ToolCallStarted(ToolCall {
            id: "tu1".into(),
            name: "read".into(),
            input: serde_json::json!({"path": "x"}),
            origin: CallOrigin::Model,
        })),
        serde_json::json!({
            "type": "tool-call-started", "id": "tu1", "name": "read",
            "input": {"path": "x"}, "origin": "model",
        })
    );

    assert_eq!(
        j(&Event::ToolCallFinished {
            record: Some(9),
            call: ToolCall { id: "tu1".into(), name: "read".into(), input: serde_json::Value::Null, origin: CallOrigin::Model },
            output: ToolOutput::ok("contents"),
        }),
        serde_json::json!({
            "type": "tool-call-finished", "record": 9, "id": "tu1", "name": "read",
            "output": "contents", "is_error": false,
        })
    );

    assert_eq!(
        j(&Event::AskUser { prompt: "delete everything?".into() }),
        serde_json::json!({"type": "ask-user", "prompt": "delete everything?"})
    );

    assert_eq!(
        j(&Event::PolicyVerdict {
            tool: "bash".into(),
            reason: "yolo".into(),
            outcome: PolicyOutcome::Yolo,
            note: None,
        }),
        serde_json::json!({"type": "policy-verdict", "tool": "bash", "reason": "yolo", "outcome": "yolo"})
    );

    assert_eq!(j(&Event::ContextSize { tokens: 128 }), serde_json::json!({"type": "context-size", "tokens": 128}));

    assert_eq!(
        j(&Event::TurnSettled { stop_reason: StopReason::ToolUse, usage: Usage::default(), timing: None }),
        serde_json::json!({
            "type": "turn-settled", "stop_reason": "tool-use",
            "usage": {"input_tokens": 0, "output_tokens": 0, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0},
        })
    );

    assert_eq!(
        j(&Event::Compacted { summary: "summarised".into(), replaced_messages: 40 }),
        serde_json::json!({"type": "compacted", "summary": "summarised", "replaced_messages": 40})
    );

    assert_eq!(
        j(&Event::PeerMessage {
            record: 3,
            from: "worktree-b".into(),
            from_cwd: "/repo/worktree-b".into(),
            channel: Some("build".into()),
            text: "green".into(),
            external: false,
        }),
        serde_json::json!({
            "type": "peer-message", "record": 3, "from": "worktree-b", "from_cwd": "/repo/worktree-b",
            "channel": "build", "text": "green", "external": false,
        })
    );

    assert_eq!(
        j(&Event::TurnBudget { record: 4, calls_left: 3 }),
        serde_json::json!({"type": "turn-budget", "record": 4, "calls_left": 3})
    );

    assert_eq!(
        j(&Event::CommandResults { record: 5, lines: vec!["1".into(), "2".into()] }),
        serde_json::json!({"type": "command-results", "record": 5, "lines": ["1", "2"]})
    );

    assert_eq!(
        j(&Event::Cancelled { usage: Usage::default(), calls: 1 }),
        serde_json::json!({
            "type": "cancelled", "calls": 1,
            "usage": {"input_tokens": 0, "output_tokens": 0, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0},
        })
    );

    assert_eq!(j(&Event::Error("append failed".into())), serde_json::json!({"type": "error", "text": "append failed"}));

    assert_eq!(j(&Event::Queued { waiting: 1 }), serde_json::json!({"type": "queued", "waiting": 1}));

    assert_eq!(
        j(&Event::Quiesced { record: 6, destination: "log 7".into() }),
        serde_json::json!({"type": "quiesced", "record": 6, "destination": "log 7"})
    );
}

#[test]
fn trigger_fired_fixture() {
    assert_eq!(
        j(&Event::TriggerFired {
            record: 12,
            condition: "the build log says green".into(),
            outcome: "it fired".into(),
            call_id: "tu9".into(),
        }),
        serde_json::json!({
            "type": "trigger-fired", "record": 12, "condition": "the build log says green",
            "outcome": "it fired", "call_id": "tu9",
        })
    );
}

#[test]
fn tool_use_input_is_parsed_from_the_raw_json_string() {
    let message = Message::assistant(vec![ContentBlock::ToolUse {
        id: "t1".into(),
        name: "bash".into(),
        input: Json("{\"cmd\":\"ls -la\"}".into()),
    }]);
    let wire = j(&Event::AssistantMessage { record: 1, message });
    assert_eq!(wire["tool_uses"][0]["input"], serde_json::json!({"cmd": "ls -la"}));
}

/// A reconnecting page must draw the transcript it drew live.
#[test]
fn replay_projects_a_branch_into_the_same_frames_the_bus_published() {
    use eidolon_core::session::{Record, RecordKind};
    use eidolon_web::wire::replay;

    let rec = |id: u64, kind: RecordKind| Record { id, parent: None, ts_ms: 0, kind };
    let branch = [
        rec(0, RecordKind::SessionStart { model: "m".into(), cwd: "/tmp".into(), system: None }),
        rec(1, RecordKind::UserMessage(Message::user_text("hi"))),
        rec(
            2,
            RecordKind::AssistantMessage(Message::assistant(vec![
                ContentBlock::text("looking"),
                ContentBlock::ToolUse { id: "t1".into(), name: "read".into(), input: Json("{\"path\":\"x\"}".into()) },
            ])),
        ),
        rec(3, RecordKind::ToolResult { tool_use_id: "t1".into(), content: "x".into(), is_error: false }),
        rec(4, RecordKind::ModelChanged { model: "other".into() }),
        rec(5, RecordKind::TurnSettled { stop_reason: StopReason::EndTurn, usage: Usage::default() }),
        rec(6, RecordKind::Note { text: "a bookmark".into() }),
    ];
    let out: Vec<serde_json::Value> =
        replay(&branch.iter().collect::<Vec<_>>()).iter().map(|w| serde_json::to_value(w).unwrap()).collect();

    assert_eq!(out.len(), 5, "the session start, the model change and the note carry no frame: {out:?}");
    assert_eq!(out[0], serde_json::json!({"type": "user-message", "record": 1, "text": "hi", "images": 0}));
    assert_eq!(
        out[1],
        serde_json::json!({
            "type": "assistant-message", "record": 2, "text": "looking", "thinking": "",
            "redacted": 0, "tool_uses": [{"id": "t1", "name": "read", "input": {"path": "x"}}],
        })
    );
    assert_eq!(
        out[2],
        serde_json::json!({
            "type": "tool-call-started", "id": "t1", "name": "read",
            "input": {"path": "x"}, "origin": "model",
        })
    );
    assert_eq!(
        out[3],
        serde_json::json!({
            "type": "tool-call-finished", "record": 3, "id": "t1", "name": "",
            "output": "x", "is_error": false,
        })
    );
    assert_eq!(
        out[4],
        serde_json::json!({
            "type": "turn-settled", "stop_reason": "end-turn",
            "usage": {"input_tokens": 0, "output_tokens": 0, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0},
        })
    );
}
