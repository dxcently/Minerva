//! A chat's folder follows its name, and can become a project: `POST /hub/sessions/<id>/{rename,promote}` and the tidy loop.
//!
//! ```text
//!   [ new chat ] -> <home>/scratch/<three-word-name>/            quiet-owl-lantern, never digits
//!   first turn settles, the chat has a title -> BETWEEN TURNS: the folder is renamed to slug(title)
//!                    (scratch/juice-shop-recon/), and the chat reopens there; nothing is lost
//!   rename       -> a custom name beats the derived title everywhere; an inbox chat's folder follows it
//!   promote      -> {parent, name}: the folder is MOVED to <parent>/<name>, registered as a project, the chat reopens in it
//! ```
//!
//! **How a chat survives its folder moving.** A door's tools run where eidolon says: `web --session <log> --cwd <dir>` is the
//! reopen-time override (`Session::open_with_cwd`; the start record keeps the folder the chat was born in, and `hello.cwd` reports
//! the override). So a move is: stop the door cleanly (between turns only), change the folder, remember it in `chat-cwds.json`,
//! bring the door back on the same log with `--cwd <new>` ([`crate::restart::bring_up`], which passes the remembered folder on
//! every reopen) and the model and `--yolo` it had. A busy chat is not touched: the tidy loop looks again each tick, and a
//! promote of a busy chat is a 409.
//!
//! **What is never moved.** Only a real directory directly under `<home>/scratch` is renamed or moved; a folder inside a project
//! (or a project itself), anything outside scratch, and a shared-dir chat are left alone. A folder several chats
//! share (a fork shares its source's) moves for ALL of them on a hand rename or promote (every one judged first, a busy one is a 409,
//! idle live doors are stopped and brought back with the new `--cwd`, saved ones just get `chat-cwds.json` updated); the tidy loop
//! still leaves shared folders alone, so two titles do not fight over one name. A rename that fails keeps the old folder and is logged once; the chat keeps working.
//!
//! **Crossing filesystems.** `rename` is the move when both sides are one filesystem. Moving from WSL's `/home` to `/mnt/c` is not
//! (`EXDEV`): then the tree is scanned first (a link or anything that is not a file or a directory refuses the move, and nothing has
//! been written), copied, checked against the scan (same entries, same sizes), and only then is the source removed.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result};
use bytes::Bytes;
use hyper::{Response, StatusCode};
use tokio_util::sync::CancellationToken;

use crate::names::{self, slug};
use crate::serve::{BoxBody, Ctx, json, problem};
use crate::sessions::{self, Restart};

/// How often the tidy loop looks at the live inbox chats.
const TICK: Duration = Duration::from_millis(500);
/// A chat must be seen between turns this many ticks in a row before its folder is touched, so the moment between a message
/// reaching the log and the door announcing its turn is never mistaken for "settled".
const IDLE_TICKS: u32 = 2;
/// How long the tidy loop leaves a chat alone after finding its log ended mid-turn.
const UNSETTLED_WAIT: Duration = Duration::from_secs(10);

/// Why a folder was not moved.
#[derive(Debug)]
pub(crate) enum Refused {
    /// A turn is running: a stop now would cut it off. Names the chat when it is another one than the one asked about.
    Busy(Option<String>),
    /// The door is starting or stopping, or another move of this chat is in flight.
    Between,
    /// Another process holds the log.
    Held,
    /// A door's log ends mid-turn (a message unanswered, a tool call with no result): reopening it would carry the turn on, which is
    /// a paid turn nobody asked for. Names the chat when it is another one than the one asked about.
    Unsettled(Option<String>),
    /// The chat or its folder is not there.
    Gone,
    /// The change itself failed; the chat is back where it was. The words are for the person.
    Failed(String),
}

/// A folder that was moved, and what became of the door.
pub(crate) struct Moved {
    pub dir: PathBuf,
    /// The chat had a door, and it is back in the new folder.
    pub reopened: bool,
    /// The chat had a door, and it did not come back; the words say why.
    pub lost: Option<String>,
}

/// One chat that goes along with a folder move: its log, and the model and `--yolo` of its door when it has one.
struct Along {
    id: String,
    log: PathBuf,
    door: Option<(String, bool)>,
}

/// The name a chat wears in a sentence for the person.
fn called(ctx: &Ctx, id: &str) -> String {
    ctx.registry.list().into_iter().find(|row| row.id == id).and_then(|row| row.title).unwrap_or_else(|| "another chat".to_string())
}

/// Runs `op` on the chat's folder with the doors out of the way: stop them (between turns), change the folder, remember it, bring the
/// doors back with `--cwd`. `op` is given the folder now and answers the folder after, canonical; it runs off the reactor.
///
/// **A folder is moved for every chat that works in it** (a fork shares its source's): a chat with a door is stopped and brought back with
/// its own model and `--yolo`; a saved one has only its remembered folder changed. If any of them is mid-turn nothing is touched, and the
/// answer names it ([`Refused::Busy`]).
pub(crate) async fn relocate<F>(ctx: &Ctx, id: &str, op: F) -> std::result::Result<Moved, Refused>
where
    F: FnOnce(&Path) -> Result<PathBuf> + Send + 'static,
{
    let Some((log, _)) = ctx.registry.find_log(id) else { return Err(Refused::Gone) };
    let Some(old) = ctx.registry.known_cwd(id) else { return Err(Refused::Gone) };
    let mut ids = vec![id.to_string()];
    ids.extend(ctx.registry.sharers(id, &old));
    let mut guards = Vec::new();
    for one in &ids {
        match ctx.registry.begin_move(one) {
            Some(guard) => guards.push(guard),
            None => return Err(Refused::Between),
        }
    }
    // Everyone is judged before anyone is stopped, so a refusal leaves every chat as it was.
    let mut chats = Vec::new();
    for one in &ids {
        let log = if one == id { log.clone() } else { crate::sessions::log_path(&ctx.registry.sessions_dir, one) };
        // A fork's first message is a turn running with no door: its `--cwd` is the folder it started in.
        if ctx.registry.forks.running(one) {
            return Err(Refused::Busy((one != id).then(|| called(ctx, one))));
        }
        let door = match ctx.registry.restartable(one) {
            Restart::Ready { model, yolo, .. } => {
                // Stopping a door and bringing it back reopens the log, and a door that opens a log ending mid-turn carries the turn on by
                // itself (hub.md section 4): only a settled log is stopped. eidolon's own `log` says; no answer is a no.
                if crate::fork::settled(ctx, &log, false).await != Some(true) {
                    return Err(Refused::Unsettled((one != id).then(|| called(ctx, one))));
                }
                Some((model, yolo))
            }
            Restart::Running => return Err(Refused::Busy((one != id).then(|| called(ctx, one)))),
            Restart::Between(_) => return Err(Refused::Between),
            Restart::NotOurs if ctx.registry.held(&log) || ctx.registry.roster_holds(&log) => return Err(Refused::Held),
            Restart::NotOurs => None,
        };
        chats.push(Along { id: one.clone(), log, door });
    }
    for chat in chats.iter().filter(|chat| chat.door.is_some()) {
        // A door that went away by itself since the check is still brought back below.
        let _ = ctx.registry.stop(&chat.id).await;
    }
    let changed = {
        let old = old.clone();
        tokio::task::spawn_blocking(move || op(&old)).await
    };
    let changed = changed.unwrap_or_else(|e| Err(anyhow::anyhow!("the folder change did not finish: {e}")));
    // The flags each door had, as the page would want them back: `-m` only when it told us a model.
    let bring = |ctx: &Ctx, chat: &Along| {
        let (model, yolo) = chat.door.clone().unwrap_or_default();
        let ctx = ctx.clone();
        let (id, log) = (chat.id.clone(), chat.log.clone());
        async move { crate::restart::bring_up(&ctx, &id, &log, Some(model.as_str()).filter(|m| !m.is_empty()), yolo).await }
    };
    match changed {
        Ok(new) => {
            for chat in &chats {
                ctx.registry.set_cwd(&chat.id, &new);
            }
            let mut moved = Moved { dir: new, reopened: false, lost: None };
            for chat in chats.iter().filter(|chat| chat.door.is_some()) {
                let failed = match bring(ctx, chat).await {
                    Ok(_) => None,
                    Err(crate::restart::Failed::Held) => Some("the chat's log was taken by another session".to_string()),
                    Err(crate::restart::Failed::Spawn(why)) => {
                        tracing::warn!(%why, id = chat.id, "perch: a chat did not come back after its folder moved");
                        Some(why)
                    }
                };
                match (failed, chat.id == id) {
                    (None, true) => moved.reopened = true,
                    (None, false) => {}
                    (Some(why), true) => moved.lost = Some(why),
                    (Some(why), false) => {
                        let other = format!("{} did not come back: {why}", called(ctx, &chat.id));
                        moved.lost = Some(match moved.lost.take() {
                            Some(first) => format!("{first}; {other}"),
                            None => other,
                        });
                    }
                }
            }
            Ok(moved)
        }
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), id, "perch: a chat's folder could not be changed; it stays where it was");
            let mut back = true;
            for chat in chats.iter().filter(|chat| chat.door.is_some()) {
                back &= bring(ctx, chat).await.is_ok();
            }
            if !back {
                return Err(Refused::Failed(format!("{e:#}; and a chat could not be brought back: continue it from the list")));
            }
            Err(Refused::Failed(format!("{e:#}")))
        }
    }
}

/// `scratch/<old>` to `scratch/<slug>` (`-2`, `-3`, ... when taken): the folder change of a rename. Only a real directory directly
/// inside scratch, and never one a project holds.
fn rename_in_scratch(registry: &sessions::Registry, old: &Path, wanted: &str) -> Result<PathBuf> {
    let scratch = registry.scratch.as_deref().context("no scratch directory")?.canonicalize().context("resolving the scratch directory")?;
    let old = old.canonicalize().with_context(|| format!("resolving {}", old.display()))?;
    anyhow::ensure!(old.parent() == Some(scratch.as_path()), "{} is not a chat folder of the inbox", old.display());
    anyhow::ensure!(std::fs::symlink_metadata(&old)?.is_dir(), "{} is not a directory", old.display());
    anyhow::ensure!(!registry.projects.holds(&old), "{} belongs to a project", old.display());
    let current = old.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_string();
    for n in 1..100 {
        let name = if n == 1 { wanted.to_string() } else { format!("{wanted}-{n}") };
        if name == current {
            return Ok(old);
        }
        let to = scratch.join(&name);
        // `rename` onto an existing empty directory would replace it: look first.
        if std::fs::symlink_metadata(&to).is_ok() {
            continue;
        }
        std::fs::rename(&old, &to).with_context(|| format!("renaming {} to {}", old.display(), to.display()))?;
        return Ok(to);
    }
    anyhow::bail!("no free folder name for {wanted}")
}

/// Whether a failed `rename` was the move across filesystems.
fn crosses(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::CrossesDevices || e.raw_os_error() == Some(libc::EXDEV)
}

/// The relative paths of a tree: directories (parents first) and files with their sizes. A link, a socket, a device or a pipe
/// anywhere in it is an error: they are not copied, and a move that cannot carry everything does not start.
fn scan(root: &Path, at: &Path, dirs: &mut Vec<PathBuf>, files: &mut Vec<(PathBuf, u64)>) -> Result<()> {
    for entry in std::fs::read_dir(root.join(at)).with_context(|| format!("listing {}", root.join(at).display()))? {
        let entry = entry?;
        let rel = at.join(entry.file_name());
        let meta = std::fs::symlink_metadata(entry.path())?;
        if meta.is_dir() {
            dirs.push(rel.clone());
            scan(root, &rel, dirs, files)?;
        } else if meta.is_file() {
            files.push((rel, meta.len()));
        } else {
            anyhow::bail!("{} is a link or a special file, which a move to another disk does not carry", entry.path().display());
        }
    }
    Ok(())
}

/// Moves a directory tree: `rename` when `src` and `dst` are one filesystem, else (or with `force_copy`) a checked copy and then the
/// removal of the source. `dst` must not exist. A copy that fails is removed again; the source is untouched until the copy checked out.
pub(crate) fn move_dir(src: &Path, dst: &Path, force_copy: bool) -> Result<()> {
    move_dir_between(src, dst, force_copy, || {})
}

/// [`move_dir`], with `between` run after the first look at `dst` and before anything is created there: the tests make `dst` appear in
/// that gap. **Only what this call created is ever removed**: `create_dir(dst)` is the one that decides who made it, and when it fails
/// (the name appeared meanwhile) nothing at `dst` is touched.
fn move_dir_between(src: &Path, dst: &Path, force_copy: bool, between: impl FnOnce()) -> Result<()> {
    anyhow::ensure!(std::fs::symlink_metadata(dst).is_err(), "{} already exists", dst.display());
    between();
    if !force_copy {
        match std::fs::rename(src, dst) {
            Ok(()) => return Ok(()),
            Err(e) if crosses(&e) => {}
            Err(e) => return Err(e).with_context(|| format!("moving {} to {}", src.display(), dst.display())),
        }
    }
    let (mut dirs, mut files) = (Vec::new(), Vec::new());
    scan(src, Path::new(""), &mut dirs, &mut files)?;
    // Not inside the closure below: a `dst` that is not ours (it appeared since the look) is an error with nothing to undo.
    std::fs::create_dir(dst).with_context(|| format!("creating {}", dst.display()))?;
    let copied = (|| -> Result<()> {
        for dir in &dirs {
            std::fs::create_dir(dst.join(dir)).with_context(|| format!("creating {}", dst.join(dir).display()))?;
        }
        for (file, _) in &files {
            std::fs::copy(src.join(file), dst.join(file)).with_context(|| format!("copying {}", src.join(file).display()))?;
        }
        // Checked against the scan before the source goes: the same folders, the same files, the same sizes.
        let (mut got_dirs, mut got_files) = (Vec::new(), Vec::new());
        scan(dst, Path::new(""), &mut got_dirs, &mut got_files)?;
        let sorted = |mut v: Vec<PathBuf>| {
            v.sort();
            v
        };
        anyhow::ensure!(sorted(got_dirs) == sorted(dirs.clone()), "the copy does not have the folders the original has");
        got_files.sort();
        let mut want_files = files.clone();
        want_files.sort();
        anyhow::ensure!(got_files == want_files, "the copy does not have the files the original has");
        Ok(())
    })();
    if let Err(e) = copied {
        let _ = std::fs::remove_dir_all(dst);
        return Err(e);
    }
    // The copy is whole and is where the chat will work, so a source that will not go away is a leftover, not a failure.
    if let Err(e) = std::fs::remove_dir_all(src) {
        tracing::warn!(error = %e, path = %src.display(), "perch: a moved folder's original could not be removed; the copy is the chat's");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The tidy loop: an inbox chat's folder follows its title, between turns
// ---------------------------------------------------------------------------

#[derive(Default)]
pub(crate) struct Tidy {
    idle: HashMap<String, u32>,
    /// `(id, slug)` pairs whose rename failed: not tried again, so a folder that cannot be renamed is not retried every tick.
    failed: HashSet<(String, String)>,
    /// Chats whose log ended mid-turn when last asked: not asked again for [`UNSETTLED_WAIT`], so a stuck chat does not cost an `eidolon log` every tick.
    unsettled: HashMap<String, std::time::Instant>,
}

/// Looks at the live inbox chats every [`TICK`] until `cancel`.
pub(crate) async fn tidy_loop(ctx: Ctx, cancel: CancellationToken) {
    let mut state = Tidy::default();
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = tick.tick() => {}
        }
        tidy_once(&ctx, &mut state).await;
    }
}

/// One look: every live inbox chat that has a name, is between turns (for [`IDLE_TICKS`] looks in a row) and whose folder does not
/// already follow it is renamed. Returns the ids it renamed (the tests read it).
pub(crate) async fn tidy_once(ctx: &Ctx, state: &mut Tidy) -> Vec<String> {
    let mut doors = ctx.registry.inbox_doors();
    // A title is learned from `eidolon sessions`, which runs only when a log it has not titled has moved.
    if doors.iter().any(|door| door.title.is_none()) {
        ctx.registry.refresh_titles().await;
        doors = ctx.registry.inbox_doors();
    }
    state.idle.retain(|id, _| doors.iter().any(|door| door.id == *id));
    state.unsettled.retain(|id, _| doors.iter().any(|door| door.id == *id));
    let mut renamed = Vec::new();
    for door in doors {
        let idle = state.idle.entry(door.id.clone()).or_insert(0);
        *idle = if door.running { 0 } else { idle.saturating_add(1) };
        if *idle < IDLE_TICKS {
            continue;
        }
        let Some(wanted) = door.title.as_deref().and_then(slug) else { continue };
        if state.failed.contains(&(door.id.clone(), wanted.clone())) {
            continue;
        }
        if state.unsettled.get(&door.id).is_some_and(|at| at.elapsed() < UNSETTLED_WAIT) {
            continue;
        }
        match follow(ctx, &door.id, &door.dir, &wanted, false).await {
            Ok(true) => renamed.push(door.id.clone()),
            Ok(false) => {}
            Err(Refused::Failed(_)) => {
                state.failed.insert((door.id.clone(), wanted));
            }
            // The log ends mid-turn: quietly skipped; reopening that door would spend, so the folder is not moved.
            Err(Refused::Unsettled(_)) => {
                state.unsettled.insert(door.id.clone(), std::time::Instant::now());
            }
            // Busy, between states, held or gone: nothing happened, and the next look decides again.
            Err(_) => {}
        }
    }
    renamed
}

/// Renames `dir` to follow `wanted`, when it is an inbox chat folder that is not a project's and not already named so. A folder another
/// chat works in too (a fork shares its source's) is left alone unless `shared_ok`: the tidy loop passes false, because two titles would
/// take turns renaming it; a rename by hand passes true, and the folder then moves for every chat that uses it ([`relocate`]).
/// `Ok(true)` when the folder changed.
async fn follow(ctx: &Ctx, id: &str, dir: &Path, wanted: &str, shared_ok: bool) -> std::result::Result<bool, Refused> {
    let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    if names::follows(name, wanted) || ctx.registry.projects.holds(dir) || (!shared_ok && ctx.registry.shared_cwd(id, dir)) {
        return Ok(false);
    }
    let registry = ctx.registry.clone();
    let wanted = wanted.to_string();
    let moved = relocate(ctx, id, move |old| rename_in_scratch(&registry, old, &wanted)).await?;
    Ok(moved.dir != dir)
}

// ---------------------------------------------------------------------------
// The routes
// ---------------------------------------------------------------------------

const NO_HOME: &str = "this server has no Minerva home to keep that in";
const OUTSIDE: &str = "that folder is not under the folders this server may open";

fn refused(why: Refused) -> Response<BoxBody> {
    match why {
        Refused::Busy(None) => problem(StatusCode::CONFLICT, "a turn is running: wait for the turn to end, then try again (moving the folder now would cut it off)"),
        Refused::Busy(Some(other)) => problem(StatusCode::CONFLICT, &format!("a turn is running in '{other}', which works in the same folder: wait for it to finish, then try again (moving the folder now would cut it off)")),
        Refused::Unsettled(None) => problem(StatusCode::CONFLICT, "that chat ended mid-turn; continue it first"),
        Refused::Unsettled(Some(other)) => problem(StatusCode::CONFLICT, &format!("'{other}', which works in the same folder, ended mid-turn; continue it first")),
        Refused::Between => problem(StatusCode::CONFLICT, "that chat is starting, stopping or being moved; try again in a moment"),
        Refused::Held => problem(StatusCode::CONFLICT, "another process is running that chat; it cannot be moved from here"),
        Refused::Gone => problem(StatusCode::CONFLICT, "that chat has no folder this server knows"),
        Refused::Failed(why) => problem(StatusCode::SERVICE_UNAVAILABLE, &why),
    }
}

/// `POST /hub/sessions/<id>/rename {"title": "<name>"}`: gives the chat a name that beats the derived title everywhere; an empty
/// title clears it. 200 `{"id", "title"}` (the title as the list now shows it); 400 not that shape or a bad name; 403 a shared-dir
/// chat; 404 no such chat. An inbox chat's folder follows the new name: at once when it is saved, within a moment when it is live
/// and between turns (the tidy loop), never while a turn runs.
pub(crate) async fn rename(ctx: &Ctx, id: &str, body: &Bytes) -> Response<BoxBody> {
    #[derive(serde::Deserialize)]
    struct Asked {
        title: String,
    }
    let log = match crate::trash::own_log(ctx, id) {
        Ok(log) => log,
        Err(refusal) => return *refusal,
    };
    let Ok(asked) = serde_json::from_slice::<Asked>(body) else {
        return problem(StatusCode::BAD_REQUEST, "expected a JSON body of {\"title\": \"<name>\"}");
    };
    let title = match names::clean_title(&asked.title) {
        Ok(title) => title,
        Err(why) => return problem(StatusCode::BAD_REQUEST, why),
    };
    let dir = ctx.registry.known_cwd(id).and_then(|cwd| ctx.registry.inbox_dir(&cwd));
    // A folder that several chats work in (a fork shares its source's) moves for all of them, now and before the name is kept: a chat
    // mid-turn among them refuses the whole rename, with its name, so nothing is half done.
    let shared = dir.as_deref().is_some_and(|dir| !ctx.registry.sharers(id, dir).is_empty());
    if shared
        && !title.is_empty()
        && let Some(dir) = &dir
        && let Some(wanted) = slug(&title)
    {
        match follow(ctx, id, dir, &wanted, true).await {
            Ok(_) => {}
            Err(Refused::Failed(why)) => tracing::debug!(%why, id, "perch: a renamed chat's folder was left as it was"),
            Err(refusal) => return refused(refusal),
        }
    }
    if let Err(e) = ctx.registry.names.set(id, Some(&title)) {
        tracing::warn!(error = %format!("{e:#}"), "perch: names.json could not be written");
        return problem(StatusCode::SERVICE_UNAVAILABLE, "that name could not be saved");
    }
    // A saved inbox chat has no door to wait for: its folder follows now. A live one is the tidy loop's, between turns.
    if !shared
        && !title.is_empty()
        && !ctx.registry.held(&log)
        && let Some(dir) = &dir
        && let Some(wanted) = slug(&title)
        && let Err(Refused::Failed(why)) = follow(ctx, id, dir, &wanted, true).await
    {
        tracing::debug!(%why, id, "perch: a renamed chat's folder was left as it was");
    }
    let shown = ctx.registry.titled_list().await.into_iter().find(|row| row.id == id).and_then(|row| row.title);
    json(StatusCode::OK, &serde_json::json!({ "id": id, "title": shown }))
}

/// `POST /hub/sessions/<id>/promote {"parent": "<abs dir>", "name": "<folder name>"}`: an inbox chat's folder is MOVED to
/// `<parent>/<name>`, registered as a project named `<name>`, and the chat reopens in it.
///
/// ```text
///   404  no such chat   403 a shared-dir chat   400 not that shape, or a name that cannot be a folder
///   409  not an inbox chat | its folder is shared with another chat | the target exists | a turn is running | held elsewhere
///   422  the parent is not a folder under the allowed roots, or is inside the inbox
///   503  the move failed (the chat is back where it was) or the project could not be saved
///   200  {"id", "path", "name", "restarted"}
/// ```
pub(crate) async fn promote(ctx: &Ctx, id: &str, body: &Bytes) -> Response<BoxBody> {
    #[derive(serde::Deserialize)]
    struct Asked {
        parent: String,
        name: String,
    }
    if let Err(refusal) = crate::trash::own_log(ctx, id) {
        return *refusal;
    }
    let Ok(asked) = serde_json::from_slice::<Asked>(body) else {
        return problem(StatusCode::BAD_REQUEST, "expected a JSON body of {\"parent\": \"<dir>\", \"name\": \"<folder name>\"}");
    };
    if let Err(why) = crate::dirs::check_name(&asked.name) {
        return problem(StatusCode::BAD_REQUEST, why);
    }
    if !ctx.registry.projects.enabled() {
        return problem(StatusCode::SERVICE_UNAVAILABLE, NO_HOME);
    }
    if ctx.registry.known_cwd(id).and_then(|cwd| ctx.registry.inbox_dir(&cwd)).is_none() {
        return problem(StatusCode::CONFLICT, "only a chat in the inbox can be made a project (this one already works in a folder of its own)");
    }
    let roots = ctx.registry.roots().to_vec();
    let parent = asked.parent.clone();
    let resolved = tokio::task::spawn_blocking(move || crate::dirs::resolve(&roots, if parent.len() > 1 { parent.trim_end_matches('/') } else { &parent })).await;
    let Ok(Some((parent, _))) = resolved else { return problem(StatusCode::UNPROCESSABLE_ENTITY, OUTSIDE) };
    if ctx.registry.is_home_internal(&parent) {
        return problem(StatusCode::UNPROCESSABLE_ENTITY, "choose a folder of yours: the Minerva home is the server's own");
    }
    if let Some(scratch) = ctx.registry.scratch.as_deref().and_then(|s| s.canonicalize().ok())
        && parent.starts_with(&scratch)
    {
        return problem(StatusCode::UNPROCESSABLE_ENTITY, "choose a folder outside the inbox: the inbox is where unsorted chats live");
    }
    let target = parent.join(&asked.name);
    if std::fs::symlink_metadata(&target).is_ok() {
        return problem(StatusCode::CONFLICT, "something with that name is already in that folder");
    }
    let registry = ctx.registry.clone();
    let (to, label) = (target.clone(), asked.name.clone());
    let moved = relocate(ctx, id, move |old| {
        // Registered first: a project that cannot be saved stops the move before anything has moved.
        let (_, created) = registry.projects.add(&to, Some(&label)).map_err(|e| anyhow::anyhow!("the project could not be saved ({e:?})"))?;
        if let Err(e) = move_dir(old, &to, false) {
            if created {
                let _ = registry.projects.remove(&to.to_string_lossy());
            }
            return Err(e);
        }
        to.canonicalize().with_context(|| format!("resolving {}", to.display()))
    })
    .await;
    match moved {
        Ok(moved) => {
            // Nothing in the old inbox row now names the folder; the list reads the new one from the cwd map.
            let mut said = serde_json::json!({ "id": id, "path": moved.dir.to_string_lossy(), "name": asked.name, "restarted": moved.reopened });
            if let Some(why) = moved.lost {
                said["warning"] = serde_json::Value::String(format!("the folder moved, but the chat did not come back on its own: {why}; continue it from the list"));
            }
            json(StatusCode::OK, &said)
        }
        Err(why) => refused(why),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(root: &Path) {
        std::fs::create_dir_all(root.join("sub/deep")).unwrap();
        std::fs::write(root.join("a.txt"), "alpha").unwrap();
        std::fs::write(root.join("sub/b.txt"), "beta beta").unwrap();
        std::fs::write(root.join("sub/deep/c.bin"), [0u8, 1, 2, 3]).unwrap();
        std::fs::create_dir(root.join("empty")).unwrap();
    }

    fn read_all(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let (mut dirs, mut files) = (Vec::new(), Vec::new());
        scan(root, Path::new(""), &mut dirs, &mut files).unwrap();
        files.sort();
        files.into_iter().map(|(rel, _)| (rel.clone(), std::fs::read(root.join(rel)).unwrap())).collect()
    }

    /// A target that appears after the first look is not ours: the copy refuses, and what is there (and the source) is untouched.
    /// Mutation: create `dst` inside the cleanup closure and `remove_dir_all(dst)` on any failure, and `mine.txt` is gone.
    #[test]
    fn a_target_that_appears_meanwhile_is_never_removed() {
        let dir = tempfile::tempdir().unwrap();
        let (src, dst) = (dir.path().join("a"), dir.path().join("b"));
        tree(&src);
        let before = read_all(&src);
        let err = move_dir_between(&src, &dst, true, || {
            std::fs::create_dir(&dst).unwrap();
            std::fs::write(dst.join("mine.txt"), "not yours").unwrap();
        })
        .unwrap_err();
        assert!(format!("{err:#}").contains("creating"), "{err:#}");
        assert_eq!(std::fs::read_to_string(dst.join("mine.txt")).unwrap(), "not yours");
        assert_eq!(read_all(&src), before, "the source is whole");
    }

    #[test]
    fn a_same_filesystem_move_is_a_rename() {
        let dir = tempfile::tempdir().unwrap();
        let (src, dst) = (dir.path().join("a"), dir.path().join("b"));
        tree(&src);
        let before = read_all(&src);
        move_dir(&src, &dst, false).unwrap();
        assert!(!src.exists());
        assert_eq!(read_all(&dst), before);
        assert!(dst.join("empty").is_dir());
        assert!(move_dir(&dst, &dst, false).is_err(), "an existing target is refused");
    }

    #[test]
    fn the_copy_path_carries_everything_and_removes_the_source_only_after() {
        let dir = tempfile::tempdir().unwrap();
        let (src, dst) = (dir.path().join("a"), dir.path().join("b"));
        tree(&src);
        let before = read_all(&src);
        move_dir(&src, &dst, true).unwrap();
        assert!(!src.exists(), "the source is gone once the copy checked out");
        assert_eq!(read_all(&dst), before);
        assert!(dst.join("empty").is_dir() && dst.join("sub/deep").is_dir());
    }

    #[test]
    fn the_copy_path_refuses_a_link_before_writing_anything() {
        let dir = tempfile::tempdir().unwrap();
        let (src, dst) = (dir.path().join("a"), dir.path().join("b"));
        tree(&src);
        std::os::unix::fs::symlink("/etc/hostname", src.join("sub/link")).unwrap();
        let err = move_dir(&src, &dst, true).unwrap_err();
        assert!(format!("{err:#}").contains("link"), "{err:#}");
        assert!(!dst.exists(), "nothing was copied");
        assert!(src.join("a.txt").is_file() && std::fs::symlink_metadata(src.join("sub/link")).is_ok(), "the source is whole");
    }

    /// A real second filesystem when this machine has one next to the temp dir (tmpfs at /dev/shm, usually): `rename` fails with
    /// EXDEV there and `move_dir` takes the copy path by itself. Silent when there is only one.
    #[test]
    fn a_move_across_mounts_falls_back_to_the_copy() {
        use std::os::unix::fs::MetadataExt as _;
        let here = tempfile::tempdir().unwrap();
        let Ok(there) = tempfile::tempdir_in("/dev/shm") else { return };
        if std::fs::metadata(here.path()).unwrap().dev() == std::fs::metadata(there.path()).unwrap().dev() {
            return;
        }
        let (src, dst) = (here.path().join("a"), there.path().join("b"));
        tree(&src);
        let before = read_all(&src);
        assert!(crosses(&std::fs::rename(&src, &dst).unwrap_err()), "the two really are different filesystems");
        move_dir(&src, &dst, false).unwrap();
        assert!(!src.exists());
        assert_eq!(read_all(&dst), before);
    }
}
