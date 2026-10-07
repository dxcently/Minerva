//! Agent mail (slice M3): `POST /hub/mail/a/<verb>` behind the agent gate, and the operator's per-chat opt-in.
//!
//! By the time a request reaches [`route`], its scoped mail token and its chat were checked, and the chat is in the request's
//! extensions as a [`crate::agent::Caller`]. [`optin`] is the operator's, behind the master gate like every `/hub/sessions` verb.

use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};

use crate::serve::{status, BoxBody, Ctx};

/// `rest` is the verb (`send`, `whoami`), already checked by [`crate::agent::verb`]: lower case, no `/`.
pub async fn route(_ctx: &Ctx, _req: Request<Incoming>, _rest: &str) -> Response<BoxBody> {
    status(StatusCode::NOT_FOUND)
}

/// `POST /hub/sessions/<id>/mailsend {on}`. `rest` is the session id, already checked by `sessions::id_ok`.
pub async fn optin(_ctx: &Ctx, _req: Request<Incoming>, _rest: &str) -> Response<BoxBody> {
    status(StatusCode::NOT_FOUND)
}
