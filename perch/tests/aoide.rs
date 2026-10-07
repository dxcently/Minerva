//! The Aoide runner (slice R2a): `perch::aoide`, driven through `fake_aoide`.
//!
//! A stub until R2a. The runner has no code yet and the fake refuses every argv with its own exit code (99, the code the real contract
//! gives "anything else"); R2a replaces this pin with the argv, timeout and cap tests.

mod common;

// Used only by the `test-bins` test below; a plain `cargo test` builds this file with nothing to run.
#[allow(unused_imports)]
use common::*;

#[cfg(feature = "test-bins")]
#[test]
fn the_fake_aoide_refuses_every_argv_until_r2a() {
    for argv in [&["--version"][..], &["mesh", "--json"], &["mail", "outbox", "--json"], &["anything", "else"]] {
        let out = std::process::Command::new(fake_aoide().expect("the stand-in aoide")).args(argv).output().unwrap();
        assert_eq!(out.status.code(), Some(99), "{argv:?}: {out:?}");
        assert!(out.stdout.is_empty(), "{argv:?}: a refusal prints nothing on stdout, which a caller would parse: {out:?}");
    }
}
