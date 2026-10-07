//! `/hub/runs`: the graph runs the page starts and watches (slices R1a, R1b).
//!
//! A run is an `eidolon workflow` child the perch starts for a graph named in `--run-graphs`, detached so a restarted perch finds it
//! again ([`adopt`]) and never killed by a shutdown ([`on_shutdown`]). The routes are split by who fills them:
//!
//! ```text
//!   GET  /hub/runs/health        health::get      probes of --jev-health and --browser-health                 (R1a)
//!   GET  /hub/runs/graphs[/<g>]  graphs::get      the graph files the new-run form shows                      (R1b)
//!   GET  /hub/runs/<rid>/log     log::get         the run's journal from `after`                              (R1b)
//!   everything else              state::route     GET|POST /hub/runs, /<rid>, /<rid>/answer|cancel|abandon    (R1a)
//! ```
//!
//! `proc` holds the process side (the detached child, adoption, the wall clock) and has no route. [`pick`] is the whole dispatch and
//! is a plain function so a test can reach it: `health` and `graphs` are the two words a run id can never be.
//!
//! Every handler has one signature, `(ctx, req, rest) -> Response`, where `rest` is the part of the path below the handler's own
//! prefix as the table above names it: `""` for `health`, `""` or `<g>` for `graphs`, the run id for `log`, the whole rest for `state`.
//!
//! **For R1b**, `state` offers what a log needs without a second reader of `runs.json`: [`state::config_of`] (the run's own config, the
//! one `eidolon --config <it> log --json <journal>` is run with) and [`state::journal_of`] (the session eidolon made in the run's
//! folder, once there is one), both `None` for a run the perch does not know; a run id is checked with [`state::rid_ok`] before it is
//! used for anything, and [`pick`] hands a log handler only a segment, so a handler that does not know the run answers the empty 404.
//!
//! **What a run may not do.** It never calls a model (the mock provider, the mock model), is never yolo, never uses the manual config, and
//! there is one at a time that is running or parked (one shared Chromium). Nothing refuses a run over what jev logs: the graph tab shows
//! jev's `decisions_log` (`GET /hub/runs/health` passes it on) and the operator decides (ruling of 2026-10-07).

pub mod graphs;
pub mod health;
pub mod log;
pub mod proc;
pub mod state;

use hyper::body::Incoming;
use hyper::{Request, Response};

use crate::serve::{BoxBody, Ctx};

/// What the runs keep in memory: the runs and `runs.json` ([`state::Store`], opened by [`adopt`]), the health probe's cache, and how many
/// stop sequences (a cancel's 5 s grace) are in flight, which is the one thing [`on_shutdown`] waits for. `Default` is how [`Ctx`] gets one
/// before any hook runs, so what needs the options or the home is filled by the hook.
#[derive(Default)]
pub struct State {
    pub(crate) store: state::Store,
    pub(crate) health: health::Cache,
    pub(crate) stopping: std::sync::atomic::AtomicUsize,
}

/// Where a request under `/hub/runs/` goes, and what it is told is its `rest`.
#[derive(Debug, PartialEq, Eq)]
enum Pick<'a> {
    Health,
    Graphs(&'a str),
    Log(&'a str),
    State(&'a str),
}

fn pick(rest: &str) -> Pick<'_> {
    if rest == "health" {
        return Pick::Health;
    }
    if rest == "graphs" {
        return Pick::Graphs("");
    }
    if let Some(graph) = rest.strip_prefix("graphs/") {
        return Pick::Graphs(graph);
    }
    match rest.strip_suffix("/log") {
        Some(rid) if !rid.is_empty() && !rid.contains('/') => Pick::Log(rid),
        _ => Pick::State(rest),
    }
}

/// `/hub/runs` and everything under it. `rest` is `""` for the bare path.
pub async fn route(ctx: &Ctx, req: Request<Incoming>, rest: &str) -> Response<BoxBody> {
    match pick(rest) {
        Pick::Health => health::get(ctx, req, "").await,
        Pick::Graphs(graph) => graphs::get(ctx, req, graph).await,
        Pick::Log(rid) => log::get(ctx, req, rid).await,
        Pick::State(rest) => state::route(ctx, req, rest).await,
    }
}

/// After the bind (lib.rs): find the runs a previous perch left detached and take them up again (D3).
pub fn adopt(ctx: &Ctx) {
    proc::adopt(ctx)
}

/// At shutdown, after the terminals and before the doors (serve.rs). A detached run is **not** killed here (D3): it outlives the perch
/// and the next one adopts it. What this waits for is only what the perch itself owns, and it bounds its own wait.
pub async fn on_shutdown(ctx: &Ctx) {
    proc::on_shutdown(ctx).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dispatch_names_one_handler_per_shape() {
        assert_eq!(pick("health"), Pick::Health);
        assert_eq!(pick("graphs"), Pick::Graphs(""));
        assert_eq!(pick("graphs/wiki-hop"), Pick::Graphs("wiki-hop"));
        assert_eq!(pick("r12/log"), Pick::Log("r12"));
        // The rest is the state's: the list, a run, and its verbs.
        for rest in ["", "r12", "r12/answer", "r12/cancel", "r12/abandon", "r12/other"] {
            assert_eq!(pick(rest), Pick::State(rest), "{rest:?}");
        }
        // A log is exactly `<rid>/log`: nothing nested, nothing empty, and a longer word is not `health` or `graphs`.
        for rest in ["/log", "a/b/log", "r12/logs", "healthy", "graphsx", "health/x"] {
            assert_eq!(pick(rest), Pick::State(rest), "{rest:?}");
        }
    }
}
