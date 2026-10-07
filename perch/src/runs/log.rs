//! `GET /hub/runs/<rid>/log?after=N` (slice R1b): the run's journal as `eidolon log --json --after N` lines.

use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};

use crate::serve::{status, BoxBody, Ctx};

/// `rest` is the run id, one path segment: `runs::pick` hands over `<rid>` of `<rid>/log` and nothing nested.
pub async fn get(_ctx: &Ctx, _req: Request<Incoming>, _rest: &str) -> Response<BoxBody> {
    status(StatusCode::NOT_FOUND)
}
