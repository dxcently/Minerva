//! Parks, end to end on the loop.
//!
//! Four properties, and each is one the design leans on: the turn a
//! `wait_for` call is made in **ends there** (the harness owns that decision,
//! because a model left to stop on its own goes and polls instead), a fire is
//! **journaled before it reaches the model** so replay can say why the session
//! woke, a park from an earlier turn **cannot** cut short a later one (or a
//! typed message could never be answered), and a session that **died parked**
//! wakes on resume — the park is rebuilt from the branch alone, the deadline
//! it was given is the deadline it keeps, and a wait the branch shows
//! finished is left finished.

use std::path::{Path, PathBuf};
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
    harness_on(session, provider, fake)
}

/// The same wiring over a session the caller made — the shape a resume
/// builds: the log is the one the dead process wrote, the parks and the
/// tool are new, and the cwd comes from the log, not the caller.
fn harness_on(session: Session, provider: Arc<ScriptedProvider>, fake: Arc<Fake>) -> Harness {
    let cwd = PathBuf::from(session.cwd().unwrap_or_else(|| ".".into()));
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
            script_only: false,
            plugin: None,
            source_hash: None,
        },
    }));
    let dispatcher = Arc::new(Dispatcher::new(
        reg,
        Arc::new(AllowAll),
        ScriptedUser::new(true),
        EventBus::default(),
        session.clone(),
        cwd,
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
/// waiting must not end a turn the operator's own message started — and the
/// turn it does not end is the turn that **retires** it: the session is not
/// still waiting on a condition it was woken away from, and the one slot has
/// to be free for a `wait_for` in the new turn (bugs tracker #21).
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
    // The wait the message interrupted ends as that turn begins, and the
    // driver is handed the fact so it can say so. Until 2026-09-28 the park
    // simply stayed armed, holding the one slot: arming a corrected
    // condition was then refused with no way to cancel it.
    let retired = h.agent.parks().take_retired();
    assert_eq!(retired.len(), 1, "one wait ended: {retired:?}");
    assert!(
        retired[0].condition.contains("never"),
        "and it is named: {:?}",
        retired[0]
    );
    assert!(
        !h.agent.parks().is_armed(),
        "the interrupted wait is over, and the slot is free"
    );
    assert!(h.agent.parks().take_retired().is_empty(), "taken once, not twice");
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

/// The property the resume leans on: a session that died parked wakes anyway.
/// The park is rebuilt from the branch — the same registration, the same
/// deadline — and the wake is the ordinary one: a `TriggerFired` on the
/// branch, a frame to the model, a turn to answer it in.
#[tokio::test]
async fn a_session_that_died_parked_wakes_when_resumed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.log");
    let provider = ScriptedProvider::new(vec![ScriptedProvider::tool(
        "call-1",
        "wait_for",
        json!({
            "condition": { "peer_idle": "eidolon-5a0e" },
            "timeout_s": 600,
            "note": "before I touch the shared file"
        }),
    )]);
    let fake = Arc::new(Fake::default());
    let h = harness(dir.path(), provider, fake);
    let out = h.turn("wait for the peer before editing").await;
    assert!(
        matches!(out, TurnOutcome::Settled { stop_reason: StopReason::Waiting, .. }),
        "the turn did not park: {out:?}"
    );
    assert!(h.agent.parks().is_armed());
    drop(h); // The process is gone. The branch is what survives.

    // The resume: the log is reopened, and nothing is armed until the
    // branch says there was a wait.
    let fake = Arc::new(Fake::default());
    let provider = ScriptedProvider::new(vec![ScriptedProvider::text("awake — carrying on")]);
    let h = harness_on(Session::open(&path).unwrap(), provider.clone(), fake.clone());
    assert!(
        !h.agent.parks().is_armed(),
        "a fresh resume armed something on its own"
    );
    let note = h
        .agent
        .rearm_from_branch(eidolon_core::wait::DEFAULT_TIMEOUT, eidolon_core::wait::MAX_TIMEOUT)
        .await
        .unwrap()
        .expect("the branch ends with a parked wait and nothing was re-armed");
    assert!(note.contains("eidolon-5a0e"), "{note}");
    assert!(h.agent.parks().is_armed(), "the re-arm did not arm");
    let waiting = h.agent.parks().waiting_on().unwrap();
    assert!(waiting.contains("eidolon-5a0e"), "{waiting}");
    assert!(
        !waiting.contains("(deadline in 600s)"),
        "the deadline restarted instead of being kept: {waiting}"
    );

    // The world changes while the resumed session waits: the ordinary wake.
    fake.idle.store(true, Ordering::SeqCst);
    h.settled().await;
    let fires = h.agent.take_fires().await;
    assert_eq!(fires.len(), 1, "the re-armed park's fire was not journaled");
    let out = h.agent.continue_turn(CancellationToken::new()).await.unwrap();
    assert!(
        matches!(out, TurnOutcome::Settled { stop_reason: StopReason::EndTurn, .. }),
        "{out:?}"
    );
    let requests = provider.requests.lock().unwrap();
    assert_eq!(requests.len(), 1, "the wake cost more than one model call");
    assert!(
        requests.last().unwrap().messages.iter().any(|m| {
            m.content.iter().any(|b| matches!(
                b,
                ContentBlock::ToolResult { tool_use_id, .. } if tool_use_id == "call-1"
            ))
        }),
        "the resumed transcript lost the registration"
    );
    let said = requests
        .last()
        .unwrap()
        .messages
        .iter()
        .map(|m| m.text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        said.contains("You asked to be woken when session eidolon-5a0e is idle"),
        "the model was not told what it had been waiting for: {said}"
    );
    assert!(
        said.contains("It resolved: it fired — session eidolon-5a0e is idle"),
        "the model was not told how the wait resolved: {said}"
    );
}

/// A deadline that passed while the process was down is not restarted: the
/// first evaluation after the resume resolves the wait, through the ordinary
/// wake — the model is told the deadline arrived, not left waiting a second
/// full timeout for a fact already settled.
#[tokio::test]
async fn a_deadline_that_passed_while_down_fires_at_the_first_evaluation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.log");
    let provider = ScriptedProvider::new(vec![ScriptedProvider::tool(
        "call-1",
        "wait_for",
        json!({ "condition": { "peer_idle": "slow-peer" }, "timeout_s": 1 }),
    )]);
    let h = harness(dir.path(), provider, Arc::new(Fake::default()));
    let out = h.turn("wait one second for something that will not come").await;
    assert!(
        matches!(out, TurnOutcome::Settled { stop_reason: StopReason::Waiting, .. }),
        "the turn did not park: {out:?}"
    );
    assert!(h.agent.parks().is_armed());
    drop(h);
    tokio::time::sleep(Duration::from_millis(1150)).await; // The deadline passes in the dark.

    let fake = Arc::new(Fake::default());
    let provider = ScriptedProvider::new(vec![ScriptedProvider::text("it will never come")]);
    let h = harness_on(Session::open(&path).unwrap(), provider.clone(), fake);
    h.agent
        .rearm_from_branch(eidolon_core::wait::DEFAULT_TIMEOUT, eidolon_core::wait::MAX_TIMEOUT)
        .await
        .unwrap()
        .expect("the branch ends with a parked wait");
    h.settled().await; // The first evaluation is enough.
    let fires = h.agent.take_fires().await;
    assert_eq!(fires.len(), 1);
    {
        let s = h.session.lock().await;
        assert!(
            s.records().iter().any(|r| matches!(&r.kind,
                RecordKind::TriggerFired { outcome, .. }
                    if outcome.starts_with("the deadline arrived first"))),
            "the fire does not say the deadline arrived"
        );
    }
    let out = h.agent.continue_turn(CancellationToken::new()).await.unwrap();
    assert!(
        matches!(out, TurnOutcome::Settled { stop_reason: StopReason::EndTurn, .. }),
        "{out:?}"
    );
    let said = provider
        .requests
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .messages
        .iter()
        .map(|m| m.text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        said.contains("It resolved: the deadline arrived first"),
        "the model was not told the deadline passed: {said}"
    );
}

/// A wait the branch shows resolved stays resolved: its `TriggerFired` is on
/// the branch, the wake happened, and a resume that re-armed it would wake
/// the model twice for one wait.
#[tokio::test]
async fn a_wait_the_branch_says_resolved_is_not_rearmed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.log");
    let provider = ScriptedProvider::new(vec![ScriptedProvider::tool(
        "call-1",
        "wait_for",
        json!({ "condition": { "peer_idle": "eidolon-5a0e" }, "timeout_s": 600 }),
    )]);
    let fake = Arc::new(Fake::default());
    let h = harness(dir.path(), provider, fake.clone());
    let out = h.turn("wait for the peer").await;
    assert!(
        matches!(out, TurnOutcome::Settled { stop_reason: StopReason::Waiting, .. }),
        "the turn did not park: {out:?}"
    );
    fake.idle.store(true, Ordering::SeqCst);
    h.settled().await;
    h.agent.take_fires().await; // The wake is on the branch.
    drop(h);

    let fake = Arc::new(Fake::default());
    let provider = ScriptedProvider::new(vec![]);
    let h = harness_on(Session::open(&path).unwrap(), provider, fake);
    let rearmed = h
        .agent
        .rearm_from_branch(eidolon_core::wait::DEFAULT_TIMEOUT, eidolon_core::wait::MAX_TIMEOUT)
        .await
        .unwrap();
    assert!(rearmed.is_none(), "a resolved wait was re-armed: {rearmed:?}");
    assert!(!h.agent.parks().is_armed(), "and it armed a park");
}

/// The reading rule itself, over records written by hand: what the branch
/// says ended the wait (its own resolution, a later turn's opening, mail the
/// park's level admitted) and what did not (mail the level refused, a
/// fan-out, an operator tool dispatch). Also here: the operator's own
/// `wait_for` dispatch arms a park the walk can read, and a branch with no
/// settle re-arms nothing — an unsettled turn is `continue_turn`'s to
/// resume, not a park's.
#[tokio::test]
async fn the_walk_reads_retirement_off_the_branch() {
    let walk = |s: &Session| {
        eidolon_core::wait::parked_wait(
            s.branch().as_slice(),
            Duration::from_secs(900),
            Duration::from_secs(6 * 60 * 60),
        )
    };
    let input = |wake: Option<u8>| {
        let mut v = json!({ "condition": { "peer_idle": "p" }, "timeout_s": 600 });
        if let Some(w) = wake {
            v["wake"] = json!(w);
        }
        Json::from_value(&v)
    };

    // The branch ends on the armed result and its `Waiting` settle: live.
    let (dir, s) = parked_branch(input(None), true);
    let parked = walk(&s).expect("the live park was not found");
    assert_eq!(parked.call_id, "call-1");
    assert_eq!(parked.timeout, Duration::from_secs(600));
    drop(s);
    drop(dir);

    // Its own resolution on the branch: finished.
    let (dir, mut s) = parked_branch(input(None), true);
    s.append(RecordKind::TriggerFired {
        call_id: "call-1".into(),
        condition: "session p is idle".into(),
        outcome: "it fired".into(),
    })
    .unwrap();
    assert!(walk(&s).is_none(), "a resolved wait was found live");
    drop(s);
    drop(dir);

    // A later turn the operator started: retired, not live.
    let (dir, mut s) = parked_branch(input(None), true);
    s.append(RecordKind::UserMessage(Message::user_text("never mind")))
        .unwrap();
    assert!(walk(&s).is_none(), "a retired wait was found live");
    drop(s);
    drop(dir);

    // Direct mail at the default level is admitted: retired.
    let (dir, mut s) = parked_branch(input(None), true);
    s.append(peer_record(None)).unwrap();
    assert!(walk(&s).is_none(), "admitted mail left the wait live");
    drop(s);
    drop(dir);

    // The same mail at `wake: 0` is refused: the level the model chose
    // holds, and the wait is still live.
    let (dir, mut s) = parked_branch(input(Some(0)), true);
    s.append(peer_record(None)).unwrap();
    assert!(walk(&s).is_some(), "mail the level refused retired the wait");
    drop(s);
    drop(dir);

    // A fan-out is not waking mail: still live at the default level.
    let (dir, mut s) = parked_branch(input(None), true);
    s.append(peer_record(Some("general".into()))).unwrap();
    assert!(walk(&s).is_some(), "a fan-out retired the wait");
    drop(s);
    drop(dir);

    // An operator tool dispatch begins no turn: retires nothing.
    let (dir, mut s) = parked_branch(input(None), true);
    s.append(RecordKind::UserToolCall {
        tool_use_id: "op-1".into(),
        name: "search".into(),
        input: Json::from_value(&json!({})),
        utterance: None,
    })
    .unwrap();
    assert!(walk(&s).is_some(), "an operator tool dispatch retired the wait");
    drop(s);
    drop(dir);

    // No settle: the turn never finished. Not a parked branch — the
    // unsettled-turn resume path owns this one.
    let (dir, s) = parked_branch(input(None), false);
    assert!(walk(&s).is_none(), "an unsettled turn was read as a park");
    drop(s);
    drop(dir);

    // The operator's own `wait_for` dispatch carries its input the same
    // way, and the walk reads it the same way.
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::create(&dir.path().join("s.log"), "m", dir.path(), Some("sys".into())).unwrap();
    s.append(RecordKind::UserToolCall {
        tool_use_id: "op-2".into(),
        name: "wait_for".into(),
        input: input(None),
        utterance: Some("wait_for peer p".into()),
    })
    .unwrap();
    s.append(RecordKind::ToolResult {
        tool_use_id: "op-2".into(),
        content: format!(
            "{}2] waiting for session p\nwoken by a frame or the deadline, whichever is first; \
the turn ends here; deadline: 600s",
            eidolon_core::wait::ARMED_MARK
        ),
        is_error: false,
    })
    .unwrap();
    s.append(RecordKind::TurnSettled { stop_reason: StopReason::Waiting, usage: Usage::default() })
        .unwrap();
    let parked = walk(&s).expect("the operator's park was not found");
    assert_eq!(parked.call_id, "op-2");
}

fn peer_record(channel: Option<String>) -> RecordKind {
    RecordKind::PeerMessage {
        from: "eidolon-1111".into(),
        from_cwd: "/tmp/elsewhere".into(),
        channel,
        text: "a note from a colleague".into(),
    }
}

/// The prefix every live-at-death branch has: the armed `wait_for`, its
/// result, and — when `settle` — the `Waiting` settle.
fn parked_branch(input: Json, settle: bool) -> (tempfile::TempDir, Session) {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Session::create(&dir.path().join("s.log"), "m", dir.path(), Some("sys".into())).unwrap();
    s.append(RecordKind::UserMessage(Message::user_text("wait for p")))
        .unwrap();
    s.append(RecordKind::AssistantMessage(Message::assistant(vec![ContentBlock::ToolUse {
        id: "call-1".into(),
        name: "wait_for".into(),
        input,
    }])))
    .unwrap();
    s.append(RecordKind::ToolResult {
        tool_use_id: "call-1".into(),
        content: format!(
            "{}1] waiting for session p\nwoken by a frame or the deadline, whichever is first; \
the turn ends here; deadline: 600s",
            eidolon_core::wait::ARMED_MARK
        ),
        is_error: false,
    })
    .unwrap();
    if settle {
        s.append(RecordKind::TurnSettled { stop_reason: StopReason::Waiting, usage: Usage::default() })
            .unwrap();
    }
    (dir, s)
}
