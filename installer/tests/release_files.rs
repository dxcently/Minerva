//! `installer/release-files.sh` checks DIR/keys.json before it writes anything: a keys.json that revokes an id with no revoke_fp entry for
//! it, names a fingerprint that is not 16 hex digits, or has a revoke or revoke_fp list the script cannot read is refused (exit 1, one
//! sentence, nothing written), in both modes. The script runs under bash on temp release dirs, in the fixture's clean environment.
#![cfg(unix)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{Env, repo_root, text};

const ID: &str = "0F7A3A5C447EC7EF";
const FP: &str = "5a1c9e0b3f7d2d48";
const BUNDLE: &str = "minerva-1.2.3-linux-x86_64.tar.gz";

/// `bash release-files.sh <args>` in the fixture's environment: (exit code, stdout, stderr).
fn run(env: &Env, args: &[&str]) -> (i32, String, String) {
    let mut c = env.cmd("bash");
    c.arg(repo_root().join("installer/release-files.sh")).args(args);
    let out = c.output().unwrap();
    let (o, e) = text(&out);
    (out.status.code().unwrap_or(-1), o, e)
}

fn p(dir: &Path) -> &str {
    dir.to_str().unwrap()
}

/// A release dir under the fixture: its one bundle, and the keys.json given (no keys.json when None).
fn release(env: &Env, name: &str, keys: Option<&str>) -> PathBuf {
    let dir = env.dir.path().join(name);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(BUNDLE), "bundle bytes").unwrap();
    if let Some(k) = keys {
        fs::write(dir.join("keys.json"), k).unwrap();
    }
    dir
}

/// Every file in a dir with its bytes: a refusal must leave the dir exactly as it was.
fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut v: Vec<(String, Vec<u8>)> =
        fs::read_dir(dir).unwrap().flatten().map(|e| (e.file_name().to_string_lossy().into_owned(), fs::read(e.path()).unwrap())).collect();
    v.sort();
    v
}

/// The script refused: exit 1, nothing on stdout, the sentence on stderr, and the dir as it was.
fn refused(env: &Env, args: &[&str], dir: &Path, before: &[(String, Vec<u8>)], sentence: &str) {
    let (code, out, err) = run(env, args);
    assert_eq!((code, out.as_str()), (1, ""), "{args:?}: {err}");
    assert!(err.contains(sentence) && err.contains("nothing was written"), "{args:?}: {err}");
    assert_eq!(snapshot(dir), before, "{args:?}: a refused keys.json writes nothing");
}

#[test]
fn a_revocation_with_its_fingerprint_passes_and_the_release_files_say_the_serial() {
    let env = Env::new();
    let keys = format!(r#"{{"schema":1,"serial":3,"expires":"2030-01-01","add":[],"revoke":["{ID}"],"revoke_fp":[{{"id":"{ID}","fp":"{FP}"}}]}}"#);
    let dir = release(&env, "good", Some(keys.as_str()));
    let (code, _, err) = run(&env, &[p(&dir)]);
    assert_eq!(code, 0, "{err}");
    assert!(dir.join("SHA256SUMS").exists() && dir.join("latest.json").exists());
    assert!(fs::read_to_string(dir.join("latest.json")).unwrap().contains(r#""keys_serial":3"#));
}

#[test]
fn a_revoked_id_without_its_fingerprint_is_refused_and_nothing_is_written() {
    let env = Env::new();
    let cases = [
        ("bare", format!(r#"{{"serial":3,"revoke":["{ID}"]}}"#)),
        ("empty_fp_list", format!(r#"{{"serial":3,"revoke":["{ID}"],"revoke_fp":[]}}"#)),
        ("fp_for_another_id", format!(r#"{{"serial":3,"revoke":["{ID}"],"revoke_fp":[{{"id":"1111111111111111","fp":"{FP}"}}]}}"#)),
    ];
    for (name, keys) in cases {
        let dir = release(&env, name, Some(keys.as_str()));
        let before = snapshot(&dir);
        refused(&env, &[p(&dir)], &dir, &before, &format!("keys.json revokes {ID} without its fingerprint"));
    }
}

#[test]
fn ids_are_compared_without_case_and_the_json_may_span_lines() {
    let env = Env::new();
    let lower = ID.to_ascii_lowercase();
    let mixed = format!(r#"{{"serial":3,"revoke":["{lower}"],"revoke_fp":[{{"id":"{ID}","fp":"{}"}}]}}"#, FP.to_ascii_uppercase());
    let dir = release(&env, "mixed", Some(mixed.as_str()));
    let (code, _, err) = run(&env, &[p(&dir)]);
    assert_eq!(code, 0, "{err}");
    // pretty-printed, CRLF, tabs, and a fingerprint with spaces (the form `minerva keys inspect` prints)
    let spread = format!(
        "{{\r\n\t\"schema\": 1,\r\n  \"serial\": 3,\n  \"revoke\": [\n    \"{ID}\"\n  ],\n  \"revoke_fp\": [\n    {{\n      \"id\": \"{lower}\",\n      \"fp\": \"5A1C 9E0B 3F7D 2D48\"\n    }}\n  ]\n}}\n"
    );
    let dir = release(&env, "spread", Some(spread.as_str()));
    let (code, _, err) = run(&env, &[p(&dir)]);
    assert_eq!(code, 0, "{err}");
    let spread_bare = format!("{{\n  \"serial\": 3,\n  \"revoke\": [\n    \"{ID}\"\n  ]\n}}\n");
    let dir = release(&env, "spread_bare", Some(spread_bare.as_str()));
    let before = snapshot(&dir);
    refused(&env, &[p(&dir)], &dir, &before, &format!("keys.json revokes {ID} without its fingerprint"));
}

#[test]
fn a_list_the_script_cannot_read_is_refused_and_nothing_is_written() {
    let env = Env::new();
    let fp_ok = format!(r#"{{"id":"{ID}","fp":"{FP}"}}"#);
    let cases: [(&str, String, &str); 10] = [
        ("not_an_object", "\"serial\": 3\n".to_string(), "keys.json is not one JSON object"),
        ("revoke_a_string", format!(r#"{{"serial":3,"revoke":"{ID}"}}"#), "the revoke list in keys.json cannot be read"),
        ("revoke_unclosed", format!(r#"{{"serial":3,"revoke":["{ID}"}}"#), "the revoke list in keys.json cannot be read"),
        ("revoke_unquoted", format!(r#"{{"serial":3,"revoke":[{ID}]}}"#), "the revoke list in keys.json cannot be read"),
        ("revoke_short_id", format!(r#"{{"serial":3,"revoke":["{}"]}}"#, &ID[..15]), "the revoke list in keys.json cannot be read"),
        ("revoke_no_comma", format!(r#"{{"serial":3,"revoke":["{ID}" "1111111111111111"]}}"#), "the revoke list in keys.json cannot be read"),
        ("revoke_twice", format!(r#"{{"serial":3,"revoke":[],"revoke":["{ID}"],"revoke_fp":[{fp_ok}]}}"#), "keys.json names revoke more than once"),
        ("fp_without_fp", format!(r#"{{"serial":3,"revoke_fp":[{{"id":"{ID}"}}]}}"#), "the revoke_fp list in keys.json cannot be read"),
        ("fp_short", format!(r#"{{"serial":3,"revoke_fp":[{{"id":"{ID}","fp":"5a1c9e0b"}}]}}"#), "the revoke_fp list in keys.json cannot be read"),
        ("fp_a_string", format!(r#"{{"serial":3,"revoke_fp":["{ID}:{FP}"]}}"#), "the revoke_fp list in keys.json cannot be read"),
    ];
    for (name, keys, sentence) in cases {
        let dir = release(&env, name, Some(keys.as_str()));
        let before = snapshot(&dir);
        refused(&env, &[p(&dir)], &dir, &before, sentence);
    }
}

#[test]
fn no_keys_json_passes_as_it_always_did() {
    let env = Env::new();
    let dir = release(&env, "none", None);
    let (code, _, err) = run(&env, &[p(&dir)]);
    assert_eq!(code, 0, "{err}");
    assert!(fs::read_to_string(dir.join("latest.json")).unwrap().contains(r#""keys_serial":0"#));
}

#[test]
fn a_keys_json_with_neither_list_passes() {
    // no revoke and no revoke_fp: nothing to check, so the release carries the serial as it always did
    let env = Env::new();
    let keys = r#"{"schema":1,"serial":3,"expires":"2030-01-01","add":[]}"#;
    let dir = release(&env, "neither", Some(keys));
    let (code, _, err) = run(&env, &[p(&dir)]);
    assert_eq!(code, 0, "{err}");
    assert!(fs::read_to_string(dir.join("latest.json")).unwrap().contains(r#""keys_serial":3"#));
}

#[test]
fn resign_runs_the_same_check_and_writes_nothing_on_a_refusal() {
    let env = Env::new();
    let good = format!(r#"{{"serial":3,"revoke":["{ID}"],"revoke_fp":[{{"id":"{ID}","fp":"{FP}"}}]}}"#);
    let dir = release(&env, "resign", Some(good.as_str()));
    let (code, _, err) = run(&env, &[p(&dir)]);
    assert_eq!(code, 0, "{err}");
    let (code, _, err) = run(&env, &["resign", p(&dir)]);
    assert_eq!(code, 0, "{err}");
    assert!(fs::read_to_string(dir.join("latest.json")).unwrap().contains(r#""keys_serial":3"#));
    // the same release with a revocation that names no fingerprint: refused in resign mode too, and latest.json is left alone
    fs::write(dir.join("keys.json"), format!(r#"{{"serial":4,"revoke":["{ID}"]}}"#)).unwrap();
    let before = snapshot(&dir);
    refused(&env, &["resign", p(&dir)], &dir, &before, &format!("keys.json revokes {ID} without its fingerprint"));
}
