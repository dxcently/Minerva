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
//! Not here, on purpose: panes, the proxy and door SSE passed to a browser (H1c); the
//! running/parked inference and the idle reaper, which read these same frames (H2).

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Arc;
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

/// What `hello` says that the perch needs (§4 step 7): the log path, hence the id for a new
/// session, and the cwd, which is what a kept session's tree would be rooted at (H2).
pub struct Hello {
    pub session: PathBuf,
    pub cwd: PathBuf,
    pub model: String,
}

/// A held stream, and the handle that closes it. Dropping it cancels the stream too, so no
/// error path can leave one open; [`Watcher::close`] is the one that also *waits*, which is
/// what the stop order needs.
pub struct Watcher {
    close: CancellationToken,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Watcher {
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
    let task = tokio::spawn(watch(port, token, close.clone(), hello_tx));
    match tokio::time::timeout(within, hello_rx).await {
        Ok(Ok(Ok(found))) => Ok((Watcher { close, task: Some(task) }, found)),
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
async fn watch(port: u16, token: Arc<str>, close: CancellationToken, hello_tx: oneshot::Sender<Result<Hello>>) {
    let mut hello_tx = Some(hello_tx);
    let stream = match tokio::time::timeout(CONNECT, TcpStream::connect((Ipv4Addr::LOCALHOST, port))).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(e)) => return give_up(&mut hello_tx, anyhow::Error::new(e).context(format!("connecting to the door on 127.0.0.1:{port}"))),
        Err(_) => return give_up(&mut hello_tx, anyhow::anyhow!("the door on 127.0.0.1:{port} did not accept a connection within {CONNECT:?}")),
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

    let mut seen = Vec::new();
    let mut told = false;
    loop {
        tokio::select! {
            _ = close.cancelled() => break,
            frame = body.frame() => match frame {
                Some(Ok(frame)) => {
                    if let Some(data) = frame.data_ref() {
                        seen.extend_from_slice(data);
                    }
                    if !told {
                        if let Some(found) = hello_from(&seen) {
                            // A caller that timed out is gone; there is nothing to hold open.
                            let Some(tell) = hello_tx.take() else { break };
                            if tell.send(Ok(found)).is_err() {
                                break;
                            }
                            told = true;
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
        return Some(Hello { session: PathBuf::from(session), cwd: PathBuf::from(cwd), model });
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
        seen.extend(frame(serde_json::json!({"type": "hello", "session": "/logs/ctf-pwn-3.eid", "cwd": "/work", "model": "mock", "yolo": false, "pending": 0, "protocol": 1})));
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
}
