//! `eidolon [--config C] workflow run|resume ...`: the stand-in for the workflow runner (slice R1a).
//!
//! It **replays the journals F0 captured** (`tests/fixtures/jev/<case>/`, built into this binary) into the run's own session, one step at
//! a time, and ends the way the real door ended: the same exit code (0 completed, 1 parked, 2 failed, 3 refused, a SIGKILL for the
//! case that was killed) and the same last stdout line, with only its `session` pointing at the file written here. The real eidolon, the
//! real jev and a model are not involved: this binary never opens a socket.
//!
//! ```text
//!   FAKE_WORKFLOW_CASE=<a..l | a file>   which case a `run` replays (default a). A file's first word is read at every start, so a live
//!                                        check can change the case between runs: `echo c > $T/case`
//!   FAKE_WORKFLOW_STEP_MS=<ms>           the pause before each step (a WorkflowStep record and what follows it); 0 by default
//!   FAKE_WORKFLOW_LINGER_MS=<ms>         the pause between the last record and the stdout line (a window to kill the child in)
//!   FAKE_WORKFLOW_HOLD_MS=<ms>           how long a case that waits for SIGTERM (d, l) waits at most; 60 s by default
//!   FAKE_WORKFLOW_ENV=1                  also log the environment the child was started with, to FAKE_LOG
//!   FAKE_WORKFLOW_DEAF=1                 ignore SIGTERM and leave a `sleep 300` in this process group (`workflow-child <pid>` in
//!                                        FAKE_LOG): what a stop has to SIGKILL, and as a group
//!   FAKE_LOG                             one `workflow <args...> config=<bool>` line per call, like the other commands record theirs
//! ```
//!
//! **A run** is invocation 0 of the case; a **resume** is the next one, and what it plays is the journal from where the file stands to the
//! end of that invocation (so a `run` that was killed half way continues from the half). Where the case sits is `<session>.fake` beside
//! the session (a name no `*.eid` listing takes for a session). A resume of a parked case needs `--answer` and one that is not parked
//! refuses it (exit 3, nothing journaled), as the real door does; the case's recorded answer is rewritten to the one given, because the
//! journal records what was answered. A recorded refusal (cases c, g, j) is replayed every time and leaves the case where it was.
//!
//! **Signals.** SIGTERM is answered the way F0 found the real one: `[cancelling]` on stderr, then either the run is `failed`
//! (`the run was cancelled`, exit 2, case d) or the program wins the race and it parks (case l). Cases d and l hold, as they were
//! captured, at the tool that was in flight until SIGTERM comes, then play the rest of what they recorded. SIGTERM at any other point of
//! any case journals a `WorkflowEnd` `Failed` and prints the failed line. Case k ends in SIGKILL, with no end record and no stdout.
//! Records are written one at a time, so a child killed from outside leaves a journal that stops where it was.

use std::io::{Read as _, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::Cmd;

static TERM: AtomicBool = AtomicBool::new(false);

extern "C" fn on_term(_: libc::c_int) {
    TERM.store(true, Ordering::SeqCst);
}

macro_rules! case {
    ($letter:literal) => {
        (
            $letter,
            include_str!(concat!("../../../tests/fixtures/jev/", $letter, "/case.json")),
            include_str!(concat!("../../../tests/fixtures/jev/", $letter, "/journal.ndjson")),
        )
    };
}

/// F0's twelve cases: `(letter, case.json, journal.ndjson)`.
const CASES: [(&str, &str, &str); 12] =
    [case!("a"), case!("b"), case!("c"), case!("d"), case!("e"), case!("f"), case!("g"), case!("h"), case!("i"), case!("j"), case!("k"), case!("l")];

/// One command the real door was given in a case, and how it ended.
struct Inv {
    answer: Option<String>,
    signal: Option<String>,
    exit: i32,
    stdout: String,
    stderr: String,
    /// How many journal lines there were once it had ended.
    records: usize,
}

struct Case {
    letter: String,
    lines: Vec<&'static str>,
    invs: Vec<Inv>,
}

fn load(letter: &str) -> Option<Case> {
    let (_, meta, journal) = CASES.iter().find(|case| case.0 == letter)?;
    let meta: Value = serde_json::from_str(meta).ok()?;
    let invs = meta["invocations"]
        .as_array()?
        .iter()
        .map(|inv| {
            Some(Inv {
                answer: inv["answer"].as_str().map(str::to_string),
                signal: inv["signal"].as_str().map(str::to_string),
                exit: i32::try_from(inv["exit"].as_i64()?).ok()?,
                stdout: inv["stdout"].as_str()?.to_string(),
                stderr: inv["stderr"].as_str()?.to_string(),
                records: usize::try_from(inv["records"].as_u64()?).ok()?,
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(Case { letter: letter.to_string(), lines: journal.lines().filter(|line| !line.trim().is_empty()).collect(), invs })
}

fn case_letter() -> String {
    let raw = std::env::var("FAKE_WORKFLOW_CASE").unwrap_or_else(|_| "a".to_string());
    let raw = raw.trim();
    if raw.len() == 1 {
        return raw.to_string();
    }
    std::fs::read_to_string(raw).ok().and_then(|text| text.split_whitespace().next().map(str::to_string)).unwrap_or_else(|| "a".to_string())
}

fn millis(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|ms| ms.trim().parse().ok()).unwrap_or(default)
}

/// Sleeps `ms`, in short slices; whether SIGTERM came meanwhile.
fn nap(ms: u64) -> bool {
    let until = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < until && !TERM.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(5).min(until.saturating_duration_since(Instant::now())));
    }
    TERM.load(Ordering::SeqCst)
}

/// The flags and positionals of `workflow run|resume`. Every flag takes a value (`--flag value` or `--flag=value`).
struct Parsed {
    positional: Vec<String>,
    flags: Vec<(String, String)>,
}

impl Parsed {
    fn flag(&self, name: &str) -> Option<&str> {
        self.flags.iter().find(|(flag, _)| flag == name).map(|(_, value)| value.as_str())
    }
}

fn parse(args: &[String]) -> Result<Parsed, String> {
    const FLAGS: [&str; 9] = ["args", "version", "model", "provider", "cwd", "budget-steps", "budget-spend", "plugin", "answer"];
    let (mut positional, mut flags) = (Vec::new(), Vec::new());
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        let Some(flag) = arg.strip_prefix("--") else {
            positional.push(arg.clone());
            continue;
        };
        let (name, value) = match flag.split_once('=') {
            Some((name, value)) => (name, value.to_string()),
            None => (flag, rest.next().ok_or_else(|| format!("a value is required for '--{flag}'"))?.clone()),
        };
        if !FLAGS.contains(&name) {
            return Err(format!("unexpected argument '--{name}' found"));
        }
        flags.push((name.to_string(), value));
    }
    Ok(Parsed { positional, flags })
}

fn usage(why: &str) -> i32 {
    eprintln!("fake_eidolon: workflow: {why}");
    2
}

/// What the door prints to stdout when it refuses, and exit 3.
fn refused(why: &str) -> i32 {
    println!("{}", json!({ "error": why, "status": "refused" }));
    let _ = std::io::stdout().flush();
    3
}

fn log_call(cmd: &Cmd) {
    let Some(path) = std::env::var_os("FAKE_LOG").map(PathBuf::from) else { return };
    let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) else { return };
    let _ = writeln!(file, "workflow {} config={}", cmd.args.join(" "), cmd.config.is_some());
    // Only when asked: a test that wants to prove the child's environment carries no secret.
    if std::env::var_os("FAKE_WORKFLOW_ENV").is_some() {
        for (name, value) in std::env::vars() {
            let _ = writeln!(file, "workflow-env {name}={value}");
        }
    }
}

/// `sessions_dir` of the `--config` file, as the real door reads it; else eidolon's own default.
fn sessions_dir(config: Option<&str>) -> Option<PathBuf> {
    match config {
        Some(path) => {
            let table: toml::Table = std::fs::read_to_string(path).ok()?.parse().ok()?;
            table.get("sessions_dir")?.as_str().map(PathBuf::from)
        }
        None => {
            let data = std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("share")))?;
            Some(data.join("eidolon").join("sessions"))
        }
    }
}

/// `<session>.fake`: `{"case": "b", "next": 1}`, where the case stands.
fn sidecar(session: &Path) -> PathBuf {
    let mut name = session.as_os_str().to_os_string();
    name.push(".fake");
    PathBuf::from(name)
}

pub fn run(cmd: &Cmd) -> i32 {
    // Before anything else: a SIGTERM that comes early is held, not fatal.
    unsafe { libc::signal(libc::SIGTERM, on_term as extern "C" fn(libc::c_int) as libc::sighandler_t) };
    log_call(cmd);
    if std::env::var_os("FAKE_WORKFLOW_DEAF").is_some() {
        deafen();
    }
    let parsed = match parse(cmd.args.get(1..).unwrap_or(&[])) {
        Ok(parsed) => parsed,
        Err(why) => return usage(&why),
    };
    match cmd.args.first().map(String::as_str) {
        Some("run") => start(cmd, &parsed),
        Some("resume") => resume(cmd, &parsed),
        _ => usage("expected `run` or `resume`"),
    }
}

/// `FAKE_WORKFLOW_DEAF`: SIGTERM is ignored (so it is by the sleeper, which inherits that) and the sleeper shares this process group.
fn deafen() {
    unsafe { libc::signal(libc::SIGTERM, libc::SIG_IGN) };
    let child = std::process::Command::new("sleep")
        .arg("300")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    let (Ok(child), Some(path)) = (child, std::env::var_os("FAKE_LOG").map(PathBuf::from)) else { return };
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "workflow-child {}", child.id());
    }
}

fn start(cmd: &Cmd, parsed: &Parsed) -> i32 {
    let [_plugin, _graph] = parsed.positional.as_slice() else { return usage("`run` takes <plugin> <workflow>") };
    let args = match parsed.flag("args") {
        None => "{}".to_string(),
        Some("-") => {
            let mut text = String::new();
            if std::io::stdin().read_to_string(&mut text).is_err() {
                return refused("reading the run's args from stdin");
            }
            text
        }
        Some(text) => text.to_string(),
    };
    match serde_json::from_str::<Value>(args.trim()) {
        Ok(Value::Object(_)) => {}
        Ok(_) => return refused("the run's args are a JSON object"),
        Err(_) => return refused("the run's args are not JSON"),
    }
    let letter = case_letter();
    let Some(case) = load(&letter) else { return usage(&format!("there is no case {letter:?} (a to l)")) };
    let Some(dir) = sessions_dir(cmd.config) else { return usage("no sessions dir: give --config with a sessions_dir") };
    if std::fs::create_dir_all(&dir).is_err() {
        return usage("the sessions dir could not be made");
    }
    // `<ms>.eid`, the way a real session is named; a clash takes the next millisecond.
    let mut ms = u64::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis())).unwrap_or(0);
    let session = loop {
        let path = dir.join(format!("{ms}.eid"));
        if !path.exists() {
            break path;
        }
        ms += 1;
    };
    if std::fs::OpenOptions::new().create_new(true).write(true).mode(0o600).open(&session).is_err() || write_sidecar(&session, &case.letter, 1).is_err() {
        return usage("the session could not be made");
    }
    play(&case, 0, &session, None, 0)
}

fn write_sidecar(session: &Path, letter: &str, next: usize) -> std::io::Result<()> {
    std::fs::write(sidecar(session), json!({ "case": letter, "next": next }).to_string())
}

fn resume(_cmd: &Cmd, parsed: &Parsed) -> i32 {
    let [session, run] = parsed.positional.as_slice() else { return usage("`resume` takes <session> <run>") };
    if parsed.flag("plugin").is_none() {
        return usage("the following required arguments were not provided: --plugin <PLUGIN>");
    }
    if run.parse::<u64>().is_err() {
        return usage("invalid value for '<RUN>': not a number");
    }
    let session = PathBuf::from(session);
    if !session.is_file() {
        return refused(&format!("{}: No such file or directory", session.display()));
    }
    let Some((letter, next)) = std::fs::read_to_string(sidecar(&session)).ok().and_then(|text| {
        let said: Value = serde_json::from_str(&text).ok()?;
        Some((said["case"].as_str()?.to_string(), usize::try_from(said["next"].as_u64()?).ok()?))
    }) else {
        return refused(&format!("no run #{run} in {}", session.display()));
    };
    let Some(case) = load(&letter) else { return refused(&format!("no run #{run} in {}", session.display())) };
    if run != "1" {
        return refused(&format!("no run #{run} in {}", session.display()));
    }
    let answer = parsed.flag("answer");
    let Some(inv) = case.invs.get(next) else {
        return refused(&if answer.is_some() { format!("run #{run} has no unanswered park to answer") } else { format!("refusing run #{run}: it has ended") });
    };
    match (&inv.answer, answer) {
        (Some(_), None) => return refused(&format!("refusing run #{run}: it is parked and waits for --answer")),
        (None, Some(_)) => return refused(&format!("run #{run} has no unanswered park to answer")),
        _ => {}
    }
    // A recorded refusal (F0 c, g, j) touched nothing and is the answer every time.
    if inv.exit == 3 {
        println!("{}", inv.stdout);
        let _ = std::io::stdout().flush();
        return 3;
    }
    // Written before anything is played: a child killed on the way leaves the case on the next invocation.
    if write_sidecar(&session, &case.letter, next + 1).is_err() {
        return usage("the session could not be updated");
    }
    let have = std::fs::read_to_string(&session).map_or(0, |text| text.lines().filter(|line| !line.trim().is_empty()).count());
    play(&case, next, &session, answer, have)
}

/// A step starts at a `WorkflowStep` record; what follows it up to the next one (its verdict, its result, a park) belongs to it.
fn begins_step(line: &str) -> bool {
    line.contains("\"kind\":{\"WorkflowStep\"")
}

fn json_string(text: &str) -> String {
    Value::String(text.to_string()).to_string()
}

/// Appends the records of invocation `idx` that the file does not have yet (from line `from`), a step at a time, and ends as the real
/// door did. The return value is the exit code.
fn play(case: &Case, idx: usize, session: &Path, answer: Option<&str>, from: usize) -> i32 {
    let inv = &case.invs[idx];
    for line in inv.stderr.lines().filter(|line| !line.is_empty() && *line != "[cancelling]") {
        eprintln!("{line}");
    }
    let to = inv.records.min(case.lines.len());
    let mut records: Vec<String> = case.lines[from.min(to)..to].iter().map(|line| line.to_string()).collect();
    // What was answered is what the journal says was answered.
    if let (Some(recorded), Some(given)) = (&inv.answer, answer)
        && recorded != given
        && let Some(line) = records.iter_mut().find(|line| line.contains("\"AskUser\"") && line.contains("\"answer\":"))
    {
        *line = line.replacen(&format!("\"answer\":{}", json_string(recorded)), &format!("\"answer\":{}", json_string(given)), 1);
    }
    let step_ms = millis("FAKE_WORKFLOW_STEP_MS", 0);
    // A case recorded under SIGTERM waits at the tool that was in flight (after the last ApiRequest) until the signal comes.
    let hold_at = (inv.signal.as_deref() == Some("TERM")).then(|| records.iter().rposition(|line| line.contains("\"kind\":{\"ApiRequest\"")).map(|at| at + 1)).flatten();
    let Ok(mut file) = std::fs::OpenOptions::new().append(true).open(session) else { return usage("the session could not be opened") };
    let mut past_hold = false;
    for (at, line) in records.iter().enumerate() {
        if hold_at == Some(at) {
            let until = Instant::now() + Duration::from_millis(millis("FAKE_WORKFLOW_HOLD_MS", 60_000));
            while !TERM.load(Ordering::SeqCst) && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(5));
            }
            eprintln!("[cancelling]");
            past_hold = true;
        } else if !past_hold {
            // The pause before a step, which a SIGTERM cuts short; and a SIGTERM that came during the last write.
            let signalled = if at > 0 && begins_step(line) { nap(step_ms) } else { TERM.load(Ordering::SeqCst) };
            if signalled {
                return cancelled(&mut file, from + at, session);
            }
        }
        if writeln!(file, "{line}").and_then(|()| file.flush()).is_err() {
            return usage("the session could not be written");
        }
    }
    if inv.signal.as_deref() == Some("KILL") {
        // The case that was killed: no end record, no stdout, nothing after this.
        unsafe { libc::kill(libc::getpid(), libc::SIGKILL) };
        std::thread::sleep(Duration::from_secs(1));
    }
    let linger = millis("FAKE_WORKFLOW_LINGER_MS", 0);
    if linger > 0 {
        nap(linger);
    }
    if !inv.stdout.is_empty() {
        println!("{}", with_session(&inv.stdout, session));
        let _ = std::io::stdout().flush();
    }
    inv.exit
}

/// The door's recorded line, its `session` the file written here. Keys come out alphabetical, as the door prints them.
fn with_session(line: &str, session: &Path) -> String {
    match serde_json::from_str::<Value>(line) {
        Ok(Value::Object(mut map)) => {
            if map.contains_key("session") {
                map.insert("session".into(), session.display().to_string().into());
            }
            Value::Object(map).to_string()
        }
        _ => line.to_string(),
    }
}

/// SIGTERM in the middle of a run: the end the token wins (F0 d). The journal gets its `WorkflowEnd`, then the failed line.
fn cancelled(file: &mut std::fs::File, written: usize, session: &Path) -> i32 {
    let now = u64::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis())).unwrap_or(0);
    let parent = written.checked_sub(1);
    let end = json!({ "id": written, "parent": parent, "ts_ms": now, "kind": { "WorkflowEnd": { "run": 1, "status": "Failed" } } });
    let _ = writeln!(file, "{end}").and_then(|()| file.flush());
    eprintln!("[cancelling]");
    println!("{}", json!({ "reason": "the run was cancelled", "run": 1, "session": session.display().to_string(), "status": "failed" }));
    let _ = std::io::stdout().flush();
    2
}
