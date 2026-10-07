//! The `minerva` command as the perch and `hoot` call it: `status --json`, `keys add|remove --json`, and the checks of
//! `web --keys-arm` that need no perch. Every run has a cleared environment and a temp HOME; nothing reads the operator's files.
#![cfg(unix)]

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use common::{Env, text};
use serde_json::{Value, json};

const MINERVA: &str = env!("CARGO_BIN_EXE_minerva");

fn run(env: &Env, args: &[&str], stdin: Option<&str>) -> (i32, String, String) {
    let mut c = env.cmd(MINERVA);
    c.args(args).stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = c.spawn().unwrap();
    if let Some(s) = stdin {
        child.stdin.take().unwrap().write_all(s.as_bytes()).unwrap();
    }
    let out = child.wait_with_output().unwrap();
    let (o, e) = text(&out);
    (out.status.code().unwrap_or(-1), o, e)
}

fn json_of(stdout: &str) -> Value {
    assert_eq!(stdout.trim_end().lines().count(), 1, "one line of JSON, nothing else: {stdout:?}");
    serde_json::from_str(stdout).unwrap_or_else(|e| panic!("not JSON ({e}): {stdout:?}"))
}

// ---- status --json ----------------------------------------------------------------------------------------------------

struct Prefix {
    prefix: PathBuf,
}

impl Prefix {
    fn new(env: &Env) -> Prefix {
        Prefix { prefix: env.dir.path().join("prefix") }
    }
    fn state(&self) -> PathBuf {
        self.prefix.join("state/minerva")
    }
    fn put(&self, name: &str, text: &str) {
        fs::create_dir_all(self.state()).unwrap();
        fs::write(self.state().join(name), text).unwrap();
    }
    fn arg(&self) -> String {
        self.prefix.display().to_string()
    }
}

fn status(env: &Env, p: &Prefix, extra: &[&str]) -> Value {
    let arg = p.arg();
    let mut args = vec!["status", "--json", "--prefix", arg.as_str()];
    args.extend_from_slice(extra);
    let (code, out, err) = run(env, &args, None);
    assert_eq!((code, err.as_str()), (0, ""), "{out}");
    json_of(&out)
}

#[test]
fn status_json_of_a_bare_prefix_is_all_absent() {
    let env = Env::new();
    let p = Prefix::new(&env);
    let v = status(&env, &p, &["--as", "hoot"]);
    // the version is the program's own when nothing is installed under the prefix
    let (_, own, _) = run(&env, &["version"], None);
    assert_eq!(v, json!({ "version": own.trim(), "notice": null, "plugins": [], "jev": { "set_up": false, "running": false } }));
    assert!(!p.prefix.exists(), "asking creates nothing");
}

#[test]
fn status_json_reads_the_cache_the_record_and_the_service() {
    let env = Env::new();
    let p = Prefix::new(&env);
    // an installed layout: `current` points at a complete version dir
    let ver = p.prefix.join("opt/minerva/0.1.0-gabc1234");
    fs::create_dir_all(&ver).unwrap();
    fs::write(ver.join(".minerva-complete"), "").unwrap();
    std::os::unix::fs::symlink("0.1.0-gabc1234", p.prefix.join("opt/minerva/current")).unwrap();
    p.put("update-check", r#"{"checked":1791269513,"latest":"0.2.0"}"#);
    p.put("plugins.json", r#"{"stamp":"0.1.0","plugins":{"jev":{"version":"0.4.0","hash":"h","how":"stock"},"browser":{"version":"0.3.0","hash":"h","how":"copy"}}}"#);
    let venv = p.prefix.join("opt/minerva/jev-venv");
    fs::create_dir_all(&venv).unwrap();
    fs::write(venv.join(".minerva-jev"), "requirements.pinned.txt\n").unwrap();
    // `; :` keeps the shell from exec-ing the sleep (which would drop the name); and the kernel fills in the command line a moment
    // after the exec, so wait until it shows
    let mut svc: Child = Command::new("sh").args(["-c", "sleep 20; :", "server.py"]).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    for _ in 0..500 {
        if fs::read(format!("/proc/{}/cmdline", svc.id())).map(|c| String::from_utf8_lossy(&c).contains("server.py")).unwrap_or(false) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    p.put("jev.pid", &svc.id().to_string());

    let v = status(&env, &p, &["--as", "hoot"]);
    svc.kill().unwrap();
    svc.wait().unwrap();
    assert_eq!(
        v,
        json!({
            "version": "0.1.0-gabc1234",
            "notice": { "line": "Minerva 0.2.0 available -- run: hoot update", "latest": "0.2.0", "stale": null, "checked": 1791269513u64 },
            "plugins": [ { "name": "browser", "version": "0.3.0", "how": "copy" }, { "name": "jev", "version": "0.4.0", "how": "stock" } ],
            "jev": { "set_up": true, "running": true, "pid": svc.id() },
        })
    );
    // the same cache, worded for a Linux terminal
    assert_eq!(status(&env, &p, &["--as", "minerva"])["notice"]["line"], "Minerva 0.2.0 available -- run: minerva update");
}

#[test]
fn status_json_with_a_corrupt_cache_has_a_null_notice_and_changes_nothing() {
    let env = Env::new();
    let p = Prefix::new(&env);
    p.put("update-check", "{ torn");
    let before = fs::read(p.state().join("update-check")).unwrap();
    let v = status(&env, &p, &["--as", "minerva"]);
    assert_eq!(v["notice"], Value::Null);
    assert_eq!(fs::read(p.state().join("update-check")).unwrap(), before);
    assert_eq!(fs::read_dir(p.state()).unwrap().count(), 1);
}

#[test]
fn status_json_never_asks_the_network() {
    // A day-old cache is what makes the daily notice fetch (and rewrite the file). Status must leave it exactly as it is.
    let env = Env::new();
    let p = Prefix::new(&env);
    p.put("update-check", r#"{"checked":1,"latest":"0.2.0"}"#);
    let started = std::time::Instant::now();
    let v = status(&env, &p, &["--as", "hoot"]);
    assert!(started.elapsed().as_secs() < 3);
    assert_eq!(v["notice"]["checked"], 1, "still the old attempt: nothing refreshed it");
    assert_eq!(fs::read_to_string(p.state().join("update-check")).unwrap(), r#"{"checked":1,"latest":"0.2.0"}"#);
}

#[test]
fn status_json_default_command_follows_wsl_and_other_arguments_are_refused() {
    let env = Env::new();
    let p = Prefix::new(&env);
    p.put("update-check", r#"{"checked":5,"latest":"9.9.9"}"#);
    let wsl = fs::read_to_string("/proc/version").map(|s| s.to_ascii_lowercase().contains("microsoft")).unwrap_or(false);
    let v = status(&env, &p, &[]);
    assert_eq!(v["notice"]["line"], format!("Minerva 9.9.9 available -- run: {} update", if wsl { "hoot" } else { "minerva" }));
    let arg = p.arg();
    for bad in [vec!["--as", "rm -rf /"], vec!["--as", "HOOT"], vec!["--nope"], vec!["stray"]] {
        let mut args = vec!["status", "--json", "--prefix", arg.as_str()];
        args.extend(bad.iter().copied());
        let (code, out, err) = run(&env, &args, None);
        assert_eq!((code, out.as_str()), (1, ""), "{bad:?}");
        assert!(err.starts_with("minerva: "), "{err}");
    }
}

#[test]
fn status_json_does_not_block_on_files_that_are_not_files() {
    // A FIFO where a state file or VERSION belongs would hang an open() until someone wrote to it. Status reads each as absent.
    let env = Env::new();
    let (_, own, _) = run(&env, &["version"], None);
    let inst = env.dir.path().join("inst");
    fs::create_dir_all(inst.join("bin")).unwrap();
    fs::copy(MINERVA, inst.join("bin/minerva")).unwrap();
    fs::set_permissions(inst.join("bin/minerva"), fs::Permissions::from_mode(0o755)).unwrap();
    let p = Prefix::new(&env);
    fs::create_dir_all(p.state()).unwrap();
    for f in [inst.join("VERSION"), p.state().join("update-check"), p.state().join("plugins.json"), p.state().join("jev.pid")] {
        assert!(Command::new("mkfifo").arg(&f).status().unwrap().success());
    }
    let mut child = env.cmd(inst.join("bin/minerva")).args(["status", "--json", "--as", "hoot", "--prefix"]).arg(&p.prefix).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let started = std::time::Instant::now();
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() > std::time::Duration::from_secs(5) {
            child.kill().unwrap();
            panic!("status --json blocked on a FIFO");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let out = child.wait_with_output().unwrap();
    let (o, e) = text(&out);
    assert_eq!((out.status.code(), e.as_str()), (Some(0), ""));
    assert_eq!(json_of(&o), json!({ "version": own.trim(), "notice": null, "plugins": [], "jev": { "set_up": false, "running": false } }));
}

#[test]
fn plain_status_still_goes_to_the_perch_script_not_to_json() {
    // `minerva status` (no --json) is the server status verb: it needs the perch-open.sh beside the program, which a test build does not
    // have, and says so; it must not print the JSON.
    let env = Env::new();
    let (code, out, err) = run(&env, &["status"], None);
    assert_ne!(code, 0);
    assert!(!out.contains("\"version\""), "{out}");
    assert!(err.contains("perch-open.sh") || err.contains("minerva:"), "{err}");
}

// ---- keys add|remove --json ---------------------------------------------------------------------------------------------

fn keypair() -> String {
    minisign::KeyPair::generate_unencrypted_keypair().unwrap().pk.to_box().unwrap().to_string()
}

/// (id, fingerprint) of a public key file, as `keys inspect --json` says them.
fn inspect(env: &Env, pubtext: &str) -> (String, String) {
    let (code, out, _) = run(env, &["keys", "inspect", "-", "--json"], Some(pubtext));
    assert_eq!(code, 0);
    let v = json_of(&out);
    (v["id"].as_str().unwrap().to_string(), v["fingerprint"].as_str().unwrap().to_string())
}

fn keys_dir(env: &Env) -> PathBuf {
    env.home.join(".config/minerva/update-keys")
}

/// Every file under `dir` with its bytes and mode (or None when there is no dir): "unchanged" means this is equal.
fn snapshot(dir: &Path) -> Option<BTreeMap<String, (Vec<u8>, u32)>> {
    if !dir.exists() {
        return None;
    }
    let mut m = BTreeMap::new();
    for e in fs::read_dir(dir).unwrap().flatten() {
        let md = e.metadata().unwrap();
        m.insert(e.file_name().to_string_lossy().into_owned(), (fs::read(e.path()).unwrap_or_default(), md.permissions().mode() & 0o7777));
    }
    m.insert(".".into(), (Vec::new(), fs::metadata(dir).unwrap().permissions().mode() & 0o7777));
    Some(m)
}

fn error_of(out: &str) -> (String, String) {
    let v = json_of(out);
    let e = &v["error"];
    assert_eq!(v.as_object().unwrap().len(), 1, "{v}");
    (e["kind"].as_str().unwrap().to_string(), e["sentence"].as_str().unwrap().to_string())
}

#[test]
fn keys_add_and_remove_json_succeed_with_the_id() {
    let env = Env::new();
    let pubtext = keypair();
    let (id, fp) = inspect(&env, &pubtext);
    // spaces and case do not matter in the confirmation
    let typed = fp.to_ascii_uppercase();
    let (code, out, err) = run(&env, &["keys", "add", "-", "--confirm", &typed, "--json"], Some(&pubtext));
    assert_eq!((code, err.as_str()), (0, ""), "no banner on stderr in --json mode");
    assert_eq!(json_of(&out), json!({ "added": id }));
    let dir = keys_dir(&env);
    assert_eq!(fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
    assert_eq!(fs::metadata(dir.join(format!("{id}.pub"))).unwrap().permissions().mode() & 0o777, 0o600);
    let (_, list, _) = run(&env, &["keys", "list", "--json"], None);
    let rows = json_of(&list);
    assert!(rows.as_array().unwrap().iter().any(|r| r["id"] == id.as_str() && r["source"] == "added by hand" && r["state"] == "trusted"), "{rows}");
    // remove it again
    let (code, out, err) = run(&env, &["keys", "remove", &id, "--confirm", &fp, "--json"], None);
    assert_eq!((code, err.as_str()), (0, ""));
    assert_eq!(json_of(&out), json!({ "removed": id }));
    assert!(!dir.join(format!("{id}.pub")).exists());
}

#[test]
fn a_wrong_confirmation_is_a_mismatch_and_the_keys_dir_is_untouched() {
    let env = Env::new();
    let (first, other) = (keypair(), keypair());
    let (first_id, first_fp) = inspect(&env, &first);
    let (other_id, other_fp) = inspect(&env, &other);
    // no keys dir at all: still none afterwards
    let none = snapshot(&keys_dir(&env));
    let (code, out, err) = run(&env, &["keys", "add", "-", "--confirm", &other_fp, "--json"], Some(&first));
    assert_eq!((code, err.as_str()), (1, ""));
    let (kind, sentence) = error_of(&out);
    assert_eq!(kind, "mismatch");
    assert!(sentence.contains("nothing changed"), "{sentence}");
    assert_eq!(snapshot(&keys_dir(&env)), none);
    assert!(!keys_dir(&env).exists());
    // with a keys dir holding a key: byte for byte the same after a refused add and a refused remove
    let (code, _, _) = run(&env, &["keys", "add", "-", "--confirm", &first_fp, "--json"], Some(&first));
    assert_eq!(code, 0);
    let before = snapshot(&keys_dir(&env));
    let (code, out, _) = run(&env, &["keys", "add", "-", "--confirm", &first_fp, "--json"], Some(&other));
    assert_eq!((code, error_of(&out).0.as_str()), (1, "mismatch"), "the confirmation is for another key");
    let (code, out, _) = run(&env, &["keys", "remove", &first_id, "--confirm", &other_fp, "--json"], None);
    assert_eq!((code, error_of(&out).0.as_str()), (1, "mismatch"));
    assert_eq!(snapshot(&keys_dir(&env)), before);
    assert!(keys_dir(&env).join(format!("{first_id}.pub")).exists());
    assert!(!keys_dir(&env).join(format!("{other_id}.pub")).exists());
    // not even an id is a confirmation
    let (code, out, _) = run(&env, &["keys", "add", "-", "--confirm", &other_id, "--json"], Some(&other));
    assert_eq!((code, error_of(&out).0.as_str()), (1, "mismatch"));
    assert_eq!(snapshot(&keys_dir(&env)), before);
}

#[test]
fn a_revoked_key_is_refused_as_revoked() {
    let env = Env::new();
    let (anchor, victim) = (keypair(), keypair());
    let (anchor_id, anchor_fp) = inspect(&env, &anchor);
    let (victim_id, victim_fp) = inspect(&env, &victim);
    let (code, _, _) = run(&env, &["keys", "add", "-", "--confirm", &anchor_fp, "--json"], Some(&anchor));
    assert_eq!(code, 0);
    fs::write(
        keys_dir(&env).join("state.json"),
        format!(r#"{{"serial":1,"revoked":[{{"id":"{victim_id}","on":"2026-01-01","by":"{anchor_id}"}}]}}"#),
    )
    .unwrap();
    let before = snapshot(&keys_dir(&env));
    let (code, out, err) = run(&env, &["keys", "add", "-", "--confirm", &victim_fp, "--json"], Some(&victim));
    assert_eq!((code, err.as_str()), (1, ""));
    let (kind, sentence) = error_of(&out);
    assert_eq!(kind, "revoked");
    assert!(sentence.contains("never accepted again"), "{sentence}");
    assert_eq!(snapshot(&keys_dir(&env)), before);
}

#[test]
fn a_forged_key_with_a_known_id_is_a_collision() {
    use base64::Engine;
    let env = Env::new();
    let (real, other) = (keypair(), keypair());
    let (id, fp) = inspect(&env, &real);
    let (code, _, _) = run(&env, &["keys", "add", "-", "--confirm", &fp, "--json"], Some(&real));
    assert_eq!(code, 0);
    // the real key's id (bytes 2..10) with the other key's key bytes
    let b64 = |t: &str| t.lines().nth(1).unwrap().trim().to_string();
    let engine = base64::engine::general_purpose::STANDARD;
    let (r, o) = (engine.decode(b64(&real)).unwrap(), engine.decode(b64(&other)).unwrap());
    let mut forged = r[..10].to_vec();
    forged.extend_from_slice(&o[10..]);
    let forged = format!("untrusted comment: forged\n{}\n", engine.encode(forged));
    let (fid, ffp) = inspect(&env, &forged);
    assert_eq!(fid, id);
    assert_ne!(ffp, fp);
    let before = snapshot(&keys_dir(&env));
    let (code, out, _) = run(&env, &["keys", "add", "-", "--confirm", &ffp, "--json"], Some(&forged));
    let (kind, sentence) = error_of(&out);
    assert_eq!((code, kind.as_str()), (1, "collision"));
    assert!(sentence.contains("DIFFERENT key"), "{sentence}");
    assert_eq!(snapshot(&keys_dir(&env)), before);
}

#[test]
fn an_open_keys_dir_or_a_lost_record_is_unsafe_not_not_found() {
    let env = Env::new();
    let (a, b) = (keypair(), keypair());
    let (a_id, a_fp) = inspect(&env, &a);
    let (_, b_fp) = inspect(&env, &b);
    let (code, _, _) = run(&env, &["keys", "add", "-", "--confirm", &a_fp, "--json"], Some(&a));
    assert_eq!(code, 0);
    let dir = keys_dir(&env);
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    let before = snapshot(&dir);
    let (code, out, _) = run(&env, &["keys", "add", "-", "--confirm", &b_fp, "--json"], Some(&b));
    let (kind, sentence) = error_of(&out);
    assert_eq!((code, kind.as_str()), (1, "unsafe"));
    assert!(sentence.contains("not a private directory"), "{sentence}");
    // remove must not claim the key does not exist while its directory is being ignored
    let (code, out, _) = run(&env, &["keys", "remove", &a_id, "--confirm", &a_fp, "--json"], None);
    assert_eq!((code, error_of(&out).0.as_str()), (1, "unsafe"));
    assert_eq!(snapshot(&dir), before);
    // a lost record
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
    fs::remove_file(dir.join("state.json")).unwrap();
    let before = snapshot(&dir);
    let (code, out, _) = run(&env, &["keys", "add", "-", "--confirm", &b_fp, "--json"], Some(&b));
    assert_eq!((code, error_of(&out).0.as_str()), (1, "unsafe"));
    assert_eq!(snapshot(&dir), before);
}

#[test]
fn removing_what_is_not_there_is_not_found() {
    let env = Env::new();
    let (code, out, err) = run(&env, &["keys", "remove", "0123456789ABCDEF", "--confirm", "0000 0000 0000 0000", "--json"], None);
    assert_eq!((code, err.as_str()), (1, ""));
    let (kind, sentence) = error_of(&out);
    assert_eq!(kind, "not-found");
    assert!(sentence.contains("0123456789ABCDEF"), "{sentence}");
    assert!(!keys_dir(&env).exists());
}

#[test]
fn what_is_not_one_of_the_named_kinds_is_failed_and_the_exit_code_is_the_same() {
    let env = Env::new();
    let (code, out, err) = run(&env, &["keys", "add", "-", "--confirm", "0000000000000000", "--json"], Some("this is not a key"));
    assert_eq!((code, err.as_str()), (1, ""));
    assert_eq!(error_of(&out).0, "failed");
    let (code, out, _) = run(&env, &["keys", "add", "--json"], None);
    assert_eq!((code, error_of(&out).0.as_str()), (1, "failed"), "a usage error still answers in JSON");
    assert!(!keys_dir(&env).exists());
}

#[test]
fn every_failure_of_add_and_remove_answers_in_json_even_before_the_key_is_read() {
    let env = Env::new();
    // --confirm without its value, and no keys directory to be had: both used to be a plain stderr line and an empty stdout
    for args in [vec!["keys", "add", "-", "--json", "--confirm"], vec!["keys", "remove", "0123456789ABCDEF", "--json", "--confirm"]] {
        let (code, out, err) = run(&env, &args, None);
        assert_eq!((code, err.as_str()), (1, ""), "{args:?}");
        let (kind, sentence) = error_of(&out);
        assert_eq!(kind, "failed", "{args:?}");
        assert!(sentence.contains("--confirm needs a value"), "{sentence}");
    }
    let mut c = env.cmd(MINERVA);
    c.env_remove("HOME").env_remove("XDG_CONFIG_HOME").args(["keys", "add", "-", "--json", "--confirm", "0000000000000000"]).stdin(Stdio::null());
    let out = c.output().unwrap();
    let (o, e) = text(&out);
    assert_eq!((out.status.code(), e.as_str()), (Some(1), ""), "{o}");
    let (kind, sentence) = error_of(&o);
    assert_eq!(kind, "failed");
    assert!(sentence.contains("HOME is not set"), "{sentence}");
    // without --json the same failures are the plain stderr line they always were
    let (code, out, err) = run(&env, &["keys", "add", "-", "--confirm"], None);
    assert_eq!((code, out.as_str()), (1, ""));
    assert!(err.starts_with("minerva: ") && err.contains("--confirm needs a value"), "{err}");
}

#[test]
fn removing_a_key_by_something_that_is_not_an_id_is_not_found_and_is_not_repeated() {
    let env = Env::new();
    for bad in ["../../etc/passwd", "\u{1b}[2Jgone", "0123456789ABCDE", "0123456789ABCDEFF", "nothex-nothex-no", "\u{9b}0K"] {
        let (code, out, err) = run(&env, &["keys", "remove", bad, "--confirm", "0000 0000 0000 0000", "--json"], None);
        assert_eq!((code, err.as_str()), (1, ""), "{bad:?}");
        let (kind, sentence) = error_of(&out);
        assert_eq!(kind, "not-found", "{bad:?}");
        assert!(sentence.contains("16 hex digits") && !sentence.contains(bad), "{bad:?}: {sentence}");
        // the same words on stderr without --json, with nothing of the argument and no control character in them
        let (code, out, err) = run(&env, &["keys", "remove", bad, "--confirm", "0000 0000 0000 0000"], None);
        assert_eq!((code, out.as_str()), (1, ""), "{bad:?}");
        assert!(err.contains("16 hex digits") && !err.contains(bad), "{bad:?}: {err:?}");
        assert!(!err.chars().any(|c| c.is_control() && c != '\n'), "{bad:?}: {err:?}");
    }
    assert!(!keys_dir(&env).exists(), "nothing was looked up, let alone made");
    // a well-formed id that is simply not there still says which one
    let (_, out, _) = run(&env, &["keys", "remove", "0123456789abcdef", "--json"], None);
    assert!(error_of(&out).1.contains("0123456789abcdef"));
}

/// A public key file whose comment line carries terminal control characters: an escape, a bell, a C1 control and DEL.
fn key_with_a_nasty_comment() -> (String, &'static str) {
    let pubtext = keypair();
    let b64 = pubtext.lines().nth(1).unwrap().to_string();
    let comment = "evil\u{1b}[31mred\u{7}bell\u{9b}c1\u{7f}del";
    (format!("untrusted comment: {comment}\n{b64}\n"), comment)
}

#[test]
fn a_comment_is_printed_without_control_characters_in_text_and_escaped_in_json() {
    let env = Env::new();
    let (nasty, comment) = key_with_a_nasty_comment();
    let plain = "evil[31mredbellc1del";
    let clean = |s: &str| !s.chars().any(|c| c.is_control() && c != '\n');
    // keys inspect
    let (code, out, err) = run(&env, &["keys", "inspect", "-"], Some(&nasty));
    assert_eq!((code, err.as_str()), (0, ""));
    assert!(clean(&out) && out.contains(&format!("comment      {plain}\n")), "{out:?}");
    // keys add: the banner on stderr, shown before the confirmation that is refused here
    let (code, _, err) = run(&env, &["keys", "add", "-", "--confirm", "0000 0000 0000 0000"], Some(&nasty));
    assert_eq!(code, 1);
    assert!(clean(&err) && err.contains(&format!("comment      {plain}\n")), "{err:?}");
    // JSON keeps the text, as JSON escapes it (the C0 controls are escapes, so no raw escape or bell reaches a terminal)
    let (_, out, _) = run(&env, &["keys", "inspect", "-", "--json"], Some(&nasty));
    assert!(!out.contains('\u{1b}') && !out.contains('\u{7}'), "{out:?}");
    assert_eq!(json_of(&out)["comment"], comment);
}

#[test]
fn without_json_nothing_about_add_and_remove_changed() {
    let env = Env::new();
    let pubtext = keypair();
    let (id, fp) = inspect(&env, &pubtext);
    let (code, out, err) = run(&env, &["keys", "add", "-", "--confirm", "ffff ffff ffff ffff"], Some(&pubtext));
    assert_eq!((code, out.as_str()), (1, ""));
    assert!(err.contains("minerva: the confirmation does not match the key's fingerprint; nothing changed"), "{err}");
    let (code, out, err) = run(&env, &["keys", "add", "-", "--confirm", &fp], Some(&pubtext));
    assert_eq!((code, out.trim()), (0, format!("added {id}").as_str()));
    assert!(err.contains("fingerprint"), "the human banner is still on stderr");
    let (code, out, _) = run(&env, &["keys", "remove", &id, "--confirm", &fp], None);
    assert_eq!((code, out.trim()), (0, format!("removed {id}").as_str()));
}

// ---- web --keys-arm: the checks that need no perch ------------------------------------------------------------------------

#[test]
fn keys_arm_refuses_a_fingerprint_that_is_not_sixteen_hex_digits_before_running_anything() {
    let env = Env::new();
    for bad in ["", "0F7A", "0F7A 3A5C", "0F7A3A5C447EC7E", "0F7A3A5C447EC7EFF", "0F7A3A5C447EC7EG", "../../etc", "0F7A3A5C447EC7EF; id"] {
        let (code, out, err) = run(&env, &["web", "--keys-arm", bad], None);
        assert_eq!((code, out.as_str()), (1, ""), "{bad:?}");
        assert!(err.contains("16 hex digits"), "{bad:?}: {err}");
    }
    assert_eq!(env.argv_log(), "", "no curl was reached");
    // anything else beside it is refused too: arming does nothing but arm
    let (code, _, err) = run(&env, &["web", "--keys-arm", "0F7A3A5C447EC7EF", "--no-open"], None);
    assert_eq!(code, 1);
    assert!(err.contains("unexpected argument"), "{err}");
    let (code, _, err) = run(&env, &["web", "--keys-arm"], None);
    assert_eq!(code, 1);
    assert!(err.contains("needs a value"), "{err}");
}
