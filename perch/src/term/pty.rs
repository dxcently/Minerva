//! The PTY host (slice T1a): `openpty` and the shell on the spawner thread, the reader and writer threads, the ring, close, and
//! the shutdown of every terminal. No route here.

use crate::serve::Ctx;

/// End every terminal, concurrently, bounded at 2.5 s: SIGHUP the session, wait, sweep what is left. Stub: ends nothing.
pub async fn shutdown(_ctx: &Ctx) {}
