//! `/hub/meta/...`: the sources for the loaded and diff tabs (slice R3): the tools, plugins and policy eidolon reports, the guard's
//! status, the home's defaults, and a chat's git head read from `.git/HEAD` without running git.
//!
//! Stub until R3: the empty 404. The guard line comes from [`crate::guard::status`].

use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};

use crate::serve::{status, BoxBody, Ctx};

/// The cached `eidolon tools|plugins|policy-rows` answers. Filled by R3; `Default` is how [`Ctx`] gets one.
#[derive(Default)]
pub struct State {}

/// `/hub/meta/<what>`. `rest` is the path below `/hub/meta/` (`tools`, `policy`, `git`, ...), never empty.
pub async fn route(_ctx: &Ctx, _req: Request<Incoming>, _rest: &str) -> Response<BoxBody> {
    status(StatusCode::NOT_FOUND)
}
