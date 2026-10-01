//! The workflow interpreter: how a plugin's `workflows/*.rn` becomes a run
//! the log can replay.
//!
//! A workflow is a Rune program with two entry points, the tool contract's
//! shape one step out: `pub fn workflow()` returns the metadata the door and
//! (later) the model see, and `pub async fn run(args)` is the program. It is
//! compiled with the plugin's `lib/*.rn` in front of it — one unit sharing a
//! namespace, the relationship a `ui.rn` has to its `prelude.rn` — and it
//! runs on a thread of its own, the same arrangement [`ScriptTool`] uses.
//!
//! ## The scope is the invariant
//!
//! Every script the host compiles as a *tool* gets all the primitives; a
//! workflow gets **its three doors and nothing else** — `tool_call`, the
//! `park`, and `ask_model`. Its compile path is a second context
//! ([`Host::workflow_compiler`]) whose `eidolon` module registers those
//! names and no others, so a workflow that reaches for
//! `fs_write`, `shell` or `api_request` does not fail a policy check — it
//! fails to **compile**. There is no effect a workflow can cause that does
//! not enter as a journaled step the one gate classifies, which is the
//! design's sentence made true of the payload it was written about.
//!
//! ## A step, and what makes a resume a replay
//!
//! The script names a tool and hands it input; the interpreter does the rest,
//! in this order: it takes the run's next index, journals
//! `WorkflowStep { run, index, call_id, tool, input }` — the request, *before*
//! the chokepoint sees it — and dispatches under the run's own deterministic
//! id `w<run>-<index>`, so the `ToolResult` the dispatcher journals is the
//! answer *that id* holds. A resume re-runs the program, and at step k the
//! branch already says what step k asked and what it got:
//!
//! - the recorded request is compared against the one this pass built, and a
//!   mismatch is a refusal — the same program asking something different is
//!   not the run that was journaled;
//! - a recorded answer is returned as the memo, and nothing runs — the
//!   chokepoint's replay check answers before the gate, because the answer on
//!   the branch already carries the verdict that produced it;
//! - a request whose answer is missing is the parked tail: it falls through
//!   to a dispatch under the recorded id, which is the one shape that
//!   re-aches the gate on a resume, and exactly where a park's question gets
//!   asked again.
//!
//! The thread the program runs on inherits the dispatch chain
//! ([`crate::host::with_tool`]) under a tag naming the *run* —
//! `triage#17`, not just a tool name — so the depth limit's error names
//! which run started which, as the design's nesting rule wants.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use anyhow::{Context as _, anyhow, bail};
use rune::termcolor::Buffer;
use rune::{Diagnostics, Source, Sources, Unit, Vm};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use eidolon_core::dispatch::{Dispatcher, QuestionKind};
use eidolon_core::message::Json;
use eidolon_core::policy::Approval;
use eidolon_core::session::RecordKind;
use eidolon_core::session::RecordId;
use eidolon_core::workflow::{RunBudget, RunPin, RunStatus};
use eidolon_core::tool::{ToolOutput};

use crate::host::{Host, with_cancel, with_tool};
use crate::script::{interpret, json_to_rune, rune_to_json};

/// The most steps a run may take. A plugin's `workflow()` may lower this for
/// itself; nothing may raise it. Over the cap the run fails naming the
/// reason — a runaway walk must end in an answer, not an budget line.
pub const MAX_WORKFLOW_STEPS: u32 = 200;

/// What one `workflow()` returns, read once at compile like a `manifest()`.
#[derive(Clone, Debug, Deserialize)]
pub struct WorkflowMeta {
    /// The workflow's name. Defaults to the file stem the compile was given.
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// The params schema the door (and later the model) sees.
    #[serde(default)]
    pub params: serde_json::Value,
    /// The approval class the door gates a start with. A workflow that only
    /// dispatches already-gated tools still declares its own class — the
    /// start is the call the operator sees.
    #[serde(default)]
    pub approval: Approval,
    /// A cap the plugin lowers for itself. The harness cap
    /// ([`MAX_WORKFLOW_STEPS`]) is the ceiling; the effective cap is the
    /// smaller of the two.
    #[serde(default)]
    pub step_cap: Option<u32>,
}

/// How a run ended. The report is the driving call's result; `Failed`
/// carries the reason; `Parked` carries the question, which is what the
/// driving call hands to a person — a parked run that reported success
/// would be a lie about a run waiting on someone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunOutcome {
    Completed { report: ToolOutput },
    Failed { reason: String },
    Parked { question: String },
}

/// A compiled workflow: the unit, the metadata, and the hash the pin
/// carries. `start` mints the run; `resume` replays one the branch holds.
pub struct Workflow {
    meta: WorkflowMeta,
    name: String,
    plugin: String,
    version: String,
    hash: String,
    unit: Arc<Unit>,
    host: Arc<Host>,
}

impl Workflow {
    /// Compile `src` with `lib` in front of it, against the workflow scope's
    /// one context, and read `workflow()`. The hash is over the source set
    /// actually compiled — every lib file and the workflow, length-prefixed
    /// in order — because that is what a resume must refuse a difference in.
    pub fn compile(
        host: &Arc<Host>,
        plugin: &str,
        version: &str,
        name: &str,
        src: &str,
        lib: &[String],
    ) -> anyhow::Result<Arc<Self>> {
        let compiler = host.workflow_compiler()?;
        let mut sources = Sources::new();
        let mut insert = |label: String, body: &str| -> anyhow::Result<()> {
            sources
                .insert(
                    Source::new(label.clone(), body)
                        .map_err(|e| anyhow!("source {label}: {e}"))?,
                )
                .map_err(|e| anyhow!("inserting {label}: {e}"))?;
            Ok(())
        };
        for (i, file) in lib.iter().enumerate() {
            insert(format!("lib/{i}"), file)?;
        }
        insert(name.to_string(), src)?;
        let mut diagnostics = Diagnostics::new();
        let unit = match rune::prepare(&mut sources)
            .with_context(&compiler.context)
            .with_diagnostics(&mut diagnostics)
            .build()
        {
            Ok(u) => u,
            Err(_) => {
                let mut buf = Buffer::no_color();
                let rendered = match diagnostics.emit(&mut buf, &sources) {
                    Ok(()) => String::from_utf8_lossy(buf.as_slice()).into_owned(),
                    Err(e) => format!("<could not render diagnostics: {e}>"),
                };
                bail!("workflow `{name}` failed to compile:\n{}", rendered.trim());
            }
        };
        let unit = Arc::new(unit);

        // The metadata, read once like a manifest.
        let mut vm = Vm::new(compiler.runtime.clone(), unit.clone());
        let value = vm
            .execute(["workflow"], ())
            .map_err(|e| anyhow!("workflow has no `pub fn workflow()`: {e}"))?
            .complete()
            .into_result()
            .map_err(|e| anyhow!("workflow() failed: {e}"))?;
        let json = rune_to_json(&value).context("workflow() must return a plain object")?;
        let mut meta: WorkflowMeta =
            serde_json::from_value(json).context("workflow() has the wrong shape")?;
        if meta.name.is_empty() {
            meta.name = name.to_string();
        }

        // The pin's hash, over what was actually compiled.
        let mut hasher = Sha256::new();
        let set: Vec<(String, &str)> = lib
            .iter()
            .enumerate()
            .map(|(i, f)| (format!("lib/{i}"), f.as_str()))
            .chain([(name.to_string(), src)])
            .collect();
        for (label, body) in set {
            hasher.update((label.len() as u64).to_le_bytes());
            hasher.update(label.as_bytes());
            hasher.update((body.len() as u64).to_le_bytes());
            hasher.update(body.as_bytes());
        }
        let hash: String = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();

        Ok(Arc::new(Workflow {
            meta,
            name: name.to_string(),
            plugin: plugin.to_string(),
            version: version.to_string(),
            hash,
            unit,
            host: host.clone(),
        }))
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn hash(&self) -> &str {
        &self.hash
    }

    /// Start a run: journal the pin — the argument map it carries is the one
    /// the program runs on, on this pass and every resume — and execute.
    /// The caller's budget, when one was set, is journaled with the pin and
    /// is the only budget this run ever has: a resume reads it from the
    /// branch and cannot name another.
    pub async fn start(
        self: &Arc<Self>,
        d: &Arc<Dispatcher>,
        args: Json,
        budget: RunBudget,
        cancel: CancellationToken,
    ) -> anyhow::Result<RunOutcome> {
        let run = d
            .session()
            .lock()
            .await
            .start_workflow(RunPin {
                plugin: self.plugin.clone(),
                workflow: self.meta.name.clone(),
                version: self.version.clone(),
                hash: self.hash.clone(),
                args: args.clone(),
            })
            .context("journaling the run's pin")?;
        d.session()
            .lock()
            .await
            .workflow_budget(run, budget)
            .context("journaling the run's budget")?;
        self.execute(d, run, budget, args, cancel).await
    }

    /// Resume a run the branch holds: refuse a program that is not the one
    /// the run started under, refuse a hole, refuse an ended-but-unanswered
    /// tail, and replay — steps with answers take them from the log, the
    /// parked tail re-dispatches, steps past the journal run fresh.
    pub async fn resume(
        self: &Arc<Self>,
        d: &Arc<Dispatcher>,
        run: RecordId,
        cancel: CancellationToken,
    ) -> anyhow::Result<RunOutcome> {
        let back = d
            .session()
            .lock()
            .await
            .workflow_run(run)
            .ok_or_else(|| anyhow!("no run #{run} on this branch"))?;
        if back.pin.hash != self.hash {
            bail!(
                "refusing run #{run}: it was started under hash {} and the program at hand is {}",
                back.pin.hash,
                self.hash
            );
        }
        if let Some(gap) = back.gap() {
            bail!(
                "refusing run #{run}: step {gap} cannot be replayed — a hole is a refusal, never a re-run"
            );
        }
        // The tail check the spec leaves to the interpreter, made here where
        // the ending is actually read: a result-less last step is the parked
        // shape only, and only when the run did not claim finality over an
        // answer it never got. `None` is a run that died mid-flight — the
        // resume is the thing that gives it an ending.
        if let Some(last) = back.steps.last()
            && last.result.is_none()
            && matches!(back.end, Some(RunStatus::Completed) | Some(RunStatus::Failed))
        {
            bail!(
                "refusing run #{run}: it ended {:?} with step {} unanswered",
                back.end,
                last.index
            );
        }
        // The budget a resume cannot escape: read from the branch, the same
        // record the start journaled, never from the caller — the resume
        // call carries no budget and cannot name a larger one.
        let budget = d
            .session()
            .lock()
            .await
            .run_budget(run)
            .unwrap_or_default();
        self.execute(d, run, budget, back.pin.args.clone(), cancel)
            .await
    }

    /// Run the program on a thread of its own, under the run scope the step
    /// door reads. The program is raced against the run token — a child of
    /// the caller's, so a park inside the program stops the *run* and never
    /// the driving call — and the ending is journaled here, after the thread
    /// answers, because this is where the outcome exists as a fact the log
    /// can hold; a resume appends its own, and the latest one wins.
    async fn execute(
        self: &Arc<Self>,
        d: &Arc<Dispatcher>,
        run: RecordId,
        budget: RunBudget,
        args: Json,
        cancel: CancellationToken,
    ) -> anyhow::Result<RunOutcome> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let wf = self.clone();
        let host = self.host.clone();
        let tag = format!("{}#{}", self.meta.name, run);
        let parent_chain = crate::host::current_chain();
        // The steps a run may take: the plugin's own cap under the harness
        // ceiling. The caller's budget is not folded into this number — it
        // is checked beside it and named in its own refusal, because a
        // bound someone set from outside the plugin is a different fact
        // from a cap the plugin chose for itself.
        let cap = self
            .meta
            .step_cap
            .unwrap_or(MAX_WORKFLOW_STEPS)
            .min(MAX_WORKFLOW_STEPS);
        // The token a park pulls down: the program's, not the caller's.
        let run_token = cancel.child_token();
        let race_token = run_token.clone();
        std::thread::Builder::new()
            .name(format!("workflow-{}", self.meta.name))
            .spawn(move || {
                let result = with_run_scope(
                    RunScope::new(run, cap, budget),
                    || {
                        with_tool(&tag, None, parent_chain, || {
                            with_cancel(run_token, || -> anyhow::Result<RunOutcome> {
                                let runtime = host.workflow_compiler()?.runtime.clone();
                                let rt = tokio::runtime::Builder::new_current_thread()
                                    .enable_all()
                                    .build()?;
                                rt.block_on(async move {
                                    let args = json_to_rune(
                                        &args.0
                                            .parse::<serde_json::Value>()
                                            .context("the run's args are not JSON")?,
                                    );
                                    // The program against the token: a park
                                    // never resolves its own future, so the
                                    // only way the cancelled arm fires is the
                                    // token a park pulled down or a caller
                                    // that gave up — the run scope says
                                    // which. Dropping the program mid-await
                                    // is the path a cancelled tool already
                                    // exercises.
                                    tokio::select! {
                                        value = async {
                                            let mut vm = Vm::new(runtime, wf.unit.clone());
                                            let mut exec = vm.execute(["run"], (args,))
                                                .map_err(|e| anyhow!("workflow has no `pub async fn run(args)`: {e}"))?;
                                            let value = exec
                                                .async_complete()
                                                .await
                                                .into_result()
                                                .map_err(|e| anyhow!("run() failed: {e}"))?;
                                            Ok(interpret_outcome(value))
                                        } => value,
                                        _ = race_token.cancelled() => match park_taken() {
                                            Some(question) => Ok(RunOutcome::Parked { question }),
                                            None => Ok(RunOutcome::Failed {
                                                reason: "the run was cancelled".into(),
                                            }),
                                        },
                                    }
                                })
                            })
                        })
                    },
                );
                let _ = tx.send(result);
            })
            .context("spawning the workflow thread")?;
        let outcome = rx
            .await
            .context("the workflow thread ended without a result")??;
        let status = match &outcome {
            RunOutcome::Completed { .. } => RunStatus::Completed,
            RunOutcome::Failed { .. } => RunStatus::Failed,
            RunOutcome::Parked { .. } => RunStatus::Parked,
        };
        d.session()
            .lock()
            .await
            .end_workflow(run, status)
            .context("journaling the run's ending")?;
        Ok(outcome)
    }
}

/// The report a `run()` returns, read the way a tool's return is — a string,
/// or `#{content, is_error}` — and wrapped as the outcome the door maps.
fn interpret_outcome(value: rune::Value) -> RunOutcome {
    match interpret(value) {
        out if !out.is_error => RunOutcome::Completed { report: out },
        out => RunOutcome::Failed {
            reason: out.content,
        },
    }
}

thread_local! {
    /// The run whose program is executing on this thread. Set for the whole
    /// thread — a run is the whole of its thread, like a tool call is — and
    /// read by the step door, which has no other way to know which run's
    /// journal it serves: nothing in the module may close over anything
    /// that belongs to a single call.
    static RUN: std::cell::RefCell<Option<Arc<RunScope>>> = const { std::cell::RefCell::new(None) };
}

/// The per-thread run state the step door reads: the run it journals under,
/// and the next index it hands out. A run is sequential by construction —
/// the script is a program, and the program walks.
struct RunScope {
    run: RecordId,
    next: AtomicU32,
    next_park: AtomicU32,
    cap: u32,
    /// The steps half of the caller's budget, when one was set — journaled
    /// with the pin, re-read by every pass, and checked beside the cap so
    /// the reason can name the caller's bound rather than the plugin's.
    steps_budget: Option<u32>,
    /// The spend half, in micros of a dollar, when set. Checked by the
    /// step door before any step that would dispatch.
    spend_micros: Option<u64>,
    /// The question this pass parked on, set by the park primitive a breath
    /// before it takes the run token down. The interpreter's cancelled arm
    /// reads it: `Some` is a park, `None` is a caller that gave up.
    park: std::sync::Mutex<Option<String>>,
}

impl RunScope {
    fn new(run: RecordId, cap: u32, budget: RunBudget) -> Self {
        RunScope {
            run,
            next: AtomicU32::new(0),
            next_park: AtomicU32::new(0),
            cap,
            steps_budget: budget.steps,
            spend_micros: budget.spend_micros,
            park: std::sync::Mutex::new(None),
        }
    }
}

/// Run `f` as the run described by `scope`: the step door on this thread
/// journals and dispatches under it. The previous scope, if any, is put back.
fn with_run_scope<R>(scope: RunScope, f: impl FnOnce() -> R) -> R {
    let prev = RUN.with(|r| r.replace(Some(Arc::new(scope))));
    let out = f();
    RUN.with(|r| *r.borrow_mut() = prev);
    out
}

fn run_scope() -> Option<Arc<RunScope>> {
    RUN.with(|r| r.borrow().clone())
}

/// The question a park left pending, if this pass is the one parking.
fn park_taken() -> Option<String> {
    RUN.with(|r| {
        r.borrow()
            .as_ref()
            .and_then(|s| s.park.lock().unwrap().clone())
    })
}

/// One step, end to end: the request journaled before the chokepoint, the
/// answer found in the log when the log already has it.
async fn step(
    host: &Host,
    tool: &str,
    input: serde_json::Value,
    cancel: &CancellationToken,
) -> Result<String, String> {
    let scope = RUN
        .with(|r| r.borrow().clone())
        .ok_or_else(|| "tool_call outside a workflow run".to_string())?;
    let d = host
        .dispatcher()
        .ok_or_else(|| "no dispatcher attached; scripts cannot call tools yet".to_string())?;
    let index = scope.next.fetch_add(1, Ordering::SeqCst);
    // The caller's budget, then the plugin's cap — both checked before
    // anything is journaled, so a refused index leaves no request without
    // an answer and a resume re-fails at the same index with the same
    // reason instead of refusing a hole. The budget names the caller: a
    // bound someone set from outside the plugin is a different fact from a
    // cap the plugin chose for itself, and a ledger reads them apart.
    if let Some(budget) = scope.steps_budget
        && index >= budget
    {
        return Err(format!(
            "the run passed its step budget ({budget} steps): the caller set the bound, and the walk stops there"
        ));
    }
    if index >= scope.cap {
        return Err(format!(
            "the run passed its step cap ({cap}): a walk this long is a bug in the workflow, and the cap is where it stops",
            cap = scope.cap
        ));
    }
    let call_id = format!("w{}-{}", scope.run, index);
    let built = Json(input.to_string());

    // What the branch already says about this index, if anything.
    let prior = {
        let session = d.session().lock().await;
        session
            .workflow_run(scope.run)
            .ok_or_else(|| format!("run #{} vanished from its own session", scope.run))?
            .steps
            .into_iter()
            .find(|s| s.index == index)
    };
    // The same program must ask the same thing. A different question under
    // the same index is not the run the journal holds — answering it from
    // the memo would pair an answer with a request nobody made.
    if let Some(prior) = &prior
        && (prior.tool != tool || prior.input != built)
    {
        return Err(format!(
            "replay mismatch at step {index} of run #{}: the journal recorded `{}` with {}, this pass built `{tool}` with {}",
            scope.run, prior.tool, prior.input.0, built.0
        ));
    }
    // The memo: the branch holds the answer, so nothing runs and the gate
    // is not consulted — the answer already carries the verdict that
    // produced it. A memo hit is also past every budget check below: it
    // runs nothing, costs nothing, and a resume must be able to replay
    // what already happened before it reaches the step that stops.
    if let Some(answer) = prior.as_ref().and_then(|p| p.result.clone()) {
        return if answer.is_error {
            Err(answer.content)
        } else {
            Ok(answer.content)
        };
    }
    // A request with no answer is the parked tail: it falls through to the
    // dispatch below, under the id the journal already holds — the one
    // shape that re-aches the gate on a resume. And it is the one shape a
    // resume may *re-fire*: the log cannot say whether the effect ran
    // before the log cut, so a run's effects are at-least-once at this
    // boundary. The memo's guarantee is "never twice on a clean log", and
    // the hole in the answers is the exception it names.
    //
    // The spend budget binds every step that would dispatch — a fresh step
    // and a re-ached parked tail alike — and never a memo hit. The branch
    // is the only book: the spend so far is priced from the ModelSpend
    // records the run's own call ids carry, so a resume reads the same
    // number the first pass read and cannot out-spend it by re-entering.
    // The check sits before the journaling on purpose: a refused step
    // journals nothing, the same as the caps above, so the resume re-fails
    // here rather than refusing a hole.
    if let Some(budget) = scope.spend_micros {
        let (micros, unpriced) = run_spend(&d, host, scope.run).await;
        if micros > budget {
            let floor = if unpriced > 0 {
                format!(", not counting {unpriced} unpriced call(s)")
            } else {
                String::new()
            };
            return Err(format!(
                "the run passed its spend budget (${:.4} of the ${:.4} allowed{floor}; priced at the keys the calls resolved to)",
                micros as f64 / 1_000_000.0,
                budget as f64 / 1_000_000.0,
            ));
        }
    }
    if prior.is_none() {
        d.session()
            .lock()
            .await
            .workflow_step(scope.run, index, &call_id, tool, built.clone())
            .map_err(|e| format!("journaling step {index}: {e:#}"))?;
    }

    host.dispatch_as(tool, input, Some(&call_id), cancel)
        .await
}

/// The run's priced spend so far, read off the branch: every
/// [`RecordKind::ModelSpend`] the run's own call ids carry, priced at the
/// key the record names — the same arithmetic the ledger sums with, against
/// the same catalog. Unpriced keys are counted, not summed: a lookup with
/// no row is a floor, and a floor is named in the refusal reason rather
/// than silently treated as free (the spend invariant, shared with the
/// ledger). In micros of a dollar, so the comparison against a budget is a
/// comparison of integers.
async fn run_spend(d: &Dispatcher, host: &Host, run: RecordId) -> (u64, usize) {
    let prefix = format!("w{run}-");
    let catalog = host.completion().map(|c| Arc::clone(&c.catalog));
    let mut micros = 0u64;
    let mut unpriced = 0usize;
    let session = d.session().lock().await;
    for rec in session.branch() {
        if let RecordKind::ModelSpend { key, call_id, usage } = &rec.kind
        {
            if !call_id.starts_with(&prefix) {
                continue;
            }
            match catalog.as_ref().and_then(|c| c.cost(key)) {
                Some(c) => micros += (c.price(usage) * 1_000_000.0) as u64,
                None => unpriced += 1,
            }
        }
    }
    (micros, unpriced)
}

/// A park: the run stops on a person, and the answer — once someone has
/// given one — is what the script reads here on a later pass.
///
/// The question's record home is [`RecordKind::AskUser`], which already
/// means exactly this: `{ call_id, prompt, answer }`, "keyed by the call id
/// so replay can match it". The park's call id is the run's own
/// deterministic shape, `w<run>-park<n>`, with n counting the park records
/// the branch holds — so a pass whose park 0 was answered and that reaches
/// a second park line journals park 1, and a pass that re-parks an
/// unanswered question journals nothing new.
///
/// A park is memo-shaped like a step and is not one: it dispatches nothing,
/// so no gate question is asked of it and no `ToolResult` is journaled — a
/// park is not an effect. What the log holds is the question and, when it
/// arrives, the answer; until then a resume parks again identically, and
/// the only thing re-stated over it is the ending.
///
/// The answer's delivery door — a dialog, a resume call, a notice — is the
/// design's open fork, and the parked-question guard lands wherever that
/// door lands: a `CallOrigin::Script` call that would supply the answer is
/// refused there, because the answer is the person's and not the script's.
async fn park(
    host: &Host,
    question: &str,
    cancel: &CancellationToken,
) -> Result<String, String> {
    let scope = run_scope().ok_or_else(|| "park outside a workflow run".to_string())?;
    let d = host
        .dispatcher()
        .ok_or_else(|| "no dispatcher attached; scripts cannot call tools yet".to_string())?;
    // The park this pass is on: the program's own park ordinal —
    // deterministic the way the step indices are — which is the id the
    // question was journaled under on the pass that parked first.
    let n = scope.next_park.fetch_add(1, Ordering::SeqCst);
    let call_id = format!("w{}-park{}", scope.run, n);
    let recorded = {
        let session = d.session().lock().await;
        session
            .branch()
            .into_iter()
            .filter_map(|rec| match &rec.kind {
                RecordKind::AskUser {
                    call_id: id,
                    prompt,
                    answer,
                } if *id == call_id => Some((prompt.clone(), answer.clone())),
                _ => None,
            })
            .next_back()
    };
    match recorded {
        Some((recorded_question, answer)) => {
            // The same program parks on the same question — a different one
            // under the same id is the step mismatch's twin, and is refused
            // for the same reason: the journal pairs an answer with the
            // question that earned it.
            if recorded_question != question {
                return Err(format!(
                    "replay mismatch at park {}: the journal recorded {recorded_question:?}, this pass asks {question:?}",
                    scope.run
                ));
            }
            // The memo: someone answered, and the script reads it here.
            if let Some(answer) = answer {
                return Ok(answer);
            }
            // Still unanswered: park again identically. No new record — the
            // question is already journaled — only the ending is re-stated.
        }
        None => {
            d.session()
                .lock()
                .await
                .append(RecordKind::AskUser {
                    call_id: call_id.clone(),
                    prompt: question.to_string(),
                    answer: None,
                })
                .map_err(|e| format!("journaling park `{call_id}`: {e:#}"))?;
        }
    }
    // Ruling 1, the delivery door: an attended session asks the person
    // inline, through the same seam any question rides — the dispatcher's
    // [`UserIo`]. Someone who is there answers and the run continues past
    // the park; the answer is journaled exactly as the resume door
    // records one — an `AskUser` under this park's own id, no
    // distinguisher — so what a later resume reads is the same either
    // way, by construction rather than by agreement. Nobody there
    // (`NoUser`), a dismissed dialog, or a caller that cancelled the wait
    // is `None`: the park is *not answered*, and what follows is what
    // always was. A decline is not an answer to the question the program
    // asked, and it journals nothing new — the question is already on the
    // branch, and replay already knows what unanswered means.
    // The same wait, opened to the door: while the seam blocks, the
    // question is registered — a peer's `answer` carries text here, while
    // the operator's dialog still decides alone — and the guard's drop is
    // what removes it on every exit, this arm included: once the run
    // parks, the question stands only on the branch, and an answer
    // arrives the way resume delivers one, not through the door.
    let (guard, mut peer_rx) =
        d.question_open(QuestionKind::Respond, &call_id, question.to_string());
    let waited = tokio::select! {
        answered = d.user().respond(question, cancel) => {
            // A peer's answer may have landed as the dialog closed: the
            // door has already told that peer "delivered", so a delivery
            // in hand wins the tie — the operator's answer is local and
            // the operator sees the run go on.
            match peer_rx.try_recv() {
                Ok(delivery) => Some((delivery.text, Some(delivery.peer))),
                Err(_) => answered.map(|a| (a, None)),
            }
        }
        delivered = &mut peer_rx => delivered.ok().map(|d| (d.text, Some(d.peer))),
    };
    drop(guard);
    if let Some((answer, peer)) = waited {
        {
            let mut session = d.session().lock().await;
            session
                .append(RecordKind::AskUser {
                    call_id: call_id.clone(),
                    prompt: question.to_string(),
                    answer: Some(answer.clone()),
                })
                .map_err(|e| format!("recording the answer to `{call_id}`: {e:#}"))?;
            if let Some(peer) = peer {
                // Provenance, written after the answer it names: the same
                // `AskUser` an operator's answer rides, then the decision
                // that a peer said this — the fourth producer the ledger
                // keeps distinct.
                session
                    .peer_answer(&call_id, &peer)
                    .map_err(|e| format!("recording that `{peer}` answered `{call_id}`: {e:#}"))?;
            }
        }
        return Ok(answer);
    }
    // Signal the interpreter, then take the run token down. This future
    // never resolves on a parking pass: the select in `execute` drops the
    // program at the await and takes the parked arm, question and all — the
    // run stops here, and the driving call carries the question out.
    *scope.park.lock().unwrap() = Some(question.to_string());
    cancel.cancel();
    std::future::pending::<()>().await;
    unreachable!("pending never resolves")
}

/// The model face: `eidolon::ask_model(prompt, flags)` over the registry's
/// one stateless model tool — `ask_model`, the name the builtin registers
/// under. The face is thin on purpose: it builds the input, and everything
/// else a step is, it already is.
///
/// The input is built, not passed — the prompt, both flags, and, when
/// `use_history`, the thread — because that is what the gate should see of
/// a call that may carry the run's context, and what the journal must hold
/// for a resume to know the request was the same one: the thread rides
/// inside the comparison, so a diverged conversation is the mismatch it
/// is. The tool answers Melete's envelope, always a plain string, and the
/// face hands it over verbatim; the script branches on `ok`, and the
/// thread rebuild is the only thing that reads inside the envelope.
async fn ask_model_step(
    host: &Host,
    prompt: String,
    use_history: bool,
    remember: bool,
    cancel: &CancellationToken,
) -> Result<String, String> {
    let scope = run_scope().ok_or_else(|| "ask_model outside a workflow run".to_string())?;
    let d = host
        .dispatcher()
        .ok_or_else(|| "no dispatcher attached; scripts cannot call tools yet".to_string())?;
    // The thread is the walk's prefix: the remembered model steps the run
    // has already passed. On a resume the journal holds later steps too,
    // and the call at step k must ride the conversation step k was made
    // with — not the whole of a future the first pass never had.
    let index = scope.next.load(Ordering::SeqCst);
    let history = if use_history {
        let session = d.session().lock().await;
        thread_from(
            &session
                .workflow_run(scope.run)
                .ok_or_else(|| format!("run #{} vanished from its own session", scope.run))?,
            index,
        )
    } else {
        Vec::new()
    };
    let mut input = serde_json::Map::new();
    input.insert("prompt".into(), serde_json::Value::String(prompt));
    input.insert("use_history".into(), serde_json::Value::Bool(use_history));
    input.insert("remember".into(), serde_json::Value::Bool(remember));
    // The thread rides whenever it was consulted, empty or not: the input
    // records what this call was built from, and an empty thread is a fact
    // about the call, not the absence of one.
    if use_history {
        input.insert("history".into(), serde_json::Value::Array(history));
    }
    step(host, "ask_model", serde_json::Value::Object(input), cancel).await
}

/// The thread a `use_history` call at step `at` rides in with: the run's
/// remembered model steps before `at`, in order, each contributing its
/// prompt and the reply its journaled answer carries — the conversation
/// the walk had built when it reached that step. A step whose recorded
/// input is not the face's shape is not a model turn — a direct
/// `tool_call("ask_model", …)` is somebody else's call — and a remembered
/// step whose answer is not an `ok` envelope has no reply to join: the
/// thread is the model's answers and only those. Both skips are reads of
/// the journal, so a resume rebuilds exactly what the first pass built,
/// or the run refuses.
fn thread_from(run: &eidolon_core::workflow::Run, at: u32) -> Vec<serde_json::Value> {
    let mut turns = Vec::new();
    for step in &run.steps {
        if step.index >= at {
            continue;
        }
        let Ok(input) = serde_json::from_str::<serde_json::Value>(&step.input.0) else {
            continue;
        };
        if input.get("remember").and_then(|f| f.as_bool()) != Some(true) {
            continue;
        }
        let Some(prompt) = input.get("prompt").and_then(|p| p.as_str()) else {
            continue;
        };
        let Some(result) = &step.result else {
            continue;
        };
        let Ok(envelope) = serde_json::from_str::<serde_json::Value>(&result.content) else {
            continue;
        };
        if envelope.get("ok").and_then(|o| o.as_bool()) != Some(true) {
            continue;
        }
        let Some(reply) = envelope.get("reply").and_then(|r| r.as_str()) else {
            continue;
        };
        turns.push(serde_json::json!({ "role": "user", "content": prompt }));
        turns.push(serde_json::json!({ "role": "assistant", "content": reply }));
    }
    turns
}

/// The flags object: `#{}` for the defaults, and the two names the spec
/// rules — `use_history`, whether the call rides the thread so far, and
/// `remember`, whether the exchange joins it. Anything else is a script
/// bug, named as one before anything runs.
fn ask_model_flags(flags: &rune::Value) -> Result<(bool, bool), String> {
    let value = serde_json::to_value(flags).map_err(|e| format!("ask_model's flags: {e}"))?;
    let Some(map) = value.as_object() else {
        // A rune `()` arrives as null, and null is the defaults, said out
        // loud; anything else that is not an object is a mistake.
        return if value.is_null() {
            Ok((true, true))
        } else {
            Err("ask_model's flags are an object: #{ use_history, remember }".into())
        };
    };
    let mut use_history = true;
    let mut remember = true;
    for (key, flag) in map {
        let Some(b) = flag.as_bool() else {
            return Err(format!("ask_model's flag `{key}` is true or false"));
        };
        match key.as_str() {
            "use_history" => use_history = b,
            "remember" => remember = b,
            other => {
                return Err(format!(
                    "ask_model has no flag `{other}`: the flags are `use_history` and `remember`"
                ))
            }
        }
    }
    Ok((use_history, remember))
}

/// The workflow scope's `eidolon` module: the step door, the park, and —
/// where a completion path exists — the model face. Every other name a
/// tool script can reach is simply not registered here, which is what
/// makes the scope's refusal a compile error rather than a policy
/// question.
pub(crate) fn workflow_module(host: &Arc<Host>) -> Result<rune::Module, rune::ContextError> {
    let mut m = rune::Module::with_crate("eidolon")?;
    let weak = Arc::downgrade(host);
    fn live(weak: &std::sync::Weak<Host>) -> Result<Arc<Host>, String> {
        weak.upgrade()
            .ok_or_else(|| "the script host is gone".to_string())
    }
    {
        let weak = weak.clone();
        m.function("tool_call", move |tool: String, input: rune::Value| {
            let host = live(&weak);
            let cancel = crate::host::current_cancel();
            async move {
                let input = serde_json::to_value(&input).map_err(|e| e.to_string())?;
                let host = host?;
                step(&host, &tool, input, &cancel).await
            }
        })
        .build()?;
    }
    {
        let weak = weak.clone();
        m.function("park", move |question: String| {
            let host = live(&weak);
            let cancel = crate::host::current_cancel();
            async move {
                let host = host?;
                // On a parking pass this never resolves — the helper pulls
                // the run token down and the interpreter's select drops the
                // program at the await. On a resume whose park was answered
                // it returns the recorded answer, and the program continues.
                park(&host, &question, &cancel).await
            }
        })
        .build()?;
    }
    // The verb is registered here and only here: a *tool* script drives the
    // same tool through `tool_call` and parses the envelope itself, while a
    // workflow gets the thread flags. The scope's gate is the completion
    // handle's, the same one `register_builtins` asks before registering
    // the tool: a host with no catalog has no `ask_model` to reach, and
    // the verb refuses to compile there — the refusal the narrow scope
    // knows how to make. The handle is attached before `register_builtins`
    // runs, so by the time any workflow compiles this gate reads the same
    // answer the tool's registration did.
    if host.has_completion() {
        let weak = weak.clone();
        m.function("ask_model", move |prompt: String, flags: rune::Value| {
            let host = live(&weak);
            let cancel = crate::host::current_cancel();
            async move {
                let host = host?;
                let (use_history, remember) = ask_model_flags(&flags)?;
                ask_model_step(&host, prompt, use_history, remember, &cancel).await
            }
        })
        .build()?;
    }
    {
        // The scope's one pure computation, and the reason it exists: a
        // watch must hash a page, and a byte-sum cannot see a same-length
        // edit. `sha256` answers in the vouch rows' own spelling —
        // `sha256:<hex>`, the same bytes `permits::content_hash` writes —
        // and touches nothing: no host, no await, no capability beyond
        // arithmetic a script could not otherwise reach.
        m.function("sha256", |text: &str| crate::permits::content_hash(text))
            .build()?;
    }
    Ok(m)
}
