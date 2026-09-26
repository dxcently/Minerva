//! The command safety classifier: shell decomposition algebra, shared.
//!
//! One classifier, in the crate both consumers link, rather than one per
//! harness. It answers a single question — *may this command run, silently or
//! with someone watching?* — and it answers it by taking the command apart
//! rather than by pattern-matching the text.
//!
//! ## The split
//!
//! This module owns **decomposition**, and nothing else:
//!
//! - parsing a command line into the shell's real grammar (`conch-parser`),
//! - composing per-leaf verdicts across pipes, `&&`/`||` chains, `;`-sequences,
//!   command substitutions and redirects,
//!   under one rule — a composite is only ever as permissive as its most
//!   restrictive part,
//! - scope-checking a filesystem target against the caller's workspace,
//! - the liveness guard on a wait-loop's condition,
//! - failing closed on anything it cannot reduce.
//!
//! The **leaf table** — "given an already-decomposed, already-safe-shaped
//! `argv`, which tier is it?" — belongs to the consumer, behind [`LeafTable`].
//! That is where deployment facts live (which toolchain commands are silent,
//! which subcommands are destructive, what an unrecognized program gets), and
//! it is the half that should be tunable without a rebuild. A leaf table can
//! only ever change which *already-decomposed* leaf is silent, reviewed or
//! blocked; it never sees anything this module has not already reduced to a
//! flat `argv`, so it cannot grant safety by construction.
//!
//! ## What it guarantees
//!
//! - **Quoted content is data.** Because the walk is over parsed structure, a
//!   `|`, `&`, `;`, `$`, `>` or backtick inside a quoted argument is never
//!   mistaken for syntax. Expansions whose runtime value is unknown collapse to
//!   [`OPAQUE`], a control character chosen so it can never collide with a
//!   program name or flag a table keys on.
//! - **Worst part wins.** Any `Deny` stage denies the composite; otherwise any
//!   `Flag` stage flags it; `read_only` only when every stage is.
//! - **A loop is stricter than a sequence.** A `while`/`until` body must be
//!   entirely `Allow` — a flagged command run once and surfaced is not the same
//!   risk as the same command run an unbounded number of times with nobody
//!   watching — and its condition must be able to become false.
//! - **Fail closed.** A command the real shell grammar cannot parse is denied,
//!   because a classifier that cannot say what a command does must not say it
//!   is fine.
//!
//! ## Origin
//!
//! Ported from Melete's `src/policy/`, which ran it as the `PreToolUse` gate on
//! every autonomous coding run for two months before this extraction. The
//! behaviour here is that classifier's, minus its own leaf table and minus the
//! consumer-specific carve-outs it grew (see [`LeafTable::program_override`],
//! which is where a carve-out belongs now).

use std::borrow::Cow;

mod scope;
mod shell;

pub use scope::{BashDefault, PolicyContext, Toolchain, normalize, path_within, target_under_tmp};

/// What to do with one command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Run it silently.
    Allow,
    /// Run it, but do not let it pass unnoticed. What "noticed" means is the
    /// consumer's: a line in a digest for an unattended run, a confirmation for
    /// an interactive one. The tier says *this deserves a human*, not *how the
    /// human is reached*.
    Flag,
    /// Refuse it.
    Deny,
}

impl Decision {
    /// The stricter of two tiers — the composition rule, as an operation.
    pub fn worse(self, other: Self) -> Self {
        match (self, other) {
            (Decision::Deny, _) | (_, Decision::Deny) => Decision::Deny,
            (Decision::Flag, _) | (_, Decision::Flag) => Decision::Flag,
            _ => Decision::Allow,
        }
    }
}

/// A classified command: the tier, why, and whether it changes anything.
///
/// The reason is a `Cow` rather than a `&'static str` so a scripted leaf table
/// can return one it built without the consumer having to leak it. Reasons the
/// algebra itself produces are always static.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub decision: Decision,
    /// Short, human-facing category for an audit line. Keep the vocabulary
    /// small and stable: it is the natural grouping key for asking how often a
    /// rule fires, which is the input to ever loosening it deliberately.
    pub reason: Cow<'static, str>,
    /// True when the command changes nothing. A `Deny` is never read-only —
    /// a refused command is not a read, and reporting it as one would let a
    /// blocked call be counted as an observation.
    pub read_only: bool,
    /// True when the algebra itself refused, because it could not reduce the
    /// command's *shape* (an unparseable line, a subshell, a background job, a
    /// heredoc, a path it has no scope to check) — as opposed to the leaf table
    /// judging what the command does.
    ///
    /// The distinction exists because the two deserve different answers on
    /// different surfaces. Unattended, both are refusals and this can be
    /// ignored. In front of a person, "I cannot take this apart, do you want
    /// it?" is a fair question, where "this is forbidden" is not — and without
    /// this flag an interactive consumer would have to either refuse every
    /// `for` loop or wave through everything the table denies.
    ///
    /// Never set on an `Allow` or a `Flag`: there is nothing to distinguish.
    pub structural: bool,
}

impl Verdict {
    pub fn allow(reason: impl Into<Cow<'static, str>>, read_only: bool) -> Self {
        Self { decision: Decision::Allow, reason: reason.into(), read_only, structural: false }
    }

    pub fn flag(reason: impl Into<Cow<'static, str>>, read_only: bool) -> Self {
        Self { decision: Decision::Flag, reason: reason.into(), read_only, structural: false }
    }

    /// A refusal on the merits — what a leaf table returns when it knows the
    /// command and will not have it.
    pub fn deny(reason: impl Into<Cow<'static, str>>) -> Self {
        Self { decision: Decision::Deny, reason: reason.into(), read_only: false, structural: false }
    }

    /// A verdict at a tier decided at runtime — what a leaf table returns when
    /// the tier came from a table lookup rather than a literal. Never
    /// structural: only the algebra refuses for want of understanding.
    pub fn tier(decision: Decision, reason: impl Into<Cow<'static, str>>, read_only: bool) -> Self {
        Self {
            decision,
            reason: reason.into(),
            read_only: read_only && decision != Decision::Deny,
            structural: false,
        }
    }

    /// A refusal for want of understanding — see [`Verdict::structural`]. Only
    /// the algebra produces these.
    pub fn refuse(reason: impl Into<Cow<'static, str>>) -> Self {
        Self { decision: Decision::Deny, reason: reason.into(), read_only: false, structural: true }
    }

    pub fn is(&self, d: Decision) -> bool {
        self.decision == d
    }

    /// A `Deny` of the same *kind* as `v` — merits or shape. Used where a
    /// composite is refused because one of its parts was, so the composite
    /// answers the way the part would have.
    pub(crate) fn deny_like(v: &Verdict) -> Self {
        Self {
            decision: Decision::Deny,
            reason: v.reason.clone(),
            read_only: false,
            structural: v.structural,
        }
    }
}

/// The consumer's half: which tier does one already-decomposed leaf get?
///
/// Every method is asked about something this module has already reduced — a
/// flat `argv`, or a bare program name. An implementation cannot see a pipe, a
/// substitution or an unscoped path, so it cannot decide anything about them.
///
/// Implementations must not panic; a caller that hosts a script should catch
/// its errors and return a `Deny` (unattended) or a `Flag` (in front of a
/// person) rather than letting one escape.
pub trait LeafTable {
    /// The tier for one leaf command's `argv`, already unwrapped from any shim.
    /// `argv[0]` is the program.
    fn classify_program(&self, argv: &[&str], ctx: &PolicyContext) -> Verdict;

    /// Whether a filesystem verb whose targets are *already confirmed in-scope*
    /// is destructive (`Flag`) or benign (`Allow`). Only these two are
    /// meaningful: the scope check has already refused everything else.
    fn fs_verb_tier(&self, prog: &str) -> Decision;

    /// The tier for a command matching nothing. `posture` is what the caller
    /// declared ([`PolicyContext::bash_default`]); an implementation is free to
    /// ignore it, which is how a table pins a floor the context cannot lower.
    fn bash_default(&self, posture: BashDefault) -> Decision;

    /// A consumer-owned verdict consulted *before* anything else about a leaf —
    /// the seam for a rule that must sit outside the tunable table.
    ///
    /// This is where a carve-out goes. Melete pins `cargo build`/`test`/`clean`
    /// here: not a safety boundary but a resource one (a runaway compile OOM'd
    /// the box), and precisely the kind of rule that is right for one
    /// deployment and wrong for the next. Keeping it here rather than in the
    /// algebra means the shared code has no opinion about anyone's hardware.
    fn program_override(&self, _argv: &[&str], _ctx: &PolicyContext) -> Option<Verdict> {
        None
    }

    /// Whether a path must never be written regardless of scope — state the
    /// consumer owns that happens to live inside the workspace. Checked on
    /// filesystem verbs and on redirect targets alike.
    fn protected_path(&self, _path: &str) -> bool {
        false
    }
}

/// The tool's own name, with any MCP qualification stripped — `bash` from
/// `mcp__eidolon__bash`, `edit_note` from `mcp__mneme__edit_note`.
///
/// A tool served over MCP arrives carrying the server it came from, and the
/// server's name is a wiring detail: the same tool must classify the same way
/// however it was reached — called directly, proxied through a backend running
/// its own tools, or served under a differently-named MCP server in someone
/// else's `--mcp-config`. Stripping here rather than in the table also means a
/// consumer's table never has to know the prefix exists.
///
/// It lives in the shared crate because both consumers wrote the same line
/// independently, and because getting it wrong is quiet: a table that has never
/// heard of the qualified name falls to whatever the consumer does with an
/// unrecognized tool, which is a gate turning into either a rubber stamp or a
/// prompt on every call.
pub fn bare_tool_name(full: &str) -> &str {
    full.rsplit("__").next().unwrap_or(full)
}

/// Placeholder an expansion collapses to when a word is flattened: its runtime
/// value is unknown, so it must never coincide with a real program name or flag
/// a table keys on. A control character keeps it out of any legitimate literal.
pub const OPAQUE: char = '\u{1}';

/// Classify a shell command line.
///
/// The entry point: parse, decompose, ask `table` about each leaf, compose. A
/// command that cannot be parsed is denied — see the module doc for why that is
/// the only honest answer.
pub fn classify_command(command: &str, ctx: &PolicyContext, table: &dyn LeafTable) -> Verdict {
    shell::classify(command, ctx, table)
}

#[cfg(test)]
mod tests;
