//! Quiesce, end to end on the loop: an idle session settles at once, a busy
//! one settles at its boundary and never before it, a timeout refuses rather
//! than forcing, and the journal stops growing the moment the marker lands.
//!
//! The invariant under test throughout is the one from the vault contract:
//! **never mid-tool-call**. A quiesce rides the boundary the loop already
//! keeps, so the test that matters is the one where a tool call is parked and
//! the ask has to wait for it.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::future::BoxFuture;
use serde_json::json;
use tokio::sync::Notify;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use eidolon_core::policy::{AllowAll, Approval};
use eidolon_core::quiesce::{self, doorbell};
use eidolon_core::session::RecordKind;
use eidolon_core::testing::{ScriptedProvider, ScriptedUser};
use eidolon_core::user::tools::ChoicesUser;
use eidolon_core::*;

/// A tool that parks until it is let go: the turn that calls it is
/// mid-tool-call, which is exactly the state a quiesce must not interrupt.
struct Gate {
    manifest: ToolManifest,
    /// One permit, so a test that looks after the call still finds it.
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl Tool for Gate {
    fn manifest(&self) -> &ToolManifest {
        &self.manifest
    }
    async fn call(&self, _: serde_json::Value, _: CallContext) -> anyhow::Result<ToolOutput> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(ToolOutput::ok("through"))
    }
}

struct Harness {
    agent: Arc<Agent>,
    session: Arc<Mutex<Session>>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

/// One session, one gated tool, a scripted model. The response list is the
/// model's turns in order; the gate is what holds one of them mid-call.
fn harness(dir: &std::path::Path, provider: Arc<ScriptedProvider>) -> Harness {
    let session = Session::create(&dir.join("s.log"), "m", dir, Some("sys".into())).unwrap();
    let session = Arc::new(Mutex::new(session));
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(Gate {
        manifest: ToolManifest {
            name: "gate".into(),
            description: "park here".into(),
            input_schema: json!({"type": "object"}),
            approval: Approval::ReadOnly,
            prompt: None,
            render: None,
            deferred: false,
        },
        entered: entered.clone(),
        release: release.clone(),
    }));
    reg.register(Arc::new(ChoicesUser::new(ScriptedUser::new(true))));
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
    Harness {
        agent,
        session,
        entered,
        release,
    }
}

/// Is this record the quiesce marker?
fn is_marker(kind: &RecordKind) -> bool {
    matches!(kind, RecordKind::Note { text } if text.contains("quiesced for projection"))
}

#[tokio::test]
async fn an_idle_session_settles_at_once_and_its_journal_stops() {
    let dir = tempfile::tempdir().unwrap();
    let h = harness(dir.path(), ScriptedProvider::new(vec![]));

    // Nothing was asked, so there is nothing to wait for — and waiting must
    // not burn the timeout to discover that.
    assert!(
        !quiesce::quiesce_wait(&h.agent, Duration::from_millis(50))
            .await
            .unwrap()
    );
    assert!(!h.agent.quiesced());
    // The ask is noted (and only once: a second ask keeps the first venue).
    assert!(h.agent.request_quiesce("cannataxis"));
    assert!(!h.agent.request_quiesce("somewhere else"));
    assert_eq!(
        h.agent.quiesce_request().map(|q| q.destination).as_deref(),
        Some("cannataxis")
    );

    // An idle session has already reached its boundary: the poll settles it
    // and journals the marker, because no turn is coming to do it.
    assert!(h.agent.poll_quiesce().await.unwrap());
    assert!(h.agent.quiesced());
    assert!(quiesce::quiesce_wait(&h.agent, Duration::from_secs(1)).await.unwrap());

    {
        let s = h.session.lock().await;
        let branch = s.branch();
        let last = *branch.last().expect("a branch");
        assert!(is_marker(&last.kind), "{:?}", last.kind);
        match &last.kind {
            RecordKind::Note { text } => {
                assert!(text.contains("cannataxis"), "{text}");
                assert!(text.contains("the journal stops here"), "{text}");
            }
            other => panic!("{other:?}"),
        }
        assert!(s.is_settled(), "a marker does not unsettle the turn ahead of it");
    }

    // The post-condition the hop depends on: birth's journal does not grow
    // again. A turn is the only thing that could, and it is refused.
    let before = h.session.lock().await.records().len();
    let e = h
        .agent
        .run_turn(vec![ContentBlock::text("more")], CancellationToken::new())
        .await
        .unwrap_err()
        .to_string();
    assert!(e.contains("quiesced"), "{e}");
    assert_eq!(
        h.session.lock().await.records().len(),
        before,
        "a refused turn writes nothing at all, not even its user message"
    );
}

/// The load-bearing one: a quiesce asked for while a tool call is in flight
/// waits for the boundary and never interrupts the call. The wait refuses by
/// timeout — which is a first-class answer, not a failure — and nothing about
/// the running turn changes.
#[tokio::test]
async fn a_quiesce_waits_for_the_boundary_and_never_breaks_a_tool_call() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::tool("t1", "gate", json!({})),
        ScriptedProvider::text("done"),
    ]);
    let h = harness(dir.path(), provider.clone());

    let turn = {
        let agent = h.agent.clone();
        tokio::spawn(async move {
            agent
                .run_turn(vec![ContentBlock::text("go")], CancellationToken::new())
                .await
        })
    };
    // The turn is now parked inside the tool call: a boundary is ahead of it
    // and this is not one.
    h.entered.notified().await;
    assert!(h.agent.turn_in_flight());

    assert!(h.agent.request_quiesce("cannataxis"));
    // Refused, not forced: the timeout fires and the session goes on. The
    // turn it was parked in is untouched.
    assert!(
        !quiesce::quiesce_wait(&h.agent, Duration::from_millis(150))
            .await
            .unwrap(),
        "a turn mid-tool-call is never interrupted"
    );
    assert!(!h.agent.quiesced());
    assert!(
        !h.session
            .lock()
            .await
            .branch()
            .iter()
            .any(|r| is_marker(&r.kind)),
        "a refusal journals nothing"
    );

    // Let the call finish; the turn settles, and *that* is the boundary.
    h.release.notify_one();
    let out = turn.await.unwrap().unwrap();
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
    assert!(h.agent.quiesced(), "the boundary the ask waited for arrived");
    assert_eq!(provider.requests.lock().unwrap().len(), 2, "both calls ran in full");

    let s = h.session.lock().await;
    let kinds: Vec<&RecordKind> = s.branch().into_iter().map(|r| &r.kind).collect();
    let settle_at = kinds
        .iter()
        .position(|k| matches!(k, RecordKind::TurnSettled { .. }))
        .expect("the turn settled");
    assert!(
        is_marker(kinds.last().unwrap()),
        "the marker is the branch's last record: {kinds:?}"
    );
    assert!(settle_at < kinds.len() - 1, "the settle precedes the marker");
    // The tool's result is on the branch: nothing was cut short.
    assert!(
        kinds
            .iter()
            .any(|k| matches!(k, RecordKind::ToolResult { content, .. } if content == "through")),
        "{kinds:?}"
    );
}

/// The doorbell's half, without a socket: the ask notes the destination and
/// answers at once, the status reports the boundary, the ring is rung so an
/// idle session's driver wakes to journal the marker, and anything that is
/// not a quiesce op is left to the socket's own answer.
#[tokio::test]
async fn the_doorbell_notes_the_ask_and_reports_the_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let h = harness(dir.path(), ScriptedProvider::new(vec![]));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

    let ask = serde_json::json!({ "op": "quiesce", "destination": "cannataxis" });
    let reply = doorbell::answer(&h.agent, &ask, &tx).expect("the quiesce op is the handler's");
    assert_eq!(reply["ok"], true);
    assert_eq!(reply["destination"], "cannataxis");
    assert_eq!(
        h.agent.quiesce_request().map(|q| q.destination).as_deref(),
        Some("cannataxis")
    );
    assert!(
        rx.try_recv().is_ok(),
        "the ring is what reaches an idle driver, which is the only thing that can journal"
    );

    // A second ask does not move the destination: the quiesce is under way.
    let again = doorbell::answer(
        &h.agent,
        &serde_json::json!({ "op": "quiesce", "destination": "elsewhere" }),
        &tx,
    )
    .unwrap();
    assert_eq!(again["ok"], true);
    assert_eq!(again["already"], true);
    assert_eq!(
        h.agent.quiesce_request().map(|q| q.destination).as_deref(),
        Some("cannataxis")
    );

    let status = doorbell::answer(&h.agent, &quiesce::quiesce_status_request(), &tx).unwrap();
    assert_eq!(status["quiesced"], false);
    assert_eq!(status["busy"], false, "no turn is in flight");
    // The ask answered without waiting for the boundary: it is not settled
    // until somebody journals the marker.
    assert!(!h.agent.quiesced());

    assert!(h.agent.poll_quiesce().await.unwrap());
    let status = doorbell::answer(&h.agent, &quiesce::quiesce_status_request(), &tx).unwrap();
    assert_eq!(status["quiesced"], true);

    // Everything else is the socket's: `ping`, `notify`, and the unknown op
    // that must still be refused rather than swallowed here.
    for req in [
        serde_json::json!({ "op": "ping" }),
        serde_json::json!({ "op": "notify" }),
        serde_json::json!({ "op": "wat" }),
    ] {
        assert!(
            doorbell::answer(&h.agent, &req, &tx).is_none(),
            "not a quiesce op: {req}"
        );
    }
}

/// A backend that owns its own loop settles at its boundary too, and the
/// quiesce outranks a wake: a peer's message that would otherwise buy another
/// turn on the driver does not, because the session is finishing.
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
            let _ = cancel;
            let pending = agent
                .session()
                .lock()
                .await
                .pending_user_message()
                .map(|m| m.text());
            self.seen.lock().unwrap().push(pending);
            let mut s = agent.session().lock().await;
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

#[tokio::test]
async fn a_driver_turns_quiesce_outranks_a_waking_message() {
    let dir = tempfile::tempdir().unwrap();
    let h = harness(dir.path(), ScriptedProvider::new(vec![]));
    let driver = Arc::new(ProbeDriver {
        seen: std::sync::Mutex::new(Vec::new()),
    });
    h.agent.set_backend(Backend::Driver(driver.clone()));
    // A waking message: without the quiesce this buys a second driver pass
    // (see the loop tests), which is exactly the turn that must not happen.
    h.agent.set_inbox(Arc::new(Waiting(std::sync::Mutex::new(vec![
        peer::PeerMessage {
            from: "eidolon-9f2c".into(),
            from_cwd: "/tmp/wt".into(),
            channel: None,
            text: "rebased onto master".into(),
            wake: true,
            external: false,
        },
    ]))));
    assert!(h.agent.request_quiesce("cannataxis"));

    h.agent.continue_turn(CancellationToken::new()).await.unwrap();

    assert_eq!(
        driver.seen.lock().unwrap().len(),
        1,
        "the settled driver turn was the boundary; nothing else ran"
    );
    assert!(h.agent.quiesced());
    let s = h.session.lock().await;
    assert!(
        is_marker(&s.branch().last().unwrap().kind),
        "the marker follows the settle: {:?}",
        s.branch().last().unwrap().kind
    );
    // The waking message was never delivered as a turn of its own, but it is
    // not lost: nothing was drained for it.
    assert!(
        !s.branch()
            .iter()
            .any(|r| matches!(r.kind, RecordKind::PeerMessage { .. }))
    );
}
