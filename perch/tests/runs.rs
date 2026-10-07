//! `/hub/runs`: start, answer, cancel, abandon and the health gate (slice R1a), the log poll and graphs (R1b).
//!
//! A stub until R1a. The seam answers the empty 404 behind the gate, and the fake's `workflow` command is a stub that refuses; R1a
//! replaces both pins with the real tests (and `src/bin/fake_eidolon/workflow.rs` with the replay).

mod common;

use common::*;

#[test]
fn runs_are_the_gate_and_a_404_until_r1a() {
    let perch = Perch::start();
    perch.assert_not_built("GET", "/hub/runs");
    perch.assert_not_built("POST", "/hub/runs");
    perch.assert_not_built("GET", "/hub/runs/health");
}

#[cfg(feature = "test-bins")]
#[test]
fn the_fakes_workflow_command_refuses_until_r1a() {
    let out = std::process::Command::new(fake_eidolon().expect("the stand-in door")).args(["--config", "/nonexistent", "workflow", "run", "p", "g"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("`workflow` is not built yet"), "{out:?}");
    assert!(out.stdout.is_empty(), "a refusal prints nothing on stdout: {out:?}");
}
