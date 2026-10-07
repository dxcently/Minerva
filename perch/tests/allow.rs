//! `/hub/sessions/<id>/allow` and `/revoke`: allow rules for a chat (slice A1b), over the fake door's `FAKE_ASKS`.
//!
//! A stub until A1b. The seams answer the empty 404 behind the gate; A1b replaces this pin with the matcher, store and answerer tests.

mod common;

use common::*;

#[test]
fn allow_and_revoke_are_the_gate_and_a_404_until_a1b() {
    let perch = Perch::start();
    perch.assert_not_built("GET", "/hub/sessions/x/allow");
    perch.assert_not_built("POST", "/hub/sessions/x/allow");
    perch.assert_not_built("POST", "/hub/sessions/x/revoke");
}
