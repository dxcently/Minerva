//! `POST /hub/sessions/<id>/fork` (hub.md section 7c): a NEW chat that starts as a copy of one saved or running chat, whole.
//!
//! **What the perch does:** copies the source log byte for byte to a fresh `<home>/sessions/<ms>.eid` (eidolon's own naming,
//! `<unix ms>.eid`, then `-1`, `-2` on a clash; `create_new`, so a name is never reused), then brings a door up on the copy the way
//! a resume does (Minerva config, the cwd the log names, which is the source's). The perch never reads or writes a byte of the
//! EIDO format: it copies a file.
//!
//! **A running source is fine, between turns.** The log is append-only and a reader of it sees each record whole or not at all
//! (`session/log.rs`: a torn tail is dropped on open, and the open that drops it is the one on the *copy*, never the source), so
//! copying a live log is safe. What is not safe is the money: a door that opens a log ending mid-turn carries the turn on by itself
//! (hub.md section 4, "Resume can spend"). So a source that is not settled (eidolon's own `log` says: `settled: false`) is a 409,
//! "wait for the turn to end", and nothing is copied. The source is only ever read.
//!
//! **Whole chat, or from a point.** `{"at": <record>}` goes to [`crate::pending::fork_at`]: a copy with no door, finished by the first
//! message (`resume --at`, stock eidolon, see that module). The head is *in memory* only, so nothing here moves it.
//!
//! A chat from eidolon's shared sessions dir (`origin: "other"`) can be forked: it is read, and the copy lands in the Minerva home.

use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use bytes::Bytes;
use hyper::{Response, StatusCode};

use crate::names;
use crate::serve::{BoxBody, Ctx, json, problem, status};
use crate::sessions::{id_ok, log_path};

/// Copies `source` to a new, never-before-used `<dir>/<ms>.eid` and returns its id. A copy that fails part way is removed.
pub(crate) fn copy_log(source: &Path, dir: &Path) -> Result<String> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut from = std::fs::File::open(source).with_context(|| format!("opening {}", source.display()))?;
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).context("reading the clock")?.as_millis();
    for n in 0u32..64 {
        let id = if n == 0 { ms.to_string() } else { format!("{ms}-{n}") };
        let path = log_path(dir, &id);
        // A chat's log is the owner's alone, whatever the umask.
        let mut to = match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&path) {
            Ok(to) => to,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e).with_context(|| format!("creating {}", path.display())),
        };
        let copied = std::io::copy(&mut from, &mut to).and_then(|_| to.flush()).and_then(|_| to.sync_all());
        if let Err(e) = copied {
            let _ = std::fs::remove_file(&path);
            return Err(e).with_context(|| format!("copying {} to {}", source.display(), path.display()));
        }
        return Ok(id);
    }
    anyhow::bail!("no free name for the copy in {}", dir.display())
}

/// Whether the log ends on a clean turn boundary, by eidolon's own `log` (read-only, so safe against a running writer).
pub(crate) async fn settled(ctx: &Ctx, log: &Path, other: bool) -> Option<bool> {
    let config = if other { None } else { ctx.registry.eidolon_config.as_deref() };
    let args = ["log".into(), log.into(), "--after".into(), "9999999999".into()];
    let out = crate::titles::run(&ctx.registry.eidolon, config, &args, 64 * 1024, crate::titles::LOG_TIMEOUT).await.ok()?;
    let text = String::from_utf8_lossy(&out);
    let head = text.lines().rev().find(|line| line.starts_with("head:"))?;
    if head.contains("settled: true") {
        Some(true)
    } else if head.contains("settled: false") {
        Some(false)
    } else {
        None
    }
}

/// `POST /hub/sessions/<id>/fork` with `{}` or no body:
///
/// ```text
///   404  no such chat (a bad id too)              422  `at` is not a point to fork from
///   400  not that shape                           409  mid-turn (a copy would continue the turn): wait for it to end
///   503  the copy or its door did not come up (the copy is removed), or eidolon could not say whether the chat is settled
///   201  {"id": "<new id>", "from": "<id>"}       the new chat is live, in the Minerva home, in the source's cwd
/// ```
pub(crate) async fn fork(ctx: &Ctx, id: &str, body: &Bytes) -> Response<BoxBody> {
    #[derive(serde::Deserialize, Default)]
    struct Asked {
        at: Option<serde_json::Value>,
    }
    if !id_ok(id) {
        return status(StatusCode::NOT_FOUND);
    }
    let asked = if body.is_empty() {
        Asked::default()
    } else {
        match serde_json::from_slice::<Asked>(body) {
            Ok(asked) => asked,
            Err(_) => return problem(StatusCode::BAD_REQUEST, "expected no body, or a JSON object"),
        }
    };
    // A point in the chat: a pending copy that keeps the branch when its first message runs (pending.rs).
    if let Some(at) = asked.at {
        return match at.as_u64() {
            Some(at) => crate::pending::fork_at(ctx, id, at).await,
            None => problem(StatusCode::BAD_REQUEST, "`at` is a record number"),
        };
    }
    if ctx.registry.forks.is_pending(id) {
        return problem(StatusCode::CONFLICT, "that fork has not had its first message yet: send one, then fork from it");
    }
    let Some((source, origin)) = ctx.registry.find_log(id) else { return status(StatusCode::NOT_FOUND) };
    if ctx.registry.id_of_log(&source).as_deref() != Some(id) {
        return status(StatusCode::NOT_FOUND);
    }
    // Between turns only: a door that opens a log ending mid-turn carries the turn on by itself (hub.md section 4, "Resume can
    // spend"), and a fork never spends. eidolon's own `log` says whether the branch is settled; past the last record it prints
    // only that line.
    match settled(ctx, &source, origin.is_some()).await {
        Some(true) => {}
        Some(false) => return problem(StatusCode::CONFLICT, "that chat is in the middle of a turn: wait for the turn to end, then fork (a copy would carry the turn on)"),
        None => return problem(StatusCode::SERVICE_UNAVAILABLE, "eidolon could not say whether that chat is between turns"),
    }
    let dir: PathBuf = ctx.registry.sessions_dir.clone();
    let copied = {
        let (source, dir) = (source.clone(), dir.clone());
        tokio::task::spawn_blocking(move || copy_log(&source, &dir)).await
    };
    let new_id = match copied {
        Ok(Ok(new_id)) => new_id,
        Ok(Err(e)) => {
            tracing::warn!(error = %format!("{e:#}"), "perch: a chat could not be copied for a fork");
            return problem(StatusCode::SERVICE_UNAVAILABLE, "that chat could not be copied");
        }
        Err(_) => return problem(StatusCode::SERVICE_UNAVAILABLE, "that chat could not be copied"),
    };
    let log = log_path(&dir, &new_id);
    // The copy's log names the folder the source was born in, which a rename or a move may have left behind: the copy works where
    // the source does now (and `bring_up` passes it as `--cwd`). It shares that folder, so neither chat moves it from under the other.
    if let Some(cwd) = ctx.registry.known_cwd(id).filter(|cwd| cwd.is_dir()) {
        ctx.registry.set_cwd(&new_id, &cwd);
    }
    // The copy starts as the source is: its model and `--yolo`, when the perch knows them (a live door says; a saved chat is what
    // this run remembered, else what `eidolon sessions` listed), else the config's default.
    ctx.registry.refresh_titles().await;
    let (model, yolo) = ctx.registry.flags_of(id).unzip();
    let model = model.filter(|m| crate::models::key_ok(m));
    let source_title = ctx.registry.titled_list().await.into_iter().find(|row| row.id == id).and_then(|row| row.title);
    match crate::restart::bring_up(ctx, &new_id, &log, model.as_deref(), yolo.unwrap_or(false)).await {
        Ok(_) => {
            if let Some(title) = source_title {
                let kept: String = title.chars().take(names::MAX_TITLE - " (fork)".len()).collect();
                if let Err(e) = ctx.registry.names.set(&new_id, Some(&format!("{kept} (fork)"))) {
                    tracing::warn!(error = %format!("{e:#}"), "perch: a fork's name could not be saved");
                }
            }
            json(StatusCode::CREATED, &serde_json::json!({ "id": new_id, "from": id }))
        }
        Err(failed) => {
            // The copy was never anyone's chat: do not leave it in the list as one.
            let _ = std::fs::remove_file(&log);
            ctx.registry.forget_dead(&new_id);
            match failed {
                crate::restart::Failed::Held => problem(StatusCode::CONFLICT, "the copy was taken by another session"),
                crate::restart::Failed::Spawn(why) => problem(StatusCode::SERVICE_UNAVAILABLE, &why),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_copy_is_byte_identical_new_and_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src.eid");
        std::fs::write(&source, b"EIDO\x01\x00abc\xff\x00torn").unwrap();
        let (a, b) = (copy_log(&source, dir.path()).unwrap(), copy_log(&source, dir.path()).unwrap());
        assert_ne!(a, b, "two copies in one millisecond still get two names");
        for id in [&a, &b] {
            assert!(id_ok(id), "{id}");
            assert_eq!(std::fs::read(log_path(dir.path(), id)).unwrap(), std::fs::read(&source).unwrap());
        }
    }

    #[test]
    fn a_copy_is_owner_only_whatever_the_source_mode() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("src.eid");
        std::fs::write(&source, b"EIDO").unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o666)).unwrap();
        let id = copy_log(&source, dir.path()).unwrap();
        let mode = std::fs::metadata(log_path(dir.path(), &id)).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode & 0o077, 0, "{mode:o}");
    }

    #[test]
    fn a_missing_source_leaves_no_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        assert!(copy_log(&dir.path().join("nope.eid"), dir.path()).is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
