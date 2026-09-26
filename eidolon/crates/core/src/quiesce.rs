//! Quiesce: the birth-side half of a session hop.
//!
//! A live session that keeps taking turns after its journal is shipped forks
//! the journal — the venue holds a snapshot, birth grows a tail the snapshot
//! does not contain, and the next hop-back clobbers a side. Quiesce turns the
//! "birth archive is read-only" convention into mechanism: a session that has
//! quiesced settles at its next turn boundary, journals a marker, and closes
//! cleanly, so the birth file stops growing and the shipper can copy a
//! settled log. It is the settled alternative to kill: a killed session
//! mid-turn leaves a torn tail (truncated on reopen — record loss) and a dead
//! TUI, where a quiesced one leaves a boundary and a goodbye.
//!
//! ## The three halves
//!
//! - **The ask** is a doorbell request: [`doorbell::answer`] answers
//!   `"op": "quiesce"` with `{"ok": true}` — the request is noted, the
//!   session will finish at its next turn boundary — and `"op":
//!   "quiesce-status"` with `{"ok": true, "quiesced": bool, "busy": bool}`,
//!   so a caller that cannot stay on the line can poll. The ask's answer
//!   never waits for the boundary: the socket serves the request and
//!   returns, and the boundary does the rest.
//! - **The wait** is a flag on [`Agent`] ([`Agent::request_quiesce`],
//!   [`Agent::quiesce_request`], [`Agent::poll_quiesce`]) plus
//!   [`quiesce_wait`] (boundary-wait with a timeout, never forced). The
//!   agent's loop notices the flag at the same turn boundaries it would
//!   journal a peer's note at — a quiesce is a note that did not ask to wake,
//!   and it waits for the boundary the same way — and journals the marker
//!   there, just after the settle. A busy session quiesces when its turn
//!   settles; an idle one has no boundary to wait for, which is what
//!   [`Agent::poll_quiesce`] settles on the spot.
//! - **The close** is the consumers': unregister from the local presence
//!   registry (deliberately dark afterward — the session's presence belongs
//!   to its venue now), and end with a goodbye naming where the session went
//!   and how to steer it. The journal close needs no ceremony: the log is
//!   append-only and durable per record, so "never grows again" is dropping
//!   every handle that could append, which is what [`Agent`]'s own refusal to
//!   start another turn makes stick until the process exits.
//!
//! ## Refusal is a first-class outcome
//!
//! A quiesce that does not settle before its timeout is *refused*, never
//! forced — `Ok(false)`, said out loud, and [`Knock::Refused`] for the
//! outside caller. Melete's side refuses to ship a session that will not
//! quiesce, and a refusal the shipper believes is what keeps a forked journal
//! from looking like a shipped one. So the wait watches the boundary and the
//! clock together, and mid-tool-call is never interrupted: the flag rides the
//! boundary the loop already keeps, and the loop never keeps a second one for
//! this.
//!
//! ## What is journaled, and what is not
//!
//! A quiesced session appends [`RecordKind::Note`][crate::session::RecordKind::Note]
//! naming the hop ("quiesced for projection to …") at its boundary, before
//! the close — a free-form marker, not a boundary record, because the settled
//! turn ahead of it is already the boundary and the log's enum is scarce. So
//! no [`crate::session::RecordKind`] variant is added for any of this, which
//! is why no `pins.rs` ordinal moved. Nothing follows it on the branch: when
//! the process exits, the birth journal's last record is the note, and the
//! venue's arrival note is the first record the hop appends. The venue side
//! ([`Session::open_projected`][crate::session::Session::open_projected]) is
//! unchanged.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::agent::Agent;

/// How long [`quiesce_and_wait`] gives a busy session to reach its boundary
/// before refusing. Long enough for a tool call to come back; short enough
/// that a shipper is not left guessing whether anyone is home. A caller that
/// needs a different deadline passes its own to [`quiesce_wait`].
pub const QUIESCE_TIMEOUT: Duration = Duration::from_secs(60);

/// The default destination, for an ask that names no venue.
pub const ANOTHER_MACHINE: &str = "another machine";

/// A quiesce that was asked for: where it is going, and whether it has
/// settled there yet.
#[derive(Clone, Debug)]
pub struct Quiesce {
    /// Where the session is going, as the goodbye should name it.
    pub destination: String,
    /// Set at the turn boundary, just before the marker is journaled — the
    /// flag the wait polls and the drivers read between turns.
    settled: Arc<AtomicBool>,
}

impl Quiesce {
    pub(crate) fn new(destination: &str) -> Self {
        Quiesce {
            destination: destination.to_string(),
            settled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Has the boundary been reached? The flag the loop sets and every
    /// waiter reads.
    pub fn settled(&self) -> bool {
        self.settled.load(Ordering::SeqCst)
    }

    /// Set at the boundary — by the loop for a busy session, by
    /// [`Agent::poll_quiesce`] for an idle one — once, so a settled
    /// quiesce stays settled.
    pub(crate) fn mark_settled(&self) -> bool {
        !self.settled.swap(true, Ordering::SeqCst)
    }

    /// The marker journaled at the boundary. A note, not a boundary record:
    /// the settled turn ahead of it already is the boundary, and the note is
    /// what says why the journal stops here.
    pub fn marker(&self) -> String {
        format!(
            "quiesced for projection to {}; the journal stops here — steer it there",
            self.destination
        )
    }
}

/// Ask this agent to quiesce, and wait for the boundary with a timeout.
///
/// Returns `true` when the boundary was reached and the marker journaled —
/// the journal will not grow again while this process lives — and `false`
/// when the timeout fired first, in which case nothing was forced and the
/// session goes on as before. An idle session quiesces at once; a busy one
/// quiesces when its turn settles.
///
/// This is the in-process half a consumer with its own driver of the way out
/// asks through; an outside caller goes over the doorbell with
/// [`knock_and_wait`], and the two meet at [`Agent::request_quiesce`].
pub async fn quiesce_and_wait(
    agent: &Agent,
    destination: &str,
    timeout: Duration,
) -> anyhow::Result<bool> {
    agent.request_quiesce(destination);
    quiesce_wait(agent, timeout).await
}

/// Wait for a quiesce that was asked for — with [`Agent::request_quiesce`],
/// the doorbell, or [`quiesce_and_wait`] — to reach its boundary, with a
/// timeout. The boundary is polled: a session that is mid-turn settles on
/// its own schedule, and the wait watches the flag rather than the turn.
pub async fn quiesce_wait(agent: &Agent, timeout: Duration) -> anyhow::Result<bool> {
    if agent.quiesce_request().is_none() {
        // Nothing was asked for: waiting on a quiesce nobody requested would
        // burn the whole timeout to answer a question no one put.
        return Ok(false);
    }
    let start = std::time::Instant::now();
    loop {
        if agent.poll_quiesce().await? {
            return Ok(true);
        }
        if start.elapsed() >= timeout {
            return Ok(false);
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// The doorbell half: what a session's socket answers, and what knocking on
/// it means.
///
/// [`crate::Agent`]-aware, so the socket's own `answer` cannot call it: the
/// consumer wires it in as the socket's extra op handler (see
/// `eidolon_swarm::socket::serve_with`). A `"quiesce"` request notes the
/// destination and answers at once — the boundary does the rest — and a
/// `"quiesce-status"` request reports whether the boundary has been reached.
/// `"state"` answers what a peer waiting on this session needs — whether a
/// turn is in flight, which no file can be asked (the roster's `busy` is
/// only ever written by one driver). `None` means "not an op of mine", and
/// the socket's own answer decides; unknown ops still refuse, exactly as it
/// refuses them.
pub mod doorbell {
    use super::*;

    /// Answer one doorbell request for a session holding `agent`.
    pub fn answer(agent: &Agent, req: &Value, ring: &mpsc::UnboundedSender<()>) -> Option<Value> {
        match req.get("op").and_then(Value::as_str).unwrap_or("") {
            "quiesce" => {
                let destination = req
                    .get("destination")
                    .and_then(Value::as_str)
                    .unwrap_or(ANOTHER_MACHINE);
                let fresh = agent.request_quiesce(destination);
                // The ring is what reaches a consumer's driver: an idle
                // session has no turn to settle at, so only its driver can
                // journal the marker, and without this it would sit idle
                // holding a request nobody looks at. Ringing is the same
                // doorbell a peer's mail uses, so every driver already
                // listens for it.
                let _ = ring.send(());
                Some(json!({
                    "ok": true,
                    "destination": destination,
                    "already": !fresh,
                }))
            }
            // What a peer asking to wait on this session needs: whether a
            // turn is in flight. The same answer `quiesce-status` carries,
            // asked without the quiesce — a park is not a hop.
            "state" => Some(json!({ "ok": true, "busy": agent.turn_in_flight() })),
            "quiesce-status" => {
                let q = agent.quiesce_request();
                Some(json!({
                    "ok": true,
                    "quiesced": q.as_ref().is_some_and(|q| q.settled()),
                    "busy": agent.turn_in_flight(),
                }))
            }
            _ => None,
        }
    }
}

/// What the terminal says on the way out: where the session went, and how to
/// reach it now that the local registry no longer knows it.
pub fn goodbye(destination: &str, session: Option<&str>) -> String {
    let who = match session {
        Some(id) => format!("session {id}"),
        None => "this session".to_string(),
    };
    format!(
        "{who} projected to {destination} — this terminal is done, the journal stops here. \
Steer it there: `melete projections steer` from the registrar, or `eidolon send` on {destination}."
    )
}

/// Ask for a quiesce over a session's socket and wait for the boundary, with
/// a timeout — the outside caller's whole path (`eidolon door quiesce`).
///
/// `send` is the one round trip the caller owns: the CLI passes a closure
/// over `eidolon_swarm::socket::request`, a test passes a script. Two shapes
/// are built here ([`quiesce_request`], [`quiesce_status_request`]) so every
/// caller and the tests share one protocol.
///
/// Three outcomes, each said out loud: settled (the journal will not grow
/// again), refused by timeout (nothing forced, the session goes on), or no
/// session there at all — an `Err` from `send`. A session that answers the
/// ask but never settles is the second, not the third: the ask went through
/// and the boundary did not arrive.
pub fn knock_and_wait(
    destination: &str,
    timeout: Duration,
    mut send: impl FnMut(&Value) -> anyhow::Result<Value>,
) -> anyhow::Result<Knock> {
    let reply = send(&quiesce_request(destination))?;
    if reply.get("ok").and_then(Value::as_bool) != Some(true) {
        anyhow::bail!(
            "the session refused the quiesce: {}",
            reply
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("no reason given")
        );
    }
    let start = std::time::Instant::now();
    loop {
        let status = send(&quiesce_status_request())?;
        if status.get("ok").and_then(Value::as_bool) != Some(true) {
            anyhow::bail!(
                "the session stopped answering while quiescing: {}",
                status
                    .get("error")
                    .and_then(Value::as_str)
                    .unwrap_or("no reason given")
            );
        }
        if status.get("quiesced").and_then(Value::as_bool) == Some(true) {
            return Ok(Knock::Settled);
        }
        if start.elapsed() >= timeout {
            return Ok(Knock::Refused);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// What knocking on a session's door came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Knock {
    /// The boundary was reached and the marker journaled: the birth journal
    /// will never grow again on that machine.
    Settled,
    /// The timeout fired first: nothing was forced and the session goes on.
    /// The shipper refuses a session that answers this way.
    Refused,
}

impl Knock {
    /// The one line an outside caller prints, so the CLI and the tests say
    /// the same thing about the same outcome.
    pub fn line(self, destination: &str, waited: Duration) -> String {
        match self {
            Knock::Settled => format!(
                "quiesced for projection to {destination}: the session reached its boundary and \
                 its journal stops here"
            ),
            Knock::Refused => format!(
                "refused: the session did not reach a boundary within {}s, so nothing was forced \
                 and it is still running",
                waited.as_secs_f32()
            ),
        }
    }
}

/// Build the `"quiesce"` request [`knock_and_wait`] sends.
pub fn quiesce_request(destination: &str) -> Value {
    json!({ "op": "quiesce", "destination": destination })
}

/// Build the `"quiesce-status"` request [`knock_and_wait`] polls with.
pub fn quiesce_status_request() -> Value {
    json!({ "op": "quiesce-status" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_goodbye_names_where_and_how() {
        let g = goodbye("cannataxis", Some("eidolon-9f2c"));
        assert!(g.contains("cannataxis"), "{g}");
        assert!(g.contains("eidolon-9f2c"), "{g}");
        assert!(g.contains("melete projections steer"), "{g}");
        assert!(g.contains("eidolon send"), "{g}");
        // A session with no registration still gets a sentence that parses.
        let g = goodbye("cannataxis", None);
        assert!(g.starts_with("this session projected"), "{g}");
        assert!(!g.contains("session this"), "{g}");
    }

    #[test]
    fn the_quiesce_requests_carry_the_op_and_the_destination() {
        let q = quiesce_request("cannataxis");
        assert_eq!(q["op"], "quiesce");
        assert_eq!(q["destination"], "cannataxis");
        assert_eq!(quiesce_status_request()["op"], "quiesce-status");
    }

    #[test]
    fn a_marker_names_the_destination() {
        let q = Quiesce::new("cannataxis");
        assert!(!q.settled());
        assert!(q.marker().contains("cannataxis"), "{}", q.marker());
        assert!(q.marker().contains("the journal stops here"), "{}", q.marker());
        // Settling is one-way: the second mark says the first one did it.
        assert!(q.mark_settled());
        assert!(!q.mark_settled());
        assert!(q.settled());
    }

    #[test]
    fn knocking_reports_settled_refused_and_a_refusal() {
        // Settles on the first status poll: the ask, then one status.
        let mut calls = vec![
            json!({"ok": true}),
            json!({"ok": true, "quiesced": true, "busy": false}),
        ]
        .into_iter();
        let out = knock_and_wait("cannataxis", Duration::from_secs(5), |_| {
            Ok(calls.next().unwrap())
        })
        .unwrap();
        assert_eq!(out, Knock::Settled);

        // Never settles: the timeout refuses rather than forcing.
        let out = knock_and_wait("cannataxis", Duration::from_millis(150), |_| {
            Ok(json!({"ok": true, "quiesced": false, "busy": true}))
        })
        .unwrap();
        assert_eq!(out, Knock::Refused);
        assert!(out.line("cannataxis", Duration::from_millis(150)).contains("nothing was forced"));

        // The ask itself refused: an error, not a timeout.
        let e = knock_and_wait("cannataxis", Duration::from_secs(5), |_| {
            Ok(json!({"ok": false, "error": "unknown op `quiesce`"}))
        })
        .unwrap_err()
        .to_string();
        assert!(e.contains("refused the quiesce"), "{e}");
        assert!(e.contains("unknown op"), "{e}");

        // A door that stopped answering mid-wait is an error too, not a
        // silent refusal: the session may be gone rather than busy.
        let mut calls = vec![json!({"ok": true}), json!({"ok": false, "error": "gone"})].into_iter();
        let e = knock_and_wait("cannataxis", Duration::from_secs(5), |_| {
            Ok(calls.next().unwrap())
        })
        .unwrap_err()
        .to_string();
        assert!(e.contains("stopped answering"), "{e}");
    }
}
