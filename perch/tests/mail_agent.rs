//! `POST /hub/mail/a/send` and `POST /hub/sessions/<id>/mailsend`: the agent's mail, opt-in per chat (slice M3).
//!
//! A stub until M3. The agent gate refuses every agent path with 401 until AG, and the opt-in route is the empty 404 behind the master
//! gate; M3 replaces these pins with the opt-in, argv, rate and attribution tests.

mod common;

use common::*;

#[test]
fn agent_mail_is_a_401_and_the_opt_in_a_404_until_m3() {
    let perch = Perch::start();
    let answer = perch.apireq("POST", "/hub/mail/a/send").send(&perch.addr);
    assert_eq!(answer.status, 401, "the master bearer on the agent route: {}", answer.head);
    assert_eq!(answer.body, "", "the refusal has no body");
    perch.assert_not_built("POST", "/hub/sessions/x/mailsend");
}
