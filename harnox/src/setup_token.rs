//! Driving `claude setup-token` to mint a long-lived Anthropic bearer (feature
//! `provider-auth`).
//!
//! The `claude` CLI authenticates headless via `claude setup-token`, which
//! mints a **long-lived OAuth token** (valid ~1yr, prefix `sk-ant-oat01-`)
//! that is supplied through the `CLAUDE_CODE_OAUTH_TOKEN` env var — it does
//! *not* write `~/.claude/.credentials.json`. The flow is interactive browser
//! OAuth: `setup-token` prints an authorize URL, the human signs in, and pastes
//! a `code#state` blob back into the **same** live process, which then prints
//! the minted token. This module first-classes that dance: [`start`] spawns
//! the process and hands back the URL plus a [`Session`]; [`Session::submit_code`]
//! feeds the pasted code in and hands back the [`OauthToken`].
//!
//! This is not an OAuth client. It is PTY automation around someone else's
//! TUI, and it couples to that TUI's rendering rather than to the protocol —
//! a far more fragile surface. The consolation is what it produces: a bearer
//! good for about a year, so a consumer's auth is one header re-minted
//! roughly annually, by hand in two minutes if this ever breaks. **Credential
//! acquisition** (here) is separate from **credential use** (a token file
//! read at call time); a consumer needs no login code of its own.
//!
//! What to do with the token — persist it to an env file, smoke-test it,
//! relay the URL to a chat — is the consumer's; this module never touches
//! disk and never logs the token.
//!
//! ## The hard-won recipe (encoded here, don't rediscover it)
//!
//! Each of these was a real failure during a by-hand run:
//!
//! - **Drive it under a real PTY** ([`portable_pty`]), never a plain pipe:
//!   `setup-token` is a raw-mode TUI that detects a non-interactive stdin and
//!   refuses.
//! - **Set a wide PTY** (`>=1000` cols) before spawning, or the printed 108-char
//!   token wraps across lines and is corrupted. (`ssh -tt` propagates the size
//!   to a remote PTY, so a consumer running `setup-token` on another box over
//!   SSH benefits identically.)
//! - **Extract the authorize URL from the OSC-8 hyperlink escape**
//!   (`ESC ] 8 ; ; <uri> ST`) when there is one, else off the rendered screen —
//!   never from the raw visible text, which is line-wrapped.
//! - **Submit the pasted code terminated with a carriage return `\r`**, not
//!   `\n`: the raw-mode TUI treats Enter as `\r` and won't submit on `\n` (a
//!   stray `\n` also produces an OAuth 400).
//! - **Reconstruct the token through a real terminal emulator** ([`vt100`]), not
//!   a "strip ANSI + regex" scrape: TUI backspace/cursor redraws silently *drop
//!   characters* from a naive scrape (`sk-ant-at0…` instead of
//!   `sk-ant-oat01-…`, a dropped `o` → `401 Invalid bearer token`). Feeding the
//!   raw PTY byte stream through vt100 and reading the *rendered screen* gives
//!   the exact token.
//!
//! ## Why a session lives in memory, not on disk
//!
//! The OAuth/PKCE state lives inside a **live child process**, so if the
//! caller (or the child) dies the authorize URL is dead with it and there is
//! nothing durable to resume. A [`Session`] is therefore an in-memory handle;
//! don't "fix" this into a file — durability would be a lie.

use std::io::{Read, Write};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use tokio::sync::{mpsc, oneshot};

/// The minted token's fixed prefix. A real token is this plus ~95 URL-safe
/// characters (108 total).
pub const TOKEN_PREFIX: &str = "sk-ant-oat01-";

/// Minimum length of the token *tail* (after [`TOKEN_PREFIX`]) we'll accept.
/// A real tail is ~95 chars; anything much shorter is a half-rendered redraw we
/// must not accept (the `sk-ant-at0…`-style corruption is far shorter). Set
/// well below the real length but far above any partial paint.
const MIN_TOKEN_TAIL: usize = 80;

/// A deliberately wide PTY so the 108-char token prints on one line instead of
/// wrapping (wrapping corrupts the scrape).
const PTY_COLS: u16 = 1000;
const PTY_ROWS: u16 = 50;

/// How long [`start`] waits for `setup-token` to print the authorize URL.
const URL_WAIT: Duration = Duration::from_secs(60);
/// How long a started session waits for the human to paste the `code#state`
/// back. OAuth codes expire fast; a stale session is reaped after this.
const CODE_WAIT: Duration = Duration::from_secs(15 * 60);
/// How long we wait, after feeding the code, for the token to render.
const TOKEN_WAIT: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// The minted token — a redacting newtype
// ---------------------------------------------------------------------------

/// A minted `sk-ant-oat01-…` OAuth token. Its `Debug`/`Display` are **redacted**
/// so an accidental `tracing::info!(?token)` or `{token}` can never leak it —
/// the raw value is reachable only through [`OauthToken::expose`].
#[derive(Clone, PartialEq, Eq)]
pub struct OauthToken(String);

impl OauthToken {
    /// The raw token. Call only where it must cross a boundary (writing the env
    /// file, the smoke-test env var); never log the result.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// A safe-to-show summary: the prefix plus the length, never the body.
    pub fn masked(&self) -> String {
        format!("{TOKEN_PREFIX}…({} chars)", self.0.len())
    }
}

impl std::fmt::Debug for OauthToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "OauthToken({})", self.masked())
    }
}

impl std::fmt::Display for OauthToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.masked())
    }
}

// ---------------------------------------------------------------------------
// Pure parsing: authorize URL + token reconstruction
// ---------------------------------------------------------------------------

/// Extract the authorize URL from a raw PTY byte stream. Tries two encodings, in
/// order of reliability:
///
/// 1. An **OSC-8 hyperlink escape** (`ESC ] 8 ; <params> ; <uri> ST`), if the
///    CLI emits one — forward-compat for a version that hyperlinks the URL.
/// 2. The **rendered screen** (this is what `claude` v2.1.x actually does): the
///    URL is printed as plain, colour-wrapped text. The wide PTY keeps it on a
///    single line (wrapping would corrupt it), so rendering the raw bytes
///    through vt100 and scanning the resulting line recovers the exact URL.
///
/// Pure, so both encodings are unit-testable against a recorded stream.
pub fn extract_authorize_url(raw: &[u8]) -> Option<String> {
    extract_osc8_url(raw).or_else(|| scan_rendered_url(raw))
}

/// The OSC-8 hyperlink case — `ESC ] 8 ; <params> ; <uri> ST` (ST = `ESC \` or
/// `BEL`). Returns the first hyperlink URI that looks like an `http(s)` link.
fn extract_osc8_url(raw: &[u8]) -> Option<String> {
    const OSC8: &[u8] = b"\x1b]8;";
    let mut i = 0;
    while i + OSC8.len() <= raw.len() {
        if &raw[i..i + OSC8.len()] != OSC8 {
            i += 1;
            continue;
        }
        // Skip the params field (up to the next ';'), then read the URI until ST.
        let after = i + OSC8.len();
        let Some(semi) = raw[after..].iter().position(|&b| b == b';') else {
            break;
        };
        let uri_start = after + semi + 1;
        let mut j = uri_start;
        while j < raw.len() {
            // ST as BEL, or as ESC '\'.
            if raw[j] == 0x07 || (raw[j] == 0x1b && raw.get(j + 1) == Some(&b'\\')) {
                break;
            }
            j += 1;
        }
        if let Ok(uri) = std::str::from_utf8(&raw[uri_start..j]) {
            let uri = uri.trim();
            if uri.starts_with("http") {
                return Some(uri.to_string());
            }
        }
        i = j.max(i + 1);
    }
    None
}

/// The rendered-text case: render the raw stream through vt100 and scan each
/// line for an `https://` URL, preferring an OAuth/authorize one (there can be
/// other links on screen). The wide PTY keeps the URL un-wrapped, so a whole
/// line is one intact URL.
fn scan_rendered_url(raw: &[u8]) -> Option<String> {
    let mut parser = vt100::Parser::new(PTY_ROWS, PTY_COLS, 0);
    parser.process(raw);
    let contents = parser.screen().contents();
    let mut fallback = None;
    for line in contents.lines() {
        let Some(pos) = line.find("https://") else { continue };
        let url: String = line[pos..].chars().take_while(|c| !c.is_whitespace()).collect();
        if url.contains("authorize") || url.contains("oauth") {
            return Some(url);
        }
        fallback.get_or_insert(url);
    }
    fallback
}

/// Reconstruct the minted token from a raw PTY byte stream by rendering it
/// through a real terminal emulator (vt100) and reading the resulting screen —
/// the only reliable way past the TUI's backspace/cursor redraws. Pure, so the
/// dropped-character corruption is directly regression-testable.
pub fn scrape_token(raw: &[u8]) -> Option<OauthToken> {
    let mut parser = vt100::Parser::new(PTY_ROWS, PTY_COLS, 0);
    parser.process(raw);
    find_token_in(&parser.screen().contents())
}

/// Find a full `sk-ant-oat01-…` token in already-rendered screen text. Scans
/// each line for the prefix and takes the run of URL-safe token characters that
/// follows; a match is accepted only when that run is long enough to be a whole
/// token (guarding against a half-painted redraw).
fn find_token_in(rendered: &str) -> Option<OauthToken> {
    for line in rendered.lines() {
        let Some(pos) = line.find(TOKEN_PREFIX) else { continue };
        let tail: String = line[pos + TOKEN_PREFIX.len()..]
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .collect();
        if tail.len() >= MIN_TOKEN_TAIL {
            return Some(OauthToken(format!("{TOKEN_PREFIX}{tail}")));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

/// What [`start`] hands back: the authorize URL to open, and the live session
/// to feed the pasted code into.
pub struct Started {
    pub authorize_url: String,
    pub session: Session,
}

/// A started-but-unresolved `setup-token` process, waiting for the human's
/// `code#state`. Consumed by [`Session::submit_code`]; dropping it abandons
/// the flow (the driver reaps the child when its code wait elapses).
pub struct Session {
    /// Send the pasted `code#state` to the driver.
    code_tx: oneshot::Sender<String>,
    /// Receive the reconstructed token (or a failure reason) from the driver.
    token_rx: oneshot::Receiver<std::result::Result<OauthToken, String>>,
}

impl Session {
    /// Whether the driver is still waiting for a code. False once the child
    /// exited or the code wait elapsed — a registry of sessions can prune on
    /// this rather than holding dead handles against a concurrency cap.
    pub fn is_alive(&self) -> bool {
        !self.code_tx.is_closed()
    }

    /// Feed the pasted `code#state` into the waiting process and reconstruct
    /// the minted token off its rendered screen. Errors name the phase that
    /// failed and never include the token.
    pub async fn submit_code(self, code: &str) -> Result<OauthToken> {
        if self.code_tx.send(code.trim().to_string()).is_err() {
            bail!("the setup-token process is no longer running (it exited, or timed out waiting for the code)");
        }
        match self.token_rx.await {
            Ok(Ok(token)) => Ok(token),
            Ok(Err(msg)) => bail!("{msg}"),
            Err(_) => bail!("the setup-token driver exited unexpectedly"),
        }
    }
}

/// Spawn `program args…` (typically `claude setup-token`, or `ssh -tt <box>
/// claude setup-token`) under a wide PTY and return once it prints the
/// authorize URL. Any inherited `CLAUDE_CODE_OAUTH_TOKEN` is removed from the
/// child's environment so an already-authed host can't short-circuit the mint.
pub async fn start(program: &str, args: &[String]) -> Result<Started> {
    let pair = native_pty_system()
        .openpty(PtySize { rows: PTY_ROWS, cols: PTY_COLS, pixel_width: 0, pixel_height: 0 })
        .context("opening a PTY for setup-token")?;
    let mut cmd = CommandBuilder::new(program);
    cmd.args(args);
    cmd.env_remove("CLAUDE_CODE_OAUTH_TOKEN");
    let child = pair.slave.spawn_command(cmd).with_context(|| format!("spawning `{program}` under a PTY"))?;
    let reader = pair.master.try_clone_reader().context("cloning the PTY reader")?;
    let writer = pair.master.take_writer().context("taking the PTY writer")?;
    // Drop the slave so the master reader sees EOF once the child exits.
    drop(pair.slave);
    let master = pair.master;

    // Bridge the blocking PTY reads onto an async channel via a dedicated thread.
    let (byte_tx, byte_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut reader = reader;
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if byte_tx.send(buf[..n].to_vec()).is_err() {
                        break; // driver gone
                    }
                }
                Err(_) => break,
            }
        }
    });

    let (url_tx, url_rx) = oneshot::channel();
    let (code_tx, code_rx) = oneshot::channel();
    let (token_tx, token_rx) = oneshot::channel();

    let pty = PtyHandles { writer, child, _master: master };
    let chans = DriverChannels { url_tx, code_rx, token_tx };
    tokio::spawn(drive(byte_rx, pty, chans));

    let authorize_url = match url_rx.await {
        Ok(Ok(url)) => url,
        Ok(Err(msg)) => bail!("{msg}"),
        Err(_) => bail!("the setup-token driver exited before returning an authorize URL"),
    };

    Ok(Started { authorize_url, session: Session { code_tx, token_rx } })
}

/// The live child + PTY handles the driver owns. `_master` is held only to keep
/// the PTY open for the lifetime of the read loop (the reader is an independent
/// clone).
struct PtyHandles {
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    _master: Box<dyn portable_pty::MasterPty + Send>,
}

/// The driver's three signalling channels: hand back the URL, receive the pasted
/// code, hand back the reconstructed token.
struct DriverChannels {
    url_tx: oneshot::Sender<std::result::Result<String, String>>,
    code_rx: oneshot::Receiver<String>,
    token_tx: oneshot::Sender<std::result::Result<OauthToken, String>>,
}

/// The live PTY driver: read until the authorize URL, hand it back, wait for the
/// pasted code, feed it with a carriage return, then reconstruct the token off
/// the rendered screen. Always kills the child on exit.
async fn drive(mut byte_rx: mpsc::UnboundedReceiver<Vec<u8>>, pty: PtyHandles, chans: DriverChannels) {
    let PtyHandles { writer, mut child, _master } = pty;
    let DriverChannels { url_tx, code_rx, token_tx } = chans;
    let mut raw: Vec<u8> = Vec::new();

    // Phase 1 — the authorize URL.
    let url = accumulate_until(&mut byte_rx, &mut raw, URL_WAIT, extract_authorize_url).await;
    let Some(url) = url else {
        let _ = url_tx.send(Err("timed out waiting for the authorize URL from `claude setup-token`".to_string()));
        let _ = child.kill();
        return;
    };
    if url_tx.send(Ok(url)).is_err() {
        // start() gave up waiting; nothing to complete.
        let _ = child.kill();
        return;
    }

    // Phase 2 — the pasted code#state (bounded so a never-answered session is
    // reaped rather than pinning a live child forever).
    let code = match tokio::time::timeout(CODE_WAIT, code_rx).await {
        Ok(Ok(code)) => code,
        _ => {
            let _ = child.kill();
            return;
        }
    };

    // Feed the code with a CARRIAGE RETURN (\r) — the raw-mode TUI won't submit
    // on \n, and a stray \n yields an OAuth 400. Blocking write, off-thread.
    let write = tokio::task::spawn_blocking(move || {
        let mut writer = writer;
        writer.write_all(format!("{}\r", code.trim()).as_bytes())?;
        writer.flush()?;
        Ok::<(), std::io::Error>(())
    })
    .await;
    if let Err(msg) = flatten_write(write) {
        let _ = token_tx.send(Err(format!("couldn't send the code to setup-token: {msg}")));
        let _ = child.kill();
        return;
    }

    // Phase 3 — the minted token, reconstructed off the rendered vt100 screen.
    let token = accumulate_until(&mut byte_rx, &mut raw, TOKEN_WAIT, scrape_token).await;
    let _ = child.kill();
    let _ = match token {
        Some(t) => token_tx.send(Ok(t)),
        None => token_tx.send(Err(
            "setup-token finished but no valid sk-ant-oat01- token appeared on screen".to_string(),
        )),
    };
}

/// Read PTY chunks into `raw` until `probe` yields a value or `overall` elapses
/// (or the child closes the stream). `probe` sees the whole accumulated raw byte
/// stream each time — both the URL extraction (OSC-8 escape) and the token
/// reconstruction (vt100 render) work off the raw bytes.
async fn accumulate_until<T>(
    byte_rx: &mut mpsc::UnboundedReceiver<Vec<u8>>,
    raw: &mut Vec<u8>,
    overall: Duration,
    mut probe: impl FnMut(&[u8]) -> Option<T>,
) -> Option<T> {
    let deadline = tokio::time::Instant::now() + overall;
    loop {
        if let Some(v) = probe(raw) {
            return Some(v);
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match tokio::time::timeout(remaining, byte_rx.recv()).await {
            Ok(Some(chunk)) => raw.extend_from_slice(&chunk),
            // Stream closed (child exited): one last probe, then give up.
            Ok(None) => return probe(raw),
            Err(_) => return None,
        }
    }
}

/// Flatten a `spawn_blocking` write result (join error / io error) to a message.
fn flatten_write(
    r: std::result::Result<std::result::Result<(), std::io::Error>, tokio::task::JoinError>,
) -> std::result::Result<(), String> {
    match r {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(e.to_string()),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a raw stream with an OSC-8 hyperlink, terminated by ST (`ESC \`).
    fn osc8(uri: &str) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"\x1b]8;;");
        v.extend_from_slice(uri.as_bytes());
        v.extend_from_slice(b"\x1b\\");
        v
    }

    #[test]
    fn extracts_the_authorize_url_from_the_osc8_escape() {
        let url = "https://claude.ai/oauth/authorize?code=1&state=abc";
        let mut raw = b"Visible wrapped te\r\nxt of the url...\r\n".to_vec();
        raw.extend_from_slice(&osc8(url));
        assert_eq!(extract_authorize_url(&raw).as_deref(), Some(url));
    }

    #[test]
    fn extracts_a_plain_colored_url_from_the_rendered_screen() {
        // The shape `claude` v2.1.x actually emits: no OSC-8 hyperlink, just the
        // URL as SGR-coloured plain text on its own line (this is what the real
        // capture showed — the OSC-8-only path timed out against it).
        let url = "https://claude.com/cai/oauth/authorize?code=true&client_id=abc&state=XYZ123";
        let raw = format!(
            "\x1b[38;2;153;153;153mBrowser didn't open? Use the url below\x1b[39m\r\n\
             \r\n\x1b[38;2;153;153;153m{url}\x1b[39m\r\n\r\nPaste code here >\r\n"
        );
        assert_eq!(extract_authorize_url(raw.as_bytes()).as_deref(), Some(url));
    }

    #[test]
    fn extract_url_ignores_a_bare_osc8_with_no_http_uri() {
        // An OSC-8 close (empty uri) must not be mistaken for the link.
        let raw = osc8(""); // ESC ] 8 ; ; ESC \
        assert_eq!(extract_authorize_url(&raw), None);
    }

    #[test]
    fn extract_url_is_none_without_a_hyperlink() {
        assert_eq!(extract_authorize_url(b"no escapes here"), None);
    }

    #[test]
    fn reconstructs_the_token_across_a_backspace_redraw() {
        // Reproduce the real corruption: the TUI prints `sk-ant-oat01`, backs up
        // over the stray leading `x`, and continues — a naive strip+regex drops
        // characters, but the rendered screen is exact.
        let tail = "A".repeat(95);
        let mut raw = Vec::new();
        raw.extend_from_slice(b"Your token:\r\n");
        raw.extend_from_slice(b"x"); // stray char
        raw.extend_from_slice(b"\x08 \x08"); // backspace, space, backspace (erase it)
        raw.extend_from_slice(format!("{TOKEN_PREFIX}{tail}").as_bytes());
        let token = scrape_token(&raw).expect("token should reconstruct");
        assert_eq!(token.expose(), format!("{TOKEN_PREFIX}{tail}"));
    }

    #[test]
    fn scrape_rejects_a_too_short_partial_token() {
        // A half-rendered `sk-ant-at0…` fragment must not be accepted.
        let raw = b"sk-ant-oat01-tooshort\r\n".to_vec();
        assert!(scrape_token(&raw).is_none());
    }

    #[test]
    fn oauth_token_never_reveals_itself_in_debug_or_display() {
        let tok = OauthToken(format!("{TOKEN_PREFIX}{}", "S".repeat(95)));
        let shown = format!("{tok} / {tok:?}");
        assert!(!shown.contains(&"S".repeat(95)), "the body must never render: {shown}");
        assert!(shown.contains("108 chars"), "should summarise length: {shown}");
    }

    /// A session whose driver has gone reports itself dead and fails a submit
    /// with the "no longer running" message rather than hanging.
    #[tokio::test]
    async fn a_dead_session_is_reported_not_awaited_forever() {
        let (code_tx, code_rx) = oneshot::channel::<String>();
        let (_token_tx, token_rx) = oneshot::channel();
        let session = Session { code_tx, token_rx };
        assert!(session.is_alive());
        drop(code_rx);
        assert!(!session.is_alive());
        let err = session.submit_code("code#state").await.unwrap_err().to_string();
        assert!(err.contains("no longer running"), "got: {err}");
    }
}
