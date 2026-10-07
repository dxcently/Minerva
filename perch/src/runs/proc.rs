//! The run's process: the detached `eidolon workflow` child, its own process group, the wall clock, and finding it again (slice R1a).
//!
//! No route here. The two hooks below are what `runs` calls from `lib.rs` and `serve.rs`; both are stubs until R1a.

use crate::serve::Ctx;

/// A live pid whose `/proc/<pid>/cmdline` names `workflow` and a run dir under the home is a run this perch owns again (D3).
pub fn adopt(_ctx: &Ctx) {}

/// Detached runs are left running (D3). Stub: waits for nothing.
pub async fn on_shutdown(_ctx: &Ctx) {}
