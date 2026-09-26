//! Credential *use*, separated from credential *acquisition*.
//!
//! A consumer holds no login code. A provider's token lives in a file the
//! user owns, mode `0600`, and is read fresh on every request — never cached
//! in a long-lived `String`, never logged. Rotating a token is replacing a
//! file. (Acquisition — for Anthropic, `claude setup-token` — is the
//! `provider-auth` feature, run a few times a year, entirely outside the
//! request path.)

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};

#[derive(Clone, Debug)]
pub enum TokenSource {
    File(PathBuf),
    Env(String),
    /// A Google OAuth refresh token source (SQLite `credentials.db` or JSON file).
    GoogleRefresh {
        path: PathBuf,
        client_id: Option<String>,
        client_secret: Option<String>,
    },
    /// A ChatGPT/Codex OAuth credential: a JSON file holding an access
    /// token, the refresh token that mints the next one, and the account id
    /// (see [`CodexCreds`]). Written by `eidolon chatgpt-login`; refreshed
    /// here, with the rotated refresh token written back — OpenAI rotates
    /// refresh tokens, so a mint that is not persisted is a credential the
    /// next refresh cannot use.
    CodexRefresh { path: PathBuf },
    /// No credential at all (a local endpoint).
    None,
}

pub const DEFAULT_GOOGLE_CLIENT_ID: &str =
    "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
pub const DEFAULT_GOOGLE_CLIENT_SECRET: &str = "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";

/// The public Codex CLI's OAuth client — the id every proxy and the CLI
/// itself present at `auth.openai.com`. A public client has no secret.
pub const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";

/// What a *structural* inspection of a credential source found — never
/// whether the credential works.
///
/// The distinction this type exists for: "a source is here" is not "the token
/// is valid", and the two are asked on paths with very different budgets. A
/// picker draw and a health check need the first answer on a hot path, with no
/// network and no side effects; only the request that presents the token gets
/// the second, from the service itself.
///
/// A consumer renders this; it does not authenticate on it. Saying a merely
/// present credential is valid would make every unauthenticated endpoint look
/// broken and every revoked key look fine until the turn that used it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceState {
    /// The source carries no credential and needs none — a local endpoint.
    NotRequired,
    /// The prerequisite is structurally present. Validity is unverified.
    Present(SourceKind),
    /// Missing, unreadable, empty, or structurally invalid. The diagnostic
    /// names the source and the problem, never a byte of the credential.
    Unusable {
        reason: SourceProblem,
        diagnostic: String,
    },
}

/// Which shape of source a probe found.
///
/// Worth distinguishing even though both answer "something is here": a plain
/// token is presented as it stands, while a refresh source has to be *minted*
/// into an access token before it can be presented at all — so a consumer
/// explaining a failure says a different sentence for each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    /// A file or environment variable holding the bearer itself.
    PlainToken,
    /// A Google OAuth refresh credential (SQLite `credentials.db` or JSON).
    GoogleRefresh,
    /// A ChatGPT/Codex OAuth credential file.
    CodexRefresh,
}

/// Why a source could not be used, as a value a caller can branch on rather
/// than a sentence it has to read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceProblem {
    /// Nothing at that path, or the variable is not set at all.
    Missing,
    /// There, and not openable or not readable.
    Unreadable,
    /// There, readable, and blank.
    Empty,
    /// Readable, non-empty, and not the shape this source requires (no
    /// `refresh_token`, unparseable JSON).
    Malformed,
}

impl std::fmt::Display for SourceProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SourceProblem::Missing => "not present",
            SourceProblem::Unreadable => "unreadable",
            SourceProblem::Empty => "empty",
            SourceProblem::Malformed => "malformed",
        })
    }
}

impl TokenSource {
    /// Inspect this source's local prerequisites, structurally and offline.
    ///
    /// A promise, in the spirit of the secret store's no-read-path invariant:
    /// this never mints a token, refreshes an OAuth credential, spawns a login
    /// CLI, opens a socket, or writes anything. [`TokenSource::read`] — the
    /// request path — does some of those; this is what a picker or a health
    /// check may call without paying for them, so a probe that reaches the
    /// network is a bug, not a slow path.
    ///
    /// Inspection follows the same precedence the read does, so the two agree
    /// about *which* source is in play; the probe only declines to say whether
    /// it works.
    pub fn probe(&self) -> SourceState {
        match self {
            TokenSource::None => SourceState::NotRequired,
            TokenSource::Env(name) => match std::env::var(name) {
                Ok(v) if !v.trim().is_empty() => SourceState::Present(SourceKind::PlainToken),
                Ok(_) => SourceState::Unusable {
                    reason: SourceProblem::Empty,
                    diagnostic: format!("credential env var {name} is empty"),
                },
                Err(_) => SourceState::Unusable {
                    reason: SourceProblem::Missing,
                    diagnostic: format!("credential env var {name} is not set"),
                },
            },
            TokenSource::File(path) => {
                let path = expand(path);
                if let Err(state) = inspect(&path) {
                    return state;
                }
                match std::fs::read_to_string(&path) {
                    Ok(s) if !s.trim().is_empty() => SourceState::Present(SourceKind::PlainToken),
                    Ok(_) => SourceState::Unusable {
                        reason: SourceProblem::Empty,
                        diagnostic: format!("credential file {} is empty", path.display()),
                    },
                    Err(e) => SourceState::Unusable {
                        reason: SourceProblem::Unreadable,
                        diagnostic: format!("reading credential file {}: {e}", path.display()),
                    },
                }
            }
            // A refresh source is inspected by reading its *structure* — the
            // same reader the request path starts from, which stops one step
            // short of the mint. `read_google_creds`/`read_codex_creds` never
            // touch the network; the round trip is what [`TokenSource::read`]
            // does next, and it is exactly what this must not do.
            TokenSource::GoogleRefresh {
                path,
                client_id,
                client_secret,
            } => {
                let path = expand(path);
                if let Err(state) = inspect(&path) {
                    return state;
                }
                match read_google_creds(&path, client_id.as_deref(), client_secret.as_deref()) {
                    Ok(_) => SourceState::Present(SourceKind::GoogleRefresh),
                    Err(e) => SourceState::Unusable {
                        reason: SourceProblem::Malformed,
                        diagnostic: format!("Google credential {}: {e:#}", path.display()),
                    },
                }
            }
            TokenSource::CodexRefresh { path } => {
                let path = expand(path);
                if let Err(state) = inspect(&path) {
                    return state;
                }
                match read_codex_creds(&path) {
                    Ok(_) => SourceState::Present(SourceKind::CodexRefresh),
                    Err(e) => SourceState::Unusable {
                        reason: SourceProblem::Malformed,
                        diagnostic: format!("ChatGPT credential {}: {e:#}", path.display()),
                    },
                }
            }
        }
    }
}

/// Whether a file is there, openable, and non-empty — the three answers that
/// need no parse, and the ones a diagnostic should name before it says
/// "malformed". `Err` is the [`SourceState::Unusable`] to return as-is.
fn inspect(path: &Path) -> Result<(), SourceState> {
    let md = match std::fs::metadata(path) {
        Ok(md) => md,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(SourceState::Unusable {
                reason: SourceProblem::Missing,
                diagnostic: format!("credential file {} does not exist", path.display()),
            });
        }
        Err(e) => {
            return Err(SourceState::Unusable {
                reason: SourceProblem::Unreadable,
                diagnostic: format!("credential file {}: {e}", path.display()),
            });
        }
    };
    if !md.is_file() {
        return Err(SourceState::Unusable {
            reason: SourceProblem::Unreadable,
            diagnostic: format!("credential path {} is not a file", path.display()),
        });
    }
    if md.len() == 0 {
        return Err(SourceState::Unusable {
            reason: SourceProblem::Empty,
            diagnostic: format!("credential file {} is empty", path.display()),
        });
    }
    Ok(())
}

impl TokenSource {
    /// Read the token now. Warns (per call) about a file that other users
    /// can read; refuses an empty token outright.
    pub fn read(&self) -> anyhow::Result<Option<String>> {
        match self {
            TokenSource::None => Ok(None),
            TokenSource::Env(name) => {
                let v = std::env::var(name).with_context(|| format!("credential env var {name} is not set"))?;
                let v = v.trim().to_string();
                if v.is_empty() {
                    bail!("credential env var {name} is empty");
                }
                Ok(Some(v))
            }
            TokenSource::File(path) => {
                let path = expand(path);
                check_mode(&path);
                let s = std::fs::read_to_string(&path)
                    .with_context(|| format!("reading credential file {}", path.display()))?;
                let s = s.trim().to_string();
                if s.is_empty() {
                    bail!("credential file {} is empty", path.display());
                }
                Ok(Some(s))
            }
            TokenSource::GoogleRefresh {
                path,
                client_id,
                client_secret,
            } => {
                let path = expand(path);
                static CACHE: std::sync::LazyLock<
                    std::sync::Mutex<
                        std::collections::HashMap<PathBuf, (String, std::time::Instant)>,
                    >,
                > = std::sync::LazyLock::new(|| {
                    std::sync::Mutex::new(std::collections::HashMap::new())
                });

                if let Ok(cache) = CACHE.lock()
                    && let Some((token, expires_at)) = cache.get(&path)
                    && std::time::Instant::now() < *expires_at
                {
                    return Ok(Some(token.clone()));
                }

                let (refresh_token, cid, csecret) = read_google_creds(
                    &path,
                    client_id.as_deref(),
                    client_secret.as_deref(),
                )?;

                if refresh_token.starts_with("ya29.") {
                    return Ok(Some(refresh_token));
                }

                // The mint is reqwest's blocking transport, and a blocking
                // client panics when one is built inside a live tokio
                // runtime — which is exactly where a provider's token gets
                // read (the async request path). So the round trip runs on
                // a thread of its own, where no runtime is ambient. The
                // expiry cache below keeps that thread rare.
                let mint = std::thread::scope(|s| {
                    s.spawn(|| google_token_mint(&refresh_token, &cid, &csecret))
                        .join()
                        .unwrap_or_else(|_| Err(anyhow::anyhow!("Google token mint thread panicked")))
                })?;

                if mint.status != 200 {
                    bail!("Google token refresh failed (HTTP {}): {}", mint.status, mint.error);
                }
                let access_token = mint.access_token.trim().to_string();
                if access_token.is_empty() {
                    bail!("Google token response contained an empty access_token");
                }
                let expires_in = mint.expires_in.clamp(60, 86400);

                let margin = 60;
                let expires_at = std::time::Instant::now()
                    + std::time::Duration::from_secs(expires_in.saturating_sub(margin));

                if let Ok(mut cache) = CACHE.lock() {
                    cache.insert(path.clone(), (access_token.clone(), expires_at));
                }

                Ok(Some(access_token))
            }
            TokenSource::CodexRefresh { path } => {
                let path = expand(path);
                static CCACHE: std::sync::LazyLock<
                    std::sync::Mutex<
                        std::collections::HashMap<PathBuf, (String, std::time::Instant)>,
                    >,
                > = std::sync::LazyLock::new(|| {
                    std::sync::Mutex::new(std::collections::HashMap::new())
                });

                if let Ok(cache) = CCACHE.lock()
                    && let Some((token, expires_at)) = cache.get(&path)
                    && std::time::Instant::now() < *expires_at
                {
                    return Ok(Some(token.clone()));
                }

                let mut creds = read_codex_creds(&path)?;
                let margin = 60;
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);

                if let (Some(access), Some(expires_at)) = (&creds.access_token, creds.expires_at)
                    && now + margin < expires_at
                    && !access.is_empty()
                {
                    if let Ok(mut cache) = CCACHE.lock() {
                        cache.insert(
                            path.clone(),
                            (access.clone(), std::time::Instant::now() + std::time::Duration::from_secs(expires_at - now - margin)),
                        );
                    }
                    return Ok(Some(access.clone()));
                }

                // The mint is a blocking client on a thread of its own, for
                // the reason the Google mint above is.
                let refresh_token = creds.refresh_token.clone();
                let minted = std::thread::scope(|s| {
                    s.spawn(|| codex_token_refresh(&refresh_token))
                        .join()
                        .unwrap_or_else(|_| Err(anyhow::anyhow!("ChatGPT token refresh thread panicked")))
                })?;

                creds.access_token = Some(minted.access_token);
                if minted.refresh_token.as_deref().is_some_and(|r| !r.is_empty()) {
                    creds.refresh_token = minted.refresh_token.unwrap();
                }
                creds.expires_at = Some(now + minted.expires_in);
                if creds.account_id.is_none() {
                    creds.account_id = minted.account_id;
                }
                let access = creds.access_token.clone().unwrap_or_default();
                write_codex_creds(&path, &creds)?;

                if let Ok(mut cache) = CCACHE.lock() {
                    let ttl = minted.expires_in.saturating_sub(margin).max(60);
                    cache.insert(path.clone(), (access.clone(), std::time::Instant::now() + std::time::Duration::from_secs(ttl)));
                }
                Ok(Some(access))
            }
        }
    }
}

/// The credential file a ChatGPT/Codex login writes and every refresh
/// rewrites. One shape, read here and written by `eidolon chatgpt-login`.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct CodexCreds {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
    pub refresh_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// Unix seconds. Absent means "mint on first use".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
}

fn read_codex_creds(path: &Path) -> anyhow::Result<CodexCreds> {
    check_mode(path);
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading ChatGPT credential file {}", path.display()))?;
    let creds: CodexCreds = serde_json::from_str(&text)
        .with_context(|| format!("parsing ChatGPT credential file {}", path.display()))?;
    if creds.refresh_token.trim().is_empty() {
        bail!("ChatGPT credential file {} has no refresh_token; run `eidolon chatgpt-login`", path.display());
    }
    Ok(creds)
}

fn write_codex_creds(path: &Path, creds: &CodexCreds) -> anyhow::Result<()> {
    let text = serde_json::to_string_pretty(creds)?;
    crate::fs::write_atomic_0600(path, text.as_bytes())
        .with_context(|| format!("writing ChatGPT credential file {}", path.display()))
}

/// The account id a credential file names, without minting anything — for
/// the `ChatGPT-Account-Id` header, which is optional.
pub fn codex_account_id(path: &Path) -> Option<String> {
    let path = expand(path);
    read_codex_creds(&path).ok().and_then(|c| c.account_id)
}

struct CodexMinted {
    access_token: String,
    refresh_token: Option<String>,
    account_id: Option<String>,
    expires_in: u64,
}

/// One blocking refresh against `auth.openai.com`. Fails with a re-login
/// instruction on `invalid_grant` — a rotated-out refresh token is the one
/// failure an operator must act on, and the message should say so.
fn codex_token_refresh(refresh_token: &str) -> anyhow::Result<CodexMinted> {
    #[derive(serde::Deserialize)]
    struct TokenResp {
        access_token: Option<String>,
        #[serde(default)]
        refresh_token: Option<String>,
        #[serde(default)]
        id_token: Option<String>,
        #[serde(default)]
        expires_in: Option<serde_json::Value>,
    }
    #[derive(serde::Deserialize)]
    struct ErrResp {
        #[serde(default)]
        error: Option<String>,
        #[serde(default)]
        error_description: Option<String>,
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;
    let resp = client
        .post("https://auth.openai.com/oauth/token")
        .header("User-Agent", "codex_cli_rs/0.55.0")
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", CODEX_CLIENT_ID),
        ])
        .send()
        .with_context(|| "POST https://auth.openai.com/oauth/token failed")?;
    let status = resp.status().as_u16();
    let text = resp.text().unwrap_or_default();
    if status != 200 {
        let e: ErrResp = serde_json::from_str(&text).unwrap_or(ErrResp { error: None, error_description: None });
        if e.error.as_deref() == Some("invalid_grant") {
            bail!("ChatGPT login expired (invalid_grant): run `eidolon chatgpt-login` again");
        }
        bail!(
            "ChatGPT token refresh failed (HTTP {status}): {}",
            e.error_description.or(e.error).unwrap_or_else(|| text.chars().take(500).collect())
        );
    }
    let t: TokenResp = serde_json::from_str(&text)
        .with_context(|| format!("parsing ChatGPT token response: {text}"))?;
    let access_token = t.access_token.filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("ChatGPT token response contained no access_token"))?;
    let expires_in = match &t.expires_in {
        Some(serde_json::Value::Number(n)) => n.as_u64().unwrap_or(3600).clamp(60, 86400),
        Some(serde_json::Value::String(s)) => s.parse().unwrap_or(3600).clamp(60, 86400),
        _ => 3600,
    };
    let account_id = t
        .id_token
        .as_deref()
        .and_then(codex_account_from_jwt)
        .or_else(|| codex_account_from_jwt(&access_token));
    Ok(CodexMinted { access_token, refresh_token: t.refresh_token, account_id, expires_in })
}

/// The `chatgpt_account_id` claim out of a JWT — top-level or nested under
/// the namespaced auth object, which is where OpenAI puts it more often.
pub fn codex_account_from_jwt(token: &str) -> Option<String> {
    use base64::Engine as _;
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(payload).ok()?;
    let val: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    val.get("chatgpt_account_id").and_then(|v| v.as_str()).map(String::from)
        .or_else(|| {
            val.get("https://api.openai.com/auth")
                .and_then(|a| a.get("chatgpt_account_id"))
                .and_then(|v| v.as_str())
                .map(String::from)
        })
}

struct Minted {
    status: u16,
    access_token: String,
    error: String,
    expires_in: u64,
}

/// One blocking round trip to the Google token endpoint, run on a thread
/// with no ambient tokio runtime (see the caller).
fn google_token_mint(
    refresh_token: &str,
    client_id: &str,
    client_secret: &str,
) -> anyhow::Result<Minted> {
    #[derive(serde::Deserialize)]
    struct TokenResp {
        access_token: Option<String>,
        #[serde(default)]
        expires_in: u64,
        #[serde(default)]
        error: Option<String>,
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;
    let resp = client
        .post("https://oauth2.googleapis.com/token")
        .form(&[
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
        ])
        .send()
        .with_context(|| "POST https://oauth2.googleapis.com/token failed")?;
    let status = resp.status().as_u16();
    let text = resp
        .text()
        .unwrap_or_default()
        .trim()
        .to_string();
    if status != 200 {
        return Ok(Minted {
            status,
            access_token: String::new(),
            error: text,
            expires_in: 0,
        });
    }
    match serde_json::from_str::<TokenResp>(&text) {
        Ok(t) => Ok(Minted {
            status,
            access_token: t.access_token.unwrap_or_default(),
            error: t.error.unwrap_or_default(),
            expires_in: if t.expires_in == 0 { 3600 } else { t.expires_in },
        }),
        Err(e) => anyhow::bail!("parsing Google token response JSON: {e}: {text}"),
    }
}

fn read_google_creds(
    path: &Path,
    custom_cid: Option<&str>,
    custom_csecret: Option<&str>,
) -> anyhow::Result<(String, String, String)> {
    let mut refresh_token = String::new();
    let mut cid = custom_cid.unwrap_or("").to_string();
    let mut csecret = custom_csecret.unwrap_or("").to_string();

    let is_db = path
        .extension()
        .is_some_and(|ext| ext == "db" || ext == "sqlite")
        || path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().contains("credentials.db"));

    if is_db {
        #[cfg(feature = "rusqlite")]
        {
            use rusqlite::OpenFlags;
            let conn = rusqlite::Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .with_context(|| format!("opening SQLite db {}", path.display()))?;

            let query = "SELECT credential_data FROM antigravity_credentials ORDER BY id DESC LIMIT 1";
            let raw_json: String = conn
                .query_row(query, [], |row| row.get(0))
                .with_context(|| format!("querying antigravity_credentials in {}", path.display()))?;

            let val: serde_json::Value =
                serde_json::from_str(&raw_json).context("parsing credential_data JSON")?;
            if let Some(rt) = val.get("refresh_token").and_then(|v| v.as_str()) {
                refresh_token = rt.to_string();
            }
            if cid.is_empty()
                && let Some(c) = val.get("client_id").and_then(|v| v.as_str())
            {
                cid = c.to_string();
            }
            if csecret.is_empty()
                && let Some(cs) = val.get("client_secret").and_then(|v| v.as_str())
            {
                csecret = cs.to_string();
            }
        }
        #[cfg(not(feature = "rusqlite"))]
        {
            bail!("rusqlite feature required to read SQLite db {}", path.display());
        }
    } else {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("reading JSON credential file {}", path.display()))?;
        let val: serde_json::Value =
            serde_json::from_str(&content).context("parsing JSON credential file")?;
        if let Some(rt) = val.get("refresh_token").and_then(|v| v.as_str()) {
            refresh_token = rt.to_string();
        } else if let Some(s) = val.as_str() {
            refresh_token = s.trim().to_string();
        }
        if cid.is_empty()
            && let Some(c) = val.get("client_id").and_then(|v| v.as_str())
        {
            cid = c.to_string();
        }
        if csecret.is_empty()
            && let Some(cs) = val.get("client_secret").and_then(|v| v.as_str())
        {
            csecret = cs.to_string();
        }
    }

    if refresh_token.is_empty() {
        bail!("no refresh_token found in {}", path.display());
    }
    if cid.is_empty() {
        cid = DEFAULT_GOOGLE_CLIENT_ID.to_string();
    }
    if csecret.is_empty() {
        csecret = DEFAULT_GOOGLE_CLIENT_SECRET.to_string();
    }

    Ok((refresh_token, cid, csecret))
}

/// A leading `~` is the user's home.
fn expand(p: &Path) -> PathBuf {
    if let Ok(rest) = p.strip_prefix("~")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    p.to_path_buf()
}

fn check_mode(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(md) = std::fs::metadata(path) {
        let mode = md.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            tracing::warn!(path = %path.display(), mode = format!("{mode:o}"), "credential file is readable by others; chmod 600 it");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_tokens_are_trimmed_and_empty_ones_refused() {
        let dir = crate::testutil::tempdir("credentials");
        let path = dir.join("token");
        crate::fs::write_atomic_0600(&path, b"  sk-test-123\n").unwrap();
        assert_eq!(TokenSource::File(path.clone()).read().unwrap().as_deref(), Some("sk-test-123"));
        crate::fs::write_atomic_0600(&path, b"\n").unwrap();
        assert!(TokenSource::File(path).read().is_err());
        assert!(TokenSource::File(dir.join("missing")).read().is_err());
        assert_eq!(TokenSource::None.read().unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn google_refresh_creds_read_from_json() {
        let dir = crate::testutil::tempdir("google_creds");
        let path = dir.join("creds.json");
        let content = r#"{
            "refresh_token": "1//test_rt_123",
            "client_id": "custom_cid",
            "client_secret": "custom_secret"
        }"#;
        std::fs::write(&path, content).unwrap();

        let (rt, cid, cs) = read_google_creds(&path, None, None).unwrap();
        assert_eq!(rt, "1//test_rt_123");
        assert_eq!(cid, "custom_cid");
        assert_eq!(cs, "custom_secret");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tilde_expands_to_home() {
        let home = std::env::var_os("HOME").expect("HOME set in tests");
        assert_eq!(expand(Path::new("~/x/token")), PathBuf::from(home).join("x/token"));
        assert_eq!(expand(Path::new("/abs/token")), PathBuf::from("/abs/token"));
    }

    #[test]
    fn codex_creds_round_trip_and_empty_refresh_is_refused() {
        let dir = crate::testutil::tempdir("codex_creds");
        let path = dir.join("chatgpt.json");
        let creds = CodexCreds {
            access_token: Some("at".into()),
            refresh_token: "rt".into(),
            account_id: Some("acc_1".into()),
            expires_at: Some(2_000_000_000),
        };
        write_codex_creds(&path, &creds).unwrap();
        let back = read_codex_creds(&path).unwrap();
        assert_eq!(back.account_id.as_deref(), Some("acc_1"));
        assert_eq!(codex_account_id(&path).as_deref(), Some("acc_1"));

        std::fs::write(&path, r#"{"access_token":"x","refresh_token":""}"#).unwrap();
        let err = read_codex_creds(&path).unwrap_err().to_string();
        assert!(err.contains("chatgpt-login"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_account_id_comes_out_of_a_jwt_either_way_it_is_nested() {
        use base64::Engine as _;
        let b64 = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s);
        let top = format!("{}.{}.sig", b64("{}"), b64(r#"{"chatgpt_account_id":"acc_top"}"#));
        let nested = format!("{}.{}.sig", b64("{}"), b64(r#"{"https://api.openai.com/auth":{"chatgpt_account_id":"acc_nested"}}"#));
        assert_eq!(codex_account_from_jwt(&top).as_deref(), Some("acc_top"));
        assert_eq!(codex_account_from_jwt(&nested).as_deref(), Some("acc_nested"));
        assert_eq!(codex_account_from_jwt("not-a-jwt"), None);
    }

    /// The probe answers the three questions that need no network — present,
    /// absent, blank — and says which, with a diagnostic that names the source
    /// and nothing else.
    #[test]
    fn a_probe_names_presence_and_problem_without_reading_for_use() {
        let dir = crate::testutil::tempdir("probe_static");
        let path = dir.join("token");
        assert_eq!(TokenSource::None.probe(), SourceState::NotRequired);
        assert!(matches!(
            TokenSource::File(dir.join("missing")).probe(),
            SourceState::Unusable { reason: SourceProblem::Missing, .. }
        ));
        crate::fs::write_atomic_0600(&path, b"\n").unwrap();
        assert!(matches!(
            TokenSource::File(path.clone()).probe(),
            SourceState::Unusable { reason: SourceProblem::Empty, .. }
        ));
        crate::fs::write_atomic_0600(&path, b"  sk-test-123\n").unwrap();
        assert_eq!(
            TokenSource::File(path.clone()).probe(),
            SourceState::Present(SourceKind::PlainToken)
        );
        // A directory is not a credential, and a probe says so rather than
        // reading it and failing obscurely.
        assert!(matches!(
            TokenSource::File(dir.clone()).probe(),
            SourceState::Unusable { reason: SourceProblem::Unreadable, .. }
        ));

        let name = "HARNOX_TEST_PROBE_UNSET";
        unsafe { std::env::remove_var(name) };
        assert!(matches!(
            TokenSource::Env(name.into()).probe(),
            SourceState::Unusable { reason: SourceProblem::Missing, .. }
        ));
        unsafe { std::env::set_var(name, "  ") };
        assert!(matches!(
            TokenSource::Env(name.into()).probe(),
            SourceState::Unusable { reason: SourceProblem::Empty, .. }
        ));
        unsafe { std::env::set_var(name, "sk-env-secret") };
        let state = TokenSource::Env(name.into()).probe();
        assert_eq!(state, SourceState::Present(SourceKind::PlainToken));
        // The value is never in the answer, whichever way it goes.
        assert!(!format!("{state:?}").contains("sk-env-secret"));
        unsafe { std::env::remove_var(name) };

        // An unreadable file is "unreadable", not "missing" — mode 000, which
        // root ignores, so this half is only asserted where the mode actually
        // bites.
        let locked = dir.join("locked");
        crate::fs::write_atomic_0600(&locked, b"x").unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read_to_string(&locked).is_err() {
            assert!(matches!(
                TokenSource::File(locked.clone()).probe(),
                SourceState::Unusable { reason: SourceProblem::Unreadable, .. }
            ));
        }
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o600)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A refresh source is probed from its *structure*, one step short of the
    /// mint: a refresh token that is there is present, one that is not is
    /// malformed, and nothing in the answer is a byte of the file. If the probe
    /// ever minted, this test would need the network — which it must not.
    #[test]
    fn a_refresh_probe_reads_the_credential_without_minting_it() {
        let dir = crate::testutil::tempdir("probe_refresh");
        let google = dir.join("creds.json");
        std::fs::write(
            &google,
            r#"{"refresh_token":"1//SUPERSECRET_GOOGLE_RT","client_id":"c","client_secret":"s"}"#,
        )
        .unwrap();
        let state = TokenSource::GoogleRefresh {
            path: google.clone(),
            client_id: None,
            client_secret: None,
        }
        .probe();
        assert_eq!(state, SourceState::Present(SourceKind::GoogleRefresh));
        assert!(!format!("{state:?}").contains("SUPERSECRET_GOOGLE_RT"));

        // Present, non-empty, and no refresh token in it: malformed, with the
        // parse problem named and the contents not.
        std::fs::write(&google, r#"{"client_id":"c","session":"SUPERSECRET_GOOGLE_RT"}"#).unwrap();
        let state = TokenSource::GoogleRefresh {
            path: google.clone(),
            client_id: None,
            client_secret: None,
        }
        .probe();
        assert!(
            matches!(state, SourceState::Unusable { reason: SourceProblem::Malformed, .. }),
            "{state:?}"
        );
        assert!(!format!("{state:?}").contains("SUPERSECRET_GOOGLE_RT"));

        // Truncated JSON: a parse failure, still redacted.
        std::fs::write(&google, r#"{"refresh_token": "SUPERSECRET_GOOGLE_RT"#).unwrap();
        let state = TokenSource::GoogleRefresh {
            path: google.clone(),
            client_id: None,
            client_secret: None,
        }
        .probe();
        assert!(
            matches!(state, SourceState::Unusable { reason: SourceProblem::Malformed, .. }),
            "{state:?}"
        );
        assert!(!format!("{state:?}").contains("SUPERSECRET_GOOGLE_RT"));

        // The Codex shape, the same two ways.
        let codex = dir.join("chatgpt.json");
        write_codex_creds(
            &codex,
            &CodexCreds {
                access_token: None,
                refresh_token: "SUPERSECRET_CODEX_RT".into(),
                account_id: None,
                expires_at: None,
            },
        )
        .unwrap();
        let state = TokenSource::CodexRefresh { path: codex.clone() }.probe();
        assert_eq!(state, SourceState::Present(SourceKind::CodexRefresh));
        assert!(!format!("{state:?}").contains("SUPERSECRET_CODEX_RT"));

        std::fs::write(&codex, r#"{"refresh_token":""}"#).unwrap();
        let state = TokenSource::CodexRefresh { path: codex.clone() }.probe();
        assert!(
            matches!(state, SourceState::Unusable { reason: SourceProblem::Malformed, .. }),
            "{state:?}"
        );

        // Absent and blank are answered before any parse is attempted.
        assert!(matches!(
            TokenSource::CodexRefresh { path: dir.join("nope.json") }.probe(),
            SourceState::Unusable { reason: SourceProblem::Missing, .. }
        ));
        std::fs::write(&codex, b"").unwrap();
        assert!(matches!(
            TokenSource::CodexRefresh { path: codex }.probe(),
            SourceState::Unusable { reason: SourceProblem::Empty, .. }
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
