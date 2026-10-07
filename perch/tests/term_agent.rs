//! `/hub/term/a/<verb>`: the agent's terminal tools (slice T2), behind AG's scoped term token.
//!
//! A stub until T2. The agent gate refuses every agent path with 401 until AG, and T2's routes sit behind it; T2 replaces this pin with
//! the list, open, read, send and close tests, driven by the fake door's `FAKE_AGENT_CALL`.

mod common;

use common::*;

#[test]
fn the_agent_terminal_routes_are_a_401_until_t2() {
    let perch = Perch::start();
    for verb in ["list", "open", "read", "send", "close"] {
        let path = format!("/hub/term/a/{verb}");
        let answer = perch.apireq("POST", &path).send(&perch.addr);
        assert_eq!(answer.status, 401, "POST {path} with the master bearer: {}", answer.head);
        assert_eq!(answer.body, "", "POST {path}: the refusal has no body");
    }
}
