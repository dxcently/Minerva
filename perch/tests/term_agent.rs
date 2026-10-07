//! `/hub/term/a/<verb>`: the agent's terminal tools (slice T2), behind AG's scoped term token.
//!
//! A stub until T2. The agent gate answers a master bearer or a login cookie on an agent path with the empty 404 (a real credential, for
//! another door) and a caller with no credential with the empty 401 (`tests/agent.rs` has the whole table); T2's routes sit behind it,
//! and T2 replaces this pin with the list, open, read, send and close tests, driven by the fake door's `FAKE_AGENT_CALL`.

mod common;

use common::*;

#[test]
fn the_agent_terminal_routes_are_a_404_for_the_master_bearer_until_t2() {
    let perch = Perch::start();
    for verb in ["list", "open", "read", "send", "close"] {
        let path = format!("/hub/term/a/{verb}");
        let answer = perch.apireq("POST", &path).send(&perch.addr);
        assert_eq!(answer.status, 404, "POST {path} with the master bearer: {}", answer.head);
        assert_eq!(answer.body, "", "POST {path}: the refusal has no body");
        let stranger = Request::new(&perch.addr, "POST", &path).send(&perch.addr);
        assert_eq!(stranger.status, 401, "POST {path} with no credential: {}", stranger.head);
    }
}
