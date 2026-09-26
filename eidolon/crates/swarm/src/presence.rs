//! The registry: a directory per live session, and the roster built from it.
//!
//! Registration is a `mkdir` and one small write, which is why it can
//! happen on the way to the first frame without spending the startup
//! budget. Nothing here opens a session log, spawns a process or waits on
//! anything — the roster is built when something asks for it, never at
//! boot.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::inbox::{self, Envelope};
use crate::socket;

/// A session that has just registered has written its `meta.json` but may
/// not have bound its socket yet. Sweeping it for failing a probe would
/// be a session deleting its neighbour's registration during that
/// neighbour's startup, so a directory is only ever swept once it is old
/// enough that the gap cannot be explained by starting up.
///
/// Shared with [`keypool`], whose lane claims ask the same question of a
/// *holder* that the sweep asks of a registration.
pub(crate) const GRACE_MS: u64 = 10_000;

/// What a live session publishes about itself. Written once at
/// registration and rewritten when something in it changes; a reader
/// treats it as a description, never as proof of life on its own — an
/// answer is the socket's job, and the pid is the second opinion asked
/// when the socket is silent.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Meta {
    pub id: String,
    pub pid: u32,
    /// The session log this session is writing.
    pub log: PathBuf,
    /// Where its tools run.
    pub cwd: PathBuf,
    /// `git rev-parse --git-common-dir` by another route: the directory
    /// every worktree of one repository shares. The channel keys on this
    /// rather than on `cwd`, because one worktree per agent gives every
    /// agent a different `cwd` and a cwd-keyed channel would split a
    /// swarm into singletons — exactly backwards.
    pub repo: Option<PathBuf>,
    pub model: String,
    pub started_ms: u64,
    /// The first thing asked of this session: what it is remembered by,
    /// and the only cheap answer to "what is it working on".
    pub title: String,
    /// Whether a turn is in flight right now.
    pub busy: bool,
}

/// Another live session.
#[derive(Clone, Debug)]
pub struct Peer {
    pub meta: Meta,
    pub dir: PathBuf,
}

impl Peer {
    pub fn inbox(&self) -> PathBuf {
        self.dir.join("inbox")
    }
    pub fn socket(&self) -> PathBuf {
        self.dir.join("sock")
    }
    /// Write a message into this peer's inbox and ring its doorbell —
    /// the delivery half of [`Presence::deliver`], free of any sender:
    /// `eidolon send` and the other outside callers have a roster, not a
    /// presence of their own.
    pub fn deliver(&self, env: &Envelope) -> Result<Delivery> {
        inbox::deliver(&self.inbox(), env)?;
        Ok(match socket::ring(&self.socket()) {
            Ok(()) => Delivery::Delivered,
            Err(e) => {
                tracing::debug!(peer = %self.meta.id, error = %e, "message written but the doorbell went unanswered");
                Delivery::Queued
            }
        })
    }
}

/// What became of a message. Both outcomes mean the message is on disk in
/// the recipient's inbox; they differ in whether anyone was told.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delivery {
    /// The recipient answered its doorbell.
    Delivered,
    /// Written, but the doorbell went unanswered — the recipient is
    /// wedged or dying. It will still find the message if it recovers.
    Queued,
}

/// This session's registration. Dropping it deregisters.
pub struct Presence {
    root: PathBuf,
    dir: PathBuf,
    id: String,
    meta: RwLock<Meta>,
}

impl Presence {
    /// Where registrations live. `$XDG_RUNTIME_DIR/eidolon`, falling back
    /// to the temp dir — never under the session tree, because a unix
    /// socket path is capped at ~108 bytes and a sessions directory is
    /// not built to respect that.
    pub fn root() -> PathBuf {
        std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("eidolon")
    }

    /// Announce this session. The id is derived from where it is working
    /// and which log it writes — `eidolon-9f2c` — because a peer has to
    /// be able to *type* it, and a millisecond timestamp is not that.
    pub fn register(
        root: &Path,
        log: &Path,
        cwd: &Path,
        model: &str,
        title: &str,
    ) -> Result<Arc<Presence>> {
        std::fs::create_dir_all(root).with_context(|| format!("creating {}", root.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700));
        }

        let base = name_for(cwd, log);
        let (id, dir) = (0u32..64)
            .map(|n| {
                if n == 0 {
                    base.clone()
                } else {
                    format!("{base}-{n}")
                }
            })
            .map(|id| {
                let dir = root.join(&id);
                (id, dir)
            })
            .find(|(_, dir)| !dir.exists())
            .context("could not find a free session id")?;

        std::fs::create_dir_all(dir.join("inbox"))
            .with_context(|| format!("creating {}", dir.display()))?;
        let meta = Meta {
            id: id.clone(),
            pid: std::process::id(),
            log: log.to_path_buf(),
            cwd: cwd.to_path_buf(),
            repo: repo_of(cwd),
            model: model.to_string(),
            started_ms: crate::now_ms(),
            title: title.to_string(),
            busy: false,
        };
        let p = Presence {
            root: root.to_path_buf(),
            dir,
            id,
            meta: RwLock::new(meta),
        };
        p.write_meta()?;
        Ok(Arc::new(p))
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn socket_path(&self) -> PathBuf {
        self.dir.join("sock")
    }

    pub fn inbox_dir(&self) -> PathBuf {
        self.dir.join("inbox")
    }

    pub fn meta(&self) -> Meta {
        self.meta.read().unwrap().clone()
    }

    /// The key a channel send fans out over: the repository if there is
    /// one, else the working directory.
    pub fn channel_key(&self) -> PathBuf {
        let m = self.meta.read().unwrap();
        m.repo.clone().unwrap_or_else(|| m.cwd.clone())
    }

    /// Change something a peer can see. Best-effort: failing to publish a
    /// new title is not worth failing a turn over.
    pub fn update(&self, f: impl FnOnce(&mut Meta)) {
        f(&mut self.meta.write().unwrap());
        if let Err(e) = self.write_meta() {
            tracing::warn!(error = %e, "could not update presence");
        }
    }

    pub fn set_busy(&self, busy: bool) {
        if self.meta.read().unwrap().busy != busy {
            self.update(|m| m.busy = busy);
        }
    }

    /// Publish the model this session is on, so a peer's roster shows what
    /// it is running *now* rather than what it launched on.
    ///
    /// Written at registration and again whenever the model moves — `:model`
    /// onto another provider, or adopting a log written on one — because
    /// the roster answers "what is this session doing", and a session that
    /// switched to the cheap lane an hour ago is not the session its
    /// neighbours should still be reading. Same shape as [`Self::set_busy`]:
    /// a write only when the answer changes, and best-effort.
    pub fn set_model(&self, model: &str) {
        if self.meta.read().unwrap().model != model {
            self.update(|m| m.model = model.to_string());
        }
    }

    fn write_meta(&self) -> Result<()> {
        let body = serde_json::to_vec_pretty(&*self.meta.read().unwrap())?;
        let tmp = self.dir.join(".meta.tmp");
        std::fs::write(&tmp, &body).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, self.dir.join("meta.json")).context("publishing meta.json")?;
        Ok(())
    }

    /// Every other live session, newest registration last.
    pub fn peers(&self) -> Vec<Peer> {
        scan(&self.root)
            .into_iter()
            .filter(|p| p.dir != self.dir)
            .collect()
    }

    /// The peers sharing this session's repository (or, outside a repo,
    /// its working directory) — the membership of the group channel,
    /// which is a *query* over presence and not an object anyone has to
    /// keep up to date.
    pub fn channel(&self) -> Vec<Peer> {
        let key = self.channel_key();
        self.peers()
            .into_iter()
            .filter(|p| p.meta.repo.as_ref().unwrap_or(&p.meta.cwd) == &key)
            .collect()
    }

    /// Resolve what an operator or a model typed. An exact id wins; a
    /// unique prefix is accepted, because the ids are long enough to be
    /// tedious and short enough to be unambiguous in a swarm of four.
    pub fn find(&self, name: &str) -> Option<Peer> {
        find_in(&self.peers(), name)
    }

    /// Write a message into one peer's inbox and ring its doorbell.
    pub fn deliver(&self, peer: &Peer, env: &Envelope) -> Result<Delivery> {
        peer.deliver(env)
    }

    /// The envelope this session would send: its own id and directory
    /// already filled in.
    pub fn envelope(&self, text: &str) -> Envelope {
        let m = self.meta.read().unwrap();
        Envelope::new(&m.id, &m.cwd.display().to_string(), text)
    }

    /// Leave the roster now rather than when the process exits — the close
    /// half of a quiesce (see `eidolon_core::quiesce`).
    ///
    /// The directory, its `meta.json`, its socket and its inbox go, so the
    /// next scan sees no session at all: a quiesced session is *deliberately*
    /// dark, because its presence belongs to the venue it moved to, and a
    /// roster still listing it is a neighbour sending mail to a journal that
    /// has stopped. Best-effort and idempotent — dropping the presence does
    /// the same thing, and a second call finds nothing left to remove.
    pub fn deregister(&self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Drop for Presence {
    fn drop(&mut self) {
        self.deregister();
    }
}

/// A session's own inbox, drained as the core's peer seam.
impl eidolon_core::peer::PeerInbox for Presence {
    fn drain(&self) -> Vec<eidolon_core::peer::PeerMessage> {
        mail_of(inbox::drain(&self.inbox_dir()))
    }

    fn drain_waking(&self) -> Vec<eidolon_core::peer::PeerMessage> {
        mail_of(inbox::drain_waking(&self.inbox_dir()))
    }
}

/// The maildir's envelope as the core sees it. Shared by both drains so
/// the mapping lives once.
fn mail_of(envs: Vec<inbox::Envelope>) -> Vec<eidolon_core::peer::PeerMessage> {
    envs.into_iter()
        .map(|e| eidolon_core::peer::PeerMessage {
            from: e.from,
            from_cwd: e.from_cwd,
            channel: e.channel,
            text: e.text,
            wake: e.wake,
            external: e.external,
        })
        .collect()
}

/// Every live session registered under `root`, oldest first.
///
/// Sessions that died without cleaning up are swept as they are found:
/// their socket does not answer, the process their `meta.json` names is
/// gone, and they are past the startup grace period. A silent socket
/// alone does not sweep — the probe's quarter-second timeout is a thing
/// a busy session fails, and a registration can also predate its
/// doorbell (the cli registers before it serves, and a failed bind
/// deliberately leaves the registration in place) — so silence is
/// checked against the registration's pid before anything is deleted.
/// Nothing else sweeps, which is deliberate — a scan happens when
/// something asks who is there, and asking is exactly when a stale entry
/// would otherwise be believed.
///
/// The token pool shares this root and is *not* a registration: a lane
/// directory holds a `holder` and never a `meta.json`, so the sweep's
/// first reading of it would be debris, and the pool would be deleted by
/// an ordinary `peers` call — the exact two-sessions-on-one-key collision
/// it exists to prevent. `keypool` is skipped by name, the one spelling
/// living in [`crate::keypool::POOL_DIR`]; a lane's own liveness is its
/// holder's presence, asked by a claimant rather than by this sweep.
///
/// Free-standing rather than a method because the question is also asked
/// by things that are not sessions: `eidolon peers` answers it without
/// registering a session of its own to ask with.
pub fn scan(root: &Path) -> Vec<Peer> {
    let mut out: Vec<Peer> = Vec::new();
    for entry in std::fs::read_dir(root).into_iter().flatten().flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        if entry.file_name() == crate::keypool::POOL_DIR {
            continue;
        }
        let Some(meta) = read_meta(&dir) else {
            // A directory with no readable meta is either mid-registration
            // or debris; sweep it once it is old enough that the first
            // explanation is out.
            sweep_if_stale(&dir, None);
            continue;
        };
        // The probe is the positive test: an answer is a live session.
        // Silence is a verdict only when the process that registered is
        // gone too — a quarter-second timeout is a thing a busy session
        // fails, and this sweep is the only removal a peer's scan can
        // cause, so it must not fire on a guess. A live pid stays in the
        // roster: mail lands in its real inbox and reports `Queued`
        // while the doorbell is silent, which is the honest outcome.
        if socket::probe(&dir.join("sock")) || pid_live(meta.pid) {
            out.push(Peer { meta, dir });
        } else {
            sweep_if_stale(&dir, Some(meta.started_ms));
        }
    }
    out.sort_by_key(|p| p.meta.started_ms);
    out
}

/// What a session in `cwd` would put on a channel: its repository, or the
/// directory itself outside one.
pub fn channel_key_of(cwd: &Path) -> PathBuf {
    repo_of(cwd).unwrap_or_else(|| cwd.to_path_buf())
}

/// Resolve a name to one peer: an exact id, or a prefix only when it is
/// unique. The one resolution rule, shared by a session's `send` and by
/// `eidolon send`, which has a scanned roster rather than a presence.
pub fn find_in(peers: &[Peer], name: &str) -> Option<Peer> {
    if let Some(p) = peers.iter().find(|p| p.meta.id == name) {
        return Some(p.clone());
    }
    let mut matches = peers.iter().filter(|p| p.meta.id.starts_with(name));
    let first = matches.next()?.clone();
    matches.next().is_none().then_some(first)
}

fn read_meta(dir: &Path) -> Option<Meta> {
    let body = std::fs::read(dir.join("meta.json")).ok()?;
    serde_json::from_slice(&body).ok()
}

/// Is a process with this pid alive? The sweep's second opinion, asked
/// when the socket is silent: `kill(pid, 0)` sends no signal, it asks
/// whether it could, and that is the whole question. The registry is one
/// machine and one user (`$XDG_RUNTIME_DIR`, `0700`), so any pid in a
/// `meta.json` this process can read was minted by a sibling beside it.
/// A reaped pid can be recycled onto a stranger, which errs the only
/// safe way: a live registration is never deleted, and a stale one
/// lingers in the roster until the stranger exits. `EPERM` — a live
/// process this user may not signal — counts as alive for the same
/// reason. Zero and anything too wide for a `pid_t` are refused before
/// the call: `kill` reads them as process-group targets (my whole
/// group / every group I may signal) and answers success, so a forged
/// or broken `pid` would read as alive forever.
fn pid_live(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    // SAFETY: `kill` with signal 0 writes nothing and touches no memory;
    // errno means something only when it returned -1.
    let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
    ret == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn sweep_if_stale(dir: &Path, started_ms: Option<u64>) {
    let age = match started_ms {
        Some(t) => crate::now_ms().saturating_sub(t),
        // No readable meta: fall back to the directory's own mtime, and
        // keep it if even that cannot be read.
        None => match dir
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
        {
            Some(d) => d.as_millis() as u64,
            None => return,
        },
    };
    if age > GRACE_MS {
        tracing::debug!(dir = %dir.display(), "sweeping the registration of a session that is gone");
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// `<directory>-<four hex of the log path>`: says where a session is
/// working, and stays unique between two sessions in the same place.
fn name_for(cwd: &Path, log: &Path) -> String {
    let base: String = cwd
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "session".into())
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .take(24)
        .collect();
    let base = if base.is_empty() {
        "session".to_string()
    } else {
        base
    };
    format!("{base}-{:04x}", crate::hash16(&log.display().to_string()))
}

/// The repository's common directory, found the way git finds it but
/// without spawning git.
///
/// A `git rev-parse --git-common-dir` is five milliseconds, and the whole
/// startup budget is ten. Walking up for `.git` costs a few `stat`s: a
/// directory is an ordinary checkout, a *file* is a worktree and names
/// the per-worktree gitdir, whose `commondir` points back at the one
/// directory every worktree of the repository shares.
fn repo_of(cwd: &Path) -> Option<PathBuf> {
    let mut here = Some(cwd);
    while let Some(dir) = here {
        let dot = dir.join(".git");
        if dot.is_dir() {
            return dot.canonicalize().ok().or(Some(dot));
        }
        if dot.is_file() {
            let body = std::fs::read_to_string(&dot).ok()?;
            let gitdir = body.strip_prefix("gitdir:")?.trim();
            let gitdir = dir.join(gitdir);
            let common = std::fs::read_to_string(gitdir.join("commondir"))
                .ok()
                .map(|c| gitdir.join(c.trim()))
                // Without a `commondir` the layout is still
                // `<common>/worktrees/<name>`, so go up twice.
                .or_else(|| {
                    gitdir
                        .parent()
                        .and_then(Path::parent)
                        .map(Path::to_path_buf)
                })?;
            return common.canonicalize().ok().or(Some(common));
        }
        here = dir.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use eidolon_core::peer::PeerInbox;

    fn register(root: &Path, cwd: &Path, name: &str) -> Arc<Presence> {
        Presence::register(root, &cwd.join(format!("{name}.eid")), cwd, "mock", name).unwrap()
    }

    /// A pid that is certainly gone: a child this test spawned and
    /// reaped. A forged meta's pid is a claim the test makes, so death
    /// is manufactured rather than guessed.
    fn a_reaped_pid() -> u32 {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    }

    #[test]
    fn a_registration_is_visible_to_a_peer_and_vanishes_when_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("run");
        let cwd = tmp.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();

        let a = register(&root, &cwd, "a");
        let b = register(&root, &cwd, "b");
        assert_ne!(
            a.id(),
            b.id(),
            "two sessions in one directory get different ids"
        );

        // Neither socket is served in this test, so neither answers the
        // probe — but both processes are alive, so `b` stays in the
        // roster: the roster is who a `send` can resolve, and a session
        // whose door is merely silent is resolvable.
        let peers = a.peers();
        assert_eq!(
            peers.len(),
            1,
            "the neighbour is listed though its socket never answered"
        );
        assert_eq!(peers[0].meta.id, b.id());
        assert!(
            b.dir().exists(),
            "and it is not swept while it is still young"
        );

        let dir = b.dir().to_path_buf();
        drop(b);
        assert!(!dir.exists(), "dropping a registration deregisters it");
    }

    /// A session that switches models publishes so at once: the roster is
    /// what a neighbour reads to decide whether to address it, and the meta
    /// carries the model it registered with until something says otherwise,
    /// so a `:model` that published nothing left peers reading the launch
    /// model for the rest of the session.
    #[test]
    fn a_model_switch_is_published_to_the_roster() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("run");
        let cwd = tmp.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        let a = register(&root, &cwd, "a");

        assert_eq!(a.meta().model, "mock", "registered on what it launched on");
        a.set_model("deepseek:deepseek-v4-pro");

        // Read back off disk, not out of the struct: the point of the
        // roster is that another process reads the file.
        let meta = read_meta(&root.join(a.id())).unwrap();
        assert_eq!(
            meta.model, "deepseek:deepseek-v4-pro",
            "the switch is what a peer now reads"
        );

        // And a re-publish of the same model is not a write, which is what
        // makes calling it on every command cheap.
        a.set_model("deepseek:deepseek-v4-pro");
        assert_eq!(a.meta().model, "deepseek:deepseek-v4-pro");
    }

    #[test]
    fn an_old_registration_with_no_socket_is_swept() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("run");
        let cwd = tmp.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        let a = register(&root, &cwd, "a");

        // A session that died without cleaning up: its meta is on disk,
        // its socket is not, its pid is gone, and it is older than the
        // grace period.
        let dead = root.join("dead-0000");
        std::fs::create_dir_all(dead.join("inbox")).unwrap();
        let mut m = a.meta();
        m.id = "dead-0000".into();
        m.pid = a_reaped_pid();
        m.started_ms = crate::now_ms() - GRACE_MS - 1;
        std::fs::write(dead.join("meta.json"), serde_json::to_vec(&m).unwrap()).unwrap();

        assert!(a.peers().is_empty());
        assert!(
            !dead.exists(),
            "the dead session's registration is swept as it is found"
        );
    }

    /// The incident: a session alive and mid-turn had its registration
    /// swept by a peer's scan because its socket did not answer inside
    /// the probe's timeout, and a `send` to it then failed with "no such
    /// session" for a session that was running when the call began. The
    /// pid is the second opinion that must survive the scan.
    #[test]
    fn a_live_process_with_a_silent_socket_is_never_swept() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("run");
        let cwd = tmp.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        let a = register(&root, &cwd, "a");

        // The reported state, exactly: meta on disk, no socket, older
        // than the grace period, pid alive (this test's own process).
        let busy = root.join("busy-0000");
        std::fs::create_dir_all(busy.join("inbox")).unwrap();
        let mut m = a.meta();
        m.id = "busy-0000".into();
        m.pid = std::process::id();
        m.started_ms = crate::now_ms() - GRACE_MS - 1;
        std::fs::write(busy.join("meta.json"), serde_json::to_vec(&m).unwrap()).unwrap();

        let peers = a.peers();
        assert_eq!(
            peers.len(),
            1,
            "a live pid is a busy session, not a dead one"
        );
        assert_eq!(peers[0].meta.id, "busy-0000");
        assert!(busy.exists(), "and its registration is not swept");
        assert!(
            matches!(
                peers[0].deliver(&a.envelope("still there?")),
                Ok(Delivery::Queued)
            ),
            "mail goes to its real inbox and is reported queued while the door is silent"
        );
    }

    /// The trade the pid check makes, pinned rather than implied:
    /// `kill(pid, 0)` cannot tell an eidolon session from any other
    /// process of this user, so a recycled pid can hold a dead session's
    /// registration in the roster — a ghost that dies when the stranger
    /// does. Accepted: the other direction deletes live registrations,
    /// which is the bug this file just fixed.
    #[test]
    fn a_pid_answers_for_any_live_process_until_it_is_reaped() {
        let mut stranger = std::process::Command::new("sleep").arg("30").spawn().unwrap();
        assert!(
            pid_live(stranger.id()),
            "a live process of this user reads as alive, eidolon or not"
        );
        stranger.kill().unwrap();
        stranger.wait().unwrap();
        assert!(!pid_live(stranger.id()), "and a reaped pid reads as gone");
    }

    /// `pid: 0` is not a process. Unguarded, `kill(0, 0)` answers for the
    /// caller's whole process group, and a pid too wide for `pid_t` would
    /// arrive negative and answer for one of those — so a forged or
    /// broken meta would read as alive forever, the ghost class this
    /// file bounds. Debris stays debris: it is swept.
    #[test]
    fn a_registration_carrying_pid_zero_is_swept() {
        assert!(
            !pid_live(0),
            "pid 0 is a process-group target, not a process"
        );
        assert!(
            !pid_live(u32::MAX),
            "a pid too wide for pid_t would arrive negative — also a group target"
        );

        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("run");
        let cwd = tmp.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        let a = register(&root, &cwd, "a");

        let forged = root.join("forged-0000");
        std::fs::create_dir_all(forged.join("inbox")).unwrap();
        let mut m = a.meta();
        m.id = "forged-0000".into();
        m.pid = 0;
        m.started_ms = crate::now_ms() - GRACE_MS - 1;
        std::fs::write(forged.join("meta.json"), serde_json::to_vec(&m).unwrap()).unwrap();

        assert!(
            a.peers().is_empty(),
            "a meta carrying pid 0 is debris, not a live session"
        );
        assert!(!forged.exists(), "and it is swept as found");
    }

    #[test]
    fn the_inbox_drains_through_the_core_seam() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("run");
        let cwd = tmp.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        let a = register(&root, &cwd, "a");

        inbox::deliver(
            &a.inbox_dir(),
            &a.envelope("edit.rs is yours, I am out of it").waking(true),
        )
        .unwrap();
        let got = a.drain();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text, "edit.rs is yours, I am out of it");
        assert!(got[0].wake);
        assert!(a.drain().is_empty());
    }

    #[test]
    fn a_worktree_and_its_checkout_share_a_channel_key() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let git = repo.join(".git");
        std::fs::create_dir_all(git.join("worktrees/wt")).unwrap();
        let wt = tmp.path().join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        // What `git worktree add` leaves behind: a `.git` *file* naming
        // the per-worktree directory, which names the common one.
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", git.join("worktrees/wt").display()),
        )
        .unwrap();
        std::fs::write(git.join("worktrees/wt/commondir"), "../..\n").unwrap();

        let root = tmp.path().join("run");
        let main = register(&root, &repo, "main");
        let side = register(&root, &wt, "side");
        assert_eq!(
            main.channel_key(),
            side.channel_key(),
            "one worktree per agent must not split the channel"
        );
    }

    #[test]
    fn outside_a_repository_the_channel_falls_back_to_the_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("run");
        let cwd = tmp.path().join("loose");
        std::fs::create_dir_all(&cwd).unwrap();
        let a = register(&root, &cwd, "a");
        assert_eq!(a.meta().repo, None);
        assert_eq!(a.channel_key(), cwd);
    }
}
