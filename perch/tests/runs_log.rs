//! `GET /hub/runs/<rid>/log?after=N` and `GET /hub/runs/graphs[/<g>]` (slice R1b).
//!
//! Every test starts its own perch with a Minerva home and a plugins folder of its own (temp dirs), so nothing reads or writes the
//! operator's `~/Minerva`. The log tests need the compiled stand-in (`--features test-bins`): its `log --json --after N PATH` answers as
//! eidolon's does, and it writes a `helper` line to `FAKE_LOG` for every call, which is how a test counts how often the perch asked. Most of
//! them seed a finished run by hand (a `runs.json`, a run folder with a config and a journal), so the records are exactly the ones the test
//! wrote and nothing waits on a clock; one runs the stand-in's `workflow` at a slow pace and polls it as the page does. The graph tests need
//! no child at all: their perch's `--eidolon` is a path that does not exist.
//!
//! Mutations that were run by hand, each by itself, and what stopped it (the tests that failed; every other test of the file passed):
//!
//! ```text
//!   the cache never matches (eidolon is asked every time)   the unchanged-journal test, the stepping test, two-polls-at-once (3)
//!   the cache ignores `after`                                the unchanged-journal test, the bad-query test, the stepping test (3)
//!   `--after N` left out of the argv                         the unchanged-journal, bad-query, stepping, two-polls and over-the-cap tests (5)
//!   a busy slot is not waited for (no single flight)         two_polls_at_once (1)
//!   an answer over 1 MiB is kept                             an_answer_over_1_mib_is_given_every_time_... (1)
//!   a run with no journal yet is a 404, not an empty 200     the no-journal test and the stepping test, whose first poll races the child (2)
//!   the method of a log request not checked                  only_a_get_without_a_query_... (1)
//!   a non-digit `after` let through                          the bad-query test (1)
//!   graphs: any name, not only the operator's list           the unlisted-name test, the no-plugins-dir test (2)
//!   graphs: the workflow file not required                   what_a_start_could_not_use_... (1)
//!   graphs: no size cap                                      what_a_start_could_not_use_... (1)
//!   graphs: any JSON, not only an object                     what_a_start_could_not_use_... (1)
//!   graphs: a link in place of the file followed             what_a_start_could_not_use_... (1)
//!   graphs: the answer re-serialized (keys sorted)           the bytes-and-pin test (1)
//!   graphs: the hash of nothing                              the bytes-and-pin test (1)
//!   graphs: a query accepted                                 only_a_get_without_a_query_... (1)
//! ```
//!
//! Not covered by a test: the cache keeps an answer only if the journal had not moved while eidolon read it (it would take a child that
//! is slower than the test can time). It is two `stamp_of` calls around the child in `log.rs`.
#![cfg_attr(not(feature = "test-bins"), allow(dead_code))]

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use common::*;
use serde_json::{Value, json};

/// The member's `config.toml` the home's is generated from.
const MEMBER: &str = "thinking_budget = 4000\n\n[policy]\nenabled = true\nyolo = false\n\n[swarm]\nenabled = true\n";
/// F0's case a: a whole wiki-hop run, 51 records with ids 0 to 50.
const CASE_A: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/jev/a/journal.ndjson");
/// eidolon-plugins' jev folder, vendored: the real graph and the workflow that pins its hash.
const VENDORED_JEV: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../eidolon-plugins/jev");
/// What the perch will read of a graph at most.
const MAX_GRAPH: usize = 1024 * 1024;

fn body(answer: &Answer) -> Value {
    serde_json::from_str(&answer.body).unwrap_or_else(|e| panic!("a JSON body ({e}): {}{}", answer.head, answer.body))
}

/// A Minerva home and a plugins folder in a temp tree that lives as long as any perch built on it.
struct Site {
    base: Arc<tempfile::TempDir>,
    home: PathBuf,
    plugins: PathBuf,
}

/// A home with its `minerva.toml`, and a plugins folder with the `jev/workflows` and `jev/graphs` folders and nothing in them.
fn site() -> Site {
    let base = tempfile::tempdir().unwrap();
    let home = base.path().join("Minerva");
    std::fs::create_dir(&home).unwrap();
    std::fs::write(home.join("minerva.toml"), "default_model = \"mock\"\ndefault_mode = \"auto\"\n").unwrap();
    let jev = base.path().join("plugins").join("jev");
    std::fs::create_dir_all(jev.join("workflows")).unwrap();
    std::fs::create_dir_all(jev.join("graphs")).unwrap();
    let (home, plugins) = (home.canonicalize().unwrap(), base.path().join("plugins").canonicalize().unwrap());
    Site { base: Arc::new(base), home, plugins }
}

impl Site {
    /// `workflows/<name>.rn` (what a start needs; the stand-in never reads it) and, when given, `graphs/<name>.json`.
    fn graph(&self, name: &str, json: Option<&[u8]>) {
        std::fs::write(self.plugins.join("jev/workflows").join(format!("{name}.rn")), "// the stand-in door never reads a graph\n").unwrap();
        if let Some(json) = json {
            std::fs::write(self.plugins.join("jev/graphs").join(format!("{name}.json")), json).unwrap();
        }
    }

    /// A perch that can only be refused: its `--eidolon` is not there, so nothing here can start a child.
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

    /// A perch with the stand-in door as `--eidolon`, its log, and `env` for every child it starts.
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

    /// A run that has ended, as an earlier perch left it: its row in `runs.json` and its folder, with a `config.toml` and, when given, the
    /// journal `<ms>.eid`. Written before the perch starts. Returns the journal's path (which may not exist).
    fn seed(&self, rid: &str, journal: Option<&str>) -> PathBuf {
        let dir = self.dir(rid);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.toml"), "default_model = \"mock\"\n").unwrap();
        let eid = dir.join("1000.eid");
        if let Some(text) = journal {
            std::fs::write(&eid, text).unwrap();
        }
        let file = self.home.join("runs.json");
        let mut runs: serde_json::Map<String, Value> = std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        runs.insert(rid.to_string(), json!({ "graph": "wiki-hop", "state": "done", "started_ms": 1, "ended_ms": 2, "resumes": 0 }));
        std::fs::write(&file, serde_json::to_vec(&runs).unwrap()).unwrap();
        eid
    }
}

fn record(id: u64) -> String {
    format!("{{\"id\":{id},\"parent\":{},\"ts_ms\":{},\"kind\":{{\"Note\":{{\"text\":\"record {id}\"}}}}}}\n", id.saturating_sub(1), 1000 + id)
}

/// `count` records, ids 0 to `count - 1`, one JSON line each.
fn journal_of(count: u64) -> String {
    (0..count).map(record).collect()
}

/// The ids of the JSON lines in a body, in order.
fn ids(text: &str) -> Vec<u64> {
    text.lines().map(|line| serde_json::from_str::<Value>(line).unwrap_or_else(|e| panic!("{line:?}: {e}"))["id"].as_u64().unwrap()).collect()
}

fn range(from: u64, to: u64) -> Vec<u64> {
    (from..=to).collect()
}

fn log_of(perch: &Perch, rid: &str, query: &str) -> Answer {
    perch.get(&format!("/hub/runs/{rid}/log{query}"))
}

/// How many times the perch has run `eidolon log --json` on the stand-in so far.
fn asks(perch: &Perch) -> usize {
    perch.fake_helpers().iter().filter(|line| line.starts_with("helper log --json")).count()
}

/// The one-line forms of the graph routes' answers.
fn graph_list(perch: &Perch) -> Vec<Value> {
    let answer = perch.get("/hub/runs/graphs");
    assert_eq!(answer.status, 200, "{}{}", answer.head, answer.body);
    assert_eq!(answer.header("content-type").as_deref(), Some("application/json"), "{}", answer.head);
    body(&answer).as_array().unwrap_or_else(|| panic!("the list is an array: {}", answer.body)).clone()
}

/// The empty answer of a route nobody serves (or a name nobody offers): a status and no body at all.
fn assert_empty(answer: &Answer, status: u16, what: &str) {
    assert_eq!((answer.status, answer.body.as_str()), (status, ""), "{what}: {}", answer.head);
}

// ---------------------------------------------------------------------------
// The gate and the shapes (no child is ever started here)
// ---------------------------------------------------------------------------

#[test]
fn the_routes_are_behind_the_token() {
    let site = site();
    site.graph("wiki-hop", Some(b"{}"));
    site.seed("r1", Some(&journal_of(3)));
    let perch = site.quiet(&[]);
    for path in ["/hub/runs/graphs", "/hub/runs/graphs/wiki-hop", "/hub/runs/r1/log", "/hub/runs/r1/log?after=1", "/hub/runs/r9/log"] {
        let bare = Request::new(&perch.addr, "GET", path).send(&perch.addr);
        assert_empty(&bare, 401, path);
        let wrong = Request::new(&perch.addr, "GET", path).bearer("Bearer nope").send(&perch.addr);
        assert_empty(&wrong, 401, path);
    }
}

#[test]
fn a_graph_the_operator_did_not_list_is_the_empty_404_however_it_is_spelt() {
    let site = site();
    site.graph("wiki-hop", Some(b"{\"id\":\"wiki-hop\"}"));
    // On disk, complete, and not in --run-graphs.
    site.graph("secret", Some(b"{\"id\":\"secret\"}"));
    let perch = site.quiet(&[]);
    assert_eq!(graph_list(&perch).iter().map(|g| g["name"].as_str().unwrap().to_string()).collect::<Vec<_>>(), ["wiki-hop"]);
    for name in [
        "secret", "Wiki-Hop", "wiki-hop.json", "wiki-hop/", "wiki-hop/x", "wiki-hop%2f", "%77iki-hop", "..", "../graphs/wiki-hop",
        "..%2fgraphs%2fwiki-hop", "%2e%2e", ".", "wiki-hop%00", "wiki+hop", "-wiki-hop", "_",
    ] {
        let answer = perch.get(&format!("/hub/runs/graphs/{name}"));
        assert_empty(&answer, 404, &format!("graphs/{name}"));
    }
    // The name is a bare name before it is anything else: a long one is refused without a lookup, and one in the list is not 404.
    assert_empty(&perch.get(&format!("/hub/runs/graphs/{}", "a".repeat(65))), 404, "a name over 64");
    assert_eq!(perch.get("/hub/runs/graphs/wiki-hop").status, 200);
}

#[test]
fn only_a_get_without_a_query_reads_a_graph_or_a_log() {
    let site = site();
    site.graph("wiki-hop", Some(b"{}"));
    site.seed("r1", Some(&journal_of(3)));
    let perch = site.quiet(&[]);
    for path in ["/hub/runs/graphs", "/hub/runs/graphs/wiki-hop", "/hub/runs/r1/log"] {
        for method in ["POST", "PUT", "DELETE", "PATCH"] {
            let answer = perch.apireq(method, path).send(&perch.addr);
            assert_empty(&answer, 405, &format!("{method} {path}"));
        }
    }
    // The graphs take no query at all.
    for path in ["/hub/runs/graphs?x=1", "/hub/runs/graphs/wiki-hop?after=1", "/hub/runs/graphs/wiki-hop?x"] {
        assert_empty(&perch.get(path), 400, path);
    }
    // A run nobody knows is the same empty 404 whatever else is wrong with the request.
    for (method, path) in [("GET", "/hub/runs/r9/log"), ("POST", "/hub/runs/r9/log"), ("GET", "/hub/runs/r9/log?after=x"), ("GET", "/hub/runs/r0/log")]
    {
        let answer = perch.apireq(method, path).send(&perch.addr);
        assert_empty(&answer, 404, &format!("{method} {path}"));
    }
    // Ids that are not run ids are never looked up (and `r1/log/x` is not a log).
    for path in [
        "/hub/runs/nope/log", "/hub/runs/R1/log", "/hub/runs/r1x/log", "/hub/runs/..%2f..%2fx/log", "/hub/runs/r1/log/x", "/hub/runs//log",
        "/hub/runs/r1/logs",
    ] {
        assert_empty(&perch.get(path), 404, path);
    }
}

// ---------------------------------------------------------------------------
// The graphs
// ---------------------------------------------------------------------------

#[test]
fn a_graph_comes_back_as_its_own_bytes_with_the_hash_the_workflow_pins() {
    let site = site();
    // The real wiki-hop of the vendored jev plugin and the workflow that pins its hash, with the line endings a Linux checkout has (the
    // checkout this runs from may have converted them: the pin is the hash of the text with `\n`).
    let on_disk = std::fs::read_to_string(format!("{VENDORED_JEV}/graphs/wiki-hop.json")).unwrap();
    let graph = on_disk.replace("\r\n", "\n").into_bytes();
    let workflow = std::fs::read_to_string(format!("{VENDORED_JEV}/workflows/wiki-hop.rn")).unwrap();
    let pin = workflow
        .lines()
        .find_map(|line| line.trim().strip_prefix("const PIN = \"sha256:")?.strip_suffix("\";"))
        .expect("the PIN line of wiki-hop.rn");
    site.graph("wiki-hop", Some(&graph));
    let perch = site.quiet(&[]);

    let listed = [json!({ "name": "wiki-hop", "sha256": pin, "bytes": graph.len() })];
    assert_eq!(graph_list(&perch), listed, "the list says name, hash and size, and not the graph");
    let one = perch.get("/hub/runs/graphs/wiki-hop");
    assert_eq!(one.status, 200, "{}{}", one.head, one.body);
    assert_eq!(one.header("content-type").as_deref(), Some("application/json"));
    let parsed = body(&one);
    assert_eq!((parsed["name"].as_str(), parsed["sha256"].as_str(), parsed["bytes"].as_u64()), (Some("wiki-hop"), Some(pin), Some(graph.len() as u64)));
    assert_eq!(parsed["graph"], serde_json::from_slice::<Value>(&graph).unwrap(), "the graph is the file's JSON");
    assert_eq!(parsed["graph"]["meta"]["jev"]["input"]["required"], json!(["start", "goal"]), "what the new-run form reads its fields from");
    // Byte for byte: the file's own text is inside the answer, so the order of `states` is the file's.
    let text = String::from_utf8(graph).unwrap();
    assert!(one.body.contains(text.trim_end()), "the answer holds the file as it is");
    let states = &one.body[one.body.find("\"states\"").unwrap()..];
    let at = |state: &str| states.find(&format!("\"{state}\": {{")).unwrap_or_else(|| panic!("{state} in {states}"));
    assert!(at("open") < at("reading") && at("reading") < at("arrived") && at("arrived") < at("failed"), "the file's order of states");
}

#[test]
fn what_a_start_could_not_use_or_a_page_could_not_show_is_not_offered() {
    let site = site();
    let pad = |size: usize| {
        let mut text = b"{\"pad\":\"".to_vec();
        text.resize(size - 2, b'a');
        text.extend_from_slice(b"\"}");
        assert_eq!(text.len(), size);
        text
    };
    site.graph("zeta", Some(b"{\"id\":\"zeta\"}"));
    site.graph("alpha", Some(b"{\"id\":\"alpha\"}"));
    site.graph("exact", Some(&pad(MAX_GRAPH)));
    site.graph("huge", Some(&pad(MAX_GRAPH + 1)));
    site.graph("array", Some(b"[1,2]"));
    site.graph("bom", Some(b"\xEF\xBB\xBF{}"));
    site.graph("half", Some(b"{\"id\":"));
    site.graph("no-json", None);
    std::fs::write(site.plugins.join("jev/graphs/no-rn.json"), "{}").unwrap();
    // A link in place of the file, and a folder in its place: neither is read.
    site.graph("linked", None);
    std::os::unix::fs::symlink(site.plugins.join("jev/graphs/alpha.json"), site.plugins.join("jev/graphs/linked.json")).unwrap();
    site.graph("dir", None);
    std::fs::create_dir(site.plugins.join("jev/graphs/dir.json")).unwrap();
    let perch = site.quiet(&["--run-graphs", "zeta,alpha,no-json,no-rn,huge,array,bom,half,exact,linked,dir,absent"]);

    let names: Vec<String> = graph_list(&perch).iter().map(|g| g["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(names, ["zeta", "alpha", "exact"], "only what a start accepts and a page can show, in the operator's order");
    for (name, want) in [("zeta", 200), ("alpha", 200), ("exact", 200), ("no-json", 404), ("no-rn", 404), ("linked", 404), ("dir", 404), ("absent", 404)] {
        let answer = perch.get(&format!("/hub/runs/graphs/{name}"));
        assert_eq!(answer.status, want, "{name}: {}", answer.head);
        if want == 404 {
            assert_eq!(answer.body, "", "{name}");
        }
    }
    let exact = body(&perch.get("/hub/runs/graphs/exact"));
    let sizes = (exact["bytes"].as_u64(), exact["graph"]["pad"].as_str().map(str::len));
    assert_eq!(sizes, (Some(MAX_GRAPH as u64), Some(MAX_GRAPH - 10)), "a file of exactly 1 MiB");
    for (name, want) in [("huge", 413), ("array", 502), ("bom", 502), ("half", 502)] {
        let answer = perch.get(&format!("/hub/runs/graphs/{name}"));
        assert_eq!(answer.status, want, "{name}: {}", answer.head);
        let said = body(&answer);
        assert!(said["error"].as_str().is_some_and(|s| !s.is_empty()) && said["error"] == said["why"], "{name}: {said}");
        assert!(!answer.body.contains(site.plugins.to_str().unwrap()), "{name}: a refusal names no path");
    }
    // Nothing offered is still a list.
    let none = site.quiet(&["--run-graphs", "absent,no-rn"]);
    assert_eq!(graph_list(&none), Vec::<Value>::new());
}

#[test]
fn with_no_plugins_dir_there_are_no_graphs_and_the_sentence_says_which_flag() {
    let site = site();
    let perch = Perch::builder()
        .member_config(MEMBER)
        .arg("--home")
        .arg(&site.home)
        .arg("--eidolon")
        .arg("/nonexistent/never-run-eidolon")
        .hold(site.base.clone())
        .start();
    for path in ["/hub/runs/graphs", "/hub/runs/graphs/wiki-hop"] {
        let answer = perch.get(path);
        assert_eq!(answer.status, 503, "{path}: {}{}", answer.head, answer.body);
        let said = body(&answer);
        assert!(said["error"].as_str().unwrap().contains("--plugins-dir") && said["error"] == said["why"], "{path}: {said}");
    }
    assert_empty(&perch.get("/hub/runs/graphs/not-offered"), 404, "a name outside the list is still a 404");
}

// ---------------------------------------------------------------------------
// The log
// ---------------------------------------------------------------------------

#[cfg(feature = "test-bins")]
mod log {
    use super::*;
    use std::io::Write as _;
    use std::time::{Duration, Instant};

    /// Every pid `runs.json` still names that is still a workflow, and its group, is killed: what a test that fails with a child alive
    /// leaves behind. Declared before the perch, so it runs after the perch is gone.
    struct Reaper(PathBuf);

    impl Drop for Reaper {
        fn drop(&mut self) {
            let Ok(text) = std::fs::read_to_string(self.0.join("runs.json")) else { return };
            let Ok(Value::Object(runs)) = serde_json::from_str::<Value>(&text) else { return };
            for run in runs.values() {
                let Some(pid) = run["pid"].as_u64() else { continue };
                let argv = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
                if argv.windows(8).any(|word| word == b"workflow") {
                    unsafe { libc::killpg(pid as libc::pid_t, libc::SIGKILL) };
                }
            }
        }
    }

    fn one(perch: &Perch, rid: &str) -> Value {
        let answer = perch.get(&format!("/hub/runs/{rid}"));
        assert_eq!(answer.status, 200, "{}{}", answer.head, answer.body);
        body(&answer)
    }

    fn assert_ndjson(answer: &Answer) {
        assert_eq!(answer.status, 200, "{}{}", answer.head, answer.body);
        assert_eq!(answer.header("content-type").as_deref(), Some("application/x-ndjson"), "{}", answer.head);
    }

    /// The lines of `text` whose ids are above `after`, as the stand-in prints them (each with its newline).
    fn above(text: &str, after: u64) -> String {
        text.lines().filter(|line| serde_json::from_str::<Value>(line).unwrap()["id"].as_u64().unwrap() > after).map(|line| format!("{line}\n")).collect()
    }

    #[test]
    fn the_log_is_what_is_above_after_in_order_and_one_question_of_an_unchanged_journal_is_asked_once() {
        let site = site();
        let journal = std::fs::read_to_string(CASE_A).unwrap();
        let eid = site.seed("r1", Some(&journal));
        let perch = site.perch(&[], &[]);
        assert_eq!(asks(&perch), 0, "nothing is asked until a page asks");

        // No cursor: the whole journal, byte for byte.
        let all = log_of(&perch, "r1", "");
        assert_ndjson(&all);
        assert_eq!((all.body.as_str(), ids(&all.body)), (journal.as_str(), range(0, 50)));
        assert_eq!(asks(&perch), 1);
        // Above 10: 11 to 50 in order, and not 10.
        let rest = log_of(&perch, "r1", "?after=10");
        assert_ndjson(&rest);
        assert_eq!((ids(&rest.body), rest.body.as_str()), (range(11, 50), above(&journal, 10).as_str()));
        assert_eq!(asks(&perch), 2);
        // The same question of the journal as it was: the same lines (a poll whose answer was lost is not told "nothing"), and no child.
        let again = log_of(&perch, "r1", "?after=10");
        assert_eq!((again.status, again.body.as_str()), (200, rest.body.as_str()));
        assert_eq!(asks(&perch), 2, "an unchanged journal and the same cursor start no eidolon");
        // At the end: an empty 200, asked once.
        for _ in 0..3 {
            let end = log_of(&perch, "r1", "?after=50");
            assert_ndjson(&end);
            assert_eq!(end.body, "");
        }
        assert_eq!(asks(&perch), 3, "an unchanged journal is asked about once, however often it is polled");
        // Past the end is the same empty answer, not an error; and zero is a cursor (record 0 is not above it).
        assert_eq!(log_of(&perch, "r1", "?after=999999999999").body, "");
        assert_eq!(ids(&log_of(&perch, "r1", "?after=0").body), range(1, 50));
        assert_eq!(asks(&perch), 5);

        // What eidolon was asked: the log of this run's journal as JSON, with the cursor as a plain number.
        let helpers: Vec<String> = perch.fake_helpers().into_iter().filter(|line| line.starts_with("helper log")).collect();
        let path = eid.display();
        assert_eq!(helpers.first().map(String::as_str), Some(format!("helper log --json {path} config=true").as_str()), "{helpers:?}");
        assert!(helpers.contains(&format!("helper log --json --after 10 {path} config=true")), "{helpers:?}");
        assert!(helpers.contains(&format!("helper log --json --after 999999999999 {path} config=true")), "{helpers:?}");
        assert!(helpers.iter().all(|line| line.starts_with("helper log --json ") && line.ends_with(" config=true")), "{helpers:?}");

        // A record appended is a journal that changed: the next poll asks again, once, and has the new line.
        std::fs::OpenOptions::new().append(true).open(&eid).unwrap().write_all(record(51).as_bytes()).unwrap();
        let moved = log_of(&perch, "r1", "?after=50");
        assert_eq!((ids(&moved.body), moved.status), (vec![51], 200));
        assert_eq!(asks(&perch), 6);
        assert_eq!(log_of(&perch, "r1", "?after=50").body, moved.body);
        assert_eq!(asks(&perch), 6, "and the changed journal is asked about once, too");
        // The leading zeros are the same number.
        assert_eq!(log_of(&perch, "r1", "?after=00051").body, "");
    }

    #[test]
    fn a_poll_sees_what_a_stepping_run_has_written_and_only_that() {
        let site = site();
        site.graph("wiki-hop", None);
        let _reaper = Reaper(site.home.clone());
        // The stand-in pauses 200 ms before each step, so the journal grows for about three seconds while the poll is going.
        let perch = site.perch(&[], &[("FAKE_WORKFLOW_CASE", "a"), ("FAKE_WORKFLOW_STEP_MS", "200")]);
        let started = perch.post_json("/hub/runs", json!({ "graph": "wiki-hop", "args": { "start": "Ancient_Egypt", "goal": "Felidae" } }));
        assert_eq!(started.status, 202, "{}{}", started.head, started.body);
        let rid = body(&started)["rid"].as_str().unwrap().to_string();

        let (mut cursor, mut seen, mut mid_run, begun) = (None::<u64>, Vec::<u64>::new(), 0, Instant::now());
        loop {
            assert!(begun.elapsed() < Duration::from_secs(40), "the run did not end in time; the log so far: {seen:?}");
            // The state is asked before the log is polled: "done" then means the journal was complete, so an empty batch after it
            // means every record has been read. Asked after the poll, a record written between the two could be missed for good.
            let state = one(&perch, &rid)["state"].as_str().unwrap().to_string();
            let answer = log_of(&perch, &rid, &cursor.map(|at| format!("?after={at}")).unwrap_or_default());
            assert_ndjson(&answer);
            let batch = ids(&answer.body);
            // Only ids above the cursor, the next one first, in order, with no gap.
            if let Some(first) = batch.first() {
                assert_eq!(*first, cursor.map_or(0, |at| at + 1), "the batch starts right above the cursor {cursor:?}: {batch:?}");
            }
            assert!(batch.windows(2).all(|pair| pair[1] == pair[0] + 1), "in order: {batch:?}");
            // Asked before the poll: a run still running then was still being written while the poll read it.
            if !batch.is_empty() && state == "running" {
                mid_run += 1;
            }
            seen.extend(&batch);
            cursor = seen.last().copied();
            if state == "done" && batch.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(seen, range(0, 50), "every record of case a once, in order");
        assert!(mid_run >= 3, "records arrived in {mid_run} batches while the run was going; the poll must see it grow");
        // The run is over and the journal is at rest: polling on costs no child.
        let before = asks(&perch);
        for _ in 0..5 {
            assert_eq!(log_of(&perch, &rid, "?after=50").body, "");
        }
        assert!(asks(&perch) <= before + 1, "five polls of a journal at rest asked eidolon {} times", asks(&perch) - before);
    }

    #[test]
    fn a_query_that_is_not_one_after_is_a_400_and_asks_eidolon_nothing() {
        let site = site();
        site.seed("r1", Some(&journal_of(5)));
        let perch = site.perch(&[], &[]);
        for query in [
            "?after=x", "?after=", "?after=-1", "?after=+1", "?after=1.5", "?after=%31", "?after=1&after=2", "?foo=1", "?after=1&foo=2", "?after",
            "?After=1", "?after=18446744073709551616", "?after=--json", "?after=1;2", "?%61fter=1",
        ] {
            let answer = log_of(&perch, "r1", query);
            assert_eq!(answer.status, 400, "{query}: {}{}", answer.head, answer.body);
            let said = body(&answer);
            assert!(said["error"].as_str().is_some_and(|s| !s.is_empty()) && said["error"] == said["why"], "{query}: {said}");
        }
        assert_eq!(asks(&perch), 0, "a refused query reached no eidolon");
        // The largest cursor there is, and a bare `?`, are questions.
        assert_eq!(log_of(&perch, "r1", "?after=18446744073709551615").body, "");
        assert_eq!(ids(&log_of(&perch, "r1", "?").body), range(0, 4));
        assert_eq!(asks(&perch), 2);
    }

    #[test]
    fn a_run_whose_journal_is_not_there_yet_has_an_empty_log_and_costs_no_child() {
        let site = site();
        site.seed("r2", None);
        let perch = site.perch(&[], &[]);
        for query in ["", "?after=3"] {
            let answer = log_of(&perch, "r2", query);
            assert_ndjson(&answer);
            assert_eq!(answer.body, "", "{query}");
        }
        assert_eq!(asks(&perch), 0, "there is nothing to ask about");
        // The journal appears while the perch is up (the child has made it): the next poll finds it.
        std::fs::write(site.dir("r2").join("1000.eid"), journal_of(3)).unwrap();
        assert_eq!(ids(&log_of(&perch, "r2", "").body), range(0, 2));
        assert_eq!(asks(&perch), 1);
    }

    #[test]
    fn two_polls_at_once_make_one_child() {
        let site = site();
        site.seed("r1", Some(&journal_of(30)));
        let perch = site.perch(&[], &[]);
        let (addr, auth) = (perch.addr.clone(), perch.auth());
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let (addr, auth, barrier) = (addr.clone(), auth.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    Request::new(&addr, "GET", "/hub/runs/r1/log?after=5").bearer(auth).send(&addr)
                })
            })
            .collect();
        for thread in threads {
            let answer = thread.join().unwrap();
            assert_eq!((answer.status, ids(&answer.body)), (200, range(6, 29)));
        }
        assert_eq!(asks(&perch), 1, "the polls shared the one question");
    }

    #[test]
    fn eidolon_failing_is_a_502_that_is_not_remembered() {
        let site = site();
        site.seed("r1", Some(&journal_of(5)));
        let perch = site.perch(&[], &[("FAKE_HELPER_FAIL", "1")]);
        for _ in 0..2 {
            let answer = log_of(&perch, "r1", "?after=1");
            assert_eq!(answer.status, 502, "{}{}", answer.head, answer.body);
            let said = body(&answer);
            assert!(said["error"].as_str().unwrap().starts_with("eidolon could not read that log"), "{said}");
            assert_eq!(said["error"], said["why"]);
        }
        assert_eq!(asks(&perch), 2, "a failure is asked again, not kept");
    }

    #[test]
    fn a_log_over_the_cap_is_a_413_and_a_cursor_past_it_is_not() {
        let site = site();
        // 170 records of 100 KB: over the 16 MiB a log may be, when it is asked for whole.
        let pad = "x".repeat(100_000);
        let big: String = (0..170).map(|id| format!("{{\"id\":{id},\"pad\":\"{pad}\"}}\n")).collect();
        assert!(big.len() > 16 * 1024 * 1024);
        site.seed("r1", Some(&big));
        let perch = site.perch(&[], &[]);
        let whole = log_of(&perch, "r1", "");
        assert_eq!(whole.status, 413, "{}", whole.head);
        assert!(body(&whole)["error"].as_str().unwrap().contains("too large"), "{}", whole.body);
        let tail = log_of(&perch, "r1", "?after=168");
        assert_eq!((tail.status, ids(&tail.body)), (200, vec![169]));
    }

    #[test]
    fn an_answer_over_1_mib_is_given_every_time_and_a_small_one_is_kept() {
        let site = site();
        // 12 records of 100 KB: 1.2 MB when asked whole, over the 1 MiB an answer is kept at and far under the 16 MiB a log may be.
        let pad = "x".repeat(100_000);
        let big: String = (0..12).map(|id| format!("{{\"id\":{id},\"pad\":\"{pad}\"}}\n")).collect();
        assert!(big.len() > 1024 * 1024 && big.len() < 2 * 1024 * 1024);
        site.seed("r1", Some(&big));
        let perch = site.perch(&[], &[]);
        for want in 1..=2 {
            let answer = log_of(&perch, "r1", "");
            assert_eq!((answer.status, answer.body.len()), (200, big.len()));
            assert_eq!(asks(&perch), want, "over 1 MiB an answer is not kept, so it is asked again");
        }
        for _ in 0..2 {
            assert_eq!(ids(&log_of(&perch, "r1", "?after=9").body), vec![10, 11]);
        }
        assert_eq!(asks(&perch), 3, "a small answer of the same journal is kept");
    }
}
