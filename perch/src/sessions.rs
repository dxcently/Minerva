//! The sessions this perch owns, and the list of the sessions it can see.
//!
//! hub.md §4 is the spec. A **door** is one `eidolon web` process = one session; the perch
//! knows a door only as a binary, two stdout lines, a `0600` token file and HTTP, so what
//! it holds per door is a pid, a port, the door's token in memory, and the [`Watcher`]
//! stream that proves the door reached `live`.
//!
//! **The list has three sources** (§4): the kept logs (`<sessions dir>/<id>.eid`, by stem),
//! the doors this perch owns, and the swarm roster
//! (`$XDG_RUNTIME_DIR/eidolon/<name>/meta.json`) — the last only as `live-elsewhere`,
//! because adopting a door the perch did not start would mean owning a lifetime it does not
//! (Q5). The roster is read by scanning; the perch never registers, and the
//! running/parked/busy inference out of it is H2's.
//!
//! **The state machine** (§4), and the only transitions this slice makes:
//!
//! ```text
//!   kept --resume--> starting --hello--> live --stop--> stopping --exit--> dead --> kept (the log stays)
//!                        |
//!                        +-- boot timeout / spawn failure --> dead
//! ```
//!
//! **Stop is the reason H1b is its own slice** (§4 "Stop"). A real door runs hyper's
//! `GracefulShutdown`: SIGTERM does *not* end it while an SSE connection is open — it waits
//! that connection out, which is minutes. So [`Registry::stop`] closes the perch's own
//! stream to the door **first**, then signals. Both `/hub/sessions/<id>/stop` and the
//! perch's own shutdown go through that one function; neither reimplements the order.
//!
//! Not here, on purpose: panes and the proxy (H1c), the idle reaper, `running`/`parked`
//! inference, `/hub/events`, `/hub/tree`, `/hub/mesh` (H2/H3).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::spawn;
use crate::watch::Watcher;

/// §4's id regex, spelled out rather than pulled in as a dependency: the id is a log's stem
/// inside the sessions dir, and this is the shape that keeps it one. Never a path.
pub fn id_ok(id: &str) -> bool {
    let mut chars = id.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    id.len() <= 128 && chars.all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

/// The one place an id becomes a path (§4): `<sessions dir>/<id>.eid`, and nothing else.
/// Whether the id is well-formed is [`id_ok`]'s business and the route's 404.
pub fn log_path(sessions_dir: &Path, id: &str) -> PathBuf {
    sessions_dir.join(format!("{id}.eid"))
}

/// The id a door's `hello` names, when that log is inside our sessions dir — `None`
/// otherwise, which is what stops a door that printed some other path from naming a session
/// (the id would then resolve to a file that is not there).
pub fn id_from_log(sessions_dir: &Path, log: &Path) -> Option<String> {
    let log = canonical(log);
    let parent = log.parent()?;
    if !same_dir(parent, sessions_dir) {
        return None;
    }
    if log.extension().and_then(|e| e.to_str()) != Some("eid") {
        return None;
    }
    let stem = log.file_stem()?.to_str()?;
    id_ok(stem).then(|| stem.to_string())
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Two paths that mean the same path. `canonicalize` needs the path to exist, which a
/// sessions dir the door has not written into yet does not.
fn same_dir(a: &Path, b: &Path) -> bool {
    canonical(a) == canonical(b)
}

/// §4's states, spelled as the wire does (`live-elsewhere`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum State {
    Kept,
    Starting,
    Live,
    Stopping,
    Dead,
    LiveElsewhere,
}

/// One row of `GET /hub/sessions`. `panes` is always 0 in H1b: there is no proxy, so no
/// pane can be open (H1c counts them). `busy` is deliberately absent — it needs the
/// roster/running inference, which is H2.
#[derive(Serialize)]
pub struct Entry {
    pub id: String,
    pub state: State,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub panes: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mtime: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// The channel a door's reaper publishes its exit on: `None` while the child is alive. One
/// alias, because this type travels through the stop path, the spawn path and the row.
pub type ExitWatch = watch::Receiver<Option<Exit>>;

/// How a door ended. Kept for the `dead` row and, in H1c, for `hub-stream-end`'s `exit`
/// field; the perch learns it from the child it reaped and from nothing else.
#[derive(Clone, Copy, Debug, Default)]
pub struct Exit {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

impl Exit {
    pub fn of(status: std::io::Result<std::process::ExitStatus>) -> Self {
        use std::os::unix::process::ExitStatusExt as _;
        match status {
            Ok(status) => Self { code: status.code(), signal: status.signal() },
            // The reap itself failed: the door is still not there, which is all `dead` says.
            Err(_) => Self::default(),
        }
    }
}

/// One door this perch owns.
///
/// The token never leaves this struct: it is not serialized, not logged, and reaches
/// nothing but the watcher's own `Authorization` header and the comparison that decides
/// whether a leftover token file is ours to remove.
pub struct Door {
    pub id: String,
    /// The session log, once the door (or the resume) named it.
    pub log: Option<PathBuf>,
    pub cwd: Option<PathBuf>,
    pub state: State,
    pub pid: u32,
    pub port: u16,
    token: Arc<str>,
    watcher: Option<Watcher>,
    exit: ExitWatch,
}

/// The registry: what the perch owns, plus the paths and roots every spawn is judged
/// against. Shared as `Arc<Registry>`, and the door table is a `Vec` because a door is
/// found by its **pid** on the exit path and by its **id** on the route path; there are a
/// handful of doors per perch, so the linear scans are cheaper than a second index.
pub struct Registry {
    /// Kept logs live here, as `<id>.eid`.
    pub sessions_dir: PathBuf,
    /// `$XDG_RUNTIME_DIR`, where a door's token file must be and the roster is scanned.
    pub runtime_dir: PathBuf,
    /// The door binary to spawn (`--eidolon`).
    pub eidolon: PathBuf,
    /// `--root`, canonicalized once at start: a new session's cwd must sit under one.
    roots: Vec<PathBuf>,
    doors: Mutex<Vec<Door>>,
}

/// §2's shutdown row: "wait up to 10 s each".
const DRAIN: Duration = Duration::from_secs(10);
/// A SIGKILLed child is reaped at once; this is only how long the perch looks.
const SETTLE: Duration = Duration::from_secs(2);

/// What a stop did, for the route's 202/409.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    Signalled,
    NotLive,
}

/// Whether a stop had to SIGKILL, which is the difference between the clean path and the
/// path where the perch has to remove a token file the door could not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Killed(pub bool);

impl Registry {
    /// The roots are canonicalized here, once: every later comparison is between two
    /// canonical paths, and a `--root` that does not exist refuses the start rather than
    /// failing every spawn with a 422 nobody can act on.
    pub fn new(sessions_dir: PathBuf, runtime_dir: PathBuf, eidolon: PathBuf, roots: Vec<PathBuf>) -> Result<Arc<Self>> {
        let mut checked = Vec::new();
        for root in roots {
            let path = root.canonicalize().with_context(|| format!("--root {} is not an existing directory", root.display()))?;
            anyhow::ensure!(path.is_dir(), "--root {} is not a directory", path.display());
            checked.push(path);
        }
        anyhow::ensure!(!checked.is_empty(), "no --root and no $HOME: a new session's cwd could not be checked against anything");
        Ok(Arc::new(Self { sessions_dir, runtime_dir, eidolon, roots: checked, doors: Mutex::new(Vec::new()) }))
    }

    /// §4 step 1, for a new session: the cwd as the kernel spells it, and only if some
    /// `--root` contains it. A path that cannot be canonicalized is not under a root, and
    /// gets the same 422 as one that is under none.
    pub fn cwd_under_a_root(&self, cwd: &Path) -> Result<PathBuf> {
        let path = cwd.canonicalize().with_context(|| format!("cwd {} is not an existing directory", cwd.display()))?;
        anyhow::ensure!(path.is_dir(), "cwd {} is not a directory", path.display());
        anyhow::ensure!(
            self.roots.iter().any(|root| path.starts_with(root)),
            "cwd {} is outside every --root",
            path.display()
        );
        Ok(path)
    }

    /// The doors this perch owns, for the list.
    fn ours(&self) -> Vec<(String, State, Option<PathBuf>, Option<PathBuf>, u32)> {
        self.doors
            .lock()
            .unwrap()
            .iter()
            .map(|door| (door.id.clone(), door.state, door.log.clone(), door.cwd.clone(), door.pid))
            .collect()
    }

    /// `GET /hub/sessions`: kept + ours + `live-elsewhere`, one row per id — a door of ours
    /// wins over a roster entry, which wins over the kept log.
    pub fn list(&self) -> Vec<Entry> {
        let mut by_id: BTreeMap<String, Entry> = BTreeMap::new();

        // Kept sessions: every `*.eid` in the sessions dir, by stem and mtime (§4 source 1).
        if let Ok(entries) = std::fs::read_dir(&self.sessions_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("eid") {
                    continue;
                }
                let Some(id) = path.file_stem().and_then(|s| s.to_str()).filter(|id| id_ok(id)) else { continue };
                let row = Entry { id: id.to_string(), state: State::Kept, cwd: None, panes: 0, mtime: mtime_secs(&path), title: None };
                by_id.insert(id.to_string(), row);
            }
        }

        // Our own doors (source 2), which override the kept row while we hold them.
        let ours = self.ours();
        for (id, state, log, cwd, _pid) in &ours {
            let row = Entry {
                id: id.clone(),
                state: *state,
                cwd: cwd.as_ref().map(|p| p.display().to_string()),
                panes: 0,
                mtime: log.as_deref().and_then(mtime_secs),
                title: None,
            };
            by_id.insert(id.clone(), row);
        }

        // The roster (source 3): a kept log another consumer holds, whose pid is not one of
        // our children. Read-only, never adopted (Q5). H2 is what infers running/parked.
        let our_pids: Vec<u32> = ours.iter().map(|(_, _, _, _, pid)| *pid).collect();
        for (id, peer) in self.roster() {
            if ours.iter().any(|(ours, ..)| *ours == id) || !by_id.contains_key(&id) || our_pids.contains(&peer.pid) {
                continue;
            }
            let row = Entry {
                id: id.clone(),
                state: State::LiveElsewhere,
                cwd: peer.cwd.as_ref().map(|p| p.display().to_string()),
                panes: 0,
                mtime: mtime_secs(&log_path(&self.sessions_dir, &id)),
                title: peer.title.filter(|t| !t.trim().is_empty()),
            };
            by_id.insert(id, row);
        }

        by_id.into_values().collect()
    }

    /// The swarm roster, as `(id, row)` for the rows whose log is a kept log here — kept
    /// being the point: a roster entry for a log this perch cannot list is a session it can
    /// say nothing about. `$XDG_RUNTIME_DIR/eidolon/<name>/meta.json` (presence.rs:33-53).
    fn roster(&self) -> Vec<(String, Roster)> {
        let mut found = Vec::new();
        let Ok(entries) = std::fs::read_dir(self.runtime_dir.join("eidolon")) else { return found };
        for entry in entries.flatten() {
            let meta = entry.path().join("meta.json");
            let Ok(bytes) = std::fs::read(&meta) else { continue };
            let Ok(row) = serde_json::from_slice::<Roster>(&bytes) else {
                tracing::debug!(path = %meta.display(), "perch: a roster row did not parse");
                continue;
            };
            let Some(id) = id_from_log(&self.sessions_dir, &row.log) else { continue };
            if !log_path(&self.sessions_dir, &id).is_file() {
                continue;
            }
            found.push((id, row));
        }
        found
    }

    /// §4 step 2's other half: the log is held by *someone else's* process, per the roster.
    /// Only consulted when the swarm is on; a consumer with the swarm off leaves no roster
    /// row, which is the gap hub.md W4 wants upstream to close.
    pub fn roster_holds(&self, log: &Path) -> bool {
        let log = canonical(log);
        let ours: Vec<u32> = self.ours().iter().map(|(_, _, _, _, pid)| *pid).collect();
        self.roster().iter().any(|(_, row)| canonical(&row.log) == log && !ours.contains(&row.pid))
    }

    /// §4 step 2, for our own doors: whether a log is held by this perch. `kept` and `dead`
    /// are not held; `starting`, `live` and `stopping` are.
    pub fn held(&self, log: &Path) -> bool {
        let log = canonical(log);
        self.doors.lock().unwrap().iter().any(|door| {
            door.log.as_deref().is_some_and(|held| canonical(held) == log) && matches!(door.state, State::Starting | State::Live | State::Stopping)
        })
    }

    /// Reserve a log before the fork, so two concurrent resumes cannot both pass the check
    /// in §4 step 2 and then both write the one log. `false` means it is already held.
    pub fn reserve(&self, id: &str, log: &Path) -> bool {
        let mut doors = self.doors.lock().unwrap();
        if doors.iter().any(|door| door.id == id && matches!(door.state, State::Starting | State::Live | State::Stopping)) {
            return false;
        }
        doors.retain(|door| door.id != id);
        doors.push(Door {
            id: id.to_string(),
            log: Some(log.to_path_buf()),
            cwd: None,
            state: State::Starting,
            pid: 0,
            port: 0,
            token: Arc::from(""),
            watcher: None,
            exit: watch::channel(None).1,
        });
        true
    }

    /// The fork happened. An entry can be found by pid from here on, and a child that died
    /// before this call is marked `dead` on the spot rather than left `starting` for good —
    /// the reap task publishes the exit through the same channel, so it is readable here.
    pub fn attach(&self, id: &str, pid: u32, exit: ExitWatch) {
        let already = *exit.borrow();
        let mut doors = self.doors.lock().unwrap();
        // One live-ish entry per pid, so the exit path can find it by pid alone.
        doors.retain(|door| door.pid != pid || door.state == State::Dead);
        if let Some(door) = doors.iter_mut().find(|door| door.id == id) {
            door.pid = pid;
            door.exit = exit;
            if already.is_some() {
                door.state = State::Dead;
                door.watcher = None;
            }
        }
    }

    /// The token file is read: the door has a port and the perch holds its token.
    pub fn ready(&self, id: &str, port: u16, token: Arc<str>) {
        if let Some(door) = self.doors.lock().unwrap().iter_mut().find(|door| door.id == id) {
            door.port = port;
            door.token = token;
        }
    }

    /// `hello` was read: the door is `live`, and `hello` is where its cwd (and, for a new
    /// session, its log — hence its id, §4 step 7) is published.
    pub fn mark_live(&self, id: &str, log: &Path, cwd: &Path) {
        if let Some(door) = self.doors.lock().unwrap().iter_mut().find(|door| door.id == id) {
            door.log = Some(log.to_path_buf());
            door.cwd = Some(cwd.to_path_buf());
            door.state = State::Live;
        }
    }

    /// A new session's door, whole: it reached `live` inside the spawn request, so there is
    /// no window in which anyone else could have known an id to reserve.
    pub fn insert(&self, id: String, log: PathBuf, cwd: PathBuf, pid: u32, port: u16, token: Arc<str>, watcher: Watcher, exit: ExitWatch) {
        let mut doors = self.doors.lock().unwrap();
        doors.retain(|door| door.id != id && (door.pid != pid || door.state == State::Dead));
        doors.push(Door { id, log: Some(log), cwd: Some(cwd), state: State::Live, pid, port, token, watcher: Some(watcher), exit });
    }

    /// A spawn that never reached `live`: the row stays, as `dead`, because "this door did
    /// not come up" is something the page has to be able to see (and a kept log under the
    /// same id is then honestly reported as dead, not as resumable-now).
    pub fn dead(&self, id: &str) {
        if let Some(door) = self.doors.lock().unwrap().iter_mut().find(|door| door.id == id) {
            door.state = State::Dead;
            door.watcher = None;
        }
    }

    /// Every id we are holding, for the shutdown path. Only live-ish doors have a stream to
    /// close and a child to signal.
    pub fn live_ids(&self) -> Vec<String> {
        self.doors
            .lock()
            .unwrap()
            .iter()
            .filter(|door| matches!(door.state, State::Starting | State::Live | State::Stopping))
            .map(|door| door.id.clone())
            .collect()
    }

    /// A child exited: the row becomes `dead`, its stream (to a process that is gone) is
    /// dropped, and a token file the door did not get to remove is removed here — a
    /// SIGKILLed door never runs its own `remove_file`.
    pub fn mark_exited(&self, pid: u32) {
        let cleanup = {
            let mut doors = self.doors.lock().unwrap();
            // The entry whose child this was: never a `dead` one, whose pid the kernel is
            // free to hand to a later door.
            let Some(door) = doors.iter_mut().find(|door| door.pid == pid && door.state != State::Dead) else { return };
            door.state = State::Dead;
            door.watcher = None;
            (spawn::door_token_path(&self.runtime_dir, door.port), door.token.clone())
        };
        drop_token_file(&cleanup.0, &cleanup.1);
    }

    /// §4 "Stop", and the one place the order lives. See the module doc: close, then
    /// signal, then wait, then SIGKILL.
    pub async fn stop(&self, id: &str) -> Stop {
        let taken = {
            let mut doors = self.doors.lock().unwrap();
            let Some(door) = doors.iter_mut().find(|door| door.id == id) else { return Stop::NotLive };
            if !matches!(door.state, State::Starting | State::Live | State::Stopping) || door.pid == 0 {
                // `pid == 0` is a reservation whose fork has not happened yet — the spawn
                // request that made it owns that child, so there is nothing here to signal.
                return Stop::NotLive;
            }
            door.state = State::Stopping;
            (door.pid, door.port, door.token.clone(), door.watcher.take(), door.exit.clone())
        };
        let (pid, port, token, watcher, exit) = taken;

        // 1. The perch's own stream to this door ends first. A door with an open
        //    `/api/events` stream is running hyper's `GracefulShutdown`, and SIGTERM alone
        //    would wait that stream out — minutes, and then a SIGKILL.
        if let Some(watcher) = watcher {
            watcher.close().await;
        }

        // 2. SIGTERM, so the door drains and removes its own token file. 3. Wait up to
        //    `DRAIN`, then SIGKILL what is left; its token file is the perch's to remove.
        let killed = signal_and_wait(pid, exit, DRAIN).await;
        if killed.0 {
            tracing::debug!(pid, port, "perch: a door ignored SIGTERM and was killed; its token file is the perch's to remove");
        }
        drop_token_file(&spawn::door_token_path(&self.runtime_dir, port), &token);
        // The reap task usually got there first; this makes the row `dead` either way.
        self.dead(id);
        Stop::Signalled
    }

    /// Every door, concurrently: the perch's own shutdown (hub.md §2 "Shutdown").
    /// Sequential 10 s waits would make a quit as slow as the number of doors.
    pub async fn stop_all(self: &Arc<Self>) {
        let mut tasks = Vec::new();
        for id in self.live_ids() {
            let registry = self.clone();
            tasks.push(tokio::spawn(async move {
                registry.stop(&id).await;
            }));
        }
        for task in tasks {
            let _ = task.await;
        }
    }
}

/// One roster row, as much of it as a read-only list needs. `Meta` has more (model, repo,
/// started_ms, busy); H2 is where `busy` earns its place, and the perch must not depend on
/// an `eidolon-*` crate to name the fields it does read.
#[derive(Deserialize)]
struct Roster {
    log: PathBuf,
    pid: u32,
    #[serde(default)]
    cwd: Option<PathBuf>,
    #[serde(default)]
    title: Option<String>,
}

/// SIGTERM, then SIGKILL if the child is still there after `grace`. Shared by the stop path
/// and by a spawn that has to give up on a door that never came up.
pub async fn signal_and_wait(pid: u32, mut exit: ExitWatch, grace: Duration) -> Killed {
    signal(pid, libc::SIGTERM);
    if tokio::time::timeout(grace, wait_exit(&mut exit)).await.is_ok() {
        return Killed(false);
    }
    tracing::warn!(pid, "perch: a door did not exit after SIGTERM; killing it");
    signal(pid, libc::SIGKILL);
    let _ = tokio::time::timeout(SETTLE, wait_exit(&mut exit)).await;
    Killed(true)
}

/// Best effort by design: a pid that is already gone gives `ESRCH`, which is the answer
/// this wanted anyway.
fn signal(pid: u32, sig: libc::c_int) {
    let sent = unsafe { libc::kill(pid as libc::pid_t, sig) };
    if sent != 0 {
        tracing::debug!(pid, sig, error = %std::io::Error::last_os_error(), "perch: a signal to a door did not land");
    }
}

/// The child's exit, as the reap task publishes it. `changed()` also fails when the sender
/// is gone, which means the reaping itself ended — the door is not there either way.
async fn wait_exit(exit: &mut ExitWatch) -> Exit {
    loop {
        if let Some(seen) = *exit.borrow_and_update() {
            return seen;
        }
        if exit.changed().await.is_err() {
            return Exit::default();
        }
    }
}

/// The perch removes a door's token file when the door could not: a SIGKILLed door never
/// runs its own `remove_file` (hub.md §2 "Shutdown", §4 "Stop").
///
/// Only the file that still holds *this* door's token is removed, so a port that has been
/// reused by another door's file — a different token — is left alone. The token is only
/// ever compared; nothing here is logged but the path and the reason.
fn drop_token_file(path: &Path, token: &str) {
    match spawn::read_token_file(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Ok(found) if found == token => {
            if let Err(e) = std::fs::remove_file(path) {
                tracing::debug!(error = %e, path = %path.display(), "perch: a killed door's token file was not removed");
            }
        }
        Ok(_) => tracing::debug!(path = %path.display(), "perch: a token file is not ours; left in place"),
        Err(e) => tracing::debug!(error = %e, path = %path.display(), "perch: a token file did not read; left in place"),
    }
}

fn mtime_secs(path: &Path) -> Option<u64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    modified.duration_since(SystemTime::UNIX_EPOCH).ok().map(|since| since.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sessions dir with logs and something that is not a log, and nothing else: enough
    /// for the list without a process in sight.
    fn sessions_dir(dir: &Path) {
        std::fs::write(dir.join("ctf-pwn-3.eid"), "{}\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "not a log\n").unwrap();
    }

    fn registry(dir: &Path, runtime: &Path) -> Arc<Registry> {
        Registry::new(dir.to_path_buf(), runtime.to_path_buf(), PathBuf::from("eidolon"), vec![dir.to_path_buf()]).unwrap()
    }

    /// The resume path up to `live`, which is what puts a door of ours in the table.
    fn our_door(registry: &Arc<Registry>, id: &str, log: &Path) {
        assert!(registry.reserve(id, log));
        registry.attach(id, 4242, watch::channel(None).1);
        registry.ready(id, 4477, Arc::from("a-door-token"));
        registry.mark_live(id, log, Path::new("/tmp"));
    }

    #[test]
    fn an_id_is_a_stem_and_never_a_path() {
        for ok in ["a", "ctf-pwn-3", "2026-09-27", "A.b_c-1", &"x".repeat(128)] {
            assert!(id_ok(ok), "{ok:?} is an id");
        }
        for no in ["", ".", "..", "-lead", ".hidden", "a/b", "a\\b", "a b", "é", &"x".repeat(129), "../escape", "a\0"] {
            assert!(!id_ok(no), "{no:?} is not an id");
        }
        // The id is resolved as `<dir>/<id>.eid` and never joined as a path, so even a
        // stem that would be a path elsewhere lands inside the sessions dir.
        assert_eq!(log_path(Path::new("/logs"), "ctf-pwn-3"), Path::new("/logs/ctf-pwn-3.eid"));
    }

    #[test]
    fn only_a_log_inside_the_sessions_dir_names_a_session() {
        let dir = tempfile::tempdir().unwrap();
        sessions_dir(dir.path());
        assert_eq!(id_from_log(dir.path(), &dir.path().join("ctf-pwn-3.eid")).as_deref(), Some("ctf-pwn-3"));
        // Elsewhere, another extension, and a stem that is not an id.
        let elsewhere = tempfile::tempdir().unwrap();
        let outside = elsewhere.path().join("ctf-pwn-3.eid");
        std::fs::write(&outside, "{}\n").unwrap();
        assert_eq!(id_from_log(dir.path(), &outside), None);
        assert_eq!(id_from_log(dir.path(), &dir.path().join("notes.txt")), None);
        std::fs::write(dir.path().join("-bad.eid"), "{}\n").unwrap();
        assert_eq!(id_from_log(dir.path(), &dir.path().join("-bad.eid")), None);
    }

    #[test]
    fn the_list_merges_kept_ours_and_the_roster() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        sessions_dir(dir.path());
        // A roster row for a kept log, from a process that is not ours.
        let peer = runtime.path().join("eidolon").join("eidolon-9f2c");
        std::fs::create_dir_all(&peer).unwrap();
        let meta = format!(r#"{{"id":"eidolon-9f2c","pid":999999,"log":"{}","cwd":"/tmp","title":"a peer","busy":true}}"#, dir.path().join("ctf-pwn-3.eid").display());
        std::fs::write(peer.join("meta.json"), meta).unwrap();
        // …and one for a log that is not here, which is not listed at all.
        let stray = runtime.path().join("eidolon").join("eidolon-stray");
        std::fs::create_dir_all(&stray).unwrap();
        std::fs::write(stray.join("meta.json"), r#"{"id":"eidolon-stray","pid":999998,"log":"/nowhere/x.eid"}"#).unwrap();

        let registry = registry(dir.path(), runtime.path());
        let list = registry.list();
        assert_eq!(list.len(), 1, "one kept log: {:?}", list.iter().map(|e| &e.id).collect::<Vec<_>>());
        let row = &list[0];
        assert_eq!(row.id, "ctf-pwn-3");
        assert_eq!(row.state, State::LiveElsewhere, "the roster holds a kept log, and we do not");
        assert_eq!(row.title.as_deref(), Some("a peer"));
        assert_eq!(row.panes, 0, "no proxy yet, so no pane can be open");
        assert!(row.mtime.is_some(), "a kept log has an mtime");
        assert!(registry.roster_holds(&dir.path().join("ctf-pwn-3.eid")), "the roster holds this log");

        // Our own door wins over the roster row, on a log of its own.
        let fresh = dir.path().join("fresh.eid");
        std::fs::write(&fresh, "{}\n").unwrap();
        our_door(&registry, "fresh", &fresh);
        let list = registry.list();
        let fresh_row = list.iter().find(|e| e.id == "fresh").expect("our own door is listed");
        assert_eq!(fresh_row.state, State::Live);
        assert_eq!(fresh_row.mtime, mtime_secs(&fresh), "our row carries the log's mtime");
        // …and the pid we own is not a peer, even when the roster names the same log.
        let meta = format!(r#"{{"id":"ours","pid":4242,"log":"{}"}}"#, fresh.display());
        std::fs::write(peer.join("meta.json"), meta).unwrap();
        assert!(!registry.roster_holds(&fresh), "our own pid is not someone else holding the log");
    }

    #[test]
    fn a_dead_row_is_not_a_session_we_hold() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        sessions_dir(dir.path());
        let registry = registry(dir.path(), runtime.path());
        let log = dir.path().join("ctf-pwn-3.eid");
        assert!(registry.reserve("ctf-pwn-3", &log), "the first resume holds the log");
        assert_eq!(registry.list()[0].state, State::Starting, "a reserved log is listed as starting");
        assert!(registry.held(&log));
        assert!(!registry.reserve("ctf-pwn-3", &log), "§4 step 2: two writers on one log is refused");
        registry.dead("ctf-pwn-3");
        assert!(!registry.held(&log), "a dead door does not hold the log");
        // The row is still there, and the list says so: "this door did not come up" is
        // something the page has to be able to see.
        assert_eq!(registry.list()[0].state, State::Dead);
        assert!(registry.reserve("ctf-pwn-3", &log), "and a resume after it is allowed");
        assert!(registry.held(&log), "the reservation holds it again");
        // With no row at all the same id is the kept log it always was.
        registry.doors.lock().unwrap().clear();
        assert_eq!(registry.list()[0].state, State::Kept);
    }

    /// The exit path finds a door by pid, so a stale `dead` row must never answer for a pid
    /// the kernel has since handed to a later door.
    #[test]
    fn a_dead_row_does_not_swallow_a_later_pid() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        sessions_dir(dir.path());
        let registry = registry(dir.path(), runtime.path());
        let first = dir.path().join("first.eid");
        std::fs::write(&first, "{}\n").unwrap();
        our_door(&registry, "first", &first);
        registry.mark_exited(4242);
        assert_eq!(registry.list().iter().find(|e| e.id == "first").unwrap().state, State::Dead);

        // A later door really can get the same pid; the row for it must be the one marked.
        let second = dir.path().join("second.eid");
        std::fs::write(&second, "{}\n").unwrap();
        our_door(&registry, "second", &second);
        assert_eq!(registry.list().iter().find(|e| e.id == "second").unwrap().state, State::Live);
        registry.mark_exited(4242);
        let list = registry.list();
        assert_eq!(list.iter().find(|e| e.id == "second").unwrap().state, State::Dead, "the live row was the one reaped");
        assert_eq!(list.iter().find(|e| e.id == "first").unwrap().state, State::Dead);
    }

    #[test]
    fn only_a_cwd_under_a_root_can_start_a_session() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = tempfile::tempdir().unwrap();
        sessions_dir(dir.path());
        let registry = registry(dir.path(), runtime.path());
        let inside = dir.path().join("work");
        std::fs::create_dir(&inside).unwrap();
        assert_eq!(registry.cwd_under_a_root(&inside).unwrap(), inside.canonicalize().unwrap());
        // Outside every root, not there at all, and a file: all the route's 422.
        let outside = tempfile::tempdir().unwrap();
        assert!(registry.cwd_under_a_root(outside.path()).unwrap_err().to_string().contains("outside every --root"));
        assert!(registry.cwd_under_a_root(&dir.path().join("nowhere")).is_err());
        assert!(registry.cwd_under_a_root(&dir.path().join("ctf-pwn-3.eid")).is_err());
    }
}
