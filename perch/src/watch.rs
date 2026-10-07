//! The watcher: the perch's *own* stream to each live door.
//!
//! Per hub.md §1, a **watcher** is one `GET /api/events` stream from the perch to a door —
//! not a pane (a pane is a browser's stream, H1c's). It is two things at once:
//!
//! - §4 step 7's proof that a spawned door came up: the door is `live` once `hello` has
//!   been read, and `hello` is where a new session's log and cwd are published;
//! - the connection the **Stop finding** is about: a real door runs hyper's
//!   `GracefulShutdown`, so SIGTERM does not end it while this stream is open. [`Watcher::close`]
//!   is therefore the first step of every stop — it cancels the task, drops the request and
//!   the socket, and *waits* for that to have happened before returning, so the SIGTERM that
//!   follows cannot race a still-open stream.
//!
//! The stream is opened with the door's Bearer, read straight off the door's `0600` token
//! file and held in memory (§10.4). It never reaches a URL, an argument, or a log line.
//!
//! **What it reads.** Lines, one `data:` frame each, up to [`ASK_LINE_MAX`]; a longer line goes by unparsed and unheld, counted as it
//! passes (fail closed: an ask that was never read is an ask a person answers), and one past [`FRAME_MAX`] is also logged, by its count
//! and never its contents. A stream is never ended for a long line: one oversize record in a session's log would fail every replay at
//! the same point. Of the frames it does parse it keeps three small things and nothing of the transcript:
//!
//! ```text
//!   turn-state          -> Watcher::running            (what a restart refuses on)
//!   tool-call-started   -> call_id -> origin           (the last CALLS_MAX calls; model | script | user)
//!   ask / ask-settled   -> the open asks               (at most ASKS_MAX; each with its call's origin joined on)
//! ```
//!
//! The open asks are what "allow for the rest of this chat" (slice A1b) matches rules against, so the watcher keeps them as the door said
//! them and adds only what a rule needs of its own: the origin, joined from the call's own frame (`None` when that frame was never read
//! or two frames disagreed about it: a rule never answers a call whose origin is unknown), and a sequence number, so a rule can tell an
//! ask first seen after it was made. The ask's input is kept as the door's JSON text and parsed only when a sink asks for it.
//!
//! **Reconnect.** A door ends a stream that lags (a `lagged` frame, then the close), and a stream can end for other reasons. While the
//! watcher has not been closed it opens another one, after a pause that starts at [`BACKOFF_MIN`], doubles to [`BACKOFF_MAX`] and starts
//! over once a stream stayed up as long as the longest pause (so a door that lags every few seconds is not chased at full speed). The
//! close flag is checked before every connect and the pause is a `select!` against it, so a closing door is never reopened (§2 Stop
//! finding). A new stream's `hello` must name the session the first one did, and nothing counts before it. Asks the door replays are
//! matched by id against what is held, so the sink hears each ask once; an ask held that the replay did not list was settled while the
//! watcher was away, and is dropped (and the sink told). A door that says `goodbye` is leaving: the watcher stops, and the reap decides
//! what becomes of the row. Only a connect that fails, or a stream that ended before its own `hello`, counts toward giving up: after
//! [`GIVE_UP_AFTER`] of those in a row the watcher stops, because a door nobody can reach has no stream to hold. A stream that got its
//! `hello` and then ended for what it said (a lag, a close) keeps the backoff and never the give-up.
//!
//! **The sink** ([`Sink`]) hears of all this off the read loop: events go into a bounded queue and one task per watcher hands them to
//! the sink, one at a time and in order, each on a blocking thread, so a sink that is slow (or stuck) never stalls the reader or a close.
//! A queue that is full drops the newest event and says so in the log; the one loss that matters is an ask not answered by a rule, which
//! is what happens to an ask anyway. `closed` is the exception: it is not an event in that queue, it is what the delivery task does when
//! the queue ends, so it cannot be dropped.
//!
//! Not here, on purpose: panes, the proxy and door SSE passed to a browser (H1c); the idle reaper (H2).

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::net::Ipv4Addr;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use bytes::Bytes;
use http_body_util::{BodyExt as _, Empty};
use hyper::body::Incoming;
use hyper::client::conn::http1::SendRequest;
use hyper::header::{AUTHORIZATION, HOST};
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use serde_json::Value;
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

/// hub.md §5's connect timeout, for the watcher as much as for the proxy: this is a socket
/// to a process on this box.
const CONNECT: Duration = Duration::from_secs(2);

/// How much the watcher reads while looking for `hello`. The door's `hello` is the first frame
/// and a few hundred bytes; a door that sends this much without one is not a door.
const HELLO_MAX: usize = 1024 * 1024;

/// The longest `data:` line the reader parses. An `ask` carries the call's whole input (a `write`'s file, a `bash` command), so this is
/// also the largest ask a rule can ever be made from; a longer line is skipped, and the ask in it stays a person's to answer.
pub const ASK_LINE_MAX: usize = 64 * 1024;

/// A line this long is not a frame a door writes (a text delta is a few hundred bytes, a replayed assistant message a few hundred
/// kilobytes at most, and the door itself refuses bodies over 8 MiB). It is dropped as it goes by, like any line over [`ASK_LINE_MAX`], but
/// counted and logged (the count, never the contents). The stream is kept: ending it would fail the next replay at the same point.
pub const FRAME_MAX: usize = 8 * 1024 * 1024;

/// How many calls' origins are kept per watcher, oldest forgotten first. An ask whose call fell off the end has an unknown origin.
pub const CALLS_MAX: usize = 1024;

/// How many asks are held open per watcher. A door asks about a handful of calls at once; one past this is left to the person and logged.
pub const ASKS_MAX: usize = 32;

/// How many ask ids refused for being past [`ASKS_MAX`] are remembered, so that one is never opened later, when room comes back, behind a
/// rule made in the meantime. The smallest id is forgotten first (the highest id ever seen is kept apart, in [`AskView::max_ask_id`]).
const REFUSED_MAX: usize = 256;

/// The longest call id or tool name kept. Both are short identifiers; an ask carrying a longer one is not tracked at all.
const ID_MAX: usize = 256;

/// The longest `reason` kept. Eidolon's are short codes; a longer one is not what the door writes, and the ask is not tracked.
const REASON_MAX: usize = 1024;

/// How many events wait for the sink before the newest is dropped.
const SINK_QUEUE: usize = 256;

/// The pause before the first reconnect, and the most it ever grows to.
pub const BACKOFF_MIN: Duration = Duration::from_millis(250);
pub const BACKOFF_MAX: Duration = Duration::from_secs(5);

/// Connects that failed, or streams that ended before their own `hello`, in a row, after which the watcher stops reconnecting (about
/// 45 s at the backoff above). A stream that said `hello` and then ended for what it said does not count.
const GIVE_UP_AFTER: u32 = 12;

/// What `hello` says that the perch needs (§4 step 7): the log path, hence the id for a new
/// session, and the cwd, which is what a kept session's tree would be rooted at (H2).
pub struct Hello {
    pub session: PathBuf,
    pub cwd: PathBuf,
    pub model: String,
    /// Whether the door answers every ask with yes (`--yolo`), as the door itself reports it.
    pub yolo: bool,
}

/// A door as the perch knows it before it has a chat id: its process and its port. [`open`] runs before the registry row exists
/// (`serve::door_up`, then `Registry::insert`), so a [`Sink`] is told this and binds it to a chat later, through
/// [`crate::sessions::Registry::id_of_pid`]. A pid alone is not enough to name a door for good — the kernel reuses them — and the port
/// is the door's own, so the pair is what one watcher is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DoorKey {
    pub pid: u32,
    pub port: u16,
}

/// Who started a call, as the door's own `tool-call-started` frame says (`eidolon/crates/web/src/wire.rs`, `Origin`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Origin {
    /// The model's own `tool_use`.
    Model,
    /// A script's nested call (`tool_call` in a Rune script).
    Script,
    /// The operator's.
    User,
}

impl Origin {
    fn parse(word: &str) -> Option<Self> {
        match word {
            "model" => Some(Origin::Model),
            "script" => Some(Origin::Script),
            "user" => Some(Origin::User),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Model => "model",
            Origin::Script => "script",
            Origin::User => "user",
        }
    }
}

/// An ask is a yes/no about one call (`approval`), or a question with options (`question`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AskKind {
    Approval,
    Question,
}

/// One ask the door has open, as the watcher copied it from the door's `ask` frame. Cheap to clone: the input is shared text.
#[derive(Clone, Debug, PartialEq)]
pub struct OpenAsk {
    /// The door's own id; it restarts at 1 with every door process, so it names an ask only within one watcher.
    pub ask_id: u64,
    pub kind: AskKind,
    /// The call the ask is about, and the tool it names (an approval; `None` for a question).
    pub call_id: Option<String>,
    pub tool: Option<String>,
    /// The call's input as JSON text (`null` for a question), at most [`ASK_LINE_MAX`] bytes, the text of what the door sent and not of
    /// what it parses to: a 64 KiB padded input is a Value of about a megabyte, and nothing here pays for that until [`OpenAsk::input`].
    pub input_json: Arc<str>,
    /// Whether eidolon asks about the call's *shape* rather than its name (outside the working directory, a compound shell line): such an
    /// ask is never allowed by name. Absent in the frame counts as `true`; always `true` for a question.
    pub structural: bool,
    pub reason: Option<String>,
    /// Who started the call this ask is about, from the call's own `tool-call-started` frame; `None` when that frame was never read (it
    /// was too long, fell off the end of the kept calls, or has not come yet) or two frames disagreed about it. Nothing is allowed on `None`.
    pub origin: Option<Origin>,
    /// Counts the asks this watcher has opened: this ask is the `seen_seq`th, and [`AskView::seq`] is the number of the latest. `0` is an
    /// ask that was already pending when the watcher first saw it (it came in a reconnect's replay, before `caught-up`): it may have
    /// been pending before any rule was made, so no rule is stamped low enough to match it.
    pub seen_seq: u64,
}

impl OpenAsk {
    /// The call's input, parsed now. `Null` for a question (and for text that is not JSON, which the watcher never keeps).
    pub fn input(&self) -> Value {
        serde_json::from_str(&self.input_json).unwrap_or(Value::Null)
    }
}

/// What a sink is told when an ask opens: the small fields only. The input is not here, and the origin is the one the ask had at the
/// moment it opened, which can be `None` for a call whose frame had not come yet; a matcher reads both from [`AskView::ask`] when it
/// decides.
#[derive(Clone, Debug, PartialEq)]
pub struct AskNote {
    pub ask_id: u64,
    pub kind: AskKind,
    pub call_id: Option<String>,
    pub tool: Option<String>,
    pub structural: bool,
    /// A snapshot, taken at open. The door can send an ask before its call's `tool-call-started`, so this can be `None` for an ask whose
    /// origin is known a moment later: re-read it from the view.
    pub origin: Option<Origin>,
    /// As [`OpenAsk::seen_seq`]: `0` is an ask that was already pending when the watcher first saw it.
    pub seen_seq: u64,
}

/// Who else wants to hear what the watcher hears. Every method is a no-op by default, so a sink overrides only what it needs and the
/// watcher keeps no knowledge of its listeners.
///
/// **The contract, in hard terms.**
/// - A method returns promptly: no blocking I/O, no sleeping, no waiting on a lock that is held across an `.await`. [`Sink::hello`] and
///   [`Sink::watching`] run inline on the task that is opening the watcher; the rest run one at a time, in the order the door said them,
///   each on a blocking thread, so a slow sink delays the events behind it (and past [`SINK_QUEUE`] of them the newest is dropped) and a
///   stuck one costs the perch a thread. Neither ever holds up the stream or a close.
/// - A sink that has to *do* something (answer an ask) starts async work and returns, and that work selects on the perch's stop
///   (`ctx.stop`), so a stopping perch does not wait for it.
/// - A panic is contained: in `hello` or `watching` it makes [`open`] fail (the caller then stops the door); in any other method it loses
///   that one event.
/// - A sink addresses a door only by its [`DoorKey`]: never by chat id, which does not exist yet when `hello` is called and can be
///   re-bound to another door later.
/// - An [`AskView`] is valid from [`Sink::watching`] until [`Sink::closed`]: after that it is empty and says [`AskView::is_closed`].
/// - `closed` is told exactly once, last, for every `hello` that was told, and cannot be lost to a full queue.
pub trait Sink: Send + Sync {
    /// The door said `hello`: it is up, and this is what it said. Called once per [`open`] that succeeds, before `open` returns.
    fn hello(&self, _key: DoorKey, _hello: &Hello) {}

    /// Called right after `hello`, with a read handle on the asks this watcher holds for the door: the open ones, with their origins joined
    /// on the moment you ask, and the counters a rule is stamped with. Valid until [`Sink::closed`].
    fn watching(&self, _key: DoorKey, _asks: AskView) {}

    /// A door's ask the watcher had not seen before (a replay of one it holds is not told again). The note is a snapshot: its origin is as of
    /// when the ask opened, so read the ask, origin and input, from the [`AskView`] at the moment of deciding. Not told once the watcher has
    /// been closed.
    fn ask(&self, _key: DoorKey, _ask: &AskNote) {}

    /// An ask the watcher held is no longer open: the door said `ask-settled` for it, or a reconnect showed it gone.
    fn settled(&self, _key: DoorKey, _ask_id: u64) {}

    /// The watcher ended (a stop closed it, the door said `goodbye`, or it gave up on a door it could not reach). The last this sink hears
    /// for the key; the view it was given is empty from here.
    fn closed(&self, _key: DoorKey) {}
}

/// A held stream, and the handle that closes it. Dropping it cancels the stream too, so no
/// error path can leave one open; [`Watcher::close`] is the one that also *waits*, which is
/// what the stop order needs.
pub struct Watcher {
    close: CancellationToken,
    task: Option<tokio::task::JoinHandle<()>>,
    /// Whether a turn is running, from the door's own `turn-state` frames. A door starts idle, so this starts `false`.
    running: Arc<AtomicBool>,
    book: Arc<Book>,
}

impl Watcher {
    /// Whether the door said a turn is running, as of the last `turn-state` it sent. This is what a restart refuses on:
    /// a restart between turns loses nothing, one in the middle of a turn loses the turn.
    ///
    /// It is carried across a reconnect, not re-learned: the door sends `turn-state` only when it changes, and only a `turn-state` frame
    /// from the live stream ever makes this `false`. What a reconnect can add is `true`: a replay whose last turn marker says a turn
    /// began and never settled. A wrong `true` costs a restart that answers 409; a wrong `false` cuts a turn, so the replay is never
    /// believed about that. A turn that *began* in the gap and left no marker is not seen until the next change (hub.md §4, the same
    /// gap a first connect has).
    pub fn running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// A read handle on this watcher's open asks; see [`AskView`].
    pub fn view(&self) -> AskView {
        AskView { book: self.book.clone() }
    }

    /// Close the perch's stream to this door and wait until the socket is gone. After this
    /// returns, the door has no SSE connection left from the perch — which is the whole
    /// precondition of the SIGTERM that [`sessions::Registry::stop`] sends next. A reconnect that was
    /// pausing, or half open, is ended too, and nothing is opened after this is called. The sink is not waited for.
    pub async fn close(mut self) {
        self.close.cancel();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.close.cancel();
    }
}

/// Open the watcher and read until `hello`, or fail within `within`. The port is the door's
/// own, from its first boot line; the token is the door's, from its `0600` file. `sink` hears `hello` (with the door's `key`) when it comes.
/// A sink that panics in `hello` or `watching` makes this an `Err` (the watcher is closed first), and the caller stops the door.
pub async fn open(key: DoorKey, token: Arc<str>, within: Duration, sink: Arc<dyn Sink>) -> Result<(Watcher, Hello)> {
    open_paced(key, token, within, sink, (BACKOFF_MIN, BACKOFF_MAX)).await
}

/// [`open`] with the reconnect pause's bounds given: the seam the tests use to run twelve failed connects in milliseconds.
async fn open_paced(key: DoorKey, token: Arc<str>, within: Duration, sink: Arc<dyn Sink>, backoff: (Duration, Duration)) -> Result<(Watcher, Hello)> {
    let close = CancellationToken::new();
    let (hello_tx, hello_rx) = oneshot::channel();
    let running = Arc::new(AtomicBool::new(false));
    let book = Arc::new(Book::default());
    // The sink hears nothing off the read loop until `hello` has been told to it, so an ask replayed in the same read as `hello` waits in
    // the queue for `release`; dropping `release` (every failure before the sink was told) ends the delivery task without a word.
    let (told, queue) = mpsc::channel(SINK_QUEUE);
    let (release, released) = oneshot::channel();
    tokio::spawn(deliver(queue, sink.clone(), key, released, close.clone(), book.clone()));
    let shared = Shared { key, token, close: close.clone(), running: running.clone(), book: book.clone(), told };
    let task = tokio::spawn(watch(shared, Some(hello_tx), backoff));
    // A caller that stops waiting (this future dropped) cancels the stream it started; the `Watcher` takes that over on success.
    let abandoned = close.clone().drop_guard();
    match tokio::time::timeout(within, hello_rx).await {
        Ok(Ok(Ok(found))) => {
            // The watcher is built before the sink is told, so the one way out of here that is not the happy one closes it.
            abandoned.disarm();
            let watcher = Watcher { close, task: Some(task), running, book };
            let told = catch_unwind(AssertUnwindSafe(|| {
                sink.hello(key, &found);
                sink.watching(key, watcher.view());
            }));
            if told.is_err() {
                // The payload is the sink's own and may say anything; only the fact is logged.
                tracing::warn!(pid = key.pid, port = key.port, "perch: a sink panicked when told of a door; the watcher is closed");
                watcher.close().await;
                let _ = release.send(Release::Failed);
                return Err(anyhow::anyhow!("the sink panicked when told of the door"));
            }
            let _ = release.send(Release::Live);
            Ok((watcher, found))
        }
        // Every failure closes the stream before it returns: the caller's next move is a
        // SIGTERM, and a door with an open stream ignores one.
        Ok(Ok(Err(e))) => {
            close.cancel();
            let _ = task.await;
            Err(e)
        }
        Ok(Err(_)) => {
            close.cancel();
            let _ = task.await;
            Err(anyhow::anyhow!("the watcher ended without a hello"))
        }
        Err(_) => {
            close.cancel();
            let _ = task.await;
            Err(anyhow::anyhow!("no hello from the door within {within:?}"))
        }
    }
}

// ---------------------------------------------------------------------------
// What the watcher holds: call origins and open asks
// ---------------------------------------------------------------------------

/// The per-watcher state the reader writes and a [`AskView`] reads. One lock, held for a map operation and never across an await.
#[derive(Default)]
struct Book {
    inner: Mutex<Held>,
}

impl Book {
    /// The watcher is over: nothing is held any more, and every view says so. Called when the watch task ends, however it ends.
    fn end(&self) {
        let mut held = self.inner.lock().unwrap();
        held.asks.clear();
        held.calls.clear();
        held.order.clear();
        held.refused.clear();
        held.closed = true;
    }

    fn is_closed(&self) -> bool {
        self.inner.lock().unwrap().closed
    }
}

/// Clears the book when the watch task is over, including when it unwinds.
struct Ending(Arc<Book>);

impl Drop for Ending {
    fn drop(&mut self) {
        self.0.end();
    }
}

/// What the door's `tool-call-started` frames have said about one call id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Call {
    Known(Origin),
    /// An origin word the perch does not know.
    Unknown,
    /// Two frames disagreed. For good: a call id that two frames disagree about names no call a rule may trust.
    Split,
}

#[derive(Default)]
struct Held {
    /// call id -> what was said of it. Bounded by [`CALLS_MAX`]; `order` is the insertion order the oldest is forgotten by, except that
    /// a [`Call::Split`] is kept for as long as an ask about that call is held, however many calls have come since.
    calls: HashMap<String, Call>,
    order: VecDeque<String>,
    /// The open asks by id, with `origin` left `None`: it is joined on when one is read, because the call's frame can come after the ask.
    asks: BTreeMap<u64, OpenAsk>,
    /// Ids of asks refused for being past [`ASKS_MAX`], never to be opened later (at most [`REFUSED_MAX`]).
    refused: BTreeSet<u64>,
    /// The `seen_seq` of the latest ask opened.
    seq: u64,
    /// The highest ask id the door has shown this watcher, one that was refused or malformed included.
    max_ask: u64,
    closed: bool,
}

/// What opening an ask came to.
enum Opened {
    New(AskNote),
    /// Already held (a replay of an ask listed before).
    Duplicate,
    /// Refused now for being past [`ASKS_MAX`], and never to be opened.
    Full,
    /// Refused before.
    Refused,
}

impl Held {
    /// Record who started `id`. A second sighting that says the same changes nothing (a reconnect replays the model's calls); one that says
    /// something else makes the origin unknown for good.
    fn note_call(&mut self, id: &str, origin: Option<Origin>) {
        let seen = origin.map_or(Call::Unknown, Call::Known);
        if let Some(known) = self.calls.get_mut(id) {
            if *known != seen {
                *known = Call::Split;
            }
            return;
        }
        if self.order.len() >= CALLS_MAX {
            self.forget_one();
        }
        self.calls.insert(id.to_string(), seen);
        self.order.push_back(id.to_string());
    }

    /// Forget the oldest call, skipping a split one an ask is still held for.
    fn forget_one(&mut self) {
        let at = self.order.iter().position(|id| !(self.calls.get(id) == Some(&Call::Split) && self.holds(id)));
        if let Some(at) = at
            && let Some(id) = self.order.remove(at)
        {
            self.calls.remove(&id);
        }
    }

    fn holds(&self, call_id: &str) -> bool {
        self.asks.values().any(|ask| ask.call_id.as_deref() == Some(call_id))
    }

    fn joined(&self, ask: &OpenAsk) -> OpenAsk {
        let origin = match ask.call_id.as_deref().and_then(|id| self.calls.get(id)) {
            Some(Call::Known(origin)) => Some(*origin),
            _ => None,
        };
        OpenAsk { origin, ..ask.clone() }
    }

    /// An ask id the door has shown, whether or not the ask can be held.
    fn observe(&mut self, ask_id: u64) {
        self.max_ask = self.max_ask.max(ask_id);
    }

    /// `preexisting`: the ask came in a reconnect's replay before `caught-up`, so it may have been pending before the watcher saw it.
    fn open(&mut self, mut ask: OpenAsk, preexisting: bool) -> Opened {
        self.observe(ask.ask_id);
        if self.asks.contains_key(&ask.ask_id) {
            return Opened::Duplicate;
        }
        if self.refused.contains(&ask.ask_id) {
            return Opened::Refused;
        }
        if self.asks.len() >= ASKS_MAX {
            self.refused.insert(ask.ask_id);
            while self.refused.len() > REFUSED_MAX {
                self.refused.pop_first();
            }
            return Opened::Full;
        }
        ask.seen_seq = if preexisting {
            0
        } else {
            self.seq += 1;
            self.seq
        };
        ask.origin = None;
        let told = self.joined(&ask);
        let note = AskNote {
            ask_id: ask.ask_id,
            kind: ask.kind,
            call_id: ask.call_id.clone(),
            tool: ask.tool.clone(),
            structural: ask.structural,
            origin: told.origin,
            seen_seq: ask.seen_seq,
        };
        self.asks.insert(ask.ask_id, ask);
        Opened::New(note)
    }
}

/// A read handle on one watcher's asks, handed to the [`Sink`] by [`Sink::watching`]. Cheap to clone; reads are a short lock.
///
/// It is valid between [`Sink::watching`] and [`Sink::closed`]. When the watcher ends (a stop, a `goodbye`, a door it gave up on) every
/// ask is let go: [`AskView::ask`] says `None`, [`AskView::asks`] is empty and [`AskView::is_closed`] is `true`. A sink addresses a door
/// by its [`DoorKey`] and never by chat id, so a view that outlives its door cannot be mistaken for a later door's.
#[derive(Clone)]
pub struct AskView {
    book: Arc<Book>,
}

impl AskView {
    /// Every ask the door has open, oldest first, each with its origin joined on as of this call.
    pub fn asks(&self) -> Vec<OpenAsk> {
        let held = self.book.inner.lock().unwrap();
        held.asks.values().map(|ask| held.joined(ask)).collect()
    }

    /// One open ask by the door's id, or `None` when it is not open (never seen, settled, too long to have been read, or the watcher is
    /// over). Read the origin here at the moment of deciding: it can be known now where it was not when the sink was told.
    pub fn ask(&self, ask_id: u64) -> Option<OpenAsk> {
        let held = self.book.inner.lock().unwrap();
        held.asks.get(&ask_id).map(|ask| held.joined(ask))
    }

    /// The `seen_seq` of the latest ask opened. A rule made now is stamped with this; an ask matches it only if its own `seen_seq` is higher.
    pub fn seq(&self) -> u64 {
        self.book.inner.lock().unwrap().seq
    }

    /// The highest ask id the door has shown this watcher, including asks it could not hold. The door's ids only go up, so an ask with an id
    /// at or below the one a rule was stamped with was asked before the rule, whether or not the watcher ever held it.
    pub fn max_ask_id(&self) -> u64 {
        self.book.inner.lock().unwrap().max_ask
    }

    /// Whether the watcher is over. A view that says so holds nothing.
    pub fn is_closed(&self) -> bool {
        self.book.is_closed()
    }
}

// ---------------------------------------------------------------------------
// The sink's side: a queue and one task that empties it
// ---------------------------------------------------------------------------

/// What the sink is to hear, in the order the reader produced it. The end of the queue is what tells it `closed`, so that is not here.
#[derive(Debug, PartialEq)]
enum Told {
    Ask(AskNote),
    Settled(u64),
}

impl Told {
    fn event(&self) -> &'static str {
        match self {
            Told::Ask(_) => "ask",
            Told::Settled(_) => "settled",
        }
    }
}

/// `open`'s word to the delivery task, once it has told the sink `hello`.
#[derive(Debug, PartialEq, Eq)]
enum Release {
    /// Deliver what comes.
    Live,
    /// The sink panicked when told: deliver nothing, only `closed`.
    Failed,
}

/// Hands [`Told`] events to the sink, one at a time, each on a blocking thread: the sink is plain code and may take its time, and a
/// runtime thread held by it would stall every task the perch has (the reader included, on a small runtime). `released` is `open`'s word
/// that the sink has been told `hello`; without it nothing is delivered, and nothing is told at all. When the queue ends (the watcher is
/// over and what it queued has been handed over) the sink is told `closed`, once, on a blocking thread: not an event in the queue, so a
/// full queue cannot lose it. An ask is dropped, not delivered, once the watcher has been closed or has ended.
async fn deliver(
    mut queue: mpsc::Receiver<Told>,
    sink: Arc<dyn Sink>,
    key: DoorKey,
    released: oneshot::Receiver<Release>,
    close: CancellationToken,
    book: Arc<Book>,
) {
    let live = match released.await {
        Ok(Release::Live) => true,
        Ok(Release::Failed) => false,
        Err(_) => return,
    };
    while live && let Some(told) = queue.recv().await {
        let event = told.event();
        if matches!(told, Told::Ask(_)) && (close.is_cancelled() || book.is_closed()) {
            continue;
        }
        let sink = sink.clone();
        let done = tokio::task::spawn_blocking(move || match told {
            Told::Ask(note) => {
                // Ids, kind, tool name and origin only: an ask's input is the operator's command line, which can hold a secret.
                tracing::info!(
                    pid = key.pid, port = key.port, ask_id = note.ask_id, kind = ?note.kind, tool = note.tool.as_deref().unwrap_or("-"),
                    origin = note.origin.map_or("unknown", Origin::as_str), seq = note.seen_seq, "perch: the sink is told of an ask"
                );
                sink.ask(key, &note);
            }
            Told::Settled(ask_id) => sink.settled(key, ask_id),
        })
        .await;
        if done.is_err() {
            // The panic's own message is the sink's and may say anything, so it is not logged.
            tracing::warn!(pid = key.pid, port = key.port, error = "panic", event, "perch: a sink panicked on an event from a watcher; the event is lost");
        }
    }
    let done = tokio::task::spawn_blocking(move || sink.closed(key)).await;
    if done.is_err() {
        tracing::warn!(pid = key.pid, port = key.port, error = "panic", event = "closed", "perch: a sink panicked on closed");
    }
}

// ---------------------------------------------------------------------------
// The stream: connect, read, and reconnect while nobody has closed it
// ---------------------------------------------------------------------------

/// What the watch task shares with the rest of the process.
struct Shared {
    key: DoorKey,
    token: Arc<str>,
    close: CancellationToken,
    running: Arc<AtomicBool>,
    book: Arc<Book>,
    told: mpsc::Sender<Told>,
}

impl Shared {
    /// Queue an event for the sink without ever waiting: a full queue loses this one, which is logged by key and never by content.
    fn tell(&self, told: Told) {
        if self.told.try_send(told).is_err() {
            tracing::warn!(pid = self.key.pid, port = self.key.port, "perch: the sink is not keeping up; an event was dropped");
        }
    }
}

/// One open connection to a door's `/api/events`. `end` closes it and waits until the socket is gone.
struct Stream {
    _sender: SendRequest<Empty<Bytes>>,
    body: Incoming,
    connection: tokio::task::JoinHandle<()>,
}

impl Stream {
    /// Aborting the connection task is not enough on its own: the socket is closed when the task's future is *dropped*, and awaiting the
    /// handle is what makes that happen before this returns — which is what lets [`Watcher::close`] promise a closed stream to the SIGTERM
    /// that follows it.
    async fn end(self) {
        let Stream { _sender, body, connection } = self;
        drop(body);
        drop(_sender);
        end_connection(connection).await;
    }
}

async fn end_connection(connection: tokio::task::JoinHandle<()>) {
    connection.abort();
    let _ = connection.await;
}

/// Connect and ask for `/api/events` with the door's Bearer. `Ok(None)` when the watcher was closed on the way: whatever was half open is
/// closed before this returns. Every wait on the door is a `select!` against the close flag.
async fn open_stream(shared: &Shared) -> Result<Option<Stream>> {
    let port = shared.key.port;
    let connected = tokio::select! {
        _ = shared.close.cancelled() => return Ok(None),
        connected = tokio::time::timeout(CONNECT, TcpStream::connect((Ipv4Addr::LOCALHOST, port))) => connected,
    };
    let socket = match connected {
        Ok(Ok(socket)) => socket,
        Ok(Err(e)) => return Err(anyhow::Error::new(e).context(format!("connecting to the door on 127.0.0.1:{port}"))),
        Err(_) => anyhow::bail!("the door on 127.0.0.1:{port} did not accept a connection within {CONNECT:?}"),
    };
    let shaken = tokio::select! {
        _ = shared.close.cancelled() => return Ok(None),
        shaken = hyper::client::conn::http1::handshake(TokioIo::new(socket)) => shaken,
    };
    let (mut sender, connection) = shaken.map_err(|e| anyhow::Error::new(e).context("the HTTP/1.1 handshake with the door"))?;
    // Hyper's connection has to be polled or nothing moves on it. Ending this task drops the socket — which *is* closing the
    // stream, so there is no second mechanism to forget.
    let connection = tokio::spawn(async move {
        if let Err(e) = connection.await {
            tracing::debug!(error = %e, "perch: the watcher's connection to a door ended");
        }
    });

    let request = Request::builder()
        .method(Method::GET)
        .uri("/api/events")
        // The door's own token, as its operator would present it. The door checks `Host` on
        // its POSTs and statics, not on this route, but naming the door is what a client on
        // loopback says, and §5's proxy will say the same.
        .header(AUTHORIZATION, format!("Bearer {token}", token = shared.token))
        .header(HOST, format!("127.0.0.1:{port}"))
        .body(Empty::<Bytes>::new())
        .expect("the watcher's request is well-formed");

    let answered = tokio::select! {
        _ = shared.close.cancelled() => {
            end_connection(connection).await;
            return Ok(None);
        }
        answered = sender.send_request(request) => answered,
    };
    match answered {
        Ok(response) if response.status().is_success() => Ok(Some(Stream { _sender: sender, body: response.into_body(), connection })),
        // The door's own 401 means the perch's Bearer was not the door's token, which is a
        // bug here and not something a caller can fix; either way this door has no watcher.
        Ok(response) => {
            end_connection(connection).await;
            anyhow::bail!("the door's /api/events answered {}", response.status())
        }
        Err(e) => {
            end_connection(connection).await;
            Err(anyhow::Error::new(e).context("asking the door for /api/events"))
        }
    }
}

/// Why one stream stopped being read.
#[derive(Debug, PartialEq, Eq)]
enum End {
    /// The watcher was closed, or there was nobody left to tell (or the door gave no hello at all): no reconnect.
    Closed,
    /// The door said `lagged`; the stream ends, and a new one is opened.
    Lagged,
    /// The door said `goodbye`: it is leaving, so the watcher stops (the reap settles the row).
    Goodbye,
    /// The stream ended or broke.
    Eof,
    /// A reconnect whose `hello` named another session: not this door, so nothing more is sent to it.
    NotOurs,
}

/// The pause after a stream that did not last: twice the last, up to [`BACKOFF_MAX`].
pub fn next_backoff(last: Duration) -> Duration {
    (last * 2).min(BACKOFF_MAX)
}

/// How one try at a stream came out, as far as pacing goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    /// It stayed up as long as the longest pause: the door is fine, start over.
    Healthy,
    /// It said `hello` and then ended for what it said (a lag, a close): the door is there, so no give-up, but the pause keeps growing.
    Content,
    /// The connect failed, or the stream ended before its own `hello`: the door is not answering.
    Failed,
}

/// The reconnect pause, and how many tries in a row have found no door.
struct Pace {
    min: Duration,
    max: Duration,
    next: Duration,
    failed: u32,
}

impl Pace {
    fn new(min: Duration, max: Duration) -> Self {
        Self { min, max, next: min, failed: 0 }
    }

    /// A try ended. `Some(pause)` before the next one; `None` when it is time to stop trying.
    fn ended(&mut self, outcome: Outcome) -> Option<Duration> {
        match outcome {
            Outcome::Healthy => *self = Self::new(self.min, self.max),
            Outcome::Content => self.failed = 0,
            Outcome::Failed => self.failed += 1,
        }
        if self.failed >= GIVE_UP_AFTER {
            return None;
        }
        let pause = self.next;
        self.next = (self.next * 2).min(self.max);
        Some(pause)
    }
}

/// Connect, read until `hello`, then hold the stream open and read it until `close` is cancelled, reconnecting if the door ends it.
async fn watch(shared: Shared, mut hello_tx: Option<oneshot::Sender<Result<Hello>>>, backoff: (Duration, Duration)) {
    let port = shared.key.port;
    // However this task ends, the asks are let go and every view says so; the sink's `closed` follows when the queue ends.
    let _ending = Ending(shared.book.clone());
    let mut session: Option<PathBuf> = None;
    let (mut pace, mut streams) = (Pace::new(backoff.0, backoff.1), 0u32);
    let mut why_not = anyhow::anyhow!("the door's /api/events ended before hello");
    loop {
        if shared.close.is_cancelled() {
            break;
        }
        let outcome = match open_stream(&shared).await {
            Ok(None) => break,
            Err(e) if hello_tx.is_some() => {
                why_not = e;
                break;
            }
            Err(e) => {
                tracing::debug!(port, error = %format!("{e:#}"), "perch: the watcher could not reach its door");
                Outcome::Failed
            }
            Ok(Some(stream)) => {
                streams += 1;
                let read = read_stream(&shared, stream, &mut hello_tx, &mut session, streams > 1, backoff.1).await;
                match &read.end {
                    End::Closed => break,
                    End::NotOurs => {
                        tracing::warn!(port, "perch: a reconnect to a door's port answered as another session; the watcher stops");
                        break;
                    }
                    End::Goodbye => {
                        tracing::debug!(port, "perch: a door said goodbye; the watcher stops");
                        break;
                    }
                    _ if hello_tx.is_some() => break,
                    other => tracing::debug!(port, end = ?other, "perch: the watcher's stream to a door ended"),
                }
                read.outcome()
            }
        };
        let Some(pause) = pace.ended(outcome) else {
            tracing::warn!(port, "perch: the door has not answered the watcher in {GIVE_UP_AFTER} tries; the watcher stops");
            break;
        };
        tracing::info!(pid = shared.key.pid, port, ?pause, "perch: the watcher's stream to a door ended; reconnecting");
        // The close flag is the first thing both the pause and the next connect look at.
        tokio::select! {
            _ = shared.close.cancelled() => break,
            _ = tokio::time::sleep(pause) => {}
        }
    }
    give_up(&mut hello_tx, why_not);
}

/// How one stream was read.
struct Read {
    end: End,
    /// The stream's own `hello` was read: the door answered.
    hello: bool,
    /// It stayed up as long as the longest pause.
    healthy: bool,
}

impl Read {
    fn outcome(&self) -> Outcome {
        match (self.healthy, self.hello) {
            (true, _) => Outcome::Healthy,
            (false, true) => Outcome::Content,
            (false, false) => Outcome::Failed,
        }
    }
}

/// Read one stream until it ends, is closed, or must be ended. `longest` is the longest reconnect pause: a stream that stayed up that
/// long got going, which is what restarts the backoff.
async fn read_stream(
    shared: &Shared,
    mut stream: Stream,
    hello_tx: &mut Option<oneshot::Sender<Result<Hello>>>,
    session: &mut Option<PathBuf>,
    reconnect: bool,
    longest: Duration,
) -> Read {
    let began = Instant::now();
    let mut scan = hello_tx.is_some().then(HelloScan::default);
    let mut lines = Lines::default();
    let mut frames = Frames::new(shared, reconnect.then(|| session.clone()).flatten());
    let end = loop {
        tokio::select! {
            _ = shared.close.cancelled() => break End::Closed,
            frame = stream.body.frame() => match frame {
                Some(Ok(frame)) => {
                    let Some(data) = frame.data_ref() else { continue };
                    if let Some(looking) = scan.as_mut() {
                        match looking.feed(data) {
                            Ok(Some(found)) => {
                                // A caller that timed out is gone; there is nothing to hold open.
                                let Some(tell) = hello_tx.take() else { break End::Closed };
                                *session = Some(found.session.clone());
                                if tell.send(Ok(found)).is_err() {
                                    break End::Closed;
                                }
                                scan = None;
                            }
                            Ok(None) => {}
                            // No hello in the first MiB: `open` is told why, and this door gets no watcher at all.
                            Err(e) => {
                                give_up(hello_tx, e);
                                break End::Closed;
                            }
                        }
                    }
                    let (skipped, huge) = (lines.skipped, lines.huge);
                    let fed = lines.feed(data, |line| frames.line(line));
                    if lines.skipped > skipped {
                        tracing::debug!(port = shared.key.port, lines = lines.skipped - skipped, "perch: a line over the bound went by unread");
                    }
                    if lines.huge > huge {
                        // The count only: what the line said is nobody's to log.
                        tracing::warn!(port = shared.key.port, lines = lines.huge - huge, "perch: a line over {FRAME_MAX} bytes went by unread");
                    }
                    if fed == Fed::Stopped {
                        break frames.end.take().unwrap_or(End::Eof);
                    }
                }
                // The door ended the stream (a lag's own close, a dead process).
                _ => break End::Eof,
            },
        }
    };
    let (healthy, hello) = (began.elapsed() >= longest, frames.live);
    stream.end().await;
    Read { end, hello, healthy }
}

/// Nothing was told yet: the caller is waiting for an answer, and this is it.
fn give_up(hello_tx: &mut Option<oneshot::Sender<Result<Hello>>>, why: anyhow::Error) {
    if let Some(tell) = hello_tx.take() {
        let _ = tell.send(Err(why));
    }
}

// ---------------------------------------------------------------------------
// The frame reader: bytes -> lines -> the five frames the watcher keeps
// ---------------------------------------------------------------------------

/// How a read ended for [`Lines::feed`].
#[derive(Debug, PartialEq, Eq)]
enum Fed {
    More,
    /// The callback said stop.
    Stopped,
}

/// Lines out of a stream that arrives in pieces. A line over [`ASK_LINE_MAX`] is dropped as it goes by: nothing of it is held (memory is
/// the same for a line of any length), and the lines after it are read as usual. One over [`FRAME_MAX`] is dropped the same way and
/// counted apart, for the log. A stream is not a transcript: nothing but the line being assembled is ever held.
#[derive(Default)]
struct Lines {
    line: Vec<u8>,
    /// Bytes of the current line so far, kept or not.
    len: usize,
    /// The current line already passed [`ASK_LINE_MAX`]: the rest of it is counted and dropped up to the newline.
    over: bool,
    /// Lines dropped for being over [`ASK_LINE_MAX`], ever (the ones over [`FRAME_MAX`] included).
    skipped: u64,
    /// Lines over [`FRAME_MAX`], ever.
    huge: u64,
}

impl Lines {
    /// One more read. `on_line` gets each complete line that fit (without its `\n`, or a `\r` before it) and says whether to go on.
    fn feed(&mut self, mut data: &[u8], mut on_line: impl FnMut(&[u8]) -> bool) -> Fed {
        loop {
            let newline = data.iter().position(|&byte| byte == b'\n');
            let part = &data[..newline.unwrap_or(data.len())];
            self.len = self.len.saturating_add(part.len());
            if !self.over {
                if self.line.len() + part.len() > ASK_LINE_MAX {
                    self.over = true;
                    self.line = Vec::new();
                } else {
                    self.line.extend_from_slice(part);
                }
            }
            let Some(at) = newline else { return Fed::More };
            let go_on = if self.over {
                self.skipped += 1;
                if self.len > FRAME_MAX {
                    self.huge += 1;
                }
                true
            } else {
                let line = self.line.strip_suffix(b"\r").unwrap_or(&self.line);
                on_line(line)
            };
            // A buffer that grew for one big line is not kept for the next small one.
            if self.line.capacity() > 4096 {
                self.line = Vec::new();
            } else {
                self.line.clear();
            }
            (self.len, self.over) = (0, false);
            if !go_on {
                return Fed::Stopped;
            }
            data = &data[at + 1..];
        }
    }
}

/// The frame kinds the reader looks at. A line that says none of these words is not parsed (most of a stream is text deltas).
const KINDS: [&str; 11] = [
    "\"hello\"",
    "\"turn-state\"",
    "\"tool-call-started\"",
    "\"ask\"",
    "\"ask-settled\"",
    "\"caught-up\"",
    "\"lagged\"",
    "\"goodbye\"",
    "\"turn-settled\"",
    "\"cancelled\"",
    "\"user-message\"",
];

/// One stream's frames, as the watcher reads them. Nothing counts until the stream's own `hello` (and on a reconnect that `hello` must
/// name the session the first stream did).
struct Frames<'a> {
    shared: &'a Shared,
    /// The session a reconnect must be talking to; `None` on the first stream.
    expect: Option<PathBuf>,
    live: bool,
    /// The held asks this reconnect's replay has listed so far, to tell which of the ones held were settled while the watcher was away.
    /// Always a subset of what is held, so it is as bounded as that is.
    seen: BTreeSet<u64>,
    /// What the replay of the log said last about whether a turn is going: a `user-message` is one begun, a `turn-settled` or `cancelled`
    /// after it ends it.
    turn_begun: bool,
    caught_up: bool,
    end: Option<End>,
}

impl<'a> Frames<'a> {
    fn new(shared: &'a Shared, expect: Option<PathBuf>) -> Self {
        Self { shared, expect, live: false, seen: BTreeSet::new(), turn_begun: false, caught_up: false, end: None }
    }

    /// One line. `false` stops the reading (`self.end` says why).
    fn line(&mut self, raw: &[u8]) -> bool {
        let Ok(text) = std::str::from_utf8(raw) else { return true };
        // `data: {...}`, with or without the space the door's own writer uses. Anything else in the stream (a comment, an `id:`) is not a frame.
        let Some(payload) = text.strip_prefix("data:").map(str::trim_start) else { return true };
        if !KINDS.iter().any(|kind| payload.contains(kind)) {
            return true;
        }
        let Ok(frame) = serde_json::from_str::<Value>(payload) else { return true };
        let Some(kind) = frame.get("type").and_then(Value::as_str).map(str::to_owned) else { return true };
        if !self.live {
            if kind == "hello" {
                let named = frame.get("session").and_then(Value::as_str);
                let ours = match &self.expect {
                    None => true,
                    Some(want) => named.is_some_and(|named| Path::new(named) == want.as_path()),
                };
                if !ours {
                    self.end = Some(End::NotOurs);
                    return false;
                }
                self.live = true;
            }
            return true;
        }
        match kind.as_str() {
            "turn-state" => {
                if let Some(running) = frame.get("running").and_then(Value::as_bool) {
                    self.shared.running.store(running, Ordering::SeqCst);
                }
            }
            "turn-settled" | "cancelled" => self.turn_begun = false,
            "user-message" => self.turn_begun = true,
            "tool-call-started" => {
                let id = frame.get("id").and_then(Value::as_str).filter(|id| !id.is_empty() && id.len() <= ID_MAX);
                if let Some(id) = id {
                    let origin = frame.get("origin").and_then(Value::as_str).and_then(Origin::parse);
                    self.shared.book.inner.lock().unwrap().note_call(id, origin);
                }
            }
            "ask" => self.ask(&frame),
            "ask-settled" => {
                if let Some(ask_id) = frame.get("ask_id").and_then(Value::as_u64) {
                    let was_open = self.shared.book.inner.lock().unwrap().asks.remove(&ask_id).is_some();
                    self.seen.remove(&ask_id);
                    if was_open {
                        self.shared.tell(Told::Settled(ask_id));
                    }
                }
            }
            "caught-up" => self.catch_up(),
            "lagged" => {
                self.end = Some(End::Lagged);
                return false;
            }
            "goodbye" => {
                self.end = Some(End::Goodbye);
                return false;
            }
            _ => {}
        }
        true
    }

    /// An `ask` frame: held and told to the sink if it is new, counted as listed if it is a replay.
    fn ask(&mut self, frame: &Value) {
        // The id is noted whatever becomes of the ask: the door's ids only go up, and a rule is stamped with the highest one seen.
        if let Some(ask_id) = frame.get("ask_id").and_then(Value::as_u64) {
            self.shared.book.inner.lock().unwrap().observe(ask_id);
        }
        let Some(ask) = parse_ask(frame) else {
            tracing::debug!(port = self.shared.key.port, "perch: an ask the watcher could not read is left to the person");
            return;
        };
        let ask_id = ask.ask_id;
        // Before `caught-up` on a reconnect the door is listing what was pending, which can have been pending since before any rule was
        // made: such an ask is not "seen after" anything.
        let reconnecting = self.expect.is_some() && !self.caught_up;
        let opened = self.shared.book.inner.lock().unwrap().open(ask, reconnecting);
        match opened {
            Opened::New(note) => {
                if reconnecting {
                    self.seen.insert(ask_id);
                }
                self.shared.tell(Told::Ask(note));
            }
            Opened::Duplicate => {
                if reconnecting {
                    self.seen.insert(ask_id);
                }
            }
            Opened::Full => {
                tracing::warn!(port = self.shared.key.port, ask_id, "perch: {ASKS_MAX} asks are open on this door; the next is left to the person");
            }
            Opened::Refused => {}
        }
    }

    /// `caught-up`: the door has listed everything it holds. On a reconnect, that settles what was lost while the stream was down.
    fn catch_up(&mut self) {
        if std::mem::replace(&mut self.caught_up, true) || self.expect.is_none() {
            return;
        }
        let gone: Vec<u64> = {
            let mut held = self.shared.book.inner.lock().unwrap();
            let gone: Vec<u64> = held.asks.keys().filter(|id| !self.seen.contains(id)).copied().collect();
            for id in &gone {
                held.asks.remove(id);
            }
            gone
        };
        for ask_id in gone {
            self.shared.tell(Told::Settled(ask_id));
        }
        // A log that ends on a turn begun and not settled says one is running. The other way is not believed: a log that ends settled can
        // still be missing the turn the door is in (it is journaled as it goes), and a `false` that is wrong cuts a turn. Only a live
        // `turn-state` clears `running`.
        if self.turn_begun {
            self.shared.running.store(true, Ordering::SeqCst);
        }
    }
}

/// An `ask` frame as the watcher keeps it. `None` for one that is not what the door writes (a missing or mistyped field, a name or id past
/// [`ID_MAX`]): such an ask is not tracked and stays the person's.
fn parse_ask(frame: &Value) -> Option<OpenAsk> {
    fn text(frame: &Value, key: &str) -> Option<String> {
        frame.get(key).and_then(Value::as_str).filter(|text| !text.is_empty() && text.len() <= ID_MAX).map(str::to_owned)
    }
    let ask_id = frame.get("ask_id")?.as_u64()?;
    match frame.get("kind")?.as_str()? {
        "approval" => {
            let (call_id, tool) = (text(frame, "call_id")?, text(frame, "tool")?);
            // Absent counts as structural, so an ask in a shape this code does not know cannot be allowed by name.
            let structural = match frame.get("structural") {
                None => true,
                Some(Value::Bool(structural)) => *structural,
                Some(_) => return None,
            };
            let reason = match frame.get("reason") {
                None | Some(Value::Null) => None,
                Some(Value::String(reason)) if reason.len() <= REASON_MAX => Some(reason.clone()),
                Some(_) => return None,
            };
            // Kept as text: the frame's line was at most `ASK_LINE_MAX`, and what it parses to is not held. Written compactly, which is no
            // longer than what the door sent, but a bound is a bound.
            let input = serde_json::to_string(frame.get("input").unwrap_or(&Value::Null)).ok().filter(|text| text.len() <= ASK_LINE_MAX)?;
            let (call_id, tool, input_json) = (Some(call_id), Some(tool), Arc::from(input));
            Some(OpenAsk { ask_id, kind: AskKind::Approval, call_id, tool, input_json, structural, reason, origin: None, seen_seq: 0 })
        }
        "question" => Some(OpenAsk {
            ask_id,
            kind: AskKind::Question,
            call_id: None,
            tool: None,
            input_json: Arc::from("null"),
            structural: true,
            reason: None,
            origin: None,
            seen_seq: 0,
        }),
        _ => None,
    }
}

/// The bytes read while looking for `hello`, and nothing after it: they accumulate (to
/// [`HELLO_MAX`]) until `hello` is found, then the buffer is released and every later
/// [`HelloScan::feed`] appends nothing.
#[derive(Default)]
struct HelloScan {
    buf: Vec<u8>,
    found: bool,
}

impl HelloScan {
    /// One more read from the stream. `Some` exactly once, when `hello` completes.
    fn feed(&mut self, data: &[u8]) -> Result<Option<Hello>> {
        if self.found {
            return Ok(None);
        }
        self.buf.extend_from_slice(data);
        if let Some(found) = hello_from(&self.buf) {
            self.found = true;
            self.buf = Vec::new();
            return Ok(Some(found));
        }
        if self.buf.len() > HELLO_MAX {
            anyhow::bail!("no hello in the first 1 MiB ({HELLO_MAX} bytes) from the door");
        }
        Ok(None)
    }

    /// Bytes held (allocated), not just used: a released buffer is 0.
    #[cfg(test)]
    fn buffered(&self) -> usize {
        self.buf.capacity()
    }
}

/// The first `hello` frame in what has been read so far. Frames the perch does not know are
/// ignored — the door's protocol is additive, and a field this slice does not read (a
/// `turn-state` in H2, an `ask` in H1c) must not make the perch treat a live door as dead.
fn hello_from(seen: &[u8]) -> Option<Hello> {
    let text = std::str::from_utf8(seen).ok()?;
    for line in text.lines() {
        // `data: {...}`, with or without the space the door's own writer uses.
        let Some(payload) = line.strip_prefix("data:").map(str::trim_start) else { continue };
        let Ok(frame) = serde_json::from_str::<Value>(payload) else { continue };
        if frame.get("type").and_then(|t| t.as_str()) != Some("hello") {
            continue;
        }
        let session = frame.get("session").and_then(|s| s.as_str())?;
        let cwd = frame.get("cwd").and_then(|s| s.as_str())?;
        let model = frame.get("model").and_then(|m| m.as_str()).unwrap_or_default().to_string();
        let yolo = frame.get("yolo").and_then(|y| y.as_bool()).unwrap_or(false);
        return Some(Hello { session: PathBuf::from(session), cwd: PathBuf::from(cwd), model, yolo });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::AtomicUsize;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    use tokio::net::TcpListener;

    /// The door's own frame, byte for byte as `stream.rs` writes it: `frame_json` is
    /// `data: {json}\n\n`.
    fn frame(value: Value) -> Vec<u8> {
        format!("data: {value}\n\n").into_bytes()
    }

    fn hello_frame() -> Vec<u8> {
        frame(json!({"type": "hello", "session": "/logs/a.eid", "cwd": "/work", "model": "mock"}))
    }

    fn started(id: &str, origin: &str) -> Vec<u8> {
        frame(json!({"type": "tool-call-started", "id": id, "name": "bash", "input": {}, "origin": origin}))
    }

    fn approval(ask_id: u64, call_id: &str, input: Value) -> Vec<u8> {
        frame(json!({
            "type": "ask", "kind": "approval", "ask_id": ask_id, "call_id": call_id, "tool": "bash", "input": input,
            "prompt": "bash. Run it?", "reason": "outside-cwd", "structural": false, "judged": null, "yolo": false, "answers": ["yes", "no"]
        }))
    }

    fn turn_state(running: bool) -> Vec<u8> {
        frame(json!({"type": "turn-state", "running": running}))
    }

    fn bare(kind: &str) -> Vec<u8> {
        frame(json!({ "type": kind }))
    }

    #[test]
    fn hello_is_read_and_other_frames_are_left_alone() {
        let mut seen = frame(json!({"type": "caught-up"}));
        assert!(hello_from(&seen).is_none(), "caught-up is not hello");
        seen.extend(frame(json!({
            "type": "hello", "session": "/logs/ctf-pwn-3.eid", "cwd": "/work", "model": "mock",
            "yolo": false, "pending": 0, "protocol": 1
        })));
        let found = hello_from(&seen).expect("the hello frame is read");
        assert_eq!(found.session, PathBuf::from("/logs/ctf-pwn-3.eid"));
        assert_eq!(found.cwd, PathBuf::from("/work"));
        assert_eq!(found.model, "mock");

        // A frame split across two reads is completed by the next one, a frame with no
        // session is not a hello, and a frame that is not JSON is ignored.
        let half = b"data: {\"type\":\"hel";
        assert!(hello_from(half).is_none());
        assert!(hello_from(&frame(json!({"type": "hello", "cwd": "/work"}))).is_none());
        assert!(hello_from(b"data: not json\n\n").is_none());
    }

    #[test]
    fn a_hello_split_across_two_reads_is_found() {
        let bytes = hello_frame();
        let (head, tail) = bytes.split_at(20);
        let mut scan = HelloScan::default();
        assert!(scan.feed(head).unwrap().is_none());
        assert!(scan.buffered() > 0, "the half frame is held until the rest arrives");
        let found = scan.feed(tail).unwrap().expect("the second read completes hello");
        assert_eq!(found.session, PathBuf::from("/logs/a.eid"));
    }

    /// Any split of the frame is found, down to one byte per read, and exactly once.
    /// Mutation: parse only the latest read instead of the whole buffer and this goes red.
    #[test]
    fn a_hello_fed_one_byte_at_a_time_is_found_once() {
        let bytes = hello_frame();
        let mut scan = HelloScan::default();
        let mut found = Vec::new();
        for (at, byte) in bytes.iter().enumerate() {
            if let Some(hello) = scan.feed(&[*byte]).unwrap() {
                found.push((at, hello.session));
            }
        }
        // `hello_from` reads a `data:` line as soon as it parses, which is before the blank line
        // that ends the frame: the closing brace is what completes the JSON.
        assert_eq!(found.len(), 1, "told once: {found:?}");
        assert_eq!(found[0].1, PathBuf::from("/logs/a.eid"));
        assert_eq!(scan.buffered(), 0, "and released");
    }

    /// `hello` and a transcript replay arriving in one read: the cap is judged only when `hello`
    /// was not found, so a replay larger than the cap does not hide it, and nothing is kept.
    /// Mutation: check `HELLO_MAX` before looking for `hello` and this goes red.
    #[test]
    fn a_hello_followed_by_two_mib_in_the_same_read_is_found_and_nothing_is_kept() {
        let mut bytes = hello_frame();
        let piece = frame(json!({"type": "text-delta", "text": "x".repeat(4000)}));
        while bytes.len() < 2 * 1024 * 1024 {
            bytes.extend_from_slice(&piece);
        }
        assert!(bytes.len() > HELLO_MAX);
        let mut scan = HelloScan::default();
        let found = scan.feed(&bytes).expect("past the cap is fine when hello is in the read").expect("hello is found");
        assert_eq!(found.session, PathBuf::from("/logs/a.eid"));
        assert_eq!(scan.buffered(), 0);
    }

    /// The leak: after `hello` a door streams its whole transcript, and none of it may be held.
    /// Mutation: keep appending after `found` (drop the early return) and this goes red.
    #[test]
    fn nothing_is_buffered_after_hello() {
        let mut scan = HelloScan::default();
        assert!(scan.feed(&hello_frame()).unwrap().is_some());
        assert_eq!(scan.buffered(), 0);
        let piece = frame(json!({"type": "text-delta", "text": "x".repeat(4000)}));
        let mut sent = 0;
        while sent < 10 * 1024 * 1024 {
            assert!(scan.feed(&piece).unwrap().is_none(), "hello is told once");
            assert_eq!(scan.buffered(), 0, "after {sent} bytes past hello");
            sent += piece.len();
        }
    }

    /// Mutation: remove the cap and this goes red.
    #[test]
    fn a_stream_with_no_hello_is_refused_past_one_mib() {
        let mut scan = HelloScan::default();
        let mut sent = 0;
        while sent < HELLO_MAX {
            let piece = vec![b'x'; 4096.min(HELLO_MAX - sent)];
            assert!(scan.feed(&piece).unwrap().is_none(), "{sent} bytes is within the limit");
            sent += piece.len();
        }
        let err = scan.feed(b"x").err().expect("past the limit is refused").to_string();
        assert!(err.contains("1 MiB"), "{err}");
    }

    // -----------------------------------------------------------------------
    // The line reader
    // -----------------------------------------------------------------------

    fn collect(lines: &mut Lines, data: &[u8], out: &mut Vec<String>) -> Fed {
        lines.feed(data, |line| {
            out.push(String::from_utf8_lossy(line).into_owned());
            true
        })
    }

    #[test]
    fn lines_come_out_whole_whatever_the_read_boundaries() {
        let text = b"data: one\r\n\r\ndata: two\n\n: a comment\ndata: three";
        let (mut whole, mut each) = (Lines::default(), Lines::default());
        let (mut a, mut b) = (Vec::new(), Vec::new());
        assert_eq!(collect(&mut whole, text, &mut a), Fed::More);
        for byte in text {
            collect(&mut each, &[*byte], &mut b);
        }
        assert_eq!(a, b, "one byte at a time is the same lines");
        assert_eq!(a, ["data: one", "", "data: two", "", ": a comment"], "a line is complete at its newline, and a CR before it goes");
    }

    /// The bound a rule can be made under: a line of exactly [`ASK_LINE_MAX`] is read, one byte more goes by unread and unheld, and the
    /// lines around it are read as usual. Mutation: hold the line instead of dropping it (remove the `over` early-out), or move the bound,
    /// and this goes red.
    #[test]
    fn a_line_over_the_ask_bound_is_skipped_unheld_and_the_next_one_is_read() {
        let mut lines = Lines::default();
        let mut got = Vec::new();
        let exact = vec![b'a'; ASK_LINE_MAX];
        let over = vec![b'b'; 100 * 1024];
        let mut bytes = b"before\n".to_vec();
        bytes.extend(&exact);
        bytes.push(b'\n');
        bytes.extend(&over);
        bytes.push(b'\n');
        bytes.extend(b"c\n");
        for chunk in bytes.chunks(7000) {
            assert_eq!(collect(&mut lines, chunk, &mut got), Fed::More);
            assert!(lines.line.capacity() <= 2 * ASK_LINE_MAX, "a line that is over the bound is never grown past twice it");
        }
        assert_eq!(got.len(), 3, "{:?}", got.iter().map(String::len).collect::<Vec<_>>());
        assert_eq!((got[0].as_str(), got[1].len(), got[2].as_str()), ("before", ASK_LINE_MAX, "c"));
        assert_eq!(lines.skipped, 1);
        assert!(lines.line.capacity() <= 4096, "and no big buffer is kept for the next line");
    }

    /// A line past [`FRAME_MAX`] goes by like any other long one: dropped as it passes with nothing held, counted apart, the reading never
    /// stopped and the line after it read; one of exactly [`FRAME_MAX`] is not "huge". Mutation: stop the reading at the bound (the shape
    /// this replaced) and the first assertion goes red; hold the line and the capacity one does.
    #[test]
    fn a_line_past_the_frame_bound_is_dropped_counted_and_holds_nothing() {
        let mut lines = Lines::default();
        let (mut got, chunk) = (Vec::new(), vec![b'x'; 1024 * 1024]);
        for _ in 0..(FRAME_MAX / chunk.len() + 2) {
            assert_eq!(collect(&mut lines, &chunk, &mut got), Fed::More, "the stream is never ended for a long line");
            assert_eq!(lines.line.capacity(), 0, "and none of it is held");
        }
        assert_eq!((lines.skipped, lines.huge), (0, 0), "it has not ended yet, so it is not counted yet");
        assert_eq!(collect(&mut lines, b"\nafter\n", &mut got), Fed::More);
        assert_eq!(got, ["after"], "the line after it is read");
        assert_eq!((lines.skipped, lines.huge), (1, 1));
        let exact = vec![b'y'; FRAME_MAX];
        collect(&mut lines, &exact, &mut got);
        collect(&mut lines, b"\n", &mut got);
        assert_eq!((lines.skipped, lines.huge), (2, 1), "a line of exactly FRAME_MAX is skipped, not huge");
    }

    // -----------------------------------------------------------------------
    // The frames, through the real line reader
    // -----------------------------------------------------------------------

    fn rig() -> (Shared, mpsc::Receiver<Told>) {
        let (told, queue) = mpsc::channel(SINK_QUEUE);
        let shared = Shared {
            key: DoorKey { pid: 1, port: 2 },
            token: Arc::from("t"),
            close: CancellationToken::new(),
            running: Arc::new(AtomicBool::new(false)),
            book: Arc::new(Book::default()),
            told,
        };
        (shared, queue)
    }

    fn drain(queue: &mut mpsc::Receiver<Told>) -> Vec<Told> {
        let mut out = Vec::new();
        while let Ok(told) = queue.try_recv() {
            out.push(told);
        }
        out
    }

    fn asks_of(told: &[Told]) -> Vec<&AskNote> {
        told.iter().filter_map(|t| if let Told::Ask(note) = t { Some(note) } else { None }).collect()
    }

    fn view_of(shared: &Shared) -> AskView {
        AskView { book: shared.book.clone() }
    }

    /// One stream's reader with its `hello` already read: `expect` is the session a reconnect must name (`None`: a first stream).
    struct Reader<'a> {
        lines: Lines,
        frames: Frames<'a>,
    }

    impl<'a> Reader<'a> {
        fn new(shared: &'a Shared, expect: Option<&str>) -> Self {
            let mut reader = Self { lines: Lines::default(), frames: Frames::new(shared, expect.map(PathBuf::from)) };
            reader.feed(&hello_frame());
            reader
        }

        fn feed(&mut self, bytes: &[u8]) -> Fed {
            self.lines.feed(bytes, |line| self.frames.line(line))
        }
    }

    /// `turn-state` is read across reads, whatever the boundaries; a delta that merely says the words, and a line past the bound, change
    /// nothing; and the line after a skipped one is read. Mutation: parse every `data:` line whatever its type, or hold a line past the
    /// bound, and this goes red.
    #[test]
    fn a_turn_state_is_found_across_reads_and_nothing_else_moves_it() {
        let (shared, _queue) = rig();
        let mut reader = Reader::new(&shared, None);
        let running = || shared.running.load(Ordering::SeqCst);
        let on = turn_state(true);
        let (head, tail) = on.split_at(11);
        reader.feed(head);
        assert!(!running(), "half a frame says nothing");
        reader.feed(tail);
        assert!(running());
        reader.feed(&frame(json!({"type": "text-delta", "text": "\"type\":\"turn-state\",\"running\":false"})));
        assert!(running(), "a delta that quotes the words is not a turn-state");
        reader.feed(&frame(json!({"type": "text-delta", "text": "x".repeat(3 * ASK_LINE_MAX)})));
        assert!(running());
        assert_eq!(reader.lines.line.capacity(), 0, "an oversized line is not held");
        reader.feed(&turn_state(false));
        assert!(!running(), "the line after a skipped one is read");
        let mut both = turn_state(true);
        both.extend(turn_state(false));
        reader.feed(&both);
        assert!(!running(), "two in one read: the later one");
    }

    /// The ask a rule is made from keeps everything the door said about the call, and its origin comes from the call's own frame; the sink
    /// is told the small fields and reads the rest (the input, parsed on demand) from the view. Mutation: take the origin from anywhere but
    /// the `tool-call-started` with that id, or alter the input, and this goes red.
    #[test]
    fn an_ask_joins_the_origin_of_its_call_and_keeps_the_input_exact() {
        let (shared, mut queue) = rig();
        let mut reader = Reader::new(&shared, None);
        let view = view_of(&shared);
        let command = json!({"command": "cargo test ", "timeout_s": 30});
        reader.feed(&started("c1", "model"));
        reader.feed(&approval(1, "c1", command.clone()));
        reader.feed(&started("c2", "script"));
        reader.feed(&approval(2, "c2", json!({"command": "ls"})));
        reader.feed(&started("c3", "user"));
        reader.feed(&approval(3, "c3", json!({"command": "ls"})));
        reader.feed(&approval(4, "c4-never-started", json!({"command": "ls"})));
        let told = drain(&mut queue);
        let asks = asks_of(&told);
        assert_eq!(
            asks.iter().map(|a| (a.ask_id, a.origin, a.seen_seq)).collect::<Vec<_>>(),
            [(1, Some(Origin::Model), 1), (2, Some(Origin::Script), 2), (3, Some(Origin::User), 3), (4, None, 4)]
        );
        let first = asks[0];
        assert_eq!((first.kind, first.call_id.as_deref(), first.tool.as_deref(), first.structural), (AskKind::Approval, Some("c1"), Some("bash"), false));
        let held = view.ask(1).unwrap();
        assert_eq!((held.input(), held.reason.as_deref()), (command, Some("outside-cwd")));
        assert!(held.input_json.contains("cargo test \""), "the trailing space is in the text: {}", held.input_json);
    }

    /// The input is held as the door's text, shared and not parsed: a 60 KiB padded input is one string that every copy of the ask points
    /// at (an `asks()` under the lock copies a pointer), and it parses only when asked. Mutation: hold a parsed `Value` and the text
    /// assertions go red.
    #[test]
    fn an_ask_input_is_held_as_text_and_parsed_only_when_asked() {
        let (shared, mut queue) = rig();
        let mut reader = Reader::new(&shared, None);
        let view = view_of(&shared);
        let input = json!({"command": "ls", "pad": " ".repeat(60 * 1024)});
        reader.feed(&approval(1, "c1", input.clone()));
        assert_eq!(drain(&mut queue).len(), 1);
        let (one, two) = (view.ask(1).unwrap(), view.asks().remove(0));
        assert!(Arc::ptr_eq(&one.input_json, &two.input_json), "one copy of the text, however many asks() are made");
        assert!(one.input_json.len() > 60 * 1024 && one.input_json.len() <= ASK_LINE_MAX, "{}", one.input_json.len());
        assert_eq!(one.input(), input);
        // Questions have no input; their text is `null`.
        reader.feed(&frame(json!({"type": "ask", "kind": "question", "ask_id": 2, "prompt": "pick", "options": []})));
        assert_eq!(&*view.ask(2).unwrap().input_json, "null");
    }

    /// An ask can come before its call's frame (the door's two feeds are not ordered against each other): the copy the sink was told has
    /// no origin, and the one read afterwards does. Mutation: join the origin once, at arrival, and the second assertion goes red.
    #[test]
    fn a_call_frame_that_comes_after_its_ask_is_joined_when_the_ask_is_read() {
        let (shared, mut queue) = rig();
        let mut reader = Reader::new(&shared, None);
        let view = AskView { book: shared.book.clone() };
        reader.feed(&approval(1, "c1", json!({"command": "ls"})));
        assert_eq!(asks_of(&drain(&mut queue))[0].origin, None, "told before the call was seen");
        assert_eq!(view.ask(1).unwrap().origin, None);
        reader.feed(&started("c1", "model"));
        assert_eq!(view.ask(1).unwrap().origin, Some(Origin::Model));
        assert_eq!(view.asks().len(), 1);
        assert_eq!(view.seq(), 1);
    }

    /// Two frames that disagree about a call leave nothing a rule may trust; so does an origin word the perch does not know; and an
    /// agreeing second sighting (a reconnect's replay) changes nothing. Mutation: let the later frame win and this goes red.
    #[test]
    fn frames_that_disagree_about_a_call_leave_its_origin_unknown() {
        let (shared, _queue) = rig();
        let mut reader = Reader::new(&shared, None);
        let view = AskView { book: shared.book.clone() };
        reader.feed(&started("agree", "model"));
        reader.feed(&started("agree", "model"));
        reader.feed(&started("split", "script"));
        reader.feed(&started("split", "model"));
        reader.feed(&started("stays", "model"));
        reader.feed(&started("stays", "robot"));
        reader.feed(&started("odd", "robot"));
        for (n, call) in ["agree", "split", "stays", "odd"].iter().enumerate() {
            reader.feed(&approval(n as u64 + 1, call, json!({"command": "ls"})));
        }
        let origins: Vec<_> = view.asks().iter().map(|a| a.origin).collect();
        assert_eq!(origins, [Some(Origin::Model), None, None, None]);
        // A conflict is for good: a third frame cannot repair it.
        reader.feed(&started("split", "model"));
        assert_eq!(view.ask(2).unwrap().origin, None);
    }

    /// A disagreement outlives the calls bound for as long as an ask about that call is held: a replay that names the call again (the door
    /// replays the model's calls) must not make it trustworthy. Once nothing is held for it, it is forgotten like any other call.
    /// Mutation: forget the oldest call whatever it is, and the second assertion goes red.
    #[test]
    fn a_disagreement_about_a_call_is_kept_while_an_ask_about_it_is_held() {
        let (shared, _queue) = rig();
        let mut reader = Reader::new(&shared, None);
        let view = view_of(&shared);
        reader.feed(&started("split", "script"));
        reader.feed(&started("split", "model"));
        reader.feed(&approval(1, "split", json!({"command": "ls"})));
        for n in 0..(2 * CALLS_MAX) {
            reader.feed(&started(&format!("later-{n}"), "model"));
        }
        assert!(shared.book.inner.lock().unwrap().calls.len() <= CALLS_MAX, "the bound holds with a split call kept");
        reader.feed(&started("split", "model"));
        assert_eq!(view.ask(1).unwrap().origin, None, "named again in a replay, still not trusted");
        reader.feed(&frame(json!({"type": "ask-settled", "ask_id": 1, "how": "answered", "answer": "no"})));
        for n in 0..(2 * CALLS_MAX) {
            reader.feed(&started(&format!("more-{n}"), "model"));
        }
        assert!(!shared.book.inner.lock().unwrap().calls.contains_key("split"), "nothing held for it: forgotten");
    }

    /// An ask the reader cannot make sense of is not tracked (it stays the person's); a missing `structural` counts as structural.
    #[test]
    fn an_ask_that_is_not_what_the_door_writes_is_not_tracked() {
        let (shared, mut queue) = rig();
        let mut reader = Reader::new(&shared, None);
        let base = json!({"type": "ask", "kind": "approval", "ask_id": 1, "call_id": "c1", "tool": "bash", "input": {"command": "ls"}, "structural": false});
        let with = |key: &str, value: Value| {
            let mut ask = base.clone();
            ask[key] = value;
            frame(ask)
        };
        let without = |key: &str| {
            let mut ask = base.clone();
            ask.as_object_mut().unwrap().remove(key);
            frame(ask)
        };
        for bad in [
            with("ask_id", json!("1")),
            with("ask_id", json!(-1)),
            with("kind", json!("rule")),
            with("structural", json!("false")),
            with("reason", json!(7)),
            with("reason", json!("r".repeat(REASON_MAX + 1))),
            with("call_id", json!("")),
            with("call_id", json!("c".repeat(ID_MAX + 1))),
            with("tool", json!("t".repeat(ID_MAX + 1))),
            with("tool", json!(7)),
            without("kind"),
            without("call_id"),
            without("tool"),
        ] {
            reader.feed(&bad);
        }
        assert_eq!(drain(&mut queue), [], "none of them was taken");
        reader.feed(&without("structural"));
        reader.feed(&frame(json!({"type": "ask", "kind": "question", "ask_id": 2, "prompt": "pick", "options": []})));
        let told = drain(&mut queue);
        let asks = asks_of(&told);
        assert_eq!((asks[0].structural, asks[0].kind), (true, AskKind::Approval), "absent counts as structural");
        assert_eq!((asks[1].kind, asks[1].call_id.as_deref(), asks[1].origin), (AskKind::Question, None, None));
    }

    /// Bounds: at most [`ASKS_MAX`] asks open (the next is left alone, and is not told), and at most [`CALLS_MAX`] calls remembered, the
    /// oldest forgotten first. Mutation: lift either bound and this goes red.
    #[test]
    fn open_asks_and_remembered_calls_are_bounded() {
        let (shared, mut queue) = rig();
        let mut reader = Reader::new(&shared, None);
        let view = AskView { book: shared.book.clone() };
        for n in 1..=(ASKS_MAX as u64 + 5) {
            reader.feed(&approval(n, "c", json!({"command": "ls"})));
        }
        assert_eq!(view.asks().len(), ASKS_MAX);
        assert_eq!(asks_of(&drain(&mut queue)).len(), ASKS_MAX, "the sink heard of the ones that were taken");
        assert!(view.ask(ASKS_MAX as u64 + 1).is_none());
        // Room comes back when one settles.
        reader.feed(&frame(json!({"type": "ask-settled", "ask_id": 1, "how": "answered", "answer": "yes"})));
        reader.feed(&approval(100, "c", json!({"command": "ls"})));
        assert_eq!(view.asks().len(), ASKS_MAX);

        for n in 0..(CALLS_MAX + 10) {
            reader.feed(&started(&format!("call-{n}"), "model"));
        }
        assert_eq!(shared.book.inner.lock().unwrap().calls.len(), CALLS_MAX);
        // The asks are full again; make room, then ask about the oldest call remembered and the newest.
        for n in 2..=5 {
            reader.feed(&frame(json!({"type": "ask-settled", "ask_id": n, "how": "answered", "answer": "no"})));
        }
        reader.feed(&approval(200, "call-0", json!({"command": "ls"})));
        reader.feed(&approval(201, &format!("call-{}", CALLS_MAX + 9), json!({"command": "ls"})));
        assert_eq!((view.ask(200).unwrap().origin, view.ask(201).unwrap().origin), (None, Some(Origin::Model)));
    }

    /// An ask refused for room is refused for good: it is not opened later when a slot frees, behind a rule made in the meantime, and the
    /// highest id the door has shown is kept, refused and unreadable asks included. Mutation: forget the refusals, or count only held asks
    /// in `max_ask_id`, and this goes red.
    #[test]
    fn an_ask_refused_for_room_is_never_opened_later_and_its_id_is_still_counted() {
        let (shared, mut queue) = rig();
        let mut reader = Reader::new(&shared, None);
        let view = view_of(&shared);
        for n in 1..=(ASKS_MAX as u64 + 3) {
            reader.feed(&approval(n, "c", json!({"command": "ls"})));
        }
        assert_eq!(view.max_ask_id(), ASKS_MAX as u64 + 3, "the refused ones are counted");
        reader.feed(&frame(json!({"type": "ask", "kind": "approval", "ask_id": 500, "call_id": 7})));
        assert_eq!(view.max_ask_id(), 500, "so is one that cannot be read");
        drain(&mut queue);
        let settled = |id: u64| frame(json!({"type": "ask-settled", "ask_id": id, "how": "answered", "answer": "yes"}));
        for n in 1..=5 {
            reader.feed(&settled(n));
        }
        let told = drain(&mut queue);
        assert_eq!(told.len(), 5, "room has come back");
        reader.feed(&approval(ASKS_MAX as u64 + 1, "c", json!({"command": "ls"})));
        reader.feed(&approval(ASKS_MAX as u64 + 2, "c", json!({"command": "ls"})));
        assert_eq!(drain(&mut queue), [], "asks that were refused when full are not opened now");
        assert!(view.ask(ASKS_MAX as u64 + 1).is_none());
        reader.feed(&approval(ASKS_MAX as u64 + 10, "c", json!({"command": "ls"})));
        assert_eq!(asks_of(&drain(&mut queue)).len(), 1, "a new ask is opened as usual");
        assert_eq!(view.seq(), ASKS_MAX as u64 + 1, "and the sequence counted only what was opened");
    }

    /// An ask first listed in a reconnect's replay, before `caught-up`, may have been pending before any rule existed: it is stamped `0`,
    /// so no rule (stamped with the sequence at its making) is older than it. An ask the same replay lists again keeps its number, and one
    /// that arrives after `caught-up` is numbered as usual. Mutation: number replayed asks like the rest and this goes red.
    #[test]
    fn an_ask_first_seen_in_a_reconnect_replay_is_stamped_as_pending_before() {
        let (shared, mut queue) = rig();
        let view = view_of(&shared);
        let mut first = Reader::new(&shared, None);
        first.feed(&approval(1, "c1", json!({"command": "a"})));
        let mut second = Reader::new(&shared, Some("/logs/a.eid"));
        second.feed(&approval(1, "c1", json!({"command": "a"})));
        second.feed(&approval(2, "c1", json!({"command": "b"})));
        second.feed(&bare("caught-up"));
        second.feed(&approval(3, "c1", json!({"command": "c"})));
        let stamps: Vec<_> = view.asks().iter().map(|a| (a.ask_id, a.seen_seq)).collect();
        assert_eq!(stamps, [(1, 1), (2, 0), (3, 2)]);
        let told = drain(&mut queue);
        assert_eq!(asks_of(&told).iter().map(|a| (a.ask_id, a.seen_seq)).collect::<Vec<_>>(), [(1, 1), (2, 0), (3, 2)], "the sink is told the same");
        assert_eq!(view.seq(), 2);
    }

    /// When the watcher is over every ask is let go, however it ended, and every view says so; what was told stays told.
    #[test]
    fn a_view_of_a_finished_watcher_holds_nothing() {
        let (shared, _queue) = rig();
        let mut reader = Reader::new(&shared, None);
        let view = view_of(&shared);
        reader.feed(&started("c1", "model"));
        reader.feed(&approval(1, "c1", json!({"command": "a"})));
        assert!(!view.is_closed());
        drop(Ending(shared.book.clone()));
        assert!(view.is_closed());
        assert_eq!((view.ask(1), view.asks().len()), (None, 0));
        assert_eq!(shared.book.inner.lock().unwrap().calls.len(), 0);
        assert_eq!(view.seq(), 1, "the counters are not rewound");
    }

    /// A settle ends the ask once: the sink hears it for an ask held, and not for one that was never held.
    #[test]
    fn a_settle_closes_the_ask_once() {
        let (shared, mut queue) = rig();
        let mut reader = Reader::new(&shared, None);
        let view = AskView { book: shared.book.clone() };
        reader.feed(&approval(1, "c1", json!({"command": "ls"})));
        drain(&mut queue);
        let settled = |id: u64| frame(json!({"type": "ask-settled", "ask_id": id, "how": "answered", "answer": "yes"}));
        reader.feed(&settled(9));
        assert_eq!(drain(&mut queue), [], "an ask never held says nothing");
        reader.feed(&settled(1));
        reader.feed(&settled(1));
        assert_eq!(drain(&mut queue), [Told::Settled(1)]);
        assert!(view.ask(1).is_none());
    }

    /// The same ask listed twice (live and in a replay) is one ask. Mutation: key the book by anything but the door's ask id and it goes red.
    #[test]
    fn an_ask_listed_twice_is_one_ask() {
        let (shared, mut queue) = rig();
        let mut reader = Reader::new(&shared, None);
        reader.feed(&approval(1, "c1", json!({"command": "ls"})));
        reader.feed(&approval(1, "c1", json!({"command": "ls"})));
        reader.feed(&approval(1, "c1", json!({"command": "rm -rf /"})));
        let told = drain(&mut queue);
        assert_eq!(asks_of(&told).len(), 1);
        assert_eq!(view_of(&shared).ask(1).unwrap().input(), json!({"command": "ls"}), "the first copy stands");
    }

    // -----------------------------------------------------------------------
    // A reconnect, frame by frame
    // -----------------------------------------------------------------------

    /// Nothing counts on a reconnect before its `hello` names the session the first stream did: an ask or a turn before it is a stranger's.
    /// Mutation: start `live` as true and this goes red.
    #[test]
    fn a_reconnect_counts_nothing_before_a_hello_for_the_same_session() {
        let (shared, mut queue) = rig();
        let (mut lines, mut frames) = (Lines::default(), Frames::new(&shared, Some(PathBuf::from("/logs/a.eid"))));
        let mut feed = |bytes: &[u8]| lines.feed(bytes, |line| frames.line(line));
        feed(&turn_state(true));
        feed(&approval(1, "c1", json!({"command": "ls"})));
        assert_eq!(drain(&mut queue), []);
        assert!(!shared.running.load(Ordering::SeqCst));
        feed(&hello_frame());
        feed(&approval(1, "c1", json!({"command": "ls"})));
        assert_eq!(asks_of(&drain(&mut queue)).len(), 1);
    }

    /// A reconnect whose `hello` is for another session stops the reading at once: what follows it is not read. Mutation: skip the
    /// comparison and this goes red.
    #[test]
    fn a_hello_for_another_session_ends_a_reconnect() {
        let (shared, mut queue) = rig();
        let (mut lines, mut frames) = (Lines::default(), Frames::new(&shared, Some(PathBuf::from("/logs/a.eid"))));
        let mut bytes = frame(json!({"type": "hello", "session": "/logs/b.eid", "cwd": "/work", "model": "mock"}));
        bytes.extend(approval(1, "c1", json!({"command": "ls"})));
        assert_eq!(lines.feed(&bytes, |line| frames.line(line)), Fed::Stopped);
        assert_eq!(frames.end, Some(End::NotOurs));
        assert_eq!(drain(&mut queue), []);
    }

    /// An ask held that the replay did not list was settled while the stream was down: it goes, and the sink is told, at `caught-up` and
    /// not before. Mutation: skip the reconciliation and the held ask stays for good (the cap fills with ghosts).
    #[test]
    fn a_reconnect_drops_the_asks_its_replay_did_not_list_and_tells_the_sink() {
        let (shared, mut queue) = rig();
        let view = view_of(&shared);
        let mut first = Reader::new(&shared, None);
        for id in [1, 2, 3] {
            first.feed(&approval(id, "c1", json!({"command": "ls"})));
        }
        drain(&mut queue);
        let mut second = Reader::new(&shared, Some("/logs/a.eid"));
        second.feed(&approval(2, "c1", json!({"command": "ls"})));
        second.feed(&approval(4, "c1", json!({"command": "ls"})));
        assert_eq!(view.asks().iter().map(|a| a.ask_id).collect::<Vec<_>>(), [1, 2, 3, 4], "nothing is dropped before caught-up");
        second.feed(&bare("caught-up"));
        let kept: Vec<_> = view.asks().iter().map(|a| (a.ask_id, a.seen_seq)).collect();
        assert_eq!(kept, [(2, 2), (4, 0)], "2 keeps the number it was first seen under; 4 came in the replay and is stamped as pending before");
        let told = drain(&mut queue);
        assert_eq!(told.len(), 3);
        assert!(matches!(&told[0], Told::Ask(ask) if ask.ask_id == 4));
        assert_eq!((&told[1], &told[2]), (&Told::Settled(1), &Told::Settled(3)));
        second.feed(&bare("caught-up"));
        assert_eq!(drain(&mut queue), [], "a second caught-up on one stream does nothing");
    }

    /// The listing of a replay is kept only while a reconnect's replay is being listed: a first stream has none, and an ask after `caught-up`
    /// is live, so what is kept can never grow with the door's traffic. Mutation: insert on every ask and one of these goes red.
    #[test]
    fn the_listing_of_a_replay_is_kept_only_while_a_replay_is_being_listed() {
        let (shared, _queue) = rig();
        let mut first = Reader::new(&shared, None);
        for id in 1..=3 {
            first.feed(&approval(id, "c1", json!({"command": "ls"})));
        }
        assert!(first.frames.seen.is_empty(), "a first stream has no replay to list");
        let mut second = Reader::new(&shared, Some("/logs/a.eid"));
        second.feed(&approval(1, "c1", json!({"command": "ls"})));
        assert_eq!(second.frames.seen.len(), 1);
        second.feed(&bare("caught-up"));
        second.feed(&approval(4, "c1", json!({"command": "ls"})));
        second.feed(&approval(5, "c1", json!({"command": "ls"})));
        assert_eq!(second.frames.seen.len(), 1, "live asks after caught-up are not listed");
    }

    /// `running` is only ever cleared by a live `turn-state`: a replay that ends on a settled turn does not clear a `true` (the log can be
    /// missing the turn the door is in, and a `false` that is wrong cuts a turn), and a replay that ends on a turn begun and not settled
    /// sets it. Mutation: clear on a settled replay (the shape this replaced), or never set, and one of these goes red.
    #[test]
    fn a_replay_never_clears_running_and_sets_it_for_a_turn_begun_and_not_settled() {
        let run = |before: bool, frames: &[Vec<u8>]| {
            let (shared, _queue) = rig();
            shared.running.store(before, Ordering::SeqCst);
            let mut reader = Reader::new(&shared, Some("/logs/a.eid"));
            for bytes in frames {
                reader.feed(bytes);
            }
            reader.feed(&bare("caught-up"));
            shared.running.load(Ordering::SeqCst)
        };
        assert!(run(true, &[bare("user-message"), bare("turn-settled")]), "settled replay: a true stays true");
        assert!(run(true, &[bare("user-message"), bare("turn-settled"), bare("user-message"), bare("cancelled")]), "so does one that ends on a cancel");
        assert!(run(true, &[]), "and one with no turns in it");
        assert!(!run(false, &[bare("user-message"), bare("turn-settled")]), "a false stays false when the replay is settled");
        assert!(!run(false, &[bare("user-message"), bare("turn-settled"), bare("user-message"), bare("cancelled")]));
        assert!(!run(false, &[]));
        assert!(run(false, &[bare("user-message")]), "a turn begun and not settled in the log: running");
        assert!(run(false, &[bare("user-message"), bare("turn-settled"), bare("user-message")]), "the last marker decides");
        // And only a live `turn-state` clears it.
        let (shared, _queue) = rig();
        shared.running.store(true, Ordering::SeqCst);
        let mut reader = Reader::new(&shared, Some("/logs/a.eid"));
        reader.feed(&bare("user-message"));
        reader.feed(&bare("turn-settled"));
        reader.feed(&bare("caught-up"));
        assert!(shared.running.load(Ordering::SeqCst));
        reader.feed(&turn_state(false));
        assert!(!shared.running.load(Ordering::SeqCst));
    }

    #[test]
    fn the_pause_doubles_to_its_cap_and_starts_over_after_a_stream_that_got_going() {
        let ms = |n: u64| Duration::from_millis(n);
        let mut pace = Pace::new(BACKOFF_MIN, BACKOFF_MAX);
        let pauses: Vec<Duration> = (0..7).map(|_| pace.ended(Outcome::Failed).unwrap()).collect();
        assert_eq!(pauses, [ms(250), ms(500), ms(1000), ms(2000), ms(4000), ms(5000), ms(5000)]);
        assert_eq!(pace.ended(Outcome::Healthy), Some(BACKOFF_MIN), "a stream that got going starts over");
        assert_eq!(pace.ended(Outcome::Failed), Some(ms(500)));
        let mut small = Pace::new(ms(5), ms(20));
        assert_eq!((0..4).map(|_| small.ended(Outcome::Failed).unwrap()).collect::<Vec<_>>(), [ms(5), ms(10), ms(20), ms(20)]);
    }

    /// Only a connect that failed or a stream that ended before its `hello` counts toward giving up: GIVE_UP_AFTER of them in a row, and
    /// not one fewer. A stream that said `hello` and ended for what it said keeps the pause growing and never gives up, and it clears the
    /// count (the door answered). Mutation: count content endings, or never clear, and one of these goes red.
    #[test]
    fn only_failures_to_reach_a_door_count_toward_giving_up() {
        let mut pace = Pace::new(BACKOFF_MIN, BACKOFF_MAX);
        for _ in 1..GIVE_UP_AFTER {
            assert!(pace.ended(Outcome::Failed).is_some());
        }
        assert_eq!(pace.ended(Outcome::Failed), None);

        let mut pace = Pace::new(BACKOFF_MIN, BACKOFF_MAX);
        for _ in 0..(10 * GIVE_UP_AFTER) {
            assert!(pace.ended(Outcome::Content).is_some(), "a door that lags every time is never given up on");
        }
        assert_eq!(pace.ended(Outcome::Content), Some(BACKOFF_MAX), "but it is paced at the cap");

        let mut pace = Pace::new(BACKOFF_MIN, BACKOFF_MAX);
        for _ in 1..GIVE_UP_AFTER {
            pace.ended(Outcome::Failed);
        }
        assert!(pace.ended(Outcome::Content).is_some(), "an answer clears the count");
        for _ in 1..GIVE_UP_AFTER {
            assert!(pace.ended(Outcome::Failed).is_some());
        }
        assert_eq!(pace.ended(Outcome::Healthy), Some(BACKOFF_MIN), "and a healthy one starts the pause over");
    }

    // -----------------------------------------------------------------------
    // The sink and the open stream, against doors that run a script
    // -----------------------------------------------------------------------

    /// Every `hello` a sink was told, with the key it was told it under.
    #[derive(Default)]
    struct Recorder(Mutex<Vec<(DoorKey, PathBuf)>>);

    impl Sink for Recorder {
        fn hello(&self, key: DoorKey, hello: &Hello) {
            self.0.lock().unwrap().push((key, hello.session.clone()));
        }
    }

    /// A door on a loopback port that answers `/api/events` with `frames` and then, when `hold`, keeps the stream open until the
    /// perch closes it. The port is the door's; the pid the test makes up, because the sink is told whatever the caller says.
    async fn fake_door(frames: Vec<u8>, hold: bool) -> (u16, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let _ = sock.read(&mut buf).await;
            sock.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n").await.unwrap();
            sock.write_all(&frames).await.unwrap();
            if hold {
                // Until the watcher drops its end.
                while sock.read(&mut buf).await.is_ok_and(|n| n > 0) {}
            }
        });
        (port, task)
    }

    /// The seam A1b hangs off: `watch::open` runs before the registry has a row, so the sink can only be given the door's own pid and port.
    /// Mutation: call `sink.hello` with a key built from anything but the one `open` was given, or not at all, and this goes red.
    #[tokio::test]
    async fn the_sink_is_told_hello_under_the_door_it_was_opened_for() {
        let (port, door) = fake_door(hello_frame(), true).await;
        let recorder = Arc::new(Recorder::default());
        let key = DoorKey { pid: 4242, port };
        let (watcher, hello) = open(key, Arc::from("t"), Duration::from_secs(5), recorder.clone()).await.expect("the door said hello");
        assert_eq!(hello.session, PathBuf::from("/logs/a.eid"));
        assert_eq!(*recorder.0.lock().unwrap(), vec![(key, PathBuf::from("/logs/a.eid"))], "told once, under the door's key");
        // The close is still the first step of a stop: it waits until the stream is gone.
        watcher.close().await;
        tokio::time::timeout(Duration::from_secs(5), door).await.expect("the door saw its stream end").unwrap();
    }

    /// A sink that panics in `hello`, or in `watching` after a `hello` that went fine, and writes down what it was told.
    struct Breaks {
        in_watching: bool,
        told: Mutex<Vec<&'static str>>,
    }

    impl Sink for Breaks {
        fn hello(&self, _key: DoorKey, _hello: &Hello) {
            self.told.lock().unwrap().push("hello");
            if !self.in_watching {
                panic!("a sink that breaks");
            }
        }
        fn watching(&self, _key: DoorKey, _asks: AskView) {
            self.told.lock().unwrap().push("watching");
            panic!("a sink that breaks");
        }
        fn closed(&self, _key: DoorKey) {
            self.told.lock().unwrap().push("closed");
        }
    }

    /// A panic in the sink's `hello` or `watching` makes `open` an `Err` (so `door_up`'s Err arm signals the door), the stream it opened is
    /// closed (the fake door holds its end open until the perch closes its own, so its read end seeing that is the proof), and the sink,
    /// which was told `hello`, is told `closed`. Mutation: let the panic through (drop the `catch_unwind`) and `open` panics instead of
    /// failing; forget the close and the door is never released, so the wait below runs out.
    #[tokio::test]
    async fn a_sink_that_panics_when_told_fails_open_and_does_not_leave_the_stream_open() {
        for in_watching in [false, true] {
            let (port, door) = fake_door(hello_frame(), true).await;
            let key = DoorKey { pid: 7, port };
            let sink = Arc::new(Breaks { in_watching, told: Mutex::new(Vec::new()) });
            let opened = tokio::spawn(open(key, Arc::from("t"), WAIT, sink.clone())).await;
            let err = opened.expect("open fails rather than panics").err().expect("open fails");
            assert!(err.to_string().contains("panicked"), "{err}");
            tokio::time::timeout(WAIT, door).await.expect("the door saw its stream end").unwrap();
            let want: &[&str] = if in_watching { &["hello", "watching", "closed"] } else { &["hello", "closed"] };
            assert!(eventually(WAIT, || *sink.told.lock().unwrap() == want).await, "{:?}", sink.told.lock().unwrap());
        }
    }

    #[tokio::test]
    async fn a_door_that_never_says_hello_tells_the_sink_nothing() {
        let (port, door) = fake_door(frame(json!({"type": "caught-up"})), false).await;
        let recorder = Arc::new(Recorder::default());
        let key = DoorKey { pid: 1, port };
        let err = open(key, Arc::from("t"), Duration::from_secs(5), recorder.clone()).await.err().expect("no hello, no watcher");
        assert!(err.to_string().contains("before hello"), "{err}");
        assert!(recorder.0.lock().unwrap().is_empty(), "nothing to tell");
        door.await.unwrap();
    }

    /// What a scripted door does on one connection, after the response head.
    enum Step {
        Send(Vec<u8>),
        Wait(u64),
        /// Keep the stream open until the peer closes it.
        Hold,
        /// Close it now.
        Close,
    }

    /// What a scripted door does with the connections that come after its scripts have run out.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Tail {
        /// Say `hello` and hold the stream open.
        Hold,
        /// Stop listening: a connect is refused.
        Refuse,
        /// Accept and close at once, before a response head.
        Drop,
        /// Accept and read the request, and never answer it.
        Silent,
    }

    /// A door that takes connections one after another, the nth running the nth script, and then does what its [`Tail`] says.
    struct Door {
        port: u16,
        accepted: Arc<AtomicUsize>,
        /// Connections that have ended, by either side.
        ended: Arc<AtomicUsize>,
        at: Arc<Mutex<Vec<Instant>>>,
        _task: tokio::task::JoinHandle<()>,
    }

    impl Door {
        fn accepted(&self) -> usize {
            self.accepted.load(Ordering::SeqCst)
        }

        fn ended(&self) -> usize {
            self.ended.load(Ordering::SeqCst)
        }

        fn key(&self) -> DoorKey {
            // The watcher never signals this pid, and reconnecting does not look at it.
            DoorKey { pid: std::process::id(), port: self.port }
        }
    }

    async fn door(scripts: Vec<Vec<Step>>) -> Door {
        door_then(scripts, Tail::Hold).await
    }

    async fn door_then(scripts: Vec<Vec<Step>>, tail: Tail) -> Door {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (accepted, ended, at) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)), Arc::new(Mutex::new(Vec::new())));
        let task = tokio::spawn({
            let (accepted, ended, at) = (accepted.clone(), ended.clone(), at.clone());
            async move {
                let mut scripts: VecDeque<_> = scripts.into();
                loop {
                    if scripts.is_empty() && tail == Tail::Refuse {
                        // The listener goes with this task: the next connect is refused.
                        break;
                    }
                    let Ok((sock, _)) = listener.accept().await else { break };
                    accepted.fetch_add(1, Ordering::SeqCst);
                    at.lock().unwrap().push(Instant::now());
                    match scripts.pop_front() {
                        Some(steps) => drop(tokio::spawn(run_script(sock, steps, ended.clone()))),
                        None => match tail {
                            Tail::Drop => {
                                drop(sock);
                                ended.fetch_add(1, Ordering::SeqCst);
                            }
                            Tail::Silent => drop(tokio::spawn(run_silent(sock, ended.clone()))),
                            Tail::Hold | Tail::Refuse => {
                                let steps = vec![Step::Send(hello_frame()), Step::Hold];
                                drop(tokio::spawn(run_script(sock, steps, ended.clone())));
                            }
                        },
                    }
                }
            }
        });
        Door { port, accepted, ended, at, _task: task }
    }

    /// Read the request and then say nothing, until the peer goes.
    async fn run_silent(mut sock: TcpStream, ended: Arc<AtomicUsize>) {
        let mut buf = vec![0u8; 4096];
        while sock.read(&mut buf).await.is_ok_and(|n| n > 0) {}
        ended.fetch_add(1, Ordering::SeqCst);
    }

    async fn run_script(mut sock: TcpStream, steps: Vec<Step>, ended: Arc<AtomicUsize>) {
        let mut buf = vec![0u8; 4096];
        let _ = sock.read(&mut buf).await;
        let _ = sock.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n").await;
        for step in steps {
            match step {
                Step::Send(bytes) => {
                    if sock.write_all(&bytes).await.is_err() {
                        break;
                    }
                }
                Step::Wait(ms) => tokio::time::sleep(Duration::from_millis(ms)).await,
                Step::Hold => {
                    while sock.read(&mut buf).await.is_ok_and(|n| n > 0) {}
                    break;
                }
                Step::Close => break,
            }
        }
        ended.fetch_add(1, Ordering::SeqCst);
    }

    fn send(frames: Vec<Vec<u8>>) -> Step {
        Step::Send(frames.concat())
    }

    #[derive(Debug, Clone, PartialEq)]
    enum Ev {
        Hello(DoorKey),
        Ask(u64, Option<Origin>),
        Settled(u64),
        Closed,
    }

    /// A sink that writes down what it was told, in order, and keeps the view it was given.
    #[derive(Default)]
    struct Log {
        events: Mutex<Vec<Ev>>,
        view: Mutex<Option<AskView>>,
    }

    impl Log {
        fn events(&self) -> Vec<Ev> {
            self.events.lock().unwrap().clone()
        }

        fn view(&self) -> AskView {
            self.view.lock().unwrap().clone().expect("the sink was given a view")
        }
    }

    impl Sink for Log {
        fn hello(&self, key: DoorKey, _hello: &Hello) {
            self.events.lock().unwrap().push(Ev::Hello(key));
        }
        fn watching(&self, _key: DoorKey, asks: AskView) {
            *self.view.lock().unwrap() = Some(asks);
        }
        fn ask(&self, _key: DoorKey, ask: &AskNote) {
            self.events.lock().unwrap().push(Ev::Ask(ask.ask_id, ask.origin));
        }
        fn settled(&self, _key: DoorKey, ask_id: u64) {
            self.events.lock().unwrap().push(Ev::Settled(ask_id));
        }
        fn closed(&self, _key: DoorKey) {
            self.events.lock().unwrap().push(Ev::Closed);
        }
    }

    async fn eventually(within: Duration, mut done: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + within;
        while !done() {
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        true
    }

    const WAIT: Duration = Duration::from_secs(5);

    /// The order the sink is told in: `hello` first, whatever else the door wrote in the same read, and then the asks, in the door's order.
    /// The sink's `hello` naps first, on a runtime with a second thread, so an ask that was let through early would be recorded before it.
    /// Mutation: let the delivery task run before `open` has told `hello` and this goes red.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_sink_hears_hello_before_an_ask_that_came_in_the_same_read() {
        struct NapsOnHello(Arc<Log>);
        impl Sink for NapsOnHello {
            fn hello(&self, key: DoorKey, hello: &Hello) {
                std::thread::sleep(Duration::from_millis(300));
                self.0.hello(key, hello);
            }
            fn watching(&self, key: DoorKey, asks: AskView) {
                self.0.watching(key, asks);
            }
            fn ask(&self, key: DoorKey, ask: &AskNote) {
                self.0.ask(key, ask);
            }
        }
        let one_write = [hello_frame(), started("c1", "model"), approval(1, "c1", json!({"command": "ls"})), bare("caught-up")].concat();
        let door = door(vec![vec![Step::Send(one_write), Step::Hold]]).await;
        let log = Arc::new(Log::default());
        let (watcher, _) = open(door.key(), Arc::from("t"), WAIT, Arc::new(NapsOnHello(log.clone()))).await.unwrap();
        assert!(eventually(WAIT, || log.events().len() == 2).await, "{:?}", log.events());
        assert_eq!(log.events(), [Ev::Hello(door.key()), Ev::Ask(1, Some(Origin::Model))]);
        assert_eq!(log.view().ask(1).map(|ask| ask.seen_seq), Some(1), "the view the sink was handed sees the same book");
        watcher.close().await;
    }

    /// The reconnect: `lagged` ends the stream, a new one is open within a second, its replay of the same asks reaches the sink as
    /// nothing new, and `turn-state` is as the door last said. Mutation: never reconnect, or tell the sink every replayed ask again, or
    /// reset `running` on a reconnect, and this goes red.
    #[tokio::test]
    async fn a_lagged_stream_is_reopened_within_a_second_and_replayed_asks_reach_the_sink_once() {
        let first = vec![
            send(vec![
                hello_frame(),
                started("c1", "model"),
                approval(1, "c1", json!({"command": "ls"})),
                approval(2, "c1", json!({"command": "pwd"})),
                turn_state(true),
                frame(json!({"type": "lagged", "dropped": 9})),
            ]),
            Step::Hold,
        ];
        // The real door's replay: the branch's model calls, the asks still pending, caught-up. Then, later, the turn ends.
        let second = vec![
            send(vec![
                hello_frame(),
                started("c1", "model"),
                approval(1, "c1", json!({"command": "ls"})),
                approval(2, "c1", json!({"command": "pwd"})),
                bare("caught-up"),
            ]),
            Step::Wait(300),
            send(vec![turn_state(false)]),
            Step::Hold,
        ];
        let door = door(vec![first, second]).await;
        let log = Arc::new(Log::default());
        let (watcher, _) = open(door.key(), Arc::from("t"), WAIT, log.clone()).await.unwrap();
        assert!(eventually(WAIT, || door.accepted() == 2).await, "the watcher opened another stream");
        let at = door.at.lock().unwrap().clone();
        assert!(at[1] - at[0] < Duration::from_secs(1), "reopened within a second: {:?}", at[1] - at[0]);
        assert!(eventually(WAIT, || door.ended() == 1).await, "the lagged stream was closed from this side");
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(watcher.running(), "turn-state carries across the gap");
        assert!(eventually(WAIT, || !watcher.running()).await, "and the new stream's turn-state is read");
        assert_eq!(
            log.events(),
            [Ev::Hello(door.key()), Ev::Ask(1, Some(Origin::Model)), Ev::Ask(2, Some(Origin::Model))],
            "each ask once, though two streams listed them"
        );
        assert_eq!(door.accepted(), 2, "and one reconnect");
        watcher.close().await;
        assert!(eventually(WAIT, || log.events().last() == Some(&Ev::Closed)).await, "{:?}", log.events());
    }

    /// An ask the replay does not list was settled in the gap: the sink is told, and the view no longer has it.
    #[tokio::test]
    async fn an_ask_the_replay_leaves_out_is_settled_for_the_sink() {
        let first = vec![
            send(vec![
                hello_frame(),
                started("c1", "model"),
                approval(1, "c1", json!({"command": "a"})),
                approval(2, "c1", json!({"command": "b"})),
                bare("lagged"),
            ]),
            Step::Hold,
        ];
        let replay = vec![hello_frame(), started("c1", "model"), approval(2, "c1", json!({"command": "b"})), bare("caught-up")];
        let second = vec![send(replay), Step::Hold];
        let door = door(vec![first, second]).await;
        let log = Arc::new(Log::default());
        let (watcher, _) = open(door.key(), Arc::from("t"), WAIT, log.clone()).await.unwrap();
        assert!(eventually(WAIT, || log.events().contains(&Ev::Settled(1))).await, "{:?}", log.events());
        assert_eq!(log.view().asks().iter().map(|a| a.ask_id).collect::<Vec<_>>(), [2]);
        watcher.close().await;
    }

    /// A stop that lands while the watcher is pausing before a reconnect: `close` returns at once, and nothing is ever opened after it
    /// (hub.md §2, the Stop finding: a stream opened behind a close would hold the door's SIGTERM). Mutation: pause with a plain sleep, or
    /// check the close flag only after the connect, and this goes red.
    #[tokio::test]
    async fn a_close_during_the_pause_never_reopens() {
        let door = door(vec![vec![send(vec![hello_frame()]), Step::Close]]).await;
        let log = Arc::new(Log::default());
        let (watcher, _) = open(door.key(), Arc::from("t"), WAIT, log.clone()).await.unwrap();
        assert!(eventually(WAIT, || door.ended() == 1).await, "the door ended the stream");
        tokio::time::sleep(Duration::from_millis(60)).await;
        let began = Instant::now();
        watcher.close().await;
        assert!(began.elapsed() < Duration::from_millis(150), "close does not wait out the pause: {:?}", began.elapsed());
        tokio::time::sleep(Duration::from_millis(700)).await;
        assert_eq!(door.accepted(), 1, "no connection was made after the close");
    }

    /// hub.md §2: the stop sequence signals the door right after `close` returns, so the stream has to be closed by then, not about to be.
    /// One runtime thread, so a `close` that only asked the task to stop would return with the socket still open; the peer looks at its end
    /// with a `peek` that does not wait. Mutation: let `close` return without awaiting the task and this goes red.
    #[tokio::test(flavor = "current_thread")]
    async fn close_returns_with_the_stream_already_closed() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let key = DoorKey { pid: std::process::id(), port: listener.local_addr().unwrap().port() };
        let peer = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let head = b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n";
            sock.write_all(head).await.unwrap();
            sock.write_all(&hello_frame()).await.unwrap();
            sock
        });
        let (watcher, _) = open(key, Arc::from("t"), WAIT, Arc::new(Log::default())).await.unwrap();
        let peer = peer.await.unwrap().into_std().unwrap();
        let mut one = [0u8; 1];
        assert!(peer.peek(&mut one).is_err_and(|e| e.kind() == std::io::ErrorKind::WouldBlock), "the stream is open and quiet before the close");
        watcher.close().await;
        assert!(matches!(peer.peek(&mut one), Ok(0)), "the watcher's end was closed by the time `close` returned");
    }

    /// A close while the first stream is open ends it before `close` returns, and a dropped watcher does the same without waiting.
    #[tokio::test]
    async fn closing_or_dropping_a_watcher_ends_its_stream() {
        let door = door(vec![]).await;
        let (watcher, _) = open(door.key(), Arc::from("t"), WAIT, Arc::new(Log::default())).await.unwrap();
        watcher.close().await;
        assert!(eventually(WAIT, || door.ended() == 1).await);
        let (watcher, _) = open(door.key(), Arc::from("t"), WAIT, Arc::new(Log::default())).await.unwrap();
        drop(watcher);
        assert!(eventually(WAIT, || door.ended() == 2).await);
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(door.accepted(), 2, "and neither came back");
    }

    /// A sink that takes two seconds over an ask, on a runtime with one thread: if the reader ever waited for it, nothing below could run.
    /// The frames after the ask are read while it sleeps, and a close does not wait for it. Mutation: call the sink inline, from the
    /// read loop, and this goes red.
    #[tokio::test(flavor = "current_thread")]
    async fn a_slow_sink_never_stalls_the_reader_or_a_close() {
        #[derive(Default)]
        struct Slow {
            began: AtomicBool,
            done: AtomicBool,
        }
        impl Sink for Slow {
            fn ask(&self, _key: DoorKey, _ask: &AskNote) {
                self.began.store(true, Ordering::SeqCst);
                std::thread::sleep(Duration::from_secs(2));
                self.done.store(true, Ordering::SeqCst);
            }
        }
        let script = vec![
            send(vec![hello_frame(), started("c1", "model"), approval(1, "c1", json!({"command": "ls"}))]),
            Step::Wait(100),
            send(vec![turn_state(true)]),
            Step::Wait(100),
            send(vec![turn_state(false)]),
            Step::Wait(100),
            send(vec![turn_state(true)]),
            Step::Hold,
        ];
        let door = door(vec![script]).await;
        let sink = Arc::new(Slow::default());
        let (watcher, _) = open(door.key(), Arc::from("t"), WAIT, sink.clone()).await.unwrap();
        assert!(eventually(WAIT, || sink.began.load(Ordering::SeqCst)).await, "the sink was called");
        let began = Instant::now();
        assert!(eventually(Duration::from_millis(1500), || watcher.running()).await, "turn-state is read while the sink sleeps");
        assert!(!sink.done.load(Ordering::SeqCst), "and the sink is still asleep: {:?}", began.elapsed());
        let closing = Instant::now();
        watcher.close().await;
        assert!(closing.elapsed() < Duration::from_millis(500), "close did not wait for the sink: {:?}", closing.elapsed());
        assert!(!sink.done.load(Ordering::SeqCst));
    }

    /// A sink that panics on one event loses that event and nothing else: the next is delivered, and the stream is read as before.
    #[tokio::test]
    async fn a_sink_that_panics_on_an_ask_loses_that_ask_only() {
        #[derive(Default)]
        struct Flaky(Mutex<Vec<u64>>);
        impl Sink for Flaky {
            fn ask(&self, _key: DoorKey, ask: &AskNote) {
                if ask.ask_id == 1 {
                    panic!("a sink that breaks on the first ask");
                }
                self.0.lock().unwrap().push(ask.ask_id);
            }
        }
        let script = vec![
            send(vec![
                hello_frame(),
                started("c1", "model"),
                approval(1, "c1", json!({"command": "a"})),
                approval(2, "c1", json!({"command": "b"})),
                turn_state(true),
            ]),
            Step::Hold,
        ];
        let door = door(vec![script]).await;
        let sink = Arc::new(Flaky::default());
        let (watcher, _) = open(door.key(), Arc::from("t"), WAIT, sink.clone()).await.unwrap();
        assert!(eventually(WAIT, || *sink.0.lock().unwrap() == [2]).await, "{:?}", sink.0.lock().unwrap());
        assert!(eventually(WAIT, || watcher.running()).await);
        watcher.close().await;
    }

    /// A line past [`FRAME_MAX`] is dropped like any other long line, counted, and the stream is kept: the ask and the turn-state after it
    /// are read on the same connection. (Ending the stream there would hand a door a way to make the watcher reconnect forever, and a
    /// reconnect is a replay.) Mutation: end the stream at FRAME_MAX again and the ask is never read from this stream, and a second
    /// connection is made.
    #[tokio::test]
    async fn a_line_past_the_frame_bound_is_dropped_and_the_stream_is_kept() {
        let mut huge = b"data: ".to_vec();
        huge.resize(6 + FRAME_MAX + 4096, b'x');
        huge.extend_from_slice(b"\n\n");
        let after = vec![started("c1", "model"), approval(1, "c1", json!({"command": "ls"})), turn_state(true)];
        let first = vec![send(vec![hello_frame()]), Step::Send(huge), send(after), Step::Hold];
        let door = door(vec![first]).await;
        let log = Arc::new(Log::default());
        let (watcher, _) = open(door.key(), Arc::from("t"), WAIT, log.clone()).await.unwrap();
        assert!(eventually(WAIT, || log.events().len() == 2).await, "the ask after the long line was read: {:?}", log.events());
        assert_eq!(log.events()[1], Ev::Ask(1, Some(Origin::Model)));
        assert!(eventually(WAIT, || watcher.running()).await, "and so was the turn-state");
        assert_eq!(door.accepted(), 1, "on the one connection");
        assert_eq!(door.ended(), 0, "which is still open");
        watcher.close().await;
    }

    /// A reconnect that finds another session on the door's port stops there: nothing it says is read, and no third connection is made.
    #[tokio::test]
    async fn a_reconnect_that_answers_as_another_session_is_dropped() {
        let first = vec![send(vec![hello_frame(), bare("lagged")]), Step::Hold];
        let stranger = frame(json!({"type": "hello", "session": "/logs/other.eid", "cwd": "/work", "model": "mock"}));
        let second = vec![send(vec![stranger, started("c9", "model"), approval(9, "c9", json!({"command": "rm -rf /"}))]), Step::Hold];
        let door = door(vec![first, second]).await;
        let log = Arc::new(Log::default());
        let (watcher, _) = open(door.key(), Arc::from("t"), WAIT, log.clone()).await.unwrap();
        assert!(eventually(WAIT, || door.accepted() == 2).await);
        assert!(eventually(WAIT, || log.events().last() == Some(&Ev::Closed)).await, "the watcher gave up: {:?}", log.events());
        tokio::time::sleep(Duration::from_millis(800)).await;
        assert_eq!(door.accepted(), 2);
        assert_eq!(log.events(), [Ev::Hello(door.key()), Ev::Closed], "the stranger's ask was never read");
        watcher.close().await;
    }

    /// A door that says `goodbye` is on its way out: the watcher ends its stream and stops (no reconnect to a door that is leaving; the
    /// reap settles the row), the sink is told `closed`, and the view says so. Mutation: reconnect after a goodbye (the shape this
    /// replaced) and the door sees a second connection.
    #[tokio::test]
    async fn a_goodbye_stops_the_watcher_and_the_sink_hears_closed() {
        let first = vec![
            send(vec![hello_frame(), started("c1", "model"), approval(1, "c1", json!({"command": "ls"}))]),
            Step::Wait(50),
            send(vec![frame(json!({"type": "goodbye", "text": "quiescing"}))]),
            Step::Hold,
        ];
        let door = door(vec![first]).await;
        let log = Arc::new(Log::default());
        let (watcher, _) = open(door.key(), Arc::from("t"), WAIT, log.clone()).await.unwrap();
        assert!(eventually(WAIT, || log.events().last() == Some(&Ev::Closed)).await, "{:?}", log.events());
        assert!(eventually(WAIT, || door.ended() == 1).await, "the stream was closed from this side");
        tokio::time::sleep(Duration::from_millis(700)).await;
        assert_eq!(door.accepted(), 1, "and not opened again");
        assert_eq!(log.events(), [Ev::Hello(door.key()), Ev::Ask(1, Some(Origin::Model)), Ev::Closed]);
        let view = log.view();
        assert!(view.is_closed() && view.asks().is_empty() && view.ask(1).is_none(), "nothing is held once the watcher is over");
        watcher.close().await;
        assert_eq!(log.events().iter().filter(|ev| **ev == Ev::Closed).count(), 1, "closed once, though `close` came after");
    }

    /// A door that stops answering ends the watcher after [`GIVE_UP_AFTER`] tries in a row, and the sink hears `closed` when it does, with
    /// nothing left in the view, and a `close` after is prompt. Both ways a door can stop: it refuses, or it accepts and drops the
    /// connection before a response. Mutation: give up without telling the sink, or never give up, and this goes red.
    #[tokio::test]
    async fn a_door_that_stops_answering_ends_the_watcher_and_the_sink_hears_closed() {
        for tail in [Tail::Refuse, Tail::Drop] {
            let first = vec![send(vec![hello_frame(), started("c1", "model"), approval(1, "c1", json!({"command": "ls"}))]), Step::Close];
            let door = door_then(vec![first], tail).await;
            let log = Arc::new(Log::default());
            let (key, pace) = (door.key(), (Duration::from_millis(1), Duration::from_millis(4)));
            let (watcher, _) = open_paced(key, Arc::from("t"), WAIT, log.clone(), pace).await.unwrap();
            assert!(eventually(WAIT, || log.events().last() == Some(&Ev::Closed)).await, "{:?}", log.events());
            assert_eq!(log.events(), [Ev::Hello(key), Ev::Ask(1, Some(Origin::Model)), Ev::Closed]);
            if tail == Tail::Drop {
                assert_eq!(door.accepted(), 1 + GIVE_UP_AFTER as usize, "one stream, then {GIVE_UP_AFTER} tries that were dropped");
            } else {
                assert_eq!(door.accepted(), 1, "one stream, then {GIVE_UP_AFTER} refusals");
            }
            assert!(log.view().is_closed() && log.view().asks().is_empty());
            let began = Instant::now();
            watcher.close().await;
            assert!(began.elapsed() < Duration::from_millis(200), "close after the give-up is prompt: {:?}", began.elapsed());
            assert_eq!(log.events().last(), Some(&Ev::Closed));
        }
    }

    /// A door that keeps lagging says `hello` every time and is never given up on: the end of a stream that spoke is not a failure to
    /// reach the door. Mutation: count every end toward giving up and the watcher stops at the twelfth, and the sink hears `closed`.
    #[tokio::test]
    async fn a_door_that_keeps_lagging_is_never_given_up_on() {
        let lag = || vec![send(vec![hello_frame(), bare("lagged")]), Step::Hold];
        let door = door((0..(2 * GIVE_UP_AFTER)).map(|_| lag()).collect()).await;
        let log = Arc::new(Log::default());
        let pace = (Duration::from_millis(1), Duration::from_millis(4));
        let (watcher, _) = open_paced(door.key(), Arc::from("t"), WAIT, log.clone(), pace).await.unwrap();
        assert!(eventually(Duration::from_secs(10), || door.accepted() > 2 * GIVE_UP_AFTER as usize).await, "{}", door.accepted());
        assert!(!log.events().contains(&Ev::Closed), "{:?}", log.events());
        watcher.close().await;
        assert!(eventually(WAIT, || log.events().last() == Some(&Ev::Closed)).await);
    }

    /// A reconnect to a door that accepts and never writes a response head does not hold up a `close`: every wait on the door is a
    /// select against the close flag, the wait for the response head included. Mutation: wait on `send_request` without the select and
    /// `close` never returns.
    #[tokio::test]
    async fn a_close_while_a_reconnect_waits_on_a_silent_door_returns_promptly() {
        let first = vec![send(vec![hello_frame()]), Step::Close];
        let door = door_then(vec![first], Tail::Silent).await;
        let log = Arc::new(Log::default());
        let pace = (Duration::from_millis(1), Duration::from_millis(4));
        let (watcher, _) = open_paced(door.key(), Arc::from("t"), WAIT, log.clone(), pace).await.unwrap();
        assert!(eventually(WAIT, || door.accepted() == 2).await, "the reconnect reached the silent door");
        tokio::time::sleep(Duration::from_millis(100)).await;
        let began = Instant::now();
        tokio::time::timeout(Duration::from_millis(500), watcher.close()).await.expect("close returned while the door said nothing");
        assert!(began.elapsed() < Duration::from_millis(500), "{:?}", began.elapsed());
        assert!(eventually(WAIT, || door.ended() == 2).await, "and the silent connection was closed from this side");
        assert!(eventually(WAIT, || log.events().last() == Some(&Ev::Closed)).await);
    }

    /// What the tracing in a test says, as text.
    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl Captured {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    impl std::io::Write for Captured {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
        type Writer = Captured;

        fn make_writer(&'a self) -> Captured {
            self.clone()
        }
    }

    /// A sink's panic is logged by the fact and the event it was on, never by its message, which may say anything (a command line, a
    /// token). The runtime has one thread, so the delivery task logs on this test's own. Mutation: log the `JoinError`, whose `Display`
    /// carries the panic's message, and the secret is in the log.
    #[tokio::test]
    async fn a_sink_panic_is_logged_without_its_message() {
        struct Leaky;
        impl Sink for Leaky {
            fn ask(&self, _key: DoorKey, _ask: &AskNote) {
                panic!("the secret in the panic message");
            }
        }
        let captured = Captured::default();
        let subscriber = tracing_subscriber::fmt().with_writer(captured.clone()).with_ansi(false).finish();
        let _logging = tracing::subscriber::set_default(subscriber);
        let script = vec![send(vec![hello_frame(), started("c1", "model"), approval(1, "c1", json!({"command": "ls"}))]), Step::Hold];
        let door = door(vec![script]).await;
        let (watcher, _) = open(door.key(), Arc::from("t"), WAIT, Arc::new(Leaky)).await.unwrap();
        assert!(eventually(WAIT, || captured.text().contains("a sink panicked on an event")).await, "{}", captured.text());
        let logged = captured.text();
        assert!(logged.contains("error=\"panic\"") && logged.contains("event=\"ask\""), "{logged}");
        assert!(!logged.contains("secret"), "the panic's message is nobody's to log: {logged}");
        watcher.close().await;
    }

    /// A sink that stops in `ask` and `settled` until it is let go, and writes down what it hears once it is.
    #[derive(Default)]
    struct Stalled {
        go: AtomicBool,
        log: Log,
    }

    impl Stalled {
        fn wait(&self) {
            while !self.go.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    impl Sink for Stalled {
        fn hello(&self, key: DoorKey, hello: &Hello) {
            self.log.hello(key, hello);
        }
        fn watching(&self, key: DoorKey, asks: AskView) {
            self.log.watching(key, asks);
        }
        fn ask(&self, key: DoorKey, ask: &AskNote) {
            self.wait();
            self.log.ask(key, ask);
        }
        fn settled(&self, key: DoorKey, ask_id: u64) {
            self.wait();
            self.log.settled(key, ask_id);
        }
        fn closed(&self, key: DoorKey) {
            self.log.closed(key);
        }
    }

    fn settled_frame(ask_id: u64) -> Vec<u8> {
        frame(json!({"type": "ask-settled", "ask_id": ask_id, "how": "answered", "answer": "no"}))
    }

    /// `closed` is not an event in the queue, so a full queue cannot lose it: a stalled sink with more than [`SINK_QUEUE`] events behind
    /// it (the rest are dropped) still hears `closed`, once and last, when it is let go. And `close` does not wait for the sink.
    /// Mutation: send `Closed` through the queue with `try_send` and this goes red.
    #[tokio::test]
    async fn a_stalled_sink_with_a_full_queue_still_hears_closed() {
        const PAIRS: u64 = 200;
        const _: () = assert!(2 * PAIRS as usize > SINK_QUEUE + 1, "more events than the queue holds");
        let mut script = vec![hello_frame(), started("c1", "model")];
        for id in 1..=PAIRS {
            script.push(approval(id, "c1", json!({"command": "ls"})));
            script.push(settled_frame(id));
        }
        script.push(turn_state(true));
        let door = door(vec![vec![send(script), Step::Hold]]).await;
        let sink = Arc::new(Stalled::default());
        let (watcher, _) = open(door.key(), Arc::from("t"), WAIT, sink.clone()).await.unwrap();
        assert!(eventually(WAIT, || watcher.running()).await, "the whole script was read while the sink stood still");
        let began = Instant::now();
        tokio::time::timeout(WAIT, watcher.close()).await.expect("close does not wait for the sink");
        assert!(began.elapsed() < Duration::from_millis(500), "{:?}", began.elapsed());
        assert!(!sink.log.events().contains(&Ev::Closed), "the sink is still stalled");
        sink.go.store(true, Ordering::SeqCst);
        assert!(eventually(WAIT, || sink.log.events().last() == Some(&Ev::Closed)).await, "{:?}", sink.log.events().last());
        let events = sink.log.events();
        assert_eq!(events.iter().filter(|ev| **ev == Ev::Closed).count(), 1, "once");
        assert_eq!(events[0], Ev::Hello(door.key()));
        assert!(events.len() <= SINK_QUEUE + 3, "what did not fit was dropped: {}", events.len());
    }

    /// An ask that is still queued behind a stalled sink when the watcher is closed is never delivered: a sink that learns of an ask
    /// after `close` could act on a door that is being stopped. Mutation: drop the close check in `deliver` and `Ask(2)` is heard.
    #[tokio::test]
    async fn asks_queued_behind_a_stalled_sink_are_dropped_once_the_watcher_is_closed() {
        let script = vec![
            send(vec![hello_frame(), started("c1", "model"), approval(1, "c1", json!({"command": "a"}))]),
            Step::Wait(50),
            send(vec![approval(2, "c1", json!({"command": "b"})), turn_state(true)]),
            Step::Hold,
        ];
        let door = door(vec![script]).await;
        let sink = Arc::new(Stalled::default());
        let (watcher, _) = open(door.key(), Arc::from("t"), WAIT, sink.clone()).await.unwrap();
        assert!(eventually(WAIT, || watcher.running()).await, "both asks were read; the first is in the sink's hands");
        watcher.close().await;
        sink.go.store(true, Ordering::SeqCst);
        assert!(eventually(WAIT, || sink.log.events().last() == Some(&Ev::Closed)).await, "{:?}", sink.log.events());
        assert_eq!(sink.log.events(), [Ev::Hello(door.key()), Ev::Ask(1, Some(Origin::Model)), Ev::Closed]);
    }
}
