//! The handlers of the projects and folder-picker routes (hub.md section 7a): `GET`/`POST /hub/projects`,
//! `DELETE /hub/projects/<n>`, `GET`/`POST /hub/dirs`. The rules live in [`crate::projects`] and [`crate::dirs`]; this
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

/// `DELETE /hub/projects/<n>`: forgets entry `n` (0-based, as listed). The folder and its files are never touched.
pub fn projects_delete(ctx: &Ctx, tail: &str) -> Response<BoxBody> {
    let Some(n) = tail.parse::<usize>().ok().filter(|_| !tail.starts_with('+')) else { return status(StatusCode::NOT_FOUND) };
    match ctx.registry.projects.remove(n) {
        Ok(Some(_)) => status(StatusCode::NO_CONTENT),
        Ok(None) => status(StatusCode::NOT_FOUND),
        Err(AddError::NoHome) => problem(StatusCode::SERVICE_UNAVAILABLE, "this server has no Minerva home to keep projects in"),
        Err(_) => problem(StatusCode::SERVICE_UNAVAILABLE, "the project list could not be saved"),
    }
}

/// `GET /hub/dirs?path=<abs>`: the subfolders of one folder, or the roots with no path. Every refusal after the query
/// is [`dirs::list`]'s one empty 404; a full house of listings is a 503.
pub async fn dirs_get(ctx: &Ctx, query: &str) -> Response<BoxBody> {
    let Some(path) = dirs::parse_query(query) else { return status(StatusCode::BAD_REQUEST) };
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    if path.is_empty() {
        return json(StatusCode::OK, &dirs::roots_listing(ctx.registry.roots(), home.as_deref()));
    }
    let Some(permit) = ctx.tree_slots.take() else {
        return problem(StatusCode::SERVICE_UNAVAILABLE, "too many directory listings in flight; retry");
    };
    let roots = ctx.registry.roots().to_vec();
    let listed = tokio::task::spawn_blocking(move || {
        let _slot = permit;
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
    let made = tokio::task::spawn_blocking(move || {
        let _slot = permit;
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
