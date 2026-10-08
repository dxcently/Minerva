//! What a door, and every other eidolon a perch starts, is told about its config (slice SA2): `EIDOLON_CONFIG` names the file the argv
//! passes with `--config`, and is removed from the environment when the argv passes none, so an `EIDOLON_CONFIG` the operator's shell
//! exported never reaches a door.
//!
//! The stand-in door records nothing about its environment, so these tests put a shell shim between the perch and it: the shim writes its
//! `EIDOLON_CONFIG` and its argv to `$SHIM_LOG`, then `exec`s the stand-in with the same argv, so the door is the process the perch forked.
//! No fake is changed. Built with `--features test-bins`, like the other tests that start a door.

#![cfg(feature = "test-bins")]

mod common;

use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use common::*;

/// Writes `eidolon` at `path`: a shell script that records each call's `EIDOLON_CONFIG` (or `<unset>`) and argv, then runs the stand-in
/// door with that argv. The stand-in writes a new chat's log under `$FAKE_SESSIONS_DIR`, which must be the sessions dir the perch reads,
/// so the script names it.
fn write_shim(path: &Path, sessions: &Path) {
    let fake = fake_eidolon().expect("the stand-in door, built with --features test-bins");
    let script = format!(
        "#!/bin/sh\n\
         printf 'env=%s argv=%s\\n' \"${{EIDOLON_CONFIG-<unset>}}\" \"$*\" >> \"$SHIM_LOG\"\n\
         FAKE_SESSIONS_DIR='{}'\n\
         export FAKE_SESSIONS_DIR\n\
         exec '{fake}' \"$@\"\n",
        sessions.display()
    );
    std::fs::write(path, script).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Every call the shim wrote, as `(EIDOLON_CONFIG, argv)`: `None` when the name was not in the environment at all.
fn calls(log: &Path) -> Vec<(Option<String>, String)> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(|line| {
            let rest = line.strip_prefix("env=").expect("a shim line");
            let (env, argv) = rest.split_once(" argv=").expect("a shim line");
            ((env != "<unset>").then(|| env.to_string()), argv.to_string())
        })
        .collect()
}

/// The doors among the calls: the ones whose argv starts a `web` server.
fn doors(seen: &[(Option<String>, String)]) -> Vec<(Option<String>, String)> {
    seen.iter().filter(|(_, argv)| argv.contains(" web --bind ")).cloned().collect()
}

/// Starts a new chat in `work` through the perch, and returns its id once its door is up.
fn start_chat(perch: &Perch, work: &Path) -> String {
    let answer = perch.post_json("/hub/sessions", serde_json::json!({ "cwd": work.display().to_string() }));
    assert_eq!(answer.status, 201, "{}{}", answer.head, answer.body);
    body_id(&answer)
}

/// A door a perch starts under `--eidolon-config` is told that config, and so is every other eidolon the perch starts under it.
#[test]
fn a_door_is_told_the_config_it_runs_under_and_so_is_every_other_call() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("root");
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let sessions = base.path().join("sessions");
    std::fs::create_dir(&sessions).unwrap();
    let config = base.path().join("minerva.toml");
    std::fs::write(&config, format!("sessions_dir = '''{}'''\n", sessions.display())).unwrap();
    let eidolon = base.path().join("eidolon");
    write_shim(&eidolon, &sessions);
    let log = base.path().join("config.log");

    let perch = Perch::builder()
        .arg("--root")
        .arg(&root)
        .arg("--eidolon")
        .arg(&eidolon)
        .arg("--eidolon-config")
        .arg(&config)
        .env("SHIM_LOG", log.display().to_string())
        .start();
    let id = start_chat(&perch, &work);
    // The listing asks eidolon for the titles of the saved chats (`eidolon --config <file> sessions`): a call that is not a door.
    assert_eq!(perch.get("/hub/sessions").status, 200);
    let seen = calls(&log);
    let want = config.display().to_string();

    let door = doors(&seen);
    assert_eq!(door.len(), 1, "one door, started once: {seen:?}");
    assert_eq!(door[0].0.as_deref(), Some(want.as_str()), "the door's environment names its config: {seen:?}");
    assert!(door[0].1.starts_with(&format!("--config {want} ")), "and its argv passes the same file: {seen:?}");
    let others = seen.iter().filter(|(_, argv)| argv.starts_with("--config ") && !argv.contains(" web --bind ")).count();
    assert!(others >= 1, "a non-door call (the titles listing) runs under the config too, so the check below is not empty: {seen:?}");
    for (env, argv) in &seen {
        if argv.starts_with("--config ") {
            assert_eq!(env.as_deref(), Some(want.as_str()), "a call under the config names it too: {argv}");
        }
    }
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
}

/// A door started with no config has the name removed, even though the perch itself was started with one exported in its environment.
#[test]
fn a_door_with_no_config_does_not_inherit_one_from_the_environment() {
    let base = tempfile::tempdir().unwrap();
    let root = base.path().join("root");
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let eidolon = base.path().join("eidolon");
    let log = base.path().join("no-config.log");

    let perch = Perch::builder()
        .arg("--root")
        .arg(&root)
        .arg("--eidolon")
        .arg(&eidolon)
        .env("SHIM_LOG", log.display().to_string())
        .env("EIDOLON_CONFIG", "/operator/shell.toml")
        .start();
    // No config means no start-up probe, so the script can follow the start: it names the perch's own sessions dir, which is a
    // harness tempdir that exists only once the perch is up.
    write_shim(&eidolon, perch.sessions.path());
    let id = start_chat(&perch, &work);
    let seen = calls(&log);

    let door = doors(&seen);
    assert_eq!(door.len(), 1, "one door, started once: {seen:?}");
    assert_eq!(door[0].0, None, "the door's environment has no EIDOLON_CONFIG: {seen:?}");
    assert!(!door[0].1.contains("--config"), "and its argv names no config either: {seen:?}");
    assert_eq!(perch.post(&format!("/hub/sessions/{id}/stop")).status, 202);
}
