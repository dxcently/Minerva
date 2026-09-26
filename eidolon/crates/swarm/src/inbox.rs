//! The maildir: one file per undelivered message.
//!
//! Written by the sender, read and removed by the recipient. Every write
//! lands under a temporary name and is renamed into place, so a reader
//! never sees half a message and two senders never collide — the ordering
//! that falls out of the filename is delivery order, which is as much
//! ordering as a swarm needs.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// One message in flight.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Envelope {
    /// The sending session's id.
    pub from: String,
    /// Where the sender is working. Shown to the recipient, since "who"
    /// is much less useful than "who, and in which tree".
    pub from_cwd: String,
    /// `None` for a direct message; the channel key for a fan-out.
    #[serde(default)]
    pub channel: Option<String>,
    pub text: String,
    /// Whether an idle recipient should start a turn on this message
    /// rather than wait for its operator. Decided by the sender — see
    /// [`crate::api::send`], where a direct message wakes and a fan-out
    /// does not — and carried here as the settled answer, since a
    /// recipient reading its inbox has no idea what the default was.
    #[serde(default)]
    pub wake: bool,
    /// Whether this was sent from outside any session — `eidolon send`,
    /// not a peer — which decides the framing the recipient's model will
    /// see: a tool injecting text, never an operator or a colleague.
    /// Absent on every envelope written before the flag existed, which
    /// reads as `false`: those were peers.
    #[serde(default)]
    pub external: bool,
    pub ts_ms: u64,
}

impl Envelope {
    pub fn new(from: &str, from_cwd: &str, text: &str) -> Self {
        Envelope {
            from: from.to_string(),
            from_cwd: from_cwd.to_string(),
            channel: None,
            text: text.to_string(),
            wake: false,
            external: false,
            ts_ms: crate::now_ms(),
        }
    }

    pub fn on_channel(mut self, channel: &str) -> Self {
        self.channel = Some(channel.to_string());
        self
    }

    pub fn waking(mut self, wake: bool) -> Self {
        self.wake = wake;
        self
    }

    /// Mark this as coming from outside any session. See the field.
    pub fn external(mut self) -> Self {
        self.external = true;
        self
    }
}

/// Write one message into `inbox`. Temp-then-rename, so a recipient
/// draining concurrently either sees the whole message or does not see it.
///
/// The name carries the time first so a plain sort is delivery order. Two
/// senders colliding on the same stem — same sender, same text, same
/// millisecond, which is exactly the shape of a scripted double-send from
/// `eidolon send` — take a numeric suffix rather than silently replacing
/// one another: an inbox must never drop mail to a name collision.
pub fn deliver(inbox: &Path, env: &Envelope) -> Result<()> {
    std::fs::create_dir_all(inbox).with_context(|| format!("creating {}", inbox.display()))?;
    let body = serde_json::to_vec(env)?;
    // Enough of the sender to keep two simultaneous senders apart.
    let stem = format!(
        "{}-{:04x}-{:04x}",
        env.ts_ms,
        crate::hash16(&env.from),
        crate::hash16(&env.text)
    );
    let tmp = inbox.join(format!(".{stem}.tmp"));
    let mut final_path = inbox.join(format!("{stem}.json"));
    for n in 1.. {
        if !final_path.exists() {
            break;
        }
        final_path = inbox.join(format!("{stem}-{n}.json"));
    }
    std::fs::write(&tmp, &body).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &final_path)
        .with_context(|| format!("renaming into {}", final_path.display()))?;
    Ok(())
}

/// Take everything waiting, oldest first, removing each as it is read.
///
/// Never returns an error: a message that will not parse is a message
/// that is dropped and logged, because the alternative is an inbox that
/// wedges a session forever on one bad file.
pub fn drain(inbox: &Path) -> Vec<Envelope> {
    take(inbox, false)
}

/// Take only the envelopes marked to wake, leaving the rest where they
/// are for a boundary drain.
///
/// The mid-turn half of delivery: a waking message reaches the recipient's
/// turn in flight at the next safe point, and a note that did not ask to
/// wake must survive this call untouched in the inbox — the file is the
/// durable copy, and removing it here would lose the message. A file that
/// will not parse is still dropped, for the same reason as [`drain`].
pub fn drain_waking(inbox: &Path) -> Vec<Envelope> {
    take(inbox, true)
}

/// The one walker both drains share: read every message file oldest
/// first, remove the ones being taken, keep the rest for later.
fn take(inbox: &Path, waking_only: bool) -> Vec<Envelope> {
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(inbox)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    let mut out = Vec::new();
    for path in files {
        let parsed = std::fs::read(&path)
            .map_err(anyhow::Error::from)
            .and_then(|b| Ok(serde_json::from_slice::<Envelope>(&b)?));
        match parsed {
            Ok(env) if waking_only && !env.wake => {
                // Not being taken: leave the file exactly where it is.
                continue;
            }
            Ok(env) => out.push(env),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "dropping an unreadable inbox message")
            }
        }
        let _ = std::fs::remove_file(&path);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivery_is_atomic_and_drains_oldest_first() {
        let tmp = tempfile::tempdir().unwrap();
        let inbox = tmp.path().join("inbox");
        let mut a = Envelope::new("one", "/tmp/a", "first");
        a.ts_ms = 1;
        let mut b = Envelope::new("two", "/tmp/b", "second");
        b.ts_ms = 2;
        deliver(&inbox, &b).unwrap();
        deliver(&inbox, &a).unwrap();

        // Nothing half-written is ever visible: only the renamed files
        // carry the `.json` extension the drain looks for.
        assert!(
            std::fs::read_dir(&inbox).unwrap().all(|e| e
                .unwrap()
                .path()
                .extension()
                .is_some_and(|x| x == "json"))
        );

        let got = drain(&inbox);
        assert_eq!(
            got.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
            ["first", "second"]
        );
        assert!(drain(&inbox).is_empty(), "draining removes what it took");
    }

    #[test]
    fn one_bad_message_does_not_wedge_the_inbox() {
        let tmp = tempfile::tempdir().unwrap();
        let inbox = tmp.path().join("inbox");
        deliver(&inbox, &Envelope::new("one", "/tmp/a", "good")).unwrap();
        std::fs::write(inbox.join("0000-0000-0000.json"), b"{ not json").unwrap();
        let got = drain(&inbox);
        assert_eq!(got.len(), 1, "the good one still arrives");
        assert!(
            drain(&inbox).is_empty(),
            "and the bad one is gone rather than retried forever"
        );
    }

    #[test]
    fn an_envelope_from_before_external_existed_reads_as_a_peer() {
        // Written by a build that had no `external` field: the flag was
        // added with a serde default rather than a format change, and
        // this is the proof an in-flight envelope still drains.
        let body = r#"{"from":"eidolon-1","from_cwd":"/p","channel":null,"text":"hey","wake":true,"ts_ms":1}"#;
        let e: Envelope = serde_json::from_str(body).unwrap();
        assert!(!e.external);
        assert!(e.wake);
        assert_eq!(e.text, "hey");
    }

    #[test]
    fn two_identical_sends_in_one_millisecond_both_survive() {
        let tmp = tempfile::tempdir().unwrap();
        let inbox = tmp.path().join("inbox");
        let mut a = Envelope::new("aoide", "/tmp/a", "same text");
        a.ts_ms = 7;
        let mut b = a.clone();
        b.wake = true;
        deliver(&inbox, &a).unwrap();
        deliver(&inbox, &b).unwrap();

        let got = drain(&inbox);
        assert_eq!(got.len(), 2, "a name collision must not drop mail");
        assert!(got.iter().any(|e| e.wake) && got.iter().any(|e| !e.wake));
    }

    #[test]
    fn draining_a_directory_that_does_not_exist_is_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(drain(&tmp.path().join("nope")).is_empty());
    }

    #[test]
    fn a_waking_drain_takes_only_the_asks_and_keeps_the_notes() {
        let tmp = tempfile::tempdir().unwrap();
        let inbox = tmp.path().join("inbox");
        let mut note = Envelope::new("one", "/tmp/a", "for the next boundary");
        note.ts_ms = 1;
        let mut ask = Envelope::new("two", "/tmp/b", "steer with this");
        ask.ts_ms = 2;
        ask.wake = true;
        let mut later = Envelope::new("three", "/tmp/c", "a second note");
        later.ts_ms = 3;
        deliver(&inbox, &note).unwrap();
        deliver(&inbox, &ask).unwrap();
        deliver(&inbox, &later).unwrap();

        let got = drain_waking(&inbox);
        assert_eq!(
            got.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
            ["steer with this"],
            "only the waking mail is taken"
        );
        // The notes are still files, oldest first when a boundary finally
        // takes everything — the kept one does not jump the later arrival.
        let rest = drain(&inbox);
        assert_eq!(
            rest.iter().map(|e| e.text.as_str()).collect::<Vec<_>>(),
            ["for the next boundary", "a second note"]
        );
    }

    #[test]
    fn a_waking_drain_still_drops_an_unreadable_message() {
        let tmp = tempfile::tempdir().unwrap();
        let inbox = tmp.path().join("inbox");
        let mut good = Envelope::new("one", "/tmp/a", "good");
        good.wake = true;
        deliver(&inbox, &good).unwrap();
        std::fs::write(inbox.join("0000-0000-0000.json"), b"{ not json").unwrap();
        assert_eq!(drain_waking(&inbox).len(), 1);
        assert!(
            drain(&inbox).is_empty(),
            "the bad file is gone rather than retried forever"
        );
    }
}
