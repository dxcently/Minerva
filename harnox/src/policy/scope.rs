//! The context a command is classified *against*, and the path geometry every
//! scope check reduces to.
//!
//! The seam is "state and geometry, no verdicts": nothing here decides a tier,
//! it only describes the run and answers where a path points. Tier decisions
//! live in [`super::shell`] (composition) and the consumer's
//! [`LeafTable`](super::LeafTable) (leaves).

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A language toolchain the caller has declared. Declaring one is what lets a
/// leaf table move that toolchain's compiler/linter/test commands into `Allow`;
/// without it, those commands fall to the [`BashDefault`] posture.
///
/// This is a *grant*, not a capability check: it says the caller expected this
/// toolchain to be used here, which is a different question from whether the
/// machine can survive the command (see [`LeafTable::program_override`](super::LeafTable::program_override)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Toolchain {
    Rust,
    Python,
    Node,
    Nix,
}

/// What to do with a shell command that matches no allow-list and is not an
/// obvious injection or escape.
///
/// A posture, not a safety property: nothing here changes what the composition
/// rules refuse. Which one is right depends on who is watching. `Deny` suits an
/// unattended run. `Flag` suits one whose work is reviewed downstream. `Allow`
/// suits a surface where every command is already in front of a person as it
/// runs — there, asking about an unrecognized *program name* buys nothing (the
/// shapes worth stopping are recognized ones) and costs the prompt fatigue that
/// makes a gate get switched off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BashDefault {
    Allow,
    #[default]
    Flag,
    Deny,
}

/// Everything the algebra needs to know about *where and for whom* a command is
/// being classified.
///
/// Deliberately small. A consumer's own situation — which grants a run holds,
/// which surface it serves, which files belong to the daemon rather than the
/// agent — is expressed through its [`LeafTable`](super::LeafTable) rather than
/// by growing this struct, because those are facts about the consumer and this
/// type is shared.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PolicyContext {
    /// The tree the caller is working in. Filesystem mutations must stay inside
    /// it. `None` means there is no local scope to check against, which is a
    /// hard refusal for a filesystem verb unless [`Self::unscoped`] is set.
    pub workspace_root: Option<PathBuf>,
    /// Toolchains whose commands the caller expects to run.
    pub toolchains: Vec<Toolchain>,
    /// Fallback posture for a command matching no allow-list.
    pub bash_default: BashDefault,
    /// Set while classifying a command bound for a host this process has no
    /// filesystem view of (a remote box over ssh, a rented worker). There is no
    /// root to scope a path against, so an unscoped filesystem mutation is
    /// surfaced for review rather than refused outright — refusing would block
    /// every remote command, and allowing silently would scope a remote path
    /// against a local checkout, which is worse than either.
    ///
    /// Never persist this: derive it per call from the tool being classified,
    /// so it can never leak onto a local command.
    #[serde(skip)]
    pub unscoped: bool,
}

impl PolicyContext {
    /// A context for work in `root` with no toolchains declared and the default
    /// `Flag` posture.
    pub fn in_workspace(root: impl Into<PathBuf>) -> Self {
        Self { workspace_root: Some(root.into()), ..Self::default() }
    }

    /// Declare a toolchain (builder form).
    pub fn with_toolchain(mut self, t: Toolchain) -> Self {
        if !self.toolchains.contains(&t) {
            self.toolchains.push(t);
        }
        self
    }

    /// Set the unrecognized-command posture (builder form).
    pub fn with_default(mut self, d: BashDefault) -> Self {
        self.bash_default = d;
        self
    }

    /// Whether `t` was declared.
    pub fn has(&self, t: Toolchain) -> bool {
        self.toolchains.contains(&t)
    }

    /// The unrecognized-command posture, as the name a script sees.
    pub fn posture(&self) -> &'static str {
        match self.bash_default {
            BashDefault::Allow => "allow",
            BashDefault::Flag => "flag",
            BashDefault::Deny => "deny",
        }
    }
}

/// Whether `p` — interpreted relative to `root` when relative — resolves to a
/// location inside `root`. Lexical, so it works on paths that do not exist yet;
/// `root` is expected to be absolute.
pub fn path_within(p: &Path, root: &Path) -> bool {
    let full = if p.is_absolute() { p.to_path_buf() } else { root.join(p) };
    normalize(&full).starts_with(normalize(root))
}

/// Lexically normalize a path: drop `.` and resolve `..` without touching disk.
///
/// Lexical on purpose. Resolving symlinks would consult the filesystem, which
/// makes the answer depend on state that can change between the check and the
/// command — and would answer nothing at all for a path being created.
pub fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            c => out.push(c.as_os_str()),
        }
    }
    out
}

/// Whether `p` resolves under the system scratch area. An escape *into* `/tmp`
/// is the one workspace escape worth surfacing rather than refusing: agents
/// legitimately stage work there, and nothing under it is durable.
pub fn target_under_tmp(p: &Path, root: &Path) -> bool {
    let full = if p.is_absolute() { p.to_path_buf() } else { root.join(p) };
    let norm = normalize(&full);
    norm.starts_with("/tmp") || norm.starts_with("/private/tmp")
}
