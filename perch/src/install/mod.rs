//! `/hub/install[/...]`: the install notice and the update keys (slice R4). **This is the trust root of updates**: a key added here is a key
//! whose holder can install software on this machine through `minerva update`, so the module is small, closed, and written to refuse.
//!
//! ```text
//!   GET  /hub/install               minerva status --json          {installed:true, version, notice, plugins, jev}
//!                                                                  | {installed:false, why} | {installed:false, managed:"nix", why}
//!   GET  /hub/install/keys          minerva keys list --json       [{id, fingerprint, state, source, expires}]
//!   POST /hub/install/keys/inspect  minerva keys inspect - --json  {pubkey}            -> {id, fingerprint, comment}
//!   POST /hub/install/keys/add      minerva keys add - --confirm FP --json     {pubkey, confirm}  -> 200 {added} | 409 {why, kind} | 403 {why}
//!   POST /hub/install/keys/remove   minerva keys remove ID --confirm FP --json {id, confirm}      -> 200 {removed} | 409 {why, kind} | 403
//!   POST /hub/install/keys/arm      (nothing runs)                 {fingerprint}      -> 200 {armed, fingerprint, expires_in}   bearer only
//! ```
//!
//! **Which `minerva`.** Only an installed copy of the perch (its canonical exe is `<prefix>/opt/minerva/<ver>/bin/perch`, the shape the
//! installer's `Layout::from_exe` knows) takes the `minerva` beside it, when that is an executable file; every other perch (a development
//! build with a stray `minerva` in its `target/debug`, say) takes `--minerva` and nothing else. Never a `PATH` lookup. Neither:
//! `{installed:false}` and every key route is a 404 with a sentence. A perch exe (or the chosen binary) under `/nix/store` is
//! `{installed:false, managed:"nix"}`, and nothing runs: Nix owns that install. An installed-shaped exe that has been deleted or replaced
//! (`/proc/self/exe` reads `(deleted)`) is `{installed:false, kind:"updated"}`: the perch is running from a version `minerva update` has
//! since removed, and the page is told to start it again. A page's key change always uses the running perch's own `minerva`.
//!
//! **What may run.** One closed enum, [`Verb`], is the only thing [`run`] takes, and its five arms are the five argv above. No route
//! reaches `update`, a rotation, `remove-all` or `jev-setup`, because there is no argv for them to reach. The key goes **on stdin**, never
//! in argv; the only values in argv are a fingerprint and a key id, each a [`Hex16`] (16 hex digits, parsed and re-spelled by this crate,
//! so neither can be an option, a path or anything with a space in it). The child gets a cleared environment with `HOME`,
//! `XDG_CONFIG_HOME` and three `MINERVA_*` names put back ([`PASSED`]), a working directory of `/`, no stderr and 64 KiB of stdout at most.
//! The three reads (`status`, `list`, `inspect`) have a 5 s timeout and are killed if the request is dropped. **A key write (`add`,
//! `remove`) is never killed partway**: `minerva` makes a keys directory in two steps, and a kill between them leaves an install no
//! release verifies against until it is recovered. The write runs in a task of its own that owns the child and the writer lock, waits for
//! the exit under a hard cap of [`WRITE_CAP`], and the request just stops waiting at 5 s (a 504 that says to list the keys) or when its
//! client goes. At most [`READERS`] reads at once (a fifth is a 503) and **one key write at a time** (a second is a 429), the lock held
//! until the child has exited.
//!
//! **The step-up (D4, b2).** `add` and `remove` run only when a person has, in a terminal, run `hoot keys allow-web <FP>`, which
//! `POST`s `/hub/install/keys/arm {fingerprint}` with the perch's bearer token. An arm is one fingerprint, lives 120 s, and is **consumed
//! by the first add or remove of that fingerprint that reaches `minerva` to write**, whatever `minerva` answers. A request the perch itself
//! refuses first leaves the arm alone: no arm, a key it will not remove, a text that is not a key, a fingerprint that is not the pasted
//! key's, a key that is already in the list (`add` runs `keys inspect -` and `keys list` before it spends anything, so a junk add can
//! never burn an arm, and a key already trusted, re-added under a fresh id, is a 409 `listed`). The request's `confirm` (the fingerprint
//! the person typed back, in any case and with spaces) must be the armed one, and `--confirm <that>` is what `minerva` checks against the
//! key's own fingerprint, so an arm for one key never lets another in. The arm route takes the **bearer only**: the cookie is a 403, and so
//! is any request a browser made (a `Sec-Fetch-*` or `Origin` header, which a page cannot omit), because a page that was opened with
//! `#token=` holds the bearer. Arms live in memory, at most [`MAX_ARMS`], and a restart forgets them.
//!
//! **What the arm does not defend** is written in the README: a script in the page's origin that rewrites the fingerprint the page shows
//! makes the person confirm the attacker's key (they are told to compare out of band); anything that runs as your uid can run
//! `minerva keys add` itself or read the bearer token; and once the web terminals land (T1b), whoever holds the page's cookie can open a
//! shell and arm from it, and an agent's `term_send` can type `hoot keys allow-web` (the guard's term-text rows are a hint, not a gate).
//!
//! **What comes back from `minerva` is parsed, never relayed.** Each answer is read as the typed shape the contract names, strings are
//! cleaned (control, format and invisible characters dropped, a cut one ends in `…`) and the page is sent a re-serialization. `add` and
//! `remove` count as done only when the exit code and the answer agree (exit 0 with `{added}`, exit 1 with `{error}`); anything else after
//! the arm is spent is a 502 or 504 of kind `unknown` that says to list the keys, because the change may have been made.
//! `remove` refuses, before it spends the arm, every key whose `source` is not `added by hand` (ruling 6).
//!
//! **Nothing here is secret but the token**, which is read from the `Authorization` header of the arm request only, compared in constant
//! time, and never written anywhere. Public keys, fingerprints and ids are not secret. They are not logged either, with two `info` lines
//! as the exception: the fingerprint of a key a terminal has allowed (so the log says which key a person armed) and the id of a key that was
//! added or removed. No key text, comment or status code is logged.
//!
//! Test knobs (`--knob`, hidden), all of which can only **restrict**: `install_arm_ms`, `install_timeout_ms` and `install_write_cap_ms` only
//! ever shorten the 120 s arm, the 5 s read timeout and the 60 s write cap; `install_managed=nix` makes the perch answer as a Nix install
//! (nothing runs).

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};

use hyper::body::Incoming;
use hyper::header::AUTHORIZATION;
use hyper::{Request, Response, StatusCode};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, OwnedMutexGuard, Semaphore};

use crate::gate;
use crate::serve::{body_of, json, status, BoxBody, Ctx};

/// How long an arm lives, and how long `minerva` may take to answer a read.
const ARM_TTL: Duration = Duration::from_secs(120);
const RUN_TIMEOUT: Duration = Duration::from_secs(5);
/// How long a key write (`add`, `remove`) may run before it is killed: the request stops waiting at [`RUN_TIMEOUT`], the child does not.
/// Past this a write that has not exited is stuck, and a stuck child holding the one writer lock is the worse of the two.
const WRITE_CAP: Duration = Duration::from_secs(60);
/// What `minerva` may print before the perch stops reading and kills it: a few hundred keys' rows fit.
const OUT_CAP: usize = 64 * 1024;
/// The public key a page may send. A minisign `.pub` is about 120 bytes; the rest is room for a comment.
const KEY_MAX: usize = 4096;
/// The request body: a 4 KiB key whose every byte is escaped as `\u00XX` is 24 KiB.
const BODY_CAP: usize = 32 * 1024;
/// The arm's body is `{"fingerprint":"..."}`.
const ARM_BODY_CAP: usize = 1024;
/// How many fingerprints may be armed at once: a person arms one, at most two or three.
const MAX_ARMS: usize = 16;
/// `status`, `list` and `inspect` at once. A fifth is a 503 and the page asks again.
const READERS: usize = 4;
/// The longest sentence of a refusal from `minerva` a page is shown whole (a cut one ends in `…`): a few lines of what to do.
const SENTENCE_MAX: usize = 600;
/// The rows of the key list a page is sent, and the plugins of the status.
const MAX_KEYS: usize = 256;
const MAX_PLUGINS: usize = 64;

/// What a key's `source` says when a person added it from a terminal or from this page: the only keys the page may remove (ruling 6).
const BY_HAND: &str = "added by hand";
const COMPILED: &str = "compiled in";
const ROTATION: &str = "added by a signed rotation";

/// The kinds `minerva keys add|remove --json` names (installer/README.md). This perch adds three of its own: `rotation` (a remove it will
/// not do), `listed` (an add of a key that is already in the list) and `unknown` (a write that was started and gave no answer to trust).
const KINDS: [&str; 7] = ["mismatch", "revoked", "collision", "compiled", "unsafe", "not-found", "failed"];

/// What a change that started and gave no answer to trust tells the person to do: the change may have been made.
macro_rules! unsure {
    ($what:literal) => {
        concat!($what, "; list the keys to see whether the change was made")
    };
}

/// The environment variables `minerva` is given, by name, and no others: where it lives, where its config is, where its state is, whether it
/// may look for updates, and (only a test double reads it) the stand-in's fixture directory.
const PASSED: [&str; 5] = ["HOME", "XDG_CONFIG_HOME", "MINERVA_HOME", "MINERVA_NO_UPDATE_CHECK", "MINERVA_FAKE_DIR"];

const NOT_HERE: &str = "no installed Minerva here (development server)";
const NIX: &str = "managed by Nix: update through nix";
const UPDATED: &str = "the server was updated; start it again";
const BUSY_READ: &str = "minerva is being asked already; try again in a moment";
const BUSY_WRITE: &str = "another key change is running; try again in a moment";
const TYPE_BACK: &str = "type the key's fingerprint back: 16 hex digits";
const BAD_BODY: &str = "that is not the request this route takes";

/// The armed keys and the two limits. `Default` is how [`Ctx`] gets one, before any hook runs.
pub struct State {
    arms: StdMutex<Arms>,
    /// Held across a whole key change (the reads before it included): one key write at a time. Owned, because the guard goes into the task
    /// that waits for the write's child, and stays there when the request that started it is gone.
    writer: Arc<Mutex<()>>,
    readers: Semaphore,
}

impl Default for State {
    fn default() -> Self {
        Self { arms: StdMutex::default(), writer: Arc::new(Mutex::new(())), readers: Semaphore::new(READERS) }
    }
}

/// Sixteen hex digits, upper case, nothing else: a fingerprint or a key id. The only way a value from a request gets into an argv.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Hex16(String);

impl Hex16 {
    /// `s` with its ASCII white space dropped, when that is exactly 16 hex digits (how a person types `0f7a 3a5c 447e c7ef`).
    fn parse(s: &str) -> Option<Hex16> {
        if s.len() > 64 {
            return None;
        }
        let bare: String = s.chars().filter(|c| !c.is_ascii_whitespace()).collect();
        (bare.len() == 16 && bare.bytes().all(|b| b.is_ascii_hexdigit())).then(|| Hex16(bare.to_ascii_uppercase()))
    }

    fn as_str(&self) -> &str {
        &self.0
    }

    /// `0F7A 3A5C 447E C7EF`, the way the page and `hoot` show a fingerprint.
    fn grouped(&self) -> String {
        self.0.as_bytes().chunks(4).map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>().join(" ")
    }
}

/// The fingerprints a person has allowed, each until a deadline. Every method is told the time, so the 120 s edge is a unit test.
#[derive(Default)]
struct Arms(HashMap<Hex16, Instant>);

impl Arms {
    fn purge(&mut self, now: Instant) {
        self.0.retain(|_, until| now < *until);
    }

    /// One arm per fingerprint: arming again moves the deadline. `false` when [`MAX_ARMS`] other fingerprints are armed.
    fn arm(&mut self, fp: Hex16, now: Instant, ttl: Duration) -> bool {
        self.purge(now);
        if !self.0.contains_key(&fp) && self.0.len() >= MAX_ARMS {
            return false;
        }
        self.0.insert(fp, now + ttl);
        true
    }

    /// Whether `fp` is armed, without spending it.
    fn live(&mut self, fp: &Hex16, now: Instant) -> bool {
        self.purge(now);
        self.0.contains_key(fp)
    }

    /// Spends the arm: `true` for exactly one caller.
    fn take(&mut self, fp: &Hex16, now: Instant) -> bool {
        self.purge(now);
        self.0.remove(fp).is_some()
    }
}

fn arms(ctx: &Ctx) -> std::sync::MutexGuard<'_, Arms> {
    ctx.install.arms.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A knob's milliseconds, never longer than `default`: a word that is not a number is no knob at all.
fn shorter(knob: Option<&str>, default: Duration) -> Duration {
    knob.and_then(|v| v.parse::<u64>().ok()).map_or(default, |ms| Duration::from_millis(ms.max(1)).min(default))
}

fn shortened(ctx: &Ctx, key: &str, default: Duration) -> Duration {
    shorter(ctx.opts.knobs.get(key), default)
}

// ---------------------------------------------------------------------------
// Which minerva
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
enum Where {
    Found(PathBuf),
    /// Nix owns this install.
    Nix,
    /// This perch's own files were removed or replaced by an update since it started.
    Updated,
    Missing,
}

fn executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// Nix owns a path that is, or resolves to, somewhere under `/nix/store`.
fn under_nix(path: &Path) -> bool {
    path.starts_with("/nix/store") || path.canonicalize().is_ok_and(|real| real.starts_with("/nix/store"))
}

/// What `/proc/self/exe` says of a program whose file has been removed.
const GONE: &str = " (deleted)";

/// `<prefix>/opt/minerva/<version>/bin/perch`: the shape of an installed copy, the one the installer's `Layout::from_exe` knows (it is
/// given a `minerva` there; this is told the perch's own name). A development build is nowhere near this shape.
fn installed_shape(path: &Path) -> bool {
    let dirs: Vec<&OsStr> = path.ancestors().skip(1).take(4).filter_map(Path::file_name).collect();
    path.file_name() == Some(OsStr::new("perch"))
        && dirs.len() == 4
        && dirs[0] == OsStr::new("bin")
        && dirs[2] == OsStr::new("minerva")
        && dirs[3] == OsStr::new("opt")
}

/// Which `minerva`: the one beside the perch **when the perch is an installed copy** (its canonical exe has [`installed_shape`]) and that is
/// an executable file, else `flag` (`--minerva`) if that is, else nothing. Never `PATH`, and never a `minerva` that merely sits next to a
/// development build. An installed-shaped exe that no longer exists is [`Where::Updated`].
fn resolve(exe: Option<&Path>, flag: Option<&Path>) -> Where {
    if exe.is_some_and(under_nix) {
        return Where::Nix;
    }
    let mut beside = None;
    if let Some(exe) = exe {
        let (named, gone) = match exe.to_str().and_then(|text| text.strip_suffix(GONE)) {
            Some(text) => (Path::new(text), true),
            None => (exe, false),
        };
        match named.canonicalize() {
            Ok(real) if !gone => {
                if installed_shape(&real) {
                    beside = real.parent().map(|dir| dir.join("minerva")).filter(|path| executable_file(path));
                }
            }
            _ if installed_shape(named) => return Where::Updated,
            _ => {}
        }
    }
    match beside.or_else(|| flag.filter(|path| executable_file(path)).map(Path::to_path_buf)) {
        Some(path) if under_nix(&path) => Where::Nix,
        Some(path) => Where::Found(path),
        None => Where::Missing,
    }
}

async fn locate(ctx: &Ctx) -> Where {
    // A hidden knob that can only take away: the perch answers as a Nix install does (and nothing runs), which a test cannot arrange by
    // putting a file in `/nix/store`.
    if ctx.opts.knobs.get("install_managed") == Some("nix") {
        return Where::Nix;
    }
    let flag = ctx.opts.minerva.clone();
    tokio::task::spawn_blocking(move || resolve(std::env::current_exe().ok().as_deref(), flag.as_deref())).await.unwrap_or(Where::Missing)
}

// ---------------------------------------------------------------------------
// Running it
// ---------------------------------------------------------------------------

/// The five things the perch ever asks `minerva`, and the only argv it ever runs.
enum Verb<'a> {
    Status,
    List,
    Inspect,
    Add(&'a Hex16),
    Remove(&'a Hex16, &'a Hex16),
}

impl Verb<'_> {
    fn argv(&self) -> Vec<&str> {
        match self {
            Verb::Status => vec!["status", "--json"],
            Verb::List => vec!["keys", "list", "--json"],
            Verb::Inspect => vec!["keys", "inspect", "-", "--json"],
            Verb::Add(fp) => vec!["keys", "add", "-", "--confirm", fp.as_str(), "--json"],
            Verb::Remove(id, fp) => vec!["keys", "remove", id.as_str(), "--confirm", fp.as_str(), "--json"],
        }
    }

    /// Whether this changes the keys directory. A write is never killed partway ([`run_write`]); a read is ([`run`]).
    fn writes(&self) -> bool {
        matches!(self, Verb::Add(_) | Verb::Remove(..))
    }
}

/// How a run ended.
enum Ran {
    /// It exited, with this code (`None`: a signal), having printed this (at most [`OUT_CAP`] bytes).
    Out(Vec<u8>, Option<i32>),
    Timeout,
    TooBig,
    Spawn(String),
}

/// The environment `minerva` is run with: exactly the names in [`PASSED`]. Nothing else of the perch's (a provider key in the shell that
/// started it, the runtime dir, a proxy, another of minerva's own `MINERVA_*` settings) reaches it.
fn passes(name: &OsStr) -> bool {
    PASSED.iter().any(|passed| name == OsStr::new(passed))
}

/// Starts `minerva` with the verb's argv and nothing of the perch's but [`PASSED`]. `kill` is `kill_on_drop`: a read has it, a write does not.
fn spawn(minerva: &Path, verb: &Verb<'_>, key_on_stdin: bool, kill: bool) -> std::io::Result<Child> {
    let mut command = Command::new(minerva);
    command.args(verb.argv()).env_clear().current_dir("/").stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(kill);
    command.stdin(if key_on_stdin { Stdio::piped() } else { Stdio::null() });
    for (name, value) in std::env::vars_os().filter(|(name, _)| passes(name)) {
        command.env(name, value);
    }
    command.spawn()
}

/// Feeds the key (if there is one), reads at most [`OUT_CAP`] bytes of stdout and waits for the exit. Past the cap a read gives up at once
/// (the caller's drop kills the child); with `drain` the rest is read and thrown away and the child is waited for, because a write must not
/// be left blocked on a full pipe nor killed.
async fn finish(child: &mut Child, key: Option<&[u8]>, drain: bool) -> Ran {
    let (Some(mut out), mut pipe) = (child.stdout.take(), child.stdin.take()) else { return Ran::Spawn("no stdout".into()) };
    // The key goes in and the pipe is closed, so a child that reads to the end sees it. A child that never reads is not this write's
    // failure: the pipe is a few KiB of room and the answer is what is judged.
    let feed = async {
        if let (Some(pipe), Some(bytes)) = (pipe.as_mut(), key) {
            let _ = pipe.write_all(bytes).await;
        }
        drop(pipe.take());
    };
    let mut got = Vec::new();
    let read = async {
        // One byte past the cap is how "over" is told from "exactly".
        let mut capped = (&mut out).take(OUT_CAP as u64 + 1);
        let read = capped.read_to_end(&mut got).await;
        if drain && read.is_ok() && got.len() > OUT_CAP {
            let _ = tokio::io::copy(&mut out, &mut tokio::io::sink()).await;
        }
        read
    };
    let ((), read) = tokio::join!(feed, read);
    if read.is_err() {
        return Ran::Spawn("reading minerva failed".into());
    }
    let over = got.len() > OUT_CAP;
    if over && !drain {
        return Ran::TooBig;
    }
    match child.wait().await {
        Ok(_) if over => Ran::TooBig,
        Ok(exit) => Ran::Out(got, exit.code()),
        Err(e) => Ran::Spawn(e.to_string()),
    }
}

/// A read (`status`, `keys list`, `keys inspect -`): answered within `within`, and the child is killed when it is not, when it prints too
/// much, or when the request is dropped.
async fn run(minerva: &Path, verb: Verb<'_>, stdin: Option<&[u8]>, within: Duration) -> Ran {
    debug_assert!(!verb.writes(), "a key write is run by run_write, which does not kill it");
    let mut child = match spawn(minerva, &verb, stdin.is_some(), true) {
        Ok(child) => child,
        Err(e) => return Ran::Spawn(e.to_string()),
    };
    // A timeout drops the work, and `kill_on_drop` ends the child.
    tokio::time::timeout(within, finish(&mut child, stdin, false)).await.unwrap_or(Ran::Timeout)
}

/// A key write (`keys add -`, `keys remove`). **The child is never killed because the request ran long or went away**: `minerva` makes the
/// keys directory and then its state file, and a kill between the two leaves an install that refuses every release until it is recovered.
/// So the child, and the owned writer lock, move into a task that waits for the exit under `cap` (past it the child is stuck and is
/// killed). The request waits `within` for the task and answers [`Ran::Timeout`] when that passes; the task carries on, and so does the
/// lock, so no second write starts under a first that is still running.
async fn run_write(minerva: &Path, verb: Verb<'_>, key: Option<&[u8]>, only: OwnedMutexGuard<()>, within: Duration, cap: Duration) -> Ran {
    debug_assert!(verb.writes(), "run_write is for the two verbs that change the keys");
    let mut child = match spawn(minerva, &verb, key.is_some(), false) {
        Ok(child) => child,
        Err(e) => return Ran::Spawn(e.to_string()),
    };
    let key = key.map(<[u8]>::to_vec);
    let task = tokio::spawn(async move {
        let _only = only;
        match tokio::time::timeout(cap, finish(&mut child, key.as_deref(), true)).await {
            Ok(ran) => ran,
            Err(_) => {
                tracing::warn!("perch: a key change did not finish in time and its minerva was killed");
                let _ = child.kill().await;
                Ran::Timeout
            }
        }
    });
    match tokio::time::timeout(within, task).await {
        Ok(Ok(ran)) => ran,
        Ok(Err(_)) => Ran::Spawn("the key change task failed".into()),
        Err(_) => Ran::Timeout,
    }
}

// ---------------------------------------------------------------------------
// Answers
// ---------------------------------------------------------------------------

fn why(code: StatusCode, sentence: &str) -> Response<BoxBody> {
    json(code, &json!({ "why": sentence }))
}

fn why_kind(code: StatusCode, sentence: &str, kind: &str) -> Response<BoxBody> {
    json(code, &json!({ "why": sentence, "kind": kind }))
}

/// Whether a character is one that lets shown text lie: a control character (Cc), a format character (every one of general category Cf:
/// the bidirectional controls, zero-width characters, the Arabic and Syriac marks, the tag characters), a line or paragraph separator, or a
/// Default_Ignorable_Code_Point that draws nothing (the Hangul and Khmer fillers, the combining grapheme joiner, the Mongolian and the
/// variation selectors VS1 to VS14 and VS17 to VS256). **VS15 and VS16** (`U+FE0E`, `U+FE0F`, text and emoji presentation) are kept: they
/// change how a character is drawn and cannot hide one.
fn hides(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{00AD}' | '\u{034F}' | '\u{0600}'..='\u{0605}' | '\u{061C}' | '\u{06DD}' | '\u{070F}' | '\u{0890}'..='\u{0891}' | '\u{08E2}'
            | '\u{115F}'..='\u{1160}' | '\u{17B4}'..='\u{17B5}' | '\u{180B}'..='\u{180F}' | '\u{200B}'..='\u{200F}' | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}' | '\u{3164}' | '\u{FE00}'..='\u{FE0D}' | '\u{FEFF}' | '\u{FFA0}' | '\u{FFF0}'..='\u{FFFB}' | '\u{110BD}'
            | '\u{110CD}' | '\u{13430}'..='\u{1343F}' | '\u{1BCA0}'..='\u{1BCA3}' | '\u{1D173}'..='\u{1D17A}' | '\u{E0000}'..='\u{E0FFF}')
}

/// A sentence or a name from `minerva` or a key file, as text a page may show: what [`hides`] dropped, trimmed, and cut to `max`
/// characters. A cut text ends in `…` (inside the `max`), so a sentence that was shortened does not read as a whole one.
fn clean(text: &str, max: usize) -> String {
    let shown: String = text.chars().filter(|c| !hides(*c)).collect();
    let shown = shown.trim();
    if shown.chars().count() <= max {
        return shown.to_string();
    }
    let cut: String = shown.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

/// The page's view of a run that gave no answer to read.
fn trouble(ran: Ran) -> Response<BoxBody> {
    match ran {
        Ran::Timeout => why(StatusCode::GATEWAY_TIMEOUT, "minerva did not answer in time"),
        Ran::TooBig => why(StatusCode::BAD_GATEWAY, "minerva's answer is over the size limit"),
        Ran::Spawn(cause) => {
            tracing::warn!(%cause, "perch: minerva could not be run");
            why(StatusCode::BAD_GATEWAY, "minerva could not be run")
        }
        Ran::Out(_, None) => why(StatusCode::BAD_GATEWAY, "minerva was stopped before it answered"),
        Ran::Out(_, Some(_)) => why(StatusCode::BAD_GATEWAY, "minerva did not give an answer"),
    }
}

fn unreadable() -> Response<BoxBody> {
    why(StatusCode::BAD_GATEWAY, "minerva's answer is not in a shape this perch reads")
}

/// The sentence for a key route when there is no `minerva` to ask.
fn not_here(found: Where) -> Response<BoxBody> {
    match found {
        Where::Nix => why_kind(StatusCode::NOT_FOUND, NIX, "nix"),
        Where::Updated => why_kind(StatusCode::NOT_FOUND, UPDATED, "updated"),
        _ => why(StatusCode::NOT_FOUND, NOT_HERE),
    }
}

// ---------------------------------------------------------------------------
// The routes
// ---------------------------------------------------------------------------

/// `/hub/install` and everything under it. `rest` is `""` for the bare path, `keys`, `keys/add`, `keys/arm`, and so on. Anything that is not
/// one of the six routes, and a known path with the other method, is the empty 404 (so `update`, `jev` and the like are nobody's); a query
/// on any of them is a 400.
pub async fn route(ctx: &Ctx, req: Request<Incoming>, rest: &str) -> Response<BoxBody> {
    let known = matches!((req.method().as_str(), rest), ("GET", "" | "keys") | ("POST", "keys/inspect" | "keys/add" | "keys/remove" | "keys/arm"));
    if !known {
        return status(StatusCode::NOT_FOUND);
    }
    if req.uri().query().is_some() {
        return status(StatusCode::BAD_REQUEST);
    }
    match rest {
        "" => notice(ctx).await,
        "keys" => keys(ctx).await,
        "keys/arm" => arm(ctx, req).await,
        _ => change(ctx, req, rest).await,
    }
}

/// `GET /hub/install`: what `minerva status --json` says, from its own files; no network and no update.
async fn notice(ctx: &Ctx) -> Response<BoxBody> {
    let minerva = match locate(ctx).await {
        Where::Found(path) => path,
        Where::Nix => return json(StatusCode::OK, &json!({ "installed": false, "managed": "nix", "why": NIX })),
        Where::Updated => return json(StatusCode::OK, &json!({ "installed": false, "kind": "updated", "why": UPDATED })),
        Where::Missing => return json(StatusCode::OK, &json!({ "installed": false, "why": NOT_HERE })),
    };
    let Ok(_slot) = ctx.install.readers.try_acquire() else { return why(StatusCode::SERVICE_UNAVAILABLE, BUSY_READ) };
    match run(&minerva, Verb::Status, None, shortened(ctx, "install_timeout_ms", RUN_TIMEOUT)).await {
        Ran::Out(bytes, Some(0)) => reshape_status(&bytes).map_or_else(unreadable, |answer| json(StatusCode::OK, &answer)),
        other => trouble(other),
    }
}

#[derive(Deserialize)]
struct StatusIn {
    version: String,
    notice: Option<NoticeIn>,
    plugins: Vec<PluginIn>,
    jev: JevIn,
}

#[derive(Deserialize)]
struct NoticeIn {
    line: Option<String>,
    latest: Option<String>,
    stale: Option<String>,
    checked: u64,
}

#[derive(Deserialize)]
struct PluginIn {
    name: String,
    version: String,
    how: String,
}

#[derive(Deserialize)]
struct JevIn {
    set_up: bool,
    running: bool,
    #[serde(default)]
    pid: Option<u32>,
}

/// The status as the page is sent it: the typed fields of the contract, cleaned and cut; whatever else was printed is dropped.
fn reshape_status(bytes: &[u8]) -> Option<Value> {
    let seen: StatusIn = serde_json::from_slice(bytes).ok()?;
    let text = |s: Option<String>, max| s.map(|s| clean(&s, max)).filter(|s| !s.is_empty());
    let notice = seen.notice.map(|n| json!({ "line": text(n.line, 300), "latest": text(n.latest, 64), "stale": text(n.stale, 300), "checked": n.checked }));
    let plugins: Vec<Value> = seen
        .plugins
        .into_iter()
        .take(MAX_PLUGINS)
        .map(|p| json!({ "name": clean(&p.name, 64), "version": clean(&p.version, 64), "how": clean(&p.how, 32) }))
        .collect();
    let mut jev = json!({ "set_up": seen.jev.set_up, "running": seen.jev.running });
    if let (true, Some(pid)) = (seen.jev.running, seen.jev.pid) {
        jev["pid"] = json!(pid);
    }
    Some(json!({ "installed": true, "version": clean(&seen.version, 64), "notice": notice, "plugins": plugins, "jev": jev }))
}

/// One row of `keys list --json`, checked.
struct Row {
    id: Hex16,
    fingerprint: Hex16,
    state: &'static str,
    /// One of the three labels `minerva` prints, spelled exactly, or `other`: a label this perch does not know is never taken for one it does.
    source: &'static str,
    expires: Option<String>,
}

impl Row {
    fn page(&self) -> Value {
        json!({ "id": self.id.as_str(), "fingerprint": self.fingerprint.grouped(), "state": self.state, "source": self.source, "expires": self.expires })
    }
}

/// The list, strictly: a row that is not what the contract says makes the whole list unreadable, because a list that quietly leaves a key
/// out is worse than none.
fn reshape_keys(bytes: &[u8]) -> Option<Vec<Row>> {
    let seen: Vec<Value> = serde_json::from_slice(bytes).ok()?;
    if seen.len() > MAX_KEYS {
        return None;
    }
    let date = |s: &str| s.len() == 10 && s.bytes().enumerate().all(|(at, b)| if matches!(at, 4 | 7) { b == b'-' } else { b.is_ascii_digit() });
    seen.iter()
        .map(|row| {
            let field = |name: &str| row.get(name).and_then(Value::as_str);
            let state = match field("state")? {
                "trusted" => "trusted",
                "revoked" => "revoked",
                "expired" => "expired",
                _ => return None,
            };
            let expires = match row.get("expires")? {
                Value::Null => None,
                Value::String(s) if date(s) => Some(s.clone()),
                _ => return None,
            };
            let source = match field("source")? {
                BY_HAND => BY_HAND,
                COMPILED => COMPILED,
                ROTATION => ROTATION,
                _ => "other",
            };
            Some(Row { id: Hex16::parse(field("id")?)?, fingerprint: Hex16::parse(field("fingerprint")?)?, state, source, expires })
        })
        .collect()
}

/// The list as rows, or the answer to send instead.
async fn listing(ctx: &Ctx, minerva: &Path) -> Result<Vec<Row>, Box<Response<BoxBody>>> {
    match run(minerva, Verb::List, None, shortened(ctx, "install_timeout_ms", RUN_TIMEOUT)).await {
        Ran::Out(bytes, Some(0)) => reshape_keys(&bytes).ok_or_else(|| Box::new(unreadable())),
        other => Err(Box::new(trouble(other))),
    }
}

/// `GET /hub/install/keys`.
async fn keys(ctx: &Ctx) -> Response<BoxBody> {
    let minerva = match locate(ctx).await {
        Where::Found(path) => path,
        other => return not_here(other),
    };
    let Ok(_slot) = ctx.install.readers.try_acquire() else { return why(StatusCode::SERVICE_UNAVAILABLE, BUSY_READ) };
    match listing(ctx, &minerva).await {
        Ok(rows) => json(StatusCode::OK, &rows.iter().map(Row::page).collect::<Vec<_>>()),
        Err(refusal) => *refusal,
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InspectBody {
    pubkey: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddBody {
    pubkey: String,
    confirm: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoveBody {
    id: String,
    confirm: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ArmBody {
    fingerprint: String,
}

fn parse<T: DeserializeOwned>(body: &[u8]) -> Result<T, Box<Response<BoxBody>>> {
    serde_json::from_slice(body).map_err(|_| Box::new(why(StatusCode::BAD_REQUEST, BAD_BODY)))
}

/// A public key a page may hand over: not empty, not past [`KEY_MAX`], and no NUL.
fn pubkey_ok(text: &str) -> Result<(), Box<Response<BoxBody>>> {
    if text.trim().is_empty() || text.contains('\0') {
        return Err(Box::new(why(StatusCode::BAD_REQUEST, "paste the public key (the .pub file's text)")));
    }
    if text.len() > KEY_MAX {
        return Err(Box::new(why(StatusCode::BAD_REQUEST, "that is too long for a public key (4 KiB at most)")));
    }
    Ok(())
}

/// `inspect`, `add` and `remove`: the three POST routes that run `minerva`, after the one check they share.
async fn change(ctx: &Ctx, req: Request<Incoming>, which: &str) -> Response<BoxBody> {
    let minerva = match locate(ctx).await {
        Where::Found(path) => path,
        other => return not_here(other),
    };
    let body = match body_of(req, BODY_CAP).await {
        Ok((_, body)) => body,
        Err(refusal) => return *refusal,
    };
    match which {
        "keys/inspect" => inspect(ctx, &minerva, &body).await,
        "keys/add" => add(ctx, &minerva, &body).await,
        _ => remove(ctx, &minerva, &body).await,
    }
}

/// What `minerva keys inspect -` says of a text: the key's id, its fingerprint and its comment (cleaned and cut), or the answer to send
/// instead (a 422 for a text that is not a public key). The comment is whoever wrote the file's words.
async fn inspected(ctx: &Ctx, minerva: &Path, pubkey: &str) -> Result<(Hex16, Hex16, String), Box<Response<BoxBody>>> {
    match run(minerva, Verb::Inspect, Some(pubkey.as_bytes()), shortened(ctx, "install_timeout_ms", RUN_TIMEOUT)).await {
        Ran::Out(bytes, Some(0)) => {
            let seen: Option<Value> = serde_json::from_slice(&bytes).ok();
            let field = |name: &str| seen.as_ref().and_then(|v| v.get(name)).and_then(Value::as_str);
            match (field("id").and_then(Hex16::parse), field("fingerprint").and_then(Hex16::parse), field("comment")) {
                (Some(id), Some(fp), Some(comment)) => Ok((id, fp, clean(comment, 200))),
                _ => Err(Box::new(unreadable())),
            }
        }
        // A non-zero exit with nothing to read is how `minerva` says that text is not a public key.
        Ran::Out(_, Some(_)) => Err(Box::new(why(StatusCode::UNPROCESSABLE_ENTITY, "that is not a public key minerva can read"))),
        other => Err(Box::new(trouble(other))),
    }
}

/// `POST /hub/install/keys/inspect {pubkey}`: the key's id, fingerprint and comment.
async fn inspect(ctx: &Ctx, minerva: &Path, body: &[u8]) -> Response<BoxBody> {
    let asked: InspectBody = match parse(body) {
        Ok(asked) => asked,
        Err(refusal) => return *refusal,
    };
    if let Err(refusal) = pubkey_ok(&asked.pubkey) {
        return *refusal;
    }
    let Ok(_slot) = ctx.install.readers.try_acquire() else { return why(StatusCode::SERVICE_UNAVAILABLE, BUSY_READ) };
    match inspected(ctx, minerva, &asked.pubkey).await {
        Ok((id, fp, comment)) => json(StatusCode::OK, &json!({ "id": id.as_str(), "fingerprint": fp.grouped(), "comment": comment })),
        Err(refusal) => *refusal,
    }
}

/// The 403 of a change nobody allowed: names the command, with the fingerprint the person typed, and the verb.
fn not_armed(fp: &Hex16, verb: &str) -> Response<BoxBody> {
    why(StatusCode::FORBIDDEN, &format!("in a terminal: hoot keys allow-web {}, then {verb} within 2 minutes", fp.grouped()))
}

fn armed(ctx: &Ctx, fp: &Hex16) -> bool {
    arms(ctx).live(fp, Instant::now())
}

/// Spends the arm. `false` when it ran out, or was spent, since [`armed`] looked.
fn spend(ctx: &Ctx, fp: &Hex16) -> bool {
    arms(ctx).take(fp, Instant::now())
}

/// How a key change ended. `Broken` is only ever made after the arm is spent, so the change may have been made: its sentence says to list
/// the keys, and its kind is `unknown`.
enum Verdict {
    Done(Hex16),
    Refused { kind: &'static str, sentence: String },
    Broken(StatusCode, &'static str),
}

/// What `minerva keys add|remove --json` printed and exited with, as a verdict: success is exit 0 with `{<word>: id}` and no `error`,
/// refusal is a non-zero exit with `{error: {kind, sentence}}` and no `<word>`. Any other combination claims nothing.
fn judge(word: &str, ran: Ran) -> Verdict {
    let (bytes, code) = match ran {
        Ran::Timeout => return Verdict::Broken(StatusCode::GATEWAY_TIMEOUT, unsure!("minerva did not answer in time")),
        Ran::TooBig => return Verdict::Broken(StatusCode::BAD_GATEWAY, unsure!("minerva's answer is over the size limit")),
        Ran::Spawn(cause) => {
            tracing::warn!(%cause, "perch: minerva could not be run");
            return Verdict::Broken(StatusCode::BAD_GATEWAY, unsure!("minerva could not be run"));
        }
        Ran::Out(_, None) => return Verdict::Broken(StatusCode::BAD_GATEWAY, unsure!("minerva was stopped before it answered")),
        Ran::Out(bytes, Some(code)) => (bytes, code),
    };
    let unreadable = Verdict::Broken(StatusCode::BAD_GATEWAY, unsure!("minerva's answer is not one this perch reads"));
    let Ok(Value::Object(answer)) = serde_json::from_slice::<Value>(&bytes) else { return unreadable };
    match (code, answer.get(word), answer.get("error")) {
        (0, Some(Value::String(id)), None) => Hex16::parse(id).map_or(unreadable, Verdict::Done),
        (code, None, Some(Value::Object(error))) if code != 0 => {
            let kind = KINDS.into_iter().find(|kind| error.get("kind").and_then(Value::as_str) == Some(*kind)).unwrap_or("failed");
            let sentence = clean(error.get("sentence").and_then(Value::as_str).unwrap_or(""), SENTENCE_MAX);
            let sentence = if sentence.is_empty() { format!("minerva refused ({kind})") } else { sentence };
            Verdict::Refused { kind, sentence }
        }
        _ => unreadable,
    }
}

fn verdict_answer(word: &str, verdict: Verdict) -> Response<BoxBody> {
    match verdict {
        Verdict::Done(id) => {
            tracing::info!(id = id.as_str(), "perch: an update key was {word}");
            json(StatusCode::OK, &json!({ word: id.as_str() }))
        }
        Verdict::Refused { kind, sentence } => why_kind(StatusCode::CONFLICT, &sentence, kind),
        Verdict::Broken(code, sentence) => why_kind(code, sentence, "unknown"),
    }
}

/// `POST /hub/install/keys/add {pubkey, confirm}`: `confirm` is the fingerprint the person typed back. Runs only under an arm for exactly
/// that fingerprint. Before the arm is spent, two reads under the writer lock: `keys inspect -` (the text must be a key, and its own
/// fingerprint must be `confirm`) and `keys list` (the key must not be in it already, by id or by fingerprint), so a request that cannot
/// succeed never burns an arm. The arm is spent when the write starts; `minerva` then compares `--confirm` with the key's own fingerprint
/// again, so the arm is for that key.
async fn add(ctx: &Ctx, minerva: &Path, body: &[u8]) -> Response<BoxBody> {
    let asked: AddBody = match parse(body) {
        Ok(asked) => asked,
        Err(refusal) => return *refusal,
    };
    if let Err(refusal) = pubkey_ok(&asked.pubkey) {
        return *refusal;
    }
    let Some(fp) = Hex16::parse(&asked.confirm) else { return why(StatusCode::BAD_REQUEST, TYPE_BACK) };
    if !armed(ctx, &fp) {
        return not_armed(&fp, "add");
    }
    let Ok(only) = ctx.install.writer.clone().try_lock_owned() else { return why(StatusCode::TOO_MANY_REQUESTS, BUSY_WRITE) };
    let (id, shown) = match inspected(ctx, minerva, &asked.pubkey).await {
        Ok((id, shown, _)) => (id, shown),
        Err(refusal) => return *refusal,
    };
    if shown != fp {
        return why_kind(StatusCode::CONFLICT, "the fingerprint is not that key's; nothing changed", "mismatch");
    }
    let rows = match listing(ctx, minerva).await {
        Ok(rows) => rows,
        Err(refusal) => return *refusal,
    };
    if rows.iter().any(|row| row.id == id || row.fingerprint == fp) {
        return why_kind(StatusCode::CONFLICT, "that key is already in the list", "listed");
    }
    if !spend(ctx, &fp) {
        return not_armed(&fp, "add");
    }
    let within = shortened(ctx, "install_timeout_ms", RUN_TIMEOUT);
    let ran = run_write(minerva, Verb::Add(&fp), Some(asked.pubkey.as_bytes()), only, within, shortened(ctx, "install_write_cap_ms", WRITE_CAP)).await;
    verdict_answer("added", judge("added", ran))
}

/// `POST /hub/install/keys/remove {id, confirm}`: the same step-up, and only a key whose source is `added by hand` (ruling 6). The list is
/// read first, under the same lock, and what the perch refuses from it does not spend the arm.
async fn remove(ctx: &Ctx, minerva: &Path, body: &[u8]) -> Response<BoxBody> {
    let asked: RemoveBody = match parse(body) {
        Ok(asked) => asked,
        Err(refusal) => return *refusal,
    };
    let Some(id) = Hex16::parse(&asked.id) else { return why(StatusCode::BAD_REQUEST, "a key id is 16 hex digits") };
    let Some(fp) = Hex16::parse(&asked.confirm) else { return why(StatusCode::BAD_REQUEST, TYPE_BACK) };
    if !armed(ctx, &fp) {
        return not_armed(&fp, "remove");
    }
    let Ok(only) = ctx.install.writer.clone().try_lock_owned() else { return why(StatusCode::TOO_MANY_REQUESTS, BUSY_WRITE) };
    let rows = match listing(ctx, minerva).await {
        Ok(rows) => rows,
        Err(refusal) => return *refusal,
    };
    let Some(row) = rows.iter().find(|row| row.id == id) else { return why_kind(StatusCode::CONFLICT, "there is no key with that id", "not-found") };
    match row.source {
        BY_HAND => {}
        COMPILED => return why_kind(StatusCode::CONFLICT, "the compiled-in key is the release key and cannot be removed here", "compiled"),
        ROTATION => return why_kind(StatusCode::CONFLICT, "a key that came with a signed rotation is removed by a signed rotation, not from this page", "rotation"),
        _ => return why_kind(StatusCode::CONFLICT, "this page removes only keys that were added by hand", "failed"),
    }
    if row.fingerprint != fp {
        return why_kind(StatusCode::CONFLICT, "the fingerprint is not that key's; nothing changed", "mismatch");
    }
    if !spend(ctx, &fp) {
        return not_armed(&fp, "remove");
    }
    let within = shortened(ctx, "install_timeout_ms", RUN_TIMEOUT);
    let ran = run_write(minerva, Verb::Remove(&id, &fp), None, only, within, shortened(ctx, "install_write_cap_ms", WRITE_CAP)).await;
    // The id `minerva` says it removed must be the one asked for.
    let verdict = match judge("removed", ran) {
        Verdict::Done(done) if done != id => Verdict::Broken(StatusCode::BAD_GATEWAY, unsure!("minerva's answer is not one this perch reads")),
        other => other,
    };
    verdict_answer("removed", verdict)
}

/// `POST /hub/install/keys/arm {fingerprint}`: what `hoot keys allow-web` calls. The perch's bearer token and nothing else, from a program
/// that is not a browser. Nothing runs.
async fn arm(ctx: &Ctx, req: Request<Incoming>) -> Response<BoxBody> {
    let headers = req.headers();
    let bearer = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|presented| gate::token_ok(presented, &ctx.token));
    // A request a browser made says so in headers a page cannot leave out. A page opened with `#token=` holds the bearer; this is what
    // stops its script from using it to arm.
    let browser = ["sec-fetch-site", "sec-fetch-mode", "sec-fetch-dest", "origin"].iter().any(|name| headers.contains_key(*name));
    if !bearer || browser {
        return why(StatusCode::FORBIDDEN, "keys are allowed from a terminal only: hoot keys allow-web <fingerprint>");
    }
    match locate(ctx).await {
        Where::Found(_) => {}
        other => return not_here(other),
    }
    let body = match body_of(req, ARM_BODY_CAP).await {
        Ok((_, body)) => body,
        Err(refusal) => return *refusal,
    };
    let asked: ArmBody = match parse(&body) {
        Ok(asked) => asked,
        Err(refusal) => return *refusal,
    };
    let Some(fp) = Hex16::parse(&asked.fingerprint) else { return why(StatusCode::BAD_REQUEST, "the fingerprint is 16 hex digits") };
    let ttl = shortened(ctx, "install_arm_ms", ARM_TTL);
    if !arms(ctx).arm(fp.clone(), Instant::now(), ttl) {
        return why(StatusCode::TOO_MANY_REQUESTS, "too many keys are allowed at once; use or wait out the others");
    }
    tracing::info!(fingerprint = fp.as_str(), "perch: the web page may change one update key");
    json(StatusCode::OK, &json!({ "armed": true, "fingerprint": fp.grouped(), "expires_in": (ttl.as_millis() as u64).div_ceil(1000) }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, secs: f64) -> Instant {
        base + Duration::from_secs_f64(secs)
    }

    /// What a person types, in any of the ways they type it, is one value; everything else is nothing. This is the only door a value of a
    /// request has into an argv, so what it refuses is as much the test as what it takes.
    #[test]
    fn a_fingerprint_is_sixteen_hex_digits_however_it_was_typed_and_nothing_else() {
        for typed in ["0F7A3A5C447EC7EF", "0f7a3a5c447ec7ef", "0f7a 3a5c 447e c7ef", " 0F7A  3A5C\t447E\nC7EF ", "0F7A 3a5C 447e C7Ef"] {
            let fp = Hex16::parse(typed).unwrap_or_else(|| panic!("{typed:?} is a fingerprint"));
            assert_eq!(fp.as_str(), "0F7A3A5C447EC7EF", "{typed:?}");
            assert_eq!(fp.grouped(), "0F7A 3A5C 447E C7EF");
        }
        let long = "0".repeat(65);
        for typed in [
            "", "0F7A3A5C447EC7E", "0F7A3A5C447EC7EF0", "0F7A3A5C447EC7EG", "0x0F7A3A5C447EC7", "--confirm 0F7A3A5C", "-0F7A3A5C447EC7E",
            "0F7A-3A5C-447E-C7EF", "0F7A:3A5C:447E:C7EF", "0F7A3A5C447EC7E\u{a0}F", "\u{ff10}F7A3A5C447EC7EF", "\u{660}F7A3A5C447EC7EF", "0F7A3A5C447EC7E\0",
            "0F7A3A5C447EC7EF;ls", "0F7A3A5C447EC7EF\u{2028}", &long,
        ] {
            assert!(Hex16::parse(typed).is_none(), "{typed:?} is not a fingerprint");
        }
    }

    /// An arm is one fingerprint, single use, and over at 120 s exactly; time is passed in, so the edge is tested without waiting.
    #[test]
    fn an_arm_is_single_use_for_its_fingerprint_and_over_at_120_seconds() {
        let (x, y) = (Hex16::parse("1111111111111111").unwrap(), Hex16::parse("2222222222222222").unwrap());
        let t = Instant::now();
        let mut arms = Arms::default();
        assert!(!arms.live(&x, t) && !arms.take(&x, t), "nothing is armed to begin with");
        assert!(arms.arm(x.clone(), t, ARM_TTL));
        assert!(arms.live(&x, at(t, 119.9)), "still live just before the end");
        assert!(!arms.live(&y, t), "an arm is for its own fingerprint");
        assert!(!arms.live(&x, at(t, 120.0)), "over at 120 s: the deadline is not inclusive");
        assert!(!arms.take(&x, at(t, 120.0)), "and an arm that has run out cannot be spent");
        // single use, and spending one fingerprint leaves another alone
        assert!(arms.arm(x.clone(), t, ARM_TTL) && arms.arm(y.clone(), t, ARM_TTL));
        assert!(arms.live(&x, t), "looking is not spending");
        assert!(arms.take(&x, at(t, 1.0)));
        assert!(!arms.take(&x, at(t, 1.0)), "the second spend finds nothing");
        assert!(arms.live(&y, at(t, 1.0)));
        // one arm per fingerprint: arming again moves the deadline and does not stack
        assert!(arms.arm(y.clone(), at(t, 100.0), ARM_TTL));
        assert!(arms.live(&y, at(t, 200.0)), "the second arm moved the deadline");
        assert!(arms.take(&y, at(t, 200.0)) && !arms.take(&y, at(t, 200.0)), "and is still one use");
    }

    #[test]
    fn only_so_many_fingerprints_are_armed_at_once_and_the_ones_that_ran_out_are_forgotten() {
        let key = |n: usize| Hex16::parse(&format!("{n:016X}")).unwrap();
        let t = Instant::now();
        let mut arms = Arms::default();
        for n in 0..MAX_ARMS {
            assert!(arms.arm(key(n), t, ARM_TTL), "{n}");
        }
        assert!(!arms.arm(key(MAX_ARMS), t, ARM_TTL), "one more is refused");
        assert!(arms.arm(key(0), t, ARM_TTL), "arming one that is already armed is not one more");
        assert!(arms.arm(key(MAX_ARMS), at(t, 121.0), ARM_TTL), "once the others ran out there is room");
        assert_eq!(arms.0.len(), 1, "and what ran out is gone, not only ignored");
    }

    /// The five argv are the whole of what the perch can run. A route that wanted to run anything else has no `Verb` to say it with.
    #[test]
    fn the_only_argv_are_the_five_the_contract_names() {
        let (id, fp) = (Hex16::parse("0123456789ABCDEF").unwrap(), Hex16::parse("0f7a 3a5c 447e c7ef").unwrap());
        let all = [
            (Verb::Status, vec!["status", "--json"]),
            (Verb::List, vec!["keys", "list", "--json"]),
            (Verb::Inspect, vec!["keys", "inspect", "-", "--json"]),
            (Verb::Add(&fp), vec!["keys", "add", "-", "--confirm", "0F7A3A5C447EC7EF", "--json"]),
            (Verb::Remove(&id, &fp), vec!["keys", "remove", "0123456789ABCDEF", "--confirm", "0F7A3A5C447EC7EF", "--json"]),
        ];
        for (verb, want) in &all {
            assert_eq!(&verb.argv(), want);
            for word in verb.argv() {
                for never in ["update", "rollback", "--rollback", "remove-all", "rotate", "rotation", "jev-setup", "uninstall", "install", "--check"] {
                    assert_ne!(word, never, "{want:?}");
                }
            }
        }
        for (verb, _) in &all[3..] {
            assert!(verb.argv().contains(&"--confirm"), "a key write always carries its confirmation");
        }
    }

    /// The names are a list, not a prefix: another of minerva's own settings (a keys directory, say) is not the perch's to hand on.
    #[test]
    fn the_child_environment_is_five_names_and_nothing_else() {
        for yes in ["HOME", "XDG_CONFIG_HOME", "MINERVA_HOME", "MINERVA_NO_UPDATE_CHECK", "MINERVA_FAKE_DIR"] {
            assert!(passes(OsStr::new(yes)), "{yes}");
        }
        assert_eq!(PASSED.len(), 5);
        for no in [
            "PATH", "XDG_RUNTIME_DIR", "XDG_DATA_HOME", "XDG_STATE_HOME", "HTTPS_PROXY", "ANTHROPIC_API_KEY", "SSH_AUTH_SOCK", "RUST_LOG", "minerva_x",
            "MINERVAX", "MINERVA_", "MINERVA_KEYS_DIR", "MINERVA_UPDATE_URL", "MINERVA_HOME2", "minerva_home", "HOME2", "home",
        ] {
            assert!(!passes(OsStr::new(no)), "{no}");
        }
    }

    #[test]
    fn a_knob_can_only_shorten() {
        let d = Duration::from_secs(120);
        assert_eq!(shorter(None, d), d);
        assert_eq!(shorter(Some("300"), d), Duration::from_millis(300));
        assert_eq!(shorter(Some("0"), d), Duration::from_millis(1), "zero is the shortest, not off");
        for longer in ["120000", "120001", "999999999", "18446744073709551615"] {
            assert!(shorter(Some(longer), d) <= d, "{longer}");
        }
        assert_eq!(shorter(Some("120000"), d), d);
        for bad in ["", "abc", "-5", "1.5", "12 ", "0x10", "99999999999999999999999"] {
            assert_eq!(shorter(Some(bad), d), d, "{bad:?} is no knob");
        }
    }

    fn touch(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::write(path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// `<dir of the perch>/minerva` first when the perch is an installed copy, `--minerva` second, nothing else: no `PATH`, no other name,
    /// no non-executable file, and a perch under Nix is Nix's whatever sits beside it.
    #[test]
    fn minerva_is_the_one_beside_an_installed_perch_else_the_flag_and_never_a_path_lookup() {
        let tmp = tempfile::tempdir().unwrap();
        let (bin, other) = (tmp.path().join("opt/minerva/0.1.0-gabc1234/bin"), tmp.path().join("other"));
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let (perch, beside, flag) = (bin.join("perch"), bin.join("minerva"), other.join("minerva"));
        touch(&perch, 0o755);
        let real = |p: &Path| p.canonicalize().unwrap();

        assert_eq!(resolve(Some(&perch), None), Where::Missing, "nothing beside it and no flag: not installed");
        touch(&flag, 0o755);
        assert_eq!(resolve(Some(&perch), Some(&flag)), Where::Found(flag.clone()), "the flag when nothing is beside the perch");
        touch(&beside, 0o755);
        assert_eq!(resolve(Some(&perch), Some(&flag)), Where::Found(real(&beside)), "beside the perch wins over the flag");
        assert_eq!(resolve(Some(&perch), None), Where::Found(real(&beside)));
        touch(&beside, 0o644);
        assert_eq!(resolve(Some(&perch), Some(&flag)), Where::Found(flag.clone()), "a file that is not executable is not a minerva");
        touch(&flag, 0o644);
        assert_eq!(resolve(Some(&perch), Some(&flag)), Where::Missing);
        std::fs::remove_file(&flag).unwrap();
        assert_eq!(resolve(None, Some(&flag)), Where::Missing, "a flag that names nothing");
        // a directory named minerva is not one, and neither is a link to nothing
        std::fs::remove_file(&beside).unwrap();
        std::fs::create_dir(&beside).unwrap();
        assert_eq!(resolve(Some(&perch), None), Where::Missing);
        std::fs::remove_dir(&beside).unwrap();
        std::os::unix::fs::symlink(tmp.path().join("nowhere"), &beside).unwrap();
        assert_eq!(resolve(Some(&perch), None), Where::Missing);
        // no exe known (current_exe failed): the flag alone
        touch(&flag, 0o755);
        assert_eq!(resolve(None, Some(&flag)), Where::Found(flag.clone()));
        assert_eq!(resolve(None, None), Where::Missing);
    }

    /// A `minerva` that merely sits next to a development build (the installer's own `cargo build` leaves one in `target/debug`) is nobody's:
    /// only the exact installed shape `<prefix>/opt/minerva/<version>/bin/perch` takes the one beside it.
    #[test]
    fn a_minerva_beside_a_perch_that_is_not_an_installed_copy_is_not_asked() {
        let tmp = tempfile::tempdir().unwrap();
        let flag = tmp.path().join("flag/minerva");
        std::fs::create_dir_all(flag.parent().unwrap()).unwrap();
        touch(&flag, 0o755);
        let layouts = [
            "target/debug/perch",
            "opt/minerva/0.1.0/sbin/perch",
            "opt/other/0.1.0/bin/perch",
            "opt/minerva/bin/perch",
            "minerva/0.1.0/bin/perch",
            "opt/minerva/0.1.0/bin/x/perch",
            "opt/minerva/0.1.0/bin/hoot",
            "opt/minerva/0.1.0/bin/perch2",
            "bin/perch",
        ];
        for layout in layouts {
            let perch = tmp.path().join(layout);
            std::fs::create_dir_all(perch.parent().unwrap()).unwrap();
            touch(&perch, 0o755);
            touch(&perch.with_file_name("minerva"), 0o755);
            assert_eq!(resolve(Some(&perch), None), Where::Missing, "{layout}: the stray minerva beside it is not run");
            assert_eq!(resolve(Some(&perch), Some(&flag)), Where::Found(flag.clone()), "{layout}: the flag is the only way in");
        }
        // and the one shape that is installed does take it
        let perch = tmp.path().join("opt/minerva/0.1.0/bin/perch");
        touch(&perch, 0o755);
        touch(&perch.with_file_name("minerva"), 0o755);
        assert_eq!(resolve(Some(&perch), None), Where::Found(perch.with_file_name("minerva").canonicalize().unwrap()));
        assert!(installed_shape(Path::new("/home/me/.local/opt/minerva/0.1.0-gabc1234/bin/perch")));
        assert!(!installed_shape(Path::new("perch")) && !installed_shape(Path::new("/perch")) && !installed_shape(Path::new("/bin/perch")));
    }

    /// A perch whose installed files are gone (`minerva update` removed its version, or replaced it) is told to start again; a development
    /// build that was rebuilt under itself is not an update, and Nix is still Nix's.
    #[test]
    fn an_installed_perch_whose_files_are_gone_is_updated_and_nothing_else_is() {
        let installed = "/nonexistent-r4/.local/opt/minerva/0.1.0/bin/perch";
        assert_eq!(resolve(Some(Path::new(installed)), None), Where::Updated, "canonicalize fails");
        assert_eq!(resolve(Some(Path::new(&format!("{installed} (deleted)"))), None), Where::Updated, "what /proc/self/exe reads");
        let tmp = tempfile::tempdir().unwrap();
        let flag = tmp.path().join("minerva");
        touch(&flag, 0o755);
        assert_eq!(resolve(Some(Path::new(&format!("{installed} (deleted)"))), Some(&flag)), Where::Updated, "a flag does not hide it");
        // a path that is gone but is not an installed copy's
        for dev in ["/nonexistent-r4/proj/target/debug/perch (deleted)", "/nonexistent-r4/proj/target/debug/perch"] {
            assert_eq!(resolve(Some(Path::new(dev)), None), Where::Missing, "{dev}");
            assert_eq!(resolve(Some(Path::new(dev)), Some(&flag)), Where::Found(flag.clone()), "{dev}");
        }
        // an installed copy that is there is not updated, and a path that merely ends in the words is not "deleted"
        let here = tmp.path().join("opt/minerva/1/bin");
        std::fs::create_dir_all(&here).unwrap();
        touch(&here.join("perch"), 0o755);
        assert_eq!(resolve(Some(&here.join("perch")), None), Where::Missing);
        assert_eq!(resolve(Some(Path::new("/nix/store/x/opt/minerva/1/bin/perch (deleted)")), None), Where::Nix);
    }

    #[test]
    fn a_perch_under_the_nix_store_is_nix_managed_and_nothing_beside_it_is_asked() {
        assert_eq!(resolve(Some(Path::new("/nix/store/abc123-minerva-0.2.0/bin/perch")), None), Where::Nix);
        let tmp = tempfile::tempdir().unwrap();
        let flag = tmp.path().join("minerva");
        touch(&flag, 0o755);
        assert_eq!(
            resolve(Some(Path::new("/nix/store/abc123-minerva/bin/perch")), Some(&flag)),
            Where::Nix,
            "a flag does not take a Nix install out of Nix's hands"
        );
        assert!(under_nix(Path::new("/nix/store/x")) && under_nix(Path::new("/nix/store")));
        for not in ["/nix/storefront/bin/perch", "/nix/bin/perch", "/usr/nix/store/perch", "/home/me/.local/bin/perch", "nix/store/perch"] {
            assert!(!under_nix(Path::new(not)), "{not}");
        }
        // a link that leads nowhere is nobody's
        let link = tmp.path().join("perch");
        std::os::unix::fs::symlink("/nix/store/not-there/bin/perch", &link).unwrap();
        assert!(!under_nix(&link));
    }

    /// Text from a key file or from `minerva` is shown to a person: nothing that moves the cursor, hides a character or reorders the line.
    #[test]
    fn clean_drops_what_lets_text_lie_and_keeps_the_rest() {
        assert_eq!(clean("release key 2", 200), "release key 2");
        assert_eq!(clean("  padded\t\r\n ", 200), "padded");
        assert_eq!(clean("a\u{1b}[31mred\u{7}\u{8}\u{7f}", 200), "a[31mred", "C0 and DEL: the escape's own bytes go");
        assert_eq!(clean("a\u{85}b\u{9b}c", 200), "abc", "C1");
        assert_eq!(clean("trusted\u{202e}txt.exe", 200), "trustedtxt.exe", "a right-to-left override");
        assert_eq!(clean("x\u{2066}y\u{2069}\u{200b}z\u{200f}\u{feff}\u{ad}\u{2028}\u{2029}\u{61c}!", 200), "xyz!");
        assert_eq!(clean("é 日本語 🔑 ✓", 200), "é 日本語 🔑 ✓", "ordinary text, any script, stays");
        let cut = clean(&"é".repeat(500), 200);
        assert_eq!((cut.chars().count(), cut.ends_with('…')), (200, true), "cut by characters, not bytes, and a cut says so");
        assert_eq!(clean(&"é".repeat(200), 200), "é".repeat(200), "exactly the most is not a cut");
        assert_eq!(clean(&format!("{}\u{200b}", "é".repeat(200)), 200), "é".repeat(200), "what is dropped does not count towards the cut");
        assert_eq!(clean("word   word word", 8), "word…", "the cut text is trimmed before the mark");
        assert_eq!(clean("", 10), "");
    }

    /// Every format character (general category Cf) and every invisible Default_Ignorable_Code_Point: the tag characters that can carry a
    /// whole message nobody sees, the old bidi isolates, the fillers that draw nothing, the variation selectors. VS15 and VS16 stay.
    #[test]
    fn clean_drops_every_format_and_invisible_character_but_keeps_the_two_presentation_selectors() {
        let gone: &[(&str, char, char)] = &[
            ("soft hyphen", '\u{ad}', '\u{ad}'),
            ("combining grapheme joiner", '\u{34f}', '\u{34f}'),
            ("arabic number signs and the end of ayah", '\u{600}', '\u{605}'),
            ("arabic letter mark", '\u{61c}', '\u{61c}'),
            ("arabic end of ayah", '\u{6dd}', '\u{6dd}'),
            ("syriac abbreviation mark", '\u{70f}', '\u{70f}'),
            ("arabic pound and piastre marks", '\u{890}', '\u{891}'),
            ("arabic disputed end of ayah", '\u{8e2}', '\u{8e2}'),
            ("hangul choseong and jungseong fillers", '\u{115f}', '\u{1160}'),
            ("khmer inherent vowels", '\u{17b4}', '\u{17b5}'),
            ("mongolian free variation selectors and vowel separator", '\u{180b}', '\u{180f}'),
            ("zero-width and bidi marks", '\u{200b}', '\u{200f}'),
            ("line and paragraph separators and the embeddings", '\u{2028}', '\u{202e}'),
            ("word joiner, invisible operators and the isolates", '\u{2060}', '\u{206f}'),
            ("hangul filler", '\u{3164}', '\u{3164}'),
            ("variation selectors 1 to 14", '\u{fe00}', '\u{fe0d}'),
            ("byte order mark", '\u{feff}', '\u{feff}'),
            ("halfwidth hangul filler", '\u{ffa0}', '\u{ffa0}'),
            ("interlinear annotation", '\u{fff0}', '\u{fffb}'),
            ("kaithi number signs", '\u{110bd}', '\u{110bd}'),
            ("kaithi number sign above", '\u{110cd}', '\u{110cd}'),
            ("egyptian hieroglyph format controls", '\u{13430}', '\u{1343f}'),
            ("shorthand format controls", '\u{1bca0}', '\u{1bca3}'),
            ("musical symbol format controls", '\u{1d173}', '\u{1d17a}'),
            ("language tag, the tag characters and variation selectors 17 to 256", '\u{e0000}', '\u{e0fff}'),
        ];
        for (what, first, last) in gone {
            for c in [*first, *last, char::from_u32((*first as u32 + *last as u32) / 2).unwrap_or(*first)] {
                assert_eq!(clean(&format!("a{c}b"), 10), "ab", "{what}: U+{:04X}", c as u32);
            }
        }
        // a message in tag characters ("evil", each letter as its tag twin) is nothing, and so is a run of variation selectors
        let tagged: String = "evil".chars().map(|c| char::from_u32(0xe0000 + c as u32).unwrap()).collect();
        assert_eq!(clean(&format!("release key{tagged}"), 50), "release key");
        assert_eq!(clean("a\u{e0100}\u{e01ef}\u{fe00}\u{3164}\u{115f}\u{206a}\u{206f}b", 50), "ab");
        // VS15 and VS16 only choose how the character before them is drawn; they hide nothing
        assert_eq!(clean("\u{2714}\u{fe0e} \u{2714}\u{fe0f}", 50), "\u{2714}\u{fe0e} \u{2714}\u{fe0f}");
        // and text in the scripts the format characters belong to is still text
        assert_eq!(clean("clé Ünï 日本 العربية שלום ไทย 🔑", 50), "clé Ünï 日本 العربية שלום ไทย 🔑");
        // the neighbours of each range are not in it
        for kept in ['\u{ac}', '\u{ae}', '\u{5ff}', '\u{606}', '\u{61b}', '\u{61d}', '\u{1161}', '\u{1810}', '\u{2027}', '\u{202f}', '\u{205f}', '\u{3165}', '\u{fe10}', '\u{ffa1}', '\u{fffc}', '\u{e1000}'] {
            let text = format!("a{kept}b");
            assert_eq!(clean(&text, 10), text, "U+{:04X} is an ordinary character", kept as u32);
        }
    }

    fn done(v: Verdict) -> String {
        match v {
            Verdict::Done(id) => id.as_str().to_string(),
            Verdict::Refused { kind, sentence } => panic!("refused {kind}: {sentence}"),
            Verdict::Broken(code, why) => panic!("broken {code}: {why}"),
        }
    }

    fn out(text: &str, code: i32) -> Ran {
        Ran::Out(text.as_bytes().to_vec(), Some(code))
    }

    /// Success needs the exit code and the answer to agree; anything else claims nothing, and a refusal's kind is one of the contract's.
    #[test]
    fn a_key_change_is_done_only_when_the_exit_and_the_answer_agree() {
        assert_eq!(done(judge("added", out(r#"{"added":"0123456789abcdef"}"#, 0))), "0123456789ABCDEF");
        assert_eq!(done(judge("removed", out(r#"{"removed":"0123456789ABCDEF"}"#, 0))), "0123456789ABCDEF");
        let broken = |v: Verdict| matches!(v, Verdict::Broken(StatusCode::BAD_GATEWAY, _));
        for (word, text, code) in [
            ("added", r#"{"added":"0123456789ABCDEF"}"#, 1),
            ("added", r#"{"added":"0123456789ABCDEF"}"#, 2),
            ("added", r#"{"error":{"kind":"mismatch","sentence":"x"}}"#, 0),
            ("added", r#"{"added":"0123456789ABCDEF","error":{"kind":"mismatch","sentence":"x"}}"#, 0),
            ("added", r#"{"added":"0123456789ABCDEF","error":{"kind":"mismatch","sentence":"x"}}"#, 1),
            ("added", r#"{"removed":"0123456789ABCDEF"}"#, 0),
            ("removed", r#"{"added":"0123456789ABCDEF"}"#, 0),
            ("added", r#"{"added":"zz"}"#, 0),
            ("added", r#"{"added":"0123456789ABCDEF0"}"#, 0),
            ("added", r#"{"added":7}"#, 0),
            ("added", r#"{"error":"mismatch"}"#, 1),
            ("added", r#"{"error":["mismatch"]}"#, 1),
            ("added", r#"["added"]"#, 0),
            ("added", r#""added""#, 0),
            ("added", "[]", 1),
            ("added", "not json", 0),
            ("added", "", 0),
            ("added", "", 1),
        ] {
            assert!(broken(judge(word, out(text, code))), "{word} {text:?} exit {code}");
        }
        assert!(matches!(judge("added", Ran::Out(Vec::new(), None)), Verdict::Broken(StatusCode::BAD_GATEWAY, _)), "killed by a signal");
        assert!(matches!(judge("added", Ran::TooBig), Verdict::Broken(StatusCode::BAD_GATEWAY, _)));
        assert!(matches!(judge("added", Ran::Spawn("no such file".into())), Verdict::Broken(StatusCode::BAD_GATEWAY, _)));
        assert!(matches!(judge("added", Ran::Timeout), Verdict::Broken(StatusCode::GATEWAY_TIMEOUT, _)));
    }

    /// Everything that gives no answer to trust after the arm is spent says the same thing, because for all of them the change may have been
    /// made: to list the keys.
    #[test]
    fn a_change_that_gave_no_answer_to_trust_says_to_list_the_keys() {
        let said = |ran: Ran, word: &str| match judge(word, ran) {
            Verdict::Broken(code, sentence) => (code, sentence),
            _ => panic!("broken"),
        };
        for (ran, code) in [
            (Ran::Timeout, StatusCode::GATEWAY_TIMEOUT),
            (Ran::TooBig, StatusCode::BAD_GATEWAY),
            (Ran::Spawn("x".into()), StatusCode::BAD_GATEWAY),
            (Ran::Out(Vec::new(), None), StatusCode::BAD_GATEWAY),
            (out("not json", 0), StatusCode::BAD_GATEWAY),
            (out(r#"{"added":"zz"}"#, 0), StatusCode::BAD_GATEWAY),
            (out("", 1), StatusCode::BAD_GATEWAY),
        ] {
            let (got, sentence) = said(ran, "added");
            assert_eq!(got, code);
            assert!(sentence.ends_with("; list the keys to see whether the change was made"), "{sentence}");
            assert!(sentence.len() > 60);
        }
    }

    #[test]
    fn a_refusal_keeps_its_kind_when_the_kind_is_one_the_contract_names_and_is_failed_otherwise() {
        let refused = |text: &str| match judge("added", out(text, 1)) {
            Verdict::Refused { kind, sentence } => (kind, sentence),
            _ => panic!("a refusal: {text}"),
        };
        for kind in KINDS {
            let (got, sentence) = refused(&format!(r#"{{"error":{{"kind":"{kind}","sentence":"nothing changed"}}}}"#));
            assert_eq!((got, sentence.as_str()), (kind, "nothing changed"));
        }
        for kind in ["rotation", "listed", "unknown", "MISMATCH", "mismatch ", "", "nix", "../x"] {
            assert_eq!(refused(&format!(r#"{{"error":{{"kind":"{kind}","sentence":"s"}}}}"#)).0, "failed", "{kind:?}");
        }
        assert_eq!(refused(r#"{"error":{"sentence":"no kind"}}"#).0, "failed");
        assert_eq!(refused(r#"{"error":{"kind":"revoked"}}"#).1, "minerva refused (revoked)", "a refusal with no sentence still says something");
        let (_, sentence) = refused(&format!(r#"{{"error":{{"kind":"failed","sentence":"{}\u001b[2J<RLO>"}}}}"#, "w".repeat(700)).replace("<RLO>", "\\u202e"));
        assert_eq!((sentence.chars().count(), sentence.ends_with('…')), (SENTENCE_MAX, true), "cut at 600, and the cut shows");
        assert!(!sentence.contains('\u{1b}') && !sentence.contains('\u{202e}'));
        // a sentence of several lines of advice, as minerva's refusals are, is shown whole
        let advice = "w ".repeat(250);
        assert_eq!(refused(&format!(r#"{{"error":{{"kind":"failed","sentence":"{}"}}}}"#, advice.trim())).1, advice.trim());
    }

    #[test]
    fn the_status_is_the_contracts_fields_cleaned_and_nothing_it_did_not_name() {
        let seen = r#"{"version":"0.1.0-gabc1234","secret":"x","notice":{"line":"Minerva 0.2.0 available -- run: hoot update","latest":"0.2.0","stale":null,"checked":1791269513,"more":1},
            "plugins":[{"name":"browser","version":"0.3.0","how":"stock","token":"t"}],"jev":{"set_up":true,"running":true,"pid":4242,"extra":[]}}"#;
        let got = reshape_status(seen.as_bytes()).unwrap();
        assert_eq!(
            got,
            json!({"installed": true, "version": "0.1.0-gabc1234",
                   "notice": {"line": "Minerva 0.2.0 available -- run: hoot update", "latest": "0.2.0", "stale": null, "checked": 1791269513u64},
                   "plugins": [{"name": "browser", "version": "0.3.0", "how": "stock"}], "jev": {"set_up": true, "running": true, "pid": 4242}})
        );
        // a pid that is not running's is dropped, an empty string is no string, and text is cleaned
        let text = r#"{"version":"1<RLO>","notice":{"line":"","latest":null,"stale":"old\u001b[2J","checked":1},"plugins":[],"jev":{"set_up":true,"running":false,"pid":9}}"#;
        let got = reshape_status(text.replace("<RLO>", "\\u202e").as_bytes()).unwrap();
        assert_eq!(got["version"], "1");
        assert_eq!((&got["notice"]["line"], &got["notice"]["stale"]), (&Value::Null, &json!("old[2J")));
        assert_eq!(got["jev"], json!({"set_up": true, "running": false}));
        assert_eq!(reshape_status(br#"{"version":"1","notice":null,"plugins":[],"jev":{"set_up":false,"running":false}}"#).unwrap()["notice"], Value::Null);
        let many: Vec<String> = (0..MAX_PLUGINS + 5).map(|n| format!(r#"{{"name":"p{n}","version":"1","how":"stock"}}"#)).collect();
        let big = format!(r#"{{"version":"1","notice":null,"plugins":[{}],"jev":{{"set_up":false,"running":false}}}}"#, many.join(","));
        assert_eq!(reshape_status(big.as_bytes()).unwrap()["plugins"].as_array().unwrap().len(), MAX_PLUGINS);
        for bad in [
            "", "null", "[]", "{}", "not json", r#"{"version":"1"}"#, r#"{"version":1,"notice":null,"plugins":[],"jev":{"set_up":false,"running":false}}"#,
            r#"{"version":"1","notice":null,"plugins":{},"jev":{"set_up":false,"running":false}}"#,
            r#"{"version":"1","notice":{"line":null,"latest":null,"stale":null},"plugins":[],"jev":{"set_up":false,"running":false}}"#,
            r#"{"version":"1","notice":null,"plugins":[],"jev":{"set_up":"yes","running":false}}"#,
            r#"{"version":"1","notice":null,"plugins":[],"jev":{"set_up":false,"running":false,"pid":-1}}"#,
        ] {
            assert!(reshape_status(bad.as_bytes()).is_none(), "{bad:?}");
        }
    }

    /// A list that leaves a key out is worse than none, so one row that is not what the contract says makes the list unreadable.
    #[test]
    fn the_key_list_is_read_strictly() {
        let row = |extra: &str| format!(r#"{{"id":"0123456789ABCDEF","fingerprint":"0f7a 3a5c 447e c7ef","state":"trusted","source":"added by hand","expires":{extra}}}"#);
        let rows = reshape_keys(format!("[{}]", row("null")).as_bytes()).unwrap();
        assert_eq!(
            rows[0].page(),
            json!({"id": "0123456789ABCDEF", "fingerprint": "0F7A 3A5C 447E C7EF", "state": "trusted", "source": "added by hand", "expires": null})
        );
        assert_eq!(reshape_keys(format!("[{}]", row(r#""2028-01-01""#)).as_bytes()).unwrap()[0].expires.as_deref(), Some("2028-01-01"));
        assert_eq!(reshape_keys(b"[]").unwrap().len(), 0);
        // the three labels are known; anything else, a lookalike included, is `other` and so is never removable
        for (label, known) in [("compiled in", "compiled in"), ("added by a signed rotation", "added by a signed rotation"), ("added by hand", "added by hand")] {
            let one = format!("[{}]", row("null").replace("added by hand", label));
            assert_eq!(reshape_keys(one.as_bytes()).unwrap()[0].source, known);
        }
        for lookalike in ["added by hand ", "Added by hand", "added  by hand", "added by hand\\u202e", "added\\u200b by hand", "added by hand\\u0000", "", &"s".repeat(65)] {
            let one = format!("[{}]", row("null").replace("added by hand", lookalike));
            assert_eq!(reshape_keys(one.as_bytes()).unwrap()[0].source, "other", "{lookalike:?}");
        }
        for bad in [r#""2028-1-01""#, r#""2028/01/01""#, r#""tomorrow""#, "2028", r#""2028-01-01x""#, "false"] {
            assert!(reshape_keys(format!("[{}]", row(bad)).as_bytes()).is_none(), "expires {bad}");
        }
        let good = row("null");
        for broken in [
            good.replace("0123456789ABCDEF", "0123456789ABCDE"),
            good.replace("0f7a 3a5c 447e c7ef", "0f7a 3a5c 447e"),
            good.replace("trusted", "ok"),
            good.replace(r#""source":"added by hand","#, ""),
            good.replace(r#""id":"0123456789ABCDEF","#, ""),
            good.replace(r#""expires":null"#, r#""x":1"#),
        ] {
            assert!(reshape_keys(format!("[{good},{broken}]").as_bytes()).is_none(), "{broken}");
        }
        let many = vec![good.clone(); MAX_KEYS + 1].join(",");
        assert!(reshape_keys(format!("[{many}]").as_bytes()).is_none(), "more rows than a key list holds");
        assert!(reshape_keys(format!("[{}]", vec![good; MAX_KEYS].join(",")).as_bytes()).is_some(), "exactly the most is fine");
        for not in ["", "{}", "null", r#"{"keys":[]}"#, "[1]", "[null]", r#"["x"]"#] {
            assert!(reshape_keys(not.as_bytes()).is_none(), "{not:?}");
        }
    }
}
