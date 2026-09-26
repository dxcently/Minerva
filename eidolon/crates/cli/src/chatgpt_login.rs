//! `eidolon chatgpt-login` — the device flow against auth.openai.com that
//! mints the credential the `chatgpt` provider rides (see
//! `harnox::llm::credentials::CodexCreds` for the file's shape and
//! `TokenSource::CodexRefresh` for how it is spent).
//!
//! No browser callback and no local listener: the flow hands the operator a
//! URL and a code, polls until the code is approved, and exchanges what
//! comes back. The client is the Codex CLI's own public OAuth client —
//! a public client has no secret, and it is the only one the backend's
//! tokens authorize.

use anyhow::{Context, bail};
use std::path::PathBuf;
use std::time::Duration;

const AUTH: &str = "https://auth.openai.com";

pub async fn run_chatgpt_login() -> anyhow::Result<()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .context("building HTTP client")?;

    // 1. Ask for a user code.
    #[derive(serde::Deserialize)]
    struct UserCode {
        user_code: String,
        device_auth_id: String,
        #[serde(default)]
        interval: Option<serde_json::Value>,
        #[serde(default)]
        verification_url: Option<String>,
    }
    let uc: UserCode = client
        .post(format!("{AUTH}/api/accounts/deviceauth/usercode"))
        .header("User-Agent", "codex_cli_rs/0.55.0")
        .json(&serde_json::json!({ "client_id": harnox::llm::CODEX_CLIENT_ID }))
        .send()
        .await
        .context("starting device login at auth.openai.com")?
        .error_for_status()
        .context("auth.openai.com refused the device-login request")?
        .json()
        .await
        .context("parsing device-login response")?;

    let url = uc.verification_url.clone()
        .unwrap_or_else(|| format!("{AUTH}/codex/device"));
    let interval = match &uc.interval {
        Some(serde_json::Value::Number(n)) => n.as_u64().unwrap_or(5).clamp(3, 15),
        Some(serde_json::Value::String(s)) => s.parse().unwrap_or(5).clamp(3, 15),
        _ => 5,
    };

    println!("Log in with your ChatGPT account (the one holding the Codex allowance):\n");
    println!("  1. open  {url}");
    println!("  2. enter this code:  {}\n", uc.user_code);
    println!("Polling every {interval}s until approved (Ctrl-C to give up)...");

    // 2. Poll until the code is approved. 403/404 mean "not yet".
    #[derive(serde::Deserialize)]
    struct Polled {
        authorization_code: String,
        code_verifier: String,
    }
    let approved = loop {
        let resp = client
            .post(format!("{AUTH}/api/accounts/deviceauth/token"))
            .header("User-Agent", "codex_cli_rs/0.55.0")
            .json(&serde_json::json!({
                "device_auth_id": uc.device_auth_id,
                "user_code": uc.user_code,
            }))
            .send()
            .await
            .context("polling device login")?;
        match resp.status() {
            reqwest::StatusCode::OK => {
                break resp.json::<Polled>().await.context("parsing approved device-login response")?;
            }
            reqwest::StatusCode::FORBIDDEN | reqwest::StatusCode::NOT_FOUND => {}
            s => bail!("device login ended with HTTP {s}"),
        }
        tokio::time::sleep(Duration::from_secs(interval)).await;
    };

    println!("Approved. Exchanging for tokens...");

    // 3. Exchange the authorization code (PKCE verifier came from step 1's
    //    poll response — the auth server holds it, not us).
    #[derive(serde::Deserialize)]
    struct Tokens {
        access_token: String,
        #[serde(default)]
        refresh_token: Option<String>,
        #[serde(default)]
        id_token: Option<String>,
        #[serde(default)]
        expires_in: Option<u64>,
    }
    let t: Tokens = client
        .post(format!("{AUTH}/oauth/token"))
        .header("User-Agent", "codex_cli_rs/0.55.0")
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", approved.authorization_code.as_str()),
            ("redirect_uri", "https://auth.openai.com/deviceauth/callback"),
            ("client_id", harnox::llm::CODEX_CLIENT_ID),
            ("code_verifier", approved.code_verifier.as_str()),
        ])
        .send()
        .await
        .context("exchanging authorization code")?
        .error_for_status()
        .context("auth.openai.com refused the code exchange")?
        .json()
        .await
        .context("parsing token response")?;

    let refresh_token = t.refresh_token.filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("token response contained no refresh_token"))?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let expires_at = now + t.expires_in.unwrap_or(3600).clamp(60, 86400);
    let account_id = t
        .id_token
        .as_deref()
        .and_then(harnox::llm::codex_account_from_jwt)
        .or_else(|| harnox::llm::codex_account_from_jwt(&t.access_token));

    let creds = harnox::llm::CodexCreds {
        access_token: Some(t.access_token),
        refresh_token,
        account_id: account_id.clone(),
        expires_at: Some(expires_at),
    };
    let path = credential_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    harnox::fs::write_atomic_0600(&path, serde_json::to_string_pretty(&creds)?.as_bytes())
        .with_context(|| format!("writing {}", path.display()))?;

    println!("Logged in. Credential at {}.", path.display());
    if let Some(id) = account_id {
        println!("Account: {id}");
    }
    println!("Pick a model with `ed -m chatgpt:gpt-6-astra`.");
    Ok(())
}

/// Where the credential lives — the built-in `chatgpt` provider's
/// `token_file`, written here so the two can never drift.
pub fn credential_path() -> anyhow::Result<PathBuf> {
    let home = std::env::var_os("HOME").context("$HOME is not set")?;
    Ok(PathBuf::from(home).join(".config/eidolon/chatgpt.json"))
}
