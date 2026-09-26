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
use crate::tool::{CallContext, CallOrigin, ToolCall, ToolManifest, ToolOutput, ToolRegistry};
use crate::user::UserIo;

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
    /// model's debugging, and a denial or a traceback arrives short.
    fn spill(&self, call: &ToolCall, output: ToolOutput) -> ToolOutput {
        if output.is_error || output.content.chars().count() <= MAX_INLINE_CHARS {
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

        let output = self.execute(&call, cancel).await;
        // Spill before journaling: the record holds the bounded text the
        // model was actually given, so a resume replays the pointer and not
        // the flood — and re-running the spill would rename nothing, the
        // path derives from the call id.
        let output = self.spill(&call, output);
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

    /// Policy verdict plus, on `Ask`, the user's answer. `Err` carries the
    /// reason the call must not run. Fails closed on a hook failure.
    pub async fn adjudicate(
        &self,
        call: &ToolCall,
        manifest: &ToolManifest,
        cancel: &CancellationToken,
    ) -> Result<(), String> {
        let cwd = self.cwd();
        let ruling = match catch(self.policy.pre_tool(call, manifest, &cwd)).await {
            Ok(r) => r,
            Err(reason) => Ruling::deny(format!("policy hook failed ({reason}); denied")),
        };
        let (result, outcome) = match &ruling.verdict {
            // A question the operator's armed switch answered before it was
            // a question. It journals — unlike the silent yes below —
            // because the record of what ran while the gate was blind is
            // the whole of what a yolo session leaves behind.
            Verdict::Allow if ruling.yolo => (Ok(()), PolicyOutcome::Yolo),
            // The only path that journals nothing. See
            // [`RecordKind::PolicyVerdict`] for why a silent yes stays
            // silent in the log too.
            Verdict::Allow => return Ok(()),
            Verdict::Deny(reason) => (
                Err(format!("denied by policy: {reason}")),
                PolicyOutcome::Refused,
            ),
            // A question the context-aware layer already answered. It reached
            // that answer without running anything, and it could only ever
            // have reached it *here*, on a call the deterministic pass had
            // decided to raise — so what is left is to record that a model
            // and not the operator said yes, and to say so on the bus.
            Verdict::Ask(_) if ruling.judged.is_some() => (Ok(()), PolicyOutcome::Judged),
            Verdict::Ask(prompt) => {
                self.bus.publish(Event::AskUser {
                    prompt: prompt.clone(),
                });
                if self.user.approve(call, manifest, &ruling, cancel).await {
                    (Ok(()), PolicyOutcome::Approved)
                } else {
                    (
                        Err("the user declined this tool call".into()),
                        PolicyOutcome::Declined,
                    )
                }
            }
        };
        self.journal_verdict(call, &ruling, outcome).await;
        result
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
            note: ruling.judged.clone(),
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
            note: ruling.judged.clone(),
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
    /// this is telemetry and not a second decision.
    pub async fn execute(&self, call: &ToolCall, cancel: CancellationToken) -> ToolOutput {
        let output = self.run(call, cancel).await;
        if let Some(observer) = self.observer.get().cloned() {
            // Caught, and its answer dropped: an observer that panics or
            // hangs is its own problem, and this call has already happened.
            // `catch` is the same net the tools themselves run under.
            let _ = catch(observer.observed(call, &output)).await;
        }
        output
    }

    /// [`Self::execute`]'s body: lookup, validation, policy, run.
    async fn run(&self, call: &ToolCall, cancel: CancellationToken) -> ToolOutput {
        let Some(tool) = self.registry.get(&call.name).cloned() else {
            return ToolOutput::error(self.unknown_tool(&call.name));
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
            return ToolOutput::error(reason);
        }

        if let Err(reason) = self.adjudicate(call, &manifest, &cancel).await {
            return ToolOutput::error(reason);
        }

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
/// build log does not.
pub const MAX_INLINE_CHARS: usize = 8_000;
/// How much of a spilled output stays inline, ahead of the spill path.
pub const SPILL_HEAD_CHARS: usize = 2_000;

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
