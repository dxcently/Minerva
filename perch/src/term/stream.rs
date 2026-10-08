//! `GET /hub/term/events?since=t1:4096,t3:0` (slice T1c): the one SSE every terminal rides, ending when the perch stops (`ctx.stop`).
//!
//! The first frame is `term-list`, the roster [`Terms::list`] gives. Then every live terminal joins:
//!
//! - a terminal named in `since` whose seq is still in its ring resumes as `term-out` from that seq;
//! - any other terminal gets `term-reset {id, seq, data, snap}`: its whole ring when the ring still starts at byte 0 (an exact replay,
//!   `snap: false`), else the screen rebuilt from the alacritty copy (`snap: true`, [`super::snap`]).
//!
//! After that the stream follows the output (`term-out`, flushed on a tick at most once per [`COALESCE`] and cut into frames of at most
//! [`FRAME_RAW`] bytes) and the changes, each of which flushes the output before it at once: `term-open`, `term-size`, `term-title`,
//! `term-hold`, `term-exit`, `term-closed`, `term-input`.
//! A reset that is longer than one frame goes out as its `term-reset` and then `term-out` frames carrying the reset's seq; the page
//! writes them in order and its cursor stays where the reset left it. A `:` keep-alive line goes out after [`KEEP_ALIVE`] of silence
//! (`--knob term_keepalive=N`, in milliseconds, for a test).
//!
//! Each connection keeps its own cursor per terminal. A cursor the ring has passed gets a `term-reset`; a listener that fell behind the
//! change channel gets a fresh `term-list`, and the terminals are reconciled against it. The body ends with
//! `hub-stream-end {reason: "stopping"}` the moment `ctx.stop` fires, so a perch stop never waits for a stream (serve.rs's drain).

use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::BodyExt as _;
use hyper::body::{Body, Frame, Incoming};
use hyper::header::{ALLOW, CACHE_CONTROL, CONTENT_TYPE, HeaderValue};
use hyper::{Method, Request, Response, StatusCode};
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::time::Instant;
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};

use super::pty::{Read, Terminal};
use super::screen::shown;
use super::snap;
use super::{By, Change, MAX_TERMS, Terms, valid_id};
use crate::serve::{BoxBody, Ctx, problem, status};

/// Output is flushed at most once in this long: a burst of reads is one flush, and one flush is at most [`FRAME_RAW`] bytes a frame.
const COALESCE: Duration = Duration::from_millis(16);
/// The most raw bytes one frame carries (`term-out`, `term-reset`).
const FRAME_RAW: usize = 32 * 1024;
/// The keep-alive when `--knob term_keepalive` names none, and the range a knob may ask for.
const KEEP_ALIVE: Duration = Duration::from_secs(15);
const KEEP_ALIVE_RANGE_MS: (u64, u64) = (10, 60_000);
/// Frames the producer may get ahead of the client (each is at most about 43 KiB of base64).
const IN_FLIGHT: usize = 32;
/// What a page may see of one line of typed input: this many characters, the last one an ellipsis when cut.
const INPUT_CHARS: usize = 200;
/// What `note_input` keeps of the text: the same as `agent_input`'s announcement (80 characters and an ellipsis).
const NOTED_CHARS: usize = 80;

/// `rest` is always `""`; the query (`since`) is on the request.
pub async fn events(ctx: &Ctx, req: Request<Incoming>, _rest: &str) -> Response<BoxBody> {
    if *req.method() != Method::GET {
        let mut refusal = status(StatusCode::METHOD_NOT_ALLOWED);
        refusal.headers_mut().insert(ALLOW, HeaderValue::from_static("GET"));
        return refusal;
    }
    let since = match since_of(req.uri().query()) {
        Ok(since) => since,
        Err(refusal) => return *refusal,
    };
    // Subscribed before the roster is read in the producer: a change between the two is seen twice at worst, never not at all.
    let terms = ctx.term.terms(ctx);
    let ticks = terms.ticks();
    let changes = terms.subscribe();
    let (tx, rx) = mpsc::channel(IN_FLIGHT);
    tokio::spawn(produce(ctx.clone(), ticks, changes, since, tx));
    let body = Frames { rx, stop: Box::pin(ctx.stop.clone().cancelled_owned()), done: false };
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/event-stream")
        .header(CACHE_CONTROL, "no-store")
        .body(body.boxed_unsync())
        .unwrap()
}

/// `?since=t1:4096,t3:0`: the seq each terminal's page has. A malformed entry is a 400 with its reason; a repeated `since` is one too.
/// A terminal named twice keeps its first seq, and entries past what the table can hold (16) are dropped: the terminals they would have
/// named are then reset, which is always right, only more bytes.
fn since_of(query: Option<&str>) -> Result<Vec<(String, u64)>, Box<Response<BoxBody>>> {
    let mut given = None;
    for pair in query.unwrap_or("").split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if key == "since" && given.replace(value).is_some() {
            return Err(Box::new(problem(StatusCode::BAD_REQUEST, "since is given once")));
        }
    }
    let Some(value) = given else { return Ok(Vec::new()) };
    let mut since: Vec<(String, u64)> = Vec::new();
    for entry in value.split(',').filter(|entry| !entry.is_empty()) {
        let shaped = entry.split_once(':').filter(|(id, seq)| {
            valid_id(id) && !seq.is_empty() && seq.len() <= 20 && seq.bytes().all(|b| b.is_ascii_digit())
        });
        let Some((id, seq)) = shaped else {
            return Err(Box::new(problem(StatusCode::BAD_REQUEST, "since is a list of t<n>:<seq>, separated by commas")));
        };
        let Ok(seq) = seq.parse::<u64>() else {
            return Err(Box::new(problem(StatusCode::BAD_REQUEST, "since is a list of t<n>:<seq>, separated by commas")));
        };
        // Every entry is shaped, even one past the table's 16: a malformed entry is a 400 wherever it sits in the list.
        if since.iter().any(|(known, _)| known == id) || since.len() == MAX_TERMS {
            continue;
        }
        since.push((id.to_string(), seq));
    }
    Ok(since)
}

/// The body the page reads. Frames come from the producer over `rx`; the end is written here, once, the moment `stop` fires (or the
/// producer is gone, which happens only when the perch stops), so the stream closes with its last frame whatever the producer was doing.
struct Frames {
    rx: mpsc::Receiver<Bytes>,
    stop: Pin<Box<WaitForCancellationFutureOwned>>,
    done: bool,
}

impl Frames {
    fn finish(&mut self) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        self.done = true;
        self.rx.close();
        Poll::Ready(Some(Ok(Frame::data(frame(json!({"type": "hub-stream-end", "reason": "stopping"}))))))
    }
}

impl Body for Frames {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        let this = self.get_mut();
        if this.done {
            return Poll::Ready(None);
        }
        // The stop first, so a perch stop is not held up behind a busy stream.
        if this.stop.as_mut().poll(cx).is_ready() {
            return this.finish();
        }
        match this.rx.poll_recv(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(bytes)) => Poll::Ready(Some(Ok(Frame::data(bytes)))),
            Poll::Ready(None) => this.finish(),
        }
    }
}

/// What the producer writes to: the channel to the body, and the stop that ends the writing. `last_sent` is when the last frame went,
/// so the keep-alive is only sent after a silence.
struct Out {
    tx: mpsc::Sender<Bytes>,
    stop: CancellationToken,
    keep_alive: Duration,
    last_sent: Instant,
}

/// The stream is over: the client went, or the perch is stopping.
struct Gone;

impl Out {
    async fn send(&mut self, bytes: Bytes) -> Result<(), Gone> {
        tokio::select! {
            biased;
            _ = self.stop.cancelled() => return Err(Gone),
            sent = self.tx.send(bytes) => sent.map_err(|_| Gone)?,
        }
        self.last_sent = Instant::now();
        Ok(())
    }

    async fn keep_alive(&mut self) -> Result<(), Gone> {
        self.send(Bytes::from_static(b": keep-alive\n\n")).await
    }
}

/// One terminal this stream follows: the terminal (kept after it leaves the table, so the bytes it wrote before it closed still go out)
/// and the seq the page has been sent up to.
struct Watch {
    term: Arc<Terminal>,
    cursor: u64,
}

impl Watch {
    fn id(&self) -> &str {
        self.term.id()
    }
}

async fn produce(ctx: Ctx, mut ticks: watch::Receiver<u64>, mut changes: broadcast::Receiver<Change>, since: Vec<(String, u64)>, tx: mpsc::Sender<Bytes>) {
    let mut out = Out { tx, stop: ctx.stop.clone(), keep_alive: keep_alive_of(&ctx), last_sent: Instant::now() };
    let mut watches = Vec::new();
    // An error is the end of the stream, and there is nothing to say about it: the body closes on its own.
    let _ = run(&ctx, &mut out, &mut ticks, &mut changes, &since, &mut watches).await;
}

async fn run(
    ctx: &Ctx,
    out: &mut Out,
    ticks: &mut watch::Receiver<u64>,
    changes: &mut broadcast::Receiver<Change>,
    since: &[(String, u64)],
    watches: &mut Vec<Watch>,
) -> Result<(), Gone> {
    let terms = ctx.term.terms(ctx);
    let stop = ctx.stop.clone();
    let listed = terms.list();
    out.send(term_list(&listed)).await?;
    for info in &listed {
        let Some(term) = terms.get(&info.id) else { continue };
        let named = since.iter().find(|(id, _)| *id == info.id).map(|(_, seq)| *seq);
        watches.push(join(out, term, named).await?);
    }
    // A resumed watch starts at a seq the ring is already past, and the ticks subscribed before it saw nothing new: the backlog goes now.
    flush(out, watches).await?;
    let mut due: Option<Instant> = None;
    let mut last_flush = Instant::now();
    loop {
        let flush_at = due.unwrap_or_else(|| Instant::now() + Duration::from_secs(86_400));
        let keep_alive_at = out.last_sent + out.keep_alive;
        tokio::select! {
            biased;
            _ = stop.cancelled() => return Ok(()),
            _ = out.tx.closed() => return Ok(()),
            _ = tokio::time::sleep_until(flush_at), if due.is_some() => {
                due = None;
                flush(out, watches).await?;
                last_flush = Instant::now();
            }
            _ = tokio::time::sleep_until(keep_alive_at) => out.keep_alive().await?,
            woke = ticks.changed() => {
                woke.map_err(|_| Gone)?;
                if due.is_none() {
                    due = Some(flush_due(last_flush, Instant::now()));
                }
            }
            next = changes.recv() => match next {
                Ok(change) => {
                    flush(out, watches).await?;
                    on_change(ctx, out, watches, change).await?;
                }
                Err(broadcast::error::RecvError::Lagged(_)) => relist(ctx, out, watches).await?,
                Err(broadcast::error::RecvError::Closed) => return Ok(()),
            },
        }
    }
}

/// When the output is next flushed, given a tick: at once if the last flush is a whole [`COALESCE`] or more ago, else when that window
/// ends. The tick arm of [`run`] takes its due time from here, so the 16 ms bound is the one a unit test pins.
fn flush_due(last_flush: Instant, now: Instant) -> Instant {
    now.max(last_flush + COALESCE)
}

/// A terminal joins the stream: `term-open` first when it is new to the page (the caller says so), then either its output from the
/// page's seq, or a reset.
async fn join(out: &mut Out, term: Arc<Terminal>, named: Option<u64>) -> Result<Watch, Gone> {
    // A seq the ring still holds, and that is not ahead of what the terminal has written, resumes; anything else is a reset.
    let resumes = named.filter(|&seq| seq <= term.seq() && matches!(term.read(seq, 0), Read::Data(_)));
    let cursor = match resumes {
        Some(seq) => seq,
        None => {
            let reset = on_thread({
                let term = term.clone();
                move || reset_of(&term)
            })
            .await?;
            send_reset(out, &term, reset).await?
        }
    };
    Ok(Watch { term, cursor })
}

/// What a page needs to be where a terminal is now: the bytes that rebuild it and the seq they are at.
struct Reset {
    seq: u64,
    snap: bool,
    data: Vec<u8>,
}

/// The reset of `term` as it is this moment. The screen and the ring are read at the same byte under the terminal's locks (`view`), so
/// the snapshot and its seq agree; the encoding happens after the locks are let go.
fn reset_of(term: &Terminal) -> Reset {
    enum Image {
        Replay(u64, Vec<u8>),
        Screen(u64, snap::Picture),
    }
    let image = term.view(|emu, ring| {
        if ring.start() == 0 {
            Image::Replay(ring.end(), ring.contents())
        } else {
            // The screen has taken every byte of the ring but the few a character still waits for: the page resumes at the same byte.
            Image::Screen(ring.end().saturating_sub(emu.lag() as u64), snap::picture(emu))
        }
    });
    match image {
        Image::Replay(seq, data) => Reset { seq, snap: false, data },
        Image::Screen(seq, picture) => Reset { seq, snap: true, data: snap::encode(&picture) },
    }
}

/// Send a reset as one `term-reset` and the rest of its bytes as `term-out` frames at the same seq. Returns the seq the page is at.
async fn send_reset(out: &mut Out, term: &Terminal, reset: Reset) -> Result<u64, Gone> {
    let mut pieces = reset.data.chunks(FRAME_RAW);
    let first = pieces.next().unwrap_or(&[]);
    out.send(frame(json!({"type": "term-reset", "id": term.id(), "seq": reset.seq, "data": base64(first), "snap": reset.snap}))).await?;
    for piece in pieces {
        out.send(frame(json!({"type": "term-out", "id": term.id(), "seq": reset.seq, "data": base64(piece)}))).await?;
    }
    Ok(reset.seq)
}

/// Every terminal's output from its cursor on, cut into frames of at most [`FRAME_RAW`] bytes. A cursor the ring has passed is reset.
async fn flush(out: &mut Out, watches: &mut [Watch]) -> Result<(), Gone> {
    for watch in watches.iter_mut() {
        loop {
            match watch.term.read(watch.cursor, FRAME_RAW) {
                Read::Data(bytes) if bytes.is_empty() => break,
                Read::Data(bytes) => {
                    let end = watch.cursor + bytes.len() as u64;
                    let full = bytes.len() == FRAME_RAW;
                    out.send(frame(json!({"type": "term-out", "id": watch.id(), "seq": end, "data": base64(&bytes)}))).await?;
                    watch.cursor = end;
                    if !full {
                        break;
                    }
                }
                Read::Behind { .. } => {
                    let term = watch.term.clone();
                    let reset = on_thread(move || reset_of(&term)).await?;
                    watch.cursor = send_reset(out, &watch.term, reset).await?;
                }
            }
        }
    }
    Ok(())
}

async fn on_change(ctx: &Ctx, out: &mut Out, watches: &mut Vec<Watch>, change: Change) -> Result<(), Gone> {
    let terms = ctx.term.terms(ctx);
    match change {
        Change::Open(id) => {
            if watches.iter().any(|watch| watch.id() == id) {
                return Ok(());
            }
            let (Some(info), Some(term)) = (terms.info(&id), terms.get(&id)) else { return Ok(()) };
            out.send(frame(json!({"type": "term-open", "id": id, "term": info}))).await?;
            watches.push(join(out, term, None).await?);
        }
        Change::Size(id) => {
            if let Some(info) = terms.info(&id) {
                out.send(frame(json!({"type": "term-size", "id": id, "cols": info.cols, "rows": info.rows}))).await?;
            }
        }
        Change::Title(id) => {
            if let Some(info) = terms.info(&id) {
                out.send(frame(json!({"type": "term-title", "id": id, "title": info.title, "fg": info.fg}))).await?;
            }
        }
        Change::Hold(id) => {
            if let Some(info) = terms.info(&id) {
                out.send(frame(json!({"type": "term-hold", "id": id, "hold": info.hold}))).await?;
            }
        }
        Change::Exit(id) => {
            // The output the shell wrote before it ended is on the page before the word that it has ended.
            if let Some(info) = terms.info(&id) {
                let (code, signal) = info.exit.map_or((None, None), |exit| (exit.code, exit.signal));
                out.send(frame(json!({"type": "term-exit", "id": id, "code": code, "signal": signal}))).await?;
            }
        }
        Change::Closed(id) => {
            // Its last bytes went out in the flush before this; the stream lets go of it now.
            watches.retain(|watch| watch.id() != id);
            out.send(frame(json!({"type": "term-closed", "id": id}))).await?;
        }
        Change::Input { id, chat, text } => {
            out.send(frame(input_frame(&id, chat.as_deref(), &text))).await?;
        }
    }
    Ok(())
}

/// The `term-input` of an agent's input. `text` is the picture [`Terms::note_input`] made of it; the page gets it as [`glyphs`] shows it.
fn input_frame(id: &str, chat: Option<&str>, text: &str) -> Value {
    json!({"type": "term-input", "id": id, "by": "agent", "chat": chat, "text": glyphs(text)})
}

/// The change channel lagged: the roster is sent again, the terminals that are gone are closed on the page, and the new ones join.
async fn relist(ctx: &Ctx, out: &mut Out, watches: &mut Vec<Watch>) -> Result<(), Gone> {
    let terms = ctx.term.terms(ctx);
    let listed = terms.list();
    out.send(term_list(&listed)).await?;
    let mut kept = Vec::new();
    for watch in std::mem::take(watches) {
        if listed.iter().any(|info| info.id == watch.id()) {
            kept.push(watch);
        } else {
            out.send(frame(json!({"type": "term-closed", "id": watch.id()}))).await?;
        }
    }
    *watches = kept;
    for info in &listed {
        if watches.iter().any(|watch| watch.id() == info.id) {
            continue;
        }
        if let Some(term) = terms.get(&info.id) {
            watches.push(join(out, term, None).await?);
        }
    }
    Ok(())
}

/// Input typed by an agent, announced without writing it to a shell (T2). `agent_input` announces its own input already, so a route
/// calls one of the two for a given input, never both. Only an agent's input is announced: the operator's typing is not an event.
impl Terms {
    pub fn note_input(&self, id: &str, by: By, chat: Option<&str>, text: &str) {
        if by != By::Agent || self.info(id).is_none() {
            return;
        }
        let picture: String = text.chars().filter(|c| shown(*c) || (*c as u32) < 0x20 || *c == '\u{7f}').map(picture_of).collect();
        let text: String = picture.chars().take(NOTED_CHARS).collect();
        let text = if picture.chars().count() > NOTED_CHARS { format!("{text}\u{2026}") } else { text };
        self.emit(Change::Input { id: id.to_string(), chat: chat.map(str::to_string), text });
    }
}

/// A control character as its picture (U+2400 on, U+2421 for DEL), the way `agent_input` shows what it typed; anything else as it is.
fn picture_of(c: char) -> char {
    match c {
        '\u{7f}' => '\u{2421}',
        c if (c as u32) < 0x20 => char::from_u32(0x2400 + c as u32).unwrap_or(' '),
        c => c,
    }
}

/// The frame text of an input: the pictures become what a person reads (`⏎` for return and newline, `⇥` for tab, `^C` and so on), and
/// it is at most [`INPUT_CHARS`] characters, the last an ellipsis when cut.
fn glyphs(text: &str) -> String {
    let mut shown_text: Vec<String> = Vec::new();
    for c in text.chars() {
        shown_text.push(match c {
            '\u{240a}' | '\u{240d}' => "\u{23ce}".to_string(),
            '\u{2409}' => "\u{21e5}".to_string(),
            '\u{2421}' => "^?".to_string(),
            c @ '\u{2400}'..='\u{241f}' => format!("^{}", char::from_u32(c as u32 - 0x2400 + 0x40).unwrap_or('?')),
            c => c.to_string(),
        });
    }
    let total: usize = shown_text.iter().map(|piece| piece.chars().count()).sum();
    let mut out: String = shown_text.concat();
    if total > INPUT_CHARS {
        out = out.chars().take(INPUT_CHARS - 1).collect();
        out.push('\u{2026}');
    }
    out
}

fn term_list(listed: &[super::Info]) -> Bytes {
    frame(json!({"type": "term-list", "terms": listed}))
}

/// One SSE data frame: a JSON object on one `data:` line, which is what a page parses.
fn frame(value: Value) -> Bytes {
    Bytes::from(format!("data: {value}\n\n"))
}

/// The producer's blocking work (a screen's lock, `/proc`): on the runtime's blocking pool, so no async task waits for it.
async fn on_thread<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Result<T, Gone> {
    tokio::task::spawn_blocking(work).await.map_err(|_| Gone)
}

fn keep_alive_of(ctx: &Ctx) -> Duration {
    let (low, high) = KEEP_ALIVE_RANGE_MS;
    match ctx.opts.knobs.get("term_keepalive").map(str::parse::<u64>) {
        None => KEEP_ALIVE,
        Some(Ok(ms)) => Duration::from_millis(ms.clamp(low, high)),
        Some(Err(_)) => {
            tracing::warn!("perch: --knob term_keepalive is not a number of milliseconds; the keep-alive stays {KEEP_ALIVE:?}");
            KEEP_ALIVE
        }
    }
}

/// Standard padded base64, the alphabet the page decodes. Written here, not taken from a crate: the perch's one other base64 is a decoder.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(chunk.get(1).copied().unwrap_or(0)) << 8) | u32::from(chunk.get(2).copied().unwrap_or(0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_is_the_standard_padded_alphabet() {
        for (plain, coded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(plain.as_bytes()), coded, "{plain:?}");
        }
        assert_eq!(base64(&[0xfb, 0xff]), "+/8=");
    }

    #[test]
    fn since_is_a_list_of_ids_and_seqs_and_nothing_else_is_taken() {
        let query = |q: &str| since_of(Some(q)).map_err(|refusal| refusal.status().as_u16());
        assert_eq!(query("since=t1:4096,t3:0").unwrap(), vec![("t1".to_string(), 4096), ("t3".to_string(), 0)]);
        assert_eq!(query("").unwrap(), vec![]);
        assert_eq!(query("other=1&since=t2:7").unwrap(), vec![("t2".to_string(), 7)]);
        assert_eq!(query("since=t1:1,t1:2").unwrap(), vec![("t1".to_string(), 1)], "the first seq for a terminal is the one kept");
        for bad in ["since=t1", "since=t1:", "since=t1:-1", "since=a:1", "since=t1:1x", "since=t1%3A1", "since=t1:1,x"] {
            assert_eq!(query(bad), Err(400), "{bad}");
        }
        assert_eq!(query("since=t1:1,").unwrap(), vec![("t1".to_string(), 1)], "an empty entry is skipped");
        assert_eq!(query("since=t1:1&since=t1:2"), Err(400), "since given twice");
        let many: Vec<String> = (1..=20).map(|n| format!("t{n}:0")).collect();
        assert_eq!(query(&format!("since={}", many.join(","))).unwrap().len(), MAX_TERMS, "at most the table's terminals");
        let mut past_the_table = many[..MAX_TERMS + 1].to_vec();
        past_the_table.push("x".to_string());
        let past = format!("since={}", past_the_table.join(","));
        assert_eq!(query(&past), Err(400), "a malformed entry past the table's {MAX_TERMS} is a 400 too");
    }

    #[test]
    fn a_frame_is_one_data_line_with_a_string_type() {
        let line = frame(json!({"type": "term-out", "id": "t1", "seq": 3, "data": "YWJj"}));
        let text = String::from_utf8(line.to_vec()).unwrap();
        assert!(text.starts_with("data: ") && text.ends_with("\n\n"), "{text:?}");
        assert_eq!(text.matches('\n').count(), 2, "one line and its blank line: {text:?}");
        let value: Value = serde_json::from_str(text.trim_start_matches("data: ").trim_end()).unwrap();
        assert_eq!(value["type"], "term-out");
    }

    #[test]
    fn an_input_shows_its_controls_as_the_page_reads_them_and_is_cut_at_200() {
        assert_eq!(glyphs("ls\u{240d}"), "ls\u{23ce}");
        assert_eq!(glyphs("\u{2403}\u{2409}x"), "^C\u{21e5}x");
        assert_eq!(glyphs("\u{241b}\u{2421}"), "^[^?");
        let long = "a".repeat(300);
        let cut = glyphs(&long);
        assert_eq!(cut.chars().count(), INPUT_CHARS);
        assert!(cut.ends_with('\u{2026}'));
    }

    #[test]
    fn a_tick_inside_the_coalescing_window_is_due_when_the_window_ends() {
        let last = Instant::now();
        let five = Duration::from_millis(5);
        assert_eq!(flush_due(last, last + five), last + COALESCE, "a tick 5 ms after a flush waits for the window to end");
        assert_eq!(flush_due(last, last + COALESCE), last + COALESCE, "a tick at the window's end is due then");
        let quiet = last + COALESCE + five;
        assert_eq!(flush_due(last, quiet), quiet, "a tick after a quiet window is due at once");
        assert_eq!(flush_due(last, last), last + COALESCE, "a tick at the instant of a flush is due a window later");
    }

    #[test]
    fn a_noted_input_is_pictured_and_capped_the_way_agent_input_is() {
        assert_eq!(picture_of('\r'), '\u{240d}');
        assert_eq!(picture_of('\u{3}'), '\u{2403}');
        assert_eq!(picture_of('\u{7f}'), '\u{2421}');
        assert_eq!(picture_of('x'), 'x');
    }

    // A runtime, for the spawner's reactor; nothing here waits on it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_agents_input_is_a_term_input_frame_and_the_operators_is_not_announced() {
        use super::super::{Entry, Host, Record, lock};
        use crate::spawn::Spawner;
        let terms = Terms::new(Host {
            spawner: Arc::new(Spawner::new().unwrap()),
            stop: CancellationToken::new(),
            ring: super::super::pty::RING_BYTES,
            state_file: None,
            env: Vec::new(),
        });
        // One ended terminal to type into: no shell, no file (`state_file` is None, so nothing is written).
        lock(&terms.inner).entries.push(Entry::Ended(Record { id: "t1".into(), ..Record::default() }));
        let mut changes = terms.subscribe();

        terms.note_input("t1", By::Agent, Some("chat"), "ls\r");
        let Ok(Change::Input { id, chat, text }) = changes.try_recv() else { panic!("an agent's input is announced") };
        let frame = input_frame(&id, chat.as_deref(), &text);
        assert_eq!(frame["type"], "term-input");
        assert_eq!(frame["id"], "t1");
        assert_eq!(frame["by"], "agent");
        assert_eq!(frame["chat"], "chat");
        assert_eq!(frame["text"], "ls\u{23ce}", "the return is the return sign");
        assert!(input_frame("t1", None, "x")["chat"].is_null(), "no chat is a null, not a missing key");

        terms.note_input("t1", By::Operator, None, "x");
        terms.note_input("t9", By::Agent, None, "x");
        assert!(changes.try_recv().is_err(), "the operator's typing is not an event, and nor is input to a terminal that is not there");
    }
}
