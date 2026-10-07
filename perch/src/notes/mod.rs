//! `/hub/notes[/...]`: the Notepad (slice N1): `<home>/notes/<name>.md`, written `0600`, with a trash beside it.
//!
//! Stub until N1: the empty 404. `POST /hub/notes/<n>/send` is a notes route that delegates to [`crate::mail::send_note`].

use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};

use crate::serve::{status, BoxBody, Ctx};

/// What the notes keep in memory. Filled by N1; `Default` is how [`Ctx`] gets one before any hook runs.
#[derive(Default)]
pub struct State {}

/// `/hub/notes` and everything under it. `rest` is `""` for the bare path, `<n>`, `<n>/rename`, `trash`, and so on.
pub async fn route(_ctx: &Ctx, _req: Request<Incoming>, _rest: &str) -> Response<BoxBody> {
    status(StatusCode::NOT_FOUND)
}
