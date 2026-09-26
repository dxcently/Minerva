//! End-to-end: scripted model → tool call → dispatch → journal → settled,
//! then crash-resume on the same log.

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::future::BoxFuture;
use serde_json::json;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use eidolon_core::policy::{AllowAll, Approval};
use eidolon_core::session::RecordKind;
use eidolon_core::testing::AlwaysAsk;
use eidolon_core::testing::{ScriptedProvider, ScriptedUser};
use eidolon_core::user::tools::ChoicesUser;
use eidolon_core::*;

struct Echo(ToolManifest);

#[async_trait]
impl Tool for Echo {
    fn manifest(&self) -> &ToolManifest {
        &self.0
    }
    async fn call(&self, input: serde_json::Value, _: CallContext) -> anyhow::Result<ToolOutput> {
        Ok(ToolOutput::ok(format!(
            "echo:{}",
            input["s"].as_str().unwrap_or("")
        )))
    }
}

fn echo(approval: Approval) -> Arc<dyn Tool> {
    Arc::new(Echo(ToolManifest {
        name: "echo".into(),
        description: "echo".into(),
        input_schema: json!({"type":"object"}),
        approval,
        prompt: Some("guidance for echo".into()),
        render: None,
        deferred: false,
    }))
}

fn harness(
    dir: &std::path::Path,
    provider: Arc<ScriptedProvider>,
    policy: Arc<dyn PolicyHook>,
    user: Arc<dyn UserIo>,
    approval: Approval,
    resume: bool,
) -> (Agent, Arc<Mutex<Session>>) {
    let path = dir.join("s.log");
    let session = if resume {
        Session::open(&path).unwrap()
    } else {
        Session::create(&path, "m", dir, Some("sys".into())).unwrap()
    };
    let session = Arc::new(Mutex::new(session));
    let mut reg = ToolRegistry::new();
    reg.register(echo(approval));
    reg.register(Arc::new(ChoicesUser::new(user.clone())));
    let d = Arc::new(Dispatcher::new(
        reg,
        policy,
        user,
        EventBus::default(),
        session.clone(),
        dir.to_path_buf(),
    ));
    let agent = Agent::new(
        provider,
        d,
        AgentConfig {
            model: "m".into(),
            system: Some("sys".into()),
            ..Default::default()
        },
    );
    (agent, session)
}

/// Every request in a turn carries the session's cache preferences, and
/// the key is stable across the tool loop — an endpoint that shards its
/// cache routes on it, so a key that changed between iterations would
/// send the second request to a machine that has never seen the first.
#[tokio::test]
async fn every_request_carries_the_sessions_cache_key() {
    use eidolon_core::provider::CacheRetention;
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::tool("t1", "echo", json!({"s":"hi"})),
        ScriptedProvider::text("done"),
    ]);
    let user = ScriptedUser::new(true);
    let (agent, _s) = harness(
        dir.path(),
        provider.clone(),
        Arc::new(AllowAll),
        user,
        Approval::ReadOnly,
        false,
    );
    agent
        .run_turn(vec![ContentBlock::text("go")], CancellationToken::new())
        .await
        .unwrap();

    let reqs = provider.requests.lock().unwrap();
    assert_eq!(reqs.len(), 2);
    for r in reqs.iter() {
        assert_eq!(
            r.cache.key.as_deref(),
            Some("s"),
            "the session log's own name"
        );
        assert_eq!(r.cache.retention, CacheRetention::Short);
    }
}

#[tokio::test]
async fn tool_loop_settles_and_journals() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::tool("t1", "echo", json!({"s":"hi"})),
        ScriptedProvider::text("done"),
    ]);
    let user = ScriptedUser::new(true);
    let (agent, session) = harness(
        dir.path(),
        provider.clone(),
        Arc::new(AllowAll),
        user,
        Approval::ReadOnly,
        false,
    );
    let out = agent
        .run_turn(vec![ContentBlock::text("go")], CancellationToken::new())
        .await
        .unwrap();
    assert!(matches!(
        out,
        TurnOutcome::Settled {
            stop_reason: StopReason::EndTurn,
            ..
        }
    ));

    // Second request carried the tool result back.
    {
        let reqs = provider.requests.lock().unwrap();
        assert_eq!(reqs.len(), 2);
        let last = reqs[1].messages.last().unwrap();
        assert!(
            matches!(&last.content[0], ContentBlock::ToolResult { content, .. } if content == "echo:hi")
        );
    }

    let s = session.lock().await;
    assert!(s.is_settled());
    let kinds: Vec<&str> = s
        .branch()
        .iter()
        .map(|r| match &r.kind {
            RecordKind::SessionStart { .. } => "start",
            RecordKind::UserMessage(_) => "user",
            RecordKind::AssistantMessage(_) => "assistant",
            RecordKind::ToolResult { .. } => "result",
            RecordKind::ContextSize { .. } => "context",
            RecordKind::TurnPace { .. } => "pace",
            RecordKind::TurnSettled { .. } => "settled",
            _ => "other",
        })
        .collect();
    // `context` sits just before `settled`, and carries the *last* call's
    // input where `settled` carries the turn's total across both calls.
    // `pace` sits between them: the same two calls, timed.
    assert_eq!(
        kinds,
        [
            "start",
            "user",
            "assistant",
            "result",
            "assistant",
            "context",
            "pace",
            "settled"
        ]
    );
    assert_eq!(s.usage().output_tokens, 2);
}

/// A command channel that answers any line beginning `! ` — the core's
/// tests are about *when* the answers land, so the marker here is a plain
/// stand-in for the real channel's and the answering is immediate.
struct Answers;

#[async_trait]
impl CommandChannel for Answers {
    async fn answer(&self, message: &Message, _: CancellationToken) -> Option<Vec<String>> {
        let lines: Vec<String> = message
            .text()
            .lines()
            .filter_map(|l| l.strip_prefix("! "))
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .map(|c| format!("[vv] {c} -> ok: 42"))
            .collect();
        (!lines.is_empty()).then_some(lines)
    }
}

/// The whole of the inline channel's loop behaviour: a reply that marks
/// commands is owed their answers *before the turn settles* — journaled as
/// the harness's words, given to the model on one more call, and answered
/// from in the same turn.
#[tokio::test]
async fn commands_written_in_prose_are_answered_before_the_turn_settles() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::text("let me look it up.\n! read the note G\n"),
        ScriptedProvider::text("it has eggs in it."),
    ]);
    let (agent, _s) = harness(
        dir.path(),
        provider.clone(),
        Arc::new(AllowAll),
        ScriptedUser::new(true),
        Approval::ReadOnly,
        false,
    );
    agent.set_command_channel(Arc::new(Answers));
    let out = agent
        .run_turn(vec![ContentBlock::text("what is on the list?")], CancellationToken::new())
        .await
        .unwrap();
    assert!(
        matches!(
            out,
            TurnOutcome::Settled {
                stop_reason: StopReason::EndTurn,
                ..
            }
        ),
        "{out:?}"
    );

    // The continuation carries the answer, framed as the harness — not the
    // operator — and the turn settled once, at the end of it.
    let reqs = provider.requests.lock().unwrap();
    assert_eq!(reqs.len(), 2, "one call for the command, one to answer from it");
    let second = reqs[1]
        .messages
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(second.contains("[vv] read the note G -> ok: 42"), "{second}");
    assert!(
        second.contains("From the harness — not the operator"),
        "the answers must not read as the operator speaking: {second}"
    );
    drop(reqs);

    // Journaled, in the one place it could be: between the reply that asked
    // and the reply that answered, with the turn settled at the end.
    let s = Session::open(&dir.path().join("s.log")).unwrap();
    let kinds: Vec<&str> = s
        .branch()
        .iter()
        .map(|r| match &r.kind {
            RecordKind::SessionStart { .. } => "start",
            RecordKind::UserMessage(_) => "user",
            RecordKind::AssistantMessage(_) => "assistant",
            RecordKind::CommandResults { .. } => "answers",
            RecordKind::TurnSettled { .. } => "settled",
            _ => "other",
        })
        .collect();
    assert_eq!(
        kinds,
        [
            "start",
            "user",
            "assistant",
            "answers",
            "assistant",
            "other",
            "other",
            "settled"
        ]
    );
    assert!(s.is_settled());
    // The model reads the same line back off the log as it read live.
    let replied = s
        .messages()
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(replied.contains("[vv] read the note G -> ok: 42"), "{replied}");
}

/// The first-exposure hedge: a reply that marks a command *and* makes a
/// tool call is still owed its answers — after the results it is owed,
/// never in front of them. Silence here is what teaches a model the
/// channel is dead: observed live, one hedged `! list notes` was never
/// answered, and the model apologised for the broken channel and fell
/// back to RPC for the rest of the session.
#[tokio::test]
async fn a_hedged_reply_is_answered_after_its_results() {
    let dir = tempfile::tempdir().unwrap();
    // One reply, two moves: the marked line, then the ordinary call.
    let mut hedge = ScriptedProvider::text("! list notes");
    hedge.extend(ScriptedProvider::tool("t1", "echo", json!({"s": "hi"})));
    let provider = ScriptedProvider::new(vec![hedge, ScriptedProvider::text("all done.")]);
    let (agent, _s) = harness(
        dir.path(),
        provider.clone(),
        Arc::new(AllowAll),
        ScriptedUser::new(true),
        Approval::ReadOnly,
        false,
    );
    agent.set_command_channel(Arc::new(Answers));
    let out = agent
        .run_turn(vec![ContentBlock::text("what is in the vault?")], CancellationToken::new())
        .await
        .unwrap();
    assert!(
        matches!(out, TurnOutcome::Settled { stop_reason: StopReason::EndTurn, .. }),
        "{out:?}"
    );

    // The next model call reads the tool result and the command's answer
    // together — the result first, the answer behind it, never ahead.
    let reqs = provider.requests.lock().unwrap();
    assert_eq!(reqs.len(), 2, "the hedge earns its ordinary second call, no more");
    let second = reqs[1]
        .messages
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.clone()),
            ContentBlock::ToolResult { content, .. } => Some(content.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let result_at = second
        .find("echo:hi")
        .expect("the tool's result is owed and present");
    let answer_at = second
        .find("[vv] list notes -> ok: 42")
        .expect("the marked line was answered");
    assert!(result_at < answer_at, "answers land after results: {second}");
    drop(reqs);

    // And on the log, the `CommandResults` sits after the `ToolResult` it
    // followed, not in front of it.
    let s = Session::open(&dir.path().join("s.log")).unwrap();
    let branch = s.branch();
    let tool_at = branch
        .iter()
        .position(|r| matches!(r.kind, RecordKind::ToolResult { .. }))
        .expect("tool result journaled");
    let answers_at = branch
        .iter()
        .position(|r| matches!(r.kind, RecordKind::CommandResults { .. }))
        .expect("the hedge was answered");
    assert!(tool_at < answers_at, "answers journal after results");
    assert!(s.is_settled());
}

/// A reply with nothing marked settles as it always did: no channel, no
/// answer, no extra call.
#[tokio::test]
async fn a_reply_that_marks_nothing_gets_no_answer_and_no_extra_call() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![ScriptedProvider::text("eggs and milk.")]);
    let (agent, _s) = harness(
        dir.path(),
        provider.clone(),
        Arc::new(AllowAll),
        ScriptedUser::new(true),
        Approval::ReadOnly,
        false,
    );
    agent.set_command_channel(Arc::new(Answers));
    agent
        .run_turn(vec![ContentBlock::text("what is on the list?")], CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
    let s = Session::open(&dir.path().join("s.log")).unwrap();
    assert!(
        !s.branch()
            .iter()
            .any(|r| matches!(r.kind, RecordKind::CommandResults { .. })),
        "nothing marked, nothing answered"
    );
}

/// A truncation is not an end of turn: half a command is a wrong answer
/// confidently given, so the channel is not asked at all.
#[tokio::test]
async fn a_reply_that_ran_out_of_room_is_not_asked_for_commands() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![vec![
        StreamEvent::TextDelta("! read the note G".into()),
        StreamEvent::BlockStop,
        StreamEvent::Stop {
            stop_reason: StopReason::MaxTokens,
            usage: Usage {
                output_tokens: 1,
                ..Default::default()
            },
        },
    ]]);
    let (agent, _s) = harness(
        dir.path(),
        provider.clone(),
        Arc::new(AllowAll),
        ScriptedUser::new(true),
        Approval::ReadOnly,
        false,
    );
    agent.set_command_channel(Arc::new(Answers));
    agent
        .run_turn(vec![ContentBlock::text("go")], CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(provider.requests.lock().unwrap().len(), 1, "no continuation");
    let s = Session::open(&dir.path().join("s.log")).unwrap();
    assert!(
        !s.branch()
            .iter()
            .any(|r| matches!(r.kind, RecordKind::CommandResults { .. }))
    );
}

/// The message assembly the answers depend on: framed as the harness, and a
/// steer delivered right behind them rides them rather than opening a
/// second user turn in a row — which a strict endpoint refuses outright.
#[test]
fn the_harnesss_answers_say_so_and_a_steer_rides_them() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::create(&dir.path().join("s.eid"), "m", dir.path(), None).unwrap();
    s.append(RecordKind::UserMessage(Message::user_text("what is on the list?")))
        .unwrap();
    s.append(RecordKind::AssistantMessage(Message::assistant(vec![
        ContentBlock::text("! read the note G"),
    ])))
    .unwrap();
    s.append(RecordKind::CommandResults {
        lines: vec!["[vv] read_note title=\"G\" → ok: eggs".into()],
    })
    .unwrap();
    s.append(RecordKind::UserMessage(Message::user_text("and the milk?")))
        .unwrap();
    s.append(RecordKind::AssistantMessage(Message::assistant(vec![
        ContentBlock::text("eggs and milk."),
    ])))
    .unwrap();

    let msgs = s.messages();
    assert_eq!(msgs.len(), 4, "{msgs:?}");
    assert_eq!(msgs[2].role, Role::User);
    let framed = msgs[2].text();
    assert!(framed.contains("From the harness — not the operator"), "{framed}");
    assert!(framed.contains("[vv] read_note title=\"G\" → ok: eggs"), "{framed}");
    assert!(
        framed.contains("and the milk?"),
        "the steer rides the answers rather than becoming a second user turn: {framed}"
    );
    assert_eq!(msgs[3].text(), "eggs and milk.");
}

#[tokio::test]
async fn policy_ask_declined_becomes_error_result() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::tool("t1", "echo", json!({"s":"hi"})),
        ScriptedProvider::text("ok"),
    ]);
    let user = ScriptedUser::new(false);
    let (agent, session) = harness(
        dir.path(),
        provider.clone(),
        Arc::new(AlwaysAsk),
        user.clone(),
        Approval::Mutating,
        false,
    );
    agent
        .run_turn(vec![ContentBlock::text("go")], CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(user.asked.lock().unwrap().len(), 1);
    let s = session.lock().await;
    let msgs = s.messages();
    assert!(
        matches!(&msgs[2].content[0], ContentBlock::ToolResult { is_error: true, content, .. } if content.contains("declined"))
    );
}

#[tokio::test]
async fn resume_replays_journaled_results_and_asks() {
    let dir = tempfile::tempdir().unwrap();
    // First run: the model asks the user, calls echo, and says bye.
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::tool(
            "a1",
            "choices_user",
            json!({"prompt":"deploy how?","options":[
                {"label":"staging","description":"the safe pick"},
                {"label":"prod","description":"the risky pick"}
            ]}),
        ),
        ScriptedProvider::tool("t1", "echo", json!({"s":"x"})),
        ScriptedProvider::text("bye"),
    ]);
    let user = ScriptedUser::new(true);
    let (agent, session) = harness(
        dir.path(),
        provider,
        Arc::new(AllowAll),
        user.clone(),
        Approval::ReadOnly,
        false,
    );
    agent
        .run_turn(vec![ContentBlock::text("go")], CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(user.asked.lock().unwrap().len(), 1);
    drop(agent);
    drop(session);

    // Chop the log back to just after the second assistant message (the
    // echo call), as if we crashed before its result was journaled.
    let path = dir.path().join("s.log");
    {
        let s = Session::open(&path).unwrap();
        let recs = s.records();
        // start, user, assistant(ask), result(ask), assistant(echo),
        // result(echo), assistant(bye), context, pace, settled
        assert_eq!(recs.len(), 10);
    }
    truncate_to_records(&path, 5);

    // Resume with a provider that only has the final reply; the echo tool
    // runs live (it was never journaled), the ask is NOT re-asked.
    let provider = ScriptedProvider::new(vec![ScriptedProvider::text("bye again")]);
    let user2 = ScriptedUser::new(true);
    let (agent, session) = harness(
        dir.path(),
        provider.clone(),
        Arc::new(AllowAll),
        user2.clone(),
        Approval::ReadOnly,
        true,
    );
    assert!(!session.lock().await.is_settled());
    let out = agent.continue_turn(CancellationToken::new()).await.unwrap();
    assert!(matches!(out, TurnOutcome::Settled { .. }));
    assert_eq!(
        user2.asked.lock().unwrap().len(),
        0,
        "choices_user must not re-prompt on resume"
    );
    let reqs = provider.requests.lock().unwrap();
    let msgs = &reqs[0].messages;
    // The ask's journaled answer (the scripted user picks the first
    // option) is in the conversation, and echo ran live.
    assert!(msgs.iter().any(|m| {
        m.content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolResult { content, .. } if content == "staging"))
    }));
    assert!(msgs.iter().any(|m| {
        m.content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolResult { content, .. } if content == "echo:x"))
    }));
}

/// Exiting the question without answering ends the turn: the dismissal is
/// journaled like any result, the settle names it, and the model is not
/// called again to be told what the operator's esc already said.
#[tokio::test]
async fn dismissed_question_ends_the_turn() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::tool(
            "a1",
            "choices_user",
            json!({"prompt":"deploy how?","options":["staging","prod"]}),
        ),
        // Only reached if the loop wrongly calls the model after the
        // dismissal.
        ScriptedProvider::text("unreached"),
    ]);
    // `false`: the scripted user declines to answer — the esc on the dialog.
    let user = ScriptedUser::new(false);
    let (agent, session) = harness(
        dir.path(),
        provider.clone(),
        Arc::new(AllowAll),
        user.clone(),
        Approval::ReadOnly,
        false,
    );
    let out = agent
        .run_turn(vec![ContentBlock::text("go")], CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(user.asked.lock().unwrap().len(), 1);
    assert!(
        matches!(out, TurnOutcome::Settled { stop_reason: StopReason::Dismissed, .. }),
        "a dismissed question settles the turn: {out:?}"
    );
    assert_eq!(
        provider.requests.lock().unwrap().len(),
        1,
        "no model call after the dismissal"
    );
    {
        let s = session.lock().await;
        let kinds: Vec<&RecordKind> = s.records().iter().map(|r| &r.kind).collect();
        assert!(matches!(
            kinds.last(),
            Some(RecordKind::TurnSettled { stop_reason: StopReason::Dismissed, .. })
        ));
        // The call is answered, and honestly: not an error, because
        // nothing failed.
        assert!(kinds.iter().any(|k| matches!(
            k,
            RecordKind::ToolResult { content, is_error: false, .. }
                if content.contains("dismissed")
        )));
    }
    assert!(session.lock().await.is_settled());
    drop(agent);
    drop(session);

    // The operator's next message is an ordinary turn, and the model reads
    // why the last one stopped. A replayed dismissal never re-prompts.
    let provider = ScriptedProvider::new(vec![ScriptedProvider::text("heard")]);
    let user2 = ScriptedUser::new(true);
    let (agent, _session) = harness(
        dir.path(),
        provider.clone(),
        Arc::new(AllowAll),
        user2.clone(),
        Approval::ReadOnly,
        true,
    );
    agent.continue_turn(CancellationToken::new()).await.unwrap();
    assert_eq!(
        user2.asked.lock().unwrap().len(),
        0,
        "a dismissed question must not re-prompt on resume"
    );
    let msgs = &provider.requests.lock().unwrap()[0].messages;
    assert!(msgs.iter().any(|m| {
        m.content
            .iter()
            .any(|b| matches!(b, ContentBlock::ToolResult { content, is_error: false, .. } if content.contains("dismissed")))
    }));
}

/// Rewrite the file keeping only the first `n` records.
fn truncate_to_records(path: &std::path::Path, n: usize) {
    let bytes = std::fs::read(path).unwrap();
    let mut pos = 6;
    for _ in 0..n {
        let len = u32::from_le_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
            as usize;
        pos += 8 + len;
    }
    std::fs::write(path, &bytes[..pos]).unwrap();
}

#[tokio::test]
async fn compaction_restarts_history_and_keeps_log() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::text("first reply"),
        ScriptedProvider::text("SUMMARY OF EVERYTHING"),
        ScriptedProvider::text("after"),
    ]);
    let user = ScriptedUser::new(true);
    let (agent, session) = harness(
        dir.path(),
        provider.clone(),
        Arc::new(AllowAll),
        user,
        Approval::ReadOnly,
        false,
    );
    agent
        .run_turn(vec![ContentBlock::text("one")], CancellationToken::new())
        .await
        .unwrap();
    assert!(agent.compact(CancellationToken::new()).await.unwrap());
    // The compaction journals the summary's measured size as the new
    // context; the scripted summariser reports one output token.
    assert_eq!(session.lock().await.last_input_tokens(), Some(1));
    agent
        .run_turn(vec![ContentBlock::text("two")], CancellationToken::new())
        .await
        .unwrap();

    {
        let reqs = provider.requests.lock().unwrap();
        // The compaction request carried the whole history plus the instruction, no tools.
        assert_eq!(reqs[1].messages.len(), 3);
        assert!(reqs[1].tools.is_empty());
        // The next real request starts from the summary.
        let m = &reqs[2].messages;
        assert_eq!(m.len(), 2);
        assert!(m[0].text().contains("SUMMARY OF EVERYTHING"));
        assert_eq!(m[1].text(), "two");
    }

    let s = session.lock().await;
    assert!(
        s.records()
            .iter()
            .any(|r| matches!(r.kind, RecordKind::UserMessage(ref m) if m.text() == "one"))
    );
    // Forking below the compaction point sees the full history again.
    let tree = s.tree();
    assert!(tree.iter().filter(|n| n.on_branch).count() >= 5);
}

/// The settle a compaction journals goes out on the bus with it.
///
/// It is the one settle that used not to: the record was written and
/// published nothing, so a consumer that counts what a session spent off the
/// bus — the TUI's ledger — priced the summariser's call as free until the
/// next turn settled, while a resume read the record and priced it at once.
/// `timing` is dropped with it because no `TurnPace` record is written, so a
/// live consumer showing a pace would be showing a number the log has not.
#[tokio::test]
async fn a_compaction_publishes_the_settle_it_journals() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::text("first reply"),
        ScriptedProvider::text("SUMMARY OF EVERYTHING"),
    ]);
    let user = ScriptedUser::new(true);
    let (agent, _session) = harness(
        dir.path(),
        provider,
        Arc::new(AllowAll),
        user,
        Approval::ReadOnly,
        false,
    );
    let mut events = agent.bus().subscribe();
    agent
        .run_turn(vec![ContentBlock::text("one")], CancellationToken::new())
        .await
        .unwrap();
    while events.try_recv().is_ok() {}

    assert!(agent.compact(CancellationToken::new()).await.unwrap());

    let mut seen = Vec::new();
    while let Ok(ev) = events.try_recv() {
        seen.push(ev);
    }
    assert!(
        seen.iter().any(|e| matches!(e, Event::Compacted { .. })),
        "the compaction itself is published"
    );
    let at = seen
        .iter()
        .position(|e| matches!(e, Event::TurnSettled { .. }))
        .expect("the settle the record was written beside is published too");
    match &seen[at] {
        Event::TurnSettled {
            stop_reason,
            usage,
            timing,
        } => {
            assert_eq!(*stop_reason, StopReason::EndTurn);
            assert_eq!(usage.output_tokens, 1, "the summariser's own tokens");
            assert!(timing.is_none(), "no `TurnPace` record was written beside it");
        }
        _ => unreachable!(),
    }
    assert!(
        seen.iter()
            .position(|e| matches!(e, Event::Compacted { .. }))
            .is_some_and(|c| c < at),
        "in the order the records were journaled"
    );
}

#[tokio::test]
async fn fork_and_model_change_are_journaled() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::text("a"),
        ScriptedProvider::text("b"),
    ]);
    let user = ScriptedUser::new(true);
    let (agent, session) = harness(
        dir.path(),
        provider.clone(),
        Arc::new(AllowAll),
        user,
        Approval::ReadOnly,
        false,
    );
    agent
        .run_turn(vec![ContentBlock::text("one")], CancellationToken::new())
        .await
        .unwrap();
    let user_rec = session
        .lock()
        .await
        .branch()
        .iter()
        .find(|r| matches!(r.kind, RecordKind::UserMessage(_)))
        .unwrap()
        .id;
    // Change the model, then fork below the change: the fork must re-journal
    // it on the new branch, or a resume would silently revert to "m".
    agent
        .set_model("other".into(), "other".into())
        .await
        .unwrap();
    agent.fork_at(user_rec).await.unwrap();
    agent
        .run_turn(vec![ContentBlock::text("two")], CancellationToken::new())
        .await
        .unwrap();
    let s = session.lock().await;
    assert_eq!(s.model().as_deref(), Some("other"));
    assert!(
        s.branch()
            .iter()
            .filter(|r| matches!(r.kind, RecordKind::ModelChanged { .. }))
            .count()
            == 1
    );
    let msgs = s.messages();
    // Branch: one → (fork) → two; the first assistant reply is off-branch.
    assert_eq!(
        msgs.iter().map(|m| m.text()).collect::<Vec<_>>(),
        ["one", "two", "b"]
    );
    assert_eq!(provider.requests.lock().unwrap()[1].model, "other");
    let tree = s.tree();
    assert!(
        tree.iter().any(|n| n.children.len() == 2),
        "the user record should have two children"
    );
}

/// A tool's `prompt` reaches the model's system prompt, and only while that
/// tool is registered. Without this the field is declared, documented and
/// silently discarded.
#[tokio::test]
async fn tool_guidance_reaches_the_system_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![ScriptedProvider::text("done")]);
    let user = ScriptedUser::new(true);
    let (agent, _session) = harness(
        dir.path(),
        provider.clone(),
        Arc::new(AllowAll),
        user,
        Approval::ReadOnly,
        false,
    );
    agent
        .run_turn(vec![ContentBlock::text("hi")], CancellationToken::new())
        .await
        .unwrap();

    let sent = provider.requests.lock().unwrap()[0]
        .system
        .clone()
        .expect("a system prompt");
    assert!(sent.contains("## Tool notes"), "{sent}");
    assert!(sent.contains("### echo"), "{sent}");
    assert!(sent.contains("guidance for echo"), "{sent}");
    // The configured prompt is kept, not replaced.
    assert!(sent.starts_with("sys"), "{sent}");
}

/// A backend that keeps its own conversation is caught up from its mark:
/// what it has already been told is not replayed, what happened on another
/// backend is. This is the whole of the "switching models loses the
/// context" fix on the core side.
#[test]
fn messages_after_a_backend_mark_are_what_it_has_not_heard() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::create(&dir.path().join("s.eid"), "prov:m", dir.path(), None).unwrap();
    s.append(RecordKind::UserMessage(Message::user_text("one")))
        .unwrap();
    s.append(RecordKind::AssistantMessage(Message::assistant(vec![
        ContentBlock::text("first answer"),
    ])))
    .unwrap();
    s.append(RecordKind::TurnSettled {
        stop_reason: StopReason::EndTurn,
        usage: Default::default(),
    })
    .unwrap();
    // No mark yet: the backend has heard nothing at all.
    assert_eq!(s.backend_session_mark("claude-cli"), None);
    assert_eq!(s.messages_after(None).len(), 2);

    // It runs a turn of its own and marks how far it is caught up.
    s.append(RecordKind::UserMessage(Message::user_text("two")))
        .unwrap();
    s.append(RecordKind::AssistantMessage(Message::assistant(vec![
        ContentBlock::text("second answer"),
    ])))
    .unwrap();
    s.append(RecordKind::TurnSettled {
        stop_reason: StopReason::EndTurn,
        usage: Default::default(),
    })
    .unwrap();
    let mark = s
        .append(RecordKind::BackendSession {
            backend: "claude-cli".into(),
            id: "sess".into(),
        })
        .unwrap();
    assert_eq!(
        s.backend_session_mark("claude-cli"),
        Some((mark, "sess".into()))
    );
    assert!(
        s.messages_after(Some(mark)).is_empty(),
        "everything so far is in its own session"
    );

    // A turn somewhere else, and it is behind again — by that turn only.
    s.append(RecordKind::UserMessage(Message::user_text("three")))
        .unwrap();
    s.append(RecordKind::AssistantMessage(Message::assistant(vec![
        ContentBlock::text("third answer"),
    ])))
    .unwrap();
    s.append(RecordKind::TurnSettled {
        stop_reason: StopReason::EndTurn,
        usage: Default::default(),
    })
    .unwrap();
    let texts: Vec<String> = s
        .messages_after(Some(mark))
        .iter()
        .map(|m| m.text())
        .collect();
    assert_eq!(texts, ["three", "third answer"]);

    // A mark the branch no longer holds repeats rather than loses.
    assert_eq!(s.messages_after(Some(9_999)).len(), s.messages().len());
}

/// A pinned file reaches the model in the system prompt, and never reaches
/// the log.
///
/// The trap the feature is built around is unchanged. The obvious
/// implementation — put the file on the user message — is wrong in the
/// expensive direction, because that message is *journaled*: every turn
/// would append another full copy of the file to the branch, `messages`
/// would replay all of them, and the log would grow by the pin's size per
/// turn. Only the declaration is journaled; the bytes are injected where
/// the message list becomes a request and nowhere else, which is also what
/// makes an edit to the file live rather than a stale paste.
///
/// Where that injection *is* changed. The block used to ride the newest user
/// message, on the theory that the cache miss would then be bounded to the
/// pin. It was not bounded to anything: the message list is rebuilt from the
/// journal on every call and the journal never holds a pin's bytes, so each
/// call found the block one message further back than the last — no two
/// consecutive requests were prefix-extensions of one another, and a prefix
/// cache forfeits everything behind the first changed byte. In the system
/// prompt it is at a fixed offset.
#[tokio::test]
async fn a_pin_reaches_the_model_and_never_the_log() {
    let dir = tempfile::tempdir().unwrap();
    let pinned = dir.path().join("conventions.md");
    std::fs::write(&pinned, "ALWAYS_SPELL_IT_THIS_WAY").unwrap();

    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::text("done"),
        ScriptedProvider::text("done again"),
    ]);
    let user = ScriptedUser::new(true);
    let (agent, _session) = harness(
        dir.path(),
        provider.clone(),
        Arc::new(AllowAll),
        user,
        Approval::ReadOnly,
        false,
    );
    assert!(
        agent
            .session()
            .lock()
            .await
            .set_pinned(pinned.to_str().unwrap(), true)
            .unwrap()
    );

    agent
        .run_turn(vec![ContentBlock::text("hi")], CancellationToken::new())
        .await
        .unwrap();

    // It is on the wire, in the system prompt, and not in the messages.
    let (system, messages) = {
        let r = provider.requests.lock().unwrap();
        (
            r[0].system.clone().unwrap_or_default(),
            r[0].messages.clone(),
        )
    };
    assert!(
        system.contains("ALWAYS_SPELL_IT_THIS_WAY"),
        "the pin reaches the model: {system}"
    );
    assert!(
        system.contains("conventions.md"),
        "and names the file it came from"
    );
    assert!(
        !messages
            .iter()
            .any(|m| m.text().contains("ALWAYS_SPELL_IT_THIS_WAY")),
        "it rides the system prompt, not a journaled message"
    );

    // And it is nowhere in the branch.
    let journaled = agent.session().lock().await.messages();
    assert!(
        !journaled
            .iter()
            .any(|m| m.text().contains("ALWAYS_SPELL_IT_THIS_WAY")),
        "the contents must never be journaled, or the branch grows by the file on every turn",
    );

    // An edit is live on the next turn rather than a stale paste, and the
    // old contents do not accumulate behind it.
    std::fs::write(&pinned, "THE_RULE_CHANGED").unwrap();
    agent
        .run_turn(vec![ContentBlock::text("again")], CancellationToken::new())
        .await
        .unwrap();
    let second = provider.requests.lock().unwrap()[1]
        .system
        .clone()
        .unwrap_or_default();
    assert!(
        second.contains("THE_RULE_CHANGED"),
        "the file is re-read each turn"
    );
    assert!(
        !second.contains("ALWAYS_SPELL_IT_THIS_WAY"),
        "and yesterday's contents are not still in the prompt"
    );
}

/// A pin does not move between calls, which is the whole reason it is in the
/// system prompt.
///
/// The regression this pins down: with the block appended to the newest user
/// message, turn two's messages no longer *extend* turn one's — the pin sat at
/// the end of turn one's user message and that message is rebuilt from the
/// journal without it — so a prefix cache kept only the header and re-read the
/// whole conversation on every call. A stable prefix is exactly the property
/// that fails silently, so it is asserted rather than assumed.
#[tokio::test]
async fn a_pin_does_not_move_the_prefix_between_turns() {
    let dir = tempfile::tempdir().unwrap();
    let pinned = dir.path().join("conventions.md");
    std::fs::write(&pinned, "SPELL_IT_THIS_WAY").unwrap();

    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::text("done"),
        ScriptedProvider::text("done again"),
    ]);
    let user = ScriptedUser::new(true);
    let (agent, _session) = harness(
        dir.path(),
        provider.clone(),
        Arc::new(AllowAll),
        user,
        Approval::ReadOnly,
        false,
    );
    agent
        .session()
        .lock()
        .await
        .set_pinned(pinned.to_str().unwrap(), true)
        .unwrap();

    agent
        .run_turn(vec![ContentBlock::text("one")], CancellationToken::new())
        .await
        .unwrap();
    agent
        .run_turn(vec![ContentBlock::text("two")], CancellationToken::new())
        .await
        .unwrap();

    let r = provider.requests.lock().unwrap();
    assert_eq!(r.len(), 2, "one call per turn");
    let first = r[0].system.clone().unwrap_or_default();
    assert!(
        first.contains("SPELL_IT_THIS_WAY"),
        "the pin is in the prefix being compared"
    );
    assert_eq!(
        r[0].system, r[1].system,
        "an unchanged pin leaves the front of the prompt byte-for-byte where it was",
    );
    let (a, b) = (r[0].messages.clone(), r[1].messages.clone());
    assert_eq!(
        b[..a.len()],
        a[..],
        "and the next request's messages still extend the last one's, so the entry is reusable",
    );
}

/// A pin that cannot be read says so, rather than vanishing.
///
/// Silence would leave the operator believing the model has the file and
/// the model reasoning without it — the worst of both, and exactly the case
/// (a path that moved) where the operator most needs telling.
#[tokio::test]
async fn a_pin_that_cannot_be_read_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![ScriptedProvider::text("done")]);
    let user = ScriptedUser::new(true);
    let (agent, _session) = harness(
        dir.path(),
        provider.clone(),
        Arc::new(AllowAll),
        user,
        Approval::ReadOnly,
        false,
    );
    agent
        .session()
        .lock()
        .await
        .set_pinned("/nonexistent/gone.md", true)
        .unwrap();
    agent
        .run_turn(vec![ContentBlock::text("hi")], CancellationToken::new())
        .await
        .unwrap();

    let system = provider.requests.lock().unwrap()[0]
        .system
        .clone()
        .unwrap_or_default();
    assert!(system.contains("gone.md"), "the pin is still named");
    assert!(
        system.contains("could not be read"),
        "and the failure is stated: {system}"
    );
}

/// The standing note reaches the system prompt, between the configured
/// prompt and the tools' guidance — a fixed position, because this string
/// is the cached prefix and a block that moved would miss on every turn.
#[tokio::test]
async fn the_session_note_sits_in_the_system_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::text("a"),
        ScriptedProvider::text("b"),
    ]);
    let user = ScriptedUser::new(true);
    let (agent, _session) = harness(
        dir.path(),
        provider.clone(),
        Arc::new(AllowAll),
        user,
        Approval::ReadOnly,
        false,
    );
    agent
        .session()
        .lock()
        .await
        .set_session_note("we are debugging the parser")
        .unwrap();
    agent
        .run_turn(vec![ContentBlock::text("hi")], CancellationToken::new())
        .await
        .unwrap();

    let sent = provider.requests.lock().unwrap()[0]
        .system
        .clone()
        .expect("a system prompt");
    let note_at = sent
        .find("we are debugging the parser")
        .expect("the note is there");
    assert!(sent.starts_with("sys"), "the configured prompt still leads");
    assert!(
        note_at < sent.find("## Tool notes").expect("guidance"),
        "the note sits above the tools' guidance"
    );

    // Cleared, it leaves nothing behind.
    agent.session().lock().await.set_session_note("").unwrap();
    agent
        .run_turn(vec![ContentBlock::text("more")], CancellationToken::new())
        .await
        .unwrap();
    let after = provider.requests.lock().unwrap()[1]
        .system
        .clone()
        .expect("a system prompt");
    assert!(!after.contains("we are debugging the parser"));
    assert!(
        after.contains("## Tool notes"),
        "and the guidance survives the clearing"
    );
}

/// Striking an assistant message takes the results of its calls with it.
///
/// This is the sharp edge of the whole feature. A `tool_result` block is
/// only well-formed against a `tool_use` the assistant actually made, so
/// dropping the message while keeping the answers would hand the model a
/// malformed turn — the same shape of failure the `PeerMessage` rule exists
/// to prevent mid-turn. The assembler gets this right by construction: a
/// message that is never pushed never opens `pending`, so the results find
/// no slot and go with it.
#[test]
fn striking_an_assistant_message_takes_its_tool_results_with_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::create(&dir.path().join("s.eid"), "prov:m", dir.path(), None).unwrap();
    s.append(RecordKind::UserMessage(Message::user_text("one")))
        .unwrap();
    let asst = s
        .append(RecordKind::AssistantMessage(Message::assistant(vec![
            ContentBlock::text("calling"),
            ContentBlock::ToolUse {
                id: "t1".into(),
                name: "echo".into(),
                input: Json("{}".into()),
            },
        ])))
        .unwrap();
    s.append(RecordKind::ToolResult {
        tool_use_id: "t1".into(),
        content: "output".into(),
        is_error: false,
    })
    .unwrap();
    s.append(RecordKind::UserMessage(Message::user_text("two")))
        .unwrap();

    let before = s.messages();
    assert!(
        has_tool_result(&before),
        "the result is there to begin with"
    );

    assert!(s.set_excluded(asst, true).unwrap());
    let after = s.messages();
    assert_eq!(
        after.iter().map(|m| m.text()).collect::<Vec<_>>(),
        ["one", "two"]
    );
    assert!(
        !has_tool_result(&after),
        "no result may outlive the call it answers"
    );
    assert!(
        after.iter().all(|m| !m.content.is_empty()),
        "and nothing is left empty"
    );

    // Striking what is already struck writes nothing: a key pressed twice
    // should not grow the log.
    let n = s.branch().len();
    assert!(!s.set_excluded(asst, true).unwrap());
    assert_eq!(s.branch().len(), n);

    // And it is a toggle, not a deletion — the record never left the log.
    assert!(s.set_excluded(asst, false).unwrap());
    assert_eq!(s.messages().len(), before.len());
    assert!(has_tool_result(&s.messages()));
}

/// Striking a *result* is the one case that must still answer.
///
/// Its `tool_use` is still on the branch, and an unanswered one is a
/// malformed turn — but the answer must not be `flush`'s placeholder, which
/// says the call was interrupted. This call ran and returned; the operator
/// took the output back. A model told the wrong story about its own tools
/// reasons from it.
#[test]
fn striking_a_result_says_it_was_withheld_rather_than_interrupted() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::create(&dir.path().join("s.eid"), "prov:m", dir.path(), None).unwrap();
    s.append(RecordKind::UserMessage(Message::user_text("build it")))
        .unwrap();
    s.append(RecordKind::AssistantMessage(Message::assistant(vec![
        ContentBlock::ToolUse {
            id: "t1".into(),
            name: "shell".into(),
            input: Json("{}".into()),
        },
    ])))
    .unwrap();
    let result = s
        .append(RecordKind::ToolResult {
            tool_use_id: "t1".into(),
            content: "x".repeat(4_000),
            is_error: true,
        })
        .unwrap();

    assert!(s.set_excluded(result, true).unwrap());
    let msgs = s.messages();
    let block = msgs
        .iter()
        .flat_map(|m| &m.content)
        .find_map(|b| match b {
            ContentBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => Some((tool_use_id, content, is_error)),
            _ => None,
        })
        .expect("the call is still answered");
    assert_eq!(block.0, "t1");
    assert!(
        !block.1.contains("xxxx"),
        "the four thousand characters are gone"
    );
    assert!(block.1.contains("withheld"), "and it says so: {}", block.1);
    assert!(
        !block.2,
        "a withheld result is not a failed one — the call did not fail"
    );
}

fn has_tool_result(msgs: &[Message]) -> bool {
    msgs.iter()
        .flat_map(|m| &m.content)
        .any(|b| matches!(b, ContentBlock::ToolResult { .. }))
}

/// The inspector reports the blocks the next turn would carry, and sizes
/// them by what is actually on the wire rather than by `Message::text()`.
///
/// The image is the case that matters. A screenshot's base64 is routinely
/// the largest single thing on a branch and `text()` cannot see one byte of
/// it, so a report built on prose alone would tell the operator a turn cost
/// nothing while it was carrying a megabyte — which is exactly the class of
/// wrong number the gauge already got caught making.
#[tokio::test]
async fn the_context_report_sizes_what_text_alone_cannot_see() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![ScriptedProvider::text("done")]);
    let user = ScriptedUser::new(true);
    let (agent, _session) = harness(
        dir.path(),
        provider.clone(),
        Arc::new(AllowAll),
        user,
        Approval::ReadOnly,
        false,
    );
    let pixels = "A".repeat(5_000);
    agent
        .run_turn(
            vec![
                ContentBlock::text("look at this"),
                ContentBlock::image("image/png", pixels, Some("shot.png".into())),
            ],
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let r = agent.context_report().await;
    let kinds: Vec<&str> = r.blocks.iter().map(|b| b.kind).collect();
    assert_eq!(
        kinds,
        ["system", "user", "assistant"],
        "the system prompt is a block like any other"
    );

    let user_block = &r.blocks[1];
    assert!(
        user_block.chars > 5_000,
        "the base64 is counted: {}",
        user_block.chars
    );
    assert!(
        user_block.label.contains("look at this"),
        "{}",
        user_block.label
    );
    assert!(
        user_block.label.contains("1 image"),
        "an image says so on the line: {}",
        user_block.label
    );

    // The tool registry's guidance is inside the system block, and also
    // reported on its own so the operator can see what the tools cost.
    assert!(r.guidance_chars > 0, "the harness's tools declare guidance");
    assert!(
        r.blocks[0].chars > r.guidance_chars,
        "guidance is part of the system block, not beside it"
    );

    // The total is the blocks, and nothing else: a report whose footer did
    // not add up would be worse than no footer.
    assert_eq!(r.chars, r.blocks.iter().map(|b| b.chars).sum::<usize>());

    // Nothing has been folded away.
    assert_eq!(r.compacted, None);
}

#[tokio::test]
async fn context_tool_footprints_follow_exclusion_and_the_selected_branch() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::tool("call-1", "echo", json!({"s": "large".repeat(100)})),
        ScriptedProvider::text("done"),
    ]);
    let (agent, session) = harness(dir.path(), provider, Arc::new(AllowAll), ScriptedUser::new(true), Approval::ReadOnly, false);
    agent.run_turn(vec![ContentBlock::text("go")], CancellationToken::new()).await.unwrap();
    let before = agent.context_report().await;
    assert_eq!(before.tools.len(), 1);
    assert_eq!(before.tools[0].name, "echo");
    assert_eq!(before.tools[0].result_chars, 505);
    assert_eq!(before.sources.iter().map(|s| s.chars).sum::<usize>(), before.chars);
    let mut locked = session.lock().await;
    let start = locked.records()[0].id;
    let result = locked.records().iter().find(|r| matches!(r.kind, RecordKind::ToolResult { .. })).unwrap().id;
    locked.set_excluded(result, true).unwrap();
    drop(locked);
    let excluded = agent.context_report().await;
    assert!(excluded.tools[0].result_chars < before.tools[0].result_chars, "withheld placeholder, not the original payload");
    assert_eq!(excluded.sources.iter().map(|s| s.chars).sum::<usize>(), excluded.chars);
    session.lock().await.fork_at(start).unwrap();
    let forked = agent.context_report().await;
    assert!(forked.tools.is_empty(), "no tool traffic from another branch");
    assert_eq!(forked.sources.iter().map(|s| s.chars).sum::<usize>(), forked.chars);
}

/// Fresh eyes: dropping the mark makes the backend a stranger to the branch
/// again, so the next turn tells it everything rather than resuming a
/// conversation the harness has stopped agreeing with.
///
/// The sentinel is load-bearing in a way that is easy to get backwards.
/// Returning the clearing record's own *position* as the mark would be
/// worse than doing nothing at all: the span after it is empty, so the
/// backend would be told nothing and would answer the next question with no
/// context whatsoever. Dropping the mark has to mean "there is no mark".
#[test]
fn dropping_the_backend_session_replays_the_whole_branch() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::create(&dir.path().join("s.eid"), "prov:m", dir.path(), None).unwrap();
    // Nothing held yet. Saying so is not the same as having dropped one,
    // which is why this answers `false` rather than `true`.
    assert!(!s.clear_backend_session("claude-cli").unwrap());

    s.append(RecordKind::UserMessage(Message::user_text("one")))
        .unwrap();
    s.append(RecordKind::AssistantMessage(Message::assistant(vec![
        ContentBlock::text("first answer"),
    ])))
    .unwrap();
    s.append(RecordKind::TurnSettled {
        stop_reason: StopReason::EndTurn,
        usage: Default::default(),
    })
    .unwrap();
    let mark = s
        .append(RecordKind::BackendSession {
            backend: "claude-cli".into(),
            id: "sess".into(),
        })
        .unwrap();
    assert_eq!(
        s.backend_session_mark("claude-cli"),
        Some((mark, "sess".into()))
    );
    assert!(s.messages_after(Some(mark)).is_empty());

    // A second backend's mark, to prove clearing is per-backend the way
    // marking is.
    let other = s
        .append(RecordKind::BackendSession {
            backend: "other".into(),
            id: "sess-2".into(),
        })
        .unwrap();

    assert!(s.clear_backend_session("claude-cli").unwrap());
    assert_eq!(
        s.backend_session_mark("claude-cli"),
        None,
        "it holds nothing now"
    );
    assert_eq!(s.backend_session("claude-cli"), None);
    assert_eq!(
        s.backend_session_mark("other"),
        Some((other, "sess-2".into())),
        "one backend's eviction is not another's"
    );
    let texts: Vec<String> = s.messages_after(None).iter().map(|m| m.text()).collect();
    assert_eq!(
        texts,
        ["one", "first answer"],
        "the whole branch, not the empty span after the sentinel"
    );

    // Clearing what is already clear writes nothing: a control pressed
    // twice should not grow the log.
    let before = s.branch().len();
    assert!(!s.clear_backend_session("claude-cli").unwrap());
    assert_eq!(s.branch().len(), before);

    // And the sentinel is not permanent — the next turn on that backend
    // marks itself and wins again, because the latest record still wins.
    let again = s
        .append(RecordKind::BackendSession {
            backend: "claude-cli".into(),
            id: "sess-3".into(),
        })
        .unwrap();
    assert_eq!(
        s.backend_session_mark("claude-cli"),
        Some((again, "sess-3".into()))
    );
}

/// A dispatch — a call the operator made rather than the model — reaches
/// the model as context. It used to vanish: only the result was journaled,
/// and `messages_after` drops a `tool_result` with no `tool_use` in front
/// of it, so the model was never told what had just been run in its name.
#[test]
fn a_call_the_operator_made_is_in_the_conversation() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::create(&dir.path().join("s.eid"), "prov:m", dir.path(), None).unwrap();
    s.append(RecordKind::UserMessage(Message::user_text("hello")))
        .unwrap();
    s.append(RecordKind::AssistantMessage(Message::assistant(vec![
        ContentBlock::text("hi"),
    ])))
    .unwrap();
    s.append(RecordKind::UserToolCall {
        tool_use_id: "vv_1".into(),
        name: "mneme_rpc".into(),
        input: message::Json(r#"{"function":"read_note"}"#.into()),
        utterance: Some("read the note Groceries".into()),
    })
    .unwrap();
    s.append(RecordKind::ToolResult {
        tool_use_id: "vv_1".into(),
        content: "eggs, milk".into(),
        is_error: false,
    })
    .unwrap();

    let msgs = s.messages();
    let last = msgs
        .last()
        .expect("the dispatch is in the conversation")
        .text();
    assert!(last.contains("operator ran a tool directly"), "{last}");
    assert!(
        last.contains("not one you made"),
        "the model must not think it called this: {last}"
    );
    assert!(
        last.contains("read the note Groceries"),
        "what was asked for: {last}"
    );
    assert!(last.contains("mneme_rpc"), "what ran: {last}");
    assert!(last.contains("eggs, milk"), "what came back: {last}");
    assert_eq!(msgs.len(), 3, "one user message, not one per record");
}

/// Interrupted between the call and its result, it still says so rather
/// than leaving a hole — the same bargain `flush` makes for a `tool_use`.
#[test]
fn an_interrupted_dispatch_says_so_rather_than_vanishing() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::create(&dir.path().join("s.eid"), "prov:m", dir.path(), None).unwrap();
    s.append(RecordKind::UserToolCall {
        tool_use_id: "vv_1".into(),
        name: "shell".into(),
        input: message::Json("{}".into()),
        utterance: None,
    })
    .unwrap();
    let one = s.messages();
    assert!(
        one.last().unwrap().text().contains("interrupted"),
        "{:?}",
        one.last()
    );

    // And a record arriving in between does not steal the pairing.
    s.append(RecordKind::UserMessage(Message::user_text("never mind")))
        .unwrap();
    s.append(RecordKind::ToolResult {
        tool_use_id: "vv_1".into(),
        content: "late".into(),
        is_error: false,
    })
    .unwrap();
    let texts: Vec<String> = s.messages().iter().map(|m| m.text()).collect();
    assert!(texts[0].contains("interrupted"), "{texts:?}");
    assert_eq!(texts[1], "never mind");
    assert!(
        !texts.iter().any(|t| t.contains("late")),
        "a result nothing claims stays unclaimed: {texts:?}"
    );
}

/// A peer's message reaches the model as itself: user-role text that says
/// another agent said it, and does not pretend to be the operator.
#[test]
fn a_peers_message_says_who_sent_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::create(&dir.path().join("s.eid"), "prov:m", dir.path(), None).unwrap();
    s.append(RecordKind::PeerMessage {
        from: "eidolon-9f2c".into(),
        from_cwd: "/home/noah/Development/eidolon".into(),
        channel: None,
        text: "I am out of edit.rs".into(),
    })
    .unwrap();
    s.append(RecordKind::UserMessage(Message::user_text("carry on")))
        .unwrap();

    let msgs = s.messages();
    assert_eq!(msgs.len(), 2);
    let peer = msgs[0].text();
    assert!(peer.contains("eidolon-9f2c"), "{peer}");
    assert!(peer.contains("another agent"), "{peer}");
    assert!(
        peer.contains("not the operator speaking"),
        "the model must not read it as its user: {peer}"
    );
    assert!(peer.ends_with("I am out of edit.rs"), "{peer}");
    assert_eq!(
        msgs[1].text(),
        "carry on",
        "and the operator's own words are still their own"
    );
}

/// An outside caller's message — `eidolon send` — reaches the model as
/// itself too, and says a different thing than a peer's: a tool that
/// injected text, with no session behind it to reply to.
#[test]
fn an_external_message_is_not_a_colleague_session() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::create(&dir.path().join("s.eid"), "prov:m", dir.path(), None).unwrap();
    s.append(RecordKind::ExternalMessage {
        from: "aoide".into(),
        channel: None,
        text: "rebase finished cleanly".into(),
    })
    .unwrap();
    s.append(RecordKind::UserMessage(Message::user_text("carry on")))
        .unwrap();

    let msgs = s.messages();
    assert_eq!(msgs.len(), 2);
    let external = msgs[0].text();
    assert!(external.contains("`aoide`"), "{external}");
    assert!(
        external.contains("not the operator speaking"),
        "the model must not read it as its user: {external}"
    );
    assert!(
        !external.contains("another agent"),
        "and not as a peer session either: {external}"
    );
    assert!(external.contains("nobody to reply to"), "{external}");
    assert!(external.ends_with("rebase finished cleanly"), "{external}");
}

/// The reason peer messages are journaled at a boundary and nowhere else:
/// a user-shaped record between a `tool_use` and its result turns every
/// outstanding call into a synthetic interruption. At the boundary it is
/// harmless, which is what this pins.
#[test]
fn a_peer_message_at_a_boundary_leaves_tool_pairing_alone() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::create(&dir.path().join("s.eid"), "prov:m", dir.path(), None).unwrap();
    s.append(RecordKind::UserMessage(Message::user_text("read it")))
        .unwrap();
    s.append(RecordKind::AssistantMessage(Message::assistant(vec![
        ContentBlock::ToolUse {
            id: "t1".into(),
            name: "read".into(),
            input: message::Json("{}".into()),
        },
    ])))
    .unwrap();
    s.append(RecordKind::ToolResult {
        tool_use_id: "t1".into(),
        content: "contents".into(),
        is_error: false,
    })
    .unwrap();
    s.append(RecordKind::TurnSettled {
        stop_reason: StopReason::EndTurn,
        usage: Usage::default(),
    })
    .unwrap();
    s.append(RecordKind::PeerMessage {
        from: "eidolon-9f2c".into(),
        from_cwd: "/tmp/wt".into(),
        channel: Some("/home/noah/Development/eidolon/.git".into()),
        text: "rebased onto master".into(),
    })
    .unwrap();

    let texts: Vec<String> = s.messages().iter().map(|m| m.text()).collect();
    assert!(
        !texts.iter().any(|t| t.contains("interrupted")),
        "nothing was interrupted: {texts:?}"
    );
    assert!(texts.last().unwrap().contains("rebased onto master"));
    assert!(
        texts.last().unwrap().contains("project channel"),
        "a channel message says it was one"
    );
    assert!(
        s.is_settled(),
        "a note from a neighbour does not unsettle a finished turn"
    );
}

/// `drain_peers` journals before it publishes, and `run_turn` folds what
/// is waiting in *ahead* of the operator's message — their neighbours'
/// notes are context for the request, not a reply to it.
#[tokio::test]
async fn peer_messages_land_ahead_of_the_turn_that_collected_them() {
    struct Waiting(std::sync::Mutex<Vec<peer::PeerMessage>>);
    impl peer::PeerInbox for Waiting {
        fn drain(&self) -> Vec<peer::PeerMessage> {
            std::mem::take(&mut *self.0.lock().unwrap())
        }
        fn drain_waking(&self) -> Vec<peer::PeerMessage> {
            let mut all = self.0.lock().unwrap();
            let (waking, waiting): (Vec<_>, Vec<_>) =
                std::mem::take(&mut *all).into_iter().partition(|m| m.wake);
            *all = waiting;
            waking
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![ScriptedProvider::text("noted")]);
    let user = ScriptedUser::new(true);
    let (agent, _session) = harness(
        dir.path(),
        provider,
        Arc::new(AllowAll),
        user,
        Approval::ReadOnly,
        false,
    );
    let mut events = agent.dispatcher().bus().subscribe();
    agent.set_inbox(Arc::new(Waiting(std::sync::Mutex::new(vec![
        peer::PeerMessage {
            from: "eidolon-9f2c".into(),
            from_cwd: "/tmp/wt".into(),
            channel: None,
            text: "edit.rs is yours".into(),
            wake: false,
            external: false,
        },
    ]))));

    agent
        .run_turn(
            vec![ContentBlock::text("what changed?")],
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let s = agent.session().lock().await;
    let kinds: Vec<&RecordKind> = s.branch().into_iter().map(|r| &r.kind).collect();
    let peer_at = kinds
        .iter()
        .position(|k| matches!(k, RecordKind::PeerMessage { .. }))
        .expect("journaled");
    let user_at = kinds
        .iter()
        .position(|k| matches!(k, RecordKind::UserMessage(_)))
        .expect("journaled");
    assert!(
        peer_at < user_at,
        "the neighbour's note comes first: {kinds:?}"
    );

    let published = std::iter::from_fn(|| events.try_recv().ok())
        .any(|e| matches!(e, Event::PeerMessage { .. }));
    assert!(published, "journaled, then published");
}

/// A fake inbox that delivers in batches, one per drain — the shape of
/// mail arriving at known moments. Waking drains take only the marked
/// mail and keep the rest, as the real one does.
struct Batched(std::sync::Mutex<std::collections::VecDeque<Vec<peer::PeerMessage>>>);
impl peer::PeerInbox for Batched {
    fn drain(&self) -> Vec<peer::PeerMessage> {
        self.0.lock().unwrap().pop_front().unwrap_or_default()
    }
    fn drain_waking(&self) -> Vec<peer::PeerMessage> {
        let mut q = self.0.lock().unwrap();
        let (waking, waiting): (Vec<_>, Vec<_>) =
            q.pop_front().unwrap_or_default().into_iter().partition(|m| m.wake);
        if !waiting.is_empty() {
            q.push_front(waiting);
        }
        waking
    }
}

/// A waking peer message that lands while a tool call is running is not
/// held to the next turn boundary: it is drained at the top of the next
/// loop iteration, beside the results it arrived after, and the model
/// reads it before its next call. This is the same timing as
/// [`Agent::steer`], and the reason an ask from a neighbour does not wait
/// out a long turn.
#[tokio::test]
async fn a_waking_peer_message_mid_turn_rides_the_results_into_the_next_call() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::tool("t1", "echo", json!({"s":"hi"})),
        ScriptedProvider::text("done"),
    ]);
    let user = ScriptedUser::new(true);
    let (agent, session) = harness(
        dir.path(),
        provider.clone(),
        Arc::new(AllowAll),
        user,
        Approval::ReadOnly,
        false,
    );
    // First drain is the turn boundary and finds nothing; the second is
    // the top of the second loop iteration, after the tool result is on
    // the branch. That is the arrival being simulated.
    agent.set_inbox(Arc::new(Batched(std::sync::Mutex::new(
        vec![
            vec![],
            vec![peer::PeerMessage {
                from: "eidolon-9f2c".into(),
                from_cwd: "/tmp/wt".into(),
                channel: None,
                text: "rebased onto master".into(),
                wake: true,
                external: false,
            }],
        ]
        .into(),
    ))));

    agent
        .run_turn(vec![ContentBlock::text("go")], CancellationToken::new())
        .await
        .unwrap();

    // The second request is the one that had to see the message: it carries
    // the tool result and the framed peer note in one user message, results
    // first — never as a second user turn in a row, and never a synthetic
    // interruption.
    {
        let reqs = provider.requests.lock().unwrap();
        assert_eq!(reqs.len(), 2);
        let last = reqs[1].messages.last().unwrap();
        assert!(
            matches!(&last.content[0], ContentBlock::ToolResult { content, .. } if content == "echo:hi")
        );
        assert!(
            last.text().contains("rebased onto master"),
            "the peer note rides the results: {last:?}"
        );
        assert!(
            last.text().contains("not the operator speaking"),
            "and keeps its frame: {last:?}"
        );
        assert!(
            !reqs[1]
                .messages
                .iter()
                .any(|m| m.text().contains("interrupted")),
            "nothing was interrupted"
        );
    }

    let s = session.lock().await;
    let kinds: Vec<&RecordKind> = s.branch().into_iter().map(|r| &r.kind).collect();
    let peer_at = kinds
        .iter()
        .position(|k| matches!(k, RecordKind::PeerMessage { .. }))
        .expect("journaled mid-turn");
    let result_at = kinds
        .iter()
        .position(|k| matches!(k, RecordKind::ToolResult { .. }))
        .expect("the tool result");
    let second_assistant = kinds
        .iter()
        .rposition(|k| matches!(k, RecordKind::AssistantMessage(_)))
        .expect("the reply after the peer");
    assert!(
        result_at < peer_at && peer_at < second_assistant,
        "peer lands after the results and before the next reply: {kinds:?}"
    );
}

/// The other half of the wake rule: a note that did not ask to wake is
/// not time-sensitive. It does not ride the results of the calls in
/// flight — the turn that was running when it arrived settles without
/// it — and it is delivered whole at the next boundary, exactly where a
/// consumer's idle drain finds it.
#[tokio::test]
async fn a_note_that_did_not_wake_waits_out_the_turn() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::tool("t1", "echo", json!({"s":"hi"})),
        ScriptedProvider::text("done"),
    ]);
    let user = ScriptedUser::new(true);
    let (agent, session) = harness(
        dir.path(),
        provider.clone(),
        Arc::new(AllowAll),
        user,
        Approval::ReadOnly,
        false,
    );
    agent.set_inbox(Arc::new(Batched(std::sync::Mutex::new(
        vec![
            vec![],
            vec![peer::PeerMessage {
                from: "eidolon-9f2c".into(),
                from_cwd: "/tmp/wt".into(),
                channel: Some("/repo".into()),
                text: "heads up, master moved".into(),
                wake: false,
                external: false,
            }],
        ]
        .into(),
    ))));

    let out = agent
        .run_turn(vec![ContentBlock::text("go")], CancellationToken::new())
        .await
        .unwrap();
    assert!(
        matches!(out, TurnOutcome::Settled { calls: 2, .. }),
        "the note neither interrupted nor unsettled the turn: {out:?}"
    );
    {
        let reqs = provider.requests.lock().unwrap();
        assert_eq!(reqs.len(), 2, "no call was spent on the note");
        assert!(
            !reqs.iter().any(|r| r
                .messages
                .iter()
                .any(|m| m.text().contains("heads up"))),
            "the model never saw it mid-turn"
        );
    }
    {
        let s = session.lock().await;
        assert!(
            !s.branch()
                .iter()
                .any(|r| matches!(r.kind, RecordKind::PeerMessage { .. })),
            "nothing was journalled mid-turn"
        );
    }

    // The boundary is where it lands, whole and framed.
    let drained = agent.drain_peers().await;
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].text, "heads up, master moved");
    assert!(
        agent
            .session()
            .lock()
            .await
            .branch()
            .iter()
            .any(|r| matches!(r.kind, RecordKind::PeerMessage { ref text, .. } if text == "heads up, master moved")),
        "journalled at the boundary"
    );
}

/// A probe driver: one pass is one `run_turn` call that records what
/// prompt was pending, says "noted" to it, and settles. Shared by the
/// peer-delivery tests on the driver backend.
struct ProbeDriver {
    seen: std::sync::Mutex<Vec<Option<String>>>,
}

impl TurnDriver for ProbeDriver {
    fn name(&self) -> &str {
        "probe"
    }

    fn run_turn<'a>(
        &'a self,
        agent: &'a Agent,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<TurnOutcome>> {
        Box::pin(async move {
            let _ = cancel; // A probe turn ignores cancellation.
            let pending = agent
                .session()
                .lock()
                .await
                .pending_user_message()
                .map(|m| m.text());
            self.seen.lock().unwrap().push(pending.clone());
            let mut s = agent.session().lock().await;
            if pending.is_some() {
                s.append(RecordKind::AssistantMessage(Message::assistant(vec![
                    ContentBlock::text("noted"),
                ])))?;
            }
            s.append(RecordKind::TurnSettled {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
            })?;
            drop(s);
            Ok(TurnOutcome::Settled {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
                calls: 1,
            })
        })
    }
}

/// A driver backend cannot be interrupted either, but a waking peer
/// message that arrived while it ran still becomes the next turn on that
/// driver at once — the same deal a steer gets. `Session::
/// pending_user_message` must see the `PeerMessage` as the pending
/// prompt, or the driver would send nothing on the second pass.
#[tokio::test]
async fn a_waking_peer_message_after_a_driver_turn_runs_the_next_driver_turn() {
    struct Inbox(std::sync::Mutex<Vec<peer::PeerMessage>>);
    impl peer::PeerInbox for Inbox {
        fn drain(&self) -> Vec<peer::PeerMessage> {
            std::mem::take(&mut *self.0.lock().unwrap())
        }
        fn drain_waking(&self) -> Vec<peer::PeerMessage> {
            let mut all = self.0.lock().unwrap();
            let (waking, waiting): (Vec<_>, Vec<_>) =
                std::mem::take(&mut *all).into_iter().partition(|m| m.wake);
            *all = waiting;
            waking
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![]);
    let user = ScriptedUser::new(true);
    let (agent, _session) = harness(
        dir.path(),
        provider,
        Arc::new(AllowAll),
        user,
        Approval::ReadOnly,
        false,
    );
    let driver = Arc::new(ProbeDriver {
        seen: std::sync::Mutex::new(Vec::new()),
    });
    agent.set_backend(Backend::Driver(driver.clone()));
    agent.set_inbox(Arc::new(Inbox(std::sync::Mutex::new(vec![
        peer::PeerMessage {
            from: "eidolon-9f2c".into(),
            from_cwd: "/tmp/wt".into(),
            channel: None,
            text: "rebased onto master".into(),
            wake: true,
            external: false,
        },
    ]))));

    agent.continue_turn(CancellationToken::new()).await.unwrap();

    let seen = driver.seen.lock().unwrap().clone();
    assert_eq!(
        seen.len(),
        2,
        "one pass, then one more for the peer message: {seen:?}"
    );
    assert_eq!(seen[0], None, "the first pass has no pending prompt");
    let second = seen[1]
        .as_deref()
        .expect("the peer message is the second prompt");
    assert!(second.contains("rebased onto master"), "{second}");
    assert!(second.contains("not the operator speaking"), "{second}");
}

/// And the non-waking half on a driver: the note is journalled when the
/// driver turn settles, but buys no second pass. It is context for
/// whatever turn comes next, not a turn of its own — the same line the
/// provider loop draws.
#[tokio::test]
async fn a_note_that_did_not_wake_buys_no_second_driver_pass() {
    struct Inbox(std::sync::Mutex<Vec<peer::PeerMessage>>);
    impl peer::PeerInbox for Inbox {
        fn drain(&self) -> Vec<peer::PeerMessage> {
            std::mem::take(&mut *self.0.lock().unwrap())
        }
        fn drain_waking(&self) -> Vec<peer::PeerMessage> {
            let mut all = self.0.lock().unwrap();
            let (waking, waiting): (Vec<_>, Vec<_>) =
                std::mem::take(&mut *all).into_iter().partition(|m| m.wake);
            *all = waiting;
            waking
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![]);
    let user = ScriptedUser::new(true);
    let (agent, session) = harness(
        dir.path(),
        provider,
        Arc::new(AllowAll),
        user,
        Approval::ReadOnly,
        false,
    );
    let driver = Arc::new(ProbeDriver {
        seen: std::sync::Mutex::new(Vec::new()),
    });
    agent.set_backend(Backend::Driver(driver.clone()));
    agent.set_inbox(Arc::new(Inbox(std::sync::Mutex::new(vec![
        peer::PeerMessage {
            from: "eidolon-9f2c".into(),
            from_cwd: "/tmp/wt".into(),
            channel: Some("/repo".into()),
            text: "heads up, master moved".into(),
            wake: false,
            external: false,
        },
    ]))));

    agent.continue_turn(CancellationToken::new()).await.unwrap();

    let seen = driver.seen.lock().unwrap().clone();
    assert_eq!(seen, vec![None], "one pass, and no prompt to answer: {seen:?}");
    let s = session.lock().await;
    assert!(
        s.branch()
            .iter()
            .any(|r| matches!(r.kind, RecordKind::PeerMessage { ref text, .. } if text == "heads up, master moved")),
        "journalled at the boundary the settle made"
    );
}

/// **The bug the context gauge made visible.**
///
/// `TurnSettled` carries the sum of every model call the turn made — two
/// calls of ten input tokens settle at twenty — and for a turn that ran a
/// hundred tools that sum is many times the size of the context. Reading
/// it as a context is what drew `ctx 12.92M/1.00M (1292%)` on a resumed
/// session: the number grew with the *length of the turn* rather than the
/// fullness of the context.
///
/// So the two numbers are two records. This pins them apart.
#[tokio::test]
async fn the_context_is_the_last_calls_input_and_not_the_turns_total() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::tool("t1", "echo", json!({"s":"hi"})),
        ScriptedProvider::text("done"),
    ]);
    let user = ScriptedUser::new(true);
    let (agent, session) = harness(
        dir.path(),
        provider,
        Arc::new(AllowAll),
        user,
        Approval::ReadOnly,
        false,
    );
    agent
        .run_turn(vec![ContentBlock::text("go")], CancellationToken::new())
        .await
        .unwrap();

    let s = session.lock().await;
    let settled = s
        .branch()
        .iter()
        .rev()
        .find_map(|r| match &r.kind {
            RecordKind::TurnSettled { usage, .. } => Some(*usage),
            _ => None,
        })
        .expect("the turn settled");
    // The scripted provider reports ten input tokens per call, and the
    // turn made two. The settle keeps saying twenty — that is what the
    // turn cost, and nothing here changes it.
    assert_eq!(settled.input_tokens, 20);
    // The context is one call's worth: the last one, which is the only
    // call that saw the whole transcript.
    assert_eq!(s.last_input_tokens(), Some(10));
}

/// A log written before `ContextSize` existed reads as **unknown** — not
/// zero, and not the old wrong number.
///
/// Unknown is what the TUI draws as a dash until a later turn writes a real
/// measurement; it is the honest direction to be wrong in, because guessing
/// is worse than saying so.
#[test]
fn a_log_without_a_context_record_says_it_does_not_know() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::create(&dir.path().join("old.eid"), "m", dir.path(), None).unwrap();
    s.append(RecordKind::UserMessage(Message::user_text("hi")))
        .unwrap();
    // The shape of the session that started this: one enormous settle.
    s.append(RecordKind::TurnSettled {
        stop_reason: StopReason::EndTurn,
        usage: Usage {
            input_tokens: 12_920_000,
            output_tokens: 26_800,
            ..Default::default()
        },
    })
    .unwrap();
    assert_eq!(
        s.last_input_tokens(),
        None,
        "a turn total is not a context size, and guessing is worse than saying so"
    );
    // A compaction restarts the history; a log from before the summary's
    // size was recorded has only the `Compacted` record, so it reads as
    // unknown — not as zero, which would claim an empty context.
    s.append(RecordKind::Compacted {
        summary: "…".into(),
        replaced_messages: 2,
    })
    .unwrap();
    assert_eq!(s.last_input_tokens(), None);
    // A compaction written by the current core journals the summary's
    // measured size right after it, and that is the number read.
    s.append(RecordKind::ContextSize { tokens: 412 }).unwrap();
    assert_eq!(s.last_input_tokens(), Some(412));
}

// ── A model that names a tool instead of calling it ──────────────────────
//
// The registered tool in this harness is `echo`, so a reply of exactly
// "echo" is the shape being caught.

/// The reply is nothing but the tool's name, so the turn asks once more with
/// the channel forced — and the second request is the one that says so.
#[tokio::test]
async fn naming_a_tool_instead_of_calling_it_is_re_asked_with_the_channel_forced() {
    use eidolon_core::provider::ToolChoice;
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::text("echo"),
        ScriptedProvider::tool("t1", "echo", json!({"s":"hi"})),
        ScriptedProvider::text("done"),
    ]);
    let user = ScriptedUser::new(true);
    let (agent, session) = harness(
        dir.path(),
        provider.clone(),
        Arc::new(AllowAll),
        user,
        Approval::ReadOnly,
        false,
    );
    let out = agent
        .run_turn(vec![ContentBlock::text("go")], CancellationToken::new())
        .await
        .unwrap();
    assert!(matches!(
        out,
        TurnOutcome::Settled {
            stop_reason: StopReason::EndTurn,
            ..
        }
    ));

    {
        let reqs = provider.requests.lock().unwrap();
        assert_eq!(
            reqs.len(),
            3,
            "prose, forced re-ask, then the reply after the tool"
        );
        assert_eq!(reqs[0].tool_choice, ToolChoice::Auto);
        assert_eq!(
            reqs[1].tool_choice,
            ToolChoice::Required,
            "the re-ask forces the channel"
        );
        assert_eq!(
            reqs[2].tool_choice,
            ToolChoice::Auto,
            "and only that one request"
        );
    }

    // The prose is kept, not swallowed: it reached the screen as it streamed,
    // and the re-ask is a continuation over the top of it.
    let guard = session.lock().await;
    let texts: Vec<String> = guard
        .branch()
        .into_iter()
        .filter_map(|r| match &r.kind {
            RecordKind::AssistantMessage(m) => Some(m.text()),
            _ => None,
        })
        .collect();
    assert!(
        texts.iter().any(|t| t.trim() == "echo"),
        "the reply that prompted the re-ask is journaled: {texts:?}"
    );
}

/// Once only. A model that answers the forced request in prose as well has
/// said its piece, and the turn settles rather than looping.
#[tokio::test]
async fn the_channel_is_forced_at_most_once_a_turn() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::text("echo"),
        ScriptedProvider::text("echo"),
    ]);
    let user = ScriptedUser::new(true);
    let (agent, _s) = harness(
        dir.path(),
        provider.clone(),
        Arc::new(AllowAll),
        user,
        Approval::ReadOnly,
        false,
    );
    let out = agent
        .run_turn(vec![ContentBlock::text("go")], CancellationToken::new())
        .await
        .unwrap();
    assert!(matches!(out, TurnOutcome::Settled { .. }));
    assert_eq!(
        provider.requests.lock().unwrap().len(),
        2,
        "one re-ask, not a loop"
    );
}

/// A tool named inside a sentence is an answer, not a missed call.
#[tokio::test]
async fn a_tool_mentioned_in_prose_does_not_force_anything() {
    let dir = tempfile::tempdir().unwrap();
    let provider =
        ScriptedProvider::new(vec![ScriptedProvider::text("You could use echo for that.")]);
    let user = ScriptedUser::new(true);
    let (agent, _s) = harness(
        dir.path(),
        provider.clone(),
        Arc::new(AllowAll),
        user,
        Approval::ReadOnly,
        false,
    );
    agent
        .run_turn(vec![ContentBlock::text("go")], CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        provider.requests.lock().unwrap().len(),
        1,
        "an ordinary answer settles the turn"
    );
}

/// An ordinary reply that happens to be one word is still an ordinary reply.
#[tokio::test]
async fn a_one_word_reply_that_is_not_a_tool_name_settles() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![ScriptedProvider::text("done")]);
    let user = ScriptedUser::new(true);
    let (agent, _s) = harness(
        dir.path(),
        provider.clone(),
        Arc::new(AllowAll),
        user,
        Approval::ReadOnly,
        false,
    );
    agent
        .run_turn(vec![ContentBlock::text("go")], CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(provider.requests.lock().unwrap().len(), 1);
}

/// A scripted provider that says something to the agent as a given call
/// starts — the one way a test can speak *during* a turn, which is when a
/// steer is meant to arrive.
struct SteeringProvider {
    inner: Arc<ScriptedProvider>,
    on_call: usize,
    texts: Vec<String>,
    agent: std::sync::OnceLock<std::sync::Weak<Agent>>,
    seen: std::sync::atomic::AtomicUsize,
}

impl SteeringProvider {
    fn new(inner: Arc<ScriptedProvider>, on_call: usize, texts: &[&str]) -> Arc<Self> {
        Arc::new(SteeringProvider {
            inner,
            on_call,
            texts: texts.iter().map(|s| s.to_string()).collect(),
            agent: Default::default(),
            seen: Default::default(),
        })
    }
}

impl Provider for SteeringProvider {
    fn name(&self) -> &str {
        "steering"
    }
    fn stream<'a>(
        &'a self,
        req: ChatRequest,
        cancel: CancellationToken,
    ) -> eidolon_core::provider::EventStream<'a> {
        let n = self.seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if n == self.on_call
            && let Some(a) = self.agent.get().and_then(std::sync::Weak::upgrade)
        {
            for text in &self.texts {
                a.steer(vec![ContentBlock::text(text.clone())]);
            }
        }
        self.inner.stream(req, cancel)
    }
}

/// An agent over any provider at all, with the session handed back —
/// [`harness`] takes a `ScriptedProvider` by name and the wrappers below
/// are not one.
fn over(dir: &std::path::Path, provider: Arc<dyn Provider>) -> (Agent, Arc<Mutex<Session>>) {
    let session = Session::create(&dir.join("s.log"), "m", dir, Some("sys".into())).unwrap();
    let session = Arc::new(Mutex::new(session));
    let mut reg = ToolRegistry::new();
    reg.register(echo(Approval::ReadOnly));
    let user = ScriptedUser::new(true);
    let d = Arc::new(Dispatcher::new(
        reg,
        Arc::new(AllowAll),
        user,
        EventBus::default(),
        session.clone(),
        dir.to_path_buf(),
    ));
    let agent = Agent::new(
        provider,
        d,
        AgentConfig {
            model: "m".into(),
            system: Some("sys".into()),
            ..Default::default()
        },
    );
    (agent, session)
}

/// A provider that cancels the turn after it has answered `on_call`
/// times — the shape of a `/stop` arriving mid-loop.
struct CancellingProvider {
    inner: Arc<ScriptedProvider>,
    on_call: usize,
    cancel: CancellationToken,
    seen: std::sync::atomic::AtomicUsize,
}

impl CancellingProvider {
    fn new(inner: Arc<ScriptedProvider>, on_call: usize, cancel: CancellationToken) -> Arc<Self> {
        Arc::new(CancellingProvider {
            inner,
            on_call,
            cancel,
            seen: Default::default(),
        })
    }
}

impl Provider for CancellingProvider {
    fn name(&self) -> &str {
        "cancelling"
    }
    fn stream<'a>(
        &'a self,
        req: ChatRequest,
        cancel: CancellationToken,
    ) -> eidolon_core::provider::EventStream<'a> {
        let n = self.seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if n == self.on_call {
            self.cancel.cancel();
        }
        self.inner.stream(req, cancel)
    }
}

/// A turn that is stopped mid-loop **says what it had already spent**.
///
/// The loop summed it all along and then dropped it: the outcome was a
/// unit variant and `Session::usage` sums settles, so an interrupted
/// turn priced at zero — live for the driver that stopped it, and again
/// on resume. Melete's `/stop` reported `$0.00 · 9 turns` because of it.
#[tokio::test]
async fn a_cancelled_turn_reports_and_journals_what_it_spent() {
    let dir = tempfile::tempdir().unwrap();
    let scripted = ScriptedProvider::new(vec![
        ScriptedProvider::tool("t1", "echo", json!({"s":"hi"})),
        ScriptedProvider::text("never reached"),
    ]);
    let cancel = CancellationToken::new();
    // Cancelled as the *second* call goes out, which is the shape a
    // `/stop` actually has: one call finished and paid for, one cut.
    let provider = CancellingProvider::new(scripted, 1, cancel.clone());
    let (agent, session) = over(dir.path(), provider);
    let out = agent
        .run_turn(vec![ContentBlock::text("go")], cancel)
        .await
        .unwrap();

    // The call that completed: 10 in at `Start`, 1 out at `Stop`. The
    // one that was cut contributes nothing, which is why this figure is
    // a floor and is drawn as a cancelled turn rather than as a bill.
    let TurnOutcome::Cancelled { usage, calls } = out else {
        panic!("{out:?}")
    };
    assert_eq!(calls, 2, "both calls were made; only one of them reported");
    assert_eq!((usage.input_tokens, usage.output_tokens), (10, 1));

    // And on the branch, so a resumed session prices it the same — the
    // spend before the boundary it explains.
    let s = session.lock().await;
    let kinds: Vec<&RecordKind> = s.branch().iter().map(|r| &r.kind).collect();
    let spend = kinds
        .iter()
        .position(|k| matches!(k, RecordKind::TurnSpend { .. }))
        .expect("a spend record");
    let stop = kinds
        .iter()
        .position(|k| matches!(k, RecordKind::Cancelled))
        .expect("a cancellation");
    assert!(
        spend < stop,
        "the cost is journaled before the boundary it explains"
    );
    assert!(matches!(
        kinds[spend],
        RecordKind::TurnSpend { calls: 2, .. }
    ));
    assert_eq!(
        s.usage().output_tokens,
        1,
        "and `usage()` counts it, where it used to sum settles alone"
    );
    assert_eq!(s.usage().input_tokens, 10);
}

/// A cancel that lands before the first model call has no cost, and says
/// so by writing no record — which is the same thing an absent record
/// means on a backend that never reported one. A zero on the branch
/// would read as a turn that was measured and found free.
#[tokio::test]
async fn a_cancel_with_nothing_spent_journals_no_spend() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![ScriptedProvider::text("never reached")]);
    let user = ScriptedUser::new(true);
    let (agent, session) = harness(
        dir.path(),
        provider,
        Arc::new(AllowAll),
        user,
        Approval::ReadOnly,
        false,
    );
    let cancel = CancellationToken::new();
    cancel.cancel();
    let out = agent
        .run_turn(vec![ContentBlock::text("go")], cancel)
        .await
        .unwrap();
    assert!(
        matches!(out, TurnOutcome::Cancelled { calls: 0, .. }),
        "{out:?}"
    );

    let s = session.lock().await;
    assert!(
        !s.branch()
            .iter()
            .any(|r| matches!(r.kind, RecordKind::TurnSpend { .. })),
        "nothing was spent, so nothing is claimed"
    );
    assert!(
        s.branch()
            .iter()
            .any(|r| matches!(r.kind, RecordKind::Cancelled)),
        "but the turn still ended"
    );
    assert!(
        !s.branch()
            .iter()
            .any(|r| matches!(r.kind, RecordKind::ContextSize { .. })),
        "no call reported an input, so the context stays *unknown* — a zero \
         would claim an empty one"
    );
    assert_eq!(s.usage(), Default::default());
}

/// A tool-calling reply that also says what the call **read** — the shape
/// a real wire has, where the input side of a call's usage is how full the
/// context was as that call went out.
fn tool_reply(id: &str, input_tokens: u64) -> Vec<eidolon_core::provider::StreamEvent> {
    use eidolon_core::message::{StopReason, Usage};
    use eidolon_core::provider::StreamEvent;
    vec![
        StreamEvent::ToolUseStart {
            id: id.into(),
            name: "echo".into(),
        },
        StreamEvent::ToolInputDelta(json!({"s":"hi"}).to_string()),
        StreamEvent::BlockStop,
        StreamEvent::Stop {
            stop_reason: StopReason::ToolUse,
            usage: Usage {
                input_tokens,
                output_tokens: 1,
                ..Default::default()
            },
        },
    ]
}

/// A turn that is **stopped** still says how full the context was.
///
/// The cancel path wrote the spend and the boundary and no
/// `ContextSize`, so every consumer went on drawing the last *settled*
/// turn's number. That is one turn stale in ordinary work and flatly
/// wrong on a branch that has just been compacted — the figure standing
/// there is the summary's own size — and it does not correct itself while
/// an operator keeps stopping turns, which is exactly what a long tool
/// loop makes them do. The measurement is in hand when the token fires:
/// the calls the turn already made reported their inputs.
#[tokio::test]
async fn a_cancelled_turn_journals_the_context_its_calls_measured() {
    let dir = tempfile::tempdir().unwrap();
    let scripted = ScriptedProvider::new(vec![
        // Two calls ran, each reading a transcript a little longer than
        // the one before: 1000 tokens, then 1500.
        tool_reply("t1", 1_000),
        tool_reply("t2", 1_500),
        // A third goes out and is cut before it reports anything.
        ScriptedProvider::text("never reached"),
    ]);
    let cancel = CancellationToken::new();
    let provider = CancellingProvider::new(scripted, 2, cancel.clone());
    let (agent, session) = over(dir.path(), provider);
    let mut events = agent.bus().subscribe();
    let out = agent
        .run_turn(vec![ContentBlock::text("go")], cancel)
        .await
        .unwrap();
    assert!(matches!(out, TurnOutcome::Cancelled { .. }), "{out:?}");

    let s = session.lock().await;
    let kinds: Vec<&RecordKind> = s.branch().iter().map(|r| &r.kind).collect();
    let context = kinds
        .iter()
        .position(|k| matches!(k, RecordKind::ContextSize { .. }))
        .expect("the stopped turn measured a context");
    assert!(
        matches!(kinds[context], RecordKind::ContextSize { tokens: 1_500 }),
        "the last call that reported an input, and never the turn's own \
         total: {:?}",
        kinds[context]
    );
    assert_eq!(
        s.last_input_tokens(),
        Some(1_500),
        "which is what the gauge and the inspector read"
    );
    // The turn's spend is a different number — 1000 + 1500, the sum over
    // every call — and reading *it* as a context is the arithmetic that
    // drew `ctx 12.92M/1.00M (1292%)`.
    let spend = kinds
        .iter()
        .position(|k| matches!(k, RecordKind::TurnSpend { .. }))
        .expect("the stopped turn priced its calls");
    assert!(matches!(kinds[spend], RecordKind::TurnSpend { calls: 3, .. }));
    assert_eq!(s.usage().input_tokens, 2_500);
    assert!(
        context < spend,
        "the measurement is written before the figures it explains"
    );
    drop(s);

    // And published, because a *live* consumer has no log to read: the
    // gauge moves in the same frame the stopped turn is reported in.
    let mut seen = Vec::new();
    while let Ok(e) = events.try_recv() {
        match e {
            Event::ContextSize { tokens } => seen.push(("context", tokens)),
            Event::Cancelled { .. } => seen.push(("cancelled", 0)),
            _ => {}
        }
    }
    assert_eq!(
        seen,
        vec![("context", 1_500), ("cancelled", 0)],
        "the context arrives before the boundary it belongs to"
    );
}

/// An agent over any provider, for the steering tests.
fn steered(dir: &std::path::Path, provider: Arc<SteeringProvider>) -> Arc<Agent> {
    let session = Session::create(&dir.join("s.log"), "m", dir, Some("sys".into())).unwrap();
    let session = Arc::new(Mutex::new(session));
    let mut reg = ToolRegistry::new();
    reg.register(echo(Approval::ReadOnly));
    let user = ScriptedUser::new(true);
    let d = Arc::new(Dispatcher::new(
        reg,
        Arc::new(AllowAll),
        user,
        EventBus::default(),
        session,
        dir.to_path_buf(),
    ));
    let agent = Arc::new(Agent::new(
        provider.clone(),
        d,
        AgentConfig {
            model: "m".into(),
            system: Some("sys".into()),
            ..Default::default()
        },
    ));
    provider.agent.set(Arc::downgrade(&agent)).ok().unwrap();
    agent
}

/// pi's `steer`: said during a tool call, read before the next model call,
/// *beside* the tool results — one user message, results first.
#[tokio::test]
async fn a_steer_is_read_before_the_next_call_beside_the_results() {
    let dir = tempfile::tempdir().unwrap();
    let scripted = ScriptedProvider::new(vec![
        ScriptedProvider::tool("t1", "echo", json!({"s":"hi"})),
        ScriptedProvider::text("changed course"),
    ]);
    let provider = SteeringProvider::new(scripted.clone(), 0, &["actually, stop after this"]);
    let agent = steered(dir.path(), provider);
    let out = agent
        .run_turn(vec![ContentBlock::text("go")], CancellationToken::new())
        .await
        .unwrap();
    assert!(
        matches!(out, TurnOutcome::Settled { calls: 2, .. }),
        "{out:?}"
    );

    let last = {
        let reqs = scripted.requests.lock().unwrap();
        assert_eq!(reqs.len(), 2);
        reqs[1].messages.last().unwrap().clone()
    };
    assert_eq!(last.role, Role::User);
    assert!(
        matches!(&last.content[0], ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "t1"),
        "results first: {:?}",
        last.content
    );
    assert!(
        matches!(&last.content[1], ContentBlock::Text { text } if text == "actually, stop after this"),
        "then the words: {:?}",
        last.content
    );
    assert_eq!(
        last.content.len(),
        2,
        "one message, not two: {:?}",
        last.content
    );

    // Journaled as the operator speaking, after the result it rode with.
    let session = agent.session().lock().await;
    let branch = session.branch();
    let kinds: Vec<&str> = branch
        .iter()
        .map(|r| match &r.kind {
            RecordKind::UserMessage(m) if m.text() == "actually, stop after this" => "steer",
            RecordKind::UserMessage(_) => "user",
            RecordKind::ToolResult { .. } => "result",
            RecordKind::AssistantMessage(_) => "assistant",
            RecordKind::TurnSettled { .. } => "settled",
            _ => "",
        })
        .filter(|k| !k.is_empty())
        .collect();
    assert_eq!(
        kinds,
        [
            "user",
            "assistant",
            "result",
            "steer",
            "assistant",
            "settled"
        ]
    );
}

/// A steer that arrives during what would have been the last call is not
/// left for the operator's next message: the turn makes one more call.
#[tokio::test]
async fn a_turn_does_not_settle_with_a_steer_unread() {
    let dir = tempfile::tempdir().unwrap();
    let scripted = ScriptedProvider::new(vec![
        ScriptedProvider::text("first"),
        ScriptedProvider::text("second"),
    ]);
    let provider = SteeringProvider::new(scripted.clone(), 0, &["one more thing"]);
    let agent = steered(dir.path(), provider);
    let out = agent
        .run_turn(vec![ContentBlock::text("go")], CancellationToken::new())
        .await
        .unwrap();
    assert!(
        matches!(out, TurnOutcome::Settled { calls: 2, .. }),
        "{out:?}"
    );
    {
        let reqs = scripted.requests.lock().unwrap();
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[1].messages.last().unwrap().text(), "one more thing");
    }
    let session = agent.session().lock().await;
    let settles = session
        .branch()
        .iter()
        .filter(|r| matches!(r.kind, RecordKind::TurnSettled { .. }))
        .count();
    assert_eq!(settles, 1, "one turn, two calls");
}

/// Two messages typed during one round of a turn — the ordinary shape
/// now that sending mid-turn steers — are journalled as two records and
/// read together beside the results, in one user message: never two user
/// turns in a row.
#[tokio::test]
async fn two_steers_at_one_safe_point_ride_one_user_message() {
    let dir = tempfile::tempdir().unwrap();
    let scripted = ScriptedProvider::new(vec![
        ScriptedProvider::tool("t1", "echo", json!({"s":"hi"})),
        ScriptedProvider::text("changed course"),
    ]);
    let provider = SteeringProvider::new(scripted.clone(), 0, &["first redirect", "second redirect"]);
    let agent = steered(dir.path(), provider);
    let out = agent
        .run_turn(vec![ContentBlock::text("go")], CancellationToken::new())
        .await
        .unwrap();
    assert!(
        matches!(out, TurnOutcome::Settled { calls: 2, .. }),
        "{out:?}"
    );
    {
        let reqs = scripted.requests.lock().unwrap();
        assert_eq!(reqs.len(), 2);
        let last = reqs[1].messages.last().unwrap();
        assert_eq!(last.role, Role::User);
        assert!(
            matches!(&last.content[0], ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "t1"),
            "results first: {:?}",
            last.content
        );
        let words: Vec<&str> = last
            .content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            words,
            ["first redirect", "second redirect"],
            "both steers in the one message: {:?}",
            last.content
        );
    }
    // The log keeps the two moments: two records, one per message said.
    let session = agent.session().lock().await;
    let steers = session
        .branch()
        .iter()
        .filter(|r| matches!(&r.kind, RecordKind::UserMessage(m)
            if m.text() == "first redirect" || m.text() == "second redirect"))
        .count();
    assert_eq!(steers, 2);
}

/// pi's `follow_up`: queued while a turn runs, run as a turn of its own
/// once it settles, inside the same `run_turn` call — so the call
/// returning means nothing queued is still going to run.
#[tokio::test]
async fn a_follow_up_runs_after_the_settle_in_the_same_call() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::text("a"),
        ScriptedProvider::text("b"),
    ]);
    let user = ScriptedUser::new(true);
    let (agent, s) = harness(
        dir.path(),
        provider.clone(),
        Arc::new(AllowAll),
        user,
        Approval::ReadOnly,
        false,
    );
    let mut events = agent.bus().subscribe();
    agent.follow_up(vec![ContentBlock::text("and then")]);
    assert_eq!(agent.queued(), 1);
    let out = agent
        .run_turn(vec![ContentBlock::text("go")], CancellationToken::new())
        .await
        .unwrap();
    assert!(matches!(out, TurnOutcome::Settled { .. }));
    assert_eq!(agent.queued(), 0);

    {
        let reqs = provider.requests.lock().unwrap();
        assert_eq!(reqs.len(), 2);
        assert_eq!(reqs[1].messages.last().unwrap().text(), "and then");
    }
    let session = s.lock().await;
    let branch = session.branch();
    assert_eq!(
        branch
            .iter()
            .filter(|r| matches!(r.kind, RecordKind::TurnSettled { .. }))
            .count(),
        2,
        "two turns, two settles"
    );
    assert_eq!(
        branch
            .iter()
            .filter(|r| matches!(r.kind, RecordKind::UserMessage(_)))
            .count(),
        2
    );

    // The queue's length is published both ways.
    let mut waits = Vec::new();
    while let Ok(e) = events.try_recv() {
        if let Event::Queued { waiting } = e {
            waits.push(waiting);
        }
    }
    assert_eq!(waits, [1, 0]);
}

/// A user message landing while tool results are owed — a steer, or the
/// operator speaking after a crash — rides in the message that carries
/// the results, and never becomes a second user turn in a row.
#[test]
fn a_user_message_on_owed_results_rides_the_same_message() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::create(&dir.path().join("s.log"), "m", dir.path(), None).unwrap();
    s.append(RecordKind::UserMessage(Message::user_text("go")))
        .unwrap();
    s.append(RecordKind::AssistantMessage(Message::assistant(vec![
        ContentBlock::ToolUse {
            id: "t1".into(),
            name: "echo".into(),
            input: Json("{}".into()),
        },
    ])))
    .unwrap();
    s.append(RecordKind::ToolResult {
        tool_use_id: "t1".into(),
        content: "out".into(),
        is_error: false,
    })
    .unwrap();
    s.append(RecordKind::UserMessage(Message::user_text("steer")))
        .unwrap();
    let msgs = s.messages();
    assert_eq!(msgs.len(), 3, "{msgs:?}");
    let last = &msgs[2];
    assert_eq!(last.role, Role::User);
    assert!(matches!(&last.content[0], ContentBlock::ToolResult { .. }));
    assert!(matches!(&last.content[1], ContentBlock::Text { text } if text == "steer"));
    // An ordinary turn boundary is untouched: nothing owed, nothing folded.
    s.append(RecordKind::AssistantMessage(Message::assistant(vec![
        ContentBlock::text("ok"),
    ])))
    .unwrap();
    s.append(RecordKind::TurnSettled {
        stop_reason: StopReason::EndTurn,
        usage: Usage::default(),
    })
    .unwrap();
    s.append(RecordKind::UserMessage(Message::user_text("next")))
        .unwrap();
    let msgs = s.messages();
    assert_eq!(msgs.len(), 5);
    assert_eq!(msgs[4].text(), "next");
}

/// A consumer that holds calls somewhere other than a screen — Melete,
/// parking one for a Telegram 👍 — overrides `approve` and is handed the
/// call and the ruling, not a sentence about them.
struct Approver(std::sync::Mutex<Vec<(String, String)>>);

#[async_trait]
impl UserIo for Approver {
    // Declines: proof that the override below is what answered.
    async fn choose(&self, _: &str, _: &[Choice], _: &CancellationToken) -> Option<String> {
        None
    }
    async fn approve(
        &self,
        call: &ToolCall,
        _: &ToolManifest,
        ruling: &Ruling,
        _: &CancellationToken,
    ) -> bool {
        self.0
            .lock()
            .unwrap()
            .push((call.name.clone(), ruling.reason_or_prompt().to_string()));
        true
    }
}

#[tokio::test]
async fn approve_is_handed_the_call_and_the_ruling() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::tool("t1", "echo", json!({"s":"hi"})),
        ScriptedProvider::text("done"),
    ]);
    let approver = Arc::new(Approver(Default::default()));
    let (agent, s) = harness(
        dir.path(),
        provider,
        Arc::new(AlwaysAsk),
        approver.clone(),
        Approval::Mutating,
        false,
    );
    agent
        .run_turn(vec![ContentBlock::text("go")], CancellationToken::new())
        .await
        .unwrap();
    let seen = approver.0.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, "echo");
    assert_eq!(seen[0].1, "Run `echo`?");
    // The call ran: the override said yes, whatever `choose` would have said.
    let session = s.lock().await;
    let branch = session.branch();
    assert!(branch.iter().any(|r| matches!(&r.kind, RecordKind::ToolResult { content, is_error: false, .. } if content == "echo:hi")));
    assert!(branch.iter().any(|r| matches!(
        &r.kind,
        RecordKind::PolicyVerdict {
            outcome: eidolon_core::session::PolicyOutcome::Approved,
            ..
        }
    )));
}

/// **The cache hit that arrived at `Stop` and went nowhere.**
///
/// On the Anthropic wire the input side of a call's usage — fresh tokens,
/// cache reads, cache writes — arrives at `message_start`, and the loop
/// folds all of it in at `Start`. On the chat-completions wire there is
/// no `Start` usage: the one usage object is the final chunk, and it
/// arrives at `Stop`. The `Stop` arm took the fresh count from it and
/// nothing else, so on z.ai, DeepSeek and every gateway a cache hit was
/// simply gone. A four-call turn on a warm prefix then settled at the sum
/// of its fresh *increments* (3332 tokens where the context was near six
/// thousand), `ContextSize` read as the last increment (555), and the
/// price never touched the cache-read rate. This scripts a `Stop` that
/// carries the counters and pins that all three reach the log.
#[tokio::test]
async fn cache_counters_that_arrive_at_stop_reach_the_settle_and_the_context() {
    use eidolon_core::message::StopReason;
    use eidolon_core::provider::StreamEvent;
    let dir = tempfile::tempdir().unwrap();
    let mut reply = ScriptedProvider::text("done");
    // What z.ai's final chunk says on a warm prefix: `prompt_tokens`
    // 45, of which 40 were read from the cache — so five were fresh.
    // (`ScriptedProvider` still puts a `Start` of ten fresh tokens in
    // front, which this `Stop`'s fresh count replaces, as an Anthropic
    // `message_delta` carrying `input_tokens` already did.)
    reply.push(StreamEvent::Stop {
        stop_reason: StopReason::EndTurn,
        usage: Usage {
            input_tokens: 5,
            output_tokens: 1,
            cache_read_input_tokens: 40,
            cache_creation_input_tokens: 2,
        },
    });
    let provider = ScriptedProvider::new(vec![reply]);
    let user = ScriptedUser::new(true);
    let (agent, session) = harness(
        dir.path(),
        provider,
        Arc::new(AllowAll),
        user,
        Approval::ReadOnly,
        false,
    );
    agent
        .run_turn(vec![ContentBlock::text("go")], CancellationToken::new())
        .await
        .unwrap();

    let s = session.lock().await;
    let settled = s
        .branch()
        .iter()
        .rev()
        .find_map(|r| match &r.kind {
            RecordKind::TurnSettled { usage, .. } => Some(*usage),
            _ => None,
        })
        .expect("the turn settled");
    assert_eq!(
        settled.input_tokens, 5,
        "the fresh count is the final chunk's, not Start's"
    );
    assert_eq!(
        settled.cache_read_input_tokens, 40,
        "the cache hit is on the settle"
    );
    assert_eq!(
        settled.cache_creation_input_tokens, 2,
        "and so is the write"
    );
    // The context is everything the last call read, cached or not.
    assert_eq!(s.last_input_tokens(), Some(47));
}

fn tool_names(r: &ChatRequest) -> Vec<String> {
    r.tools.iter().map(|t| t.name.clone()).collect()
}

/// A deferred tool is registered but not offered until the branch reaches
/// for it — through `tool_search`, or by calling it — and what the branch
/// has reached for is read off the log rather than kept anywhere: a
/// resumed session offers the same list, and a fork below the search does
/// not.
#[tokio::test]
async fn a_deferred_tool_is_offered_once_the_branch_has_reached_for_it() {
    let dir = tempfile::tempdir().unwrap();
    fn hidden() -> Arc<dyn Tool> {
        Arc::new(Echo(ToolManifest {
            name: "hidden_echo".into(),
            description: "An echo nobody is shown at first.".into(),
            input_schema: json!({"type":"object"}),
            approval: Approval::ReadOnly,
            prompt: Some("guidance for hidden".into()),
            render: None,
            deferred: true,
        }))
    }
    let build = |provider: Arc<ScriptedProvider>, resume: bool| -> (Agent, Arc<Mutex<Session>>) {
        let path = dir.path().join("s.log");
        let session = if resume {
            Session::open(&path).unwrap()
        } else {
            Session::create(&path, "m", dir.path(), Some("sys".into())).unwrap()
        };
        let session = Arc::new(Mutex::new(session));
        let mut reg = ToolRegistry::new();
        reg.register(echo(Approval::ReadOnly));
        reg.register(hidden());
        let search = Arc::new(ToolSearch::new());
        reg.register(search.clone());
        let d = Arc::new(Dispatcher::new(
            reg,
            Arc::new(AllowAll),
            ScriptedUser::new(true),
            EventBus::default(),
            session.clone(),
            dir.path().to_path_buf(),
        ));
        search.attach(&d);
        (
            Agent::new(
                provider,
                d,
                AgentConfig {
                    model: "m".into(),
                    system: Some("sys".into()),
                    ..Default::default()
                },
            ),
            session,
        )
    };

    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::tool("t1", "tool_search", json!({"query": "hidden"})),
        ScriptedProvider::tool("t2", "hidden_echo", json!({"s": "now"})),
        ScriptedProvider::text("done"),
    ]);
    let (agent, session) = build(provider.clone(), false);
    agent
        .run_turn(vec![ContentBlock::text("go")], CancellationToken::new())
        .await
        .unwrap();
    {
        let reqs = provider.requests.lock().unwrap();
        assert_eq!(reqs.len(), 3);
        assert_eq!(
            tool_names(&reqs[0]),
            ["echo", "tool_search"],
            "deferred, so not offered"
        );
        assert!(
            !reqs[0]
                .system
                .as_deref()
                .unwrap_or("")
                .contains("guidance for hidden"),
            "nor are its notes"
        );
        assert_eq!(
            tool_names(&reqs[1]),
            ["echo", "hidden_echo", "tool_search"],
            "the search opened it, in registration order"
        );
        assert!(
            reqs[1]
                .system
                .as_deref()
                .unwrap_or("")
                .contains("guidance for hidden"),
            "and its notes joined the prompt with it"
        );
        assert_eq!(
            tool_names(&reqs[2]),
            ["echo", "hidden_echo", "tool_search"],
            "and it stays open"
        );
    }
    // The search's own result told the model what it opened, schema included,
    // and the call to the deferred tool ran like any other.
    let fork_point = {
        let s = session.lock().await;
        let results: Vec<(String, String)> = s
            .branch()
            .into_iter()
            .filter_map(|r| match &r.kind {
                RecordKind::ToolResult {
                    tool_use_id,
                    content,
                    ..
                } => Some((tool_use_id.clone(), content.clone())),
                _ => None,
            })
            .collect();
        assert!(
            results[0].1.contains("hidden_echo") && results[0].1.contains("input_schema"),
            "{}",
            results[0].1
        );
        assert_eq!(results[1], ("t2".to_string(), "echo:now".to_string()));
        s.branch()[0].id
    };

    // Resume in a fresh process: the reach is read off the log, nothing is
    // replayed and nothing was journaled for it.
    let provider2 = ScriptedProvider::new(vec![
        ScriptedProvider::text("again"),
        ScriptedProvider::text("forked"),
    ]);
    let (agent2, _session2) = build(provider2.clone(), true);
    agent2
        .run_turn(vec![ContentBlock::text("more")], CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        tool_names(&provider2.requests.lock().unwrap()[0]),
        ["echo", "hidden_echo", "tool_search"]
    );

    // A fork below the search has not reached for it.
    agent2.fork_at(fork_point).await.unwrap();
    agent2
        .run_turn(
            vec![ContentBlock::text("from the top")],
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        tool_names(&provider2.requests.lock().unwrap()[1]),
        ["echo", "tool_search"]
    );
}

/// Calling a deferred tool by name — a model that remembers it, an
/// operator's dispatch line — opens it without a search.
#[tokio::test]
async fn a_deferred_tool_called_by_name_is_offered_from_then_on() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.log");
    let session = Arc::new(Mutex::new(
        Session::create(&path, "m", dir.path(), None).unwrap(),
    ));
    let mut reg = ToolRegistry::new();
    reg.register(echo(Approval::ReadOnly));
    reg.register(Arc::new(Echo(ToolManifest {
        name: "hidden_echo".into(),
        description: "hidden".into(),
        input_schema: json!({"type":"object"}),
        approval: Approval::ReadOnly,
        prompt: None,
        render: None,
        deferred: true,
    })));
    let d = Arc::new(Dispatcher::new(
        reg,
        Arc::new(AllowAll),
        ScriptedUser::new(true),
        EventBus::default(),
        session.clone(),
        dir.path().to_path_buf(),
    ));
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::tool("t1", "hidden_echo", json!({"s": "x"})),
        ScriptedProvider::text("ok"),
    ]);
    let agent = Agent::new(
        provider.clone(),
        d,
        AgentConfig {
            model: "m".into(),
            ..Default::default()
        },
    );
    agent
        .run_turn(vec![ContentBlock::text("go")], CancellationToken::new())
        .await
        .unwrap();
    let reqs = provider.requests.lock().unwrap();
    assert_eq!(tool_names(&reqs[0]), ["echo"]);
    assert_eq!(
        tool_names(&reqs[1]),
        ["echo", "hidden_echo"],
        "the call itself reached for it"
    );
}

// ## The wrap-up nudge

/// An agent under a stated iteration budget, with the bus kept so what
/// the loop says on exhaustion can be heard.
#[allow(clippy::type_complexity)]
fn budgeted(
    dir: &std::path::Path,
    responses: Vec<Vec<StreamEvent>>,
    max_iterations: u32,
) -> (
    Agent,
    Arc<ScriptedProvider>,
    Arc<Mutex<Session>>,
    tokio::sync::broadcast::Receiver<Event>,
) {
    let path = dir.join("s.log");
    let session = Arc::new(Mutex::new(
        Session::create(&path, "m", dir, Some("sys".into())).unwrap(),
    ));
    let mut reg = ToolRegistry::new();
    reg.register(echo(Approval::ReadOnly));
    let bus = EventBus::default();
    let rx = bus.subscribe();
    let d = Arc::new(Dispatcher::new(
        reg,
        Arc::new(AllowAll),
        ScriptedUser::new(true),
        bus,
        session.clone(),
        dir.to_path_buf(),
    ));
    let provider = ScriptedProvider::new(responses);
    let agent = Agent::new(
        provider.clone(),
        d,
        AgentConfig {
            model: "m".into(),
            max_iterations,
            ..Default::default()
        },
    );
    (agent, provider, session, rx)
}

/// A model told to wrap up can settle on a summary instead of being cut
/// off — the outcome the nudge exists to produce.
#[tokio::test]
async fn a_turn_near_its_limit_is_told_to_wrap_up() {
    let dir = tempfile::tempdir().unwrap();
    // Budget 4, so the nudge lands when 2 calls remain: after the second
    // reply's results are on the branch, before the third call.
    let (agent, provider, session, _rx) = budgeted(
        dir.path(),
        vec![
            ScriptedProvider::tool("t1", "echo", json!({"s":"a"})),
            ScriptedProvider::tool("t2", "echo", json!({"s":"b"})),
            ScriptedProvider::tool("t3", "echo", json!({"s":"c"})),
            ScriptedProvider::text("wrapped up: done a b c"),
        ],
        4,
    );
    let out = agent
        .run_turn(vec![ContentBlock::text("go")], CancellationToken::new())
        .await
        .unwrap();
    assert!(
        matches!(out, TurnOutcome::Settled { .. }),
        "a model that heeds the nudge settles: {out:?}"
    );

    // Exactly one nudge on the branch, saying what was left when it fired.
    let s = session.lock().await;
    let branch = s.branch();
    let budgets: Vec<_> = branch
        .iter()
        .filter(|r| matches!(r.kind, RecordKind::TurnBudget { .. }))
        .collect();
    assert_eq!(budgets.len(), 1, "the nudge fires once per turn");
    assert!(matches!(
        budgets[0].kind,
        RecordKind::TurnBudget { calls_left: 2 }
    ));

    // The call it preceded carried it, folded into the message holding
    // the previous reply's results — one user message, results first,
    // then the harness's words, never two user turns in a row.
    let reqs = provider.requests.lock().unwrap();
    let third = &reqs[2];
    let carrier = third
        .messages
        .iter()
        .find(|m| {
            m.content.iter().any(|b| {
                matches!(b, ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "t2")
            })
        })
        .expect("t2's result rides a user message");
    assert!(
        carrier.text().contains("not the operator"),
        "the frame travels with the number: {}",
        carrier.text()
    );
    assert!(
        carrier.text().contains("2 model calls left"),
        "the model learns what it has to spend: {}",
        carrier.text()
    );
    assert_eq!(
        carrier.role,
        harnox::llm::message::Role::User,
        "user-role, like a steer"
    );
    // And the branch remembers it, so the last call (and any later
    // resume) still sees the nudge where it landed.
    assert!(reqs[3].messages.iter().any(|m| m
        .text()
        .contains("not the operator")));
}

/// A model that ignores the nudge still exhausts — the wall did not move
/// — but the branch says the model was warned, and the error says what
/// the operator can do about it.
#[tokio::test]
async fn a_turn_that_ignores_the_nudge_exhausts_with_it_journaled() {
    let dir = tempfile::tempdir().unwrap();
    // Budget 3: the nudge fires when 1 call remains (half the budget,
    // clamped to at most 8), the model calls a tool anyway, and the wall
    // arrives.
    let (agent, _provider, session, mut rx) = budgeted(
        dir.path(),
        vec![
            ScriptedProvider::tool("t1", "echo", json!({"s":"a"})),
            ScriptedProvider::tool("t2", "echo", json!({"s":"b"})),
            ScriptedProvider::tool("t3", "echo", json!({"s":"c"})),
        ],
        3,
    );
    let out = agent
        .run_turn(vec![ContentBlock::text("go")], CancellationToken::new())
        .await
        .unwrap();
    assert!(matches!(out, TurnOutcome::Exhausted { calls: 3, .. }));

    let s = session.lock().await;
    let branch = s.branch();
    assert!(matches!(
        branch
            .iter()
            .rev()
            .find(|r| matches!(r.kind, RecordKind::TurnBudget { .. }))
            .map(|r| &r.kind),
        Some(RecordKind::TurnBudget { calls_left: 1 })
    ));
    // The wall is also a way for a turn to end without a settle, so it
    // says how full the context was, exactly as a cancel does: the last
    // call read 10 tokens (the turn's own total is 30 over three calls,
    // and reading *that* as a context is the arithmetic the gauge was
    // fixed for).
    let kinds: Vec<&RecordKind> = branch.iter().map(|r| &r.kind).collect();
    let context = kinds
        .iter()
        .position(|k| matches!(k, RecordKind::ContextSize { .. }))
        .expect("the exhausted turn measured a context");
    assert!(matches!(kinds[context], RecordKind::ContextSize { tokens: 10 }));
    let spend = kinds
        .iter()
        .position(|k| matches!(k, RecordKind::TurnSpend { .. }))
        .expect("the exhausted turn priced its calls");
    assert!(context < spend, "the measurement precedes the spend");
    assert_eq!(s.last_input_tokens(), Some(10));
    assert_eq!(s.usage().input_tokens, 30);
    drop(s);

    // The error names the limit and the way out, in the same breath.
    let mut error = None;
    let mut gauged = None;
    while let Ok(ev) = rx.try_recv() {
        match ev {
            Event::Error(e) => error = Some(e),
            Event::ContextSize { tokens } => gauged = Some(tokens),
            _ => {}
        }
    }
    let error = error.expect("the loop said something about the limit");
    assert!(error.contains("3 model calls"), "{error}");
    assert!(error.contains("continues from where this stopped"), "{error}");
    assert_eq!(gauged, Some(10), "and a live consumer is told the context");
}

/// The nudge only exists for turns that get deep: a turn that settles in
/// a few calls never hears about a budget it never came close to.
#[tokio::test]
async fn a_short_turn_never_hears_about_the_budget() {
    let dir = tempfile::tempdir().unwrap();
    let (agent, _provider, session, _rx) = budgeted(
        dir.path(),
        vec![
            ScriptedProvider::tool("t1", "echo", json!({"s":"a"})),
            ScriptedProvider::text("done"),
        ],
        128,
    );
    agent
        .run_turn(vec![ContentBlock::text("go")], CancellationToken::new())
        .await
        .unwrap();
    let s = session.lock().await;
    let branch = s.branch();
    assert!(
        !branch
            .iter()
            .any(|r| matches!(r.kind, RecordKind::TurnBudget { .. })),
        "no nudge for a turn that settles early"
    );
}
