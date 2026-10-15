//! The runs' state and the routes that read and change it (slice R1a): `<home>/runs.json`, the per-run dir, the start gate, and
//! `GET|POST /hub/runs`, `GET /hub/runs/<rid>`, `POST /hub/runs/<rid>/answer|cancel|abandon`.
//!
//! ```text
//!   GET  /hub/runs[?chat=<id>]     [row], newest first, 50 at most; with ?chat= only that chat's runs
//!        400 a query that is not ?chat=<a chat's id>
//!   POST /hub/runs {graph, args?, chat?}  202 {rid, state: "running"}, with chat too when one was given
//!        400 a body that is not {graph: string, args?: object, chat?: id}, or args over 64 KiB     404 a graph outside --run-graphs, or with no
//!        workflows/<g>.rn, or a chat the perch does not know      409 {run, chat?} a run is running or parked (one shared Chromium)
//!        503 jev or the browser service is down (the sentence names its start command), or there is no --home / --plugins-dir
//!   GET  /hub/runs/<rid>           row + {question?, report?, refused?, out, err}: the tails of the child's stdout and stderr
//!   POST /hub/runs/<rid>/answer {text}   202 {rid, state: "running"}: a parked run is resumed with --answer=<text>
//!        400 no text, over 1 KiB or a control character       409 not parked, or a child is alive      503 a service is down
//!   POST /hub/runs/<rid>/cancel    200 row (202 if the child is still going): SIGTERM to the group, 5 s, SIGKILL; the state is cancelled
//!   POST /hub/runs/<rid>/abandon   200 row: a parked run becomes abandoned, no child is started; 409 if it is not parked
//! ```
//!
//! **A row** is `{rid, graph, state, started_ms, ended_ms?, exit?, why?, args?, chat?, resumes, stopping?}`. `state` is one of
//!
//! ```text
//!   running    a child (the first `workflow run`, or a `workflow resume`) is alive
//!   parked     the graph asked a person (`question` on the detail); answer it or abandon it
//!   done       completed (`report`, eidolon's JSON string, on the detail)         failed     the graph or eidolon ended it (`why`)
//!   cancelled  the page asked, and the group was stopped                          abandoned  a parked run the page gave up on
//!   refused    eidolon refused to start it (nothing was journaled; `why`)         lost       the child is gone and the journal has no end
//! ```
//!
//! A refused **resume** of a parked run (eidolon names the failed step it cannot replay: F0 cases c, g, j) leaves the run `parked` with
//! `refused: <eidolon's sentence>`: it is still a park in the journal, it still holds the shared browser, and abandon is how it ends.
//!
//! **The cap** is one run that is running or parked, taken in the same lock that creates the next one, so two starts together make one run
//! and a 409. **`runs.json`** is `{rid: Run}`, `0600`, written whole through `write_atomic_0600` after every change and only trusted once it
//! is written; a file that does not parse is set aside by [`crate::statefile`]. At most [`KEPT`] runs are kept in it (the oldest finished
//! ones go first); a run's directory is never removed.
//!
//! **What a person can ask of eidolon** is closed: the argv is built here from the run's own record (see [`super::proc`]), and the only text
//! that reaches it from a request is the graph name (a bare name from `--run-graphs`), the args (a JSON object on stdin, from a file) and the
//! answer (one `--answer=<text>` argument, 1 KiB, no control character). The chat tag is none of these: it is a label in `runs.json` and a
//! list's filter, and it never reaches eidolon.

use std::collections::BTreeMap;
use std::os::unix::fs::DirBuilderExt as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context as _, Result};
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use serde::de::{IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::serve::{body_of, json, problem, status, BoxBody, Ctx};
use crate::sessions;

/// The most runs `runs.json` keeps, and the most a listing shows.
pub const KEPT: usize = 500;
pub const LISTED: usize = 50;
/// A start's body: the args (64 KiB at most) and the room for `{"graph": ...}` around them.
const ARGS_MAX: usize = 64 * 1024;
const START_CAP: usize = ARGS_MAX + 4096;
/// A start's `chat` that is not a chat's id (the shape `sessions::id_ok` takes).
const CHAT_SHAPE: &str = "chat is a chat's id: a letter or digit, then letters, digits, '.', '_' or '-', 128 bytes at most";
/// An answer: 1 KiB of text and its JSON.
pub const ANSWER_MAX: usize = 1024;
const ANSWER_CAP: usize = 8 * 1024;
/// The words kept of what a run said, so `runs.json` stays small: a menu of 64 options is about 3 KiB.
const KEEP_TEXT: usize = 16 * 1024;
/// Small args are kept on the row, for a list that says what each run was asked.
const KEEP_ARGS: usize = 2048;
/// How long a cancel answers after it signals before it says the child is still going.
const CANCEL_WAIT: std::time::Duration = std::time::Duration::from_millis(super::proc::GRACE.as_millis() as u64 + 4000);

/// Where a run is. Everything but `Running` and `Parked` is an end.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Running,
    Parked,
    Done,
    Failed,
    Cancelled,
    Abandoned,
    Refused,
    Lost,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Running => "running",
            Phase::Parked => "parked",
            Phase::Done => "done",
            Phase::Failed => "failed",
            Phase::Cancelled => "cancelled",
            Phase::Abandoned => "abandoned",
            Phase::Refused => "refused",
            Phase::Lost => "lost",
        }
    }

    /// Holds the one shared browser: running or parked.
    pub fn holds(self) -> bool {
        matches!(self, Phase::Running | Phase::Parked)
    }
}

/// Who is stopping a run's child, so the end is told truthfully whatever eidolon says on the way out (F0: SIGTERM races to `failed`
/// or to `parked`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stop {
    Cancel,
    Wall,
}

/// What `runs.json` keeps of one run. The id is the map key.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub graph: String,
    pub state: Phase,
    pub started_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_ms: Option<u64>,
    /// The live child, its own process group leader. `None` when no child is alive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// When the live child started (the wall clock's start), and how long `out.log` was by then: the door's line is the last one past it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_ms: Option<u64>,
    #[serde(default)]
    pub out_off: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Stop>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<i32>,
    /// The session eidolon made for the run (a file in the run's dir) and the run's number in it, from the door's line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eid_run: Option<u64>,
    #[serde(default)]
    pub resumes: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
    /// The chat the page had open when the run was started: a label, checked when it is written and when it is read, and never sent to
    /// eidolon (see the README's "The chat tag").
    #[serde(default, deserialize_with = "tag_or_none", skip_serializing_if = "Option::is_none")]
    pub chat: Option<String>,
}

impl Run {
    fn new(graph: &str, args: Option<Value>, chat: Option<String>) -> Self {
        Self {
            graph: graph.to_string(),
            state: Phase::Running,
            started_ms: crate::archive::now_ms(),
            ended_ms: None,
            pid: None,
            child_ms: None,
            out_off: 0,
            stop: None,
            exit: None,
            session: None,
            eid_run: None,
            resumes: 0,
            why: None,
            question: None,
            report: None,
            refused: None,
            args,
            chat,
        }
    }
}

/// A chat that is not a string is no tag, of any JSON type and any depth. One odd tag in a hand-edited runs.json must not set every
/// run aside: serde refuses the whole map over one bad field. The nested value is skipped with `IgnoredAny`, which is not depth-limited
/// (a `Value` is, at serde_json's 128 levels).
fn tag_or_none<'de, D: Deserializer<'de>>(de: D) -> std::result::Result<Option<String>, D::Error> {
    struct Tag;

    impl<'de> Visitor<'de> for Tag {
        type Value = Option<String>;

        fn expecting(&self, out: &mut std::fmt::Formatter) -> std::fmt::Result {
            out.write_str("a chat tag")
        }

        fn visit_str<E>(self, text: &str) -> std::result::Result<Self::Value, E> {
            Ok(Some(text.to_owned()))
        }

        fn visit_bool<E>(self, _: bool) -> std::result::Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_i64<E>(self, _: i64) -> std::result::Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_u64<E>(self, _: u64) -> std::result::Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_f64<E>(self, _: f64) -> std::result::Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> std::result::Result<Self::Value, A::Error> {
            while seq.next_element::<IgnoredAny>()?.is_some() {}
            Ok(None)
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> std::result::Result<Self::Value, A::Error> {
            while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
            Ok(None)
        }
    }

    de.deserialize_any(Tag)
}

/// `r<digits>`, nine at most: the only shape of a run id, and so the only thing a request can join to a path.
pub fn rid_ok(rid: &str) -> bool {
    rid.len() <= 10 && rid.strip_prefix('r').is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

fn rid_number(rid: &str) -> u64 {
    rid.strip_prefix('r').and_then(|n| n.parse().ok()).unwrap_or(0)
}

/// The text of an answer, or the sentence for why it is not one. Nothing else reaches eidolon's argv from a request.
pub fn answer_ok(text: &str) -> std::result::Result<(), &'static str> {
    if text.trim().is_empty() {
        return Err("write an answer: the number of an option, its exact label, or stop");
    }
    if text.len() > ANSWER_MAX {
        return Err("an answer is 1 KiB at most");
    }
    if text.chars().any(char::is_control) {
        return Err("an answer has no control characters");
    }
    Ok(())
}

/// `text` cut to at most `max` bytes on a character boundary.
fn clip(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// A refusal with a sentence a person can act on, under both names the callers read (`error` as every route has it, `why` as the spec
/// of this one does) and whatever else the status carries (`run`).
pub(crate) fn refuse(code: StatusCode, sentence: &str, extra: Value) -> Response<BoxBody> {
    let mut body = serde_json::json!({ "error": sentence, "why": sentence });
    if let (Some(into), Value::Object(from)) = (body.as_object_mut(), extra) {
        into.extend(from);
    }
    json(code, &body)
}

// ---------------------------------------------------------------------------
// The store: <home>/runs.json
// ---------------------------------------------------------------------------

/// Why a run could not be begun.
pub enum Begin {
    /// This run is running or parked.
    Busy(String),
    Io(String),
}

/// Why a parked run could not be taken for a resume.
pub enum Resume {
    Missing,
    NotParked(Phase),
    Child,
}

/// `<home>/runs.json`, in memory. `Default` is how [`Ctx`] gets one before [`open`](Self::open) (the adopt hook) fills it.
#[derive(Default)]
pub struct Store {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    file: Option<PathBuf>,
    runs: BTreeMap<String, Run>,
}

fn write(file: &Option<PathBuf>, runs: &BTreeMap<String, Run>) -> Result<()> {
    let Some(file) = file else { return Ok(()) };
    let body = serde_json::to_vec_pretty(runs).context("serializing the runs")?;
    harnox::fs::write_atomic_0600(file, &body).with_context(|| format!("writing {}", file.display()))
}

impl Store {
    /// Reads `file` (absent is empty; one that does not parse is set aside, see [`crate::statefile`]).
    pub(crate) fn open(&self, file: PathBuf) {
        let runs = crate::statefile::load(Some(&file));
        let mut inner = self.inner.lock().unwrap();
        *inner = Inner { file: Some(file), runs };
    }

    pub(crate) fn get(&self, rid: &str) -> Option<Run> {
        self.inner.lock().unwrap().runs.get(rid).cloned()
    }

    /// Every run, newest first.
    pub(crate) fn all(&self) -> Vec<(String, Run)> {
        let mut all: Vec<(String, Run)> = self.inner.lock().unwrap().runs.iter().map(|(rid, run)| (rid.clone(), run.clone())).collect();
        all.sort_by_key(|(rid, _)| std::cmp::Reverse(rid_number(rid)));
        all
    }

    /// The run that holds the browser, if one does.
    pub(crate) fn holder(&self) -> Option<String> {
        self.inner.lock().unwrap().runs.iter().find(|(_, run)| run.state.holds()).map(|(rid, _)| rid.clone())
    }

    /// One change to one run. `change` runs under the lock and must not block; the memory takes it only if `runs.json` was written.
    /// `None` is a run that is not there.
    pub(crate) fn edit<T>(&self, rid: &str, change: impl FnOnce(&mut Run) -> T) -> Result<Option<T>> {
        let mut inner = self.inner.lock().unwrap();
        let Some(run) = inner.runs.get(rid) else { return Ok(None) };
        let mut next = run.clone();
        let out = change(&mut next);
        if next == *run {
            return Ok(Some(out));
        }
        let mut all = inner.runs.clone();
        all.insert(rid.to_string(), next);
        write(&inner.file, &all)?;
        inner.runs = all;
        Ok(Some(out))
    }

    /// The next run, if none holds the browser: its id and its directory (`<home>/runs/<rid>`, `0700`) made in the same step. The chat tag is
    /// in the same write as the row, so a run never exists without it.
    pub(crate) fn begin(&self, home: &Path, graph: &str, args: Option<Value>, chat: Option<String>) -> std::result::Result<String, Begin> {
        let mut inner = self.inner.lock().unwrap();
        if let Some((rid, _)) = inner.runs.iter().find(|(_, run)| run.state.holds()) {
            return Err(Begin::Busy(rid.clone()));
        }
        let runs_dir = home.join("runs");
        let io = |what: &str, e: std::io::Error| Begin::Io(format!("{what}: {e}"));
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&runs_dir).map_err(|e| io("the runs folder could not be made", e))?;
        // Past every id in the file and every folder there: a `runs.json` that was lost must not reuse a run's folder.
        let mut next = inner.runs.keys().map(|rid| rid_number(rid)).max().unwrap_or(0);
        for entry in std::fs::read_dir(&runs_dir).map_err(|e| io("the runs folder could not be read", e))?.flatten() {
            if let Some(name) = entry.file_name().to_str().filter(|name| rid_ok(name)) {
                next = next.max(rid_number(name));
            }
        }
        let rid = loop {
            next += 1;
            if next > 999_999_999 {
                return Err(Begin::Io("the run ids are used up".to_string()));
            }
            let rid = format!("r{next}");
            match std::fs::DirBuilder::new().mode(0o700).create(runs_dir.join(&rid)) {
                Ok(()) => break rid,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(io("the run's folder could not be made", e)),
            }
        };
        let mut all = inner.runs.clone();
        all.insert(rid.clone(), Run::new(graph, args, chat));
        while all.len() > KEPT {
            // The oldest run that is over, of any chat: never one that holds the browser.
            let Some(oldest) = all.iter().filter(|(_, run)| !run.state.holds()).map(|(rid, _)| rid.clone()).min_by_key(|rid| rid_number(rid)) else { break };
            all.remove(&oldest);
        }
        write(&inner.file, &all).map_err(|e| Begin::Io(format!("{e:#}")))?;
        inner.runs = all;
        Ok(rid)
    }

    /// Takes a parked run with no child for a resume: it is `running` from this step, so a second answer is a 409. Gives back the record
    /// as it was.
    pub(crate) fn resume(&self, rid: &str) -> std::result::Result<Run, Resume> {
        let mut inner = self.inner.lock().unwrap();
        let Some(run) = inner.runs.get(rid) else { return Err(Resume::Missing) };
        if run.state != Phase::Parked {
            return Err(Resume::NotParked(run.state));
        }
        if run.pid.is_some() {
            return Err(Resume::Child);
        }
        let before = run.clone();
        let mut next = before.clone();
        next.state = Phase::Running;
        next.resumes += 1;
        next.refused = None;
        next.exit = None;
        let mut all = inner.runs.clone();
        all.insert(rid.to_string(), next);
        if let Err(e) = write(&inner.file, &all) {
            tracing::warn!(error = %format!("{e:#}"), "perch: runs.json could not be written");
            return Err(Resume::Missing);
        }
        inner.runs = all;
        Ok(before)
    }
}

// ---------------------------------------------------------------------------
// What a run looks like to the page
// ---------------------------------------------------------------------------

fn row(rid: &str, run: &Run) -> Value {
    let mut out = serde_json::json!({ "rid": rid, "graph": run.graph, "state": run.state.as_str(), "started_ms": run.started_ms, "resumes": run.resumes });
    let map = out.as_object_mut().expect("a row is an object");
    if let Some(ended) = run.ended_ms {
        map.insert("ended_ms".into(), ended.into());
    }
    if let Some(exit) = run.exit {
        map.insert("exit".into(), exit.into());
    }
    if let Some(why) = &run.why {
        map.insert("why".into(), why.clone().into());
    }
    if let Some(args) = &run.args {
        map.insert("args".into(), args.clone());
    }
    // A tag a hand-edited runs.json carries that is not a chat's id is left off, so no page is handed a path or markup.
    if let Some(chat) = run.chat.as_deref().filter(|chat| sessions::id_ok(chat)) {
        map.insert("chat".into(), chat.into());
    }
    if run.stop.is_some() && run.state == Phase::Running {
        map.insert("stopping".into(), true.into());
    }
    out
}

/// The row and what the run last said, for one run.
fn detail(dir: &Path, rid: &str, run: &Run) -> Value {
    let mut out = row(rid, run);
    let map = out.as_object_mut().expect("a row is an object");
    for (key, value) in [("question", &run.question), ("report", &run.report), ("refused", &run.refused)] {
        if let Some(text) = value {
            map.insert(key.into(), text.clone().into());
        }
    }
    map.insert("out".into(), super::proc::tail(&dir.join("out.log")).into());
    map.insert("err".into(), super::proc::tail(&dir.join("err.log")).into());
    out
}

/// `<home>/runs/<rid>` for a run this perch knows. The id has been checked, and the folder is the perch's own.
pub fn dir_of(ctx: &Ctx, rid: &str) -> Option<PathBuf> {
    let home = ctx.registry.home()?;
    (rid_ok(rid) && ctx.runs.store.get(rid).is_some()).then(|| home.join("runs").join(rid))
}

/// The run's own config (R1b runs `eidolon --config <this> log --json <journal>`), for a run that exists.
pub fn config_of(ctx: &Ctx, rid: &str) -> Option<PathBuf> {
    dir_of(ctx, rid).map(|dir| dir.join("config.toml"))
}

/// The run's journal: the session eidolon made in the run's folder, once there is one.
pub fn journal_of(ctx: &Ctx, rid: &str) -> Option<PathBuf> {
    let dir = dir_of(ctx, rid)?;
    ctx.runs.store.get(rid)?.session.filter(|path| path.is_file()).or_else(|| super::proc::find_session(&dir))
}

// ---------------------------------------------------------------------------
// The routes
// ---------------------------------------------------------------------------

/// What a request below `/hub/runs` is, from its method and the rest of its path.
#[derive(Debug, PartialEq, Eq)]
enum Verb<'a> {
    List,
    Create,
    One(&'a str),
    Answer(&'a str),
    Cancel(&'a str),
    Abandon(&'a str),
    Nobody,
    NotAllowed,
}

fn verb<'a>(method: &Method, rest: &'a str) -> Verb<'a> {
    let (rid, tail) = match rest.split_once('/') {
        Some((rid, tail)) => (rid, Some(tail)),
        None => (rest, None),
    };
    if rest.is_empty() {
        return match *method {
            Method::GET => Verb::List,
            Method::POST => Verb::Create,
            _ => Verb::NotAllowed,
        };
    }
    if !rid_ok(rid) {
        return Verb::Nobody;
    }
    match tail {
        None if *method == Method::GET => Verb::One(rid),
        None => Verb::NotAllowed,
        Some(word @ ("answer" | "cancel" | "abandon")) if *method == Method::POST => match word {
            "answer" => Verb::Answer(rid),
            "cancel" => Verb::Cancel(rid),
            _ => Verb::Abandon(rid),
        },
        Some("answer" | "cancel" | "abandon") => Verb::NotAllowed,
        Some(_) => Verb::Nobody,
    }
}

/// Everything under `/hub/runs` that is not `health`, `graphs` or a log (see `runs::pick`). `rest` is `""`, `<rid>` or `<rid>/<verb>`.
pub async fn route(ctx: &Ctx, req: Request<Incoming>, rest: &str) -> Response<BoxBody> {
    match verb(req.method(), rest) {
        Verb::Nobody => status(StatusCode::NOT_FOUND),
        Verb::NotAllowed => status(StatusCode::METHOD_NOT_ALLOWED),
        Verb::List => list(ctx, req.uri().query()),
        Verb::Create => create(ctx, req).await,
        Verb::One(rid) => one(ctx, rid),
        Verb::Answer(rid) => answer(ctx, req, rid).await,
        Verb::Cancel(rid) => cancel(ctx, rid).await,
        Verb::Abandon(rid) => abandon(ctx, rid),
    }
}

/// `?chat=<id>`, the one query the list takes: the id as it is, with no percent-decoding (a `%` is not an id's character, so an encoded one
/// is refused like any other). No query, or an empty one, is no filter. Any other pair is a sentence, not a filter.
fn chat_query(query: Option<&str>) -> std::result::Result<Option<&str>, &'static str> {
    let Some(query) = query.filter(|query| !query.is_empty()) else { return Ok(None) };
    match query.split_once('=') {
        Some(("chat", chat)) if sessions::id_ok(chat) => Ok(Some(chat)),
        _ => Err("the only query is ?chat=<a chat's id>"),
    }
}

fn list(ctx: &Ctx, query: Option<&str>) -> Response<BoxBody> {
    let chat = match chat_query(query) {
        Ok(chat) => chat,
        Err(why) => return problem(StatusCode::BAD_REQUEST, why),
    };
    // The filter is before the cap, so one chat's strip keeps its own newest runs whatever the other chats did.
    let rows: Vec<Value> = ctx.runs.store.all().iter().filter(|(_, run)| chat.is_none_or(|chat| run.chat.as_deref() == Some(chat))).take(LISTED).map(|(rid, run)| row(rid, run)).collect();
    json(StatusCode::OK, &rows)
}

fn one(ctx: &Ctx, rid: &str) -> Response<BoxBody> {
    let (Some(dir), Some(run)) = (dir_of(ctx, rid), ctx.runs.store.get(rid)) else { return status(StatusCode::NOT_FOUND) };
    json(StatusCode::OK, &detail(&dir, rid, &run))
}

/// A JSON object body under `cap`, or the 400/413 to answer with.
async fn object(req: Request<Incoming>, cap: usize, shape: &str) -> std::result::Result<serde_json::Map<String, Value>, Box<Response<BoxBody>>> {
    let (_, bytes) = body_of(req, cap).await?;
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(Value::Object(map)) => Ok(map),
        _ => Err(Box::new(problem(StatusCode::BAD_REQUEST, shape))),
    }
}

async fn create(ctx: &Ctx, req: Request<Incoming>) -> Response<BoxBody> {
    let shape = "expected {\"graph\": \"<name>\", \"args\": {...}}";
    let body = match object(req, START_CAP, shape).await {
        Ok(body) => body,
        Err(refusal) => return *refusal,
    };
    let Some(graph) = body.get("graph").and_then(Value::as_str) else { return problem(StatusCode::BAD_REQUEST, shape) };
    let args = match body.get("args") {
        None => Value::Object(serde_json::Map::new()),
        Some(args @ Value::Object(_)) => args.clone(),
        Some(_) => return problem(StatusCode::BAD_REQUEST, "args must be a JSON object"),
    };
    // Compact, which is also what goes to eidolon on stdin.
    let args_text = args.to_string();
    if args_text.len() > ARGS_MAX {
        return problem(StatusCode::BAD_REQUEST, "args are 64 KiB at most");
    }
    // The chat the page had open: absent or null is no tag. A label only, never a path, an argument or an environment value.
    let chat = match body.get("chat") {
        None | Some(Value::Null) => None,
        Some(Value::String(chat)) if sessions::id_ok(chat) => Some(chat.clone()),
        Some(_) => return problem(StatusCode::BAD_REQUEST, CHAT_SHAPE),
    };
    // The name is a bare name from the operator's list before it is joined to any path.
    if !crate::graph_name_ok(graph) || !ctx.opts.run_graphs.iter().any(|offered| offered == graph) {
        return status(StatusCode::NOT_FOUND);
    }
    let Some(plugins) = ctx.opts.plugins_dir.as_deref() else {
        return refuse(StatusCode::SERVICE_UNAVAILABLE, "this perch has no --plugins-dir, so there are no graphs to run", Value::Null);
    };
    let workflow = plugins.join("jev").join("workflows").join(format!("{graph}.rn"));
    if !std::fs::metadata(&workflow).is_ok_and(|meta| meta.is_file()) {
        return status(StatusCode::NOT_FOUND);
    }
    let Some(home) = ctx.registry.home().map(Path::to_path_buf) else {
        return refuse(StatusCode::SERVICE_UNAVAILABLE, "this perch has no --home, so it has nowhere to keep runs", Value::Null);
    };
    // A chat the perch does not know is refused before the holder and the services are asked about, so a refused start makes nothing.
    if let Some(chat) = chat.clone() {
        let registry = ctx.registry.clone();
        match tokio::task::spawn_blocking(move || chat_known(&registry, &chat)).await {
            Ok(true) => {}
            Ok(false) => return refuse(StatusCode::NOT_FOUND, "no such chat", Value::Null),
            Err(_) => return problem(StatusCode::INTERNAL_SERVER_ERROR, "the chat could not be read"),
        }
    }
    // The cheap refusal first, and again where the run is made: between the two another start may have won.
    if let Some(rid) = ctx.runs.store.holder() {
        return busy(ctx, &rid);
    }
    if let Err(refusal) = super::health::gate(ctx).await {
        return *refusal;
    }
    let kept = (args_text.len() <= KEEP_ARGS).then_some(args);
    let rid = match ctx.runs.store.begin(&home, graph, kept, chat.clone()) {
        Ok(rid) => rid,
        Err(Begin::Busy(rid)) => return busy(ctx, &rid),
        Err(Begin::Io(why)) => {
            tracing::warn!(%why, "perch: a run could not be begun");
            return refuse(StatusCode::SERVICE_UNAVAILABLE, "that run could not be started: its folder could not be made", Value::Null);
        }
    };
    let started = super::proc::prepare(&home, &rid, args_text.as_bytes()).and_then(|stdin| super::proc::launch(ctx, &rid, super::proc::start_argv(&home, &rid, plugins, graph), Some(stdin)));
    if let Err(e) = started {
        tracing::warn!(error = %format!("{e:#}"), rid, "perch: a run could not be started");
        let why = "eidolon could not be started for that run".to_string();
        let _ = ctx.runs.store.edit(&rid, |run| {
            run.state = Phase::Failed;
            run.ended_ms = Some(crate::archive::now_ms());
            run.why = Some(why.clone());
        });
        return refuse(StatusCode::SERVICE_UNAVAILABLE, &why, Value::Null);
    }
    let mut answer = serde_json::json!({ "rid": rid, "state": "running" });
    if let Some(chat) = chat {
        answer["chat"] = chat.into();
    }
    json(StatusCode::ACCEPTED, &answer)
}

/// Whether `id` is a chat this perch knows, asked the way the operator's terminal asks (`term::routes::chat_folder`, its twin): the folder
/// it was started in is remembered (a door it holds, or `chat-cwds.json`), or the list has it. One shared `Registry::knows` for both is a
/// follow-up for the sessions and term owners.
fn chat_known(registry: &sessions::Registry, id: &str) -> bool {
    sessions::id_ok(id) && (registry.known_cwd(id).is_some() || registry.list().iter().any(|row| row.id == id))
}

/// The 409 of a start while a run holds the browser: the holder's id, and its chat when it has one, so the page can say which chat holds it.
fn busy(ctx: &Ctx, rid: &str) -> Response<BoxBody> {
    let mut extra = serde_json::json!({ "run": rid });
    if let Some(chat) = ctx.runs.store.get(rid).and_then(|run| run.chat).filter(|chat| sessions::id_ok(chat)) {
        extra["chat"] = chat.into();
    }
    refuse(
        StatusCode::CONFLICT,
        &format!("run {rid} is still going or parked, and there is one shared browser: finish it, cancel it or abandon it first"),
        extra,
    )
}

async fn answer(ctx: &Ctx, req: Request<Incoming>, rid: &str) -> Response<BoxBody> {
    let Some(dir) = dir_of(ctx, rid) else { return status(StatusCode::NOT_FOUND) };
    let shape = "expected {\"text\": \"<the number of an option, its exact label, or stop>\"}";
    let body = match object(req, ANSWER_CAP, shape).await {
        Ok(body) => body,
        Err(refusal) => return *refusal,
    };
    let Some(text) = body.get("text").and_then(Value::as_str) else { return problem(StatusCode::BAD_REQUEST, shape) };
    if let Err(why) = answer_ok(text) {
        return problem(StatusCode::BAD_REQUEST, why);
    }
    // Refused before a service is asked about: a run that is not waiting has nothing to resume, whatever jev is doing.
    match ctx.runs.store.get(rid) {
        Some(run) if run.state != Phase::Parked => return not_parked(run.state),
        Some(run) if run.pid.is_some() => return refuse(StatusCode::CONFLICT, "a process of that run is still going", Value::Null),
        Some(_) => {}
        None => return status(StatusCode::NOT_FOUND),
    }
    // A resume calls jev and the browser again, and a call that fails is journaled and can never be replayed: a run resumed against a
    // service that is down is a run lost, so it is refused here and stays parked.
    if let Err(refusal) = super::health::gate(ctx).await {
        return *refusal;
    }
    let before = match ctx.runs.store.resume(rid) {
        Ok(before) => before,
        Err(Resume::Missing) => return status(StatusCode::NOT_FOUND),
        Err(Resume::NotParked(state)) => return not_parked(state),
        Err(Resume::Child) => return refuse(StatusCode::CONFLICT, "a process of that run is still going", Value::Null),
    };
    let Some(plugins) = ctx.opts.plugins_dir.as_deref() else { return give_back(ctx, rid, &before, "this perch has no --plugins-dir") };
    let Some(session) = before.session.clone().filter(|path| path.is_file()).or_else(|| super::proc::find_session(&dir)) else {
        return give_back(ctx, rid, &before, "that run has no journal to resume");
    };
    let home = ctx.registry.home().map(Path::to_path_buf).unwrap_or_default();
    let argv = super::proc::resume_argv(&home, rid, plugins, &session, before.eid_run.unwrap_or(1), text);
    if let Err(e) = super::proc::launch(ctx, rid, argv, None) {
        tracing::warn!(error = %format!("{e:#}"), rid, "perch: a run could not be resumed");
        return give_back(ctx, rid, &before, "eidolon could not be started for that run");
    }
    json(StatusCode::ACCEPTED, &serde_json::json!({ "rid": rid, "state": "running" }))
}

fn not_parked(state: Phase) -> Response<BoxBody> {
    refuse(StatusCode::CONFLICT, &format!("that run is {}, not waiting for an answer", state.as_str()), serde_json::json!({ "state": state.as_str() }))
}

/// A resume that could not begin: the run is parked again, as it was, and the person is told why.
fn give_back(ctx: &Ctx, rid: &str, before: &Run, why: &str) -> Response<BoxBody> {
    let before = before.clone();
    let _ = ctx.runs.store.edit(rid, move |run| *run = before);
    refuse(StatusCode::SERVICE_UNAVAILABLE, why, Value::Null)
}

async fn cancel(ctx: &Ctx, rid: &str) -> Response<BoxBody> {
    let Some(dir) = dir_of(ctx, rid) else { return status(StatusCode::NOT_FOUND) };
    // Marked under the lock before anything is signalled, so the end is told as a cancel whatever eidolon says on the way out.
    let marked = ctx.runs.store.edit(rid, |run| match (run.state, run.pid) {
        (Phase::Running, Some(_)) => {
            let first = run.stop.is_none();
            run.stop.get_or_insert(Stop::Cancel);
            Ok(first)
        }
        (Phase::Running, None) => Err("that run is only starting; try again in a moment".to_string()),
        (Phase::Parked, _) => Err("a parked run has no process to cancel; abandon it".to_string()),
        (state, _) => Err(format!("that run is {} already", state.as_str())),
    });
    match marked {
        Ok(Some(Ok(first))) => {
            if first {
                super::proc::stop(ctx, rid);
            }
        }
        Ok(Some(Err(why))) => return refuse(StatusCode::CONFLICT, &why, Value::Null),
        Ok(None) => return status(StatusCode::NOT_FOUND),
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "perch: runs.json could not be written");
            return refuse(StatusCode::SERVICE_UNAVAILABLE, "that run could not be stopped: runs.json could not be written", Value::Null);
        }
    }
    let deadline = std::time::Instant::now() + CANCEL_WAIT;
    loop {
        match ctx.runs.store.get(rid) {
            Some(run) if run.state != Phase::Running => return json(StatusCode::OK, &detail(&dir, rid, &run)),
            Some(_) if std::time::Instant::now() < deadline => tokio::time::sleep(std::time::Duration::from_millis(25)).await,
            Some(run) => return json(StatusCode::ACCEPTED, &row(rid, &run)),
            None => return status(StatusCode::NOT_FOUND),
        }
    }
}

fn abandon(ctx: &Ctx, rid: &str) -> Response<BoxBody> {
    let Some(dir) = dir_of(ctx, rid) else { return status(StatusCode::NOT_FOUND) };
    let done = ctx.runs.store.edit(rid, |run| {
        if run.state == Phase::Parked && run.pid.is_none() {
            run.state = Phase::Abandoned;
            run.ended_ms = Some(crate::archive::now_ms());
            run.refused = None;
            Ok(())
        } else {
            Err(run.state)
        }
    });
    match done {
        Ok(Some(Ok(()))) => match ctx.runs.store.get(rid) {
            Some(run) => json(StatusCode::OK, &detail(&dir, rid, &run)),
            None => status(StatusCode::NOT_FOUND),
        },
        Ok(Some(Err(state))) => refuse(StatusCode::CONFLICT, &format!("only a parked run can be abandoned, and that run is {}", state.as_str()), serde_json::json!({ "state": state.as_str() })),
        Ok(None) => status(StatusCode::NOT_FOUND),
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "perch: runs.json could not be written");
            refuse(StatusCode::SERVICE_UNAVAILABLE, "that run could not be abandoned: runs.json could not be written", Value::Null)
        }
    }
}

/// What the door's line and the journal said, kept on the run when its child has ended. Only [`super::proc`] builds one.
pub(crate) fn kept(text: Option<&str>) -> Option<String> {
    text.filter(|text| !text.is_empty()).map(|text| clip(text, KEEP_TEXT))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_id_is_r_and_digits_and_nothing_a_path_can_use() {
        for ok in ["r1", "r12", "r999999999"] {
            assert!(rid_ok(ok), "{ok:?}");
        }
        for bad in ["", "r", "1", "R1", "r-1", "r1x", "r01a", "../x", "r1/..", "r1/log", ".", "r\n1", "r١", "r1234567890", "health", "graphs"] {
            assert!(!rid_ok(bad), "{bad:?}");
        }
    }

    #[test]
    fn an_answer_is_one_line_of_text_up_to_1_kib() {
        for ok in ["0", "-1", "stop", "stop: enough", "Egypt", "a label with spaces and é", &"x".repeat(ANSWER_MAX)] {
            assert_eq!(answer_ok(ok), Ok(()), "{ok:?}");
        }
        for bad in ["", "   ", "\n", "0\n", "a\tb", "a\0b", "a\u{1b}[31m", "a\u{7f}", "a\u{85}", &"x".repeat(ANSWER_MAX + 1)] {
            assert!(answer_ok(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_verb_is_the_method_and_the_rest() {
        use Method as M;
        assert_eq!(verb(&M::GET, ""), Verb::List);
        assert_eq!(verb(&M::POST, ""), Verb::Create);
        assert_eq!(verb(&M::GET, "r7"), Verb::One("r7"));
        assert_eq!(verb(&M::POST, "r7/answer"), Verb::Answer("r7"));
        assert_eq!(verb(&M::POST, "r7/cancel"), Verb::Cancel("r7"));
        assert_eq!(verb(&M::POST, "r7/abandon"), Verb::Abandon("r7"));
        assert_eq!(verb(&M::DELETE, ""), Verb::NotAllowed);
        assert_eq!(verb(&M::POST, "r7"), Verb::NotAllowed);
        assert_eq!(verb(&M::GET, "r7/cancel"), Verb::NotAllowed);
        for rest in ["r7/other", "r7/answer/x", "../x", "x", "r7/", "/r7", "r7/log/x"] {
            assert_eq!(verb(&M::POST, rest), Verb::Nobody, "{rest:?}");
        }
    }

    fn file(dir: &Path) -> PathBuf {
        dir.join("runs.json")
    }

    #[test]
    fn one_run_holds_the_browser_and_the_check_and_the_insert_are_one_step() {
        let home = tempfile::tempdir().unwrap();
        let store = Store::default();
        store.open(file(home.path()));
        let first = store.begin(home.path(), "wiki-hop", None, None).ok().unwrap();
        assert_eq!(first, "r1");
        assert!(home.path().join("runs/r1").is_dir(), "the folder is made with the id");
        assert!(matches!(store.begin(home.path(), "wiki-hop", None, None), Err(Begin::Busy(rid)) if rid == "r1"), "running holds it");
        store.edit("r1", |run| run.state = Phase::Parked).unwrap();
        assert!(matches!(store.begin(home.path(), "wiki-hop", None, None), Err(Begin::Busy(_))), "so does parked");
        store.edit("r1", |run| run.state = Phase::Abandoned).unwrap();
        assert_eq!(store.begin(home.path(), "wiki-hop", None, None).ok().unwrap(), "r2", "an end lets the next begin");
        assert_eq!(store.all().iter().map(|(rid, _)| rid.as_str()).collect::<Vec<_>>(), ["r2", "r1"], "newest first");
    }

    #[test]
    fn an_id_is_never_reused_even_when_runs_json_was_lost() {
        let home = tempfile::tempdir().unwrap();
        let store = Store::default();
        store.open(file(home.path()));
        store.begin(home.path(), "g", None, None).ok().unwrap();
        store.edit("r1", |run| run.state = Phase::Done).unwrap();
        std::fs::remove_file(file(home.path())).unwrap();
        let fresh = Store::default();
        fresh.open(file(home.path()));
        assert_eq!(fresh.begin(home.path(), "g", None, None).ok().unwrap(), "r2", "r1's folder is still there");
    }

    #[test]
    fn a_resume_takes_a_parked_run_once() {
        let home = tempfile::tempdir().unwrap();
        let store = Store::default();
        store.open(file(home.path()));
        let rid = store.begin(home.path(), "g", None, None).ok().unwrap();
        assert!(matches!(store.resume(&rid), Err(Resume::NotParked(Phase::Running))));
        store.edit(&rid, |run| run.state = Phase::Parked).unwrap();
        let before = store.resume(&rid).ok().unwrap();
        assert_eq!(before.state, Phase::Parked);
        assert_eq!(store.get(&rid).unwrap().state, Phase::Running);
        assert_eq!(store.get(&rid).unwrap().resumes, 1);
        assert!(matches!(store.resume(&rid), Err(Resume::NotParked(Phase::Running))), "the second answer finds it running");
        assert!(matches!(store.resume("r99"), Err(Resume::Missing)));
    }

    #[test]
    fn runs_json_is_0600_and_a_change_that_cannot_be_written_is_not_made() {
        use std::os::unix::fs::PermissionsExt as _;
        let home = tempfile::tempdir().unwrap();
        let store = Store::default();
        store.open(file(home.path()));
        let rid = store.begin(home.path(), "g", None, None).ok().unwrap();
        assert_eq!(std::fs::metadata(file(home.path())).unwrap().permissions().mode() & 0o777, 0o600);
        // A directory where the file goes: the rename over it fails, and the memory keeps what it had.
        std::fs::remove_file(file(home.path())).unwrap();
        std::fs::create_dir(file(home.path())).unwrap();
        assert!(store.edit(&rid, |run| run.state = Phase::Done).is_err());
        assert_eq!(store.get(&rid).unwrap().state, Phase::Running);
    }

    #[test]
    fn the_file_keeps_at_most_the_newest_runs_and_never_one_that_holds_the_browser() {
        let home = tempfile::tempdir().unwrap();
        let store = Store::default();
        store.open(file(home.path()));
        for _ in 0..KEPT + 3 {
            let rid = store.begin(home.path(), "g", None, None).ok().unwrap();
            store.edit(&rid, |run| run.state = Phase::Done).unwrap();
        }
        let all = store.all();
        assert_eq!(all.len(), KEPT);
        assert_eq!(all[0].0, format!("r{}", KEPT + 3));
        assert!(store.get("r1").is_none() && store.get("r4").is_some());
        assert!(home.path().join("runs/r1").is_dir(), "a folder is never removed");
    }

    #[test]
    fn what_a_run_says_is_cut_on_a_character() {
        let long = "é".repeat(KEEP_TEXT);
        let cut = kept(Some(&long)).unwrap();
        assert!(cut.len() <= KEEP_TEXT && long.starts_with(&cut));
        assert_eq!(kept(Some("")), None);
        assert_eq!(kept(None), None);
    }

    #[test]
    fn a_row_says_stopping_only_while_a_stop_is_pending() {
        let mut run = Run::new("g", None, None);
        assert!(row("r1", &run).get("stopping").is_none());
        run.stop = Some(Stop::Cancel);
        assert_eq!(row("r1", &run)["stopping"], Value::Bool(true));
        run.state = Phase::Cancelled;
        assert!(row("r1", &run).get("stopping").is_none(), "an end is not stopping");
        assert_eq!(row("r1", &run)["state"], "cancelled");
    }

    #[test]
    fn the_list_takes_one_chat_or_no_query() {
        assert_eq!(chat_query(None), Ok(None));
        assert_eq!(chat_query(Some("")), Ok(None));
        assert_eq!(chat_query(Some("chat=aa")), Ok(Some("aa")));
        assert_eq!(chat_query(Some("chat=c0ffee01")), Ok(Some("c0ffee01")));
        let longest = format!("chat={}", "a".repeat(128));
        assert_eq!(chat_query(Some(longest.as_str())), Ok(Some(&longest[5..])));
        let too_long = format!("chat={}", "a".repeat(129));
        for bad in ["chat=", "chat=a&chat=b", "x=1", "limit=5", "chat=a&x=1", "chat=a%2Fb", "chat=..", "chat=a=b", "chat", "chat=a b", "chat=a/b", "chat=%2e", too_long.as_str()] {
            assert!(chat_query(Some(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_row_carries_the_chat_only_when_it_is_a_chats_id() {
        let mut run = Run::new("g", None, Some("c0ffee01".into()));
        assert_eq!(row("r1", &run)["chat"], "c0ffee01");
        let long = "x".repeat(129);
        for bad in ["../etc", "a b", "<b>x</b>", "", long.as_str()] {
            run.chat = Some(bad.to_string());
            assert!(row("r1", &run).get("chat").is_none(), "{bad:?}");
        }
        run.chat = None;
        assert!(row("r1", &run).get("chat").is_none());
    }

    #[test]
    fn a_chat_is_in_the_same_write_as_the_new_run_and_an_edit_keeps_it() {
        let home = tempfile::tempdir().unwrap();
        let store = Store::default();
        store.open(file(home.path()));
        let rid = store.begin(home.path(), "g", None, Some("c0ffee01".into())).ok().unwrap();
        let saved: BTreeMap<String, Run> = serde_json::from_str(&std::fs::read_to_string(file(home.path())).unwrap()).unwrap();
        assert_eq!(saved[&rid].chat.as_deref(), Some("c0ffee01"), "on disk straight after begin");
        store.edit(&rid, |run| run.state = Phase::Done).unwrap();
        assert_eq!(store.get(&rid).unwrap().chat.as_deref(), Some("c0ffee01"), "an edit leaves the tag as it was");
    }

    #[test]
    fn a_chat_that_is_not_a_string_is_no_tag_and_the_file_still_loads() {
        let home = tempfile::tempdir().unwrap();
        let text = r#"{"r1":{"graph":"g","state":"done","started_ms":1,"chat":7},"r2":{"graph":"g","state":"done","started_ms":2,"chat":{"x":1}},"r3":{"graph":"g","state":"parked","started_ms":3,"chat":[1]},"r4":{"graph":"g","state":"done","started_ms":4,"chat":true},"r5":{"graph":"g","state":"done","started_ms":5,"chat":null},"r6":{"graph":"g","state":"done","started_ms":6,"chat":"ok1"}}"#;
        std::fs::write(file(home.path()), text).unwrap();
        // The load a perch makes when it opens runs.json: one odd tag must not set the whole file aside.
        let store = Store::default();
        store.open(file(home.path()));
        assert!(file(home.path()).exists(), "runs.json is where it was, not set aside");
        let runs: BTreeMap<String, Run> = store.all().into_iter().collect();
        assert_eq!(runs.len(), 6, "one odd tag does not set every run aside");
        assert_eq!(runs["r3"].state, Phase::Parked, "a parked run is still parked");
        for rid in ["r1", "r2", "r3", "r4", "r5"] {
            assert!(runs[rid].chat.is_none(), "{rid}: a tag that is not a string is no tag");
        }
        assert_eq!(runs["r6"].chat.as_deref(), Some("ok1"));
    }

    #[test]
    fn a_chat_nested_past_the_depth_limit_is_no_tag_and_the_file_still_loads() {
        let home = tempfile::tempdir().unwrap();
        // Two hundred levels: a Value stops at serde_json's 128, but an ignored key of this depth always loaded.
        let deep = "[".repeat(200) + &"]".repeat(200);
        let text = format!(r#"{{"r1":{{"graph":"g","state":"done","started_ms":1,"chat":{deep}}},"r2":{{"graph":"g","state":"done","started_ms":2,"chat":"ok1"}}}}"#);
        std::fs::write(file(home.path()), text).unwrap();
        let store = Store::default();
        store.open(file(home.path()));
        assert!(file(home.path()).exists(), "runs.json is where it was, not set aside");
        let runs: BTreeMap<String, Run> = store.all().into_iter().collect();
        assert_eq!(runs.len(), 2, "a deep tag does not set every run aside");
        assert!(runs["r1"].chat.is_none(), "a deep tag is no tag");
        assert_eq!(runs["r2"].chat.as_deref(), Some("ok1"));
    }
}
