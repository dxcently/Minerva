//! `/hub/mail` and `POST /hub/notes/<n>/send`: send a note, receive cards (slice M2), over `fake_aoide`.
//!
//! A stub until M2. The seam answers the empty 404 behind the gate; M2 replaces this pin with the send, stat-gate, reconcile and card tests.

mod common;

use common::*;

#[test]
fn mail_is_the_gate_and_a_404_until_m2() {
    let perch = Perch::start();
    perch.assert_not_built("GET", "/hub/mail");
    perch.assert_not_built("GET", "/hub/mail/nodes");
}
