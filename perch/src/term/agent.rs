//! The agent's terminal routes, `POST /hub/term/a/<verb>` (slice T2): list, open, read, send, close.
//!
//! Behind the agent gate ([`crate::agent`]): by the time a request is here, its scoped term token and its chat were checked, and the
//! chat is in the request's extensions as a [`crate::agent::Caller`]. The stub answers the empty 404 until T2.

use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};

use crate::serve::{status, BoxBody, Ctx};

/// `rest` is the verb, already checked by [`crate::agent::verb`]: lower case, no `/`.
pub async fn route(_ctx: &Ctx, _req: Request<Incoming>, _rest: &str) -> Response<BoxBody> {
    status(StatusCode::NOT_FOUND)
}
