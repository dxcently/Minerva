//! `/hub/mail` and `POST /hub/notes/<n>/send`: send a note, receive cards (slice M2), over `fake_aoide`.
//!
//! A real perch in front of `fake_aoide` (R2a) with a Minerva home, an Aoide home and a `HOME` of their own, all temp dirs, so nothing
//! reads or runs anything of the operator's: the real `aoide` and `aoided` are never named, and every aoide path option (`--aoide`,
//! `--aoide-home`, `--aoide-daemon-socket`, `--aoide-events`) is a path under the test's tree. The fake logs every call it gets
//! (`FAKE_AOIDE_LOG`), which is how these tests say what was run, with what argv and as whom. `fake_eidolon` is the door the cards are
//! "placed" in front of, and it records every request it is sent: no card ever reaches it.
//!
//! What is tested, by the order a letter travels: the gate and the no-home 404; the node menu; the send (its argv, its answers, every
//! refusal and that a refusal runs nothing, the row it leaves); the stat gate of the receive task (no page looking, no mailbox, an
//! unchanged mailbox: no call); the cards (decoded, clamped, deduped, text only ever a JSON string); receipts and the outbox; place,
//! dismiss and save; and the files (modes, a planted link, a FIFO, a corrupt `cards.json`, the store cap).
//!
//! Run with `cargo test --features test-bins`: the fakes are `test-bins` targets, so without the feature this file is empty.
//!
//! Mutations that were run by hand, each by itself, and what stopped it (the tests that failed; every other test of the file passed):
//!
//! ```text
//!   the mesh membership check dropped              a_mesh_the_node_is_not_in_is_refused_when_the_perch_knows_its_meshes
//!   the paired check dropped                       a_node_that_is_unknown_or_not_paired_is_a_sentence_and_nothing_is_sent
//!   the 64 KiB check dropped                       what_costs_nothing_to_refuse_is_refused_before_anything_runs
//!   the 4 KiB body cap raised                      what_costs_nothing_to_refuse_is_refused_before_anything_runs
//!   deny_unknown_fields dropped on the send body   what_costs_nothing_to_refuse_is_refused_before_anything_runs (`from` is accepted)
//!   the sender made minerva-agent                  the three tests that read the argv (this machine, a node, two meshes)
//!   the subject not cleaned                        a_subject_is_one_printable_line_whatever_the_note_starts_with
//!   a failed row write not reported                the planted-link test and the FIFO test (no `warning`)
//!   O_NOFOLLOW dropped from the appends            the_mail_folder_is_private_and_a_planted_link_or_a_loose_file_is_not_followed
//!   the file or the folder not tightened           the same test (a 0644 file and a 0755 folder from before)
//!   the stat gate dropped                          the three tests of the gate (first read, the page that left, the missing mailbox)
//!   the "a page is looking" check dropped          the gate test, the page-that-left test, and the receipt test
//!   the outbox cache dropped                       the outbox test (a second `mail outbox` inside the minute)
//!   the receipt node check dropped                 the outbox test (a receipt from another node delivers)
//!   the dedupe by message id dropped               the stat-gate test (7 lines in received.jsonl) and the place/dismiss test
//!   the clip at 64 KiB dropped                     what_a_sender_chose_is_clamped_and_never_trusted
//!   the mailbox classed by something else          letters_become_cards_decoded_dated_and_attributed (minerva-agent)
//!   a message id not checked in the path           the_master_token_is_not_a_way_into_the_agents_routes_and_nothing_else_is_a_mail_route
//!   the chat not checked in `place`                a_card_is_placed_in_a_known_chat_dismissed_or_saved_and_nothing_is_ever_said_to_a_door
//!   a dismissed card placeable again               the same test
//!   dismiss taking any body                        the same test
//!   the store cap dropped                          a_full_store_stops_the_reading_and_says_so
//!   a marker prefix is a letter (a lib unit test)  mail::recv::tests::a_structured_letter_is_decoded_and_a_broken_one_is_kept_raw
//! ```
#![cfg(feature = "test-bins")]

mod common;

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::*;
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// The rig
// ---------------------------------------------------------------------------

fn stock() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/aoide")
}

/// A message id made of one hex digit, as the fixtures spell them.
fn id(digit: char) -> String {
    digit.to_string().repeat(64)
}

/// The registry the perch reads: every shape a node can have, and in it every field that must never be shown. `osaka` and `yomi` are in
/// meshes (`mesh.json` has rows for them); `multi` is trusted in two meshes but `mesh --json` has no row for it; `bridge` is not paired;
/// `stray` is paired and unknown to `send.json`, so `fake_aoide` answers it with `unknown-node`; `self` and `Bad Name` are not nodes a
/// letter can go to.
const REGISTRY: &str = r#"{
  "schemaVersion": "0",
  "nodes": [
    {"name": "osaka", "url": "http://user:hunter2@127.0.0.1:8711", "verified": true, "allows": ["message"],
     "pubkey": "PUBKEY-SENTINEL-0123", "tokenFile": "/home/khoa/.aoide/TOKENFILE-SENTINEL", "bearerSecret": "BEARER-SENTINEL"},
    {"name": "yomi", "url": "http://yomi:8710", "verified": true},
    {"name": "bridge", "url": "http://bridge:8710"},
    {"name": "multi", "url": "http://multi:8710", "verified": true},
    {"name": "stray", "url": "http://stray:8710", "verified": true},
    {"name": "self", "url": "http://self:8710", "verified": true},
    {"name": "Bad Name", "url": "http://bad:8710", "verified": true}
  ]
}"#;

const SENTINELS: [&str; 7] = ["PUBKEY-SENTINEL", "TOKENFILE-SENTINEL", "BEARER-SENTINEL", "pubkey", "tokenFile", "hunter2", "http://"];

/// How a perch is started.
struct Setup {
    aoide: bool,
    home: bool,
    hub: bool,
    base_file: bool,
    agent: bool,
    entries: Option<Vec<Value>>,
    knobs: Vec<(String, String)>,
    env: Vec<(String, String)>,
}

impl Setup {
    fn new() -> Self {
        Self {
            aoide: true,
            home: true,
            hub: false,
            base_file: true,
            agent: false,
            entries: None,
            knobs: vec![("mail_tick_ms".to_string(), "50".to_string()), ("mail_pause_ms".to_string(), "300".to_string())],
            env: Vec::new(),
        }
    }

    fn no_aoide(mut self) -> Self {
        self.aoide = false;
        self
    }

    fn no_home(mut self) -> Self {
        self.home = false;
        self
    }

    fn hub(mut self) -> Self {
        self.hub = true;
        self
    }

    fn no_base(mut self) -> Self {
        self.base_file = false;
        self
    }

    fn agent(mut self) -> Self {
        self.agent = true;
        self
    }

    fn entries(mut self, entries: Vec<Value>) -> Self {
        self.entries = Some(entries);
        self
    }

    fn knob(mut self, key: &str, value: &str) -> Self {
        self.knobs.retain(|(k, _)| k != key);
        self.knobs.push((key.to_string(), value.to_string()));
        self
    }

    fn env(mut self, key: &str, value: &str) -> Self {
        self.env.push((key.to_string(), value.to_string()));
        self
    }

    fn start(self) -> Rig {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().canonicalize().unwrap();
        let (home, aoide, user, fixtures, log, agent) =
            (root.join("Minerva"), root.join("aoide"), root.join("user"), root.join("fixtures"), root.join("fake.log"), root.join("agent"));
        std::fs::create_dir(&home).unwrap();
        std::fs::create_dir(&user).unwrap();
        std::fs::create_dir_all(aoide.join("state/mail")).unwrap();
        std::fs::write(aoide.join("state/nodes.json"), REGISTRY).unwrap();
        if self.base_file {
            std::fs::write(aoide.join("state/mail/base.jsonl"), "{\"seq\":9}\n").unwrap();
        }
        lay_fixtures(&fixtures, self.entries.as_deref());
        // `HOME` is empty and has no `.local/bin/aoide`: a perch with no `--aoide` has no aoide, and never the operator's.
        let mut builder = Perch::builder()
            .env("HOME", user.to_str().unwrap())
            // What a perch started from inside a conducted session has in its environment: none of it may reach a call.
            .env("AOIDE_SESSION_ID", "a-conducted-session")
            .env("AOIDE_STATE_DIR", "/nonexistent/ambient-state");
        for (key, value) in &self.env {
            builder = builder.env(key, value);
        }
        if self.hub {
            builder = builder.hub();
        } else {
            builder = builder.arg("--root").arg(&root);
        }
        if self.home {
            builder = builder.arg("--home").arg(&home);
        }
        if self.aoide {
            builder = builder.arg("--aoide").arg(steered(root.join("aoide-bin"), &fixtures, &log, &self.env));
        }
        builder = builder
            .arg("--aoide-home")
            .arg(&aoide)
            .arg("--aoide-daemon-socket")
            .arg(root.join("aoided.sock"))
            .arg("--aoide-events")
            .arg(root.join("events.jsonl"));
        for (key, value) in &self.knobs {
            builder = builder.arg("--knob").arg(format!("{key}={value}"));
        }
        if self.agent {
            for (flag, name) in [("--term-token-file", "term.token"), ("--mail-token-file", "mail.token"), ("--agent-url-file", "perch.url")] {
                builder = builder.arg(flag).arg(agent.join(name));
            }
        }
        let perch = builder.hold(base).start();
        Rig { perch, root, home, aoide, fixtures, log, agent }
    }
}

/// A copy of the stock fixtures, with the entries of both `mail read` files replaced when a test brings its own.
/// The stand-in, run through a script that sets its steering: the fixtures, the log, and every `FAKE_AOIDE_*` the test gave the perch. A
/// perch hands its children only its allowlist (`aoide::ENV_KEEP`, and the AOIDE_* it sets itself), so the steering cannot ride on the
/// perch's environment, and the script is what carries it.
fn steered(path: PathBuf, fixtures: &Path, log: &Path, env: &[(String, String)]) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let quote = |text: &str| format!("'{}'", text.replace('\'', "'\\''"));
    let mut settings = format!("FAKE_AOIDE_DIR={} FAKE_AOIDE_LOG={}", quote(&fixtures.display().to_string()), quote(&log.display().to_string()));
    for (key, value) in env.iter().filter(|(key, _)| key.starts_with("FAKE_AOIDE_")) {
        settings.push_str(&format!(" {key}={}", quote(value)));
    }
    let bin = fake_aoide().expect("the stand-in aoide").replace('\'', "'\\''");
    std::fs::write(&path, format!("#!/bin/sh\n{settings} exec '{bin}' \"$@\"\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn lay_fixtures(dest: &Path, entries: Option<&[Value]>) {
    std::fs::create_dir(dest).unwrap();
    for file in std::fs::read_dir(stock()).unwrap().flatten() {
        if file.path().is_file() {
            std::fs::copy(file.path(), dest.join(file.file_name())).unwrap();
        }
    }
    if let Some(entries) = entries {
        set_entries(dest, entries);
    }
}

fn set_entries(dir: &Path, entries: &[Value]) {
    for name in ["mail-read.json", "mail-read-reread.json"] {
        let path = dir.join(name);
        let mut whole: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        whole["data"]["entries"] = json!(entries);
        // Renamed into place: a fake that is mid-read never sees half a file.
        let next = dir.join(format!("{name}.next"));
        std::fs::write(&next, serde_json::to_string_pretty(&whole).unwrap()).unwrap();
        std::fs::rename(&next, &path).unwrap();
    }
}

/// One entry of `mail read`, from `osaka` unless said.
fn entry(kind: &str, from_node: &str, from_name: &str, msgid: &str, text: &str) -> Value {
    json!({
        "seq": 3,
        "receivedAt": "2026-10-07T09:10:00Z",
        "type": kind,
        "via": "osaka",
        "envelope": {
            "header": { "version": "1", "from": { "node": from_node, "name": from_name }, "type": kind },
            "text": text,
            "sig": "ab",
            "msgid": msgid,
        },
    })
}

struct Rig {
    perch: Perch,
    root: PathBuf,
    /// `--home`: the Minerva home, where `mail/` goes.
    home: PathBuf,
    /// `--aoide-home`.
    aoide: PathBuf,
    /// The fixture directory the fake reads (a copy: a test may rewrite it).
    fixtures: PathBuf,
    log: PathBuf,
    /// Where `--term-token-file` and the others point, when the perch was started with them.
    agent: PathBuf,
}

/// `check`, polled until it says `Some`, for `within` at most.
fn until<T>(within: Duration, what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + within;
    loop {
        if let Some(found) = check() {
            return found;
        }
        assert!(Instant::now() < deadline, "gave up waiting for {what} ({within:?})");
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn json_of(answer: &Answer, status: u16, what: &str) -> Value {
    assert_eq!(answer.status, status, "{what}: {}{}", answer.head, answer.body);
    assert_eq!(answer.header("content-type").as_deref(), Some("application/json"), "{what}: {}", answer.head);
    serde_json::from_str(&answer.body).unwrap_or_else(|e| panic!("{what} is JSON ({e}): {:?}", answer.body))
}

fn error_of(answer: &Answer, status: u16, what: &str) -> String {
    json_of(answer, status, what)["error"].as_str().unwrap_or_else(|| panic!("{what}: an error sentence in {}", answer.body)).to_string()
}

fn mode(path: &Path) -> u32 {
    std::fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777
}

fn sha_of(text: &str) -> String {
    harnox::crypto::hex(&harnox::crypto::sha256(text.as_bytes()))
}

impl Rig {
    fn mail_dir(&self) -> PathBuf {
        self.home.join("mail")
    }

    fn base_file(&self) -> PathBuf {
        self.aoide.join("state/mail/base.jsonl")
    }

    /// The `AOIDE_*` variables every call of the fake was run with: the three the runner sets from the perch's options, every one a temp
    /// path of this test, and nothing of the perch's own environment (`AOIDE_SESSION_ID`, `AOIDE_STATE_DIR`, ...).
    fn aoide_env(&self) -> Value {
        json!({
            "AOIDE_ROOT": self.aoide,
            "AOIDE_DAEMON_SOCKET": self.root.join("aoided.sock"),
            "AOIDE_AUDIT_LOG": self.root.join("events.jsonl"),
        })
    }

    /// Every call the fake got, in order: its argv and the environment it was run with.
    fn calls(&self) -> Vec<Value> {
        let text = std::fs::read_to_string(&self.log).unwrap_or_default();
        text.lines().filter_map(|line| serde_json::from_str::<Value>(line).ok()).filter(|line| line.get("argv").is_some()).collect()
    }

    /// The calls that a request asked for: every call but the schema check, which the perch makes itself before it asks aoide for anything.
    fn asked_calls(&self) -> Vec<Value> {
        self.calls().into_iter().filter(|call| call["argv"][0] != "schema").collect()
    }

    fn asked(&self) -> Vec<Vec<String>> {
        self.asked_calls().iter().map(|call| call["argv"].as_array().unwrap().iter().map(|word| word.as_str().unwrap().to_string()).collect()).collect()
    }

    /// The files of the mail folder other than its lock: the folder is made at start, so only what a request kept counts.
    fn kept_files(&self) -> Vec<String> {
        let mut names: Vec<String> =
            std::fs::read_dir(self.mail_dir()).map(|dir| dir.flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
        names.retain(|name| name != ".lock");
        names
    }

    fn argvs(&self) -> Vec<Vec<String>> {
        self.calls()
            .iter()
            .map(|call| call["argv"].as_array().unwrap().iter().map(|word| word.as_str().unwrap().to_string()).collect())
            .collect()
    }

    /// The calls whose argv starts with these words.
    fn with(&self, words: &[&str]) -> Vec<Vec<String>> {
        self.argvs().into_iter().filter(|argv| argv.len() >= words.len() && argv.iter().zip(words).all(|(a, b)| a == b)).collect()
    }

    fn sends(&self) -> Vec<Vec<String>> {
        self.with(&["mail", "send"])
    }

    fn reads(&self) -> Vec<Vec<String>> {
        self.with(&["mail", "read"])
    }

    fn mail(&self) -> Value {
        json_of(&self.perch.get("/hub/mail"), 200, "GET /hub/mail")
    }

    /// `GET /hub/mail` until it shows `want` cards.
    fn wait_cards(&self, want: usize) -> Value {
        until(Duration::from_secs(15), &format!("{want} cards"), || {
            let mail = self.mail();
            (mail["received"].as_array().unwrap().len() == want).then_some(mail)
        })
    }

    /// Until the fake has been run for `n` reads (the reread counts).
    fn wait_reads(&self, n: usize) {
        until(Duration::from_secs(15), &format!("{n} reads"), || (self.reads().len() >= n).then_some(()));
    }

    /// What the fake prints for the next `mail read`.
    fn set_entries(&self, entries: &[Value]) {
        set_entries(&self.fixtures, entries);
    }

    fn touch_mailbox(&self) {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new().append(true).open(self.base_file()).unwrap();
        file.write_all(b"{\"seq\":10}\n").unwrap();
    }

    /// A note made through the API: its name.
    fn note(&self, title: &str, text: &str) -> String {
        let answer = self.perch.post_json("/hub/notes", json!({ "title": title, "text": text }));
        json_of(&answer, 201, "POST /hub/notes")["name"].as_str().unwrap().to_string()
    }

    fn note_text(&self, name: &str) -> String {
        json_of(&self.perch.get(&format!("/hub/notes/{name}")), 200, "GET /hub/notes/<n>")["text"].as_str().unwrap().to_string()
    }

    fn send(&self, name: &str, body: Value) -> Answer {
        self.send_raw(name, &serde_json::to_vec(&body).unwrap())
    }

    fn send_raw(&self, name: &str, body: &[u8]) -> Answer {
        self.perch.apireq("POST", &format!("/hub/notes/{name}/send")).body(body.to_vec()).send(&self.perch.addr)
    }

    /// The lines of a file of the mail folder, each parsed.
    fn lines(&self, file: &str) -> Vec<Value> {
        let text = std::fs::read_to_string(self.mail_dir().join(file)).unwrap_or_default();
        text.lines().map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{file}: a line that is not JSON ({e}): {line:?}"))).collect()
    }

    fn verb(&self, msgid: &str, verb: &str, body: Value) -> Answer {
        self.perch.post_json(&format!("/hub/mail/{msgid}/{verb}"), body)
    }

    fn verb_raw(&self, msgid: &str, verb: &str, body: &[u8]) -> Answer {
        self.perch.apireq("POST", &format!("/hub/mail/{msgid}/{verb}")).body(body.to_vec()).send(&self.perch.addr)
    }
}

fn ids_of(mail: &Value) -> Vec<String> {
    mail["received"].as_array().unwrap().iter().map(|card| card["msgid"].as_str().unwrap().to_string()).collect()
}

fn card<'a>(mail: &'a Value, msgid: &str) -> &'a Value {
    mail["received"].as_array().unwrap().iter().find(|card| card["msgid"] == msgid).unwrap_or_else(|| panic!("no card {msgid} in {mail}"))
}

fn sent_row<'a>(mail: &'a Value, msgid: &str) -> &'a Value {
    mail["sent"].as_array().unwrap().iter().find(|row| row["msgid"] == msgid).unwrap_or_else(|| panic!("no sent row {msgid} in {mail}"))
}

/// The fixture's own text of the entry with this message id (`mail-read-reread.json` has all of them).
fn fixture_text(msgid: &str) -> String {
    let whole: Value = serde_json::from_str(&std::fs::read_to_string(stock().join("mail-read-reread.json")).unwrap()).unwrap();
    whole["data"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["envelope"]["msgid"] == msgid)
        .map(|entry| entry["envelope"]["text"].as_str().unwrap().to_string())
        .unwrap_or_else(|| panic!("no fixture entry {msgid}"))
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

fn every_route() -> Vec<(&'static str, String)> {
    let a = id('a');
    vec![
        ("GET", "/hub/mail".to_string()),
        ("GET", "/hub/mail/nodes".to_string()),
        ("POST", format!("/hub/mail/{a}/place")),
        ("POST", format!("/hub/mail/{a}/dismiss")),
        ("POST", format!("/hub/mail/{a}/save")),
        ("POST", "/hub/notes/x/send".to_string()),
    ]
}

#[test]
fn every_mail_route_is_behind_the_token() {
    let rig = Setup::new().start();
    for (method, path) in every_route() {
        let bare = Request::new(&rig.perch.addr, method, &path).send(&rig.perch.addr);
        assert_eq!((bare.status, bare.body.as_str()), (401, ""), "{method} {path} with no credential: {}", bare.head);
        for wrong in ["Bearer ", "Bearer x", "Bearer null", "Basic Zm9vOmJhcg=="] {
            let answer = Request::new(&rig.perch.addr, method, &path).bearer(wrong).send(&rig.perch.addr);
            assert_eq!((answer.status, answer.body.as_str()), (401, ""), "{method} {path} with {wrong:?}: {}", answer.head);
        }
    }
    assert!(rig.calls().is_empty(), "a refused request ran aoide: {:?}", rig.argvs());
    assert!(rig.kept_files().is_empty(), "a refused request kept a file in the mail folder");
}

#[test]
fn a_scoped_agent_token_opens_none_of_the_operators_mail() {
    let rig = Setup::new().agent().start();
    let read = |name: &str| std::fs::read_to_string(rig.agent.join(name)).unwrap().trim().to_string();
    let (mail, term) = (read("mail.token"), read("term.token"));
    assert!(mail.len() >= 40 && term.len() >= 40 && mail != term);
    for token in [&mail, &term] {
        for (method, path) in every_route() {
            let answer = Request::new(&rig.perch.addr, method, &path).bearer(format!("Bearer {token}")).body(b"{}".to_vec()).send(&rig.perch.addr);
            assert_eq!((answer.status, answer.body.as_str()), (401, ""), "{method} {path} with a scoped token: {}", answer.head);
        }
    }
    assert!(rig.calls().is_empty() && rig.kept_files().is_empty());
}

#[test]
fn the_master_token_is_not_a_way_into_the_agents_routes_and_nothing_else_is_a_mail_route() {
    let rig = Setup::new().start();
    let a = id('a');
    let not_routes = [
        ("POST", "/hub/mail".to_string()),
        ("POST", "/hub/mail/nodes".to_string()),
        ("GET", format!("/hub/mail/{a}/place")),
        ("GET", format!("/hub/mail/{a}")),
        ("POST", format!("/hub/mail/{a}")),
        ("POST", format!("/hub/mail/{a}/")),
        ("POST", format!("/hub/mail/{a}/frobnicate")),
        ("POST", format!("/hub/mail/{a}/place/x")),
        ("POST", format!("/hub/mail/{}/place", "A".repeat(64))),
        ("POST", format!("/hub/mail/{}/place", &a[1..])),
        ("POST", format!("/hub/mail/%61{}/place", &a[1..])),
        ("GET", "/hub/mail/x/y/z".to_string()),
        // The agent's routes belong to the agent gate: the operator's token is a 404 there and these arms never claim them.
        ("POST", "/hub/mail/a/send".to_string()),
        ("POST", "/hub/mail/a/whoami".to_string()),
        ("GET", "/hub/mail/a/send".to_string()),
    ];
    for (method, path) in not_routes {
        let answer = rig.perch.apireq(method, &path).send(&rig.perch.addr);
        assert_eq!((answer.status, answer.body.as_str()), (404, ""), "{method} {path}: {}", answer.head);
    }
    let (place, dismiss) = (format!("/hub/mail/{a}/place?x=1"), format!("/hub/mail/{a}/dismiss?x=1"));
    for (method, path) in [("GET", "/hub/mail?x=1"), ("GET", "/hub/mail/nodes?x=1"), ("POST", place.as_str()), ("POST", dismiss.as_str())] {
        let answer = rig.perch.apireq(method, path).body(b"{}".to_vec()).send(&rig.perch.addr);
        assert_eq!(answer.status, 400, "{method} {path}: {}{}", answer.head, answer.body);
    }
    assert!(rig.sends().is_empty());
}

#[test]
fn a_perch_without_a_home_has_no_mail_and_asks_aoide_nothing() {
    let rig = Setup::new().no_home().start();
    for (method, path) in every_route() {
        let answer = rig.perch.apireq(method, &path).body(b"{\"node\":\"self\"}".to_vec()).send(&rig.perch.addr);
        assert_eq!((answer.status, answer.body.as_str()), (404, ""), "{method} {path}: {}", answer.head);
    }
    std::thread::sleep(Duration::from_millis(400));
    assert!(rig.calls().is_empty(), "no home, no mail, and no aoide call: {:?}", rig.argvs());
}

// ---------------------------------------------------------------------------
// The node menu
// ---------------------------------------------------------------------------

#[test]
fn the_node_menu_puts_this_machine_first_and_shows_a_node_that_is_not_paired_as_such() {
    let rig = Setup::new().start();
    let answer = rig.perch.get("/hub/mail/nodes");
    let menu = json_of(&answer, 200, "GET /hub/mail/nodes");
    let rows = menu.as_array().unwrap();
    assert_eq!(rows[0], json!({ "name": "self", "verified": true, "self": true, "meshes": [] }), "this machine is first");
    let rest: Vec<(&str, bool, Vec<&str>)> = rows[1..]
        .iter()
        .map(|row| {
            let meshes = row["meshes"].as_array().unwrap().iter().map(|mesh| mesh.as_str().unwrap()).collect();
            (row["name"].as_str().unwrap(), row["verified"].as_bool().unwrap(), meshes)
        })
        .collect();
    assert_eq!(
        rest,
        vec![
            ("osaka", true, vec!["alpha", "beta"]),
            ("yomi", true, vec!["alpha"]),
            ("bridge", false, vec![]),
            ("multi", true, vec![]),
            ("stray", true, vec![])
        ],
        "the registry in its order, and `self` and a name outside the grammar left out"
    );
    for row in rows {
        let mut keys: Vec<&str> = row.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["meshes", "name", "self", "verified"], "{row}");
    }
    assert_eq!(rows.iter().filter(|row| row["self"] == true).count(), 1, "only one row is this machine");
    for sentinel in SENTINELS {
        assert!(!answer.body.contains(sentinel), "the menu carries {sentinel}: {}", answer.body);
    }
    // The menu asks `mesh --json` (cached) and never mail.
    json_of(&rig.perch.get("/hub/mail/nodes"), 200, "the second ask");
    assert_eq!(rig.with(&["mesh"]).len(), 1, "one `mesh --json` serves two asks");
    assert!(rig.with(&["mail"]).is_empty(), "{:?}", rig.argvs());
}

#[test]
fn without_aoide_the_menu_is_this_machine_and_the_list_says_why_nothing_comes() {
    let rig = Setup::new().no_aoide().start();
    // The registry is a file, so its nodes are listed; what needs aoide (their meshes) is not there.
    let menu = json_of(&rig.perch.get("/hub/mail/nodes"), 200, "GET /hub/mail/nodes");
    let names: Vec<&str> = menu.as_array().unwrap().iter().map(|row| row["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["self", "osaka", "yomi", "bridge", "multi", "stray"]);
    assert!(menu.as_array().unwrap().iter().all(|row| row["meshes"] == json!([])), "{menu}");
    let mail = rig.mail();
    assert_eq!((mail["available"].clone(), mail["received"].clone(), mail["sent"].clone()), (json!(false), json!([]), json!([])));
    assert!(mail["error"].as_str().unwrap().contains("--aoide"), "{mail}");
    let name = rig.note("a note", "text");
    let error = error_of(&rig.send(&name, json!({ "node": "self" })), 503, "a send with no aoide");
    assert!(!error.is_empty());
    assert!(!rig.mail_dir().join("sent.jsonl").exists(), "nothing was sent, so nothing is on record");
    std::thread::sleep(Duration::from_millis(300));
    assert!(rig.calls().is_empty(), "there is no aoide to have been asked: {:?}", rig.argvs());
}

// ---------------------------------------------------------------------------
// The send
// ---------------------------------------------------------------------------

#[test]
fn a_note_sent_to_this_machine_is_one_exact_call_and_one_row() {
    let rig = Setup::new().start();
    let name = rig.note("Plan: the mesh", "line one\nline two");
    let text = rig.note_text(&name);
    assert_eq!(text, "Plan: the mesh\n\nline one\nline two");

    let answer = rig.send(&name, json!({ "node": "self" }));
    let sent = json_of(&answer, 202, "the send");
    assert_eq!(
        sent,
        json!({ "msgid": id('5'), "disposition": "filed", "delivery": null, "status": "delivered", "to": "self/minerva" }),
        "the answer has what the page needs and no `mesh` or `warning` it has nothing to say with"
    );

    // Exactly the argv the runner builds: a fixed sender, a node that is `self`, the title as the subject, the whole note after `--`.
    let calls = rig.asked_calls();
    assert_eq!(calls.len(), 1, "{:?}", rig.argvs());
    assert_eq!(
        rig.asked()[0],
        ["mail", "send", "--json", "--to=self/minerva", "--from=minerva", "--subject=Plan: the mesh", "--", text.as_str()]
    );
    // Run as nobody in particular: no session id, and none of the perch's own AOIDE_* variables (the three that are there are the
    // temp paths the perch was told, which the runner passes on).
    assert_eq!((calls[0]["session"].clone(), calls[0]["aoide_env"].clone()), (Value::Null, rig.aoide_env()), "{}", calls[0]);

    let rows = rig.lines("sent.jsonl");
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    let mut keys: Vec<&str> = row.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["disposition", "from", "msgid", "note", "origin", "sha256", "status", "subject", "to", "ts"], "{row}");
    assert_eq!(
        (row["origin"].as_str(), row["from"].as_str(), row["note"].as_str(), row["subject"].as_str(), row["to"].as_str()),
        (Some("operator"), Some("minerva"), Some(name.as_str()), Some("Plan: the mesh"), Some("self/minerva"))
    );
    assert_eq!((row["msgid"].as_str(), row["disposition"].as_str(), row["status"].as_str()), (Some(id('5').as_str()), Some("filed"), Some("delivered")));
    assert_eq!(row["sha256"], sha_of(&text), "the hash of what was sent");
    assert!(row["ts"].as_u64().unwrap() > 1_700_000_000);
    let file = std::fs::read_to_string(rig.mail_dir().join("sent.jsonl")).unwrap();
    assert!(!file.contains("line one"), "the text of a letter is not kept: {file}");
    assert_eq!((mode(&rig.mail_dir()), mode(&rig.mail_dir().join("sent.jsonl"))), (0o700, 0o600));

    // GET shows the row, delivered at once (it was filed on this machine) and with no outbox asked about.
    let mail = rig.mail();
    assert_eq!(sent_row(&mail, &id('5'))["status"], "delivered");
    assert!(rig.with(&["mail", "outbox"]).is_empty());
}

#[test]
fn a_note_sent_to_a_node_says_what_aoide_said_with_hold_and_mesh_in_the_argv() {
    let rig = Setup::new().start();
    let name = rig.note("For osaka", "hello osaka");
    let text = rig.note_text(&name);

    let plain = json_of(&rig.send(&name, json!({ "node": "osaka" })), 202, "to osaka");
    assert_eq!(plain["msgid"], id('6'));
    assert_eq!((plain["disposition"].clone(), plain["status"].clone(), plain["to"].clone()), (json!("spooled"), json!("queued"), json!("osaka/minerva")));
    assert_eq!(plain["delivery"]["status"], "queued");
    assert_eq!(plain["delivery"]["ackPending"], false);
    assert!(plain.get("mesh").is_none() && plain.get("warning").is_none(), "{plain}");

    let held = json_of(&rig.send(&name, json!({ "node": "osaka", "hold": true })), 202, "held");
    assert_eq!((held["msgid"].clone(), held["disposition"].clone(), held["status"].clone()), (json!(id('7')), json!("held"), json!("held")));

    let meshed = json_of(&rig.send(&name, json!({ "node": "osaka", "mesh": "beta", "hold": false })), 202, "in a mesh");
    assert_eq!((meshed["msgid"].clone(), meshed["mesh"].clone()), (json!(id('6')), json!("beta")));

    let sends = rig.sends();
    assert_eq!(sends.len(), 3, "{sends:?}");
    let base = |extra: &[&str], mesh: &[&str]| -> Vec<String> {
        let mut argv: Vec<String> = ["mail", "send", "--json", "--to=osaka/minerva", "--from=minerva"].map(str::to_string).to_vec();
        argv.extend(mesh.iter().map(|word| word.to_string()));
        argv.push("--subject=For osaka".to_string());
        argv.extend(extra.iter().map(|word| word.to_string()));
        argv.push("--".to_string());
        argv.push(text.clone());
        argv
    };
    assert_eq!(sends[0], base(&[], &[]));
    assert_eq!(sends[1], base(&["--hold"], &[]));
    assert_eq!(sends[2], base(&[], &["--mesh=beta"]));

    let rows = rig.lines("sent.jsonl");
    assert_eq!(rows.len(), 3);
    assert_eq!((rows[0].get("mesh"), rows[2]["mesh"].as_str()), (None, Some("beta")));
    assert_eq!((rows[1]["status"].as_str(), rows[1]["msgid"].as_str()), (Some("held"), Some(id('7').as_str())));
    assert!(rows.iter().all(|row| row["to"] == "osaka/minerva" && row["from"] == "minerva" && row["origin"] == "operator"));
}

#[test]
fn a_subject_is_one_printable_line_whatever_the_note_starts_with() {
    let rig = Setup::new().start();
    // `title_of` keeps a line separator and a bidi override, which the runner refuses in a subject: the perch cleans the title first.
    let name = rig.note("", "Plan\u{2028}B \u{202e}x\nbody");
    let sent = json_of(&rig.send(&name, json!({ "node": "self" })), 202, "the send");
    assert_eq!(sent["msgid"], id('5'));
    assert_eq!(rig.sends()[0][5], "--subject=PlanB x");
    assert_eq!(rig.lines("sent.jsonl")[0]["subject"], "PlanB x");
    // A long first line is cut at 120 characters.
    let long = rig.note("", &format!("{}\nrest", "é".repeat(300)));
    json_of(&rig.send(&long, json!({ "node": "self" })), 202, "the long title");
    assert_eq!(rig.sends()[1][5], format!("--subject={}", "é".repeat(120)));
}

#[test]
fn what_costs_nothing_to_refuse_is_refused_before_anything_runs() {
    let rig = Setup::new().start();
    let name = rig.note("A note", "its text");
    let empty = {
        let answer = rig.perch.post_json("/hub/notes", json!({}));
        json_of(&answer, 201, "an empty note")["name"].as_str().unwrap().to_string()
    };
    let huge = "x".repeat(5000);
    let cases: Vec<(String, String, u16, &str)> = vec![
        (name.clone(), String::new(), 400, "expected"),
        (name.clone(), "not json".to_string(), 400, "expected"),
        (name.clone(), "[]".to_string(), 400, "expected"),
        (name.clone(), "\"self\"".to_string(), 400, "expected"),
        (name.clone(), "{}".to_string(), 400, "expected"),
        (name.clone(), json!({ "node": 5 }).to_string(), 400, "expected"),
        (name.clone(), json!({ "node": "self", "hold": "yes" }).to_string(), 400, "expected"),
        (name.clone(), json!({ "node": "self", "mesh": 3 }).to_string(), 400, "expected"),
        // There is no field to name a sender with, or to add anything else.
        (name.clone(), json!({ "node": "self", "from": "minerva-agent" }).to_string(), 400, "expected"),
        (name.clone(), json!({ "node": "self", "subject": "mine" }).to_string(), 400, "expected"),
        (name.clone(), json!({ "node": "self", "text": "mine" }).to_string(), 400, "expected"),
        (name.clone(), json!({ "node": "self", "hold": true }).to_string(), 400, "hold needs another node"),
        (name.clone(), json!({ "node": "self", "mesh": "alpha" }).to_string(), 400, "no mesh"),
        (name.clone(), json!({ "node": "Osaka" }).to_string(), 400, "lower case"),
        (name.clone(), json!({ "node": "a b" }).to_string(), 400, "lower case"),
        (name.clone(), json!({ "node": "../etc" }).to_string(), 400, "lower case"),
        (name.clone(), json!({ "node": "" }).to_string(), 400, "lower case"),
        (name.clone(), json!({ "node": "-x" }).to_string(), 400, "lower case"),
        (name.clone(), json!({ "node": "a".repeat(65) }).to_string(), 400, "lower case"),
        (name.clone(), json!({ "node": "--hold" }).to_string(), 400, "lower case"),
        (name.clone(), json!({ "node": "osaka", "mesh": "Alpha" }).to_string(), 400, "mesh"),
        (name.clone(), json!({ "node": "osaka", "mesh": "" }).to_string(), 400, "mesh"),
        (name.clone(), json!({ "node": "osaka", "mesh": "a b" }).to_string(), 400, "mesh"),
        (name.clone(), json!({ "node": "osaka", "mesh": "--hold" }).to_string(), 400, "mesh"),
        (empty.clone(), json!({ "node": "self" }).to_string(), 400, "subject"),
        ("ghost".to_string(), json!({ "node": "self" }).to_string(), 404, ""),
        (name.clone(), json!({ "node": "self", "pad": huge }).to_string(), 413, ""),
    ];
    for (note, body, status, words) in &cases {
        let answer = rig.send_raw(note, body.as_bytes());
        assert_eq!(answer.status, *status, "{note} <- {body:.80}: {}{}", answer.head, answer.body);
        if !words.is_empty() {
            let error = error_of(&answer, *status, body);
            assert!(error.contains(words), "{body:.80}: {error}");
        }
    }
    assert!(rig.calls().is_empty(), "a refusal that costs nothing ran something: {:?}", rig.argvs());
    assert!(!rig.mail_dir().join("sent.jsonl").exists(), "and left a row");

    // The text of a letter is 64 KiB at most. At the limit it goes; one byte over is a 413 with no process.
    let at_limit = rig.note("", &"a".repeat(64 * 1024));
    json_of(&rig.send(&at_limit, json!({ "node": "self" })), 202, "exactly 64 KiB");
    let over = rig.note("", &"a".repeat(64 * 1024 + 1));
    let error = error_of(&rig.send(&over, json!({ "node": "self" })), 413, "one byte over");
    assert!(error.contains("64 KiB"), "{error}");
    assert_eq!(rig.sends().len(), 1, "only the note at the limit was sent: {:?}", rig.argvs().iter().map(Vec::len).collect::<Vec<_>>());
    assert_eq!(rig.lines("sent.jsonl").len(), 1);
}

#[test]
fn a_node_that_is_unknown_or_not_paired_is_a_sentence_and_nothing_is_sent() {
    let rig = Setup::new().start();
    let name = rig.note("For nobody", "text");
    let unknown = error_of(&rig.send(&name, json!({ "node": "ghost" })), 409, "an unregistered node");
    assert_eq!(unknown, "no node named ghost is registered with Aoide on this machine");
    let unpaired = error_of(&rig.send(&name, json!({ "node": "bridge" })), 409, "an unpaired node");
    assert!(unpaired.contains("node bridge is registered but not paired") && unpaired.contains("pair"), "{unpaired}");
    assert!(rig.sends().is_empty(), "neither reached `mail send`: {:?}", rig.argvs());
    assert!(!rig.mail_dir().join("sent.jsonl").exists());

    // A node the registry vouches for and aoide then does not know: aoide's refusal, in the perch's words and not aoide's.
    let stray = error_of(&rig.send(&name, json!({ "node": "stray" })), 409, "a node aoide does not know");
    assert_eq!(stray, "no node named stray is registered with Aoide on this machine");
    assert_eq!(rig.sends().len(), 1);
    assert!(!rig.mail_dir().join("sent.jsonl").exists(), "a refused letter has no row");
}

#[test]
fn a_node_in_two_meshes_asks_which_and_a_resend_with_one_goes() {
    let rig = Setup::new().start();
    let name = rig.note("Two meshes", "text");
    let refused = rig.send(&name, json!({ "node": "multi" }));
    let body = json_of(&refused, 409, "the ambiguous node");
    assert_eq!(body["meshes"], json!(["alpha", "beta"]));
    assert!(body["why"].as_str().unwrap().contains("more than one mesh"), "{body}");
    assert_eq!(body["error"], body["why"], "the same sentence for a handler of either");
    assert_eq!(rig.sends().len(), 1);
    assert!(!rig.mail_dir().join("sent.jsonl").exists());

    let sent = json_of(&rig.send(&name, json!({ "node": "multi", "mesh": "alpha" })), 202, "the resend");
    assert_eq!((sent["msgid"].clone(), sent["mesh"].clone(), sent["status"].clone()), (json!(id('8')), json!("alpha"), json!("queued")));
    let argv = &rig.sends()[1];
    assert_eq!(&argv[3..6], ["--to=multi/minerva", "--from=minerva", "--mesh=alpha"]);
    assert_eq!(rig.lines("sent.jsonl")[0]["mesh"], "alpha");
}

#[test]
fn a_mesh_the_node_is_not_in_is_refused_when_the_perch_knows_its_meshes() {
    let rig = Setup::new().start();
    let name = rig.note("Wrong mesh", "text");
    let gamma = error_of(&rig.send(&name, json!({ "node": "osaka", "mesh": "gamma" })), 409, "osaka in gamma");
    assert!(gamma.contains("node osaka is not in mesh gamma") && gamma.contains("alpha, beta"), "{gamma}");
    let beta = error_of(&rig.send(&name, json!({ "node": "yomi", "mesh": "beta" })), 409, "yomi in beta");
    assert!(beta.contains("node yomi is not in mesh beta") && beta.contains("alpha"), "{beta}");
    assert!(rig.sends().is_empty(), "{:?}", rig.argvs());
    // A node `mesh --json` has no row for is aoide's to judge: the name only has to be a name.
    json_of(&rig.send(&name, json!({ "node": "multi", "mesh": "gamma" })), 202, "multi in a mesh the perch cannot check");
    json_of(&rig.send(&name, json!({ "node": "osaka", "mesh": "alpha" })), 202, "osaka in alpha");
    assert_eq!(rig.sends().len(), 2);
}

#[test]
fn an_answer_that_never_came_may_have_gone_and_leaves_a_row_that_says_so() {
    for (flood, label) in [("100", "output that is not JSON"), ("5000000", "more output than the runner reads")] {
        // The flood spares the schema check, so the send is the one that is flooded.
        let rig = Setup::new().env("FAKE_AOIDE_FLOOD", flood).env("FAKE_AOIDE_SPARES_SCHEMA", "1").start();
        let name = rig.note("Maybe", "text");
        let answer = rig.send(&name, json!({ "node": "self" }));
        let error = error_of(&answer, 502, label);
        assert!(error.contains("may or may not have been sent"), "{label}: {error}");
        assert_eq!(rig.sends().len(), 1, "{label}");
        let rows = rig.lines("sent.jsonl");
        assert_eq!(rows.len(), 1, "{label}: the attempt is on record");
        let got = (rows[0]["msgid"].clone(), rows[0]["disposition"].clone(), rows[0]["status"].clone());
        assert_eq!(got, (Value::Null, json!("unknown"), json!("unknown")), "{label}");
        assert_eq!(rows[0]["sha256"], sha_of(&rig.note_text(&name)), "{label}");
        // And the page sees it as unknown, with nothing to follow it by.
        let mail = rig.mail();
        assert_eq!(mail["sent"][0]["status"], "unknown", "{label}");
    }
}

#[test]
fn many_sends_at_once_write_whole_lines() {
    let rig = Setup::new().start();
    let name = rig.note("Many", &"line of text\n".repeat(200));
    let answers: Vec<u16> = std::thread::scope(|scope| {
        let threads: Vec<_> = (0..12).map(|_| scope.spawn(|| rig.send(&name, json!({ "node": "self" })).status)).collect();
        threads.into_iter().map(|thread| thread.join().unwrap()).collect()
    });
    // Two run at once at most: the others are 429 at once, and nothing is half-written for any of them.
    assert!(answers.iter().all(|status| *status == 202 || *status == 429), "{answers:?}");
    let accepted = answers.iter().filter(|status| **status == 202).count();
    let rows = rig.lines("sent.jsonl");
    assert_eq!(rows.len(), accepted, "every line whole and none lost");
    assert!(rows.iter().all(|row| row["msgid"] == id('5')));
}

#[test]
fn the_mail_folder_is_private_and_a_planted_link_or_a_loose_file_is_not_followed() {
    let rig = Setup::new().start();
    let name = rig.note("Private", "text");
    let dir = rig.mail_dir();
    // A folder and a file from before, too open: tightened and kept. The perch made the folder at start (its lock is in it).
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    let sent = dir.join("sent.jsonl");
    std::fs::write(&sent, "{\"old\":1}\n").unwrap();
    std::fs::set_permissions(&sent, std::fs::Permissions::from_mode(0o644)).unwrap();
    let body = json_of(&rig.send(&name, json!({ "node": "self" })), 202, "into a loose folder");
    assert!(body.get("warning").is_none(), "{body}");
    assert_eq!((mode(&dir), mode(&sent)), (0o700, 0o600));
    let rows = rig.lines("sent.jsonl");
    assert_eq!((rows.len(), rows[0]["old"].clone()), (2, json!(1)), "the old line is kept and the new one follows");

    // A link planted where the file goes: the letter is sent (it cannot be un-sent), the row is not written through the link.
    let victim = rig.root.join("victim.txt");
    std::fs::write(&victim, "keep me\n").unwrap();
    std::fs::remove_file(&sent).unwrap();
    std::os::unix::fs::symlink(&victim, &sent).unwrap();
    let body = json_of(&rig.send(&name, json!({ "node": "self" })), 202, "through a link");
    assert!(body["warning"].as_str().unwrap().contains("sent.jsonl"), "{body}");
    assert_eq!(body["msgid"], id('5'));
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep me\n", "nothing was written through the link");
    assert_eq!(rig.sends().len(), 2);

    // A link planted as the folder.
    std::fs::remove_file(&sent).unwrap();
    let aside = rig.root.join("aside");
    std::fs::rename(&dir, &aside).unwrap();
    let elsewhere = rig.root.join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &dir).unwrap();
    let body = json_of(&rig.send(&name, json!({ "node": "self" })), 202, "into a linked folder");
    assert!(body.get("warning").is_some(), "{body}");
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0, "nothing was made behind the link");
}

// ---------------------------------------------------------------------------
// The receive task and its gate
// ---------------------------------------------------------------------------

#[test]
fn nothing_is_asked_of_aoide_until_a_page_looks_and_then_a_reread_and_one_read() {
    let rig = Setup::new().start();
    std::thread::sleep(Duration::from_millis(400));
    assert!(rig.calls().is_empty(), "no page is looking: {:?}", rig.argvs());

    let first = rig.mail();
    assert_eq!((first["available"].clone(), first["error"].clone()), (json!(true), Value::Null));
    let mail = rig.wait_cards(7);
    assert_eq!(mail["more"], 0);
    rig.wait_reads(2);
    std::thread::sleep(Duration::from_millis(200));
    let reads = rig.reads();
    assert_eq!(reads.len(), 2, "{reads:?}");
    assert_eq!(reads[0], ["mail", "read", "--for=minerva", "--json", "--reread"], "the reread comes once, first");
    assert_eq!(reads[1], ["mail", "read", "--for=minerva", "--json"]);
    assert_eq!(rig.asked_calls().len(), 2, "and nothing else: {:?}", rig.argvs());
    // As the reader that moves only Minerva's own cursor, with nothing of the perch's environment.
    for call in rig.asked_calls() {
        assert_eq!((call["session"].clone(), call["aoide_env"].clone()), (json!("minerva-perch"), rig.aoide_env()), "{call}");
    }

    // An unchanged mailbox costs no call, however long it is looked at.
    for _ in 0..30 {
        rig.mail();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(rig.asked_calls().len(), 2, "{:?}", rig.argvs());

    // A changed one costs one plain read, and the letters it prints again are not new cards.
    rig.touch_mailbox();
    until(Duration::from_secs(10), "the third read", || (rig.reads().len() == 3).then_some(()));
    std::thread::sleep(Duration::from_millis(400));
    let reads = rig.reads();
    assert_eq!(reads.len(), 3, "{reads:?}");
    assert_eq!(reads[2], ["mail", "read", "--for=minerva", "--json"], "no reread this time");
    assert_eq!(ids_of(&rig.mail()).len(), 7);
    assert_eq!(rig.lines("received.jsonl").len(), 7, "a letter is written once, by message id");
    assert_eq!((mode(&rig.mail_dir()), mode(&rig.mail_dir().join("received.jsonl")), mode(&rig.mail_dir().join("cards.json"))), (0o700, 0o600, 0o600));
}

#[test]
fn a_page_that_stopped_looking_stops_the_reading_and_the_next_look_starts_it_again() {
    let rig = Setup::new().knob("mail_watch_ms", "500").start();
    rig.mail();
    rig.wait_cards(7);
    rig.wait_reads(2);
    std::thread::sleep(Duration::from_millis(800));
    rig.touch_mailbox();
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(rig.reads().len(), 2, "nobody looked for longer than the watch: the change is not read");
    rig.mail();
    until(Duration::from_secs(10), "a read after the next look", || (rig.reads().len() == 3).then_some(()));
}

#[test]
fn a_mailbox_that_is_not_there_costs_no_call_and_one_that_appears_is_read() {
    let rig = Setup::new().no_base().start();
    rig.mail();
    std::thread::sleep(Duration::from_millis(600));
    assert!(rig.asked_calls().is_empty(), "no mailbox file, nothing to read: {:?}", rig.argvs());
    assert_eq!(rig.mail()["received"], json!([]));
    std::fs::write(rig.base_file(), "{\"seq\":1}\n").unwrap();
    rig.wait_cards(7);
    rig.wait_reads(2);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(rig.reads().len(), 2, "one reread and one read, and no more while the file stays as it is");
}

// ---------------------------------------------------------------------------
// The cards
// ---------------------------------------------------------------------------

#[test]
fn letters_become_cards_decoded_dated_and_attributed() {
    let rig = Setup::new().start();
    rig.mail();
    let answer = {
        rig.wait_cards(7);
        rig.perch.get("/hub/mail")
    };
    let mail = json_of(&answer, 200, "GET /hub/mail");
    assert_eq!(mail["sent"], json!([]));
    assert_eq!(
        ids_of(&mail),
        [id('f'), id('4'), id('3'), id('1'), id('b'), id('a'), id('9')],
        "newest first; the receipt and the entry with an id that is not an id are not cards"
    );
    let keys = |card: &Value| {
        let mut keys: Vec<&str> = card.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        keys.join(",")
    };
    assert_eq!(
        keys(&mail["received"][0]),
        "at,broken,chat,clipped,format,from_label,mailbox,msgid,node,reply_to,seen,sender,sender_claimed,seq,state,subject,text,thread,via"
    );

    let a = card(&mail, &id('a'));
    assert_eq!(a["format"], "letter");
    assert_eq!(a["subject"], "Plan: ship the mesh view");
    assert_eq!(a["text"], "First line.\nSecond line, with \"quotes\" and 世界.");
    let who = (a["sender"].clone(), a["node"].clone(), a["mailbox"].clone(), a["via"].clone());
    assert_eq!(who, (json!("minerva"), json!("workbench"), json!("minerva"), json!("self")));
    assert_eq!((a["at"].clone(), a["seq"].clone(), a["state"].clone(), a["chat"].clone()), (json!(1_791_363_600u64), json!(1), json!("new"), Value::Null));
    assert_eq!((a["broken"].clone(), a["clipped"].clone(), a["thread"].clone(), a["reply_to"].clone()), (json!(false), json!(false), Value::Null, Value::Null));

    let b = card(&mail, &id('b'));
    assert_eq!((b["sender"].clone(), b["mailbox"].clone(), b["subject"].clone()), (json!("minerva-agent"), json!("minerva-agent"), json!("Review the diff")));
    assert_eq!((b["thread"].clone(), b["reply_to"].clone()), (json!(id('c')), json!(id('d'))));

    let one = card(&mail, &id('1'));
    let body = (one["format"].clone(), one["subject"].clone(), one["text"].clone());
    assert_eq!(body, (json!("raw"), json!("plain text from a peer"), json!("plain text from a peer\nwith two lines")));
    let who = (one["sender"].clone(), one["node"].clone(), one["mailbox"].clone(), one["at"].clone());
    assert_eq!(who, (json!("other"), json!("osaka"), json!("conductor"), json!(1_791_364_200u64)));

    let cut = card(&mail, &id('3'));
    assert_eq!((cut["broken"].clone(), cut["format"].clone(), cut["subject"].clone()), (json!(true), json!("raw"), json!("(an unreadable letter)")));
    assert_eq!(cut["text"], fixture_text(&id('3')).as_str(), "the text is kept whole");

    let raw = card(&mail, &id('f'));
    assert_eq!((raw["format"].clone(), raw["subject"].clone(), raw["sender"].clone()), (json!("raw"), json!("a raw note, no letter marker"), json!("minerva")));
    let old = card(&mail, &id('9'));
    assert_eq!((old["at"].clone(), old["text"].clone()), (json!(1_791_273_600u64), json!("read long ago, printed again only by --reread")));

    // Hostile text is a JSON string and nothing else: kept exactly, and not one control byte in the body that carries it.
    let hostile = fixture_text(&id('4'));
    let four = card(&mail, &id('4'));
    assert_eq!(four["text"], hostile.as_str());
    assert!(answer.body.bytes().all(|byte| byte >= 0x20), "a raw control byte in the body");
    assert!(hostile.contains('\u{1b}') && hostile.contains("<script>"), "the fixture is hostile");
    let subject = four["subject"].as_str().unwrap();
    assert!(subject.starts_with("<script>alert(1)</script>") && subject.chars().all(|c| !c.is_control() && c != '\u{202e}'), "{subject:?}");
    assert!(subject.chars().count() <= 120 && !subject.contains('\n'));
}

#[test]
fn what_a_sender_chose_is_clamped_and_never_trusted() {
    let long_subject = format!("AOIDE-LETTER/1\n{}", json!({ "subject": "s".repeat(500), "body": "b" }));
    let mut hostile = entry("letter", "<b>evil</b>\u{1b}[1m", "minerva-agent\n--hold", &id('7'), "x");
    hostile["via"] = json!("<img src=x onerror=1>");
    hostile["receivedAt"] = json!("yesterday-ish");
    let big = format!("{}tail", "世".repeat(30_000));
    let entries = vec![
        hostile,
        entry("letter", "osaka", "minerva", &id('6'), &long_subject),
        entry("letter", "osaka", "minerva", &id('5'), &big),
        entry("letter", "osaka", "minerva", &id('4'), "a\0b"),
        entry("letter", "osaka", "Minerva", &id('3'), "case"),
        entry("note-to-self", "osaka", "minerva", &id('2'), "not a letter"),
        entry("letter", "osaka", "minerva", &id('E'), "uppercase id"),
        entry("letter", "osaka", "minerva", &id('1')[..63], "short id"),
        entry("letter", "osaka", "minerva", &id('8'), "eight"),
        entry("letter", "osaka", "minerva", &id('8'), "a second letter with the same id"),
    ];
    let rig = Setup::new().entries(entries).start();
    rig.mail();
    let mail = rig.wait_cards(6);
    assert_eq!(
        ids_of(&mail),
        [id('8'), id('3'), id('4'), id('5'), id('6'), id('7')],
        "a type that is not a letter, and an id that is not 64 lower case hex digits, are no cards; a repeat in one read is one"
    );

    // Header fields outside their grammars are nothing, and the sender is `other`.
    let h = card(&mail, &id('7'));
    assert_eq!((h["node"].clone(), h["mailbox"].clone(), h["sender"].clone(), h["via"].clone()), (Value::Null, Value::Null, json!("other"), Value::Null));
    assert_eq!(h["at"], h["seen"], "a time that is not a time is when the perch read it");
    assert!(h["seen"].as_u64().unwrap().abs_diff(harnox::time::now_secs()) < 120);
    assert_eq!(card(&mail, &id('3'))["sender"], "other", "`Minerva` is not `minerva`");
    assert_eq!(card(&mail, &id('3'))["mailbox"], Value::Null);

    // A subject is one line of 120 characters; a body is 64 KiB at a character; a NUL is a replacement character.
    let six = card(&mail, &id('6'));
    assert_eq!((six["subject"].clone(), six["text"].clone(), six["format"].clone()), (json!("s".repeat(120)), json!("b"), json!("letter")));
    let five = card(&mail, &id('5'));
    let text = five["text"].as_str().unwrap();
    assert_eq!(five["clipped"], true);
    assert!(text.len() <= 64 * 1024 && text.len() > 64 * 1024 - 3 && text.chars().all(|c| c == '世'), "{}", text.len());
    assert_eq!(card(&mail, &id('4'))["text"], "a\u{fffd}b");

    // The first letter with an id is the letter: a later one with the same id does not change what was read.
    assert_eq!(card(&mail, &id('8'))["text"], "eight");
    rig.wait_reads(2);
    rig.set_entries(&[entry("letter", "osaka", "minerva", &id('8'), "changed")]);
    rig.touch_mailbox();
    until(Duration::from_secs(10), "the read of the changed letter", || (rig.reads().len() == 3).then_some(()));
    std::thread::sleep(Duration::from_millis(300));
    let mail = rig.mail();
    assert_eq!((mail["received"].as_array().unwrap().len(), card(&mail, &id('8'))["text"].clone()), (6, json!("eight")));
    assert_eq!(rig.lines("received.jsonl").len(), 6);
}

// ---------------------------------------------------------------------------
// Receipts and the outbox
// ---------------------------------------------------------------------------

#[test]
fn a_receipt_from_the_node_a_letter_went_to_marks_it_delivered_and_the_outbox_says_the_rest() {
    let rig = Setup::new().start();
    let name = rig.note("Receipts", "text");
    // 6666 goes to osaka (the stock mailbox has osaka's receipt for it), 8888 goes to multi (the outbox does not have it), 5555 stays here.
    json_of(&rig.send(&name, json!({ "node": "osaka" })), 202, "to osaka");
    json_of(&rig.send(&name, json!({ "node": "multi", "mesh": "alpha" })), 202, "to multi");
    json_of(&rig.send(&name, json!({ "node": "self" })), 202, "to self");
    // A row is "after" an outbox answer only by a whole second.
    std::thread::sleep(Duration::from_millis(1100));
    rig.mail();
    let mail = until(Duration::from_secs(15), "the rows to settle", || {
        let mail = rig.mail();
        (sent_row(&mail, &id('6'))["status"] == "delivered" && sent_row(&mail, &id('8'))["status"] == "accepted").then_some(mail)
    });
    assert_eq!(sent_row(&mail, &id('5'))["status"], "delivered", "filed on this machine");
    assert_eq!(sent_row(&mail, &id('6'))["reason"], Value::Null);
    assert_eq!(mail["sent"].as_array().unwrap().len(), 3);
    assert_eq!(mail["sent"][0]["msgid"], id('5'), "newest first");
    // The outbox was asked once, by the task, and not again: nothing waits on it any more. A GET never asks it.
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(rig.with(&["mail", "outbox", "--json"]).len(), 1, "{:?}", rig.argvs());
    // The receipt was a receipt and not a card.
    assert_eq!(ids_of(&mail).len(), 7);
    let cards = std::fs::read_to_string(rig.mail_dir().join("cards.json")).unwrap();
    assert!(cards.contains(&id('6')), "the receipt is kept in cards.json: {cards}");
}

#[test]
fn a_letter_that_is_still_in_the_outbox_shows_aoides_status_and_reason_and_a_forged_receipt_changes_nothing() {
    // A receipt for 6666 that did not come from osaka, and one with no node at all.
    let entries = vec![
        entry("receipt", "yomi", "minerva", &id('2'), &id('6')),
        entry("receipt", "<b>x</b>", "minerva", &id('3'), &id('6')),
        entry("letter", "osaka", "minerva", &id('1'), "a letter, so there is a read to make"),
    ];
    let rig = Setup::new().entries(entries).start();
    let name = rig.note("Waiting", "text");
    json_of(&rig.send(&name, json!({ "node": "osaka" })), 202, "to osaka");
    std::thread::sleep(Duration::from_millis(1100));
    rig.wait_cards(1);
    let mail = until(Duration::from_secs(15), "the outbox status", || {
        let mail = rig.mail();
        (sent_row(&mail, &id('6'))["status"] == "retrying").then_some(mail)
    });
    assert_eq!(sent_row(&mail, &id('6'))["reason"], "connection refused");
    // Still waiting, and still not asked again inside the minute: the answer is cached.
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(rig.with(&["mail", "outbox", "--json"]).len(), 1, "{:?}", rig.argvs());
    assert_eq!(sent_row(&rig.mail(), &id('6'))["status"], "retrying", "a receipt from another node does not deliver it");
    let cards = std::fs::read_to_string(rig.mail_dir().join("cards.json")).unwrap();
    assert!(!cards.contains("delivered\":{\"6"), "{cards}");
}

// ---------------------------------------------------------------------------
// Place, dismiss, save
// ---------------------------------------------------------------------------

#[test]
fn a_card_is_placed_in_a_known_chat_dismissed_or_saved_and_nothing_is_ever_said_to_a_door() {
    let rig = Setup::new().hub().start();
    let (chat, _, _) = live(&rig.perch);
    rig.mail();
    rig.wait_cards(7);
    let (a, b, four) = (id('a'), id('b'), id('4'));

    // Place: the body is `{chat}` and nothing else, and the chat is one this perch knows.
    for (body, status) in [
        (b"".to_vec(), 400),
        (b"not json".to_vec(), 400),
        (b"[]".to_vec(), 400),
        (b"{}".to_vec(), 400),
        (json!({ "chat": chat, "extra": 1 }).to_string().into_bytes(), 400),
        (json!({ "chat": 5 }).to_string().into_bytes(), 400),
        (json!({ "chat": "no-such-chat" }).to_string().into_bytes(), 404),
        (json!({ "chat": "../../etc" }).to_string().into_bytes(), 404),
        (json!({ "chat": "" }).to_string().into_bytes(), 404),
        (json!({ "chat": "x".repeat(200) }).to_string().into_bytes(), 404),
    ] {
        let answer = rig.verb_raw(&a, "place", &body);
        assert_eq!(answer.status, status, "{}: {}{}", String::from_utf8_lossy(&body), answer.head, answer.body);
    }
    assert_eq!(rig.verb(&id('e'), "place", json!({ "chat": chat })).status, 404, "a card nobody read");
    assert_eq!(card(&rig.mail(), &a)["state"], "new", "nothing above changed it");
    let placed = json_of(&rig.verb(&a, "place", json!({ "chat": chat })), 200, "place");
    assert_eq!(placed, json!({ "msgid": a, "state": "placed", "chat": chat }));
    let mail = rig.mail();
    assert_eq!((card(&mail, &a)["state"].clone(), card(&mail, &a)["chat"].clone()), (json!("placed"), json!(chat)));
    let kept: Value = serde_json::from_str(&std::fs::read_to_string(rig.mail_dir().join("cards.json")).unwrap()).unwrap();
    assert_eq!((kept["cards"][&a]["state"].clone(), kept["cards"][&a]["chat"].clone()), (json!("placed"), json!(chat)));

    // Dismiss: nothing in the body, or `{}`. A dismissed card is gone from the list and cannot be placed.
    for body in [&b"{\"x\":1}"[..], b"garbage", b"[]"] {
        assert_eq!(rig.verb_raw(&b, "dismiss", body).status, 400, "{}", String::from_utf8_lossy(body));
    }
    assert_eq!(rig.verb_raw(&b, "dismiss", &vec![b' '; 2000]).status, 413);
    assert_eq!(rig.verb(&id('e'), "dismiss", json!({})).status, 404);
    assert_eq!(json_of(&rig.verb_raw(&b, "dismiss", b""), 200, "dismiss"), json!({ "msgid": b, "state": "dismissed" }));
    assert!(!ids_of(&rig.mail()).contains(&b));
    assert_eq!(ids_of(&rig.mail()).len(), 6);
    let again = error_of(&rig.verb(&b, "place", json!({ "chat": chat })), 409, "place on a dismissed card");
    assert!(again.contains("dismissed"), "{again}");
    // And a letter that is read again stays dismissed.
    rig.touch_mailbox();
    until(Duration::from_secs(10), "the third read", || (rig.reads().len() == 3).then_some(()));
    std::thread::sleep(Duration::from_millis(300));
    assert!(!ids_of(&rig.mail()).contains(&b));

    // Save: a note is made from the card, with a letter's subject on top.
    assert_eq!(rig.verb_raw(&a, "save", b"{\"x\":1}").status, 400);
    assert_eq!(rig.verb(&id('e'), "save", json!({})).status, 404);
    let saved = json_of(&rig.verb_raw(&a, "save", b""), 201, "save a letter");
    assert_eq!(rig.note_text(saved["name"].as_str().unwrap()), "Plan: ship the mesh view\n\nFirst line.\nSecond line, with \"quotes\" and 世界.");
    assert!(saved["sha"].as_str().unwrap().len() == 64 && saved["mtime"].is_u64(), "{saved}");
    let raw = json_of(&rig.verb(&id('1'), "save", json!({})), 201, "save a raw card");
    assert_eq!(rig.note_text(raw["name"].as_str().unwrap()), "plain text from a peer\nwith two lines");
    let hostile = json_of(&rig.verb(&four, "save", json!({})), 201, "save the hostile card");
    assert_eq!(rig.note_text(hostile["name"].as_str().unwrap()), fixture_text(&four), "the text of a card is saved as it came");

    // The text of a card that is not on file is a 404, not an empty note.
    std::fs::remove_file(rig.mail_dir().join("received.jsonl")).unwrap();
    let gone = error_of(&rig.verb(&four, "save", json!({})), 404, "save without the text");
    assert!(gone.contains("not on file"), "{gone}");

    // Through all of it: not one request reached the door. A card is data in this perch's files and nothing posts it anywhere.
    let records = rig.perch.fake_records();
    assert!(records.iter().all(|line| !line.contains("/api/say") && !line.starts_with("POST ")), "a door was spoken to: {records:?}");
}

// ---------------------------------------------------------------------------
// The files
// ---------------------------------------------------------------------------

#[test]
fn a_cards_file_that_does_not_parse_refuses_mail_until_it_is_moved_aside() {
    let rig = Setup::new().start();
    std::fs::create_dir_all(rig.mail_dir()).unwrap();
    std::fs::set_permissions(rig.mail_dir(), std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(rig.mail_dir().join("cards.json"), "this is {not json").unwrap();
    // The page says why, and no letter is read into a store that does not parse; the file is not set aside behind the operator's back.
    let mail = until(Duration::from_secs(5), "the refusal", || {
        let mail = rig.mail();
        mail["error"].as_str().is_some().then_some(mail)
    });
    assert!(mail["error"].as_str().unwrap().contains("cards.json does not parse"), "{mail}");
    assert_eq!(mail["received"], json!([]));
    std::thread::sleep(Duration::from_millis(400));
    assert!(rig.reads().is_empty(), "no letter is read into it: {:?}", rig.argvs());
    assert_eq!(std::fs::read_to_string(rig.mail_dir().join("cards.json")).unwrap(), "this is {not json", "the file is kept as it is");
    // Moved aside, the store starts over with the mailbox's letters, and nothing is lost of them.
    std::fs::rename(rig.mail_dir().join("cards.json"), rig.mail_dir().join("cards.json.old")).unwrap();
    // The cards arrive before the read clears the complaint, so both are waited for.
    until(Duration::from_secs(10), "the store started over", || {
        let mail = rig.mail();
        (mail["error"].is_null() && mail["received"].as_array().is_some_and(|cards| cards.len() == 7)).then_some(())
    });
}

#[test]
fn a_fifo_where_a_file_goes_is_an_error_sentence_and_not_a_hang() {
    use std::os::unix::ffi::OsStrExt as _;
    let rig = Setup::new().start();
    std::fs::create_dir_all(rig.mail_dir()).unwrap();
    std::fs::set_permissions(rig.mail_dir(), std::fs::Permissions::from_mode(0o700)).unwrap();
    for file in ["received.jsonl", "sent.jsonl"] {
        let path = std::ffi::CString::new(rig.mail_dir().join(file).as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    }
    let started = Instant::now();
    let mail = rig.mail();
    assert!(started.elapsed() < Duration::from_secs(5), "the list waited on a FIFO");
    assert_eq!(mail["received"], json!([]));
    assert!(mail["error"].as_str().unwrap().contains("could not be used as a plain file"), "{mail}");
    // The send runs (the letter is aoide's now) and says the row could not be kept.
    let name = rig.note("Fifo", "text");
    let sent = json_of(&rig.send(&name, json!({ "node": "self" })), 202, "a send with a FIFO for its row");
    assert!(sent["warning"].as_str().unwrap().contains("sent.jsonl"), "{sent}");
}

#[test]
fn a_full_store_stops_the_reading_and_says_so() {
    let rig = Setup::new().start();
    std::fs::create_dir_all(rig.mail_dir()).unwrap();
    std::fs::set_permissions(rig.mail_dir(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut cards = serde_json::Map::new();
    for n in 0..5000u32 {
        cards.insert(format!("{n:064x}"), json!({ "state": "new", "at": 1 }));
    }
    std::fs::write(rig.mail_dir().join("cards.json"), json!({ "v": 1, "cards": cards, "delivered": {} }).to_string()).unwrap();
    rig.mail();
    let mail = until(Duration::from_secs(10), "the full-store notice", || {
        let mail = rig.mail();
        mail["error"].is_string().then_some(mail)
    });
    assert!(mail["error"].as_str().unwrap().contains("card store is full"), "{mail}");
    std::thread::sleep(Duration::from_millis(300));
    assert!(rig.asked_calls().is_empty(), "a full store reads nothing: {:?}", rig.argvs());
}

// ---------------------------------------------------------------------------
// The M2 fix round: the reread state machine, the caps, the sends
// ---------------------------------------------------------------------------

fn rereads(rig: &Rig) -> usize {
    rig.reads().iter().filter(|argv| argv.last().map(String::as_str) == Some("--reread")).count()
}

/// A plain read that is too big to keep is not the end of it: the cursor may have moved past letters that were not kept, so a reread is
/// due, and it is made before any plain read. Here the plain output is 5 MiB (over the plain cap) and the reread is not flooded: the
/// reread keeps the letters, and the plain read after it is too big again, so the reread is armed again each time. Nothing is lost.
#[test]
fn a_plain_read_too_big_re_arms_the_reread_and_the_reread_keeps_the_letters() {
    let rig = Setup::new()
        .env("FAKE_AOIDE_FLOOD", &(5 * 1024 * 1024).to_string())
        .env("FAKE_AOIDE_FLOOD_SPARES_REREAD", "1")
        .env("FAKE_AOIDE_SPARES_SCHEMA", "1")
        .start();
    rig.mail();
    until(Duration::from_secs(10), "the seven letters of the reread", || {
        let mail = rig.mail();
        (mail["received"].as_array().unwrap().len() == 7).then_some(())
    });
    until(Duration::from_secs(10), "a second reread, armed by the plain read", || (rereads(&rig) >= 2).then_some(()));
    let reads = rig.reads();
    assert_eq!(reads[0], ["mail", "read", "--for=minerva", "--json", "--reread"], "the first look is a reread");
    assert_eq!(reads[1], ["mail", "read", "--for=minerva", "--json"], "then the plain read, which is too big");
    assert_eq!(reads[2], ["mail", "read", "--for=minerva", "--json", "--reread"], "and the reread it arms");
    let error = rig.mail()["error"].as_str().unwrap_or_default().to_string();
    assert!(!error.contains("could not be read here"), "the reread kept the letters, so nothing is lost: {error}");
}

/// A reread too big to print can never succeed (the history only grows): it is not armed again, and the page says the letters could not be
/// read here, and how to read them in a terminal, until the perch restarts. Plain reads go on for new letters.
#[test]
fn a_reread_too_big_to_print_is_a_loss_said_on_the_page_and_never_armed_again() {
    let rig = Setup::new().env("FAKE_AOIDE_FLOOD", &(20 * 1024 * 1024).to_string()).env("FAKE_AOIDE_SPARES_SCHEMA", "1").start();
    let mail = until(Duration::from_secs(10), "the loss", || {
        let mail = rig.mail();
        mail["error"].as_str().unwrap_or_default().contains("could not be read here").then_some(mail)
    });
    assert!(mail["error"].as_str().unwrap().contains("aoide mail read --for=minerva --reread"), "{mail}");
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(rereads(&rig), 1, "a reread too big to print is not tried again: {:?}", rig.reads());
    assert!(rig.mail()["error"].as_str().unwrap_or_default().contains("could not be read here"), "the loss stays until a restart");
}

/// Every letter of one read is kept, not the first 2000 by position: 2001 letters make 25 cards shown and 1976 more, with none dropped.
#[test]
fn two_thousand_and_one_letters_are_all_kept_and_the_page_counts_the_rest() {
    let rig = Setup::new().start();
    let entries: Vec<Value> = (1..=2001)
        .map(|i| {
            json!({
                "seq": i, "receivedAt": "2026-10-07T09:10:00Z", "type": "letter", "via": "osaka",
                "envelope": {
                    "header": { "version": "1", "from": { "node": "osaka", "name": "minerva-agent" }, "type": "letter" },
                    "text": format!("letter {i}"), "sig": "ab", "msgid": format!("{i:064x}"),
                },
            })
        })
        .collect();
    rig.set_entries(&entries);
    let mail = until(Duration::from_secs(20), "all 2001 letters", || {
        let mail = rig.mail();
        (mail["received"].as_array().unwrap().len() as u64 + mail["more"].as_u64().unwrap() == 2001).then_some(mail)
    });
    assert_eq!((mail["received"].as_array().unwrap().len(), mail["more"].as_u64()), (25, Some(1976)), "{mail}");
    assert_eq!(mail["error"], Value::Null, "{mail}");
}

/// A send that outlives its timeout after aoide has started is not a resend invitation: the row is `unknown`, the page says to check the
/// outbox, and it is a 502 and never a 504.
#[test]
fn a_send_that_times_out_after_aoide_started_is_an_unknown_row_and_no_resend() {
    let rig = Setup::new().env("FAKE_AOIDE_SLEEP_MS", "3000").env("FAKE_AOIDE_SPARES_SCHEMA", "1").knob("mail_send_ms", "500").start();
    let name = rig.note("Slow", "text");
    let answer = rig.send(&name, json!({ "node": "self" }));
    assert_eq!(answer.status, 502, "{}{}", answer.head, answer.body);
    assert!(answer.body.contains("may or may not have been sent"), "{}", answer.body);
    let rows = rig.lines("sent.jsonl");
    assert_eq!((rows.len(), rows[0]["status"].clone()), (1, json!("unknown")), "{rows:?}");
    assert_eq!(rig.asked().len(), 1, "one send, and nothing sent again for it: {:?}", rig.argvs());
}

/// Two sends run at once, and a third is 429 at once with a sentence that says to wait; the refused one keeps no row.
#[test]
fn the_third_send_at_once_is_429_and_the_two_that_run_are_kept() {
    let rig = Setup::new().env("FAKE_AOIDE_SLEEP_MS", "800").env("FAKE_AOIDE_SPARES_SCHEMA", "1").start();
    let name = rig.note("Busy", "text");
    let answers: Vec<(u16, String)> = std::thread::scope(|scope| {
        let threads: Vec<_> = (0..3)
            .map(|_| {
                scope.spawn(|| {
                    let answer = rig.send(&name, json!({ "node": "self" }));
                    (answer.status, answer.body)
                })
            })
            .collect();
        threads.into_iter().map(|thread| thread.join().unwrap()).collect()
    });
    let mut codes: Vec<u16> = answers.iter().map(|(status, _)| *status).collect();
    codes.sort_unstable();
    assert_eq!(codes, vec![202, 202, 429], "{answers:?}");
    let refused = answers.iter().find(|(status, _)| *status == 429).unwrap();
    assert!(refused.1.contains("sends are running already"), "{}", refused.1);
    assert_eq!(rig.lines("sent.jsonl").len(), 2, "the two that ran are kept, and the refused one kept nothing");
}

/// A schema that does not list a flag the perch sends turns mail off, with a sentence that names the flag, for the listing and every send.
#[test]
fn a_schema_without_a_flag_the_perch_sends_turns_mail_off_with_the_sentence() {
    let rig = Setup::new().start();
    let mut schema: Value = serde_json::from_str(&std::fs::read_to_string(rig.fixtures.join("schema.json")).unwrap()).unwrap();
    let read = schema["commands"].as_array_mut().unwrap().iter_mut().find(|command| command["path"] == json!(["mail", "read"])).unwrap();
    let kept: Vec<Value> = read["flags"].as_array().unwrap().iter().filter(|flag| flag["name"] != "reread").cloned().collect();
    read["flags"] = Value::Array(kept);
    std::fs::write(rig.fixtures.join("schema.json"), schema.to_string()).unwrap();
    let mail = until(Duration::from_secs(10), "the schema's refusal", || {
        let mail = rig.mail();
        mail["error"].as_str().is_some_and(|error| error.contains("--reread")).then_some(mail)
    });
    assert_eq!(mail["available"], json!(false), "{mail}");
    let name = rig.note("Off", "text");
    let answer = rig.send(&name, json!({ "node": "self" }));
    assert_eq!(answer.status, 503, "{}{}", answer.head, answer.body);
    assert!(answer.body.contains("--reread"), "{}", answer.body);
    assert!(rig.asked().is_empty(), "nothing was sent: {:?}", rig.argvs());
}

/// The perch's own proxy and provider key are not handed to aoide: the runner passes its allowlist, and a send's call shows neither.
#[test]
fn the_proxy_and_the_provider_key_of_the_perch_do_not_reach_aoide() {
    let rig = Setup::new().env("HTTP_PROXY", "http://proxy.invalid:3128").env("OPENAI_API_KEY", "sk-made-up-for-the-test").start();
    let name = rig.note("Env", "text");
    json_of(&rig.send(&name, json!({ "node": "self" })), 202, "the send");
    let calls = rig.calls();
    assert!(!calls.is_empty());
    for call in &calls {
        let names: Vec<&str> = call["env_names"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        for banned in ["HTTP_PROXY", "OPENAI_API_KEY"] {
            assert!(!names.contains(&banned), "{banned} reached aoide: {names:?}");
        }
    }
}
