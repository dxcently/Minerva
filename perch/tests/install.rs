//! `/hub/install`: the install notice and the update keys (slice R4), driven through `fake_minerva`.
//!
//! A stub until R4. The seam answers the empty 404 behind the gate, and `fake_minerva` refuses every argv; R4 replaces both pins with the
//! notice, arm and key tests.

mod common;

use common::*;

#[test]
fn install_is_the_gate_and_a_404_until_r4() {
    let perch = Perch::start();
    perch.assert_not_built("GET", "/hub/install");
    perch.assert_not_built("GET", "/hub/install/keys");
    perch.assert_not_built("POST", "/hub/install/keys/arm");
}

#[cfg(feature = "test-bins")]
#[test]
fn the_fake_minerva_refuses_every_argv_until_r4() {
    for argv in [&["status", "--json"][..], &["keys", "list", "--json"], &["update"]] {
        let out = std::process::Command::new(fake_minerva().expect("the stand-in minerva")).args(argv).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{argv:?}: {out:?}");
        assert!(out.stdout.is_empty(), "{argv:?}: a refusal prints nothing on stdout, which a caller would parse: {out:?}");
    }
}
