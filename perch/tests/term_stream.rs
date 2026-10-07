//! `GET /hub/term/events`: the one SSE for every terminal (slice T1c).
//!
//! A stub until T1c. The seam answers the empty 404 behind the gate; T1c replaces this pin with the replay, reset, keep-alive and shutdown
//! tests (`Pane::open_path` in `common` reads such a stream).

mod common;

use common::*;

#[test]
fn the_terminal_stream_is_the_gate_and_a_404_until_t1c() {
    let perch = Perch::start();
    perch.assert_not_built("GET", "/hub/term/events");
}
