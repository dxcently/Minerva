//! A quiesce through `eidolon_web::run` itself, since the wiring between its
//! start-up and the wake task is half of what is under test. The rest of
//! waking is `src/wake.rs`'s tests.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use eidolon_core::agent::{Agent, AgentConfig};
use eidolon_core::dispatch::Dispatcher;
use eidolon_core::event::EventBus;
use eidolon_core::policy::AllowAll;
use eidolon_core::session::Session;
use eidolon_core::testing::ScriptedProvider;
use eidolon_core::tool::ToolRegistry;
use eidolon_web::driver::Presence;
use eidolon_web::user::WebUser;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct Roster {
    gone: AtomicBool,
}

impl Presence for Roster {
    fn set_busy(&self, _busy: bool) {}
    fn title_if_empty(&self, _text: &str) {}
    fn deregister(&self) {
        self.gone.store(true, Ordering::SeqCst);
    }
    fn id(&self) -> String {
        "eidolon-test".to_string()
    }
}

fn app() -> (Arc<Agent>, Arc<WebUser>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::create(&dir.path().join("t.eid"), "m", dir.path(), None).unwrap();
    let session = Arc::new(tokio::sync::Mutex::new(session));
    let user = Arc::new(WebUser::new());
    let dispatcher = Arc::new(Dispatcher::new(
        ToolRegistry::new(),
        Arc::new(AllowAll),
        user.clone(),
        EventBus::new(64),
        session,
        dir.path().to_path_buf(),
    ));
    let provider = ScriptedProvider::new(vec![ScriptedProvider::text("hi there")]);
    let agent = Arc::new(Agent::new(provider, dispatcher, AgentConfig { model: "m".into(), ..Default::default() }));
    (agent, user, dir)
}

/// The token file's name is the only place a test learns the bound port.
async fn door_of(runtime: &Path) -> (SocketAddr, String) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(entries) = std::fs::read_dir(runtime) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let Some(port) = name
                    .strip_prefix("web-")
                    .and_then(|n| n.strip_suffix(".token"))
                    .and_then(|p| p.parse::<u16>().ok())
                else {
                    continue;
                };
                let token = std::fs::read_to_string(entry.path()).unwrap();
                return (SocketAddr::from(([127, 0, 0, 1], port)), token);
            }
        }
        assert!(tokio::time::Instant::now() < deadline, "no token file in {}", runtime.display());
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn till(sock: &mut TcpStream, seen: &mut String, needle: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !seen.contains(needle) {
        let mut buf = [0u8; 4096];
        let left = deadline - tokio::time::Instant::now();
        let n = tokio::time::timeout(left, sock.read(&mut buf))
            .await
            .unwrap_or_else(|_| panic!("no {needle:?} within 10s: {seen:?}"))
            .unwrap();
        assert!(n > 0, "the stream closed before {needle:?}: {seen:?}");
        seen.push_str(&String::from_utf8_lossy(&buf[..n]));
    }
}

#[tokio::test]
async fn a_quiesced_door_says_goodbye_and_the_run_returns() {
    let (agent, user, dir) = app();
    // What the swarm socket's op handler does before it rings.
    assert!(agent.request_quiesce("cannataxis"));
    let (ring, doorbell) = mpsc::unbounded_channel();
    let roster = Arc::new(Roster::default());
    let runtime = dir.path().join("runtime");
    let running = tokio::spawn(eidolon_web::run(
        agent,
        user,
        eidolon_web::WebOptions {
            bind: "127.0.0.1:0".parse().unwrap(),
            ui_dir: None,
            cwd: dir.path().to_path_buf(),
            runtime_dir: runtime.clone(),
            yolo: Default::default(),
            doorbell: Some(doorbell),
            presence: Some(roster.clone() as Arc<dyn Presence>),
        },
        CancellationToken::new(),
    ));

    let (bound, token) = door_of(&runtime).await;
    let mut sock = TcpStream::connect(bound).await.unwrap();
    let req = format!("GET /api/events HTTP/1.1\r\nHost: {bound}\r\nAuthorization: Bearer {token}\r\n\r\n");
    sock.write_all(req.as_bytes()).await.unwrap();
    // Subscribed to the goodbye's feed by now.
    let mut seen = String::new();
    till(&mut sock, &mut seen, "\"type\":\"caught-up\"").await;

    ring.send(()).unwrap();

    till(&mut sock, &mut seen, "\"type\":\"goodbye\"").await;
    let text = serde_json::to_string(&eidolon_core::quiesce::goodbye("cannataxis", Some("eidolon-test"))).unwrap();
    assert!(seen.contains(&format!("\"text\":{text}")), "the frame carries the goodbye: {seen:?}");
    assert!(roster.gone.load(Ordering::SeqCst), "a quiesced session is deliberately dark");
    let ran = tokio::time::timeout(Duration::from_secs(10), running)
        .await
        .expect("run() returns when the door closes");
    ran.unwrap().unwrap();
    let left: Vec<String> = std::fs::read_dir(&runtime)
        .map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "the token file outlived the run: {left:?}");
}
