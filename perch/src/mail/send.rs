//! Sending a note as mail (slice M2): `GET /hub/mail/nodes` and the body of `POST /hub/notes/<n>/send`.

use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};

use crate::serve::{status, BoxBody, Ctx};

/// `GET /hub/mail/nodes`. `rest` is always `""`.
pub async fn nodes(_ctx: &Ctx, _req: Request<Incoming>, _rest: &str) -> Response<BoxBody> {
    status(StatusCode::NOT_FOUND)
}

/// `POST /hub/notes/<n>/send {node, mesh?, hold?}`, called by `notes` through `mail::send_note`. `rest` is the note's name.
pub async fn send_note(_ctx: &Ctx, _req: Request<Incoming>, _rest: &str) -> Response<BoxBody> {
    status(StatusCode::NOT_FOUND)
}
