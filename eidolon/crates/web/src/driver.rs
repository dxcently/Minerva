//! Turn ownership: the async counterpart of `crates/tui/src/app.rs`'s
//! `driver()`. One [`Handle`] per server owns the one turn slot; routes and
//! wakes (`crate::wake`) go through it and never call the agent's turn methods
//! directly, so `POST /api/cancel` always cancels the turn `turn-state` named.

use std::sync::Arc;

use eidolon_core::agent::{Agent, TurnOutcome};
use eidolon_core::message::ContentBlock;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

pub enum Say {
    /// A new turn when idle, else a follow-up queued behind the running one.
    Send(Vec<ContentBlock>),
    /// Read before the running turn's next call.
    Steer(Vec<ContentBlock>),
}

struct Turn {
    cancel: CancellationToken,
    handle: tokio::task::JoinHandle<anyhow::Result<TurnOutcome>>,
}

/// An empty turn slot, held: see [`Handle::idle`].
pub struct Idle<'a> {
    handle: &'a Handle,
    slot: tokio::sync::MutexGuard<'a, Option<Turn>>,
}

impl Idle<'_> {
    pub fn start_continue(mut self) {
        self.handle.start_continue(&mut self.slot);
    }
}

fn said_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// How a turn task ended, one signal for every ending: `Exhausted` and a
/// failed task put nothing on the bus to wait for. Read by `crate::wake`, not
/// by the page, which settles off the bus.
#[derive(Clone, Debug)]
pub enum Ended {
    Settled,
    Cancelled,
    Exhausted,
    /// The task returned `Err` or panicked.
    Failed(String),
}

/// The part of the swarm roster this door touches. A trait the CLI fills with
/// `eidolon_swarm::Presence`, as `eidolon_core::peer::PeerInbox` is, so this
/// crate does not depend on the swarm.
pub trait Presence: Send + Sync {
    fn set_busy(&self, busy: bool);
    /// Truncated as `crates/tui/src/app.rs` does it.
    fn title_if_empty(&self, text: &str);
    fn deregister(&self);
    fn id(&self) -> String;
}

/// Owns the one turn a session runs at a time; the TUI driver's `turn`
/// variable, reachable from HTTP handlers. Cheap to clone.
#[derive(Clone)]
pub struct Handle {
    agent: Arc<Agent>,
    turn: Arc<Mutex<Option<Turn>>>,
    presence: Option<Arc<dyn Presence>>,
    /// `turn-state`: nothing on the bus says a turn started.
    state: tokio::sync::broadcast::Sender<bool>,
    ended: tokio::sync::broadcast::Sender<Ended>,
    bye: tokio::sync::broadcast::Sender<String>,
}

impl Handle {
    pub fn new(agent: Arc<Agent>, presence: Option<Arc<dyn Presence>>) -> Self {
        let (state, _) = tokio::sync::broadcast::channel(16);
        let (ended, _) = tokio::sync::broadcast::channel(8);
        let (bye, _) = tokio::sync::broadcast::channel(4);
        Handle {
            agent,
            turn: Arc::new(Mutex::new(None)),
            presence,
            state,
            ended,
            bye,
        }
    }

    pub fn subscribe_state(&self) -> tokio::sync::broadcast::Receiver<bool> {
        self.state.subscribe()
    }

    /// No replay: subscribe before starting the turn you wait on.
    pub fn subscribe_ended(&self) -> tokio::sync::broadcast::Receiver<Ended> {
        self.ended.subscribe()
    }

    pub fn subscribe_bye(&self) -> tokio::sync::broadcast::Receiver<String> {
        self.bye.subscribe()
    }

    /// Sent before the door's token is cancelled, so a page reads where the
    /// session went rather than a socket that ended.
    pub fn goodbye(&self, text: &str) {
        let _ = self.bye.send(text.to_string());
    }

    /// The roster first: a peer must never see this session idle while a turn runs.
    fn announce(&self, running: bool) {
        if let Some(p) = &self.presence {
            p.set_busy(running);
        }
        let _ = self.state.send(running);
    }

    fn start_continue(&self, slot: &mut Option<Turn>) {
        let cancel = CancellationToken::new();
        let a = self.agent.clone();
        let t = cancel.clone();
        let handle = tokio::spawn(async move { a.continue_turn(t).await });
        *slot = Some(Turn { cancel, handle });
        self.announce(true);
        self.watch();
    }

    /// Continue a turn a crash left unsettled, as the TUI driver does before
    /// its first `select!`. `true` when one was started, not finished.
    pub async fn finish_unsettled(&self) -> bool {
        let needs = self.agent.session().lock().await.needs_finishing();
        if !needs {
            return false;
        }
        let mut slot = self.turn.lock().await;
        if slot.is_some() {
            return false;
        }
        self.start_continue(&mut slot);
        true
    }

    /// The empty turn slot, held; `None` while a turn runs. While held no say
    /// can start a turn, so what a wake journals lands between turns and its
    /// continuation starts under the same hold. The TUI gets this from its
    /// single driver thread.
    pub async fn idle(&self) -> Option<Idle<'_>> {
        let mut slot = self.turn.lock().await;
        self.reap(&mut slot).await;
        if slot.is_some() {
            return None;
        }
        Some(Idle { handle: self, slot })
    }

    pub async fn running(&self) -> bool {
        self.turn.lock().await.is_some()
    }

    /// Whether the message was queued behind a running turn.
    pub async fn say(&self, say: Say) -> bool {
        let mut slot = self.turn.lock().await;
        self.reap(&mut slot).await;
        match say {
            Say::Steer(blocks) => {
                self.agent.steer(blocks);
                false
            }
            Say::Send(blocks) => {
                if let Some(p) = &self.presence {
                    p.title_if_empty(&said_text(&blocks));
                }
                if slot.is_some() {
                    self.agent.follow_up(blocks);
                    true
                } else {
                    let cancel = CancellationToken::new();
                    let a = self.agent.clone();
                    let t = cancel.clone();
                    let handle = tokio::spawn(async move { a.run_turn(blocks, t).await });
                    *slot = Some(Turn { cancel, handle });
                    self.announce(true);
                    self.watch();
                    false
                }
            }
        }
    }

    /// Whether a turn was running to cancel.
    pub async fn cancel(&self) -> bool {
        let mut slot = self.turn.lock().await;
        self.reap(&mut slot).await;
        match slot.as_ref() {
            Some(t) => {
                t.cancel.cancel();
                true
            }
            None => false,
        }
    }

    /// Announces the end as it happens rather than at the next route call.
    fn watch(&self) {
        let this = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                let mut slot = this.turn.lock().await;
                if slot.is_none() {
                    return;
                }
                if matches!(&*slot, Some(t) if t.handle.is_finished()) {
                    this.finish(&mut slot).await;
                    return;
                }
            }
        });
    }

    /// Nothing here awaits a turn to completion, as the TUI's `select!` does,
    /// so a finished handle must be noticed before the slot is read.
    async fn reap(&self, slot: &mut Option<Turn>) {
        if matches!(slot, Some(t) if t.handle.is_finished()) {
            self.finish(slot).await;
        }
    }

    async fn finish(&self, slot: &mut Option<Turn>) {
        let Some(turn) = slot.take() else { return };
        let ended = match turn.handle.await {
            Ok(Ok(TurnOutcome::Settled { .. })) => Ended::Settled,
            Ok(Ok(TurnOutcome::Cancelled { .. })) => Ended::Cancelled,
            Ok(Ok(TurnOutcome::Exhausted { .. })) => Ended::Exhausted,
            Ok(Err(e)) => Ended::Failed(e.to_string()),
            Err(join_err) => Ended::Failed(format!("turn task panicked: {join_err}")),
        };
        let _ = self.ended.send(ended);
        self.announce(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eidolon_core::agent::AgentConfig;
    use eidolon_core::dispatch::Dispatcher;
    use eidolon_core::event::EventBus;
    use eidolon_core::message::Message;
    use eidolon_core::policy::AllowAll;
    use eidolon_core::provider::{ChatRequest, EventStream, Provider, StreamEvent};
    use eidolon_core::session::{RecordKind, Session};
    use eidolon_core::testing::ScriptedProvider;
    use eidolon_core::tool::ToolRegistry;
    use eidolon_core::user::NoUser;

    fn test_agent(provider: Arc<dyn Provider>) -> (Arc<Agent>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let session = Session::create(&dir.path().join("t.eid"), "m", dir.path(), None).unwrap();
        let session = Arc::new(tokio::sync::Mutex::new(session));
        let bus = EventBus::new(16);
        let dispatcher = Arc::new(Dispatcher::new(
            ToolRegistry::new(),
            Arc::new(AllowAll),
            Arc::new(NoUser),
            bus,
            session,
            dir.path().to_path_buf(),
        ));
        let agent = Agent::new(provider, dispatcher, AgentConfig { model: "m".into(), ..Default::default() });
        (Arc::new(agent), dir)
    }

    /// Starts a response and hangs until cancelled.
    struct Blocking;

    impl Provider for Blocking {
        fn name(&self) -> &str {
            "blocking"
        }
        fn stream<'a>(&'a self, _req: ChatRequest, cancel: CancellationToken) -> EventStream<'a> {
            Box::pin(async_stream::stream! {
                yield Ok(StreamEvent::Start { usage: eidolon_core::message::Usage::default() });
                cancel.cancelled().await;
            })
        }
    }

    struct Failing;

    impl Provider for Failing {
        fn name(&self) -> &str {
            "failing"
        }
        fn stream<'a>(&'a self, _req: ChatRequest, _cancel: CancellationToken) -> EventStream<'a> {
            Box::pin(futures_util::stream::iter(vec![Err(anyhow::anyhow!("boom"))]))
        }
    }

    #[tokio::test]
    async fn a_settled_turn_broadcasts_ended_settled() {
        let (agent, _dir) = test_agent(ScriptedProvider::new(vec![ScriptedProvider::text("hi")]));
        let handle = Handle::new(agent, None);
        let mut ended = handle.subscribe_ended();
        handle.say(Say::Send(vec![ContentBlock::text("hello")])).await;
        let e = ended.recv().await.unwrap();
        assert!(matches!(e, Ended::Settled), "{e:?}");
        assert!(!handle.running().await);
    }

    #[tokio::test]
    async fn a_failed_turn_broadcasts_ended_failed_with_the_message() {
        let (agent, _dir) = test_agent(Arc::new(Failing));
        let handle = Handle::new(agent, None);
        let mut ended = handle.subscribe_ended();
        handle.say(Say::Send(vec![ContentBlock::text("hello")])).await;
        match ended.recv().await.unwrap() {
            Ended::Failed(msg) => assert!(msg.contains("boom"), "{msg}"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_cancelled_turn_broadcasts_ended_cancelled() {
        let (agent, _dir) = test_agent(Arc::new(Blocking));
        let handle = Handle::new(agent, None);
        let mut ended = handle.subscribe_ended();
        handle.say(Say::Send(vec![ContentBlock::text("hello")])).await;
        // Cancel mid-call, not before the call began.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(handle.cancel().await);
        let e = ended.recv().await.unwrap();
        assert!(matches!(e, Ended::Cancelled), "{e:?}");
    }

    #[tokio::test]
    async fn subscribing_late_misses_nothing_it_did_not_ask_for() {
        let (agent, _dir) = test_agent(ScriptedProvider::new(vec![ScriptedProvider::text("hi")]));
        let handle = Handle::new(agent, None);
        assert!(!handle.running().await);
    }

    #[tokio::test]
    async fn finish_unsettled_reports_false_and_starts_nothing_for_a_settled_session() {
        let (agent, _dir) = test_agent(ScriptedProvider::new(vec![ScriptedProvider::text("hi")]));
        let handle = Handle::new(agent, None);
        assert!(!handle.finish_unsettled().await, "a settled session has nothing to finish");
        assert!(!handle.running().await);
    }

    #[tokio::test]
    async fn finish_unsettled_reports_true_and_registers_a_running_turn_for_an_orphaned_message() {
        // A crash between journaling the message and calling the model.
        let dir = tempfile::tempdir().unwrap();
        let mut session = Session::create(&dir.path().join("t.eid"), "m", dir.path(), None).unwrap();
        session.append(RecordKind::UserMessage(Message::user_text("orphaned"))).unwrap();
        assert!(session.needs_finishing(), "the fixture must need finishing, or this test proves nothing");
        let session = Arc::new(tokio::sync::Mutex::new(session));
        let bus = EventBus::new(16);
        let dispatcher = Arc::new(Dispatcher::new(
            ToolRegistry::new(),
            Arc::new(AllowAll),
            Arc::new(NoUser),
            bus,
            session,
            dir.path().to_path_buf(),
        ));
        let agent = Arc::new(Agent::new(
            ScriptedProvider::new(vec![ScriptedProvider::text("finished")]),
            dispatcher,
            AgentConfig { model: "m".into(), ..Default::default() },
        ));
        let handle = Handle::new(agent, None);
        let mut ended = handle.subscribe_ended();
        assert!(handle.finish_unsettled().await, "a session needing finishing must report true");
        // Running as soon as it returns, before the continuation has progressed.
        assert!(handle.running().await);
        let e = ended.recv().await.unwrap();
        assert!(matches!(e, Ended::Settled), "{e:?}");
    }
}
