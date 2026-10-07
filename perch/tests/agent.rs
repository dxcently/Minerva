//! The agent gate (slice AG): the scoped term and mail tokens, and which chat a call came from.
//!
//! A stub until AG. Its gate refuses every agent path with 401, a master bearer included, which is the fail-closed answer; AG replaces
//! this pin with the token, attribution and `whoami` tests (and `src/bin/fake_eidolon/agent.rs` with the fake door's own call).

mod common;

use common::*;

#[test]
fn an_agent_path_is_a_401_for_everyone_until_ag() {
    let perch = Perch::start();
    for (method, path) in [("POST", "/hub/term/a/whoami"), ("POST", "/hub/mail/a/whoami")] {
        let bare = Request::new(&perch.addr, method, path).send(&perch.addr);
        assert_eq!(bare.status, 401, "{method} {path} with no credential: {}", bare.head);
        // The master bearer is the credential of every other /hub route and must open nothing here.
        let master = perch.apireq(method, path).send(&perch.addr);
        assert_eq!(master.status, 401, "{method} {path} with the master bearer: {}", master.head);
        assert_eq!(master.body, "", "{method} {path}: the refusal has no body");
    }
}
