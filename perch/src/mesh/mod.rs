//! `/hub/mesh`: the mesh view (slice R2b). Network-free: it reads Aoide's own files and asks the `aoide` CLI for `mesh --json`
//! and `mail outbox --json`, and never runs anything that touches a node.
//!
//! Stub until R2b: the empty 404, which is what `GET /hub/mesh` and `/hub/mesh/events` answered before this module existed.
//! R2b also exports `nodes(ctx)` for the mail send route.

use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};

use crate::serve::{status, BoxBody, Ctx};

/// The mesh's caches (`mesh --json`, `mail outbox --json`, the events cursor). Filled by R2b; `Default` is how [`Ctx`] gets one.
#[derive(Default)]
pub struct State {}

/// `/hub/mesh` and everything under it. `rest` is `""` for the bare path, `events` for the tail, and so on.
pub async fn route(_ctx: &Ctx, _req: Request<Incoming>, _rest: &str) -> Response<BoxBody> {
    status(StatusCode::NOT_FOUND)
}
