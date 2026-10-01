//! The journaled shape of a **workflow run**: the fifth plugin payload, and
//! the one the Melete convergence re-homes its skill machinery into.
//!
//! A workflow is a Rune script that drives dispatch over its own plugin's
//! tools, and nothing else: every step is a tool call the one
//! [`Dispatcher`][crate::dispatch::Dispatcher] classifies and journals, so a
//! run adds no execution path (the design record is the vault,
//! `wiki/projects/Eidolon/The Plugin System (design)`, §Workflows). What this
//! module owns is the *run's* records and the reading half of them: a run's
//! opening record carries the pin — which workflow, at which version, with
//! which hash — each step journals its request before the chokepoint sees it,
//! and the ending says whether the run completed, failed, or **parked** on a
//! person.
//!
//! **Why the records exist.** `dispatch` journals a result for every call, but
//! a script's call is in no assistant message and no `UserToolCall`, so
//! without a request record a run's results belong to nothing — invisible to
//! [`Session::messages_after`][crate::session::Session::messages_after] and to
//! every consumer replaying the branch. That is
//! [`RecordKind::UserToolCall`][crate::session::RecordKind::UserToolCall]'s
//! gap, one step further out: the operator's call is at least the operator's.
//!
//! **Why that is what makes resume replay.** The interpreter's state is
//! re-derived from what was journaled rather than stored anywhere: a resumed
//! run re-runs its program and, at step k, takes the answer the log already
//! holds instead of firing the side effect again. So a step's `call_id` is the
//! run's own and deterministic (`w<run>-<index>` is the shape) — memoization
//! keyed by a fresh id would re-fire every step it was meant to spare.
//!
//! **What a hop needs.** A run is a session (ruling 8), so it ships as one;
//! what does not ship with it is the *program*. The pin is what lets a venue
//! refuse a run whose workflow it does not have at the hash the run was
//! started with, rather than replaying into a changed program — the same
//! content-hash machinery the warrant uses.

use serde::{Deserialize, Serialize};

use crate::message::Json;
use crate::session::{Record, RecordId, RecordKind};

/// How a workflow run stopped.
///
/// A journaled enum: **append only**, for [`RecordKind`]'s reason — its
/// ordinal is inside the bytes [`RecordKind::WorkflowEnd`] writes, so a
/// variant added anywhere but the end re-reads every older log's ending as
/// something else.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, bitcode::Encode, bitcode::Decode)]
pub enum RunStatus {
    /// The program reached its end.
    Completed,
    /// It stopped on an error, or a resume refused it — a changed program, a
    /// hole in its steps.
    Failed,
    /// It is waiting on a person.
    ///
    /// Its own value rather than an absence or a failure, because the door
    /// owes a program caller a status distinct from success for a run that is
    /// waiting on somebody: a parked run that exits zero is a lie about a run
    /// waiting on a person.
    Parked,
}

/// The per-run budget a caller sets at start (ruling 4): how many steps the
/// run may take and how much it may spend, either or both. A resume cannot
/// escape it — the budget is journaled with the run ([`RecordKind::WorkflowBudget`])
/// and every pass re-reads it from the branch, so the caller's resume call
/// carries no budget of its own and cannot name a larger one. Over the
/// budget the run fails with the reason, journaled like any other failure.
///
/// Spend is in **micros of a dollar**, the ledger's own unit, because a
/// budget is a bound and a float is not one. Priced spend counts; an
/// unpriced call is a floor and is said in the refusal reason rather than
/// silently treated as zero (the spend invariant, shared with the ledger).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RunBudget {
    /// The most steps the run may dispatch.
    pub steps: Option<u32>,
    /// The most the run may spend, in micros of a dollar.
    pub spend_micros: Option<u64>,
}

impl RunBudget {
    /// Whether this budget constrains anything. A budget with neither half
    /// journals nothing: a record that constrains nothing is a lie about a
    /// constraint nobody made.
    pub fn is_empty(&self) -> bool {
        self.steps.is_none() && self.spend_micros.is_none()
    }
}

/// What a run was started as: the program's identity, and the arguments it
/// was handed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunPin {
    /// The plugin directory that ships the workflow, by name.
    pub plugin: String,
    /// The workflow file, by name — the program the interpreter runs.
    pub workflow: String,
    /// The version the plugin's manifest declared.
    pub version: String,
    /// The hash of the compiled source set: the workflow's own file plus every
    /// `lib/*.rn` the plugin compiles in front of it. The interpreter computes
    /// it over the program it holds, so a resume compares two strings rather
    /// than re-deriving what "the same program" means.
    pub hash: String,
    /// The arguments the run was started with, verbatim — what the door
    /// received. Deliberately not re-derived on resume: a replayed run must be
    /// handed the same words, and an absent argument must not be able to pass
    /// for an empty one.
    pub args: Json,
}

/// One step of a run, with the answer the log already holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunStep {
    /// Its position in the run, from zero.
    pub index: u32,
    /// The call id the step dispatched under — the run's own, deterministic.
    pub call_id: String,
    /// The tool the step called.
    pub tool: String,
    /// The input it called it with, verbatim.
    pub input: Json,
    /// The journaled result, when one landed on this branch. `None` is a step
    /// whose answer is not here: the run parked on it, or the log was cut
    /// mid-step.
    pub result: Option<StepResult>,
}

/// What a step's call produced, as the log holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StepResult {
    pub content: String,
    pub is_error: bool,
}

/// A run as a resume reads it back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    /// The `WorkflowStart` record's id — the run's identity in every other
    /// record that names it.
    pub id: RecordId,
    /// The program it was started as.
    pub pin: RunPin,
    /// The steps on this branch, in index order.
    pub steps: Vec<RunStep>,
    /// The latest ending on this branch; `None` is a run still going, or one
    /// that parked and has not been resumed.
    pub end: Option<RunStatus>,
}

impl Run {
    /// The first index this branch cannot replay, if any.
    ///
    /// Two holes, and a resume refuses at the first rather than walking
    /// past it — an interpreter that walks past either dispatches a call a
    /// second time, which is exactly the side effect memoization exists to
    /// prevent.
    ///
    /// An **index hole**: a step's record left the branch without its
    /// position doing so — struck by an [`RecordKind::Excluded`], skipped by
    /// a fork — so the index the walk expects is not the index the log has.
    ///
    /// A **result hole**: a step's request is here and the answer dispatched
    /// under its call id is not, and the step is not the last — walking past
    /// it would re-fire the call whose answer went missing.
    ///
    /// A result hole on the **last** step is deliberately *not* reported:
    /// that is the shape of a run that stopped mid-step, and it is
    /// replayable only when the ending is [`RunStatus::Parked`]. `gap` does
    /// not consult the ending — a `Completed` run whose last step has no
    /// answer passes here and must be refused by the caller, which is where
    /// the ending is actually read on resume.
    pub fn gap(&self) -> Option<u32> {
        let last = self.steps.len().saturating_sub(1);
        self.steps
            .iter()
            .enumerate()
            .find(|(i, step)| {
                step.index as usize != *i || (step.result.is_none() && *i != last)
            })
            .map(|(i, _)| i as u32)
    }
}

/// Every run on a branch, in the order they started.
pub fn runs(branch: &[&Record]) -> Vec<Run> {
    branch
        .iter()
        .filter(|r| matches!(r.kind, RecordKind::WorkflowStart { .. }))
        .filter_map(|r| run(branch, r.id))
        .collect()
}

/// One run, read back from the branch it started on.
///
/// `None` when `id` names no run here — a record of another kind, a fork that
/// never had it, or the wrong id — which is the only answer that can be given
/// without inventing one.
pub fn run(branch: &[&Record], id: RecordId) -> Option<Run> {
    let start = branch.iter().find(|r| r.id == id)?;
    let RecordKind::WorkflowStart {
        plugin,
        workflow,
        version,
        hash,
        args,
    } = &start.kind
    else {
        return None;
    };
    let pin = RunPin {
        plugin: plugin.clone(),
        workflow: workflow.clone(),
        version: version.clone(),
        hash: hash.clone(),
        args: args.clone(),
    };

    let mut steps = Vec::new();
    let mut end = None;
    for r in branch {
        match &r.kind {
            RecordKind::WorkflowStep {
                run,
                index,
                call_id,
                tool,
                input,
            } if *run == id => {
                // The answer, by id: the `ToolResult` the chokepoint journaled
                // for this step's call. A tool that calls a tool journals its
                // own results too, which is why the lookup is keyed by the
                // call id and never by "the next result after this record".
                let result = branch.iter().find_map(|q| match &q.kind {
                    RecordKind::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } if tool_use_id == call_id => Some(StepResult {
                        content: content.clone(),
                        is_error: *is_error,
                    }),
                    _ => None,
                });
                steps.push(RunStep {
                    index: *index,
                    call_id: call_id.clone(),
                    tool: tool.clone(),
                    input: input.clone(),
                    result,
                });
            }
            RecordKind::WorkflowEnd { run, status } if *run == id => end = Some(*status),
            _ => {}
        }
    }
    // The journal writes them in order; the sort is here so a log that was
    // assembled out of order (a fork, a hand-built test) still reads as the
    // sequence the interpreter will walk, and `gap` then judges the sequence.
    steps.sort_by_key(|s| s.index);

    Some(Run {
        id,
        pin,
        steps,
        end,
    })
}
