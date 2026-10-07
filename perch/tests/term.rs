//! `/hub/term`: the operator's terminal routes (slice T1b), over a real `/bin/sh`.
//!
//! A stub until T1b. The seam answers the empty 404 behind the gate; T1b replaces this pin with the create, input, resize, hold, close and
//! text tests.

mod common;

use common::*;

#[test]
fn terminals_are_the_gate_and_a_404_until_t1b() {
    let perch = Perch::start();
    perch.assert_not_built("GET", "/hub/term");
    perch.assert_not_built("POST", "/hub/term");
    perch.assert_not_built("POST", "/hub/term/t1/input");
}
