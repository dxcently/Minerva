//! The handlers of the projects and folder-picker routes (hub.md section 7a): `GET`/`POST /hub/projects`,
//! `DELETE /hub/projects`, `GET`/`POST /hub/dirs`. The rules live in [`crate::projects`] and [`crate::dirs`]; this
//! file only reads a request and writes an answer, behind the same gates as every `/hub` route.

use bytes::Bytes;
use hyper::{Response, StatusCode};

use crate::dirs::{self, MakeError};
use crate::projects::AddError;
use crate::serve::{BoxBody, Ctx, json, problem, status};

const OUTSIDE: &str = "that folder is not under the folders this server may open";

/// `GET /hub/projects`: `[{name, path, created}]`, in the order they were added.
pub fn projects_get(ctx: &Ctx) -> Response<BoxBody> {
    json(StatusCode::OK, &ctx.registry.projects.list())
}

/// `POST /hub/projects {"path": "<dir>", "name": "<label>"?}`: 201 with the entry, or 200 with the existing one when the
/// folder already is a project. The folder must be a directory under the roots and pass the picker's rules (422).
pub async fn projects_post(ctx: &Ctx, body: &Bytes) -> Response<BoxBody> {
    #[derive(serde::Deserialize)]
    struct Asked {
        path: String,
        name: Option<String>,
    }
    let Ok(asked) = serde_json::from_slice::<Asked>(body) else {
        return problem(StatusCode::BAD_REQUEST, "expected a JSON body of {\"path\": \"<dir>\", \"name\": \"<label>\"?}");
    };
    let roots = ctx.registry.roots().to_vec();
    let path = asked.path;
    let found = tokio::task::spawn_blocking(move || dirs::resolve(&roots, if path.len() > 1 { path.trim_end_matches('/') } else { &path })).await;
    let Ok(Some((real, _))) = found else { return problem(StatusCode::UNPROCESSABLE_ENTITY, OUTSIDE) };
    if ctx.registry.is_home_internal(&real) {
        return problem(StatusCode::UNPROCESSABLE_ENTITY, "the Minerva home is the server's own: a project is a folder of yours, not one inside it");
    }
    match ctx.registry.projects.add(&real, asked.name.as_deref()) {
        Ok((project, true)) => json(StatusCode::CREATED, &project),
        Ok((project, false)) => json(StatusCode::OK, &project),
        Err(AddError::NoHome) => problem(StatusCode::SERVICE_UNAVAILABLE, "this server has no Minerva home to keep projects in"),
        Err(AddError::BadName(why)) => problem(StatusCode::BAD_REQUEST, why),
        Err(AddError::Write(why)) => {
            tracing::warn!(error = %why, "perch: projects.json could not be written");
            problem(StatusCode::SERVICE_UNAVAILABLE, "the project list could not be saved")
        }
    }
}

/// `DELETE /hub/projects {"path": "<dir>"}`: forgets the project whose folder is `path`, exactly as listed. The folder and its
/// files are never touched. 404 when no project has that path (a stale page asking twice, or for the wrong one).
pub fn projects_delete(ctx: &Ctx, body: &Bytes) -> Response<BoxBody> {
    #[derive(serde::Deserialize)]
    struct Asked {
        path: String,
    }
    let Ok(asked) = serde_json::from_slice::<Asked>(body) else {
        return problem(StatusCode::BAD_REQUEST, "expected a JSON body of {\"path\": \"<dir>\"}");
    };
    match ctx.registry.projects.remove(&asked.path) {
        Ok(Some(_)) => status(StatusCode::NO_CONTENT),
        Ok(None) => status(StatusCode::NOT_FOUND),
        Err(AddError::NoHome) => problem(StatusCode::SERVICE_UNAVAILABLE, "this server has no Minerva home to keep projects in"),
        Err(_) => problem(StatusCode::SERVICE_UNAVAILABLE, "the project list could not be saved"),
    }
}

/// `POST /hub/projects/rename {"path": "<dir>", "name": "<label>"}`: gives the project whose folder is `path` a new label (200 with the
/// entry). The folder is never touched. 404 no project has that path, 400 a bad name.
pub fn projects_rename(ctx: &Ctx, body: &Bytes) -> Response<BoxBody> {
    #[derive(serde::Deserialize)]
    struct Asked {
        path: String,
        name: String,
    }
    let Ok(asked) = serde_json::from_slice::<Asked>(body) else {
        return problem(StatusCode::BAD_REQUEST, "expected a JSON body of {\"path\": \"<dir>\", \"name\": \"<label>\"}");
    };
    match ctx.registry.projects.rename(&asked.path, &asked.name) {
        Ok(Some(project)) => json(StatusCode::OK, &project),
        Ok(None) => status(StatusCode::NOT_FOUND),
        Err(AddError::NoHome) => problem(StatusCode::SERVICE_UNAVAILABLE, "this server has no Minerva home to keep projects in"),
        Err(AddError::BadName(why)) => problem(StatusCode::BAD_REQUEST, why),
        Err(AddError::Write(why)) => {
            tracing::warn!(error = %why, "perch: projects.json could not be written");
            problem(StatusCode::SERVICE_UNAVAILABLE, "the project list could not be saved")
        }
    }
}

/// `GET /hub/dirs?path=<abs>`: the subfolders of one folder, or the roots with no path. Every refusal after the query
/// is [`dirs::list`]'s one empty 404; a full house of listings is a 503.
pub async fn dirs_get(ctx: &Ctx, query: &str) -> Response<BoxBody> {
    let Some(path) = dirs::parse_query(query) else { return status(StatusCode::BAD_REQUEST) };
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    if path.is_empty() {
        let roots: Vec<_> = ctx.registry.roots().iter().filter(|root| !ctx.registry.is_home_internal(root)).cloned().collect();
        return json(StatusCode::OK, &dirs::roots_listing(&roots, home.as_deref()));
    }
    let Some(permit) = ctx.tree_slots.take() else {
        return problem(StatusCode::SERVICE_UNAVAILABLE, "too many directory listings in flight; retry");
    };
    let roots = ctx.registry.roots().to_vec();
    let registry = ctx.registry.clone();
    let listed = tokio::task::spawn_blocking(move || {
        let _slot = permit;
        // The Minerva home's own folders are not offered; `scratch/` is listed so its chat folders can be reached.
        let (real, _) = dirs::resolve(&roots, if path.len() > 1 { path.trim_end_matches('/') } else { &path })?;
        let scratch = registry.scratch.as_deref().and_then(|s| s.canonicalize().ok());
        if registry.is_home_internal(&real) && !scratch.as_deref().is_some_and(|s| real == s) {
            return None;
        }
        dirs::list(&roots, home.as_deref(), &path)
    });
    match listed.await {
        Ok(Some(listing)) => json(StatusCode::OK, &listing),
        _ => status(StatusCode::NOT_FOUND),
    }
}

/// `POST /hub/dirs {"parent": "<abs>", "name": "<one name>"}`: makes that one folder (201 `{path, name}`).
/// 400 a bad name, 422 a parent that is not a folder under the roots, 409 it exists.
pub async fn dirs_post(ctx: &Ctx, body: &Bytes) -> Response<BoxBody> {
    #[derive(serde::Deserialize)]
    struct Asked {
        parent: String,
        name: String,
    }
    let Ok(asked) = serde_json::from_slice::<Asked>(body) else {
        return problem(StatusCode::BAD_REQUEST, "expected a JSON body of {\"parent\": \"<dir>\", \"name\": \"<folder name>\"}");
    };
    let Some(permit) = ctx.tree_slots.take() else {
        return problem(StatusCode::SERVICE_UNAVAILABLE, "too many directory operations in flight; retry");
    };
    let roots = ctx.registry.roots().to_vec();
    let registry = ctx.registry.clone();
    let made = tokio::task::spawn_blocking(move || {
        let _slot = permit;
        let parent = if asked.parent.len() > 1 { asked.parent.trim_end_matches('/') } else { &asked.parent };
        if dirs::resolve(&roots, parent).is_some_and(|(real, _)| registry.is_home_internal(&real)) {
            return Err(MakeError::Outside);
        }
        dirs::make(&roots, &asked.parent, &asked.name)
    })
    .await;
    match made {
        Ok(Ok(path)) => {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            json(StatusCode::CREATED, &serde_json::json!({ "path": path.to_string_lossy(), "name": name }))
        }
        Ok(Err(MakeError::BadName(why))) => problem(StatusCode::BAD_REQUEST, why),
        Ok(Err(MakeError::Outside)) => problem(StatusCode::UNPROCESSABLE_ENTITY, OUTSIDE),
        Ok(Err(MakeError::Exists)) => problem(StatusCode::CONFLICT, "a folder with that name already exists there"),
        Ok(Err(MakeError::Io(why))) => problem(StatusCode::UNPROCESSABLE_ENTITY, &format!("the folder could not be made ({why})")),
        Err(_) => status(StatusCode::INTERNAL_SERVER_ERROR),
    }
}
