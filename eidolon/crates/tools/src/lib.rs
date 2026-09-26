//! The Rust engine primitives behind the built-in tools.
//!
//! These are plain functions — no manifest, no policy, no Rune. They are
//! what a Rune wrapper script calls after the dispatcher has already said
//! yes. The split is deliberate (vault, *Extension model*, 2026-09-03):
//! the heavy lifting stays in Rust, and the *tool* the model sees is a Rune
//! script that composes calls to these, on the same contract as any
//! user-authored skill. There is nothing privileged about `read`; it is a
//! script that happens to ship with the harness.
//!
//! Day-one set, taken from pi's own core surface plus `grep`: [`fs::read`],
//! [`fs::write`], [`fs::edit`], [`shell::run`], [`search::grep`]. [`web::fetch`]
//! joined them because the alternative was `curl | sed`: the shell can
//! *retrieve* a page, and turning one into something a model can read is a
//! job with an answer worth keeping in one place. [`web::search`] came with
//! it, and [`api::request`] after that: the two primitives that carry a
//! credential. In both, the key is a parameter, it becomes a header inside the
//! call, and what comes back is the answer — so the credential never exists as
//! a string a script, a tool result or a log can reach. `api` is the general
//! one: a *service* the operator declared (where it is, and which header the
//! key rides in) and a relative path under it, since the alternative for the
//! next API was a script shelling out to `curl` with the token on the command
//! line.
//!
//! Every primitive resolves relative paths against an explicit `cwd`; none
//! consults the process working directory. [`web`] is the exception that
//! proves it — a URL is not a path and there is nothing to resolve it
//! against.

pub mod api;
pub mod fs;
pub mod image;
pub mod search;
pub mod shell;
pub mod web;

use std::path::{Path, PathBuf};

/// Resolve `path` against `cwd`, expanding a leading `~`.
pub fn resolve(cwd: &Path, path: &str) -> PathBuf {
    let p = if let Some(rest) = path.strip_prefix("~/") {
        match std::env::var_os("HOME") {
            Some(h) => PathBuf::from(h).join(rest),
            None => PathBuf::from(path),
        }
    } else {
        PathBuf::from(path)
    };
    if p.is_absolute() { p } else { cwd.join(p) }
}
