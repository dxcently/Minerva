//! `/hub/runs`: start, answer, cancel, abandon and the health gate (slice R1a), the log poll and graphs (R1b).
//!
//! Every test starts its own perch with a Minerva home and a plugins folder of its own (temp dirs), so nothing reads or writes the
//! operator's `~/Minerva`. `fake_eidolon`'s `workflow` command is the only thing that answers a run: it replays the journals F0 captured
//! (`tests/fixtures/jev/`) and ends with the exit code and the last stdout line the real door ended with. No model, no jev and no
//! browser take part. The tests of what a perch does with a child (the flow, detaching, adoption, cancel, the wall clock) need the
//! compiled stand-in and so need `--features test-bins`; the ones that only ask and are refused (the gate, the shapes, the health
//! answer) run without it, and none of those can reach a spawn: they start a perch whose `--eidolon` is a path that does not exist.
//!
//! The health tests serve the services' `/health` themselves, each on a loopback port the OS picks, and hand the perch that port through
//! `--jev-health` and `--browser-health`, so two suites at once never ask for the same one. A "down" service is a port that was bound
//! and let go, so nothing listens there. Only the test that never connects keeps literal addresses.
//!
//! Mutations that were run by hand, each by itself, and what stopped it (the tests that failed; every other test of the file passed):
//!
//! ```text
//!   the child without process_group(0)             the detached test, the cancel test, the ignoring-SIGTERM test, the adopted cancel, the wall
//!                                                  clock, two starts at once, abandon, stopped-from-outside and lost-child (9)
//!   the cap check dropped (holder and begin)       the park, the refused resume, the outliving run, two starts at once
//!   a refused resume ended as `refused`            a_refused_resume_leaves_the_run_parked_...
//!   an answer that does not ask the services       a_decision_log_never_stops_a_run_and_a_resume_waits_...
//!   kill_on_drop(true) on the child                the three tests of a run that outlives its perch
//!   a cancel told as eidolon said it               the cancel, the adopted cancel and the ignoring-SIGTERM tests
//!   `--provider mock` left out of the argv         the argv, the park, the detached and the answers tests
//!   the run_ms knob ignored                        the_wall_clock_stops_a_child_that_runs_too_long
//!   a resume that leaves the run `parked`          a_second_answer_while_the_first_is_resuming_..., a_wrong_answer_parks_again_...
//! ```
// What only a run with the stand-in door uses (the helpers of a site, a wait, an answer) is unused without `--features test-bins`.
#![cfg_attr(not(feature = "test-bins"), allow(dead_code))]

mod common;

use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::*;
use serde_json::{json, Value};

/// The member's `config.toml` the home's is generated from: tables a run must carry over unchanged, and a swarm it must turn off.
const MEMBER: &str = "thinking_budget = 4000\n\n[policy]\nenabled = true\nyolo = false\n\n[swarm]\nenabled = true\n";

// ---------------------------------------------------------------------------
// The shared pieces
// ---------------------------------------------------------------------------

fn body(answer: &Answer) -> Value {
    serde_json::from_str(&answer.body).unwrap_or_else(|e| panic!("a JSON body ({e}): {}{}", answer.head, answer.body))
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// `check`, polled until it says `Some`, for `within` at most.
fn until<T>(within: Duration, what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + within;
    loop {
        if let Some(found) = check() {
            return found;
        }
        assert!(Instant::now() < deadline, "gave up waiting for {what} ({within:?})");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A Minerva home and a plugins folder with the one graph a run may name, in a temp tree that lives as long as any perch built on it.
struct Site {
    base: Arc<tempfile::TempDir>,
    home: PathBuf,
    plugins: PathBuf,
}

fn site() -> Site {
    let base = tempfile::tempdir().unwrap();
    let home = base.path().join("Minerva");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(home.join("minerva.toml"), "default_model = \"mock\"\ndefault_mode = \"auto\"\n").unwrap();
    let workflows = base.path().join("plugins").join("jev").join("workflows");
    std::fs::create_dir_all(&workflows).unwrap();
    std::fs::write(workflows.join("wiki-hop.rn"), "// the stand-in door never reads a graph\n").unwrap();
    let (home, plugins) = (home.canonicalize().unwrap(), base.path().join("plugins").canonicalize().unwrap());
    Site { base: Arc::new(base), home, plugins }
}

impl Site {
    /// A perch on this home that can only be refused: its `--eidolon` is not there, so a request that wrongly got past a gate fails
    /// as a 503 with another sentence instead of starting whatever `eidolon` the machine has.
    fn quiet(&self, extra: &[&str]) -> Perch {
        Perch::builder()
            .member_config(MEMBER)
            .arg("--home")
            .arg(&self.home)
            .arg("--plugins-dir")
            .arg(&self.plugins)
            .arg("--eidolon")
            .arg("/nonexistent/never-run-eidolon")
            .args(extra.iter().copied())
            .hold(self.base.clone())
            .start()
    }

    /// A perch on this home with the stand-in door as `--eidolon`, its log, and `env` for every child it starts.
    fn perch(&self, extra: &[&str], env: &[(&str, &str)]) -> Perch {
        let mut builder = Perch::builder()
            .hub()
            .member_config(MEMBER)
            .arg("--home")
            .arg(&self.home)
            .arg("--plugins-dir")
            .arg(&self.plugins)
            .args(extra.iter().copied())
            .hold(self.base.clone());
        for (name, value) in env {
            builder = builder.env(*name, *value);
        }
        builder.start()
    }

    fn dir(&self, rid: &str) -> PathBuf {
        self.home.join("runs").join(rid)
    }

    /// `<home>/runs.json` as it is on disk: `{rid: run}`; `null` when there is none.
    fn store(&self) -> Value {
        std::fs::read_to_string(self.home.join("runs.json")).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or(Value::Null)
    }

    /// The pid `runs.json` holds for a run: the live child.
    fn pid(&self, rid: &str) -> Option<u32> {
        self.store()[rid]["pid"].as_u64().map(|pid| pid as u32)
    }

    /// Every `*.eid` in the run's folder, joined: the journal the stand-in has written so far.
    fn journal(&self, rid: &str) -> String {
        let mut text = String::new();
        for entry in std::fs::read_dir(self.dir(rid)).into_iter().flatten().flatten() {
            if entry.path().extension().is_some_and(|ext| ext == "eid") {
                text.push_str(&std::fs::read_to_string(entry.path()).unwrap_or_default());
            }
        }
        text
    }

    fn records(&self, rid: &str) -> usize {
        self.journal(rid).lines().filter(|line| !line.trim().is_empty()).count()
    }
}

// ---------------------------------------------------------------------------
// The services' /health, stood in for
// ---------------------------------------------------------------------------

enum Answering {
    With(u16, String),
    /// Takes the connection and never says anything.
    Mute,
}

/// A service's `/health` on a loopback port for as long as it is alive; dropping it frees the port.
struct Stub {
    port: u16,
    seen: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Stub {
    /// Answers 200 with `body` on a port the OS picks.
    fn answering(body: &str) -> Self {
        Self::serve(0, Answering::With(200, body.to_string()))
    }

    /// Answers 200 with `body` on `port` again: a service that was down and is back at the address the perch was told.
    fn answering_on(port: u16, body: &str) -> Self {
        Self::serve(port, Answering::With(200, body.to_string()))
    }

    fn failing(status: u16, body: &str) -> Self {
        Self::serve(0, Answering::With(status, body.to_string()))
    }

    fn mute() -> Self {
        Self::serve(0, Answering::Mute)
    }

    /// Binds `port` (0 is one the OS picks) and serves on it, until dropped.
    fn serve(port: u16, how: Answering) -> Self {
        let deadline = Instant::now() + Duration::from_secs(3);
        let listener = loop {
            match TcpListener::bind(("127.0.0.1", port)) {
                Ok(listener) => break listener,
                Err(e) if Instant::now() < deadline => {
                    let _ = e;
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(e) => panic!("127.0.0.1:{port}: {e}"),
            }
        };
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let (seen, stop) = (Arc::new(Mutex::new(Vec::new())), Arc::new(AtomicBool::new(false)));
        let thread = {
            let (seen, stop) = (seen.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut held = Vec::new();
                while !stop.load(Ordering::SeqCst) {
                    let mut sock = match listener.accept() {
                        Ok((sock, _)) => sock,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5));
                            continue;
                        }
                        Err(_) => break,
                    };
                    sock.set_nonblocking(false).unwrap();
                    sock.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                    let (mut head, mut buf) = (Vec::new(), [0u8; 1024]);
                    while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                        match sock.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => head.extend_from_slice(&buf[..n]),
                        }
                    }
                    seen.lock().unwrap().push(String::from_utf8_lossy(&head).to_string());
                    match &how {
                        Answering::With(code, text) => {
                            let wire = format!(
                                "HTTP/1.1 {code} Stub\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                                text.len()
                            );
                            let _ = sock.write_all(wire.as_bytes());
                        }
                        Answering::Mute => held.push(sock),
                    }
                }
            })
        };
        Self { port, seen, stop, thread: Some(thread) }
    }

    /// The request heads it has been sent.
    fn heads(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// What jev's `/health` says (J1), with one field of the stand-in's own that no page should ever be handed.
fn jev_says(decisions_log: &str) -> String {
    json!({
        "status": "ok", "jevlike": true, "openjev": true, "decisions_log": decisions_log, "ckpt": "jev-base-v0.pt", "loaded": true,
        "openjev_dtype": "bfloat16", "secret": "not for the page"
    })
    .to_string()
}

fn browser_says() -> String {
    json!({ "status": "ok", "pid": 4242 }).to_string()
}

/// `POST /hub/runs` for the one graph.
fn start(perch: &Perch, args: Value) -> Answer {
    perch.post_json("/hub/runs", json!({ "graph": "wiki-hop", "args": args }))
}

fn one(perch: &Perch, rid: &str) -> Value {
    let answer = perch.get(&format!("/hub/runs/{rid}"));
    assert_eq!(answer.status, 200, "{}{}", answer.head, answer.body);
    body(&answer)
}

/// The run's detail, polled until its state is `want`.
fn wait_run(perch: &Perch, rid: &str, want: &str, within: Duration) -> Value {
    let mut last = Value::Null;
    let found = until(within, &format!("{rid} to be {want}"), || {
        last = one(perch, rid);
        (last["state"] == want).then(|| last.clone())
    });
    let _ = last;
    found
}

fn answer_to(perch: &Perch, rid: &str, text: &str) -> Answer {
    perch.post_json(&format!("/hub/runs/{rid}/answer"), json!({ "text": text }))
}

fn rows(perch: &Perch) -> Vec<Value> {
    let answer = perch.get("/hub/runs");
    assert_eq!(answer.status, 200, "{}{}", answer.head, answer.body);
    assert_eq!(answer.header("content-type").as_deref(), Some("application/json"), "{}", answer.head);
    body(&answer).as_array().unwrap_or_else(|| panic!("the list is an array: {}", answer.body)).clone()
}

// ---------------------------------------------------------------------------
// The gate and the shapes (no child is ever started here)
// ---------------------------------------------------------------------------

#[test]
fn the_routes_are_behind_the_token_and_the_list_starts_empty() {
    let perch = Perch::start();
    for (method, path) in [
        ("GET", "/hub/runs"),
        ("POST", "/hub/runs"),
        ("GET", "/hub/runs/health"),
        ("GET", "/hub/runs/r1"),
        ("POST", "/hub/runs/r1/answer"),
        ("POST", "/hub/runs/r1/cancel"),
        ("POST", "/hub/runs/r1/abandon"),
    ] {
        let bare = Request::new(&perch.addr, method, path).send(&perch.addr);
        assert_eq!(bare.status, 401, "{method} {path} with no credential: {}", bare.head);
        assert_eq!(bare.body, "", "{method} {path}: the 401 has no body");
    }
    assert_eq!(rows(&perch), Vec::<Value>::new(), "a perch with no runs lists none, home or not");
}

#[test]
fn a_start_with_nowhere_to_run_is_a_503_that_says_which_flag() {
    // No --plugins-dir: there are no graphs.
    let bare = Perch::start();
    let answer = bare.post_json("/hub/runs", json!({ "graph": "wiki-hop" }));
    assert_eq!(answer.status, 503, "{}{}", answer.head, answer.body);
    assert!(body(&answer)["error"].as_str().unwrap().contains("--plugins-dir"), "{}", answer.body);
    // A graph, and no --home: nowhere to keep it.
    let site = site();
    let homeless = Perch::builder().arg("--plugins-dir").arg(&site.plugins).arg("--eidolon").arg("/nonexistent/never-run-eidolon").hold(site.base.clone()).start();
    let answer = homeless.post_json("/hub/runs", json!({ "graph": "wiki-hop" }));
    assert_eq!(answer.status, 503, "{}{}", answer.head, answer.body);
    assert!(body(&answer)["error"].as_str().unwrap().contains("--home"), "{}", answer.body);
    assert_eq!(rows(&homeless), Vec::<Value>::new());
}

#[test]
fn a_request_that_is_not_a_run_is_refused_before_anything_is_made() {
    let site = site();
    let perch = site.quiet(&["--run-graphs", "wiki-hop,missing"]);
    let post = |text: &str| perch.apireq("POST", "/hub/runs").body(text.as_bytes().to_vec()).send(&perch.addr);
    // The body's shape.
    for (text, why) in [
        ("{", "not JSON"),
        ("[]", "an array"),
        ("\"wiki-hop\"", "a string"),
        ("{}", "no graph"),
        ("{\"graph\":7}", "a graph that is not a name"),
        ("{\"graph\":\"wiki-hop\",\"args\":[1]}", "args that are an array"),
        ("{\"graph\":\"wiki-hop\",\"args\":\"x\"}", "args that are a string"),
    ] {
        let answer = post(text);
        assert_eq!(answer.status, 400, "{why}: {}{}", answer.head, answer.body);
    }
    // 64 KiB of args is the most; one byte more is a sentence, and a body past the cap is the 413 every route has.
    let big = |len: usize| json!({ "graph": "wiki-hop", "args": { "x": "a".repeat(len) } }).to_string();
    assert_eq!(post(&big(64 * 1024)).status, 400, "args over 64 KiB");
    assert_eq!(post(&big(100 * 1024)).status, 413, "a body over the cap");
    // A graph is a bare name from --run-graphs whose file exists; everything else is the empty 404.
    for graph in ["other", "../wiki-hop", "wiki-hop/../x", "/etc/passwd", "", "WIKI-HOP", "wiki-hop ", "missing"] {
        let answer = post(&json!({ "graph": graph }).to_string());
        assert_eq!((answer.status, answer.body.as_str()), (404, ""), "graph {graph:?}: {}", answer.head);
    }
    // The methods and paths a run has none of.
    for (method, path, want) in [
        ("PUT", "/hub/runs", 405),
        ("DELETE", "/hub/runs", 405),
        ("POST", "/hub/runs/r1", 405),
        ("GET", "/hub/runs/r1/cancel", 405),
        ("GET", "/hub/runs/r1/answer", 405),
        ("POST", "/hub/runs/health", 405),
        ("GET", "/hub/runs/r1", 404),
        ("GET", "/hub/runs/r1/other", 404),
        ("GET", "/hub/runs/health/x", 404),
        ("GET", "/hub/runs/..", 404),
        ("GET", "/hub/runs/R1", 404),
        ("POST", "/hub/runs/r9/cancel", 404),
        ("POST", "/hub/runs/r9/abandon", 404),
    ] {
        let answer = perch.apireq(method, path).send(&perch.addr);
        assert_eq!(answer.status, want, "{method} {path}: {}{}", answer.head, answer.body);
    }
    let answer = perch.post_json("/hub/runs/r9/answer", json!({ "text": "0" }));
    assert_eq!(answer.status, 404, "an answer to a run nobody started: {}{}", answer.head, answer.body);
    assert_eq!(rows(&perch), Vec::<Value>::new());
    assert!(!site.home.join("runs").exists() && !site.home.join("runs.json").exists(), "nothing was made for a request that was refused");
}

/// A loopback port nothing listens on: bound, read and let go.
fn closed_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0)).unwrap().local_addr().unwrap().port()
}

// ---------------------------------------------------------------------------
// The chat tag (RT): refused when it is not a chat, and when the perch does not know it, before anything is made
// ---------------------------------------------------------------------------

#[test]
fn a_chat_that_is_not_a_chat_id_is_a_400_and_nothing_is_made() {
    let site = site();
    let perch = site.quiet(&["--run-graphs", "wiki-hop"]);
    let long = "a".repeat(129);
    for chat in [json!(7), json!(true), json!({}), json!(""), json!("../x"), json!("a/b"), json!("a\nb"), json!("%2e"), json!(".x"), json!(long)] {
        let answer = perch.post_json("/hub/runs", json!({ "graph": "wiki-hop", "chat": chat }));
        assert_eq!(answer.status, 400, "chat {chat}: {}{}", answer.head, answer.body);
        assert!(body(&answer)["error"].is_string(), "chat {chat}: {}", answer.body);
    }
    // 128 bytes of letters is the most the shape takes: it is refused for being unknown, not for its shape.
    let most = "a".repeat(128);
    let answer = perch.post_json("/hub/runs", json!({ "graph": "wiki-hop", "chat": most }));
    assert_eq!(answer.status, 404, "{}{}", answer.head, answer.body);
    assert_eq!(body(&answer)["error"], "no such chat", "{}", answer.body);
    assert!(!site.home.join("runs").exists() && !site.home.join("runs.json").exists(), "nothing was made for a chat that is refused");
}

#[test]
fn a_chat_the_perch_does_not_know_is_a_404_with_a_sentence_and_nothing_is_made() {
    let site = site();
    // jev is down (a closed port), so a start with no chat is a 503. The unknown chat is refused before the services are asked about.
    let jev = format!("http://127.0.0.1:{}/health", closed_port());
    let perch = site.quiet(&["--run-graphs", "wiki-hop", "--jev-health", jev.as_str()]);
    let answer = perch.post_json("/hub/runs", json!({ "graph": "wiki-hop", "chat": "c0ffee99" }));
    assert_eq!(answer.status, 404, "{}{}", answer.head, answer.body);
    assert_eq!(body(&answer)["error"], "no such chat", "{}", answer.body);
    let plain = perch.post_json("/hub/runs", json!({ "graph": "wiki-hop" }));
    assert_eq!(plain.status, 503, "a start with no chat is asked about jev: {}{}", plain.head, plain.body);
    assert!(!site.home.join("runs").exists() && !site.home.join("runs.json").exists(), "nothing was made for an unknown chat");
}

#[test]
fn the_list_takes_one_chat_and_keeps_fifty_of_its_runs() {
    let site = site();
    // 130 finished runs written before the perch starts: 60 of chat aa, 60 of chat bb, and 10 with no chat.
    let mut runs = serde_json::Map::new();
    for n in 1..=130u64 {
        let mut run = json!({ "graph": "wiki-hop", "state": "done", "started_ms": n, "ended_ms": n + 1, "exit": 0 });
        match n {
            1..=60 => run["chat"] = json!("aa"),
            61..=120 => run["chat"] = json!("bb"),
            _ => {}
        }
        runs.insert(format!("r{n}"), run);
    }
    std::fs::write(site.home.join("runs.json"), Value::Object(runs).to_string()).unwrap();
    let perch = site.quiet(&[]);
    let listed = |path: &str| -> Vec<String> {
        let answer = perch.get(path);
        assert_eq!(answer.status, 200, "{path}: {}{}", answer.head, answer.body);
        body(&answer).as_array().unwrap_or_else(|| panic!("the list is an array: {}", answer.body)).iter().map(|row| row["rid"].as_str().unwrap().to_string()).collect()
    };
    // Newest first, and the chat's own fifty, though 130 runs are kept: a chat's strip is not cut by the other chats' runs.
    let run_range = |from: u64, to: u64| -> Vec<String> { (from..=to).rev().map(|n| format!("r{n}")).collect() };
    let aa = listed("/hub/runs?chat=aa");
    assert_eq!(aa.len(), 50, "{aa:?}");
    assert_eq!((aa[0].as_str(), aa[49].as_str()), ("r60", "r11"));
    assert_eq!(aa, run_range(11, 60));
    assert_eq!(listed("/hub/runs?chat=bb"), run_range(71, 120));
    assert_eq!(listed("/hub/runs"), run_range(81, 130), "no query is every run, as it was");
    assert_eq!(listed("/hub/runs?chat=gone"), Vec::<String>::new(), "a chat with no runs lists none");
}

#[test]
fn a_list_query_other_than_one_chat_is_a_400() {
    let site = site();
    let perch = site.quiet(&[]);
    for query in ["chat=", "chat=a&chat=b", "x=1", "limit=5", "chat=a&x=1", "chat=a%2Fb", "chat=..", "chat=a=b"] {
        let answer = perch.get(&format!("/hub/runs?{query}"));
        assert_eq!(answer.status, 400, "?{query}: {}{}", answer.head, answer.body);
        assert!(body(&answer)["error"].is_string(), "?{query}: {}", answer.body);
    }
    // An empty query is no filter at all.
    assert_eq!(perch.get("/hub/runs?").status, 200);
    assert_eq!(rows(&perch), Vec::<Value>::new());
}

#[test]
fn a_chat_written_into_runs_json_by_hand_is_left_off_the_row() {
    let site = site();
    let long = "x".repeat(200);
    let runs = json!({
        "r1": { "graph": "wiki-hop", "state": "done", "started_ms": 1, "chat": "../etc" },
        "r2": { "graph": "wiki-hop", "state": "done", "started_ms": 2, "chat": "a b" },
        "r3": { "graph": "wiki-hop", "state": "done", "started_ms": 3, "chat": "<b>x</b>" },
        "r4": { "graph": "wiki-hop", "state": "done", "started_ms": 4, "chat": long },
        "r5": { "graph": "wiki-hop", "state": "done", "started_ms": 5, "chat": "ok1" }
    });
    std::fs::write(site.home.join("runs.json"), runs.to_string()).unwrap();
    let perch = site.quiet(&[]);
    let listed = rows(&perch);
    assert_eq!(listed.len(), 5, "{listed:?}");
    for row in &listed {
        let rid = row["rid"].as_str().unwrap();
        let detail = one(&perch, rid);
        if rid == "r5" {
            assert_eq!((row["chat"].as_str(), detail["chat"].as_str()), (Some("ok1"), Some("ok1")), "{row} {detail}");
        } else {
            assert!(row.get("chat").is_none() && detail.get("chat").is_none(), "{rid}: {row} {detail}");
        }
    }
    // The filter reads the same tag: the one run whose tag is an id is the one a chat lists.
    let only = rows_at(&perch, "/hub/runs?chat=ok1");
    assert_eq!(only.iter().map(|row| row["rid"].as_str().unwrap()).collect::<Vec<_>>(), ["r5"]);
}

#[test]
fn a_chat_that_is_not_a_string_is_no_tag_and_one_odd_tag_sets_no_run_aside() {
    let site = site();
    // A number, an object, an array and a bool, each on its own run, and a good tag on a fifth: every run is listed, and runs.json stays.
    let runs = json!({
        "r1": { "graph": "wiki-hop", "state": "done", "started_ms": 1, "chat": 7 },
        "r2": { "graph": "wiki-hop", "state": "done", "started_ms": 2, "chat": { "x": 1 } },
        "r3": { "graph": "wiki-hop", "state": "parked", "started_ms": 3, "chat": [1] },
        "r4": { "graph": "wiki-hop", "state": "done", "started_ms": 4, "chat": true },
        "r5": { "graph": "wiki-hop", "state": "done", "started_ms": 5, "chat": "ok1" }
    });
    std::fs::write(site.home.join("runs.json"), runs.to_string()).unwrap();
    let perch = site.quiet(&[]);
    let listed = rows(&perch);
    assert_eq!(listed.len(), 5, "every run is listed: {listed:?}");
    for row in &listed {
        let rid = row["rid"].as_str().unwrap();
        let detail = one(&perch, rid);
        if rid == "r5" {
            assert_eq!((row["chat"].as_str(), detail["chat"].as_str()), (Some("ok1"), Some("ok1")), "{row} {detail}");
        } else {
            assert!(row.get("chat").is_none() && detail.get("chat").is_none(), "{rid}: {row} {detail}");
        }
    }
    assert_eq!(one(&perch, "r3")["state"], "parked", "a parked run is listed as parked");
    let set_aside = std::fs::read_dir(&site.home).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with("runs.json.bad-"));
    assert!(!set_aside, "no odd tag set runs.json aside");
    assert_eq!(rows_at(&perch, "/hub/runs?chat=ok1").iter().map(|row| row["rid"].as_str().unwrap()).collect::<Vec<_>>(), ["r5"]);
}

#[test]
fn a_tag_of_any_json_shape_loads_and_only_a_chat_id_answers() {
    let site = site();
    // Eleven shapes as the file holds them (serde_json's Value cannot hold 1e400, so the file is written as text): a number past f64's
    // range, a string that is not text, a nesting past 128 levels, and the rest. Every run is listed and runs.json stays where it is; only
    // `ok1` and the escaped `\u0061A` (which is `aA`) answer a chat.
    let deep = "[".repeat(200) + &"]".repeat(200);
    let text = r#"{"r1":{"graph":"wiki-hop","state":"done","started_ms":1,"chat":1e400},"r2":{"graph":"wiki-hop","state":"done","started_ms":2,"chat":-1e400},"r3":{"graph":"wiki-hop","state":"done","started_ms":3,"chat":"\ud800"},"r4":{"graph":"wiki-hop","state":"done","started_ms":4,"chat":@DEEP@},"r5":{"graph":"wiki-hop","state":"done","started_ms":5,"chat":"\u0061A"},"r6":{"graph":"wiki-hop","state":"done","started_ms":6,"chat":"ok1"},"r7":{"graph":"wiki-hop","state":"done","started_ms":7,"chat":7},"r8":{"graph":"wiki-hop","state":"done","started_ms":8,"chat":{"x":1}},"r9":{"graph":"wiki-hop","state":"done","started_ms":9,"chat":[1]},"r10":{"graph":"wiki-hop","state":"done","started_ms":10,"chat":true},"r11":{"graph":"wiki-hop","state":"done","started_ms":11,"chat":null}}"#.replace("@DEEP@", &deep);
    std::fs::write(site.home.join("runs.json"), text).unwrap();
    let perch = site.quiet(&[]);
    let listed = rows(&perch);
    assert_eq!(listed.len(), 11, "every run is listed: {listed:?}");
    for row in &listed {
        let rid = row["rid"].as_str().unwrap();
        let detail = one(&perch, rid);
        let want = match rid {
            "r5" => Some("aA"),
            "r6" => Some("ok1"),
            _ => None,
        };
        assert_eq!((row["chat"].as_str(), detail["chat"].as_str()), (want, want), "{rid}: {row} {detail}");
    }
    let set_aside = std::fs::read_dir(&site.home).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with("runs.json.bad-"));
    assert!(!set_aside, "no tag set runs.json aside");
    assert_eq!(rows_at(&perch, "/hub/runs?chat=aA").iter().map(|row| row["rid"].as_str().unwrap()).collect::<Vec<_>>(), ["r5"]);
    assert_eq!(rows_at(&perch, "/hub/runs?chat=ok1").iter().map(|row| row["rid"].as_str().unwrap()).collect::<Vec<_>>(), ["r6"]);
}

/// `GET <path>` as a list, for a path that carries a query.
fn rows_at(perch: &Perch, path: &str) -> Vec<Value> {
    let answer = perch.get(path);
    assert_eq!(answer.status, 200, "{path}: {}{}", answer.head, answer.body);
    body(&answer).as_array().unwrap_or_else(|| panic!("the list is an array: {}", answer.body)).clone()
}

// ---------------------------------------------------------------------------
// The health answer and the health gate
// ---------------------------------------------------------------------------

/// The `--jev-health` and `--browser-health` flags for these two loopback ports: a stub's, or ports nothing listens on.
fn health_flags(jev: u16, browser: u16) -> Vec<String> {
    let (jev, browser) = (format!("http://127.0.0.1:{jev}/health"), format!("http://127.0.0.1:{browser}/health"));
    vec!["--jev-health".into(), jev, "--browser-health".into(), browser]
}

/// `flags` as the `&str`s that a perch's arguments take.
fn refs(flags: &[String]) -> Vec<&str> {
    flags.iter().map(String::as_str).collect()
}

/// Two loopback ports nothing listens on: both are bound at once, so they differ, and then let go.
fn closed_ports() -> (u16, u16) {
    let (first, second) = (TcpListener::bind(("127.0.0.1", 0)).unwrap(), TcpListener::bind(("127.0.0.1", 0)).unwrap());
    (first.local_addr().unwrap().port(), second.local_addr().unwrap().port())
}

fn health(perch: &Perch) -> Value {
    let answer = perch.get("/hub/runs/health");
    assert_eq!(answer.status, 200, "{}{}", answer.head, answer.body);
    assert_eq!(answer.header("content-type").as_deref(), Some("application/json"), "{}", answer.head);
    body(&answer)
}

const START: &str = "minerva services start";

#[test]
fn health_says_what_each_service_says_of_itself_and_nothing_more() {
    let jev = Stub::answering(&jev_says("off"));
    let browser = Stub::answering(&browser_says());
    let site = site();
    let perch = site.quiet(&refs(&health_flags(jev.port, browser.port)));
    let said = health(&perch);
    // J1's seven fields ride along as jev said them; its own extra one does not, and neither does the browser's pid.
    assert_eq!(
        said["jev"],
        json!({
            "configured": true, "up": true, "start": START, "status": "ok", "jevlike": true, "openjev": true, "decisions_log": "off",
            "ckpt": "jev-base-v0.pt", "loaded": true, "openjev_dtype": "bfloat16"
        })
    );
    assert_eq!(said["browser"], json!({ "configured": true, "up": true, "start": START, "status": "ok" }));
    assert!(said["at_ms"].as_u64().unwrap() > 1_700_000_000_000, "{said}");
    // A second tab asking costs nothing: both answers are cached for a few seconds.
    assert_eq!(health(&perch)["jev"], said["jev"]);
    assert_eq!((jev.heads().len(), browser.heads().len()), (1, 1), "two reads, one probe each: {:?} {:?}", jev.heads(), browser.heads());
    // The probe is a plain GET of the path it was given and carries no credential of the perch's.
    let head = jev.heads()[0].to_ascii_lowercase();
    assert!(head.starts_with("get /health http/1.1\r\n") && head.contains(&format!("host: 127.0.0.1:{}", jev.port)), "{head:?}");
    assert!(!head.contains("authorization") && !head.contains("cookie") && !head.contains(&perch.token.trim().to_ascii_lowercase()), "{head:?}");
}

#[test]
fn a_decision_log_path_rides_along_and_an_old_jev_just_lacks_the_fields() {
    {
        let (jev, browser) = (Stub::answering(&jev_says("/var/lib/jev/decisions.ndjson")), Stub::answering(&browser_says()));
        let site = site();
        let said = health(&site.quiet(&refs(&health_flags(jev.port, browser.port))));
        assert_eq!(said["jev"]["up"], true, "{said}");
        assert_eq!(said["jev"]["decisions_log"], "/var/lib/jev/decisions.ndjson", "the page shows it; the perch does not judge it: {said}");
    }
    // A jev from before J1 has `status` and nothing else: the page says unknown for the rest, so the keys are absent, not null.
    let (jev, browser) = (Stub::answering("{\"status\":\"ok\"}"), Stub::answering(&browser_says()));
    let site = site();
    let said = health(&site.quiet(&refs(&health_flags(jev.port, browser.port))));
    assert_eq!(said["jev"], json!({ "configured": true, "up": true, "start": START, "status": "ok" }));
}

#[test]
fn a_service_that_is_down_says_why_and_how_to_start_it() {
    let site = site();
    let (jev, browser) = closed_ports();
    let said = health(&site.quiet(&refs(&health_flags(jev, browser))));
    for service in ["jev", "browser"] {
        assert_eq!(said[service], json!({ "configured": true, "up": false, "start": START, "why": "connection refused" }), "{service}: {said}");
    }
}

#[test]
fn what_a_service_answers_that_is_not_a_health_is_down_with_the_reason() {
    let (jev, browser) = (Stub::failing(500, "oops"), Stub::answering("this is not json"));
    let site = site();
    let said = health(&site.quiet(&refs(&health_flags(jev.port, browser.port))));
    assert_eq!((said["jev"]["up"].as_bool(), said["jev"]["why"].as_str()), (Some(false), Some("answered 500")), "{said}");
    assert_eq!((said["browser"]["up"].as_bool(), said["browser"]["why"].as_str()), (Some(false), Some("the answer is not a JSON object")), "{said}");
}

#[test]
fn a_service_that_does_not_answer_is_down_within_about_a_second() {
    let (jev, browser) = (Stub::mute(), Stub::answering(&browser_says()));
    let site = site();
    let perch = site.quiet(&refs(&health_flags(jev.port, browser.port)));
    let asked = Instant::now();
    let said = health(&perch);
    assert!(asked.elapsed() < Duration::from_secs(3), "a probe is bounded: {:?}", asked.elapsed());
    assert_eq!((said["jev"]["up"].as_bool(), said["jev"]["why"].as_str()), (Some(false), Some("did not answer in time")), "{said}");
    assert_eq!(said["browser"]["up"], true, "a dead jev costs the browser nothing: {said}");
}

#[test]
fn a_health_address_that_is_not_loopback_is_not_connected_to() {
    let site = site();
    let perch = site.quiet(&["--jev-health", "http://10.255.255.1:48140/health", "--browser-health", "http://example.invalid:48141/health"]);
    let asked = Instant::now();
    let said = health(&perch);
    assert!(asked.elapsed() < Duration::from_millis(900), "refused as a probe, not tried: {:?}", asked.elapsed());
    for service in ["jev", "browser"] {
        assert_eq!(said[service]["up"], false, "{said}");
        assert_eq!(said[service]["why"], "its health address is not a loopback http address", "{said}");
    }
    let answer = start(&perch, json!({}));
    assert_eq!(answer.status, 503, "{}{}", answer.head, answer.body);
    assert!(body(&answer)["error"].as_str().unwrap().contains("its health address is not a loopback http address"), "{}", answer.body);
}

#[test]
fn no_health_flag_is_nothing_to_wait_for() {
    let site = site();
    let said = health(&site.quiet(&[]));
    for service in ["jev", "browser"] {
        assert_eq!(said[service], json!({ "configured": false, "up": false, "start": START }), "{service}: {said}");
    }
}

#[test]
fn a_start_against_a_service_that_is_down_is_a_503_that_names_how_to_start_it() {
    let site = site();
    // Both ports are free until a stub takes one: the perch is told these two addresses once, for the whole test.
    let (jev_port, browser_port) = closed_ports();
    let jev_down = "jev is not running (connection refused); start it with: minerva services start";
    let browser_down = "the browser service is not running (connection refused); start it with: minerva services start";
    let refused = |perch: &Perch| -> String {
        let answer = start(perch, json!({ "start": "Cat", "goal": "Felidae" }));
        assert_eq!(answer.status, 503, "{}{}", answer.head, answer.body);
        let said = body(&answer);
        assert_eq!(said["error"], said["why"], "one sentence under both names: {said}");
        said["error"].as_str().unwrap().to_string()
    };
    let perch = site.quiet(&refs(&health_flags(jev_port, browser_port)));
    // jev down, the browser up.
    let browser = Stub::answering_on(browser_port, &browser_says());
    assert_eq!(refused(&perch), jev_down);
    // The other way round.
    drop(browser);
    let jev = Stub::answering_on(jev_port, &jev_says("off"));
    assert_eq!(refused(&perch), browser_down);
    // Both: each sentence, joined, so one answer says everything that has to be started.
    drop(jev);
    assert_eq!(refused(&perch), format!("{jev_down} · {browser_down}"));
    // Nothing was begun by any of them: no row, no folder, no file.
    assert_eq!(rows(&perch), Vec::<Value>::new());
    assert!(!site.home.join("runs").exists() && !site.home.join("runs.json").exists(), "a refused start leaves nothing behind");
}

#[test]
fn a_start_asks_the_services_again_instead_of_trusting_the_last_answer() {
    let jev = Stub::answering(&jev_says("off"));
    let browser = Stub::answering(&browser_says());
    let site = site();
    let perch = site.quiet(&refs(&health_flags(jev.port, browser.port)));
    assert_eq!(health(&perch)["jev"]["up"], true, "the page has just seen it up, and that answer is cached for five seconds");
    drop(jev);
    let answer = start(&perch, json!({}));
    assert_eq!(answer.status, 503, "a stale up must not start a run against a jev that has gone: {}{}", answer.head, answer.body);
    assert_eq!(health(&perch)["jev"]["up"], false, "what the gate saw is what the next poll shows");
}

// ---------------------------------------------------------------------------
// The flow: a perch, a stand-in door, and the runs it replays
// ---------------------------------------------------------------------------

#[cfg(feature = "test-bins")]
mod flow {
    use super::*;

    /// A case that is held until SIGTERM (d, l) is held at most this long, so a child a failing test leaves behind goes by itself.
    const HOLD: (&str, &str) = ("FAKE_WORKFLOW_HOLD_MS", "30000");

    /// What a test that ends with a child alive (or fails with one) leaves behind: every pid `runs.json` still names that is still a
    /// workflow, and its group, is killed. Declared before the perch, so it runs after the perch is gone.
    struct Reaper(PathBuf);

    impl Drop for Reaper {
        fn drop(&mut self) {
            let Ok(text) = std::fs::read_to_string(self.0.join("runs.json")) else { return };
            let Ok(Value::Object(runs)) = serde_json::from_str::<Value>(&text) else { return };
            for run in runs.values() {
                let Some(pid) = run["pid"].as_u64() else { continue };
                let argv = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
                if argv.windows(8).any(|word| word == b"workflow") {
                    kill_group(pid as u32, libc::SIGKILL);
                }
            }
        }
    }

    fn kill_group(pid: u32, signal: i32) {
        unsafe { libc::killpg(pid as libc::pid_t, signal) };
    }

    /// Gone, or a zombie nobody has reaped yet: either way it runs nothing. The orphans of a perch that was stopped are init's to reap.
    fn dead(pid: u32) -> bool {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(_) => true,
            Ok(stat) => stat.rsplit_once(')').is_some_and(|(_, rest)| rest.trim_start().starts_with('Z')),
        }
    }

    fn wait_dead(pid: u32, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline && !dead(pid) {
            std::thread::sleep(Duration::from_millis(20));
        }
        dead(pid)
    }

    /// The process group in `/proc/<pid>/stat`.
    fn pgrp(pid: u32) -> u32 {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
        stat.rsplit_once(')').unwrap().1.split_whitespace().nth(2).unwrap().parse().unwrap()
    }

    /// The case file a test writes the letter of the next run into: `FAKE_WORKFLOW_CASE` is read at every start.
    fn cases(site: &Site, first: &str) -> (PathBuf, String) {
        let file = site.base.path().join("case");
        std::fs::write(&file, first).unwrap();
        let named = file.to_str().unwrap().to_string();
        (file, named)
    }

    fn pick(case: &Path, letter: &str) {
        std::fs::write(case, letter).unwrap();
    }

    fn started(perch: &Perch, args: Value) -> String {
        let answer = start(perch, args);
        assert_eq!(answer.status, 202, "{}{}", answer.head, answer.body);
        body(&answer)["rid"].as_str().unwrap().to_string()
    }

    /// The perch's own record of the child it has: set before the 202 is answered.
    fn pid_of(site: &Site, rid: &str) -> u32 {
        until(Duration::from_secs(5), "the child's pid in runs.json", || site.pid(rid))
    }

    /// Waits for the stand-in to have written a record that says `needle`: where a held case is, or how far a slow one has got.
    fn journaled(site: &Site, rid: &str, needle: &str) {
        until(Duration::from_secs(15), &format!("{needle:?} in the journal of {rid}"), || site.journal(rid).contains(needle).then_some(()));
    }

    /// A held case (d, l) has written its 24th record, an `ApiRequest`, and waits there for SIGTERM: from here a signal is the one F0 sent.
    fn held(site: &Site, rid: &str) {
        until(Duration::from_secs(15), &format!("{rid} to be held at its tool"), || (site.records(rid) >= 24).then_some(()));
    }

    fn workflow_lines(perch: &Perch, verb: &str) -> Vec<String> {
        perch.fake_records().into_iter().filter(|line| line.starts_with(&format!("workflow {verb} "))).collect()
    }

    fn cancel(perch: &Perch, rid: &str) -> Answer {
        perch.post(&format!("/hub/runs/{rid}/cancel"))
    }

    #[test]
    fn a_run_is_the_workflow_door_on_the_mock_with_its_args_on_stdin_and_ends_done() {
        let site = site();
        let _reaper = Reaper(site.home.clone());
        let perch = site.perch(&[], &[("FAKE_WORKFLOW_CASE", "a")]);
        let args = json!({ "start": "Ancient_Egypt", "goal": "Felidae" });
        let answer = start(&perch, args.clone());
        assert_eq!(answer.status, 202, "{}{}", answer.head, answer.body);
        assert_eq!(answer.header("content-type").as_deref(), Some("application/json"));
        assert_eq!(body(&answer), json!({ "rid": "r1", "state": "running" }));
        let done = wait_run(&perch, "r1", "done", Duration::from_secs(15));
        assert_eq!((done["graph"].as_str(), done["exit"].as_i64(), done["resumes"].as_u64()), (Some("wiki-hop"), Some(0), Some(0)), "{done}");
        assert_eq!(done["args"], args, "the args the page sent are on the row");
        assert!(done["report"].as_str().unwrap().contains("arrived"), "{done}");
        assert!(done.get("question").is_none() && done.get("why").is_none() && done.get("stopping").is_none(), "{done}");
        assert!(done["ended_ms"].as_u64().unwrap() >= done["started_ms"].as_u64().unwrap(), "{done}");
        // What the stand-in was asked: the workflow door, the jev folder, the graph, the args on stdin, the mock provider, a config.
        let calls = workflow_lines(&perch, "run");
        assert_eq!(calls, [format!("workflow run {}/jev wiki-hop --args - --provider mock config=true", site.plugins.display())]);
        // The run's own config: the home's with three keys changed, and [policy] exactly as the member has it.
        let dir = site.dir("r1");
        let config: toml::Table = std::fs::read_to_string(dir.join("config.toml")).unwrap().parse().unwrap();
        let home_config: toml::Table = std::fs::read_to_string(site.home.join("config.toml")).unwrap().parse().unwrap();
        assert_eq!(config["sessions_dir"].as_str(), Some(dir.to_str().unwrap()));
        assert_eq!(config["default_model"].as_str(), Some("mock"));
        assert_eq!(config["swarm"]["enabled"].as_bool(), Some(false));
        assert_eq!(home_config["swarm"]["enabled"].as_bool(), Some(true), "the home's own config is not touched");
        assert_eq!(config["policy"], home_config["policy"], "[policy] is the member's, so the run shares the permits store");
        assert_eq!(config["thinking_budget"], home_config["thinking_budget"]);
        assert_ne!(config["sessions_dir"], home_config["sessions_dir"], "the run's journal is in the run's folder, not among the chats");
        let sent: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("args.json")).unwrap()).unwrap();
        assert_eq!(sent, args);
        // Everything the perch wrote for a run is private.
        assert_eq!(mode(&site.home.join("runs")), 0o700);
        assert_eq!(mode(&dir), 0o700);
        for file in ["config.toml", "args.json", "out.log", "err.log"] {
            assert_eq!(mode(&dir.join(file)), 0o600, "{file}");
        }
        assert_eq!(mode(&site.home.join("runs.json")), 0o600);
        // The journal is in the run's folder, and the door's last stdout line names it.
        let journals: Vec<PathBuf> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "eid")).collect();
        assert_eq!(journals.len(), 1, "{journals:?}");
        let out = std::fs::read_to_string(dir.join("out.log")).unwrap();
        let door: Value = serde_json::from_str(out.lines().last().unwrap()).unwrap();
        assert_eq!((door["status"].as_str(), door["session"].as_str()), (Some("completed"), journals[0].to_str()), "{out}");
        assert!(perch.list().is_empty(), "a run is not a chat: nothing was added to the sessions the page lists");
        // And the list is the same run, without the tails.
        let listed = rows(&perch);
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert_eq!((listed[0]["rid"].as_str(), listed[0]["state"].as_str()), (Some("r1"), Some("done")));
        assert!(listed[0].get("out").is_none() && listed[0].get("report").is_none(), "{}", listed[0]);
    }

    #[test]
    fn every_ending_the_door_can_report_is_told_as_it_is() {
        let site = site();
        let _reaper = Reaper(site.home.clone());
        let (case, named) = cases(&site, "a");
        let perch = site.perch(&[], &[("FAKE_WORKFLOW_CASE", &named)]);
        // e is a graph whose own final state is `failed`, which the door still reports as completed: the run is done, and says so.
        for (letter, rid) in [("a", "r1"), ("e", "r2"), ("f", "r3")] {
            pick(&case, letter);
            assert_eq!(started(&perch, json!({})), rid);
            let done = wait_run(&perch, rid, "done", Duration::from_secs(15));
            assert_eq!(done["exit"], 0, "case {letter}: {done}");
            assert!(done["report"].as_str().is_some_and(|report| report.contains("final_state")), "case {letter}: {done}");
        }
        assert_eq!(rows(&perch).iter().map(|row| row["rid"].as_str().unwrap().to_string()).collect::<Vec<_>>(), ["r3", "r2", "r1"], "newest first");
    }

    #[test]
    fn a_park_waits_for_an_answer_and_an_answer_finishes_the_run() {
        let site = site();
        let _reaper = Reaper(site.home.clone());
        let perch = site.perch(&[], &[("FAKE_WORKFLOW_CASE", "b")]);
        assert_eq!(started(&perch, json!({ "start": "Ancient_Egypt", "goal": "Felidae" })), "r1");
        let parked = wait_run(&perch, "r1", "parked", Duration::from_secs(15));
        assert!(parked["question"].as_str().unwrap().contains("Which link leads toward the article"), "{parked}");
        assert_eq!(parked["exit"], 1, "{parked}");
        assert!(parked.get("report").is_none() && parked.get("refused").is_none() && parked.get("ended_ms").is_none(), "a park is not an end: {parked}");
        assert!(parked["out"].as_str().unwrap().contains("\"status\":\"parked\""), "the tail of what the door printed: {parked}");
        assert_eq!(site.pid("r1"), None, "a parked run has no process, only a journal");
        // It holds the one browser: another start is a 409 that names it, and no second child was started.
        let busy = start(&perch, json!({}));
        assert_eq!(busy.status, 409, "{}{}", busy.head, busy.body);
        assert_eq!(body(&busy)["run"], "r1");
        assert!(body(&busy)["error"].as_str().unwrap().contains("r1"), "{}", busy.body);
        assert_eq!(workflow_lines(&perch, "run").len(), 1);
        // The answer resumes the same run: the same folder, the journal the first child left, and the answer as one argument.
        let answered = answer_to(&perch, "r1", "0");
        assert_eq!(answered.status, 202, "{}{}", answered.head, answered.body);
        assert_eq!(body(&answered), json!({ "rid": "r1", "state": "running" }));
        let done = wait_run(&perch, "r1", "done", Duration::from_secs(15));
        assert_eq!((done["resumes"].as_u64(), done["exit"].as_i64()), (Some(1), Some(0)), "{done}");
        assert!(done["report"].as_str().unwrap().contains("arrived") && done.get("question").is_none(), "{done}");
        let dir = site.dir("r1");
        let resumes = workflow_lines(&perch, "resume");
        assert_eq!(resumes.len(), 1, "{resumes:?}");
        assert!(resumes[0].starts_with(&format!("workflow resume --plugin {}/jev {}/", site.plugins.display(), dir.display())), "{}", resumes[0]);
        assert!(resumes[0].ends_with(".eid 1 --answer=0 --provider mock config=true"), "{}", resumes[0]);
        assert!(site.journal("r1").contains("\"answer\":\"0\""), "the journal says what was answered");
        // The run has ended, and the next one is free to start.
        assert_eq!(started(&perch, json!({})), "r2");
        wait_run(&perch, "r2", "parked", Duration::from_secs(15));
        assert_eq!(answer_to(&perch, "r1", "0").status, 409, "an answer to a run that is done");
    }

    #[test]
    fn an_answer_that_is_not_an_answer_is_refused_and_the_run_stays_parked() {
        let site = site();
        let _reaper = Reaper(site.home.clone());
        let perch = site.perch(&[], &[("FAKE_WORKFLOW_CASE", "b")]);
        started(&perch, json!({}));
        wait_run(&perch, "r1", "parked", Duration::from_secs(15));
        let too_long = "x".repeat(1025);
        for text in ["", "   ", "0\n", "a\tb", "a\u{1b}[31m", "a\u{0}b", too_long.as_str()] {
            let answer = answer_to(&perch, "r1", text);
            assert_eq!(answer.status, 400, "{text:?}: {}{}", answer.head, answer.body);
            assert!(body(&answer)["error"].is_string(), "a sentence a person can act on: {}", answer.body);
        }
        for raw in ["{", "[]", "{}", "{\"text\":5}", "{\"text\":null}", "{\"answer\":\"0\"}"] {
            let answer = perch.apireq("POST", "/hub/runs/r1/answer").body(raw.as_bytes().to_vec()).send(&perch.addr);
            assert_eq!(answer.status, 400, "{raw}: {}{}", answer.head, answer.body);
        }
        // Not one of them started a child: the run is as it was, and still waiting.
        assert!(workflow_lines(&perch, "resume").is_empty(), "{:?}", perch.fake_records());
        let still = one(&perch, "r1");
        assert_eq!((still["state"].as_str(), still["resumes"].as_u64()), (Some("parked"), Some(0)), "{still}");
        // The longest answer is 1 KiB, and it is the answer.
        assert_eq!(answer_to(&perch, "r1", &"x".repeat(1024)).status, 202);
        wait_run(&perch, "r1", "done", Duration::from_secs(15));
        let late = answer_to(&perch, "r1", "0");
        assert_eq!(late.status, 409, "{}{}", late.head, late.body);
        assert_eq!(body(&late)["state"], "done");
    }

    #[test]
    fn a_wrong_answer_parks_again_and_a_label_or_a_dash_is_still_one_argument() {
        let site = site();
        let _reaper = Reaper(site.home.clone());
        let (case, named) = cases(&site, "h");
        let perch = site.perch(&[], &[("FAKE_WORKFLOW_CASE", &named)]);
        // h: `banana` is not an option, so the graph asks again; `0` then settles it.
        started(&perch, json!({}));
        wait_run(&perch, "r1", "parked", Duration::from_secs(15));
        assert_eq!(answer_to(&perch, "r1", "banana").status, 202);
        let again = wait_run(&perch, "r1", "parked", Duration::from_secs(15));
        assert_eq!((again["resumes"].as_u64(), again["exit"].as_i64()), (Some(1), Some(1)), "parked again, by the door's own word: {again}");
        assert!(again["question"].is_string(), "{again}");
        assert_eq!(answer_to(&perch, "r1", "0").status, 202);
        let done = wait_run(&perch, "r1", "done", Duration::from_secs(15));
        assert_eq!(done["resumes"], 2, "{done}");
        // i: the answer is an option's label, spaces and all.
        pick(&case, "i");
        assert_eq!(started(&perch, json!({})), "r2");
        wait_run(&perch, "r2", "parked", Duration::from_secs(15));
        assert_eq!(answer_to(&perch, "r2", "Registered cat breed number 002").status, 202);
        wait_run(&perch, "r2", "done", Duration::from_secs(15));
        // b: the answer starts with a dash, which a flag parser would take for a flag if it were its own argument.
        pick(&case, "b");
        assert_eq!(started(&perch, json!({})), "r3");
        wait_run(&perch, "r3", "parked", Duration::from_secs(15));
        assert_eq!(answer_to(&perch, "r3", "-1").status, 202);
        wait_run(&perch, "r3", "done", Duration::from_secs(15));
        let answers: Vec<String> = workflow_lines(&perch, "resume").iter().map(|line| line.split(" --answer=").nth(1).unwrap().to_string()).collect();
        assert_eq!(
            answers,
            ["banana --provider mock config=true", "0 --provider mock config=true", "Registered cat breed number 002 --provider mock config=true", "-1 --provider mock config=true"]
        );
        assert!(site.journal("r3").contains("\"answer\":\"-1\""), "the stand-in took `-1` as the answer, so it reached it as one argument");
    }

    #[test]
    fn a_second_answer_while_the_first_is_resuming_is_a_409_and_two_at_once_make_one_resume() {
        let site = site();
        let _reaper = Reaper(site.home.clone());
        // A slow resume (200 ms a step), so the run is still going when the second answer comes.
        let perch = site.perch(&[], &[("FAKE_WORKFLOW_CASE", "b"), ("FAKE_WORKFLOW_STEP_MS", "200")]);
        started(&perch, json!({}));
        wait_run(&perch, "r1", "parked", Duration::from_secs(20));
        assert_eq!(answer_to(&perch, "r1", "0").status, 202);
        let second = answer_to(&perch, "r1", "0");
        assert_eq!(second.status, 409, "{}{}", second.head, second.body);
        assert_eq!(body(&second)["state"], "running", "a resumed run is running from the moment the 202 was answered: {}", second.body);
        assert_eq!(one(&perch, "r1")["resumes"], 1);
        wait_run(&perch, "r1", "done", Duration::from_secs(30));
        assert_eq!(workflow_lines(&perch, "resume").len(), 1, "one child resumed it");
        // The same park, answered by two requests at the same moment: one resumes it, the other is told.
        started(&perch, json!({}));
        wait_run(&perch, "r2", "parked", Duration::from_secs(20));
        let (addr, auth) = (perch.addr.clone(), perch.auth());
        let gate = std::sync::Barrier::new(4);
        let mut statuses: Vec<u16> = std::thread::scope(|scope| {
            let racers: Vec<_> = (0..4)
                .map(|_| {
                    scope.spawn(|| {
                        let request = Request::new(&addr, "POST", "/hub/runs/r2/answer").bearer(auth.clone()).body(b"{\"text\":\"0\"}".to_vec());
                        gate.wait();
                        request.send(&addr).status
                    })
                })
                .collect();
            racers.into_iter().map(|racer| racer.join().unwrap()).collect()
        });
        statuses.sort_unstable();
        assert_eq!(statuses, [202, 409, 409, 409]);
        wait_run(&perch, "r2", "done", Duration::from_secs(30));
        assert_eq!(workflow_lines(&perch, "resume").len(), 2, "one resume for each park, not one for each request");
    }

    #[test]
    fn a_refused_resume_leaves_the_run_parked_with_the_sentence_and_abandon_ends_it() {
        let site = site();
        let _reaper = Reaper(site.home.clone());
        let perch = site.perch(&[], &[("FAKE_WORKFLOW_CASE", "c")]);
        started(&perch, json!({}));
        let parked = wait_run(&perch, "r1", "parked", Duration::from_secs(15));
        let question = parked["question"].as_str().unwrap().to_string();
        let records = site.records("r1");
        // c: eidolon refuses the resume, naming the step it cannot replay (exit 3), and the journal is untouched.
        assert_eq!(answer_to(&perch, "r1", "0").status, 202);
        let refused = until(Duration::from_secs(15), "the refusal", || {
            let run = one(&perch, "r1");
            (run["state"] == "parked" && run["resumes"] == 1 && run.get("refused").is_some()).then_some(run)
        });
        assert!(refused["refused"].as_str().unwrap().contains("jev_entail"), "eidolon's sentence, as it said it: {refused}");
        assert_eq!((refused["exit"].as_i64(), refused["question"].as_str()), (Some(3), Some(question.as_str())), "still parked on the same question: {refused}");
        assert_eq!(site.records("r1"), records, "a refused resume writes nothing");
        // It is still a park: it holds the browser, and asking again is allowed (it is refused again, the same way).
        assert_eq!(start(&perch, json!({})).status, 409);
        assert_eq!(answer_to(&perch, "r1", "0").status, 202);
        until(Duration::from_secs(15), "the second refusal", || {
            let run = one(&perch, "r1");
            (run["state"] == "parked" && run["resumes"] == 2 && run.get("refused").is_some()).then_some(())
        });
        // Abandon is how it ends.
        let abandoned = perch.post("/hub/runs/r1/abandon");
        assert_eq!(abandoned.status, 200, "{}{}", abandoned.head, abandoned.body);
        let gone = body(&abandoned);
        assert_eq!(gone["state"], "abandoned");
        assert!(gone["ended_ms"].is_u64() && gone.get("refused").is_none(), "{gone}");
        assert_eq!(started(&perch, json!({})), "r2", "the browser is free again");
    }

    #[test]
    fn abandon_ends_a_park_without_starting_anything_and_only_a_park() {
        let site = site();
        let _reaper = Reaper(site.home.clone());
        let (case, named) = cases(&site, "b");
        let perch = site.perch(&[], &[("FAKE_WORKFLOW_CASE", &named), HOLD]);
        started(&perch, json!({}));
        wait_run(&perch, "r1", "parked", Duration::from_secs(15));
        let abandoned = perch.post("/hub/runs/r1/abandon");
        assert_eq!(abandoned.status, 200, "{}{}", abandoned.head, abandoned.body);
        assert_eq!(body(&abandoned)["state"], "abandoned");
        assert!(workflow_lines(&perch, "resume").is_empty() && workflow_lines(&perch, "run").len() == 1, "no child was started to end it");
        assert_eq!(answer_to(&perch, "r1", "0").status, 409, "an abandoned run is not waiting for an answer");
        let again = perch.post("/hub/runs/r1/abandon");
        assert_eq!(again.status, 409, "{}{}", again.head, again.body);
        assert_eq!(body(&again)["state"], "abandoned");
        assert_eq!(cancel(&perch, "r1").status, 409, "and it has no process to cancel");
        // A run that is done is not a park, and one that is running is not either.
        pick(&case, "a");
        started(&perch, json!({}));
        wait_run(&perch, "r2", "done", Duration::from_secs(15));
        assert_eq!(perch.post("/hub/runs/r2/abandon").status, 409);
        pick(&case, "d");
        started(&perch, json!({}));
        held(&site, "r3");
        let running = perch.post("/hub/runs/r3/abandon");
        assert_eq!(running.status, 409, "{}{}", running.head, running.body);
        assert_eq!(body(&running)["state"], "running");
        assert_eq!(answer_to(&perch, "r3", "0").status, 409, "a run that is going is not waiting either");
        assert_eq!(cancel(&perch, "r3").status, 200);
    }

    #[test]
    fn a_cancel_stops_the_group_and_the_run_is_cancelled_whatever_eidolon_said_on_the_way_out() {
        let site = site();
        let _reaper = Reaper(site.home.clone());
        let (case, named) = cases(&site, "d");
        let perch = site.perch(&[], &[("FAKE_WORKFLOW_CASE", &named), HOLD]);
        // d: SIGTERM and eidolon's token wins: the door says `failed`. l: the program wins: the door says `parked`.
        for (letter, rid, door, exit) in [("d", "r1", "failed", 2), ("l", "r2", "parked", 1)] {
            pick(&case, letter);
            assert_eq!(started(&perch, json!({})), rid);
            held(&site, rid);
            let pid = pid_of(&site, rid);
            let cancelled = cancel(&perch, rid);
            assert_eq!(cancelled.status, 200, "{}{}", cancelled.head, cancelled.body);
            let run = body(&cancelled);
            assert_eq!((run["state"].as_str(), run["exit"].as_i64()), (Some("cancelled"), Some(exit)), "case {letter}: {run}");
            assert!(run["out"].as_str().unwrap().contains(&format!("\"status\":\"{door}\"")), "case {letter}: the door did say {door}: {run}");
            assert!(run.get("question").is_none() && run.get("why").is_none() && run.get("stopping").is_none() && run["ended_ms"].is_u64(), "{run}");
            assert!(is_gone(pid), "the child was reaped");
            assert!(site.journal(rid).contains("WorkflowEnd"), "it was given the signal and the time to write its end");
            assert_eq!(site.pid(rid), None);
        }
        // Cancelled is an end: it frees the browser and cannot be cancelled again.
        let twice = cancel(&perch, "r2");
        assert_eq!(twice.status, 409, "{}{}", twice.head, twice.body);
        assert!(body(&twice)["error"].as_str().unwrap().contains("cancelled already"), "{}", twice.body);
        assert_eq!(started(&perch, json!({})), "r3");
        assert_eq!(cancel(&perch, "r3").status, 200);
        // A park has no process to cancel: the sentence says to abandon it.
        pick(&case, "b");
        started(&perch, json!({}));
        wait_run(&perch, "r4", "parked", Duration::from_secs(15));
        let parked = cancel(&perch, "r4");
        assert_eq!(parked.status, 409, "{}{}", parked.head, parked.body);
        assert!(body(&parked)["error"].as_str().unwrap().contains("abandon it"), "{}", parked.body);
    }

    #[test]
    fn a_child_that_ignores_sigterm_is_killed_after_the_grace_and_so_is_everything_in_its_group() {
        let site = site();
        let _reaper = Reaper(site.home.clone());
        let perch = site.perch(&[], &[("FAKE_WORKFLOW_CASE", "d"), ("FAKE_WORKFLOW_DEAF", "1"), HOLD]);
        started(&perch, json!({}));
        held(&site, "r1");
        let pid = pid_of(&site, "r1");
        let sleeper: u32 = until(Duration::from_secs(5), "the sleeper in the child's group", || {
            perch.fake_records().iter().find_map(|line| line.strip_prefix("workflow-child ").and_then(|pid| pid.trim().parse().ok()))
        });
        assert!(!dead(sleeper) && pgrp(sleeper) == pid, "the sleeper is in the child's own group");
        let asked = Instant::now();
        let cancelled = cancel(&perch, "r1");
        assert_eq!(cancelled.status, 200, "{}{}", cancelled.head, cancelled.body);
        assert!(asked.elapsed() >= Duration::from_secs(4), "SIGTERM is given its five seconds first: {:?}", asked.elapsed());
        let run = body(&cancelled);
        assert_eq!((run["state"].as_str(), run["exit"].as_i64()), (Some("cancelled"), Some(137)), "killed, and told as the cancel it was: {run}");
        assert!(is_gone(pid) && wait_dead(sleeper, Duration::from_secs(5)), "the group, not just the child");
    }

    #[test]
    fn the_wall_clock_stops_a_child_that_runs_too_long() {
        let site = site();
        let _reaper = Reaper(site.home.clone());
        let perch = site.perch(&["--knob", "run_ms=1500"], &[("FAKE_WORKFLOW_CASE", "d"), HOLD]);
        started(&perch, json!({}));
        let pid = pid_of(&site, "r1");
        let failed = wait_run(&perch, "r1", "failed", Duration::from_secs(20));
        assert!(failed["why"].as_str().unwrap().contains("took longer than"), "{failed}");
        assert!(is_gone(pid), "stopped, not left");
        assert_eq!(started(&perch, json!({})), "r2", "and the browser is free");
    }

    #[test]
    fn a_park_is_not_on_the_clock_and_a_resumed_child_has_a_clock_of_its_own() {
        let site = site();
        let _reaper = Reaper(site.home.clone());
        let perch = site.perch(&["--knob", "run_ms=2500"], &[("FAKE_WORKFLOW_CASE", "b")]);
        started(&perch, json!({}));
        wait_run(&perch, "r1", "parked", Duration::from_secs(15));
        std::thread::sleep(Duration::from_millis(3200));
        let waiting = one(&perch, "r1");
        assert_eq!((waiting["state"].as_str(), waiting.get("why")), (Some("parked"), None), "a person is slower than a graph: {waiting}");
        assert_eq!(answer_to(&perch, "r1", "0").status, 202);
        wait_run(&perch, "r1", "done", Duration::from_secs(15));
    }

    #[test]
    fn a_child_that_is_stopped_from_outside_is_told_as_it_ended() {
        let site = site();
        let _reaper = Reaper(site.home.clone());
        let (case, named) = cases(&site, "d");
        let perch = site.perch(&[], &[("FAKE_WORKFLOW_CASE", &named), HOLD]);
        // d: someone else's SIGTERM. Nobody asked the perch, so it is not a cancel: eidolon's own word stands.
        started(&perch, json!({}));
        held(&site, "r1");
        kill_group(pid_of(&site, "r1"), libc::SIGTERM);
        let failed = wait_run(&perch, "r1", "failed", Duration::from_secs(15));
        assert_eq!((failed["why"].as_str(), failed["exit"].as_i64()), (Some("the run was cancelled"), Some(2)), "{failed}");
        // l: the same signal, the other end of the race: the graph parked, and so it is.
        pick(&case, "l");
        started(&perch, json!({}));
        held(&site, "r2");
        kill_group(pid_of(&site, "r2"), libc::SIGTERM);
        let parked = wait_run(&perch, "r2", "parked", Duration::from_secs(15));
        assert!(parked["question"].is_string(), "{parked}");
        assert_eq!(perch.post("/hub/runs/r2/abandon").status, 200);
        // k: killed with no end and no last line: the journal has no end, so the run is lost, and says how its child went.
        pick(&case, "k");
        started(&perch, json!({}));
        let lost = wait_run(&perch, "r3", "lost", Duration::from_secs(15));
        assert_eq!(lost["exit"], 137, "{lost}");
        let why = lost["why"].as_str().unwrap();
        assert!(why.contains("killed by signal 9") && why.contains("journal has no end"), "{why}");
        assert_eq!(started(&perch, json!({})), "r4", "a lost run does not hold the browser");
    }

    #[test]
    fn the_child_is_detached_from_the_perch_and_carries_nothing_it_should_not() {
        let site = site();
        let _reaper = Reaper(site.home.clone());
        let perch = site.perch(&[], &[("FAKE_WORKFLOW_CASE", "d"), HOLD]);
        started(&perch, json!({ "start": "Cat", "goal": "Felidae" }));
        held(&site, "r1");
        let pid = pid_of(&site, "r1");
        let dir = site.dir("r1");
        let (argv, environ) = proc_of(pid);
        let argv: Vec<&str> = argv.split('\0').filter(|word| !word.is_empty()).collect();
        let config = dir.join("config.toml");
        let plugin = site.plugins.join("jev");
        assert_eq!(
            argv,
            [
                fake_eidolon().unwrap(),
                "--config",
                config.to_str().unwrap(),
                "workflow",
                "run",
                plugin.to_str().unwrap(),
                "wiki-hop",
                "--args",
                "-",
                "--provider",
                "mock"
            ]
        );
        assert!(!argv.contains(&"--yolo"), "a run is never yolo");
        let token = perch.token.trim();
        assert!(!argv.iter().any(|word| word.contains(token)) && !environ.contains(token), "no token in the child's argv or environment");
        // Its three streams are the run's files, not pipes to the perch.
        let link = |fd: u32| std::fs::read_link(format!("/proc/{pid}/fd/{fd}")).unwrap();
        assert_eq!(link(0), dir.join("args.json"), "the args are on stdin, from a file");
        assert_eq!((link(1), link(2)), (dir.join("out.log"), dir.join("err.log")));
        // Its own group, led by itself: a signal to the perch's group does not reach it, and a signal to its group reaches all of it.
        assert_eq!(pgrp(pid), pid);
        assert_ne!(pgrp(perch.child.id()), pid);
        assert_eq!(cancel(&perch, "r1").status, 200);
    }

    #[test]
    fn two_starts_at_once_make_one_run_and_a_409() {
        let site = site();
        let _reaper = Reaper(site.home.clone());
        let perch = site.perch(&[], &[("FAKE_WORKFLOW_CASE", "d"), HOLD]);
        let (addr, auth) = (perch.addr.clone(), perch.auth());
        let gate = std::sync::Barrier::new(6);
        let statuses: Vec<u16> = std::thread::scope(|scope| {
            let racers: Vec<_> = (0..6)
                .map(|_| {
                    scope.spawn(|| {
                        let request = Request::new(&addr, "POST", "/hub/runs").bearer(auth.clone()).body(b"{\"graph\":\"wiki-hop\"}".to_vec());
                        gate.wait();
                        request.send(&addr).status
                    })
                })
                .collect();
            racers.into_iter().map(|racer| racer.join().unwrap()).collect()
        });
        assert_eq!(statuses.iter().filter(|status| **status == 202).count(), 1, "{statuses:?}");
        assert_eq!(statuses.iter().filter(|status| **status == 409).count(), 5, "{statuses:?}");
        assert_eq!(rows(&perch).len(), 1);
        held(&site, "r1");
        assert_eq!(workflow_lines(&perch, "run").len(), 1, "one child");
        assert_eq!(cancel(&perch, "r1").status, 200);
    }

    #[test]
    fn runs_json_is_private_and_a_lost_or_broken_one_never_reuses_a_runs_folder() {
        let site = site();
        let _reaper = Reaper(site.home.clone());
        // What a lost runs.json leaves: a folder of an earlier run, and a file that is not JSON.
        std::fs::create_dir_all(site.home.join("runs").join("r7")).unwrap();
        std::fs::write(site.home.join("runs.json"), "{ this is not json").unwrap();
        let perch = site.perch(&[], &[("FAKE_WORKFLOW_CASE", "a")]);
        assert_eq!(rows(&perch), Vec::<Value>::new(), "a file that does not parse is an empty list, not a refusal to start");
        let aside: Vec<String> = until(Duration::from_secs(5), "the broken file set aside", || {
            let found: Vec<String> =
                std::fs::read_dir(&site.home).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).filter(|name| name.starts_with("runs.json.bad-")).collect();
            (!found.is_empty()).then_some(found)
        });
        assert_eq!(std::fs::read_to_string(site.home.join(&aside[0])).unwrap(), "{ this is not json", "kept as it was, to be read");
        assert_eq!(started(&perch, json!({})), "r8", "past the folder that is there");
        wait_run(&perch, "r8", "done", Duration::from_secs(15));
        assert_eq!(mode(&site.home.join("runs.json")), 0o600);
        let kept = site.store();
        assert_eq!(kept.as_object().unwrap().keys().collect::<Vec<_>>(), ["r8"]);
        assert_eq!(kept["r8"]["state"], "done");
        assert!(kept["r8"].get("pid").is_none(), "no pid is kept for a child that has gone: {kept}");
    }

    // -----------------------------------------------------------------------
    // D3: the child outlives the perch, and the next perch takes it up
    // -----------------------------------------------------------------------

    #[test]
    fn a_run_outlives_the_perch_and_the_next_perch_takes_it_up() {
        let site = site();
        let _reaper = Reaper(site.home.clone());
        let env = [("FAKE_WORKFLOW_CASE", "a"), ("FAKE_WORKFLOW_LINGER_MS", "5000")];
        let mut first = site.perch(&[], &env);
        started(&first, json!({}));
        let pid = pid_of(&site, "r1");
        journaled(&site, "r1", "WorkflowEnd");
        let status = first.stop();
        assert!(status.success(), "a perch with a run going stops cleanly: {status}");
        assert!(!dead(pid), "the child did not go with the perch");
        assert!(proc_of(pid).0.contains("workflow"), "and it is still the workflow it was");
        assert_eq!(site.store()["r1"]["state"], "running", "the perch left the record as it was: the next one settles it");
        assert_eq!(site.pid("r1"), Some(pid));
        let second = site.perch(&[], &env);
        assert_eq!(one(&second, "r1")["state"], "running", "taken up where it was");
        let busy = start(&second, json!({}));
        assert_eq!(busy.status, 409, "an adopted run holds the browser as it did: {}{}", busy.head, busy.body);
        assert_eq!(body(&busy)["run"], "r1");
        let done = wait_run(&second, "r1", "done", Duration::from_secs(30));
        assert!(done["report"].as_str().unwrap().contains("arrived"), "told from the door's line the child left in out.log: {done}");
        assert_eq!(site.pid("r1"), None);
        assert!(wait_dead(pid, Duration::from_secs(5)));
    }

    #[test]
    fn a_run_that_ended_while_the_perch_was_down_is_told_from_what_it_left_and_can_be_answered() {
        let site = site();
        let _reaper = Reaper(site.home.clone());
        let env = [("FAKE_WORKFLOW_CASE", "b"), ("FAKE_WORKFLOW_LINGER_MS", "800")];
        let mut first = site.perch(&[], &env);
        started(&first, json!({}));
        let pid = pid_of(&site, "r1");
        journaled(&site, "r1", "WorkflowEnd");
        assert!(first.stop().success());
        assert!(wait_dead(pid, Duration::from_secs(10)), "the child finished its park with no perch to tell");
        let second = site.perch(&[], &env);
        let parked = wait_run(&second, "r1", "parked", Duration::from_secs(10));
        assert!(parked["question"].as_str().unwrap().contains("Which link leads toward the article"), "the question is the door's last line: {parked}");
        assert_eq!(site.pid("r1"), None);
        // The journal is found by its name in the run's folder, so the park can still be answered.
        assert_eq!(answer_to(&second, "r1", "0").status, 202);
        let done = wait_run(&second, "r1", "done", Duration::from_secs(15));
        assert_eq!(done["resumes"], 1, "{done}");
    }

    #[test]
    fn a_child_killed_while_the_perch_was_down_is_lost_unless_its_journal_says_it_ended() {
        let site = site();
        let _reaper = Reaper(site.home.clone());
        // Killed half way through: no door line and a journal with no end.
        let env = [("FAKE_WORKFLOW_CASE", "a"), ("FAKE_WORKFLOW_STEP_MS", "300")];
        let mut first = site.perch(&[], &env);
        started(&first, json!({}));
        let pid = pid_of(&site, "r1");
        journaled(&site, "r1", "browser_open");
        assert!(first.stop().success());
        kill_group(pid, libc::SIGKILL);
        assert!(wait_dead(pid, Duration::from_secs(5)));
        let second = site.perch(&[], &env);
        let lost = wait_run(&second, "r1", "lost", Duration::from_secs(10));
        assert!(lost["why"].as_str().unwrap().contains("journal has no end"), "{lost}");
        assert!(lost["ended_ms"].is_u64() && site.pid("r1").is_none(), "{lost}");
        let next = started(&second, json!({}));
        assert_eq!(next, "r2", "a lost run does not hold the browser");
        assert_eq!(cancel(&second, "r2").status, 200);
        // Killed after the last record, before the last line: the journal has its end, and that is what is told.
        let env = [("FAKE_WORKFLOW_CASE", "a"), ("FAKE_WORKFLOW_LINGER_MS", "20000")];
        let mut third = site.perch(&[], &env);
        started(&third, json!({}));
        let pid = pid_of(&site, "r3");
        journaled(&site, "r3", "WorkflowEnd");
        assert!(third.stop().success());
        kill_group(pid, libc::SIGKILL);
        assert!(wait_dead(pid, Duration::from_secs(5)));
        let fourth = site.perch(&[], &env);
        let done = wait_run(&fourth, "r3", "done", Duration::from_secs(10));
        assert!(done.get("report").is_none() && done.get("why").is_none(), "the journal says it completed, and nothing more: {done}");
    }

    #[test]
    fn a_run_the_perch_did_not_start_can_still_be_cancelled() {
        let site = site();
        let _reaper = Reaper(site.home.clone());
        let env = [("FAKE_WORKFLOW_CASE", "d"), HOLD];
        let mut first = site.perch(&[], &env);
        started(&first, json!({}));
        held(&site, "r1");
        let pid = pid_of(&site, "r1");
        assert!(first.stop().success());
        let second = site.perch(&[], &env);
        assert_eq!(one(&second, "r1")["state"], "running");
        let cancelled = cancel(&second, "r1");
        assert_eq!(cancelled.status, 200, "{}{}", cancelled.head, cancelled.body);
        assert_eq!(body(&cancelled)["state"], "cancelled", "told as the cancel it was, though the door said failed");
        assert!(wait_dead(pid, Duration::from_secs(5)));
        assert_eq!(started(&second, json!({})), "r2");
        assert_eq!(cancel(&second, "r2").status, 200);
    }

    // -----------------------------------------------------------------------
    // The health gate, with a child behind it
    // -----------------------------------------------------------------------

    #[test]
    fn a_decision_log_never_stops_a_run_and_a_resume_waits_for_the_services() {
        let jev = Stub::answering(&jev_says("/var/lib/jev/decisions.ndjson"));
        let browser = Stub::answering(&browser_says());
        let site = site();
        let _reaper = Reaper(site.home.clone());
        let (jev_port, browser_port) = (jev.port, browser.port);
        let perch = site.perch(&refs(&health_flags(jev_port, browser_port)), &[("FAKE_WORKFLOW_CASE", "b")]);
        // jev logs its decisions to a file: the graph tab shows that, and the operator decides. The perch starts the run.
        assert_eq!(started(&perch, json!({})), "r1");
        wait_run(&perch, "r1", "parked", Duration::from_secs(15));
        assert_eq!(health(&perch)["jev"]["decisions_log"], "/var/lib/jev/decisions.ndjson");
        // A resume calls jev and the browser again, and a call that fails is journaled and can never be replayed: with a service down
        // the answer is refused and the run stays parked, so it can be answered when the service is back.
        drop(jev);
        let refused = answer_to(&perch, "r1", "0");
        assert_eq!(refused.status, 503, "{}{}", refused.head, refused.body);
        assert_eq!(body(&refused)["error"], "jev is not running (connection refused); start it with: minerva services start");
        let still = one(&perch, "r1");
        assert_eq!((still["state"].as_str(), still["resumes"].as_u64()), (Some("parked"), Some(0)), "{still}");
        assert!(workflow_lines(&perch, "resume").is_empty(), "no child was started for a refused answer");
        let _jev = Stub::answering_on(jev_port, &jev_says("off"));
        assert_eq!(answer_to(&perch, "r1", "0").status, 202);
        wait_run(&perch, "r1", "done", Duration::from_secs(15));
    }

    #[test]
    fn a_jev_from_before_j1_starts_a_run_too() {
        let (jev, browser) = (Stub::answering("{\"status\":\"ok\"}"), Stub::answering(&browser_says()));
        let site = site();
        let _reaper = Reaper(site.home.clone());
        let perch = site.perch(&refs(&health_flags(jev.port, browser.port)), &[("FAKE_WORKFLOW_CASE", "a")]);
        assert_eq!(started(&perch, json!({})), "r1");
        wait_run(&perch, "r1", "done", Duration::from_secs(15));
    }

    /// A chat the perch knows the way a page sees one: a log in `<home>/sessions`, which `list` reads.
    fn seed_chat(site: &Site, id: &str) {
        std::fs::create_dir_all(site.home.join("sessions")).unwrap();
        std::fs::write(site.home.join("sessions").join(format!("{id}.eid")), "").unwrap();
    }

    /// Every file under `dir`, at any depth.
    fn files_under(dir: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                found.extend(files_under(&path));
            } else {
                found.push(path);
            }
        }
        found
    }

    #[test]
    fn a_run_started_beside_a_chat_keeps_the_chat_on_every_answer() {
        let site = site();
        let _reaper = Reaper(site.home.clone());
        seed_chat(&site, "c0ffee01");
        let perch = site.perch(&[], &[("FAKE_WORKFLOW_CASE", "a")]);
        let answer = perch.post_json("/hub/runs", json!({ "graph": "wiki-hop", "chat": "c0ffee01" }));
        assert_eq!(answer.status, 202, "{}{}", answer.head, answer.body);
        assert_eq!(body(&answer), json!({ "rid": "r1", "state": "running", "chat": "c0ffee01" }));
        let done = wait_run(&perch, "r1", "done", Duration::from_secs(15));
        assert_eq!(done["chat"], "c0ffee01", "the detail: {done}");
        assert_eq!(rows(&perch)[0]["chat"], "c0ffee01", "the list row");
        assert_eq!(rows_at(&perch, "/hub/runs?chat=c0ffee01").iter().map(|row| row["rid"].as_str().unwrap()).collect::<Vec<_>>(), ["r1"]);
        assert_eq!(site.store()["r1"]["chat"], "c0ffee01", "runs.json keeps it");
        // The chat never reached eidolon, and nothing the run wrote carries it: not the argv, not its config, args, logs or journal.
        assert!(perch.fake_records().iter().all(|line| !line.contains("c0ffee01")), "{:?}", perch.fake_records());
        let files = files_under(&site.dir("r1"));
        assert!(files.len() >= 4, "{files:?}");
        for file in files {
            let text = String::from_utf8_lossy(&std::fs::read(&file).unwrap_or_default()).to_string();
            assert!(!text.contains("c0ffee01") && !file.to_string_lossy().contains("c0ffee01"), "{}", file.display());
        }
    }

    #[test]
    fn a_run_with_no_chat_is_what_it_was_before() {
        let site = site();
        let _reaper = Reaper(site.home.clone());
        let perch = site.perch(&[], &[("FAKE_WORKFLOW_CASE", "a")]);
        let first = start(&perch, json!({}));
        assert_eq!(first.status, 202, "{}{}", first.head, first.body);
        assert_eq!(body(&first), json!({ "rid": "r1", "state": "running" }), "exactly the answer it was");
        wait_run(&perch, "r1", "done", Duration::from_secs(15));
        let second = perch.post_json("/hub/runs", json!({ "graph": "wiki-hop", "chat": null }));
        assert_eq!(second.status, 202, "{}{}", second.head, second.body);
        assert_eq!(body(&second), json!({ "rid": "r2", "state": "running" }), "a null chat is no chat");
        wait_run(&perch, "r2", "done", Duration::from_secs(15));
        for rid in ["r1", "r2"] {
            assert!(one(&perch, rid).get("chat").is_none(), "{rid}: the detail has no chat");
            assert!(site.store()[rid].get("chat").is_none(), "{rid}: runs.json has no chat");
        }
        assert!(rows(&perch).iter().all(|row| row.get("chat").is_none()), "the list has no chat");
    }

    #[test]
    fn a_chat_the_perch_remembers_but_cannot_list_is_known() {
        let site = site();
        let _reaper = Reaper(site.home.clone());
        // A chat with a folder remembered in chat-cwds.json and no log: the list does not have it, the perch's memory does.
        let folder = site.base.path().join("chat-folder");
        std::fs::create_dir_all(&folder).unwrap();
        let folder = folder.canonicalize().unwrap();
        std::fs::write(site.home.join("chat-cwds.json"), json!({ "c0ffee02": folder.display().to_string() }).to_string()).unwrap();
        let perch = site.perch(&[], &[("FAKE_WORKFLOW_CASE", "a")]);
        let answer = perch.post_json("/hub/runs", json!({ "graph": "wiki-hop", "chat": "c0ffee02" }));
        assert_eq!(answer.status, 202, "{}{}", answer.head, answer.body);
        assert_eq!(body(&answer)["chat"], "c0ffee02");
        wait_run(&perch, "r1", "done", Duration::from_secs(15));
    }

    #[test]
    fn the_holder_409_names_its_chat() {
        let site = site();
        let _reaper = Reaper(site.home.clone());
        seed_chat(&site, "c0ffee01");
        seed_chat(&site, "c0ffee02");
        let perch = site.perch(&[], &[("FAKE_WORKFLOW_CASE", "b")]);
        let parked = perch.post_json("/hub/runs", json!({ "graph": "wiki-hop", "chat": "c0ffee01" }));
        assert_eq!(parked.status, 202, "{}{}", parked.head, parked.body);
        wait_run(&perch, "r1", "parked", Duration::from_secs(15));
        // A start with no chat, and one beside the other chat: both are the holder's 409, and it names the holder's chat, not the asker's.
        for asked in [None, Some("c0ffee02")] {
            let mut request = json!({ "graph": "wiki-hop" });
            if let Some(chat) = asked {
                request["chat"] = json!(chat);
            }
            let busy = perch.post_json("/hub/runs", request);
            assert_eq!(busy.status, 409, "{asked:?}: {}{}", busy.head, busy.body);
            assert_eq!((body(&busy)["run"].as_str(), body(&busy)["chat"].as_str()), (Some("r1"), Some("c0ffee01")), "{asked:?}: {}", busy.body);
            assert!(body(&busy)["error"].as_str().unwrap().contains("r1"), "{}", busy.body);
        }
        assert_eq!(workflow_lines(&perch, "run").len(), 1, "one child");
    }

    #[test]
    fn an_unknown_chat_is_a_404_even_while_a_run_holds_the_browser() {
        let site = site();
        let _reaper = Reaper(site.home.clone());
        seed_chat(&site, "c0ffee01");
        let perch = site.perch(&[], &[("FAKE_WORKFLOW_CASE", "b")]);
        assert_eq!(started(&perch, json!({})), "r1");
        wait_run(&perch, "r1", "parked", Duration::from_secs(15));
        // The existence check runs before the holder check: the answer is about the chat, not the browser.
        let unknown = perch.post_json("/hub/runs", json!({ "graph": "wiki-hop", "chat": "c0ffee99" }));
        assert_eq!(unknown.status, 404, "{}{}", unknown.head, unknown.body);
        assert_eq!(body(&unknown)["error"], "no such chat", "{}", unknown.body);
        assert_eq!(workflow_lines(&perch, "run").len(), 1, "no child was started for it");
    }

    #[test]
    fn cancel_and_abandon_answers_carry_the_chat() {
        let site = site();
        let _reaper = Reaper(site.home.clone());
        seed_chat(&site, "c0ffee01");
        let (case, named) = cases(&site, "b");
        let perch = site.perch(&[], &[("FAKE_WORKFLOW_CASE", &named), HOLD]);
        let parked = perch.post_json("/hub/runs", json!({ "graph": "wiki-hop", "chat": "c0ffee01" }));
        assert_eq!(parked.status, 202, "{}{}", parked.head, parked.body);
        wait_run(&perch, "r1", "parked", Duration::from_secs(15));
        let abandoned = perch.post("/hub/runs/r1/abandon");
        assert_eq!(abandoned.status, 200, "{}{}", abandoned.head, abandoned.body);
        assert_eq!(body(&abandoned)["state"], "abandoned");
        assert_eq!(body(&abandoned)["chat"], "c0ffee01", "{}", abandoned.body);
        // A run that is held and then cancelled, beside the same chat: its cancel answer names the chat too.
        pick(&case, "d");
        let held_start = perch.post_json("/hub/runs", json!({ "graph": "wiki-hop", "chat": "c0ffee01" }));
        assert_eq!(held_start.status, 202, "{}{}", held_start.head, held_start.body);
        held(&site, "r2");
        let cancelled = cancel(&perch, "r2");
        assert_eq!(cancelled.status, 200, "{}{}", cancelled.head, cancelled.body);
        assert_eq!((body(&cancelled)["state"].as_str(), body(&cancelled)["chat"].as_str()), (Some("cancelled"), Some("c0ffee01")), "{}", cancelled.body);
    }
}

// ---------------------------------------------------------------------------
// The stand-in itself: it has to be the real door's shape, or every test above proves nothing
// ---------------------------------------------------------------------------

#[cfg(feature = "test-bins")]
mod stand_in {
    use super::*;
    use std::os::unix::process::ExitStatusExt as _;
    use std::process::{Command, Output, Stdio};

    fn fixture(letter: &str) -> (Value, Vec<String>) {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/jev").join(letter);
        let meta = serde_json::from_str(&std::fs::read_to_string(dir.join("case.json")).unwrap()).unwrap();
        let journal = std::fs::read_to_string(dir.join("journal.ndjson")).unwrap().lines().map(str::to_string).collect();
        (meta, journal)
    }

    fn lines(path: &Path) -> Vec<String> {
        std::fs::read_to_string(path).unwrap_or_default().lines().filter(|line| !line.trim().is_empty()).map(str::to_string).collect()
    }

    /// A sessions dir and the config that names it, as the perch writes one for a run.
    struct Door {
        _dir: tempfile::TempDir,
        config: PathBuf,
        sessions: PathBuf,
    }

    fn fresh() -> Door {
        let dir = tempfile::tempdir().unwrap();
        let sessions = dir.path().join("s");
        let config = dir.path().join("config.toml");
        std::fs::write(&config, format!("sessions_dir = {:?}\n", sessions.to_str().unwrap())).unwrap();
        Door { _dir: dir, config, sessions }
    }

    impl Door {
        fn command(&self, letter: &str, extra: &[&str]) -> Command {
            let mut command = Command::new(fake_eidolon().expect("the stand-in door"));
            command.arg("--config").arg(&self.config).arg("workflow").args(extra);
            command.env("FAKE_WORKFLOW_CASE", letter).env("FAKE_WORKFLOW_HOLD_MS", "100").env_remove("FAKE_LOG");
            command.stdout(Stdio::piped()).stderr(Stdio::piped());
            command
        }

        fn spawn_run(&self, letter: &str, args: &str, env: &[(&str, &str)]) -> std::process::Child {
            let mut command = self.command(letter, &["run", "/p/jev", "wiki-hop", "--args", "-", "--provider", "mock"]);
            command.envs(env.iter().copied()).stdin(Stdio::piped());
            let mut child = command.spawn().unwrap();
            child.stdin.take().unwrap().write_all(args.as_bytes()).unwrap();
            child
        }

        fn run(&self, letter: &str, args: &str) -> Output {
            self.spawn_run(letter, args, &[]).wait_with_output().unwrap()
        }

        fn resume(&self, letter: &str, session: &Path, run: &str, answer: Option<&str>) -> Output {
            let mut extra = vec!["resume", "--plugin", "/p/jev", session.to_str().unwrap(), run];
            let given = answer.map(|text| format!("--answer={text}"));
            extra.extend(given.as_deref());
            extra.extend(["--provider", "mock"]);
            self.command(letter, &extra).stdin(Stdio::null()).output().unwrap()
        }

        fn sessions_made(&self) -> Vec<PathBuf> {
            std::fs::read_dir(&self.sessions).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "eid")).collect()
        }

        /// The one session the door has made.
        fn session(&self) -> PathBuf {
            let found = self.sessions_made();
            assert_eq!(found.len(), 1, "{found:?}");
            found[0].clone()
        }

        /// Once the door has written at least `records` of its journal.
        fn written(&self, records: usize) -> Option<()> {
            self.sessions_made().first().filter(|session| lines(session).len() >= records).map(|_| ())
        }
    }

    fn said(out: &Output) -> Value {
        let text = String::from_utf8_lossy(&out.stdout);
        serde_json::from_str(text.trim()).unwrap_or_else(|e| panic!("one JSON line on stdout ({e}): {text:?}"))
    }

    #[test]
    fn it_replays_every_case_the_way_the_real_door_ended() {
        for letter in ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l"] {
            let (meta, journal) = fixture(letter);
            let door = fresh();
            let mut session: Option<PathBuf> = None;
            for (n, inv) in meta["invocations"].as_array().unwrap().iter().enumerate() {
                let out = match &session {
                    None => door.run(letter, &meta["args"].to_string()),
                    Some(session) => door.resume(letter, session, "1", inv["answer"].as_str()),
                };
                let (exit, what) = (inv["exit"].as_i64().unwrap() as i32, format!("case {letter}, invocation {}", n + 1));
                let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                if exit == 137 {
                    assert_eq!(out.status.signal(), Some(9), "{what}: the one that was killed is killed: {stderr}");
                } else {
                    assert_eq!(out.status.code(), Some(exit), "{what}: {stderr}");
                }
                // The door's last line is the recorded one, but for the session, which is the file written here.
                let recorded = inv["stdout"].as_str().unwrap();
                if recorded.is_empty() {
                    assert!(out.stdout.is_empty(), "{what}: {:?}", String::from_utf8_lossy(&out.stdout));
                } else {
                    let (mut want, mut got) = (serde_json::from_str::<Value>(recorded).unwrap(), said(&out));
                    if want.get("session").is_some() {
                        assert_eq!(got["session"].as_str(), session.as_deref().map_or_else(|| door.session(), Path::to_path_buf).to_str(), "{what}");
                    }
                    for line in [&mut want, &mut got] {
                        line.as_object_mut().unwrap().remove("session");
                    }
                    assert_eq!(got, want, "{what}");
                }
                // Its stderr: the lines it had, and the cancel notice where it had one.
                let recorded_err = inv["stderr"].as_str().unwrap();
                for line in recorded_err.lines().filter(|line| !line.is_empty()) {
                    assert!(stderr.contains(line), "{what}: {line:?} is missing from {stderr:?}");
                }
                if session.is_none() {
                    session = Some(door.session());
                }
            }
            let written = lines(session.as_ref().unwrap());
            let want: Vec<&String> = journal.iter().filter(|line| !line.trim().is_empty()).collect();
            assert_eq!(written.iter().collect::<Vec<_>>(), want, "case {letter}: the session is the journal F0 captured");
        }
    }

    #[test]
    fn it_refuses_what_the_real_door_refuses() {
        let refused = |out: &Output, why: &str| {
            assert_eq!(out.status.code(), Some(3), "{why}: {}", String::from_utf8_lossy(&out.stderr));
            let line = said(out);
            assert_eq!(line["status"], "refused", "{why}: {line}");
            line["error"].as_str().unwrap().to_string()
        };
        let door = fresh();
        // The args are one JSON object.
        for bad in ["[1]", "{", "", "\"x\""] {
            refused(&door.run("a", bad), bad);
        }
        assert!(!door.sessions.exists() || std::fs::read_dir(&door.sessions).unwrap().next().is_none(), "a refused start makes no session");
        // Usage mistakes are not refusals: exit 2, nothing on stdout.
        for argv in [&["run", "/p/jev"][..], &["run", "/p/jev", "g", "--bogus", "x"], &["resume", "s", "1"], &["nothing"]] {
            let out = door.command("a", argv).stdin(Stdio::null()).output().unwrap();
            assert_eq!((out.status.code(), out.stdout.is_empty()), (Some(2), true), "{argv:?}: {}", String::from_utf8_lossy(&out.stderr));
        }
        assert_eq!(door.command("z", &["run", "/p/jev", "g", "--args", "{}"]).stdin(Stdio::null()).output().unwrap().status.code(), Some(2), "no such case");
        // A resume names a journal that is there and a run in it.
        refused(&door.resume("a", &door._dir.path().join("none.eid"), "1", None), "no such journal");
        let parked = fresh();
        assert_eq!(parked.run("b", "{}").status.code(), Some(1));
        let session = parked.session();
        let before = lines(&session);
        assert!(refused(&parked.resume("b", &session, "1", None), "a park with no answer").contains("--answer"));
        refused(&parked.resume("b", &session, "2", Some("0")), "no run 2");
        assert_eq!(lines(&session), before, "a refusal writes nothing");
        assert_eq!(parked.resume("b", &session, "1", Some("0")).status.code(), Some(0));
        // A run that has ended has no park to answer and none to continue.
        refused(&parked.resume("b", &session, "1", Some("0")), "answered twice");
        refused(&parked.resume("b", &session, "1", None), "an ended run, no answer");
        // c: a refusal the real door gave is given every time, and leaves the case where it was.
        let broken = fresh();
        assert_eq!(broken.run("c", "{}").status.code(), Some(1));
        let session = broken.session();
        let first = refused(&broken.resume("c", &session, "1", Some("0")), "c, once");
        assert!(first.contains("jev_entail"), "{first}");
        assert_eq!(refused(&broken.resume("c", &session, "1", Some("0")), "c, twice"), first);
        assert_eq!(lines(&session).len(), 30, "the journal is as the first invocation left it");
    }

    #[test]
    fn a_case_file_names_the_case_at_every_start() {
        let door = fresh();
        let file = door._dir.path().join("which");
        std::fs::write(&file, "b\n").unwrap();
        assert_eq!(door.command(file.to_str().unwrap(), &["run", "/p/jev", "g", "--args", "{}"]).stdin(Stdio::null()).output().unwrap().status.code(), Some(1), "b parks");
        std::fs::write(&file, "a").unwrap();
        let out = door.command(file.to_str().unwrap(), &["run", "/p/jev", "g", "--args", "{}"]).stdin(Stdio::null()).output().unwrap();
        assert_eq!((out.status.code(), said(&out)["status"].as_str().map(str::to_string)), (Some(0), Some("completed".to_string())), "a completes");
    }

    fn term(child: &std::process::Child) {
        assert_eq!(unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) }, 0);
    }

    #[test]
    fn it_dies_to_sigterm_the_way_f0_found_it() {
        // In the middle of any case: the token wins, the journal gets its end, the door says failed.
        let door = fresh();
        let child = door.spawn_run("a", "{}", &[("FAKE_WORKFLOW_STEP_MS", "200")]);
        until(Duration::from_secs(10), "a few records", || door.written(5));
        term(&child);
        let out = child.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{}", String::from_utf8_lossy(&out.stderr));
        let line = said(&out);
        assert_eq!((line["status"].as_str(), line["reason"].as_str()), (Some("failed"), Some("the run was cancelled")), "{line}");
        let written = lines(&door.session());
        assert!(written.len() < 51 && written.last().unwrap().contains("\"WorkflowEnd\"") && written.last().unwrap().contains("\"Failed\""), "{}", written.last().unwrap());
        assert!(String::from_utf8_lossy(&out.stderr).contains("[cancelling]"));
        // d and l are held where they were captured, and come out the two ways it can go.
        for (letter, exit, status, records) in [("d", 2, "failed", 25), ("l", 1, "parked", 30)] {
            let door = fresh();
            let child = door.spawn_run(letter, "{}", &[("FAKE_WORKFLOW_HOLD_MS", "30000")]);
            until(Duration::from_secs(10), "the held tool", || door.written(24));
            std::thread::sleep(Duration::from_millis(300));
            assert_eq!(lines(&door.session()).len(), 24, "case {letter} waits for the signal where it was captured");
            term(&child);
            let out = child.wait_with_output().unwrap();
            assert_eq!(out.status.code(), Some(exit), "case {letter}: {}", String::from_utf8_lossy(&out.stderr));
            assert_eq!(said(&out)["status"], status, "case {letter}");
            assert_eq!(lines(&door.session()).len(), records, "case {letter}: the rest of what was captured is played after the signal");
        }
        // SIGKILL leaves the journal where it stood: no end.
        let door = fresh();
        let mut child = door.spawn_run("a", "{}", &[("FAKE_WORKFLOW_STEP_MS", "200")]);
        until(Duration::from_secs(10), "a few records", || door.written(5));
        child.kill().unwrap();
        let out = child.wait_with_output().unwrap();
        assert_eq!(out.status.signal(), Some(9));
        assert!(!std::fs::read_to_string(door.session()).unwrap().contains("\"WorkflowEnd\""));
    }
}
