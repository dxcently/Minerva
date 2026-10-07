//! `/hub/mesh` and `/hub/mesh/events`: the network-free mesh view (slice R2b), and `mesh::nodes`, which mail asks.
//!
//! A real perch, in front of `fake_aoide` (R2a) and a temp Aoide home written here. Every perch gets `HOME` of its own, so the default
//! `~/.aoide` is a temp tree too, and no test reads or runs anything of the operator's. The fake logs every call it gets
//! (`FAKE_AOIDE_LOG`), which is how the tests say what was run and with what environment; the daemon is a `UnixListener` in the test that
//! records what it is sent.
//!
//! Run with `cargo test --features test-bins`: the fake is a `test-bins` target, so without the feature this file is empty.
#![cfg(feature = "test-bins")]

mod common;

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::*;
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// The rig
// ---------------------------------------------------------------------------

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/aoide")
}

/// A registry with the shapes of both Aoides (`allows` flat, `grants` per mesh), and in it every field that must never be shown.
const REGISTRY: &str = r#"{
  "schemaVersion": "0",
  "nodes": [
    {"name": "osaka", "url": "http://user:hunter2@127.0.0.1:8711", "verified": true, "autogate": false, "allows": ["message", "read"],
     "via": "ssh://khoa@osaka", "addedAt": "2026-09-01T00:00:00Z", "hub": true,
     "pubkey": "PUBKEY-SENTINEL-0123", "tokenFile": "/home/khoa/.aoide/TOKENFILE-SENTINEL", "bearerSecret": "BEARER-SENTINEL"},
    {"name": "yomi", "url": "http://yomi:8710", "verified": true, "autogate": true, "addedAt": "2026-10-01T00:00:00Z",
     "grants": {"alpha": ["message"], "beta": ["message", "spawn"]}, "pubkey": "PUBKEY-SENTINEL-4567"},
    {"name": "bridge", "url": "http://bridge:8710", "addedAt": "2026-10-02T00:00:00Z"}
  ]
}"#;

/// What must not be in any answer: the three fields' values and names, a credential in a url, and what an agent said.
const SENTINELS: [&str; 9] =
    ["PUBKEY-SENTINEL", "TOKENFILE-SENTINEL", "BEARER-SENTINEL", "pubkey", "tokenFile", "bearerSecret", "hunter2", "SAY-SENTINEL", "SOCKET-SENTINEL"];

const GRAPH: &str = r#"{
  "schemaVersion": "0",
  "nodes": [
    {"id": "project:p", "kind": "project", "name": "p", "path": "/p"},
    {"id": "session:s1", "kind": "session", "agent": "claude", "cwd": "/p", "state": "running", "title": "t", "petname": "brisk-fox",
     "say": "SAY-SENTINEL", "activity": "SAY-SENTINEL", "socket": "/run/SOCKET-SENTINEL.sock"},
    {"id": "node:osaka", "kind": "node", "name": "osaka", "state": "fresh", "children": {"nodes": [{"id": "x", "say": "SAY-SENTINEL"}], "edges": []}}
  ],
  "edges": [{"from": "project:p", "to": "session:s1", "kind": "anchors"}]
}"#;

/// Which flags a perch is started with.
#[derive(Clone)]
struct Setup {
    aoide: bool,
    home: bool,
    events: bool,
    socket: bool,
    state: bool,
    ttl_ms: Option<u64>,
    fake_dir: PathBuf,
}

impl Setup {
    fn all() -> Self {
        Self { aoide: true, home: true, events: true, socket: true, state: true, ttl_ms: None, fake_dir: fixtures() }
    }

    fn start(self) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        // `HOME` of the perch: empty, so nothing of the real one is the default of anything.
        let user = root.join("user");
        std::fs::create_dir(&user).unwrap();
        let (home, events, socket, log) = (root.join("aoide"), root.join("events.jsonl"), root.join("aoided.sock"), root.join("fake.log"));
        let state = home.join("state");
        if self.state {
            std::fs::create_dir_all(state.join("node-cache")).unwrap();
            std::fs::create_dir_all(state.join("stage")).unwrap();
            std::fs::write(state.join("nodes.json"), REGISTRY).unwrap();
            std::fs::write(state.join("stage/graph.json"), GRAPH).unwrap();
        }
        let mut builder = Perch::builder()
            .env("HOME", user.to_str().unwrap())
            .env("FAKE_AOIDE_DIR", self.fake_dir.to_str().unwrap())
            .env("FAKE_AOIDE_LOG", log.to_str().unwrap())
            // What a perch started from inside a conducted session has in its environment: none of it may reach a call.
            .env("AOIDE_SESSION_ID", "a-conducted-session")
            .env("AOIDE_STATE_DIR", "/nonexistent/ambient-state");
        if self.aoide {
            builder = builder.arg("--aoide").arg(fake_aoide().expect("the stand-in aoide"));
        }
        if self.home {
            builder = builder.arg("--aoide-home").arg(&home);
        }
        if self.events {
            builder = builder.arg("--aoide-events").arg(&events);
        }
        if self.socket {
            builder = builder.arg("--aoide-daemon-socket").arg(&socket);
        }
        if let Some(ms) = self.ttl_ms {
            builder = builder.arg("--knob").arg(format!("mesh_ttl_ms={ms}"));
        }
        let perch = builder.hold(dir).start();
        Rig { perch, home, state, events, socket, log }
    }
}

struct Rig {
    perch: Perch,
    /// `--aoide-home`, and `state/` in it.
    home: PathBuf,
    state: PathBuf,
    events: PathBuf,
    socket: PathBuf,
    log: PathBuf,
}

fn json_of(answer: &Answer, what: &str) -> Value {
    assert_eq!(answer.status, 200, "{what}: {}", answer.head);
    assert_eq!(answer.header("content-type").as_deref(), Some("application/json"), "{what}: {}", answer.head);
    serde_json::from_str(&answer.body).unwrap_or_else(|e| panic!("{what} is JSON: {e}: {:?}", answer.body))
}

impl Rig {
    fn get(&self, path: &str) -> Value {
        json_of(&self.perch.get(path), path)
    }

    fn snapshot(&self) -> Value {
        self.get("/hub/mesh")
    }

    /// Every line the fake logged that is a call (`argv`), in order.
    fn calls(&self) -> Vec<Value> {
        let text = std::fs::read_to_string(&self.log).unwrap_or_default();
        text.lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap_or_else(|e| panic!("log line {line:?}: {e}")))
            .filter(|line| line.get("argv").is_some())
            .collect()
    }

    fn argvs(&self) -> Vec<Vec<String>> {
        let mut argvs: Vec<Vec<String>> =
            self.calls().iter().map(|call| call["argv"].as_array().unwrap().iter().map(|word| word.as_str().unwrap().to_string()).collect()).collect();
        argvs.sort();
        argvs
    }

    /// How many times `mesh --json` was run.
    fn mesh_runs(&self) -> usize {
        self.argvs().iter().filter(|argv| argv.first().map(String::as_str) == Some("mesh")).count()
    }

    fn write_state(&self, name: &str, text: &str) {
        let path = self.state.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    /// `GET /hub/mesh/events` with the query as given.
    fn tail(&self, query: &str) -> Value {
        self.get(&format!("/hub/mesh/events{query}"))
    }
}

fn words(list: &[&str]) -> Vec<String> {
    list.iter().map(|word| word.to_string()).collect()
}

fn names(snapshot: &Value, part: &str) -> Vec<String> {
    snapshot[part].as_array().unwrap().iter().map(|row| row["name"].as_str().unwrap().to_string()).collect()
}

// ---------------------------------------------------------------------------
// The calls
// ---------------------------------------------------------------------------

/// The fake's log holds the three calls and nothing else, each run with the perch's places and none of its ambient identity. This is also
/// `Aoide::from_options` at the perch level: the four flags arrive as the four variables.
#[test]
fn a_snapshot_runs_version_mesh_and_outbox_and_nothing_else() {
    let rig = Setup::all().start();
    let snapshot = rig.snapshot();
    assert_eq!(rig.argvs(), vec![words(&["--version"]), words(&["mail", "outbox", "--json"]), words(&["mesh", "--json"])]);
    let calls = rig.calls();
    assert_eq!(calls.len(), 3);
    for call in &calls {
        assert_eq!(call["session"], Value::Null, "the perch's own AOIDE_SESSION_ID is not handed on: {call}");
        assert_eq!(
            call["aoide_env"],
            json!({
                "AOIDE_ROOT": rig.home.to_str().unwrap(),
                "AOIDE_AUDIT_LOG": rig.events.to_str().unwrap(),
                "AOIDE_DAEMON_SOCKET": rig.socket.to_str().unwrap(),
            }),
            "the four flags are the four variables, and the ambient AOIDE_STATE_DIR is gone"
        );
        assert_eq!(call["xdg_runtime_dir"], rig.perch.runtime.path().to_str().unwrap());
    }
    assert_eq!(snapshot["available"], true);
    assert_eq!(snapshot["aoide"], json!({"path": fake_aoide().unwrap(), "version": "0.0.26"}));
    assert_eq!(snapshot["daemon"], json!({"socket": rig.socket.to_str().unwrap(), "ping_ms": null}), "nothing listens");
    assert_eq!(snapshot["errors"], json!([]));
    let sections = snapshot["mesh"]["report"]["sections"].as_array().unwrap();
    assert_eq!(sections.iter().map(|section| section["name"].as_str().unwrap()).collect::<Vec<_>>(), ["alpha", "beta"]);
    assert_eq!(snapshot["outbox"]["entries"].as_array().unwrap().len(), 2);
}

/// Two reads inside the minute run `mesh --json` once; a third past it runs it again. The ttl knob is what lets a test pass the minute.
#[test]
fn two_gets_within_the_ttl_cause_one_mesh_run() {
    let rig = Setup::all().start();
    let (first, second) = (rig.snapshot(), rig.snapshot());
    assert_eq!(first, second);
    assert_eq!(rig.argvs().len(), 3, "the second read ran nothing");
    assert_eq!(rig.mesh_runs(), 1);
}

#[test]
fn a_run_is_asked_for_again_once_its_ttl_has_passed() {
    let rig = Setup { ttl_ms: Some(700), ..Setup::all() }.start();
    rig.snapshot();
    assert_eq!(rig.mesh_runs(), 1);
    std::thread::sleep(Duration::from_millis(800));
    rig.snapshot();
    assert_eq!(rig.mesh_runs(), 2, "past the ttl it runs again");
    assert_eq!(rig.argvs().len(), 6, "all three are cached alike");
}

/// A failure is a sentence in `errors` and is asked for again on the next read, while what did answer is still there.
#[test]
fn a_failed_run_is_a_sentence_and_is_not_cached() {
    let dir = tempfile::tempdir().unwrap();
    // The fake exits 99 for a fixture that is not there.
    for name in ["version.txt", "outbox.json"] {
        std::fs::copy(fixtures().join(name), dir.path().join(name)).unwrap();
    }
    let rig = Setup { fake_dir: dir.path().to_path_buf(), ..Setup::all() }.start();
    let first = rig.snapshot();
    assert_eq!(first["mesh"], Value::Null);
    let errors = first["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].as_str().unwrap().starts_with("aoide mesh: "), "{errors:?}");
    assert_eq!(first["outbox"]["entries"].as_array().unwrap().len(), 2, "the other parts are not spoiled");
    assert_eq!(first["aoide"]["version"], "0.0.26");
    std::fs::copy(fixtures().join("mesh.json"), dir.path().join("mesh.json")).unwrap();
    let second = rig.snapshot();
    assert_eq!(second["errors"], json!([]));
    assert_eq!(second["mesh"]["report"]["sections"].as_array().unwrap().len(), 2);
    assert_eq!(rig.mesh_runs(), 2, "the failure was not kept");
    assert_eq!(rig.argvs().len(), 4, "and the outbox and the version were: three, then one more mesh");
}

// ---------------------------------------------------------------------------
// The registry, the cache, the graph
// ---------------------------------------------------------------------------

/// The allowlist, from outside: no answer of either route carries a secret field, its value or its name. Reverting the allowlist (a node
/// serialized as it is in the file) fails here, and so does a graph or a url that is not projected.
#[test]
fn no_pubkey_tokenfile_or_bearersecret_is_in_any_response() {
    let rig = Setup::all().start();
    // The feed has lines of the classes that are shown, so that route's body is not empty either.
    std::fs::write(&rig.events, "{\"class\":\"audit\",\"kind\":\"hand-edit\",\"payload\":{\"file\":\"sessions.json\"}}\n").unwrap();
    for path in ["/hub/mesh", "/hub/mesh/events", "/hub/mesh/events?after=0"] {
        let answer = rig.perch.get(path);
        assert_eq!(answer.status, 200, "{path}");
        for sentinel in SENTINELS {
            assert!(!answer.body.contains(sentinel), "{sentinel} is in the answer to {path}: {}", answer.body);
        }
    }
    let snapshot = rig.snapshot();
    // What is shown of a node is exactly the allowlist.
    for node in snapshot["registry"].as_array().unwrap() {
        let mut keys: Vec<&str> = node.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, ["addedAt", "allows", "autogate", "grants", "name", "url", "verified", "via"], "{node}");
    }
    assert_eq!(snapshot["registry"][0]["url"], "http://127.0.0.1:8711", "a credential in a url is not shown");
    // The graph is counted and listed, and what the agents said is not in it.
    assert_eq!(snapshot["graph"]["total"], 3);
    assert_eq!(snapshot["graph"]["kinds"], json!({"node": 1, "project": 1, "session": 1}));
    assert_eq!(snapshot["graph"]["edges"], 1);
    assert_eq!(snapshot["graph"]["nodes"][1]["petname"], "brisk-fox");
    assert_eq!(snapshot["graph"]["nodes"][2]["children"], 1);
}

/// Aoide before P-CHARTER writes `allows` flat and Aoide after it grants per mesh; both are read, and a shape that is neither is said
/// in `errors` and not guessed at.
#[test]
fn both_registry_shapes_parse() {
    let rig = Setup::all().start();
    let snapshot = rig.snapshot();
    assert_eq!(names(&snapshot, "registry"), ["osaka", "yomi", "bridge"]);
    let [osaka, yomi, bridge] = &snapshot["registry"].as_array().unwrap()[..] else { panic!("three nodes") };
    assert_eq!((osaka["verified"].clone(), osaka["allows"].clone(), osaka["grants"].clone()), (json!(true), json!(["message", "read"]), json!({})));
    assert_eq!(osaka["via"], "ssh://khoa@osaka");
    assert_eq!((yomi["allows"].clone(), yomi["grants"].clone()), (json!([]), json!({"alpha": ["message"], "beta": ["message", "spawn"]})));
    assert_eq!((yomi["autogate"].clone(), yomi["addedAt"].clone()), (json!(true), json!("2026-10-01T00:00:00Z")));
    assert_eq!((bridge["verified"].clone(), bridge["via"].clone(), bridge["allows"].clone()), (json!(false), Value::Null, json!([])));
    assert_eq!(snapshot["errors"], json!([]));

    // A registry that says things in a shape neither Aoide wrote.
    rig.write_state(
        "nodes.json",
        r#"{"peers": [{"name": "old", "allows": ["read"]}, {"name": "new", "grants": {"alpha": 7}}, {"name": "Not A Name"}, {"url": "x"}]}"#,
    );
    let odd = rig.snapshot();
    assert_eq!(names(&odd, "registry"), ["old", "new"], "the pre-rename key is read too");
    assert_eq!(odd["registry"][0]["allows"], json!(["read"]));
    assert_eq!(odd["registry"][1]["grants"], json!({}));
    let errors: Vec<&str> = odd["errors"].as_array().unwrap().iter().map(|e| e.as_str().unwrap()).collect();
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(errors.iter().any(|e| e.contains("2 registry entries were left out")), "{errors:?}");
    assert!(errors.iter().any(|e| e.contains("1 registry fields were left out")), "{errors:?}");

    // Not JSON, and not a registry at all: no nodes, and a sentence for each.
    for (text, says) in [("not json", "is not JSON"), (r#"{"version": 1}"#, "not in a shape")] {
        rig.write_state("nodes.json", text);
        let broken = rig.snapshot();
        assert_eq!(broken["registry"], json!([]));
        assert!(broken["errors"][0].as_str().unwrap().contains(says), "{broken}");
    }
}

/// A link under `nodes.json` is not followed: the registry is a file of Aoide's own, and a name that points elsewhere is not it.
#[test]
fn a_registry_that_is_a_link_is_not_read() {
    let rig = Setup::all().start();
    let elsewhere = rig.home.join("elsewhere.json");
    std::fs::write(&elsewhere, r#"[{"name": "planted"}]"#).unwrap();
    std::fs::remove_file(rig.state.join("nodes.json")).unwrap();
    std::os::unix::fs::symlink(&elsewhere, rig.state.join("nodes.json")).unwrap();
    let snapshot = rig.snapshot();
    assert_eq!(snapshot["registry"], json!([]));
    assert!(snapshot["errors"][0].as_str().unwrap().contains("plain file"), "{snapshot}");
}

/// A roster another node sent is text in a file. It comes back as a JSON string, unchanged, with its age; it is never parsed, and what
/// is not a plain `.json` file under 64 files is not listed.
#[test]
fn hostile_cache_text_comes_back_as_a_json_string() {
    let rig = Setup::all().start();
    let hostile = "<img src=x onerror=alert(1)></script><script>alert(2)</script>\u{1b}[31mred\u{1b}]0;owned\u{7} {{7*7}} \u{202e}gnp.exe";
    let cache = rig.state.join("node-cache");
    std::fs::write(cache.join("yomi.json"), hostile).unwrap();
    std::fs::write(cache.join("osaka.json"), r#"{"fetchedAt": "x", "graph": {"nodes": []}}"#).unwrap();
    std::fs::write(cache.join("big.json"), "b".repeat(64 * 1024 + 500)).unwrap();
    std::fs::write(cache.join("notes.txt"), "not a roster").unwrap();
    std::os::unix::fs::symlink(rig.state.join("nodes.json"), cache.join("link.json")).unwrap();
    std::fs::File::options().write(true).open(cache.join("osaka.json")).unwrap().set_modified(std::time::SystemTime::now() - Duration::from_secs(600)).unwrap();

    let answer = rig.perch.get("/hub/mesh");
    assert!(!answer.body.contains('\u{1b}') && !answer.body.contains('\u{7}'), "a control character is escaped in the JSON, not sent raw");
    let snapshot = json_of(&answer, "/hub/mesh");
    assert_eq!(names(&snapshot, "cache"), ["big", "osaka", "yomi"], "regular .json files, by name; a link and a .txt are not rosters");
    let [big, osaka, yomi] = &snapshot["cache"].as_array().unwrap()[..] else { panic!("three rosters") };
    assert_eq!(yomi["text"], hostile, "text is the file's, character for character");
    assert!(yomi["age_secs"].as_u64().unwrap() < 60);
    assert!((600..620).contains(&osaka["age_secs"].as_u64().unwrap()), "{osaka}");
    assert_eq!(osaka["text"], r#"{"fetchedAt": "x", "graph": {"nodes": []}}"#, "a roster that is JSON is still text here");
    assert_eq!((big["text"].as_str().unwrap().len(), big["cut"].clone()), (64 * 1024, json!(true)));
    assert_eq!((osaka["cut"].clone(), yomi["cut"].clone()), (json!(false), json!(false)));
    assert_eq!(snapshot["errors"], json!([]));

    // Sixty-four files at most.
    for n in 0..70 {
        std::fs::write(cache.join(format!("n{n:02}.json")), "{}").unwrap();
    }
    let many = rig.snapshot();
    assert_eq!(many["cache"].as_array().unwrap().len(), 64);
    assert!(many["errors"][0].as_str().unwrap().contains("more than 64 files"), "{many}");
}

// ---------------------------------------------------------------------------
// Not available
// ---------------------------------------------------------------------------

fn bare_perch(state: Option<&str>) -> (Perch, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let user = dir.path().join("user");
    std::fs::create_dir(&user).unwrap();
    if let Some(registry) = state {
        std::fs::create_dir_all(user.join(".aoide/state")).unwrap();
        std::fs::write(user.join(".aoide/state/nodes.json"), registry).unwrap();
    }
    // No --aoide, and a HOME with no ~/.local/bin/aoide: the perch has no program.
    (Perch::builder().env("HOME", user.to_str().unwrap()).hold(dir).start(), user)
}

#[test]
fn no_aoide_and_no_state_is_unavailable_with_a_sentence() {
    let (perch, _user) = bare_perch(None);
    let snapshot = json_of(&perch.get("/hub/mesh"), "/hub/mesh");
    assert_eq!(snapshot["available"], false);
    let errors = snapshot["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 1);
    assert!(errors[0].as_str().unwrap().starts_with("Aoide is not available here"), "{errors:?}");
    assert!(errors[0].as_str().unwrap().contains("--aoide"), "the sentence says what to do: {errors:?}");
    for empty in ["registry", "cache"] {
        assert_eq!(snapshot[empty], json!([]));
    }
    for none in ["aoide", "graph", "mesh", "outbox"] {
        assert_eq!(snapshot[none], Value::Null, "{none}");
    }
    assert_eq!(snapshot["daemon"], json!({"socket": null, "ping_ms": null}));
    // The feed is an empty one, not an error.
    let events = json_of(&perch.get("/hub/mesh/events"), "/hub/mesh/events");
    assert_eq!(events, json!({"events": [], "next": 0, "reset": false}));
}

/// State and no program is available, and the home is `~/.aoide` of the perch's own `HOME` when no flag names one. Nothing is run.
#[test]
fn state_without_a_program_is_available_and_says_what_is_missing() {
    let (perch, user) = bare_perch(Some(r#"[{"name": "osaka", "verified": true}]"#));
    let snapshot = json_of(&perch.get("/hub/mesh"), "/hub/mesh");
    assert_eq!(snapshot["available"], true);
    assert_eq!(names(&snapshot, "registry"), ["osaka"], "read from ~/.aoide of the perch's HOME ({})", user.display());
    assert_eq!((snapshot["aoide"].clone(), snapshot["mesh"].clone(), snapshot["outbox"].clone()), (Value::Null, Value::Null, Value::Null));
    let errors = snapshot["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 1);
    assert!(errors[0].as_str().unwrap().starts_with("no aoide is configured"), "{errors:?}");
}

/// A program and no state folder (a fresh install): available, nothing to list, and the calls still answer.
#[test]
fn a_program_without_state_is_available_and_lists_nothing() {
    let rig = Setup { state: false, ..Setup::all() }.start();
    let snapshot = rig.snapshot();
    assert_eq!(snapshot["available"], true);
    assert_eq!((snapshot["registry"].clone(), snapshot["cache"].clone(), snapshot["graph"].clone()), (json!([]), json!([]), Value::Null));
    assert_eq!(snapshot["errors"], json!([]));
    assert_eq!(snapshot["mesh"]["report"]["sections"].as_array().unwrap().len(), 2);
}

// ---------------------------------------------------------------------------
// The events
// ---------------------------------------------------------------------------

fn kinds(tail: &Value) -> Vec<String> {
    tail["events"].as_array().unwrap().iter().map(|event| event["kind"].as_str().unwrap().to_string()).collect()
}

/// Only the `audit` and `gate` classes come back; a secret's name, a forwarded notification, a line that is not an event and a half line
/// do not; and a file truncated in place (aoided does it at 1 MiB) is read again from 0.
#[test]
fn the_events_tail_keeps_two_classes_and_survives_a_truncate_in_place() {
    let rig = Setup::all().start();
    let lines = [
        r#"{"v":0,"ts":1,"class":"audit","kind":"hand-edit","source":"aoided","payload":{"file":"sessions.json"}}"#,
        r#"{"v":0,"ts":2,"class":"secret","kind":"released","payload":{"secret":"SECRET-NAME-SENTINEL","consumer":"CONSUMER-SENTINEL"}}"#,
        r#"{"v":0,"ts":3,"class":"gate","kind":"pending","payload":{"node":"osaka","note":"<script>alert(1)</script>\u001b[31m"}}"#,
        r#"{"v":0,"ts":4,"class":"notification","kind":"forwarded","payload":{"body":"NOTIFICATION-SENTINEL"}}"#,
        r#"{"v":0,"ts":5,"class":"rice","kind":"repaint"}"#,
        r#"{"v":0,"ts":6,"class":"content","kind":"page"}"#,
        "this is not json",
        r#"{"v":0,"ts":7,"class":"audit","kind":"last"}"#,
    ];
    let mut text: String = lines.iter().map(|line| format!("{line}\n")).collect();
    text.push_str("{\"class\":\"gate\",\"kind\":\"half");
    std::fs::write(&rig.events, text).unwrap();
    let whole = lines.iter().map(|line| line.len() as u64 + 1).sum::<u64>();

    let first = rig.tail("?after=0");
    assert_eq!(kinds(&first), ["hand-edit", "pending", "last"]);
    assert_eq!((first["next"].clone(), first["reset"].clone()), (json!(whole), json!(false)), "the cursor stops before the half line");
    let answer = rig.perch.get("/hub/mesh/events?after=0");
    for sentinel in ["SECRET-NAME-SENTINEL", "CONSUMER-SENTINEL", "NOTIFICATION-SENTINEL"] {
        assert!(!answer.body.contains(sentinel), "{sentinel} is in the feed's answer: {}", answer.body);
    }
    assert_eq!(first["events"][1]["payload"]["note"], "<script>alert(1)</script>\u{1b}[31m", "remote text is a string, whole");

    // The line is finished, and the next poll has exactly it.
    let mut file = std::fs::OpenOptions::new().append(true).open(&rig.events).unwrap();
    file.write_all(b"-over\"}\n").unwrap();
    let next = rig.tail(&format!("?after={whole}"));
    assert_eq!(kinds(&next), ["half-over"]);
    let end = std::fs::metadata(&rig.events).unwrap().len();
    assert_eq!(next["next"], end);
    let quiet = rig.tail(&format!("?after={end}"));
    assert_eq!((kinds(&quiet), quiet["next"].clone(), quiet["reset"].clone()), (Vec::<String>::new(), json!(end), json!(false)));

    // Truncated in place, then written to again: the file is shorter than the cursor, so it starts over.
    std::fs::OpenOptions::new().write(true).truncate(true).open(&rig.events).unwrap().write_all(b"{\"class\":\"audit\",\"kind\":\"after-truncate\"}\n").unwrap();
    let again = rig.tail(&format!("?after={end}"));
    assert_eq!(again["reset"], true);
    assert_eq!(kinds(&again), ["after-truncate"]);
    assert_eq!(again["next"], std::fs::metadata(&rig.events).unwrap().len());
}

/// A call reads 64 KiB of the file at most, the calls together lose and repeat nothing, and the first poll (no cursor) is the newest
/// window and starts on a whole line.
#[test]
fn a_poll_of_the_events_is_at_most_64_kib_and_the_polls_lose_nothing() {
    let rig = Setup::all().start();
    let line = |n: usize| format!("{{\"class\":\"gate\",\"kind\":\"k{n:05}\",\"payload\":\"{}\"}}\n", "p".repeat(200));
    std::fs::write(&rig.events, (0..1000).map(line).collect::<String>()).unwrap();
    let size = std::fs::metadata(&rig.events).unwrap().len();
    assert!(size > 3 * 64 * 1024);

    let (mut after, mut seen) = (0u64, Vec::new());
    let mut calls = 0;
    while after < size {
        let answer = rig.perch.get(&format!("/hub/mesh/events?after={after}"));
        assert!(answer.body.len() <= 64 * 1024 + 100, "a poll answered {} bytes", answer.body.len());
        let tail = json_of(&answer, "events");
        let next = tail["next"].as_u64().unwrap();
        assert!(next > after && next - after <= 64 * 1024, "{after} -> {next}");
        seen.extend(kinds(&tail));
        (after, calls) = (next, calls + 1);
    }
    assert!(calls >= 4, "{calls}");
    assert_eq!(seen, (0..1000).map(|n| format!("k{n:05}")).collect::<Vec<_>>());

    let newest = rig.tail("");
    assert_eq!(newest["next"], size);
    assert_eq!(kinds(&newest).last().map(String::as_str), Some("k00999"));
    let first = &kinds(&newest)[0];
    assert_ne!(first, "k00000", "the first poll is the end of the file and not all of it");
    assert_eq!(kinds(&newest).len(), 1000 - first[1..].parse::<usize>().unwrap(), "from a whole line to the end, none missing");
}

/// A feed that is not there yet is an empty one (and a cursor from one that was is past its end, so it starts over), and with no
/// `--aoide-events` there is nothing to tail: the file next to the socket is not guessed at.
#[test]
fn a_missing_feed_is_empty_and_no_flag_means_no_feed() {
    let rig = Setup::all().start();
    assert_eq!(rig.tail(""), json!({"events": [], "next": 0, "reset": false}));
    assert_eq!(rig.tail("?after=40"), json!({"events": [], "next": 0, "reset": true}), "a cursor from a feed that is gone starts over");
    std::fs::write(&rig.events, "{\"class\":\"audit\",\"kind\":\"there\"}\n").unwrap();
    assert_eq!(kinds(&rig.tail("")), ["there"]);

    let flagless = Setup { events: false, ..Setup::all() }.start();
    std::fs::write(&flagless.events, "{\"class\":\"audit\",\"kind\":\"beside-the-socket\"}\n").unwrap();
    assert_eq!(flagless.tail(""), json!({"events": [], "next": 0, "reset": false}));
    flagless.snapshot();
    assert!(
        flagless.calls().iter().all(|call| call["aoide_env"].get("AOIDE_AUDIT_LOG").is_none()),
        "no flag, no AOIDE_AUDIT_LOG: {:?}",
        flagless.calls()
    );
}

/// A feed that is a link is not followed (502); the only query is `after=<digits>`.
#[test]
fn the_events_route_takes_one_offset_and_reads_a_plain_file() {
    let rig = Setup::all().start();
    for bad in ["after=", "after=-1", "after=x", "after=1.5", "after=%31", "after=1&after=2", "after=1&x=2", "x=1", "after"] {
        let answer = rig.perch.get(&format!("/hub/mesh/events?{bad}"));
        assert_eq!(answer.status, 400, "{bad}: {}", answer.head);
    }
    let elsewhere = rig.home.join("elsewhere.jsonl");
    std::fs::write(&elsewhere, "{\"class\":\"audit\",\"kind\":\"planted\"}\n").unwrap();
    std::os::unix::fs::symlink(&elsewhere, &rig.events).unwrap();
    let answer = rig.perch.get("/hub/mesh/events?after=0");
    assert_eq!(answer.status, 502, "{}", answer.head);
    assert!(!answer.body.contains("planted"));
}

// ---------------------------------------------------------------------------
// The daemon
// ---------------------------------------------------------------------------

/// A stand-in aoided on a Unix socket: what every connection sent, line by line, and how many connections there were. `reply` is what it
/// answers a line with, `None` for a daemon that takes the line and says nothing for longer than the perch waits.
struct Daemon {
    lines: Arc<Mutex<Vec<String>>>,
    connections: Arc<Mutex<usize>>,
}

impl Daemon {
    fn bind(socket: &Path, reply: Option<&'static str>) -> Self {
        let listener = UnixListener::bind(socket).unwrap();
        let daemon = Self { lines: Arc::default(), connections: Arc::default() };
        let (lines, connections) = (daemon.lines.clone(), daemon.connections.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                *connections.lock().unwrap() += 1;
                let lines = lines.clone();
                std::thread::spawn(move || {
                    let mut writer = stream.try_clone().unwrap();
                    let mut reader = BufReader::new(stream);
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 {
                            return;
                        }
                        lines.lock().unwrap().push(line.trim_end().to_string());
                        match reply {
                            Some(reply) => {
                                let _ = writeln!(writer, "{reply}");
                            }
                            None => std::thread::sleep(Duration::from_secs(4)),
                        }
                    }
                });
            }
        });
        daemon
    }

    fn lines(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }
}

const PONG: &str = r#"{"ok":true,"daemon":"aoided","pid":424242,"version":"0.0.26","sealPubkeyHex":"SEAL-SENTINEL"}"#;

/// The daemon is sent one ping on one connection, however many tabs are open, and never a `subscribe` or a `dispatch`; what it answers is
/// read for `ok` and nothing else.
#[test]
fn the_daemon_sees_one_ping_per_read_and_nothing_else() {
    let rig = Setup::all().start();
    let daemon = Daemon::bind(&rig.socket, Some(PONG));
    let answer = rig.perch.get("/hub/mesh");
    let snapshot = json_of(&answer, "/hub/mesh");
    assert!(snapshot["daemon"]["ping_ms"].as_u64().is_some(), "{}", snapshot["daemon"]);
    assert_eq!(daemon.lines(), [r#"{"v":0,"op":"ping"}"#], "exactly one request, and it is a ping");
    assert_eq!(*daemon.connections.lock().unwrap(), 1);
    for secret in ["SEAL-SENTINEL", "424242"] {
        assert!(!answer.body.contains(secret), "{secret} of the daemon's reply reached the page");
    }
    // The events come from the file; reading them asks the daemon nothing.
    rig.tail("");
    rig.snapshot();
    let lines = daemon.lines();
    assert_eq!(lines.len(), 2, "one more ping for the second snapshot: {lines:?}");
    assert!(lines.iter().all(|line| line == r#"{"v":0,"op":"ping"}"#), "{lines:?}");
    for line in &lines {
        assert!(!line.contains("subscribe") && !line.contains("dispatch"), "{line}");
    }
}

/// A daemon that does not answer in a second, says no, or is not there is `null`, and the snapshot is not held up past that.
#[test]
fn a_daemon_that_does_not_answer_is_null_and_does_not_hold_the_read_up() {
    let silent = Setup::all().start();
    let daemon = Daemon::bind(&silent.socket, None);
    let begun = Instant::now();
    let snapshot = silent.snapshot();
    assert_eq!(snapshot["daemon"]["ping_ms"], Value::Null);
    assert!(begun.elapsed() < Duration::from_millis(3500), "waited {:?}", begun.elapsed());
    assert_eq!(daemon.lines(), [r#"{"v":0,"op":"ping"}"#]);
    assert_eq!(snapshot["errors"], json!([]), "a daemon that is down is not an error: the dot is what says it");

    for reply in [r#"{"ok":false,"error":"no"}"#, "not json", r#"{"ok":"yes"}"#, "[]"] {
        let rig = Setup::all().start();
        Daemon::bind(&rig.socket, Some(reply));
        assert_eq!(rig.snapshot()["daemon"]["ping_ms"], Value::Null, "{reply}");
    }
    // A path that is a file and not a socket.
    let file = Setup::all().start();
    std::fs::write(&file.socket, "not a socket").unwrap();
    assert_eq!(file.snapshot()["daemon"]["ping_ms"], Value::Null);
}

// ---------------------------------------------------------------------------
// nodes(), for mail
// ---------------------------------------------------------------------------

/// A program that runs the stand-in with this test's fixtures and log. The runner passes its environment on, but a test cannot set the
/// process environment without racing the others, so the settings live in a script.
fn wrapper(path: PathBuf, fixtures: &Path, log: Option<&Path>) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let log = log.map(|log| format!("FAKE_AOIDE_LOG='{}' ", log.display())).unwrap_or_default();
    let script = format!("#!/bin/sh\nFAKE_AOIDE_DIR='{}' {log}exec '{}' \"$@\"\n", fixtures.display(), fake_aoide().unwrap());
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A `Ctx` over options that name only what `mesh::nodes` reads. The seams were laid so that no slice adds a field to it.
async fn ctx_for(dir: &Path, aoide: Option<PathBuf>, home: Option<PathBuf>) -> perch::serve::Ctx {
    let sessions = dir.join("sessions");
    let runtime = dir.join("run");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::create_dir_all(&runtime).unwrap();
    let opts = perch::Options {
        bind: "127.0.0.1:0".parse().unwrap(),
        ui_dir: None,
        roots: vec![sessions.clone()],
        eidolon: PathBuf::from("/nonexistent/eidolon"),
        sessions_dir: sessions.clone(),
        eidolon_config: None,
        scratch: None,
        other_sessions: None,
        modes: Default::default(),
        code_ttl: Duration::from_secs(60),
        plugins_dir: None,
        run_graphs: Vec::new(),
        jev_health: None,
        browser_health: None,
        aoide,
        aoide_home: home,
        aoide_daemon_socket: None,
        aoide_events: None,
        minerva: None,
        term_token_file: None,
        mail_token_file: None,
        agent_url_file: None,
        knobs: perch::Knobs::default(),
    };
    let registry = perch::sessions::Registry::new(sessions, runtime, opts.eidolon.clone(), opts.roots.clone()).unwrap();
    perch::serve::Ctx {
        bound: "127.0.0.1:0".parse().unwrap(),
        token: Arc::from("not-a-real-token"),
        ui: None,
        registry,
        spawner: Arc::new(perch::spawn::Spawner::new().unwrap()),
        tree_slots: perch::tree::Slots::new(perch::tree::MAX_LISTINGS),
        logins: Arc::new(perch::login::Logins::new(Duration::from_secs(60))),
        opts: Arc::new(opts),
        stop: tokio_util::sync::CancellationToken::new(),
        runs: Arc::default(),
        mesh: Arc::default(),
        meta: Arc::default(),
        install: Arc::default(),
        term: Arc::default(),
        notes: Arc::default(),
        mail: Arc::default(),
        allow: Arc::default(),
        agent: Arc::default(),
    }
}

fn logged(log: &Path) -> Vec<Vec<String>> {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    text.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|line| line.get("argv").cloned())
        .map(|argv| argv.as_array().unwrap().iter().map(|word| word.as_str().unwrap().to_string()).collect())
        .collect()
}

fn info(name: &str, verified: bool, meshes: &[&str]) -> perch::mesh::NodeInfo {
    perch::mesh::NodeInfo { name: name.to_string(), verified, meshes: meshes.iter().map(|mesh| mesh.to_string()).collect() }
}

/// What mail's send route asks: the registry's nodes with the meshes `mesh --json` has a row for them in (R2a's fixture: `osaka` in
/// alpha and beta, so a send to it must say which; `yomi` in alpha only; `bridge` in none). One `mesh --json` serves any number of asks,
/// and nothing else is run.
#[tokio::test]
async fn nodes_lists_the_meshes_per_node() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("aoide");
    std::fs::create_dir_all(home.join("state")).unwrap();
    std::fs::write(home.join("state/nodes.json"), REGISTRY).unwrap();
    let log = dir.path().join("fake.log");
    let ctx = ctx_for(dir.path(), Some(wrapper(dir.path().join("aoide-bin"), &fixtures(), Some(&log))), Some(home)).await;
    let expected = vec![info("osaka", true, &["alpha", "beta"]), info("yomi", true, &["alpha"]), info("bridge", false, &[])];
    assert_eq!(perch::mesh::nodes(&ctx).await, expected);
    assert_eq!(perch::mesh::nodes(&ctx).await, expected);
    assert_eq!(logged(&log), [words(&["mesh", "--json"])], "one run for two asks, and no version, outbox, mail or node call");
    let shown = serde_json::to_string(&expected).unwrap();
    assert!(shown.contains("\"meshes\":[\"alpha\",\"beta\"]") && !shown.contains("PUBKEY"), "{shown}");
}

/// No program, no state, or a program that fails is a shorter list and never an error: mail then has its own sentences.
#[tokio::test]
async fn nodes_without_a_program_or_a_registry_is_a_shorter_list() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("aoide");
    std::fs::create_dir_all(home.join("state")).unwrap();
    std::fs::write(home.join("state/nodes.json"), REGISTRY).unwrap();
    // No program: the nodes, with no meshes (and nothing run).
    let ctx = ctx_for(dir.path(), None, Some(home.clone())).await;
    assert_eq!(perch::mesh::nodes(&ctx).await, vec![info("osaka", true, &[]), info("yomi", true, &[]), info("bridge", false, &[])]);
    // No registry: nothing to offer.
    let ctx = ctx_for(dir.path(), None, Some(dir.path().join("nowhere"))).await;
    assert_eq!(perch::mesh::nodes(&ctx).await, vec![]);
    // A program that exits 99 for the mesh (no fixture): the nodes, with no meshes.
    let empty = tempfile::tempdir().unwrap();
    let broken = wrapper(dir.path().join("broken-aoide"), empty.path(), None);
    let ctx = ctx_for(dir.path(), Some(broken), Some(home)).await;
    assert_eq!(perch::mesh::nodes(&ctx).await, vec![info("osaka", true, &[]), info("yomi", true, &[]), info("bridge", false, &[])]);
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

/// Behind the token like every `/hub` route (and a refusal ran nothing), `GET` only, and no other path below it.
#[test]
fn the_mesh_routes_are_behind_the_token_and_answer_get_only() {
    let rig = Setup::all().start();
    let addr = &rig.perch.addr;
    for path in ["/hub/mesh", "/hub/mesh/", "/hub/mesh/events", "/hub/mesh/events?after=0", "/hub/mesh/nodes"] {
        let bare = Request::new(addr, "GET", path).send(addr);
        assert_eq!((bare.status, bare.body.as_str()), (401, ""), "{path} with no credential: {}", bare.head);
        let wrong = Request::new(addr, "GET", path).bearer("Bearer not-the-token").send(addr);
        assert_eq!((wrong.status, wrong.body.as_str()), (401, ""), "{path} with a wrong token: {}", wrong.head);
    }
    assert!(rig.calls().is_empty(), "a request without the token ran aoide");

    // A POST reaches the module and is a 404 there; any other method is the gate's own 405 before the module is asked.
    for (method, want) in [("POST", 404), ("DELETE", 405)] {
        for path in ["/hub/mesh", "/hub/mesh/events"] {
            let answer = rig.perch.apireq(method, path).send(addr);
            assert_eq!((answer.status, answer.body.as_str()), (want, ""), "{method} {path}: {}", answer.head);
        }
    }
    for path in ["/hub/mesh/nodes", "/hub/mesh/events/x", "/hub/mesh/node-cache", "/hub/meshed"] {
        let answer = rig.perch.get(path);
        assert_eq!((answer.status, answer.body.as_str()), (404, ""), "{path}: {}", answer.head);
    }
    assert_eq!(rig.perch.get("/hub/mesh?x=1").status, 400, "the snapshot takes no query");
    assert!(rig.calls().is_empty(), "none of those ran aoide");
    // And with the token it is a 200 that did: the gate was the only thing in the way.
    assert_eq!(rig.perch.get("/hub/mesh").status, 200);
    assert_eq!(rig.calls().len(), 3);
}
