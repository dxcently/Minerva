//! The Aoide runner (slice R2a): the one place that builds an `aoide` argv. It always uses the explicit binary path from `--aoide`
//! (or `~/.local/bin/aoide` when that is executable) and never a `PATH` lookup, because `PATH` finds a stale one first.
//!
//! No route and no `Ctx` field: `mesh` and `mail` call it. R2a fills this file; there is nothing here yet.
