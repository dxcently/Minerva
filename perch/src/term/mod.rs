//! `/hub/term`: web terminals the perch hosts (slices T1a, T1b, T1c, T2).
//!
//! The perch owns the PTYs and a screen copy of each; the page draws them and an agent's `term_*` tools drive them. The routes are
//! split by who fills them:
//!
//! ```text
//!   GET  /hub/term/events        stream::events   the one SSE every terminal rides                          (T1c)
//!   POST /hub/term/a/<verb>      agent::route     list, open, read, send, close, whoami: scoped token         (T2)
//!   everything else              routes::route    GET|POST /hub/term, /<t>/input|resize|hold|close|text       (T1b)
//! ```
//!
//! `pty` (the PTY, its reader and writer threads, close and shutdown), `screen` (the alacritty copy) and `snap` (the join snapshot)
//! are the host and have no route. [`pick`] is the whole dispatch and is a plain function so a test can reach it. A terminal id is
//! `t<n>`, so `events` and `a` are never ids, which is what lets the two words be matched ahead of the catch-all.
//!
//! **An agent path is routed as one only if the gate took it as one.** `serve` asks [`crate::agent::agent_scope`] before the master
//! check, and [`pick`] asks the same predicate ([`crate::agent::verb`]); `a` and `a/...` in any other shape are a 404 here, so an
//! operator's master token never reaches an agent handler by a spelling the gate did not recognise.
//!
//! Every handler has one signature, `(ctx, req, rest) -> Response`, where `rest` is the part of the path below the handler's own
//! prefix: `""` for `events`, the verb for `agent`, the whole rest for `routes`.
//!
//! **Blocking.** Every call below that reads or changes a screen takes that screen's lock, and the lock is held by the thread that
//! parses the program's output, a few milliseconds at a time: [`Terms::text`], [`Terms::resize`] and the `Terminal` methods behind
//! them block for that long. They are for threads that can wait. A handler on the async runtime that calls one goes through
//! `tokio::task::spawn_blocking` (or accepts the stall), and the one path with a deadline, [`Terms::shutdown`], never takes the lock
//! in the blocking way. [`Terms::input`], [`Terms::agent_input`], [`Terms::set_hold`], [`Terms::info`], [`Terms::list`] and
//! [`Terms::open`] do not take a screen's lock.

pub mod agent;
pub mod pty;
pub mod routes;
pub mod screen;
pub mod snap;
pub mod stream;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, oneshot, watch};
use tokio_util::sync::CancellationToken;

use crate::serve::{status, BoxBody, Ctx};
use crate::spawn::Spawner;
use pty::StartArgs;
use screen::{clean_title, tail_bytes, Text};

pub use pty::{Read, Ring, Terminal};

// ---- the limits --------------------------------------------------------------------------------------------------------------

/// Terminals the perch holds at once, ended ones included (an ended terminal stays until it is closed or [`KEEP_ENDED_MS`] pass).
pub const MAX_TERMS: usize = 16;
/// How long an ended terminal is kept, shown, and readable: 30 minutes after it ended, across a perch restart too.
pub const KEEP_ENDED_MS: u64 = 30 * 60 * 1000;
/// What one `input` may carry.
pub const MAX_INPUT: usize = 64 * 1024;
/// Ids are `t<n>` with `n` up to six digits (`^t[0-9]{1,6}$`), so that `a` and `events` are never one.
const MAX_ID: u32 = 999_999;
/// How long closing a terminal waits for what is in its session to go before it is killed.
pub const CLOSE_GRACE: Duration = Duration::from_secs(2);
/// The same at shutdown, and the whole of it. `serve.rs` gives the hook 3 s; this ends in 2.5 s whatever the terminals do: a hang-up
/// of 1.7 s, up to 0.4 s for what was killed to be gone, and the wait itself is cut at 2.5 s.
pub const SHUTDOWN_GRACE: Duration = Duration::from_millis(1700);
pub const SHUTDOWN_BOUND: Duration = Duration::from_millis(2500);
/// How much earlier than [`SHUTDOWN_BOUND`] a terminal's record stops waiting for its screen, so that it comes back (without text, if
/// it must) before the shutdown gives up on it.
const RECORD_MARGIN: Duration = Duration::from_millis(150);
/// Agent terminals one chat may hold when the caller names no cap (D5). Ended ones that have not been closed count, and are let go
/// first when the chat wants another.
pub const AGENT_CHAT_CAP: usize = 4;
/// The most `terms.json` is read from: a file larger than this is not ours (the perch writes at most [`MAX_TERMS`] records of 64 KiB).
const MAX_FILE: u64 = 2 * 1024 * 1024;
/// How far in the future a record's end may be before it is taken for a lie and moved back to now.
const CLOCK_SLACK_MS: u64 = 60_000;
/// The characters of what an agent typed that an event carries.
const TYPED_TEXT: usize = 80;

// ---- the vocabulary ----------------------------------------------------------------------------------------------------------

/// Who opened a terminal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum By {
    #[default]
    Operator,
    Agent,
}

/// Who opened a terminal, and for which chat (an operator's terminal may be linked to the chat open beside it).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Owner {
    pub by: By,
    pub chat: Option<String>,
}

impl Owner {
    /// Whether this is an agent's terminal and `chat` is the chat that agent belongs to. An operator's terminal linked to a chat is not
    /// the agent's, whatever it sits beside.
    pub fn is_agents_of(&self, chat: &str) -> bool {
        self.by == By::Agent && self.chat.as_deref() == Some(chat)
    }
}

/// How a shell ended: a code, or the number of the signal that killed it. Neither when the perch could not tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exit {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// A terminal as `GET /hub/term` lists it (slice T1b serializes this as it is). Times are Unix milliseconds.
#[derive(Clone, Debug, Serialize)]
pub struct Info {
    pub id: String,
    pub title: String,
    /// The comm of what runs in the foreground (`bash`, `vim`); empty once the shell has ended.
    pub fg: String,
    pub by: By,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat: Option<String>,
    pub cwd: String,
    pub cols: u16,
    pub rows: u16,
    pub alive: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit: Option<Exit>,
    /// How many bytes the terminal has ever written: where a stream that has seen everything is.
    pub seq: u64,
    pub started_ms: u64,
    /// The last output, or the last input.
    pub last_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_last_ms: Option<u64>,
    pub hold: bool,
    /// Ended because the perch stopped (D3), and listed from `terms.json`: a page says "ended when the perch stopped".
    #[serde(skip_serializing_if = "is_false")]
    pub ended: bool,
}

/// What `terms.json` keeps of an ended terminal: enough to list it and to read its last screen. The file is read back as untrusted
/// ([`Record::vetted`]).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Record {
    pub id: String,
    pub title: String,
    pub cwd: String,
    pub cols: u16,
    pub rows: u16,
    pub by: By,
    pub chat: Option<String>,
    pub started_ms: u64,
    pub ended_ms: u64,
    pub exit: Option<Exit>,
    /// The shell was alive when the perch stopped, and the perch ended it.
    pub by_perch: bool,
    /// The last screen, at most 64 KiB.
    pub text: String,
}

/// What `terms.json` holds: the ended terminals, and the number the next id will have. `next` is the high-water mark of ids, kept apart
/// from the records so that closing or expiring a record never lowers it: an id is not handed out twice, whatever became of its
/// terminal, and a page that kept one by name does not find a stranger under it.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct File {
    next: u32,
    terms: Vec<Record>,
}

impl Record {
    /// The record as read from a file the perch wrote: ids must be canonical, the screen and the strings are cleaned and bounded as if
    /// someone else had written them (nothing that [`screen::shown`] refuses survives: controls, format characters, tags, separators),
    /// a time in the future is moved back to now, and a record whose time is up is nothing.
    fn vetted(mut self, now: u64) -> Option<Record> {
        id_number(&self.id)?;
        // An end in the future would keep the record for ever; a little ahead is a clock that differs, not a lie.
        if self.ended_ms > now.saturating_add(CLOCK_SLACK_MS) {
            self.ended_ms = now;
        }
        self.started_ms = self.started_ms.min(self.ended_ms);
        if now.saturating_sub(self.ended_ms) > KEEP_ENDED_MS {
            return None;
        }
        self.title = clean_title(&self.title);
        (self.cols, self.rows) = screen::clamp_size(Some(self.cols as u32), Some(self.rows as u32));
        self.cwd = self.cwd.chars().filter(|c| screen::shown(*c)).take(4096).collect();
        self.chat = self.chat.filter(|chat| chat.len() <= 128 && chat.chars().all(screen::shown));
        let text: String = self.text.chars().filter(|c| matches!(c, '\n' | '\t') || screen::shown(*c)).collect();
        self.text = tail_bytes(text, screen::SAVED_TEXT_BYTES);
        Some(self)
    }

    fn info(&self) -> Info {
        Info {
            id: self.id.clone(),
            title: self.title.clone(),
            fg: String::new(),
            by: self.by,
            chat: self.chat.clone(),
            cwd: self.cwd.clone(),
            cols: self.cols,
            rows: self.rows,
            alive: false,
            exit: self.exit,
            seq: 0,
            started_ms: self.started_ms,
            last_ms: self.ended_ms,
            agent_last_ms: None,
            hold: false,
            ended: self.by_perch,
        }
    }
}

/// `t<n>`: the number of a canonical terminal id, which has no leading zero and at most six digits.
fn id_number(id: &str) -> Option<u32> {
    let digits = id.strip_prefix('t')?;
    (digits.len() <= 6 && !digits.starts_with('0') && digits.bytes().all(|b| b.is_ascii_digit())).then(|| digits.parse().ok())?
}

/// Whether a path segment can be a terminal id: `^t[0-9]{1,6}$`.
pub fn valid_id(id: &str) -> bool {
    id.strip_prefix('t').is_some_and(|digits| (1..=6).contains(&digits.len()) && digits.bytes().all(|b| b.is_ascii_digit()))
}

/// Something that changed. A stream (T1c) waits on [`Terms::ticks`] for output and on [`Terms::subscribe`] for these; a receiver that
/// lags past the buffer has lost some and should list the terminals again and compare.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Open(String),
    Size(String),
    Title(String),
    Hold(String),
    Exit(String),
    Closed(String),
    /// What an agent typed (T1c's `note_input` sends it). `text` is already shortened and made printable.
    Input { id: String, chat: Option<String>, text: String },
}

/// What the terminals tell whoever listens: a counter that moves on any change (output coalesces on it), and the changes themselves.
pub struct Hub {
    tick: watch::Sender<u64>,
    changes: broadcast::Sender<Change>,
}

impl Hub {
    fn new() -> Arc<Hub> {
        Arc::new(Hub { tick: watch::channel(0).0, changes: broadcast::channel(256).0 })
    }

    /// Something was written, or changed.
    pub fn touch(&self) {
        self.tick.send_modify(|tick| *tick = tick.wrapping_add(1));
    }

    pub fn emit(&self, change: Change) {
        let _ = self.changes.send(change);
        self.touch();
    }
}

/// Why `input` did not go in. T1b maps `Ended` to 409, `TooBig` to 413 and `Full` to 503; T2 maps `Held` to a refusal that says the
/// operator has the terminal on hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputError {
    NotFound,
    TooBig,
    /// The shell has ended (or the terminal is an ended one from `terms.json`).
    Ended,
    /// The shell is not reading and the queue is full.
    Full,
    /// The operator's hold is on, and this was an agent's input (the operator's own is never held).
    Held,
}

/// An id that is not a live terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Missing {
    NotFound,
    /// It is an ended terminal from `terms.json`: it can be read and closed, nothing else.
    Ended,
}

/// Why `open` did not. T1b maps `Cap` and `ChatCap` to 409, `Cwd` to 422.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenError {
    /// [`MAX_TERMS`] are held already.
    Cap,
    /// The chat has as many agent terminals open as `chat_cap` allows ([`AGENT_CHAT_CAP`] when the caller names none).
    ChatCap,
    /// An agent's terminal is opened for a chat; this one named none.
    NoChat,
    /// The folder is not one.
    Cwd,
    /// The perch is stopping.
    Stopping,
    Spawn(String),
}

/// A terminal to open. `cwd` was checked by the caller (under a `--root`); `None` is `$HOME`.
#[derive(Default)]
pub struct OpenSpec {
    pub owner: Owner,
    pub cwd: Option<PathBuf>,
    pub cols: Option<u32>,
    pub rows: Option<u32>,
    pub title: Option<String>,
    /// For an agent: how many of this chat's terminals may be held, ended ones that have not been closed included ([`AGENT_CHAT_CAP`]
    /// when `None`); checked together with the cap under one lock, so two calls at once cannot both pass.
    pub chat_cap: Option<usize>,
}

/// What the terminals need of the perch, read once on first use.
#[derive(Clone)]
pub struct Host {
    pub spawner: Arc<Spawner>,
    pub stop: CancellationToken,
    /// Bytes of raw output each terminal's ring holds.
    pub ring: usize,
    /// `<home>/terms.json`; `None` without `--home`, and nothing is kept.
    pub state_file: Option<PathBuf>,
    /// The environment a shell starts from (the perch's own; [`pty::child_env`] adds and removes).
    pub env: Vec<(OsString, OsString)>,
}

impl Host {
    pub fn of(ctx: &Ctx) -> Host {
        Host {
            spawner: ctx.spawner.clone(),
            stop: ctx.stop.clone(),
            ring: ring_knob(&ctx.opts.knobs),
            state_file: ctx.registry.home().map(|home| home.join("terms.json")),
            env: std::env::vars_os().collect(),
        }
    }

    fn home(&self) -> PathBuf {
        self.env.iter().find(|(name, _)| name == "HOME").map_or_else(|| PathBuf::from("/"), |(_, home)| PathBuf::from(home))
    }
}

/// `--knob term_ring=N`: the ring's size, for a test that wants it small. Out of range is clamped; a word is ignored with a warning.
fn ring_knob(knobs: &crate::Knobs) -> usize {
    match knobs.get("term_ring").map(str::parse::<usize>) {
        None => pty::RING_BYTES,
        Some(Ok(bytes)) => bytes.clamp(pty::RING_MIN, pty::RING_MAX),
        Some(Err(_)) => {
            tracing::warn!("perch: --knob term_ring is not a number; the ring stays {} bytes", pty::RING_BYTES);
            pty::RING_BYTES
        }
    }
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

// ---- the terminals -----------------------------------------------------------------------------------------------------------

/// `ctx.term`: `Default` is how [`Ctx`] gets one before any hook runs, and the terminals themselves are made on first use, from the
/// context (the options, the home, the spawner). No terminal exists, and `terms.json` is not read, until something asks.
#[derive(Default)]
pub struct State {
    terms: OnceLock<Terms>,
}

impl State {
    /// The terminals, made on the first call.
    pub fn terms(&self, ctx: &Ctx) -> &Terms {
        self.terms.get_or_init(|| Terms::new(Host::of(ctx)))
    }

    /// The terminals, if anything has made them.
    pub fn get(&self) -> Option<&Terms> {
        self.terms.get()
    }
}

enum Entry {
    Live(Arc<Terminal>),
    /// Ended with the perch (or earlier), read back from `terms.json`.
    Ended(Record),
}

impl Entry {
    fn id(&self) -> &str {
        match self {
            Entry::Live(term) => term.id(),
            Entry::Ended(record) => &record.id,
        }
    }

    fn info(&self) -> Info {
        match self {
            Entry::Live(term) => term.info(),
            Entry::Ended(record) => record.info(),
        }
    }

    fn owner(&self) -> Owner {
        match self {
            Entry::Live(term) => term.owner().clone(),
            Entry::Ended(record) => Owner { by: record.by, chat: record.chat.clone() },
        }
    }

    /// When this one ended, if it has: a shell that ended on its own, or a record read back.
    fn ended_ms(&self) -> Option<u64> {
        match self {
            Entry::Live(term) => term.exited().map(|(at, _)| at),
            Entry::Ended(record) => Some(record.ended_ms),
        }
    }
}

#[derive(Default)]
struct Inner {
    entries: Vec<Entry>,
    /// Ids handed out whose shell is still being forked: they count against the caps.
    reserved: Vec<(String, Owner)>,
    /// The number the next id will have: the high-water mark. Only ever raised; `terms.json` keeps it.
    next: u32,
}

/// The terminals of a perch: at most [`MAX_TERMS`], each a shell on a PTY with a screen copy (see `pty`, `screen`). Ending is
/// [`Terms::close`] for one and [`Terms::shutdown`] for all.
pub struct Terms {
    host: Host,
    inner: Mutex<Inner>,
    /// Held across collecting what `terms.json` is to say and writing it, so two writers cannot hand each other's older view to the
    /// disk. Taken before `inner`, never while `inner` is held.
    file: Mutex<()>,
    hub: Arc<Hub>,
    stopping: AtomicBool,
}

/// A place in the table and an id, held while a shell is forked: given back if the open does not finish.
struct Slot<'a> {
    terms: &'a Terms,
    id: String,
    done: bool,
}

impl Slot<'_> {
    /// Put the started terminal in the table, unless the perch has begun to stop: shutdown takes the table's terminals once, so one
    /// that arrived after would be left running. That is decided under the table's lock, the lock shutdown takes them under. The new
    /// terminal is then hung up and the open is told the perch is stopping.
    async fn commit(mut self, term: Arc<Terminal>) -> Result<(), OpenError> {
        let refused = {
            let mut inner = lock(&self.terms.inner);
            inner.reserved.retain(|(id, _)| *id != self.id);
            self.done = true;
            let refused = self.terms.stopping.load(Ordering::Acquire);
            if !refused {
                inner.entries.push(Entry::Live(term.clone()));
            }
            refused
        };
        if refused {
            self.terms.hang(term, SHUTDOWN_GRACE).wait().await;
            return Err(OpenError::Stopping);
        }
        Ok(())
    }
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        if !self.done {
            lock(&self.terms.inner).reserved.retain(|(id, _)| *id != self.id);
        }
    }
}

/// A terminal being ended. Waiting for it is optional: `close` answers at once and the session goes in the background.
pub struct Hangup(Option<oneshot::Receiver<()>>);

impl Hangup {
    pub async fn wait(self) {
        if let Some(done) = self.0 {
            let _ = done.await;
        }
    }
}

impl Terms {
    /// The terminals of this perch. Reads `terms.json` (a file that is not a plain file, is over 2 MiB, or does not parse is set
    /// aside; records whose time is up are trimmed; set-aside copies that are old are deleted), so the page can still say "ended when
    /// the perch stopped" for what a previous perch ended. Ids continue from the file's high-water mark.
    pub fn new(host: Host) -> Terms {
        let now = now_ms();
        let file = match host.state_file.as_deref() {
            Some(path) => {
                prune_aside(path, now);
                load_file(path)
            }
            None => File::default(),
        };
        let read_count = file.terms.len();
        let mut kept: Vec<Record> = Vec::new();
        for record in file.terms.into_iter().filter_map(|record| record.vetted(now)) {
            if kept.len() < MAX_TERMS && !kept.iter().any(|k| k.id == record.id) {
                kept.push(record);
            }
        }
        // Ids are not reused across restarts, so a page that kept something by id does not find a stranger under it: the file's mark,
        // and past every id still in it, whichever is higher.
        let past_records = kept.iter().filter_map(|r| id_number(&r.id)).max().map_or(1, |n| n + 1);
        let next = file.next.min(MAX_ID + 1).max(past_records).max(1);
        let terms = Terms {
            host,
            inner: Mutex::new(Inner { entries: kept.into_iter().map(Entry::Ended).collect(), reserved: Vec::new(), next }),
            file: Mutex::new(()),
            hub: Hub::new(),
            stopping: AtomicBool::new(false),
        };
        if terms.inner_len() != read_count {
            terms.write_state(None);
        }
        terms
    }

    fn inner_len(&self) -> usize {
        lock(&self.inner).entries.len()
    }

    /// A counter that moves whenever any terminal wrote or changed. Wait on `changed()` and compare cursors: it coalesces, so a flood of
    /// output is one wake-up, not a queue.
    pub fn ticks(&self) -> watch::Receiver<u64> {
        self.hub.tick.subscribe()
    }

    /// The changes that are not output: a terminal opened, resized, retitled, held, ended, closed, or typed into by an agent.
    pub fn subscribe(&self) -> broadcast::Receiver<Change> {
        self.hub.changes.subscribe()
    }

    /// Tell the listeners something (T1c's `note_input`).
    pub fn emit(&self, change: Change) {
        self.hub.emit(change);
    }

    /// Every terminal, oldest first, ended ones included. Also where terminals whose time is up are let go.
    pub fn list(&self) -> Vec<Info> {
        self.prune();
        lock(&self.inner).entries.iter().map(Entry::info).collect()
    }

    pub fn info(&self, id: &str) -> Option<Info> {
        lock(&self.inner).entries.iter().find(|e| e.id() == id).map(Entry::info)
    }

    /// A terminal that is running or whose shell has ended on its own, but not one read back from `terms.json`.
    pub fn get(&self, id: &str) -> Option<Arc<Terminal>> {
        match lock(&self.inner).entries.iter().find(|e| e.id() == id)? {
            Entry::Live(term) => Some(term.clone()),
            Entry::Ended(_) => None,
        }
    }

    /// Open a terminal: a login shell in `spec.cwd` (else `$HOME`) on a new PTY, forked on the spawner thread.
    ///
    /// An agent's terminal (`spec.owner.by` is [`By::Agent`]) is for a chat: with none, `NoChat`. The chat holds at most `chat_cap`
    /// ([`AGENT_CHAT_CAP`] when `None`) of them, ended ones that were not closed counting; when it is full, or the table is, its own
    /// oldest ended terminal is closed to make room (and `Change::Closed` says so), else the open is refused.
    pub async fn open(&self, spec: OpenSpec) -> Result<String, OpenError> {
        if self.stopping.load(Ordering::Acquire) || self.host.stop.is_cancelled() {
            return Err(OpenError::Stopping);
        }
        if spec.owner.by == By::Agent && spec.owner.chat.is_none() {
            return Err(OpenError::NoChat);
        }
        self.prune();
        let (cols, rows) = screen::clamp_size(spec.cols, spec.rows);
        let cwd = spec.cwd.unwrap_or_else(|| self.host.home());
        if !cwd.is_dir() {
            return Err(OpenError::Cwd);
        }
        let (slot, evicted) = self.reserve(&spec.owner, spec.chat_cap)?;
        self.let_go(evicted);
        let env = pty::child_env(&self.host.env, &slot.id);
        let forked = {
            let cwd = cwd.clone();
            self.host.spawner.run(move || pty::spawn(cols, rows, &cwd, &env)).await
        };
        let spawned = match forked {
            Ok(Ok(spawned)) => spawned,
            Ok(Err(e)) | Err(e) => return Err(OpenError::Spawn(e.to_string())),
        };
        let title = spec.title.map(|title| clean_title(&title)).unwrap_or_default();
        let args = StartArgs { id: slot.id.clone(), owner: spec.owner, cwd, title, cols, rows, ring: self.host.ring };
        let term = Terminal::start(args, spawned, self.hub.clone()).map_err(|e| OpenError::Spawn(e.to_string()))?;
        let id = slot.id.clone();
        slot.commit(term).await?;
        self.hub.emit(Change::Open(id.clone()));
        Ok(id)
    }

    /// Take a place and an id, evicting the ended terminals of the same agent chat that stand in the way (they are returned, already
    /// out of the table, for [`Terms::let_go`]). The id counter is the high-water mark, and a raise of it is written to `terms.json`
    /// before this returns, so not even a crash hands the number out again.
    fn reserve(&self, owner: &Owner, chat_cap: Option<usize>) -> Result<(Slot<'_>, Vec<Entry>), OpenError> {
        let (id, evicted) = {
            let mut inner = lock(&self.inner);
            if inner.next > MAX_ID {
                return Err(OpenError::Cap);
            }
            // The agent's chat; an agent's terminal with none is `NoChat` (also checked by `open`, before anything is touched).
            let chat = match (owner.by, owner.chat.as_deref()) {
                (By::Agent, None) => return Err(OpenError::NoChat),
                (By::Agent, Some(chat)) => Some(chat),
                (By::Operator, _) => None,
            };
            // The chat's ended terminals, oldest first: the ones to let go of if the chat or the table is full.
            let mut ended: Vec<(u64, String)> = Vec::new();
            let (mut running, mut held) = (0usize, 0usize);
            if let Some(chat) = chat {
                for entry in &inner.entries {
                    if !entry.owner().is_agents_of(chat) {
                        continue;
                    }
                    held += 1;
                    match entry.ended_ms() {
                        Some(at) => ended.push((at, entry.id().to_string())),
                        None => running += 1,
                    }
                }
                running += inner.reserved.iter().filter(|(_, o)| o.is_agents_of(chat)).count();
                held += inner.reserved.iter().filter(|(_, o)| o.is_agents_of(chat)).count();
                ended.sort();
            }
            let total = inner.entries.len() + inner.reserved.len();
            let cap = chat_cap.unwrap_or(AGENT_CHAT_CAP);
            // What must go for the chat to hold one more, and for the table to: ended terminals of the chat, as many as it takes.
            let by_chat = if chat.is_some() { (held + 1).saturating_sub(cap) } else { 0 };
            let by_table = (total + 1).saturating_sub(MAX_TERMS);
            if chat.is_some() && running >= cap {
                return Err(OpenError::ChatCap);
            }
            let evict = by_chat.max(by_table);
            if evict > ended.len() {
                return Err(if by_table > ended.len() { OpenError::Cap } else { OpenError::ChatCap });
            }
            let mut evicted = Vec::new();
            for (_, gone) in ended.into_iter().take(evict) {
                if let Some(at) = inner.entries.iter().position(|e| e.id() == gone) {
                    evicted.push(inner.entries.remove(at));
                }
            }
            inner.next = inner.next.max(1);
            let id = format!("t{}", inner.next);
            inner.next += 1;
            inner.reserved.push((id.clone(), owner.clone()));
            (id, evicted)
        };
        // The mark is on disk before the id is used for anything. (Not under `inner`: the file's lock comes first.)
        self.write_state(None);
        Ok((Slot { terms: self, id, done: false }, evicted))
    }

    /// Finish what [`Terms::reserve`] let go of: say so, hang up the ones that still had a session, and keep the file true.
    fn let_go(&self, evicted: Vec<Entry>) {
        for entry in evicted {
            self.hub.emit(Change::Closed(entry.id().to_string()));
            if let Entry::Live(term) = entry {
                self.hang(term, CLOSE_GRACE);
            }
        }
    }

    /// Queue `bytes` for the shell of terminal `id`, as the operator's. All of it or none. The hold does not stop it.
    pub fn input(&self, id: &str, bytes: &[u8]) -> Result<(), InputError> {
        match self.find(id) {
            None => Err(InputError::NotFound),
            Some(Entry::Ended(_)) => Err(InputError::Ended),
            Some(Entry::Live(_)) if bytes.len() > MAX_INPUT => Err(InputError::TooBig),
            Some(Entry::Live(term)) => term.input(bytes),
        }
    }

    /// Queue `bytes` for the shell of terminal `id`, as an agent of `chat`: the one way an agent's input gets in. All of it or none;
    /// refused with [`InputError::Held`] while the operator's hold is on (read under the lock that queues the bytes, so nothing
    /// slips in behind a hold set a moment before). It stamps the terminal ("an agent typed just now") and announces
    /// `Change::Input` with what was typed, shortened and made printable. Whether `chat` may drive this terminal at all is for the
    /// route to decide before it calls (T2).
    pub fn agent_input(&self, id: &str, chat: &str, bytes: &[u8]) -> Result<(), InputError> {
        match self.find(id) {
            None => Err(InputError::NotFound),
            Some(Entry::Ended(_)) => Err(InputError::Ended),
            Some(Entry::Live(_)) if bytes.len() > MAX_INPUT => Err(InputError::TooBig),
            Some(Entry::Live(term)) => {
                term.agent_input(bytes)?;
                self.hub.emit(Change::Input { id: id.to_string(), chat: Some(chat.to_string()), text: typed_text(bytes) });
                Ok(())
            }
        }
    }

    /// Resize the PTY and the screen (clamped to 20-400 columns and 5-200 rows); the size now. **Blocking** (the screen's lock).
    pub fn resize(&self, id: &str, cols: u32, rows: u32) -> Result<(u16, u16), Missing> {
        Ok(self.live(id)?.resize(cols, rows))
    }

    /// Set the operator's hold on a terminal; whether it changed. While it is on an agent's input is refused. Turning it on also
    /// stops what an agent already had under way: its queued input is dropped, and what was written to the PTY and not yet read by
    /// the program is flushed (see `Terminal::set_hold` and the README for exactly what that is and is not).
    pub fn set_hold(&self, id: &str, hold: bool) -> Result<bool, Missing> {
        Ok(self.live(id)?.set_hold(hold))
    }

    /// The screen as text, of a running terminal, or the saved last screen of an ended one. **Blocking** for a running one (the
    /// screen's lock).
    pub fn text(&self, id: &str, lines: usize, max_chars: usize) -> Option<Text> {
        match self.find(id)? {
            Entry::Live(term) => Some(term.text(lines, max_chars)),
            Entry::Ended(record) => Some(screen::text_of_saved(&record.text, lines, max_chars)),
        }
    }

    fn live(&self, id: &str) -> Result<Arc<Terminal>, Missing> {
        match self.find(id) {
            None => Err(Missing::NotFound),
            Some(Entry::Ended(_)) => Err(Missing::Ended),
            Some(Entry::Live(term)) => Ok(term),
        }
    }

    fn find(&self, id: &str) -> Option<Entry> {
        let inner = lock(&self.inner);
        Some(match inner.entries.iter().find(|e| e.id() == id)? {
            Entry::Live(term) => Entry::Live(term.clone()),
            Entry::Ended(record) => Entry::Ended(record.clone()),
        })
    }

    /// Close a terminal: it is gone from the list now, and its whole session is hung up and swept (see `pty`) in the background. The
    /// returned [`Hangup`] can be waited for. `None` when there is no such terminal.
    pub fn close(&self, id: &str) -> Option<Hangup> {
        let entry = {
            let mut inner = lock(&self.inner);
            let at = inner.entries.iter().position(|e| e.id() == id)?;
            inner.entries.remove(at)
        };
        self.hub.emit(Change::Closed(id.to_string()));
        Some(match entry {
            Entry::Live(term) => self.hang(term, CLOSE_GRACE),
            Entry::Ended(_) => {
                self.write_state(None);
                Hangup(None)
            }
        })
    }

    /// End every terminal, concurrently, and be done within [`SHUTDOWN_BOUND`] whatever they do (D3: terminals end with the perch). No
    /// terminal can be opened after this begins, and one whose open was in flight is hung up when it lands (`Slot::commit`).
    ///
    /// Each terminal is hung up, and *then* its last screen is recorded (the reader has taken the last of what the session wrote by
    /// then), on a blocking task of its own that tries the screen's lock until the deadline rather than waiting for it: a screen whose
    /// lock cannot be had in time is recorded without its text, and a terminal that cannot be ended in time is recorded as it is. The
    /// file is written once, at the end, with every record and the id mark.
    pub async fn shutdown(&self) {
        self.stopping.store(true, Ordering::Release);
        let deadline = tokio::time::Instant::now() + SHUTDOWN_BOUND;
        let until = deadline.into_std() - RECORD_MARGIN;
        let entries = std::mem::take(&mut lock(&self.inner).entries);
        let mut records = Vec::new();
        let mut tasks = Vec::new();
        for entry in entries {
            match entry {
                Entry::Ended(record) => records.push(record),
                Entry::Live(term) => {
                    // Whether the perch ends it is decided before it does: a shell that is gone by itself ended on its own.
                    let by_perch = term.is_alive();
                    let work = {
                        let term = term.clone();
                        tokio::task::spawn_blocking(move || {
                            term.hangup(SHUTDOWN_GRACE);
                            term.record_until(by_perch, until)
                        })
                    };
                    tasks.push((work, term, by_perch));
                }
            }
        }
        for (work, term, by_perch) in tasks {
            match tokio::time::timeout_at(deadline, work).await {
                Ok(Ok(record)) => records.push(record),
                _ => {
                    tracing::warn!("perch: a terminal's session did not end within {SHUTDOWN_BOUND:?} at shutdown; leaving it");
                    records.push(term.record_bare(by_perch));
                }
            }
        }
        self.write_state(Some(records));
    }

    /// End a terminal's session on a thread of its own (it sleeps and reads `/proc`): the runtime's blocking pool, else a plain thread.
    fn hang(&self, term: Arc<Terminal>, grace: Duration) -> Hangup {
        let (done, wait) = oneshot::channel();
        let work = move || {
            term.hangup(grace);
            let _ = done.send(());
        };
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => drop(runtime.spawn_blocking(work)),
            Err(_) => drop(std::thread::spawn(work)),
        }
        Hangup(Some(wait))
    }

    /// Let go of the terminals whose shell ended more than [`KEEP_ENDED_MS`] ago (their session is swept and the shell reaped) and of
    /// the ended ones read back from `terms.json` whose time is up.
    fn prune(&self) {
        let now = now_ms();
        let mut stale: Vec<Arc<Terminal>> = Vec::new();
        let mut file_changed = false;
        lock(&self.inner).entries.retain(|entry| match entry {
            Entry::Live(term) => match term.exited() {
                Some((at, _)) if now.saturating_sub(at) > KEEP_ENDED_MS => {
                    stale.push(term.clone());
                    false
                }
                _ => true,
            },
            Entry::Ended(record) => {
                let keep = now.saturating_sub(record.ended_ms) <= KEEP_ENDED_MS;
                file_changed |= !keep;
                keep
            }
        });
        for term in stale {
            self.hub.emit(Change::Closed(term.id().to_string()));
            self.hang(term, CLOSE_GRACE);
        }
        if file_changed {
            self.write_state(None);
        }
    }

    /// Write `terms.json` (`0600`, atomically): `{"next": ..., "terms": [...]}`, the newest [`MAX_TERMS`] records and the id mark. The
    /// file is never deleted, only rewritten; with no records it says `"terms": []` and still keeps the mark. `records` is what to
    /// write (the shutdown's, whose terminals are no longer in the table); `None` is the ended terminals now in the table.
    ///
    /// One mutex is held from the moment the records are collected to the moment the file is renamed into place, so two writers (a
    /// close and an expiry, an open and a close) cannot hand the disk each other's older view. Not to be called with `inner` held.
    fn write_state(&self, records: Option<Vec<Record>>) {
        let Some(path) = &self.host.state_file else { return };
        let _writing = lock(&self.file);
        let (next, mut terms) = {
            let inner = lock(&self.inner);
            let terms = records.unwrap_or_else(|| {
                inner
                    .entries
                    .iter()
                    .filter_map(|entry| match entry {
                        Entry::Ended(record) => Some(record.clone()),
                        Entry::Live(_) => None,
                    })
                    .collect()
            });
            (inner.next, terms)
        };
        terms.sort_by_key(|record| std::cmp::Reverse(record.ended_ms));
        terms.truncate(MAX_TERMS);
        let written = serde_json::to_vec_pretty(&File { next, terms })
            .map_err(std::io::Error::other)
            .and_then(|body| harnox::fs::write_atomic_0600(path, &body));
        if let Err(e) = written {
            tracing::warn!(error = %e, "perch: terms.json could not be written");
        }
    }
}

/// `terms.json` as the previous perch (or someone else) left it. A file that is not a plain file (a symlink, a pipe, a folder), is
/// larger than [`MAX_FILE`], cannot be read or does not parse is set aside, never opened past what was checked, and the table starts
/// without it. Absent is empty.
fn load_file(path: &Path) -> File {
    use std::io::Read as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return File::default(),
        Err(e) => {
            set_aside(path, &format!("could not be examined: {e}"));
            return File::default();
        }
    };
    if !meta.file_type().is_file() {
        set_aside(path, "is not a plain file");
        return File::default();
    }
    if meta.len() > MAX_FILE {
        set_aside(path, &format!("is larger than {MAX_FILE} bytes"));
        return File::default();
    }
    // `O_NOFOLLOW`: a link put in the path after the check above is refused, not followed; the read stops one byte past the limit.
    let mut body = Vec::new();
    let read = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .and_then(|file| file.take(MAX_FILE + 1).read_to_end(&mut body));
    if let Err(e) = read {
        set_aside(path, &format!("could not be read: {e}"));
        return File::default();
    }
    if body.len() as u64 > MAX_FILE {
        set_aside(path, &format!("is larger than {MAX_FILE} bytes"));
        return File::default();
    }
    match serde_json::from_slice(&body) {
        Ok(file) => file,
        Err(e) => {
            set_aside(path, &format!("does not parse: {e}"));
            File::default()
        }
    }
}

/// `<file>.bad-<unix seconds>` (`-1`, `-2` on a clash), the name a set-aside copy gets.
const ASIDE: &str = ".bad-";

/// Rename `path` out of the way, to `<name>.bad-<unix seconds>`; where that is not possible the file stays and the warning says so
/// (the next write replaces it).
fn set_aside(path: &Path, why: &str) {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("terms.json");
    let secs = now_ms() / 1000;
    for n in 0u32..100 {
        let suffix = if n == 0 { format!("{name}{ASIDE}{secs}") } else { format!("{name}{ASIDE}{secs}-{n}") };
        let to = path.with_file_name(suffix);
        if std::fs::symlink_metadata(&to).is_ok() {
            continue;
        }
        match std::fs::rename(path, &to) {
            Ok(()) => tracing::warn!(path = %path.display(), kept_as = %to.display(), "perch: {name} {why}; it was set aside and the list starts empty"),
            Err(e) => tracing::warn!(path = %path.display(), error = %e, "perch: {name} {why}, and it could not be set aside; the next write replaces it"),
        }
        return;
    }
}

/// Delete the copies [`set_aside`] made that are older than [`KEEP_ENDED_MS`] (by the time in their names), so that a file that goes bad
/// over and over does not fill the folder. Plain files only, and only names of exactly that shape. Once, when the table is made.
fn prune_aside(path: &Path, now: u64) {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else { return };
    let prefix = format!("{name}{ASIDE}");
    let Ok(dir) = std::fs::read_dir(path.parent().unwrap_or_else(|| Path::new("."))) else { return };
    for entry in dir.flatten() {
        let file_name = entry.file_name();
        let Some(rest) = file_name.to_str().and_then(|n| n.strip_prefix(&prefix)) else { continue };
        let Some(secs) = rest.split('-').next().and_then(|s| s.parse::<u64>().ok()) else { continue };
        if rest.split('-').count() > 2 || now.saturating_sub(secs.saturating_mul(1000)) <= KEEP_ENDED_MS {
            continue;
        }
        if entry.metadata().is_ok_and(|m| m.file_type().is_file()) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// What an agent typed, for an event: printable, [`TYPED_TEXT`] characters at most. A control character is shown as its picture
/// (U+2400 on: `␍` for return, `␃` for ^C), what the screen would not show is left out.
fn typed_text(bytes: &[u8]) -> String {
    let mut text = String::new();
    let mut count = 0;
    for c in String::from_utf8_lossy(bytes).chars() {
        let shown = match c {
            '\u{7f}' => Some('\u{2421}'),
            c if (c as u32) < 0x20 => char::from_u32(0x2400 + c as u32),
            c if screen::shown(c) => Some(c),
            _ => None,
        };
        let Some(shown) = shown else { continue };
        if count == TYPED_TEXT {
            text.push('\u{2026}');
            break;
        }
        text.push(shown);
        count += 1;
    }
    text
}

impl Drop for Terms {
    /// Terminals still running when the table goes (a test that failed, say) are hung up hard, not left behind.
    fn drop(&mut self) {
        let entries = std::mem::take(&mut lock(&self.inner).entries);
        for entry in entries {
            if let Entry::Live(term) = entry {
                term.hangup(Duration::ZERO);
            }
        }
    }
}

/// Where a request under `/hub/term/` goes, and what it is told is its `rest`.
#[derive(Debug, PartialEq, Eq)]
enum Pick<'a> {
    Events,
    Agent(&'a str),
    Operator(&'a str),
    /// `a` or `a/...` that is not exactly `a/<verb>`: nobody's route.
    Nobody,
}

fn pick(rest: &str) -> Pick<'_> {
    if rest == "events" {
        return Pick::Events;
    }
    if let Some(verb) = crate::agent::verb(rest) {
        return Pick::Agent(verb);
    }
    if rest == "a" || rest.starts_with("a/") {
        return Pick::Nobody;
    }
    Pick::Operator(rest)
}

/// `/hub/term` and everything under it. `rest` is `""` for the bare path.
pub async fn route(ctx: &Ctx, req: Request<Incoming>, rest: &str) -> Response<BoxBody> {
    match pick(rest) {
        Pick::Events => stream::events(ctx, req, "").await,
        Pick::Agent(verb) => agent::route(ctx, req, verb).await,
        Pick::Operator(rest) => routes::route(ctx, req, rest).await,
        Pick::Nobody => status(StatusCode::NOT_FOUND),
    }
}

/// At shutdown, before the doors are stopped (serve.rs): end every terminal. Bounded by the host (2.5 s), not by the caller.
pub async fn shutdown(ctx: &Ctx) {
    pty::shutdown(ctx).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{agent_scope, Scope};
    use std::time::Instant;

    #[test]
    fn the_dispatch_names_one_handler_per_shape() {
        assert_eq!(pick("events"), Pick::Events);
        assert_eq!(pick("a/list"), Pick::Agent("list"));
        assert_eq!(pick("a/whoami"), Pick::Agent("whoami"));
        for rest in ["", "t1", "t1/input", "t12/text", "events/x", "eventsx", "t1/a/list", "ab/list"] {
            assert_eq!(pick(rest), Pick::Operator(rest), "{rest:?}");
        }
        for rest in ["a", "a/", "a/x/y", "a/List", "a/%6cist", "a/x.y"] {
            assert_eq!(pick(rest), Pick::Nobody, "{rest:?}");
        }
    }

    /// The gate and the dispatch cannot disagree: a path is an agent handler's exactly when `serve` took it through the scoped gate.
    #[test]
    fn a_path_is_an_agent_route_exactly_when_the_gate_took_it_as_one() {
        for path in [
            "/hub/term",
            "/hub/term/",
            "/hub/term/events",
            "/hub/term/a",
            "/hub/term/a/",
            "/hub/term/a/list",
            "/hub/term/a/x/y",
            "/hub/term/a/%6cist",
            "/hub/term/%61/list",
            "/hub/term/t1/input",
            "/hub/term/t1/a/list",
            "/hub/term//a/list",
            "/hub/term/a//list",
            "/hub/term///a/list",
        ] {
            let rest = crate::serve::below(path, "/hub/term").expect("every path in the table is in the namespace");
            let dispatched = matches!(pick(rest), Pick::Agent(_));
            assert_eq!(dispatched, agent_scope(path) == Some(Scope::Term), "{path}");
        }
    }

    #[test]
    fn only_an_agents_own_terminal_is_the_agents_of_its_chat() {
        let agent = Owner { by: By::Agent, chat: Some("c1".into()) };
        assert!(agent.is_agents_of("c1"));
        assert!(!agent.is_agents_of("c2"));
        assert!(!agent.is_agents_of(""));
        let linked = Owner { by: By::Operator, chat: Some("c1".into()) };
        assert!(!linked.is_agents_of("c1"), "an operator's terminal beside a chat is not the chat's agent's");
        assert!(!Owner { by: By::Agent, chat: None }.is_agents_of("c1"));
    }

    #[test]
    fn what_an_agent_typed_is_shown_short_and_printable() {
        assert_eq!(typed_text(b"ls -la\r"), "ls -la\u{240d}", "a return is its picture");
        assert_eq!(typed_text(b"\x03\x1b[A\x7f\t"), "\u{2403}\u{241b}[A\u{2421}\u{2409}");
        assert_eq!(typed_text("h\u{e9}llo \u{202e}x\u{200b}".as_bytes()), "h\u{e9}llo x", "what the screen would not show is left out");
        assert_eq!(typed_text(b"a\xffb"), "a\u{fffd}b", "bytes that are not text are marked, not dropped");
        let long = typed_text(&vec![b'x'; 5000]);
        assert_eq!(long.chars().count(), TYPED_TEXT + 1);
        assert!(long.ends_with('\u{2026}'));
        assert_eq!(typed_text(&[b'x'; TYPED_TEXT]).chars().count(), TYPED_TEXT, "exactly the limit is not cut");
        assert_eq!(typed_text(b""), "");
    }

    /// A record whose every string carries `c`, read back: no string keeps it.
    fn nothing_invisible_survives(c: char) {
        let now = now_ms();
        let record = Record {
            id: "t1".into(),
            title: format!("a{c}b"),
            cwd: format!("/x{c}y"),
            chat: Some(format!("c{c}h")),
            ended_ms: now,
            text: format!("l1{c}x\nl2\t{c}y"),
            ..Record::default()
        };
        let vetted = record.vetted(now).expect("a record that is only odd is still a record");
        assert_eq!(vetted.title, "ab", "U+{:04X} in a title", c as u32);
        assert_eq!(vetted.cwd, "/xy", "U+{:04X} in a folder", c as u32);
        assert_eq!(vetted.chat, None, "U+{:04X} in a chat id: the id is dropped, not repaired", c as u32);
        assert_eq!(vetted.text, "l1x\nl2\ty", "U+{:04X} in a saved screen (the newline and the tab stay)", c as u32);
    }

    macro_rules! invisible_classes {
        ($($name:ident: $($c:expr),+;)*) => {
            $(
                #[test]
                fn $name() {
                    for c in [$($c),+] {
                        nothing_invisible_survives(c);
                    }
                }
            )*
        };
    }

    invisible_classes! {
        tag_characters_are_not_kept_in_a_record: '\u{e0001}', '\u{e0020}', '\u{e0041}', '\u{e007f}';
        the_arabic_letter_mark_is_not_kept_in_a_record: '\u{61c}';
        the_word_joiner_and_the_invisible_operators_are_not_kept_in_a_record: '\u{2060}', '\u{2062}', '\u{2064}', '\u{206a}', '\u{206f}';
        variation_selectors_are_not_kept_in_a_record: '\u{fe00}', '\u{fe0f}';
        supplementary_variation_selectors_are_not_kept_in_a_record: '\u{e0100}', '\u{e01ef}';
        the_soft_hyphen_is_not_kept_in_a_record: '\u{ad}';
        the_mongolian_vowel_separator_is_not_kept_in_a_record: '\u{180e}';
        the_interlinear_annotation_marks_are_not_kept_in_a_record: '\u{fff9}', '\u{fffa}', '\u{fffb}';
        line_and_paragraph_separators_are_not_kept_in_a_record: '\u{2028}', '\u{2029}';
        zero_width_spaces_joiners_and_direction_marks_are_not_kept_in_a_record: '\u{200b}', '\u{200d}', '\u{200e}', '\u{202e}', '\u{feff}';
        controls_are_not_kept_in_a_record: '\u{1b}', '\u{7}', '\u{85}', '\u{9b}', '\u{7f}';
    }

    #[test]
    fn a_record_that_ends_in_the_future_is_moved_back_to_now_and_one_a_little_ahead_is_left() {
        let now = now_ms();
        let record = |ended_ms: u64, started_ms: u64| Record { id: "t4".into(), ended_ms, started_ms, ..Record::default() };
        let lie = record(now + 10 * 365 * 86_400_000, now + 11 * 365 * 86_400_000).vetted(now).unwrap();
        assert_eq!(lie.ended_ms, now, "ten years ahead is moved to now, so that it expires with everything else");
        assert_eq!(lie.started_ms, now, "and it did not start after it ended");
        let skew = record(now + CLOCK_SLACK_MS, now).vetted(now).unwrap();
        assert_eq!(skew.ended_ms, now + CLOCK_SLACK_MS, "a minute ahead is a clock that differs");
        let beyond = record(now + CLOCK_SLACK_MS + 1, now).vetted(now).unwrap();
        assert_eq!(beyond.ended_ms, now);
        assert!(record(now - KEEP_ENDED_MS - 1, 0).vetted(now).is_none(), "and a record whose time is up is nothing");
        assert!(record(now - KEEP_ENDED_MS, 0).vetted(now).is_some());
        assert!(Record { id: "t4".into(), ..Record::default() }.vetted(now).is_none(), "no end at all is the year 1970");
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    #[test]
    fn a_terms_file_over_two_mebibytes_is_set_aside_unread_and_one_under_it_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("terms.json");
        let pad = |n: usize| format!("{{\"next\": 7, \"terms\": [], \"pad\": \"{}\"}}", "x".repeat(n));

        std::fs::write(&path, pad(MAX_FILE as usize + 1)).unwrap();
        let file = load_file(&path);
        assert_eq!((file.next, file.terms.len()), (0, 0), "too big: not read");
        assert!(!path.exists());
        assert_eq!(names_in(dir.path()).len(), 1, "set aside, not deleted");
        assert!(names_in(dir.path())[0].starts_with("terms.json.bad-"));

        std::fs::write(&path, pad(MAX_FILE as usize - 100)).unwrap();
        assert_eq!(load_file(&path).next, 7, "just under the limit it is a file like any other");
        assert!(path.exists());
    }

    #[test]
    fn a_terms_file_that_is_a_link_or_a_pipe_is_set_aside_without_being_opened() {
        use std::os::unix::ffi::OsStrExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("terms.json");
        let target = dir.path().join("elsewhere.json");
        std::fs::write(&target, r#"{"next": 5, "terms": []}"#).unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert_eq!(load_file(&path).next, 0, "a link is not followed");
        assert!(std::fs::symlink_metadata(&path).is_err(), "the link itself was moved out of the way");
        assert!(target.exists(), "and what it pointed at was not touched");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), r#"{"next": 5, "terms": []}"#);

        // A pipe would block a reader for ever; it is set aside by its type before anything opens it.
        let fifo = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        let started = Instant::now();
        assert_eq!(load_file(&path).next, 0);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(std::fs::symlink_metadata(&path).is_err());

        // A folder in its place is the same.
        std::fs::create_dir(&path).unwrap();
        assert_eq!(load_file(&path).next, 0);
        assert!(std::fs::symlink_metadata(&path).is_err());
        let aside = names_in(dir.path()).into_iter().filter(|n| n.starts_with("terms.json.bad-")).count();
        assert_eq!(aside, 3, "each of the three kept under a name of its own: {:?}", names_in(dir.path()));
    }

    #[test]
    fn copies_set_aside_long_ago_are_deleted_when_the_table_is_made_and_nothing_else_is() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("terms.json");
        let now = now_ms();
        let old = (now - KEEP_ENDED_MS - 10_000) / 1000;
        let fresh = (now - 60_000) / 1000;
        let make = |name: String| std::fs::write(dir.path().join(name), "x").unwrap();
        make(format!("terms.json.bad-{old}"));
        make(format!("terms.json.bad-{old}-2"));
        make(format!("terms.json.bad-{fresh}"));
        make("terms.json.bad-notatime".into());
        make(format!("terms.json.bad-{old}-1-2"));
        make(format!("other.json.bad-{old}"));
        make("terms.json".into());
        std::fs::create_dir(dir.path().join(format!("terms.json.bad-{old}-9"))).unwrap();
        std::os::unix::fs::symlink(dir.path().join("terms.json"), dir.path().join(format!("terms.json.bad-{old}-7"))).unwrap();

        prune_aside(&path, now);
        let mut left = names_in(dir.path());
        left.retain(|n| n != "terms.json");
        let mut want = vec![
            format!("terms.json.bad-{fresh}"),
            "terms.json.bad-notatime".to_string(),
            format!("terms.json.bad-{old}-1-2"),
            format!("other.json.bad-{old}"),
            format!("terms.json.bad-{old}-9"),
            format!("terms.json.bad-{old}-7"),
        ];
        want.sort();
        assert_eq!(left, want, "only plain files named exactly that way and old enough go");
        assert!(dir.path().join("terms.json").exists(), "and never the file itself");
    }

    #[test]
    fn terms_json_is_an_object_with_the_mark_and_the_records_and_reads_back_whole() {
        let file = File {
            next: 9,
            terms: vec![Record { id: "t3".into(), ended_ms: 5, text: "x".into(), by_perch: true, ..Record::default() }],
        };
        let json = serde_json::to_value(&file).unwrap();
        assert_eq!(json["next"], 9);
        assert_eq!(json["terms"][0]["id"], "t3");
        assert!(json.as_object().unwrap().len() == 2, "{json}");
        let back: File = serde_json::from_value(json).unwrap();
        assert_eq!((back.next, back.terms.len()), (9, 1));
        let bare: File = serde_json::from_str("{}").unwrap();
        assert_eq!((bare.next, bare.terms.len()), (0, 0), "everything has a default");
        assert!(serde_json::from_str::<File>(r#"[{"id": "t1"}]"#).is_err(), "a bare list of records is not the format");
    }
}

/// The terminal host against a real `/bin/sh` on a real PTY. Each test makes its own `Terms` in its own temporary home, so none sees
/// another's terminals, files or ids. What a test types never contains the line it waits for (it is built by the shell from pieces,
/// or is computed), so the echo of the command cannot be mistaken for its output.
#[cfg(test)]
mod e2e {
    use super::*;
    use alacritty_terminal::grid::Dimensions as _;
    use std::os::unix::fs::PermissionsExt as _;
    use std::time::Instant;

    /// What a test waits for before it calls the terminal broken.
    const WAIT: Duration = Duration::from_secs(8);
    /// A stand-in for a credential the perch might one day have in its environment.
    const SENTINEL: &str = "sentinel-master-8c1f27d94b";

    fn host(home: &Path) -> Host {
        let env: Vec<(OsString, OsString)> = vec![
            ("SHELL".into(), "/bin/sh".into()),
            ("HOME".into(), home.into()),
            ("PATH".into(), std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into())),
            ("MINERVA_HOME".into(), home.join("Minerva").into()),
            ("MINERVA_TEST_TOKEN".into(), SENTINEL.into()),
        ];
        Host {
            spawner: Arc::new(Spawner::new().unwrap()),
            stop: CancellationToken::new(),
            ring: pty::RING_BYTES,
            state_file: Some(home.join("state").join("terms.json")),
            env,
        }
    }

    /// The processes (other than this one) whose working directory is under `root`: what a test's shells and their jobs are, since they
    /// all start there. The test's own temporary folder is the only place this looks.
    fn processes_under(root: &Path) -> Vec<i32> {
        let own = std::process::id() as i32;
        let Ok(dir) = std::fs::read_dir("/proc") else { return Vec::new() };
        dir.flatten()
            .filter_map(|entry| entry.file_name().to_str().and_then(|n| n.parse::<i32>().ok()))
            .filter(|pid| *pid != own)
            .filter(|pid| std::fs::read_link(format!("/proc/{pid}/cwd")).is_ok_and(|cwd| cwd.starts_with(root)))
            .filter(|pid| running(*pid))
            .collect()
    }

    /// A table of terminals in a temporary home that leaves nothing behind: dropped, it hangs up whatever the table still holds, and
    /// then kills any process that is still working in the home (a test that failed half-way, a job that got away), so that no test
    /// leaves a shell or a `sleep` running whatever it did.
    struct Rig {
        terms: std::mem::ManuallyDrop<Terms>,
        home: tempfile::TempDir,
        taken: bool,
    }

    impl Rig {
        fn new() -> Rig {
            Rig::tuned(|_| {})
        }

        fn tuned(tune: impl FnOnce(&mut Host)) -> Rig {
            let home = tempfile::tempdir().unwrap();
            let mut host = host(home.path());
            tune(&mut host);
            Rig { terms: std::mem::ManuallyDrop::new(Terms::new(host)), home, taken: false }
        }

        /// The table itself, to be dropped by the test. The rig's own helpers cannot be used after this.
        fn take_terms(&mut self) -> Terms {
            assert!(!self.taken);
            self.taken = true;
            unsafe { std::mem::ManuallyDrop::take(&mut self.terms) }
        }

        fn file(&self) -> PathBuf {
            self.home.path().join("state").join("terms.json")
        }

        /// What is still running in this rig's home.
        fn left_running(&self) -> Vec<i32> {
            processes_under(&std::fs::canonicalize(self.home.path()).unwrap())
        }

        async fn open(&self) -> String {
            self.terms.open(OpenSpec::default()).await.unwrap()
        }

        /// Type a line and press return.
        fn send(&self, id: &str, line: &str) {
            self.terms.input(id, format!("{line}\r").as_bytes()).unwrap();
        }

        /// The screen's lines, with the shell's prompts taken off the front of a line: the line discipline echoes what is typed before the
        /// shell has started, so the prompt it prints when it does is followed by the first output on the same row, and a command put in
        /// the background gives the prompt back before it prints anything.
        fn lines(&self, id: &str) -> Vec<String> {
            let text = self.terms.text(id, 400, 7000).map(|text| text.text).unwrap_or_default();
            let bare = |mut line: &str| {
                while let Some(rest) = line.strip_prefix("$ ").or_else(|| line.strip_prefix("# ")) {
                    line = rest;
                }
                line.to_string()
            };
            text.lines().map(bare).collect()
        }

        /// Wait for a screen line that is exactly `line`.
        async fn see(&self, id: &str, line: &str) {
            let end = Instant::now() + WAIT;
            while !self.lines(id).iter().any(|l| l == line) {
                assert!(Instant::now() < end, "no line {line:?} on the screen: {:#?}", self.lines(id));
                tokio::time::sleep(Duration::from_millis(15)).await;
            }
        }

        /// Wait for a screen line that starts with `prefix` and has something after it; what follows.
        async fn value(&self, id: &str, prefix: &str) -> String {
            let end = Instant::now() + WAIT;
            loop {
                let lines = self.lines(id);
                if let Some(rest) = lines.iter().find_map(|l| l.strip_prefix(prefix).filter(|rest| !rest.is_empty())) {
                    return rest.to_string();
                }
                assert!(Instant::now() < end, "no line starting {prefix:?} on the screen: {lines:#?}");
                tokio::time::sleep(Duration::from_millis(15)).await;
            }
        }

        /// Make the shell write `format` (a `printf` format) to its terminal, and say what the terminal wrote to the shell's stdin in the
        /// second after: the replies, if there were any.
        async fn probe(&self, id: &str, n: u32, format: &str) -> Vec<u8> {
            self.send(
                id,
                &format!(
                    "stty raw -echo min 0 time 1; printf '{format}'; sleep 1; x=$(dd bs=256 count=1 2>/dev/null | od -An -tx1 | tr -d ' \\n'); \
                     stty sane; printf 'R%s<%s>\\n' {n} \"$x\""
                ),
            );
            let hex = self.value(id, &format!("R{n}<")).await;
            unhex(hex.strip_suffix('>').unwrap_or_else(|| panic!("a probe line without its end: {hex:?}")))
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            if !self.taken {
                unsafe { std::mem::ManuallyDrop::drop(&mut self.terms) };
            }
            let root = std::fs::canonicalize(self.home.path()).unwrap_or_else(|_| self.home.path().to_path_buf());
            for _ in 0..3 {
                let stragglers = processes_under(&root);
                if stragglers.is_empty() {
                    return;
                }
                for pid in stragglers {
                    unsafe { libc::kill(pid, libc::SIGKILL) };
                }
                std::thread::sleep(Duration::from_millis(30));
            }
        }
    }

    fn unhex(hex: &str) -> Vec<u8> {
        (0..hex.len() / 2).map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap_or_else(|_| panic!("not hex: {hex:?}"))).collect()
    }

    /// Whether `pid` can still run: a zombie is dead, and what is gone has no `/proc` entry.
    fn running(pid: i32) -> bool {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok();
        stat.and_then(|s| s.rsplit_once(')').and_then(|(_, rest)| rest.trim_start().chars().next())).is_some_and(|state| !matches!(state, 'Z' | 'X'))
    }

    async fn until(what: &str, mut ok: impl FnMut() -> bool) {
        let end = Instant::now() + WAIT;
        while !ok() {
            assert!(Instant::now() < end, "gave up waiting for {what}");
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
    }

    fn pid_of(value: &str) -> i32 {
        value.trim().parse().unwrap_or_else(|_| panic!("not a pid: {value:?}"))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_shell_runs_what_is_typed_and_the_terminal_says_what_it_is() {
        let rig = Rig::new();
        let id = rig.open().await;
        assert_eq!(id, "t1");
        rig.send(&id, "echo hi; echo T=$((20+22)); echo D=$(pwd)");
        assert_eq!(rig.value(&id, "T=").await, "42", "the shell ran it: the 42 is computed, the typed line has none");
        rig.see(&id, "hi").await;
        let home = std::fs::canonicalize(rig.home.path()).unwrap();
        assert_eq!(rig.value(&id, "D=").await, home.display().to_string(), "it starts in the folder it was given (here $HOME)");

        let info = rig.terms.info(&id).unwrap();
        assert_eq!((info.by, info.chat.as_deref(), info.cols, info.rows), (By::Operator, None, screen::DEFAULT_COLS, screen::DEFAULT_ROWS));
        assert!(info.alive && info.exit.is_none() && !info.hold && !info.ended && info.agent_last_ms.is_none());
        assert!(info.seq > 0 && info.started_ms <= info.last_ms && !info.fg.is_empty(), "{info:?}");
        assert_eq!(info.cwd, rig.home.path().display().to_string());
        assert_eq!(rig.terms.list().len(), 1);
        rig.terms.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_resize_reaches_the_shell_and_the_screen_and_is_clamped() {
        let rig = Rig::new();
        let id = rig.open().await;
        let mut changes = rig.terms.subscribe();
        assert_eq!(rig.terms.resize(&id, 40, 10), Ok((40, 10)));
        assert_eq!(changes.try_recv(), Ok(Change::Size(id.clone())));
        assert_eq!(rig.terms.resize(&id, 40, 10), Ok((40, 10)));
        assert!(changes.try_recv().is_err(), "the same size again is not a change");
        rig.send(&id, "stty size");
        rig.see(&id, "10 40").await;
        let info = rig.terms.info(&id).unwrap();
        assert_eq!((info.cols, info.rows), (40, 10));
        assert_eq!(rig.terms.get(&id).unwrap().view(|emu, _| (emu.term.columns(), emu.term.screen_lines())), (40, 10));

        assert_eq!(rig.terms.resize(&id, 1, 1), Ok((20, 5)), "too small is made the smallest");
        assert_eq!(rig.terms.resize(&id, 100_000, 100_000), Ok((400, 200)));
        assert_eq!(rig.terms.resize(&id, u32::MAX, 0), Ok((400, 5)));
        rig.send(&id, "stty size");
        rig.see(&id, "5 400").await;
        assert_eq!(rig.terms.resize("t99", 80, 24), Err(Missing::NotFound));
        rig.terms.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn closing_a_terminal_ends_what_it_started_in_the_background_too() {
        let rig = Rig::new();
        let id = rig.open().await;
        rig.send(&id, "sleep 1000 & echo BG=$!; (sleep 1000 & echo SUB=$!); echo SH=$$");
        let (bg, sub, shell) = (pid_of(&rig.value(&id, "BG=").await), pid_of(&rig.value(&id, "SUB=").await), pid_of(&rig.value(&id, "SH=").await));
        let members = pty::live_members(shell);
        assert!(members.contains(&shell) && members.contains(&bg) && members.contains(&sub), "{members:?} of {shell} {bg} {sub}");
        assert!(running(bg) && running(sub));

        let hangup = rig.terms.close(&id).expect("it was open");
        assert!(rig.terms.list().is_empty() && rig.terms.info(&id).is_none(), "gone from the table at once");
        hangup.wait().await;
        assert!(pty::live_members(shell).is_empty(), "nothing of the session is left: {:?}", pty::live_members(shell));
        assert!(!running(bg) && !running(sub) && !running(shell), "the job, the orphan and the shell are dead");
        until("the shell to be reaped", || !Path::new(&format!("/proc/{shell}")).exists()).await;

        assert!(rig.terms.close(&id).is_none(), "closing twice finds nothing");
        assert_eq!(rig.terms.input(&id, b"x"), Err(InputError::NotFound));
        assert!(rig.terms.text(&id, 10, 100).is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_cursor_position_query_gets_exactly_one_reply_on_the_shells_stdin() {
        let rig = Rig::new();
        let id = rig.open().await;
        let got = rig.probe(&id, 1, r"\033[6n").await;
        let text = String::from_utf8(got.clone()).unwrap();
        let body = text.strip_prefix("\x1b[").and_then(|t| t.strip_suffix('R')).unwrap_or_else(|| panic!("not one cursor report: {got:?}"));
        let (row, col) = body.split_once(';').unwrap_or_else(|| panic!("not a position: {text:?}"));
        assert!(row.parse::<u16>().is_ok() && col.parse::<u16>().is_ok(), "{text:?}");
        assert!(!body.contains('\x1b'), "one report, not two: {text:?}");
        rig.terms.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn only_the_answers_the_perch_chose_to_give_reach_the_shell() {
        let rig = Rig::new();
        let id = rig.open().await;
        // The clipboard read and write (OSC 52), the pixel size (CSI 14 t), XTVERSION and DECRQSS: nothing is written back for any of them.
        let none = rig.probe(&id, 2, r"\033]52;c;?\007\033]52;c;aGVsbG8=\007\033[14t\033[>0q\033P$qm\033\\").await;
        assert!(none.is_empty(), "the shell was answered: {none:?}");
        // The background colour is a question the perch answers, once, from its own palette.
        let background = rig.probe(&id, 3, r"\033]11;?\007").await;
        assert_eq!(background, b"\x1b]11;rgb:0505/0a0a/0505\x07".to_vec());
        // And still nothing for the clipboard afterwards: the one answer did not open a door.
        let after = rig.probe(&id, 4, r"\033]52;p;?\007").await;
        assert!(after.is_empty(), "{after:?}");
        rig.terms.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_seventeenth_terminal_is_refused_until_one_is_closed() {
        let rig = Rig::new();
        let mut ids = Vec::new();
        for n in 1..=MAX_TERMS {
            ids.push(rig.open().await);
            assert_eq!(ids.last().unwrap(), &format!("t{n}"));
        }
        assert_eq!(rig.terms.open(OpenSpec::default()).await, Err(OpenError::Cap));
        assert_eq!(rig.terms.list().len(), MAX_TERMS);
        rig.terms.close(&ids[3]).unwrap().wait().await;
        assert_eq!(rig.open().await, "t17", "an id is never reused, and the refused open used none");
        assert_eq!(rig.terms.open(OpenSpec::default()).await, Err(OpenError::Cap));
        rig.terms.shutdown().await;
        assert!(ids.iter().all(|id| rig.terms.get(id).is_none()));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_shell_has_its_terminal_id_and_none_of_the_perchs_credentials() {
        let rig = Rig::new();
        let id = rig.open().await;
        rig.send(&id, "echo C=$COLORTERM; echo X=${MINERVA_TEST_TOKEN-unset}; env | grep '^MINERVA_' | sort");
        assert_eq!(rig.value(&id, "C=").await, "truecolor");
        assert_eq!(rig.value(&id, "X=").await, "unset", "a name that looks like a credential is not passed on");
        let home = rig.home.path().join("Minerva").display().to_string();
        rig.see(&id, "MINERVA_TERM=t1").await;
        rig.see(&id, &format!("MINERVA_HOME={home}")).await;
        let mine: Vec<String> = rig.lines(&id).into_iter().filter(|l| l.starts_with("MINERVA_")).collect();
        assert_eq!(mine.len(), 2, "no other MINERVA_ name: {mine:?}");
        let screen = rig.lines(&id).join("\n");
        assert!(!screen.contains(SENTINEL), "the credential's value is nowhere on the shell's screen");
        rig.terms.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_shell_that_ends_keeps_its_place_and_says_how() {
        let rig = Rig::new();
        let id = rig.open().await;
        rig.send(&id, "exit 3");
        until("the shell to end", || rig.terms.info(&id).is_some_and(|info| !info.alive)).await;
        let info = rig.terms.info(&id).unwrap();
        assert_eq!(info.exit, Some(Exit { code: Some(3), signal: None }));
        assert!(info.fg.is_empty() && !info.ended, "it ended by itself, not with the perch: {info:?}");
        assert_eq!(rig.terms.input(&id, b"x"), Err(InputError::Ended));
        assert!(rig.terms.text(&id, 10, 500).is_some(), "its last screen can still be read");
        assert_eq!(rig.terms.list().len(), 1, "and it is listed until it is closed");
        rig.terms.close(&id).unwrap().wait().await;

        let id = rig.open().await;
        rig.send(&id, "kill -KILL $$");
        until("the shell to be killed", || rig.terms.info(&id).is_some_and(|info| !info.alive)).await;
        assert_eq!(rig.terms.info(&id).unwrap().exit, Some(Exit { code: None, signal: Some(9) }), "a signal is told apart from a code");
        rig.terms.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_shell_that_does_not_read_fills_its_queue_and_is_told_so() {
        let rig = Rig::new();
        let id = rig.open().await;
        assert_eq!(rig.terms.input(&id, &vec![b'x'; MAX_INPUT + 1]), Err(InputError::TooBig), "one byte over what one call may carry");
        // Raw and silent, and busy: the line discipline holds a few KiB and then stops taking, so the writer has nowhere to put the rest.
        rig.send(&id, "stty -icanon -echo; echo S=$$; sleep 1000");
        rig.value(&id, "S=").await;
        let chunk = vec![b'x'; 16 * 1024];
        let mut sent = 0usize;
        let refused = loop {
            match rig.terms.input(&id, &chunk) {
                Ok(()) => sent += chunk.len(),
                Err(e) => break e,
            }
            assert!(sent < 64 * 1024 * 1024, "{sent} bytes went in and the queue never said it was full");
            tokio::time::sleep(Duration::from_millis(2)).await;
        };
        assert_eq!(refused, InputError::Full);
        assert!(sent >= INPUT_QUEUE_FLOOR, "a full queue is the whole of the queue and more: {sent}");
        // Nothing waited, and the terminal is not stuck: it still ends when it is told to.
        let started = Instant::now();
        rig.terms.close(&id).unwrap().wait().await;
        assert!(started.elapsed() < CLOSE_GRACE + Duration::from_secs(1), "{:?}", started.elapsed());
    }

    /// The queue holds 64 KiB, so a refusal cannot come before that much went in.
    const INPUT_QUEUE_FLOOR: usize = pty::INPUT_QUEUE - 16 * 1024;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_agents_chat_holds_only_as_many_terminals_as_it_is_allowed() {
        let rig = Rig::new();
        let agent = |chat: &str| OpenSpec {
            owner: Owner { by: By::Agent, chat: Some(chat.into()) },
            chat_cap: Some(4),
            ..OpenSpec::default()
        };
        let mut mine = Vec::new();
        for _ in 0..4 {
            mine.push(rig.terms.open(agent("c1")).await.unwrap());
        }
        assert_eq!(rig.terms.open(agent("c1")).await, Err(OpenError::ChatCap));
        let other = rig.terms.open(agent("c2")).await.expect("another chat has its own allowance");
        let operator = OpenSpec { owner: Owner { by: By::Operator, chat: Some("c1".into()) }, ..OpenSpec::default() };
        rig.terms.open(operator).await.expect("an operator's terminal is not an agent's, whatever chat it sits beside");
        let info = rig.terms.info(&mine[1]).unwrap();
        assert_eq!((info.by, info.chat.as_deref()), (By::Agent, Some("c1")));
        assert_eq!(rig.terms.info(&other).unwrap().chat.as_deref(), Some("c2"));

        // A shell that ended still holds a place until it is closed, so it counts; but the chat that wants another has it let go.
        rig.send(&mine[0], "exit");
        until("a terminal to end", || rig.terms.info(&mine[0]).is_some_and(|info| !info.alive)).await;
        assert_eq!(rig.terms.list().len(), 6, "four of c1 (one ended), one of c2, one of the operator's");
        let mut changes = rig.terms.subscribe();
        let fifth = rig.terms.open(agent("c1")).await.expect("the chat's ended terminal made room");
        assert!(rig.terms.info(&mine[0]).is_none(), "and it was closed to do so");
        let seen: Vec<Change> =
            std::iter::from_fn(|| changes.try_recv().ok()).filter(|c| matches!(c, Change::Closed(_) | Change::Open(_))).collect();
        assert_eq!(seen, [Change::Closed(mine[0].clone()), Change::Open(fifth)], "the page is told it went");
        assert_eq!(rig.terms.open(agent("c1")).await, Err(OpenError::ChatCap), "four running ones are four");
        assert_eq!(rig.terms.list().len(), 6);
        rig.terms.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_agent_terminal_has_a_chat_and_the_default_allowance_is_four() {
        let rig = Rig::new();
        let agent = |chat: Option<&str>, cap: Option<usize>| OpenSpec {
            owner: Owner { by: By::Agent, chat: chat.map(String::from) },
            chat_cap: cap,
            ..OpenSpec::default()
        };
        assert_eq!(rig.terms.open(agent(None, None)).await, Err(OpenError::NoChat), "an agent's terminal belongs to a chat");
        assert_eq!(rig.terms.open(agent(None, Some(9))).await, Err(OpenError::NoChat), "whatever allowance it names");
        assert!(rig.terms.list().is_empty(), "and nothing was taken");
        assert_eq!(rig.terms.open(OpenSpec::default()).await.map(|id| id.starts_with('t')), Ok(true), "an operator's needs none");

        // No cap named: four.
        for _ in 0..AGENT_CHAT_CAP {
            rig.terms.open(agent(Some("c1"), None)).await.unwrap();
        }
        assert_eq!(rig.terms.open(agent(Some("c1"), None)).await, Err(OpenError::ChatCap));
        // The caller's own number is used when it names one, in either direction.
        assert_eq!(rig.terms.open(agent(Some("c2"), Some(1))).await.map(|_| ()), Ok(()));
        assert_eq!(rig.terms.open(agent(Some("c2"), Some(1))).await, Err(OpenError::ChatCap));
        assert_eq!(rig.terms.open(agent(Some("c3"), Some(0))).await, Err(OpenError::ChatCap), "a chat allowed none gets none");
        rig.terms.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_chat_that_fills_the_table_with_ended_terminals_gets_its_own_let_go_and_no_one_elses() {
        let rig = Rig::new();
        let agent = |chat: &str| OpenSpec { owner: Owner { by: By::Agent, chat: Some(chat.into()) }, ..OpenSpec::default() };
        // Four ended terminals of c1, four running ones of c2, and the operator fills the rest.
        let mut c1 = Vec::new();
        for _ in 0..4 {
            c1.push(rig.terms.open(agent("c1")).await.unwrap());
        }
        for id in &c1 {
            rig.send(id, "exit");
            until("a terminal to end", || rig.terms.info(id).is_some_and(|info| !info.alive)).await;
        }
        for _ in 0..4 {
            rig.terms.open(agent("c2")).await.unwrap();
        }
        for _ in 0..8 {
            rig.open().await;
        }
        assert_eq!(rig.terms.list().len(), MAX_TERMS);
        assert_eq!(rig.terms.open(agent("c2")).await, Err(OpenError::ChatCap), "c2 is at its allowance and has nothing ended to give");
        assert_eq!(rig.terms.open(OpenSpec::default()).await, Err(OpenError::Cap), "the operator has none of c1's to take");
        assert_eq!(rig.terms.list().len(), MAX_TERMS, "and nothing was let go for those refusals");

        // c1 wants one more: its oldest ended terminal makes room (the table was full and so was its allowance).
        let again = rig.terms.open(agent("c1")).await.unwrap();
        let held: Vec<String> = rig.terms.list().into_iter().filter(|info| info.chat.as_deref() == Some("c1")).map(|info| info.id).collect();
        assert_eq!(held.len(), 4, "{held:?}");
        assert!(!held.contains(&c1[0]) && held.contains(&again) && c1[1..].iter().all(|id| held.contains(id)), "the oldest went: {held:?}");
        assert_eq!(rig.terms.list().len(), MAX_TERMS);
        rig.terms.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_title_is_what_the_program_set_clean_and_short() {
        let rig = Rig::new();
        let id = rig.terms.open(OpenSpec { title: Some("start\u{1b}x".into()), ..OpenSpec::default() }).await.unwrap();
        assert_eq!(rig.terms.info(&id).unwrap().title, "startx", "a title given at open is made printable like any other");
        let mut changes = rig.terms.subscribe();
        rig.send(&id, "printf '\\033]2;%s\\007' mytitle");
        until("the title", || rig.terms.info(&id).unwrap().title == "mytitle").await;
        let seen: Vec<Change> = std::iter::from_fn(|| changes.try_recv().ok()).collect();
        assert!(seen.contains(&Change::Title(id.clone())), "{seen:?}");

        rig.send(&id, "printf '\\033]2;'; head -c 300 /dev/zero | tr '\\0' w; printf '\\007'");
        until("the long title", || rig.terms.info(&id).unwrap().title.starts_with("www")).await;
        assert_eq!(rig.terms.info(&id).unwrap().title.chars().count(), screen::MAX_TITLE, "cut to the longest a title may be");
        rig.terms.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_ring_keeps_the_last_bytes_and_says_when_a_reader_is_behind() {
        let rig = Rig::tuned(|host| host.ring = 1024);
        let id = rig.open().await;
        rig.send(&id, "i=0; while [ $i -lt 300 ]; do echo L$i; i=$((i+1)); done");
        rig.see(&id, "L299").await;
        let term = rig.terms.get(&id).unwrap();
        let (start, end, held, behind, tail, seq) = term.view(|_, ring| {
            let held = ring.contents();
            (ring.start(), ring.end(), held.len(), ring.read(0, 10), ring.read(ring.start(), 5000), term.seq())
        });
        assert_eq!((end - start) as usize, 1024, "the ring is the size it was made");
        assert_eq!(held, 1024);
        assert!(start > 0 && end > 1500, "more than the ring holds was written: {start}..{end}");
        assert_eq!(behind, Read::Behind { start });
        assert!(matches!(&tail, Read::Data(bytes) if bytes.len() == 1024 && String::from_utf8_lossy(bytes).contains("L299")), "{tail:?}");
        assert_eq!(seq, end, "the count the terminal reports is the ring's end");
        assert_eq!(rig.terms.info(&id).unwrap().seq, end);
        rig.terms.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn what_a_page_follows_changes_in_order_and_a_hold_that_changes_nothing_says_nothing() {
        let rig = Rig::new();
        let mut changes = rig.terms.subscribe();
        let id = rig.open().await;
        assert_eq!(rig.terms.set_hold(&id, true), Ok(true));
        assert_eq!(rig.terms.set_hold(&id, true), Ok(false), "already on: no change, no event");
        assert!(rig.terms.info(&id).unwrap().hold);
        assert_eq!(rig.terms.set_hold("t9", true), Err(Missing::NotFound));
        rig.send(&id, "exit");
        let mut seen = Vec::new();
        let exit = Change::Exit(id.clone());
        while seen.last() != Some(&exit) {
            seen.push(tokio::time::timeout(WAIT, changes.recv()).await.expect("an event").expect("the stream"));
        }
        rig.terms.close(&id).unwrap().wait().await;
        seen.extend(std::iter::from_fn(|| changes.try_recv().ok()));
        let id = || id.clone();
        assert_eq!(seen, vec![Change::Open(id()), Change::Hold(id()), Change::Exit(id()), Change::Closed(id())]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_open_that_cannot_happen_says_why_and_takes_nothing() {
        let rig = Rig::new();
        let nowhere = rig.home.path().join("nowhere");
        assert_eq!(rig.terms.open(OpenSpec { cwd: Some(nowhere), ..OpenSpec::default() }).await, Err(OpenError::Cwd));
        let file = rig.home.path().join("a-file");
        std::fs::write(&file, "x").unwrap();
        assert_eq!(rig.terms.open(OpenSpec { cwd: Some(file), ..OpenSpec::default() }).await, Err(OpenError::Cwd), "a file is not a folder");
        assert!(rig.terms.list().is_empty());

        let sub = rig.home.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let id = rig.terms.open(OpenSpec { cwd: Some(sub.clone()), ..OpenSpec::default() }).await.unwrap();
        assert_eq!(id, "t1", "the refused opens used no id and no place");
        rig.send(&id, "echo D=$(pwd)");
        assert_eq!(rig.value(&id, "D=").await, std::fs::canonicalize(&sub).unwrap().display().to_string());

        rig.terms.shutdown().await;
        assert_eq!(rig.terms.open(OpenSpec::default()).await, Err(OpenError::Stopping), "no terminal opens once the perch is stopping");
        let stopped = Rig::new();
        stopped.terms.host.stop.cancel();
        assert_eq!(stopped.terms.open(OpenSpec::default()).await, Err(OpenError::Stopping), "the perch's own stop token says so too");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn shutdown_ends_every_shell_in_time_even_one_that_ignores_the_hangup() {
        let rig = Rig::new();
        let (plain, stubborn, idle) = (rig.open().await, rig.open().await, rig.open().await);
        rig.send(&plain, "sleep 1000 & echo BG=$!; echo SH=$$");
        rig.send(&stubborn, "trap '' HUP TERM; sleep 1000 & echo BG=$!; echo SH=$$; wait");
        rig.send(&idle, "echo SH=$$");
        let (plain_bg, plain_sh) = (pid_of(&rig.value(&plain, "BG=").await), pid_of(&rig.value(&plain, "SH=").await));
        let (stubborn_bg, stubborn_sh) = (pid_of(&rig.value(&stubborn, "BG=").await), pid_of(&rig.value(&stubborn, "SH=").await));
        let idle_sh = pid_of(&rig.value(&idle, "SH=").await);
        assert!(running(plain_bg) && running(stubborn_bg) && running(stubborn_sh));

        let started = Instant::now();
        rig.terms.shutdown().await;
        let took = started.elapsed();
        assert!(took < SHUTDOWN_BOUND, "shutdown took {took:?}");
        assert!(took >= Duration::from_millis(1600), "the one that ignores the hang-up was given its grace before it was killed: {took:?}");
        for pid in [plain_bg, plain_sh, stubborn_bg, stubborn_sh, idle_sh] {
            assert!(!running(pid), "{pid} is still running");
        }
        for shell in [plain_sh, stubborn_sh, idle_sh] {
            assert!(pty::live_members(shell).is_empty(), "session {shell}: {:?}", pty::live_members(shell));
        }
        assert!(rig.terms.list().is_empty(), "nothing is left to list but what the file keeps for the next perch");
        assert_eq!(rig.terms.open(OpenSpec::default()).await, Err(OpenError::Stopping));
        let file = file_at(&rig.file());
        assert_eq!(file.terms.len(), 3);
        assert_eq!(file.next, 4, "the mark is past the three");
        assert!(file.terms.iter().all(|record| record.by_perch), "each of them was alive when the perch ended it");
        assert!(file.terms.iter().all(|record| record.exit.is_some()), "and each says how it ended: {:?}", file.terms);
        assert!(rig.left_running().is_empty(), "{:?}", rig.left_running());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn closing_does_not_wait_out_the_grace_for_a_job_that_goes_at_the_hangup() {
        // A job the shell put in a group of its own gets the hang-up too, so it goes now and not at the SIGKILL after the grace.
        let rig = Rig::new();
        let id = rig.open().await;
        rig.send(&id, "sleep 1000 & echo BG=$!");
        let bg = pid_of(&rig.value(&id, "BG=").await);
        let started = Instant::now();
        rig.terms.close(&id).unwrap().wait().await;
        assert!(started.elapsed() < Duration::from_millis(1500), "closing waited out the grace: {:?}", started.elapsed());
        assert!(!running(bg));
    }

    /// The one way out, written down so it is a decision and not a surprise: a process that starts a session of its own is not in the
    /// terminal's, and closing the terminal does not find it. (The test kills it itself.)
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn what_starts_a_session_of_its_own_is_not_found_by_a_close() {
        if !["/usr/bin/setsid", "/bin/setsid"].iter().any(|path| Path::new(path).exists()) {
            return;
        }
        let rig = Rig::new();
        let id = rig.open().await;
        rig.send(&id, "setsid sh -c 'echo SS=$$; exec sleep 1000' &");
        let escaped = pid_of(&rig.value(&id, "SS=").await);
        assert!(running(escaped));
        rig.terms.close(&id).unwrap().wait().await;
        let survived = running(escaped);
        unsafe { libc::kill(escaped, libc::SIGKILL) };
        assert!(survived, "setsid is the documented way out of a terminal's session; if this fails, the README's limitation is gone");
        until("the escaped process to die", || !running(escaped)).await;
    }

    fn file_at(file: &Path) -> File {
        serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap()
    }

    fn records_in(file: &Path) -> Vec<Record> {
        file_at(file).terms
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn terms_json_is_private_keeps_the_last_screen_and_is_read_back_by_the_next_perch() {
        let rig = Rig::new();
        let (gone, live) = (rig.open().await, rig.open().await);
        rig.send(&gone, "echo OLD$((1+2)); exit 3");
        rig.send(&live, "echo LAST$((6*7))");
        rig.see(&live, "LAST42").await;
        until("the first shell to end", || rig.terms.info(&gone).is_some_and(|info| !info.alive)).await;
        let before = file_at(&rig.file());
        assert!((before.next, before.terms.len()) == (3, 0), "while the perch runs the file keeps the id mark and no record: {before:?}");

        rig.terms.shutdown().await;
        assert_eq!(std::fs::metadata(rig.file()).unwrap().permissions().mode() & 0o777, 0o600, "the screens may hold anything");
        let json: serde_json::Value = serde_json::from_slice(&std::fs::read(rig.file()).unwrap()).unwrap();
        assert_eq!(json["next"], 3, "{json}");
        assert!(json["terms"].is_array() && json.as_object().unwrap().len() == 2);
        let records = records_in(&rig.file());
        let by = |id: &str| records.iter().find(|record| record.id == id).unwrap_or_else(|| panic!("no record {id}"));
        assert_eq!(records.len(), 2);
        assert!(by("t2").by_perch && by("t2").text.contains("LAST42"), "{:?}", by("t2"));
        assert!(!by("t1").by_perch && by("t1").text.contains("OLD3"), "{:?}", by("t1"));
        assert_eq!(by("t1").exit, Some(Exit { code: Some(3), signal: None }));
        let names: Vec<String> = std::fs::read_dir(rig.file().parent().unwrap()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into()).collect();
        assert_eq!(names, ["terms.json"], "no temporary file is left beside it");

        // The next perch lists what the last one ended, readably, and nothing in it can be driven.
        let next = Terms::new(rig.terms.host.clone());
        let mut listed: Vec<(String, bool, bool)> = next.list().into_iter().map(|info| (info.id, info.alive, info.ended)).collect();
        listed.sort();
        assert_eq!(listed, [("t1".to_string(), false, false), ("t2".to_string(), false, true)], "`ended` is what says the perch ended it");
        let t2 = next.info("t2").unwrap();
        assert!(t2.fg.is_empty() && t2.seq == 0 && t2.ended, "{t2:?}");
        assert!(t2.exit.is_some_and(|exit| exit.code.is_none() && exit.signal.is_some()), "it says it was ended by a signal: {t2:?}");
        let text = next.text("t2", 400, 7000).unwrap();
        assert!(text.text.contains("LAST42") && text.cursor.is_none());
        assert_eq!(next.input("t2", b"x"), Err(InputError::Ended));
        assert_eq!(next.resize("t2", 80, 24), Err(Missing::Ended));
        assert_eq!(next.set_hold("t2", true), Err(Missing::Ended));
        assert!(next.get("t2").is_none());
        assert_eq!(next.open(OpenSpec::default()).await.unwrap(), "t3", "an id is not handed out again after a restart");

        next.close("t2").expect("an ended terminal can be closed").wait().await;
        let kept = records_in(&rig.file());
        assert_eq!(kept.iter().map(|record| record.id.as_str()).collect::<Vec<_>>(), ["t1"], "closing one takes it out of the file");
        next.close("t1").unwrap().wait().await;
        let emptied = file_at(&rig.file());
        assert!(emptied.terms.is_empty() && emptied.next == 4, "no ended terminal, no record, and the file stays with its mark: {emptied:?}");
        next.shutdown().await;
        assert_eq!(records_in(&rig.file()).len(), 1, "and the terminal that ended with this perch is the new file");
        assert_eq!(file_at(&rig.file()).next, 4);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_terms_file_is_read_as_if_someone_else_wrote_it() {
        use serde_json::json;
        let rig = Rig::new();
        let now = now_ms();
        let record = |id: &str| {
            json!({"id": id, "title": "t", "cwd": "/", "cols": 80, "rows": 24, "started_ms": now - 2000, "ended_ms": now - 1000, "by_perch": true, "text": "x"})
        };
        let mut good = record("t5");
        good["title"] = json!("ok\u{1b}[31m\nline");
        good["cwd"] = json!("/tmp/\u{7}x");
        good["cols"] = json!(1000);
        good["rows"] = json!(0);
        good["by"] = json!("agent");
        good["chat"] = json!("chat-1");
        good["text"] = json!("a\u{1b}]0;x\u{7}b\nc\td");
        let mut bad_chat = record("t6");
        bad_chat["chat"] = json!("c\nhat");
        let mut expired = record("t7");
        expired["ended_ms"] = json!(1);
        let mut huge = record("t8");
        huge["text"] = json!("z".repeat(200_000));
        let mut future = record("t4");
        future["ended_ms"] = json!(now + 10 * 365 * 86_400_000u64);
        let mut tagged = record("t3");
        tagged["title"] = json!("hi\u{e0041}\u{e0042}\u{2060}there");
        tagged["text"] = json!("see\u{202e}me\u{e0041}");
        let all = json!({
            "next": 2,
            "terms": [good, record("t5"), record("t05"), record("x1"), record("t1234567"), record(""), record("../etc"), expired, bad_chat, huge, future, tagged],
        });
        std::fs::create_dir_all(rig.file().parent().unwrap()).unwrap();
        std::fs::write(rig.file(), serde_json::to_vec(&all).unwrap()).unwrap();

        let next = Terms::new(rig.terms.host.clone());
        let mut ids: Vec<String> = next.list().into_iter().map(|info| info.id).collect();
        ids.sort();
        assert_eq!(ids, ["t3", "t4", "t5", "t6", "t8"], "a repeated id, a non-canonical one, an expired one and an empty one are not records");
        assert_eq!(next.info("t3").unwrap().title, "hithere", "tags, joiners and the like are not kept");
        assert_eq!(next.text("t3", 10, 100).unwrap().text, "seeme");
        let t4 = next.info("t4").unwrap();
        assert!(t4.last_ms <= now_ms() + 1000, "a record that said it ended in ten years ended now: {}", t4.last_ms);
        let t5 = next.info("t5").unwrap();
        assert_eq!((t5.cols, t5.rows, t5.by, t5.chat.as_deref()), (400, 5, By::Agent, Some("chat-1")), "sizes are clamped, not trusted");
        assert!(!t5.title.chars().any(char::is_control) && !t5.cwd.chars().any(char::is_control), "{t5:?}");
        let text = next.text("t5", 50, 500).unwrap().text;
        assert!(!text.chars().any(|c| c.is_control() && c != '\n' && c != '\t'), "an escape in a saved screen is gone: {text:?}");
        assert!(text.starts_with('a') && text.contains("c\td"));
        assert_eq!(next.info("t6").unwrap().chat, None, "a chat id with a newline in it is dropped");
        let kept = records_in(&rig.file());
        assert_eq!(kept.len(), 5, "the file was rewritten without what was refused");
        assert!(kept.iter().find(|record| record.id == "t8").unwrap().text.len() <= screen::SAVED_TEXT_BYTES);
        assert_eq!(next.open(OpenSpec::default()).await.unwrap(), "t9", "the mark is below the ids in the file, so it goes past them");
        next.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_id_mark_in_the_file_is_kept_whatever_the_records_do() {
        use serde_json::json;
        let rig = Rig::new();
        let now = now_ms();
        let expired = json!({"id": "t7", "cols": 80, "rows": 24, "ended_ms": now - KEEP_ENDED_MS - 5000, "by_perch": true, "text": "x"});
        std::fs::create_dir_all(rig.file().parent().unwrap()).unwrap();
        std::fs::write(rig.file(), serde_json::to_vec(&json!({"next": 50, "terms": [expired]})).unwrap()).unwrap();
        let next = Terms::new(rig.terms.host.clone());
        assert!(next.list().is_empty(), "the only record's time was up");
        assert_eq!(file_at(&rig.file()).next, 50, "and the file, rewritten without it, still has the mark");
        assert_eq!(next.open(OpenSpec::default()).await.unwrap(), "t50", "an id past anything that was ever handed out");
        next.shutdown().await;

        // A mark that is not a number any id could have is held to the last one, so a flood of ids is a refusal, not a wrap-around.
        std::fs::write(rig.file(), br#"{"next": 4000000000, "terms": []}"#).unwrap();
        let spent = Terms::new(rig.terms.host.clone());
        assert_eq!(spent.open(OpenSpec::default()).await, Err(OpenError::Cap));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_id_is_not_handed_out_again_after_its_terminal_is_closed_and_the_perch_restarts() {
        let rig = Rig::new();
        let (a, b, c) = (rig.open().await, rig.open().await, rig.open().await);
        assert_eq!((a.as_str(), b.as_str(), c.as_str()), ("t1", "t2", "t3"));
        rig.terms.close(&c).unwrap().wait().await;
        rig.terms.shutdown().await;
        let file = file_at(&rig.file());
        assert_eq!(file.next, 4, "the mark counts the closed terminal too");
        assert!(file.terms.iter().all(|record| record.id != "t3"), "though it is not in the file");

        let second = Terms::new(rig.terms.host.clone());
        let d = second.open(OpenSpec::default()).await.unwrap();
        assert_eq!(d, "t4", "above the highest id ever handed out, not above the highest id kept");
        // And without any shutdown at all: the mark was written when the id was taken.
        second.close(&d).unwrap().wait().await;
        drop(second);
        let third = Terms::new(rig.terms.host.clone());
        assert_eq!(third.open(OpenSpec::default()).await.unwrap(), "t5", "a perch that died after the close still did not reuse it");
        third.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_terms_file_is_never_deleted_only_rewritten() {
        let rig = Rig::new();
        let id = rig.open().await;
        rig.send(&id, "exit");
        until("the shell to end", || rig.terms.info(&id).is_some_and(|info| !info.alive)).await;
        rig.terms.close(&id).unwrap().wait().await;
        assert!(rig.file().exists(), "closing the last ended terminal leaves the file");
        assert!(file_at(&rig.file()).terms.is_empty());
        let next = Terms::new(rig.terms.host.clone());
        assert!(next.list().is_empty());
        next.shutdown().await;
        assert!(rig.file().exists());
        assert_eq!(file_at(&rig.file()).next, 2);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn writers_of_the_file_do_not_hand_the_disk_each_others_older_view() {
        // Fifteen ended terminals from a previous perch, closed from fifteen threads at once, each of which writes the file after its own
        // close: whichever write lands last must be of the table after the last close, never of an older view collected earlier.
        let rig = Rig::new();
        let now = now_ms();
        let records: Vec<serde_json::Value> = (1..=15)
            .map(|n| serde_json::json!({"id": format!("t{n}"), "cols": 80, "rows": 24, "ended_ms": now - 1000, "by_perch": true, "text": "x".repeat(2000)}))
            .collect();
        std::fs::create_dir_all(rig.file().parent().unwrap()).unwrap();
        std::fs::write(rig.file(), serde_json::to_vec(&serde_json::json!({"next": 16, "terms": records})).unwrap()).unwrap();
        for _ in 0..5 {
            std::fs::write(rig.file(), serde_json::to_vec(&serde_json::json!({"next": 16, "terms": records})).unwrap()).unwrap();
            let table = Terms::new(rig.terms.host.clone());
            assert_eq!(table.list().len(), 15);
            std::thread::scope(|scope| {
                for n in 1..=15 {
                    let table = &table;
                    scope.spawn(move || {
                        table.close(&format!("t{n}")).expect("an ended terminal can be closed");
                    });
                }
            });
            assert!(table.list().is_empty());
            let file = file_at(&rig.file());
            assert!(file.terms.is_empty(), "a write of an older view landed last: {:?}", file.terms.iter().map(|r| &r.id).collect::<Vec<_>>());
            assert_eq!(file.next, 16);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_terms_file_with_too_many_records_is_cut_and_a_corrupt_one_is_set_aside() {
        let rig = Rig::new();
        let now = now_ms();
        let flood: Vec<serde_json::Value> = (100..140)
            .map(|n| serde_json::json!({"id": format!("t{n}"), "cols": 80, "rows": 24, "ended_ms": now - 1000, "by_perch": true, "text": "x"}))
            .collect();
        std::fs::create_dir_all(rig.file().parent().unwrap()).unwrap();
        std::fs::write(rig.file(), serde_json::to_vec(&serde_json::json!({"terms": flood})).unwrap()).unwrap();
        let next = Terms::new(rig.terms.host.clone());
        assert_eq!(next.list().len(), MAX_TERMS);
        assert_eq!(records_in(&rig.file()).len(), MAX_TERMS);
        assert_eq!(next.open(OpenSpec::default()).await, Err(OpenError::Cap), "ended terminals hold their places until they are closed");

        std::fs::write(rig.file(), b"not json {").unwrap();
        let again = Terms::new(rig.terms.host.clone());
        assert!(again.list().is_empty());
        let aside = std::fs::read_dir(rig.file().parent().unwrap()).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().starts_with("terms.json.bad-")).count();
        assert_eq!(aside, 1, "the file that did not parse is kept, not overwritten");
        assert_eq!(again.open(OpenSpec::default()).await.unwrap(), "t1");
        again.shutdown().await;

        // A number that is not a size at all makes the file one that does not parse, and it is set aside whole in the same way.
        std::fs::remove_file(rig.file()).unwrap();
        std::fs::write(rig.file(), br#"{"terms": [{"id": "t1", "cols": 100000, "ended_ms": 1}]}"#).unwrap();
        let wrong = Terms::new(rig.terms.host.clone());
        assert!(wrong.list().is_empty() && !rig.file().exists());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn making_the_table_deletes_the_copies_that_were_set_aside_long_ago_and_keeps_the_recent_ones() {
        let rig = Rig::new();
        let dir = rig.file().parent().unwrap().to_path_buf();
        std::fs::create_dir_all(&dir).unwrap();
        let now = now_ms();
        let (old, fresh) = ((now - KEEP_ENDED_MS - 60_000) / 1000, now / 1000);
        std::fs::write(dir.join(format!("terms.json.bad-{old}")), "old").unwrap();
        std::fs::write(dir.join(format!("terms.json.bad-{fresh}")), "fresh").unwrap();
        // A file that is bad now is set aside now, and survives the cleaning that runs just before it.
        std::fs::write(rig.file(), b"not json").unwrap();
        let again = Terms::new(rig.terms.host.clone());
        assert!(again.list().is_empty());
        let names: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert!(!names.contains(&format!("terms.json.bad-{old}")), "{names:?}");
        assert!(names.contains(&format!("terms.json.bad-{fresh}")), "{names:?}");
        assert_eq!(names.iter().filter(|n| n.starts_with("terms.json.bad-")).count(), 2, "the fresh one and the one just set aside: {names:?}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn shutdown_is_in_time_when_a_screen_is_held_and_that_terminal_is_recorded_without_its_text() {
        let rig = Rig::new();
        let (held, free) = (rig.open().await, rig.open().await);
        rig.send(&held, "echo HELD=$((6*7))");
        rig.send(&free, "echo FREE=$((6*7))");
        assert_eq!(rig.value(&held, "HELD=").await, "42");
        assert_eq!(rig.value(&free, "FREE=").await, "42");
        let term = rig.terms.get(&held).unwrap();
        let sid = term.sid();
        // Something holds that screen's lock and will not give it up (a reader that is stuck, a caller that waits for ever).
        let (locked, locked_rx) = std::sync::mpsc::channel();
        let (release, release_rx) = std::sync::mpsc::channel::<()>();
        let holder = {
            let term = term.clone();
            std::thread::spawn(move || {
                let _screen = term.hold_screen();
                locked.send(()).unwrap();
                let _ = release_rx.recv_timeout(Duration::from_secs(30));
            })
        };
        locked_rx.recv().unwrap();

        let started = Instant::now();
        rig.terms.shutdown().await;
        let took = started.elapsed();
        assert!(took < SHUTDOWN_BOUND + Duration::from_millis(300), "shutdown took {took:?} with one screen held");
        let records = records_in(&rig.file());
        let by = |id: &str| records.iter().find(|record| record.id == id).unwrap_or_else(|| panic!("no record {id}: {records:?}"));
        assert_eq!(by(&held).text, "", "its screen could not be had in time");
        assert!(by(&held).by_perch && by(&free).by_perch, "both were alive when the perch ended them");
        assert!(by(&free).text.contains("FREE=42") || by(&free).text.contains("FREE"), "the other one's screen is there: {:?}", by(&free).text);

        release.send(()).unwrap();
        holder.join().unwrap();
        until("the held terminal's session to be gone", || pty::live_members(sid).is_empty()).await;
        assert!(rig.left_running().is_empty(), "{:?}", rig.left_running());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_open_that_lands_after_the_shutdown_has_taken_the_table_is_hung_up_and_refused() {
        let rig = Rig::new();
        let (slot, evicted) = rig.terms.reserve(&Owner::default(), None).unwrap();
        assert!(evicted.is_empty());
        let (home, env) = (rig.home.path().to_path_buf(), pty::child_env(&rig.terms.host.env, &slot.id));
        let spawned = rig.terms.host.spawner.run(move || pty::spawn(80, 24, &home, &env)).await.unwrap().unwrap();
        let args = StartArgs {
            id: slot.id.clone(),
            owner: Owner::default(),
            cwd: rig.home.path().to_path_buf(),
            title: String::new(),
            cols: 80,
            rows: 24,
            ring: pty::RING_BYTES,
        };
        let term = Terminal::start(args, spawned, rig.terms.hub.clone()).unwrap();
        let sid = term.sid();
        assert!(!pty::live_members(sid).is_empty(), "the shell is running");

        // The open is in flight (its shell is started, its place is reserved) when the shutdown comes and takes the table.
        rig.terms.shutdown().await;
        assert_eq!(slot.commit(term).await, Err(OpenError::Stopping));
        assert!(pty::live_members(sid).is_empty(), "it was hung up, not left to run: {:?}", pty::live_members(sid));
        assert!(rig.terms.list().is_empty() && lock(&rig.terms.inner).reserved.is_empty());
        until("its shell to be gone", || rig.left_running().is_empty()).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn opens_that_race_a_shutdown_either_land_in_it_or_are_refused_and_nothing_is_left_running() {
        for ms in [0u64, 1, 2, 4, 7, 11] {
            let rig = Rig::new();
            let stop = async {
                tokio::time::sleep(Duration::from_millis(ms)).await;
                rig.terms.shutdown().await;
            };
            let (opened, ()) = tokio::join!(rig.terms.open(OpenSpec::default()), stop);
            assert!(matches!(opened, Ok(_) | Err(OpenError::Stopping)), "{ms} ms: {opened:?}");
            until("nothing to be running in the home", || rig.left_running().is_empty()).await;
            assert!(rig.terms.list().is_empty(), "{ms} ms");
            assert_eq!(rig.terms.open(OpenSpec::default()).await, Err(OpenError::Stopping));
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_shell_is_given_its_terminal_and_nothing_else_to_hold() {
        let rig = Rig::new();
        let id = rig.open().await;
        // `ls` lists the shell's descriptors (its own are the ones it has when it is asked, with no pipe or substitution of the shell's
        // own in the way).
        rig.send(&id, "echo FDBEGIN; ls -l /proc/$$/fd; echo FDEND");
        rig.see(&id, "FDEND").await;
        let lines = rig.lines(&id);
        let (from, to) = (lines.iter().position(|l| l == "FDBEGIN").unwrap(), lines.iter().position(|l| l == "FDEND").unwrap());
        let held: Vec<&str> = lines[from + 1..to].iter().filter_map(|l| l.split_once(" -> ").map(|(_, target)| target)).collect();
        assert!((3..=4).contains(&held.len()), "descriptors 0, 1 and 2 and the shell's own copy of its terminal, at most: {held:?}");
        assert!(held.iter().all(|t| t.starts_with("/dev/pts/") || *t == "/dev/tty"), "nothing but the terminal: {held:?}");
        assert_eq!(held.iter().filter(|t| t.starts_with("/dev/pts/")).collect::<std::collections::BTreeSet<_>>().len(), 1, "one terminal: {held:?}");
        rig.terms.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_program_that_sets_its_title_in_a_loop_costs_a_few_events_and_the_last_title_wins() {
        let rig = Rig::new();
        let id = rig.open().await;
        let mut changes = rig.terms.subscribe();
        rig.send(&id, "i=0; while [ $i -lt 400 ]; do printf '\\033]2;title %s\\007' $i; i=$((i+1)); done; echo DONE");
        rig.see(&id, "DONE").await;
        until("the last title", || rig.terms.info(&id).unwrap().title == "title 399").await;
        tokio::time::sleep(Duration::from_millis(250)).await;
        let titles = std::iter::from_fn(|| changes.try_recv().ok()).filter(|change| matches!(change, Change::Title(_))).count();
        assert!((1..=8).contains(&titles), "400 titles became {titles} events");
        // And a title that is the one it already was says nothing.
        rig.send(&id, "printf '\\033]2;title 399\\007'; echo AGAIN");
        rig.see(&id, "AGAIN").await;
        tokio::time::sleep(Duration::from_millis(250)).await;
        assert!(!std::iter::from_fn(|| changes.try_recv().ok()).any(|change| matches!(change, Change::Title(_))));
        rig.terms.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_hold_takes_back_what_was_written_to_the_pty_and_the_shell_has_not_read() {
        let rig = Rig::new();
        let id = rig.open().await;
        let echoed = |line: &'static str| {
            let rig = &rig;
            let id = id.clone();
            async move { until("a typed line to be echoed", || rig.lines(&id).iter().any(|l| l == line)).await }
        };
        // The shell is busy for two seconds and reads nothing: the line discipline holds what is typed to it and echoes it.
        rig.send(&id, "echo S=$$; sleep 2; echo SLEPT");
        rig.value(&id, "S=").await;
        rig.terms.agent_input(&id, "c1", b"echo AG$((6*7))\r").unwrap();
        echoed("echo AG$((6*7))").await;
        rig.send(&id, "echo OP$((1+1))");
        echoed("echo OP$((1+1))").await;

        assert_eq!(rig.terms.set_hold(&id, true), Ok(true));
        rig.see(&id, "SLEPT").await;
        rig.send(&id, "echo OK$((3+4))");
        rig.see(&id, "OK7").await;
        let lines = rig.lines(&id);
        assert!(!lines.iter().any(|l| l == "AG42"), "the agent's line was flushed before the shell could read it: {lines:?}");
        assert!(!lines.iter().any(|l| l == "OP2"), "and so was the operator's, written at the same time: the PTY cannot tell them apart: {lines:?}");

        // With the hold off the agent can type again.
        assert_eq!(rig.terms.set_hold(&id, false), Ok(true));
        rig.terms.agent_input(&id, "c1", b"echo AG$((6*7))\r").unwrap();
        rig.see(&id, "AG42").await;
        rig.terms.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_agents_input_is_stamped_announced_and_refused_under_a_hold_and_the_operators_is_not() {
        let rig = Rig::new();
        let id = rig.open().await;
        let mut changes = rig.terms.subscribe();
        assert_eq!(rig.terms.agent_input("t99", "c1", b"x"), Err(InputError::NotFound));
        assert_eq!(rig.terms.agent_input(&id, "c1", &vec![b'x'; MAX_INPUT + 1]), Err(InputError::TooBig));
        assert!(rig.terms.info(&id).unwrap().agent_last_ms.is_none());

        rig.terms.agent_input(&id, "c1", b"echo A$((6*7))\r").unwrap();
        rig.see(&id, "A42").await;
        assert!(rig.terms.info(&id).unwrap().agent_last_ms.is_some(), "the terminal knows an agent typed");
        let seen: Vec<Change> = std::iter::from_fn(|| changes.try_recv().ok()).collect();
        let typed = Change::Input { id: id.clone(), chat: Some("c1".into()), text: "echo A$((6*7))\u{240d}".into() };
        assert_eq!(seen.iter().filter(|c| **c == typed).count(), 1, "{seen:?}");

        assert_eq!(rig.terms.set_hold(&id, true), Ok(true));
        while changes.try_recv().is_ok() {}
        assert_eq!(rig.terms.agent_input(&id, "c1", b"echo B$((6*7))\r"), Err(InputError::Held));
        assert!(!std::iter::from_fn(|| changes.try_recv().ok()).any(|c| matches!(c, Change::Input { .. })), "a refusal is not an event");
        rig.terms.input(&id, b"echo C$((6*7))\r").unwrap();
        rig.see(&id, "C42").await;
        assert!(!rig.lines(&id).iter().any(|l| l == "B42"), "nothing of what was refused ran");

        rig.terms.set_hold(&id, false).unwrap();
        rig.send(&id, "exit");
        until("the shell to end", || rig.terms.info(&id).is_some_and(|info| !info.alive)).await;
        assert_eq!(rig.terms.agent_input(&id, "c1", b"x"), Err(InputError::Ended));
        rig.terms.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_agent_that_fills_its_lane_does_not_keep_the_operators_interrupt_out() {
        let rig = Rig::new();
        let id = rig.open().await;
        rig.send(&id, "stty -icanon -echo; sh -c 'echo SP=$$; exec sleep 1000'");
        let sleeper = pid_of(&rig.value(&id, "SP=").await);
        let chunk = vec![b'x'; 16 * 1024];
        let full = loop {
            match rig.terms.agent_input(&id, "c1", &chunk) {
                Ok(()) => tokio::time::sleep(Duration::from_millis(2)).await,
                Err(e) => break e,
            }
        };
        assert_eq!(full, InputError::Full, "the agent's lane is bounded");
        assert_eq!(rig.terms.input(&id, b"\x03"), Ok(()), "and the operator's is a lane of its own");
        until("the interrupt to reach the foreground program", || !running(sleeper)).await;
        rig.terms.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_last_output_of_a_shell_is_on_the_screen_when_the_terminal_says_it_ended() {
        let rig = Rig::new();
        for round in 0..12 {
            let id = rig.open().await;
            rig.send(&id, "printf FINAL%s $((6*7)); exit 5");
            until("it to end", || rig.terms.info(&id).is_some_and(|info| !info.alive)).await;
            let text = rig.terms.text(&id, 50, 2000).unwrap().text;
            assert!(text.contains("FINAL42"), "round {round}: the shell ended and its last output was not there yet: {text:?}");
            assert_eq!(rig.terms.info(&id).unwrap().exit, Some(Exit { code: Some(5), signal: None }));
            rig.terms.close(&id).unwrap().wait().await;
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_job_that_outlives_its_shell_keeps_the_terminal_listed_and_closing_it_kills_the_job() {
        let rig = Rig::new();
        let id = rig.open().await;
        rig.send(&id, "sleep 1000 & echo BG=$!; printf LAST%s $((6*7)); exit");
        let job = pid_of(&rig.value(&id, "BG=").await);
        until("the shell to end", || rig.terms.info(&id).is_some_and(|info| !info.alive)).await;
        let text = rig.terms.text(&id, 50, 2000).unwrap().text;
        assert!(text.contains("LAST42"), "the job holds the PTY open, so the reader never sees the end of it; the output is there all the same: {text:?}");
        assert!(running(job), "the job outlives the shell that started it");
        assert_eq!(rig.terms.list().len(), 1, "and the ended terminal is listed until it is closed");

        rig.terms.close(&id).unwrap().wait().await;
        assert!(!running(job), "closing the terminal ended the job");
        assert!(rig.left_running().is_empty(), "{:?}", rig.left_running());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_open_dropped_while_the_shell_is_being_forked_leaves_no_shell_and_the_spawner_goes_on() {
        let rig = Rig::new();
        for ms in [0u64, 0, 1, 1, 2, 3, 5, 8] {
            // The future is polled once, parks behind the fork, and is dropped when the time is up: before the fork, during it, or after.
            let result = tokio::time::timeout(Duration::from_millis(ms), rig.terms.open(OpenSpec::default())).await;
            if let Ok(Ok(id)) = result {
                rig.terms.close(&id).unwrap().wait().await;
            }
        }
        until("nothing to be left running in the home", || rig.left_running().is_empty()).await;
        assert!(lock(&rig.terms.inner).reserved.is_empty(), "no place is held for an open that is gone");
        assert!(rig.terms.list().is_empty());

        // The thread that forks is still there, for a shell and for anything else.
        let id = rig.open().await;
        rig.send(&id, "echo OK$((6*7))");
        rig.see(&id, "OK42").await;
        assert_eq!(rig.terms.host.spawner.run(|| 7).await.unwrap(), 7);
        rig.terms.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_table_that_is_dropped_takes_its_terminals_sessions_with_it() {
        let mut rig = Rig::new();
        let id = rig.open().await;
        rig.send(&id, "sleep 1000 & echo BG=$!; echo SH=$$");
        let (job, shell) = (pid_of(&rig.value(&id, "BG=").await), pid_of(&rig.value(&id, "SH=").await));
        let sid = rig.terms.get(&id).unwrap().sid();
        assert!(pty::live_members(sid).contains(&shell) && pty::live_members(sid).contains(&job));

        drop(rig.take_terms());
        assert!(pty::live_members(sid).is_empty(), "no member of the session survives the table: {:?}", pty::live_members(sid));
        assert!(!running(job) && !running(shell));
    }
}
