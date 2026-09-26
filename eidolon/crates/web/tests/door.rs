//! The door over a real socket, in hand-written HTTP/1.1: `Request<Incoming>`
//! cannot be built outside a connection, so the gates are tested here. Every
//! request carries the token unless the test is about the token.

use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use eidolon_core::agent::{Agent, AgentConfig};
use eidolon_core::dispatch::Dispatcher;
use eidolon_core::event::EventBus;
use eidolon_core::message::Message;
use eidolon_core::policy::{AllowAll, Approval, PolicyHook, Ruling, Verdict};
use eidolon_core::provider::Provider;
use eidolon_core::session::{RecordKind, Session};
use eidolon_core::testing::ScriptedProvider;
use eidolon_core::tool::{CallContext, Tool, ToolCall, ToolManifest, ToolOutput, ToolRegistry};
use eidolon_core::user::{Choice, UserIo};
use eidolon_web::driver::Handle;
use eidolon_web::files::Static;
use eidolon_web::serve::{self, Ctx};
use eidolon_web::user::WebUser;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

async fn app() -> (Arc<Agent>, Arc<WebUser>, tempfile::TempDir) {
    let provider = ScriptedProvider::new(vec![ScriptedProvider::text("hi there")]);
    app_with(Arc::new(AllowAll), ToolRegistry::new(), provider).await
}

async fn app_with(
    policy: Arc<dyn PolicyHook>,
    registry: ToolRegistry,
    provider: Arc<dyn Provider>,
) -> (Arc<Agent>, Arc<WebUser>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let session = Session::create(&dir.path().join("t.eid"), "m", dir.path(), None).unwrap();
    let session = Arc::new(tokio::sync::Mutex::new(session));
    let user = Arc::new(WebUser::new());
    let io: Arc<dyn UserIo> = user.clone();
    let dispatcher = Arc::new(Dispatcher::new(
        registry,
        policy,
        io,
        EventBus::new(64),
        session,
        dir.path().to_path_buf(),
    ));
    let agent = Arc::new(Agent::new(provider, dispatcher, AgentConfig { model: "m".into(), ..Default::default() }));
    (agent, user, dir)
}

/// Asks before `note`, so the ask comes through the real chokepoint.
struct AskNote;

#[async_trait]
impl PolicyHook for AskNote {
    async fn pre_tool(&self, call: &ToolCall, _manifest: &ToolManifest, _cwd: &Path) -> Ruling {
        if call.name == "note" {
            Ruling {
                verdict: Verdict::Ask("note — writes outside the working directory. Run it?".into()),
                reason: Some("outside-cwd".into()),
                structural: true,
                judged: None,
                yolo: false,
            }
        } else {
            Ruling::allow()
        }
    }
}

/// Counts its calls: a declined call still gets a `tool-call-finished`.
struct Note {
    manifest: ToolManifest,
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for Note {
    fn manifest(&self) -> &ToolManifest {
        &self.manifest
    }

    async fn call(&self, _input: serde_json::Value, _ctx: CallContext) -> anyhow::Result<ToolOutput> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::ok("ran"))
    }
}

async fn asking_app() -> (Arc<Agent>, Arc<WebUser>, Arc<AtomicUsize>, tempfile::TempDir) {
    let calls = Arc::new(AtomicUsize::new(0));
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(Note {
        manifest: ToolManifest {
            name: "note".into(),
            description: "note something".into(),
            input_schema: serde_json::json!({"type": "object", "properties": {"text": {"type": "string"}}}),
            approval: Approval::Mutating,
            prompt: None,
            render: None,
            deferred: false,
        },
        calls: calls.clone(),
    }));
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::tool("t1", "note", serde_json::json!({"text": "hi"})),
        ScriptedProvider::text("done"),
    ]);
    let (agent, user, dir) = app_with(Arc::new(AskNote), registry, provider).await;
    (agent, user, calls, dir)
}

fn question(user: Arc<WebUser>, prompt: &'static str) -> (tokio::task::JoinHandle<Option<String>>, CancellationToken) {
    let cancel = CancellationToken::new();
    let opts = [Choice { label: "a".into(), description: None }, Choice { label: "b".into(), description: None }];
    let handle = tokio::spawn({
        let user = user.clone();
        let cancel = cancel.clone();
        async move { user.choose(prompt, &opts, &cancel).await }
    });
    (handle, cancel)
}

/// `serve::run` directly, since `lib::run` prints its port rather than
/// returning it. The token is a real one, so no short string passes by accident.
async fn serve(agent: Arc<Agent>, user: Arc<WebUser>) -> (SocketAddr, String, CancellationToken) {
    serve_with(agent, user, None).await
}

async fn serve_ui(agent: Arc<Agent>, user: Arc<WebUser>, ui: &Path) -> (SocketAddr, String, CancellationToken) {
    serve_with(agent, user, Some(Static::new(ui).unwrap())).await
}

async fn serve_with(
    agent: Arc<Agent>,
    user: Arc<WebUser>,
    ui: Option<Static>,
) -> (SocketAddr, String, CancellationToken) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bound = listener.local_addr().unwrap();
    let token = harnox::crypto::random_token();
    let ctx = Ctx {
        agent: agent.clone(),
        user: user.clone(),
        driver: Handle::new(agent, None),
        bound,
        cwd: "/tmp".into(),
        yolo: false,
        token: Arc::from(token.as_str()),
        ui,
    };
    let cancel = CancellationToken::new();
    let c = cancel.clone();
    tokio::spawn(async move { serve::run(ctx, listener, c).await.unwrap() });
    (bound, token, cancel)
}

/// `auth` is the whole `Authorization` header; `None` sends none.
async fn call_auth(
    addr: SocketAddr,
    auth: Option<&str>,
    method: &str,
    path: &str,
    host: &str,
    body: Option<&str>,
) -> (u16, String) {
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
    if let Some(auth) = auth {
        req.push_str(&format!("Authorization: {auth}\r\n"));
    }
    if let Some(b) = body {
        req.push_str(&format!("Content-Type: application/json\r\nContent-Length: {}\r\n\r\n{b}", b.len()));
    } else {
        req.push_str("\r\n");
    }
    let mut sock = TcpStream::connect(addr).await.unwrap();
    sock.write_all(req.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    sock.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw).to_string();
    let code = text
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or_else(|| panic!("no status line in {text:?}"));
    (code, text)
}

async fn call(addr: SocketAddr, token: &str, method: &str, path: &str, host: &str, body: Option<&str>) -> (u16, String) {
    call_auth(addr, Some(&format!("Bearer {token}")), method, path, host, body).await
}

/// A write error is the server refusing mid-body, not a failure.
async fn call_oversize(addr: SocketAddr, token: &str, path: &str, len: usize) -> (u16, String) {
    let mut sock = TcpStream::connect(addr).await.unwrap();
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {token}\r\n\
         Content-Type: application/json\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n"
    );
    sock.write_all(head.as_bytes()).await.unwrap();
    let chunk = vec![b'x'; 64 * 1024];
    let mut written = 0;
    while written < len {
        let take = chunk.len().min(len - written);
        if sock.write_all(&chunk[..take]).await.is_err() {
            break;
        }
        written += take;
    }
    let mut raw = Vec::new();
    // A reset is fine: what matters is the bytes already read.
    let _ = sock.read_to_end(&mut raw).await;
    let text = String::from_utf8_lossy(&raw).to_string();
    let code = text.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    (code, text)
}

/// An SSE body never ends, so it is read in stages up to a needle.
struct Sse {
    sock: TcpStream,
    seen: String,
    cursor: usize,
}

impl Sse {
    async fn open(addr: SocketAddr, token: &str) -> Self {
        Self::open_with(addr, "/api/events", Some(token)).await
    }

    async fn open_query(addr: SocketAddr, token: &str) -> Self {
        Self::open_with(addr, &format!("/api/events?token={token}"), None).await
    }

    async fn open_with(addr: SocketAddr, path: &str, token: Option<&str>) -> Self {
        let mut sock = TcpStream::connect(addr).await.unwrap();
        let auth = token.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
        let req = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\n{auth}\r\n");
        sock.write_all(req.as_bytes()).await.unwrap();
        Sse { sock, seen: String::new(), cursor: 0 }
    }

    /// The stretch since the last call, up to `needle`. Make the needle the
    /// end of what you assert on: frames arrive split across reads.
    async fn til(&mut self, needle: &str) -> String {
        loop {
            if let Some(i) = self.seen[self.cursor..].find(needle) {
                let end = self.cursor + i + needle.len();
                let fresh = self.seen[self.cursor..end].to_string();
                self.cursor = end;
                return fresh;
            }
            let mut buf = [0u8; 4096];
            let n = tokio::time::timeout(std::time::Duration::from_secs(5), self.sock.read(&mut buf))
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for {needle:?} in {:?}", self.seen))
                .unwrap();
            if n == 0 {
                panic!("the stream closed before {needle:?} arrived: {:?}", self.seen);
            }
            self.seen.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    }
}

fn at(haystack: &str, needle: &str) -> usize {
    haystack.find(needle).unwrap_or_else(|| panic!("{needle:?} is not in {haystack:?}"))
}

fn ask_id(frame: &str) -> u64 {
    let at = at(frame, "\"ask_id\":") + "\"ask_id\":".len();
    frame[at..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>()
        .parse()
        .unwrap_or_else(|_| panic!("no ask_id in {frame:?}"))
}

#[tokio::test]
async fn the_stream_says_hello_then_replays_the_branch_then_caught_up() {
    let (agent, user, _dir) = app().await;
    agent
        .session()
        .lock()
        .await
        .append(RecordKind::UserMessage(Message::user_text("earlier")))
        .unwrap();
    let (bound, token, cancel) = serve(agent, user).await;

    let mut sse = Sse::open(bound, &token).await;
    let head = sse.til("\"type\":\"caught-up\"").await;
    cancel.cancel();

    assert!(head.starts_with("HTTP/1.1 200"), "{head:?}");
    assert!(head.contains("content-type: text/event-stream"), "{head:?}");
    let hello = at(&head, "\"type\":\"hello\"");
    let replayed = at(&head, "\"type\":\"user-message\"");
    let caught_up = at(&head, "\"type\":\"caught-up\"");
    assert!(hello < replayed, "hello comes first: {head:?}");
    assert!(replayed < caught_up, "the branch is replayed before caught-up: {head:?}");
    assert!(head.contains("\"text\":\"earlier\""), "{head:?}");
    assert!(head.contains("\"model\":\"m\""), "hello carries the session's model: {head:?}");
    let hello_frame = &head[hello..replayed];
    assert!(hello_frame.contains(&format!("\"protocol\":{}", eidolon_web::PROTOCOL)), "{hello_frame:?}");
    assert_eq!(eidolon_web::PROTOCOL, 1, "the version this door speaks");
}

#[tokio::test]
async fn a_said_message_becomes_a_turn_the_open_stream_carries_live() {
    let (agent, user, _dir) = app().await;
    let (bound, token, cancel) = serve(agent, user).await;
    // Caught up first, so what follows comes off the bus, not the replay.
    let mut sse = Sse::open(bound, &token).await;
    let head = sse.til("caught-up").await;
    assert!(!head.contains("assistant-message"), "nothing has been said yet: {head:?}");

    let (code, body) = call(bound, &token, "POST", "/api/say", &bound.to_string(), Some("{\"text\":\"hello\"}")).await;
    assert_eq!(code, 202, "{body:?}");
    assert!(body.contains("\"queued\":false"), "an idle session starts the turn: {body:?}");

    let live = sse.til("\"type\":\"turn-settled\"").await;
    cancel.cancel();
    assert!(live.contains("\"type\":\"turn-state\""), "the turn's start is announced: {live:?}");
    assert!(live.contains("\"type\":\"user-message\""), "{live:?}");
    assert!(live.contains("\"type\":\"assistant-message\""), "{live:?}");
    assert!(live.contains("\"text\":\"hi there\""), "{live:?}");
}

#[tokio::test]
async fn an_ask_reaches_the_stream_and_an_answer_resolves_it() {
    let (agent, user, _dir) = app().await;
    let (bound, token, cancel) = serve(agent, user.clone()).await;
    let mut sse = Sse::open(bound, &token).await;
    sse.til("\"type\":\"caught-up\"").await;

    let (asking, _c) = question(user.clone(), "pick one");

    let opened = sse.til("\"prompt\":\"pick one\",\"options\":[{\"label\":\"a\"},{\"label\":\"b\"}]}").await;
    assert!(opened.contains("\"type\":\"ask\""), "{opened:?}");
    assert!(opened.contains("\"kind\":\"question\""), "{opened:?}");
    assert!(opened.contains("\"options\":[{\"label\":\"a\"},{\"label\":\"b\"}]"), "{opened:?}");
    let id = ask_id(&opened);

    let (code, body) = call(
        bound,
        &token,
        "POST",
        "/api/answer",
        &bound.to_string(),
        Some(&format!("{{\"ask_id\":{id},\"answer\":\"b\"}}")),
    )
    .await;
    assert_eq!(code, 204, "{body:?}");
    assert_eq!(asking.await.unwrap().as_deref(), Some("b"));
    let settled = sse.til("\"answer\":\"b\"}").await;
    assert!(settled.contains(&format!("\"ask_id\":{id}")), "{settled:?}");
    assert!(settled.contains("\"how\":\"answered\""), "{settled:?}");
    assert!(settled.contains("\"answer\":\"b\""), "{settled:?}");

    let (code, _) = call(
        bound,
        &token,
        "POST",
        "/api/answer",
        &bound.to_string(),
        Some(&format!("{{\"ask_id\":{id},\"answer\":\"b\"}}")),
    )
    .await;
    assert_eq!(code, 409);
    cancel.cancel();
}

#[tokio::test]
async fn an_approval_ask_carries_the_call_and_a_yes_runs_the_tool() {
    let (agent, user, calls, _dir) = asking_app().await;
    let (bound, token, cancel) = serve(agent, user).await;
    let host = bound.to_string();
    let mut sse = Sse::open(bound, &token).await;
    sse.til("\"type\":\"caught-up\"").await;

    let (code, body) = call(bound, &token, "POST", "/api/say", &host, Some("{\"text\":\"note it\"}")).await;
    assert_eq!(code, 202, "{body:?}");

    let opened = sse.til("\"answers\":[\"yes\",\"no\"]}").await;
    assert!(opened.contains("\"type\":\"ask\""), "{opened:?}");
    assert!(opened.contains("\"kind\":\"approval\""), "{opened:?}");
    assert!(opened.contains("\"call_id\":\"t1\""), "{opened:?}");
    assert!(opened.contains("\"tool\":\"note\""), "{opened:?}");
    assert!(opened.contains("\"input\":{\"text\":\"hi\"}"), "{opened:?}");
    assert!(opened.contains("\"prompt\":\"note — writes outside the working directory. Run it?\""), "{opened:?}");
    assert!(opened.contains("\"reason\":\"outside-cwd\""), "{opened:?}");
    assert!(opened.contains("\"structural\":true"), "{opened:?}");
    assert!(opened.contains("\"answers\":[\"yes\",\"no\"]"), "{opened:?}");
    let id = ask_id(&opened);
    assert_eq!(calls.load(Ordering::SeqCst), 0, "the call is blocked on the answer");

    let (code, body) = call(bound, &token, "POST", "/api/answer", &host, Some(&format!("{{\"ask_id\":{id},\"answer\":\"yes\"}}"))).await;
    assert_eq!(code, 204, "{body:?}");
    let after = sse.til("\"is_error\":false}").await;
    assert!(after.contains("\"type\":\"ask-settled\""), "{after:?}");
    assert!(after.contains(&format!("\"ask_id\":{id}")), "{after:?}");
    assert!(after.contains("\"how\":\"answered\""), "{after:?}");
    assert!(after.contains("\"answer\":\"yes\""), "{after:?}");
    assert!(after.contains("\"type\":\"policy-verdict\""), "{after:?}");
    assert!(after.contains("\"outcome\":\"approved\""), "{after:?}");
    assert!(after.contains("\"tool\":\"note\""), "{after:?}");
    assert!(after.contains("\"output\":\"ran\""), "{after:?}");
    assert!(after.contains("\"is_error\":false"), "{after:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 1, "the approved call ran once");
    cancel.cancel();
}

#[tokio::test]
async fn a_no_declines_the_call_without_running_it() {
    let (agent, user, calls, _dir) = asking_app().await;
    let (bound, token, cancel) = serve(agent, user).await;
    let host = bound.to_string();
    let mut sse = Sse::open(bound, &token).await;
    sse.til("\"type\":\"caught-up\"").await;

    call(bound, &token, "POST", "/api/say", &host, Some("{\"text\":\"note it\"}")).await;
    let id = ask_id(&sse.til("\"answers\":[\"yes\",\"no\"]}").await);

    let (code, body) = call(bound, &token, "POST", "/api/answer", &host, Some(&format!("{{\"ask_id\":{id},\"answer\":\"no\"}}"))).await;
    assert_eq!(code, 204, "{body:?}");

    let after = sse.til("\"is_error\":true}").await;
    assert!(after.contains("\"how\":\"answered\""), "{after:?}");
    assert!(after.contains("\"answer\":\"no\""), "{after:?}");
    assert!(after.contains("\"outcome\":\"declined\""), "{after:?}");
    assert!(after.contains("\"is_error\":true"), "{after:?}");
    assert!(after.contains("declined this tool call"), "{after:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 0, "nothing ran after a no");
    cancel.cancel();
}

#[tokio::test]
async fn a_stream_opened_after_the_ask_still_receives_it_before_caught_up() {
    let (agent, user, _calls, _dir) = asking_app().await;
    let (bound, token, cancel) = serve(agent, user).await;
    let host = bound.to_string();
    let mut first = Sse::open(bound, &token).await;
    first.til("\"type\":\"caught-up\"").await;

    call(bound, &token, "POST", "/api/say", &host, Some("{\"text\":\"note it\"}")).await;
    let opened = first.til("\"answers\":[\"yes\",\"no\"]}").await;
    let id = ask_id(&opened);

    let mut late = Sse::open(bound, &token).await;
    let head = late.til("\"type\":\"caught-up\"").await;
    assert!(head.contains("\"pending\":1"), "hello counts what it hands over: {head:?}");
    assert!(head.contains(&format!("\"ask_id\":{id}")), "the same ask, by id: {head:?}");
    assert!(at(&head, "\"kind\":\"approval\"") < at(&head, "\"type\":\"caught-up\""), "{head:?}");

    let (code, _) = call(bound, &token, "POST", "/api/answer", &host, Some(&format!("{{\"ask_id\":{id},\"answer\":\"yes\"}}"))).await;
    assert_eq!(code, 204);
    let settled = late.til("\"answer\":\"yes\"}").await;
    assert!(settled.contains(&format!("\"ask_id\":{id}")), "{settled:?}");
    cancel.cancel();
}

#[tokio::test]
async fn two_pending_asks_are_answered_independently_over_http() {
    let (agent, user, _dir) = app().await;
    let (bound, token, cancel) = serve(agent, user.clone()).await;
    let host = bound.to_string();
    let mut sse = Sse::open(bound, &token).await;
    sse.til("\"type\":\"caught-up\"").await;

    let (first, _first_token) = question(user.clone(), "first?");
    let first_id = ask_id(&sse.til("\"prompt\":\"first?\",\"options\":[{\"label\":\"a\"},{\"label\":\"b\"}]}").await);
    let (second, _second_token) = question(user.clone(), "second?");
    let second_id = ask_id(&sse.til("\"prompt\":\"second?\",\"options\":[{\"label\":\"a\"},{\"label\":\"b\"}]}").await);
    assert_ne!(first_id, second_id);

    let mut late = Sse::open(bound, &token).await;
    let head = late.til("\"type\":\"caught-up\"").await;
    assert!(head.contains("\"pending\":2"), "{head:?}");
    let first_at = at(&head, &format!("\"ask_id\":{first_id},\"prompt\""));
    let second_at = at(&head, &format!("\"ask_id\":{second_id},\"prompt\""));
    assert!(first_at < second_at, "the older ask comes first: {head:?}");
    assert!(second_at < at(&head, "\"type\":\"caught-up\""), "and both come before caught-up: {head:?}");

    // Newest first, so the older ask is not merely queued behind it.
    let (code, _) = call(bound, &token, "POST", "/api/answer", &host, Some(&format!("{{\"ask_id\":{second_id},\"answer\":\"b\"}}"))).await;
    assert_eq!(code, 204);
    assert_eq!(second.await.unwrap().as_deref(), Some("b"));
    assert!(!first.is_finished(), "the first ask survived the second's answer");

    let ask = format!("{{\"ask_id\":{first_id},\"answer\":\"c\"}}");
    let (code, body) = call(bound, &token, "POST", "/api/answer", &host, Some(&ask)).await;
    assert_eq!(code, 422, "a label neither ask offers: {body:?}");
    let (code, _) = call(bound, &token, "POST", "/api/answer", &host, Some(&format!("{{\"ask_id\":{first_id},\"answer\":\"a\"}}"))).await;
    assert_eq!(code, 204, "a rejected answer leaves the ask pending");
    assert_eq!(first.await.unwrap().as_deref(), Some("a"));

    let (code, _) = call(bound, &token, "POST", "/api/answer", &host, Some(&format!("{{\"ask_id\":{first_id},\"answer\":\"a\"}}"))).await;
    assert_eq!(code, 409, "and now nothing is pending under it");
    cancel.cancel();
}

#[tokio::test]
async fn an_approval_refuses_an_answer_that_is_neither_yes_nor_no() {
    let (agent, user, calls, _dir) = asking_app().await;
    let (bound, token, cancel) = serve(agent, user).await;
    let host = bound.to_string();
    let mut sse = Sse::open(bound, &token).await;
    sse.til("\"type\":\"caught-up\"").await;

    call(bound, &token, "POST", "/api/say", &host, Some("{\"text\":\"note it\"}")).await;
    let id = ask_id(&sse.til("\"answers\":[\"yes\",\"no\"]}").await);

    let (code, body) = call(bound, &token, "POST", "/api/answer", &host, Some(&format!("{{\"ask_id\":{id},\"answer\":\"maybe\"}}"))).await;
    assert_eq!(code, 422, "{body:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    let (code, _) = call(bound, &token, "POST", "/api/answer", &host, Some(&format!("{{\"ask_id\":{id},\"answer\":\"no\"}}"))).await;
    assert_eq!(code, 204, "the ask was still pending and still answerable");
    let after = sse.til("\"outcome\":\"declined\"}").await;
    assert!(after.contains("\"outcome\":\"declined\""), "{after:?}");
    cancel.cancel();
}

#[tokio::test]
async fn cancelling_the_turn_settles_the_pending_ask_as_cancelled() {
    let (agent, user, calls, _dir) = asking_app().await;
    let (bound, token, cancel) = serve(agent, user).await;
    let host = bound.to_string();
    let mut sse = Sse::open(bound, &token).await;
    sse.til("\"type\":\"caught-up\"").await;

    call(bound, &token, "POST", "/api/say", &host, Some("{\"text\":\"note it\"}")).await;
    let id = ask_id(&sse.til("\"answers\":[\"yes\",\"no\"]}").await);

    let (code, body) = call(bound, &token, "POST", "/api/cancel", &host, None).await;
    assert_eq!(code, 204, "{body:?}");

    let settled = sse.til("\"how\":\"cancelled\"}").await;
    assert!(settled.contains(&format!("\"ask_id\":{id}")), "{settled:?}");
    assert!(settled.contains("\"how\":\"cancelled\""), "{settled:?}");
    assert!(!settled.contains("\"answer\""), "a cancelled ask carries no answer: {settled:?}");
    assert_eq!(calls.load(Ordering::SeqCst), 0, "a cancelled question runs nothing");

    let (code, _) = call(bound, &token, "POST", "/api/answer", &host, Some(&format!("{{\"ask_id\":{id},\"answer\":\"yes\"}}"))).await;
    assert_eq!(code, 409);
    cancel.cancel();
}

#[tokio::test]
async fn every_post_refuses_a_host_header_that_is_not_the_bound_address() {
    let (agent, user, _dir) = app().await;
    let (bound, token, cancel) = serve(agent, user).await;

    for (path, body) in [
        ("/api/say", "{\"text\":\"hello\"}"),
        ("/api/cancel", "{}"),
        ("/api/answer", "{\"ask_id\":1,\"answer\":\"yes\"}"),
    ] {
        let (code, resp) = call(bound, &token, "POST", path, "evil.example:4477", Some(body)).await;
        assert_eq!(code, 403, "a rebound name must not reach {path}: {resp:?}");
    }
    // The control: the bound authority is not refused.
    let (code, _) = call(bound, &token, "POST", "/api/cancel", &bound.to_string(), None).await;
    assert_eq!(code, 409, "no turn is running");
    cancel.cancel();
}

/// Statuses are what a probe maps, so a caller without the token must not
/// learn that `Host` is checked at all.
#[tokio::test]
async fn the_token_gate_runs_before_the_host_check() {
    let (agent, user, _dir) = app().await;
    let (bound, token, cancel) = serve(agent.clone(), user).await;

    for (path, body) in [
        ("/api/say", Some("{\"text\":\"hello\"}")),
        ("/api/cancel", None),
        ("/api/answer", Some("{\"ask_id\":1,\"answer\":\"yes\"}")),
    ] {
        let (code, resp) = call_auth(bound, None, "POST", path, "evil.example:4477", body).await;
        assert_eq!(code, 401, "no token and a rebound name is answered by the token gate ({path}): {resp:?}");
    }

    let (code, resp) = call(bound, &token, "POST", "/api/say", "evil.example:4477", Some("{\"text\":\"hello\"}")).await;
    assert_eq!(code, 403, "the right token does not buy a rebound name: {resp:?}");

    let user_messages = {
        let session = agent.session().lock().await;
        session.branch().iter().filter(|r| matches!(r.kind, RecordKind::UserMessage(_))).count()
    };
    assert_eq!(user_messages, 0, "a refused POST is not a turn");
    cancel.cancel();
}

#[tokio::test]
async fn every_api_path_refuses_a_request_that_presents_no_token() {
    let (agent, user, _dir) = app().await;
    let (bound, token, cancel) = serve(agent, user).await;
    let host = bound.to_string();

    for (method, path, body) in [
        ("GET", "/api/events", None),
        ("POST", "/api/say", Some("{\"text\":\"hello\"}")),
        ("POST", "/api/cancel", None),
        ("POST", "/api/answer", Some("{\"ask_id\":1,\"answer\":\"yes\"}")),
        ("GET", "/api/nope", None),
    ] {
        let (code, resp) = call_auth(bound, None, method, path, &host, body).await;
        assert_eq!(code, 401, "{method} {path} with no token: {resp:?}");
        assert!(!resp.contains("error"), "the 401 carries no detail: {resp:?}");
        assert!(!resp.contains(&token), "and never the token: {resp:?}");
    }
    cancel.cancel();
}

#[tokio::test]
async fn a_token_that_is_not_this_run_s_is_refused_like_no_token_at_all() {
    let (agent, user, _dir) = app().await;
    let (bound, token, cancel) = serve(agent, user).await;
    let host = bound.to_string();
    let wrong = "Bearer not-the-token-000000000000000000000000";

    for (method, path, auth, body) in [
        ("GET", "/api/events", Some(wrong), None),
        ("POST", "/api/say", Some(wrong), Some("{\"text\":\"hello\"}")),
        ("POST", "/api/cancel", Some(wrong), None),
        ("POST", "/api/answer", Some(wrong), Some("{\"ask_id\":1,\"answer\":\"yes\"}")),
        ("GET", "/api/events?token=not-the-token-0000", None, None),
    ] {
        let (code, resp) = call_auth(bound, auth, method, path, &host, body).await;
        assert_eq!(code, 401, "{method} {path} with a wrong token: {resp:?}");
    }

    let (code, body) = call(bound, &token, "POST", "/api/cancel", &host, None).await;
    assert_eq!(code, 409, "the right token reaches the route: {body:?}");
    cancel.cancel();
}

#[tokio::test]
async fn the_query_token_opens_the_stream_and_no_post() {
    let (agent, user, _dir) = app().await;
    let (bound, token, cancel) = serve(agent, user).await;
    let host = bound.to_string();

    let mut sse = Sse::open_query(bound, &token).await;
    let head = sse.til("\"type\":\"caught-up\"").await;
    assert!(head.starts_with("HTTP/1.1 200"), "{head:?}");
    assert!(head.contains("\"type\":\"hello\""), "{head:?}");

    for (path, body) in [
        ("/api/say", Some("{\"text\":\"hello\"}")),
        ("/api/cancel", None),
        ("/api/answer", Some("{\"ask_id\":1,\"answer\":\"yes\"}")),
    ] {
        let url = format!("{path}?token={token}");
        let (code, resp) = call_auth(bound, None, "POST", &url, &host, body).await;
        assert_eq!(code, 401, "a POST does not take the query form ({path}): {resp:?}");
    }
    cancel.cancel();
}

/// The 8 MiB is really sent, so this is the server's refusal, not a header read.
#[tokio::test]
async fn an_oversize_body_is_refused_at_the_cap_and_starts_no_turn() {
    let (agent, user, _dir) = app().await;
    let (bound, token, cancel) = serve(agent.clone(), user).await;
    let host = bound.to_string();

    let (code, resp) = call_oversize(bound, &token, "/api/say", eidolon_web::serve::MAX_BODY + 1).await;
    assert_eq!(code, 413, "{resp:?}");

    let user_messages = {
        let session = agent.session().lock().await;
        session.branch().iter().filter(|r| matches!(r.kind, RecordKind::UserMessage(_))).count()
    };
    assert_eq!(user_messages, 0, "the refused body was never a turn");

    let (code, body) = call(bound, &token, "POST", "/api/say", &host, Some("{\"text\":\"hello\"}")).await;
    assert_eq!(code, 202, "{body:?}");
    assert!(body.contains("\"queued\":false"), "the door still works after a refusal: {body:?}");
    cancel.cancel();
}

fn token_files(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("web-") && n.ends_with(".token"))
        .collect();
    names.sort();
    names
}

/// `cancel` is what `crates/cli`'s `cancel_on_signals` fires.
#[tokio::test]
async fn a_cancelled_run_removes_its_token_file() {
    let (agent, user, dir) = app().await;
    // Not created: `run` makes it, as on a first run.
    let runtime = dir.path().join("runtime");
    let cancel = CancellationToken::new();
    let running = tokio::spawn(eidolon_web::run(
        agent,
        user,
        eidolon_web::WebOptions {
            bind: "127.0.0.1:0".parse().unwrap(),
            ui_dir: None,
            cwd: dir.path().to_path_buf(),
            runtime_dir: runtime.clone(),
            yolo: Default::default(),
            doorbell: None,
            presence: None,
        },
        cancel.clone(),
    ));

    // The file appears once the port is bound.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let written = loop {
        let files = token_files(&runtime);
        if !files.is_empty() {
            break files;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no token file in {} within 10s",
            runtime.display()
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    };
    assert_eq!(written.len(), 1, "one door, one token file: {written:?}");

    cancel.cancel();
    running.await.unwrap().unwrap();
    let left = token_files(&runtime);
    assert!(left.is_empty(), "the token file outlived the run: {left:?}");
}

#[tokio::test]
async fn run_refuses_a_bind_that_is_not_loopback() {
    let (agent, user, _dir) = app().await;
    let opts = eidolon_web::WebOptions {
        bind: "0.0.0.0:4477".parse().unwrap(),
        ui_dir: None,
        cwd: "/tmp".into(),
        runtime_dir: "/tmp/eidolon".into(),
        yolo: Default::default(),
        doorbell: None,
        presence: None,
    };
    let err = eidolon_web::run(agent, user, opts, CancellationToken::new()).await.unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("0.0.0.0:4477"), "the refusal names the address: {msg}");
}

#[tokio::test]
async fn the_verbs_answer_statuses_and_nothing_else_is_served() {
    let (agent, user, _dir) = app().await;
    let (bound, token, cancel) = serve(agent, user).await;
    let host = bound.to_string();

    let (code, _) = call(bound, &token, "POST", "/api/cancel", &host, None).await;
    assert_eq!(code, 409);

    let (code, body) = call(bound, &token, "POST", "/api/answer", &host, Some("{\"ask_id\":1,\"answer\":\"yes\"}")).await;
    assert_eq!(code, 409, "{body:?}");

    let (code, _) = call(bound, &token, "POST", "/api/say", &host, Some("{}")).await;
    assert_eq!(code, 400);

    // Includes routes the door this was ported from carried.
    for path in ["/", "/index.html", "/v1/models", "/ext", "/auth"] {
        let (code, body) = call(bound, &token, "GET", path, &host, None).await;
        assert_eq!(code, 404, "{path} is not this door's: {body:?}");
    }
    for path in ["/api/secret", "/api/jev"] {
        let (code, body) = call(bound, &token, "GET", path, &host, None).await;
        assert_eq!(code, 404, "{path} is not this door's: {body:?}");
    }
    cancel.cancel();
}

// ---------------------------------------------------------------------------
// The static route: `--ui-dir`.
// ---------------------------------------------------------------------------

const INDEX: &str = "<!doctype html><title>eidolon</title>\n";
const APP: &str = "export const hello = () => 'hi';\n";
/// `secret.txt` inside the UI directory, and beside it, outside.
const INSIDE: &str = "inside-the-ui-directory";
const SECRET: &str = "S3CRET-OUTSIDE-THE-UI-DIRECTORY";

fn ui_dir(dir: &Path) -> std::path::PathBuf {
    let ui = dir.join("ui");
    std::fs::create_dir_all(ui.join("assets")).unwrap();
    std::fs::write(ui.join("index.html"), INDEX).unwrap();
    std::fs::write(ui.join("assets/app.js"), APP).unwrap();
    std::fs::write(ui.join("secret.txt"), INSIDE).unwrap();
    std::fs::write(dir.join("secret.txt"), SECRET).unwrap();
    std::os::unix::fs::symlink(dir.join("secret.txt"), ui.join("escape.txt")).unwrap();
    ui
}

fn header<'a>(resp: &'a str, name: &str) -> Option<&'a str> {
    resp.split("\r\n")
        .skip(1)
        .take_while(|line| !line.is_empty())
        .find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.eq_ignore_ascii_case(name).then(|| value.trim())
        })
}

fn body_of(resp: &str) -> &str {
    resp.split_once("\r\n\r\n").map(|(_, body)| body).unwrap_or("")
}

/// The CSP as a literal: a lost directive looks like no change at all.
fn assert_static_headers(resp: &str) {
    assert_eq!(header(resp, "x-content-type-options"), Some("nosniff"), "{resp:?}");
    assert_eq!(header(resp, "referrer-policy"), Some("no-referrer"), "{resp:?}");
    assert_eq!(header(resp, "cache-control"), Some("no-cache"), "{resp:?}");
    let csp = "default-src 'self'; connect-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'";
    assert_eq!(header(resp, "content-security-policy"), Some(csp), "{resp:?}");
}

fn assert_refused_static(resp: &str) {
    assert_static_headers(resp);
    assert_eq!(body_of(resp), "", "a refusal carries no body: {resp:?}");
    assert!(!resp.contains(SECRET), "a refusal must not carry the file either: {resp:?}");
}

#[tokio::test]
async fn the_root_serves_index_and_a_nested_file_with_its_type() {
    let (agent, user, dir) = app().await;
    let ui = ui_dir(dir.path());
    let (bound, _token, cancel) = serve_ui(agent, user, &ui).await;
    let host = bound.to_string();

    let (code, resp) = call_auth(bound, None, "GET", "/", &host, None).await;
    assert_eq!(code, 200, "{resp:?}");
    assert_eq!(header(&resp, "content-type"), Some("text/html; charset=utf-8"), "{resp:?}");
    assert_eq!(body_of(&resp), INDEX, "the whole file, unfiltered: {resp:?}");
    assert_static_headers(&resp);

    let (code, resp) = call_auth(bound, None, "GET", "/assets/app.js", &host, None).await;
    assert_eq!(code, 200, "{resp:?}");
    assert_eq!(header(&resp, "content-type"), Some("text/javascript; charset=utf-8"), "{resp:?}");
    assert_eq!(header(&resp, "content-length"), Some(APP.len().to_string().as_str()), "{resp:?}");
    assert_eq!(body_of(&resp), APP);
    assert_static_headers(&resp);

    std::fs::write(ui.join("blob.dat"), "raw\n").unwrap();
    let (code, resp) = call_auth(bound, None, "GET", "/blob.dat", &host, None).await;
    assert_eq!(code, 200);
    assert_eq!(header(&resp, "content-type"), Some("application/octet-stream"), "{resp:?}");
    cancel.cancel();
}

#[tokio::test]
async fn a_head_carries_the_headers_and_no_body() {
    let (agent, user, dir) = app().await;
    let ui = ui_dir(dir.path());
    let (bound, _token, cancel) = serve_ui(agent, user, &ui).await;
    let host = bound.to_string();

    let (code, resp) = call_auth(bound, None, "HEAD", "/", &host, None).await;
    assert_eq!(code, 200, "{resp:?}");
    assert_eq!(header(&resp, "content-type"), Some("text/html; charset=utf-8"), "{resp:?}");
    assert_eq!(header(&resp, "content-length"), Some(INDEX.len().to_string().as_str()), "{resp:?}");
    assert_eq!(body_of(&resp), "", "a HEAD has no body: {resp:?}");
    assert_static_headers(&resp);

    let (code, resp) = call_auth(bound, None, "HEAD", "/missing.js", &host, None).await;
    assert_eq!(code, 404, "{resp:?}");
    assert_refused_static(&resp);
    cancel.cancel();
}

/// The file outside exists and is readable, so a 404 is the door refusing.
#[tokio::test]
async fn every_escape_lands_outside_the_directory_and_is_refused() {
    let (agent, user, dir) = app().await;
    let ui = ui_dir(dir.path());
    let (bound, _token, cancel) = serve_ui(agent, user, &ui).await;
    let host = bound.to_string();
    assert_eq!(std::fs::read_to_string(dir.path().join("secret.txt")).unwrap(), SECRET);

    // The same name inside is served, or "no secret came back" proves nothing.
    let (code, resp) = call_auth(bound, None, "GET", "/secret.txt", &host, None).await;
    assert_eq!(code, 200, "{resp:?}");
    assert_eq!(body_of(&resp), INSIDE, "the file in the directory, not the one beside it");

    for path in [
        "/../secret.txt",
        "/%2e%2e/secret.txt",
        "/..%2fsecret.txt",
        "/%2e%2e%2fsecret.txt",
        "/assets/../../secret.txt",
        "/..\\secret.txt",
        "/%5c..%5csecret.txt",
        "/%00",
        "/assets/%00.js",
        "/escape.txt",
    ] {
        let (code, resp) = call_auth(bound, None, "GET", path, &host, None).await;
        assert_eq!(code, 404, "{path} must not be served: {resp:?}");
        assert!(!resp.contains(SECRET), "{path} came back with the file outside the directory: {resp:?}");
        assert!(!resp.contains(INSIDE), "{path} came back with the directory's own file: {resp:?}");
        assert_refused_static(&resp);
    }
    cancel.cancel();
}

#[tokio::test]
async fn a_directory_is_never_listed_and_a_missing_file_is_the_same_404() {
    let (agent, user, dir) = app().await;
    let ui = ui_dir(dir.path());
    let (bound, _token, cancel) = serve_ui(agent, user, &ui).await;
    let host = bound.to_string();

    for path in ["/assets", "/assets/", "/assets/../assets", "/missing.js", "/index.htm"] {
        let (code, resp) = call_auth(bound, None, "GET", path, &host, None).await;
        assert_eq!(code, 404, "{path}: {resp:?}");
        assert!(!resp.contains("app.js"), "{path} listed a directory: {resp:?}");
        assert_refused_static(&resp);
    }
    cancel.cancel();

    let (agent, user, dir) = app().await;
    let empty = dir.path().join("empty");
    std::fs::create_dir(&empty).unwrap();
    let (bound, _token, cancel) = serve_ui(agent, user, &empty).await;
    let (code, resp) = call_auth(bound, None, "GET", "/", &bound.to_string(), None).await;
    assert_eq!(code, 404, "no index.html is no index.html: {resp:?}");
    assert!(!resp.contains("index"), "and not a listing either: {resp:?}");
    cancel.cancel();
}

#[tokio::test]
async fn only_get_and_head_are_served_from_the_ui_directory() {
    let (agent, user, dir) = app().await;
    let ui = ui_dir(dir.path());
    let (bound, token, cancel) = serve_ui(agent, user, &ui).await;
    let host = bound.to_string();

    for (method, body) in [("POST", Some("{\"text\":\"hi\"}")), ("PUT", None), ("DELETE", None), ("PATCH", None)] {
        for path in ["/", "/assets/app.js", "/missing.js", "/../secret.txt"] {
            let (code, resp) = call(bound, &token, method, path, &host, body).await;
            assert_eq!(code, 405, "{method} {path}: {resp:?}");
            assert_eq!(header(&resp, "allow"), Some("GET, HEAD"), "{method} {path}: {resp:?}");
            assert_refused_static(&resp);
        }
    }
    cancel.cancel();
}

#[tokio::test]
async fn static_files_need_no_token_and_the_api_still_does() {
    let (agent, user, dir) = app().await;
    let ui = ui_dir(dir.path());
    let (bound, _token, cancel) = serve_ui(agent, user, &ui).await;
    let host = bound.to_string();

    let (code, resp) = call_auth(bound, None, "GET", "/", &host, None).await;
    assert_eq!(code, 200, "a static path with no token at all: {resp:?}");

    for (method, path, body) in [
        ("GET", "/api/events", None),
        ("GET", "/api/secret", None),
        ("POST", "/api/say", Some("{\"text\":\"hi\"}")),
        ("POST", "/api/answer", Some("{\"ask_id\":1,\"answer\":\"yes\"}")),
        ("POST", "/api/cancel", None),
    ] {
        let (code, resp) = call_auth(bound, None, method, path, &host, body).await;
        assert_eq!(code, 401, "{method} {path} without a token: {resp:?}");
        assert_eq!(body_of(&resp), "", "the 401 says nothing: {resp:?}");
        assert_eq!(header(&resp, "x-content-type-options"), Some("nosniff"), "an /api refusal carries its two headers too: {resp:?}");
    }
    cancel.cancel();
}

#[tokio::test]
async fn the_api_namespace_is_whole_and_a_neighbouring_file_is_not() {
    let (agent, user, _dir) = app().await;
    let (bound, token, cancel) = serve(agent, user).await;
    let host = bound.to_string();

    for method in ["GET", "POST"] {
        let (code, resp) = call_auth(bound, None, method, "/api", &host, None).await;
        assert_eq!(code, 401, "{method} /api with no token: {resp:?}");
    }
    let (unknown, _) = call(bound, &token, "GET", "/api/nope", &host, None).await;
    let (code, resp) = call(bound, &token, "GET", "/api", &host, None).await;
    assert_eq!(code, unknown, "the bare /api past the token is the 404 no route claims: {resp:?}");
    cancel.cancel();

    let (agent, user, dir) = app().await;
    let ui = ui_dir(dir.path());
    std::fs::write(ui.join("api.js"), APP).unwrap();
    let (bound, token, cancel) = serve_ui(agent, user, &ui).await;
    let host = bound.to_string();

    for method in ["GET", "POST"] {
        let (code, resp) = call_auth(bound, None, method, "/api", &host, None).await;
        assert_eq!(code, 401, "{method} /api with no token and a UI directory: {resp:?}");
    }
    let (unknown, _) = call(bound, &token, "GET", "/api/nope", &host, None).await;
    let (code, resp) = call(bound, &token, "GET", "/api", &host, None).await;
    assert_eq!(code, unknown, "and the file directory does not answer the bare /api either: {resp:?}");

    let (code, resp) = call_auth(bound, None, "GET", "/api.js", &host, None).await;
    assert_eq!(code, 200, "/api.js is a static path: {resp:?}");
    assert_eq!(body_of(&resp), APP, "{resp:?}");
    assert_static_headers(&resp);
    cancel.cancel();
}

/// Through `lib::run`, so the token looked for is the run's own, from its file.
#[tokio::test]
async fn a_served_file_carries_no_token_of_the_run() {
    let (agent, user, dir) = app().await;
    let ui = ui_dir(dir.path());
    let runtime = dir.path().join("runtime");
    let cancel = CancellationToken::new();
    let running = tokio::spawn(eidolon_web::run(
        agent,
        user,
        eidolon_web::WebOptions {
            bind: "127.0.0.1:0".parse().unwrap(),
            ui_dir: Some(ui),
            cwd: dir.path().to_path_buf(),
            runtime_dir: runtime.clone(),
            yolo: Default::default(),
            doorbell: None,
            presence: None,
        },
        cancel.clone(),
    ));

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let (bound, token) = loop {
        let files = token_files(&runtime);
        if let Some(name) = files.first() {
            let port: u16 = name.trim_start_matches("web-").trim_end_matches(".token").parse().unwrap();
            break (SocketAddr::from(([127, 0, 0, 1], port)), std::fs::read_to_string(runtime.join(name)).unwrap());
        }
        assert!(std::time::Instant::now() < deadline, "no token file in {} within 10s", runtime.display());
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    };
    // A real token, not an empty string that no response could contain.
    assert_eq!(token.len(), 43, "the run's token, out of its own file: {token:?}");
    let host = bound.to_string();

    for (path, want) in [("/", INDEX), ("/assets/app.js", APP)] {
        let (code, resp) = call_auth(bound, None, "GET", path, &host, None).await;
        assert_eq!(code, 200, "{path}: {resp:?}");
        assert_eq!(body_of(&resp), want, "{path} was not served as written: {resp:?}");
        assert!(!resp.contains(&token), "{path} carries this run's token: {resp:?}");
        assert_static_headers(&resp);
    }
    cancel.cancel();
    running.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_rebound_host_does_not_read_a_static_file() {
    let (agent, user, dir) = app().await;
    let ui = ui_dir(dir.path());
    let (bound, _token, cancel) = serve_ui(agent, user, &ui).await;

    for path in ["/", "/index.html", "/assets/app.js"] {
        let (code, resp) = call_auth(bound, None, "GET", path, "evil.example:4477", None).await;
        assert_eq!(code, 403, "a rebound name must not reach {path}: {resp:?}");
        assert_static_headers(&resp);
        assert_eq!(body_of(&resp), "{\"error\":\"Host does not match the bound address\"}", "{resp:?}");
        let (code, resp) = call_auth(bound, None, "HEAD", path, "evil.example", None).await;
        assert_eq!(code, 403, "…nor on a HEAD: {resp:?}");
        assert!(!resp.contains(SECRET), "{resp:?}");
    }
    // The control: the bound authority is served.
    let (code, resp) = call_auth(bound, None, "GET", "/", &bound.to_string(), None).await;
    assert_eq!(code, 200, "{resp:?}");
    cancel.cancel();
}

/// Sparse files, and a HEAD at the cap, so 32 MiB is never read or sent.
#[tokio::test]
async fn a_file_over_the_cap_is_refused_and_one_at_the_cap_is_not() {
    use eidolon_web::files::MAX_FILE;
    let (agent, user, dir) = app().await;
    let ui = ui_dir(dir.path());
    std::fs::File::create(ui.join("big.bin")).unwrap().set_len(MAX_FILE + 1).unwrap();
    std::fs::File::create(ui.join("at-cap.bin")).unwrap().set_len(MAX_FILE).unwrap();
    let (bound, _token, cancel) = serve_ui(agent, user, &ui).await;
    let host = bound.to_string();

    let (code, resp) = call_auth(bound, None, "GET", "/big.bin", &host, None).await;
    assert_eq!(code, 413, "over the cap: {resp:?}");
    assert_refused_static(&resp);

    let (code, resp) = call_auth(bound, None, "HEAD", "/at-cap.bin", &host, None).await;
    assert_eq!(code, 200, "the cap itself is served: {resp:?}");
    assert_eq!(header(&resp, "content-length"), Some(MAX_FILE.to_string().as_str()), "{resp:?}");
    assert_eq!(header(&resp, "content-type"), Some("application/octet-stream"), "{resp:?}");
    cancel.cancel();
}

#[tokio::test]
async fn run_refuses_a_ui_dir_that_is_not_a_directory() {
    let (agent, user, dir) = app().await;
    let runtime = dir.path().join("runtime");
    let missing = dir.path().join("nowhere");
    let file = dir.path().join("a.txt");
    std::fs::write(&file, "not a directory").unwrap();

    for ui_dir in [missing.clone(), file.clone()] {
        let err = eidolon_web::run(
            agent.clone(),
            user.clone(),
            eidolon_web::WebOptions {
                bind: "127.0.0.1:0".parse().unwrap(),
                ui_dir: Some(ui_dir.clone()),
                cwd: dir.path().to_path_buf(),
                runtime_dir: runtime.clone(),
                yolo: Default::default(),
                doorbell: None,
                presence: None,
            },
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains(&ui_dir.display().to_string()), "the refusal names the path: {msg}");
        assert!(token_files(&runtime).is_empty(), "no token file for a door that never started");
    }
}

