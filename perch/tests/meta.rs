//! `/hub/meta/*`: the sources of the loaded and diff tabs (slice R3).
//!
//! A stub until R3. The seam answers the empty 404 behind the gate, and the fake's `tools`, `plugins` and `policy-rows` commands are
//! stubs that refuse; R3 replaces both pins with the real tests (and `src/bin/fake_eidolon/meta.rs` with the stock text).

mod common;

use common::*;

#[test]
fn meta_is_the_gate_and_a_404_until_r3() {
    let perch = Perch::start();
    perch.assert_not_built("GET", "/hub/meta/tools");
    perch.assert_not_built("GET", "/hub/meta/policy");
}

#[cfg(feature = "test-bins")]
#[test]
fn the_fakes_meta_commands_refuse_until_r3() {
    for name in ["tools", "plugins", "policy-rows"] {
        let out = std::process::Command::new(fake_eidolon().expect("the stand-in door")).args(["--config", "/nonexistent", name]).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{name}: {out:?}");
        assert!(String::from_utf8_lossy(&out.stderr).contains(&format!("`{name}` is not built yet")), "{name}: {out:?}");
        assert!(out.stdout.is_empty(), "{name}: a refusal prints nothing on stdout: {out:?}");
    }
}
