//! Receiving mail as cards (slice M2): `GET /hub/mail`, `POST /hub/mail/<msgid>/place|dismiss|save`, and the task that reads the
//! Aoide mailbox while a page is looking.

use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};

use crate::serve::{status, BoxBody, Ctx};

/// `rest` is `""` or `<msgid>/<verb>`; `nodes` and `a/...` never get here (`mail::pick`).
pub async fn route(_ctx: &Ctx, _req: Request<Incoming>, _rest: &str) -> Response<BoxBody> {
    status(StatusCode::NOT_FOUND)
}

/// The receive task (started after the bind, runs only while a page has polled in the last minute). Stub: starts nothing.
pub fn start(_ctx: &Ctx) {}
