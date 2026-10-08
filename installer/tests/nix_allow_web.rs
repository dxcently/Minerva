//! `nix/minerva.sh keys allow-web FP` (arm-5): under Nix the page is not let change the update keys, so the verb says so and arms
//! nothing. The script runs as a copy with its build-time placeholders filled in, under the fixture's temp HOME and runtime dir, with
//! the curl shim first on PATH: nothing may reach curl, and no runtime file may be made.
#![cfg(unix)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use common::{Env, repo_root, text};

/// nix/minerva.sh with the placeholders the Nix build fills in replaced by harmless paths (the verbs under test never reach them).
fn copy_of_script(env: &Env) -> PathBuf {
    let mut src = fs::read_to_string(repo_root().join("nix/minerva.sh")).unwrap().replace("\r\n", "\n");
    for name in ["browser", "core", "eidolon", "perch", "plugins", "setup", "version", "webui"] {
        src = src.replace(&format!("@{name}@"), "/nonexistent");
    }
    assert!(!src.contains("@perch@"), "a placeholder is left in the copy");
    let path = env.dir.path().join("minerva.sh");
    fs::write(&path, src).unwrap();
    path
}

/// `bash <script> <args>` in the fixture's environment: (exit code, stdout, stderr).
fn nix(env: &Env, script: &Path, args: &[&str]) -> (i32, String, String) {
    let mut c = env.cmd("bash");
    c.arg(script).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let out = c.output().unwrap();
    let (o, e) = text(&out);
    (out.status.code().unwrap_or(-1), o, e)
}

/// The names in the fixture's runtime dir (its own decoy tokens and nothing else the script made).
fn runtime_files(env: &Env) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(env.run.join("minerva")).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

#[test]
fn keys_allow_web_under_nix_says_so_and_arms_nothing() {
    let env = Env::new();
    let script = copy_of_script(&env);
    let before = runtime_files(&env);
    for args in [
        vec!["keys", "allow-web", "0F7A3A5C447EC7EF"],
        vec!["keys", "allow-web", "0F7A 3A5C 447E C7EF"],
        vec!["keys", "allow-web", "0F7A3A5C447EC7EF", "--port", "48110", "--token-file", "/nonexistent/token"],
    ] {
        let (code, out, err) = nix(&env, &script, &args);
        assert_eq!((code, out.as_str()), (1, ""), "{args:?}: {err}");
        assert!(err.contains("managed by Nix: update through nix") && err.contains("nothing was armed"), "{args:?}: {err}");
    }
    assert_eq!(env.argv_log(), "", "no request reached curl, so the perch was never asked to arm");
    assert_eq!(runtime_files(&env), before, "no header file, pid or log was made in the runtime dir");
}

#[test]
fn keys_verbs_other_than_the_arm_are_not_run_here() {
    let env = Env::new();
    let script = copy_of_script(&env);
    let (code, out, err) = nix(&env, &script, &["keys", "list"]);
    assert_eq!((code, out.as_str()), (2, ""));
    assert!(err.contains("only 'keys allow-web FP' is available here"), "{err}");
    assert_eq!(env.argv_log(), "");
}
