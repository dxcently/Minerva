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
//! The watcher reads frames only until `hello` and holds no transcript: [`HelloScan`] keeps
//! bytes until `hello` is found (capped at [`HELLO_MAX`]), releases them, and from then on the
//! stream is read and thrown away. The stream stays open for the door's whole life and a long
//! session sends every text delta of every turn down it, so keeping them would grow the perch
//! by the whole transcript, per door.
//!
//! Not here, on purpose: panes, the proxy and door SSE passed to a browser (H1c); the
//! running/parked inference and the idle reaper, which read these same frames (H2) and will
//! need a bounded line reader of their own, not this buffer.

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;
use bytes::Bytes;
use http_body_util::{BodyExt as _, Empty};
use hyper::header::{AUTHORIZATION, HOST};
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

/// hub.md §5's connect timeout, for the watcher as much as for the proxy: this is a socket
/// to a process on this box.
const CONNECT: Duration = Duration::from_secs(2);

/// How much the watcher reads while looking for `hello`. The door's `hello` is the first frame
/// and a few hundred bytes; a door that sends this much without one is not a door.
const HELLO_MAX: usize = 1024 * 1024;

/// What `hello` says that the perch needs (§4 step 7): the log path, hence the id for a new
/// session, and the cwd, which is what a kept session's tree would be rooted at (H2).
pub struct Hello {
    pub session: PathBuf,
    pub cwd: PathBuf,
    pub model: String,
    /// Whether the door answers every ask with yes (`--yolo`), as the door itself reports it.
    pub yolo: bool,
}

/// A held stream, and the handle that closes it. Dropping it cancels the stream too, so no
/// error path can leave one open; [`Watcher::close`] is the one that also *waits*, which is
/// what the stop order needs.
pub struct Watcher {
    close: CancellationToken,
    task: Option<tokio::task::JoinHandle<()>>,
    /// Whether a turn is running, from the door's own `turn-state` frames. A door starts idle, so this starts `false`.
    running: Arc<AtomicBool>,
}

impl Watcher {
    /// Whether the door said a turn is running, as of the last `turn-state` it sent. This is what a restart refuses on:
    /// a restart between turns loses nothing, one in the middle of a turn loses the turn.
    pub fn running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Close the perch's stream to this door and wait until the socket is gone. After this
    /// returns, the door has no SSE connection left from the perch — which is the whole
    /// precondition of the SIGTERM that [`sessions::Registry::stop`] sends next.
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
/// own, from its first boot line; the token is the door's, from its `0600` file.
pub async fn open(port: u16, token: Arc<str>, within: Duration) -> Result<(Watcher, Hello)> {
    let close = CancellationToken::new();
    let (hello_tx, hello_rx) = oneshot::channel();
    let running = Arc::new(AtomicBool::new(false));
    let task = tokio::spawn(watch(port, token, close.clone(), hello_tx, running.clone()));
    match tokio::time::timeout(within, hello_rx).await {
        Ok(Ok(Ok(found))) => Ok((Watcher { close, task: Some(task), running }, found)),
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

/// Connect, ask for `/api/events` with the door's Bearer, read until `hello`, then hold the
/// stream open until `close` is cancelled or the door ends it.
async fn watch(port: u16, token: Arc<str>, close: CancellationToken, hello_tx: oneshot::Sender<Result<Hello>>, running: Arc<AtomicBool>) {
    let mut hello_tx = Some(hello_tx);
    let stream = match tokio::time::timeout(CONNECT, TcpStream::connect((Ipv4Addr::LOCALHOST, port))).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(e)) => return give_up(&mut hello_tx, anyhow::Error::new(e).context(format!("connecting to the door on 127.0.0.1:{port}"))),
        Err(_) => {
            let why = anyhow::anyhow!("the door on 127.0.0.1:{port} did not accept a connection within {CONNECT:?}");
            return give_up(&mut hello_tx, why);
        }
    };
    let (mut sender, connection) = match hyper::client::conn::http1::handshake(TokioIo::new(stream)).await {
        Ok(handshake) => handshake,
        Err(e) => return give_up(&mut hello_tx, anyhow::Error::new(e).context("the HTTP/1.1 handshake with the door")),
    };
    // Hyper's connection has to be polled or nothing moves on it. This task is aborted at
    // the end of this function, and aborting it drops the socket — which *is* closing the
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
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .header(HOST, format!("127.0.0.1:{port}"))
        .body(Empty::<Bytes>::new())
        .expect("the watcher's request is well-formed");

    let mut body = match sender.send_request(request).await {
        Ok(response) if response.status().is_success() => response.into_body(),
        // The door's own 401 means the perch's Bearer was not the door's token, which is a
        // bug here and not something a caller can fix; either way this door has no watcher.
        Ok(response) => return give_up(&mut hello_tx, anyhow::anyhow!("the door's /api/events answered {}", response.status())),
        Err(e) => return give_up(&mut hello_tx, anyhow::Error::new(e).context("asking the door for /api/events")),
    };

    let mut scan = HelloScan::default();
    let mut turns = TurnScan::default();
    let mut told = false;
    loop {
        tokio::select! {
            _ = close.cancelled() => break,
            frame = body.frame() => match frame {
                Some(Ok(frame)) => {
                    if let Some(data) = frame.data_ref() {
                        if let Some(now) = turns.feed(data) {
                            running.store(now, Ordering::SeqCst);
                        }
                        match scan.feed(data) {
                            Ok(Some(found)) => {
                                // A caller that timed out is gone; there is nothing to hold open.
                                let Some(tell) = hello_tx.take() else { break };
                                if tell.send(Ok(found)).is_err() {
                                    break;
                                }
                                told = true;
                            }
                            Ok(None) => {}
                            Err(e) => {
                                give_up(&mut hello_tx, e);
                                break;
                            }
                        }
                    }
                }
                // The door ended the stream (its own `goodbye`, a lag, or a dead process).
                _ => break,
            },
        }
    }
    // Aborting is not enough on its own: the socket is closed when the task's future is
    // *dropped*, and awaiting the handle is what makes that happen before this function
    // returns — which is what lets `Watcher::close` promise a closed stream to the SIGTERM
    // that follows it.
    connection.abort();
    let _ = connection.await;
    if !told {
        give_up(&mut hello_tx, anyhow::anyhow!("the door's /api/events ended before hello"));
    }
}

/// Nothing was told yet: the caller is waiting for an answer, and this is it.
fn give_up(hello_tx: &mut Option<oneshot::Sender<Result<Hello>>>, why: anyhow::Error) {
    if let Some(tell) = hello_tx.take() {
        let _ = tell.send(Err(why));
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

/// The longest line [`TurnScan`] looks at. A `turn-state` frame is a few dozen bytes; a text delta or a transcript replay line
/// can be much larger and is skipped without being held, so the perch still keeps no transcript.
const LINE_MAX: usize = 4096;

/// The door's `turn-state` frames, found in a stream the perch otherwise throws away: lines are assembled up to [`LINE_MAX`],
/// and only a `data:` line of that size is parsed. A longer line is dropped as it goes by and costs no memory.
#[derive(Default)]
struct TurnScan {
    line: Vec<u8>,
    /// The current line already passed [`LINE_MAX`]: its remainder is ignored up to the newline.
    over: bool,
}

impl TurnScan {
    /// One more read. `Some(running)` for the last `turn-state` completed in it, if any.
    fn feed(&mut self, data: &[u8]) -> Option<bool> {
        let mut last = None;
        for &byte in data {
            if byte == b'\n' {
                if !self.over && let Some(now) = turn_state(&self.line) {
                    last = Some(now);
                }
                self.line.clear();
                self.over = false;
            } else if !self.over {
                if self.line.len() < LINE_MAX {
                    self.line.push(byte);
                } else {
                    self.over = true;
                    self.line = Vec::new();
                }
            }
        }
        last
    }
}

/// `data: {"type":"turn-state","running":<bool>}` -> the bool; anything else is `None`.
fn turn_state(line: &[u8]) -> Option<bool> {
    let text = std::str::from_utf8(line).ok()?;
    let payload = text.strip_prefix("data:")?.trim_start();
    if !payload.contains("turn-state") {
        return None;
    }
    let frame: serde_json::Value = serde_json::from_str(payload).ok()?;
    if frame.get("type")?.as_str()? != "turn-state" {
        return None;
    }
    frame.get("running")?.as_bool()
}

/// The first `hello` frame in what has been read so far. Frames the perch does not know are
/// ignored — the door's protocol is additive, and a field this slice does not read (a
/// `turn-state` in H2, an `ask` in H1c) must not make the perch treat a live door as dead.
fn hello_from(seen: &[u8]) -> Option<Hello> {
    let text = std::str::from_utf8(seen).ok()?;
    for line in text.lines() {
        // `data: {...}`, with or without the space the door's own writer uses.
        let Some(payload) = line.strip_prefix("data:").map(str::trim_start) else { continue };
        let Ok(frame) = serde_json::from_str::<serde_json::Value>(payload) else { continue };
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

    /// The door's own frame, byte for byte as `stream.rs` writes it: `frame_json` is
    /// `data: {json}\n\n`.
    fn frame(value: serde_json::Value) -> Vec<u8> {
        format!("data: {value}\n\n").into_bytes()
    }

    #[test]
    fn hello_is_read_and_other_frames_are_left_alone() {
        let mut seen = frame(serde_json::json!({"type": "caught-up"}));
        assert!(hello_from(&seen).is_none(), "caught-up is not hello");
        seen.extend(frame(serde_json::json!({
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
        assert!(hello_from(&frame(serde_json::json!({"type": "hello", "cwd": "/work"}))).is_none());
        assert!(hello_from(b"data: not json\n\n").is_none());
    }

    /// `turn-state` is the one frame the perch reads after `hello`, whole lines only and whatever the read boundaries are; a
    /// line past the cap, and a text delta that merely says the words, change nothing. Mutation: parse every `data:` line
    /// whatever its type, or hold a line past the cap, and this goes red.
    #[test]
    fn a_turn_state_is_found_across_reads_and_nothing_else_moves_it() {
        let mut scan = TurnScan::default();
        let on = frame(serde_json::json!({"type": "turn-state", "running": true}));
        let (head, tail) = on.split_at(11);
        assert_eq!(scan.feed(head), None, "half a frame says nothing");
        assert_eq!(scan.feed(tail), Some(true));
        let said = frame(serde_json::json!({"type": "text-delta", "text": "\"type\":\"turn-state\",\"running\":false"}));
        assert_eq!(scan.feed(&said), None, "a delta that quotes the words is not a turn-state");
        let big = frame(serde_json::json!({"type": "text-delta", "text": "x".repeat(3 * LINE_MAX)}));
        assert_eq!(scan.feed(&big), None);
        assert!(scan.line.capacity() < 2 * LINE_MAX, "an oversized line is not held");
        let off = frame(serde_json::json!({"type": "turn-state", "running": false}));
        assert_eq!(scan.feed(&off), Some(false), "the line after a skipped one is read");
        let mut both = on.clone();
        both.extend(&off);
        assert_eq!(scan.feed(&both), Some(false), "two in one read: the later one");
    }

    fn hello_frame() -> Vec<u8> {
        frame(serde_json::json!({"type": "hello", "session": "/logs/a.eid", "cwd": "/work", "model": "mock"}))
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
        let piece = frame(serde_json::json!({"type": "text-delta", "text": "x".repeat(4000)}));
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
        let piece = frame(serde_json::json!({"type": "text-delta", "text": "x".repeat(4000)}));
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
}
