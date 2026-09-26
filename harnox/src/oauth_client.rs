//! A browserless OAuth 2.1 client for the server in `oauth_server` (feature
//! `oauth-client`).
//!
//! That server guards `/mcp` with OAuth (authorization-code + PKCE S256),
//! gated by an operator passphrase at a consent screen — it deliberately
//! issues no static bearer tokens. Rather than weaken that, a consumer acts as
//! a well-behaved OAuth client: it runs the whole flow itself and mints a
//! **fresh access token** whenever it needs one, re-bootstrapping from the
//! passphrase every time, which also makes it self-healing. Nothing is
//! persisted; the registered client and grant live in the server's store and
//! age out on their own.
//!
//! The flow, against the server's endpoints:
//!   1. POST /oauth/register        → a client_id (public client, no secret)
//!   2. GET  /oauth/authorize?…     → the consent HTML (carries a request_id)
//!   3. POST /oauth/consent         → 302 to redirect_uri?code=… (passphrase gate)
//!   4. POST /oauth/token           → { access_token, expires_in, refresh_token, … }
//!
//! Standard OAuth clients assume a browser redirect for the consent leg; this
//! one POSTs the passphrase to the consent form directly, which is why it is
//! hand-written rather than an `oauth2`-crate flow. The `request_id` it scrapes
//! from the consent page is a contract with `oauth_server::consent_page`.
//!
//! ## Every leg of the flow is bounded
//!
//! Every request carries [`HTTP_TIMEOUT`] / [`CONNECT_TIMEOUT`]. Before these
//! existed, a wedged or unreachable origin — server down, mid-restart, or a
//! firewall drop — left the mint's `reqwest` future pending **forever**: the
//! caller's log showed "minting access token" and then nothing else, ever.
//!
//! ## Transient mint failures are retried
//!
//! Connection failures, interrupted bodies, timeouts, HTTP 502/503/504, and
//! explicit lost-flow-state responses restart the entire flow with bounded
//! backoff ([`MintRetry`]). Passphrase and PKCE refusals do not retry. This is
//! separate from a resource caller's re-mint after a rejected stale bearer.
//!
//! ## Refreshing a grant
//!
//! A caller holding a grant's refresh token ([`MintedToken::refresh_token`])
//! can mint fresh access tokens without the operator passphrase:
//! [`refresh_access_token`] posts `grant_type=refresh_token` and returns the
//! fresh [`MintedToken`] — including the **rotated** replacement refresh
//! token, which the server issues on every refresh. The retry posture mirrors
//! the mint's ([`refresh_access_token_retrying`]) with one deliberate
//! exception: an OAuth refusal (`invalid_grant`) is surfaced as-is, because
//! re-presenting a rotated-away, revoked, or expired token cannot get better
//! by retrying.
//!
//! ## Caching
//!
//! A token is valid for its whole advertised lifetime, so re-minting per call
//! buys nothing and costs four round trips. [`TokenCache`] keeps one live
//! token per OAuth origin in memory until shortly before it expires; a caller
//! that sees its bearer rejected anyway (the server restarted and dropped its
//! grants) calls [`TokenCache::invalidate`] and mints afresh.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;

use crate::crypto::{random_bytes, sha256};
use crate::time::now_secs;

/// Per-request bound on every leg of the OAuth dance. 20s is generous for
/// four loopback round trips against a colocated server and still far under
/// any caller's turn budget even across all four sequential legs.
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(20);
/// Bound on establishing the TCP connection itself, tighter than
/// [`HTTP_TIMEOUT`] since a connect that hasn't succeeded by then is not going
/// to start responding either — failing fast gets a clearer error sooner.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Who the client says it is when it registers and authorizes.
#[derive(Clone, Debug)]
pub struct ClientIdentity {
    /// Shown on the server's consent page (`Melete`).
    pub client_name: String,
    /// A loopback redirect URI. Never actually served — it only needs to be a
    /// value the server accepts (`http://127.0.0.1…`) and echoes back with the
    /// `code`.
    pub redirect_uri: String,
    /// The scope to request — the server's single supported scope.
    pub scope: String,
}

/// A freshly minted access token and its advertised lifetime.
#[derive(Clone, Debug)]
pub struct MintedToken {
    pub access_token: String,
    /// Standard OAuth lifetime in seconds. `None` on a server that doesn't
    /// report one — [`cache_ttl`] then falls back to a deliberately short
    /// assumption rather than trusting a documented figure.
    pub expires_in: Option<u64>,
    /// The refresh grant the server issued alongside this token, when it
    /// issues one — this server does (30 days, rotated on every use). It is
    /// what a **credential lease** rides: whoever holds it can mint fresh
    /// access tokens via [`refresh_access_token`] without ever seeing the
    /// operator passphrase, and the hub that issued the lease can end it via
    /// the server's `POST /oauth/revoke`. On a refresh this is the *rotated
    /// replacement* — persist it and retire the one presented, which the
    /// server keeps working only for a short grace window. Callers that
    /// ignore the field are unaffected: minting behaves exactly as before.
    pub refresh_token: Option<String>,
}

#[derive(Deserialize)]
struct RegisterResp {
    client_id: String,
}

#[derive(Deserialize)]
struct TokenResp {
    access_token: String,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    refresh_token: Option<String>,
}

/// A `reqwest` client carrying [`HTTP_TIMEOUT`] and [`CONNECT_TIMEOUT`]. The
/// flow itself must not follow redirects (it reads the consent 302's
/// `Location`); a caller probing the resource with a minted token may.
pub fn bounded_client(follow_redirects: bool) -> Result<reqwest::Client> {
    let mut b = reqwest::Client::builder().timeout(HTTP_TIMEOUT).connect_timeout(CONNECT_TIMEOUT);
    if !follow_redirects {
        b = b.redirect(reqwest::redirect::Policy::none());
    }
    b.build().context("building HTTP client")
}

/// Mint a fresh access token. `base` is the server origin **without** the
/// `/mcp` suffix (e.g. `http://127.0.0.1:8000` — see [`oauth_base`]);
/// `password` is the operator passphrase that gates the consent screen.
pub async fn mint_access_token(base: &str, password: &str, identity: &ClientIdentity) -> Result<MintedToken> {
    let base = base.trim_end_matches('/');
    let client = bounded_client(false)?;

    // 1. Dynamic client registration.
    let reg: RegisterResp = client
        .post(format!("{base}/oauth/register"))
        .json(&serde_json::json!({
            "redirect_uris": [identity.redirect_uri],
            "client_name": identity.client_name,
        }))
        .send()
        .await
        .context("POST /oauth/register")?
        .error_for_status()
        .context("client registration rejected")?
        .json()
        .await
        .context("parsing register response")?;
    let client_id = reg.client_id;

    // 2. PKCE + a fresh authorize request.
    let verifier = URL_SAFE_NO_PAD.encode(random_bytes());
    let challenge = URL_SAFE_NO_PAD.encode(sha256(verifier.as_bytes()));
    let state = URL_SAFE_NO_PAD.encode(&random_bytes()[..16]);

    let authorize_resp = client
        .get(format!("{base}/oauth/authorize"))
        .query(&[
            ("response_type", "code"),
            ("client_id", client_id.as_str()),
            ("redirect_uri", identity.redirect_uri.as_str()),
            ("scope", identity.scope.as_str()),
            ("state", state.as_str()),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
        ])
        .send()
        .await
        .context("GET /oauth/authorize")?;
    let authorize_status = authorize_resp.error_for_status_ref().map(|_| ()).map_err(|e| e.without_url());
    let consent_html = authorize_resp.text()
        .await
        .context("reading consent page")?;
    if consent_html.contains("unknown client_id — register first") {
        bail!("OAuth flow state lost or expired; restart mint");
    }
    authorize_status.context("authorize rejected")?;
    let request_id =
        extract_request_id(&consent_html).context("could not find request_id in the consent page")?;

    // 3. Approve with the operator passphrase → 302 carrying the auth code.
    let consent_resp = client
        .post(format!("{base}/oauth/consent"))
        .form(&[
            ("request_id", request_id.as_str()),
            ("password", password),
            ("action", "approve"),
        ])
        .send()
        .await
        .context("POST /oauth/consent")?;

    if !consent_resp.status().is_redirection() {
        let status = consent_resp.error_for_status_ref().map(|_| ()).map_err(|e| e.without_url());
        let body = consent_resp.text().await.context("reading consent rejection")?;
        if body.contains("this authorization request expired — start again from the client") {
            bail!("OAuth flow state lost or expired; restart mint");
        }
        status.context("consent endpoint rejected request")?;
        if body.contains("Incorrect passphrase.") {
            bail!("consent rejected: incorrect operator passphrase");
        }
        bail!("consent was not approved (status was not a redirect)");
    }
    let location = consent_resp
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .context("consent redirect had no Location header")?;
    let (code, returned_state) = parse_code_and_state(location)?;
    if returned_state.as_deref() != Some(state.as_str()) {
        bail!("OAuth state mismatch on consent redirect (possible CSRF)");
    }

    // 4. Exchange the code for an access token (PKCE verifier proves it's us).
    let response = client
        .post(format!("{base}/oauth/token"))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", identity.redirect_uri.as_str()),
            ("client_id", client_id.as_str()),
            ("code_verifier", verifier.as_str()),
        ])
        .send()
        .await
        .context("POST /oauth/token")?;
    let status = response.status();
    if status == reqwest::StatusCode::BAD_REQUEST {
        let body: serde_json::Value = response.json().await.context("reading token rejection")?;
        if body["error"] == "invalid_grant"
            && body["error_description"] == "unknown or already-used code"
        {
            bail!("OAuth flow state lost or expired; restart mint");
        }
        bail!("token exchange rejected (HTTP {status})");
    }
    let tok: TokenResp = response.error_for_status()
        .context("token exchange rejected")?
        .json()
        .await
        .context("parsing token response")?;

    Ok(MintedToken {
        access_token: tok.access_token,
        expires_in: tok.expires_in,
        refresh_token: tok.refresh_token,
    })
}

/// How a transiently interrupted mint is retried.
///
/// The shape is deliberately a value rather than a set of constants: what is
/// right depends on who is waiting. An unattended daemon should spend ten
/// seconds absorbing a restart rather than fail a scheduled run; a person at a
/// keyboard would rather be told quickly and press the key again. [`Default`]
/// is the unattended posture — four attempts, ×4 backoff, capped at 8s — which
/// is the one that ran in Melete before this moved into the crate.
#[derive(Clone, Copy, Debug)]
pub struct MintRetry {
    /// Total attempts, including the first. `1` disables retrying.
    pub attempts: u32,
    /// Delay after the first failed attempt; each later one is ×4 this.
    pub base_delay: Duration,
    /// Ceiling on that growth.
    pub max_delay: Duration,
}

impl Default for MintRetry {
    fn default() -> Self {
        Self {
            attempts: 4,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(8),
        }
    }
}

impl MintRetry {
    /// One attempt, no waiting — for a caller that would rather surface the
    /// failure than absorb it.
    pub const fn none() -> Self {
        Self { attempts: 1, base_delay: Duration::ZERO, max_delay: Duration::ZERO }
    }

    /// How long to wait after attempt number `attempt` (1-indexed) failed.
    fn delay_after(&self, attempt: u32) -> Duration {
        let multiplier = 4u32.saturating_pow(attempt.saturating_sub(1));
        std::cmp::min(self.base_delay.saturating_mul(multiplier), self.max_delay)
    }
}

/// Whether a failure was transport-level: connection failure, timeout, or an
/// interrupted request/body, rather than an HTTP or OAuth refusal.
///
/// Inspect the whole source chain: JSON decoding can wrap a reqwest body
/// error in another reqwest error whose own category is only `decode`. Plain
/// malformed JSON without an underlying transport error is not retryable.
pub fn is_transient_connection_error(e: &anyhow::Error) -> bool {
    e.chain()
        .filter_map(|cause| cause.downcast_ref::<reqwest::Error>())
        .any(|re| re.is_connect() || re.is_timeout() || re.is_body()
            || (re.is_request() && !re.is_builder() && re.status().is_none()))
}

fn is_retryable_mint_error(e: &anyhow::Error) -> bool {
    is_transient_connection_error(e)
        || e.chain().any(|cause| cause.to_string() == "OAuth flow state lost or expired; restart mint")
        || e.chain().filter_map(|cause| cause.downcast_ref::<reqwest::Error>())
            .any(|e| matches!(e.status().map(|s| s.as_u16()), Some(502..=504)))
}

/// Mint with bounded whole-flow retries on transport interruptions, HTTP
/// 502/503/504, or the server's explicit lost-flow-state responses. Never resume
/// a single-use code exchange; never retry a passphrase or PKCE refusal.
pub async fn mint_access_token_retrying(
    base: &str,
    password: &str,
    identity: &ClientIdentity,
    retry: MintRetry,
) -> Result<MintedToken> {
    let attempts = retry.attempts.max(1);
    for attempt in 1..attempts {
        match mint_access_token(base, password, identity).await {
            Ok(minted) => return Ok(minted),
            Err(e) if is_retryable_mint_error(&e) => {
                let delay = retry.delay_after(attempt);
                tracing::info!(
                    "{base} mint interrupted (attempt {attempt}/{attempts}), retrying in {}ms",
                    delay.as_millis()
                );
                tokio::time::sleep(delay).await;
            }
            Err(e) => return Err(e),
        }
    }
    mint_access_token(base, password, identity).await.inspect_err(|e| {
        if attempts > 1 && is_retryable_mint_error(e) {
            tracing::warn!("{base} mint still failing after {attempts} attempts, giving up: {e:#}");
        }
    })
}

/// Trade a refresh token for a fresh [`MintedToken`] (`grant_type=refresh_token`).
///
/// `base` is the server origin **without** the `/mcp` suffix (see
/// [`oauth_base`]); `refresh_token` is a live grant's refresh token; `client_id`
/// is the id of the client the grant was minted under. The response carries the
/// server's **rotated** refresh token in [`MintedToken::refresh_token`] — this
/// server rotates on every refresh — so a caller must persist the new one and
/// stop presenting the old, which keeps working only for the server's short
/// concurrent-refresh grace window.
///
/// A refused refresh (`invalid_grant`: rotated away past its grace, revoked, or
/// expired) is surfaced as an `Err` carrying the server's own error words, and
/// deliberately never folded into a retry or a silent re-mint: which of those
/// three it is decides what the caller does next, and the caller — lease-holder
/// or hub — is the one that knows.
pub async fn refresh_access_token(base: &str, refresh_token: &str, client_id: &str) -> Result<MintedToken> {
    let base = base.trim_end_matches('/');
    let client = bounded_client(false)?;
    let response = client
        .post(format!("{base}/oauth/token"))
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
        ])
        .send()
        .await
        .context("POST /oauth/token (refresh)")?;
    let status = response.status();
    if status == reqwest::StatusCode::BAD_REQUEST {
        // A settled OAuth refusal. Surface the server's own words: this is the
        // rotated-away / revoked / expired case, and it must be distinguishable
        // from a network hiccup.
        let body: serde_json::Value = response.json().await.context("reading refresh rejection")?;
        bail!(
            "refresh rejected: {} ({})",
            body["error"].as_str().unwrap_or("invalid_request"),
            body["error_description"].as_str().unwrap_or("no description"),
        );
    }
    let tok: TokenResp = response
        .error_for_status()
        .context("refresh rejected")?
        .json()
        .await
        .context("parsing refresh response")?;
    Ok(MintedToken {
        access_token: tok.access_token,
        expires_in: tok.expires_in,
        refresh_token: tok.refresh_token,
    })
}

/// Whether a refresh failure was transport-level or a gateway 5xx — the cases
/// [`refresh_access_token_retrying`] absorbs. An OAuth refusal is deliberately
/// absent from this set: re-presenting a rotated-away or revoked token cannot
/// get better by retrying.
fn is_retryable_refresh_error(e: &anyhow::Error) -> bool {
    is_transient_connection_error(e)
        || e.chain()
            .filter_map(|cause| cause.downcast_ref::<reqwest::Error>())
            .any(|e| matches!(e.status().map(|s| s.as_u16()), Some(502..=504)))
}

/// [`refresh_access_token`] with the mint's transient-interruption posture
/// ([`mint_access_token_retrying`]): bounded backoff over connection failures,
/// timeouts, interrupted bodies, and HTTP 502/503/504.
///
/// The same refresh token is re-presented on each attempt, which is safe for
/// exactly two reasons: a retry after a failure that never reached the server
/// re-presents a token that was never rotated, and a retry after a *lost
/// response* lands inside the server's concurrent-refresh grace window. A
/// settled `invalid_grant` is never retried — see [`refresh_access_token`].
pub async fn refresh_access_token_retrying(
    base: &str,
    refresh_token: &str,
    client_id: &str,
    retry: MintRetry,
) -> Result<MintedToken> {
    let attempts = retry.attempts.max(1);
    for attempt in 1..attempts {
        match refresh_access_token(base, refresh_token, client_id).await {
            Ok(minted) => return Ok(minted),
            Err(e) if is_retryable_refresh_error(&e) => {
                let delay = retry.delay_after(attempt);
                tracing::info!(
                    "{base} refresh interrupted (attempt {attempt}/{attempts}), retrying in {}ms",
                    delay.as_millis()
                );
                tokio::time::sleep(delay).await;
            }
            Err(e) => return Err(e),
        }
    }
    refresh_access_token(base, refresh_token, client_id).await.inspect_err(|e| {
        if attempts > 1 && is_retryable_refresh_error(e) {
            tracing::warn!("{base} refresh still failing after {attempts} attempts, giving up: {e:#}");
        }
    })
}

/// The OAuth origin for a resource URL: the `/mcp` suffix stripped
/// (`http://127.0.0.1:8000/mcp` → `http://127.0.0.1:8000`). The server serves
/// its `/oauth/*` endpoints at the origin, not under `/mcp`, so a caller
/// minting a token from a resource URL must strip that suffix first. Also the
/// key [`TokenCache`] is indexed by, so the two spellings of one server can't
/// produce two cache entries.
pub fn oauth_base(resource_url: &str) -> String {
    let trimmed = resource_url.trim_end_matches('/');
    trimmed.strip_suffix("/mcp").unwrap_or(trimmed).to_string()
}

/// Pull the hidden `request_id` value out of the consent HTML
/// (`<input type=hidden name=request_id value="…">`).
fn extract_request_id(html: &str) -> Option<String> {
    let anchor = "name=request_id value=\"";
    let start = html.find(anchor)? + anchor.len();
    let rest = &html[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// Extract `code` (and `state`) from a `redirect_uri?code=…&state=…` Location.
fn parse_code_and_state(location: &str) -> Result<(String, Option<String>)> {
    let url = reqwest::Url::parse(location).with_context(|| format!("parsing consent redirect {location}"))?;
    let mut code = None;
    let mut state = None;
    for (k, v) in url.query_pairs() {
        match k.as_ref() {
            "code" => code = Some(v.into_owned()),
            "state" => state = Some(v.into_owned()),
            "error" => bail!("authorization failed: {v}"),
            _ => {}
        }
    }
    let code = code.context("no authorization code in consent redirect")?;
    Ok((code, state))
}

// ---------------------------------------------------------------------------
// The in-memory token cache
// ---------------------------------------------------------------------------

/// Safety margin subtracted from a token's advertised lifetime, so a token is
/// replaced before it can expire mid-use in whatever it was handed to.
const EXPIRY_MARGIN_SECS: u64 = 60;
/// How long a token is cached when the server advertises no lifetime.
/// Deliberately far below the server's documented 1-hour TTL: an unverified
/// assumption that runs long fails as 401s inside a spawned agent, where
/// nothing can retry it, so the fallback errs short.
const DEFAULT_TTL_SECS: u64 = 300;
/// Floor on the cached lifetime, so a pathologically short `expires_in` can't
/// make the cache a no-op with extra bookkeeping.
const MIN_TTL_SECS: u64 = 60;

/// How long a freshly minted token may be reused, from its advertised lifetime.
pub fn cache_ttl(expires_in: Option<u64>) -> u64 {
    expires_in
        .map(|secs| secs.saturating_sub(EXPIRY_MARGIN_SECS))
        .unwrap_or(DEFAULT_TTL_SECS)
        .max(MIN_TTL_SECS)
}

/// A minted token and the unix second after which it must not be reused.
struct CachedToken {
    token: String,
    good_until: u64,
}

/// Live access tokens by OAuth origin (see [`oauth_base`]). In-memory only —
/// nothing about a bearer reaches disk. A consumer typically holds one in a
/// `static LazyLock<TokenCache>` for the process's lifetime.
#[derive(Default)]
pub struct TokenCache {
    entries: Mutex<HashMap<String, CachedToken>>,
    retry: MintRetry,
}

impl TokenCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// A cache whose mints use `retry` instead of the unattended default —
    /// see [`MintRetry`].
    pub fn with_retry(retry: MintRetry) -> Self {
        Self { retry, ..Self::default() }
    }

    /// The cached token for `origin`, if one is still within its lifetime.
    pub fn get(&self, origin: &str) -> Option<String> {
        let cache = self.entries.lock().ok()?;
        let entry = cache.get(origin)?;
        (now_secs() < entry.good_until).then(|| entry.token.clone())
    }

    /// Cache `token` for `origin` until shortly before it expires. Only the
    /// access token is kept — a [`MintedToken::refresh_token`] is the
    /// lease-holder's to persist, not cache state.
    pub fn store(&self, origin: &str, token: &MintedToken) {
        if let Ok(mut cache) = self.entries.lock() {
            cache.insert(
                origin.to_string(),
                CachedToken {
                    token: token.access_token.clone(),
                    good_until: now_secs() + cache_ttl(token.expires_in),
                },
            );
        }
    }

    /// Drop any cached token for `origin`, so the next [`get_or_mint`] mints
    /// afresh. For a caller that saw its bearer rejected (a 401 from the
    /// server) and wants the self-healing re-mint.
    ///
    /// [`get_or_mint`]: TokenCache::get_or_mint
    pub fn invalidate(&self, origin: &str) {
        if let Ok(mut cache) = self.entries.lock() {
            cache.remove(origin);
        }
    }

    /// The unix-second deadline of the token cached for `origin`, if any.
    ///
    /// For an in-process caller the cache is self-healing: [`get_or_mint`]
    /// mints fresh once the deadline passes. A caller that instead hands a
    /// minted token to a **separate long-lived process** (which bakes the
    /// bearer into its config once at spawn time) has no such path back to
    /// freshness; this lets it carry the deadline alongside the token so it
    /// can notice its own copy going stale.
    ///
    /// [`get_or_mint`]: TokenCache::get_or_mint
    pub fn good_until(&self, origin: &str) -> Option<u64> {
        self.entries.lock().ok()?.get(origin).map(|e| e.good_until)
    }

    /// The cached token for `origin`, or a freshly minted one (via
    /// [`mint_access_token_retrying`], so a server that is merely mid-restart
    /// is waited out rather than reported) stored for next time. A mint failure
    /// is logged here, at the one site every caller funnels through — many
    /// callers propagate the `Err` with `?` rather than logging it themselves,
    /// and a failing mint must never be silent.
    pub async fn get_or_mint(&self, origin: &str, password: &str, identity: &ClientIdentity) -> Result<String> {
        if let Some(token) = self.get(origin) {
            tracing::debug!("reusing the cached access token for {origin}");
            return Ok(token);
        }
        tracing::info!("minting access token via OAuth from {origin}");
        let minted = match mint_access_token_retrying(origin, password, identity, self.retry).await {
            Ok(m) => m,
            Err(e) => {
                tracing::error!("minting access token from {origin} failed: {e:#}");
                return Err(e.context(format!("minting access token from {origin}")));
            }
        };
        self.store(origin, &minted);
        Ok(minted.access_token)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A token is reused for its advertised lifetime less the safety margin, so
    /// it is always replaced before the server would reject it.
    #[test]
    fn cache_ttl_backs_off_from_the_advertised_expiry() {
        assert_eq!(cache_ttl(Some(3600)), 3600 - EXPIRY_MARGIN_SECS);
    }

    /// No advertised lifetime ⇒ the short assumption, never the documented
    /// 1-hour figure. A compile-time check, since both sides are consts.
    const _: () = assert!(DEFAULT_TTL_SECS < 3600);

    #[test]
    fn cache_ttl_without_an_expiry_falls_back_short() {
        assert_eq!(cache_ttl(None), DEFAULT_TTL_SECS);
    }

    /// A lifetime at or under the margin can't underflow into "cache forever"
    /// (saturating) and can't collapse to zero either — it floors instead.
    #[test]
    fn cache_ttl_floors_a_pathologically_short_expiry() {
        assert_eq!(cache_ttl(Some(10)), MIN_TTL_SECS);
        assert_eq!(cache_ttl(Some(0)), MIN_TTL_SECS);
    }

    #[test]
    fn oauth_base_strips_the_resource_suffix_only() {
        assert_eq!(oauth_base("http://127.0.0.1:8000/mcp"), "http://127.0.0.1:8000");
        assert_eq!(oauth_base("http://127.0.0.1:8000/mcp/"), "http://127.0.0.1:8000");
        assert_eq!(oauth_base("http://127.0.0.1:8000"), "http://127.0.0.1:8000");
    }

    #[test]
    fn cache_round_trips_invalidates_and_reports_the_deadline() {
        let cache = TokenCache::new();
        let origin = "http://127.0.0.1:8999";
        assert_eq!(cache.get(origin), None);
        cache.store(origin, &MintedToken { access_token: "t".into(), expires_in: Some(3600), refresh_token: None });
        assert_eq!(cache.get(origin).as_deref(), Some("t"));
        let deadline = cache.good_until(origin).unwrap();
        assert!(deadline >= now_secs() + 3600 - EXPIRY_MARGIN_SECS - 1);
        cache.invalidate(origin);
        assert_eq!(cache.get(origin), None);
        assert_eq!(cache.good_until(origin), None);
    }

    #[test]
    fn request_id_is_scraped_from_the_consent_markup() {
        let html = r#"<form><input type=hidden name=request_id value="abc-123"><input"#;
        assert_eq!(extract_request_id(html).as_deref(), Some("abc-123"));
        assert_eq!(extract_request_id("<p>no form</p>"), None);
    }

    /// The backoff grows ×4 per attempt and then stops at the ceiling, so a
    /// long outage costs a bounded wait per attempt rather than a doubling one.
    #[test]
    fn backoff_grows_by_four_and_caps() {
        let r = MintRetry::default();
        assert_eq!(r.delay_after(1), Duration::from_millis(500));
        assert_eq!(r.delay_after(2), Duration::from_millis(2000));
        assert_eq!(r.delay_after(3), r.max_delay);
        // Far past any real attempt count: saturating, never wrapping to zero.
        assert_eq!(r.delay_after(u32::MAX), r.max_delay);
    }

    /// A server that is not listening at all is the transient case: the mint is
    /// retried, and the failure that finally surfaces still reads as one.
    #[tokio::test]
    async fn a_refused_connection_is_transient_and_retried() {
        // Bind and drop, so the port is known-free and nothing answers on it.
        let addr = {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            l.local_addr().unwrap()
        };
        let retry = MintRetry {
            attempts: 3,
            base_delay: Duration::from_millis(5),
            max_delay: Duration::from_millis(20),
        };
        let started = std::time::Instant::now();
        let err = mint_access_token_retrying(
            &format!("http://{addr}"),
            "pw",
            &identity(),
            retry,
        )
        .await
        .unwrap_err();

        assert!(is_transient_connection_error(&err), "not classified transient: {err:#}");
        // Two failed attempts slept before the third; anything less means the
        // retry did not happen at all.
        let slept = retry.delay_after(1) + retry.delay_after(2);
        assert!(started.elapsed() >= slept, "returned too fast to have retried");
    }

    /// A server that *answers* — here by rejecting the registration — has given
    /// a settled answer, so it is asked exactly once however many attempts the
    /// policy allows. This is the half that keeps a wrong passphrase from
    /// costing the full backoff before the operator is told.
    #[tokio::test]
    async fn a_rejected_registration_is_not_retried() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let connections = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = connections.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                tokio::spawn(async move {
                    use tokio::io::AsyncWriteExt;
                    let _ = sock
                        .write_all(b"HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\n\r\n")
                        .await;
                });
            }
        });

        let err = mint_access_token_retrying(
            &format!("http://{addr}"),
            "pw",
            &identity(),
            MintRetry {
                attempts: 3,
                base_delay: Duration::from_millis(5),
                max_delay: Duration::from_millis(20),
            },
        )
        .await
        .unwrap_err();

        assert!(!is_transient_connection_error(&err), "a 500 must not read as a socket failure");
        assert_eq!(connections.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    fn identity() -> ClientIdentity {
        ClientIdentity {
            client_name: "test".into(),
            redirect_uri: "http://127.0.0.1/cb".into(),
            scope: "vault.read".into(),
        }
    }

    #[test]
    fn code_and_state_are_parsed_and_an_error_redirect_fails() {
        let (code, state) = parse_code_and_state("http://127.0.0.1/cb?code=C&state=S").unwrap();
        assert_eq!((code.as_str(), state.as_deref()), ("C", Some("S")));
        assert!(parse_code_and_state("http://127.0.0.1/cb?error=access_denied").is_err());
        assert!(parse_code_and_state("http://127.0.0.1/cb?state=S").is_err());
    }

    #[tokio::test]
    async fn premature_eof_restarts_mint_with_a_bound() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let attempts = std::sync::Arc::new(AtomicUsize::new(0));
        let counter = attempts.clone();
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                counter.fetch_add(1, Ordering::SeqCst);
                let mut buf = [0; 4096];
                assert!(socket.read(&mut buf).await.unwrap() > 0);
                socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{").await.unwrap();
                // Drop in the middle of the advertised JSON body.
            }
        });
        let err = mint_access_token_retrying(&base, "pw", &identity(), MintRetry {
            attempts: 3, base_delay: Duration::from_millis(1), max_delay: Duration::from_millis(1),
        }).await.unwrap_err();
        assert!(is_transient_connection_error(&err), "{err:#}");
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        server.abort();
    }

    /// The refresh path shares the mint's transient posture: a server that is
    /// not listening at all is retried with the same bounded backoff.
    #[tokio::test]
    async fn a_refused_connection_on_refresh_is_transient_and_retried() {
        // Bind and drop, so the port is known-free and nothing answers on it.
        let addr = {
            let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            l.local_addr().unwrap()
        };
        let retry = MintRetry {
            attempts: 3,
            base_delay: Duration::from_millis(5),
            max_delay: Duration::from_millis(20),
        };
        let started = std::time::Instant::now();
        let err = refresh_access_token_retrying(
            &format!("http://{addr}"),
            "some-refresh-token",
            "client-1",
            retry,
        )
        .await
        .unwrap_err();

        assert!(is_transient_connection_error(&err), "not classified transient: {err:#}");
        let slept = retry.delay_after(1) + retry.delay_after(2);
        assert!(started.elapsed() >= slept, "returned too fast to have retried");
    }

    /// A settled refusal — invalid_grant, the rotated-away/revoked/expired
    /// case — is answered exactly once and surfaced with the server's own
    /// error words. Retrying it cannot help and must not mask which refusal
    /// it was.
    #[tokio::test]
    async fn a_refresh_refusal_is_surfaced_and_not_retried() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let connections = std::sync::Arc::new(AtomicUsize::new(0));
        let counter = connections.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else { return };
                counter.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(async move {
                    let body = br#"{"error":"invalid_grant","error_description":"unknown or already-used refresh token"}"#;
                    let head = format!(
                        "HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
                        body.len()
                    );
                    let _ = sock.write_all(head.as_bytes()).await;
                    let _ = sock.write_all(&body[..]).await;
                });
            }
        });

        let err = refresh_access_token_retrying(
            &format!("http://{addr}"),
            "rotated-away-token",
            "client-1",
            MintRetry {
                attempts: 3,
                base_delay: Duration::from_millis(5),
                max_delay: Duration::from_millis(20),
            },
        )
        .await
        .unwrap_err();

        assert!(!is_transient_connection_error(&err), "a 400 must not read as a socket failure");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("invalid_grant") && msg.contains("unknown or already-used refresh token"),
            "the server's own error must surface: {msg}"
        );
        assert_eq!(connections.load(Ordering::SeqCst), 1, "a settled refusal must be answered once");
    }
}
