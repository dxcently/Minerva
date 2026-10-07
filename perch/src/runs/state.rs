//! The runs' state and the routes that read and change it (slice R1a): `<home>/runs.json`, the per-run dir, the start gate, and
//! `GET|POST /hub/runs`, `GET /hub/runs/<rid>`, `POST /hub/runs/<rid>/answer|cancel|abandon`.

use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};

use crate::serve::{status, BoxBody, Ctx};

/// Everything under `/hub/runs` that is not `health`, `graphs` or a log (see `runs::pick`). `rest` is `""`, `<rid>` or `<rid>/<verb>`.
pub async fn route(_ctx: &Ctx, _req: Request<Incoming>, _rest: &str) -> Response<BoxBody> {
    status(StatusCode::NOT_FOUND)
}
