//! `fake_eidolon` — a stand-in for `eidolon web`, for the perch's tests.
//!
//! A real door is WSL-only, heavy, and wants a model; the perch's lifecycle tests must not
//! depend on one. This binary is the door's **contract** and nothing else (hub.md §12):
//!
//! - the argv `eidolon web --bind ADDR (--cwd DIR | --session PATH) [-m MODEL]`, and the
//!   argv is recorded so a test can show there is no token in it;
//! - the two boot lines, exactly: `listening on http://<addr>/` then `token file: <path>`;
//! - its own token, minted here, written `0600` to `$XDG_RUNTIME_DIR/eidolon/web-<port>.token`
//!   and never printed (`crates/web/src/lib.rs:120-137`);
//! - `GET /api/events`, gated on that token, answering `hello` and then holding the stream
//!   open — every request's method, path and `Authorization` is appended to `$FAKE_LOG`;
//! - **SIGTERM as the real door behaves**: while a stream is open it does *not* exit (hyper's
//!   `GracefulShutdown` waits the connection out, which is minutes), and once no stream is
//!   open it removes its token file and exits. That is what makes "close the watcher before
//!   SIGTERM" the difference between a clean stop and a 10 s hang plus a SIGKILL.
//!
//! Knobs, all by environment (§12): `FAKE_LOG`, `FAKE_SESSIONS_DIR`, `FAKE_SILENT`,
//! `FAKE_MODE=0644`, `FAKE_TOKEN_ELSEWHERE`, `FAKE_EXIT_AFTER_MS`, `FAKE_IGNORE_SIGTERM`.
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
use tokio::signal::unix::{signal, SignalKind};

/// Everything one fake door knows about itself.
struct Fake {
    token: String,
    token_file: PathBuf,
    /// The log the door says it opened: `<sessions dir>/<id>.eid` for `--cwd`, the given
    /// path for `--session`.
    session: PathBuf,
    cwd: PathBuf,
    /// The file every request is recorded to, if a test asked for one.
    log: Option<PathBuf>,
    /// How many `/api/events` streams are open right now. SIGTERM waits for this to be zero.
    streams: AtomicUsize,
    /// `FAKE_IGNORE_SIGTERM`: never exit on SIGTERM, so the perch's SIGKILL path and its
    /// token-file cleanup are the only things that end this door.
    ignore_sigterm: bool,
}

impl Fake {
    fn record(&self, line: &str) {
        let Some(path) = &self.log else { return };
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{line}");
        }
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

    // A tiny runtime: the door serves one connection at a time in tests, and nothing here
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
        session,
        cwd,
        log: std::env::var_os("FAKE_LOG").map(PathBuf::from),
        streams: AtomicUsize::new(0),
        ignore_sigterm: std::env::var_os("FAKE_IGNORE_SIGTERM").is_some(),
    });

    // Recorded before the boot lines: the port and pid a test needs to check /proc and the
    // token file, and the argv it must show carries no token. The argv is a test's business,
    // so it is the one line that repeats the process's own words; a door's token is not in
    // them (the perch never passes one) and the token minted above is never here.
    fake.record(&format!("start pid={} port={port}", std::process::id()));
    fake.record(&format!("argv {}", argv[1..].join(" ")));
    fake.record(&format!("model {}", args.model.as_deref().unwrap_or("-")));

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
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
            // A door that dies of its own accord leaves its token file behind, exactly as a
            // crashed one does: the perch's child-exit path has to leave no orphan either.
            fake.record("exit-now");
            std::process::exit(1);
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
    // and the perch has already closed the watcher by the time this signal arrives — the
    // kernel's close and this process's read of it are two different events, so this window
    // is what makes "closed first" observable from in here rather than a scheduling race.
    let window = Instant::now() + Duration::from_millis(500);
    while fake.streams.load(Ordering::SeqCst) > 0 && Instant::now() < window {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    fake.record(&format!("sigterm streams={}", fake.streams.load(Ordering::SeqCst)));
    // The stream the perch holds open is its watcher; it is closed by the perch (or killed
    // with this process), and until then this door does not exit.
    while fake.streams.load(Ordering::SeqCst) > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
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
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
        }
    }
}

/// One HTTP/1.1 connection, by hand: a test double does not need hyper, and hand-parsing
/// keeps "hold the stream open" and "wait for the peer to close" explicit.
async fn connection(mut sock: TcpStream, fake: Arc<Fake>) -> std::io::Result<()> {
    let mut head = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        let read = sock.read(&mut buf).await?;
        if read == 0 {
            return Ok(());
        }
        head.extend_from_slice(&buf[..read]);
        if head.windows(4).any(|w| w == b"\r\n\r\n") || head.len() > 64 * 1024 {
            break;
        }
    }
    let text = String::from_utf8_lossy(&head).to_string();
    let mut lines = text.lines();
    let request = lines.next().unwrap_or_default().to_string();
    let mut method_and_path = request.split_whitespace();
    let method = method_and_path.next().unwrap_or_default().to_string();
    let path = method_and_path.next().unwrap_or_default().to_string();
    // Header names go on the wire in lower case (hyper writes them that way), so this read
    // is case-insensitive: a `Authentication`-shaped guess would refuse the perch's own
    // watcher and the test would blame the perch.
    let auth = text
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
        .map(|(_, value)| value.trim().to_string())
        .unwrap_or_else(|| "-".to_string());
    fake.record(&format!("{method} {path} auth={auth}"));

    if path != "/api/events" {
        return reply(&mut sock, "404 Not Found", "no such route", "text/plain").await;
    }
    // The door's token gate, exactly as the real one's: `Authorization: Bearer`, constant
    // time not required here, 401 with an empty body.
    if auth != format!("Bearer {}", fake.token) {
        return reply(&mut sock, "401 Unauthorized", "", "text/plain").await;
    }

    fake.streams.fetch_add(1, Ordering::SeqCst);
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

    // Hold it. The read is what notices the perch closing the watcher, which is the whole
    // point of the Stop finding: this door will not exit on SIGTERM until it returns.
    loop {
        match sock.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
    }
    fake.streams.fetch_sub(1, Ordering::SeqCst);
    fake.record("stream-closed");
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
