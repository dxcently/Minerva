//! `/hub/install[/...]`: the install notice and the update keys (slice R4). The perch asks the `minerva` binary beside it (or
//! `--minerva`, never a `PATH` lookup) with fixed argv; it never updates and never touches the network.
//!
//! Stub until R4: the empty 404.

use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};

use crate::serve::{status, BoxBody, Ctx};

/// The armed keys (an arm is one fingerprint, 120 s, single use) and the one-writer lock. Filled by R4; `Default` is how [`Ctx`] gets one.
#[derive(Default)]
pub struct State {}

/// `/hub/install` and everything under it. `rest` is `""` for the bare path, `keys`, `keys/add`, `keys/arm`, and so on.
pub async fn route(_ctx: &Ctx, _req: Request<Incoming>, _rest: &str) -> Response<BoxBody> {
    status(StatusCode::NOT_FOUND)
}
