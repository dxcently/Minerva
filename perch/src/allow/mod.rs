//! "Allow for the rest of this chat": rules the operator makes from an ask, and the answerer that applies them (slices A1a, A1b).
//!
//! The watcher reads every door's asks ([`crate::watch`]) and tells a [`watch::Sink`] about them; [`State`] is that sink, so
//! `serve::door_up` hands `ctx.allow` to `watch::open` and the rules are matched with no page open. The three routes are session
//! verbs, handed over by `serve` before the body is read:
//!
//! ```text
//!   GET  /hub/sessions/<id>/allow    list     the chat's mode and rules
//!   POST /hub/sessions/<id>/allow    create   a rule built from the watcher's own copy of a pending ask
//!   POST /hub/sessions/<id>/revoke   revoke   one rule
//! ```
//!
//! Every handler has the one signature `(ctx, req, id) -> Response`, where `id` is a session id already checked by `sessions::id_ok`.
//! The pure matcher is `matcher`, the rule storage `store`.

pub mod matcher;
pub mod store;

use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};

use crate::serve::{status, BoxBody, Ctx};
use crate::watch;

/// The rules and what the sink needs to bind a door to a chat. Filled by A1b; `Default` is how [`Ctx`] gets one before any hook runs.
#[derive(Default)]
pub struct State {}

/// The sink is a no-op until A1b: `hello` and `frame` are the trait's defaults.
impl watch::Sink for State {}

/// `GET /hub/sessions/<id>/allow`.
pub async fn list(_ctx: &Ctx, _req: Request<Incoming>, _id: &str) -> Response<BoxBody> {
    status(StatusCode::NOT_FOUND)
}

/// `POST /hub/sessions/<id>/allow {ask_id}`.
pub async fn create(_ctx: &Ctx, _req: Request<Incoming>, _id: &str) -> Response<BoxBody> {
    status(StatusCode::NOT_FOUND)
}

/// `POST /hub/sessions/<id>/revoke {rid}`.
pub async fn revoke(_ctx: &Ctx, _req: Request<Incoming>, _id: &str) -> Response<BoxBody> {
    status(StatusCode::NOT_FOUND)
}
