//! Parks, end to end on the loop.
//!
//! Three properties, and each is one the design leans on: the turn a
//! `wait_for` call is made in **ends there** (the harness owns that decision,
//! because a model left to stop on its own goes and polls instead), a fire is
//! **journaled before it reaches the model** so replay can say why the session
//! woke, and a park from an earlier turn **cannot** cut short a later one (or
//! a typed message could never be answered).

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::json;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use eidolon_core::policy::AllowAll;
use eidolon_core::session::RecordKind;
use eidolon_core::testing::{ScriptedProvider, ScriptedUser};
use eidolon_core::wait::{Answer, Term, Vantage, WaitFor};
use eidolon_core::*;

/// The world a condition is asked about, with nothing real behind it: a peer
/// whose idleness a test can move, and a background command that may or may
/// not have finished.
#[derive(Default)]
struct Fake {
    idle: AtomicBool,
    exited: AtomicBool,
}

#[async_trait]
impl Vantage for Fake {
    async fn ask(&self, term: &Term) -> Answer {
        match term {
            Term::PeerIdle(_) => match self.idle.load(Ordering::SeqCst) {
                true => Answer::Yes,
                false => Answer::No,
            },
            Term::CommandFinished(_) => match self.exited.load(Ordering::SeqCst) {
                true => Answer::Yes,
                false => Answer::No,
            },
            _ => Answer::No,
        }
    }
}

/// A tool that does nothing, so a turn can make a call without anything
/// happening — the shape a park's settle check is reached by.
struct Noop {
    manifest: ToolManifest,
}

#[async_trait]
impl Tool for Noop {
    fn manifest(&self) -> &ToolManifest {
        &self.manifest
    }
    async fn call(&self, _: serde_json::Value, _: CallContext) -> anyhow::Result<ToolOutput> {
        Ok(ToolOutput::ok("nothing"))
    }
}

struct Harness {
    agent: Arc<Agent>,
    session: Arc<Mutex<Session>>,
}

fn harness(dir: &Path, provider: Arc<ScriptedProvider>, fake: Arc<Fake>) -> Harness {
    let session = Session::create(&dir.join("s.log"), "m", dir, Some("sys".into())).unwrap();
    let session = Arc::new(Mutex::new(session));
    // The parks are the consumer's: the tool is built against them, the
    // dispatcher is built against that registry, and the agent is handed the
    // same handle afterwards. That ordering is why the parks live behind a
    // `OnceLock` on the agent.
    let parks = Arc::new(Parks::new());
    parks.set_vantage(fake);
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(WaitFor::new(
        parks.clone(),
        std::time::Duration::from_secs(900),
        std::time::Duration::from_secs(6 * 60 * 60),
    )));
    reg.register(Arc::new(Noop {
        manifest: ToolManifest {
            name: "noop".into(),
            description: "does nothing".into(),
            input_schema: json!({"type": "object"}),
            approval: eidolon_core::policy::Approval::ReadOnly,
            prompt: None,
            render: None,
            deferred: false,
        },
    }));
    let dispatcher = Arc::new(Dispatcher::new(
        reg,
        Arc::new(AllowAll),
        ScriptedUser::new(true),
        EventBus::default(),
        session.clone(),
        dir.to_path_buf(),
    ));
    let agent = Arc::new(Agent::new(
        provider,
        dispatcher,
        AgentConfig {
            model: "m".into(),
            system: Some("sys".into()),
            ..Default::default()
        },
    ));
    agent.set_parks(parks);
    Harness { agent, session }
}

impl Harness {
    async fn turn(&self, said: &str) -> TurnOutcome {
        self.agent
            .run_turn(vec![ContentBlock::text(said)], CancellationToken::new())
            .await
            .expect("the turn failed")
    }

    /// Wait for the watcher to resolve the park.
    async fn settled(&self) {
        let start = Instant::now();
        while self.agent.parks().is_armed() && start.elapsed() < Duration::from_secs(5) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!self.agent.parks().is_armed(), "the park never resolved");
    }
}

/// The property the whole primitive rests on: the reply that armed the park is
/// the reply that ends the turn, so nothing the model can call afterwards can
/// look at the condition.
#[tokio::test]
async fn the_turn_a_park_is_armed_in_ends_there() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::tool(
            "call-1",
            "wait_for",
            json!({
                "condition": { "peer_idle": "eidolon-5a0e" },
                "timeout_s": 30,
                "note": "before I touch the shared file"
            }),
        ),
        ScriptedProvider::text("the peer is idle — carrying on"),
    ]);
    let fake = Arc::new(Fake::default());
    let h = harness(dir.path(), provider.clone(), fake.clone());

    let out = h.turn("wait for the peer before editing").await;
    assert!(
        matches!(out, TurnOutcome::Settled { stop_reason: StopReason::Waiting, .. }),
        "the turn did not settle as Waiting: {out:?}"
    );
    assert_eq!(
        provider.requests.lock().unwrap().len(),
        1,
        "the loop made a second model call after the park was armed"
    );
    assert!(h.agent.parks().is_armed(), "nothing is armed");
    assert!(
        h.agent.parks().waiting_on().unwrap().contains("eidolon-5a0e"),
        "{:?}",
        h.agent.parks().waiting_on()
    );
    // The settle is on the branch, and says why the turn stopped.
    {
        let s = h.session.lock().await;
        assert!(
            matches!(
                s.records().last().map(|r| &r.kind),
                Some(RecordKind::TurnSettled { stop_reason: StopReason::Waiting, .. })
            ),
            "the last record is not a Waiting settle"
        );
    }

    // The world changes under it: the peer goes idle, the harness wakes.
    fake.idle.store(true, Ordering::SeqCst);
    h.settled().await;
    let fires = h.agent.take_fires().await;
    assert_eq!(fires.len(), 1, "the fire was not journaled");
    {
        let s = h.session.lock().await;
        assert!(
            s.records().iter().any(|r| matches!(
                &r.kind,
                RecordKind::TriggerFired { condition, outcome, .. }
                    if outcome == "it fired — session eidolon-5a0e is idle"
                        && condition.contains("eidolon-5a0e")
            )),
            "no TriggerFired record on the branch: {:?}",
            s.records().len()
        );
    }

    // And the wake is a turn: the model reads the frame, not a colleague's note.
    let out = h.agent.continue_turn(CancellationToken::new()).await.unwrap();
    assert!(
        matches!(out, TurnOutcome::Settled { stop_reason: StopReason::EndTurn, .. }),
        "{out:?}"
    );
    let requests = provider.requests.lock().unwrap();
    let last = requests.last().expect("no second request");
    let said = last
        .messages
        .iter()
        .map(|m| m.text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        said.contains("You asked to be woken when session eidolon-5a0e is idle"),
        "the model was not told what it was waiting for: {said}"
    );
    assert!(
        said.contains("It resolved: it fired — session eidolon-5a0e is idle"),
        "the model was not told which trigger fired: {said}"
    );
    assert!(
        said.contains("not a peer's message and not the operator's"),
        "the frame does not say who is speaking: {said}"
    );
}

/// A condition that is already true is not a park: the call says so and the
/// turn goes on, which is the difference between waiting and being woken by
/// something that had already happened.
#[tokio::test]
async fn an_already_true_condition_does_not_park() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    fake.idle.store(true, Ordering::SeqCst);
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::tool(
            "call-1",
            "wait_for",
            json!({ "condition": { "peer_idle": "p" } }),
        ),
        ScriptedProvider::text("nothing to wait for — carrying on"),
    ]);
    let h = harness(dir.path(), provider.clone(), fake);

    let out = h.turn("check the peer").await;
    assert!(
        matches!(out, TurnOutcome::Settled { stop_reason: StopReason::EndTurn, .. }),
        "{out:?}"
    );
    assert_eq!(
        provider.requests.lock().unwrap().len(),
        2,
        "the turn stopped instead of carrying on"
    );
    assert!(!h.agent.parks().is_armed(), "an already-true condition armed a park");
}

/// A park belongs to the turn that asked for it. One armed earlier and still
/// waiting must not end a turn the operator's own message started.
#[tokio::test]
async fn a_park_from_an_earlier_turn_cannot_end_a_later_one() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::tool(
            "call-1",
            "wait_for",
            json!({ "condition": { "peer_idle": "never" }, "timeout_s": 600 }),
        ),
        ScriptedProvider::tool("call-2", "noop", json!({})),
        ScriptedProvider::text("done"),
    ]);
    let fake = Arc::new(Fake::default());
    let h = harness(dir.path(), provider.clone(), fake);

    let out = h.turn("wait for a peer that never goes idle").await;
    assert!(
        matches!(out, TurnOutcome::Settled { stop_reason: StopReason::Waiting, .. }),
        "the first turn did not park: {out:?}"
    );

    // The operator types. The park is still armed; this turn must still run to
    // its own end — two more model calls, and an ordinary settle.
    let out = h.turn("never mind, do something else").await;
    assert!(
        matches!(out, TurnOutcome::Settled { stop_reason: StopReason::EndTurn, .. }),
        "a park from the previous turn ended this one: {out:?}"
    );
    assert_eq!(provider.requests.lock().unwrap().len(), 3);
    assert!(
        h.agent.parks().is_armed(),
        "the older park was spent by an unrelated turn"
    );
}

/// `any` names the leaf that actually fired, not the condition whole: the
/// deciding fact is one leaf's answer, and the words are what the journal
/// keeps, so replay gets the same name.
#[tokio::test]
async fn the_fire_names_the_leaf_that_fired() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::tool(
            "call-1",
            "wait_for",
            json!({ "condition": { "any": [
                { "peer_idle": "eidolon-aaaa" },
                { "file_exists": "/tmp/never-there" }
            ] } }),
        ),
        ScriptedProvider::text("carrying on"),
    ]);
    let fake = Arc::new(Fake::default());
    let h = harness(dir.path(), provider, fake.clone());

    let out = h.turn("wait for either").await;
    assert!(
        matches!(out, TurnOutcome::Settled { stop_reason: StopReason::Waiting, .. }),
        "the turn did not park: {out:?}"
    );
    fake.idle.store(true, Ordering::SeqCst);
    h.settled().await;
    h.agent.take_fires().await;
    let s = h.session.lock().await;
    let outcome = s
        .records()
        .iter()
        .find_map(|r| match &r.kind {
            RecordKind::TriggerFired { outcome, .. } => Some(outcome.clone()),
            _ => None,
        })
        .expect("no TriggerFired record on the branch");
    assert!(
        outcome == "it fired — session eidolon-aaaa is idle",
        "the fire does not name the leaf that fired: {outcome}"
    );
    assert!(
        !outcome.contains("/tmp/never-there"),
        "the fire names a leaf that did not fire: {outcome}"
    );
}

/// The deadline says what was still open, from the same per-leaf
/// evaluation — so the model can decide re-arm, re-ask, or give up
/// without rereading its own registration.
#[tokio::test]
async fn the_deadline_names_what_stayed_open() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::tool(
            "call-1",
            "wait_for",
            json!({
                "condition": { "all": [
                    { "peer_idle": "eidolon-bbbb" },
                    { "file_exists": "/tmp/also-never" }
                ] },
                "timeout_s": 1
            }),
        ),
        ScriptedProvider::text("giving up"),
    ]);
    let fake = Arc::new(Fake::default());
    let h = harness(dir.path(), provider, fake);

    let out = h.turn("wait briefly for two things that never come").await;
    assert!(
        matches!(out, TurnOutcome::Settled { stop_reason: StopReason::Waiting, .. }),
        "the turn did not park: {out:?}"
    );
    h.settled().await;
    h.agent.take_fires().await;
    let s = h.session.lock().await;
    let outcome = s
        .records()
        .iter()
        .find_map(|r| match &r.kind {
            RecordKind::TriggerFired { outcome, .. } => Some(outcome.clone()),
            _ => None,
        })
        .expect("no TriggerFired record on the branch");
    assert!(
        outcome == "the deadline arrived first — still open: session eidolon-bbbb is idle; \
/tmp/also-never exists",
        "the deadline does not name what stayed open: {outcome}"
    );
}
