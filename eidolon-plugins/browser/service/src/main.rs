//! `eidolon-browser`: one shared headless Chromium, driven over HTTP by the
//! `browser_*` tools in `../tools/`.
//!
//! ```text
//! GET  /health   {"status": "ok", "chromium_installed": bool}   no auth, never launches
//! POST /call     {"method": "open", "args": {...}}  ->  {"ok": true, "result": ...}
//!                                                       {"ok": false, "error": "..."}
//! ```
//!
//! `/call` wants `Authorization: Bearer <token>`. The token is the file
//! `EIDOLON_BROWSER_TOKEN_FILE` (default `~/.config/eidolon/browser.token`),
//! made on first run; the tools name the same file. Listens on
//! `127.0.0.1:$EIDOLON_SERVICE_PORT` (default 8090). Chromium comes from
//! `EIDOLON_BROWSER_CHROME`, else the first of chromium / google-chrome on
//! PATH, and is started on the first call that needs it, not before.

mod browser;
mod filter;
mod origin;
mod proxy;
mod snapshot;
mod sweep;
mod text;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State as Extract;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Json;
use axum::routing::{get, post};
use serde_json::{Value, json};
use subtle::ConstantTimeEq;
use tokio::sync::Mutex;

use crate::browser::{State, truthy};
use crate::text::repr;

const METHODS: [&str; 6] = ["open", "snapshot", "click", "type", "read", "back"];

struct App {
    /// Every `/call` takes this, first come first served, for its whole run.
    state: Mutex<State>,
    expected: Vec<u8>,
    chrome: Option<PathBuf>,
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("eidolon-browser: {e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), String> {
    let port: u16 = match std::env::var("EIDOLON_SERVICE_PORT") {
        Ok(p) => p
            .parse()
            .map_err(|_| format!("EIDOLON_SERVICE_PORT is not a port: {p:?}"))?,
        Err(_) => 8090,
    };
    let token = match std::env::var("EIDOLON_SERVICE_TOKEN") {
        Ok(t) if !t.is_empty() => t,
        _ => load_token(&token_file())?,
    };

    let swept = sweep::sweep(
        &std::env::temp_dir(),
        Duration::from_secs(60),
        SystemTime::now(),
    );
    if !swept.is_empty() {
        println!(
            "swept {} profile dir(s) left by an earlier crash: {}",
            swept.len(),
            swept.join(", ")
        );
    }

    let chrome = find_chrome();
    let proxy = proxy::Proxy::start()
        .await
        .map_err(|e| format!("could not start the confine proxy: {e}"))?;
    let app = Arc::new(App {
        state: Mutex::new(State::new(chrome.clone(), proxy)),
        expected: format!("Bearer {token}").into_bytes(),
        chrome: chrome.clone(),
    });

    let router = Router::new()
        .route("/health", get(health))
        .route("/call", post(call))
        .with_state(app.clone());
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|e| format!("could not listen on 127.0.0.1:{port}: {e}"))?;
    println!(
        "browser service on http://127.0.0.1:{port} (chromium: {})",
        chrome
            .as_deref()
            .map_or("not found".into(), |p| p.display().to_string())
    );
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown())
        .await
        .map_err(|e| e.to_string())?;
    app.state.lock().await.close().await;
    Ok(())
}

async fn health(Extract(app): Extract<Arc<App>>) -> Json<Value> {
    Json(json!({
        "status": "ok",
        "chromium_installed": app.chrome.as_deref().is_some_and(Path::is_file),
    }))
}

async fn call(
    Extract(app): Extract<Arc<App>>,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, Json<Value>) {
    let supplied = headers
        .get("authorization")
        .map_or(&[][..], |v| v.as_bytes());
    if !bool::from(supplied.ct_eq(&app.expected)) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "ok": false, "error": "bad token" })),
        );
    }
    let body: Value = match serde_json::from_slice(&body) {
        Ok(v @ Value::Object(_)) => v,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "ok": false, "error": "invalid JSON body" })),
            );
        }
    };
    let method = body.get("method").cloned().unwrap_or(Value::Null);
    let Some(method) = method.as_str().filter(|m| METHODS.contains(m)) else {
        return (
            StatusCode::OK,
            Json(json!({ "ok": false, "error": format!("unknown method {}", repr(&method)) })),
        );
    };
    let args = match body.get("args") {
        Some(a @ Value::Object(_)) => a.clone(),
        a if !truthy(a) => json!({}),
        Some(_) => {
            return (
                StatusCode::OK,
                Json(json!({ "ok": false, "error": "args must be an object" })),
            );
        }
        None => json!({}),
    };
    let result = app.state.lock().await.call(method, &args).await;
    let reply = match result {
        Ok(v) => json!({ "ok": true, "result": v }),
        Err(e) => json!({ "ok": false, "error": e }),
    };
    (StatusCode::OK, Json(reply))
}

fn token_file() -> PathBuf {
    if let Some(p) = std::env::var_os("EIDOLON_BROWSER_TOKEN_FILE") {
        return PathBuf::from(p);
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(".config/eidolon/browser.token")
}

/// The token in `path`, made there (mode 0600) if it isn't yet.
fn load_token(path: &Path) -> Result<String, String> {
    use std::io::{Read, Write};
    if let Ok(t) = std::fs::read_to_string(path) {
        let t = t.trim();
        if !t.is_empty() {
            return Ok(t.to_string());
        }
    }
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|e| format!("could not read /dev/urandom: {e}"))?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    opts.open(path)
        .and_then(|mut f| f.write_all(token.as_bytes()))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    println!("made a new token at {}", path.display());
    Ok(token)
}

fn find_chrome() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("EIDOLON_BROWSER_CHROME") {
        return Some(PathBuf::from(p));
    }
    let path = std::env::var_os("PATH")?;
    for name in [
        "chromium",
        "chromium-browser",
        "google-chrome-stable",
        "google-chrome",
        "chrome",
    ] {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

async fn shutdown() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler");
        tokio::select! {
            _ = ctrl_c => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = ctrl_c.await;
}
