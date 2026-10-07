//! `/hub/mail`: Aoide mail, sent from a note and received as cards (slices M2, M3).
//!
//! Mail goes through the Aoide CLI only, never a transport of the perch's own. The routes are split by who fills them:
//!
//! ```text
//!   GET  /hub/mail/nodes            send::nodes       the nodes a note can be sent to                         (M2)
//!   POST /hub/mail/a/<verb>         agent::route      the agent's `mail_send` and whoami: scoped token        (M3)
//!   everything else                 recv::route       GET /hub/mail, POST /hub/mail/<msgid>/place|dismiss|save (M2)
//!
//!   POST /hub/notes/<n>/send        mail::send_note   called by `notes`, which owns the route                 (M2)
//!   POST /hub/sessions/<id>/mailsend mail::optin      a chat's opt-in to agent mail, operator only            (M3)
//! ```
//!
//! [`pick`] is the whole dispatch below `/hub/mail` and is a plain function so a test can reach it. A message id is 64 hex digits, so
//! `nodes` and `a` are never ids. As for `term`, an agent path is routed as one only if the gate took it as one: `serve` asks
//! [`crate::agent::agent_scope`] before the master check and [`pick`] asks the same predicate ([`crate::agent::verb`]).
//!
//! Every handler has one signature, `(ctx, req, rest) -> Response`, where `rest` is the part of the path below the handler's own
//! prefix: `""` for `nodes`, the verb for `agent`, the whole rest for `recv`; for `send_note` the note's name and for `optin` the
//! chat's id (already checked by `sessions::id_ok`).

pub mod agent;
pub mod recv;
pub mod send;

use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};

use crate::serve::{status, BoxBody, Ctx};

/// What the mail side keeps: the card store, the read cursor's last stat, the agent opt-ins. Filled by [`start`] and M2/M3's handlers;
/// `Default` is how [`Ctx`] gets one before any hook runs.
#[derive(Default)]
pub struct State {}

/// Where a request under `/hub/mail/` goes, and what it is told is its `rest`.
#[derive(Debug, PartialEq, Eq)]
enum Pick<'a> {
    Nodes,
    Agent(&'a str),
    Cards(&'a str),
    /// `a` or `a/...` that is not exactly `a/<verb>`: nobody's route.
    Nobody,
}

fn pick(rest: &str) -> Pick<'_> {
    if rest == "nodes" {
        return Pick::Nodes;
    }
    if let Some(verb) = crate::agent::verb(rest) {
        return Pick::Agent(verb);
    }
    if rest == "a" || rest.starts_with("a/") {
        return Pick::Nobody;
    }
    Pick::Cards(rest)
}

/// `/hub/mail` and everything under it. `rest` is `""` for the bare path.
pub async fn route(ctx: &Ctx, req: Request<Incoming>, rest: &str) -> Response<BoxBody> {
    match pick(rest) {
        Pick::Nodes => send::nodes(ctx, req, "").await,
        Pick::Agent(verb) => agent::route(ctx, req, verb).await,
        Pick::Cards(rest) => recv::route(ctx, req, rest).await,
        Pick::Nobody => status(StatusCode::NOT_FOUND),
    }
}

/// `POST /hub/notes/<name>/send {node, mesh?, hold?}`: the notes route delegates here. `name` is the note's.
pub async fn send_note(ctx: &Ctx, req: Request<Incoming>, name: &str) -> Response<BoxBody> {
    send::send_note(ctx, req, name).await
}

/// `POST /hub/sessions/<id>/mailsend {on}`: whether this chat's agent may send mail. `id` is a checked session id.
pub async fn optin(ctx: &Ctx, req: Request<Incoming>, id: &str) -> Response<BoxBody> {
    agent::optin(ctx, req, id).await
}

/// After the bind (lib.rs): the receive task, which only works while a page is looking.
pub fn start(ctx: &Ctx) {
    recv::start(ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{agent_scope, Scope};

    #[test]
    fn the_dispatch_names_one_handler_per_shape() {
        assert_eq!(pick("nodes"), Pick::Nodes);
        assert_eq!(pick("a/send"), Pick::Agent("send"));
        assert_eq!(pick("a/whoami"), Pick::Agent("whoami"));
        let msgid = "0123456789abcdef".repeat(4);
        let place = format!("{msgid}/place");
        for rest in ["", msgid.as_str(), place.as_str(), "nodes/x", "nodesx", "ab/send", "b/send"] {
            assert_eq!(pick(rest), Pick::Cards(rest), "{rest:?}");
        }
        for rest in ["a", "a/", "a/x/y", "a/Send", "a/%73end"] {
            assert_eq!(pick(rest), Pick::Nobody, "{rest:?}");
        }
    }

    #[test]
    fn a_path_is_an_agent_route_exactly_when_the_gate_took_it_as_one() {
        for path in [
            "/hub/mail",
            "/hub/mail/",
            "/hub/mail/nodes",
            "/hub/mail/a",
            "/hub/mail/a/",
            "/hub/mail/a/send",
            "/hub/mail/a/x/y",
            "/hub/mail/a/%73end",
            "/hub/mail/%61/send",
            "/hub/mail/t1/a/send",
            "/hub/mail//a/send",
            "/hub/mail/a//send",
            "/hub/mail//a/x",
        ] {
            let rest = crate::serve::below(path, "/hub/mail").expect("every path in the table is in the namespace");
            let dispatched = matches!(pick(rest), Pick::Agent(_));
            assert_eq!(dispatched, agent_scope(path) == Some(Scope::Mail), "{path}");
        }
    }
}
