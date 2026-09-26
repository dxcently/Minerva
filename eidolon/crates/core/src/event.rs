//! Structured events the core publishes. A consumer (TUI, CLI, Melete)
//! subscribes and draws; it never has to infer state from what it drew.
//!
//! The bus is a `tokio::sync::broadcast` channel: many subscribers, lagging
//! ones drop the oldest events rather than stalling the loop. A renderer
//! that falls behind loses deltas, never correctness, because every durable
//! fact (messages, results, turn boundaries) is also in the session log.

use tokio::sync::broadcast;

use crate::message::{Message, StopReason, Usage};
use crate::session::RecordId;
use crate::tool::{ToolCall, ToolOutput};
use crate::usage::TurnTiming;

#[derive(Clone, Debug)]
pub enum Event {
    /// A model call has started streaming.
    MessageStart,
    TextDelta(String),
    ThinkingDelta(String),
    /// The model began a tool-use block; input follows as deltas.
    ToolUseStart {
        id: String,
        name: String,
    },
    ToolInputDelta {
        id: String,
        partial_json: String,
    },
    /// A complete assistant message has been assembled and journaled.
    ///
    /// `record` is where it landed on the branch. A consumer drawing a
    /// transcript needs it to say *which* record a line on screen came
    /// from — the answer to "strike this from the context" is a
    /// `RecordId`, and the id exists here for free because `append`
    /// returned it a line earlier. Reconstructing it later means matching
    /// a rendering against a log, which is the guessing this avoids.
    AssistantMessage {
        record: RecordId,
        message: Message,
    },
    /// The operator's own message, journaled at the top of a turn.
    ///
    /// A consumer draws this one itself, from what was typed, rather than
    /// waiting for it to come back around — so this exists only to say
    /// where it landed.
    UserMessage {
        record: RecordId,
        message: Message,
    },
    /// The dispatcher accepted a call and is about to evaluate policy.
    ToolCallStarted(ToolCall),
    /// The dispatcher finished a call (allowed, denied, or failed — all three
    /// produce an output).
    ///
    /// `record` is the journaled `ToolResult`, when there is one: the
    /// third dispatch rung (`Dispatcher::execute`, for a backend whose own
    /// stream is the journal) runs a call without journaling its result,
    /// and `None` says so rather than naming a record that is not there.
    ToolCallFinished {
        record: Option<RecordId>,
        call: ToolCall,
        output: ToolOutput,
    },
    /// A question is being put to the user: the policy gate's `Ask`
    /// verdict going to `UserIo::approve`.
    AskUser {
        prompt: String,
    },
    /// The gate answered something other than a silent yes, and the record
    /// of it is on the branch.
    ///
    /// Published after the record, like everything else. The one that has to
    /// reach a consumer is [`crate::session::PolicyOutcome::Judged`]: a call
    /// the context-aware layer waved through runs with no question and no
    /// error, so without this it would be indistinguishable on screen from a
    /// call the table allowed outright — a model quietly answering for the
    /// operator, which is exactly the thing that must not be quiet.
    PolicyVerdict {
        tool: String,
        reason: String,
        outcome: crate::session::PolicyOutcome,
        note: Option<String>,
    },
    /// How full the context is, published just before [`Event::TurnSettled`].
    ///
    /// Published for a turn that ends without a settle too — one the
    /// operator stopped ([`Event::Cancelled`]), and one that hit its
    /// iteration limit (which publishes only [`Event::Error`] to say so):
    /// the calls those turns had already made measured the context as
    /// surely as a finished turn's last call did, and a gauge that only
    /// moved on a settle sat on the previous turn's number for as long as
    /// turns kept ending that way.
    ///
    /// Separate from the settle because the settle's `usage` is the
    /// *turn's* total across every model call it made, and this is the
    /// last call's input alone — see
    /// [`crate::session::RecordKind::ContextSize`]. A consumer drawing a
    /// gauge wants this one; a consumer counting what the session cost
    /// wants the other.
    ContextSize {
        tokens: u64,
    },
    /// The turn is over: a response arrived with no tool calls pending.
    /// This is a published fact, journaled as a record, not an inference.
    ///
    /// A compaction's summariser call publishes one too, beside the
    /// [`Compacted`](Event::Compacted) it belongs to: it is a response with
    /// no tool calls pending, it is journaled as the same record, and a
    /// consumer that priced a session from the bus but not from the log
    /// would read a compacted one as free. `timing` is `None` there — no
    /// `TurnPace` record is written for it.
    ///
    /// `timing` is the part of this that is *ours* rather than the
    /// endpoint's: how long the turn's calls took, measured around each
    /// request, since no wire the harness speaks reports durations. It
    /// travels with the settle *and* is written beside it
    /// ([`crate::session::RecordKind::TurnPace`]), so a resume reads the
    /// same numbers; `None` is a turn nobody timed — a driver that owns its
    /// own loop (the Claude CLI), a turn that never reached a model, or a
    /// log written before the pace was journaled.
    TurnSettled {
        stop_reason: StopReason,
        usage: Usage,
        timing: Option<TurnTiming>,
    },
    /// The branch was summarised; the provider-facing history restarts.
    Compacted {
        summary: String,
        replaced_messages: u32,
    },
    /// A message from another harness session — or, with `external`, from
    /// an outside caller through `eidolon send` — has been journaled onto
    /// the branch. Published after the record, like everything else — at a
    /// turn boundary, and mid-turn at the same safe points as a steer.
    PeerMessage {
        record: RecordId,
        from: String,
        from_cwd: String,
        channel: Option<String>,
        text: String,
        /// Whether the sender is outside any session; the TUI ignores it
        /// (the sender's name says enough to draw), a line-oriented
        /// consumer says which of the two kinds it was.
        external: bool,
    },
    /// The wrap-up nudge was journaled: the harness told the model the
    /// turn was near its model-call limit, with `calls_left` remaining.
    /// Published after the record, at the top of a loop iteration, so a
    /// consumer drawing it interrupts nothing that streams.
    TurnBudget {
        record: RecordId,
        calls_left: u32,
    },
    /// The harness answered commands the model wrote in its own prose —
    /// the inline command channel — and the turn is going round once more
    /// so the model can read them.
    ///
    /// Published after the record, like everything else, and at the point
    /// the loop would otherwise have settled: a consumer drawing the
    /// transcript has an entry to make between the reply that asked and
    /// the continuation that answers, which is where the lines happened.
    /// `lines` is what the model is given, one entry per command, in the
    /// order it wrote them — the same text the frame in
    /// [`crate::session::command_frame`] carries.
    CommandResults {
        record: RecordId,
        lines: Vec<String>,
    },
    /// The current turn was cancelled locally.
    /// The turn was interrupted, and what it had spent getting there —
    /// which a consumer's own ledger needs, since no `TurnSettled` is
    /// coming to carry it. Journaled too, as
    /// [`crate::session::RecordKind::TurnSpend`], so a resumed session
    /// reads the same. A [`Event::ContextSize`] arrives first whenever one
    /// of the stopped turn's calls reported an input, and is journaled as
    /// the same record a settle writes.
    Cancelled {
        usage: Usage,
        calls: u32,
    },
    Error(String),
    /// The follow-up queue changed length: a message was queued for after
    /// the turn, or one was taken to run. `waiting` is what is left.
    ///
    /// The message itself is not here. When it runs it is journaled and
    /// published as [`Event::UserMessage`] like any other, and until then
    /// it is the consumer's own words, which the consumer already has.
    Queued {
        waiting: usize,
    },
    /// A quiesce reached its boundary: the marker naming the projection is
    /// on the branch and the journal stops here. Published after the record,
    /// at the boundary the settled turn just made — see [`crate::quiesce`].
    ///
    /// A consumer draws its goodbye from this: the destination is where the
    /// session went, and the record is what `eidolon log` will show. It is
    /// deliberately not an `Error` and not a `TurnSettled`: nothing went
    /// wrong, and the turn that settled said so itself.
    Quiesced {
        record: RecordId,
        destination: String,
    },
    /// A park this session armed has resolved, and the record is on the
    /// branch. Published after the record, at a boundary — or at the
    /// mid-turn safe point a waking peer's note uses.
    TriggerFired {
        record: RecordId,
        condition: String,
        outcome: String,
        /// The `wait_for` call that armed the park: what lets a live view
        /// settle the checklist block that call drew, beside the journal
        /// the record already names. Replay needs none of this — the
        /// record is what a resumed session reads.
        call_id: String,
    },
}

/// Handle for publishing and subscribing.
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Event>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(1024)
    }
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        EventBus { tx }
    }

    /// Publish. A bus with no subscribers is fine — headless runs have none.
    pub fn publish(&self, e: Event) {
        let _ = self.tx.send(e);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}
