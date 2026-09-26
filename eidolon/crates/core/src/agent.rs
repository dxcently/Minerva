//! The agent loop: stream a model response, journal it, dispatch its tool
//! calls, feed the results back, repeat until a response has no tool calls
//! — then publish `TurnSettled`.
//!
//! The loop is deliberately small. Everything with a policy dimension is in
//! [`crate::dispatch`], everything with a wire dimension is behind
//! [`crate::provider::Provider`], and everything with a durability dimension
//! is a [`crate::session::RecordKind`]. What is left here is sequencing and
//! the assembly of streamed deltas into a complete [`Message`].
//!
//! ## Cancellation
//!
//! One `CancellationToken` per turn. Cancelling it aborts the in-flight
//! stream (the provider must honour it) and any running tool, journals a
//! `Cancelled` record, and returns `TurnOutcome::Cancelled`. A partially
//! streamed assistant message is journaled as far as it got, so nothing the
//! user saw on screen vanishes on resume — but its tool uses get synthetic
//! error results, never real execution.
//!
//! ## Speaking to a turn in flight
//!
//! Two queues on the agent, and they are the core's rather than a
//! consumer's because an external driver (Melete, driving this the way it
//! drives pi's RPC mode) has to reach them without a TUI in the way:
//!
//! - [`Agent::steer`] is pi's `steer`: a message for the turn that is
//!   running, delivered as a user message **before the next model call**
//!   and after the tool calls the current reply asked for have run — so
//!   the model reads the redirect beside their results, and never mid-call.
//!   A turn does not settle while one is undelivered.
//! - [`Agent::follow_up`] is pi's `follow_up`: a message for *after* the
//!   turn, run as a turn of its own by the same [`Agent::run_turn`] call
//!   once the first has settled — so, as with pi's `agent_settled`, the
//!   call returning means nothing queued is still going to run.
//!
//! Both journal an ordinary `UserMessage`, because both are the operator
//! speaking; what differs is only *when*. Neither needed a record kind.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use futures_util::future::BoxFuture;

use crate::attribution::Attribution;
use crate::dispatch::Dispatcher;
use crate::event::{Event, EventBus};
use crate::message::{ContentBlock, ImageSource, Message, Role, StopReason, Usage};
use crate::provider::{
    CacheOptions, CacheRetention, ChatRequest, Provider, ThinkingConfig, ToolChoice,
};
use crate::quiesce::Quiesce;
use crate::wait::Parks;
use crate::session::{RecordId, RecordKind, Session};
use crate::tool::{CallOrigin, ToolCall};
use crate::usage::{CallTiming, TurnTiming, UsageExt};
use harnox::llm::StreamSink;

#[derive(Clone, Debug)]
pub struct AgentConfig {
    /// The id the backend is told to use (`openai/ministral-3:14b`).
    pub model: String,
    /// What the session journals and shows (`fau:openai/ministral-3:14b`).
    /// Empty means "same as `model`".
    pub model_key: String,
    pub system: Option<String>,
    pub max_tokens: u32,
    pub thinking: Option<ThinkingConfig>,
    /// Upper bound on model calls per turn. A runaway tool loop stops here
    /// with `TurnOutcome::Exhausted` rather than spending forever — and a
    /// turn that gets close is *told* it is close (see
    /// [`crate::session::RecordKind::TurnBudget`]), so the common case is a
    /// model that wraps up and settles rather than one that is cut off.
    ///
    /// The default counts *model calls*, not tool calls, and the two
    /// diverge by model: a model that batches five reads into one reply
    /// spends one iteration on them, while one that makes a single call
    /// per reply — measured on this machine, glm at a mean of 1.12 calls
    /// per message — spends one iteration per tool. The default is set
    /// for the second kind, which is also the kind that needs it: the
    /// p90 turn here was 49 calls and p99 was 92, and every turn that
    /// ever hit the old default of 64 was cut off mid-work rather than
    /// looping. Configurable per machine as `max_iterations`.
    pub max_iterations: u32,
    /// How long the endpoint should hold what a turn writes to its prompt
    /// cache. See [`CacheRetention`]; the default is the endpoint's own
    /// short entry, and `Long` is the one worth declaring for a session
    /// that is read and thought about between turns.
    pub cache_retention: CacheRetention,
    /// Which project instructions file (`AGENTS.md`, `CLAUDE.md`) the
    /// working directory's own instructions are read from, if from
    /// anywhere. See [`crate::project`]; `Auto` is the default and a
    /// directory holding neither file costs nothing.
    pub project_instructions: crate::project::ProjectInstructions,
}

impl Default for AgentConfig {
    fn default() -> Self {
        AgentConfig {
            model: String::new(),
            model_key: String::new(),
            system: None,
            max_tokens: 64_000,
            thinking: None,
            max_iterations: 128,
            cache_retention: CacheRetention::default(),
            project_instructions: crate::project::ProjectInstructions::default(),
        }
    }
}

/// The cache key for a session: the name of its log.
///
/// An endpoint that shards its prompt cache routes on this, so a request
/// lands on the machine holding the entry its predecessor wrote. It has to
/// be stable for exactly as long as the prefix is, and the session log's
/// name is that by construction — one log per conversation, and a resumed
/// session, having adopted the log, adopts the key with it. Read live for
/// the same reason: `adopt_session` swaps the log underneath, and a key
/// cached at construction would point a new conversation at an old shard.
///
/// Clamped to 64 characters, which is the shortest limit any endpoint
/// documents for the field.
fn cache_key(path: &std::path::Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    Some(stem.chars().take(64).collect())
}

/// A backend that runs the whole turn itself — its own model loop, its own
/// tools — and journals what it observes. The Claude CLI is one. The
/// harness keeps policy authority through the backend's pre-tool hook
/// calling [`Dispatcher::adjudicate`]; the driver is responsible for
/// journaling assistant messages, tool results and `TurnSettled` exactly
/// as the provider loop would, so consumers cannot tell the two apart.
pub trait TurnDriver: Send + Sync {
    fn name(&self) -> &str;
    /// Run the pending user message to a settled turn. Return
    /// `TurnOutcome::Cancelled` when the token fires; the agent journals it.
    fn run_turn<'a>(
        &'a self,
        agent: &'a Agent,
        cancel: CancellationToken,
    ) -> BoxFuture<'a, anyhow::Result<TurnOutcome>>;
}

/// What answers a turn: a model behind the harness's own loop, or a driver
/// that owns the loop.
#[derive(Clone)]
pub enum Backend {
    Provider(Arc<dyn Provider>),
    Driver(Arc<dyn TurnDriver>),
}

/// One block of the prompt the next turn would carry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextBlock {
    /// A prompt source or replay role, depending on the report collection.
    pub kind: &'static str,
    /// One line saying what it is — the opening of the prose, or what the
    /// message is made of when it is not prose.
    pub label: String,
    /// Its size **in characters**, and deliberately not in tokens.
    ///
    /// The harness has no tokeniser. An estimate here would sit beside
    /// [`ContextReport::tokens`], which is a real figure the wire
    /// reported, and the operator would have no way to tell the measured
    /// number from the invented one. Characters are approximate and say
    /// so; this is `usage::context_line` drawing a dash rather than a
    /// zero, one level up.
    pub chars: usize,
}

/// One tool call retained in replay, not its lifetime spend or invocation telemetry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextTool {
    pub id: String,
    pub name: String,
    /// Tool name plus serialized arguments, using the report's character convention.
    pub input_chars: usize,
    pub result_chars: usize,
    pub is_error: bool,
}

/// What the next turn's prompt would carry, block by block: the report
/// behind `:context`.
///
/// Built from the **same two calls the turn makes** —
/// [`Agent::system_prompt`] and [`crate::session::Session::messages`] — and
/// not from a second assembly that could disagree with them. It runs no
/// model, because an inspector that costs a turn is one nobody opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextReport {
    pub blocks: Vec<ContextBlock>,
    /// Disjoint source totals; their sum equals `chars`.
    pub sources: Vec<ContextBlock>,
    /// A drill-down into the tool input/result sources, not additional characters.
    pub tools: Vec<ContextTool>,
    /// Every block's characters, summed.
    pub chars: usize,
    /// How much of the system block is the tool registry's guidance rather
    /// than the operator's own prompt. Read from the registry, never
    /// recomposed — the system block above is what actually goes out.
    pub guidance_chars: usize,
    /// The last measured context size, from `RecordKind::ContextSize`.
    /// `None` is *unknown*, which is what a log written before that record
    /// existed honestly has.
    pub tokens: Option<u64>,
    /// The compaction in force and how many messages it replaced, if the
    /// branch has one. The history below it is on disk and out of view.
    pub compacted: Option<u32>,
}

/// What [`Agent::fresh_eyes`] found to do.
///
/// Three answers rather than a bool because "nothing happened" has two
/// very different reasons here, and a control that reported success at
/// doing nothing would be worse than no control: on a provider backend
/// there is no session to drop and never was, while on a driver backend
/// there simply is not one *yet*.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FreshEyes {
    /// The mark was dropped; the next turn replays the branch.
    Dropped,
    /// A driver backend that holds no session for this branch yet.
    NothingHeld,
    /// A backend that keeps no conversation of its own — every turn is
    /// already built from the branch, so there is nothing to shed.
    NoSessionKept,
}

impl std::fmt::Display for FreshEyes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FreshEyes::Dropped => write!(
                f,
                "dropped the backend's session; the next turn replays the branch"
            ),
            FreshEyes::NothingHeld => write!(
                f,
                "nothing held: this backend has no session for this branch yet"
            ),
            FreshEyes::NoSessionKept => write!(
                f,
                "this backend keeps no session of its own; every turn is already built from the branch"
            ),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnOutcome {
    /// A response arrived with no tool calls pending.
    ///
    /// `usage` is the turn's total over every model call it made, and
    /// `calls` is how many that was — the figure a run's digest reports as
    /// its turn count, beside the cost `usage` prices to. Neither is on
    /// the `TurnSettled` record: the count is recoverable from the branch
    /// (one `AssistantMessage` per call), and a journaled struct grows no
    /// more freely than a journaled enum does.
    Settled {
        stop_reason: StopReason,
        usage: Usage,
        calls: u32,
    },
    /// The turn was interrupted. `usage` is what it had already spent
    /// over the `calls` model calls it managed before the cancel — which
    /// is real money and used to be dropped here: the loop summed it and
    /// then returned a unit variant, so a driver that stopped a turn was
    /// told nothing about what stopping it had cost. It is journaled as
    /// well ([`RecordKind::TurnSpend`]), so a resumed session prices the
    /// same turn the same way.
    Cancelled { usage: Usage, calls: u32 },
    /// The turn hit `max_iterations`. Carries what it spent for the same
    /// reason `Cancelled` does, and more pressingly: a turn that
    /// exhausted its iteration limit is by definition one that made a
    /// great many calls.
    ///
    /// The spend is journaled ([`RecordKind::TurnSpend`]) and **not**
    /// published: there is no event carrying an exhausted turn's usage, so
    /// a consumer counting live learns it on resume. The **context** is
    /// both journaled and published ([`Event::ContextSize`]) — the gauge
    /// has nowhere else to learn it, and a limit that fired is one more
    /// way for a turn's number to go stale.
    Exhausted { usage: Usage, calls: u32 },
}

/// Did this turn cost anything the harness actually measured?
///
/// The question a [`RecordKind::TurnSpend`] is worth writing for. All
/// four counters, because a turn that read its whole context from cache
/// and was cancelled before a token came back spent real money on
/// `cache_read_input_tokens` alone.
fn spent(u: Usage) -> bool {
    u.input_tokens > 0
        || u.output_tokens > 0
        || u.cache_read_input_tokens > 0
        || u.cache_creation_input_tokens > 0
}

pub struct Agent {
    /// Swappable: `:model` may move a session between providers, or from
    /// a provider to the Claude CLI. Read once per turn.
    backend: std::sync::RwLock<Backend>,
    dispatcher: Arc<Dispatcher>,
    session: Arc<Mutex<Session>>,
    bus: EventBus,
    /// Behind a lock so a consumer can share one `Arc<Agent>` between a
    /// running turn and a command that switches models.
    config: std::sync::RwLock<AgentConfig>,
    /// Messages from other harness sessions, when the consumer registered
    /// this one in a swarm. `None` is the ordinary single-session case and
    /// costs nothing.
    inbox: std::sync::RwLock<Option<Arc<dyn crate::peer::PeerInbox>>>,
    /// Where a pinned persona's note is read from, when the consumer
    /// configured a vault. `None` is the ordinary case — no vault, no
    /// persona, and not one fetch attempted — exactly as `inbox` is for a
    /// session in no swarm.
    personas: std::sync::RwLock<Option<Arc<dyn crate::persona::PersonaSource>>>,
    /// How this session's outbound vault calls are labelled in the vault's
    /// audit log — see [`crate::attribution`]. The consumer owns it, because
    /// the label's session id and working directory are the consumer's to
    /// know; what the agent adds is the two parts that move with the
    /// conversation, the persona it is wearing and the model it runs. `None`
    /// when the consumer configured no attribution, which is every session
    /// with no vault to write to and costs nothing.
    attribution: std::sync::RwLock<Option<Arc<Attribution>>>,
    /// What has been said to the turn in flight and not yet delivered,
    /// as the content it was said with — see [`Agent::steer`].
    steers: std::sync::Mutex<Vec<Vec<ContentBlock>>>,
    /// What is waiting for the turn to settle — see [`Agent::follow_up`].
    follow_ups: std::sync::Mutex<VecDeque<Vec<ContentBlock>>>,
    /// The inline command channel, when one is armed — see
    /// [`Agent::set_command_channel`]. `None` on a session whose tools are
    /// the whole of how the model reaches anything.
    commands: std::sync::RwLock<Option<Arc<dyn crate::commands::CommandChannel>>>,
    /// The quiesce this session has been asked for, if any — see
    /// [`crate::quiesce`]. `None` is the ordinary case and costs nothing.
    /// Not a cancel: the flag rides a boundary the loop already keeps.
    quiesce: std::sync::Mutex<Option<Quiesce>>,
    /// How many turns are in flight right now. Depth rather than a flag
    /// because the entry points nest — `run_turn` calls `continue_turn` —
    /// and the outermost one is what has to cover the whole turn, including
    /// the records `run_turn` writes before the loop starts. [`Agent::
    /// poll_quiesce`] reads it to tell "the boundary is now" from "the
    /// boundary is when this turn settles", which is the whole of the
    /// never-mid-turn rule.
    turning: AtomicUsize,
    /// The parks this session can arm — see [`crate::wait`]. Handed over
    /// once by the consumer, before the first turn, because the `wait_for`
    /// tool is built (and registered into the dispatcher) before this agent
    /// exists; a cycle is the only alternative. Read by the loop, which ends
    /// the turn a park was armed in, and by the driver that wakes to a fire.
    parks: OnceLock<Arc<Parks>>,
}

/// Counts a turn in flight for as long as the guard lives, whatever way the
/// turn returns — so the count [`Agent::turn_in_flight`] reads cannot be left
/// raised by an early return or an `Err`.
struct InFlight<'a>(&'a AtomicUsize);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Agent {
    pub fn new(
        provider: Arc<dyn Provider>,
        dispatcher: Arc<Dispatcher>,
        config: AgentConfig,
    ) -> Self {
        Self::with_backend(Backend::Provider(provider), dispatcher, config)
    }

    pub fn with_backend(
        backend: Backend,
        dispatcher: Arc<Dispatcher>,
        config: AgentConfig,
    ) -> Self {
        let session = dispatcher.session().clone();
        let bus = dispatcher.bus().clone();
        Agent {
            backend: std::sync::RwLock::new(backend),
            dispatcher,
            session,
            bus,
            config: std::sync::RwLock::new(config),
            inbox: std::sync::RwLock::new(None),
            personas: std::sync::RwLock::new(None),
            attribution: std::sync::RwLock::new(None),
            steers: std::sync::Mutex::new(Vec::new()),
            follow_ups: std::sync::Mutex::new(VecDeque::new()),
            commands: std::sync::RwLock::new(None),
            quiesce: std::sync::Mutex::new(None),
            turning: AtomicUsize::new(0),
            parks: OnceLock::new(),
        }
    }

    /// Say something to the turn in flight.
    ///
    /// Delivered as a user message before the next model call — after the
    /// tool calls the current reply asked for have run, so the model reads
    /// the redirect beside their results — and never inside a call. A turn
    /// does not settle while one is undelivered: a reply that arrives with
    /// no tool calls while a steer is waiting is followed by one more call
    /// carrying it, because a message the model only reads once the
    /// operator speaks again was not a steer. With no turn in flight it
    /// waits for the next one and rides its first request.
    ///
    /// The content is the message's own, not just its words: a picture
    /// attached to a message sent mid-turn steers with it, and would have
    /// been dropped between the keystroke and the delivery by a steer of
    /// plain strings. The TUI sends every message typed during a turn this
    /// way, which is why the queue had to carry blocks before the steer
    /// did.
    ///
    /// On a backend that owns its own loop (the Claude CLI) there is no
    /// call to ride, so a steer that arrives mid-turn becomes the next turn
    /// on that backend, run at once by the same `run_turn` call — the
    /// nearest thing the driver can offer, and still "before the operator
    /// speaks again".
    ///
    /// pi's `steer`; Melete's `/steer`.
    pub fn steer(&self, input: Vec<ContentBlock>) {
        self.steers.lock().unwrap().push(input);
    }

    /// Queue a message for after the turn settles.
    ///
    /// [`Agent::run_turn`] runs it as a further turn of its own before it
    /// returns, so a call that comes back has run everything that was
    /// queued for it — pi's `follow_up`, and pi's `agent_settled`. A turn
    /// that ends any other way (cancelled, errored, exhausted) leaves the
    /// queue as it is for the consumer to decide about; see
    /// [`Agent::take_follow_up`].
    ///
    /// The explicit after-the-settle path, for a consumer that wants that
    /// shape. The TUI does not use it for typed messages any more — a
    /// message sent while a turn runs steers it ([`Agent::steer`]) — so in
    /// this tree the queue is exercised by tests and by whatever consumer
    /// reaches for it next.
    pub fn follow_up(&self, input: Vec<ContentBlock>) {
        let waiting = {
            let mut q = self.follow_ups.lock().unwrap();
            q.push_back(input);
            q.len()
        };
        self.bus.publish(Event::Queued { waiting });
    }

    /// How many follow-ups are waiting.
    pub fn queued(&self) -> usize {
        self.follow_ups.lock().unwrap().len()
    }

    /// Take the next follow-up without running it.
    ///
    /// For a consumer whose turn ended without settling and which wants
    /// to carry on anyway — one that cancelled a turn starts the next
    /// queued message as a turn of its own rather than dropping it.
    pub fn take_follow_up(&self) -> Option<Vec<ContentBlock>> {
        let (next, waiting) = {
            let mut q = self.follow_ups.lock().unwrap();
            let next = q.pop_front();
            (next, q.len())
        };
        if next.is_some() {
            self.bus.publish(Event::Queued { waiting });
        }
        next
    }

    fn has_steers(&self) -> bool {
        !self.steers.lock().unwrap().is_empty()
    }

    /// Ask this session to finish at its next turn boundary — see
    /// [`crate::quiesce`]. Returns `false` when a quiesce is already under
    /// way, in which case the first destination stands.
    ///
    /// **Not a cancel.** Nothing in flight is interrupted and no turn is
    /// shortened: a turn that is running settles exactly as it would have,
    /// and the marker and the settled flag appear at that boundary. An idle
    /// session has no boundary to wait for, and [`Agent::poll_quiesce`]
    /// settles it at once.
    pub fn request_quiesce(&self, destination: &str) -> bool {
        let mut slot = self.quiesce.lock().unwrap();
        if slot.is_some() {
            return false;
        }
        *slot = Some(Quiesce::new(destination));
        true
    }

    /// The quiesce this session has been asked for, if any. `None` is the
    /// ordinary case: nothing was ever asked of this session, or nobody
    /// registered it among peers to be asked.
    pub fn quiesce_request(&self) -> Option<Quiesce> {
        self.quiesce.lock().unwrap().clone()
    }

    /// Has the asked-for quiesce reached its boundary? True means the marker
    /// is on the branch and the journal stops here — see
    /// [`Agent::refuse_if_quiesced`] for why no turn starts after it.
    pub fn quiesced(&self) -> bool {
        self.quiesce_request().is_some_and(|q| q.settled())
    }

    /// Is a turn running right now? The roster publishes the same fact, and
    /// this is the one a quiesce consult reads.
    pub fn turn_in_flight(&self) -> bool {
        self.turning.load(Ordering::SeqCst) > 0
    }

    /// This session's parks. The consumer wires the facts a condition is
    /// evaluated against ([`Parks::set_vantage`]) and registers `wait_for`
    /// against the same handle with [`Agent::set_parks`]; what the agent
    /// does with it is end the turn a park was armed in, and journal a fire.
    ///
    /// A `OnceLock` rather than a constructor argument for the reason
    /// [`Dispatcher::observe`] uses one: the registry is assembled first —
    /// the tool has to exist before the dispatcher that serves it — and the
    /// agent that reads the parks comes after.
    pub fn parks(&self) -> &Arc<Parks> {
        self.parks.get_or_init(|| Arc::new(Parks::new()))
    }

    /// Hand this agent the registry a `wait_for` tool was already built
    /// against. Called once, before the first turn; a second call is
    /// ignored, and an agent nobody hands one gets an empty registry that
    /// can never arm anything.
    pub fn set_parks(&self, parks: Arc<Parks>) {
        let _ = self.parks.set(parks);
    }

    /// Journal every park that has resolved, and publish it.
    ///
    /// A fire is a conversation fact, so the log is where it lands — and the
    /// watcher cannot write the log itself, which is single-writer by
    /// construction. This is the writer, called at the same safe points a
    /// waking peer's mail is (after the previous reply's tool results are on
    /// the branch) and by a driver that wakes to one while idle.
    ///
    /// A park that resolved mid-turn is delivered here rather than left for
    /// the settle: the session is already awake and the model can act on it,
    /// so there is nothing left to wait for.
    pub async fn take_fires(&self) -> Vec<RecordId> {
        let mut ids = Vec::new();
        for fire in self.parks().take_fires() {
            let outcome = fire.outcome.words();
            let call_id = fire.call_id.clone();
            let record = match self.session.lock().await.append(RecordKind::TriggerFired {
                call_id: fire.call_id,
                condition: fire.condition.clone(),
                outcome: outcome.clone(),
            }) {
                Ok(r) => r,
                Err(e) => {
                    self.bus.publish(Event::Error(format!(
                        "could not journal a resolved wait: {e:#}"
                    )));
                    continue;
                }
            };
            self.bus.publish(Event::TriggerFired {
                record,
                condition: fire.condition,
                outcome,
                call_id,
            });
            ids.push(record);
        }
        ids
    }

    /// Reach the boundary now, if one is waiting and no turn is in flight.
    ///
    /// An idle session's boundary *is* this instant, and no turn is coming to
    /// journal the marker — so this journals it here, which is what makes an
    /// idle quiesce settle rather than spin out its timeout. A busy session
    /// answers `false`: its boundary is ahead of it, and the loop settles
    /// there, never here. That is the whole of the never-mid-turn rule, and
    /// it is why this can be called in a poll loop.
    pub async fn poll_quiesce(&self) -> anyhow::Result<bool> {
        if self.quiesced() {
            return Ok(true);
        }
        if self.turn_in_flight() {
            return Ok(false);
        }
        self.settle_quiesce().await
    }

    /// Journal the marker and set the flag, once. Called at a boundary — by
    /// the loop when a turn settles, by [`Agent::poll_quiesce`] when there
    /// was no turn to settle.
    ///
    /// Journal first, then mark: a marker that could not be written leaves
    /// the quiesce *unsettled*, so a waiter refuses by timeout rather than
    /// believing a journal that does not hold the note. The session lock is
    /// the serialization point, so two boundaries racing cannot write two
    /// markers.
    async fn settle_quiesce(&self) -> anyhow::Result<bool> {
        let mut session = self.session.lock().await;
        let Some(q) = self.quiesce.lock().unwrap().clone() else {
            return Ok(false);
        };
        if q.settled() {
            return Ok(true);
        }
        let record = session.append(RecordKind::Note { text: q.marker() })?;
        q.mark_settled();
        drop(session);
        self.bus.publish(Event::Quiesced {
            record,
            destination: q.destination.clone(),
        });
        Ok(true)
    }

    /// A quiesced session takes no new turn: a turn is exactly what would
    /// grow the journal this promised had stopped. The drivers stop on their
    /// own — this is the backstop that makes "never grows again" true even
    /// for a caller that did not notice the goodbye.
    fn refuse_if_quiesced(&self) -> anyhow::Result<()> {
        if self.quiesced() {
            anyhow::bail!(
                "this session has quiesced: its journal stops here, so no new turn starts on it"
            );
        }
        Ok(())
    }

    /// Journal every waiting steer as a user message, in the order each
    /// was said, and publish each. Returns how many were delivered.
    ///
    /// Called only where a user-shaped record is safe: at the top of a
    /// loop iteration, once the previous reply's tool results are on the
    /// branch, and after a driver's turn has settled. Between a `tool_use`
    /// and its results it would turn every outstanding call into a
    /// synthetic interruption — the rule [`crate::peer`] states.
    async fn deliver_steers(&self) -> anyhow::Result<usize> {
        let inputs: Vec<Vec<ContentBlock>> = std::mem::take(&mut *self.steers.lock().unwrap());
        let n = inputs.len();
        for input in inputs {
            let message = Message::user(input);
            let record = self
                .session
                .lock()
                .await
                .append(RecordKind::UserMessage(message.clone()))?;
            self.bus.publish(Event::UserMessage { record, message });
        }
        Ok(n)
    }

    /// Take delivery of messages from other harness sessions. Set by a
    /// consumer that registered this session in a swarm; unset, every
    /// peer path below is a no-op.
    pub fn set_inbox(&self, inbox: Arc<dyn crate::peer::PeerInbox>) {
        *self.inbox.write().unwrap() = Some(inbox);
    }

    /// Give the agent somewhere to read persona notes from. Without one, a
    /// pinned persona resolves to nothing and the turn runs in the
    /// harness's own voice.
    pub fn set_persona_source(&self, source: Arc<dyn crate::persona::PersonaSource>) {
        *self.personas.write().unwrap() = Some(source);
    }

    /// Tell the agent how its outbound vault calls are labelled — see
    /// [`crate::attribution`]. The consumer builds the cell (it owns the
    /// session id and the working directory) and hands the same one to the
    /// vault client; the agent keeps it current, seeding the model from the
    /// config it already has and refreshing the persona from the resolution
    /// each turn already runs.
    ///
    /// Unset by default: a session with no vault writes no audit events, and
    /// a label nobody reads is not worth composing.
    pub fn set_attribution(&self, attribution: Arc<Attribution>) {
        attribution.set_model(self.model_key());
        *self.attribution.write().unwrap() = Some(attribution);
    }

    /// Arm the inline command channel: the commands the model writes in its
    /// own prose, and the answers it is owed for them.
    ///
    /// Unset by default, and deliberately an arming rather than a fallback:
    /// a channel is a *surface* — the model is taught a convention, and the
    /// harness promises to answer it — and a session with no channel must
    /// not half-have one. Set by the consumer that also splices the
    /// teaching, so the two cannot come apart.
    ///
    /// Armed or not, this is only read where a tool result may land: after a
    /// reply that asked for no tools and ended cleanly, before the turn
    /// settles. See [`crate::commands::CommandChannel`].
    pub fn set_command_channel(&self, channel: Arc<dyn crate::commands::CommandChannel>) {
        *self.commands.write().unwrap() = Some(channel);
    }

    fn command_channel(&self) -> Option<Arc<dyn crate::commands::CommandChannel>> {
        self.commands.read().unwrap().clone()
    }

    /// Whether a persona could be resolved at all — what a consumer asks
    /// before offering `:persona` as something to do.
    pub fn has_persona_source(&self) -> bool {
        self.personas.read().unwrap().is_some()
    }

    pub fn persona_source(&self) -> Option<Arc<dyn crate::persona::PersonaSource>> {
        self.personas.read().unwrap().clone()
    }

    /// A stable capability note, independent of the pin or vault availability.
    /// Driver backends append this too; checking configuration never does I/O.
    pub fn vault_guidance(&self) -> Option<&'static str> {
        self.has_persona_source().then_some(crate::persona::CITATION_GUIDANCE)
    }

    /// The branch's pinned persona, resolved from its note — **once per
    /// turn**, above the loop, for the reason the pins beside it are read
    /// there: the result goes into the cached prefix, and a prefix that
    /// moved between iterations of a tool loop would miss the cache on
    /// every one of them.
    ///
    /// Forgiving in both of the ways it can fail. No source configured, or
    /// nothing pinned, is not a failure at all and asks nothing. A fetch
    /// that errors logs and resolves to nothing — a vault server that is
    /// restarting must cost the operator a voice, not a turn.
    pub async fn resolve_persona(&self) -> crate::persona::Resolved {
        let Some(source) = self.persona_source() else {
            return Default::default();
        };
        let Some(pin) = self.session.lock().await.persona() else {
            return Default::default();
        };
        match crate::persona::resolve(source.as_ref(), &pin).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("resolving the pinned persona '{pin}' failed: {e:#}");
                Default::default()
            }
        }
    }

    /// Fold everything waiting from other sessions onto the branch.
    ///
    /// Called at turn boundaries, where *everything* waiting is taken — a
    /// boundary is where a note that did not ask to wake is delivered.
    /// Mid-turn only the mail marked to wake is taken, at the same safe
    /// points as [`Agent::deliver_steers`] — once the previous reply's
    /// tool results are on the branch, and again before a settle so an
    /// ask that arrived during the last call is read before the turn
    /// ends. Never between an assistant's `tool_use` and its results.
    ///
    /// Journal first, then publish, like everything else — and at a
    /// boundary the records go on *before* the user message of the turn
    /// that is about to start, so the model reads what its neighbours said
    /// before it reads what the operator asked for. That order is the
    /// useful one: "I am out of `edit.rs`" is context for the request, not
    /// a reply to it. Mid-turn the same record lands after the results it
    /// rides with, which is the order a steer lands in.
    ///
    /// Returns what was drained, so a consumer can decide whether any of
    /// it was worth waking for. A journal failure drops the message rather
    /// than failing the turn, and says so on the bus — the alternative is
    /// a session that cannot take a turn because a neighbour wrote it a
    /// note.
    pub async fn drain_peers(&self) -> Vec<crate::peer::PeerMessage> {
        let Some(inbox) = self.inbox.read().unwrap().clone() else {
            return Vec::new();
        };
        self.journal_peers(inbox.drain()).await
    }

    /// The mid-turn half: only the mail marked to wake, at the steer
    /// points. The rest stays in the inbox — the sender's durable write —
    /// for the boundary [`Agent::drain_peers`] takes everything at.
    async fn drain_waking_peers(&self) -> Vec<crate::peer::PeerMessage> {
        let Some(inbox) = self.inbox.read().unwrap().clone() else {
            return Vec::new();
        };
        self.journal_peers(inbox.drain_waking()).await
    }

    /// Journal what the inbox handed over, then publish it. Shared by both
    /// drains; what differs is which mail the inbox agreed to hand over.
    async fn journal_peers(
        &self,
        messages: Vec<crate::peer::PeerMessage>,
    ) -> Vec<crate::peer::PeerMessage> {
        if messages.is_empty() {
            return messages;
        }
        let mut session = self.session.lock().await;
        let mut kept = Vec::with_capacity(messages.len());
        for m in messages {
            // An external sender gets its own record: replay and every
            // consumer must be able to tell `eidolon send`'s machine
            // input from a colleague's note, exactly as the framing does.
            let kind = if m.external {
                RecordKind::ExternalMessage {
                    from: m.from.clone(),
                    channel: m.channel.clone(),
                    text: m.text.clone(),
                }
            } else {
                RecordKind::PeerMessage {
                    from: m.from.clone(),
                    from_cwd: m.from_cwd.clone(),
                    channel: m.channel.clone(),
                    text: m.text.clone(),
                }
            };
            match session.append(kind) {
                Ok(record) => kept.push((record, m)),
                Err(e) => {
                    tracing::error!(error = %e, from = %m.from, "failed to journal a peer message");
                    self.bus.publish(Event::Error(format!(
                        "failed to journal a message from {}: {e}",
                        m.from
                    )));
                }
            }
        }
        drop(session);
        for (record, m) in &kept {
            self.bus.publish(Event::PeerMessage {
                record: *record,
                from: m.from.clone(),
                from_cwd: m.from_cwd.clone(),
                channel: m.channel.clone(),
                text: m.text.clone(),
                external: m.external,
            });
        }
        kept.into_iter().map(|(_, m)| m).collect()
    }

    pub fn backend(&self) -> Backend {
        self.backend.read().unwrap().clone()
    }

    /// Swap the backend. Takes effect on the next turn; a turn in flight
    /// keeps the backend it started with.
    pub fn set_backend(&self, backend: Backend) {
        *self.backend.write().unwrap() = backend;
    }

    pub fn backend_name(&self) -> String {
        match self.backend() {
            Backend::Provider(p) => p.name().to_string(),
            Backend::Driver(d) => d.name().to_string(),
        }
    }

    pub fn bus(&self) -> &EventBus {
        &self.bus
    }

    /// A snapshot of the current configuration.
    pub fn config(&self) -> AgentConfig {
        self.config.read().unwrap().clone()
    }

    pub fn dispatcher(&self) -> &Arc<Dispatcher> {
        &self.dispatcher
    }

    /// Switch models mid-session. `key` is journaled (so a resume resolves
    /// the same backend), `id` is what the backend is told.
    pub async fn set_model(&self, key: String, id: String) -> anyhow::Result<()> {
        self.session
            .lock()
            .await
            .append(RecordKind::ModelChanged { model: key.clone() })?;
        let mut c = self.config.write().unwrap();
        c.model = id;
        c.model_key = key.clone();
        drop(c);
        // The vault label names the model the session is running, so a switch
        // has to move it: a session that changed models mid-conversation would
        // otherwise attribute everything after the switch to the old one.
        if let Some(a) = self.attribution.read().unwrap().as_ref() {
            a.set_model(key);
        }
        Ok(())
    }

    /// Take the model from a log rather than writing one to it.
    ///
    /// [`Agent::set_model`] journals a `ModelChanged`, because a switch is
    /// a thing that happened to the conversation. Adopting a session is
    /// the opposite direction: the log already says which model it was
    /// written with, and journaling that back would be the harness telling
    /// the session what it just told the harness. Pair it with
    /// [`Agent::adopt_session`], and with a backend built for `id`.
    pub fn adopt_model(&self, key: String, id: String) {
        let mut c = self.config.write().unwrap();
        c.model = id;
        c.model_key = key.clone();
        drop(c);
        // Replay rather than a switch, but the label follows the session
        // either way: a resumed conversation is running the model the log
        // says, and that is what its next writes are attributed to.
        if let Some(a) = self.attribution.read().unwrap().as_ref() {
            a.set_model(key);
        }
    }

    /// Continue in a different log, in place.
    ///
    /// The session is behind an `Arc<Mutex<_>>` the dispatcher holds too,
    /// so replacing its *contents* — rather than the `Arc` — is what makes
    /// one swap reach everything that journals: the agent's loop, the
    /// chokepoint's tool results, a backend driver's transcript. Nothing
    /// re-registers and no consumer re-subscribes.
    ///
    /// The caller owns the rest of the move: the backend for the new
    /// session's model ([`Agent::adopt_model`]) and the directory its tools
    /// run in ([`Dispatcher::set_cwd`]). A turn in flight would journal
    /// into the log it started in, so swap between turns.
    pub async fn adopt_session(&self, session: Session) {
        *self.session.lock().await = session;
    }

    /// The journaled/displayed model name.
    pub fn model_key(&self) -> String {
        let c = self.config.read().unwrap();
        if c.model_key.is_empty() {
            c.model.clone()
        } else {
            c.model_key.clone()
        }
    }

    /// Move the head to an older record so the next turn forks from there.
    /// If the branch below `id` had a different model in effect than the
    /// agent is configured with, a `ModelChanged` is journaled on the new
    /// branch so the log and the config agree on resume.
    pub async fn fork_at(&self, id: crate::session::RecordId) -> anyhow::Result<()> {
        let mut session = self.session.lock().await;
        session.fork_at(id)?;
        let key = self.model_key();
        if session.model().as_deref() != Some(key.as_str()) {
            session.append(RecordKind::ModelChanged { model: key })?;
        }
        Ok(())
    }

    /// Summarise the branch so far and journal a `Compacted` record, with a
    /// `ContextSize` after it carrying the summary's measured size so the
    /// restarted context reads as the summary and not as zero. The summary
    /// is produced by the model itself with no tools offered, from the same
    /// messages it would otherwise have been sent next. A branch with
    /// nothing to compact (no user message since the last compaction) is
    /// left alone and returns `Ok(false)`.
    pub async fn compact(&self, cancel: CancellationToken) -> anyhow::Result<bool> {
        let provider = match self.backend() {
            Backend::Provider(p) => p,
            Backend::Driver(d) => anyhow::bail!(
                "the {} backend manages its own context; compaction is not available here",
                d.name()
            ),
        };
        let cfg = self.config();
        let messages = self.session.lock().await.messages();
        if messages.len() < 2 {
            return Ok(false);
        }
        let mut prompt_messages = messages.clone();
        // The API wants alternation to end on a user message; if the branch
        // ends on an assistant turn that is already the case after we push.
        prompt_messages.push(Message::user_text(COMPACT_PROMPT));
        let req = ChatRequest {
            model: cfg.model.clone(),
            system: Some(COMPACT_SYSTEM.into()),
            messages: prompt_messages,
            tools: Vec::new(),
            max_tokens: cfg.max_tokens.max(2048),
            thinking: None,
            // Nothing will ever share this prefix: the system prompt is
            // the summariser's, not the session's, so not one token of it
            // matches what the next ordinary turn sends. A breakpoint here
            // would write the whole history to the cache at 1.25× and read
            // it back never.
            cache: CacheOptions::off(),
            // The summariser is handed text, and a compaction prompt that
            // carried a screenshot would pay for the pixels twice.
            vision: Some(false),
            // Left unanswered, like the turn's own. A summary would arguably
            // rather be deterministic, but that would be a *second* policy
            // about temperature living here, invisible from the definition
            // that names the first; an operator who sets a model to 0.2 has
            // said what they want from it, compaction included.
            temperature: None,
            // No tools are offered, so there is nothing to force.
            tool_choice: ToolChoice::Auto,
        };
        // The summariser's own clock is dropped, and only the clock: the
        // settle that closes this call is published below like any other's,
        // because a consumer counts what a session spent from the bus live
        // and from the log on a resume, and a settle journaled without being
        // published makes the two frames disagree. No `TurnPace` record is
        // written for it, though, so `timing` is `None` — a resume has no
        // pace to read either, and a live consumer that showed one would be
        // showing a number the log does not have.
        //
        // One attempt, and it always was: a compaction that retried would
        // re-run a call the operator is waiting on and cannot see, and the
        // caller here reports the failure itself. The same consumer as a
        // turn's calls all the same — the assembly, the bus and the repair
        // are not worth a second copy.
        let (reply, stop, usage, _compacting) = self
            .stream_with_retries(
                &provider,
                req,
                &cancel,
                &harnox::llm::RetryPolicy {
                    attempts: 1,
                    ..Default::default()
                },
            )
            .await?;
        if stop == StopReason::Cancelled {
            return Ok(false);
        }
        let summary = reply.text();
        if summary.trim().is_empty() {
            anyhow::bail!("the model returned an empty summary");
        }
        let mut session = self.session.lock().await;
        session.append(RecordKind::Compacted {
            summary: summary.clone(),
            replaced_messages: messages.len() as u32,
        })?;
        // The compaction restarts the provider-facing history at the summary,
        // so the context from here on is the summary's size — measured as the
        // summariser's output. Journal it after the `Compacted` it describes:
        // `Session::last_input_tokens` walks backwards, so this is the number
        // it reads, and a log from before this was written has only the
        // `Compacted` record and reads as unknown rather than as an empty
        // context. A provider that reports no output for the summary leaves
        // the same unknown, which is safer than claiming zero.
        if usage.output_tokens > 0 {
            session.append(RecordKind::ContextSize {
                tokens: usage.output_tokens,
            })?;
        }
        session.append(RecordKind::TurnSettled {
            stop_reason: stop,
            usage,
        })?;
        drop(session);
        self.bus.publish(Event::Compacted {
            summary,
            replaced_messages: messages.len() as u32,
        });
        if usage.output_tokens > 0 {
            self.bus.publish(Event::ContextSize {
                tokens: usage.output_tokens,
            });
        }
        // The settle the record above was written beside, published in the
        // order the records were: a consumer that closes its accounting on
        // this one (the TUI's ledger) reads the same numbers a resume reads
        // out of the log, so a compacted session's spend does not read as
        // free until the next turn settles.
        self.bus.publish(Event::TurnSettled {
            stop_reason: stop,
            usage,
            timing: None,
        });
        Ok(true)
    }

    /// Drop the backend's own session for this branch, so the next turn
    /// rebuilds the conversation from the log instead of resuming one.
    ///
    /// Agora calls this *fresh eyes*, and the name is the argument for it:
    /// a backend that keeps its own conversation (the Claude CLI) is
    /// carrying every kilobyte of tool output that conversation ever saw,
    /// none of which the harness stored and none of which the branch needs
    /// replayed. Dropping the session sheds exactly that, and keeps the
    /// transcript.
    ///
    /// It is also the primitive every other context control needs, because
    /// editing what the branch means below the backend's mark leaves that
    /// backend answering from the unedited version — see
    /// [`crate::session::Session::clear_backend_session`].
    /// What the next turn would send, block by block, without sending it.
    ///
    /// The rule this obeys is the one that makes it worth having: it asks
    /// the *same* functions the turn asks. A report assembled its own way
    /// would be a second opinion about what the model saw, and the whole
    /// value of an inspector is that it is not one.
    pub async fn context_report(&self) -> ContextReport {
        let cfg = self.config();
        let mut blocks = Vec::new();
        let note = self.session.lock().await.session_note();
        // The turn's own resolve, run for real — a report that named the
        // pinned persona without fetching it would be describing an
        // intention rather than the prompt, the same objection the pins
        // below answer by reading.
        //
        // Drawn as its own block although the wire sees one system prompt:
        // whether a persona is being worn, and what it costs, is a
        // question about the prompt that the prompt's own first line
        // cannot answer once the persona is what that line is. The two
        // blocks are the one string decomposed, not counted twice — the
        // `system` block below is built with the persona left out.
        let persona = self.resolve_persona().await;
        if !persona.is_empty() {
            blocks.push(ContextBlock {
                kind: "persona",
                label: persona.name.clone().unwrap_or_else(|| "(unnamed)".into()),
                chars: persona.prefix.chars().count(),
            });
        }
        // The project instructions, resolved by the same rule and drawn as
        // their own block for the same reason — a 77-kc `AGENTS.md` is a
        // question about the prompt the operator will want to have seen
        // the size of, and `:context` is where it is asked.
        let project =
            crate::project::resolve(self.config().project_instructions, &self.dispatcher.cwd());
        if !project.is_empty() {
            blocks.push(ContextBlock {
                kind: "project",
                label: project.file.clone(),
                chars: project.block.chars().count(),
            });
        }
        let session = self.session.lock().await;
        let offered = crate::tool_search::offered(
            self.dispatcher.registry(),
            &crate::tool_search::Reach::of(&session),
        );
        // The pin block is left out here for the reason the persona and the
        // project block are: it is drawn as its own row below, naming the
        // paths it read, and the two rows are that one system string
        // decomposed rather than a part of it counted twice.
        if let Some(system) = self.system_prompt(&cfg, note.as_deref(), "", "", None, &offered) {
            blocks.push(ContextBlock {
                kind: "system",
                label: first_line(&system),
                chars: system.chars().count(),
            });
        }
        let guidance_chars = self
            .dispatcher
            .registry()
            .guidance_for(&offered)
            .map_or(0, |g| g.chars().count());

        let pin_paths = session.pins();
        let tokens = session.last_input_tokens();
        let compacted = session
            .branch()
            .into_iter()
            .rev()
            .find_map(|r| match &r.kind {
                RecordKind::Compacted {
                    replaced_messages, ..
                } => Some(*replaced_messages),
                _ => None,
            });
        let messages = session.messages();
        for m in &messages {
            let (label, chars) = describe(m);
            blocks.push(ContextBlock {
                kind: match m.role {
                    Role::User => "user",
                    Role::Assistant => "assistant",
                },
                label,
                chars,
            });
        }
        drop(session);

        // The turn's own gather, run for real — an inspector that reported
        // a pin it had not tried to read would be describing an intention
        // rather than the prompt. It is drawn as its own row although the
        // wire sees it inside the system prompt, for the reason persona and
        // project are: the paths and their size are what the row is for.
        if let Some(pins) = read_pins(&pin_paths) {
            blocks.push(ContextBlock {
                kind: "pins",
                label: pin_paths.join(", "),
                chars: pins.chars().count(),
            });
        }

        let (sources, tools) = context_sources(&blocks, &messages, guidance_chars);
        ContextReport {
            chars: blocks.iter().map(|b| b.chars).sum(),
            blocks,
            sources,
            tools,
            guidance_chars,
            tokens,
            compacted,
        }
    }

    pub async fn fresh_eyes(&self) -> anyhow::Result<FreshEyes> {
        let Backend::Driver(d) = self.backend() else {
            return Ok(FreshEyes::NoSessionKept);
        };
        let name = d.name().to_string();
        let dropped = self.session.lock().await.clear_backend_session(&name)?;
        Ok(if dropped {
            FreshEyes::Dropped
        } else {
            FreshEyes::NothingHeld
        })
    }

    /// Composed per turn rather than stored, because the registry can change
    /// between turns and stale guidance is worse than none — it would
    /// describe tools the model can no longer call.
    /// The configured system prompt, the operator's standing note, and the
    /// registered tools' own guidance ([`crate::tool::ToolRegistry::guidance`]).
    ///
    /// A block sits in a **fixed** position, because this string is the
    /// cached prefix: a block that moved between turns would miss the
    /// cache on every one of them.
    ///
    /// A pinned persona goes **first**, ahead of the configured prompt, for
    /// two reasons. It is what the rest is read in the voice of, and a
    /// character introduced after the instructions reads as an afterthought
    /// to them. And it is the most stable block of the five — a persona is
    /// pinned once and then worn for the length of a conversation — so it
    /// is the right thing to have at the front of a prefix cache. Changing
    /// it, like changing the note or the project instructions below it, is
    /// one deliberate miss.
    ///
    /// The project instructions sit between the configured prompt and the
    /// session note: the prompt is the harness's voice and they are the
    /// directory's, and of the two things the operator authored they are
    /// the one that outlives the session.
    ///
    /// The pinned files come after the note and ahead of the tools' own
    /// guidance, because the guidance is the one block here that can change
    /// *within* a session — a deferred tool's notes join the prompt when the
    /// tool does — so everything stable sits in front of it.
    ///
    /// A pin lives here rather than riding the newest user message, which is
    /// where it first looks like it belongs: the message list is rebuilt from
    /// the journal on every call and the journal never holds a pin's bytes, so
    /// a block appended there **moves** from one call to the next — and a
    /// block that moves forfeits the cache from its old position, on every
    /// call, not once. At a fixed offset a pin costs one deliberate miss on
    /// the turn the file changes, which is the trade the note above makes.
    ///
    /// `offered` is the request's tool list by name, so a deferred tool's
    /// notes join the prompt when the tool does and not before.
    pub fn system_prompt(
        &self,
        cfg: &AgentConfig,
        note: Option<&str>,
        persona: &str,
        project: &str,
        pins: Option<&str>,
        offered: &[String],
    ) -> Option<String> {
        let persona = (!persona.trim().is_empty()).then(|| persona.to_string());
        let project = (!project.trim().is_empty()).then(|| project.to_string());
        let note = note
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(|n| format!("## About this session\n\n{n}"));
        let pins = pins
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_string);
        let parts: Vec<String> = [
            persona,
            cfg.system.clone(),
            self.vault_guidance().map(str::to_string),
            project,
            note,
            pins,
            self.dispatcher.registry().guidance_for(offered),
        ]
        .into_iter()
        .flatten()
        .map(|p| p.trim_end().to_string())
        .filter(|p| !p.is_empty())
        .collect();
        (!parts.is_empty()).then(|| parts.join("\n\n"))
    }

    pub fn session(&self) -> &Arc<Mutex<Session>> {
        &self.session
    }

    /// Run one user turn to completion — and then every follow-up queued
    /// for it, each as a turn of its own, so that this returning means
    /// nothing queued is still going to run.
    ///
    /// The outcome is the *last* turn's. A consumer counting what the
    /// whole call cost reads the `TurnSettled` events, which come one per
    /// settle. A turn that ends without settling stops the run there and
    /// leaves the rest of the queue where it was.
    pub async fn run_turn(
        &self,
        input: Vec<ContentBlock>,
        cancel: CancellationToken,
    ) -> anyhow::Result<TurnOutcome> {
        self.refuse_if_quiesced()?;
        // In flight from here, not from inside the loop: the records
        // `one_turn` writes before the model is called are part of the turn,
        // and an idle-boundary quiesce landing between them and the loop
        // would journal its marker in the middle of a turn that then grew.
        self.turning.fetch_add(1, Ordering::SeqCst);
        self.parks().begin_turn();
        let _in_flight = InFlight(&self.turning);
        let mut outcome = self.one_turn(input, &cancel).await?;
        while matches!(outcome, TurnOutcome::Settled { .. }) {
            // A quiesced session runs nothing it queued for after the settle:
            // the settle was the boundary it was waiting for, and a
            // follow-up is another turn on a journal that has stopped.
            if self.quiesced() {
                break;
            }
            let Some(next) = self.take_follow_up() else {
                break;
            };
            outcome = self.one_turn(next, &cancel).await?;
        }
        Ok(outcome)
    }

    /// One user message, journaled and run to a settle.
    async fn one_turn(
        &self,
        input: Vec<ContentBlock>,
        cancel: &CancellationToken,
    ) -> anyhow::Result<TurnOutcome> {
        // The turn boundary a peer's message waits for: before the user
        // message, so a neighbour's note is context for the request rather
        // than a reply to it. Mid-turn delivery is `continue_turn`'s job,
        // at the same safe points as a steer.
        self.drain_peers().await;
        let message = Message::user(input);
        let record = self
            .session
            .lock()
            .await
            .append(RecordKind::UserMessage(message.clone()))?;
        // Journal, then publish — the same order everything else here
        // follows. A consumer has usually drawn this message already, from
        // what the operator typed; what it could not know until now is
        // which record it became.
        self.bus.publish(Event::UserMessage { record, message });
        self.continue_turn(cancel.clone()).await
    }

    /// Continue from the current branch without adding a user message —
    /// used to resume a session whose last turn did not settle.
    pub async fn continue_turn(&self, cancel: CancellationToken) -> anyhow::Result<TurnOutcome> {
        self.refuse_if_quiesced()?;
        // In flight for the whole of this call, whichever way it returns —
        // the flag a quiesce consult reads to know the boundary is not now.
        self.turning.fetch_add(1, Ordering::SeqCst);
        let _in_flight = InFlight(&self.turning);
        // Which turn this is, for the parks armed inside it: only a park
        // this turn asked for may end it.
        let epoch = self.parks().begin_turn();
        let provider = match self.backend() {
            Backend::Provider(p) => p,
            Backend::Driver(d) => return self.drive(d, cancel).await,
        };
        let mut total = Usage::default();
        let mut calls: u32 = 0;
        // The most recent input a call of this turn reported — the context
        // as last measured, for the `ContextSize` a cancel writes before it
        // leaves (see `Agent::cancelled`). A call that reported none leaves
        // the earlier measurement standing: an unknown is not a zero.
        let mut context: Option<u64> = None;
        // What the calls cost in wall clock, measured here: written as a
        // `TurnPace` record beside the settle and published with it (see
        // `TurnTiming`).
        let mut timing = TurnTiming::default();

        // Answer any tool uses left dangling by a crash before calling the
        // model again; the dispatcher replays journaled results and runs
        // the rest.
        let dangling = self.session.lock().await.unanswered_tool_uses();
        for (id, name, input) in dangling {
            if cancel.is_cancelled() {
                return self.cancelled(total, calls, None).await;
            }
            let call = ToolCall {
                id,
                name,
                input: input.to_value(),
                origin: CallOrigin::Model,
            };
            self.dispatcher.dispatch(call, cancel.child_token()).await;
        }

        // Read once per turn, like the persona below it, and for the same
        // reason: this block lands in the cached prefix, so re-reading the
        // file on every iteration of a tool loop would let a mid-turn edit
        // rewrite the front of the prompt the model is already reasoning
        // from.
        let pins = read_pins(&self.session.lock().await.pins());
        // Resolved once for the whole turn, beside the pins and for the
        // same reason: it lands in the cached prefix, so re-reading it per
        // iteration would move the front of the prompt on every pass of a
        // tool loop and miss the cache on all of them. Fresh *per turn*
        // though — the note is edited by the character it describes, and a
        // body held across turns goes on speaking as someone she has
        // stopped being.
        let persona = self.resolve_persona().await;
        // The vault label is kept here, off the resolution the turn has
        // already run: the write it attributes happens inside this turn, and
        // `:persona` is a pin the next turn is the first to wear. Only the
        // note's own name — a pin may be a path, and a path in the audit log
        // names the note rather than the character.
        if let Some(a) = self.attribution.read().unwrap().as_ref() {
            a.set_persona(persona.name.clone());
        }
        let persona = persona.prefix;
        // The project instructions are read on the same schedule and for
        // half of the same reason: stable through a tool loop for the
        // cache, and fresh per turn because an `AGENTS.md` is edited
        // mid-conversation — often by the model itself, which is the one
        // editor a held copy is guaranteed to be stale against.
        let project =
            crate::project::resolve(self.config().project_instructions, &self.dispatcher.cwd())
                .block;

        // Set for exactly one request, by the branch below that catches a
        // model naming a tool instead of calling it. `forced_once` is what
        // keeps that from being a loop: a model that answers the forced
        // request in prose too has said its piece, and the turn settles on
        // what it said.
        let mut force = false;
        let mut forced_once = false;

        // The wrap-up nudge fires once, when the turn is a few calls from
        // the wall — see `RecordKind::TurnBudget`. `nudge_at` is a few
        // calls' runway rather than a fraction of the budget so that it
        // means the same thing at every setting: room to finish the step
        // in flight, not a proportion of the turn. Clamped so a small
        // budget (a test's, or a deliberately tight one) still gets its
        // nudge, and a budget of one gets it before the only call.
        let max_iterations = self.config().max_iterations;
        let nudge_at = (max_iterations / 2).clamp(1, 8);
        let mut nudged = false;

        for iteration in 0..max_iterations {
            if cancel.is_cancelled() {
                return self.cancelled(total, calls, context).await;
            }
            // What was said to this turn since the last call goes on the
            // branch here, where the previous reply's tool results already
            // are — so the next request carries both, results first.
            self.deliver_steers().await?;
            // Mail marked to wake is just as time-sensitive as a steer, so
            // a turn in flight takes it at the same safe point. A note
            // that did not ask to wake is not: it stays in the inbox for
            // the boundary, where everything is taken. The first iteration
            // is skipped because the user message that opened the turn is
            // still the last user-shaped record, and a peer note
            // journaled behind it would be a second user turn in a row;
            // anything that arrived in that instant is caught by the
            // settle check below or the next iteration.
            if calls > 0 {
                self.drain_waking_peers().await;
                // A park that resolved while this turn was running is the
                // same kind of fact as a peer's note and lands in the same
                // place: this safe point, never between a `tool_use` and its
                // results.
                self.take_fires().await;
            }
            // The wrap-up nudge, at the same safe point: the results of
            // the previous reply's calls are on the branch, so it rides
            // them exactly as a steer would. Firing here rather than at
            // the last call means the model has a few calls to finish
            // and summarise, which is the whole point of a wall it can
            // see coming rather than one it hits.
            if !nudged && max_iterations - iteration == nudge_at {
                nudged = true;
                let record = self.session.lock().await.append(RecordKind::TurnBudget {
                    calls_left: nudge_at,
                })?;
                self.bus.publish(Event::TurnBudget {
                    record,
                    calls_left: nudge_at,
                });
            }

            let cfg = self.config();
            let forcing = std::mem::take(&mut force);
            // The tools offered are read off the branch here, beside the
            // messages: every tool not deferred, plus the deferred ones
            // this branch has searched for or called. Recomputed per
            // request and kept nowhere, so a resume or a fork offers what
            // its own branch has earned (`crate::tool_search`).
            let (messages, cache_key, note, offered) = {
                let s = self.session.lock().await;
                let reach = crate::tool_search::Reach::of(&s);
                (
                    s.messages(),
                    cache_key(s.path()),
                    s.session_note(),
                    crate::tool_search::offered(self.dispatcher.registry(), &reach),
                )
            };
            // Read once for the whole turn, above the loop, and composed in
            // unchanged on every iteration: a file that changed mid-turn
            // would make the model's own earlier reasoning refer to
            // something it can no longer see.
            let registry = self.dispatcher.registry();
            let tools: Vec<crate::provider::ToolDef> = registry
                .manifests()
                .into_iter()
                .filter(|m| offered.contains(&m.name))
                .map(|m| m.for_model())
                .collect();
            let req = ChatRequest {
                model: cfg.model.clone(),
                system: self.system_prompt(
                    &cfg,
                    note.as_deref(),
                    &persona,
                    &project,
                    pins.as_deref(),
                    &offered,
                ),
                messages,
                tools,
                max_tokens: cfg.max_tokens,
                thinking: cfg.thinking.clone(),
                cache: CacheOptions {
                    retention: cfg.cache_retention,
                    key: cache_key,
                },
                // Left unanswered here on purpose. The core does not hold a
                // catalog and so cannot know whether *this* model can see;
                // the provider that serves the model does, and fills it in
                // on the way past (`HttpProvider::stream`). `None` means
                // nobody has said yet — which is not the same as "no", and
                // is why the field is three-valued.
                vision: None,
                // Unanswered here for the same reason and filled in the same
                // place: the definition names it, and `None` is nobody said
                // rather than zero.
                temperature: None,
                // Ordinarily the model's own choice. `force` is set only
                // after it has already answered a tool-shaped question in
                // prose — see below.
                tool_choice: if forcing {
                    ToolChoice::Required
                } else {
                    ToolChoice::Auto
                },
            };

            let tool_names: Vec<String> = req.tools.iter().map(|t| t.name.clone()).collect();
            let (message, stop_reason, usage, one_call) = match self
                .stream_with_retries(&provider, req, &cancel, &harnox::llm::RetryPolicy::default())
                .await
            {
                Ok(x) => x,
                // A forced request is the harness's idea, not the operator's,
                // so it may not cost them the turn. An endpoint that will not
                // take `tool_choice` says so with a 400, and the reply that
                // prompted the retry is already journaled and on screen —
                // settling on it is what would have happened without this.
                Err(e) if forcing => {
                    tracing::warn!(error = %format!("{e:#}"), "the forced-tool request failed; settling on the reply that prompted it");
                    self.bus.publish(Event::Error(format!(
                        "could not re-ask with a tool required: {e:#}"
                    )));
                    let usage = total;
                    let mut session = self.session.lock().await;
                    if timing.calls > 0 {
                        session.append(RecordKind::TurnPace {
                            timing,
                            output_tokens: usage.output_tokens,
                        })?;
                    }
                    session.append(RecordKind::TurnSettled {
                        stop_reason: StopReason::EndTurn,
                        usage,
                    })?;
                    drop(session);
                    self.bus.publish(Event::TurnSettled {
                        stop_reason: StopReason::EndTurn,
                        usage,
                        timing: Some(timing),
                    });
                    return Ok(TurnOutcome::Settled {
                        stop_reason: StopReason::EndTurn,
                        usage,
                        calls,
                    });
                }
                Err(e) => {
                    self.bus.publish(Event::Error(format!("{e:#}")));
                    return Err(e);
                }
            };
            total += usage;
            calls += 1;
            timing.add(one_call);
            // What this call read, when it said: the last measurement of
            // the turn, and the one a cancel journals.
            let read = usage.total_input();
            if read > 0 {
                context = Some(read);
            }

            // Journal whatever arrived, even if cancelled mid-stream.
            if !message.content.is_empty() {
                let record = self
                    .session
                    .lock()
                    .await
                    .append(RecordKind::AssistantMessage(message.clone()))?;
                self.bus.publish(Event::AssistantMessage {
                    record,
                    message: message.clone(),
                });
            }
            if stop_reason == StopReason::Cancelled || cancel.is_cancelled() {
                return self.cancelled(total, calls, context).await;
            }

            let tool_uses: Vec<ToolCall> = message
                .tool_uses()
                .map(|(id, name, input)| ToolCall {
                    id: id.to_string(),
                    name: name.to_string(),
                    input: input.to_value(),
                    origin: CallOrigin::Model,
                })
                .collect();

            if tool_uses.is_empty() {
                // The commands the model wrote in its own prose, answered
                // here — where a reply's tool results would land, and
                // before the turn can settle. A reply that marks a command
                // is owed its answer exactly as a reply that made a call is
                // owed its result, and it is owed it *now*: the model reads
                // the answer and goes on in the same turn instead of
                // yielding to the operator with a question the harness has
                // already answered. Journaled, then published, in that
                // order like everything else; then one more call —
                // a continuation, not a retry. The prose that prompted it
                // is already journaled and on screen, and nothing published
                // may be re-run.
                //
                // `EndTurn` only. A truncated tail (`MaxTokens`) may be half
                // a command, and a half-read command is a wrong answer
                // confidently given; the discipline the channel teaches is
                // to write commands last and stop, and a model that ran out
                // of room mid-command has not finished writing one. Replies
                // that made calls are answered at the dispatch seam below,
                // where their marks ride with their results.
                if stop_reason == StopReason::EndTurn {
                    let answers = match self.command_channel() {
                        Some(channel) => channel.answer(&message, cancel.child_token()).await,
                        None => None,
                    };
                    if let Some(lines) = answers.filter(|l| !l.is_empty()) {
                        let record = self
                            .session
                            .lock()
                            .await
                            .append(RecordKind::CommandResults { lines: lines.clone() })?;
                        self.bus.publish(Event::CommandResults {
                            record,
                            lines,
                        });
                        continue;
                    }
                }
                // A reply that is nothing but the name of a tool it was
                // offered is a model that reached for the tool and missed the
                // channel. Ask again with the channel forced — which tool,
                // and with what arguments, stays its own choice; the harness
                // supplies neither.
                //
                // A *continuation*, not a retry: the prose is already
                // journaled and already on screen, and `stream_with_retries`
                // is emphatic that nothing published may be re-run. So the
                // model sees its own answer and is asked again over the top
                // of it, which is also the honest thing to show the operator.
                if !forced_once
                    && stop_reason == StopReason::EndTurn
                    && names_only_a_tool(&message, &tool_names)
                {
                    tracing::info!(reply = %message.text().trim(), "the model named a tool instead of calling it; re-asking with the channel forced");
                    forced_once = true;
                    force = true;
                    continue;
                }
                // A steer that arrived during this call has not been read.
                // The turn is not over until it has been: one more call,
                // carrying it, and the model settles on what it makes of
                // the redirect rather than on a reply that never saw it.
                if self.has_steers() {
                    continue;
                }
                // The same goes for mail that asked to wake: it is
                // time-sensitive, so a turn must not settle with one still
                // in the inbox. Draining here is safe because this reply
                // made no tool calls — there are no results owed. A note
                // that did not ask to wake is *not* held to this: it waits
                // in the inbox for the next boundary, which is the deal
                // its sender chose.
                if !self.drain_waking_peers().await.is_empty() {
                    continue;
                }
                // Two different numbers, and the difference is the whole
                // point. This call is the last of the turn and so the only
                // one that saw the entire transcript: its input *is* the
                // context. `total` is the sum over every call the loop
                // made, which is what the turn cost.
                let context = usage.total_input();
                let usage = total;
                let mut session = self.session.lock().await;
                session.append(RecordKind::ContextSize { tokens: context })?;
                // Before the settle, as `ContextSize` is: the pace belongs to
                // the calls the settle's `usage` totals.
                if timing.calls > 0 {
                    session.append(RecordKind::TurnPace {
                        timing,
                        output_tokens: usage.output_tokens,
                    })?;
                }
                session.append(RecordKind::TurnSettled { stop_reason, usage })?;
                drop(session);
                self.bus.publish(Event::ContextSize { tokens: context });
                self.bus.publish(Event::TurnSettled {
                    stop_reason,
                    usage,
                    timing: Some(timing),
                });
                // A quiesce that was asked for during this turn rides this
                // boundary: the settle above is the last thing that
                // happened, and the marker behind it is what says the
                // journal stops here. `Err` is reported rather than
                // returned — the settle is already published, and nothing
                // published is re-run.
                if let Err(e) = self.settle_quiesce().await {
                    self.bus.publish(Event::Error(format!(
                        "could not journal the quiesce marker: {e:#}"
                    )));
                }
                return Ok(TurnOutcome::Settled {
                    stop_reason,
                    usage,
                    calls,
                });
            }

            // Whether *this* reply's calls asked to end the turn — a
            // dismissed question does. Earlier turns' results are replayed
            // answers and never carry the flag.
            let mut dismissed = false;
            for call in tool_uses {
                if cancel.is_cancelled() {
                    return self.cancelled(total, calls, context).await;
                }
                let output = self.dispatcher.dispatch(call, cancel.child_token()).await;
                dismissed |= output.ends_turn;
            }

            // The reply that armed a park is the reply that ends the turn.
            // The harness owns that decision on purpose: leaving it to the
            // model to stop and wait is the polling this whole mechanism
            // exists to remove, and a model that has just been told "you
            // will be woken" has every incentive to go and look instead.
            // Settling here is safe because every `tool_use` of this reply
            // has its result journaled — nothing is owed, so replay
            // synthesizes no interruption.
            //
            // Only a park armed by *this* turn counts: one from an earlier
            // turn that has not fired yet must not cut short a turn the
            // operator's own message started. A session already asked to
            // quiesce settles on the marker instead — it is leaving.
            if let Some(condition) = self
                .parks()
                .armed_for(epoch)
                .filter(|_| self.quiesce_request().is_none())
            {
                tracing::info!(condition = %condition, "the turn ends here: the session is parked");
                return self
                    .settle_harness(StopReason::Waiting, total, context, timing, calls)
                    .await;
            }

            // A question the operator exited without answering ends the
            // turn here rather than going back to the model: the result is
            // journaled like every result, but fed back as an ordinary
            // reply the model has one move — ask again — and that is the
            // dialog the dismissal just closed. The settle names the
            // reason (`StopReason::Dismissed`) instead of dressing up as
            // the model stopping, and the next word is the operator's. A
            // park armed by the same reply outranks this, above: the
            // session's own ask to be woken stands.
            if dismissed {
                return self
                    .settle_harness(StopReason::Dismissed, total, context, timing, calls)
                    .await;
            }

            // A reply that made calls can mark commands too, and the
            // first-exposure hedge makes that the common shape: a model
            // that does not yet trust the channel writes the marked line
            // *and* reaches for the ordinary tool in the same reply, and a
            // channel that stays silent about it has confirmed the
            // distrust — observed live, one hedged `! list notes` answered
            // with nothing, after which the model decided the channel was
            // dead, apologised for it, and never marked again. So the
            // marks are answered here as well: after the results this
            // reply is owed, never in front of them, journaled then
            // published like everything else. No continuation is forced —
            // a reply that made calls already earns its next model call,
            // and that call reads the results and the answers together.
            //
            // `ToolUse` only, for the same reason the settle path is
            // `EndTurn` only: a truncated tail may be half a command, and
            // half a command answered is a wrong answer confidently given.
            if stop_reason == StopReason::ToolUse {
                let answers = match self.command_channel() {
                    Some(channel) => channel.answer(&message, cancel.child_token()).await,
                    None => None,
                };
                if let Some(lines) = answers.filter(|l| !l.is_empty()) {
                    let record = self
                        .session
                        .lock()
                        .await
                        .append(RecordKind::CommandResults { lines: lines.clone() })?;
                    self.bus.publish(Event::CommandResults { record, lines });
                }
            }
        }

        self.bus.publish(Event::Error(format!(
            "turn hit the iteration limit ({max_iterations} model calls) — the work is still on \
the branch, so a follow-up message continues from where this stopped; raise `max_iterations` \
in the config if turns keep needing more"
        )));
        // Journaled for the reason a cancel's spend is: this turn made
        // `max_iterations` model calls and then ended without a settle,
        // so it is the most expensive way a turn can finish and the one
        // least excusable to price at nothing.
        //
        // The context rides with it, exactly as a cancel's does: the calls
        // this turn made read the whole transcript, and a turn that ended
        // at the iteration limit left the gauge on the previous turn's
        // number just as surely as one the operator stopped. Written
        // before the spend, like the settle's own, so the resume walk
        // hands the measurement to this turn's ledger row.
        let mut session = self.session.lock().await;
        if let Some(tokens) = context.filter(|t| *t > 0) {
            session.append(RecordKind::ContextSize { tokens })?;
        }
        if spent(total) {
            session.append(RecordKind::TurnSpend {
                usage: total,
                calls,
            })?;
        }
        drop(session);
        if let Some(tokens) = context.filter(|t| *t > 0) {
            self.bus.publish(Event::ContextSize { tokens });
        }
        Ok(TurnOutcome::Exhausted {
            usage: total,
            calls,
        })
    }

    /// End the turn as cancelled, saying what it had spent getting there.
    ///
    /// The spend is journaled **before** the `Cancelled` it explains, so
    /// that a reader walking the branch has the figure in hand by the
    /// time it meets the boundary, and so that a log truncated between
    /// the two loses the cancellation rather than the cost.
    ///
    /// The record is written only when there is a **usage** to write,
    /// which is not the same as having made calls. A cancel before the
    /// first model call has no cost. But so, in effect, does a cancelled
    /// turn on the Claude CLI, which reports usage in one final `result`
    /// event that a killed process never sends: the count is known and
    /// the cost is not. Journaling a zero there would put a turn on the
    /// branch that had been measured and found free, which is the one
    /// thing the pricing here is careful never to say — unknown is the
    /// honest answer, and it is what an absent record means. The count
    /// still travels on the outcome and the event, where a consumer can
    /// say "9 turns" without pricing them.
    ///
    /// The **context** rides the same way, and `context` is what a call of
    /// this turn last measured: the input of the most recent call that
    /// reported one, which is the last call that saw the whole transcript.
    /// A cancelled turn used to journal no [`RecordKind::ContextSize`] at
    /// all, so every consumer went on drawing the last *settled* turn's
    /// number. That is stale for one turn of ordinary work and flatly
    /// wrong after a compaction — the standing figure is then the
    /// summary's own size — and it never corrected itself while the
    /// operator kept stopping turns (long tool loops are exactly when one
    /// stops one), which is how a gauge reads `ctx 3.7k` on a context of
    /// two hundred thousand tokens. `None` — no call reported an input, a
    /// cancel before the first call or a stream cut before the wire's
    /// usage chunk — journals nothing, leaving the last real measurement
    /// standing rather than replacing it with a zero.
    async fn cancelled(
        &self,
        usage: Usage,
        calls: u32,
        context: Option<u64>,
    ) -> anyhow::Result<TurnOutcome> {
        // Zero is not a context size: it is what a call that reported
        // nothing looks like, and a wire's usage that has not arrived is
        // unknown rather than empty.
        let context = context.filter(|t| *t > 0);
        let mut session = self.session.lock().await;
        if let Some(tokens) = context {
            session.append(RecordKind::ContextSize { tokens })?;
        }
        if spent(usage) {
            session.append(RecordKind::TurnSpend { usage, calls })?;
        }
        session.append(RecordKind::Cancelled)?;
        drop(session);
        if let Some(tokens) = context {
            self.bus.publish(Event::ContextSize { tokens });
        }
        self.bus.publish(Event::Cancelled { usage, calls });
        Ok(TurnOutcome::Cancelled { usage, calls })
    }

    /// The settle a harness-owned end writes: the context as last
    /// measured, the pace of the turn's calls, and the settle itself, each
    /// journaled before it is published, with a pending quiesce riding the
    /// boundary. Two ends reach it — a park the reply armed
    /// ([`StopReason::Waiting`]) and a question the operator dismissed
    /// ([`StopReason::Dismissed`]). The model's own settle is written
    /// inline above, because its context is the last call's input rather
    /// than this loop's running figure.
    ///
    /// `context` passes the same rule a cancel's does: zero is not a
    /// context size, so a stream that never reported usage leaves the last
    /// real measurement standing rather than replacing it with a zero.
    async fn settle_harness(
        &self,
        stop_reason: StopReason,
        usage: Usage,
        context: Option<u64>,
        timing: TurnTiming,
        calls: u32,
    ) -> anyhow::Result<TurnOutcome> {
        let context = context.filter(|t| *t > 0);
        let mut session = self.session.lock().await;
        if let Some(tokens) = context {
            session.append(RecordKind::ContextSize { tokens })?;
        }
        if timing.calls > 0 {
            session.append(RecordKind::TurnPace {
                timing,
                output_tokens: usage.output_tokens,
            })?;
        }
        session.append(RecordKind::TurnSettled { stop_reason, usage })?;
        drop(session);
        if let Some(tokens) = context {
            self.bus.publish(Event::ContextSize { tokens });
        }
        self.bus.publish(Event::TurnSettled {
            stop_reason,
            usage,
            timing: Some(timing),
        });
        if let Err(e) = self.settle_quiesce().await {
            self.bus.publish(Event::Error(format!(
                "could not journal the quiesce marker: {e:#}"
            )));
        }
        Ok(TurnOutcome::Settled {
            stop_reason,
            usage,
            calls,
        })
    }

    /// A backend that owns its loop runs the turn — and one more for every
    /// steer or waking peer message that arrived while it did.
    ///
    /// The harness cannot put a message in front of a call it does not
    /// make, so a steer or an ask here rides the next turn on the same
    /// driver, run at once. A note that did not ask to wake is journalled
    /// at this boundary and left for the next turn, whatever starts it.
    /// Usage and calls are summed across those and the stop reason is the
    /// last one's, so the outcome reads as one turn to the consumer that
    /// asked for one; the log shows the settles it took.
    async fn drive(
        &self,
        driver: Arc<dyn TurnDriver>,
        cancel: CancellationToken,
    ) -> anyhow::Result<TurnOutcome> {
        let mut total = Usage::default();
        let mut calls = 0;
        loop {
            let out = match driver.run_turn(self, cancel.clone()).await {
                // The driver's own spend before it stopped, plus
                // whatever earlier turns on this driver had already cost:
                // the outcome reads as one turn to the consumer that
                // asked for one, cancelled or not.
                Ok(TurnOutcome::Cancelled { usage, calls: n }) => {
                    total += usage;
                    // No context from here: `usage` is the backend's total
                    // for a turn it did not finish, which is a turn total
                    // and never a context. A driver that *did* measure one
                    // journals it beside this, through its own sink — see
                    // `journal_context` in the Claude driver — so what
                    // arrives here is only the spend.
                    return self.cancelled(total, calls + n, None).await;
                }
                Ok(TurnOutcome::Settled {
                    stop_reason,
                    usage,
                    calls: n,
                }) => {
                    total += usage;
                    calls += n;
                    TurnOutcome::Settled {
                        stop_reason,
                        usage: total,
                        calls,
                    }
                }
                Ok(o) => return Ok(o),
                Err(e) => {
                    self.bus.publish(Event::Error(format!("{e:#}")));
                    return Err(e);
                }
            };
            if cancel.is_cancelled() {
                return Ok(out);
            }
            // A quiesce outranks a wake. The session is finishing, so a
            // message that would otherwise buy another turn on this driver
            // does not: the settled driver turn just above *was* the
            // boundary, and this is where the marker goes.
            if self.quiesce_request().is_some() {
                if let Err(e) = self.settle_quiesce().await {
                    self.bus.publish(Event::Error(format!(
                        "could not journal the quiesce marker: {e:#}"
                    )));
                }
                return Ok(out);
            }
            // A settled driver turn is a boundary: everything waiting is
            // journalled, waking or not. But only an ask — a waking
            // message, or a steer — buys another turn on the driver; a
            // note that did not ask to wake is context for whatever turn
            // comes next, not a turn of its own.
            let steers = self.deliver_steers().await?;
            let peers = self.drain_peers().await;
            let asks = peers.iter().filter(|m| m.wake).count();
            if steers == 0 && asks == 0 {
                return Ok(out);
            }
        }
    }

    /// One streamed model call, through the shared consumer: the retry rules
    /// and the assembly live in [`harnox::llm::stream_with_retries`], because
    /// a one-shot completion needs exactly them and must not grow a second
    /// interpreter of the same stream. What is added here is what only this
    /// crate knows: the bus a consumer draws from, the registry's tool names,
    /// and the repair a journaled `tool_use` block must pass through.
    ///
    /// A shared or free endpoint returns 502s and drops connections under
    /// load, and a long agentic turn makes many calls — so the chance of at
    /// least one failing approaches certainty. Retrying only before anything
    /// has been published keeps the transcript and the log exactly as they
    /// would have been had the attempt succeeded first time.
    async fn stream_with_retries(
        &self,
        provider: &Arc<dyn Provider>,
        req: ChatRequest,
        cancel: &CancellationToken,
        policy: &harnox::llm::RetryPolicy,
    ) -> anyhow::Result<(Message, StopReason, Usage, CallTiming)> {
        // What a streamed tool name may resolve onto. Gathered before the
        // stream so a hallucinated or gateway-mangled name never becomes a
        // journaled `ToolUse` — see [`finalize_tool_calls`].
        let known: Vec<String> = self
            .dispatcher
            .registry()
            .manifests()
            .into_iter()
            .map(|m| m.name)
            .collect();
        let mut sink = BusSink { bus: &self.bus };
        let collected =
            harnox::llm::stream_with_retries(&**provider, req, cancel, policy, &mut sink).await?;
        Ok((
            finalize_tool_calls(collected.message, &known),
            collected.stop_reason,
            collected.usage,
            collected.timing,
        ))
    }
}

const COMPACT_SYSTEM: &str = "You are compacting a coding session's context. Produce a dense, factual summary that lets the same assistant continue the work without the original transcript.";
const COMPACT_PROMPT: &str = "Summarise this conversation for continuation. Include: the user's goals and constraints; decisions made and why; files touched and what changed; current state of the work; what remains; any facts, paths, commands or errors that will be needed. Plain prose and lists, no preamble.";

/// Publishes a stream as it arrives, and makes its tool arguments safe to
/// journal — the two things this crate, and only this crate, has a say in.
///
/// The bus half is why a consumer sees deltas as events instead of inferring
/// them from a rendering; the repair half is the one place a *policy* about
/// the log touches assembly at all.
struct BusSink<'a> {
    bus: &'a EventBus,
}

impl StreamSink for BusSink<'_> {
    fn message_start(&mut self) {
        self.bus.publish(Event::MessageStart);
    }
    fn text_delta(&mut self, text: &str) {
        self.bus.publish(Event::TextDelta(text.to_string()));
    }
    fn thinking_delta(&mut self, text: &str) {
        self.bus.publish(Event::ThinkingDelta(text.to_string()));
    }
    fn tool_use_start(&mut self, id: &str, name: &str) {
        self.bus.publish(Event::ToolUseStart {
            id: id.to_string(),
            name: name.to_string(),
        });
    }
    fn tool_input_delta(&mut self, id: Option<&str>, partial: &str) {
        if let Some(id) = id {
            self.bus.publish(Event::ToolInputDelta {
                id: id.to_string(),
                partial_json: partial.to_string(),
            });
        }
    }
    fn retrying(&mut self, attempt: u32, of: u32, error: &anyhow::Error) {
        self.bus.publish(Event::Error(format!(
            "{:#} — retrying ({attempt}/{of})",
            error
        )));
    }
    fn tool_input(&self, name: &str, raw: &str) -> String {
        journal_safe_tool_input(name, raw)
    }
}

/// Whatever a stream accumulated for a tool call, as the JSON object that
/// will be journaled and replayed onto the wire.
///
/// A `tool_use` block is re-sent on every later request of the turn, so an
/// input that does not parse poisons the rest of the session permanently —
/// see [`crate::schema::repair_tool_input`], which salvages what it can and
/// answers with an empty object when it cannot. Applied as the block closes,
/// before anything is journaled or published.
fn journal_safe_tool_input(name: &str, raw: &str) -> String {
    match crate::schema::repair_tool_input(raw) {
        Ok(v) => v.to_string(),
        Err((v, why)) => {
            tracing::warn!(tool = %name, %why, "unusable tool input; journaling an empty object");
            v.to_string()
        }
    }
}

/// The tool policy the loop applies to whatever a stream assembled.
///
/// An unknown tool name is not merely a call that will fail. The block is
/// replayed onto the wire on every later request of the turn, and a strict
/// endpoint (ollama, through litellm) rejects the whole request with "tool
/// '…' not found" — so one hallucinated name, or one name a gateway mangled,
/// ends the session permanently. The name is therefore resolved here or the
/// block does not become a tool call at all.
fn finalize_tool_calls(message: Message, known: &[String]) -> Message {
    let content = message
        .content
        .into_iter()
        .map(|b| match b {
            ContentBlock::ToolUse { id, name, input } => match resolve_tool_name(&name, known) {
                Some(resolved) => {
                    if resolved != name {
                        tracing::warn!(got = %name, using = %resolved, "recovered a mangled tool name");
                    }
                    ContentBlock::ToolUse {
                        id,
                        name: resolved,
                        input,
                    }
                }
                None => {
                    tracing::warn!(got = %truncate(&name, 80), "unresolvable tool name; keeping it as text rather than a tool call");
                    ContentBlock::Text {
                        text: format!(
                            "[dropped a call to the unknown tool `{}`; the available tools are: {}]",
                            truncate(&name, 80),
                            known.join(", ")
                        ),
                    }
                }
            },
            other => other,
        })
        .collect();
    Message::assistant(content)
}

/// Map a streamed tool name onto one that is actually registered.
///
/// Exact match first. Failing that, a gateway that mis-parses a chat
/// template hands over a name with the real one buried in it
/// (`…(563 bytes)[TOOL_CALLS]read`), and a model that is guessing produces a
/// near miss (`read_file`); both are recoverable when exactly one registered
/// name is implicated. Anything ambiguous returns `None` — inventing a tool
/// call the model did not make is worse than dropping one it did.
fn resolve_tool_name(name: &str, known: &[String]) -> Option<String> {
    if known.iter().any(|k| k == name) {
        return Some(name.to_string());
    }
    let mut hits: Vec<&String> = known.iter().filter(|k| name.contains(k.as_str())).collect();
    if hits.len() > 1 {
        // `read` inside `read_file` is a containment, not an ambiguity: when
        // one candidate contains the others, it is the specific one.
        hits.sort_by_key(|k| std::cmp::Reverse(k.len()));
        if hits[1..].iter().all(|k| hits[0].contains(k.as_str())) {
            hits.truncate(1);
        }
    }
    if let [only] = hits[..] {
        return Some(only.clone());
    }
    None
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    format!("{}…", s.chars().take(n).collect::<String>())
}

/// Longest a single pin may contribute, in characters.
///
/// A pin is for a spec, a checklist, a set of conventions — things a turn
/// should be reminded of. A cap keeps a mistakenly pinned build artefact
/// from silently eating the window, and the truncation announces itself so
/// the model does not reason from a file it was handed half of without
/// being told.
const MAX_PIN: usize = 32_000;

/// The pinned files, read fresh, as one block for the prompt.
///
/// A pin that cannot be read says so rather than vanishing. Silence would
/// leave the operator believing the model has the file and the model
/// reasoning without it, which is the worst of both — and a path that has
/// gone away is exactly the case where the operator needs telling.
fn read_pins(paths: &[String]) -> Option<String> {
    if paths.is_empty() {
        return None;
    }
    let mut out = String::from(
        "<pinned>\nThe operator has pinned these files into every turn. They are re-read each turn, so this is their current content.\n",
    );
    for path in paths {
        out.push_str(&format!("\n<file path=\"{path}\">\n"));
        match std::fs::read_to_string(path) {
            Ok(text) if text.chars().count() > MAX_PIN => {
                let kept: String = text.chars().take(MAX_PIN).collect();
                out.push_str(&kept);
                out.push_str(&format!("\n[…truncated at {MAX_PIN} characters; the file is longer than a pin may carry]\n"));
            }
            Ok(text) => {
                out.push_str(&text);
                if !text.ends_with('\n') {
                    out.push('\n');
                }
            }
            Err(e) => out.push_str(&format!("[this pinned file could not be read: {e}]\n")),
        }
        out.push_str("</file>\n");
    }
    out.push_str("</pinned>");
    Some(out)
}

/// A block's opening, on one line, for a label.
fn first_line(s: &str) -> String {
    let line = s
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    ellipsize(line, 60)
}

fn ellipsize(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    format!(
        "{}…",
        s.chars().take(max.saturating_sub(1)).collect::<String>()
    )
}

/// What one message is, and what it costs in characters.
///
/// A message is rarely just prose: it can be thinking that has to be
/// replayed verbatim, a run of tool calls, the results of one, or a
/// screenshot whose base64 is the largest thing on the branch by an order
/// of magnitude. Reporting only `text()` would say a turn cost nothing
/// while it was in fact carrying a megabyte of image.
/// Account from assembled replay, so excluded/compacted/off-branch calls cannot
/// creep back in through a second walk of the journal.
fn context_sources(blocks: &[ContextBlock], messages: &[Message], guidance: usize) -> (Vec<ContextBlock>, Vec<ContextTool>) {
    let mut sizes = std::collections::BTreeMap::<&'static str, usize>::new();
    for b in blocks.iter().filter(|b| !matches!(b.kind, "user" | "assistant")) {
        *sizes.entry(b.kind).or_default() += b.chars;
    }
    let system = sizes.entry("system").or_default();
    let guidance = guidance.min(*system);
    *system -= guidance;
    *sizes.entry("tool guidance").or_default() += guidance;
    let mut tools: Vec<ContextTool> = Vec::new();
    let mut calls = std::collections::HashMap::new();
    for m in messages {
        for b in &m.content {
            if let ContentBlock::ToolUse { id, name, input, .. } = b {
                calls.insert(id.clone(), tools.len());
                tools.push(ContextTool { id: id.clone(), name: name.clone(), input_chars: name.chars().count() + input.0.chars().count(),
                    result_chars: 0, is_error: false });
            }
        }
    }
    for m in messages {
        for b in &m.content {
            let (kind, chars) = match b {
                ContentBlock::Text { text } => (if m.role == Role::User { "user / frames" } else { "assistant prose" }, text.chars().count()),
                ContentBlock::Thinking { thinking, signature } => ("thinking", thinking.chars().count() + signature.chars().count()),
                ContentBlock::RedactedThinking { data } => ("thinking", data.chars().count()),
                ContentBlock::ToolUse { name, input, .. } => ("tool arguments", name.chars().count() + input.0.chars().count()),
                ContentBlock::ToolResult { tool_use_id, content, is_error } => {
                    let chars = content.chars().count();
                    let i = *calls.entry(tool_use_id.clone()).or_insert_with(|| {
                        tools.push(ContextTool { id: tool_use_id.clone(), name: "unmatched result".into(), input_chars: 0,
                            result_chars: 0, is_error: false });
                        tools.len() - 1
                    });
                    tools[i].result_chars += chars;
                    tools[i].is_error |= is_error;
                    ("tool results", chars)
                }
                ContentBlock::Image { source, .. } => ("image payload", match source {
                    ImageSource::Base64 { data, media_type } => data.chars().count() + media_type.chars().count(),
                    ImageSource::Url { url } => url.chars().count(),
                }),
            };
            *sizes.entry(kind).or_default() += chars;
        }
    }
    let mut sources: Vec<_> = sizes.into_iter().filter(|(_, chars)| *chars > 0)
        .map(|(kind, chars)| ContextBlock { kind, label: kind.into(), chars }).collect();
    sources.sort_by(|a, b| b.chars.cmp(&a.chars).then(a.kind.cmp(b.kind)));
    tools.sort_by(|a, b| (b.input_chars + b.result_chars).cmp(&(a.input_chars + a.result_chars)).then(a.id.cmp(&b.id)));
    (sources, tools)
}

fn describe(m: &Message) -> (String, usize) {
    let mut chars = 0;
    let mut prose = String::new();
    let mut tools: Vec<&str> = Vec::new();
    let mut results = 0;
    let mut images = 0;
    let mut thinking = 0;
    for b in &m.content {
        match b {
            ContentBlock::Text { text } => {
                chars += text.chars().count();
                if prose.is_empty() {
                    prose = first_line(text);
                }
            }
            ContentBlock::Thinking {
                thinking: t,
                signature,
            } => {
                chars += t.chars().count() + signature.chars().count();
                thinking += t.chars().count();
            }
            ContentBlock::RedactedThinking { data } => chars += data.chars().count(),
            ContentBlock::ToolUse { name, input, .. } => {
                chars += name.chars().count() + input.0.chars().count();
                tools.push(name);
            }
            ContentBlock::ToolResult { content, .. } => {
                chars += content.chars().count();
                results += 1;
            }
            ContentBlock::Image { source, alt } => {
                // A URL image costs the URL; base64 costs the bytes, which
                // is usually the largest single thing on the branch.
                chars += match source {
                    ImageSource::Base64 { data, media_type } => {
                        data.chars().count() + media_type.chars().count()
                    }
                    ImageSource::Url { url } => url.chars().count(),
                };
                images += 1;
                if prose.is_empty()
                    && let Some(a) = alt.as_deref().filter(|a| !a.trim().is_empty())
                {
                    prose = ellipsize(a, 60);
                }
            }
        }
    }
    let mut parts = Vec::new();
    if !prose.is_empty() {
        parts.push(prose);
    }
    if thinking > 0 {
        parts.push(format!("thinking ({thinking})"));
    }
    if !tools.is_empty() {
        parts.push(format!("calls {}", tools.join(", ")));
    }
    if results > 0 {
        parts.push(format!(
            "{results} tool result{}",
            if results == 1 { "" } else { "s" }
        ));
    }
    if images > 0 {
        parts.push(format!(
            "{images} image{}",
            if images == 1 { "" } else { "s" }
        ));
    }
    (
        if parts.is_empty() {
            "(empty)".to_string()
        } else {
            parts.join(" · ")
        },
        chars,
    )
}

/// Whether an assistant message is *only* the name of one of the tools it
/// was offered — the shape a model takes when it reaches for a tool and
/// writes its name instead of calling it.
///
/// An equality test on the whole reply, not a search within it. "You could
/// use `peers` for that" is an answer; `peers` alone is not one, and the
/// difference is the entire justification for asking again. Backticks come
/// off because a model that types a tool name often types it as code, and
/// the message must carry no tool calls of its own — a reply that both
/// called something and named something else is not this.
fn names_only_a_tool(message: &Message, tools: &[String]) -> bool {
    if tools.is_empty() || message.tool_uses().next().is_some() {
        return false;
    }
    let text = message.text();
    let name = text.trim().trim_matches(&['`', '"', '\'', '.'][..]).trim();
    !name.is_empty() && tools.iter().any(|t| t == name)
}

#[cfg(test)]
mod assembler_tests {
    use super::*;
    use crate::message::Json;
    use crate::provider::StreamEvent;
    use crate::testing::ScriptedProvider;

    #[test]
    fn context_sources_partition_replay_and_pair_results_by_call_id() {
        let messages = vec![
            Message::assistant(vec![
                ContentBlock::text("αβ"),
                ContentBlock::Thinking { thinking: "think".into(), signature: "sig".into() },
                ContentBlock::ToolUse { id: "a".into(), name: "read".into(), input: Json("{}".into()) },
                ContentBlock::ToolUse { id: "b".into(), name: "bash".into(), input: Json("{}".into()) },
            ]),
            Message::user(vec![
                ContentBlock::ToolResult { tool_use_id: "b".into(), content: "bad".into(), is_error: true },
                ContentBlock::ToolResult { tool_use_id: "a".into(), content: "large".repeat(20), is_error: false },
                ContentBlock::text("follow up"),
                ContentBlock::image("image/png", "AAAA", None),
            ]),
        ];
        let mut blocks = vec![ContextBlock { kind: "system", label: "".into(), chars: 30 }];
        blocks.extend(messages.iter().map(|m| ContextBlock { kind: if m.role == Role::User { "user" } else { "assistant" },
            label: String::new(), chars: describe(m).1 }));
        let (sources, tools) = context_sources(&blocks, &messages, 10);
        assert_eq!(sources.iter().map(|b| b.chars).sum::<usize>(), blocks.iter().map(|b| b.chars).sum::<usize>());
        assert_eq!(sources.iter().find(|b| b.kind == "system").unwrap().chars, 20);
        assert_eq!(sources.iter().find(|b| b.kind == "thinking").unwrap().chars, 8);
        assert_eq!(sources.iter().find(|b| b.kind == "assistant prose").unwrap().chars, 2, "characters, not UTF-8 bytes");
        assert_eq!(tools[0].id, "a", "largest first, independent of result arrival order");
        assert_eq!(tools[0].result_chars, 100);
        assert_eq!(tools[1].result_chars, 3);
        assert!(tools[1].is_error);
        assert_eq!(tools.iter().map(|t| t.input_chars + t.result_chars).sum::<usize>(),
            sources.iter().filter(|s| matches!(s.kind, "tool arguments" | "tool results")).map(|s| s.chars).sum::<usize>());
        assert!(context_sources(&[], &[], 0).0.is_empty());
    }

    fn known() -> Vec<String> {
        ["read", "write", "edit", "bash", "grep"]
            .into_iter()
            .map(String::from)
            .collect()
    }

    /// The policy half, on a message as the shared consumer settles one: the
    /// name resolved against the registry, or the block turned back into
    /// prose. The assembly and the repair are upstream's, and tested there;
    /// what is asserted here is what this crate decides.
    fn tool_block(name: &str) -> Message {
        finalize_tool_calls(
            Message::assistant(vec![ContentBlock::ToolUse {
                id: "id1".into(),
                name: name.into(),
                input: Json("{\"path\":\"a\"}".into()),
            }]),
            &known(),
        )
    }

    #[test]
    fn a_known_name_passes_through() {
        let m = tool_block("read");
        assert!(matches!(&m.content[0], ContentBlock::ToolUse { name, .. } if name == "read"));
    }

    #[test]
    fn a_name_mangled_by_the_gateway_is_recovered() {
        // Verbatim from litellm: a tool result's text ran into the next
        // call's template, and the whole thing arrived as the name.
        let m = tool_block("created /tmp/x/index.html (563 bytes)[TOOL_CALLS]read");
        assert!(
            matches!(&m.content[0], ContentBlock::ToolUse { name, .. } if name == "read"),
            "{:?}",
            m.content
        );
    }

    #[test]
    fn a_near_miss_resolves_to_the_specific_tool() {
        let m = tool_block("read_file");
        assert!(matches!(&m.content[0], ContentBlock::ToolUse { name, .. } if name == "read"));
    }

    #[test]
    fn an_ambiguous_name_is_not_guessed_at() {
        let m = tool_block("read_then_write");
        match &m.content[0] {
            ContentBlock::Text { text } => assert!(text.contains("unknown tool"), "{text}"),
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_name_never_becomes_a_tool_use() {
        let m = tool_block("frobnicate");
        assert!(
            !m.content
                .iter()
                .any(|b| matches!(b, ContentBlock::ToolUse { .. }))
        );
        // The model is still told what happened and what it could have used.
        match &m.content[0] {
            ContentBlock::Text { text } => assert!(
                text.contains("frobnicate") && text.contains("grep"),
                "{text}"
            ),
            other => panic!("expected text, got {other:?}"),
        }
    }

    #[test]
    fn tool_input_is_always_journaled_as_parseable_json() {
        // Two calls collapsed onto one block by the gateway.
        let input = journal_safe_tool_input("write", "{\"path\":\"a\",\"content\":\"x\"}{\"path\":\"b\"}");
        let v: serde_json::Value =
            serde_json::from_str(&input).expect("must round-trip onto the wire");
        assert_eq!(v["path"], "a");
    }

    #[test]
    fn file_content_survives_the_repair_path_byte_for_byte() {
        // The adversarial case for both new text paths: content that itself
        // contains the tool-call template and JSON punctuation.
        let content = "function f() {\n  // write[ARGS]{\"x\": 1}\n  return \"}{\";\n}\n";
        let args = serde_json::json!({ "path": "a.js", "content": content }).to_string();
        let input = journal_safe_tool_input("write", &args);
        let v: serde_json::Value = serde_json::from_str(&input).unwrap();
        assert_eq!(v["content"].as_str().unwrap(), content);
    }

    #[test]
    fn unparseable_input_still_journals_an_object() {
        assert_eq!(journal_safe_tool_input("write", "not json at all"), "{}");
    }

    /// The wiring, end to end: a streamed tool call through the shared
    /// consumer with this crate's sink arrives repaired and resolved, and
    /// the deltas reached the bus on the way past. Without this the two
    /// halves above could each be right while nothing joined them.
    #[tokio::test]
    async fn a_streamed_tool_call_is_repaired_and_resolved_as_it_arrives() {
        let provider = ScriptedProvider::new(vec![vec![
            StreamEvent::ToolUseStart {
                id: "id1".into(),
                name: "read_file".into(),
            },
            StreamEvent::ToolInputDelta("{\"path\":\"a\"}{\"path\":\"b\"}".into()),
            StreamEvent::BlockStop,
        ]]);
        let bus = EventBus::new(16);
        let mut rx = bus.subscribe();
        let mut sink = BusSink { bus: &bus };
        let c = harnox::llm::stream_with_retries(
            &*provider,
            ChatRequest {
                model: "m".into(),
                max_tokens: 8,
                ..Default::default()
            },
            &CancellationToken::new(),
            &harnox::llm::RetryPolicy::default(),
            &mut sink,
        )
        .await
        .unwrap();
        let m = finalize_tool_calls(c.message, &known());
        match &m.content[0] {
            ContentBlock::ToolUse { name, input, .. } => {
                assert_eq!(name, "read", "a near miss is resolved to the registered tool");
                let v: serde_json::Value = serde_json::from_str(&input.0).unwrap();
                assert_eq!(v["path"], "a", "the first of two collapsed objects is kept");
            }
            other => panic!("expected a tool use, got {other:?}"),
        }
        let mut seen = Vec::new();
        while let Ok(e) = rx.try_recv() {
            seen.push(format!("{e:?}"));
        }
        assert!(
            seen.iter().any(|e| e.contains("ToolUseStart"))
                && seen.iter().any(|e| e.contains("ToolInputDelta")),
            "the deltas were published as they arrived: {seen:?}"
        );
    }
}
