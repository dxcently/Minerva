//! The pre-tool policy hook — the harness's analogue of Melete's
//! `PreToolUse` seam.
//!
//! It is one trait with one method, evaluated in exactly one place
//! ([`crate::dispatch::Dispatcher::adjudicate`], reached by all three rungs).
//! A hook that panics or errors is treated as a denial: the chokepoint fails
//! closed. That property is what makes it safe to hand this trait to Melete's
//! policy spine later as a plain implementation instead of a vendored
//! extension.
//!
//! The core defines the seam and ships [`AllowAll`]; the classifier that
//! actually answers with something other than "yes" lives in `eidolon-rune`
//! (the shell decomposition algebra from `harnox::policy`, over a Rune leaf
//! table). The core does not depend on it, so a consumer that wants no policy
//! compiles none.

use std::path::Path;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::tool::{ToolCall, ToolManifest};

/// Approval tier a tool declares in its manifest.
///
/// **An input to a hook, not a policy.** It is what the tool's author says the
/// tool is for — a claim about the tool, made once, without seeing any
/// arguments — so it can inform a decision but can never be one: `bash`
/// declares `Mutating` and that is equally true of `mkdir` and of
/// `curl … | sh`. Deciding between those two is exactly what the classifier
/// exists to do, per call, from the arguments.
///
/// A tier-based hook that read this field *as* the policy was shipped and
/// removed; see [`Verdict::Ask`]. Mneme publishes `requires_confirm` on its
/// destructive tier, so for its tools this is read off the server rather than
/// hand-authored.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Approval {
    ReadOnly,
    /// Changes state in a way that can be undone or re-done (write, edit,
    /// most shell).
    #[default]
    Mutating,
    /// Changes state in a way that cannot (delete, force-push, `rm -rf`).
    Destructive,
}

impl Approval {
    /// The name a script sees, so a leaf table can read the declaration
    /// without knowing the enum.
    pub fn as_str(self) -> &'static str {
        match self {
            Approval::ReadOnly => "read_only",
            Approval::Mutating => "mutating",
            Approval::Destructive => "destructive",
        }
    }
}

/// The hook's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Allow,
    /// Refuse, with a reason that goes back to the model as an error result.
    Deny(String),
    /// Put the call in front of the user; run it only if they say yes.
    ///
    /// This is the harness's version of "surfaced for review". An unattended
    /// consumer has nowhere to surface a call to, so Melete's classifier runs
    /// such a call and writes it into a digest; here the transcript already
    /// shows every call as it happens, which frees this tier to mean the
    /// stronger thing — *stop and ask*.
    ///
    /// It needs no separate posture for a headless run: [`crate::user::NoUser`]
    /// declines every question, so an `Ask` with nobody at the keyboard is a
    /// refusal, which is the right answer there and the wrong one in front of a
    /// person. That is also why a classifier that fails to load answers `Ask`
    /// rather than `Deny` — a broken table must not brick an interactive
    /// session, and must not wave calls through an unattended one.
    ///
    /// A tier-based hook that asked on everything `Mutating` used to fill this
    /// role and was removed: it stopped `mkdir` and waved through
    /// `curl … | sh`, so it cost a prompt on every write while distinguishing
    /// nothing that mattered.
    Ask(String),
}

/// The hook's answer, plus what the classifier called it.
///
/// [`Verdict`] alone is what the chokepoint needs in order to act; this is
/// what the *log* needs in order to remember. They are separate because a
/// verdict is a sentence for a person to read — "`bash` — writes outside the
/// working directory. Run it?" — and a ledger has to group thousands of those
/// by the thing that would have to change for them to stop, which is the
/// classifier's own reason and not the sentence built around it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ruling {
    /// What the deterministic pass decided. **The context-aware layer never
    /// rewrites this** — see [`Ruling::judged`].
    pub verdict: Verdict,
    /// The classifier's own reason, from its small fixed vocabulary: the unit
    /// a table edit flips, and so the unit a ledger has to group by. `None`
    /// from a hook with no classifier behind it.
    pub reason: Option<String>,
    /// The answer was reached for want of understanding rather than on the
    /// merits — a subshell, a `for` loop, a write outside the working
    /// directory, a line that would not parse. Carried across from
    /// `harnox::policy::Verdict::structural`, and the reason a refusal the
    /// algebra could not justify becomes a question here.
    pub structural: bool,
    /// Set when the context-aware layer answered an [`Verdict::Ask`] on the
    /// operator's behalf: its one-line reasoning.
    ///
    /// It is a **separate field rather than a rewritten `verdict`**, and that
    /// is the whole safety argument for that layer. The only thing a model can
    /// do here is answer a question the deterministic pass had already decided
    /// to raise: it cannot reach an `Allow` (there was no question), and it
    /// cannot reach a `Deny` made on the merits (the table's answer stands).
    /// A confused judge therefore costs a prompt that was going to be asked
    /// anyway — never the work, and never a refusal nobody chose.
    pub judged: Option<String>,
    /// Set when [`crate::yolo::Yolo`] turned this question into a yes
    /// because the operator armed the switch — not because anything read
    /// the call.
    ///
    /// It rides beside [`Self::judged`] rather than reusing it for the
    /// same reason [`crate::session::PolicyOutcome::Judged`] is not an
    /// `Approved` with a note: these are three different things saying
    /// yes, and a ledger that could not tell a model's yes from a blanket
    /// one from a person's would be the boundary quietly moving itself.
    /// The [`Verdict`] is rewritten here — unlike the judge, which may
    /// only annotate — and that is precisely what the operator asked for
    /// when they armed it.
    pub yolo: bool,
}

impl Ruling {
    pub fn allow() -> Self {
        Verdict::Allow.into()
    }

    pub fn deny(reason: impl Into<String>) -> Self {
        Verdict::Deny(reason.into()).into()
    }

    pub fn ask(prompt: impl Into<String>) -> Self {
        Verdict::Ask(prompt.into()).into()
    }

    /// The classifier's reason if it gave one, else the verdict's own wording.
    /// What a ledger groups by, and never empty.
    pub fn reason_or_prompt(&self) -> &str {
        if let Some(r) = &self.reason {
            return r;
        }
        match &self.verdict {
            Verdict::Allow => "allowed",
            Verdict::Deny(s) | Verdict::Ask(s) => s,
        }
    }
}

/// A bare verdict is a ruling with nothing else known about it — which is the
/// honest shape for [`AllowAll`] and for any hook that is not a classifier.
impl From<Verdict> for Ruling {
    fn from(verdict: Verdict) -> Self {
        Ruling {
            verdict,
            reason: None,
            structural: false,
            judged: None,
            yolo: false,
        }
    }
}

#[async_trait]
pub trait PolicyHook: Send + Sync {
    /// Decide one call.
    ///
    /// `cwd` is where the tool would run — the dispatcher's live copy, not a
    /// path captured at startup, because a session that adopts another
    /// session's log moves the working directory with it. A hook that scopes
    /// filesystem work needs the same answer the tool will get.
    async fn pre_tool(&self, call: &ToolCall, manifest: &ToolManifest, cwd: &Path) -> Ruling;
}

/// The one wording of what a verdict meant on screen, for the outcomes the
/// transcript would not otherwise show — which is exactly one of them.
///
/// A refusal is already visible as the tool error it produced, and a question
/// the operator answered was visible as the question. A call the judge waved
/// through is visible as nothing at all: it ran, it produced output, and
/// nothing distinguishes it from a call the table allowed outright. So that
/// is the one this draws, live and on resume alike, from the same words —
/// the arrangement [`crate::usage::stop_note`] already uses, and for the same
/// reason: two wordings of one fact drift.
///
/// [`crate::session::PolicyOutcome::Yolo`] is invisible in the same way and
/// is deliberately *not* a second one. The judge is invisible per call and
/// invisible as a layer — nothing on screen says it is there. Yolo is the
/// opposite: it is a posture the operator armed on purpose and which says so
/// continuously, on the status line and in the banner the headless run
/// prints. A line per waived call would be that fact restated on every
/// iteration of a tool loop — which is to say, the interruption the mode
/// exists to remove, wearing a different hat. The journal keeps the ledger's
/// copy.
///
/// The rest are journaled for a ledger to read, not for a person to.
pub fn verdict_note(
    outcome: &crate::session::PolicyOutcome,
    tool: &str,
    note: Option<&str>,
) -> Option<String> {
    match outcome {
        crate::session::PolicyOutcome::Judged => Some(match note {
            Some(n) => format!("{tool} — allowed without asking you: {n}"),
            None => format!("{tool} — allowed without asking you"),
        }),
        _ => None,
    }
}

/// Allow everything. The core's only shipped hook, and the right one for a
/// consumer that has not opted into a classifier — a test harness, an embedder
/// with its own gate.
pub struct AllowAll;

#[async_trait]
impl PolicyHook for AllowAll {
    async fn pre_tool(&self, _: &ToolCall, _: &ToolManifest, _: &Path) -> Ruling {
        Ruling::allow()
    }
}
