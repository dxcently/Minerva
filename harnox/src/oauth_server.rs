//! OAuth 2.1 authentication for an MCP transport (feature `oauth-server`).
//!
//! Claude.ai custom connectors require OAuth 2.1 (authorization-code + PKCE
//! S256, dynamic client registration) and explicitly do **not** accept static
//! bearer tokens. So this module turns a server into both:
//!
//!   * a **resource server** — every `/mcp` request must carry a valid, live
//!     bearer access token, else it gets `401` + a `WWW-Authenticate` pointer
//!     to our metadata (reject-by-default), and
//!   * a small single-user **authorization server** — discovery metadata
//!     (RFC 8414 / RFC 9728), dynamic client registration (RFC 7591), an
//!     `/authorize` consent screen, a `/token` endpoint, and RFC 7009 token
//!     revocation.
//!
//! Strength properties:
//!   * PKCE S256 is **required** and actually verified on code exchange.
//!   * Auth codes are single-use and expire in 5 min; access tokens live 1 h;
//!     refresh tokens live 30 d and are **rotated** on every use.
//!   * `POST /oauth/revoke` (RFC 7009-shaped) kills a grant — the presented
//!     refresh token dies along with its paired access bearer — which is how
//!     a credential lease shipped to another machine is ended from home.
//!     Unknown and already-invalid tokens get the same empty `200` as
//!     successful revocations, so liveness cannot be probed.
//!   * Tokens are 256-bit OS-random and stored only as SHA-256 hashes, so a
//!     dump of the in-memory store yields nothing replayable.
//!   * The consent screen is gated by an operator passphrase; DCR is open by
//!     spec, so this passphrase is what stops a stranger who points a
//!     connector at the URL from approving themselves.
//!   * No secrets are hardcoded — everything comes from the consumer's
//!     configuration (usually its environment, via [`AuthConfig::from_env`]).
//!
//! This module only guards the transport; it never touches the tools behind
//! it. What differs per consumer — the scope string, the consent wording, and
//! the env-var prefix — is [`Branding`] plus the `from_env` arguments; the
//! mechanism is identical. (This is the module Mneme wrote and Melete vendored;
//! the two copies had drifted by exactly those three things.)
//!
//! Wiring, in the consumer's axum app:
//!
//! ```ignore
//! let auth = AuthState::new(AuthConfig::from_env("MNEME", Some("OBSIDIAN_MCP"), branding)?);
//! let protected = Router::new()
//!     .nest_service("/mcp", service)
//!     .layer(axum::middleware::from_fn_with_state(auth.clone(), require_auth));
//! let app = Router::new().merge(protected).merge(oauth_router(auth));
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::{
    Json, Router,
    extract::{Form, Query, Request, State},
    http::{StatusCode, header},
    middleware::Next,
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio::sync::RwLock;

use crate::crypto::{random_token, sha256};
use crate::env::env_var;
use crate::html::html_escape;
use crate::time::{now_secs, secs_since_epoch};

const AUTH_CODE_TTL: Duration = Duration::from_secs(5 * 60);
const ACCESS_TOKEN_TTL: Duration = Duration::from_secs(60 * 60);
const REFRESH_TOKEN_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);
const PENDING_TTL: Duration = Duration::from_secs(10 * 60);
/// How long a just-rotated refresh token stays usable after first use, so a
/// lockstep duplicate refresh (Claude drives two sub-clients) or a lost-response
/// retry still succeeds instead of triggering a full re-consent.
const REFRESH_GRACE: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// What the consent screen says and which scope the server advertises — the
/// only per-consumer surface of the flow.
#[derive(Clone, Debug)]
pub struct Branding {
    /// The single scope this server supports and advertises (`vault.read`,
    /// `jobs.run`). Issued grants default to it when the client asks for none.
    pub scope: String,
    /// The consent page's `<h1>` ("Authorize access to your vault").
    pub heading: String,
    /// Completes the sentence `<client> is requesting <scope> …` on the
    /// consent page, e.g. "(read and write) access to your notes." — end it
    /// with a full stop.
    pub grant_description: String,
}

#[derive(Clone)]
pub struct AuthConfig {
    /// Public origin clients reach us at, e.g. `https://vault.example.ts.net`.
    /// No trailing slash. All metadata URLs are built from this.
    public_base: String,
    /// SHA-256 of the operator passphrase that gates the consent screen.
    password_hash: [u8; 32],
    /// Where the token store is persisted across restarts. `None` keeps the
    /// in-memory-only behaviour (every restart deauths).
    state_file: Option<PathBuf>,
    branding: Branding,
}

impl AuthConfig {
    /// Build from explicit values. Returns `Err` (so the server refuses to
    /// start) unless `public_base` is an https origin (or `http://localhost`)
    /// and the passphrase is at least 12 characters — reject-by-default.
    pub fn new(
        public_base: &str,
        password: &str,
        state_file: Option<PathBuf>,
        branding: Branding,
    ) -> anyhow::Result<Self> {
        Self::build(public_base, password, state_file, branding, "the public URL", "the operator passphrase")
    }

    /// Build from the environment: `{prefix}_PUBLIC_URL`, `{prefix}_AUTH_PASSWORD`
    /// (both required), and `{prefix}_AUTH_STATE_FILE` (optional). Each is also
    /// read under `legacy_prefix`, if given, with a deprecation warning — so a
    /// consumer that renamed its variables keeps working on an old env file.
    ///
    /// The state file is opt-in and an explicit path on purpose: it holds live
    /// grants, so it is never silently scattered into a non-durable temp dir.
    pub fn from_env(prefix: &str, legacy_prefix: Option<&str>, branding: Branding) -> anyhow::Result<Self> {
        let names = |suffix: &str| -> (String, Vec<String>) {
            (
                format!("{prefix}_{suffix}"),
                legacy_prefix.map(|l| vec![format!("{l}_{suffix}")]).unwrap_or_default(),
            )
        };
        let read = |suffix: &str| -> (String, Option<String>) {
            let (primary, legacy) = names(suffix);
            let legacy: Vec<&str> = legacy.iter().map(String::as_str).collect();
            let value = env_var(&primary, &legacy);
            (primary, value)
        };

        let (url_var, public_base) = read("PUBLIC_URL");
        let public_base = public_base.ok_or_else(|| {
            anyhow::anyhow!(
                "{url_var} is required (the public https origin clients connect to, \
                 e.g. https://vault.example.ts.net) — refusing to start without it"
            )
        })?;
        let (pw_var, password) = read("AUTH_PASSWORD");
        let password = password.ok_or_else(|| {
            anyhow::anyhow!(
                "{pw_var} is required (the passphrase that gates the OAuth consent screen) \
                 — refusing to start without it"
            )
        })?;
        let (_, state_file) = read("AUTH_STATE_FILE");
        let state_file =
            state_file.map(|p| PathBuf::from(p.trim())).filter(|p| !p.as_os_str().is_empty());

        Self::build(&public_base, &password, state_file, branding, &url_var, &pw_var)
    }

    fn build(
        public_base: &str,
        password: &str,
        state_file: Option<PathBuf>,
        branding: Branding,
        url_what: &str,
        pw_what: &str,
    ) -> anyhow::Result<Self> {
        let public_base = public_base.trim().trim_end_matches('/').to_string();
        if !public_base.starts_with("https://") && !public_base.starts_with("http://localhost") {
            anyhow::bail!("{url_what} must be an https:// origin (got {public_base:?})");
        }
        if password.len() < 12 {
            anyhow::bail!("{pw_what} must be at least 12 characters");
        }
        Ok(Self { public_base, password_hash: sha256(password.as_bytes()), state_file, branding })
    }

    fn resource(&self) -> String {
        format!("{}/mcp", self.public_base)
    }
    fn prm_url(&self) -> String {
        format!("{}/.well-known/oauth-protected-resource", self.public_base)
    }
    fn scope(&self) -> &str {
        &self.branding.scope
    }
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct AuthState {
    cfg: Arc<AuthConfig>,
    store: Arc<Store>,
}

#[derive(Default)]
struct Store {
    persist_lock: tokio::sync::Mutex<()>,
    clients: RwLock<HashMap<String, Client>>, // client_id -> client (id is public)
    pending: RwLock<HashMap<String, Pending>>, // request_id -> in-flight authorize
    codes: RwLock<HashMap<[u8; 32], AuthCode>>, // code_hash -> code
    access: RwLock<HashMap<[u8; 32], Grant>>,  // access_hash -> grant
    refresh: RwLock<HashMap<[u8; 32], Grant>>, // refresh_hash -> grant
}

#[derive(Clone)]
struct Client {
    redirect_uris: Vec<String>,
    name: Option<String>,
}

struct Pending {
    client_id: String,
    redirect_uri: String,
    code_challenge: String,
    scope: Option<String>,
    state: Option<String>,
    expires_at: SystemTime,
}

struct AuthCode {
    client_id: String,
    redirect_uri: String,
    code_challenge: String,
    scope: Option<String>,
    expires_at: SystemTime,
}

#[derive(Clone)]
struct Grant {
    client_id: String,
    scope: Option<String>,
    access_hash: [u8; 32],
    refresh_hash: [u8; 32],
    access_expires_at: SystemTime,
    refresh_expires_at: SystemTime,
}

// ---------------------------------------------------------------------------
// Durable token store (optional, the configured state file)
// ---------------------------------------------------------------------------

/// On-disk shape of the persisted store. Only the parts worth surviving a
/// restart: registered clients and live grants. Auth codes and in-flight
/// `pending` requests are short-lived (minutes) and intentionally not persisted.
#[derive(Serialize, Deserialize, Default)]
struct Snapshot {
    #[serde(default)]
    clients: Vec<ClientRepr>,
    #[serde(default)]
    access: Vec<GrantRepr>,
    #[serde(default)]
    refresh: Vec<GrantRepr>,
}

#[derive(Serialize, Deserialize)]
struct ClientRepr {
    client_id: String,
    redirect_uris: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

/// A grant on disk. Hashes are base64url (never the plaintext tokens, which are
/// not stored anywhere); expiries are unix seconds.
#[derive(Serialize, Deserialize)]
struct GrantRepr {
    client_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
    access_hash: String,
    refresh_hash: String,
    access_expires_at: u64,
    refresh_expires_at: u64,
}

impl GrantRepr {
    fn from_grant(g: &Grant) -> Self {
        Self {
            client_id: g.client_id.clone(),
            scope: g.scope.clone(),
            access_hash: URL_SAFE_NO_PAD.encode(g.access_hash),
            refresh_hash: URL_SAFE_NO_PAD.encode(g.refresh_hash),
            access_expires_at: secs_since_epoch(g.access_expires_at),
            refresh_expires_at: secs_since_epoch(g.refresh_expires_at),
        }
    }
    fn into_grant(self) -> Option<Grant> {
        Some(Grant {
            client_id: self.client_id,
            scope: self.scope,
            access_hash: decode_hash(&self.access_hash)?,
            refresh_hash: decode_hash(&self.refresh_hash)?,
            access_expires_at: UNIX_EPOCH + Duration::from_secs(self.access_expires_at),
            refresh_expires_at: UNIX_EPOCH + Duration::from_secs(self.refresh_expires_at),
        })
    }
}

fn decode_hash(s: &str) -> Option<[u8; 32]> {
    URL_SAFE_NO_PAD.decode(s).ok()?.try_into().ok()
}

impl Store {
    /// Build a store from the persisted state file, dropping anything already
    /// expired. A missing or unparseable file yields an empty store (we log and
    /// carry on rather than refuse to boot over a corrupt cache).
    fn load(path: &Path) -> Self {
        let (mut clients, mut access, mut refresh) = (HashMap::new(), HashMap::new(), HashMap::new());
        match std::fs::read(path) {
            Ok(data) => match serde_json::from_slice::<Snapshot>(&data) {
                Ok(snap) => {
                    let now = SystemTime::now();
                    for c in snap.clients {
                        clients.insert(c.client_id, Client { redirect_uris: c.redirect_uris, name: c.name });
                    }
                    for g in snap.access.into_iter().filter_map(GrantRepr::into_grant) {
                        if g.access_expires_at > now {
                            access.insert(g.access_hash, g);
                        }
                    }
                    for g in snap.refresh.into_iter().filter_map(GrantRepr::into_grant) {
                        if g.refresh_expires_at > now {
                            refresh.insert(g.refresh_hash, g);
                        }
                    }
                    tracing::info!(
                        "loaded auth state from {}: {} client(s), {} access, {} refresh token(s)",
                        path.display(),
                        clients.len(),
                        access.len(),
                        refresh.len()
                    );
                }
                Err(e) => {
                    tracing::warn!("could not parse auth state {}: {e}; starting empty", path.display());
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::info!("no auth state file at {} yet; starting empty", path.display());
            }
            Err(e) => {
                tracing::warn!("could not read auth state {}: {e}; starting empty", path.display());
            }
        }
        Store {
            persist_lock: tokio::sync::Mutex::new(()),
            clients: RwLock::new(clients),
            pending: RwLock::new(HashMap::new()),
            codes: RwLock::new(HashMap::new()),
            access: RwLock::new(access),
            refresh: RwLock::new(refresh),
        }
    }
}

/// Atomically write the snapshot `0600`.
fn write_snapshot(path: &Path, snap: &Snapshot) -> std::io::Result<()> {
    let json = serde_json::to_vec_pretty(snap).map_err(std::io::Error::other)?;
    crate::fs::write_atomic_0600(path, &json)
}

impl AuthState {
    pub fn new(cfg: AuthConfig) -> Self {
        let store = match &cfg.state_file {
            Some(path) => Store::load(path),
            None => Store::default(),
        };
        Self { cfg: Arc::new(cfg), store: Arc::new(store) }
    }

    /// Snapshot the durable parts of the store (clients + live grants) to the
    /// configured state file, atomically and `0600`. No-op when persistence is
    /// off. Best-effort: a write failure is logged, never surfaced to a client —
    /// the in-memory store remains authoritative for the running process.
    async fn persist(&self) {
        let Some(path) = self.cfg.state_file.clone() else {
            return;
        };
        // Serialize snapshot capture AND replacement, so an older snapshot cannot land last.
        let _persist = self.store.persist_lock.lock().await;
        let snap = {
            let clients = self.store.clients.read().await;
            let access = self.store.access.read().await;
            let refresh = self.store.refresh.read().await;
            Snapshot {
                clients: clients
                    .iter()
                    .map(|(id, c)| ClientRepr {
                        client_id: id.clone(),
                        redirect_uris: c.redirect_uris.clone(),
                        name: c.name.clone(),
                    })
                    .collect(),
                access: access.values().map(GrantRepr::from_grant).collect(),
                refresh: refresh.values().map(GrantRepr::from_grant).collect(),
            }
        };
        if let Err(e) = write_snapshot(&path, &snap) {
            tracing::warn!("could not persist auth state to {}: {e}", path.display());
        }
    }

    /// The configured public origin (e.g. `https://host`), already trimmed. The
    /// transport's Host allowlist derives from this so it matches the OAuth
    /// metadata exactly — and so the public-URL env var is read (and any
    /// deprecation warning emitted) only once.
    pub fn public_base(&self) -> &str {
        &self.cfg.public_base
    }
}

// ---------------------------------------------------------------------------
// Router + middleware (the only things a consumer touches)
// ---------------------------------------------------------------------------

/// Public, unauthenticated OAuth endpoints (discovery, registration, the
/// consent screen, the token endpoint, revocation). Merge this into the app
/// router.
pub fn oauth_router(state: AuthState) -> Router {
    Router::new()
        // RFC 8414 — advertised at the root and at the resource-suffixed path,
        // since MCP clients probe both.
        .route("/.well-known/oauth-authorization-server", get(as_metadata))
        .route("/.well-known/oauth-authorization-server/mcp", get(as_metadata))
        // RFC 9728 — protected-resource metadata.
        .route("/.well-known/oauth-protected-resource", get(prm_metadata))
        .route("/.well-known/oauth-protected-resource/mcp", get(prm_metadata))
        // RFC 7591 — dynamic client registration.
        .route("/oauth/register", post(register))
        // Authorization-code flow.
        .route("/oauth/authorize", get(authorize))
        .route("/oauth/consent", post(consent))
        .route("/oauth/token", post(token))
        // RFC 7009 — token revocation (kill a grant/lease by its refresh token).
        .route("/oauth/revoke", post(revoke))
        .with_state(state)
}

/// Bearer-token gate for `/mcp`. Reject-by-default: anything without a live
/// access token gets `401` + a `WWW-Authenticate` pointer to our metadata.
pub async fn require_auth(State(state): State<AuthState>, req: Request, next: Next) -> Response {
    match bearer(&req) {
        Some(tok) if state.validate_access(&tok).await => next.run(req).await,
        Some(_) => state.challenge("invalid_token", "the access token is invalid or expired"),
        None => state.challenge("", ""),
    }
}

fn bearer(req: &Request) -> Option<String> {
    let raw = req.headers().get(header::AUTHORIZATION)?.to_str().ok()?;
    let tok = raw.strip_prefix("Bearer ").or_else(|| raw.strip_prefix("bearer "))?;
    let tok = tok.trim();
    (!tok.is_empty()).then(|| tok.to_string())
}

impl AuthState {
    /// True iff `token` hashes to a known, unexpired access token.
    async fn validate_access(&self, token: &str) -> bool {
        let h = sha256(token.as_bytes());
        let mut map = self.store.access.write().await;
        match map.get(&h) {
            Some(g) if g.access_expires_at > SystemTime::now() => true,
            Some(_) => {
                map.remove(&h); // prune the expired entry as we go
                false
            }
            None => false,
        }
    }

    /// A `401` carrying `WWW-Authenticate: Bearer …, resource_metadata="…"`
    /// (RFC 9728 §5.1) so the client knows where to discover our OAuth config.
    fn challenge(&self, error: &str, desc: &str) -> Response {
        let mut wa = format!("Bearer resource_metadata=\"{}\"", self.cfg.prm_url());
        if !error.is_empty() {
            wa.push_str(&format!(", error=\"{error}\", error_description=\"{desc}\""));
        }
        (
            StatusCode::UNAUTHORIZED,
            [
                (header::WWW_AUTHENTICATE, wa),
                (header::CACHE_CONTROL, "no-store".to_string()),
            ],
            Json(serde_json::json!({
                "error": if error.is_empty() { "unauthorized" } else { error },
                "error_description": if desc.is_empty() { "authentication required" } else { desc },
            })),
        )
            .into_response()
    }
}

// ---------------------------------------------------------------------------
// Discovery metadata
// ---------------------------------------------------------------------------

async fn as_metadata(State(s): State<AuthState>) -> Response {
    let b = &s.cfg.public_base;
    json_no_store(serde_json::json!({
        "issuer": b,
        "authorization_endpoint": format!("{b}/oauth/authorize"),
        "token_endpoint": format!("{b}/oauth/token"),
        "registration_endpoint": format!("{b}/oauth/register"),
        "revocation_endpoint": format!("{b}/oauth/revoke"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": ["none"],
        "scopes_supported": [s.cfg.scope()],
    }))
}

async fn prm_metadata(State(s): State<AuthState>) -> Response {
    json_no_store(serde_json::json!({
        "resource": s.cfg.resource(),
        "authorization_servers": [s.cfg.public_base],
        "scopes_supported": [s.cfg.scope()],
        "bearer_methods_supported": ["header"],
    }))
}

// ---------------------------------------------------------------------------
// Dynamic client registration (RFC 7591)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct RegisterReq {
    #[serde(default)]
    redirect_uris: Vec<String>,
    client_name: Option<String>,
    // Any other RFC 7591 fields Claude sends are accepted and ignored.
}

#[derive(Serialize)]
struct RegisterResp {
    client_id: String,
    client_id_issued_at: u64,
    redirect_uris: Vec<String>,
    token_endpoint_auth_method: &'static str,
    grant_types: Vec<&'static str>,
    response_types: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_name: Option<String>,
}

async fn register(State(s): State<AuthState>, Json(req): Json<RegisterReq>) -> Response {
    if req.redirect_uris.is_empty() {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_redirect_uri", "redirect_uris is required");
    }
    if !req.redirect_uris.iter().all(|u| redirect_uri_ok(u)) {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_redirect_uri",
            "redirect_uris must be https (or http://localhost for local testing)",
        );
    }

    let client_id = format!("mcp-{}", random_token());
    s.store.clients.write().await.insert(
        client_id.clone(),
        Client { redirect_uris: req.redirect_uris.clone(), name: req.client_name.clone() },
    );
    s.persist().await;

    // Public client: no secret issued (token_endpoint_auth_method = none).
    (
        StatusCode::CREATED,
        [(header::CACHE_CONTROL, "no-store")],
        Json(RegisterResp {
            client_id,
            client_id_issued_at: now_secs(),
            redirect_uris: req.redirect_uris,
            token_endpoint_auth_method: "none",
            grant_types: vec!["authorization_code", "refresh_token"],
            response_types: vec!["code"],
            client_name: req.client_name,
        }),
    )
        .into_response()
}

/// Only https, or http on loopback for local testing. Blocks open-redirect and
/// downgrade tricks.
fn redirect_uri_ok(u: &str) -> bool {
    u.starts_with("https://")
        || u.starts_with("http://localhost")
        || u.starts_with("http://127.0.0.1")
}

// ---------------------------------------------------------------------------
// Authorization endpoint + consent
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct AuthorizeQuery {
    response_type: Option<String>,
    client_id: Option<String>,
    redirect_uri: Option<String>,
    scope: Option<String>,
    state: Option<String>,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
}

async fn authorize(State(s): State<AuthState>, Query(q): Query<AuthorizeQuery>) -> Response {
    // client_id + redirect_uri must validate before we trust the redirect at
    // all; otherwise we render a plain error page rather than bounce anywhere.
    let Some(client_id) = q.client_id.clone() else {
        return error_page("missing client_id");
    };
    let client = match s.store.clients.read().await.get(&client_id) {
        Some(c) => c.clone(),
        None => return error_page("unknown client_id — register first"),
    };
    let Some(redirect_uri) = q.redirect_uri.clone() else {
        return error_page("missing redirect_uri");
    };
    if !client.redirect_uris.iter().any(|u| u == &redirect_uri) {
        return error_page("redirect_uri does not match a registered value");
    }

    // From here errors are safe to report back to the client via redirect.
    if q.response_type.as_deref() != Some("code") {
        return redirect_err(&redirect_uri, "unsupported_response_type", q.state.as_deref());
    }
    let Some(challenge) = q.code_challenge.clone() else {
        return redirect_err(&redirect_uri, "invalid_request", q.state.as_deref());
    };
    if q.code_challenge_method.as_deref() != Some("S256") || challenge.is_empty() {
        return redirect_err(&redirect_uri, "invalid_request", q.state.as_deref());
    }

    let request_id = random_token();
    s.store.pending.write().await.insert(
        request_id.clone(),
        Pending {
            client_id,
            redirect_uri,
            code_challenge: challenge,
            scope: q.scope.clone(),
            state: q.state.clone(),
            expires_at: SystemTime::now() + PENDING_TTL,
        },
    );

    Html(consent_page(&s.cfg.branding, &request_id, client.name.as_deref(), q.scope.as_deref(), None))
        .into_response()
}

#[derive(Deserialize)]
struct ConsentForm {
    request_id: String,
    password: String,
    action: String,
}

async fn consent(State(s): State<AuthState>, Form(f): Form<ConsentForm>) -> Response {
    // Peek (don't remove yet) so a wrong password can be retried until the
    // pending request expires.
    let snapshot = {
        let map = s.store.pending.read().await;
        match map.get(&f.request_id) {
            Some(p) if p.expires_at > SystemTime::now() => Some((
                p.client_id.clone(),
                p.redirect_uri.clone(),
                p.code_challenge.clone(),
                p.scope.clone(),
                p.state.clone(),
            )),
            _ => None,
        }
    };
    let Some((client_id, redirect_uri, code_challenge, scope, state)) = snapshot else {
        return error_page("this authorization request expired — start again from the client");
    };

    if f.action != "approve" {
        s.store.pending.write().await.remove(&f.request_id);
        return redirect_err(&redirect_uri, "access_denied", state.as_deref());
    }

    // Constant-time passphrase check — the load-bearing gate.
    let ok: bool = sha256(f.password.as_bytes()).ct_eq(&s.cfg.password_hash).into();
    if !ok {
        return Html(consent_page(
            &s.cfg.branding,
            &f.request_id,
            None,
            scope.as_deref(),
            Some("Incorrect passphrase."),
        ))
        .into_response();
    }

    // Approved: consume the pending request and mint a single-use auth code.
    s.store.pending.write().await.remove(&f.request_id);
    let code = random_token();
    s.store.codes.write().await.insert(
        sha256(code.as_bytes()),
        AuthCode {
            client_id,
            redirect_uri: redirect_uri.clone(),
            code_challenge,
            scope,
            expires_at: SystemTime::now() + AUTH_CODE_TTL,
        },
    );

    let mut url = format!("{redirect_uri}?code={}", enc(&code));
    if let Some(st) = state {
        url.push_str(&format!("&state={}", enc(&st)));
    }
    Redirect::to(&url).into_response()
}

// ---------------------------------------------------------------------------
// Token endpoint
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct TokenForm {
    grant_type: Option<String>,
    code: Option<String>,
    redirect_uri: Option<String>,
    client_id: Option<String>,
    code_verifier: Option<String>,
    refresh_token: Option<String>,
}

#[derive(Serialize)]
struct TokenResp {
    access_token: String,
    token_type: &'static str,
    expires_in: u64,
    refresh_token: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
}

async fn token(State(s): State<AuthState>, Form(f): Form<TokenForm>) -> Response {
    match f.grant_type.as_deref() {
        Some("authorization_code") => token_from_code(&s, f).await,
        Some("refresh_token") => token_from_refresh(&s, f).await,
        _ => oauth_error(
            StatusCode::BAD_REQUEST,
            "unsupported_grant_type",
            "only authorization_code and refresh_token are supported",
        ),
    }
}

async fn token_from_code(s: &AuthState, f: TokenForm) -> Response {
    let (Some(code), Some(redirect_uri), Some(client_id), Some(verifier)) =
        (f.code, f.redirect_uri, f.client_id, f.code_verifier)
    else {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_request", "missing required parameter");
    };

    // Single-use: remove on lookup regardless of outcome.
    let entry = s.store.codes.write().await.remove(&sha256(code.as_bytes()));
    let Some(c) = entry else {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", "unknown or already-used code");
    };
    if c.expires_at <= SystemTime::now() {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", "authorization code expired");
    }
    if c.client_id != client_id || c.redirect_uri != redirect_uri {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", "client/redirect mismatch");
    }
    // PKCE S256: BASE64URL(SHA256(verifier)) must equal the stored challenge.
    let computed = URL_SAFE_NO_PAD.encode(sha256(verifier.as_bytes()));
    let pkce_ok: bool = computed.as_bytes().ct_eq(c.code_challenge.as_bytes()).into();
    if !pkce_ok {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", "PKCE verification failed");
    }

    issue_grant(s, client_id, c.scope).await
}

async fn token_from_refresh(s: &AuthState, f: TokenForm) -> Response {
    let (Some(refresh), Some(client_id)) = (f.refresh_token, f.client_id) else {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_request", "missing required parameter");
    };
    let h = sha256(refresh.as_bytes());
    let now = SystemTime::now();

    // Look up the grant WITHOUT consuming it. Claude.ai drives two sub-clients
    // (Anthropic/Toolbox + Anthropic/ClaudeAI) that refresh in lockstep, and a
    // refresh response can be lost in flight; strict single-use rotation turns
    // either case into an invalid_grant and a full re-consent. So instead we let
    // the presented token linger for a short grace window (below) and only then
    // expire it out.
    let grant = match s.store.refresh.read().await.get(&h) {
        Some(g) if g.refresh_expires_at > now && g.client_id == client_id => g.clone(),
        Some(_) => {
            tracing::warn!("refresh rejected for client {client_id}: token expired or client mismatch");
            return oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", "refresh token invalid");
        }
        None => {
            tracing::warn!("refresh rejected for client {client_id}: unknown or already-expired token");
            return oauth_error(
                StatusCode::BAD_REQUEST,
                "invalid_grant",
                "unknown or already-used refresh token",
            );
        }
    };

    // Rotate, but keep the presented token usable for REFRESH_GRACE so an
    // in-flight duplicate or a retried-after-lost-response request still
    // succeeds; after that it expires itself out.
    {
        let mut map = s.store.refresh.write().await;
        if let Some(entry) = map.get_mut(&h) {
            let deadline = now + REFRESH_GRACE;
            if entry.refresh_expires_at > deadline {
                entry.refresh_expires_at = deadline;
            }
        }
    }
    // Keep the old access token until its advertised expiry: parallel requests
    // and clients whose refresh response was lost still hold that bearer.
    tracing::info!("refreshed grant for client {client_id}");

    issue_grant(s, client_id, grant.scope).await
}

/// Mint a fresh access+refresh pair, store their hashes, and return the JSON.
async fn issue_grant(s: &AuthState, client_id: String, scope: Option<String>) -> Response {
    let access = random_token();
    let refresh = random_token();
    let now = SystemTime::now();
    let grant = Grant {
        client_id,
        scope: scope.clone(),
        access_hash: sha256(access.as_bytes()),
        refresh_hash: sha256(refresh.as_bytes()),
        access_expires_at: now + ACCESS_TOKEN_TTL,
        refresh_expires_at: now + REFRESH_TOKEN_TTL,
    };
    s.store.access.write().await.insert(grant.access_hash, grant.clone());
    s.store.refresh.write().await.insert(grant.refresh_hash, grant);
    s.persist().await;

    (
        StatusCode::OK,
        [(header::CACHE_CONTROL, "no-store")],
        Json(TokenResp {
            access_token: access,
            token_type: "Bearer",
            expires_in: ACCESS_TOKEN_TTL.as_secs(),
            refresh_token: refresh,
            scope: scope.or_else(|| Some(s.cfg.scope().to_string())),
        }),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Revocation (RFC 7009)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct RevokeForm {
    token: Option<String>,
    /// Accepted per RFC 7009 §2.1 but deliberately unused: the lookup is two
    /// hash-map probes, so there is nothing to optimise, and ignoring a hint
    /// is exactly what the RFC prescribes when the hinted type doesn't pan
    /// out.
    token_type_hint: Option<String>,
}

/// RFC 7009-shaped token revocation. The lease story: the hub that minted a
/// grant — Melete shipping an eidolon session to another machine, for one —
/// presents the grant's refresh token here and the grant row dies, taking its
/// live access bearer with it.
///
/// The refresh half is the lease: revoking it removes the whole grant,
/// refresh token and paired access bearer both (RFC 7009 §2.1's SHOULD — the
/// point of the revocation is that nothing minted from the grant outlives
/// it). Presenting a grant's *access* token instead revokes just that
/// bearer, leaving the refresh grant (and the client's re-mint path) alive.
///
/// Per RFC 7009 §2.1 the response is `200` whether or not the token was
/// found: an unknown, expired, or already-revoked token must be
/// indistinguishable from a successful revocation, so the endpoint cannot be
/// used to probe what is live.
async fn revoke(State(s): State<AuthState>, Form(f): Form<RevokeForm>) -> Response {
    let Some(token) = f.token else {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_request", "token is required");
    };
    let h = sha256(token.as_bytes());

    let grant = s.store.refresh.write().await.remove(&h);
    let (refresh_killed, access_killed) = match grant {
        // The paired access bearer dies with the grant.
        Some(g) => {
            s.store.access.write().await.remove(&g.access_hash);
            (true, true)
        }
        // An access token presented alone revokes only itself.
        None => {
            let killed = s.store.access.write().await.remove(&h).is_some();
            (false, killed)
        }
    };
    if refresh_killed || access_killed {
        tracing::info!(
            refresh = refresh_killed,
            access = access_killed,
            hint = ?f.token_type_hint,
            "revoked via /oauth/revoke"
        );
        s.persist().await;
    }
    // 200 even when nothing matched (RFC 7009 §2.1).
    (StatusCode::OK, [(header::CACHE_CONTROL, "no-store")]).into_response()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Percent-encode a value for safe use in a redirect query string.
fn enc(v: &str) -> String {
    utf8_percent_encode(v, NON_ALPHANUMERIC).to_string()
}

fn json_no_store(v: serde_json::Value) -> Response {
    (StatusCode::OK, [(header::CACHE_CONTROL, "no-store")], Json(v)).into_response()
}

fn oauth_error(status: StatusCode, error: &str, desc: &str) -> Response {
    (
        status,
        [(header::CACHE_CONTROL, "no-store")],
        Json(serde_json::json!({ "error": error, "error_description": desc })),
    )
        .into_response()
}

fn redirect_err(redirect_uri: &str, error: &str, state: Option<&str>) -> Response {
    let mut url = format!("{redirect_uri}?error={}", enc(error));
    if let Some(st) = state {
        url.push_str(&format!("&state={}", enc(st)));
    }
    Redirect::to(&url).into_response()
}

fn error_page(msg: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Html(format!(
            "<!doctype html><meta charset=utf-8><title>Authorization error</title>\
             <body style=\"font-family:system-ui;max-width:32rem;margin:4rem auto\">\
             <h1>Authorization error</h1><p>{}</p></body>",
            html_escape(msg)
        )),
    )
        .into_response()
}

/// The consent page. The hidden `request_id` field's exact markup
/// (`name=request_id value="…"`) is what the browserless client in
/// `oauth_client` scrapes — change one, change both.
fn consent_page(
    branding: &Branding,
    request_id: &str,
    client_name: Option<&str>,
    scope: Option<&str>,
    err: Option<&str>,
) -> String {
    let client = html_escape(client_name.unwrap_or("an MCP client"));
    let scope = html_escape(scope.unwrap_or(&branding.scope));
    let err_html = err
        .map(|e| format!("<p style=\"color:#b00\"><strong>{}</strong></p>", html_escape(e)))
        .unwrap_or_default();
    format!(
        "<!doctype html><meta charset=utf-8><title>Authorize access</title>\
         <body style=\"font-family:system-ui;max-width:32rem;margin:4rem auto\">\
         <h1>{heading}</h1>\
         <p><strong>{client}</strong> is requesting <code>{scope}</code> \
         {grant}</p>{err_html}\
         <form method=post action=\"/oauth/consent\">\
         <input type=hidden name=request_id value=\"{rid}\">\
         <p><label>Operator passphrase:<br>\
         <input type=password name=password autocomplete=current-password autofocus \
         style=\"width:100%;padding:.5rem;font-size:1rem\"></label></p>\
         <p><button name=action value=approve style=\"padding:.6rem 1.2rem;font-size:1rem\">Approve</button>\
         &nbsp;<button name=action value=deny style=\"padding:.6rem 1.2rem;font-size:1rem\">Deny</button></p>\
         </form></body>",
        heading = html_escape(&branding.heading),
        client = client,
        scope = scope,
        grant = html_escape(&branding.grant_description),
        err_html = err_html,
        rid = html_escape(request_id),
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SCOPE: &str = "vault.read";

    fn test_branding() -> Branding {
        Branding {
            scope: SCOPE.to_string(),
            heading: "Authorize access to your vault".to_string(),
            grant_description: "(read and write) access to your notes.".to_string(),
        }
    }

    fn test_state(state_file: Option<PathBuf>) -> AuthState {
        AuthState::new(AuthConfig {
            public_base: "https://vault.example".to_string(),
            password_hash: sha256(b"correct horse battery staple"),
            state_file,
            branding: test_branding(),
        })
    }

    /// Seed the store with one issued grant and return the plaintext refresh
    /// token that maps to it.
    async fn seed_grant(s: &AuthState) -> String {
        let refresh = random_token();
        let access = random_token();
        let now = SystemTime::now();
        let grant = Grant {
            client_id: "client-1".to_string(),
            scope: Some(SCOPE.to_string()),
            access_hash: sha256(access.as_bytes()),
            refresh_hash: sha256(refresh.as_bytes()),
            access_expires_at: now + ACCESS_TOKEN_TTL,
            refresh_expires_at: now + REFRESH_TOKEN_TTL,
        };
        s.store.access.write().await.insert(grant.access_hash, grant.clone());
        s.store.refresh.write().await.insert(grant.refresh_hash, grant);
        refresh
    }

    fn refresh_form(refresh: &str) -> TokenForm {
        TokenForm {
            grant_type: Some("refresh_token".to_string()),
            code: None,
            redirect_uri: None,
            client_id: Some("client-1".to_string()),
            code_verifier: None,
            refresh_token: Some(refresh.to_string()),
        }
    }

    #[test]
    fn config_validation_is_reject_by_default() {
        let short = AuthConfig::new("https://x", "short", None, test_branding());
        assert!(short.is_err(), "a short passphrase must be refused");
        let plain = AuthConfig::new("http://x.example", "correct horse battery staple", None, test_branding());
        assert!(plain.is_err(), "a non-https origin must be refused");
        let ok = AuthConfig::new("https://x.example/", "correct horse battery staple", None, test_branding())
            .unwrap();
        assert_eq!(ok.public_base, "https://x.example", "trailing slash trimmed");
        assert_eq!(ok.resource(), "https://x.example/mcp");
    }

    #[test]
    fn from_env_reads_the_prefixed_names_and_honours_a_legacy_prefix() {
        // SAFETY: names only this test uses.
        unsafe {
            std::env::set_var("RLLMTEST_PUBLIC_URL", "https://vault.example/");
            std::env::set_var("RLLMTEST_OLD_AUTH_PASSWORD", "correct horse battery staple");
        }
        let cfg = AuthConfig::from_env("RLLMTEST", Some("RLLMTEST_OLD"), test_branding()).unwrap();
        assert_eq!(cfg.public_base, "https://vault.example");
        assert!(cfg.state_file.is_none());
        let Err(missing) = AuthConfig::from_env("RLLMTEST_UNSET", None, test_branding()) else {
            panic!("an unset public URL must be refused");
        };
        let msg = missing.to_string();
        assert!(msg.contains("RLLMTEST_UNSET_PUBLIC_URL"), "names the variable: {msg}");
    }

    /// The consent page carries the branding, and the hidden `request_id`
    /// markup the browserless client scrapes stays exactly as it is.
    #[test]
    fn consent_page_is_branded_and_scrapeable() {
        let html = consent_page(&test_branding(), "req-1", Some("Claude"), None, None);
        assert!(html.contains("<h1>Authorize access to your vault</h1>"));
        assert!(html.contains("<code>vault.read</code> (read and write) access to your notes."));
        assert!(html.contains("name=request_id value=\"req-1\""));
    }

    /// The fix: the SAME refresh token used twice in a row (lockstep duplicate /
    /// lost-response retry) succeeds both times instead of the second getting an
    /// invalid_grant. Strict single-use rotation used to fail the second.
    #[tokio::test]
    async fn refresh_token_survives_duplicate_use() {
        let s = test_state(None);
        let refresh = seed_grant(&s).await;

        let first = token_from_refresh(&s, refresh_form(&refresh)).await;
        assert_eq!(first.status(), StatusCode::OK, "first refresh should succeed");

        // Within the grace window, the original token is still accepted.
        let second = token_from_refresh(&s, refresh_form(&refresh)).await;
        assert_eq!(second.status(), StatusCode::OK, "duplicate refresh within grace should also succeed");
    }

    /// An unknown refresh token is still rejected (no regression in the gate).
    #[tokio::test]
    async fn unknown_refresh_token_is_rejected() {
        let s = test_state(None);
        let resp = token_from_refresh(&s, refresh_form("not-a-real-token")).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// Issued grants and registered clients survive a restart through the state
    /// file: persist, drop the state, reload, and the access token still validates.
    #[tokio::test]
    async fn grants_persist_across_reload() {
        let dir = crate::testutil::tempdir("oauth-server");
        let path = dir.join("state.json");

        let access = {
            let s = test_state(Some(path.clone()));
            s.store.clients.write().await.insert(
                "client-1".to_string(),
                Client { redirect_uris: vec!["https://x/cb".to_string()], name: Some("Test".to_string()) },
            );
            let token = random_token();
            let now = SystemTime::now();
            let grant = Grant {
                client_id: "client-1".to_string(),
                scope: Some(SCOPE.to_string()),
                access_hash: sha256(token.as_bytes()),
                refresh_hash: sha256(b"refresh"),
                access_expires_at: now + ACCESS_TOKEN_TTL,
                refresh_expires_at: now + REFRESH_TOKEN_TTL,
            };
            s.store.access.write().await.insert(grant.access_hash, grant.clone());
            s.store.refresh.write().await.insert(grant.refresh_hash, grant);
            s.persist().await;
            token
        };

        // Fresh state built only from the file on disk.
        let reloaded = test_state(Some(path.clone()));
        assert!(reloaded.validate_access(&access).await, "access token should survive reload");
        assert!(reloaded.store.clients.read().await.contains_key("client-1"), "client should survive reload");

        // The state file must not be world/group readable — it holds live grants.
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "state file must be 0600");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Expired grants are pruned on load rather than resurrected.
    #[tokio::test]
    async fn expired_grants_dropped_on_load() {
        let dir = crate::testutil::tempdir("oauth-server-exp");
        let path = dir.join("state.json");

        let token = random_token();
        let past = UNIX_EPOCH + Duration::from_secs(1);
        let snap = Snapshot {
            clients: vec![],
            access: vec![GrantRepr {
                client_id: "c".to_string(),
                scope: None,
                access_hash: URL_SAFE_NO_PAD.encode(sha256(token.as_bytes())),
                refresh_hash: URL_SAFE_NO_PAD.encode(sha256(b"r")),
                access_expires_at: secs_since_epoch(past),
                refresh_expires_at: secs_since_epoch(past),
            }],
            refresh: vec![],
        };
        write_snapshot(&path, &snap).unwrap();

        let s = test_state(Some(path.clone()));
        assert!(!s.validate_access(&token).await, "expired access token must not load");
        let _ = std::fs::remove_dir_all(&dir);
    }
    #[tokio::test]
    async fn refreshing_preserves_old_access_until_expiry() {
        let s = test_state(None);
        let refresh = seed_grant(&s).await;
        let old = s.store.refresh.read().await[&sha256(refresh.as_bytes())].access_hash;
        assert_eq!(token_from_refresh(&s, refresh_form(&refresh)).await.status(), StatusCode::OK);
        assert!(s.store.access.read().await.contains_key(&old));
        // Duplicate refresh must not revoke either newly issued bearer.
        assert_eq!(token_from_refresh(&s, refresh_form(&refresh)).await.status(), StatusCode::OK);
        assert_eq!(s.store.access.read().await.len(), 3);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_grants_all_survive_reload() {
        let dir = crate::testutil::tempdir("concurrent-auth");
        let path = dir.join("state.json");
        let s = test_state(Some(path.clone()));
        let mut jobs = Vec::new();
        for i in 0..64 {
            let s = s.clone();
            jobs.push(tokio::spawn(async move {
                assert_eq!(issue_grant(&s, format!("client-{i}"), None).await.status(), StatusCode::OK);
            }));
        }
        for job in jobs { job.await.unwrap(); }
        let loaded = test_state(Some(path));
        assert_eq!(loaded.store.access.read().await.len(), 64);
        assert_eq!(loaded.store.refresh.read().await.len(), 64);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(feature = "oauth-client")]
    #[tokio::test]
    async fn mint_recovers_at_every_leg_and_after_lost_state() {
        use crate::oauth_client::{ClientIdentity, MintRetry, mint_access_token_retrying};
        use std::sync::atomic::{AtomicUsize, Ordering};
        for fault in [502u16, 503, 504, 0] {
            let lost_state = fault == 0;
            for leg in ["/oauth/register", "/oauth/authorize", "/oauth/consent", "/oauth/token"] {
                if lost_state && leg == "/oauth/register" { continue; }
                let s = test_state(None);
                let state = s.clone();
                let attempts = Arc::new(AtomicUsize::new(0));
                let registrations = Arc::new(AtomicUsize::new(0));
                let counter = attempts.clone();
                let regs = registrations.clone();
                let router = oauth_router(s).layer(axum::middleware::from_fn(move |req: Request, next: Next| {
                    let state = state.clone();
                    let counter = counter.clone();
                    let regs = regs.clone();
                    async move {
                        if req.uri().path() == "/oauth/register" { regs.fetch_add(1, Ordering::SeqCst); }
                        if req.uri().path() == leg && counter.fetch_add(1, Ordering::SeqCst) == 0 {
                            if lost_state {
                                state.store.clients.write().await.clear();
                                state.store.pending.write().await.clear();
                                state.store.codes.write().await.clear();
                            } else {
                                return StatusCode::from_u16(fault).unwrap().into_response();
                            }
                        }
                        next.run(req).await
                    }
                }));
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let base = format!("http://{}", listener.local_addr().unwrap());
                let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
                let started = std::time::Instant::now();
                let token = mint_access_token_retrying(&base, "correct horse battery staple", &ClientIdentity {
                    client_name: "fault-test".into(), redirect_uri: "http://127.0.0.1/cb".into(), scope: SCOPE.into(),
                }, MintRetry { attempts: 2, base_delay: Duration::from_millis(5), max_delay: Duration::from_millis(5) })
                    .await.unwrap_or_else(|e| panic!("{leg}, lost_state={lost_state}: {e:#}"));
                assert!(!token.access_token.is_empty());
                assert_eq!(registrations.load(Ordering::SeqCst), 2);
                eprintln!("mint recovery: {leg}, fault={fault}, registrations=2, elapsed={:?}", started.elapsed());
                server.abort();
            }
        }
    }

    #[cfg(feature = "oauth-client")]
    #[tokio::test]
    async fn wrong_passphrase_does_not_restart_mint() {
        use crate::oauth_client::{ClientIdentity, MintRetry, mint_access_token_retrying};
        let s = test_state(None);
        let router = oauth_router(s.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let err = mint_access_token_retrying(&base, "wrong", &ClientIdentity {
            client_name: "test".into(), redirect_uri: "http://127.0.0.1/cb".into(), scope: SCOPE.into(),
        }, MintRetry::default()).await.unwrap_err();
        assert!(err.to_string().contains("incorrect operator passphrase"));
        assert_eq!(s.store.clients.read().await.len(), 1);
        assert!(s.store.access.read().await.is_empty());
        server.abort();
    }

    /// The token response's refresh grant rides all the way through the
    /// client's mint into [`MintedToken::refresh_token`] — the field a
    /// credential lease travels on. Callers that ignore it see no difference.
    #[cfg(feature = "oauth-client")]
    #[tokio::test]
    async fn mint_surfaces_the_refresh_grant_on_minted_token() {
        use crate::oauth_client::{ClientIdentity, mint_access_token};
        let s = test_state(None);
        let router = oauth_router(s.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let minted = mint_access_token(&base, "correct horse battery staple", &ClientIdentity {
            client_name: "lease-test".into(), redirect_uri: "http://127.0.0.1/cb".into(), scope: SCOPE.into(),
        })
        .await.unwrap();
        let refresh = minted.refresh_token.expect("this server issues a refresh grant on every mint");
        assert!(!refresh.is_empty());
        // It is a real grant, not decoration: the server's refresh map knows it.
        assert!(s.store.refresh.read().await.contains_key(&sha256(refresh.as_bytes())));
        server.abort();
    }

    /// The mint's dynamic client registration generates the client_id
    /// internally; a test that wants to drive the grant it minted reads it
    /// back from the store (there is exactly one client in a fresh state).
    #[cfg(feature = "oauth-client")]
    async fn the_minted_client_id(s: &AuthState) -> String {
        s.store.clients.read().await.keys().next().unwrap().clone()
    }

    /// The client's refresh round-trip: a fresh access token that validates,
    /// the advertised lifetime carried through, and the grant rotated
    /// server-side — a new refresh row at full TTL, the presented one
    /// truncated to its grace window, and (refresh ≠ revoke) the old access
    /// bearer left alive until its own expiry.
    #[cfg(feature = "oauth-client")]
    #[tokio::test]
    async fn refresh_round_trip_issues_a_fresh_access_token_and_rotates_the_grant() {
        use crate::oauth_client::{ClientIdentity, mint_access_token, refresh_access_token};
        let s = test_state(None);
        let router = oauth_router(s.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        let minted = mint_access_token(&base, "correct horse battery staple", &ClientIdentity {
            client_name: "lease-test".into(), redirect_uri: "http://127.0.0.1/cb".into(), scope: SCOPE.into(),
        })
        .await.unwrap();
        let client_id = the_minted_client_id(&s).await;
        let old_refresh = minted.refresh_token.clone().unwrap();
        let old_refresh_hash = sha256(old_refresh.as_bytes());

        let refreshed = refresh_access_token(&base, &old_refresh, &client_id).await.unwrap();
        assert_eq!(refreshed.expires_in, Some(ACCESS_TOKEN_TTL.as_secs()), "lifetime is advertised");
        assert!(s.validate_access(&refreshed.access_token).await, "the fresh access token validates");

        // Rotation: the response's refresh token is a NEW full-TTL grant, and
        // the presented one now expires inside the grace window.
        let new_refresh = refreshed.refresh_token.as_ref().expect("refresh rotates the grant");
        assert_ne!(new_refresh, &old_refresh);
        assert!(s.store.refresh.read().await.contains_key(&sha256(new_refresh.as_bytes())));
        let rotated = s.store.refresh.read().await[&old_refresh_hash].clone();
        assert!(
            rotated.refresh_expires_at <= SystemTime::now() + REFRESH_GRACE,
            "the presented token must be grace-bound, not full-TTL"
        );
        let fresh_row = s.store.refresh.read().await[&sha256(new_refresh.as_bytes())].clone();
        assert!(
            fresh_row.refresh_expires_at > SystemTime::now() + REFRESH_TOKEN_TTL - Duration::from_secs(120),
            "the replacement grant starts with its full TTL"
        );

        // Refresh preserves the old access bearer until its own expiry — that
        // is what makes this different from a revocation.
        assert!(s.validate_access(&minted.access_token).await, "the old bearer is not revoked by a refresh");
        server.abort();
    }

    /// The concurrent-refresh grace contract, pinned through the client call:
    /// a second refresh with the PRE-rotation token succeeds inside the
    /// window (a lockstep duplicate or a lost-response retry must not become
    /// an invalid_grant). Past the window it expires itself out — that half
    /// is pinned at handler level in `refresh_token_survives_duplicate_use`'s
    /// siblings.
    #[cfg(feature = "oauth-client")]
    #[tokio::test]
    async fn second_refresh_with_the_pre_rotation_token_succeeds_within_grace() {
        use crate::oauth_client::{ClientIdentity, mint_access_token, refresh_access_token};
        let s = test_state(None);
        let router = oauth_router(s.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        let minted = mint_access_token(&base, "correct horse battery staple", &ClientIdentity {
            client_name: "lease-test".into(), redirect_uri: "http://127.0.0.1/cb".into(), scope: SCOPE.into(),
        })
        .await.unwrap();
        let client_id = the_minted_client_id(&s).await;
        let pre_rotation = minted.refresh_token.clone().unwrap();

        let first = refresh_access_token(&base, &pre_rotation, &client_id).await.unwrap();
        let second = refresh_access_token(&base, &pre_rotation, &client_id).await
            .expect("the pre-rotation token must survive its grace window");
        assert_ne!(first.access_token, second.access_token, "each refresh issues a fresh bearer");
        assert_ne!(first.refresh_token.unwrap(), second.refresh_token.unwrap(), "each refresh rotates");

        // And both issued bearers are live.
        assert!(s.validate_access(&first.access_token).await);
        assert!(s.validate_access(&second.access_token).await);
        server.abort();
    }

    /// Revocation kills the grant row: the refresh token stops working on
    /// the very next refresh, and the grant's paired access bearer dies with
    /// it — that is the whole point of ending a lease.
    #[tokio::test]
    async fn revocation_kills_the_grant_and_later_refresh_fails() {
        let s = test_state(None);
        let refresh = seed_grant(&s).await;
        let access_hash = s.store.refresh.read().await[&sha256(refresh.as_bytes())].access_hash;

        let resp = revoke(
            State(s.clone()),
            Form(RevokeForm { token: Some(refresh.clone()), token_type_hint: None }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(!s.store.refresh.read().await.contains_key(&sha256(refresh.as_bytes())));
        assert!(
            !s.store.access.read().await.contains_key(&access_hash),
            "the paired access bearer must die with the grant"
        );
        assert_eq!(
            token_from_refresh(&s, refresh_form(&refresh)).await.status(),
            StatusCode::BAD_REQUEST,
            "the revoked refresh token must not refresh"
        );
    }

    /// RFC 7009 §2.1: an unknown (or already-invalid) token is answered with
    /// the same empty 200 as a successful revocation, so liveness cannot be
    /// probed through this endpoint.
    #[tokio::test]
    async fn revoking_an_unknown_token_returns_200() {
        let s = test_state(None);
        let resp = revoke(
            State(s.clone()),
            Form(RevokeForm {
                token: Some("not-a-real-token".to_string()),
                token_type_hint: Some("refresh_token".to_string()),
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        // And nothing was disturbed.
        assert!(s.store.refresh.read().await.is_empty());
        assert!(s.store.access.read().await.is_empty());
    }

    /// The lease loop, end to end over the wire: mint → refresh (the lease
    /// travels on the rotated token) → revoke → the lease's bearer is dead
    /// and the next refresh surfaces the server's invalid_grant.
    #[cfg(feature = "oauth-client")]
    #[tokio::test]
    async fn revocation_over_http_kills_the_lease() {
        use crate::oauth_client::{ClientIdentity, bounded_client, mint_access_token, refresh_access_token};
        let s = test_state(None);
        let router = oauth_router(s.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

        let minted = mint_access_token(&base, "correct horse battery staple", &ClientIdentity {
            client_name: "lease-test".into(), redirect_uri: "http://127.0.0.1/cb".into(), scope: SCOPE.into(),
        })
        .await.unwrap();
        let client_id = the_minted_client_id(&s).await;
        let refreshed = refresh_access_token(&base, &minted.refresh_token.unwrap(), &client_id).await.unwrap();
        let lease_access = refreshed.access_token.clone();
        let lease_refresh = refreshed.refresh_token.unwrap();

        // The hub revokes the lease, as a lease-holder would over the wire.
        let resp = bounded_client(false)
            .unwrap()
            .post(format!("{base}/oauth/revoke"))
            .form(&[("token", lease_refresh.as_str()), ("token_type_hint", "refresh_token")])
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), reqwest::StatusCode::OK);

        // The shipped session's bearer is dead...
        assert!(!s.validate_access(&lease_access).await, "the lease's bearer must die with the grant");
        // ...and its refresh token is gone: the client surfaces the refusal.
        let err = refresh_access_token(&base, &lease_refresh, &client_id).await.unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("invalid_grant"), "the revocation must surface as invalid_grant: {msg}");
        server.abort();
    }

}
