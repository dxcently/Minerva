//! Presence and messaging between concurrent harness sessions.
//!
//! Several sessions work on one project at once — as many as five have
//! worked on this repository at a time — and the protocol that made that
//! survivable was a human one: claim files by message before editing,
//! announce what changed on the way out. This crate is that protocol made
//! mechanical.
//!
//! ## It is the control socket, pointed sideways
//!
//! Nothing here is a new transport. `crates/claude/src/hook.rs` already
//! binds a unix socket in `$XDG_RUNTIME_DIR` for the life of a turn; the
//! *session control socket* planned for the terminal integrations is that
//! socket made session-scoped, and a peer is simply another client of it.
//! What a peer needs beyond what an editor keybind needs is the ability to
//! **enumerate** sessions before it picks one — a keybind knows which
//! session it means, a peer does not — so each live session publishes a
//! `meta.json` beside its socket:
//!
//! ```text
//! $XDG_RUNTIME_DIR/eidolon/<id>/
//!     meta.json    who this session is: pid, log, cwd, repo, model, title
//!     sock         the doorbell (0600)
//!     inbox/       one file per undelivered message
//! ```
//!
//! Liveness is *the socket answers*. A directory whose socket refuses is a
//! session that died without cleaning up, and is swept by whoever noticed.
//! There is no daemon and no broker: the directory is the registry, which
//! is the same posture as *the log is the truth* one level down.
//!
//! ## Delivery: a maildir, and a doorbell
//!
//! A message is a file the **sender** writes into the recipient's `inbox/`
//! (temp name, then rename — atomic, multi-writer safe, ordered by name),
//! followed by a ring on the recipient's socket. The socket carries a
//! notification, never the payload, so durability belongs to the sender's
//! write and a recipient that was busy, wedged or restarting still finds
//! its mail.
//!
//! [`SessionLog`](eidolon_core::session::SessionLog) is deliberately *not*
//! reused for this, tempting as its framing is: it is single-writer by
//! construction — `append` checks `id == next_id()` against an in-memory
//! vec on an fd it holds open — and an inbox has many senders. A directory
//! of files is the right primitive; a maildir, not a log.
//!
//! A message addressed to **one** session also rings to be answered: one
//! agent naming another is one agent asking it for something, and a note
//! that waits for an operator watching a different pane arrives an hour
//! late. A fan-out on the channel does not — "everyone on this
//! repository" is an announcement, and an announcement that starts a turn
//! in every session at once is how a swarm spends its time on
//! acknowledgements. Both of those are *defaults* rather than properties
//! of the two kinds — a channel message that everyone really must act on
//! says so and wakes them all. What a wake buys is the same in every
//! state: a recipient mid-turn takes the message at its next safe point
//! and steers with it; an idle one starts a turn. Without it, the
//! message waits in the inbox for a turn boundary.
//!
//! ## What is not here
//!
//! When a drained message becomes part of the conversation is the agent's
//! business, not this crate's. The answer is *at a turn boundary, and
//! mid-turn — for the mail marked to wake — at the same safe points as a
//! steer*, never between an assistant's `tool_use` and its results; see
//! [`eidolon_core::peer`]. This crate only knows how to find a session,
//! hand it a message, and take delivery of one.

pub mod api;
pub mod inbox;
pub mod keypool;
pub mod presence;
pub mod socket;

pub use api::{CHANNEL, roster, roster_of, send, send_external, tilde};
pub use inbox::Envelope;
pub use presence::{Delivery, Meta, Peer, Presence, channel_key_of, scan};

/// Wall-clock milliseconds. Used for envelope timestamps and ids.
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// FNV-1a over a string. Session ids want a few stable hex digits and
/// nothing more; pulling in a hasher crate for that would be silly.
pub(crate) fn hash16(s: &str) -> u16 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    (h ^ (h >> 16) ^ (h >> 32) ^ (h >> 48)) as u16
}
