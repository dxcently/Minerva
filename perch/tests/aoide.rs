//! The Aoide runner (slice R2a): `perch::aoide`, driven through `fake_aoide` and nothing else. The real `aoide` is never run here, and
//! `~/.aoide` is never touched: the fake replays `tests/fixtures/aoide/` and logs every call it gets, refused ones included.
//!
//! Each test gets its own log and its own steering of the fake through `Aoide::env` (`FAKE_AOIDE_LOG`, `FAKE_AOIDE_DIR`, the knobs), because
//! the process environment is shared by every test thread. The two things that are about the *process* environment (what a perch started
//! under a conducted session, or with a stale `aoide` first on `PATH`, hands its children) run in a second copy of this test binary with
//! exactly that environment: `no_path_lookup_and_no_ambient_identity` starts `inner_environment_probe`.
//!
//! Run with `cargo test --features test-bins`: the fake is a `test-bins` target, so without the feature this file is empty.
#![cfg(feature = "test-bins")]

mod common;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use perch::aoide::{Aoide, Error, Limits, Reply, Sender, Shape, To, READER, STDERR_KEEP, STDOUT_CAP, TEXT_MAX};
use serde_json::Value;

// ---------------------------------------------------------------------------
// The fake, and what it was asked
// ---------------------------------------------------------------------------

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/aoide")
}

fn fixture(name: &str) -> Value {
    let text = std::fs::read_to_string(fixtures().join(name)).unwrap_or_else(|e| panic!("fixture {name}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("fixture {name}: {e}"))
}

struct Fake {
    _dir: tempfile::TempDir,
    log: PathBuf,
}

impl Fake {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("fake-aoide.log");
        Self { _dir: dir, log }
    }

    /// A runner for the fake that logs here and replays the repository's fixtures.
    fn aoide(&self) -> Aoide {
        Aoide::new(common::fake_aoide().expect("the stand-in aoide")).env("FAKE_AOIDE_LOG", &self.log).env("FAKE_AOIDE_DIR", fixtures())
    }

    /// Every line the fake logged: a call has an `argv`, an event has an `event`.
    fn lines(&self) -> Vec<Value> {
        let text = std::fs::read_to_string(&self.log).unwrap_or_default();
        text.lines().map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("log line {line:?}: {e}"))).collect()
    }

    fn calls(&self) -> Vec<Value> {
        self.lines().into_iter().filter(|line| line.get("argv").is_some()).collect()
    }

    fn argvs(&self) -> Vec<Vec<String>> {
        self.calls().iter().map(|call| call["argv"].as_array().unwrap().iter().map(|a| a.as_str().unwrap().to_string()).collect()).collect()
    }

    /// The fake run by hand, with no environment but its own steering: what it does with an argv the runner would never build.
    fn raw(&self, argv: &[&str], env: &[(&str, &str)]) -> std::process::Output {
        Command::new(common::fake_aoide().unwrap())
            .args(argv)
            .env_clear()
            .env("FAKE_AOIDE_LOG", &self.log)
            .env("FAKE_AOIDE_DIR", fixtures())
            .envs(env.iter().copied())
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }
}

fn words(words: &[&str]) -> Vec<String> {
    words.iter().map(|word| word.to_string()).collect()
}

fn node(name: &str) -> To {
    To::Node(name.to_string())
}

fn letter(to: To, mesh: Option<&str>, hold: bool, subject: &str, text: &str) -> Shape {
    Shape::MailSend { to, from: Sender::Minerva, mesh: mesh.map(str::to_string), subject: subject.to_string(), hold, text: text.to_string() }
}

fn failed(outcome: Result<Reply, Error>) -> Reply {
    match outcome {
        Err(Error::Failed(reply)) => reply,
        other => panic!("expected aoide to say no, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// The argv
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_shape_runs_with_its_exact_argv() {
    let fake = Fake::new();
    let aoide = fake.aoide();
    assert_eq!(aoide.version().await.unwrap().as_deref(), Some("0.0.26"));
    aoide.run(&Shape::MeshJson).await.unwrap();
    aoide.run(&Shape::MailOutboxJson).await.unwrap();
    let plain = aoide.run(&Shape::MailRead { reread: false }).await.unwrap();
    let again = aoide.run(&Shape::MailRead { reread: true }).await.unwrap();
    assert_eq!(plain.data().unwrap()["entries"].as_array().unwrap().len(), 8);
    assert_eq!(again.data().unwrap()["entries"].as_array().unwrap().len(), 9, "--reread prints the old letter too");
    aoide.run(&letter(To::SelfNode, None, false, "to myself", "a note")).await.unwrap();
    aoide.run(&letter(node("osaka"), None, false, "to osaka", "a note")).await.unwrap();
    let agent = Shape::MailSend {
        to: node("multi"),
        from: Sender::MinervaAgent,
        mesh: Some("alpha".to_string()),
        subject: "from the agent".to_string(),
        hold: false,
        text: "a note".to_string(),
    };
    aoide.run(&agent).await.unwrap();
    aoide.run(&letter(node("osaka"), None, true, "held", "a note")).await.unwrap();
    assert_eq!(
        fake.argvs(),
        vec![
            words(&["--version"]),
            words(&["mesh", "--json"]),
            words(&["mail", "outbox", "--json"]),
            words(&["mail", "read", "--for=minerva", "--json"]),
            words(&["mail", "read", "--for=minerva", "--json", "--reread"]),
            words(&["mail", "send", "--json", "--to=self/minerva", "--from=minerva", "--subject=to myself", "--", "a note"]),
            words(&["mail", "send", "--json", "--to=osaka/minerva", "--from=minerva", "--subject=to osaka", "--", "a note"]),
            words(&["mail", "send", "--json", "--to=multi/minerva", "--from=minerva-agent", "--mesh=alpha", "--subject=from the agent", "--", "a note"]),
            words(&["mail", "send", "--json", "--to=osaka/minerva", "--from=minerva", "--subject=held", "--hold", "--", "a note"]),
        ]
    );
}

#[tokio::test]
async fn the_text_arrives_as_the_last_argument_byte_for_byte() {
    let fake = Fake::new();
    let aoide = fake.aoide();
    let big = "ünï".repeat(TEXT_MAX / 5);
    assert!(big.len() <= TEXT_MAX && big.len() > TEXT_MAX - 5);
    let texts = [
        "--help",
        "--",
        "-- --json --to=evil/minerva",
        "ends with --hold --",
        "--subject=forged",
        "line one\nline two\r\n\ttabbed\n",
        "世界 \u{1f600} \u{202e} \u{7f}",
        "\u{1b}[31mred\u{1b}[0m",
        "$(touch pwned) `id` ; | & > < \\ \" '",
        "  leading and trailing  ",
        "x",
        big.as_str(),
    ];
    for text in texts {
        aoide.run(&letter(node("osaka"), None, false, "s", text)).await.unwrap_or_else(|e| panic!("{:?}: {e}", text.chars().take(20).collect::<String>()));
    }
    let argvs = fake.argvs();
    assert_eq!(argvs.len(), texts.len());
    for (argv, text) in argvs.iter().zip(texts) {
        assert_eq!(argv.last().map(String::as_str), Some(text), "the text is the last argument, unchanged");
        assert_eq!(argv[argv.len() - 2], "--");
        assert_eq!(argv.len(), 8, "one word of text and no more: {:?}", &argv[..argv.len() - 1]);
    }
}

#[tokio::test]
async fn a_subject_that_is_a_command_word_passes_in_the_equals_form() {
    let fake = Fake::new();
    let aoide = fake.aoide();
    for subject in ["status", "send", "mail", "node", "--hold", "--help", "--", "-x"] {
        aoide.run(&letter(node("osaka"), None, false, subject, "t")).await.unwrap_or_else(|e| panic!("{subject}: {e}"));
    }
    for (argv, subject) in fake.argvs().iter().zip(["status", "send", "mail", "node", "--hold", "--help", "--", "-x"]) {
        assert!(argv.contains(&format!("--subject={subject}")), "{argv:?}");
        assert_eq!(argv.iter().filter(|word| word.starts_with("--subject")).count(), 1, "{argv:?}");
    }
}

#[tokio::test]
async fn what_is_refused_is_refused_before_any_spawn() {
    let fake = Fake::new();
    let aoide = fake.aoide();
    let refused = [
        (letter(node("osaka"), None, false, "two\nlines", "t"), "one line"),
        (letter(node("osaka"), None, false, "carriage\rreturn", "t"), "one line"),
        (letter(node("osaka"), Some("Lab"), false, "s", "t"), "lower case"),
        (letter(node("osaka"), Some("lab; rm -rf"), false, "s", "t"), "lower case"),
        (letter(node("osaka"), Some(""), false, "s", "t"), "lower case"),
        (letter(To::SelfNode, None, true, "s", "t"), "hold needs another node"),
        (letter(node("self"), None, true, "s", "t"), "self is this machine"),
        (letter(node("../etc"), None, false, "s", "t"), "lower case"),
        (letter(node("osaka"), None, false, &"s".repeat(121), "t"), "120 characters"),
        (letter(node("osaka"), None, false, "s", &"t".repeat(TEXT_MAX + 1)), "64 KiB"),
        (letter(node("osaka"), None, false, "s", ""), "needs text"),
        (letter(To::SelfNode, Some("lab"), false, "s", "t"), "no mesh"),
    ];
    for (shape, says) in &refused {
        match aoide.run(shape).await {
            Err(Error::Refused(sentence)) => assert!(sentence.contains(says), "{shape:?}: {sentence:?} does not say {says:?}"),
            other => panic!("{shape:?} was not refused: {other:?}"),
        }
    }
    assert_eq!(fake.lines(), Vec::<Value>::new(), "not one of them started a process");
    assert!(!fake.log.exists(), "the fake never ran, so it never even created its log");
}

// ---------------------------------------------------------------------------
// The child's environment
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_reader_identity_is_set_on_reads_only() {
    let fake = Fake::new();
    let aoide = fake.aoide();
    let shapes = [
        Shape::Version,
        Shape::MeshJson,
        Shape::MailOutboxJson,
        Shape::MailRead { reread: false },
        Shape::MailRead { reread: true },
        letter(To::SelfNode, None, false, "s", "t"),
        letter(node("osaka"), None, true, "s", "t"),
    ];
    for shape in &shapes {
        aoide.run(shape).await.unwrap_or_else(|e| panic!("{shape:?}: {e}"));
    }
    let sessions: Vec<Option<String>> = fake.calls().iter().map(|call| call["session"].as_str().map(str::to_string)).collect();
    let reader = Some(READER.to_string());
    assert_eq!(sessions, vec![None, None, None, reader.clone(), reader, None, None], "only the two reads carry an identity");
}

#[tokio::test]
async fn the_four_places_reach_the_child_as_their_variables() {
    let fake = Fake::new();
    let aoide = fake
        .aoide()
        .root(Some("/r/aoide-home".into()))
        .audit_log(Some("/r/events.log".into()))
        .daemon_socket(Some("/r/d.sock".into()))
        .runtime_dir(Some("/r/run".into()));
    aoide.run(&Shape::MeshJson).await.unwrap();
    let call = &fake.calls()[0];
    let want = serde_json::json!({ "AOIDE_ROOT": "/r/aoide-home", "AOIDE_AUDIT_LOG": "/r/events.log", "AOIDE_DAEMON_SOCKET": "/r/d.sock" });
    assert_eq!(call["aoide_env"], want);
    assert_eq!(call["xdg_runtime_dir"], "/r/run");
}

fn alive(pid: i64) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// Run by `no_path_lookup_and_no_ambient_identity` in a copy of this binary whose environment is the thing under test: a stale `aoide` first
/// on `PATH`, a perch's worth of ambient `AOIDE_*` variables, and a stdin that is a pipe nobody closes. By hand it only complains.
#[tokio::test]
#[ignore = "started by no_path_lookup_and_no_ambient_identity, which gives it the environment it needs"]
async fn inner_environment_probe() {
    let log = std::env::var_os("PROBE_LOG").expect("PROBE_LOG: this test is started by no_path_lookup_and_no_ambient_identity");
    let aoide = Aoide::new(common::fake_aoide().unwrap())
        .env("FAKE_AOIDE_LOG", log)
        .env("FAKE_AOIDE_DIR", fixtures())
        .env("FAKE_AOIDE_STDIN", "1")
        .limits(Limits { timeout: Duration::from_secs(3), ..Limits::default() });
    // A bare name is what `Command::new` would look up on PATH, and a stale `aoide` is first there.
    for bare in ["aoide", "./aoide"] {
        let outcome = Aoide::new(bare).run(&Shape::Version).await;
        assert!(matches!(outcome, Err(Error::Refused(_))), "{bare}: {outcome:?}");
    }
    let shapes = [
        Shape::Version,
        Shape::MeshJson,
        Shape::MailOutboxJson,
        Shape::MailRead { reread: false },
        letter(To::SelfNode, None, false, "s", "t"),
        letter(node("osaka"), None, true, "s", "t"),
    ];
    for shape in &shapes {
        aoide.run(shape).await.unwrap_or_else(|e| panic!("{shape:?}: {e}"));
    }
    println!("inner probe ok");
}

#[test]
fn no_path_lookup_and_no_ambient_identity() {
    let dir = tempfile::tempdir().unwrap();
    let stale = dir.path().join("stale");
    std::fs::create_dir(&stale).unwrap();
    let (marker, log) = (dir.path().join("stale-ran"), dir.path().join("probe.log"));
    let script = stale.join("aoide");
    std::fs::write(&script, "#!/bin/sh\necho ran >> \"$STALE_MARKER\"\nexit 98\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut inner = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "inner_environment_probe", "--nocapture", "--test-threads=1"])
        .env_clear()
        .env("PATH", format!("{}:/usr/bin:/bin", stale.display()))
        .env("STALE_MARKER", &marker)
        .env("PROBE_LOG", &log)
        .env("AOIDE_SESSION_ID", "ambient-session")
        .env("AOIDE_STATE_DIR", "/ambient/state")
        .env("AOIDE_STAGE_DIR", "/ambient/stage")
        .env("AOIDE_CONDUCT_AUTOGATE", "1")
        .env("AOIDE_ROOT", "/ambient/root")
        .env("AOIDE_AUDIT_LOG", "/ambient/audit.log")
        .env("AOIDE_DAEMON_SOCKET", "/ambient/aoided.sock")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Held open until the probe is done: a child that inherited it would wait on it forever.
    let held = inner.stdin.take();
    let out = inner.wait_with_output().unwrap();
    drop(held);
    let (stdout, stderr) = (String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success() && stdout.contains("inner probe ok") && stdout.contains("1 passed"), "the probe failed:\n{stdout}\n{stderr}");
    assert!(!marker.exists(), "the stale aoide on PATH was run");

    let text = std::fs::read_to_string(&log).unwrap();
    let lines: Vec<Value> = text.lines().map(|line| serde_json::from_str(line).unwrap()).collect();
    let calls: Vec<&Value> = lines.iter().filter(|line| line.get("argv").is_some()).collect();
    assert_eq!(calls.len(), 6, "the probe's six calls reached the fake, and nothing else did");
    for call in &calls {
        // Whatever the perch itself was started under, the child has none of it: the paths are the runner's to give (and it gave none).
        assert_eq!(call["aoide_env"], serde_json::json!({}), "{call}");
    }
    let sessions: Vec<Option<&str>> = calls.iter().map(|call| call["session"].as_str()).collect();
    assert_eq!(sessions, [None, None, None, Some(READER), None, None], "the ambient session id reached no call, and the reader reached the read");
    let stdins: Vec<&Value> = lines.iter().filter(|line| line["event"] == "stdin").collect();
    assert_eq!(stdins.len(), 6);
    assert!(stdins.iter().all(|line| line["bytes"] == 0), "stdin is closed for every call: {stdins:?}");
}

// ---------------------------------------------------------------------------
// The limits
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_timeout_kills_a_fake_that_sleeps() {
    let fake = Fake::new();
    let aoide = fake.aoide().env("FAKE_AOIDE_SLEEP_MS", "1500").limits(Limits { timeout: Duration::from_millis(300), ..Limits::default() });
    let begun = Instant::now();
    let outcome = aoide.run(&Shape::MeshJson).await;
    assert!(matches!(outcome, Err(Error::Timeout(_))), "{outcome:?}");
    assert!(begun.elapsed() < Duration::from_millis(1200), "it answered at the timeout and not when the child chose: {:?}", begun.elapsed());
    assert!(outcome.unwrap_err().to_string().contains("did not answer"));
    let pid = fake.calls()[0]["pid"].as_i64().unwrap();
    assert!(!alive(pid), "the child was killed and reaped");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(fake.lines().iter().all(|line| line.get("event").is_none()), "a killed fake never woke: {:?}", fake.lines());
}

#[tokio::test]
async fn the_stdout_cap_holds_to_the_byte() {
    let fake = Fake::new();
    let exact = fake.aoide().env("FAKE_AOIDE_FLOOD", STDOUT_CAP.to_string()).run(&Shape::Version).await.unwrap();
    assert_eq!(exact.stdout.len(), STDOUT_CAP, "exactly the cap is taken whole");
    let over = fake.aoide().env("FAKE_AOIDE_FLOOD", (STDOUT_CAP + 1).to_string()).run(&Shape::Version).await;
    assert!(matches!(over, Err(Error::TooBig(cap)) if cap == STDOUT_CAP), "{over:?}");
    assert!(over.unwrap_err().to_string().contains("more than 1048576 bytes"));
}

#[tokio::test]
async fn a_flood_is_stopped_at_the_cap_and_the_child_is_killed() {
    let fake = Fake::new();
    let begun = Instant::now();
    // A gigabyte on offer.
    let over = fake.aoide().env("FAKE_AOIDE_FLOOD", "1073741824").run(&Shape::MeshJson).await;
    assert!(matches!(over, Err(Error::TooBig(_))), "{over:?}");
    assert!(begun.elapsed() < Duration::from_secs(5), "{:?}", begun.elapsed());
    assert!(!alive(fake.calls()[0]["pid"].as_i64().unwrap()), "the child was killed and reaped");
}

#[tokio::test]
async fn a_child_that_fills_stderr_is_not_stalled_and_only_its_start_is_kept() {
    let fake = Fake::new();
    // Two megabytes on a pipe that holds sixty-four KiB: a runner that read stdout to the end first would wait for ever.
    let aoide = fake.aoide().env("FAKE_AOIDE_STDERR_FLOOD", "2097152").limits(Limits { timeout: Duration::from_secs(5), ..Limits::default() });
    let begun = Instant::now();
    let reply = aoide.run(&Shape::Version).await.unwrap();
    assert!(begun.elapsed() < Duration::from_secs(4), "{:?}", begun.elapsed());
    assert_eq!(reply.stdout.trim(), "aoide 0.0.26");
    assert_eq!(reply.stderr.len(), STDERR_KEEP);
}

#[tokio::test]
async fn a_process_that_outlives_the_child_on_its_pipe_does_not_hold_the_call() {
    let fake = Fake::new();
    // The fake leaves a `sleep 4` that holds stdout and stderr, as a daemon an aoide started would.
    let aoide = fake.aoide().env("FAKE_AOIDE_STRAGGLER_MS", "4000");
    let begun = Instant::now();
    let reply = aoide.run(&Shape::MeshJson).await.unwrap();
    assert!(begun.elapsed() < Duration::from_secs(2), "the call went on with what it had: {:?}", begun.elapsed());
    assert_eq!(reply.data().unwrap()["report"]["sections"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn a_json_call_that_prints_something_else_is_not_json_and_a_version_may_be() {
    let fake = Fake::new();
    let outcome = fake.aoide().env("FAKE_AOIDE_FLOOD", "10").run(&Shape::MeshJson).await;
    assert!(matches!(outcome, Err(Error::NotJson)), "{outcome:?}");
    let version = fake.aoide().env("FAKE_AOIDE_FLOOD", "10").run(&Shape::Version).await.unwrap();
    assert_eq!(version.stdout, "xxxxxxxxxx");
}

// ---------------------------------------------------------------------------
// What aoide says
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_multi_mesh_refusal_sentence_is_returned_verbatim_and_a_resend_with_a_mesh_passes() {
    let fake = Fake::new();
    let aoide = fake.aoide();
    let said = fixture("send.json")["multi"]["stdout"]["message"].as_str().unwrap().to_string();
    assert!(said.contains("trusted in more than one mesh"), "{said}");
    let reply = failed(aoide.run(&letter(node("multi"), None, false, "s", "t")).await);
    assert_eq!((reply.code, reply.status()), (1, Some("error")));
    assert_eq!(reply.message(), Some(said.as_str()));
    assert_eq!(reply.sentence(), said);
    assert_eq!(reply.data().unwrap()["reason"], "ambiguous-mesh");
    assert_eq!(reply.data().unwrap()["meshes"], serde_json::json!(["alpha", "beta"]), "a caller can offer the choice");
    let as_error = aoide.run(&letter(node("multi"), None, false, "s", "t")).await.unwrap_err();
    assert_eq!(as_error.to_string(), said, "what a caller shows is aoide's sentence and nothing added");

    let sent = aoide.run(&letter(node("multi"), Some("alpha"), false, "s", "t")).await.unwrap();
    assert_eq!(sent.data().unwrap()["msgid"], "8".repeat(64));
    let argvs = fake.argvs();
    assert!(!argvs[0].iter().any(|word| word.starts_with("--mesh")), "{:?}", argvs[0]);
    assert!(argvs[2].contains(&"--mesh=alpha".to_string()), "{:?}", argvs[2]);
}

#[tokio::test]
async fn unknown_and_unpaired_nodes_each_say_why() {
    let fake = Fake::new();
    let aoide = fake.aoide();
    let ghost = failed(aoide.run(&letter(node("ghost"), None, false, "s", "t")).await);
    assert_eq!(ghost.sentence(), "no node named `ghost`");
    assert_eq!(ghost.data().unwrap()["reason"], "unknown-node");
    let bridge = failed(aoide.run(&letter(node("bridge"), None, false, "s", "t")).await);
    assert!(bridge.sentence().contains("not paired"), "{}", bridge.sentence());
    assert_eq!(bridge.data().unwrap()["reason"], "unpaired-node");
    assert_eq!(bridge.data().unwrap()["name"], "bridge");
}

#[tokio::test]
async fn a_send_answers_with_what_was_scripted_for_its_node_and_its_hold() {
    let fake = Fake::new();
    let aoide = fake.aoide();
    let filed = aoide.run(&letter(To::SelfNode, None, false, "s", "t")).await.unwrap();
    assert_eq!(filed.data().unwrap()["envelope"]["msgid"], "5".repeat(64), "a self send answers with the mailbase entry");
    assert_eq!(filed.data().unwrap()["disposition"], "filed");
    let spooled = aoide.run(&letter(node("osaka"), None, false, "s", "t")).await.unwrap();
    assert_eq!(spooled.data().unwrap()["msgid"], "6".repeat(64));
    assert_eq!(spooled.data().unwrap()["disposition"], "spooled");
    assert_eq!(spooled.data().unwrap()["delivery"]["status"], "queued");
    let held = aoide.run(&letter(node("osaka"), None, true, "s", "t")).await.unwrap();
    assert_eq!(held.data().unwrap()["msgid"], "7".repeat(64));
    assert_eq!(held.data().unwrap()["disposition"], "held");
}

// ---------------------------------------------------------------------------
// The fake keeps its half of the bargain
// ---------------------------------------------------------------------------

#[test]
fn the_fake_refuses_every_argv_the_runner_would_not_build() {
    let fake = Fake::new();
    let refused: Vec<Vec<&str>> = vec![
        vec![],
        vec!["--help"],
        vec!["--version", "--json"],
        vec!["mesh"],
        vec!["mesh", "--json", "--extra"],
        vec!["mail"],
        vec!["mail", "outbox"],
        vec!["mail", "outbox", "retry", "--refused"],
        vec!["mail", "read", "--for", "minerva", "--json"],
        vec!["mail", "read", "--for=other", "--json"],
        vec!["mail", "read", "--all-names", "--json"],
        vec!["mail", "read", "--json", "--for=minerva"],
        vec!["mail", "poll"],
        vec!["mail", "export"],
        vec!["node", "list", "--json"],
        vec!["node", "status", "osaka"],
        vec!["session", "list", "--json"],
        vec!["pair", "osaka"],
        vec!["spawn"],
        vec!["dispatch"],
        vec!["send", "--to=osaka/minerva", "--", "t"],
        vec!["mail", "send", "--to=osaka/minerva", "--from=minerva", "--subject=s", "--", "t"],
        vec!["mail", "send", "--json", "--to", "osaka/minerva", "--from=minerva", "--subject=s", "--", "t"],
        vec!["mail", "send", "--json", "--to=osaka/minerva", "--from=minerva", "--subject=s", "t"],
        vec!["mail", "send", "--json", "--to=osaka/minerva", "--from=minerva", "--subject=s", "--", "t", "u"],
        vec!["mail", "send", "--json", "--to=osaka/minerva", "--from=minerva", "--subject=s", "--", ""],
        vec!["mail", "send", "--json", "--to=osaka/minerva", "--from=evil", "--subject=s", "--", "t"],
        vec!["mail", "send", "--json", "--to=osaka/other", "--from=minerva", "--subject=s", "--", "t"],
        vec!["mail", "send", "--json", "--to=Osaka/minerva", "--from=minerva", "--subject=s", "--", "t"],
        vec!["mail", "send", "--json", "--to=osaka/minerva", "--from=minerva", "--subject=a\nb", "--", "t"],
        vec!["mail", "send", "--json", "--to=osaka/minerva", "--from=minerva", "--subject=", "--", "t"],
        vec!["mail", "send", "--json", "--to=self/minerva", "--from=minerva", "--subject=s", "--hold", "--", "t"],
        vec!["mail", "send", "--json", "--to=self/minerva", "--from=minerva", "--mesh=lab", "--subject=s", "--", "t"],
        vec!["mail", "send", "--json", "--to=osaka/minerva", "--from=minerva", "--subject=s", "--hold", "--mesh=lab", "--", "t"],
        vec!["mail", "send", "--json", "--to=osaka/minerva", "--from=minerva", "--mesh=Lab", "--subject=s", "--", "t"],
        vec!["mail", "send", "--json", "--from=minerva", "--to=osaka/minerva", "--subject=s", "--", "t"],
    ];
    for argv in &refused {
        let out = fake.raw(argv, &[("AOIDE_SESSION_ID", READER)]);
        assert_eq!(out.status.code(), Some(99), "{argv:?}: {out:?}");
        assert!(out.stdout.is_empty(), "{argv:?}: a refusal prints nothing on stdout, which a caller would parse");
        assert!(!out.stderr.is_empty(), "{argv:?}: and says why on stderr");
    }
    assert_eq!(fake.calls().len(), refused.len(), "every refused call is in the log too");
}

#[test]
fn the_fake_answers_a_mail_read_only_to_the_reader() {
    let fake = Fake::new();
    let read = ["mail", "read", "--for=minerva", "--json"];
    let not_the_reader: [&[(&str, &str)]; 4] = [&[], &[("AOIDE_SESSION_ID", "")], &[("AOIDE_SESSION_ID", "someone-else")], &[("AOIDE_SESSION_ID", "minerva")]];
    for env in not_the_reader {
        let out = fake.raw(&read, env);
        assert_eq!(out.status.code(), Some(99), "{env:?}: {out:?}");
        assert!(out.stdout.is_empty());
    }
    let out = fake.raw(&read, &[("AOIDE_SESSION_ID", READER)]);
    assert_eq!(out.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&out.stdout).contains("\"command\": \"mail.read\""));
    // The other shapes ask nobody who they are.
    assert_eq!(fake.raw(&["mesh", "--json"], &[]).status.code(), Some(0));
}

#[test]
fn the_fixtures_hold_the_cases_the_other_slices_lean_on() {
    let entries = fixture("mail-read.json")["data"]["entries"].as_array().unwrap().clone();
    let name = |entry: &Value| entry["envelope"]["header"]["from"]["name"].as_str().unwrap_or_default().to_string();
    let text = |entry: &Value| entry["envelope"]["text"].as_str().unwrap_or_default().to_string();
    let marker = "AOIDE-LETTER/1\n";
    let structured = |entry: &Value| text(entry).strip_prefix(marker).is_some_and(|rest| serde_json::from_str::<Value>(rest).is_ok());
    assert!(entries.iter().any(|e| name(e) == "minerva" && structured(e)), "a structured letter from the operator's page");
    assert!(entries.iter().any(|e| name(e) == "minerva-agent" && structured(e)), "a structured letter from an agent");
    assert!(entries.iter().any(|e| name(e) == "minerva" && !text(e).starts_with(marker)), "a raw note from the page");
    assert!(entries.iter().any(|e| !["minerva", "minerva-agent"].contains(&name(e).as_str()) && !text(e).starts_with(marker)), "a peer's raw text");
    let sent = fixture("send.json")["osaka"]["stdout"]["data"]["msgid"].as_str().unwrap().to_string();
    assert!(entries.iter().any(|e| e["type"] == "receipt" && text(e) == sent), "a receipt whose text is the msgid osaka's send returned");
    assert!(entries.iter().any(|e| text(e).starts_with(marker) && !structured(e)), "a broken AOIDE-LETTER");
    assert!(entries.iter().any(|e| text(e).contains("<script>") && text(e).contains('\u{1b}')), "hostile text");
    assert!(entries.iter().any(|e| e["envelope"]["header"]["from"]["node"].as_str().is_some_and(|n| n.contains("<b>"))), "hostile header fields");
    assert_eq!(fixture("mail-read-reread.json")["data"]["entries"].as_array().unwrap().len(), entries.len() + 1);

    let send = fixture("send.json");
    for node in ["self", "osaka", "bridge", "multi"] {
        assert!(send.get(node).is_some(), "send.json scripts {node}");
    }
    assert!(send["osaka"]["with_hold"].is_object() && send["multi"]["with_mesh"].is_object());
    assert_eq!(fixture("outbox.json")["data"]["entries"].as_array().unwrap().len(), 2);
    assert!(fixture("mesh.json")["data"]["report"]["sections"].as_array().unwrap().len() >= 2, "a node that is in two meshes needs two");
    assert!(std::fs::read_to_string(fixtures().join("version.txt")).unwrap().starts_with("aoide 0.0.26"));
}
