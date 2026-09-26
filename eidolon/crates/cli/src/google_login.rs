use anyhow::{Context, bail};
use percent_encoding::{NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub const GOOGLE_CLIENT_ID: &str =
    "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
pub const GOOGLE_CLIENT_SECRET: &str = "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";

pub async fn run_google_login() -> anyhow::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("binding loopback listener for Google OAuth callback")?;
    let port = listener.local_addr()?.port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");

    let scopes = [
        "https://www.googleapis.com/auth/cloud-platform",
        "https://www.googleapis.com/auth/userinfo.email",
        "https://www.googleapis.com/auth/userinfo.profile",
        "https://www.googleapis.com/auth/cclog",
        "https://www.googleapis.com/auth/experimentsandconfigs",
    ]
    .join(" ");

    let auth_url = format!(
        "https://accounts.google.com/o/oauth2/v2/auth?\
client_id={GOOGLE_CLIENT_ID}&\
redirect_uri={}&\
response_type=code&\
scope={}&\
access_type=offline&\
prompt=consent",
        utf8_percent_encode(&redirect_uri, NON_ALPHANUMERIC),
        utf8_percent_encode(&scopes, NON_ALPHANUMERIC)
    );

    println!("Open the following URL in your browser to authorize Google Antigravity:\n");
    println!("{auth_url}\n");
    println!("Waiting for OAuth callback on port {port}...");

    let (mut stream, _) = listener
        .accept()
        .await
        .context("accepting OAuth callback connection")?;

    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf).await?;
    let req_str = String::from_utf8_lossy(&buf[..n]);

    let code = match extract_code(&req_str) {
        Some(c) => c,
        None => {
            let resp = "HTTP/1.1 400 Bad Request\r\nContent-Type: text/plain\r\n\r\nMissing authorization code.";
            let _ = stream.write_all(resp.as_bytes()).await;
            bail!("OAuth callback did not contain authorization code");
        }
    };

    let html = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n<html><body><h2>Login successful!</h2><p>You may close this tab and return to your terminal.</p></body></html>";
    let _ = stream.write_all(html.as_bytes()).await;

    println!("Received authorization code. Minting refresh token...");

    let client = reqwest::Client::new();
    let res = client
        .post("https://oauth2.googleapis.com/token")
        .form(&[
            ("client_id", GOOGLE_CLIENT_ID),
            ("client_secret", GOOGLE_CLIENT_SECRET),
            ("code", &code),
            ("grant_type", "authorization_code"),
            ("redirect_uri", &redirect_uri),
        ])
        .send()
        .await
        .context("exchanging code at https://oauth2.googleapis.com/token")?;

    if !res.status().is_success() {
        let err_text = res.text().await.unwrap_or_default();
        bail!("Token exchange failed: {err_text}");
    }

    #[derive(serde::Deserialize)]
    struct TokenResp {
        access_token: Option<String>,
        refresh_token: Option<String>,
    }

    let token_data: TokenResp = res.json().await.context("parsing token JSON response")?;
    let refresh_token = token_data
        .refresh_token
        .ok_or_else(|| anyhow::anyhow!("Response contained no refresh_token (did you grant offline access?)"))?;

    save_credentials(&refresh_token, &token_data.access_token)?;

    println!("Successfully logged in! Credential saved.");
    Ok(())
}

fn extract_code(req: &str) -> Option<String> {
    let line = req.lines().next()?;
    let path = line.split_whitespace().nth(1)?;
    let query = path.split('?').nth(1)?;
    for pair in query.split('&') {
        let mut kv = pair.split('=');
        if let (Some(k), Some(v)) = (kv.next(), kv.next())
            && k == "code"
        {
            return percent_decode_str(v).decode_utf8().ok().map(|s| s.into_owned());
        }
    }
    None
}

fn save_credentials(refresh_token: &str, access_token: &Option<String>) -> anyhow::Result<()> {
    let creds_dir = if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".local/state/gcli2api/creds")
    } else {
        PathBuf::from(".local/state/gcli2api/creds")
    };
    std::fs::create_dir_all(&creds_dir)?;

    let db_path = creds_dir.join("credentials.db");
    let json_val = serde_json::json!({
        "client_id": GOOGLE_CLIENT_ID,
        "client_secret": GOOGLE_CLIENT_SECRET,
        "refresh_token": refresh_token,
        "access_token": access_token,
        "project_id": "aicode-consumers"
    });
    let json_str = serde_json::to_string(&json_val)?;

    let conn = rusqlite::Connection::open(&db_path)?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS antigravity_credentials (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            filename TEXT NOT NULL,
            credential_data TEXT NOT NULL,
            disabled INTEGER DEFAULT 0,
            enable_credit INTEGER DEFAULT 1
        )",
        [],
    )?;

    conn.execute(
        "INSERT INTO antigravity_credentials (filename, credential_data, disabled, enable_credit) VALUES ('ag_login.json', ?, 0, 1)",
        [&json_str],
    )?;

    // Also write ~/.config/eidolon/antigravity.json for direct use
    if let Some(home) = std::env::var_os("HOME") {
        let config_dir = PathBuf::from(home).join(".config/eidolon");
        std::fs::create_dir_all(&config_dir)?;
        let json_path = config_dir.join("antigravity.json");
        harnox::fs::write_atomic_0600(&json_path, json_str.as_bytes())?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_code_parses_callback_path_and_decodes_utf8() {
        let req = "GET /callback?state=xyz123&code=4%2F0AeaYSHC_test_code_abc%20123 HTTP/1.1\r\nHost: 127.0.0.1:8085\r\n\r\n";
        assert_eq!(extract_code(req).as_deref(), Some("4/0AeaYSHC_test_code_abc 123"));
    }
}
