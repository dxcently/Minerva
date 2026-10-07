use super::*;
use crate::keys::tests::trust_with;
use crate::notice;
use crate::verify::testkeys::{keypair, sign};
use std::process::{Child, Command, Stdio};

fn layout() -> (tempfile::TempDir, Layout) {
    let t = tempfile::tempdir().unwrap();
    let l = Layout::new(t.path().join("p"));
    fs::create_dir_all(l.state_dir()).unwrap();
    (t, l)
}

fn opts<'a>(l: &'a Layout, version: &'a str, who: &'a str) -> Opts<'a> {
    Opts { layout: l, version, who }
}

fn cache(l: &Layout, text: &str) {
    fs::write(l.state_dir().join("update-check"), text).unwrap();
}

fn set_up(l: &Layout) {
    fs::create_dir_all(jev::venv_dir(l)).unwrap();
    fs::write(jev::venv_dir(l).join(".minerva-jev"), "requirements.pinned.txt\n").unwrap();
}

/// A live process whose command line names `server.py`, the way the jev service's does. (`; :` keeps the shell from exec-ing the
/// sleep, which would drop the name from the command line.) Every caller kills and waits for it.
#[allow(clippy::zombie_processes)]
fn fake_service() -> Child {
    let child = Command::new("sh").args(["-c", "sleep 20; :", "server.py"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    // spawn returns as soon as the exec happens; the kernel fills in the command line a moment later
    for _ in 0..500 {
        if fs::read(format!("/proc/{}/cmdline", child.id())).map(|c| String::from_utf8_lossy(&c).contains("server.py")).unwrap_or(false) {
            return child;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    panic!("the fake service never showed its command line");
}

#[test]
fn a_bare_prefix_says_nothing_and_fails_nowhere() {
    let t = tempfile::tempdir().unwrap();
    let l = Layout::new(t.path().join("never-created"));
    let r = report(&opts(&l, "0.1.0-gx", "minerva"));
    assert_eq!(r, json!({ "version": "0.1.0-gx", "notice": null, "plugins": [], "jev": { "set_up": false, "running": false } }));
    assert!(!l.prefix.exists(), "reading creates nothing");
}

#[test]
fn the_notice_line_is_the_one_the_cli_prints() {
    let t = tempfile::tempdir().unwrap();
    let (pk, sk) = keypair();
    let (_k, trust) = trust_with(&pk);
    let rel = t.path().join("rel");
    fs::create_dir_all(&rel).unwrap();
    let m = format!(r#"{{"schema":1,"version":"0.2.0","full":"0.2.0-gx","bundle":{{"name":"b.tar.gz","sha256":"{}","size":1}}}}"#, "0".repeat(64));
    fs::write(rel.join("latest.json"), &m).unwrap();
    fs::write(rel.join("latest.json.minisig"), sign(&sk, m.as_bytes())).unwrap();
    let l = Layout::new(t.path().join("p"));
    let base = format!("file://{}", rel.display());
    for who in ["hoot", "minerva"] {
        let said = notice::check(&l, &base, &trust, "0.1.0-gx", who, 1_000_000).unwrap();
        let n = report(&opts(&l, "0.1.0-gx", who))["notice"].clone();
        assert_eq!(n["line"], json!(said), "status words the notice exactly as the command does");
        assert_eq!(n["latest"], "0.2.0");
        assert_eq!(n["stale"], Value::Null);
        assert_eq!(n["checked"], 1_000_000);
    }
}

#[test]
fn equal_or_older_gives_no_line_but_still_the_answer() {
    let (_t, l) = layout();
    cache(&l, r#"{"checked":77,"latest":"0.1.0"}"#);
    let n = report(&opts(&l, "0.1.0-gabc", "hoot"))["notice"].clone();
    assert_eq!(n, json!({ "line": null, "latest": "0.1.0", "stale": null, "checked": 77 }));
    // an offline machine writes the attempt without an answer
    cache(&l, r#"{"checked":78}"#);
    let n = report(&opts(&l, "0.1.0-gabc", "hoot"))["notice"].clone();
    assert_eq!(n, json!({ "line": null, "latest": null, "stale": null, "checked": 78 }));
}

#[test]
fn a_stale_warning_rides_with_or_without_a_newer_release() {
    let (_t, l) = layout();
    let w = "release info is stale (signed 2026-01-01) -- the maintainer should re-sign";
    cache(&l, &format!(r#"{{"checked":5,"latest":"0.2.0","stale":"{w}"}}"#));
    let n = report(&opts(&l, "0.1.0", "minerva"))["notice"].clone();
    assert_eq!(n["line"], format!("Minerva 0.2.0 available -- run: minerva update; {w}"));
    assert_eq!(n["stale"], w);
    cache(&l, &format!(r#"{{"checked":5,"latest":"0.1.0","stale":"{w}"}}"#));
    let n = report(&opts(&l, "0.1.0", "minerva"))["notice"].clone();
    assert_eq!(n["line"], w);
}

#[test]
fn a_cache_that_does_not_parse_is_null() {
    let (_t, l) = layout();
    for bad in ["", "{ not json", "[]", r#"{"latest":"0.2.0"}"#, r#"{"checked":"yesterday"}"#, r#"{"checked":-1}"#, r#"{"checked":1,"latest":7}"#] {
        cache(&l, bad);
        assert_eq!(report(&opts(&l, "0.1.0", "hoot"))["notice"], Value::Null, "{bad:?}");
    }
    // a file past the cap is not one this program wrote: whether it is cut mid-value, or is whole JSON followed by padding
    cache(&l, &format!(r#"{{"checked":1,"latest":"0.2.0","stale":"{}"}}"#, "x".repeat(CACHE_CAP as usize)));
    assert_eq!(report(&opts(&l, "0.1.0", "hoot"))["notice"], Value::Null);
    cache(&l, &format!(r#"{{"checked":1,"latest":"0.2.0"}}{}"#, " ".repeat(CACHE_CAP as usize)));
    assert_eq!(report(&opts(&l, "0.1.0", "hoot"))["notice"], Value::Null);
    cache(&l, r#"{"checked":1,"latest":"0.2.0"}"#);
    assert_eq!(report(&opts(&l, "0.1.0", "hoot"))["notice"]["checked"], 1, "and the same cache without the padding is read");
}

#[test]
fn a_latest_that_is_not_a_version_gives_no_line() {
    let (_t, l) = layout();
    cache(&l, r#"{"checked":1,"latest":"soon"}"#);
    let n = report(&opts(&l, "0.1.0", "hoot"))["notice"].clone();
    assert_eq!(n["line"], Value::Null);
    assert_eq!(n["latest"], "soon", "reported as written; the page decides what to show");
}

#[test]
fn a_day_old_cache_is_read_not_refreshed() {
    // notice::check would fetch here (and rewrite the file); status must not
    let (_t, l) = layout();
    let text = r#"{"checked":1,"latest":"0.2.0"}"#;
    cache(&l, text);
    let r = report(&opts(&l, "0.1.0", "hoot"));
    assert_eq!(r["notice"]["checked"], 1);
    assert_eq!(fs::read_to_string(l.state_dir().join("update-check")).unwrap(), text);
    assert_eq!(fs::read_dir(l.state_dir()).unwrap().count(), 1, "nothing else was written");
}

#[test]
fn plugins_are_the_record_sorted_without_hashes() {
    let (_t, l) = layout();
    assert_eq!(report(&opts(&l, "0.1.0", "hoot"))["plugins"], json!([]), "no plugins.json");
    let rec = r#"{"stamp":"0.1.0","plugins":{
        "subagent":{"version":"0.2.0","hash":"aa","how":"copy","installed":"bb"},
        "browser":{"version":"0.3.0","hash":"cc","how":"stock"}}}"#;
    fs::write(l.state_dir().join("plugins.json"), rec).unwrap();
    let p = report(&opts(&l, "0.1.0", "hoot"))["plugins"].clone();
    assert_eq!(p, json!([{ "name": "browser", "version": "0.3.0", "how": "stock" }, { "name": "subagent", "version": "0.2.0", "how": "copy" }]));
    // the same names `minerva plugins status` lists
    let names: Vec<String> = plugins::load_state(&l).plugins.keys().cloned().collect();
    assert_eq!(names, ["browser", "subagent"]);
    fs::write(l.state_dir().join("plugins.json"), "{ corrupt").unwrap();
    assert_eq!(report(&opts(&l, "0.1.0", "hoot"))["plugins"], json!([]), "a corrupt record is no record");
}

#[test]
fn a_record_past_the_cap_is_no_record() {
    let (_t, l) = layout();
    let rec = r#"{"stamp":"0.1.0","plugins":{"browser":{"version":"0.3.0","hash":"cc","how":"stock"}}}"#;
    fs::write(l.state_dir().join("plugins.json"), rec).unwrap();
    assert_eq!(report(&opts(&l, "0.1.0", "hoot"))["plugins"].as_array().unwrap().len(), 1, "the same record, unpadded, is read");
    // whole valid JSON followed by padding past the cap: not a file this program wrote
    fs::write(l.state_dir().join("plugins.json"), format!("{rec}{}", " ".repeat(RECORD_CAP as usize))).unwrap();
    assert_eq!(report(&opts(&l, "0.1.0", "hoot"))["plugins"], json!([]));
}

/// Runs `f` on its own thread and fails the test, instead of hanging the whole suite, when it has not finished in 5 s.
fn within_5s<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(std::time::Duration::from_secs(5)).expect("it blocked on a FIFO")
}

#[test]
fn something_that_is_not_a_regular_file_reads_as_absent_and_does_not_block() {
    let (_t, l) = layout();
    set_up(&l);
    for name in ["update-check", "plugins.json", "jev.pid"] {
        assert!(Command::new("mkfifo").arg(l.state_dir().join(name)).status().unwrap().success());
    }
    let prefix = l.prefix.clone();
    let r = within_5s(move || report(&opts(&Layout::new(prefix), "0.1.0", "hoot")));
    assert_eq!(r, json!({ "version": "0.1.0", "notice": null, "plugins": [], "jev": { "set_up": true, "running": false } }));
    // and a directory, or a link to nowhere, is no file either
    let (_t, l) = layout();
    fs::create_dir(l.state_dir().join("update-check")).unwrap();
    std::os::unix::fs::symlink("/nonexistent/x", l.state_dir().join("plugins.json")).unwrap();
    let r = report(&opts(&l, "0.1.0", "hoot"));
    assert_eq!((&r["notice"], &r["plugins"]), (&Value::Null, &json!([])));
}

#[test]
fn jev_not_set_up_is_not_running_whatever_the_pid_file_says() {
    let (_t, l) = layout();
    let mut svc = fake_service();
    fs::write(l.state_dir().join("jev.pid"), svc.id().to_string()).unwrap();
    assert_eq!(report(&opts(&l, "0.1.0", "hoot"))["jev"], json!({ "set_up": false, "running": false }));
    assert_eq!(jev::status(&l), "jev service: not set up (minerva jev-setup)");
    svc.kill().unwrap();
    svc.wait().unwrap();
}

#[test]
fn jev_set_up_states_agree_with_the_jev_status_sentence() {
    let (_t, l) = layout();
    set_up(&l);
    // set up, no pid file
    assert_eq!(report(&opts(&l, "0.1.0", "hoot"))["jev"], json!({ "set_up": true, "running": false }));
    assert_eq!(jev::status(&l), "jev service: set up, not running");
    // a pid file with junk, with a dead pid, with a live process that is not the service (this very test)
    for pid in ["junk".to_string(), "4294967295".to_string(), std::process::id().to_string(), String::new()] {
        fs::write(l.state_dir().join("jev.pid"), &pid).unwrap();
        assert_eq!(report(&opts(&l, "0.1.0", "hoot"))["jev"], json!({ "set_up": true, "running": false }), "{pid:?}");
        assert_eq!(jev::status(&l), "jev service: set up, not running", "{pid:?}");
    }
    // running: the pid is named, and it is the one the sentence names
    let mut svc = fake_service();
    fs::write(l.state_dir().join("jev.pid"), format!("{}\n", svc.id())).unwrap();
    assert_eq!(report(&opts(&l, "0.1.0", "hoot"))["jev"], json!({ "set_up": true, "running": true, "pid": svc.id() }));
    assert_eq!(jev::status(&l), format!("jev service: running (pid {})", svc.id()));
    svc.kill().unwrap();
    svc.wait().unwrap();
    assert_eq!(report(&opts(&l, "0.1.0", "hoot"))["jev"], json!({ "set_up": true, "running": false }), "gone with the process");
}
