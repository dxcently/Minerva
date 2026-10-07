//! `/hub/term`: web terminals the perch hosts (slices T1a, T1b, T1c, T2).
//!
//! The perch owns the PTYs and a screen copy of each; the page draws them and an agent's `term_*` tools drive them. The routes are
//! split by who fills them:
//!
//! ```text
//!   GET  /hub/term/events        stream::events   the one SSE every terminal rides                          (T1c)
//!   POST /hub/term/a/<verb>      agent::route     list, open, read, send, close, whoami: scoped token         (T2)
//!   everything else              routes::route    GET|POST /hub/term, /<t>/input|resize|hold|close|text       (T1b)
//! ```
//!
//! `pty` (the PTY, its reader and writer threads, close and shutdown), `screen` (the alacritty copy) and `snap` (the join snapshot)
//! are the host and have no route. [`pick`] is the whole dispatch and is a plain function so a test can reach it. A terminal id is
//! `t<n>`, so `events` and `a` are never ids, which is what lets the two words be matched ahead of the catch-all.
//!
//! **An agent path is routed as one only if the gate took it as one.** `serve` asks [`crate::agent::agent_scope`] before the master
//! check, and [`pick`] asks the same predicate ([`crate::agent::verb`]); `a` and `a/...` in any other shape are a 404 here, so an
//! operator's master token never reaches an agent handler by a spelling the gate did not recognise.
//!
//! Every handler has one signature, `(ctx, req, rest) -> Response`, where `rest` is the part of the path below the handler's own
//! prefix: `""` for `events`, the verb for `agent`, the whole rest for `routes`.

pub mod agent;
pub mod pty;
pub mod routes;
pub mod screen;
pub mod snap;
pub mod stream;

use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};

use crate::serve::{status, BoxBody, Ctx};

/// The terminals: T1a's `Terms`. Filled by T1a; `Default` is how [`Ctx`] gets one before any hook runs.
#[derive(Default)]
pub struct State {}

/// Where a request under `/hub/term/` goes, and what it is told is its `rest`.
#[derive(Debug, PartialEq, Eq)]
enum Pick<'a> {
    Events,
    Agent(&'a str),
    Operator(&'a str),
    /// `a` or `a/...` that is not exactly `a/<verb>`: nobody's route.
    Nobody,
}

fn pick(rest: &str) -> Pick<'_> {
    if rest == "events" {
        return Pick::Events;
    }
    if let Some(verb) = crate::agent::verb(rest) {
        return Pick::Agent(verb);
    }
    if rest == "a" || rest.starts_with("a/") {
        return Pick::Nobody;
    }
    Pick::Operator(rest)
}

/// `/hub/term` and everything under it. `rest` is `""` for the bare path.
pub async fn route(ctx: &Ctx, req: Request<Incoming>, rest: &str) -> Response<BoxBody> {
    match pick(rest) {
        Pick::Events => stream::events(ctx, req, "").await,
        Pick::Agent(verb) => agent::route(ctx, req, verb).await,
        Pick::Operator(rest) => routes::route(ctx, req, rest).await,
        Pick::Nobody => status(StatusCode::NOT_FOUND),
    }
}

/// At shutdown, before the doors are stopped (serve.rs): end every terminal. Bounded by the host (2.5 s), not by the caller.
pub async fn shutdown(ctx: &Ctx) {
    pty::shutdown(ctx).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{agent_scope, Scope};

    #[test]
    fn the_dispatch_names_one_handler_per_shape() {
        assert_eq!(pick("events"), Pick::Events);
        assert_eq!(pick("a/list"), Pick::Agent("list"));
        assert_eq!(pick("a/whoami"), Pick::Agent("whoami"));
        for rest in ["", "t1", "t1/input", "t12/text", "events/x", "eventsx", "t1/a/list", "ab/list"] {
            assert_eq!(pick(rest), Pick::Operator(rest), "{rest:?}");
        }
        for rest in ["a", "a/", "a/x/y", "a/List", "a/%6cist", "a/x.y"] {
            assert_eq!(pick(rest), Pick::Nobody, "{rest:?}");
        }
    }

    /// The gate and the dispatch cannot disagree: a path is an agent handler's exactly when `serve` took it through the scoped gate.
    #[test]
    fn a_path_is_an_agent_route_exactly_when_the_gate_took_it_as_one() {
        for path in [
            "/hub/term",
            "/hub/term/",
            "/hub/term/events",
            "/hub/term/a",
            "/hub/term/a/",
            "/hub/term/a/list",
            "/hub/term/a/x/y",
            "/hub/term/a/%6cist",
            "/hub/term/%61/list",
            "/hub/term/t1/input",
            "/hub/term/t1/a/list",
            "/hub/term//a/list",
            "/hub/term/a//list",
            "/hub/term///a/list",
        ] {
            let rest = crate::serve::below(path, "/hub/term").expect("every path in the table is in the namespace");
            let dispatched = matches!(pick(rest), Pick::Agent(_));
            assert_eq!(dispatched, agent_scope(path) == Some(Scope::Term), "{path}");
        }
    }
}
