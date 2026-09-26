//! The doorbell: a session-scoped unix socket, `0600`.
//!
//! Two things use it and neither carries a payload of any size. **`ping`**
//! is how liveness is decided — a presence directory whose socket answers
//! is live, a fact no file can lie about the way a stale `meta.json` can.
//! Silence is not the opposite verdict: a busy session can fail to answer
//! inside the timeout, so the sweep pairs it with the registration's pid
//! (`presence::pid_live`). **`notify`** is the ring:
//! the sender has already
//! written the message into the recipient's inbox, and this is what tells
//! an idle session to go and look rather than wait for its next turn.
//!
//! The shape is `crates/claude/src/hook.rs`'s, deliberately: one JSON
//! request per connection, one JSON reply, close. That socket has already
//! paid for the 108-byte `sun_path` lesson, which is why the directory
//! lives under `$XDG_RUNTIME_DIR` and never under the session tree.
//!
//! The client half is synchronous. It is called from a Rune script's own
//! thread and from the roster scan, both of which want an answer or a
//! failure now, and every call carries a short timeout so one wedged peer
//! cannot stall a listing.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::{Value, json};
use eidolon_core::ipc;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// How long a client waits on a peer before calling it unreachable. A
/// peer on the same machine answers in microseconds; anything past this
/// is a session that is not going to answer at all.
const TIMEOUT: Duration = Duration::from_millis(250);

/// An op the socket itself does not know, answered by whoever holds the
/// session. Today that is the quiesce half of a session hop (see
/// `eidolon_core::quiesce`) — the socket carries no protocol of its own
/// beyond `ping` and `notify`, and the session it belongs to is the only
/// thing that can answer a question about its own loop.
///
/// `Some(reply)` means the handler owned the request; `None` means "not
/// mine", and [`answer_value`] decides. It runs on the accept task before
/// the reply is written, so it must not block: note the request, answer,
/// and let the boundary do the waiting.
pub type OpHandler =
    Arc<dyn Fn(&Value, &mpsc::UnboundedSender<()>) -> Option<Value> + Send + Sync>;

/// Listen on `path`. Every `notify` that arrives is one send on `ring` —
/// the message itself is already in the inbox, so the notification needs
/// to carry nothing but the fact that there is mail.
pub async fn serve(
    path: &Path,
    ring: mpsc::UnboundedSender<()>,
    cancel: CancellationToken,
) -> Result<tokio::task::JoinHandle<()>> {
    serve_with(path, ring, cancel, None).await
}

/// [`serve`], plus a handler for the ops this socket does not know — see
/// [`OpHandler`]. A session whose consumer holds the agent passes one; a
/// test or a bare doorbell passes nothing.
pub async fn serve_with(
    path: &Path,
    ring: mpsc::UnboundedSender<()>,
    cancel: CancellationToken,
    handler: Option<OpHandler>,
) -> Result<tokio::task::JoinHandle<()>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let listener = ipc::bind_private(path)?;
    Ok(tokio::spawn(async move {
        loop {
            let (mut stream, _) = tokio::select! {
                r = listener.accept() => match r { Ok(x) => x, Err(_) => break },
                _ = cancel.cancelled() => break,
            };
            let ring = ring.clone();
            let handler = handler.clone();
            tokio::spawn(async move {
                let Ok(buf) = ipc::read_request(&mut stream).await else { return };
                let reply = match serde_json::from_slice::<Value>(&buf) {
                    Ok(req) => handler
                        .and_then(|h| h(&req, &ring))
                        .unwrap_or_else(|| answer_value(&req, &ring)),
                    Err(e) => json!({ "ok": false, "error": format!("unreadable request: {e}") }),
                };
                let _ = stream.write_all(reply.to_string().as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    }))
}

/// The socket's own ops, on an already-parsed request. Split from the
/// per-connection task so a handler can be consulted first without parsing
/// twice.
fn answer_value(req: &Value, ring: &mpsc::UnboundedSender<()>) -> Value {
    match req.get("op").and_then(Value::as_str).unwrap_or("") {
        "ping" => json!({ "ok": true }),
        "notify" => {
            // A closed receiver means the consumer is gone but the task
            // is not; say so rather than claiming a delivery nobody will
            // act on.
            match ring.send(()) {
                Ok(()) => json!({ "ok": true }),
                Err(_) => {
                    json!({ "ok": false, "error": "this session is no longer taking messages" })
                }
            }
        }
        other => json!({ "ok": false, "error": format!("unknown op `{other}`") }),
    }
}

/// Is anything listening? The positive liveness test: an answer is a
/// live session, full stop. A silence is not a verdict by itself — the
/// sweep pairs it with the registration's pid, because a busy session
/// can fail to answer inside the timeout.
pub fn probe(path: &Path) -> bool {
    request(path, &json!({ "op": "ping" })).is_ok()
}

/// Tell a session it has mail. `Err` when nobody answered — which the
/// caller reports as *queued* rather than *delivered*, since the message
/// is already in the inbox either way.
pub fn ring(path: &Path) -> Result<()> {
    let reply = request(path, &json!({ "op": "notify" }))?;
    if reply.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(())
    } else {
        anyhow::bail!(
            "{}",
            reply
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("the peer refused the notification")
        )
    }
}

/// One JSON request over the doorbell, one JSON reply — the client half of
/// the protocol, exposed for a caller that needs to ask something the
/// socket's own two ops do not cover (`eidolon door quiesce`, whose
/// questions and answers are `eidolon_core::quiesce`'s).
///
/// Synchronous, with the same short timeout `ring` and `probe` carry: it is
/// called from outside any session, where an answer or a failure is wanted
/// now rather than awaited on a runtime.
pub fn request(path: &Path, req: &Value) -> Result<Value> {
    use std::os::unix::net::UnixStream;
    let mut s =
        UnixStream::connect(path).with_context(|| format!("connecting to {}", path.display()))?;
    s.set_read_timeout(Some(TIMEOUT))?;
    s.set_write_timeout(Some(TIMEOUT))?;
    s.write_all(serde_json::to_string(req)?.as_bytes())?;
    s.shutdown(std::net::Shutdown::Write)?;
    let mut buf = String::new();
    s.read_to_string(&mut buf)?;
    serde_json::from_str(&buf).with_context(|| format!("unreadable reply from {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_ring_reaches_the_channel_and_a_dead_socket_is_not_live() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("sock");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let handle = serve(&sock, tx, cancel.clone()).await.unwrap();

        let s = sock.clone();
        tokio::task::spawn_blocking(move || {
            assert!(probe(&s), "a served socket answers");
            ring(&s).unwrap();
        })
        .await
        .unwrap();
        rx.recv().await.expect("the ring arrives as a notification");

        cancel.cancel();
        // The listener is gone but the file remains — exactly the stale
        // directory a dead session leaves behind.
        handle.abort();
        drop(handle);
        let s = sock.clone();
        let alive = tokio::task::spawn_blocking(move || probe(&s))
            .await
            .unwrap();
        assert!(
            !alive,
            "a socket nothing is listening on is not a live session"
        );
    }

    #[tokio::test]
    async fn an_unknown_op_is_refused_rather_than_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("sock");
        let (tx, _rx) = mpsc::unbounded_channel();
        let _h = serve(&sock, tx, CancellationToken::new()).await.unwrap();
        let s = sock.clone();
        let reply =
            tokio::task::spawn_blocking(move || request(&s, &json!({ "op": "wat" })).unwrap())
                .await
                .unwrap();
        assert_eq!(reply["ok"], false);
    }

    /// A handler owns the ops it recognises and nothing else: a `quiesce`
    /// reaches it, an unknown op it declines still falls through to the
    /// socket's own refusal, and `ping` never leaves the socket.
    #[tokio::test]
    async fn a_handler_answers_its_own_ops_and_declines_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let sock = tmp.path().join("sock");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let handler: OpHandler = {
            let seen = seen.clone();
            std::sync::Arc::new(move |req: &Value, ring: &mpsc::UnboundedSender<()>| {
                let op = req.get("op").and_then(Value::as_str).unwrap_or("").to_string();
                seen.lock().unwrap().push(op.clone());
                if op == "quiesce" {
                    // The ring is the handler's to use: this is how an idle
                    // session's driver learns it has one.
                    let _ = ring.send(());
                    return Some(json!({ "ok": true, "destination": "cannataxis" }));
                }
                None
            })
        };
        let _h = serve_with(&sock, tx, CancellationToken::new(), Some(handler))
            .await
            .unwrap();

        let s = sock.clone();
        let answer = tokio::task::spawn_blocking(move || {
            let ask = request(&s, &json!({ "op": "quiesce" })).unwrap();
            let ping = request(&s, &json!({ "op": "ping" })).unwrap();
            let other = request(&s, &json!({ "op": "wat" })).unwrap();
            (ask, ping, other)
        })
        .await
        .unwrap();
        assert_eq!(answer.0["ok"], true);
        assert_eq!(answer.0["destination"], "cannataxis");
        assert_eq!(answer.1["ok"], true, "ping is the socket's own");
        assert_eq!(answer.2["ok"], false, "the handler declined `wat`");
        assert!(
            answer.2["error"].as_str().unwrap().contains("unknown op"),
            "{:?}",
            answer.2
        );
        rx.recv().await.expect("the handler rang the doorbell");
        // The handler is consulted for every request and declines what it
        // does not own; `ping` was answered by the socket behind it.
        assert_eq!(&*seen.lock().unwrap(), &["quiesce", "ping", "wat"]);
    }
}
