//! `fake_eidolon` — a stand-in for `eidolon web`, for the perch's tests.
//!
//! A real door is WSL-only, heavy, and wants a model; the perch's tests must not depend on
//! one. This binary is the door's **contract** and nothing else (hub.md §12), modelled on
//! eidolon at `0194f2b` (see `README.md` beside this file for what that means):
//!
//! - the argv `eidolon web --bind ADDR (--cwd DIR | --session PATH) [-m MODEL]`, and the
//!   argv is recorded so a test can show there is no token in it;
//! - the two boot lines, exactly: `listening on http://<addr>/` then `token file: <path>`;
//! - its own token, minted here, written `0600` to `$XDG_RUNTIME_DIR/eidolon/web-<port>.token`
//!   and never printed (`crates/web/src/lib.rs:120-137`);
//! - the door's `/api` routes, each behind that token (`Authorization: Bearer`, else a 401 with
//!   an empty body) and, on a POST, behind the `Host` check (it must be the door's own bound
//!   address, else a 403, `serve.rs:172`):
//!   `GET /api/events` (`hello`, then every frame broadcast to the door's streams),
//!   `POST /api/say` (202 `{"queued":false}`; a body with a `documents` key is a 422),
//!   `POST /api/answer` (204, and an `ask-settled` frame, with the `note` if one came, goes to
//!   every open stream), `POST /api/cancel` (409), `POST /api/launch` (200 `{"opened":..}`),
//!   and a 404 for any other path;
//! - request bodies, read by `Content-Length` or `Transfer-Encoding: chunked` (hyper sends
//!   either), refused with a 413 past 8 MiB as the door's `MAX_BODY` does, and never parsed
//!   beyond what a route above needs: images are **not** validated, because the perch must not
//!   either, and the bytes are what a test compares;
//! - every request's method, path and `Authorization` is appended to `$FAKE_LOG`, followed by
//!   one `hdr ...` line with the headers a proxy must or must not forward, then (once the body
//!   is in) `body-len=N`, with the body bytes written to `$FAKE_LOG.body`;
//! - **SIGTERM as the real door behaves**: while a stream is open it does *not* exit (hyper's
//!   `GracefulShutdown` waits the connection out, which is minutes), and once no stream is
//!   open it removes its token file and exits. That is what makes "close the streams before
//!   SIGTERM" the difference between a clean stop and a 10 s hang plus a SIGKILL.
//!
//! Knobs, all by environment (§12): `FAKE_LOG`, `FAKE_SESSIONS_DIR`, `FAKE_SILENT`,
//! `FAKE_MODE=0644`, `FAKE_TOKEN_ELSEWHERE`, `FAKE_EXIT_AFTER_MS`, `FAKE_IGNORE_SIGTERM`,
//! `FAKE_LATE_PANE_MS`, `FAKE_EXIT_CODE` (the code `FAKE_EXIT_AFTER_MS` exits with, default 1),
//! `FAKE_EXIT_BEFORE_BOOT=<code>` (exit before any boot line), and
//! `FAKE_REFUSE=<path>` (a 401 on that path even with the right token).
//!
//! Built only with `--features test-bins` (`required-features` in Cargo.toml), so a normal
//! `cargo build` ships no test double.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::broadcast;

/// The door's `MAX_BODY` (`serve.rs:51`).
const MAX_BODY: usize = 8 * 1024 * 1024;

/// Everything one fake door knows about itself.
struct Fake {
    token: String,
    token_file: PathBuf,
    port: u16,
    /// The log the door says it opened: `<sessions dir>/<id>.eid` for `--cwd`, the given
    /// path for `--session`.
    session: PathBuf,
    cwd: PathBuf,
    /// The file every request is recorded to, if a test asked for one.
    log: Option<PathBuf>,
    /// How many `/api/events` streams are open right now. SIGTERM waits for this to be zero.
    streams: AtomicUsize,
    /// How many `/api/events` streams have ever been opened (the first is the perch's watcher).
    opened: AtomicUsize,
    /// `FAKE_LATE_PANE_MS`: the head of the third and later `/api/events` streams (the pages'
    /// panes; the first is the watcher and the second a pane that is read normally) is held back
    /// this long, with the stream already counted as open. A pane the perch is still setting up
    /// when a stop lands, and so one the stop must wait for.
    late_pane: Option<Duration>,
    /// `FAKE_IGNORE_SIGTERM`: never exit on SIGTERM, so the perch's SIGKILL path and its
    /// token-file cleanup are the only things that end this door.
    ignore_sigterm: bool,
    /// `FAKE_REFUSE`: a path that answers 401 whatever token it is given.
    refuse: Option<String>,
    /// Every frame a route broadcasts goes out to every open `/api/events` stream, as the
    /// door's own broadcast does. Raw bytes, so a half frame can be sent as one.
    frames: broadcast::Sender<Vec<u8>>,
}

impl Fake {
    fn record(&self, line: &str) {
        let Some(path) = &self.log else { return };
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{line}");
        }
    }

    /// `<FAKE_LOG>.body`: the last request body, byte for byte.
    fn write_body(&self, body: &[u8]) {
        let Some(path) = &self.log else { return };
        let mut name = path.as_os_str().to_os_string();
        name.push(".body");
        let _ = std::fs::write(PathBuf::from(name), body);
    }
}

fn main() {
    // A door prints two lines and nothing else on stdout, ever (§10.4: nothing a child
    // prints may carry a token). Diagnostics would go to stderr, which the perch inherits.
    let argv: Vec<String> = std::env::args().collect();
    let args = match Args::parse(&argv) {
        Ok(args) => args,
        Err(why) => {
            eprintln!("fake_eidolon: {why}");
            std::process::exit(2);
        }
    };

    // A tiny runtime: the door serves a few connections at a time in tests, and nothing here
    // needs more than net + signal + time.
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("a tokio runtime");
    runtime.block_on(serve(args, argv));
}

/// The door's argv, as much of it as this stand-in must understand. An unknown flag is a
/// mistake in whatever built the argv, so it is a refusal rather than something ignored.
struct Args {
    bind: std::net::SocketAddr,
    session: Option<PathBuf>,
    cwd: Option<PathBuf>,
    model: Option<String>,
}

impl Args {
    fn parse(argv: &[String]) -> Result<Self, String> {
        let mut rest = argv.iter().skip(1);
        match rest.next().map(String::as_str) {
            Some("web") => {}
            other => return Err(format!("expected the `web` subcommand, got {other:?}")),
        }
        let (mut bind, mut session, mut cwd, mut model) = (None, None, None, None);
        while let Some(arg) = rest.next() {
            let mut value = |name: &str| rest.next().cloned().ok_or_else(|| format!("{name} needs a value"));
            match arg.as_str() {
                "--bind" => bind = Some(value("--bind")?.parse().map_err(|e| format!("--bind: {e}"))?),
                "--session" => session = Some(PathBuf::from(value("--session")?)),
                "--cwd" => cwd = Some(PathBuf::from(value("--cwd")?)),
                "-m" | "--model" => model = Some(value("--model")?),
                other => return Err(format!("unknown argument {other:?}")),
            }
        }
        let bind = bind.ok_or("--bind is required")?;
        if session.is_some() == cwd.is_some() {
            return Err("exactly one of --session or --cwd is required".into());
        }
        Ok(Self { bind, session, cwd, model })
    }
}

async fn serve(args: Args, argv: Vec<String>) {
    let listener = TcpListener::bind(args.bind).await.expect("binding the fake door");
    let port = listener.local_addr().expect("the bound address").port();

    // The door's own runtime dir, at the mode the perch checks: another user must not be
    // able to read the token file it is about to write, let alone the directory.
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR");
    let token_dir = PathBuf::from(&runtime_dir).join("eidolon");
    std::fs::create_dir_all(&token_dir).expect("creating the door's runtime dir");
    owner_only(&token_dir, 0o700);

    let token = harnox::crypto::random_token();
    // `FAKE_TOKEN_ELSEWHERE`: a path outside `<runtime>/eidolon`, which the perch must refuse
    // rather than open. Named apart from the real one so it can never coincide with it.
    let token_file = if std::env::var_os("FAKE_TOKEN_ELSEWHERE").is_some() {
        std::env::temp_dir().join(format!("fake-door-{port}.token"))
    } else {
        token_dir.join(format!("web-{port}.token"))
    };
    write_token(&token_file, &token);

    // The session log. `--session` names an existing one; `--cwd` is a new session, whose log
    // the door names — the perch learns the id from `hello.session` and from nowhere else
    // (hub.md §4 step 7).
    let session = match (&args.session, &args.cwd) {
        (Some(log), _) => log.clone(),
        (None, Some(_cwd)) => {
            // A real door names a new session's log under its own configured sessions dir;
            // `FAKE_SESSIONS_DIR` is how a test points this stand-in at the same one the perch
            // was told about, and the door's own default is the fallback.
            let dir = std::env::var_os("FAKE_SESSIONS_DIR").map(PathBuf::from).unwrap_or_else(|| {
                std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(".")).join(".local/share/eidolon/sessions")
            });
            std::fs::create_dir_all(&dir).expect("creating the sessions dir");
            let log = dir.join(format!("fake-{}.eid", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()));
            std::fs::write(&log, "{\"fake\":true}\n").expect("writing the session log");
            log
        }
        (None, None) => unreachable!("Args::parse refused this"),
    };
    let cwd = args.cwd.clone().or_else(|| session.parent().map(Path::to_path_buf)).expect("a cwd");

    let fake = Arc::new(Fake {
        token,
        token_file: token_file.clone(),
        port,
        session,
        cwd,
        log: std::env::var_os("FAKE_LOG").map(PathBuf::from),
        streams: AtomicUsize::new(0),
        opened: AtomicUsize::new(0),
        late_pane: std::env::var("FAKE_LATE_PANE_MS").ok().and_then(|ms| ms.parse().ok()).map(Duration::from_millis),
        ignore_sigterm: std::env::var_os("FAKE_IGNORE_SIGTERM").is_some(),
        refuse: std::env::var("FAKE_REFUSE").ok().filter(|path| !path.is_empty()),
        frames: broadcast::channel(64).0,
    });

    // Recorded before the boot lines: the port and pid a test needs to check /proc and the
    // token file, and the argv it must show carries no token. The argv is a test's business,
    // so it is the one line that repeats the process's own words; a door's token is not in
    // them (the perch never passes one) and the token minted above is never here.
    fake.record(&format!("start pid={} port={port}", std::process::id()));
    fake.record(&format!("argv {}", argv[1..].join(" ")));
    fake.record(&format!("model {}", args.model.as_deref().unwrap_or("-")));

    // `FAKE_EXIT_BEFORE_BOOT=<code>`: a door that ends before it says anything (no boot lines),
    // with that exit code. With 0 it is the "exit 0 while `starting`" case, which is a failed
    // spawn and not a stop.
    if let Some(code) = std::env::var("FAKE_EXIT_BEFORE_BOOT").ok().and_then(|code| code.parse::<i32>().ok()) {
        let _ = std::fs::remove_file(&token_file);
        fake.record("exit-before-boot");
        std::process::exit(code);
    }

    // A silent door drives §4 step 5's startup timeout: nothing is printed, so the perch
    // never learns a port, and it kills this process (which removes its own token file).
    if std::env::var_os("FAKE_SILENT").is_none() {
        println!("listening on http://127.0.0.1:{port}/");
        println!("token file: {}", token_file.display());
    }

    if let Some(ms) = std::env::var_os("FAKE_EXIT_AFTER_MS") {
        let ms: u64 = ms.to_string_lossy().parse().expect("FAKE_EXIT_AFTER_MS is a number");
        let fake = fake.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(ms)).await;
            // A door that dies mid-frame: half a frame, no terminator, to every open stream,
            // and a moment for it to reach the socket before the process goes. The page must
            // be told how the stream ended even though the door never finished a frame.
            let _ = fake.frames.send(b"data: {\"type\":\"turn-st".to_vec());
            tokio::time::sleep(Duration::from_millis(50)).await;
            // A door that dies of its own accord leaves its token file behind, exactly as a
            // crashed one does: the perch's child-exit path has to leave no orphan either.
            fake.record("exit-now");
            // `FAKE_EXIT_CODE` (default 1): 0 is a door that stops on its own accord, cleanly.
            let code = std::env::var("FAKE_EXIT_CODE").ok().and_then(|code| code.parse::<i32>().ok()).unwrap_or(1);
            std::process::exit(code);
        });
    }

    tokio::spawn(accept(listener, fake.clone()));
    let _ = on_sigterm(fake).await;
}

/// SIGTERM, as the real door answers it. With a stream open the door *waits* — hyper's
/// `GracefulShutdown` will not cut an open SSE response, and neither will this — and only
/// then removes its token file and exits. With no stream open it is immediate.
async fn on_sigterm(fake: Arc<Fake>) {
    let mut term = signal(SignalKind::terminate()).expect("listening for SIGTERM");
    let mut int = signal(SignalKind::interrupt()).expect("listening for SIGINT");
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
    }
    // The count when the signal lands, before the window below: this is the line that says
    // whether the perch closed its streams *before* it signalled, where `sigterm streams=N`
    // only says whether they were closed by the end of the window. A beat of 20 ms lets this
    // process read a close that the kernel delivered just ahead of the signal; it is far short
    // of the window, and of `FAKE_LATE_PANE_MS`, so a stream the perch did not wait for is
    // still counted.
    tokio::time::sleep(Duration::from_millis(20)).await;
    fake.record(&format!("sigterm-received streams={}", fake.streams.load(Ordering::SeqCst)));
    if fake.ignore_sigterm {
        fake.record("sigterm-ignored");
        // Never exits: the perch must SIGKILL this door and remove the token file itself.
        // The handlers stay installed and keep swallowing the signal, so no default action
        // ever ends the process.
        loop {
            tokio::select! {
                _ = term.recv() => {}
                _ = int.recv() => {}
            }
        }
    }
    // A real door's graceful shutdown starts by giving its open responses a moment to end,
    // and the perch has already closed its streams by the time this signal arrives — the
    // kernel's close and this process's read of it are two different events, so this window
    // is what makes "closed first" observable from in here rather than a scheduling race.
    let window = Instant::now() + Duration::from_millis(500);
    while fake.streams.load(Ordering::SeqCst) > 0 && Instant::now() < window {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    fake.record(&format!("sigterm streams={}", fake.streams.load(Ordering::SeqCst)));
    // A stream the perch holds open is its watcher or a pane; it is closed by the perch (or
    // killed with this process), and until then this door does not exit.
    while fake.streams.load(Ordering::SeqCst) > 0 {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let _ = std::fs::remove_file(&fake.token_file);
    fake.record("exit");
    std::process::exit(0);
}

async fn accept(listener: TcpListener, fake: Arc<Fake>) {
    loop {
        match listener.accept().await {
            Ok((sock, _peer)) => {
                let fake = fake.clone();
                tokio::spawn(async move {
                    let _ = connection(sock, fake).await;
                });
            }
            // Nothing here ends this loop: the door lives until it is signalled or exits.
            Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
        }
    }
}

/// How reading a body ended, when it did not end well.
enum Bad {
    /// Past [`MAX_BODY`]: the door's 413.
    TooBig,
    /// The peer went away, or sent something that is not HTTP/1.1.
    Gone,
}

/// Read more from the socket into `buf`, or say the peer is gone.
async fn fill(sock: &mut TcpStream, buf: &mut Vec<u8>) -> Result<(), Bad> {
    let mut chunk = vec![0u8; 256 * 1024];
    match sock.read(&mut chunk).await {
        Ok(0) | Err(_) => Err(Bad::Gone),
        Ok(n) => {
            buf.extend_from_slice(&chunk[..n]);
            Ok(())
        }
    }
}

/// One CRLF-terminated line off the front of `buf`, without the CRLF.
async fn line(sock: &mut TcpStream, buf: &mut Vec<u8>) -> Result<String, Bad> {
    loop {
        if let Some(at) = buf.windows(2).position(|w| w == b"\r\n") {
            let text = String::from_utf8_lossy(&buf[..at]).to_string();
            buf.drain(..at + 2);
            return Ok(text);
        }
        if buf.len() > 64 * 1024 {
            return Err(Bad::Gone);
        }
        fill(sock, buf).await?;
    }
}

/// The request body: `Transfer-Encoding: chunked` if the head says so, else `Content-Length`
/// bytes, else nothing. `buf` holds whatever followed the head already.
async fn read_body(sock: &mut TcpStream, mut buf: Vec<u8>, headers: &[(String, String)]) -> Result<Vec<u8>, Bad> {
    let header = |name: &str| headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str());
    if header("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked")) {
        let mut body = Vec::new();
        loop {
            let size_line = line(sock, &mut buf).await?;
            let size = usize::from_str_radix(size_line.split(';').next().unwrap_or("").trim(), 16).map_err(|_| Bad::Gone)?;
            if size == 0 {
                // Trailers, if any, up to the blank line.
                while !line(sock, &mut buf).await?.is_empty() {}
                return Ok(body);
            }
            if body.len() + size > MAX_BODY {
                return Err(Bad::TooBig);
            }
            while buf.len() < size + 2 {
                fill(sock, &mut buf).await?;
            }
            body.extend_from_slice(&buf[..size]);
            buf.drain(..size + 2);
        }
    }
    let want: usize = header("content-length").and_then(|v| v.trim().parse().ok()).unwrap_or(0);
    if want > MAX_BODY {
        return Err(Bad::TooBig);
    }
    while buf.len() < want {
        fill(sock, &mut buf).await?;
    }
    buf.truncate(want);
    Ok(buf)
}

/// One HTTP/1.1 connection, by hand: a test double does not need hyper, and hand-parsing
/// keeps "hold the stream open" and "wait for the peer to close" explicit.
async fn connection(mut sock: TcpStream, fake: Arc<Fake>) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let head_end = loop {
        if let Some(at) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
        if buf.len() > 64 * 1024 || fill(&mut sock, &mut buf).await.is_err() {
            return Ok(());
        }
    };
    let after_head = buf.split_off(head_end);
    let text = String::from_utf8_lossy(&buf).to_string();
    let mut lines = text.lines();
    let request = lines.next().unwrap_or_default().to_string();
    let mut method_and_path = request.split_whitespace();
    let method = method_and_path.next().unwrap_or_default().to_string();
    let path = method_and_path.next().unwrap_or_default().to_string();
    // Header names go on the wire in lower case (hyper writes them that way), so the lookup
    // is case-insensitive: names are folded here, once.
    let headers: Vec<(String, String)> =
        lines.filter_map(|line| line.split_once(':')).map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string())).collect();
    let header = |name: &str| headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.clone());
    let auth = header("authorization").unwrap_or_else(|| "-".to_string());
    fake.record(&format!("{method} {path} auth={auth}"));
    let seen = |name: &str| header(name).unwrap_or_else(|| "-".to_string());
    fake.record(&format!(
        "hdr host={} content-type={} accept={} cookie={} origin={} referer={} sec-fetch-site={} last-event-id={}",
        seen("host"),
        seen("content-type"),
        seen("accept"),
        seen("cookie"),
        seen("origin"),
        seen("referer"),
        seen("sec-fetch-site"),
        seen("last-event-id"),
    ));

    // The body, if the request has one, is read before anything is answered: a door that
    // answered and closed with the rest of a body unread would reset the connection and the
    // peer would never read the answer. It is the POSTs that have bodies.
    let mut body = Vec::new();
    if method == "POST" {
        match read_body(&mut sock, after_head, &headers).await {
            Ok(read) => {
                fake.record(&format!("body-len={}", read.len()));
                fake.write_body(&read);
                body = read;
            }
            Err(Bad::TooBig) => {
                // A body the perch should have cut off before the door ever saw it: a test can ask.
                fake.record("body-refused");
                return reply(&mut sock, "413 Payload Too Large", "", "text/plain").await;
            }
            Err(Bad::Gone) => return Ok(()),
        }
    }

    // The door's gates on every /api route. A path outside /api is not the door's at all.
    if !path.starts_with("/api/") {
        return reply(&mut sock, "404 Not Found", "no such route", "text/plain").await;
    }
    // The token gate, exactly as the real one's: `Authorization: Bearer`, constant time not
    // required here, 401 with an empty body. `FAKE_REFUSE` is a path that 401s regardless.
    if auth != format!("Bearer {}", fake.token) || fake.refuse.as_deref() == Some(path.as_str()) {
        return reply(&mut sock, "401 Unauthorized", "", "text/plain").await;
    }
    // `serve.rs:172`: a POST whose Host is not the door's own bound address is refused. The
    // perch's rewrite of Host is what satisfies this.
    if method == "POST" && header("host").as_deref() != Some(format!("127.0.0.1:{}", fake.port).as_str()) {
        return reply(&mut sock, "403 Forbidden", "{\"error\":\"bad host\"}", "application/json").await;
    }

    match (method.as_str(), path.as_str()) {
        ("GET", "/api/events") => events(&mut sock, &fake).await,
        ("POST", "/api/say") => {
            // Images are not validated, and a `documents` key is the one thing this door
            // refuses by name (`attach.rs`: "this door takes images only").
            let refused = serde_json::from_slice::<serde_json::Value>(&body).ok().is_some_and(|v| v.get("documents").is_some());
            if refused {
                let error = "{\"error\":\"this door takes images only: documents are not accepted\"}";
                reply(&mut sock, "422 Unprocessable Entity", error, "application/json").await
            } else {
                reply(&mut sock, "202 Accepted", "{\"queued\":false}", "application/json").await
            }
        }
        ("POST", "/api/answer") => {
            let Some(asked) = serde_json::from_slice::<serde_json::Value>(&body).ok() else {
                return reply(&mut sock, "400 Bad Request", "{\"error\":\"not json\"}", "application/json").await;
            };
            let (Some(ask_id), Some(answer)) = (asked.get("ask_id").filter(|v| v.is_number()), asked.get("answer").and_then(|v| v.as_str())) else {
                return reply(&mut sock, "400 Bad Request", "{\"error\":\"ask_id and answer\"}", "application/json").await;
            };
            let mut settled = serde_json::json!({ "type": "ask-settled", "ask_id": ask_id, "how": "answered", "answer": answer });
            if let Some(note) = asked.get("note").and_then(|v| v.as_str()) {
                settled["note"] = serde_json::Value::String(note.to_string());
            }
            let _ = fake.frames.send(format!("data: {settled}\n\n").into_bytes());
            reply(&mut sock, "204 No Content", "", "text/plain").await
        }
        ("POST", "/api/cancel") => reply(&mut sock, "409 Conflict", "{\"error\":\"no turn is running\"}", "application/json").await,
        ("POST", "/api/launch") => {
            let window = serde_json::from_slice::<serde_json::Value>(&body).ok().and_then(|v| v.get("window").and_then(|w| w.as_str().map(str::to_string)));
            let opened = serde_json::json!({ "opened": format!("fake {}", window.as_deref().unwrap_or("-")) });
            reply(&mut sock, "200 OK", &opened.to_string(), "application/json").await
        }
        _ => reply(&mut sock, "404 Not Found", "no such route", "text/plain").await,
    }
}

/// `GET /api/events`: `hello`, then every broadcast frame, held open until the peer closes.
async fn events(sock: &mut TcpStream, fake: &Arc<Fake>) -> std::io::Result<()> {
    fake.streams.fetch_add(1, Ordering::SeqCst);
    let nth = fake.opened.fetch_add(1, Ordering::SeqCst);
    if let Some(late) = fake.late_pane.filter(|_| nth >= 2) {
        // Counted as open, and not answering yet.
        fake.record("late-stream-held");
        tokio::time::sleep(late).await;
    }
    // Subscribed before `hello` is written: a frame broadcast after a client has seen `hello`
    // must reach it, or a test that answers an ask the moment `hello` arrives would race.
    let mut frames = fake.frames.subscribe();
    // Chunked, because the stream never ends by itself: `hello`, then nothing, with the
    // connection held open. A client that stops reading (or closes) is what ends this.
    let hello = serde_json::json!({
        "type": "hello",
        "session": fake.session.display().to_string(),
        "cwd": fake.cwd.display().to_string(),
        "model": "mock",
        "yolo": false,
        "pending": 0,
        "protocol": 1,
    });
    let frame = format!("data: {hello}\n\n");
    sock.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-store\r\ntransfer-encoding: chunked\r\n\r\n").await?;
    sock.write_all(format!("{:x}\r\n{frame}\r\n", frame.len()).as_bytes()).await?;
    sock.flush().await?;
    fake.record("hello-sent");

    // Hold it. The read is what notices the peer closing, which is the whole point of the Stop
    // finding: this door will not exit on SIGTERM until it returns.
    let mut sink = [0u8; 1024];
    loop {
        tokio::select! {
            read = sock.read(&mut sink) => match read {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            },
            sent = frames.recv() => match sent {
                Ok(bytes) => {
                    let mut chunk = format!("{:x}\r\n", bytes.len()).into_bytes();
                    chunk.extend_from_slice(&bytes);
                    chunk.extend_from_slice(b"\r\n");
                    if sock.write_all(&chunk).await.is_err() || sock.flush().await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            },
        }
    }
    // Recorded before the count falls: SIGTERM waits for the count to reach zero and then
    // writes its own line, so this line is always the earlier of the two (H1b review finding 5).
    fake.record("stream-closed");
    fake.streams.fetch_sub(1, Ordering::SeqCst);
    Ok(())
}

async fn reply(sock: &mut TcpStream, status: &str, body: &str, kind: &str) -> std::io::Result<()> {
    let head = format!("HTTP/1.1 {status}\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len());
    sock.write_all(head.as_bytes()).await?;
    sock.write_all(body.as_bytes()).await?;
    sock.flush().await
}

/// `0600`, the way the real door writes it — or `0644` under `FAKE_MODE=0644`, which is the
/// token file the perch must refuse rather than read.
fn write_token(path: &Path, token: &str) {
    harnox::fs::write_atomic_0600(path, token.as_bytes()).expect("writing the token file");
    if std::env::var_os("FAKE_MODE").is_some_and(|mode| mode.to_string_lossy() == "0644") {
        owner_only(path, 0o644);
    }
}

fn owner_only(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("setting the mode");
}
