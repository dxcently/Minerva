//! What the composer's `model` and `mode` menus call: `GET /hub/models` and `POST /hub/sessions/<id>/restart`.
//!
//! **A running door cannot change its model or its `--yolo`.** eidolon's `web` command takes both at start and the door has no
//! route that moves either (its routes are `events`, `say`, `answer`, `launch`, `cancel`), so "switch model" is **stop the
//! door cleanly, then resume the same log with the new flags**. The chat keeps its id, its folder and the Minerva config
//! (`--config` is the perch's, passed again by [`spawn::argv`]); the log is the transcript, so nothing is lost; the page's pane
//! stays on the id and reconnects, which replays the log. `effort` is not a flag of `web` (there is none in eidolon's CLI at
//! this revision), so there is no effort route and the page shows none.
//!
//! **Every reopen passes `--cwd`** when the perch remembers a folder for the chat (`chat-cwds.json`): eidolon's `web --session <log> --cwd <dir>`
//! runs the tools in `<dir>` and leaves the log's start record alone, which is how a chat survives its folder being renamed to its
//! title or moved into a project.
//!
//! **Only between turns.** A restart in the middle of a turn would cut it off, so a door whose `turn-state` says running is
//! a 409 with a sentence to show. The perch knows because the stream it holds to each door ([`crate::watch`]) reads
//! `turn-state`; that is also why a resumed door's stream is kept now, as a new session's always was.
//!
//! **A restart that fails does not leave the chat dead if it can help it:** when the new flags do not come up (an unknown
//! model, a provider that refuses to start), the perch brings the chat back with the flags it had and says so.

use std::path::Path;

use bytes::Bytes;
use hyper::{Response, StatusCode};

use crate::models;
use crate::serve::{BoxBody, Ctx, Up, door_up, json, problem};
use crate::sessions::{self, Restart};
use crate::spawn;
use crate::titles::RunError;

/// Why a door was not brought up on a log.
pub(crate) enum Failed {
    /// Something already holds that log (a live door of ours, or the roster says another process does).
    Held,
    /// The door did not come up, or opened another log; the words are for the person.
    Spawn(String),
}

/// Spawn a door on an existing log with the Minerva config and the given `-m` / `--yolo`, check it opened *that* log, and make
/// the row live, holding the perch's stream to it. This is what a resume is, and what the second half of a restart is. The
/// caller has already judged the id, the log, and whether anything holds it. Returns the model and yolo the door reports.
pub(crate) async fn bring_up(ctx: &Ctx, id: &str, log: &Path, model: Option<&str>, yolo: bool) -> Result<(String, bool), Failed> {
    // Reserved before the fork, so a second resume in flight cannot pass the caller's check too.
    if !ctx.registry.reserve(id, log) {
        return Err(Failed::Held);
    }
    // `--cwd` is eidolon's reopen-time override: the tools run where the perch last put this chat (its folder may have been renamed
    // after its title, or moved into a project), while the log keeps naming the folder it was born in. Passed whenever the perch
    // remembers a folder that is still there; for a chat that never moved it is the folder the log names, which changes nothing.
    let cwd = ctx.registry.reopen_cwd(id);
    let target = match &cwd {
        Some(cwd) => spawn::Target::KeptIn(log, cwd),
        None => spawn::Target::Kept(log),
    };
    let argv = spawn::argv(&ctx.registry.eidolon, ctx.registry.eidolon_config.as_deref(), target, model, yolo);
    let up = door_up(ctx, argv, Some(id)).await.map_err(Failed::Spawn)?;
    let Up { pid, port, token, exit, watcher, hello } = up;
    // The door must have opened the log we asked it to. A door that opened another one is exactly the two-writers case, wearing a hat.
    if ctx.registry.id_of_log(&hello.session).as_deref() != Some(id) {
        let why = format!("the door opened {}, not the log we asked for", hello.session.display());
        ctx.registry.dead(id, pid);
        crate::serve::give_up(&ctx.registry, watcher, pid, port, &token, exit).await;
        return Err(Failed::Spawn(why));
    }
    // The cwd the log named is kept as the checked path when it is under a `--root`; when it is not, it is kept as given (the
    // list still shows it) and `/hub/tree` refuses it, because the tree re-checks its root against `--root` on every request.
    let cwd = ctx.registry.cwd_under_a_root(&hello.cwd).unwrap_or_else(|_| hello.cwd.clone());
    ctx.registry.mark_live(id, &hello.session, &cwd);
    ctx.registry.set_flags(id, &hello.model, hello.yolo);
    ctx.registry.keep_watcher(id, watcher);
    Ok((hello.model, hello.yolo))
}

/// `GET /hub/models`: `[{"key","name"?,"note"?}]`, eidolon's own listing, cached ten minutes.
pub(crate) async fn models(ctx: &Ctx) -> Response<BoxBody> {
    match models::list(&ctx.registry.eidolon, ctx.registry.eidolon_config.as_deref()).await {
        Ok(list) => json(StatusCode::OK, &list),
        Err(RunError::Timeout) => problem(StatusCode::GATEWAY_TIMEOUT, "eidolon did not list its models in time"),
        Err(RunError::TooBig) => problem(StatusCode::BAD_GATEWAY, "eidolon's model listing is over the size limit"),
        Err(RunError::Failed(why)) => {
            tracing::warn!(%why, "perch: eidolon models failed");
            problem(StatusCode::BAD_GATEWAY, "eidolon could not list its models")
        }
    }
}

/// `POST /hub/sessions/<id>/restart` with `{"model": "<m>"?, "yolo": <bool>?}` (at least one): see the module doc.
///
/// ```text
///   400  not that shape, or a model that cannot be a `-m` value     404  no such chat
///   409  a turn is running ("wait for the turn to end") | starting or stopping | not running here | something holds the log
///   200  {"id", "model", "yolo", "restarted"}   restarted is false when the asked flags were already the door's
///   503  {"error", "reverted"}   the new flags did not come up; reverted says whether the old ones did
/// ```
pub(crate) async fn restart(ctx: &Ctx, id: &str, body: &Bytes) -> Response<BoxBody> {
    #[derive(serde::Deserialize)]
    struct Asked {
        model: Option<String>,
        yolo: Option<bool>,
    }
    const SHAPE: &str = "expected a JSON body of {\"model\": \"<m>\"?, \"yolo\": <true|false>?}, at least one";
    if !sessions::id_ok(id) {
        return crate::serve::status(StatusCode::NOT_FOUND);
    }
    let Ok(asked) = serde_json::from_slice::<Asked>(body) else { return problem(StatusCode::BAD_REQUEST, SHAPE) };
    if asked.model.is_none() && asked.yolo.is_none() {
        return problem(StatusCode::BAD_REQUEST, SHAPE);
    }
    if asked.model.as_deref().is_some_and(|model| !models::key_ok(model)) {
        return problem(StatusCode::BAD_REQUEST, "that is not a model name (letters, digits and : . _ - / @ + only)");
    }
    let (log, model, yolo) = match ctx.registry.restartable(id) {
        Restart::Ready { log, model, yolo } => (log, model, yolo),
        Restart::Running => return problem(StatusCode::CONFLICT, "a turn is running: wait for the turn to end, then switch (a restart now would cut it off)"),
        Restart::Between(_) => return problem(StatusCode::CONFLICT, "that chat is starting or stopping; try again in a moment"),
        Restart::NotOurs if ctx.registry.find_log(id).is_some() => {
            return problem(StatusCode::CONFLICT, "that chat is not running here; continue it first");
        }
        Restart::NotOurs => return crate::serve::status(StatusCode::NOT_FOUND),
    };
    let (want_model, want_yolo) = (asked.model.clone().unwrap_or_else(|| model.clone()), asked.yolo.unwrap_or(yolo));
    if want_model == model && want_yolo == yolo {
        return json(StatusCode::OK, &serde_json::json!({ "id": id, "model": model, "yolo": yolo, "restarted": false }));
    }
    // Stopping and reopening a log that ends mid-turn would carry the turn on by itself (hub.md section 4): a paid turn nobody asked for.
    if crate::fork::settled(ctx, &log, false).await != Some(true) {
        return problem(StatusCode::CONFLICT, "that chat ended mid-turn; continue it first");
    }
    // `-m` is always passed when the door told us its model: a resume with no `-m` is the config's default, not "the same".
    let keep = Some(model.as_str()).filter(|m| !m.is_empty());
    let new = Some(want_model.as_str()).filter(|m| !m.is_empty());

    if ctx.registry.stop(id).await == sessions::Stop::NotLive {
        return problem(StatusCode::CONFLICT, "that chat stopped before it could be restarted");
    }
    match bring_up(ctx, id, &log, new, want_yolo).await {
        Ok((model, yolo)) => json(StatusCode::OK, &serde_json::json!({ "id": id, "model": model, "yolo": yolo, "restarted": true })),
        Err(Failed::Held) => problem(StatusCode::CONFLICT, "that chat's log was taken by another session while it restarted"),
        Err(Failed::Spawn(why)) => {
            tracing::warn!(%why, id, "perch: a restart did not come up");
            // Back to what it was, so a typo in a model name costs a message, not the chat.
            let reverted = bring_up(ctx, id, &log, keep, yolo).await.is_ok();
            let words = if reverted {
                format!("{want_model} did not start, so the chat is back on {model}")
            } else {
                format!("{want_model} did not start, and the chat could not be brought back; continue it from the list")
            };
            json(StatusCode::SERVICE_UNAVAILABLE, &serde_json::json!({ "error": words, "reverted": reverted }))
        }
    }
}
