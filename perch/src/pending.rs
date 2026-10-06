//! Fork from a point (hub.md section 7c): `POST /hub/sessions/<id>/fork {"at": <record>}`, then the fork's first message.
//!
//! eidolon has no fork-only command, and the head of a log is not in the file, so a point in the middle of a chat can only be
//! kept by *a turn run from it*: `eidolon resume <log> --at <record> "<message>"` moves the head in memory, runs the person's own
//! message, and the records it appends carry the parent that makes the branch stay. That turn is the one the person was going to
//! send anyway, so a fork from here costs nothing extra, and everything else here is stock eidolon commands and eidolon's own JSON:
//! no byte of the EIDO format is read or written by the perch.
//!
//! ```text
//!   POST .../<id>/fork {at}   check `at` against eidolon's own `log --json` lines, copy the log (as a whole-chat fork does),
//!                             remember {from, at, pending} in <home>/forks.json          -> 201 {id, from, at, pending: true}
//!        (a PENDING fork has no door: nothing may append to its copy, so it is never resumed, restarted or forked)
//!   GET  .../<fork>/transcript  the lines of the branch that ends at `at` (not the whole copy)
//!   POST .../<fork>/first {text, images?, yolo?}
//!                             eidolon [--config C] [--yolo] resume <copy> --at <at> --cwd <dir> [--image P]... -- "<text>"
//!                             stdin closed, bounded time, output kept (tail)                          -> 202 {id, state: "running"}
//!   GET  .../<fork>/first     {state: none|running|failed|done, elapsed_ms, out, error?}
//!                             exit 0 and the log settled: the fork is no longer pending, a door comes up on the copy (it heads on
//!                             the new branch), state done. Anything else: state failed, the fork stays pending and can be retried.
//! ```
//!
//! **Which points.** Only a reply that ended a turn (an assistant message with no tool call, and every earlier tool call answered).
//! `resume --at N` on a branch that ends in a user message, or in a reply that asked for tools, first *finishes that turn*, which is a
//! model call the person did not ask for; the check refuses those points with a sentence, before anything is copied.
//!
//! **Headless asks.** `resume` reads approvals from stdin (`StdinUser`); with stdin closed it reads end-of-file and the ask is declined,
//! never waited on. With `--yolo` the gate answers yes by itself. The page says so beside the composer.
//!
//! **The records are what the run left.** A pending fork shows the chain of records ending at `at`; a fork whose first message has
//! run shows the chain ending at the log's last record, which is where a door opens it.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use bytes::Bytes;
use hyper::{Response, StatusCode};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt as _;
use tokio::process::Command;

use crate::serve::{BoxBody, Ctx, json, problem, status};
use crate::sessions::{id_ok, log_path};

/// How long a first message may run before it is stopped. `MINERVA_PERCH_FIRST_MS` shortens it (the tests do).
const MAX_RUN: Duration = Duration::from_secs(10 * 60);
const KNOB: &str = "MINERVA_PERCH_FIRST_MS";
/// What is kept of a run's output: the tail, shown while it works and after it fails.
const KEEP_OUT: usize = 32 * 1024;
/// A first message is a message: bounded like one.
const MAX_TEXT: usize = 100_000;
/// Images in a first message: as many as the door takes in one `say`, each at the door's size.
const MAX_IMAGES: usize = 8;
const MAX_IMAGE: usize = 4 * 1024 * 1024;

fn max_run() -> Duration {
    std::env::var(KNOB).ok().and_then(|ms| ms.parse().ok()).map(Duration::from_millis).unwrap_or(MAX_RUN)
}

/// What the perch remembers of a chat made by fork-from-here.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Fork {
    pub from: String,
    pub at: u64,
    /// No first message has run: the copy has no door and its head is `at`.
    pub pending: bool,
    /// The model and `--yolo` the source ran with when it was forked, which the door comes up with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub yolo: bool,
    /// The first message's run while it is in flight, so a perch that restarts mid-run still knows ([`recover`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub running: Option<Running>,
}

/// The `eidolon resume` of a first message, as `forks.json` remembers it while it runs.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Running {
    /// Its pid (it is its own process group leader).
    pub pid: u32,
    pub started_ms: u64,
    /// The highest record id the copy had before the run: a run adds records above it, so a log with more than this has been written to.
    pub base: u64,
}

/// One first message's run, in memory.
pub struct Run {
    started: Instant,
    out: Mutex<String>,
    outcome: Mutex<Option<std::result::Result<(), String>>>,
}

impl Run {
    fn push(&self, text: &str) {
        let mut out = self.out.lock().unwrap();
        out.push_str(text);
        if out.len() > KEEP_OUT {
            let mut cut = out.len() - KEEP_OUT;
            while !out.is_char_boundary(cut) {
                cut += 1;
            }
            out.drain(..cut);
        }
    }

    fn finish(&self, outcome: std::result::Result<(), String>) {
        *self.outcome.lock().unwrap() = Some(outcome);
    }
}

/// `<home>/forks.json` `{id: Fork}`, written atomically with mode `0600`, and the runs in flight.
pub struct Forks {
    file: Option<PathBuf>,
    map: Mutex<BTreeMap<String, Fork>>,
    runs: Mutex<HashMap<String, Arc<Run>>>,
}

impl Forks {
    pub fn open(file: Option<PathBuf>) -> Self {
        let map = crate::statefile::load(file.as_deref());
        Self { file, map: Mutex::new(map), runs: Mutex::new(HashMap::new()) }
    }

    pub fn get(&self, id: &str) -> Option<Fork> {
        self.map.lock().unwrap().get(id).cloned()
    }

    /// Whether `id` is a fork still waiting for its first message.
    pub fn is_pending(&self, id: &str) -> bool {
        self.get(id).is_some_and(|fork| fork.pending)
    }

    /// Whether a first message is running for `id` right now.
    pub fn running(&self, id: &str) -> bool {
        self.runs.lock().unwrap().get(id).is_some_and(|run| run.outcome.lock().unwrap().is_none())
    }

    /// Changes one entry (`None` removes it). The memory changes only if the write succeeded.
    fn put(&self, id: &str, fork: Option<Fork>) -> Result<()> {
        let mut map = self.map.lock().unwrap();
        let mut next = map.clone();
        match fork {
            Some(fork) => next.insert(id.to_string(), fork),
            None => next.remove(id),
        };
        if next == *map {
            return Ok(());
        }
        if let Some(file) = &self.file {
            let body = serde_json::to_vec_pretty(&next).context("serializing the forks")?;
            harnox::fs::write_atomic_0600(file, &body).with_context(|| format!("writing {}", file.display()))?;
        }
        *map = next;
        Ok(())
    }

    /// Reserves the one run a fork may have: `None` when one is in flight (the check and the insert are one lock).
    fn begin_run(&self, id: &str) -> Option<Arc<Run>> {
        let mut runs = self.runs.lock().unwrap();
        if runs.get(id).is_some_and(|run| run.outcome.lock().unwrap().is_none()) {
            return None;
        }
        let run = Arc::new(Run { started: Instant::now(), out: Mutex::new(String::new()), outcome: Mutex::new(None) });
        runs.insert(id.to_string(), run.clone());
        Some(run)
    }

    /// Gives a reservation back (the run never started).
    fn cancel_run(&self, id: &str, run: &Arc<Run>) {
        let mut runs = self.runs.lock().unwrap();
        if runs.get(id).is_some_and(|have| Arc::ptr_eq(have, run)) {
            runs.remove(id);
        }
    }

    /// Writes down (or, with `None`, forgets) the run that is in flight.
    fn mark_running(&self, id: &str, running: Option<Running>) -> Result<()> {
        match self.get(id) {
            Some(fork) if fork.running != running => self.put(id, Some(Fork { running, ..fork })),
            _ => Ok(()),
        }
    }

    /// The first message has run: this is an ordinary chat now.
    fn settle(&self, id: &str) -> Result<()> {
        match self.get(id) {
            Some(fork) if fork.pending => self.put(id, Some(Fork { pending: false, ..fork })),
            _ => Ok(()),
        }
    }

    /// Every fork, for the one pass at start.
    fn marked(&self) -> Vec<(String, Fork)> {
        self.map.lock().unwrap().iter().map(|(id, fork)| (id.clone(), fork.clone())).collect()
    }

    /// Drops what is known of a chat that is gone for good (the trash was emptied).
    pub fn forget(&self, id: &str) {
        self.runs.lock().unwrap().remove(id);
        if let Err(e) = self.put(id, None) {
            tracing::warn!(error = %format!("{e:#}"), "perch: forks.json could not be written");
        }
    }
}

// ---------------------------------------------------------------------------
// eidolon's own JSON lines: which points can be forked, and which records a branch has
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Link {
    id: u64,
    parent: Option<u64>,
}

/// The lines of `text` (eidolon's `log --json`, one record per line) that are on the chain ending at `head` (the last record when
/// `None`), in file order. A line that is not a record is skipped.
pub fn branch(text: &[u8], head: Option<u64>) -> Vec<&[u8]> {
    let lines: Vec<(&[u8], Link)> =
        text.split(|b| *b == b'\n').filter(|line| !line.is_empty()).filter_map(|line| serde_json::from_slice::<Link>(line).ok().map(|link| (line, link))).collect();
    let parents: HashMap<u64, Option<u64>> = lines.iter().map(|(_, link)| (link.id, link.parent)).collect();
    let mut keep = HashSet::new();
    let mut cur = head.or_else(|| lines.last().map(|(_, link)| link.id));
    while let Some(id) = cur {
        if !keep.insert(id) {
            break;
        }
        cur = parents.get(&id).copied().flatten();
    }
    lines.into_iter().filter(|(_, link)| keep.contains(&link.id)).map(|(line, _)| line).collect()
}

fn kind_of(record: &serde_json::Value) -> (&str, Option<&serde_json::Value>) {
    match record.get("kind") {
        Some(serde_json::Value::String(name)) => (name.as_str(), None),
        Some(serde_json::Value::Object(map)) => map.iter().next().map(|(name, value)| (name.as_str(), Some(value))).unwrap_or(("", None)),
        _ => ("", None),
    }
}

fn blocks(message: Option<&serde_json::Value>) -> impl Iterator<Item = &serde_json::Value> {
    message.and_then(|m| m.get("content")).and_then(serde_json::Value::as_array).into_iter().flatten()
}

/// Whether the branch ending at record `at` can be forked from without a model call: `at` is an assistant message that asked for no
/// tool, and every tool call before it was answered. The words are for the person.
pub fn check_at(text: &[u8], at: u64) -> std::result::Result<(), &'static str> {
    let records: Vec<serde_json::Value> = branch(text, Some(at)).into_iter().filter_map(|line| serde_json::from_slice(line).ok()).collect();
    let Some(point) = records.iter().find(|r| r.get("id").and_then(serde_json::Value::as_u64) == Some(at)) else {
        return Err("that point is not in this chat");
    };
    match kind_of(point) {
        ("AssistantMessage", message) => {
            if blocks(message).any(|b| b.get("type").and_then(serde_json::Value::as_str) == Some("tool_use")) {
                return Err("that reply asked for tools: fork from the reply that ended the turn");
            }
        }
        _ => return Err("a fork starts after one of the assistant's replies; after your own message the model would have to answer it first, which is a turn you did not ask for"),
    }
    let (mut asked, mut answered) = (HashSet::new(), HashSet::new());
    for record in &records {
        match kind_of(record) {
            ("AssistantMessage", message) => {
                for b in blocks(message).filter(|b| b.get("type").and_then(serde_json::Value::as_str) == Some("tool_use")) {
                    if let Some(id) = b.get("id").and_then(serde_json::Value::as_str) {
                        asked.insert(id.to_string());
                    }
                }
            }
            ("UserMessage", message) => {
                for b in blocks(message).filter(|b| b.get("type").and_then(serde_json::Value::as_str) == Some("tool_result")) {
                    if let Some(id) = b.get("tool_use_id").and_then(serde_json::Value::as_str) {
                        answered.insert(id.to_string());
                    }
                }
            }
            ("ToolResult", Some(result)) => {
                if let Some(id) = result.get("tool_use_id").and_then(serde_json::Value::as_str) {
                    answered.insert(id.to_string());
                }
            }
            _ => {}
        }
    }
    if asked.is_subset(&answered) { Ok(()) } else { Err("a tool call before that point never got its result") }
}

/// The transcript of a chat made by fork-from-here: the chain ending at `at` while it is pending, else the chain ending at the last
/// record. A chat that is not one is served as it is.
pub fn shown(ctx: &Ctx, id: &str, lean: &[u8]) -> Option<Vec<u8>> {
    let fork = ctx.registry.forks.get(id)?;
    let lines = branch(lean, fork.pending.then_some(fork.at));
    let mut out = Vec::with_capacity(lean.len());
    for line in lines {
        out.extend_from_slice(line);
        out.push(b'\n');
    }
    Some(out)
}

// ---------------------------------------------------------------------------
// POST /hub/sessions/<id>/fork {at}
// ---------------------------------------------------------------------------

/// The fork of `id` at record `at`: a pending copy. 404 no such chat; 422 a point that cannot be forked (a sentence); 413/502/504 eidolon
/// could not read the chat; 503 the copy could not be made; 201 `{"id", "from", "at", "pending": true}`.
pub(crate) async fn fork_at(ctx: &Ctx, id: &str, at: u64) -> Response<BoxBody> {
    let Some((source, _)) = ctx.registry.find_log(id) else { return status(StatusCode::NOT_FOUND) };
    if ctx.registry.id_of_log(&source).as_deref() != Some(id) {
        return status(StatusCode::NOT_FOUND);
    }
    if ctx.registry.forks.is_pending(id) {
        return problem(StatusCode::CONFLICT, "that fork has not had its first message yet: send one, then fork from it");
    }
    if ctx.registry.home().is_none() {
        return problem(StatusCode::SERVICE_UNAVAILABLE, "this server has no Minerva home to keep that in");
    }
    let lean = match crate::images::lean(ctx, id).await {
        Ok(lean) => lean,
        Err(refusal) => return refusal,
    };
    let checked = tokio::task::spawn_blocking(move || check_at(&lean, at)).await;
    match checked {
        Ok(Ok(())) => {}
        Ok(Err(why)) => return problem(StatusCode::UNPROCESSABLE_ENTITY, why),
        Err(_) => return problem(StatusCode::SERVICE_UNAVAILABLE, "that chat could not be read"),
    }
    let dir = ctx.registry.sessions_dir.clone();
    let copied = {
        let (source, dir) = (source.clone(), dir.clone());
        tokio::task::spawn_blocking(move || crate::fork::copy_log(&source, &dir)).await
    };
    let new_id = match copied {
        Ok(Ok(new_id)) => new_id,
        Ok(Err(e)) => {
            tracing::warn!(error = %format!("{e:#}"), "perch: a chat could not be copied for a fork from a point");
            return problem(StatusCode::SERVICE_UNAVAILABLE, "that chat could not be copied");
        }
        Err(_) => return problem(StatusCode::SERVICE_UNAVAILABLE, "that chat could not be copied"),
    };
    if let Some(cwd) = ctx.registry.known_cwd(id).filter(|cwd| cwd.is_dir()) {
        ctx.registry.set_cwd(&new_id, &cwd);
    }
    ctx.registry.refresh_titles().await;
    let (model, yolo) = ctx.registry.flags_of(id).unzip();
    let model = model.filter(|m| crate::models::key_ok(m));
    let fork = Fork { from: id.to_string(), at, pending: true, model, yolo: yolo.unwrap_or(false), running: None };
    if let Err(e) = ctx.registry.forks.put(&new_id, Some(fork)) {
        tracing::warn!(error = %format!("{e:#}"), "perch: forks.json could not be written");
        let _ = std::fs::remove_file(log_path(&dir, &new_id));
        ctx.registry.forget_cwd(&new_id);
        return problem(StatusCode::SERVICE_UNAVAILABLE, "that fork could not be saved");
    }
    let source_title = ctx.registry.titled_list().await.into_iter().find(|row| row.id == id).and_then(|row| row.title);
    if let Some(title) = source_title {
        let kept: String = title.chars().take(crate::names::MAX_TITLE - " (fork)".len()).collect();
        if let Err(e) = ctx.registry.names.set(&new_id, Some(&format!("{kept} (fork)"))) {
            tracing::warn!(error = %format!("{e:#}"), "perch: a fork's name could not be saved");
        }
    }
    json(StatusCode::CREATED, &serde_json::json!({ "id": new_id, "from": id, "at": at, "pending": true }))
}

// ---------------------------------------------------------------------------
// POST/GET /hub/sessions/<id>/first
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct Pic {
    media_type: String,
    data: String,
}

#[derive(Deserialize)]
struct First {
    text: String,
    #[serde(default)]
    images: Vec<Pic>,
    yolo: Option<bool>,
}

fn extension(media_type: &str) -> Option<&'static str> {
    match media_type {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        _ => None,
    }
}

/// What the child is run with: the whole argv, as a test reads it.
fn argv(config: Option<&Path>, yolo: bool, log: &Path, at: u64, cwd: Option<&Path>, images: &[PathBuf], text: &str) -> Vec<OsString> {
    let mut argv: Vec<OsString> = Vec::new();
    if let Some(config) = config {
        argv.push("--config".into());
        argv.push(config.as_os_str().to_os_string());
    }
    if yolo {
        argv.push("--yolo".into());
    }
    argv.extend(["resume".into(), log.as_os_str().to_os_string(), "--at".into(), at.to_string().into()]);
    if let Some(cwd) = cwd {
        argv.push("--cwd".into());
        argv.push(cwd.as_os_str().to_os_string());
    }
    for image in images {
        argv.push("--image".into());
        argv.push(image.as_os_str().to_os_string());
    }
    // After `--`, so a message that starts with a dash is still the message.
    argv.push("--".into());
    argv.push(text.into());
    argv
}

/// `POST /hub/sessions/<id>/first`: see the module doc.
///
/// ```text
///   400 not that shape, an empty message, too many or bad images       404 no such chat, or not a pending fork
///   409 already running | a door or another process holds the copy     503 no Minerva home
///   202 {"id", "state": "running"}
/// ```
pub(crate) async fn first(ctx: &Ctx, id: &str, body: &Bytes) -> Response<BoxBody> {
    if !id_ok(id) {
        return status(StatusCode::NOT_FOUND);
    }
    let Some(fork) = ctx.registry.forks.get(id).filter(|fork| fork.pending) else { return status(StatusCode::NOT_FOUND) };
    let log = match crate::trash::own_log(ctx, id) {
        Ok(log) => log,
        Err(refusal) => return *refusal,
    };
    let Ok(asked) = serde_json::from_slice::<First>(body) else {
        return problem(StatusCode::BAD_REQUEST, "expected a JSON body of {\"text\": \"<message>\", \"images\"?: [{\"media_type\", \"data\"}]}");
    };
    if asked.text.trim().is_empty() {
        return problem(StatusCode::BAD_REQUEST, "write the first message of the fork");
    }
    if asked.text.len() > MAX_TEXT {
        return problem(StatusCode::BAD_REQUEST, "that message is too long");
    }
    if asked.images.len() > MAX_IMAGES {
        return problem(StatusCode::BAD_REQUEST, "at most 8 images in one message");
    }
    if ctx.registry.forks.running(id) {
        return problem(StatusCode::CONFLICT, "the first message of that fork is already running");
    }
    let mut pictures: Vec<(&'static str, Vec<u8>)> = Vec::new();
    for pic in &asked.images {
        let Some(ext) = extension(&pic.media_type) else { return problem(StatusCode::BAD_REQUEST, "images must be png, jpeg, gif or webp") };
        let Some(bytes) = crate::images::decode(&pic.data).filter(|b| !b.is_empty() && b.len() <= MAX_IMAGE) else {
            return problem(StatusCode::BAD_REQUEST, "an image is not valid base64, or is over 4 MiB");
        };
        pictures.push((ext, bytes));
    }
    let Some(home) = ctx.registry.home().map(Path::to_path_buf) else { return problem(StatusCode::SERVICE_UNAVAILABLE, "this server has no Minerva home to keep that in") };
    let cwd = ctx.registry.reopen_cwd(id).or_else(|| ctx.registry.known_cwd(id).filter(|cwd| cwd.is_dir()));
    let yolo = asked.yolo.unwrap_or(fork.yolo);
    // The highest record the copy has before the run, which is how a perch that restarts mid-run tells whether the run wrote anything.
    let base = match crate::images::lean(ctx, id).await {
        Ok(lean) => highest_id(&lean),
        Err(refusal) => return refusal,
    };
    // Reserved in one step (the check and the insert are one lock), and only then is the folder looked at: a move takes its own guard first
    // and looks for a run second, so whichever comes second sees the other.
    let Some(run) = ctx.registry.forks.begin_run(id) else { return problem(StatusCode::CONFLICT, "the first message of that fork is already running") };
    if ctx.registry.held(&log) || ctx.registry.roster_holds(&log) || ctx.registry.is_moving(id) {
        ctx.registry.forks.cancel_run(id, &run);
        return problem(StatusCode::CONFLICT, "that fork is being changed or held by a session; try again in a moment");
    }
    let (ctx, id_owned) = (ctx.clone(), id.to_string());
    tokio::spawn(async move { execute(ctx, id_owned, fork, log, home, cwd, yolo, base, pictures, asked.text, run).await });
    json(StatusCode::ACCEPTED, &serde_json::json!({ "id": id, "state": "running" }))
}

/// The highest `id` among eidolon's JSON lines (0 when there are none).
fn highest_id(text: &[u8]) -> u64 {
    text.split(|b| *b == b'\n').filter_map(|line| serde_json::from_slice::<Link>(line).ok()).map(|link| link.id).max().unwrap_or(0)
}

/// Writes the pictures where `--image` can read them (`<home>/first-<id>/<n>.<ext>`, `0700` folder, `0600` files), and removes the folder when dropped.
struct Pics {
    dir: PathBuf,
    files: Vec<PathBuf>,
}

impl Pics {
    fn write(home: &Path, id: &str, pictures: &[(&'static str, Vec<u8>)]) -> Result<Self> {
        use std::io::Write as _;
        use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
        let dir = home.join(format!("first-{id}"));
        let _ = std::fs::remove_dir_all(&dir);
        let mut made = Self { dir: dir.clone(), files: Vec::new() };
        if pictures.is_empty() {
            return Ok(made);
        }
        // `0700` said outright, whatever the umask: the folder holds what the person attached.
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir).with_context(|| format!("creating {}", dir.display()))?;
        for (n, (ext, bytes)) in pictures.iter().enumerate() {
            let path = dir.join(format!("{n}.{ext}"));
            let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path).with_context(|| format!("creating {}", path.display()))?;
            file.write_all(bytes).with_context(|| format!("writing {}", path.display()))?;
            made.files.push(path);
        }
        Ok(made)
    }
}

impl Drop for Pics {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Decodes a byte stream into text a chunk at a time, keeping a character that was split across two reads.
#[derive(Default)]
struct Utf8Tail(Vec<u8>);

impl Utf8Tail {
    fn feed(&mut self, chunk: &[u8]) -> String {
        self.0.extend_from_slice(chunk);
        let mut out = String::new();
        loop {
            match std::str::from_utf8(&self.0) {
                Ok(text) => {
                    out.push_str(text);
                    self.0.clear();
                    return out;
                }
                Err(e) => {
                    let valid = e.valid_up_to();
                    out.push_str(std::str::from_utf8(&self.0[..valid]).unwrap_or_default());
                    match e.error_len() {
                        // Not a character yet: wait for the rest of it.
                        None => {
                            self.0.drain(..valid);
                            return out;
                        }
                        Some(bad) => {
                            out.push('\u{fffd}');
                            self.0.drain(..valid + bad);
                        }
                    }
                }
            }
        }
    }
}

async fn pump(mut from: impl tokio::io::AsyncRead + Unpin, run: Arc<Run>) {
    let (mut buf, mut tail) = ([0u8; 4096], Utf8Tail::default());
    loop {
        match from.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => run.push(&tail.feed(&buf[..n])),
        }
    }
}

/// The last words of what the run printed, for a failure the person reads.
fn tail_words(out: &str) -> String {
    let lines: Vec<&str> = out.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    let last: Vec<&str> = lines.iter().rev().take(3).rev().copied().collect();
    let words = last.join(" / ");
    let cut: String = words.chars().rev().take(300).collect::<Vec<_>>().into_iter().rev().collect();
    cut
}

#[allow(clippy::too_many_arguments)]
async fn execute(ctx: Ctx, id: String, fork: Fork, log: PathBuf, home: PathBuf, cwd: Option<PathBuf>, yolo: bool, base: u64, pictures: Vec<(&'static str, Vec<u8>)>, text: String, run: Arc<Run>) {
    let outcome = ran(&ctx, &id, &fork, &log, &home, cwd.as_deref(), yolo, base, &pictures, &text, &run).await;
    if let Err(why) = &outcome {
        tracing::warn!(%why, id, "perch: the first message of a fork did not finish");
    }
    // The run is over, whatever it came to: it is no longer something a restart has to look for.
    if let Err(e) = ctx.registry.forks.mark_running(&id, None) {
        tracing::warn!(error = %format!("{e:#}"), "perch: forks.json could not be written");
    }
    run.finish(outcome);
}

#[allow(clippy::too_many_arguments)]
async fn ran(ctx: &Ctx, id: &str, fork: &Fork, log: &Path, home: &Path, cwd: Option<&Path>, yolo: bool, base: u64, pictures: &[(&'static str, Vec<u8>)], text: &str, run: &Arc<Run>) -> std::result::Result<(), String> {
    let pics = Pics::write(home, id, pictures).map_err(|e| format!("{e:#}"))?;
    let all = argv(ctx.registry.eidolon_config.as_deref(), yolo, log, fork.at, cwd, &pics.files, text);
    let mut command = Command::new(&ctx.registry.eidolon);
    // Its own process group, killed as a group (below, and when this future is dropped): the tools a turn starts die with it.
    command.args(&all).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true).process_group(0);
    let mut child = command.spawn().map_err(|e| format!("eidolon could not be started: {e}"))?;
    let mut group = Group(child.id());
    // Written down before anything else: a perch that restarts while this runs finds the run in `forks.json` ([`recover`]). Not written
    // means not trusted: the run is stopped rather than left where nothing could find it.
    let started_ms = crate::archive::now_ms();
    if let Some(pid) = child.id()
        && let Err(e) = ctx.registry.forks.mark_running(id, Some(Running { pid, started_ms, base }))
    {
        group.kill();
        let _ = child.kill().await;
        return Err(format!("the run could not be recorded, so it was stopped: {e:#}"));
    }
    let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
    let readers = [stdout.map(|out| tokio::spawn(pump(out, run.clone()))), stderr.map(|err| tokio::spawn(pump(err, run.clone())))];
    let limit = max_run();
    let waited = tokio::time::timeout(limit, child.wait()).await;
    let status = match waited {
        Ok(Ok(status)) => status,
        Ok(Err(e)) => return Err(format!("waiting for eidolon: {e}")),
        Err(_) => {
            group.kill();
            let _ = child.kill().await;
            return Err(format!("the first message ran longer than {} and was stopped; the fork is still waiting for one", spoken(limit)));
        }
    };
    group.0 = None; // the child was reaped: its group id may be anyone's now
    for reader in readers.into_iter().flatten() {
        let _ = tokio::time::timeout(Duration::from_secs(2), reader).await;
    }
    drop(pics);
    if !status.success() {
        let said = tail_words(&run.out.lock().unwrap());
        return Err(if said.is_empty() { format!("eidolon exited with {status}") } else { format!("eidolon exited with {status}: {said}") });
    }
    // A door opened on a log that ends mid-turn carries the turn on by itself, which is a second model call: look first.
    match crate::fork::settled(ctx, log, false).await {
        Some(true) => {}
        Some(false) => return Err("the turn did not finish (eidolon stopped part way); the fork is still waiting for a first message".to_string()),
        None => return Err("eidolon could not say whether the turn finished; the fork is still waiting for a first message".to_string()),
    }
    // From here the message has been spent: a retry would be a second one. The fork is an ordinary chat even if its door does not come up.
    ctx.registry.forks.settle(id).map_err(|e| format!("{e:#}"))?;
    let model = fork.model.clone().filter(|m| crate::models::key_ok(m));
    match crate::restart::bring_up(ctx, id, log, model.as_deref(), yolo || fork.yolo).await {
        Ok(_) => Ok(()),
        Err(crate::restart::Failed::Held) => Err("the reply is saved, but the copy was taken by another session; continue it from the list".to_string()),
        Err(crate::restart::Failed::Spawn(why)) => Err(format!("the reply is saved, but the chat did not come back ({why}); continue it from the list")),
    }
}

/// A run's process group, killed when this is dropped while it still holds the pid (the future was dropped, or an error left early).
struct Group(Option<u32>);

impl Group {
    fn kill(&self) {
        if let Some(pid) = self.0 {
            unsafe { libc::killpg(pid as libc::pid_t, libc::SIGKILL) };
        }
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Whether `pid` is an `eidolon ... resume <log> ...` that is still running: its argv (not just a live pid, which may be anyone's now)
/// names `resume` and this log. A zombie has no argv, so it is not one.
fn is_resume_of(pid: u32, log: &Path) -> bool {
    let Ok(raw) = std::fs::read(format!("/proc/{pid}/cmdline")) else { return false };
    let args: Vec<&[u8]> = raw.split(|b| *b == 0).collect();
    let log = log.as_os_str().as_encoded_bytes();
    args.contains(&b"resume".as_slice()) && args.contains(&log)
}

/// What a fork's first message was doing when the perch last stopped: every fork `forks.json` says was running gets a run entry that
/// holds the first message off (an orphan that is still going, or one not yet looked at), and the work that settles each is returned
/// for [`recover`]. Called once, before the perch serves.
pub(crate) fn adopt(ctx: &Ctx) -> Vec<(String, Fork, Running, Arc<Run>)> {
    let mut todo = Vec::new();
    for (id, fork) in ctx.registry.forks.marked() {
        let Some(running) = fork.running.clone() else { continue };
        let ago = Duration::from_millis(crate::archive::now_ms().saturating_sub(running.started_ms));
        let run = Arc::new(Run { started: Instant::now().checked_sub(ago).unwrap_or_else(Instant::now), out: Mutex::new(String::new()), outcome: Mutex::new(None) });
        ctx.registry.forks.runs.lock().unwrap().insert(id.clone(), run.clone());
        if is_resume_of(running.pid, &log_path(&ctx.registry.sessions_dir, &id)) {
            run.push("the perch restarted while this ran; the run is still going on its own and the page will show how it ends\n");
        }
        todo.push((id, fork, running, run));
    }
    todo
}

/// Settles what [`adopt`] found, never spending anything and never bringing a door up (the person opens a settled fork themselves):
///
/// ```text
///   the run is still going                 wait for it to end, then as below (a new first message is refused meanwhile)
///   it wrote nothing                       the fork is pending as before; it can be tried again (state none)
///   it wrote records past the point on a   and eidolon says the log is settled: the fork is an ordinary chat now, saved
///   chain through it                       not settled / cannot say: state failed with a sentence, still pending, NOT continued
/// ```
pub(crate) async fn recover(ctx: Ctx, todo: Vec<(String, Fork, Running, Arc<Run>)>) {
    for (id, fork, running, run) in todo {
        let log = log_path(&ctx.registry.sessions_dir, &id);
        if is_resume_of(running.pid, &log) {
            let ctx = ctx.clone();
            tokio::spawn(async move {
                while is_resume_of(running.pid, &log) {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
                after_orphan(&ctx, &id, &fork, &running, &log, &run).await;
            });
        } else {
            after_orphan(&ctx, &id, &fork, &running, &log, &run).await;
        }
    }
}

async fn after_orphan(ctx: &Ctx, id: &str, fork: &Fork, running: &Running, log: &Path, run: &Arc<Run>) {
    if let Err(e) = ctx.registry.forks.mark_running(id, None) {
        tracing::warn!(error = %format!("{e:#}"), "perch: forks.json could not be written");
    }
    let wrote = match crate::images::lean(ctx, id).await {
        Ok(lean) => Ok(wrote_past(&lean, fork.at, running.base)),
        Err(_) => Err(()),
    };
    let outcome = match wrote {
        Ok(false) => None,
        Ok(true) => match crate::fork::settled(ctx, log, false).await {
            Some(true) => Some(ctx.registry.forks.settle(id).map_err(|e| format!("{e:#}"))),
            Some(false) => Some(Err("the first message was running when the perch restarted and its turn did not finish; nothing was continued, and the fork is still waiting for a first message".to_string())),
            None => Some(Err("the first message was running when the perch restarted; eidolon could not say whether its turn finished, so nothing was continued and the fork is still waiting for a first message".to_string())),
        },
        Err(()) => Some(Err("the first message was running when the perch restarted and the copy could not be read to see how it ended; the fork is still waiting for a first message".to_string())),
    };
    match outcome {
        // Settled, or it never wrote: no failure to show. A settled fork is an ordinary saved chat; a pending one can be tried again.
        None | Some(Ok(())) => ctx.registry.forks.cancel_run(id, run),
        Some(Err(why)) => {
            tracing::warn!(%why, id, "perch: a fork's first message was found after a restart");
            run.finish(Err(why));
        }
    }
}

/// Whether the chain that ends at the log's last record runs through `at` and ends above `base`: records were written past the point.
pub fn wrote_past(text: &[u8], at: u64, base: u64) -> bool {
    let chain = branch(text, None);
    let ids: Vec<u64> = chain.iter().filter_map(|line| serde_json::from_slice::<Link>(line).ok()).map(|link| link.id).collect();
    ids.contains(&at) && ids.last().is_some_and(|last| *last > base.max(at))
}

fn spoken(limit: Duration) -> String {
    let secs = limit.as_secs();
    if secs >= 60 && secs.is_multiple_of(60) { format!("{} minutes", secs / 60) } else { format!("{} seconds", secs.max(1)) }
}

/// `GET /hub/sessions/<id>/first`: `{"state": "none"|"running"|"failed"|"done", "elapsed_ms", "out", "error"?, "yolo", "model"?, "at"}`. 404 for a chat that is not
/// a fork from a point.
pub(crate) fn state(ctx: &Ctx, id: &str) -> Response<BoxBody> {
    let Some(fork) = ctx.registry.forks.get(id).filter(|_| id_ok(id)) else { return status(StatusCode::NOT_FOUND) };
    let run = ctx.registry.forks.runs.lock().unwrap().get(id).cloned();
    let said = match run {
        None => serde_json::json!({ "state": "none", "elapsed_ms": 0, "out": "" }),
        Some(run) => {
            let elapsed = run.started.elapsed().as_millis() as u64;
            let out = run.out.lock().unwrap().clone();
            match run.outcome.lock().unwrap().clone() {
                None => serde_json::json!({ "state": "running", "elapsed_ms": elapsed, "out": out }),
                Some(Ok(())) => serde_json::json!({ "state": "done", "elapsed_ms": elapsed, "out": out }),
                Some(Err(why)) => serde_json::json!({ "state": "failed", "elapsed_ms": elapsed, "out": out, "error": why }),
            }
        }
    };
    let mut said = said;
    said["yolo"] = fork.yolo.into();
    said["model"] = fork.model.into();
    said["at"] = fork.at.into();
    json(StatusCode::OK, &said)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: u64, parent: Option<u64>, kind: serde_json::Value) -> String {
        format!("{}\n", serde_json::json!({ "id": id, "parent": parent, "ts_ms": id, "kind": kind }))
    }
    fn user(id: u64, parent: Option<u64>, text: &str) -> String {
        rec(id, parent, serde_json::json!({ "UserMessage": { "role": "user", "content": [{ "type": "text", "text": text }] } }))
    }
    fn bot(id: u64, parent: Option<u64>, content: serde_json::Value) -> String {
        rec(id, parent, serde_json::json!({ "AssistantMessage": { "role": "assistant", "content": content } }))
    }
    fn say(text: &str) -> serde_json::Value {
        serde_json::json!([{ "type": "text", "text": text }])
    }

    #[test]
    fn a_branch_is_the_chain_to_its_head_and_nothing_else() {
        let text = [user(1, None, "a"), bot(2, Some(1), say("b")), user(3, Some(2), "c"), bot(4, Some(3), say("d")), user(5, Some(2), "other"), bot(6, Some(5), say("e"))].concat();
        let ids = |head| branch(text.as_bytes(), head).iter().map(|l| serde_json::from_slice::<Link>(l).unwrap().id).collect::<Vec<_>>();
        assert_eq!(ids(Some(4)), [1, 2, 3, 4]);
        assert_eq!(ids(Some(2)), [1, 2]);
        assert_eq!(ids(None), [1, 2, 5, 6], "the last record is the head a door opens at");
        assert!(ids(Some(99)).is_empty());
        assert!(branch(b"not json\n{\"fake\":true}\n", None).is_empty());
    }

    #[test]
    fn only_a_reply_that_ended_the_turn_can_be_forked_from() {
        let call = serde_json::json!([{ "type": "text", "text": "looking" }, { "type": "tool_use", "id": "t1", "name": "ls", "input": {} }]);
        let result = rec(4, Some(3), serde_json::json!({ "UserMessage": { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "t1", "content": "x" }] } }));
        let text = [user(1, None, "a"), bot(2, Some(1), say("b")), user(3, Some(2), "c"), bot(4, Some(3), call.clone())].concat();
        assert_eq!(check_at(text.as_bytes(), 2), Ok(()));
        assert!(check_at(text.as_bytes(), 1).unwrap_err().contains("assistant's replies"), "a user message is a turn to finish");
        assert!(check_at(text.as_bytes(), 4).unwrap_err().contains("asked for tools"));
        assert!(check_at(text.as_bytes(), 9).unwrap_err().contains("not in this chat"));
        let answered = [user(1, None, "a"), bot(2, Some(1), call), result, bot(5, Some(4), say("done"))].concat();
        assert_eq!(check_at(answered.as_bytes(), 5), Ok(()));
        let owed = [user(1, None, "a"), bot(2, Some(1), serde_json::json!([{ "type": "tool_use", "id": "t9", "name": "ls", "input": {} }])), bot(3, Some(2), say("skipped on"))].concat();
        assert!(check_at(owed.as_bytes(), 3).unwrap_err().contains("never got its result"));
    }

    #[test]
    fn output_keeps_its_tail_and_a_split_character_survives() {
        let run = Run { started: Instant::now(), out: Mutex::new(String::new()), outcome: Mutex::new(None) };
        for _ in 0..(KEEP_OUT / 3 + 10) {
            run.push("aé");
        }
        let out = run.out.lock().unwrap().clone();
        assert!(out.len() <= KEEP_OUT && out.ends_with("aé") && out.is_char_boundary(0));
        let mut tail = Utf8Tail::default();
        let bytes = "né".as_bytes();
        assert_eq!(tail.feed(&bytes[..2]), "n");
        assert_eq!(tail.feed(&bytes[2..]), "é");
        assert_eq!(tail.feed(&[b'a', 0xff, b'b']), "a\u{fffd}b");
    }

    #[test]
    fn the_argv_is_stock_resume_with_the_message_after_a_dash_dash() {
        let all = argv(Some(Path::new("/h/minerva.toml")), true, Path::new("/h/sessions/7.eid"), 12, Some(Path::new("/w")), &[PathBuf::from("/h/first-7/0.png")], "-rf things");
        let all: Vec<String> = all.iter().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(all.join(" "), "--config /h/minerva.toml --yolo resume /h/sessions/7.eid --at 12 --cwd /w --image /h/first-7/0.png -- -rf things");
    }
}
