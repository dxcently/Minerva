//! `/hub/term/a/<verb>`: the agent's terminal tools (slice T2), behind AG's scoped term token.
//!
//! Two layers, as `tests/agent.rs` has its own. The gate's answers need no door and run on every build: the master bearer and no
//! credential on the agent paths, the mail token on a term path, the term token from a socket no door holds. Everything an agent does
//! runs through the stand-in door (`--features test-bins`): `FAKE_AGENT_CALL=@<script>` names the calls, one `<verb>:<json>` per line,
//! and a `say` to a session makes its door run them from its own socket. The door logs the status and body the route gave it, so a test
//! reads what the perch answered, not what the door made of it.
//!
//! The wait is the one claim about time: a quiet read answers after its quiet, a timed-out one after its timeout, and a perch stop ends
//! a long wait in under three seconds.

mod common;

use std::ffi::OsString;
use std::path::PathBuf;

use common::*;
#[cfg(feature = "test-bins")]
use std::time::{Duration, Instant};

/// The sentence the guard gives an agent that types `aoide`, spelled here so a change to it is a change to this spec.
#[cfg(feature = "test-bins")]
const REASON_AOIDE: &str = "Minerva: agents send Aoide mail with mail_send (opt-in per chat); the aoide CLI is not for agents here";

// ---------------------------------------------------------------------------
// The gate, with no door
// ---------------------------------------------------------------------------

/// The three files the perch writes for the agent gate, in a directory it makes.
struct Files {
    _root: tempfile::TempDir,
    term: PathBuf,
    mail: PathBuf,
    url: PathBuf,
}

impl Files {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("agent");
        Self { term: dir.join("term.token"), mail: dir.join("mail.token"), url: dir.join("perch.url"), _root: root }
    }

    fn flags(&self) -> Vec<OsString> {
        let mut flags = Vec::new();
        for (flag, path) in [("--term-token-file", &self.term), ("--mail-token-file", &self.mail), ("--agent-url-file", &self.url)] {
            flags.push(flag.into());
            flags.push(path.into());
        }
        flags
    }

    fn term_token(&self) -> String {
        std::fs::read_to_string(&self.term).unwrap().trim().to_string()
    }

    fn mail_token(&self) -> String {
        std::fs::read_to_string(&self.mail).unwrap().trim().to_string()
    }

    /// What the stand-in door reads to find the perch and its tokens (the names `fake_eidolon/agent.rs` reads).
    #[cfg(feature = "test-bins")]
    fn fake_env(&self) -> Vec<(String, String)> {
        vec![
            ("FAKE_AGENT_URL_FILE".into(), self.url.display().to_string()),
            ("FAKE_AGENT_TERM_TOKEN_FILE".into(), self.term.display().to_string()),
            ("FAKE_AGENT_MAIL_TOKEN_FILE".into(), self.mail.display().to_string()),
        ]
    }
}

/// A perch with the three files and no door.
fn gate_perch() -> (Perch, Files) {
    let files = Files::new();
    let perch = Perch::builder().args(files.flags()).start();
    (perch, files)
}

/// A tool's request as the gate sees it when it is not a door: a POST with the given token as the bearer.
fn call(perch: &Perch, path: &str, token: &str) -> Answer {
    Request::new(&perch.addr, "POST", path).bearer(format!("Bearer {token}")).body(b"{}".to_vec()).send(&perch.addr)
}

#[test]
fn without_a_door_the_master_bearer_is_a_404_and_no_credential_a_401_on_every_verb() {
    let perch = Perch::start();
    for verb in ["list", "open", "read", "send", "close"] {
        let path = format!("/hub/term/a/{verb}");
        let answer = perch.apireq("POST", &path).send(&perch.addr);
        assert_eq!(answer.status, 404, "POST {path} with the master bearer: {}", answer.head);
        assert_eq!(answer.body, "", "POST {path}: the refusal has no body");
        let stranger = Request::new(&perch.addr, "POST", &path).send(&perch.addr);
        assert_eq!(stranger.status, 401, "POST {path} with no credential: {}", stranger.head);
    }
}

#[test]
fn the_term_token_from_a_socket_no_door_holds_is_403_and_the_mail_token_is_401() {
    let (perch, files) = gate_perch();
    let (term, mail) = (files.term_token(), files.mail_token());
    assert_ne!(term, mail, "one secret per scope");
    for verb in ["list", "open", "read", "send", "close"] {
        let path = format!("/hub/term/a/{verb}");
        let from_test = call(&perch, &path, &term);
        assert_eq!(from_test.status, 403, "POST {path} with the term token from the test process: {}", from_test.head);
        let wrong_scope = call(&perch, &path, &mail);
        assert_eq!(wrong_scope.status, 401, "POST {path} with the mail token: {}", wrong_scope.head);
    }
}

// ---------------------------------------------------------------------------
// Through the stand-in door
// ---------------------------------------------------------------------------

/// One line of the door's log, read back: `agent pid=<pid> via=door <scope>/<verb> <status> <body>`, or `... error <why>` when the call
/// got no answer at all.
#[cfg(feature = "test-bins")]
#[derive(Debug)]
struct Answered {
    named: String,
    status: Option<u16>,
    body: serde_json::Value,
    text: String,
}

#[cfg(feature = "test-bins")]
impl Answered {
    fn parse(line: &str) -> Self {
        let tail = line.splitn(4, ' ').nth(3).unwrap_or_else(|| panic!("a door line: {line:?}"));
        let (named, after) = tail.split_once(' ').unwrap_or((tail, ""));
        let (word, text) = after.split_once(' ').unwrap_or((after, ""));
        let status = word.parse::<u16>().ok();
        let body = match status {
            Some(_) => serde_json::from_str(text).unwrap_or(serde_json::Value::Null),
            None => serde_json::Value::Null,
        };
        Self { named: named.to_string(), status, body, text: text.to_string() }
    }

    /// The status. A call that got no answer fails the test here, with what the door said.
    fn code(&self) -> u16 {
        self.status.unwrap_or_else(|| panic!("{} got no answer: {}", self.named, self.text))
    }

    /// The `error` a refusal names (`{"error": why}`).
    fn why(&self) -> &str {
        self.body["error"].as_str().unwrap_or_else(|| panic!("{} {}: no error in {:?}", self.named, self.code(), self.text))
    }

    /// The `id` an answer made.
    fn id(&self) -> String {
        self.body["id"].as_str().unwrap_or_else(|| panic!("{} {}: no id in {:?}", self.named, self.code(), self.text)).to_string()
    }
}

/// A perch with the three files, the stand-in door as its `--eidolon`, `/bin/sh` for every shell, and a `FAKE_AGENT_CALL` that reads its
/// calls from a script this rig writes before each `say`.
#[cfg(feature = "test-bins")]
struct Rig {
    perch: Perch,
    files: Files,
    script: PathBuf,
    _dir: tempfile::TempDir,
}

#[cfg(feature = "test-bins")]
impl Rig {
    fn new() -> Self {
        Self::with_flags(Vec::new())
    }

    /// [`Rig::new`], with more of the perch's own flags (a `--home` a second perch reads its `terms.json` back from).
    fn with_flags(extra: Vec<OsString>) -> Self {
        let files = Files::new();
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("calls.txt");
        std::fs::write(&script, "").unwrap();
        let perch = Perch::builder()
            .hub()
            .args(files.flags())
            .args(extra)
            .envs(files.fake_env())
            .env("SHELL", "/bin/sh")
            .env("FAKE_AGENT_CALL", format!("@{}", script.display()))
            .start();
        Self { perch, files, script, _dir: dir }
    }

    /// Writes `calls` as the script, then says to session `id`: its door runs them one after another, and this returns at once.
    fn say(&self, id: &str, calls: &str) {
        std::fs::write(&self.script, calls).unwrap();
        let said = self.perch.post_json(&format!("/s/{id}/api/say"), serde_json::json!({ "text": "hi" }));
        assert_eq!(said.status, 202, "{}{}", said.head, said.body);
    }

    /// [`Rig::say`], then the answers: waits until the door (pid `pid`) has logged one line per call, and returns them in order.
    fn run(&self, id: &str, pid: u32, calls: &str) -> Vec<Answered> {
        let before = self.lines(pid).len();
        let wanted = calls.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')).count();
        self.say(id, calls);
        let mark = format!("agent pid={pid} ");
        assert!(
            wait_record(&self.perch, Duration::from_secs(20), |records| records.iter().filter(|line| line.starts_with(&mark)).count() >= before + wanted),
            "the door made {wanted} calls: {:?}",
            self.perch.fake_records()
        );
        self.lines(pid).iter().skip(before).map(|line| Answered::parse(line)).collect()
    }

    /// One call, and its answer.
    fn one(&self, id: &str, pid: u32, call: &str) -> Answered {
        self.run(id, pid, call).remove(0)
    }

    /// The lines the door with this pid has logged, in order.
    fn lines(&self, pid: u32) -> Vec<String> {
        let mark = format!("agent pid={pid} ");
        self.perch.fake_records().into_iter().filter(|line| line.starts_with(&mark)).collect()
    }
}

/// Opens a terminal for the chat whose door is `pid`, through that door, and returns its id.
#[cfg(feature = "test-bins")]
fn open_by(rig: &Rig, id: &str, pid: u32) -> String {
    let made = rig.one(id, pid, "term/open:{}");
    assert_eq!(made.code(), 201, "{}", made.text);
    made.id()
}

/// Opens a terminal the way the operator does (no chat), and returns its id.
#[cfg(feature = "test-bins")]
fn open_as_operator(perch: &Perch) -> String {
    let answer = perch.post_json("/hub/term", serde_json::json!({}));
    assert_eq!(answer.status, 201, "{}{}", answer.head, answer.body);
    body_id(&answer)
}

/// `GET /hub/term`, as the operator's page reads it: every terminal, with its folder.
#[cfg(feature = "test-bins")]
fn operator_list(perch: &Perch) -> Vec<serde_json::Value> {
    let answer = perch.get("/hub/term");
    assert_eq!(answer.status, 200, "{}", answer.head);
    serde_json::from_str(&answer.body).unwrap()
}

#[cfg(feature = "test-bins")]
fn operator_row(perch: &Perch, id: &str) -> Option<serde_json::Value> {
    operator_list(perch).into_iter().find(|row| row["id"] == id)
}

/// The row of `id` in an agent's `list` answer.
#[cfg(feature = "test-bins")]
fn listed(rows: &serde_json::Value, id: &str) -> serde_json::Value {
    rows.as_array().unwrap().iter().find(|row| row["id"] == id).cloned().unwrap_or_else(|| panic!("{id} is not in {rows}"))
}

/// No secret is in the perch's argv or environment, its boot lines or its trace-level log: the harness runs every perch at `RUST_LOG=trace`.
#[cfg(feature = "test-bins")]
fn assert_no_secret(perch: &Perch, secrets: &[&str]) {
    let (argv, environ) = proc_of(perch.child.id());
    for secret in secrets {
        assert!(secret.len() >= 40, "a real token, not a placeholder");
        assert!(!argv.contains(secret), "a token in the perch's argv");
        assert!(!environ.contains(secret), "a token in the perch's environment");
        assert!(!perch.out().contains(secret), "a token on the perch's stdout");
        assert!(!perch.err().contains(secret), "a token in the perch's log");
    }
}

#[cfg(feature = "test-bins")]
#[test]
fn list_marks_the_chats_own_terminals_and_no_others() {
    let rig = Rig::new();
    let (a, pid_a, _) = live(&rig.perch);
    let (b, pid_b, _) = live(&rig.perch);
    let (c, pid_c, _) = live(&rig.perch);
    let operator = open_as_operator(&rig.perch);
    let ours = open_by(&rig, &a, pid_a);
    let third = open_by(&rig, &c, pid_c);

    let from_a = rig.one(&a, pid_a, "term/list:{}");
    assert_eq!(from_a.code(), 200, "{}", from_a.text);
    let mine = listed(&from_a.body, &ours);
    assert_eq!(mine["mine"], true, "the terminal this chat opened is its own");
    assert_eq!(mine["by"], "agent");
    assert_eq!(mine["chat"], a.as_str());
    assert_eq!(mine["alive"], true, "a running terminal is listed alive");
    assert_eq!(mine["hold"], false, "and it is not on hold until the operator holds it");
    assert!(mine["title"].is_string() && mine["fg"].is_string(), "the row names its title and foreground: {mine}");
    let theirs = listed(&from_a.body, &operator);
    assert_eq!(theirs["mine"], false, "the operator's terminal is not the agent's");
    assert_eq!(theirs["by"], "operator");

    let from_b = rig.one(&b, pid_b, "term/list:{}");
    assert_eq!(from_b.code(), 200, "{}", from_b.text);
    assert_eq!(listed(&from_b.body, &ours)["mine"], false, "a terminal another chat opened is listed, and is not mine");
    assert_eq!(listed(&from_b.body, &operator)["mine"], false);
    assert_eq!(listed(&from_b.body, &third)["mine"], false, "a third chat's terminal is listed too, and is not mine");
}

#[cfg(feature = "test-bins")]
#[test]
fn open_takes_the_chats_folder_and_refuses_a_fifth_terminal() {
    let rig = Rig::new();
    let (a, pid, _) = live(&rig.perch);
    let titled = rig.one(&a, pid, "term/open:{\"title\":\"w\",\"cols\":100,\"rows\":30}");
    assert_eq!(titled.code(), 201, "{}", titled.text);
    let made = rig.run(&a, pid, &"term/open:{}\n".repeat(4));
    let codes: Vec<u16> = made.iter().map(Answered::code).collect();
    assert_eq!(codes, [201, 201, 201, 409]);
    assert_eq!(made[3].why(), "this chat has 4 terminals open; close one first");

    let work = rig.perch.work.canonicalize().unwrap().display().to_string();
    let mut ids = vec![titled.id()];
    ids.extend(made[..3].iter().map(Answered::id));
    for id in &ids {
        let row = operator_row(&rig.perch, id).unwrap();
        assert_eq!(row["cwd"], work.as_str(), "a shell starts in the folder its chat was given");
    }

    let sized = operator_row(&rig.perch, &titled.id()).unwrap();
    assert_eq!((sized["cols"].as_u64(), sized["rows"].as_u64()), (Some(100), Some(30)), "the size the chat asked for: {sized}");
    let plain = operator_row(&rig.perch, &made[0].id()).unwrap();
    assert_eq!((plain["cols"].as_u64(), plain["rows"].as_u64()), (Some(120), Some(32)), "a plain open is 120 by 32: {plain}");
    let listing = rig.one(&a, pid, "term/list:{}");
    assert_eq!(listing.code(), 200, "{}", listing.text);
    assert_eq!(listed(&listing.body, &titled.id())["title"], "w", "the chat's list names the title it gave");
}

#[cfg(feature = "test-bins")]
#[test]
fn open_then_send_then_read_for_the_text_answers_with_it() {
    let rig = Rig::new();
    let (a, pid, _) = live(&rig.perch);
    // The quiet clock is a minute, so only the text can answer the read early: a read that ignores `for` waits out its 20 s.
    let calls = "term/open:{}\n\
                 term/send:{\"id\":\"$id\",\"text\":\"echo ho$((2*3))\",\"keys\":[\"enter\"]}\n\
                 term/read:{\"id\":\"$id\",\"wait\":{\"for\":\"ho6\",\"idle_ms\":60000,\"timeout_s\":20}}";
    let started = Instant::now();
    let made = rig.run(&a, pid, calls);
    let took = started.elapsed();
    let codes: Vec<u16> = made.iter().map(Answered::code).collect();
    assert_eq!(codes, [201, 200, 200]);
    assert!(made[1].body["seq"].is_u64(), "a send answers the terminal's seq: {}", made[1].text);
    assert!(made[2].body["text"].as_str().unwrap().contains("ho6"), "the read waited for the output: {}", made[2].text);
    assert_eq!(made[2].body["alive"], true);
    let cursor = made[2].body.get("cursor");
    assert!(cursor.is_some_and(|cursor| cursor.is_object() || cursor.is_null()), "the read names its cursor: {}", made[2].text);
    assert!(made[2].body["alt_screen"].is_boolean(), "the read names whether a full-screen program is up: {}", made[2].text);
    assert!(made[2].body["seq"].is_u64(), "the read names the terminal's seq: {}", made[2].text);
    assert!(took < Duration::from_secs(15), "the read answered on its text, not at its 20 s timeout: {took:?}");
}

#[cfg(feature = "test-bins")]
#[test]
fn the_operators_terminal_is_readable_and_writable_by_an_agent() {
    let rig = Rig::new();
    let (a, pid, _) = live(&rig.perch);
    let operator = open_as_operator(&rig.perch);
    let calls = format!(
        "term/read:{{\"id\":\"{operator}\"}}\n\
         term/send:{{\"id\":\"{operator}\",\"text\":\"echo opq$((4+1))\",\"keys\":[\"enter\"]}}\n\
         term/read:{{\"id\":\"{operator}\",\"wait\":{{\"for\":\"opq5\",\"timeout_s\":20}}}}"
    );
    let made = rig.run(&a, pid, &calls);
    let codes: Vec<u16> = made.iter().map(Answered::code).collect();
    assert_eq!(codes, [200, 200, 200]);
    assert!(made[2].body["text"].as_str().unwrap().contains("opq5"), "{}", made[2].text);
}

#[cfg(feature = "test-bins")]
#[test]
fn another_chats_terminal_is_readable_and_writable_but_only_its_opener_closes_it() {
    let rig = Rig::new();
    let (a, pid_a, _) = live(&rig.perch);
    let (b, pid_b, _) = live(&rig.perch);
    let ours = open_by(&rig, &a, pid_a);
    let operator = open_as_operator(&rig.perch);
    let calls = format!(
        "term/read:{{\"id\":\"{ours}\"}}\n\
         term/send:{{\"id\":\"{ours}\",\"text\":\"echo cb$((7*6))\",\"keys\":[\"enter\"]}}\n\
         term/read:{{\"id\":\"{ours}\",\"wait\":{{\"for\":\"cb42\",\"timeout_s\":20}}}}\n\
         term/close:{{\"id\":\"{ours}\"}}\n\
         term/close:{{\"id\":\"{operator}\"}}"
    );
    let made = rig.run(&b, pid_b, &calls);
    let codes: Vec<u16> = made.iter().map(Answered::code).collect();
    assert_eq!(codes, [200, 200, 200, 403, 403]);
    assert!(made[2].body["text"].as_str().unwrap().contains("cb42"), "{}", made[2].text);
    assert_eq!(made[3].why(), "only a terminal this chat opened can be closed");
    assert_eq!(made[4].why(), "only a terminal this chat opened can be closed");

    let closed = rig.one(&a, pid_a, &format!("term/close:{{\"id\":\"{ours}\"}}"));
    assert_eq!(closed.code(), 202, "{}", closed.text);
    assert_eq!(closed.body["id"], ours.as_str());
    assert!(operator_row(&rig.perch, &ours).is_none(), "a closed terminal leaves the list");
}

#[cfg(feature = "test-bins")]
#[test]
fn the_operators_hold_refuses_an_agents_send_with_423_and_its_reads_still_answer() {
    let rig = Rig::new();
    let (a, pid, _) = live(&rig.perch);
    let ours = open_by(&rig, &a, pid);
    let on = rig.perch.post_json(&format!("/hub/term/{ours}/hold"), serde_json::json!({ "hold": true }));
    assert_eq!(on.status, 200, "{}", on.body);
    let listing = rig.one(&a, pid, "term/list:{}");
    assert_eq!(listing.code(), 200, "{}", listing.text);
    assert_eq!(listed(&listing.body, &ours)["hold"], true, "the list says the operator holds it");

    let made = rig.run(
        &a,
        pid,
        &format!("term/send:{{\"id\":\"{ours}\",\"text\":\"echo held\",\"keys\":[\"enter\"]}}\nterm/read:{{\"id\":\"{ours}\"}}"),
    );
    assert_eq!(made[0].code(), 423, "{}", made[0].text);
    assert!(made[0].why().contains("on hold"), "{}", made[0].text);
    assert_eq!(made[1].code(), 200, "a read answers while the hold is on");
    assert!(!made[1].body["text"].as_str().unwrap().contains("echo held"), "a refused send types nothing");

    let off = rig.perch.post_json(&format!("/hub/term/{ours}/hold"), serde_json::json!({ "hold": false }));
    assert_eq!(off.status, 200, "{}", off.body);
    let released = rig.one(&a, pid, "term/list:{}");
    assert_eq!(released.code(), 200, "{}", released.text);
    assert_eq!(listed(&released.body, &ours)["hold"], false, "and not once it is let go");
    let again = rig.one(&a, pid, &format!("term/send:{{\"id\":\"{ours}\",\"text\":\"echo back$((1+1))\",\"keys\":[\"enter\"]}}"));
    assert_eq!(again.code(), 200, "a send after the hold is off: {}", again.text);
}

#[cfg(feature = "test-bins")]
#[test]
fn a_wait_that_is_never_met_answers_at_its_timeout() {
    let rig = Rig::new();
    let (a, pid, _) = live(&rig.perch);
    let ours = open_by(&rig, &a, pid);
    let started = Instant::now();
    let made = rig.one(&a, pid, &format!("term/read:{{\"id\":\"{ours}\",\"wait\":{{\"for\":\"never-printed\",\"idle_ms\":60000,\"timeout_s\":1}}}}"));
    let took = started.elapsed();
    assert_eq!(made.code(), 200, "{}", made.text);
    assert!(!made.body["text"].as_str().unwrap().contains("never-printed"));
    assert!(took >= Duration::from_millis(900), "the timeout is one second, and the answer came in {took:?}");
    assert!(took < Duration::from_secs(20), "the answer came at the timeout, not after it: {took:?}");
}

#[cfg(feature = "test-bins")]
#[test]
fn a_quiet_screen_answers_its_wait_long_before_the_timeout() {
    let rig = Rig::new();
    let (a, pid, _) = live(&rig.perch);
    let ours = open_by(&rig, &a, pid);
    let started = Instant::now();
    let made = rig.one(&a, pid, &format!("term/read:{{\"id\":\"{ours}\",\"wait\":{{\"idle_ms\":300,\"timeout_s\":30}}}}"));
    let took = started.elapsed();
    assert_eq!(made.code(), 200, "{}", made.text);
    assert!(took < Duration::from_secs(15), "a shell at its prompt is quiet at once, not at the 30 s timeout: {took:?}");
}

#[cfg(feature = "test-bins")]
#[test]
fn a_stop_during_a_long_wait_exits_within_three_seconds() {
    let mut rig = Rig::new();
    let (a, pid, _) = live(&rig.perch);
    let ours = open_by(&rig, &a, pid);
    // The read is held inside the perch for up to a minute: the say answers at once, and the door's call waits on the perch.
    rig.say(&a, &format!("term/read:{{\"id\":\"{ours}\",\"wait\":{{\"idle_ms\":60000,\"timeout_s\":60}}}}"));
    std::thread::sleep(Duration::from_millis(500));
    let started = Instant::now();
    let _ = rig.perch.stop();
    let took = started.elapsed();
    assert!(took < Duration::from_secs(3), "a stop ends a held read at once, and it took {took:?}");
    // The held read is answered on the way down: the door logs the 503 the stop gives it, not an error for want of an answer.
    assert!(
        wait_record(&rig.perch, Duration::from_secs(5), |records| records.iter().any(|line| line.starts_with(&format!("agent pid={pid} ")) && line.contains(" term/read "))),
        "the door logged the held read: {:?}",
        rig.perch.fake_records()
    );
    let logged = rig.lines(pid).into_iter().find(|line| line.contains(" term/read ")).unwrap();
    let answered = Answered::parse(&logged);
    assert_eq!(answered.code(), 503, "{}", answered.text);
    assert_eq!(answered.why(), "the perch is shutting down");
}

#[cfg(feature = "test-bins")]
#[test]
fn an_agents_input_reaches_the_stream_and_stamps_the_terminal() {
    let rig = Rig::new();
    let (a, pid, _) = live(&rig.perch);
    let ours = open_by(&rig, &a, pid);
    let before = operator_row(&rig.perch, &ours).unwrap();
    assert!(before["agent_last_ms"].is_null(), "no agent has typed yet: {before}");
    let mut pane = Pane::open_path(&rig.perch, "/hub/term/events?since=t1:0");

    let sent = rig.one(&a, pid, &format!("term/send:{{\"id\":\"{ours}\",\"text\":\"echo ti\",\"keys\":[\"enter\"]}}"));
    assert_eq!(sent.code(), 200, "{}", sent.text);
    assert!(pane.read_until("\"term-input\"", Duration::from_secs(10)), "the stream names the agent's input: {:?}", pane.data().0);
    let frame = pane.frames().into_iter().find(|frame| frame["type"] == "term-input").unwrap();
    assert_eq!(frame["id"], ours.as_str());
    assert_eq!(frame["by"], "agent");
    assert_eq!(frame["chat"], a.as_str());
    let after = operator_row(&rig.perch, &ours).unwrap();
    assert!(after["agent_last_ms"].is_u64(), "the list says when an agent last typed: {after}");
}

#[cfg(feature = "test-bins")]
#[test]
fn a_send_that_names_aoide_is_refused_before_a_byte_and_stamps_nothing() {
    let rig = Rig::new();
    let (a, pid, _) = live(&rig.perch);
    let ours = open_by(&rig, &a, pid);
    let made = rig.run(
        &a,
        pid,
        &format!("term/send:{{\"id\":\"{ours}\",\"text\":\"aoide x\",\"keys\":[\"enter\"]}}\nterm/read:{{\"id\":\"{ours}\"}}"),
    );
    assert_eq!(made[0].code(), 403, "{}", made[0].text);
    assert_eq!(made[0].why(), REASON_AOIDE);
    assert_eq!(made[1].code(), 200, "{}", made[1].text);
    assert!(!made[1].body["text"].as_str().unwrap().contains("aoide x"), "a refused send types nothing");
    let row = operator_row(&rig.perch, &ours).unwrap();
    assert!(row["agent_last_ms"].is_null(), "a refused send is not an agent typing: {row}");
}

#[cfg(feature = "test-bins")]
#[test]
fn a_send_with_a_bad_shape_is_400_and_an_unknown_terminal_is_404() {
    let rig = Rig::new();
    let (a, pid, _) = live(&rig.perch);
    let keys33 = format!("term/send:{{\"id\":\"$id\",\"keys\":[{}]}}", vec!["\"enter\""; 33].join(","));
    let long = format!("term/send:{{\"id\":\"$id\",\"text\":\"{}\"}}", "x".repeat(4097));
    let calls = [
        "term/open:{}",
        "term/send:{\"id\":\"$id\"}",
        "term/send:{}",
        "term/send:{\"id\":\"$id\",\"text\":\"x\",\"keys\":[\"nope\"]}",
        "term/send:{\"id\":\"$id\",\"keys\":[\"enter\",\"ctrl-1\"]}",
        "term/send:{\"id\":\"$id\",\"text\":\"x\",\"bogus\":1}",
        keys33.as_str(),
        long.as_str(),
        "term/send:{\"id\":\"t999\",\"text\":\"x\"}",
        "term/send:{\"id\":\"../etc\",\"text\":\"x\"}",
        "term/read:{\"id\":\"t999\"}",
        "term/close:{\"id\":\"t999\"}",
        "term/open:{\"cwd\":\"/\"}",
    ]
    .join("\n");
    let made = rig.run(&a, pid, &calls);
    let codes: Vec<u16> = made.iter().map(Answered::code).collect();
    assert_eq!(codes, [201, 400, 400, 400, 400, 400, 400, 400, 404, 404, 404, 404, 400]);
    assert!(made[3].why().contains("an unknown key"), "{}", made[3].text);
    assert!(made[7].why().contains("4 KiB"), "{}", made[7].text);
}

#[cfg(feature = "test-bins")]
#[test]
fn a_read_outside_its_bounds_is_400() {
    let rig = Rig::new();
    let (a, pid, _) = live(&rig.perch);
    let ours = open_by(&rig, &a, pid);
    let long_for = "y".repeat(201);
    let calls = [
        format!("term/read:{{\"id\":\"{ours}\",\"lines\":0}}"),
        format!("term/read:{{\"id\":\"{ours}\",\"lines\":401}}"),
        format!("term/read:{{\"id\":\"{ours}\",\"max_chars\":0}}"),
        format!("term/read:{{\"id\":\"{ours}\",\"max_chars\":7001}}"),
        format!("term/read:{{\"id\":\"{ours}\",\"wait\":{{\"for\":\"\"}}}}"),
        format!("term/read:{{\"id\":\"{ours}\",\"wait\":{{\"for\":\"{long_for}\"}}}}"),
        format!("term/read:{{\"id\":\"{ours}\",\"wait\":{{\"idle_ms\":60001}}}}"),
        format!("term/read:{{\"id\":\"{ours}\",\"wait\":{{\"timeout_s\":61}}}}"),
        format!("term/read:{{\"id\":\"{ours}\",\"wait\":{{\"bogus\":1}}}}"),
        format!("term/read:{{\"id\":\"{ours}\",\"bogus\":1}}"),
        "term/read:{}".to_string(),
    ]
    .join("\n");
    let made = rig.run(&a, pid, &calls);
    let codes: Vec<u16> = made.iter().map(Answered::code).collect();
    assert_eq!(codes, [400; 11]);
}

#[cfg(feature = "test-bins")]
#[test]
fn a_body_over_16_kib_is_413_and_a_list_takes_no_fields() {
    let rig = Rig::new();
    let (a, pid, _) = live(&rig.perch);
    let padded = format!("term/list:{{\"pad\":\"{}\"}}", "x".repeat(17_000));
    let made = rig.one(&a, pid, &padded);
    assert_eq!(made.code(), 413, "{}", made.text);
    let named = rig.one(&a, pid, "term/list:{\"pad\":\"x\"}");
    assert_eq!(named.code(), 400, "a list names no field: {}", named.text);
}

#[cfg(feature = "test-bins")]
#[test]
fn a_get_is_405_and_an_unserved_verb_is_404() {
    let rig = Rig::new();
    let (a, pid, _) = live(&rig.perch);
    let made = rig.run(&a, pid, "[method=GET]term/list:{}\n[method=GET]term/open:{}\nterm/nope:{}");
    let codes: Vec<u16> = made.iter().map(Answered::code).collect();
    assert_eq!(codes, [405, 405, 404]);
    let terminals = operator_list(&rig.perch);
    assert!(terminals.is_empty(), "a GET opens nothing: {terminals:?}");
}

#[cfg(feature = "test-bins")]
#[test]
fn the_term_token_is_in_no_argv_environment_stdout_stderr_or_the_doors_log() {
    let rig = Rig::new();
    let (a, pid, _) = live(&rig.perch);
    let ours = open_by(&rig, &a, pid);
    let _ = rig.run(&a, pid, &format!("term/read:{{\"id\":\"{ours}\"}}\nterm/send:{{\"id\":\"{ours}\",\"text\":\"aoide x\"}}"));
    let term = rig.files.term_token();
    assert_no_secret(&rig.perch, &[&term]);
    assert!(!rig.perch.fake_records().iter().any(|line| line.contains(&term)), "the door's log names no token");
}

#[cfg(feature = "test-bins")]
#[test]
fn an_ended_terminal_keeps_its_screen_and_refuses_an_agents_input() {
    let rig = Rig::new();
    let (a, pid, _) = live(&rig.perch);
    let ours = open_by(&rig, &a, pid);
    let typed = rig.run(
        &a,
        pid,
        &format!("term/send:{{\"id\":\"{ours}\",\"text\":\"echo ab$((3*7))\",\"keys\":[\"enter\"]}}\nterm/read:{{\"id\":\"{ours}\",\"wait\":{{\"for\":\"ab21\",\"timeout_s\":20}}}}"),
    );
    let codes: Vec<u16> = typed.iter().map(Answered::code).collect();
    assert_eq!(codes, [200, 200]);

    let exit = rig.one(&a, pid, &format!("term/send:{{\"id\":\"{ours}\",\"text\":\"exit\",\"keys\":[\"enter\"]}}"));
    assert_eq!(exit.code(), 200, "{}", exit.text);
    // A read with no wait answers at once, so the shell's end is polled for, one read at a time, up to a deadline.
    let read_it = format!("term/read:{{\"id\":\"{ours}\"}}");
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut read = rig.one(&a, pid, &read_it);
    while read.code() == 200 && read.body["alive"] == true && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        read = rig.one(&a, pid, &read_it);
    }
    assert_eq!(read.code(), 200, "{}", read.text);
    assert_eq!(read.body["alive"], false, "the shell ended: {}", read.text);
    assert!(!read.body["exit"].is_null(), "an ended shell names its exit: {}", read.text);
    assert!(read.body["text"].as_str().unwrap().contains("ab21"), "the ended terminal keeps the screen it left: {}", read.text);

    let refused = rig.one(&a, pid, &format!("term/send:{{\"id\":\"{ours}\",\"text\":\"echo late\",\"keys\":[\"enter\"]}}"));
    assert_eq!(refused.code(), 409, "{}", refused.text);
    assert_eq!(refused.why(), "that terminal's shell has ended");
}

#[cfg(feature = "test-bins")]
#[test]
fn an_ended_terminal_read_back_from_terms_json_answers_an_agent_from_its_saved_screen() {
    let home = tempfile::tempdir().unwrap();
    let flags: Vec<OsString> = vec!["--home".into(), home.path().into()];
    // The first perch is stopped with its shell alive: the shutdown ends the shell and keeps its screen in `terms.json` under the home.
    let mut first = Rig::with_flags(flags.clone());
    let (a, pid, _) = live(&first.perch);
    let ours = open_by(&first, &a, pid);
    let typed = first.run(
        &a,
        pid,
        &format!("term/send:{{\"id\":\"{ours}\",\"text\":\"echo ab$((3*7))\",\"keys\":[\"enter\"]}}\nterm/read:{{\"id\":\"{ours}\",\"wait\":{{\"for\":\"ab21\",\"timeout_s\":20}}}}"),
    );
    let codes: Vec<u16> = typed.iter().map(Answered::code).collect();
    assert_eq!(codes, [200, 200]);
    let _ = first.perch.stop();

    // The second perch reads the file back: the terminal is ended, and an agent reads the screen the shell left.
    let second = Rig::with_flags(flags);
    let (b, pid_b, _) = live(&second.perch);
    let read = second.one(&b, pid_b, &format!("term/read:{{\"id\":\"{ours}\"}}"));
    assert_eq!(read.code(), 200, "{}", read.text);
    assert_eq!(read.body["alive"], false, "a shell the shutdown ended: {}", read.text);
    assert!(read.body["text"].as_str().unwrap().contains("ab21"), "the screen it left, read back from terms.json: {}", read.text);
    let refused = second.one(&b, pid_b, &format!("term/send:{{\"id\":\"{ours}\",\"text\":\"echo late\",\"keys\":[\"enter\"]}}"));
    assert_eq!(refused.code(), 409, "{}", refused.text);
    assert_eq!(refused.why(), "that terminal's shell has ended");
}
