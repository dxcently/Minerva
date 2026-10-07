//! `GET /hub/runs/graphs` and `GET /hub/runs/graphs/<g>` (slice R1b): the graph JSON and its hash, for the new-run form.

use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};

use crate::serve::{status, BoxBody, Ctx};

/// `rest` is `""` for the list and the graph's name for one graph; a name outside `--run-graphs` is the empty 404.
pub async fn get(_ctx: &Ctx, _req: Request<Incoming>, _rest: &str) -> Response<BoxBody> {
    status(StatusCode::NOT_FOUND)
}
