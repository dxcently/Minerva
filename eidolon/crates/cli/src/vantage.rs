//! What a park's condition is asked about, where the dependencies live.
//!
//! `eidolon_core::wait` owns the condition algebra and evaluates it, and
//! deliberately owns none of the facts: a peer's state is the swarm's
//! doorbell, a background command's exit status is `eidolon-tools`' file,
//! whether a path exists is the filesystem's, and whether the vault's log has
//! moved is a remote client's. This is the adapter that answers those
//! questions for one session. It sits in the binary rather than in a crate
//! because the binary is the one place they meet.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use eidolon_core::wait::{Answer, Condition, Term, Vantage, POLL};
use eidolon_remote::{Mneme, Tail, TailQuery};
use eidolon_swarm::{Presence, socket};

/// How much of a log `log_matches` searches. A development server's log is
/// small; a runaway one is not, and a readiness check that reads a gigabyte
/// per sample costs more than the answer is worth. What is read is the
/// *end* of the file, which is where a readiness line lands.
const LOG_READ_CAP: u64 = 4 * 1024 * 1024;

/// How often a condition that names the vault is sampled.
///
/// A Mneme sample is not a file read: it opens an MCP session and posts the
/// call, against a service that may be on the far side of a tunnel. At
/// [`POLL`]'s 200 ms that is ten requests a second for every parked session;
/// two seconds buys a wake that is two seconds late, and a parked session is
/// asleep, so lateness is the whole of what it costs.
const VAULT_CADENCE: Duration = Duration::from_secs(2);

pub struct Facts {
    /// The session's working directory, for the paths the model gave
    /// relative — the same resolution every other tool does.
    cwd: PathBuf,
    /// This session's swarm registration, for asking a peer a question.
    /// `None` when the session is not in a swarm: a condition naming a peer
    /// then answers `Gone`, which is the truth — there is nobody to ask.
    swarm: Option<Arc<Presence>>,
    /// The vault client, when this session has one. `None` is a session
    /// configured without Mneme: a condition naming the vault then answers
    /// unanswerable, which is also the truth, and it says so at arm time
    /// rather than after a deadline.
    mneme: Option<Mneme>,
}

impl Facts {
    pub fn new(cwd: PathBuf, swarm: Option<Arc<Presence>>, mneme: Option<Mneme>) -> Self {
        Self { cwd, swarm, mneme }
    }

    fn resolve(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.cwd.join(path)
        }
    }

    /// The status file a background command's reaper writes, derived from the
    /// log path the model was handed: `bg-<id>.log` → `bg-<id>.exit`. The
    /// naming convention is `eidolon_tools::shell`'s, and this is the one
    /// place outside it that depends on it.
    fn exit_path(log: &Path) -> PathBuf {
        log.with_extension("exit")
    }

    /// Ask a session what it is doing. Nothing answering is expected rather
    /// than an error: a session that died is a fact about the condition.
    fn peer_state(&self, id: &str) -> Answer {
        if self.swarm.is_none() {
            return gone(id);
        }
        // A unique prefix is enough, the way `send` resolves one, because
        // that is the spelling a session has in hand from `peers`.
        let found = eidolon_swarm::scan(&Presence::root())
            .into_iter()
            .find(|p| p.meta.id == id || p.meta.id.starts_with(id));
        let Some(peer) = found else {
            return gone(id);
        };
        // The peer answers about its own loop — the only answer that is not
        // a guess. The roster's `busy` is written by one driver only, so it
        // reads as permanently idle for a headless `run` or `chat` peer and
        // must never be what a wait is decided on.
        match socket::request(&peer.socket(), &serde_json::json!({ "op": "state" })) {
            Ok(reply) if reply.get("ok").and_then(serde_json::Value::as_bool) == Some(true) => {
                match reply.get("busy").and_then(serde_json::Value::as_bool) {
                    Some(true) => Answer::No,
                    _ => Answer::Yes,
                }
            }
            // No answer at all: gone, or wedged. Nothing can be asked of it
            // either way, which is the whole of what "gone" means — and it
            // resolves the park instead of burning its deadline.
            _ => gone(id),
        }
    }

    /// Has the background command finished? The exit file is written by the
    /// reaper through a temporary and a rename, so one that parses is one
    /// that finished.
    fn exit_status(&self, log: &Path) -> Answer {
        let body = match std::fs::read(Facts::exit_path(&self.resolve(log))) {
            Ok(body) => body,
            Err(_) => return Answer::No,
        };
        match serde_json::from_slice::<eidolon_tools::shell::BackgroundExit>(&body) {
            Ok(_) => Answer::Yes,
            Err(_) => Answer::No,
        }
    }

    /// Does the log hold this text? A literal match, not a pattern: a wait is
    /// a readiness check, and the alternative was a regex engine in the crate
    /// that owns the policy instead of the one that owns the tools.
    fn log_matches(&self, log: &Path, pattern: &str) -> Answer {
        match tail(&self.resolve(log)) {
            Some(text) => answer(text.contains(pattern)),
            None => Answer::No,
        }
    }

    /// One sample of the vault's log: has it moved past `since` under this
    /// filter?
    ///
    /// The sample never latches and never holds a request — `eidolon_remote`
    /// is where that is decided and documented — and a failure that is not one
    /// of Mneme's two permanent answers keeps the park waiting. The deadline
    /// is what bounds it, and a wake saying "this can never fire" because the
    /// network blinked would be worse than a late one.
    async fn vault_sample(
        &self,
        vault: &str,
        since: u64,
        path: Option<&str>,
        written_by: Option<&str>,
    ) -> Answer {
        let Some(mneme) = &self.mneme else {
            return Answer::Unanswerable("this session has no vault".to_string());
        };
        let q = TailQuery {
            vault: vault.to_string(),
            since,
            path: path.map(str::to_string),
            written_by: written_by.map(str::to_string),
        };
        match mneme.audit_tail(&q).await {
            Ok(Tail::Moved) => Answer::Yes,
            Ok(Tail::Still) => Answer::No,
            Ok(Tail::Gone(why)) => Answer::Unanswerable(why),
            Err(e) => {
                tracing::debug!(error = %e, "a vault sample failed; the park keeps waiting");
                Answer::No
            }
        }
    }
}

fn answer(holds: bool) -> Answer {
    if holds {
        Answer::Yes
    } else {
        Answer::No
    }
}

fn gone(id: &str) -> Answer {
    Answer::Unanswerable(format!("session {id} is gone"))
}

#[async_trait]
impl Vantage for Facts {
    /// The slowest leaf in the condition sets the pace for all of it: one
    /// sample asks every leaf, so a fast leaf cannot be sampled faster than
    /// the slow one sharing its condition.
    fn cadence(&self, condition: &Condition) -> Duration {
        if condition.terms().iter().any(|t| matches!(t, Term::MnemeSeq { .. })) {
            VAULT_CADENCE
        } else {
            POLL
        }
    }

    async fn ask(&self, term: &Term) -> Answer {
        match term {
            Term::PeerIdle(id) => self.peer_state(id),
            Term::CommandFinished(log) => self.exit_status(log),
            Term::FileExists(path) => answer(self.resolve(path).exists()),
            Term::LogMatches { log, pattern } => self.log_matches(log, pattern),
            Term::MnemeSeq { vault, since, path, written_by } => {
                self.vault_sample(vault, *since, path.as_deref(), written_by.as_deref())
                    .await
            }
        }
    }
}

/// The end of a file, as text: up to [`LOG_READ_CAP`] bytes of it, so a
/// growing log is not read whole on every sample.
fn tail(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    if len > LOG_READ_CAP {
        file.seek(SeekFrom::Start(len - LOG_READ_CAP)).ok()?;
    }
    let mut bytes = Vec::new();
    file.take(LOG_READ_CAP).read_to_end(&mut bytes).ok()?;
    // Lossy on purpose: seeking into the middle of a log can land inside a
    // multi-byte character, and losing one byte of a preview is better than
    // refusing to answer about a file that is plainly there.
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn facts() -> Facts {
        Facts::new(std::env::temp_dir(), None, None)
    }

    fn condition(v: serde_json::Value) -> Condition {
        Condition::parse(&v).expect("a condition")
    }

    /// The pace is the slowest leaf's, and the vault's leaf is the one that
    /// leaves the machine: sampling it at `POLL` would be ten requests a
    /// second per parked session, against a service that may be a tunnel away.
    #[test]
    fn a_condition_naming_the_vault_is_sampled_at_the_vaults_pace() {
        let f = facts();
        assert_eq!(f.cadence(&condition(json!({ "file_exists": "/tmp/ready" }))), POLL);
        assert_eq!(
            f.cadence(&condition(json!({
                "all": [
                    { "file_exists": "/tmp/ready" },
                    { "mneme_seq": { "vault": "default", "since": 1 } }
                ]
            }))),
            VAULT_CADENCE,
            "one remote leaf slows the whole condition"
        );
    }

    /// A session configured without Mneme cannot answer about the vault, and
    /// says so rather than parking: a condition nobody can satisfy is not a
    /// wait, it is a deadline.
    #[tokio::test]
    async fn a_vault_leaf_without_a_vault_is_unanswerable() {
        let term = Term::MnemeSeq {
            vault: "default".into(),
            since: 1,
            path: None,
            written_by: None,
        };
        match facts().ask(&term).await {
            Answer::Unanswerable(why) => assert!(why.contains("no vault"), "{why}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
}
