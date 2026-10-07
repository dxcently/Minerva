//! The watcher's ask reader and the fake door's asks (slice A1a): `FAKE_ASKS`, `FAKE_LAG`, the 409 for a second answer.
//!
//! A stub until A1a. The fake's ask hooks do nothing, so a second answer to one ask is as good as the first; A1a replaces this pin with
//! the frame-reader, origin, reconnect and slow-sink tests (and `src/bin/fake_eidolon/asks.rs` with the scripted asks).

mod common;

// Used only by the `test-bins` test below; a plain `cargo test` builds this file with nothing to run.
#[allow(unused_imports)]
use common::*;

#[cfg(feature = "test-bins")]
#[test]
fn the_fake_door_settles_a_second_answer_too_until_a1a() {
    let perch = Perch::start_hub(&[]);
    let (id, _pid, _port) = live(&perch);
    for _ in 0..2 {
        let answered = perch.post_json(&format!("/s/{id}/api/answer"), serde_json::json!({ "ask_id": 1, "answer": "yes" }));
        assert_eq!(answered.status, 204, "{}", answered.head);
    }
}
