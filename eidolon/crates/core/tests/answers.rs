//! The asking pair, at the door: the registry the asking seams hold, the
//! refusals that teach, and the advertisement that exists only while a
//! seam agrees to ask. The five requirements of the flow half are the
//! checklist — each named where it is pinned.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use eidolon_core::dispatch::{Dispatcher, OutstandingQuestion, QuestionBoard, QuestionKind};
use eidolon_core::policy::{AllowAll, Approval};
use eidolon_core::session::Session;
use eidolon_core::testing::{AlwaysAsk, HeldUser};
use eidolon_core::tool::{CallContext, Tool, ToolManifest, ToolOutput};
use eidolon_core::user::NoUser;

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

fn echo_tool() -> Echo {
    Echo(ToolManifest {
        name: "echo".into(),
        description: "echoes".into(),
        input_schema: json!({"type": "object"}),
        approval: Approval::ReadOnly,
        prompt: None,
        render: None,
        deferred: false,
        script_only: false,
        plugin: None,
        source_hash: None,
    })
}

/// A board that remembers what it was told, in order — the advertisement's
/// other end.
#[derive(Default)]
struct RecordingBoard {
    seen: std::sync::Mutex<Vec<Vec<String>>>,
}

impl QuestionBoard for RecordingBoard {
    fn publish(&self, questions: &[OutstandingQuestion]) {
        let mut ids: Vec<String> = questions.iter().map(|q| q.id.clone()).collect();
        ids.sort();
        self.seen.lock().unwrap().push(ids);
    }
}

/// A dispatcher with an echo tool and the user seam a test holds. `policy`
/// is the test's to choose: `AlwaysAsk` raises a gate question on every
/// call; `AllowAll` never asks.
fn dispatcher_with(
    policy: Arc<dyn eidolon_core::policy::PolicyHook>,
    user: Arc<dyn eidolon_core::user::UserIo>,
) -> Arc<Dispatcher> {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::create(&dir.path().join("t.eid"), "prov:m", dir.path(), None).unwrap();
    // The tempdir must outlive the session's file; leak it — tests are short.
    std::mem::forget(dir);
    let mut reg = eidolon_core::tool::ToolRegistry::new();
    reg.register(Arc::new(echo_tool()));
    Arc::new(Dispatcher::new(
        reg,
        policy,
        user,
        eidolon_core::event::EventBus::default(),
        Arc::new(tokio::sync::Mutex::new(session)),
        std::env::temp_dir(),
    ))
}

/// A gate question is refused with the reason, the entry survives the
/// refusal (the operator's dialog still stands), and it is gone when the
/// dialog ends — the door then says there is no such question.
/// *Requirement: refusals teach; registry removed on every exit.*
#[tokio::test]
async fn a_gate_question_refuses_a_peer_and_leaves_when_the_dialog_ends() {
    let held = HeldUser::new();
    let d = dispatcher_with(Arc::new(AlwaysAsk), held.clone());

    let turn = {
        let d = d.clone();
        tokio::spawn(async move {
            d.dispatch(
                eidolon_core::tool::ToolCall {
                    id: "gate-1".into(),
                    name: "echo".into(),
                    input: json!({"s": "a"}),
                    origin: eidolon_core::tool::CallOrigin::Model,
                },
                CancellationToken::new(),
            )
            .await
        })
    };
    held.entered.notified().await;

    let outstanding = d.outstanding_questions();    assert_eq!(outstanding.len(), 1, "the gate's question is registered");
    assert_eq!(outstanding[0].kind, QuestionKind::Approve);
    assert_eq!(outstanding[0].text, "Run `echo`?");

    // The refusal teaches, and names what the question is: the operator's
    // alone. Nothing journals — a refused answer leaves no record.
    let why = d.deliver_answer("gate-1", "yes", "eidolon-peer").unwrap_err();
    assert!(why.contains("the operator's alone"), "{why}");
    assert_eq!(d.outstanding_questions().len(), 1, "the dialog still stands");

    // The operator answers; the entry is removed by the same exit.
    held.release.notify_one();
    turn.await.unwrap();
    let why = d.deliver_answer("gate-1", "yes", "eidolon-peer").unwrap_err();
    assert!(
        why.contains("no question `gate-1` is outstanding"),
        "{why}"
    );
}

/// A park's question is what the door exists to carry: the first answer is
/// delivered, the second is told who won — "already answered" is not "no
/// question outstanding" — and an unknown id says there is nothing there.
/// *Requirements: first completion wins and the loser is told; refusals
/// teach.*
#[tokio::test]
async fn a_park_question_delivers_the_first_answer_and_tells_the_loser() {
    let d = dispatcher_with(Arc::new(AllowAll), Arc::new(NoUser));

    let (guard, rx) = d.question_open(QuestionKind::Respond, "w1-park0", "which way?".into());
    assert_eq!(
        d.deliver_answer("w1-park0", "left", "eidolon-aaa").unwrap(),
        ()
    );
    let delivery = rx.await.unwrap();
    assert_eq!(delivery.text, "left");
    assert_eq!(delivery.peer, "eidolon-aaa");

    // The loser is told, with the winner's name — not a shrug.
    let why = d
        .deliver_answer("w1-park0", "right", "eidolon-bbb")
        .unwrap_err();
    assert!(
        why.contains("already answered by `eidolon-aaa`"),
        "{why}"
    );
    // A question that never was says so, differently.
    let why = d.deliver_answer("w1-park9", "left", "eidolon-aaa").unwrap_err();
    assert!(why.contains("no question `w1-park9` is outstanding"), "{why}");

    drop(guard);
}

/// The guard is the removal: every exit of the asking seam — answered,
/// cancelled, errored — takes the question off the door. *Requirement:
/// registry removed on every exit.*
#[tokio::test]
async fn dropping_the_guard_removes_the_question_wherever_the_wait_ended() {
    let d = dispatcher_with(Arc::new(AllowAll), Arc::new(NoUser));

    let (guard, _rx) = d.question_open(QuestionKind::Respond, "w2-park0", "hold".into());
    assert_eq!(d.outstanding_questions().len(), 1);
    drop(guard);
    let why = d.deliver_answer("w2-park0", "late", "eidolon-aaa").unwrap_err();
    assert!(why.contains("no question `w2-park0` is outstanding"), "{why}");

    // A delivery aimed at a seam that already went is a tombstone with the
    // loser told, not a hang and not a silent drop.
    let (_guard2, rx2) = d.question_open(QuestionKind::Respond, "w2-park1", "hold".into());
    drop(rx2);
    let why = d.deliver_answer("w2-park1", "late", "eidolon-aaa").unwrap_err();
    assert!(
        why.contains("ended before the answer landed"),
        "{why}"
    );
    assert!(why.contains("answered by `eidolon-aaa`"), "{why}");
}

/// Nothing is advertised before the seam agrees to ask, and nothing after
/// it stops: the board sees the question appear with the registration and
/// vanish with the removal — including the empty advertisement the board
/// is set against.
/// *Requirement: nothing advertised before the inner seam agrees to ask.*
#[tokio::test]
async fn the_board_sees_exactly_the_questions_the_seams_hold() {
    let d = dispatcher_with(Arc::new(AllowAll), Arc::new(NoUser));
    let board = Arc::new(RecordingBoard::default());
    d.set_question_board(board.clone());
    assert_eq!(
        board.seen.lock().unwrap().last().map(Vec::len),
        Some(0),
        "setting the board publishes what stands: nothing"
    );

    let (guard, _rx) = d.question_open(QuestionKind::Respond, "w3-park0", "hold".into());
    assert_eq!(board.seen.lock().unwrap().last().map(Vec::len), Some(1));
    drop(guard);
    assert_eq!(
        board.seen.lock().unwrap().last().map(Vec::len),
        Some(0),
        "the advertisement ends when the wait does"
    );
}

/// An answer that names no peer is refused: the provenance record is only
/// as honest as the name it carries, and the door credits what arrives.
#[tokio::test]
async fn an_answer_that_names_no_peer_is_refused() {
    let d = dispatcher_with(Arc::new(AllowAll), Arc::new(NoUser));
    let (_guard, _rx) = d.question_open(QuestionKind::Respond, "w4-park0", "hold".into());
    let why = d.deliver_answer("w4-park0", "left", "").unwrap_err();
    assert!(why.contains("names no peer"), "{why}");
    assert_eq!(d.outstanding_questions().len(), 1, "still standing");
}
