//! What wakes an idle door: a peer's ring, a resolved park, and the end of a
//! turn, as `crates/tui/src/app.rs`'s driver wakes, with the same core calls.
//!
//! Every wake acts only under [`Handle::idle`]. With a turn running it does
//! nothing: journaling then would land between a `tool_use` and its results,
//! and the turn takes mail and fires at its own safe points.
//!
//! A quiesce is asked first, so a settled one closes the door instead of
//! buying the turn a message would have.

use std::sync::Arc;

use eidolon_core::agent::Agent;
use tokio::sync::{broadcast, mpsc};
use tokio_util::sync::CancellationToken;

use crate::driver::{Ended, Handle, Idle, Presence};

/// Runs until `close` is cancelled or the handle is gone.
pub async fn watch(
    agent: Arc<Agent>,
    driver: Handle,
    presence: Option<Arc<dyn Presence>>,
    mut doorbell: Option<mpsc::UnboundedReceiver<()>>,
    mut ended: broadcast::Receiver<Ended>,
    close: CancellationToken,
) {
    let mut parks = agent.parks().events();
    loop {
        let rang = async {
            match &mut doorbell {
                Some(rx) => rx.recv().await,
                None => std::future::pending().await,
            }
        };
        let park_ring = async {
            let _ = parks.recv().await;
        };
        tokio::select! {
            _ = close.cancelled() => return,
            // Mail or a quiesce request; the ring does not say which.
            _ = rang => {
                let Some(idle) = driver.idle().await else { continue };
                if close_if_quiesced(&agent, &driver, &presence, &close).await {
                    return;
                }
                collect(&agent, idle).await;
            }
            // Also rings on a sample tick, which `take_fires` finds empty.
            _ = park_ring => {
                let Some(idle) = driver.idle().await else { continue };
                if close_if_quiesced(&agent, &driver, &presence, &close).await {
                    return;
                }
                if !agent.take_fires().await.is_empty() {
                    idle.start_continue();
                }
            }
            // The boundary a quiesce waits for, and where mail that landed
            // just before the settle is collected.
            res = ended.recv() => {
                match res {
                    Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => return,
                }
                let Some(idle) = driver.idle().await else { continue };
                if close_if_quiesced(&agent, &driver, &presence, &close).await {
                    return;
                }
                collect(&agent, idle).await;
            }
        }
    }
}

/// `app.rs`'s `collect`. Mail the park does not admit is still delivered.
async fn collect(agent: &Arc<Agent>, idle: Idle<'_>) {
    let messages = agent.drain_peers().await;
    if messages.iter().any(|m| agent.parks().admits(m.wake)) {
        idle.start_continue();
    }
}

/// The steps `crates/cli/src/main.rs` takes, in its order, then the door's
/// token is cancelled, so it closes the way SIGTERM closes it.
async fn close_if_quiesced(
    agent: &Arc<Agent>,
    driver: &Handle,
    presence: &Option<Arc<dyn Presence>>,
    close: &CancellationToken,
) -> bool {
    match agent.poll_quiesce().await {
        Ok(true) => {}
        Ok(false) => return false,
        Err(e) => {
            // Unsettled without a marker: go on, and say so, as the headless drivers do.
            eprintln!("[quiesce] could not journal the marker: {e:#}");
            return false;
        }
    }
    let destination = agent.quiesce_request().map(|q| q.destination).unwrap_or_default();
    let id = presence.as_ref().map(|p| p.id());
    driver.goodbye(&eidolon_core::quiesce::goodbye(&destination, id.as_deref()));
    if let Some(p) = presence {
        p.deregister();
    }
    close.cancel();
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;

    use eidolon_core::agent::AgentConfig;
    use eidolon_core::dispatch::Dispatcher;
    use eidolon_core::event::{Event, EventBus};
    use eidolon_core::message::{ContentBlock, StopReason, Usage};
    use eidolon_core::peer::{PeerInbox, PeerMessage};
    use eidolon_core::policy::AllowAll;
    use eidolon_core::provider::{ChatRequest, EventStream, Provider, StreamEvent};
    use eidolon_core::session::Session;
    use eidolon_core::testing::ScriptedProvider;
    use eidolon_core::tool::ToolRegistry;
    use eidolon_core::user::NoUser;
    use eidolon_core::wait::{Answer, Arm, Condition, Parks, Spec, Term, Vantage, Wake};

    use crate::driver::Say;

    /// Keeps the real roster's rule that only an empty title is set.
    #[derive(Default)]
    struct Roster {
        busy: Mutex<Vec<bool>>,
        titles: Mutex<Vec<String>>,
        gone: AtomicBool,
    }

    impl Presence for Roster {
        fn set_busy(&self, busy: bool) {
            self.busy.lock().unwrap().push(busy);
        }
        fn title_if_empty(&self, text: &str) {
            let mut titles = self.titles.lock().unwrap();
            if titles.is_empty() {
                titles.push(text.chars().take(72).collect());
            }
        }
        fn deregister(&self) {
            self.gone.store(true, Ordering::SeqCst);
        }
        fn id(&self) -> String {
            "eidolon-test".to_string()
        }
    }

    struct Watching;

    #[async_trait::async_trait]
    impl Vantage for Watching {
        fn cadence(&self, _condition: &Condition) -> Duration {
            Duration::from_millis(20)
        }
        async fn ask(&self, term: &Term) -> Answer {
            match term {
                Term::FileExists(path) if path.exists() => Answer::Yes,
                Term::FileExists(_) => Answer::No,
                other => Answer::Unanswerable(format!("this fake cannot answer {other:?}")),
            }
        }
    }

    /// Records what was taken, not how often: a turn drains its boundary empty
    /// on the way in, so a drain count cannot show mail was left alone.
    #[derive(Default)]
    struct Mail {
        waiting: Mutex<Vec<PeerMessage>>,
        took: Mutex<Vec<String>>,
    }

    impl Mail {
        fn empty() -> Arc<Self> {
            Arc::new(Mail::default())
        }
        fn with(messages: Vec<PeerMessage>) -> Arc<Self> {
            Arc::new(Mail { waiting: Mutex::new(messages), ..Default::default() })
        }
        fn deliver(&self, message: PeerMessage) {
            self.waiting.lock().unwrap().push(message);
        }
        fn took(&self) -> Vec<String> {
            self.took.lock().unwrap().clone()
        }
        fn handed(&self) -> usize {
            self.took.lock().unwrap().len()
        }
    }

    impl PeerInbox for Mail {
        fn drain(&self) -> Vec<PeerMessage> {
            let taken = std::mem::take(&mut *self.waiting.lock().unwrap());
            self.took.lock().unwrap().extend(taken.iter().map(|m| m.text.clone()));
            taken
        }
        fn drain_waking(&self) -> Vec<PeerMessage> {
            let mut waiting = self.waiting.lock().unwrap();
            let (waking, staying): (Vec<_>, Vec<_>) = waiting.drain(..).partition(|m| m.wake);
            *waiting = staying;
            self.took.lock().unwrap().extend(waking.iter().map(|m| m.text.clone()));
            waking
        }
    }

    fn note() -> PeerMessage {
        PeerMessage {
            from: "eidolon-9f2c".into(),
            from_cwd: "/tmp/tree".into(),
            channel: None,
            text: "out of driver.rs".into(),
            wake: true,
            external: false,
        }
    }

    /// The core takes waking mail at a mid-turn safe point itself, so only
    /// quiet mail shows what the door does with mail that waits.
    fn quiet() -> PeerMessage {
        PeerMessage { wake: false, ..note() }
    }

    /// Holds its first call until `open`, so "busy" is not a race.
    struct Gated {
        calls: AtomicUsize,
        open: tokio::sync::Semaphore,
    }

    impl Default for Gated {
        fn default() -> Self {
            Gated { calls: AtomicUsize::new(0), open: tokio::sync::Semaphore::new(0) }
        }
    }

    impl Gated {
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
        fn open(&self) {
            self.open.add_permits(1);
        }
    }

    impl Provider for Gated {
        fn name(&self) -> &str {
            "gated"
        }
        fn stream<'a>(&'a self, _req: ChatRequest, _cancel: CancellationToken) -> EventStream<'a> {
            let first = self.calls.fetch_add(1, Ordering::SeqCst) == 0;
            Box::pin(async_stream::stream! {
                yield Ok(StreamEvent::Start { usage: Usage::default() });
                if first {
                    let _ = self.open.acquire().await;
                }
                yield Ok(StreamEvent::TextDelta("ok".into()));
                yield Ok(StreamEvent::BlockStop);
                yield Ok(StreamEvent::Stop { stop_reason: StopReason::EndTurn, usage: Usage::default() });
            })
        }
    }

    struct Door {
        agent: Arc<Agent>,
        parks: Arc<Parks>,
        driver: Handle,
        roster: Arc<Roster>,
        ring: mpsc::UnboundedSender<()>,
        doorbell: Option<mpsc::UnboundedReceiver<()>>,
        _dir: tempfile::TempDir,
    }

    async fn door(provider: Arc<dyn Provider>) -> Door {
        let dir = tempfile::tempdir().unwrap();
        let session = Session::create(&dir.path().join("t.eid"), "m", dir.path(), None).unwrap();
        let session = Arc::new(tokio::sync::Mutex::new(session));
        let dispatcher = Arc::new(Dispatcher::new(
            ToolRegistry::new(),
            Arc::new(AllowAll),
            Arc::new(NoUser),
            EventBus::new(64),
            session,
            dir.path().to_path_buf(),
        ));
        let agent = Agent::new(provider, dispatcher, AgentConfig { model: "m".into(), ..Default::default() });
        // Before anything reads `agent.parks()`: it is a `OnceLock`.
        let parks = Arc::new(Parks::new());
        parks.set_vantage(Arc::new(Watching));
        agent.set_parks(parks.clone());
        let agent = Arc::new(agent);
        let roster = Arc::new(Roster::default());
        let (ring, doorbell) = mpsc::unbounded_channel();
        Door {
            driver: Handle::new(agent.clone(), Some(roster.clone() as Arc<dyn Presence>)),
            agent,
            parks,
            roster,
            ring,
            doorbell: Some(doorbell),
            _dir: dir,
        }
    }

    impl Door {
        /// As `crate::run` starts it.
        fn start(&mut self) -> CancellationToken {
            let close = CancellationToken::new();
            let ended = self.driver.subscribe_ended();
            tokio::spawn(watch(
                self.agent.clone(),
                self.driver.clone(),
                Some(self.roster.clone() as Arc<dyn Presence>),
                self.doorbell.take(),
                ended,
                close.clone(),
            ));
            close
        }
    }

    async fn event(bus: &mut broadcast::Receiver<Event>, want: &str, ok: impl Fn(&Event) -> bool) -> Event {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let left = deadline - tokio::time::Instant::now();
            match tokio::time::timeout(left, bus.recv()).await {
                Ok(Ok(e)) if ok(&e) => return e,
                Ok(Ok(_)) => continue,
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(broadcast::error::RecvError::Closed)) => panic!("the bus closed before {want} arrived"),
                Err(_) => panic!("nothing matching {want} within 5s"),
            }
        }
    }

    /// Roster writes land on their own task, after the bus has spoken.
    async fn until(what: &str, ok: impl Fn() -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !ok() {
            assert!(tokio::time::Instant::now() < deadline, "timed out waiting for {what}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn a_resolved_park_wakes_an_idle_door() {
        let provider = ScriptedProvider::new(vec![ScriptedProvider::text("woken")]);
        let mut door = door(provider).await;
        let mut bus = door.agent.bus().subscribe();
        let _close = door.start();

        // False when armed: one that already held would answer `Arm::Already` and never fire.
        let watched = door._dir.path().join("ready");
        let condition = Condition::parse(&serde_json::json!({"file_exists": watched.display().to_string()})).unwrap();
        let armed = door
            .parks
            .arm(Spec {
                call_id: "call-1".into(),
                condition,
                wake: Wake::Waking,
                timeout: Duration::from_secs(30),
                epoch: door.parks.begin_turn(),
            })
            .await;
        assert!(matches!(armed, Arm::Armed(_)), "{armed:?}");
        std::fs::write(&watched, "done").unwrap();

        event(&mut bus, "a fire", |e| matches!(e, Event::TriggerFired { .. })).await;
        event(&mut bus, "the woke turn", |e| matches!(e, Event::TurnSettled { .. })).await;
        until("the roster busy and then idle", || {
            door.roster.busy.lock().unwrap().as_slice() == [true, false]
        })
        .await;
        assert!(door.roster.titles.lock().unwrap().is_empty(), "a park says nothing to name the session after");
    }

    #[tokio::test]
    async fn a_ring_while_a_turn_is_running_starts_no_second_turn() {
        let provider = Arc::new(Gated::default());
        let mut door = door(provider.clone()).await;
        let inbox = Mail::empty();
        door.agent.set_inbox(inbox.clone());
        let mut bus = door.agent.bus().subscribe();
        let close = door.start();

        assert!(!door.driver.say(Say::Send(vec![ContentBlock::text("hello door")])).await);
        until("the roster to show the session busy", || door.roster.busy.lock().unwrap().as_slice() == [true]).await;
        assert_eq!(
            door.roster.titles.lock().unwrap().as_slice(),
            ["hello door"],
            "the first thing said names the session, exactly as the TUI's roster does"
        );
        // Past the turn's first safe point; the next waits on the gate.
        until("the running turn to reach its provider", || provider.calls() == 1).await;

        inbox.deliver(quiet());
        door.ring.send(()).unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(provider.calls(), 1, "a ring must not start a second turn");
        assert!(inbox.took().is_empty(), "a turn in flight takes its own mail at its safe points");

        provider.open();
        event(&mut bus, "the running turn's end", |e| matches!(e, Event::TurnSettled { .. })).await;
        until("the waiting message to be collected", || !inbox.took().is_empty()).await;
        event(&mut bus, "collected mail", |e| matches!(e, Event::PeerMessage { .. })).await;
        assert_eq!(provider.calls(), 1, "mail that did not ask to wake starts no turn");
        until("the roster idle again", || door.roster.busy.lock().unwrap().as_slice() == [true, false]).await;
        close.cancel();
    }

    #[tokio::test]
    async fn an_admitted_peer_message_wakes_an_idle_door() {
        let provider = ScriptedProvider::new(vec![ScriptedProvider::text("on it")]);
        let mut door = door(provider).await;
        let inbox = Mail::with(vec![note()]);
        door.agent.set_inbox(inbox.clone());
        let mut bus = door.agent.bus().subscribe();
        let _close = door.start();

        door.ring.send(()).unwrap();
        event(&mut bus, "a delivered note", |e| matches!(e, Event::PeerMessage { .. })).await;
        event(&mut bus, "the woke turn", |e| matches!(e, Event::TurnSettled { .. })).await;
        assert_eq!(inbox.handed(), 1);
        until("the roster idle again", || door.roster.busy.lock().unwrap().as_slice() == [true, false]).await;
        assert!(door.roster.titles.lock().unwrap().is_empty(), "a peer's note is not the operator's words");
    }
}

