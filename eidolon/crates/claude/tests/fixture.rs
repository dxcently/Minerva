//! Replay a captured `claude` stream through the sink and check the journal.

use std::sync::Arc;

use tokio::sync::Mutex;

use eidolon_claude::stream::Sink;
use eidolon_core::policy::AllowAll;
use eidolon_core::session::RecordKind;
use eidolon_core::testing::{ScriptedProvider, ScriptedUser};
use eidolon_core::*;

#[tokio::test]
async fn salve_fixture_journals_a_settled_turn() {
    let dir = tempfile::tempdir().unwrap();
    let session = Arc::new(Mutex::new(
        Session::create(&dir.path().join("s.log"), "m", dir.path(), None).unwrap(),
    ));
    let d = Arc::new(Dispatcher::new(
        ToolRegistry::new(),
        Arc::new(AllowAll),
        ScriptedUser::new(true),
        EventBus::default(),
        session.clone(),
        dir.path().to_path_buf(),
    ));
    let agent = Agent::new(ScriptedProvider::new(vec![]), d, AgentConfig::default());
    let mut rx = agent.bus().subscribe();
    session
        .lock()
        .await
        .append(RecordKind::UserMessage(Message::user_text(
            "Reply with exactly the two words: salve mundi",
        )))
        .unwrap();

    let mut sink = Sink::new(&agent, None);
    for line in include_str!("fixtures/salve.jsonl").lines() {
        sink.line(line).await.unwrap();
    }
    let out = sink.finish().await.unwrap();
    assert!(matches!(
        out,
        TurnOutcome::Settled {
            stop_reason: StopReason::EndTurn,
            ..
        }
    ));
    sink.caught_up().await.unwrap();

    let s = session.lock().await;
    assert_eq!(
        s.backend_session("claude-cli").as_deref(),
        Some("e2a60067-7aa5-4b62-b09a-2c9653e6927c")
    );
    // The end-of-turn mark is the last record on the branch: everything
    // above it — this turn included — is in the CLI's own session now, so
    // the next turn replays none of it.
    let (mark, _) = s.backend_session_mark("claude-cli").unwrap();
    assert_eq!(Some(mark), s.head());
    assert!(s.messages_after(Some(mark)).is_empty());
    assert!(s.is_settled());
    let reply = s.last_assistant_text().unwrap();
    assert!(reply.to_lowercase().contains("salve"), "{reply}");
    assert_eq!(s.usage().output_tokens, 106);
    assert!(
        s.messages().last().unwrap().content.iter().any(
            |b| matches!(b, ContentBlock::Thinking { signature, .. } if !signature.is_empty())
        )
    );

    let mut saw_delta = false;
    let mut saw_settled = false;
    while let Ok(ev) = rx.try_recv() {
        match ev {
            Event::TextDelta(_) => saw_delta = true,
            Event::TurnSettled { .. } => saw_settled = true,
            _ => {}
        }
    }
    assert!(saw_delta && saw_settled);
}
