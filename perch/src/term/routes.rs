//! The operator's terminal routes (slice T1b): `GET|POST /hub/term` and `/hub/term/<t>/input|resize|hold|close|text`.

use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};

use crate::serve::{status, BoxBody, Ctx};

/// `rest` is `""` or `<t>/<verb>`; `events` and `a/...` never get here (`term::pick`), so a terminal id (`t<n>`) is all that is left.
pub async fn route(_ctx: &Ctx, _req: Request<Incoming>, _rest: &str) -> Response<BoxBody> {
    status(StatusCode::NOT_FOUND)
}
