//! `fake_aoide` — a stand-in for the `aoide` CLI, for the perch's tests (slice R2a). The runner (`perch::aoide`), the mesh view and mail are
//! driven through it as `CARGO_BIN_EXE_fake_aoide`; the real `aoide` is never run by a test.
//!
//! **It accepts exactly the argvs the runner builds, and exits 99 with nothing on stdout for anything else.** That is the other half of
//! the runner's own checks: a runner that drifted (a spaced flag, a missing `--`, a node it should have refused) is refused here by a
//! parser that shares no code with it. The shapes, in the order the runner spells them:
//!
//! ```text
//! schema
//! mesh --json
//! mail outbox --json
//! mail read --for=minerva --json [--reread]          AOIDE_SESSION_ID must be minerva-perch, else exit 99
//! mail send --json --to=<node>/minerva --from=<minerva|minerva-agent> [--mesh=<m>] --subject=<s> [--hold] -- <text>
//! ```
//!
//! `--version` is not a shape: aoide 0.0.26 has no such flag and exits 2 with usage on stderr, and so does this (the perch never asks).
//!
//! `<node>` is `self` or a name matching `^[a-z0-9][a-z0-9-]*$`; so is `<m>`. The subject is one line of 1 to 120 characters. `--hold` and
//! `--mesh` with `self` are refused, as aoide refuses a hold without `<node>/<name>`.
//!
//! **It replays, it does not compute.** Each shape prints a file of the fixture directory `FAKE_AOIDE_DIR` (required; the perch keeps its
//! own at `tests/fixtures/aoide/`): `schema.json` (what `schema` prints: the version and the commands the perch uses), `mesh.json`, `outbox.json`, `mail-read.json` and
//! `mail-read-reread.json` (what a `--reread` prints; falls back to `mail-read.json`), and for a send `send.json`, a table by node with
//! the exit code and stdout of the reply, `with_hold` and `with_mesh` variants, and `unknown-node` generated for a node that is not in it.
//! A missing file is exit 99 with the file's name, so a test that forgot its fixture fails loudly and not as an empty answer.
//!
//! **What it records** is `FAKE_AOIDE_LOG`, one JSON line per call, written before anything else (so a refused call is in it too):
//! `{"argv": [...], "session": <AOIDE_SESSION_ID or null>, "aoide_env": {every other AOIDE_* variable}, "xdg_runtime_dir": ...,`
//! `"env_names": [...], "pid": N}`, where `env_names` is the name, and never the value, of every variable the call was run with (how a
//! test sees what the runner let through).
//! A call that sleeps adds `{"event": "woke", "pid": N}` when it wakes, so a test can tell a child that was killed from one that was not.
//! A line with an `event` is not a call; a line with an `argv` is.
//!
//! **Knobs**, by environment: `FAKE_AOIDE_SLEEP_MS` (wait before answering), `FAKE_AOIDE_FLOOD=<n>` (print n bytes of `x` on stdout in
//! place of the fixture), `FAKE_AOIDE_STDERR_FLOOD=<n>` (write n bytes to stderr first, more than a pipe holds), `FAKE_AOIDE_STRAGGLER_MS=<n>`
//! (leave a process that holds stdout open for n ms after the answer, as a daemon an aoide started would), and `FAKE_AOIDE_STDIN=1` (read
//! stdin to its end first and log `{"event": "stdin", "bytes": N}`: a closed stdin ends at once, a held-open pipe never does), and
//! `FAKE_AOIDE_SPARES_SCHEMA=1` (the sleep and the flood leave `schema` alone, so a slowed or flooded send follows a schema check that passed),
//! and `FAKE_AOIDE_FLOOD_SPARES_REREAD=1` (the flood leaves a `--reread` alone, so a plain read is the one that is too big).
//!
//! The perch hands its children only its allowlist, so a test that wants these knobs in the child runs the fake through a script that
//! sets them (`steered` in `tests/mail.rs`, `wrapper` in `tests/mesh.rs`).
//!
//! Built only with `--features test-bins` (`required-features` in Cargo.toml), so a normal `cargo build` ships no test double.

use std::io::Write as _;
use std::path::PathBuf;

use serde_json::{json, Map, Value};

const READER: &str = "minerva-perch";
const SUBJECT_MAX: usize = 120;
const TEXT_MAX: usize = 64 * 1024;

enum Call {
    Schema,
    Mesh,
    Outbox,
    Read { reread: bool },
    Send(Letter),
}

struct Letter {
    node: String,
    mesh: Option<String>,
    hold: bool,
}

fn main() {
    let argv: Vec<String> = std::env::args_os().skip(1).map(|arg| arg.to_string_lossy().into_owned()).collect();
    log(&argv);
    if argv == ["--version"] {
        // The real aoide's answer to a flag it does not have: usage on stderr, and exit 2.
        eprintln!("[usage] --version: unknown command: `--version`\n\nrun 'aoide --help' for the full command list");
        std::process::exit(2);
    }
    let call = parse(&argv).unwrap_or_else(|why| refuse(&why));
    if let Call::Read { .. } = call {
        let session = std::env::var("AOIDE_SESSION_ID").ok();
        if session.as_deref() != Some(READER) {
            refuse(&format!("mail read as {session:?}: the reader is {READER}"));
        }
    }
    if std::env::var_os("FAKE_AOIDE_STDIN").is_some() {
        // Reads stdin to its end: at once for a closed one (/dev/null), never for a pipe somebody holds open.
        let mut taken = Vec::new();
        let bytes = std::io::Read::read_to_end(&mut std::io::stdin(), &mut taken).map(|_| taken.len()).ok();
        append(&json!({ "event": "stdin", "pid": std::process::id(), "bytes": bytes }));
    }
    // `FAKE_AOIDE_SPARES_SCHEMA=1`: the sleep and the flood leave `schema` alone, so a test can slow or flood a send once the schema check
    // has passed.
    let spares = std::env::var_os("FAKE_AOIDE_SPARES_SCHEMA").is_some() && matches!(call, Call::Schema);
    if let Some(ms) = number("FAKE_AOIDE_SLEEP_MS").filter(|_| !spares) {
        std::thread::sleep(std::time::Duration::from_millis(ms));
        append(&json!({ "event": "woke", "pid": std::process::id() }));
    }
    if let Some(bytes) = number("FAKE_AOIDE_STDERR_FLOOD") {
        flood(&mut std::io::stderr(), bytes, b'e');
    }
    if let Some(ms) = number("FAKE_AOIDE_STRAGGLER_MS") {
        // A process that outlives this one and holds the same pipes. `sleep` is run by `sh` so there is nothing to wait on here.
        let _ = std::process::Command::new("/bin/sh").arg("-c").arg(format!("sleep {}.{:03}", ms / 1000, ms % 1000)).spawn();
    }
    // `FAKE_AOIDE_FLOOD_SPARES_REREAD=1`: the flood leaves a reread alone, so the plain read is the one that is too big.
    let flood_spares = spares || (std::env::var_os("FAKE_AOIDE_FLOOD_SPARES_REREAD").is_some() && matches!(call, Call::Read { reread: true }));
    if let Some(bytes) = number("FAKE_AOIDE_FLOOD").filter(|_| !flood_spares) {
        flood(&mut std::io::stdout(), bytes, b'x');
        std::process::exit(0);
    }
    let (code, stdout) = answer(&call);
    let mut out = std::io::stdout();
    let _ = out.write_all(stdout.as_bytes());
    let _ = out.flush();
    std::process::exit(code);
}

/// Exit 99 with the reason on stderr and nothing on stdout, which a caller would parse.
fn refuse(why: &str) -> ! {
    eprintln!("fake_aoide: {why}");
    std::process::exit(99);
}

fn number(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.parse().ok()
}

fn flood(to: &mut dyn std::io::Write, bytes: u64, fill: u8) {
    let chunk = vec![fill; 8192];
    let mut left = bytes;
    while left > 0 {
        let n = left.min(chunk.len() as u64) as usize;
        if to.write_all(&chunk[..n]).is_err() {
            return;
        }
        left -= n as u64;
    }
    let _ = to.flush();
}

// ---------------------------------------------------------------------------
// The log
// ---------------------------------------------------------------------------

fn append(line: &Value) {
    let Some(path) = std::env::var_os("FAKE_AOIDE_LOG") else { return };
    let mut text = line.to_string();
    text.push('\n');
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(text.as_bytes());
    }
}

fn log(argv: &[String]) {
    let mut aoide_env = Map::new();
    for (key, value) in std::env::vars_os() {
        let key = key.to_string_lossy().into_owned();
        if key.starts_with("AOIDE_") && key != "AOIDE_SESSION_ID" {
            aoide_env.insert(key, Value::String(value.to_string_lossy().into_owned()));
        }
    }
    append(&json!({
        "argv": argv,
        "session": std::env::var("AOIDE_SESSION_ID").ok(),
        "aoide_env": aoide_env,
        "xdg_runtime_dir": std::env::var("XDG_RUNTIME_DIR").ok(),
        // The names only, never a value, of every variable the call was run with: how a test sees what the runner let through.
        "env_names": std::env::vars_os().map(|(key, _)| key.to_string_lossy().into_owned()).collect::<Vec<_>>(),
        "pid": std::process::id(),
    }));
}

// ---------------------------------------------------------------------------
// The shapes
// ---------------------------------------------------------------------------

fn name_ok(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn parse(argv: &[String]) -> Result<Call, String> {
    let words: Vec<&str> = argv.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["schema"] => Ok(Call::Schema),
        ["mesh", "--json"] => Ok(Call::Mesh),
        ["mail", "outbox", "--json"] => Ok(Call::Outbox),
        ["mail", "read", "--for=minerva", "--json"] => Ok(Call::Read { reread: false }),
        ["mail", "read", "--for=minerva", "--json", "--reread"] => Ok(Call::Read { reread: true }),
        ["mail", "send", rest @ ..] => parse_send(rest),
        _ => Err(format!("not a shape the perch runs: {argv:?}")),
    }
}

fn parse_send(rest: &[&str]) -> Result<Call, String> {
    let mut words = rest.iter().copied().peekable();
    if words.next() != Some("--json") {
        return Err("mail send: --json comes first".to_string());
    }
    let to = words.next().and_then(|word| word.strip_prefix("--to=")).ok_or("mail send: --to=<node>/minerva comes second")?;
    let node = to.strip_suffix("/minerva").ok_or("mail send: the mailbox is minerva")?;
    if node != "self" && !name_ok(node) {
        return Err(format!("mail send: {node:?} is not a node name"));
    }
    let from = words.next().and_then(|word| word.strip_prefix("--from=")).ok_or("mail send: --from=<who> comes third")?;
    if from != "minerva" && from != "minerva-agent" {
        return Err(format!("mail send: --from={from}: the sender is minerva or minerva-agent"));
    }
    let mesh = match words.peek().and_then(|word| word.strip_prefix("--mesh=")) {
        Some(mesh) => {
            words.next();
            if !name_ok(mesh) {
                return Err(format!("mail send: {mesh:?} is not a mesh name"));
            }
            Some(mesh.to_string())
        }
        None => None,
    };
    let subject = words.next().and_then(|word| word.strip_prefix("--subject=")).ok_or("mail send: --subject=<s> comes next")?;
    let line = !subject.chars().any(|c| (c.is_control() && c != '\t') || c == '\u{2028}' || c == '\u{2029}');
    if subject.trim().is_empty() || !line || subject.chars().count() > SUBJECT_MAX {
        return Err("mail send: the subject is one line of 1 to 120 characters".to_string());
    }
    let hold = words.peek() == Some(&"--hold");
    if hold {
        words.next();
    }
    if words.next() != Some("--") {
        return Err("mail send: the text comes after --".to_string());
    }
    let text = words.next().ok_or("mail send: no text")?;
    if words.next().is_some() {
        return Err("mail send: the text is one argument".to_string());
    }
    if text.is_empty() || text.len() > TEXT_MAX {
        return Err("mail send: the text is 1 byte to 64 KiB".to_string());
    }
    if node == "self" && hold {
        return Err("mail send: --hold needs <node>/<name>, not self".to_string());
    }
    if node == "self" && mesh.is_some() {
        return Err("mail send: self is in no mesh".to_string());
    }
    Ok(Call::Send(Letter { node: node.to_string(), mesh, hold }))
}

// ---------------------------------------------------------------------------
// The replies
// ---------------------------------------------------------------------------

fn fixture(name: &str) -> Result<String, String> {
    let dir = std::env::var_os("FAKE_AOIDE_DIR").ok_or("FAKE_AOIDE_DIR is not set")?;
    let path = PathBuf::from(dir).join(name);
    std::fs::read_to_string(&path).map_err(|e| format!("fixture {name}: {e}"))
}

fn answer(call: &Call) -> (i32, String) {
    let replayed = match call {
        Call::Schema => fixture("schema.json").map(|text| (0, text)),
        Call::Mesh => fixture("mesh.json").map(|text| (0, text)),
        Call::Outbox => fixture("outbox.json").map(|text| (0, text)),
        Call::Read { reread } => {
            let reread_file = if *reread { fixture("mail-read-reread.json").ok() } else { None };
            match reread_file {
                Some(text) => Ok((0, text)),
                None => fixture("mail-read.json").map(|text| (0, text)),
            }
        }
        Call::Send(send) => send_reply(send),
    };
    replayed.unwrap_or_else(|why| refuse(&why))
}

/// The scripted reply for a send: the entry for the node in `send.json` (or the one of its `with_hold` / `with_mesh` variants the argv asks
/// for), or `unknown-node` for a node that is not in the table.
fn send_reply(send: &Letter) -> Result<(i32, String), String> {
    let table: Value = serde_json::from_str(&fixture("send.json")?).map_err(|e| format!("fixture send.json: {e}"))?;
    let Some(entry) = table.get(&send.node).filter(|_| send.node != "_") else {
        if send.node == "self" {
            return Err("fixture send.json has no entry for self".to_string());
        }
        let message = format!("no node named `{}`", send.node);
        let envelope = json!({ "status": "error", "command": "mail.send", "message": message, "gated": false,
                               "data": { "reason": "unknown-node", "name": send.node } });
        return Ok((1, serde_json::to_string_pretty(&envelope).unwrap_or_default() + "\n"));
    };
    let variants = [(send.hold, "with_hold"), (send.mesh.is_some(), "with_mesh")];
    let chosen = variants.iter().filter(|(asked, _)| *asked).find_map(|(_, key)| entry.get(*key)).unwrap_or(entry);
    let code = chosen.get("code").and_then(Value::as_i64).unwrap_or(0) as i32;
    let stdout = match chosen.get("stdout") {
        Some(Value::String(raw)) => raw.clone(),
        Some(json) => serde_json::to_string_pretty(json).unwrap_or_default() + "\n",
        None => String::new(),
    };
    Ok((code, stdout))
}
