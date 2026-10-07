//! `/hub/notes`: the notepad's storage and routes (slice N1).
//!
//! A stub until N1. The seam answers the empty 404 behind the gate; N1 replaces this pin with the name, cap, etag, rename and trash tests.

mod common;

use common::*;

#[test]
fn notes_are_the_gate_and_a_404_until_n1() {
    let perch = Perch::start();
    perch.assert_not_built("GET", "/hub/notes");
    perch.assert_not_built("POST", "/hub/notes");
    perch.assert_not_built("GET", "/hub/notes/trash");
}
