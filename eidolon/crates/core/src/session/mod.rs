//! Sessions: the append-only binary log and the conversation branch
//! reconstructed from it.
//!
//! See [`log`] for the on-disk format. This module is the layer above it:
//! it knows which records are conversation-shaped and turns a branch of the
//! record tree back into the `Vec<Message>` the provider is sent.
//!
//! ## Records and the tree
//!
//! Every record has an id (its sequence number in the file) and a parent id.
//! A normal append's parent is the current head. A **fork** is nothing more
//! than moving the head to an older record and appending: the next record's
//! parent points into the past and two leaves now share an ancestor. The
//! tree is reconstructed on load by walking parent pointers, exactly like a
//! commit graph. No per-branch files, no database.
//!
//! ## What is journaled
//!
//! - every user and assistant message, in full (thinking blocks and their
//!   signatures included — the API needs them replayed);
//! - every tool result, keyed by the `tool_use` id it answers;
//! - every `TurnSettled`, so resume can land on a clean boundary;
//! - every answered `choices_user`, so a replayed script gets the same
//!   answer without re-prompting;
//! - every message from another harness session, journaled at the turn
//!   boundary it was folded in at, so a resumed swarm reads the same way;
//! - cancellations, so a resume knows the last turn did not settle;
//! - compactions, so the provider-facing history restarts at the summary
//!   while the log keeps everything.
//!
//! ## Export
//!
//! `eidolon log --json` writes each record as one JSON line — the record's
//! own serde form, externally tagged — for a reader that cannot decode
//! bitcode. There is no second file on disk; the journal is the only truth
//! and the export is a door opened on demand ([`trace_line`]).

pub mod log;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::message::{ContentBlock, Json, Message, Role, StopReason, Usage};
use crate::usage::TurnTiming;
pub use log::{RecordId, Refusal, SessionLog};

/// The typed payload of one record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, bitcode::Encode, bitcode::Decode)]
pub enum RecordKind {
    /// First record of every session.
    SessionStart {
        model: String,
        cwd: String,
        system: Option<String>,
    },
    UserMessage(Message),
    AssistantMessage(Message),
    /// Result for one `tool_use` id. Journaled as the dispatcher returns it,
    /// before it is folded into the next user message.
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
    TurnSettled {
        stop_reason: StopReason,
        usage: Usage,
    },
    /// A question asked of the user and the answer given. Keyed by the call
    /// id so replay can match it.
    AskUser {
        call_id: String,
        prompt: String,
        answer: Option<String>,
    },
    Cancelled,
    ModelChanged {
        model: String,
    },
    /// The conversation up to this point was summarised; the provider-facing
    /// message list restarts here with `summary` as the opening user
    /// message. Everything before it stays in the log — a fork below the
    /// compaction point sees the full history again.
    Compacted {
        summary: String,
        replaced_messages: u32,
    },
    /// A backend that keeps its own session (the Claude CLI) and the id to
    /// resume it with. The latest one on the branch wins.
    ///
    /// Its position is also a high-water mark: the record is written when
    /// that session is known to hold the branch so far, so a later turn on
    /// the same backend only has to replay what comes *after* it. That is
    /// how a branch whose earlier turns ran on some other backend still
    /// reaches this one — see `Session::backend_session_mark`.
    ///
    /// **An empty `id` names no session**, and is how the mark is dropped
    /// (`Session::clear_backend_session`): the latest record for a backend
    /// still wins, and what this one says is that the backend holds
    /// nothing, so the next turn there starts fresh and is told the whole
    /// branch. It is written this way rather than as a variant of its own
    /// because the record already means "the latest one wins" and naming
    /// no session is a thing it can already say — and because a journaled
    /// enum grows only at the end, so a variant is a scarce, coordinated
    /// thing to spend.
    BackendSession {
        backend: String,
        id: String,
    },
    /// A tool call the **operator** made — a dispatch, not the model's
    /// doing — journaled just before it runs so that the `ToolResult`
    /// following it has something to pair with.
    ///
    /// Without it a user-originated call left an orphan result on the
    /// branch: nothing in [`Session::messages_after`] claimed it, so the
    /// model never learned what the operator had just done, and nothing in
    /// a consumer's replay claimed it either, so a resumed session lost the
    /// call entirely. `utterance` is what the operator actually typed, when
    /// there was a form of it worth keeping — the classifier's reading of a
    /// sentence is not always the sentence, and both are worth having.
    UserToolCall {
        tool_use_id: String,
        name: String,
        input: Json,
        utterance: Option<String>,
    },
    /// Free-form marker a consumer may write (a bookmark, a label).
    Note {
        text: String,
    },
    /// A message from **another harness session** — a peer working on the
    /// same project, often in another worktree.
    ///
    /// Its own kind for the same reason [`RecordKind::UserToolCall`] is:
    /// this is neither the operator speaking nor the model, and folding it
    /// into a `UserMessage` would tell both the model and every consumer
    /// replaying the branch that the person at the keyboard said it. What
    /// the model is told is [`crate::peer::frame`]'s wording, built in
    /// [`Session::messages_after`] from these fields.
    ///
    /// Written at turn boundaries, and mid-turn at the same safe points as
    /// a steer — after the previous reply's tool results are on the branch,
    /// never between an assistant's `tool_use` and its results. A
    /// user-shaped record in that gap would reach `flush` with those
    /// results outstanding and turn every one of them into a synthetic
    /// interruption. See [`crate::peer`].
    PeerMessage {
        from: String,
        from_cwd: String,
        channel: Option<String>,
        text: String,
    },
    /// How big the context actually was when the turn ended: the input of
    /// the **last** model call of the turn, which is the only one that
    /// saw the whole transcript.
    ///
    /// Written for every way a turn can end without a settle, not only the
    /// settled one: the last call of a turn the operator stopped, and the
    /// last call of a turn that hit `max_iterations`, measured the context
    /// as surely as the last call of a finished one. A turn that ends
    /// without writing this leaves every consumer drawing the previous
    /// turn's number, which is how a gauge goes on saying `ctx 3.7k` (a
    /// compaction's summary) through an hour of work. Both of those
    /// endings write the most recent input any of their calls reported,
    /// and write nothing when none did.
    ///
    /// Its own record because [`RecordKind::TurnSettled`]'s `usage` is a
    /// *turn total* and cannot be made to answer this. The agent loop
    /// sums every model call it makes — a turn that ran forty tools made
    /// forty calls, each resending a transcript slightly longer than the
    /// last — so `total_input` there is forty transcripts' worth. That is
    /// exactly right for what the turn cost and exactly wrong for how
    /// full the context is, and no arithmetic recovers the second from
    /// the first.
    ///
    /// It was wrong in both directions at once before this existed:
    /// [`Session::last_input_tokens`] read the sum, so the context gauge
    /// tripped on a number that grew with the *length of the turn* rather
    /// than the size of the context — one long tool loop and every
    /// subsequent turn drew a context that was nowhere near full — and the
    /// TUI's gauge drew the same figure, reaching 1292% of a
    /// million-token window on a session that had used a fraction of it.
    ///
    /// A compaction is the one turn where the context *after* the turn is
    /// not the last call's input: the summariser read the whole history,
    /// but what the next turn reads is only the summary. There the record
    /// carries the summariser's output instead — the measured size of the
    /// restarted context — and is written after the [`RecordKind::Compacted`]
    /// record it describes.
    ///
    /// **Appended, as a journaled enum must be** (see the invariant in
    /// `AGENTS.md`): bitcode encodes a variant by its ordinal, so this
    /// had to go at the end and nowhere else. A log written before it
    /// exists simply has none, which reads as *unknown* rather than as
    /// zero — and unknown is the honest answer, since the number was
    /// never recorded and cannot be recomputed without re-tokenising the
    /// branch. Unknown is also what the TUI draws as a dash until a later
    /// turn writes a real measurement.
    ContextSize {
        tokens: u64,
    },
    /// What the gate decided about one call, and what became of the
    /// question — **written only when the answer was not a silent yes.**
    ///
    /// The three things worth remembering are the ones nothing else in the
    /// log holds. A refusal is already here as the tool error it produced,
    /// but the error says "denied by policy" and not *which rule*; an
    /// approval leaves no trace at all beyond a call that happened to run;
    /// and a call the context-aware layer waved through leaves a trace
    /// indistinguishable from one the table allowed outright, which is the
    /// one thing an operator most needs to be able to tell apart.
    ///
    /// **`reason` is the classifier's own, not the sentence built around
    /// it.** It comes from a small fixed vocabulary and is the unit a table
    /// edit flips, which is what makes it the unit a ledger can group by:
    /// "this reason fired forty times and you said yes to all forty" is an
    /// argument for editing one line of `policy.rn`, and the prompt text
    /// wrapped around it is not.
    ///
    /// Silent allows are deliberately absent. Recording them would double
    /// the length of the log to say that nothing happened, and every
    /// question this record exists to answer is a question about the calls
    /// that were *not* waved through. The cost is that the denominator — how
    /// often a rule was consulted at all — is not recoverable from the log,
    /// which is a real limit on any promotion loop built over it and a
    /// better trade than a log made mostly of the word "allow".
    ///
    /// **Replay draws it and changes nothing.** It is not a message, so
    /// [`Session::messages`] never sees it and the model is not told the
    /// gate's reasoning; a consumer redraws it on resume exactly as it drew
    /// it live, the way [`crate::usage::stop_note`] already works. That is
    /// the whole of its replay answer, and it is why the record could be
    /// added without reserving anything else.
    ///
    /// **Appended, as a journaled enum must be** — see the invariant in
    /// `AGENTS.md`. The order was agreed with the concurrent session
    /// working on context controls before either of us wrote one.
    PolicyVerdict {
        tool_use_id: String,
        /// As called, MCP qualification and all — `mcp__eidolon__bash`
        /// rather than `bash` — because what a ledger wants to know is
        /// which surface asked.
        tool: String,
        reason: String,
        /// The gate answered for want of understanding rather than on the
        /// merits. A `structural` question is one no table edit can silence,
        /// so a ledger must not offer it as a promotion candidate.
        structural: bool,
        outcome: PolicyOutcome,
        /// The judge's one-line reasoning, on [`PolicyOutcome::Judged`].
        /// Nothing carries one on [`PolicyOutcome::Yolo`]: there was no
        /// reasoning, which is the point of it.
        note: Option<String>,
    },
    /// A record the operator has struck from the replay, or restored.
    ///
    /// The row stays on disk and on screen; what changes is whether
    /// [`Session::messages_after`] hands it to the model. That is the whole
    /// of the feature — a failed experiment, a four-thousand-line build
    /// log, a tool call that answered the wrong question, all of which are
    /// worth keeping and not worth re-sending on every turn of a tool loop.
    ///
    /// Latest-wins per `target`, like [`RecordKind::ModelChanged`], so
    /// striking and restoring is a toggle and not a growing pile of
    /// contradictions. Journaled because a resume has to reproduce it: a
    /// session reopened tomorrow must send what it sent today.
    ///
    /// **Appended at the end, as a journaled enum must be**, immediately
    /// after `PolicyVerdict` — an order agreed out loud with the two other
    /// sessions in this tree before any of us wrote it, because bitcode
    /// encodes a variant by its ordinal and three sessions each
    /// independently appending "at the end" is exactly the merge the
    /// invariant forbids.
    Excluded {
        target: RecordId,
        excluded: bool,
    },
    /// A standing note the operator has attached to this session, injected
    /// into every turn's system prompt. Latest on the branch wins; empty
    /// clears it.
    ///
    /// It lives here and not in `config.toml` because nothing in the
    /// harness writes that file — a session runs on what was there when it
    /// launched — and because a note is *about this conversation*, which is
    /// exactly what the branch is for. In the system prompt rather than
    /// beside the last message because it is **stable**: written once and
    /// then unchanged for fifty turns, which is what a cached prefix is
    /// for. Changing it is one deliberate prefix miss.
    SessionNote {
        text: String,
    },
    /// A file the operator has pinned into the prompt, re-read at the start
    /// of every turn. Latest word per path wins.
    ///
    /// **Only the declaration is journaled, never the contents.** A pin
    /// that wrote its bytes onto the user message would append another full
    /// copy of the file to the log every turn, and `Session::messages`
    /// would replay all of them — so the branch would grow by the pin's
    /// size per turn and the model would see the file once per turn it had
    /// been pinned. The contents are injected where the message list
    /// becomes a request and nowhere else, which is also what makes an edit
    /// to the file live rather than a stale paste.
    Pinned {
        path: String,
        pinned: bool,
    },
    /// The persona pinned to this branch — a vault note under
    /// [`crate::persona::PERSONA_FOLDER`] whose body prefixes every turn's
    /// system prompt. Latest on the branch wins; empty clears it.
    ///
    /// **Only the pin is journaled, never the note's body**, for
    /// [`RecordKind::Pinned`]'s reason and one more: a persona note is
    /// edited *by the persona*, so a body written into the log would be a
    /// snapshot of a character that has since moved on. The body is
    /// fetched fresh at the start of every turn and spliced where the
    /// message list becomes a request, which is what makes an edit to the
    /// note live on the very next turn.
    ///
    /// The string is whatever the operator pinned — a frontmatter `name`
    /// or a vault path — and not a resolved path, because resolution is
    /// the vault's answer and it may change: a persona that grows from a
    /// flat note into a folder must not strand the sessions that pinned
    /// it. See [`crate::persona::note_candidates`].
    ///
    /// It is journaled rather than held as a posture (unlike `Detail`,
    /// the trace cursor or the yolo switch) because it changes what the
    /// model was *sent*: a session reopened tomorrow has to reproduce the
    /// prompt it ran on today, and a resumed conversation coming back in
    /// somebody else's voice is the same class of error as one coming
    /// back on the wrong model.
    ///
    /// **Appended at the end, as a journaled enum must be** — see the
    /// invariant in `AGENTS.md`. The slot immediately after
    /// [`RecordKind::Pinned`] was agreed out loud with the other session
    /// working in this tree before either of us wrote a variant, because
    /// bitcode encodes a variant by its ordinal and two sessions each
    /// independently appending "at the end" is exactly the merge the
    /// invariant forbids.
    PersonaPinned {
        persona: String,
    },
    /// What a turn that **did not settle** had already spent: the usage
    /// summed over the model calls it made before it was cancelled or
    /// ran out of iterations, and how many that was.
    ///
    /// Its own record for the reason [`RecordKind::ContextSize`] is one:
    /// the fact belongs on a record that already exists, and no existing
    /// record can be made to carry it. [`RecordKind::TurnSettled`] is
    /// never written for a turn that did not settle, and
    /// [`RecordKind::Cancelled`] is a unit variant whose shape cannot
    /// change — bitcode encodes a variant by its ordinal *and* its
    /// payload, so giving `Cancelled` fields would misread every log that
    /// already has one. So the spend is a second record, written
    /// immediately before the `Cancelled` it explains, and every reader
    /// of `Cancelled` goes on reading it unchanged.
    ///
    /// Without it a cancelled turn cost the operator real money and told
    /// nobody. The agent loop had the figure — `total`, summed as it went
    /// — and dropped it on the way out: [`TurnOutcome::Cancelled`]
    /// carried nothing, and [`Session::usage`] sums settles, so an
    /// interrupted turn priced at zero both live and on resume.
    ///
    /// **It is a floor, and it has to be read as one.** The call that
    /// was actually interrupted contributes nothing on the
    /// chat-completions wire, whose one usage object is the final chunk,
    /// and contributes its input and none of its output on the Anthropic
    /// one — so the missing part is largest exactly where the cancel
    /// landed. What is here is the completed calls, measured; what is
    /// missing is the one that was cut. That is why the consumers draw
    /// it against `StopReason::Cancelled` rather than folding it in as a
    /// settle: a floor labelled a floor is worth having, and the same
    /// figure presented as a bill is the one thing the pricing here is
    /// careful never to be. Zero was a worse floor.
    ///
    /// **Appended, as a journaled enum must be** — see the invariant in
    /// `AGENTS.md`. It goes after [`RecordKind::PersonaPinned`], which
    /// is the last variant, and *not* after `ContextSize`, which merely
    /// carries the note explaining why *it* was last when it was
    /// written. That is the mistake this variant made on its first
    /// attempt, and it is worth leaving written down: placed at ordinal
    /// 14 of 20 it shifted the five variants after it, so a
    /// `PersonaPinned` written as 18 was read back as `Pinned { path,
    /// pinned }`, the bytes ran out, and a debug build could not open a
    /// single existing log. Exactly the failure the invariant describes
    /// — silent, delayed, and invisible to the build, which compiled and
    /// passed every test because the tests write their logs with the
    /// same binary that reads them. "Append at the end" means the end of
    /// the *list*; a doc comment saying "appended" is evidence about the
    /// variant it sits on and about nothing below it.
    ///
    /// The ordering is pinned by
    /// `every_record_kind_still_reads_as_it_was_written` in
    /// `crates/core/tests/pins.rs`, which decodes fixed bytes captured
    /// from a *committed* build — so a shift like the one above now
    /// fails in `cargo test` instead of on somebody's real log.
    ///
    /// A log written before it exists has none, which reads as
    /// *unknown*: what an older cancel spent was never recorded and
    /// cannot be recovered.
    TurnSpend {
        usage: Usage,
        calls: u32,
    },
    /// The harness told the model this turn was near its model-call
    /// limit, and how many calls it had left when it was told — the
    /// *wrap-up nudge*, fired once per turn when
    /// [`crate::agent::AgentConfig::max_iterations`] is a few calls
    /// away.
    ///
    /// Its own kind for [`RecordKind::PeerMessage`]'s reason: this is
    /// the harness speaking, neither the operator nor the model, and a
    /// `UserMessage` would tell both the model and every consumer
    /// replaying the branch that the person at the keyboard said it.
    /// What the model is told is [`budget_frame`]'s wording, rebuilt in
    /// [`Session::messages_after`] from `calls_left` — which is why the
    /// record carries the number and not the sentence.
    ///
    /// Journaled at the top of a loop iteration, the same safe point a
    /// steer is delivered at, and folded the same way: riding the user
    /// message that carries the previous reply's tool results rather
    /// than becoming a second user turn in a row.
    ///
    /// The nudge exists because exhaustion, when it came, was almost
    /// never a runaway loop: reading the logs on this machine, every
    /// turn that hit the limit was cut off mid-edit or mid-test-run by
    /// a model that makes one tool call per reply and so spends one
    /// iteration per tool. A wall the model can see coming is a tool —
    /// it finishes the step in flight and settles on a summary the
    /// operator can continue from, instead of the turn dying on an
    /// error with its work half-landed.
    ///
    /// **Appended, as a journaled enum must be** — after
    /// [`RecordKind::TurnSpend`], the last variant, with its ordinal in
    /// `crates/core/tests/pins.rs`.
    TurnBudget {
        calls_left: u32,
    },
    /// A message from **outside any session** — `eidolon send`, the door
    /// through which a script or an external tool (Aoide is the first
    /// expected one) injects text into a running session without being
    /// one itself.
    ///
    /// A sibling of [`RecordKind::PeerMessage`] with its own kind for the
    /// same reason: neither the operator nor the model said it, and what
    /// the model is told is [`crate::peer::external_frame`]'s wording —
    /// which says a different thing than a peer's, because an external
    /// sender is not a colleague session and there is nobody to reply to.
    /// Its own variant rather than a flag on `PeerMessage` because
    /// bitcode encodes a variant's payload densely: a field added to one
    /// changes its bytes and misreads every log already holding one.
    ///
    /// `channel` is carried for the transcript only — a fan-out arrives
    /// as one record per recipient, and the channel key is what lets a
    /// consumer draw it as the broadcast it was. The frame ignores it.
    ///
    /// Same delivery rules as a peer message: written at turn boundaries
    /// and mid-turn at the same safe points as a steer, never between an
    /// assistant's `tool_use` and its results. See [`crate::peer`].
    ///
    /// **Appended, as a journaled enum must be** — after
    /// [`RecordKind::TurnBudget`], the last variant, with its ordinal in
    /// `crates/core/tests/pins.rs`.
    ExternalMessage {
        from: String,
        channel: Option<String>,
        text: String,
    },
    /// How fast the turn's model calls went, as the harness timed them
    /// around each request — written just before the
    /// [`RecordKind::TurnSettled`] it belongs to, the way
    /// [`RecordKind::ContextSize`] is, because the settle's `usage` is the
    /// total across the calls this describes.
    ///
    /// Journaled rather than shown and forgotten because nothing else can
    /// recover it: no wire the harness speaks reports durations, and the
    /// settle's own timestamps bracket the whole turn — tool calls and
    /// approvals included — which is a different number. Its own record
    /// rather than a field on `TurnSettled`, for the reason spelled out on
    /// [`RecordKind`]: a field added to a variant already on disk rewrites
    /// its bytes and misreads every log that holds one.
    ///
    /// Written only when the loop timed at least one call, so a turn that
    /// never reached a model, a driver that owns its own loop, and a
    /// cancel (which writes no settle) all carry no pace at all — absent,
    /// never a zero claiming the call was instant.
    ///
    /// **Appended, as a journaled enum must be** — after
    /// [`RecordKind::ExternalMessage`], the last variant, with its ordinal
    /// in `crates/core/tests/pins.rs`.
    TurnPace {
        timing: TurnTiming,
        /// The tokens those calls produced, so the record says its own rate
        /// without the settle beside it. The same number the settle counts,
        /// repeated rather than left to be inferred: a reader of one line
        /// of `eidolon log` has no settle in hand.
        output_tokens: u64,
    },
    /// What the harness answered when the model wrote **commands in its own
    /// prose** — the inline command channel's half of a round trip, written
    /// where a `ToolResult` would sit had the model made a call instead.
    ///
    /// Its own kind for [`RecordKind::PeerMessage`]'s reason, one step
    /// closer to home: these lines are the harness speaking, and a
    /// `UserMessage` would tell both the model and every consumer replaying
    /// the branch that the operator said them. What the model is told is
    /// [`command_frame`]'s wording, rebuilt in [`Session::messages_after`]
    /// from `lines` — which is why the record carries the lines and not the
    /// sentence around them. The lines themselves are data: each one names
    /// the call that actually ran (under the wire's argument names) and
    /// carries its answer whole, and no reader can rebuild that from anything
    /// else on the branch.
    ///
    /// **A reply that marks commands is owed their answers**, exactly as a
    /// reply that made a tool call is owed its results, and it is owed them
    /// *before the turn settles*: the model must read them and answer from
    /// them in the same turn rather than yielding to the operator with a
    /// question the harness has already answered. So this record is written
    /// at the point the loop would otherwise settle, and the loop goes
    /// round once more — a continuation, not a retry: the prose that
    /// prompted it is already journaled and already on screen.
    ///
    /// **Appended, as a journaled enum must be** — after
    /// [`RecordKind::TurnPace`], the last variant, with its ordinal in
    /// `crates/core/tests/pins.rs`.
    CommandResults {
        lines: Vec<String>,
    },
    /// A park this session armed resolved: the condition came true, its
    /// deadline arrived, or a term turned out to be unanswerable.
    ///
    /// Its own kind for [`RecordKind::PeerMessage`]'s reason: what the
    /// model is told about a wake is not a peer's note and not the
    /// operator's word, and replay has to rebuild the frame it was woken by
    /// from the record rather than from whatever the model made of it. The
    /// *registration* needs no kind of its own — the `wait_for` tool_use and
    /// its journaled result already say a park is armed, and this record,
    /// naming that call id, says it is spent.
    TriggerFired {
        /// The `tool_use` id of the `wait_for` call that armed it.
        call_id: String,
        /// The condition in words, as the model wrote it.
        condition: String,
        /// What became of it, in the words the model reads.
        outcome: String,
    },
}

/// What became of a call the gate did not wave through silently.
///
/// A journaled enum: **append only**, for the reason spelled out on
/// [`RecordKind`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, bitcode::Encode, bitcode::Decode)]
pub enum PolicyOutcome {
    /// Refused on the merits. Never put to anybody — the table said no, and
    /// that is the answer.
    Refused,
    /// Asked, and the operator said yes.
    Approved,
    /// Asked, and the operator said no — or there was nobody to ask, which
    /// [`crate::user::NoUser`] answers the same way and on purpose.
    Declined,
    /// Asked, and the context-aware layer answered yes on the operator's
    /// behalf before the question reached them.
    ///
    /// Its own outcome rather than an [`Self::Approved`] with a note,
    /// because a promotion loop that counted a model's yes as a person's yes
    /// would be a boundary quietly moving itself — the one thing the whole
    /// arrangement is built to prevent.
    Judged,
    /// Asked, and nothing answered: the operator had armed
    /// [`crate::yolo::Yolo`], so the question became a yes without being
    /// put to anybody or read by anything.
    ///
    /// Its own outcome for the reason [`Self::Judged`] is, one step
    /// further along. A yolo yes is not evidence about the call — nothing
    /// looked at it — so a ledger counting these as approvals would be
    /// promoting a table entry on the strength of a switch being on. What
    /// it *is* evidence about is the mode: this is the record of what ran
    /// while the gate was blind, and the `reason` on it is still the
    /// classifier's own, so the question it would have asked is legible
    /// afterwards even though nobody was asked it.
    ///
    /// Last, and appended: this enum is journaled by variant order.
    Yolo,
}

/// What the model is told in place of a tool result the operator struck.
///
/// Deliberately not silence and not `flush`'s interrupted-call placeholder:
/// the call ran and produced something, and the one thing the model needs to
/// know is that what came back is not being shown to it — so that it asks
/// again rather than reasoning from an absence it reads as an empty result.
const WITHHELD: &str = "[the operator withheld this result from the conversation; it ran and returned output, which is on the branch but not in your context]";

/// A record as stored: id, parent, wall-clock time, payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, bitcode::Encode, bitcode::Decode)]
pub struct Record {
    pub id: RecordId,
    pub parent: Option<RecordId>,
    pub ts_ms: u64,
    pub kind: RecordKind,
}

/// One trace line: the record's own serde form and a newline — the one
/// JSON shape a record has, and the only export door: `eidolon log --json`
/// writes these, so a reader never needs a second file on disk.
pub fn trace_line(record: &Record) -> serde_json::Result<String> {
    let mut line = serde_json::to_string(record)?;
    line.push('\n');
    Ok(line)
}

/// An open session: the log plus derived views over the current branch.
pub struct Session {
    log: SessionLog<Record>,
    /// The reopen-time cwd override — see [`Session::open_with_cwd`].
    /// `None` unless this reopening named a cwd of its own.
    cwd_override: Option<PathBuf>,
}

impl Session {
    /// Create a new session file and write its `SessionStart` record.
    pub fn create(
        path: &Path,
        model: &str,
        cwd: &Path,
        system: Option<String>,
    ) -> anyhow::Result<Self> {
        let log = SessionLog::create(path)?;
        let mut s = Session {
            log,
            cwd_override: None,
        };
        s.append(RecordKind::SessionStart {
            model: model.to_string(),
            cwd: cwd.display().to_string(),
            system,
        })?;
        Ok(s)
    }

    /// Open an existing session, replaying its records. The head is the
    /// last record in the file (append order), which is the last thing
    /// that happened — not necessarily the longest branch.
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let log = SessionLog::open(path)?;
        Ok(Session {
            log,
            cwd_override: None,
        })
    }

    /// Open a session to read it, not to run it: the journal is left
    /// exactly as found, torn tail and all, so reading a *live* session
    /// can never repair — that is, truncate — it under its writer. The
    /// listing and export paths take this one; resuming takes `open`.
    pub fn open_readonly(path: &Path) -> anyhow::Result<Self> {
        let log = SessionLog::open_readonly(path)?;
        Ok(Session {
            log,
            cwd_override: None,
        })
    }

    /// Open an existing session to run it in `cwd` rather than in the one
    /// its `SessionStart` record carries — the **reopen-time cwd
    /// override**, and the whole of what a session arriving from another
    /// machine needs to run here (see [`Session::open_projected`]).
    ///
    /// The override is in-memory on purpose. It says where *this
    /// reopening* runs the session, which is the caller's statement to
    /// make each time the file is opened — not a fact about the branch,
    /// which is what the journal holds and what must survive every
    /// reopen. So the birth cwd stays on the record exactly as written
    /// ([`Session::start`] still answers it), [`Session::cwd`] answers
    /// the override for as long as this session is open, and nothing on
    /// disk is rewritten — effective state diverging from start state
    /// without falsifying history, the way a `ModelChanged` record
    /// overrides the start model. What the journal *does* hold for a
    /// machine hop is the projection's note, which names the new cwd.
    pub fn open_with_cwd(path: &Path, cwd: &Path) -> anyhow::Result<Self> {
        let mut s = Session::open(path)?;
        s.cwd_override = Some(cwd.to_path_buf());
        Ok(s)
    }

    /// Reopen a session that arrived from another machine — a hop, in the
    /// astral-projection sense — and mark the fact on the branch. This is
    /// the venue side of a projection, and it is deliberately the whole of
    /// it: one file copy (the shipper's) plus this call, with nothing
    /// Melete-shaped anywhere in it. A session hopped between two
    /// standalone eidolon boxes takes exactly this path.
    ///
    /// Three things, and the first is free. [`SessionLog::open`] verifies
    /// every record's crc32 on the way in, so a transfer that lost or
    /// flipped bytes is refused here rather than poisoning a turn —
    /// transfer integrity comes with the format, not on top of it. Then
    /// every backend session mark is dropped
    /// ([`Session::clear_backend_sessions`]): the ids name conversations
    /// that live on the machine this session was born on, so no backend
    /// here holds them, and the next turn on any of those backends replays
    /// the whole branch. A session that never used a keeping backend —
    /// every eidolon-native one — carries no marks and hops free. Last, a
    /// [`RecordKind::Note`] records the projection itself: where it
    /// arrived from, the birth cwd it left, the cwd it now runs under.
    /// Every path earlier on the branch is birth-machine text — tool-call
    /// arguments, tool results — which is history, and history is not
    /// rewritten.
    ///
    /// `from` names the machine the session arrived from, when the caller
    /// knows one; the note is honest without it. The file is edited in
    /// place: the venue's copy becomes an ordinary session of this
    /// machine, which every surface that opens a session can then reopen —
    /// naming [`Session::open_with_cwd`] again, or the equivalent `--cwd`,
    /// when it wants somewhere other than the birth directory.
    pub fn open_projected(path: &Path, cwd: &Path, from: Option<&str>) -> anyhow::Result<Self> {
        let mut s = Session::open_with_cwd(path, cwd)?;
        let birth = s.start().map(|(_, cwd, _)| cwd.to_string());
        let dropped = s.clear_backend_sessions()?;
        let dropped = if dropped.is_empty() {
            "none held".to_string()
        } else {
            dropped.join(", ")
        };
        s.append(RecordKind::Note {
            text: format!(
                "projected: arrived from {}, born at {}, now running at {}; \
                 paths in earlier records refer to the machine it was born on; \
                 backend session marks cleared: {dropped}",
                from.unwrap_or("another machine"),
                birth.as_deref().unwrap_or("(no start record)"),
                cwd.display(),
            ),
        })?;
        // The landing, for the model's own context. The note above is
        // operator-facing — a `Note` rides no replay — so without this the
        // model would keep every birth-machine path in its history and never
        // learn the ground moved: the first turn would `cat` a file that is
        // not there and take the failure for the file's. External, not
        // user: the words must not read as the operator's. Skipped, not
        // dropped, when the branch owes results — a message between an
        // assistant's `tool_use` and its results poisons the replay, and
        // the owed work runs before anything could read this anyway.
        if s.unanswered_tool_uses().is_empty() {
            let born = birth.as_deref().unwrap_or("(unknown)");
            let where_it_was = if Path::new(born).is_dir() {
                format!("was born at {born}")
            } else {
                format!("was born at {born}, a directory that does not exist on this machine")
            };
            s.append(RecordKind::ExternalMessage {
                from: "eidolon import".into(),
                channel: None,
                text: format!(
                    "This session has been projected onto this machine. It {where_it_was}; \
                     its tools now run from {}. Absolute paths in turns before this message \
                     refer to the machine it was born on and will not resolve here — \
                     re-derive them against the new working directory before using them.",
                    cwd.display(),
                ),
            })?;
        }
        Ok(s)
    }

    pub fn path(&self) -> &Path {
        self.log.path()
    }

    pub fn head(&self) -> Option<RecordId> {
        self.log.head()
    }

    pub fn records(&self) -> &[Record] {
        self.log.records()
    }

    /// Append a record whose parent is the current head; becomes the head.
    pub fn append(&mut self, kind: RecordKind) -> anyhow::Result<RecordId> {
        let id = self.log.next_id();
        let rec = Record {
            id,
            parent: self.log.head(),
            ts_ms: now_ms(),
            kind,
        };
        self.log.append(rec)
    }

    /// Move the head to an older record. The next append forks from there.
    pub fn fork_at(&mut self, id: RecordId) -> anyhow::Result<()> {
        self.log.set_head(id)
    }

    /// The records on the branch ending at the head, root first.
    pub fn branch(&self) -> Vec<&Record> {
        self.branch_from(self.log.head())
    }

    pub fn branch_from(&self, head: Option<RecordId>) -> Vec<&Record> {
        let mut out = Vec::new();
        let mut cur = head;
        while let Some(id) = cur {
            match self.log.get(id) {
                Some(r) => {
                    out.push(r);
                    cur = r.parent;
                }
                None => break,
            }
        }
        out.reverse();
        out
    }

    /// The `SessionStart` record's fields for the current branch. These
    /// are the birth facts — model, cwd, system — and they do not change:
    /// what changed since is on later records, which is why
    /// [`Session::model`] and [`Session::cwd`] exist beside this.
    pub fn start(&self) -> Option<(&str, &str, Option<&str>)> {
        self.branch().into_iter().find_map(|r| match &r.kind {
            RecordKind::SessionStart { model, cwd, system } => {
                Some((model.as_str(), cwd.as_str(), system.as_deref()))
            }
            _ => None,
        })
    }

    /// The directory this session runs in: the reopen-time override when
    /// this reopening named one ([`Session::open_with_cwd`]), else the
    /// birth cwd the `SessionStart` record carries. `None` only for a log
    /// with no start record, which [`Session::create`] cannot produce.
    ///
    /// This is the accessor every consumer that *acts* on the cwd uses —
    /// the one that decides where tools run. A consumer that *reports*
    /// where a session was born reads [`Session::start`].
    pub fn cwd(&self) -> Option<String> {
        if let Some(cwd) = &self.cwd_override {
            return Some(cwd.display().to_string());
        }
        self.start().map(|(_, cwd, _)| cwd.to_string())
    }

    /// The model in effect at the head (last `ModelChanged`, else start).
    pub fn model(&self) -> Option<String> {
        let mut model = None;
        for r in self.branch() {
            match &r.kind {
                RecordKind::SessionStart { model: m, .. } => model = Some(m.clone()),
                RecordKind::ModelChanged { model: m } => model = Some(m.clone()),
                _ => {}
            }
        }
        model
    }

    /// Rebuild the provider-facing message list for the current branch.
    ///
    /// Tool results are folded into a single `User` message following the
    /// assistant message they answer, in the order the assistant asked for
    /// them, which is the shape the API requires. A `tool_use` with no
    /// journaled result (the session was cancelled mid-dispatch) gets a
    /// synthetic error result so the conversation stays well-formed.
    pub fn messages(&self) -> Vec<Message> {
        self.messages_after(None)
    }

    /// The same list, restricted to the records that follow `after`.
    ///
    /// A backend that keeps its own conversation (the Claude CLI) uses this
    /// to catch that conversation up: `after` is its high-water mark from
    /// [`Session::backend_session_mark`], and what comes back is everything
    /// the branch has said since — the turns it answered itself excluded,
    /// because its own mark sits after them. A compaction still wins: the
    /// list never reaches back past the summary. An `after` that is not on
    /// this branch (a fork moved the head) yields the whole branch, which
    /// repeats rather than loses.
    pub fn messages_after(&self, after: Option<RecordId>) -> Vec<Message> {
        let mut out: Vec<Message> = Vec::new();
        let mut pending: Vec<(String, Option<ContentBlock>)> = Vec::new();
        // A user-originated call waiting for its result. It cannot join
        // `pending`: a `tool_result` block is only well-formed against a
        // `tool_use` the *assistant* asked for, and this one had no
        // assistant behind it. So the pair becomes plain user text
        // instead — which is also the honest thing to tell the model,
        // since it did not make this call and should not think it did.
        let mut ran: Option<(String, String, Json, Option<String>)> = None;
        // The last record the walk below took, for the one rule that looks
        // backwards: a user message landing directly after another one.
        let mut previous: Option<&Record> = None;
        let branch = self.branch();
        let compacted = branch
            .iter()
            .rposition(|r| matches!(r.kind, RecordKind::Compacted { .. }))
            .unwrap_or(0);
        let struck = self.excluded();
        let seen = after
            .and_then(|id| branch.iter().position(|r| r.id == id))
            .map_or(0, |i| i + 1);
        let start = compacted.max(seen);
        if start >= branch.len() {
            return out;
        }

        let flush = |out: &mut Vec<Message>, pending: &mut Vec<(String, Option<ContentBlock>)>| {
            if pending.is_empty() {
                return;
            }
            let blocks = pending
                .drain(..)
                .map(|(id, b)| {
                    b.unwrap_or(ContentBlock::ToolResult {
                        tool_use_id: id,
                        content: "tool call was interrupted before it produced a result".into(),
                        is_error: true,
                    })
                })
                .collect();
            out.push(Message::user(blocks));
        };

        // What the model is told about a call the operator made. An
        // unanswered one still gets a line, for the same reason `flush`
        // invents a result: a hole in the conversation is worse than a
        // sentence saying there is one.
        let ran_message =
            |name: &str, input: &Json, utterance: &Option<String>, result: Option<(&str, bool)>| {
                let mut t = String::from(
                    "[The operator ran a tool directly. This call is not one you made.]\n",
                );
                if let Some(u) = utterance
                    .as_deref()
                    .map(str::trim)
                    .filter(|u| !u.is_empty())
                {
                    t.push_str(&format!("they asked for: {u}\n"));
                }
                t.push_str(&format!("{name} {}\n", input.0));
                match result {
                    Some((content, true)) => t.push_str(&format!("it failed:\n{content}")),
                    Some((content, false)) => t.push_str(&format!("it returned:\n{content}")),
                    None => t.push_str("it was interrupted before it produced a result"),
                }
                Message::user_text(t)
            };

        for r in &branch[start..] {
            // A struck record is simply not walked, and the pairing looks
            // after itself from there: an assistant message that is never
            // pushed never opens `pending`, so the `ToolResult`s answering
            // its calls find no slot and are dropped with it. That is the
            // rule the whole feature turns on — a `tool_result` block is
            // only well-formed against a `tool_use` the assistant actually
            // made, so striking the call and keeping the answer would hand
            // the model a malformed turn.
            //
            // A struck **result** is the exception, and is handled in its
            // own arm rather than here: its `tool_use` is still on the
            // branch and still has to be answered.
            if struck.contains(&r.id) && !matches!(r.kind, RecordKind::ToolResult { .. }) {
                continue;
            }
            // A user call is answered by the very next record or not at
            // all — the dispatcher writes the pair back to back — so
            // anything else arriving first means it was interrupted.
            if let Some((id, ..)) = &ran
                && !matches!(&r.kind, RecordKind::ToolResult { tool_use_id, .. } if tool_use_id == id)
            {
                let (_, n, i, u) = ran.take().expect("checked just above");
                out.push(ran_message(&n, &i, &u, None));
            }
            match &r.kind {
                RecordKind::Compacted { summary, .. } => {
                    out.push(Message::user_text(format!(
                        "[The conversation so far was compacted. Summary:]\n\n{summary}"
                    )));
                }
                RecordKind::UserMessage(m) => {
                    // A user message landing while results are owed is a
                    // steer (or the operator speaking after a crash), and
                    // it goes out *in* the message that carries the
                    // results rather than after it: results first, then
                    // the words. One message rather than two because a
                    // strict endpoint (DeepSeek's reasoner) refuses two
                    // user turns in a row, and because every wire accepts
                    // this shape — Anthropic as one user message, the
                    // chat-completions wire as `tool` messages followed by
                    // a `user` one.
                    //
                    // The same holds between user messages that land back
                    // to back with nothing between them on the branch:
                    // now that a message sent mid-turn steers, two typed
                    // during one round of calls are delivered at the same
                    // safe point, and the second must not become a second
                    // user turn in a row either. The harness's own answers
                    // to the model's inline commands count as the first of
                    // that pair for the same reason: a steer delivered
                    // after them (and a steer is delivered on the next
                    // iteration, which is exactly where the answers leave
                    // off) rides them rather than opening a second user
                    // turn — results first, then the words, as ever.
                    // A framed note — a peer's, an outside caller's, the
                    // wrap-up nudge — is a different matter: it keeps its
                    // own message, so what the frame says about who is
                    // speaking stays around only what that sender said.
                    let stacked = matches!(
                        previous.map(|p| &p.kind),
                        Some(RecordKind::UserMessage(_) | RecordKind::CommandResults { .. })
                    );
                    let rode_results = !pending.is_empty();
                    if rode_results {
                        flush(&mut out, &mut pending);
                    }
                    match out.last_mut().filter(|l| l.role == Role::User) {
                        Some(last) if rode_results || stacked => {
                            last.content.extend(m.content.iter().cloned());
                        }
                        _ => out.push(m.clone()),
                    }
                }
                // A peer's message reaches the model as user-role text
                // with a frame saying who is speaking, exactly as a
                // dispatch does. There is nowhere else it could go: a
                // `tool_result` needs a `tool_use` behind it, and an
                // assistant-role message would be this model claiming to
                // have said what another one did. Like a steer, a peer
                // message landing while results are owed rides in the
                // message that carries them rather than becoming a second
                // user turn in a row.
                RecordKind::PeerMessage {
                    from,
                    from_cwd,
                    channel,
                    text,
                } => {
                    let message = Message::user_text(crate::peer::frame(
                        from,
                        from_cwd,
                        channel.as_deref(),
                        text,
                    ));
                    if pending.is_empty() {
                        out.push(message);
                    } else {
                        flush(&mut out, &mut pending);
                        if let Some(last) = out.last_mut() {
                            last.content.extend(message.content.iter().cloned());
                        }
                    }
                }
                // An outside caller's message rides exactly where a
                // peer's does, with a frame that says a different thing:
                // a tool injecting text, not a colleague session.
                RecordKind::ExternalMessage { from, text, .. } => {
                    let message =
                        Message::user_text(crate::peer::external_frame(from, text));
                    if pending.is_empty() {
                        out.push(message);
                    } else {
                        flush(&mut out, &mut pending);
                        if let Some(last) = out.last_mut() {
                            last.content.extend(message.content.iter().cloned());
                        }
                    }
                }
                // A park resolving rides exactly where a peer's note does,
                // and must: it is user-shaped input to the model, it arrives
                // after a turn settled rather than mid-call, and a frame is
                // the only way the model can tell it from a colleague or from
                // the operator.
                RecordKind::TriggerFired {
                    condition, outcome, ..
                } => {
                    let message =
                        Message::user_text(crate::wait::fired_frame(condition, outcome, None));
                    if pending.is_empty() {
                        out.push(message);
                    } else {
                        flush(&mut out, &mut pending);
                        if let Some(last) = out.last_mut() {
                            last.content.extend(message.content.iter().cloned());
                        }
                    }
                }
                // The harness's answers to commands the model wrote in its
                // own prose. They land where a reply's tool results land and
                // read the way `flush` makes them read: user-role text that
                // says who is speaking, because a `tool_result` block needs
                // a `tool_use` behind it and there is none here.
                RecordKind::CommandResults { lines } => {
                    let message = Message::user_text(command_frame(lines));
                    if pending.is_empty() {
                        out.push(message);
                    } else {
                        flush(&mut out, &mut pending);
                        if let Some(last) = out.last_mut() {
                            last.content.extend(message.content.iter().cloned());
                        }
                    }
                }
                // The wrap-up nudge rides exactly where a peer message or
                // a steer does: user-role, framed as the harness's own
                // words, and folded into the message carrying any owed
                // results rather than after it.
                RecordKind::TurnBudget { calls_left } => {
                    let message = Message::user_text(budget_frame(*calls_left));
                    if pending.is_empty() {
                        out.push(message);
                    } else {
                        flush(&mut out, &mut pending);
                        if let Some(last) = out.last_mut() {
                            last.content.extend(message.content.iter().cloned());
                        }
                    }
                }
                RecordKind::AssistantMessage(m) => {
                    flush(&mut out, &mut pending);
                    out.push(m.clone());
                    pending = m
                        .tool_uses()
                        .map(|(id, _, _)| (id.to_string(), None))
                        .collect();
                }
                RecordKind::UserToolCall {
                    tool_use_id,
                    name,
                    input,
                    utterance,
                } => {
                    flush(&mut out, &mut pending);
                    ran = Some((
                        tool_use_id.clone(),
                        name.clone(),
                        input.clone(),
                        utterance.clone(),
                    ));
                }
                RecordKind::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => {
                    // A struck result still answers, because the call above
                    // it is still there and an unanswered `tool_use` is a
                    // malformed turn. What it must not do is answer with
                    // `flush`'s placeholder: "interrupted before it produced
                    // a result" is what a cancelled call says, and this call
                    // produced one — the operator simply took it back. A
                    // model told the wrong story about its own tools will
                    // reason from it.
                    let withheld = struck.contains(&r.id);
                    let content: &str = if withheld { WITHHELD } else { content };
                    let is_error = !withheld && *is_error;
                    if ran.as_ref().is_some_and(|(id, ..)| id == tool_use_id) {
                        let (_, n, i, u) = ran.take().expect("checked just above");
                        out.push(ran_message(&n, &i, &u, Some((content, is_error))));
                    } else if let Some(slot) = pending.iter_mut().find(|(id, _)| id == tool_use_id)
                    {
                        slot.1 = Some(ContentBlock::ToolResult {
                            tool_use_id: tool_use_id.clone(),
                            content: content.to_string(),
                            is_error,
                        });
                    }
                }
                _ => {}
            }
            // For the next record's `stacked` check: the last record this
            // walk actually took. A struck record skips this assignment,
            // which is the point — striking something between two user
            // messages leaves them back to back.
            previous = Some(r);
        }
        flush(&mut out, &mut pending);
        if let Some((_, n, i, u)) = ran {
            out.push(ran_message(&n, &i, &u, None));
        }

        // A trailing assistant message means the last turn stopped before a
        // user message; that is valid input for the next call as long as
        // its tool uses were answered, which flush guarantees.
        debug_assert!(out.iter().all(|m| !m.content.is_empty()));
        out
    }

    /// Ids of `tool_use` blocks on the branch that have no result yet.
    pub fn unanswered_tool_uses(&self) -> Vec<(String, String, Json)> {
        let mut pending: Vec<(String, String, Json)> = Vec::new();
        for r in self.branch() {
            match &r.kind {
                RecordKind::AssistantMessage(m) => {
                    pending = m
                        .tool_uses()
                        .map(|(i, n, j)| (i.to_string(), n.to_string(), j.clone()))
                        .collect();
                }
                RecordKind::ToolResult { tool_use_id, .. } => {
                    pending.retain(|(i, _, _)| i != tool_use_id)
                }
                RecordKind::UserMessage(_) | RecordKind::TurnSettled { .. } => pending.clear(),
                _ => {}
            }
        }
        pending
    }

    /// Whether the branch still holds a turn a model call can finish —
    /// the question "finish an unsettled turn" asks on reopen, of which
    /// [`Session::is_settled`] is only the log-shape half.
    ///
    /// Most unsettled branches do want finishing: the ask never got its
    /// reply, a cancel cut the turn mid-flight, or a crash left tool uses
    /// dangling ([`Session::unanswered_tool_uses`]) — the continuation is
    /// what answers them. One shape does not: the branch ends on the
    /// model's own reply with no boundary behind it. A crash in the window
    /// between those two writes lands there, and so does every journal
    /// another harness keeps that records its row accounting as a `Note`
    /// after the reply instead of a boundary — a session arriving on a hop,
    /// for one. The reply already stands; a continuation has no question to
    /// answer and only spends a duplicate on top of a complete answer.
    /// Reopen shows such a branch as it stands. The settle state itself is
    /// untouched — `is_settled` stays the accounting truth, and the usage
    /// floors built on it keep reading it.
    pub fn needs_finishing(&self) -> bool {
        if self.is_settled() {
            return false;
        }
        // Results a crash left owed come first: they are the reason the
        // finish-an-unsettled-turn recovery exists, and an assistant
        // message carrying them sits below the owed ones on the branch.
        if !self.unanswered_tool_uses().is_empty() {
            return true;
        }
        // The same walk `is_settled` runs, read for direction: the first
        // record that stops it says whether the turn is waiting to speak
        // (finish it) or already spoke (leave it).
        for r in self.branch().into_iter().rev() {
            match &r.kind {
                RecordKind::AssistantMessage(_) => return false,
                RecordKind::UserMessage(_) | RecordKind::Cancelled => return true,
                _ => continue,
            }
        }
        false
    }

    /// The most recent backend session id on the branch for `backend`.
    pub fn backend_session(&self, backend: &str) -> Option<String> {
        self.backend_session_mark(backend).map(|(_, id)| id)
    }

    /// The most recent `BackendSession` record for `backend`: where it sits
    /// on the branch, and the session id it names. The position is the
    /// high-water mark described on the record — everything at or before it
    /// is already in that backend's own session, and [`Session::messages_after`]
    /// turns what follows into the messages it still has to be told.
    pub fn backend_session_mark(&self, backend: &str) -> Option<(RecordId, String)> {
        self.branch()
            .into_iter()
            .rev()
            .find_map(|r| match &r.kind {
                RecordKind::BackendSession { backend: b, id } if b == backend => {
                    Some((r.id, id.clone()))
                }
                _ => None,
            })
            // An empty id is the eviction sentinel: the latest word on this
            // backend is that it holds nothing. There is no mark, so
            // [`Session::messages_after`] is asked for the whole branch
            // rather than for the span after the record — which is the
            // point, and the opposite of what returning the position would
            // do.
            .filter(|(_, id)| !id.is_empty())
    }

    /// The records struck from the replay on this branch.
    ///
    /// Latest word per target wins, so a strike followed by a restore
    /// leaves nothing behind.
    pub fn excluded(&self) -> std::collections::HashSet<RecordId> {
        let mut out = std::collections::HashSet::new();
        for r in self.branch() {
            if let RecordKind::Excluded { target, excluded } = &r.kind {
                if *excluded {
                    out.insert(*target);
                } else {
                    out.remove(target);
                }
            }
        }
        out
    }

    /// The standing note for this branch, if one is set and non-empty.
    pub fn session_note(&self) -> Option<String> {
        self.branch()
            .into_iter()
            .rev()
            .find_map(|r| match &r.kind {
                RecordKind::SessionNote { text } => Some(text.clone()),
                _ => None,
            })
            .filter(|t| !t.trim().is_empty())
    }

    /// Set (or with an empty string, clear) the standing note.
    pub fn set_session_note(&mut self, text: &str) -> anyhow::Result<()> {
        self.append(RecordKind::SessionNote {
            text: text.to_string(),
        })?;
        Ok(())
    }

    /// The persona pinned on this branch, if one is set and non-empty —
    /// as pinned, not as resolved. Latest word wins, like the note.
    pub fn persona(&self) -> Option<String> {
        self.branch()
            .into_iter()
            .rev()
            .find_map(|r| match &r.kind {
                RecordKind::PersonaPinned { persona } => Some(persona.clone()),
                _ => None,
            })
            .filter(|p| !p.trim().is_empty())
    }

    /// Pin (or with an empty string, clear) the branch's persona.
    /// `Ok(false)` when it is already what it would be set to — a repeat
    /// pin is not worth a record, and every one of them would be a
    /// deliberate prefix miss for no change.
    pub fn set_persona(&mut self, persona: &str) -> anyhow::Result<bool> {
        let persona = persona.trim();
        if self.persona().as_deref().unwrap_or("") == persona {
            return Ok(false);
        }
        self.append(RecordKind::PersonaPinned {
            persona: persona.to_string(),
        })?;
        Ok(true)
    }

    /// The paths pinned on this branch, in the order they were first
    /// pinned — stable, because a set in a `HashSet`'s order would move a
    /// block around the prompt between turns and miss the cache every time.
    pub fn pins(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for r in self.branch() {
            if let RecordKind::Pinned { path, pinned } = &r.kind {
                let at = out.iter().position(|p| p == path);
                match (pinned, at) {
                    (true, None) => out.push(path.clone()),
                    (false, Some(i)) => {
                        out.remove(i);
                    }
                    _ => {}
                }
            }
        }
        out
    }

    /// Pin a path into every turn, or unpin it. `Ok(false)` when it is
    /// already in that state.
    pub fn set_pinned(&mut self, path: &str, pinned: bool) -> anyhow::Result<bool> {
        if self.pins().iter().any(|p| p == path) == pinned {
            return Ok(false);
        }
        self.append(RecordKind::Pinned {
            path: path.to_string(),
            pinned,
        })?;
        Ok(true)
    }

    /// Strike `target` from the replay, or restore it.
    ///
    /// `Ok(false)` when it is already in that state, so pressing the key
    /// twice does not grow the log with a record that changes nothing.
    pub fn set_excluded(&mut self, target: RecordId, excluded: bool) -> anyhow::Result<bool> {
        if self.excluded().contains(&target) == excluded {
            return Ok(false);
        }
        self.append(RecordKind::Excluded { target, excluded })?;
        Ok(true)
    }

    /// Forget the backend's own session for this branch: the next turn on
    /// `backend` starts a new one and is told the branch as a transcript.
    ///
    /// **The eviction primitive the context controls are built on.** A
    /// `BackendSession` mark is a claim that the backend's own conversation
    /// already holds the branch up to that point. Anything that changes what
    /// the branch *means* below the mark — a compaction today, an exclusion
    /// or a pin or a session note in time — makes that claim false, and the
    /// backend would go on answering out of a history the harness has since
    /// edited. Dropping the mark is the whole of the fix, and it is why
    /// "fresh eyes" is a control at all rather than a synonym for a new
    /// session: the branch is kept and only the backend's memory of it goes.
    ///
    /// Journaled rather than remembered, because a resume has to reproduce
    /// it — a session reopened tomorrow must not quietly resume the
    /// conversation the operator threw away.
    ///
    /// `Ok(false)` when there was no mark to drop, so a caller can say
    /// "nothing held" instead of reporting a success it did not have.
    pub fn clear_backend_session(&mut self, backend: &str) -> anyhow::Result<bool> {
        if self.backend_session_mark(backend).is_none() {
            return Ok(false);
        }
        self.append(RecordKind::BackendSession {
            backend: backend.to_string(),
            id: String::new(),
        })?;
        Ok(true)
    }

    /// [`Session::clear_backend_session`] for every backend at once,
    /// naming the backends whose marks were actually live.
    ///
    /// This is the shape a session arriving from another machine is
    /// received with (see [`Session::open_projected`]): a backend session
    /// id exists on the machine that made it, so a branch that crossed
    /// machines holds nothing any backend *here* can resume, and every
    /// mark on it says so. The per-backend calls are the real work — each
    /// appends the same empty-id eviction record — this only walks the
    /// branch for the distinct names worth evicting, so the caller can say
    /// what was dropped instead of "everything, probably".
    pub fn clear_backend_sessions(&mut self) -> anyhow::Result<Vec<String>> {
        // Latest mark per backend wins, exactly as
        // [`Session::backend_session_mark`] reads the branch: a backend
        // whose newest word is the empty id holds nothing and is not
        // named, whatever its older records said.
        let mut marks: Vec<(String, bool)> = Vec::new();
        for r in self.branch() {
            if let RecordKind::BackendSession { backend, id } = &r.kind {
                let live = !id.is_empty();
                match marks.iter_mut().find(|(b, _)| b == backend) {
                    Some((_, l)) => *l = live,
                    None => marks.push((backend.clone(), live)),
                }
            }
        }
        let out: Vec<String> = marks
            .into_iter()
            .filter(|(_, live)| *live)
            .map(|(b, _)| b)
            .collect();
        for backend in &out {
            self.clear_backend_session(backend)?;
        }
        Ok(out)
    }

    /// The user-shaped message that has not been answered yet: the last
    /// `UserMessage` or peer message on the branch with no assistant
    /// message after it. A peer message is returned framed exactly as
    /// [`Session::messages_after`] frames it, so a backend that sends its
    /// own prompt (the Claude CLI) answers a peer's note rather than
    /// ignoring it.
    pub fn pending_user_message(&self) -> Option<Message> {
        let mut pending: Option<Message> = None;
        for r in self.branch() {
            match &r.kind {
                RecordKind::UserMessage(m) => pending = Some(m.clone()),
                RecordKind::PeerMessage {
                    from,
                    from_cwd,
                    channel,
                    text,
                } => {
                    pending = Some(Message::user_text(crate::peer::frame(
                        from,
                        from_cwd,
                        channel.as_deref(),
                        text,
                    )));
                }
                // An external sender's note is as pending as a peer's —
                // and framed as what it is, so a backend driving its own
                // prompt answers the tool, not an imaginary operator.
                RecordKind::ExternalMessage { from, text, .. } => {
                    pending = Some(Message::user_text(crate::peer::external_frame(
                        from, text,
                    )));
                }
                RecordKind::AssistantMessage(_) | RecordKind::TurnSettled { .. } => pending = None,
                _ => {}
            }
        }
        pending
    }

    /// Whether the branch ends on a clean turn boundary.
    pub fn is_settled(&self) -> bool {
        for r in self.branch().into_iter().rev() {
            match &r.kind {
                RecordKind::TurnSettled { .. } | RecordKind::SessionStart { .. } => return true,
                RecordKind::AssistantMessage(_)
                | RecordKind::UserMessage(_)
                | RecordKind::Cancelled => return false,
                _ => continue,
            }
        }
        true
    }

    /// Input tokens the provider reported for the most recent turn that
    /// ended on the branch — settled, cancelled or exhausted — the size of
    /// the context as last measured. Feeds the context report and the TUI's
    /// gauge.
    ///
    /// A turn that did not settle counts here for the reason it journals
    /// the record at all: its last call read the whole transcript, and a
    /// gauge that only ever moved on a settle would sit on a stale number
    /// for as long as the operator kept stopping turns — or the loop kept
    /// hitting its iteration limit.
    pub fn last_input_tokens(&self) -> Option<u64> {
        for r in self.branch().into_iter().rev() {
            match &r.kind {
                RecordKind::ContextSize { tokens } => return Some(*tokens),
                // A compaction restarts the provider-facing history, so
                // nothing before it is a context size anymore. A compaction
                // written by the current core is followed by a `ContextSize`
                // carrying the summary's measured size, and the walk reaches
                // that first. A log from before that was recorded has only
                // this record, and the summary's size is not recoverable from
                // it — so the answer is *unknown*, not zero.
                RecordKind::Compacted { .. } => return None,
                _ => {}
            }
        }
        None
    }

    /// Every record with its children, for drawing the fork tree. Children
    /// are in id order; the current head is marked.
    pub fn tree(&self) -> Vec<TreeNode<'_>> {
        let head = self.head();
        let on_branch: std::collections::HashSet<RecordId> =
            self.branch().iter().map(|r| r.id).collect();
        self.records()
            .iter()
            .map(|r| TreeNode {
                record: r,
                children: self
                    .records()
                    .iter()
                    .filter(|c| c.parent == Some(r.id))
                    .map(|c| c.id)
                    .collect(),
                is_head: head == Some(r.id),
                on_branch: on_branch.contains(&r.id),
            })
            .collect()
    }

    /// Total usage journaled on the branch — what the session has spent.
    ///
    /// Both records that carry a turn's cost: the settle, and the
    /// [`RecordKind::TurnSpend`] of a turn that never reached one. They
    /// are never both written for the same turn, so this cannot double
    /// count; a cancelled turn used to be missing from it entirely,
    /// which made an interrupted session read as free.
    pub fn usage(&self) -> Usage {
        let mut u = Usage::default();
        for r in self.branch() {
            match &r.kind {
                RecordKind::TurnSettled { usage, .. } | RecordKind::TurnSpend { usage, .. } => {
                    u += *usage
                }
                _ => {}
            }
        }
        u
    }

    /// Last assistant text on the branch, if the branch is a conversation.
    pub fn last_assistant_text(&self) -> Option<String> {
        self.branch().into_iter().rev().find_map(|r| match &r.kind {
            RecordKind::AssistantMessage(m) if m.role == Role::Assistant => Some(m.text()),
            _ => None,
        })
    }
}

/// One row of [`Session::tree`].
pub struct TreeNode<'a> {
    pub record: &'a Record,
    pub children: Vec<RecordId>,
    pub is_head: bool,
    pub on_branch: bool,
}

/// What the model is told by [`RecordKind::TurnBudget`], rebuilt from
/// `calls_left` at request time — the record journals the number and not
/// the sentence, so the wording can be edited without breaking replay.
///
/// The frame matters for [`crate::peer::frame`]'s reason: words that
/// arrive in the user role read as the operator's unless they say
/// otherwise, and a model that takes the harness's bookkeeping for an
/// instruction from the person it works for will act on it as one.
pub fn budget_frame(calls_left: u32) -> String {
    let call = if calls_left == 1 { "call" } else { "calls" };
    format!(
        "[From the harness — not the operator: this turn has {calls_left} model {call} \
left before it is stopped. Finish the step you are in the middle of, then stop \
calling tools and reply with what you have done, what remains, and anything the \
operator should know. The conversation does not end here — the work stays in \
context and the next message can continue exactly where you leave off.]"
    )
}

/// What the model is told when the harness answers the commands it wrote in
/// its own prose — one entry per command, and the frame that says who is
/// speaking.
///
/// The frame matters for [`budget_frame`]'s reason, and one of its own: these
/// lines arrive in the user role (a `tool_result` needs a `tool_use` behind
/// it, and there is no call — the model wrote prose), so without a frame they
/// would read as the operator answering the model's commands, which is the
/// one thing they are not. What they are is the harness replying to the
/// model's own marked lines, and the wording says so.
///
/// The header names no command and no count: the lines below it are the whole
/// of what was answered, in the order the model wrote them, and each one
/// names the call that actually ran. A channel that answered three of five
/// lines says so in the lines, not here.
pub fn command_frame(lines: &[String]) -> String {
    format!(
        "[From the harness — not the operator: the answers to the commands you wrote as \
`! ` lines in your last message, one per command, in the order you wrote them. Read \
them and continue — the conversation has already moved on from the reply they answer.]\n{}",
        lines.join("\n")
    )
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
