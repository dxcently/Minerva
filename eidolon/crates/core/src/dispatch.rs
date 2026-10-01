//! The single async chokepoint every tool call passes through.
//!
//! This is the seam the whole policy story hangs on. There is no other way
//! to run a tool: the agent loop calls [`Dispatcher::dispatch`] for the
//! model's `tool_use` blocks, a Rune script's host functions call it for
//! `choices_user` and for tools it invokes, a future panel or palette calls it
//! for user-initiated actions. Because there is one path, the policy hook
//! is written once, the journal entry is written once, and the events are
//! published once.
//!
//! Order of operations for one call:
//!
//! 1. publish `ToolCallStarted`;
//! 2. look the tool up — unknown name is an error *result*, not a panic;
//! 3. evaluate the policy hook, **fail closed**: a hook that errors or
//!    panics is a `Deny`;
//! 4. on `Ask`, put the question to the [`UserIo`] and treat "no" or "no
//!    user" as `Deny`;
//! 5. run the tool under the cancellation token; a panic inside a tool is
//!    caught and becomes an error result;
//! 6. journal the result (`ToolResult` record) and publish `ToolCallFinished`.
//!
//! Step 3 journals too, and only when the answer was not a silent yes: a
//! `PolicyVerdict` record saying which rule fired and what became of the
//! question. It is written here rather than at step 6 because this is where
//! a verdict exists, and because the two rungs that skip step 6 entirely
//! still need it — nothing on a backend's own stream carries *the harness's*
//! decision about the calls that stream reports.
//!
//! A call the *operator* originated ([`CallOrigin::User`] — a dispatch)
//! gets one more record, a `UserToolCall`, written before step 5. A
//! model's call is already on the branch inside the assistant message
//! that asked for it; the operator's is not, and without it the result
//! journaled at step 6 belongs to nothing — invisible to
//! `Session::messages`, so the model never learns what was just run, and
//! invisible to a consumer replaying the branch, so a resumed session
//! loses the call. Use [`Dispatcher::dispatch_user`] to keep the words
//! the operator typed alongside the call they were read as.
//!
//! A backend that runs its own tools (the Claude CLI) still passes through
//! here: its pre-tool hook calls [`Dispatcher::adjudicate`], which is steps
//! 3–4 alone — the policy verdict, its record, and the user's answer — with
//! no execution and no result journaled, because the backend does both and
//! reports them on its stream. One policy, two entry points, no third.
//!
//! `choices_user` is an ordinary registered tool whose implementation
//! happens to talk to the `UserIo` — so it takes the same path and gets
//! journaled the same way. The journal is what makes a replayed script see
//! its earlier answer instead of asking twice.
//!
//! ## Large outputs spill
//!
//! A tool can return more text than belongs in context (half a megabyte of
//! build log). What travels inline is capped at [`MAX_INLINE_CHARS`]:
//! anything longer is written whole to a `*.spills` file beside the session
//! log, and what is journaled, published and handed back is the head
//! ([`SPILL_HEAD_CHARS`]) plus the path. The model pages the rest with the
//! `read` tool, which already offsets and limits. The journal holds the
//! pointer the model saw, so a resume replays the same bounded text rather
//! than the flood.
//!
//! One exemption, because the advice above is what a spill *chain* is made
//! of: a call that names its own read length ([`named_read_length`]) is
//! never spilled — the built-in `read`'s `limit`, or the `limit` inside the
//! envelope a wrapper passes its arguments through, as the vault's
//! `mneme_rpc` does with `args`. Spilling the page the receipt asked for
//! answers it with a second pointer, and the model pages again.

use std::collections::HashMap;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use anyhow::Context as _;
use async_trait::async_trait;
use futures_util::FutureExt;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::event::{Event, EventBus};
use crate::policy::{PolicyHook, Ruling, Verdict};
use crate::session::{PolicyOutcome, RecordId, RecordKind, Session};
use crate::tool::{CallContext, CallOrigin, Tool, ToolCall, ToolManifest, ToolOutput, ToolRegistry};
use crate::user::{UserIo, Words, with_operator_note};

/// A reader of calls that ran — telemetry, never a decision.
///
/// The chokepoint is the only place that sees *every* call, so it is the only
/// place that can measure one class of them without a second code path: a
/// loop-side command channel wants the model's own structured calls counted
/// (how often a hand-written `mneme_rpc` call is rejected for its argument
/// shape), and those are ordinary dispatched calls with nothing else watching.
///
/// It is deliberately **not** a policy hook and deliberately not able to
/// change anything: `observed` is handed the call and the output it already
/// produced, its return value is discarded, and a panic inside it is caught
/// and dropped — an observer that breaks must cost a measurement, never the
/// call it was watching. What it writes is the observer's own business; the
/// journal and the bus stay the dispatcher's.
#[async_trait]
pub trait ToolObserver: Send + Sync {
    async fn observed(&self, call: &ToolCall, output: &ToolOutput);
}

pub struct Dispatcher {
    registry: ToolRegistry,
    policy: Arc<dyn PolicyHook>,
    user: Arc<dyn UserIo>,
    bus: EventBus,
    session: Arc<Mutex<Session>>,
    /// Set once, after construction, by whoever has something to watch calls
    /// with — the registry cannot hold it, because the observer is usually a
    /// *tool* in that registry and the link would be a cycle. `None` is the
    /// ordinary case and costs one atomic load per call.
    observer: OnceLock<Arc<dyn ToolObserver>>,
    /// Where tools run. Live rather than fixed: the consumer may adopt a
    /// different session (the TUI's session picker does), and a session
    /// carries the directory it was started in — so the chokepoint holds
    /// the one copy everything else reads, per call, instead of every
    /// tool closing over a path captured when the process started.
    cwd: std::sync::RwLock<std::path::PathBuf>,
    /// Where spilled outputs go. The session log's own path with a `spills`
    /// extension would do — but the session behind the mutex is only
    /// replaceable between turns, never mid-call, and `dispatch` is async,
    /// so the path is captured here at construction and refreshed by
    /// [`Dispatcher::set_session_path`] when the consumer adopts a log.
    /// `None` (tests, the Claude driver's throwaway sessions) means the
    /// spill names itself off the tools' working directory instead.
    spill_dir: std::sync::RwLock<Option<PathBuf>>,
    /// The outstanding-question registry: what the door's `answer` op
    /// serves against, and what the roster's projection publishes. Owned
    /// by an `Arc` so the guard a asking seam holds can remove its entry
    /// on every exit without a reference back to the dispatcher.
    questions: Arc<std::sync::Mutex<QuestionBook>>,
    /// Set once, after construction, by whoever owns the roster-facing
    /// projection of the registry — the swarm writes what a peer's
    /// `peers` read shows. `None` (tests, sessions without a swarm) costs
    /// one `Option`.
    question_board: OnceLock<Arc<dyn QuestionBoard>>,
}

/// The kind of question a peer's answer may — or may never — resolve.
/// `Approve` is the gate's: approval decides what runs on this session,
/// and that decision is the operator's alone. `Respond` is a workflow
/// park's inline question, which is exactly what the door exists to
/// carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuestionKind {
    Approve,
    Respond,
}

impl QuestionKind {
    /// The word the roster and the refusals use.
    pub fn word(self) -> &'static str {
        match self {
            QuestionKind::Approve => "approve",
            QuestionKind::Respond => "respond",
        }
    }
}

/// A question the asking seam raised and is blocking on — the fact a
/// peer's answer answers. `text` is the short form a roster shows; the
/// full prompt already sits in the asker's journal as the
/// [`crate::session::RecordKind::AskUser`] the answer will ride.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutstandingQuestion {
    pub id: String,
    pub kind: QuestionKind,
    pub text: String,
}

/// What a peer's answer carries, delivered to the seam that asked.
#[derive(Clone, Debug)]
pub struct PeerDelivery {
    pub text: String,
    pub peer: String,
}

/// The book behind the registry: the open questions with the channel each
/// answer rides, and the tombstones that let the door tell "already
/// answered" from "no question outstanding" instead of teaching the loser
/// to retry blind.
#[derive(Default)]
struct QuestionBook {
    open: HashMap<String, (OutstandingQuestion, tokio::sync::oneshot::Sender<PeerDelivery>)>,
    answered: HashMap<String, String>,
}

/// The roster-facing projection of the registry. Implemented by whoever
/// owns both — the swarm writes the file a peer's `peers` read shows.
/// Called on every change, so a question exists to a peer exactly while
/// the asking seam holds it: nothing is advertised before the seam agrees
/// to ask, and nothing after it stops.
pub trait QuestionBoard: Send + Sync {
    fn publish(&self, questions: &[OutstandingQuestion]);
}

/// Held by the asking seam for exactly as long as its question stands.
/// Dropping it removes the registration — answer, refusal, cancel, error,
/// every exit — so the door and the roster see the question end when the
/// wait does, not when somebody remembers to say so.
pub struct QuestionGuard {
    book: Arc<std::sync::Mutex<QuestionBook>>,
    board: Option<Arc<dyn QuestionBoard>>,
    id: String,
}

impl Drop for QuestionGuard {
    fn drop(&mut self) {
        let mut book = self.book.lock().unwrap();
        if book.open.remove(&self.id).is_some() {
            let qs: Vec<OutstandingQuestion> =
                book.open.values().map(|(q, _)| q.clone()).collect();
            if let Some(board) = &self.board {
                board.publish(&qs);
            }
        }
    }
}

impl Dispatcher {
    pub fn new(
        registry: ToolRegistry,
        policy: Arc<dyn PolicyHook>,
        user: Arc<dyn UserIo>,
        bus: EventBus,
        session: Arc<Mutex<Session>>,
        cwd: std::path::PathBuf,
    ) -> Self {
        Dispatcher {
            registry,
            policy,
            user,
            bus,
            session,
            observer: OnceLock::new(),
            cwd: std::sync::RwLock::new(cwd),
            spill_dir: std::sync::RwLock::new(None),
            questions: Arc::new(std::sync::Mutex::new(QuestionBook::default())),
            question_board: OnceLock::new(),
        }
    }

    /// Publish the registry's changes to the roster's projection. Set once
    /// at startup, by whoever owns both the dispatcher and the swarm.
    pub fn set_question_board(&self, board: Arc<dyn QuestionBoard>) {
        let _ = self.question_board.set(board);
        self.publish_questions();
    }

    /// Register a question the asking seam is blocking on, and take the
    /// guard that removes it on every exit. The receiver is what a
    /// door-carried answer arrives on — the seam races it against its own
    /// [`UserIo`] wait.
    pub fn question_open(
        &self,
        kind: QuestionKind,
        id: &str,
        text: String,
    ) -> (QuestionGuard, tokio::sync::oneshot::Receiver<PeerDelivery>) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        {
            let mut book = self.questions.lock().unwrap();
            book.open.insert(
                id.to_string(),
                (OutstandingQuestion { id: id.to_string(), kind, text }, tx),
            );
        }
        self.publish_questions();
        (
            QuestionGuard {
                book: Arc::clone(&self.questions),
                board: self.question_board.get().cloned(),
                id: id.to_string(),
            },
            rx,
        )
    }

    /// The door's `answer`: carry a peer's text to the question `id`
    /// names. The refusals teach — each names which of the ways the
    /// answer cannot land, and "already answered" is told apart from "no
    /// question outstanding". An `Approve` question is never delivered:
    /// the entry goes back, because the operator's dialog is still
    /// standing and the reason says why no peer answer reaches it.
    pub fn deliver_answer(&self, id: &str, text: &str, peer: &str) -> Result<(), String> {
        if peer.is_empty() {
            return Err("the answer names no peer — the door credits an answer to the session that sent it, and this one claimed nothing".into());
        }
        let verdict = {
            let mut book = self.questions.lock().unwrap();
            match book.open.remove(id) {
                Some((q, tx)) if q.kind == QuestionKind::Approve => {
                    book.open.insert(id.to_string(), (q, tx));
                    Err(format!(
                        "the question `{id}` is a gate question — approval decides what runs on \
this session, and that is the operator's alone; a peer's answer is refused and the \
operator's dialog still stands"
                    ))
                }
                Some((_, tx)) => {
                    let peer = peer.to_string();
                    let delivered = tx
                        .send(PeerDelivery { text: text.to_string(), peer: peer.clone() })
                        .is_ok();
                    book.answered.insert(id.to_string(), peer.clone());
                    match delivered {
                        true => Ok(()),
                        false => Err(format!(
                            "the asking seam for `{id}` ended before the answer landed — the \
wait is over, and the answer nobody read is recorded as answered by `{peer}`"
                        )),
                    }
                }
                None => Err(match book.answered.get(id).cloned() {
                    Some(p) => format!("the question `{id}` was already answered by `{p}`"),
                    None => format!(
                        "no question `{id}` is outstanding here — `peers` shows the \
questions that are"
                    ),
                }),
            }
        };
        if verdict.is_ok() {
            self.publish_questions();
        }
        verdict
    }

    /// What stands, for the roster and for tests: the questions the asking
    /// seams hold right now, by id.
    pub fn outstanding_questions(&self) -> Vec<OutstandingQuestion> {
        let book = self.questions.lock().unwrap();
        let mut qs: Vec<OutstandingQuestion> =
            book.open.values().map(|(q, _)| q.clone()).collect();
        qs.sort_by(|a, b| a.id.cmp(&b.id));
        qs
    }

    fn publish_questions(&self) {
        if let Some(board) = self.question_board.get() {
            board.publish(&self.outstanding_questions());
        }
    }

    /// Point spills at the directory beside the session log. Called by the
    /// consumer that owns the log's path — at construction and after every
    /// [`Agent::adopt_session`], beside [`Dispatcher::set_cwd`]. `None`
    /// spills into the tools' working directory instead.
    pub fn set_spill_dir(&self, dir: Option<PathBuf>) {
        *self.spill_dir.write().unwrap() = dir;
    }

    /// The spill directory for a log path: `<log>.spills/` beside it, so a
    /// session hop carries both. What the consumer hands to
    /// [`Dispatcher::set_spill_dir`].
    pub fn spill_dir_for(log: &std::path::Path) -> PathBuf {
        let mut dir = log.to_path_buf();
        dir.set_extension("spills");
        dir
    }

    /// Watch every call this chokepoint runs. Set once, and only for an
    /// observer that could not be handed over at construction — the
    /// registry owns it, and the registry is built before the dispatcher.
    pub fn observe(&self, observer: Arc<dyn ToolObserver>) {
        let _ = self.observer.set(observer);
    }

    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
    }

    pub fn user(&self) -> &Arc<dyn UserIo> {
        &self.user
    }

    pub fn bus(&self) -> &EventBus {
        &self.bus
    }

    pub fn session(&self) -> &Arc<Mutex<Session>> {
        &self.session
    }

    pub fn cwd(&self) -> std::path::PathBuf {
        self.cwd.read().unwrap().clone()
    }

    /// Point the tools at a different directory. Takes effect on the next
    /// call; a tool already running keeps the directory it was given.
    pub fn set_cwd(&self, cwd: std::path::PathBuf) {
        *self.cwd.write().unwrap() = cwd;
    }

    /// Where a spilled output is written: one file per call in the spill
    /// directory, or beside the tools' working directory when no log has
    /// been named. The name derives from the call id, so a replayed call
    /// re-spills to the same path rather than accumulating files.
    fn spill_path(&self, call: &ToolCall) -> PathBuf {
        let dir = self
            .spill_dir
            .read()
            .unwrap()
            .clone()
            .unwrap_or_else(|| self.cwd());
        let safe_id: String = call
            .id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .take(48)
            .collect();
        let safe_tool: String = call
            .name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .take(24)
            .collect();
        dir.join(format!("{safe_id}-{safe_tool}.txt"))
    }

    /// Cap one call's output for context: short outputs pass through; a
    /// long one is written whole to its spill file and replaced by its
    /// head plus the path. Error outputs are never spilled — they are the
    /// model's debugging, and a denial or a traceback arrives short. Nor is
    /// a call that named its own read length (see [`named_read_length`]).
    fn spill(&self, call: &ToolCall, output: ToolOutput) -> ToolOutput {
        if output.is_error
            || named_read_length(call)
            || output.content.chars().count() <= MAX_INLINE_CHARS
        {
            return output;
        }
        let path = self.spill_path(call);
        let head: String = output.content.chars().take(SPILL_HEAD_CHARS).collect();
        let total = output.content.chars().count();
        let receipt = match write_spill(&path, &output.content) {
            Ok(bytes) => format!(
                "{head}\n[…output too long for context ({total} chars); the whole {bytes} bytes are in {} — page it with `read` (offset/limit)]",
                path.display()
            ),
            Err(e) => {
                tracing::error!(error = %format!("{e:#}"), path = %path.display(), "failed to spill a tool output");
                format!(
                    "{head}\n[…output too long for context ({total} chars); the spill file could not be written ({e:#}), so only the head is shown]"
                )
            }
        };
        ToolOutput {
            content: receipt,
            is_error: false,
            ends_turn: false,
        }
    }

    /// Run one tool call end to end. Never returns `Err`: every failure
    /// mode is an error *output* the model can read, because the
    /// alternative — the loop aborting on a bad tool — is worse for the
    /// user in every case.
    pub async fn dispatch(&self, call: ToolCall, cancel: CancellationToken) -> ToolOutput {
        self.dispatch_with(call, None, cancel).await
    }

    /// The same, for a call the **operator** wrote, carrying the words they
    /// wrote it in — a dispatch-mode utterance, or `eidolon do`'s argument.
    ///
    /// The words are worth journaling because the call is a *reading* of
    /// them: a classifier that resolved "read the note Groceries" as
    /// `get_index` produced a truthful record of what ran and a useless
    /// record of what was wanted, and only the pair says what happened.
    pub async fn dispatch_user(
        &self,
        call: ToolCall,
        utterance: &str,
        cancel: CancellationToken,
    ) -> ToolOutput {
        self.dispatch_with(call, Some(utterance.to_string()), cancel)
            .await
    }

    async fn dispatch_with(
        &self,
        call: ToolCall,
        utterance: Option<String>,
        cancel: CancellationToken,
    ) -> ToolOutput {
        self.bus.publish(Event::ToolCallStarted(call.clone()));

        // A replayed call (resume after crash) already has an answer, and
        // that answer is already a record — so the event names it, exactly
        // as a fresh one does.
        if let Some((record, out)) = self.replayed(&call).await {
            self.bus.publish(Event::ToolCallFinished {
                record: Some(record),
                call,
                output: out.clone(),
            });
            return out;
        }

        // A call the model made is already on the branch, inside the
        // assistant message that asked for it. One the operator made is
        // not, so it is written here — before it runs, so that a crash
        // between the two leaves a call without a result rather than a
        // result belonging to nothing.
        if call.origin == CallOrigin::User {
            self.journal_user_call(&call, utterance).await;
        }

        let (output, operator_note) = self.execute(&call, cancel).await;
        // Spill before journaling: the record holds the bounded text the
        // model was actually given, so a resume replays the pointer and not
        // the flood — and re-running the spill would rename nothing, the
        // path derives from the call id.
        let mut output = self.spill(&call, output);
        // After the spill, so the spill and its file hold only what the tool wrote.
        output.content = with_operator_note(&output.content, &call.id, operator_note.as_ref());
        let record = self.journal(&call, &output).await;
        self.bus.publish(Event::ToolCallFinished {
            record,
            call,
            output: output.clone(),
        });
        output
    }

    async fn replayed(&self, call: &ToolCall) -> Option<(RecordId, ToolOutput)> {
        let session = self.session.lock().await;
        session
            .branch()
            .into_iter()
            .rev()
            .find_map(|r| match &r.kind {
                RecordKind::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } if tool_use_id == &call.id => Some((
                    r.id,
                    ToolOutput {
                        content: content.clone(),
                        is_error: *is_error,
                        // A replayed answer resumes the turn it belonged
                        // to; it does not re-end it. See
                        // [`ToolOutput::ends_turn`].
                        ends_turn: false,
                    },
                )),
                _ => None,
            })
    }

    /// Policy verdict plus, on `Ask`, the user's answer and whatever note
    /// came with it. `Err` carries the reason the call must not run. Fails
    /// closed on a hook failure.
    pub async fn adjudicate(
        &self,
        call: &ToolCall,
        manifest: &ToolManifest,
        cancel: &CancellationToken,
    ) -> (Result<(), String>, Option<Words>) {
        let cwd = self.cwd();
        let ruling = match catch(self.policy.pre_tool(call, manifest, &cwd)).await {
            Ok(r) => r,
            Err(reason) => Ruling::deny(format!("policy hook failed ({reason}); denied")),
        };
        let (result, outcome, operator_note) = match &ruling.verdict {
            // A question the operator's armed switch answered before it was
            // a question. It journals — unlike the silent yes below —
            // because the record of what ran while the gate was blind is
            // the whole of what a yolo session leaves behind.
            Verdict::Allow if ruling.yolo => (Ok(()), PolicyOutcome::Yolo, None),
            // A question the store answered before it was asked: the vouch
            // the operator granted covered this call, and the table's
            // fallthrough never became a question at all. It journals with
            // the grant's naming in the note, because a silent yes here
            // would be indistinguishable from a table row's — and the whole
            // point of a vouch is that it is *data* said yes, a record the
            // operator wrote once, not a rule they hand-edited. See
            // [`Ruling::vouched`].
            //
            // No operator note: nobody was asked, so there are no words
            // said at a dialog to carry.
            Verdict::Allow if ruling.vouched.is_some() => (Ok(()), PolicyOutcome::Approved, None),
            // The only path that journals nothing. See
            // [`RecordKind::PolicyVerdict`] for why a silent yes stays
            // silent in the log too.
            Verdict::Allow => return (Ok(()), None),
            Verdict::Deny(reason) => (
                Err(format!("denied by policy: {reason}")),
                PolicyOutcome::Refused,
                None,
            ),
            // A question the context-aware layer already answered. It reached
            // that answer without running anything, and it could only ever
            // have reached it *here*, on a call the deterministic pass had
            // decided to raise — so what is left is to record that a model
            // and not the operator said yes, and to say so on the bus.
            Verdict::Ask(_) if ruling.judged.is_some() => (Ok(()), PolicyOutcome::Judged, None),
            Verdict::Ask(prompt) => {
                self.bus.publish(Event::AskUser {
                    prompt: prompt.clone(),
                });
                // Registered while the seam blocks, so the door can say
                // what an attempt is aimed at: a peer's answer to a gate
                // question is refused with the reason and journals
                // nothing — approval decides what runs, and that is the
                // operator's alone. The guard is what removes the entry
                // when the dialog ends, one way or the other.
                let _question = self.question_open(QuestionKind::Approve, &call.id, prompt.clone());
                let consent = self.user.approve(call, manifest, &ruling, cancel).await;
                if consent.given {
                    (Ok(()), PolicyOutcome::Approved, consent.operator_note)
                } else {
                    (
                        Err("the user declined this tool call".into()),
                        PolicyOutcome::Declined,
                        consent.operator_note,
                    )
                }
            }
        };
        self.journal_verdict(call, &ruling, outcome).await;
        (result, operator_note)
    }

    /// Write what the gate decided, and publish it.
    ///
    /// In `adjudicate` rather than in `dispatch` because this is the one
    /// place a verdict exists, and all three rungs reach it. That includes
    /// the two that journal nothing else: a backend running its own tools
    /// reports its calls and results on its own stream, but no stream
    /// carries *the harness's* decision about them, so this is the only
    /// record of a gate that fired on that path.
    async fn journal_verdict(&self, call: &ToolCall, ruling: &Ruling, outcome: PolicyOutcome) {
        let reason = ruling.reason_or_prompt().to_string();
        let mut session = self.session.lock().await;
        if let Err(e) = session.append(RecordKind::PolicyVerdict {
            tool_use_id: call.id.clone(),
            tool: call.name.clone(),
            reason: reason.clone(),
            structural: ruling.structural,
            outcome: outcome.clone(),
            // The judge's reasoning on `Judged`; on the store's yes, the
            // grant's naming. One slot, two producers, never both — each
            // says who answered, which is the fact a ledger reads.
            note: ruling.judged.clone().or_else(|| ruling.vouched.clone()),
        }) {
            tracing::error!(error = %e, "failed to journal a policy verdict");
            self.bus.publish(Event::Error(format!(
                "failed to journal a policy verdict: {e}"
            )));
        }
        drop(session);
        self.bus.publish(Event::PolicyVerdict {
            tool: call.name.clone(),
            reason,
            outcome,
            note: ruling.judged.clone().or_else(|| ruling.vouched.clone()),
        });
    }

    /// An unknown tool name is usually a near miss (`read_file` for `read`,
    /// a name the model invented, a name it hallucinated from an example).
    /// Listing what exists turns a dead end into one retry.
    fn unknown_tool(&self, name: &str) -> String {
        let available: Vec<String> = self
            .registry
            .manifests()
            .into_iter()
            .map(|m| m.name)
            .collect();
        let mut msg = format!("unknown tool `{name}`");
        if let Some(near) = nearest(name, &available) {
            msg.push_str(&format!("; did you mean `{near}`?"));
        }
        if !available.is_empty() {
            msg.push_str(&format!(" Available tools: {}.", available.join(", ")));
        }
        msg
    }

    /// Policy, validation and execution — but no journal and no events.
    ///
    /// The third rung of the ladder. [`Self::adjudicate`] decides without
    /// running; this decides and runs; [`Self::dispatch`] decides, runs,
    /// journals and publishes. A backend that reports tool activity on its
    /// own stream (the Claude CLI, whose MCP server calls this) needs the
    /// middle rung: the harness must own the policy decision and perform the
    /// work, while the log and the event bus stay driven by the stream, in
    /// stream order. Journaling here as well would write each result twice —
    /// once unpaired — and would land the result *before* the assistant
    /// message that contains its `tool_use`, because the backend runs the
    /// tool before it reports the message.
    ///
    /// Every outcome, including the refusals that never ran, is then handed
    /// to the [`ToolObserver`] if one is attached — see that trait for why
    /// this is telemetry and not a second decision. The operator's note on
    /// the call comes back beside the output, for the caller to join after
    /// whatever it does to the output.
    pub async fn execute(&self, call: &ToolCall, cancel: CancellationToken) -> (ToolOutput, Option<Words>) {
        let (output, operator_note) = self.run(call, cancel).await;
        if let Some(observer) = self.observer.get().cloned() {
            // Caught, and its answer dropped: an observer that panics or
            // hangs is its own problem, and this call has already happened.
            // `catch` is the same net the tools themselves run under.
            let _ = catch(observer.observed(call, &output)).await;
        }
        (output, operator_note)
    }

    /// [`Self::execute`]'s body: lookup, validation, policy, run.
    async fn run(&self, call: &ToolCall, cancel: CancellationToken) -> (ToolOutput, Option<Words>) {
        let Some(tool) = self.registry.get(&call.name).cloned() else {
            return (ToolOutput::error(self.unknown_tool(&call.name)), None);
        };
        let manifest = tool.manifest().clone();

        // Check the input against the manifest before the tool sees it. A
        // tool implementation reports a bad input in its own vocabulary — a
        // Rune script raises an interpreter error naming a Rune type — and
        // none of that tells the caller which field to fix. This does.
        //
        // First the quiet repair: a streaming model sometimes emits an
        // object-typed parameter as that object's JSON text, which the
        // server's serde will reject and the model will re-emit unchanged —
        // a loop only the harness can break. What converts losslessly is
        // accepted, exactly as stringly numbers already are.
        let mut input = call.input.clone();
        crate::schema::coerce(&manifest.input_schema, &mut input);
        if let Err(reason) = crate::schema::validate(&call.name, &manifest.input_schema, &input) {
            return (ToolOutput::error(reason), None);
        }

        let (cleared, operator_note) = self.adjudicate(call, &manifest, &cancel).await;
        let output = match cleared {
            Ok(()) => self.invoke(tool, input, call, cancel).await,
            Err(reason) => ToolOutput::error(reason),
        };
        (output, operator_note)
    }

    async fn invoke(&self, tool: Arc<dyn Tool>, input: serde_json::Value, call: &ToolCall, cancel: CancellationToken) -> ToolOutput {
        if cancel.is_cancelled() {
            return ToolOutput::error("cancelled before the tool ran");
        }

        let ctx = CallContext {
            cwd: self.cwd(),
            cancel: cancel.clone(),
            call_id: call.id.clone(),
        };
        let fut = tool.call(input, ctx);
        let result = tokio::select! {
            r = catch(fut) => r,
            _ = cancel.cancelled() => return ToolOutput::error("cancelled while the tool was running"),
        };
        match result {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => ToolOutput::error(format!("{e:#}")),
            Err(panic) => ToolOutput::error(format!("tool `{}` panicked: {panic}", call.name)),
        }
    }

    async fn journal_user_call(&self, call: &ToolCall, utterance: Option<String>) {
        let mut session = self.session.lock().await;
        if let Err(e) = session.append(RecordKind::UserToolCall {
            tool_use_id: call.id.clone(),
            name: call.name.clone(),
            input: crate::message::Json(call.input.to_string()),
            utterance,
        }) {
            tracing::error!(error = %e, "failed to journal a user tool call");
            self.bus.publish(Event::Error(format!(
                "failed to journal a user tool call: {e}"
            )));
        }
    }

    /// Journal the result, and say where it landed so the event can name
    /// it. `None` when the append failed — the call still happened and the
    /// consumer still hears about it, it simply has no record to point at.
    async fn journal(&self, call: &ToolCall, output: &ToolOutput) -> Option<RecordId> {
        let mut session = self.session.lock().await;
        match session.append(RecordKind::ToolResult {
            tool_use_id: call.id.clone(),
            content: output.content.clone(),
            is_error: output.is_error,
        }) {
            Ok(id) => Some(id),
            Err(e) => {
                tracing::error!(error = %e, "failed to journal tool result");
                self.bus
                    .publish(Event::Error(format!("failed to journal tool result: {e}")));
                None
            }
        }
    }
}

/// Outputs at or under this many characters travel inline. Above it they
/// spill to a file and only the head goes to context. 8k chars is ~2k
/// tokens: a full default `read` (2000 short lines) still fits, while a
/// build log does not — and a `read` that names its own `limit` skips the
/// cap altogether ([`named_read_length`]).
pub const MAX_INLINE_CHARS: usize = 8_000;
/// How much of a spilled output stays inline, ahead of the spill path.
pub const SPILL_HEAD_CHARS: usize = 2_000;

/// Whether the call names its own read length: a value under one of
/// [`READ_LENGTH_KEYS`], anywhere in its arguments — at the call's own
/// level or inside the envelope a wrapper passes them through.
///
/// The cap exists for output nobody chose — a build log, a whole-file dump,
/// whatever a `bash` command decided to say. A length is the caller saying
/// how much it is about to pull into context, which is the one thing the
/// cap cannot know better than it does.
///
/// Spilling it is also what makes a chain. A spilled output's receipt says
/// "page it with `read` (offset/limit)"; the model does exactly that, the
/// page is over the cap again, and the answer is a pointer to a *second*
/// file holding the same text under a new name. Every step obeys the
/// receipt and no step gets closer to the bytes, until the context is spent
/// on heads and paths. A named length travels whole however long it is.
///
/// The walk goes through nested objects because the length is not always at
/// the call's own level: `read` takes `limit` beside `path`, while the
/// vault's `mneme_rpc` is `{function, args}` and pages with `args.limit` —
/// `read_note` called with `offset`/`limit` under `args` is the same
/// page-length request wearing the envelope, and a model obeying the
/// receipt would otherwise re-spill on every call. Which word a tool picked
/// for its envelope is not the dispatcher's to know, so any nested object
/// is walked rather than `args` by name.
fn named_read_length(call: &ToolCall) -> bool {
    names_a_length(&call.input)
}

/// The words a call has used for how much it wants back. **`limit` is the
/// surface's own**: `read`, `grep`, `fetch` and `search` all name it that
/// now (standardized 2026-09-29 — they had shipped `limit`, `max`, `max`
/// and `count`, four words and three units for one idea), and the vault's
/// functions below `mneme_rpc` take `limit` too.
///
/// The older spellings stay readable, because the dispatcher does not get
/// to rename what it is handed. A built-in's copy on disk is a live source
/// — an older release's seed that an upgrade has not refreshed, or the
/// operator's own edit — and a foreign tool's schema (the vault's, a
/// user's wrapper) is not the harness's to standardize; a call that names
/// `max` there means what a `limit` means here.
const READ_LENGTH_KEYS: &[&str] = &["limit", "max", "count"];

/// A read length in this value or in any object under it.
fn names_a_length(value: &serde_json::Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    object.iter().any(|(key, v)| {
        (READ_LENGTH_KEYS.contains(&key.as_str()) && is_a_length(v)) || names_a_length(v)
    })
}

/// A value that names a length: a positive count, or the same count
/// streamed as a string — the schema's coercion already takes `"40"` as 40,
/// and the model that emitted it meant the same thing.
fn is_a_length(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Number(n) => n.as_u64(),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
    .is_some_and(|limit| limit > 0)
}

/// Write a spilled output whole. Parent directories are created; the bytes
/// are the output's, and the count is returned for the receipt.
fn write_spill(path: &std::path::Path, content: &str) -> anyhow::Result<usize> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(path, content).with_context(|| format!("writing {}", path.display()))?;
    Ok(content.len())
}

/// Run a future, turning a panic into an `Err(String)`.
async fn catch<F: Future>(fut: F) -> Result<F::Output, String> {
    AssertUnwindSafe(fut).catch_unwind().await.map_err(|p| {
        if let Some(s) = p.downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = p.downcast_ref::<String>() {
            s.clone()
        } else {
            "non-string panic payload".to_string()
        }
    })
}

/// The closest candidate within a small edit distance, or `None`. Scaled by
/// length so `read`/`grep` (distance 4) never suggest each other while
/// `read`/`reed` does.
fn nearest<'a>(name: &str, candidates: &'a [String]) -> Option<&'a str> {
    let limit = (name.len() / 3).max(1);
    candidates
        .iter()
        .map(|c| (distance(name, c), c.as_str()))
        .filter(|(d, _)| *d <= limit)
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c)
}

fn distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let sub = prev[j - 1] + usize::from(a[i - 1] != b[j - 1]);
            cur[j] = sub.min(prev[j] + 1).min(cur[j - 1] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}
