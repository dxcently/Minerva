//! `GET /hub/term/events?since=...` (slice T1c): the one SSE every terminal rides, ending when the perch stops (`ctx.stop`).

use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};

use crate::serve::{status, BoxBody, Ctx};

/// `rest` is always `""`; the query (`since`) is on the request.
pub async fn events(_ctx: &Ctx, _req: Request<Incoming>, _rest: &str) -> Response<BoxBody> {
    status(StatusCode::NOT_FOUND)
}
