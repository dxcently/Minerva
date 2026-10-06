//! The handlers of archive, pin and trash (hub.md section 7b): `POST /hub/sessions/<id>/{archive,unarchive,pin,unpin,trash}`,
//! `GET /hub/trash`, `POST /hub/trash/<id>/restore`, `POST /hub/trash/empty`. The state and the 7-day rule are [`crate::archive`]'s.
//!
//! **Only the Minerva home's own chats.** A chat from eidolon's shared sessions dir (`origin: "other"`) is read-only to the perch:
//! archive, pin and trash are 403 on it. Everything this file moves or removes is a regular file `<id>.eid` directly inside
//! `<home>/sessions` or `<home>/trash`; an id is [`sessions::id_ok`] before it is ever joined to a path, and a symlink is never
//! moved, followed or removed through.
//!
//! **Delete is a move.** `trash` renames the log into `<home>/trash/` (same filesystem, so atomic, keeping its name). A chat
//! that is live is a 409 unless the body says `{"stop": true}`, which stops its door cleanly first. Only `empty` removes a
//! file for good, and only from the trash dir, and only with `{"confirm": true}`.

use bytes::Bytes;
use hyper::{Response, StatusCode};

use crate::archive::{self, now_ms};
use crate::serve::{BoxBody, Ctx, json, problem, status};
use crate::sessions::{id_ok, log_path};

const NO_HOME: &str = "this server has no Minerva home to keep that in";

/// One chat in the trash, as `GET /hub/trash` lists it. `mtime` is the log's last write, in unix seconds.
#[derive(serde::Serialize)]
pub struct TrashRow {
    pub id: String,
    pub title: String,
    pub mtime: Option<u64>,
}

/// The home's own log for `id`, or the refusal to answer with. Never a shared-dir one (403), never a symlink's target (404).
pub(crate) fn own_log(ctx: &Ctx, id: &str) -> Result<std::path::PathBuf, Box<Response<BoxBody>>> {
    if !id_ok(id) {
        return Err(Box::new(status(StatusCode::NOT_FOUND)));
    }
    if ctx.registry.home().is_none() {
        return Err(Box::new(problem(StatusCode::SERVICE_UNAVAILABLE, NO_HOME)));
    }
    let Some((log, origin)) = ctx.registry.find_log(id) else { return Err(Box::new(status(StatusCode::NOT_FOUND))) };
    if origin.is_some() {
        return Err(Box::new(problem(StatusCode::FORBIDDEN, "that chat belongs to eidolon's own sessions folder, which is read-only here")));
    }
    if ctx.registry.id_of_log(&log).as_deref() != Some(id) {
        return Err(Box::new(status(StatusCode::NOT_FOUND)));
    }
    Ok(log)
}

fn exists_fn(ctx: &Ctx) -> impl Fn(&str) -> bool + '_ {
    |id| log_path(&ctx.registry.sessions_dir, id).is_file()
}

fn held(ctx: &Ctx, log: &std::path::Path) -> bool {
    ctx.registry.held(log) || ctx.registry.roster_holds(log)
}

fn saved(what: anyhow::Result<()>) -> Response<BoxBody> {
    match what {
        Ok(()) => status(StatusCode::NO_CONTENT),
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "perch: archive.json could not be written");
            problem(StatusCode::SERVICE_UNAVAILABLE, "that could not be saved")
        }
    }
}

/// `POST /hub/sessions/<id>/archive` (`on`) and `.../unarchive`: 204. A live chat cannot be archived (409).
pub async fn archive(ctx: &Ctx, id: &str, on: bool) -> Response<BoxBody> {
    let log = match own_log(ctx, id) {
        Ok(log) => log,
        Err(refusal) => return *refusal,
    };
    if on && held(ctx, &log) {
        return problem(StatusCode::CONFLICT, "that chat is running; stop it first, or leave it where it is");
    }
    let exists = exists_fn(ctx);
    let done = if on { ctx.registry.archive.archive(id, now_ms(), &exists) } else { ctx.registry.archive.unarchive(id, now_ms(), &exists) };
    saved(done)
}

/// `POST /hub/sessions/<id>/pin` (`on`) and `.../unpin`: 204. Pinning an archived chat brings it out of `other`.
pub async fn pin(ctx: &Ctx, id: &str, on: bool) -> Response<BoxBody> {
    if let Err(refusal) = own_log(ctx, id) {
        return *refusal;
    }
    let done = ctx.registry.archive.set_pinned(id, on, &exists_fn(ctx));
    saved(done)
}

/// `POST /hub/sessions/<id>/trash {"stop"?: bool}`: moves the log into `<home>/trash/` and forgets its pin and archive mark.
/// 409 when something holds the log, unless `stop` is true and the holder is a door of ours (stopped cleanly first).
pub async fn trash(ctx: &Ctx, id: &str, body: &Bytes) -> Response<BoxBody> {
    #[derive(serde::Deserialize, Default)]
    struct Asked {
        #[serde(default)]
        stop: bool,
    }
    let asked = if body.is_empty() {
        Asked::default()
    } else {
        match serde_json::from_slice::<Asked>(body) {
            Ok(asked) => asked,
            Err(_) => return problem(StatusCode::BAD_REQUEST, "expected a JSON body of {\"stop\": true}?"),
        }
    };
    let log = match own_log(ctx, id) {
        Ok(log) => log,
        Err(refusal) => return *refusal,
    };
    if ctx.registry.roster_holds(&log) {
        return problem(StatusCode::CONFLICT, "another process is running that chat; it cannot be deleted from here");
    }
    if ctx.registry.forks.running(id) {
        return problem(StatusCode::CONFLICT, "the first message of that fork is still running; wait for it to finish");
    }
    // A folder move in flight has stopped this chat's door and will bring it back on the log: the log must not leave under it.
    if ctx.registry.is_moving(id) {
        return problem(StatusCode::CONFLICT, "that chat is being moved; try again in a moment");
    }
    if ctx.registry.held(&log) {
        if !asked.stop {
            return problem(StatusCode::CONFLICT, "that chat is running; stop it first");
        }
        ctx.registry.stop(id).await;
        if held(ctx, &log) {
            return problem(StatusCode::CONFLICT, "that chat did not stop");
        }
    }
    let Some(home) = ctx.registry.home() else { return problem(StatusCode::SERVICE_UNAVAILABLE, NO_HOME) };
    let dir = archive::trash_dir(home);
    let dest = log_path(&dir, id);
    let moved = (|| -> std::io::Result<bool> {
        archive::ensure_trash(&dir)?;
        if std::fs::symlink_metadata(&dest).is_ok() {
            return Ok(false);
        }
        std::fs::rename(&log, &dest)?;
        Ok(true)
    })();
    match moved {
        Ok(true) => {}
        Ok(false) => return problem(StatusCode::CONFLICT, "a chat with that name is already in the trash"),
        Err(e) => {
            tracing::warn!(error = %e, "perch: a chat could not be moved into the trash");
            return problem(StatusCode::SERVICE_UNAVAILABLE, "that chat could not be moved into the trash");
        }
    }
    let forgot = ctx.registry.archive.forget(id, &exists_fn(ctx));
    saved(forgot)
}

/// A regular file `<id>.eid` directly in `dir` (never a symlink), by id.
fn trashed_ids(dir: &std::path::Path) -> Vec<(String, std::path::PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("eid") {
                return None;
            }
            let id = path.file_stem()?.to_str().filter(|id| id_ok(id))?.to_string();
            std::fs::symlink_metadata(&path).ok()?.is_file().then_some((id, path))
        })
        .collect()
}

/// `GET /hub/trash`: `[{id, title, mtime}]`, newest first.
pub async fn list(ctx: &Ctx) -> Response<BoxBody> {
    let Some(home) = ctx.registry.home() else { return json(StatusCode::OK, &Vec::<TrashRow>::new()) };
    let mut rows: Vec<TrashRow> = trashed_ids(&archive::trash_dir(home))
        .into_iter()
        .map(|(id, path)| {
            let mtime = crate::titles::mtime_secs(&path);
            TrashRow { title: ctx.registry.trash_title(&id, mtime), id, mtime }
        })
        .collect();
    rows.sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| a.id.cmp(&b.id)));
    json(StatusCode::OK, &rows)
}

/// `POST /hub/trash/<id>/restore`: moves it back into `sessions/` (404 not in the trash, 409 a chat of that name is there). A
/// restored chat gets a fresh week before it can age into `other`, and is not pinned again.
pub async fn restore(ctx: &Ctx, id: &str) -> Response<BoxBody> {
    if !id_ok(id) {
        return status(StatusCode::NOT_FOUND);
    }
    let Some(home) = ctx.registry.home() else { return problem(StatusCode::SERVICE_UNAVAILABLE, NO_HOME) };
    let from = log_path(&archive::trash_dir(home), id);
    if !std::fs::symlink_metadata(&from).is_ok_and(|m| m.is_file()) {
        return status(StatusCode::NOT_FOUND);
    }
    let to = log_path(&ctx.registry.sessions_dir, id);
    if std::fs::symlink_metadata(&to).is_ok() || ctx.registry.find_log(id).is_some() {
        return problem(StatusCode::CONFLICT, "a chat with that name already exists");
    }
    if let Err(e) = std::fs::rename(&from, &to) {
        tracing::warn!(error = %e, "perch: a chat could not be restored");
        return problem(StatusCode::SERVICE_UNAVAILABLE, "that chat could not be restored");
    }
    let marked = ctx.registry.archive.unarchive(id, now_ms(), &exists_fn(ctx));
    saved(marked)
}

/// `POST /hub/trash/empty {"confirm": true}`: removes every chat in the trash for good; `{"removed": n}`. Only regular `.eid`
/// files directly inside `<home>/trash` are removed.
pub async fn empty(ctx: &Ctx, body: &Bytes) -> Response<BoxBody> {
    #[derive(serde::Deserialize)]
    struct Asked {
        confirm: bool,
    }
    if !serde_json::from_slice::<Asked>(body).is_ok_and(|asked| asked.confirm) {
        return problem(StatusCode::BAD_REQUEST, "expected {\"confirm\": true}");
    }
    let Some(home) = ctx.registry.home() else { return problem(StatusCode::SERVICE_UNAVAILABLE, NO_HOME) };
    let mut removed = 0;
    for (id, path) in trashed_ids(&archive::trash_dir(home)) {
        match std::fs::remove_file(&path) {
            Ok(()) => {
                removed += 1;
                ctx.registry.forget_cwd(&id);
            }
            Err(e) => tracing::warn!(error = %e, "perch: a trashed chat could not be removed"),
        }
    }
    json(StatusCode::OK, &serde_json::json!({ "removed": removed }))
}

/// A chat that was continued is active again: out of `other` by hand, with a fresh week.
pub fn continued(ctx: &Ctx, id: &str) {
    if ctx.registry.archive.is_archived(id)
        && let Err(e) = ctx.registry.archive.unarchive(id, now_ms(), &exists_fn(ctx))
    {
        tracing::warn!(error = %format!("{e:#}"), "perch: archive.json could not be written");
    }
}
