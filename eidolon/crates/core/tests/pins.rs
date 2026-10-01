//! **A journaled enum grows at the end — and this is what enforces it.**
//!
//! bitcode writes a `RecordKind` with its variant's ordinal in front, so a
//! variant added anywhere but the end re-reads every older log's records
//! below it as something else. Nothing in the build says so, and nothing
//! in the other tests can: they write their logs with the binary that
//! reads them, and a round trip always agrees with itself. What caught the
//! one that happened (2026-09-06: `TurnSpend` put after `ContextSize`,
//! which read as the end of the list and was not, so every `PersonaPinned`
//! on disk came back as `Pinned` and every persona session died at its
//! second record) was a session log on disk. `cargo test` was green the
//! whole time.
//!
//! So these bytes stand in for the logs on disk. Each row is one record as
//! a build at the committed ordering wrote it, and it must decode to the
//! record that wrote it *and* be what the current build writes for that
//! record. **To add a variant:** put it last in the enum, give it its
//! ordinal in [`ordinal`] — the match is exhaustive on purpose, so the
//! build refuses a variant nobody placed — encode one instance, and add a
//! row. **Never change an existing row.** A row that has to change is a
//! log that can no longer be read.

use eidolon_core::message::{ContentBlock, Json, Message, StopReason, Usage};
use eidolon_core::session::{PolicyOutcome, RecordKind};
use eidolon_core::usage::TurnTiming;
use eidolon_core::workflow::RunStatus;

/// Where each variant sits. Exhaustive: a variant missing here does not
/// compile, which is the moment to ask where it goes (the end) and to pin
/// it below.
fn ordinal(k: &RecordKind) -> u8 {
    use RecordKind::*;
    match k {
        SessionStart { .. } => 0,
        UserMessage(_) => 1,
        AssistantMessage(_) => 2,
        ToolResult { .. } => 3,
        TurnSettled { .. } => 4,
        AskUser { .. } => 5,
        Cancelled => 6,
        ModelChanged { .. } => 7,
        Compacted { .. } => 8,
        BackendSession { .. } => 9,
        UserToolCall { .. } => 10,
        Note { .. } => 11,
        PeerMessage { .. } => 12,
        ContextSize { .. } => 13,
        PolicyVerdict { .. } => 14,
        Excluded { .. } => 15,
        SessionNote { .. } => 16,
        Pinned { .. } => 17,
        PersonaPinned { .. } => 18,
        TurnSpend { .. } => 19,
        TurnBudget { .. } => 20,
        ExternalMessage { .. } => 21,
        TurnPace { .. } => 22,
        CommandResults { .. } => 23,
        TriggerFired { .. } => 24,
        WorkflowStart { .. } => 25,
        WorkflowStep { .. } => 26,
        WorkflowEnd { .. } => 27,
        ModelSpend { .. } => 28,
        ApiRequest { .. } => 29,
        WorkflowBudget { .. } => 30,
        PeerAnswer { .. } => 31,
        OperatorMessage { .. } => 32,
        TurnFailed { .. } => 33,
    }
}

fn usage() -> Usage {
    Usage {
        input_tokens: 5,
        output_tokens: 3,
        cache_creation_input_tokens: 1,
        cache_read_input_tokens: 2,
    }
}

/// One record per variant and the bytes it was written as. The first
/// nineteen were captured from a build at commit `152be41`; `TurnSpend`
/// from the build that added it, at the end. `TurnPace` likewise, from the
/// build that added it (2026-09-12), `CommandResults` from the build that
/// added it (2026-09-14), `TriggerFired` from the build that added it
/// (2026-09-22), and the three `Workflow*` records from the build that added
/// them (2026-09-27). `ModelSpend` likewise, appended after the workflow
/// records (2026-09-27). `ApiRequest` likewise, appended after `ModelSpend`
/// (2026-09-27). `WorkflowBudget` likewise, appended after `ApiRequest` —
/// the caller's per-run budget, journaled so a resume reads its
/// constraint from the branch (2026-09-27). `PeerAnswer` likewise,
/// appended after `WorkflowBudget` — the fourth producer: an accepted
/// peer answer's provenance (2026-09-27). `TurnFailed` likewise, appended
/// after `OperatorMessage` — a provider-killed turn's row (2026-09-29).
fn pinned() -> Vec<(RecordKind, &'static str)> {
    vec![
        (
            RecordKind::SessionStart {
                model: "m".into(),
                cwd: "/w".into(),
                system: Some("s".into()),
            },
            "00016d022f77010173",
        ),
        (
            RecordKind::UserMessage(Message::user_text("hi")),
            "01000100026869",
        ),
        (
            RecordKind::AssistantMessage(Message::assistant(vec![ContentBlock::text("ok")])),
            "02010100026f6b",
        ),
        (
            RecordKind::ToolResult {
                tool_use_id: "t1".into(),
                content: "out".into(),
                is_error: false,
            },
            "03027431036f757400",
        ),
        (
            RecordKind::TurnSettled {
                stop_reason: StopReason::EndTurn,
                usage: usage(),
            },
            "04000605060306010602",
        ),
        (
            RecordKind::AskUser {
                call_id: "c".into(),
                prompt: "p?".into(),
                answer: Some("a".into()),
            },
            "05016302703f010161",
        ),
        (RecordKind::Cancelled, "06"),
        (RecordKind::ModelChanged { model: "m2".into() }, "07026d32"),
        (
            RecordKind::Compacted {
                summary: "sum".into(),
                replaced_messages: 4,
            },
            "080373756d0404",
        ),
        (
            RecordKind::BackendSession {
                backend: "claude-cli".into(),
                id: "sid".into(),
            },
            "090a636c617564652d636c6903736964",
        ),
        (
            RecordKind::UserToolCall {
                tool_use_id: "u1".into(),
                name: "bash".into(),
                input: Json("{}".into()),
                utterance: Some("ls".into()),
            },
            "0a0275310462617368027b7d01026c73",
        ),
        (RecordKind::Note { text: "n".into() }, "0b016e"),
        (
            RecordKind::PeerMessage {
                from: "eidolon-1".into(),
                from_cwd: "/p".into(),
                channel: None,
                text: "hey".into(),
            },
            "0c096569646f6c6f6e2d31022f700003686579",
        ),
        (RecordKind::ContextSize { tokens: 555 }, "0d042b02"),
        (
            RecordKind::PolicyVerdict {
                tool_use_id: "v1".into(),
                tool: "bash".into(),
                reason: "r".into(),
                structural: true,
                outcome: PolicyOutcome::Approved,
                note: None,
            },
            "0e02763104626173680172010100",
        ),
        (
            RecordKind::Excluded {
                target: 7,
                excluded: true,
            },
            "0f060701",
        ),
        (RecordKind::SessionNote { text: "sn".into() }, "1002736e"),
        (
            RecordKind::Pinned {
                path: "wiki/x.md".into(),
                pinned: true,
            },
            "110977696b692f782e6d6401",
        ),
        (
            RecordKind::PersonaPinned {
                persona: "μέλι".into(),
            },
            "1208cebcceadcebbceb9",
        ),
        (
            RecordKind::TurnSpend {
                usage: usage(),
                calls: 2,
            },
            "1306050603060106020402",
        ),
        (
            RecordKind::TurnBudget { calls_left: 4 },
            "140404",
        ),
        (
            RecordKind::ExternalMessage {
                from: "aoide".into(),
                channel: None,
                text: "rebased".into(),
            },
            "1505616f696465000772656261736564",
        ),
        (
            RecordKind::TurnPace {
                timing: TurnTiming {
                    calls: 2,
                    first_token_ms: Some(300),
                    decode_ms: 3_000,
                    total_ms: 3_500,
                },
                output_tokens: 300,
            },
            "16040201042c0104b80b04ac0d042c01",
        ),
        (
            RecordKind::CommandResults {
                lines: vec!["[vv] read_note title=\"G\" → ok: eggs".into()],
            },
            "1701255b76765d20726561645f6e6f7465207469746c653d22472220e28692206f6b3a2065676773",
        ),
        (
            RecordKind::TriggerFired {
                call_id: "t1".into(),
                condition: "the build finished".into(),
                outcome: "it fired".into(),
            },
            "1802743112746865206275696c642066696e6973686564086974206669726564",
        ),
        (
            RecordKind::WorkflowStart {
                plugin: "jira".into(),
                workflow: "triage".into(),
                version: "0.1.0".into(),
                hash: "ab12".into(),
                args: Json("{\"since\":\"2026-09-01\"}".into()),
            },
            "19046a6972610674726961676505302e312e300461623132167b2273696e6365223a22323032362d30392d3031227d",
        ),
        (
            RecordKind::WorkflowStep {
                run: 3,
                index: 0,
                call_id: "w3-0".into(),
                tool: "jira_search".into(),
                input: Json("{\"jql\":\"x\"}".into()),
            },
            "1a060304000477332d300b6a6972615f7365617263680b7b226a716c223a2278227d",
        ),
        (
            RecordKind::WorkflowEnd {
                run: 3,
                status: RunStatus::Parked,
            },
            "1b060302",
        ),
        (
            RecordKind::ModelSpend {
                key: "mock".into(),
                call_id: "w3-1".into(),
                usage: usage(),
            },
            "1c046d6f636b0477332d310605060306010602",
        ),
        (
            RecordKind::ApiRequest {
                consumer: "plugin:browser".into(),
                service: "api.example.com".into(),
            },
            "1d0e706c7567696e3a62726f777365720f6170692e6578616d706c652e636f6d",
        ),
        (
            RecordKind::WorkflowBudget {
                run: 3,
                steps: Some(2),
                spend_micros: Some(500_000),
            },
            "1e0603010402010220a10700",
        ),
        (
            RecordKind::PeerAnswer {
                call_id: "w1-park0".into(),
                peer: "eidolon-aaaa".into(),
            },
            "1f0877312d7061726b300c6569646f6c6f6e2d61616161",
        ),
        (
            RecordKind::OperatorMessage {
                from: "noah".into(),
                text: "come back".into(),
            },
            "20046e6f616809636f6d65206261636b",
        ),
        (
            RecordKind::TurnFailed {
                error: "gw returned HTTP 502".into(),
            },
            "211467772072657475726e6564204854545020353032",
        ),
    ]
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

/// Every record ever written still reads as what wrote it, and is still
/// written the same way.
#[test]
fn every_record_kind_still_reads_as_it_was_written() {
    for (kind, bytes) in pinned() {
        let bytes = hex(bytes);
        let back: RecordKind =
            bitcode::decode(&bytes).unwrap_or_else(|e| panic!("{kind:?} no longer decodes: {e}"));
        assert_eq!(back, kind, "a log on disk now reads as a different record");
        assert_eq!(
            bitcode::encode(&kind),
            bytes,
            "{kind:?} is now written differently, so the log format moved"
        );
    }
}

/// The ordinal is the first byte, and the table above is the order on
/// disk. A variant added anywhere but the end fails here for every
/// variant it pushed down.
#[test]
fn a_variant_sits_where_the_table_says() {
    let rows = pinned();
    for (i, (kind, _)) in rows.iter().enumerate() {
        assert_eq!(
            ordinal(kind) as usize,
            i,
            "{kind:?} is pinned out of order in this file"
        );
        assert_eq!(
            bitcode::encode(kind)[0],
            ordinal(kind),
            "{kind:?} is not at the ordinal the table says"
        );
    }
}
