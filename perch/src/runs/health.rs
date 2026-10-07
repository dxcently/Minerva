//! `GET /hub/runs/health` (slice R1a): the probes of `--jev-health` and `--browser-health`, a second at most each, cached for a few.

use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};

use crate::serve::{status, BoxBody, Ctx};

/// `rest` is always `""`.
pub async fn get(_ctx: &Ctx, _req: Request<Incoming>, _rest: &str) -> Response<BoxBody> {
    status(StatusCode::NOT_FOUND)
}
