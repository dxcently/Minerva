//! `FAKE_ASKS`, `FAKE_LAG`: the door's open asks, for the watcher and the allow rules. **Stubs until slice A1a.**
//!
//! A1a fills these three hooks, and keeps whatever state they share in this file (a `static`); `fake_eidolon.rs` only calls them, at the
//! three places below, and `Fake`'s fields are reachable from here (`fake.frames` is the broadcast every open `/api/events` stream
//! relays). Every hook is a no-op today, so the door behaves exactly as it did before they existed.
//!
//! - `FAKE_ASKS=<file of frames>`: after a `say`, the frames in that file are emitted (`tool-call-started` with its `origin`, then `ask`).
//! - a second `POST /api/answer` for one ask answers `409`.
//! - `FAKE_LAG=1`: a stream is ended with a `lagged` frame, so the perch must reconnect, and the asks still open are replayed to the new one.

use std::sync::Arc;

use super::Fake;

/// What a new `/api/events` stream is given after `hello`, before it settles into relaying the broadcast.
#[derive(Default)]
pub struct Opening {
    /// Raw SSE bytes (`data: {...}\n\n`, any number of frames) written at once: the asks still open, replayed as the real door replays
    /// its pending asks to a client that connects late.
    pub replay: Vec<u8>,
    /// End the stream after `replay` (the `FAKE_LAG=1` case: a `lagged` frame, then the close).
    pub end: bool,
}

/// `POST /api/say` was accepted and answered 202. Emit what `FAKE_ASKS` names onto `fake.frames`; spawn rather than block, because this
/// runs on the connection that carried the `say`.
pub fn after_say(_fake: &Arc<Fake>) {}

/// `POST /api/answer {ask_id, answer, note?}` passed the token, `Host` and shape checks; `body` is the parsed JSON. `Some((status line,
/// body))` is sent instead of the usual `204` and the `ask-settled` frame (a second answer: `("409 Conflict", ...)`); `None` carries on.
/// The answer log the allow tests read is written here too.
pub fn on_answer(_fake: &Fake, _body: &serde_json::Value) -> Option<(&'static str, &'static str)> {
    None
}

/// The `nth` `/api/events` stream (0 is the perch's watcher) has just been sent `hello`.
pub fn on_stream(_fake: &Fake, _nth: usize) -> Opening {
    Opening::default()
}
