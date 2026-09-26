//! The harness's tool registry, served to the Claude CLI as an MCP server.
//!
//! This is what makes "lock Claude to our tools" possible. With
//! [`crate::ToolMode::Eidolon`] the CLI is launched with `--tools ""` (no
//! built-ins), `--mcp-config` naming this server and `--strict-mcp-config`
//! (no user or project servers), so the only tools it can reach are the
//! ones in [`eidolon_core::tool::ToolRegistry`].
//!
//! ## Why that is worth doing
//!
//! Normally the CLI runs its own tools and the harness only *adjudicates*
//! them through the `PreToolUse` hook, deciding without executing. Through
//! MCP the calls come back to
//! [`Dispatcher::execute`](eidolon_core::dispatch::Dispatcher::execute), so
//! the work is done by the harness's own primitives, in its working
//! directory, under its policy — the same code path a `--provider` turn
//! uses. The CLI becomes a planner rather than an actor.
//!
//! Journaling stays with the stream sink, not this bridge. The CLI runs a
//! tool *before* it reports the assistant message carrying the `tool_use`,
//! so a result journaled here would land ahead of its own call and unpaired
//! beside the one the sink writes. `execute` exists for exactly this: the
//! harness owns policy and execution, the stream owns the log's order.
//!
//! ## Two ends, because the Dispatcher is in the parent
//!
//! The CLI spawns MCP servers itself, so `eidolon mcp` runs as a *grandchild*
//! process with no access to the session, the registry or the user. It is
//! therefore a bridge, exactly as `eidolon hook` is: it speaks MCP on stdio
//! and forwards each request over a unix socket to [`serve`], which is
//! running inside the harness and holds the real `Dispatcher`.
//!
//! **Never leave the CLI with no tools at all.** A model carrying a system
//! prompt that describes tools, but with no working tool channel, does not
//! stop — it writes plausible tool calls *and their results* as prose (the
//! classic ReAct "hallucinated observation"). `--tools ""` and
//! `--mcp-config` must ship in the same invocation.

use std::path::Path;
use std::sync::Arc;

use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

use eidolon_core::dispatch::Dispatcher;
use eidolon_core::ipc;
use eidolon_core::tool::{CallOrigin, ToolCall};

/// The MCP server name the CLI knows us by. Tools therefore reach the model
/// as `mcp__eidolon__<tool>`; [`crate::is_eidolon_tool`] recognises them.
pub const SERVER: &str = "eidolon";

/// Protocol version offered when the client does not name one.
const PROTOCOL: &str = "2024-11-05";

// ---------------------------------------------------------------- server

/// Listen on `path` for bridge requests. Returns the task so the driver can
/// abort it at the end of the turn, exactly as it does the hook listener.
pub async fn serve(
    path: &Path,
    dispatcher: Arc<Dispatcher>,
    cancel: CancellationToken,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    let listener = ipc::bind_private(path)?;
    Ok(tokio::spawn(async move {
        loop {
            let (mut stream, _) = tokio::select! {
                r = listener.accept() => match r { Ok(x) => x, Err(_) => break },
                _ = cancel.cancelled() => break,
            };
            let d = dispatcher.clone();
            let c = cancel.clone();
            tokio::spawn(async move {
                // As the hook listener: an implausible peer gets a closed
                // connection, not a reply.
                let Ok(buf) = ipc::read_request(&mut stream).await else { return };
                let reply = answer(&d, &buf, &c).await;
                let _ = stream.write_all(reply.to_string().as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    }))
}

/// One bridge request: `{"op":"list"}` or
/// `{"op":"call","name":…,"input":…}`.
async fn answer(d: &Dispatcher, payload: &[u8], cancel: &CancellationToken) -> Value {
    let req: Value = match serde_json::from_slice(payload) {
        Ok(v) => v,
        Err(e) => return json!({ "error": format!("unreadable bridge request: {e}") }),
    };
    match req.get("op").and_then(Value::as_str) {
        Some("list") => {
            let tools: Vec<Value> = d
                .registry()
                .manifests()
                .into_iter()
                .map(|m| json!({ "name": m.name, "description": m.description, "inputSchema": m.input_schema }))
                .collect();
            json!({ "tools": tools })
        }
        Some("call") => {
            let name = req
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if name.is_empty() {
                return json!({ "error": "call needs a `name`" });
            }
            let input = req.get("input").cloned().unwrap_or_else(|| json!({}));
            // A model-originated call, because that is what it is: the CLI is
            // relaying its model's tool use. Policy sees it as such, and the
            // id is the CLI's own `tool_use` id so the two views agree.
            let id = req
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("mcp_{}", crate::now_ms()));
            let call = ToolCall {
                id,
                name,
                input,
                origin: CallOrigin::Model,
            };
            // `execute`, not `dispatch`: the stream sink journals and
            // publishes this call in stream order. See `Dispatcher::execute`.
            let out = d.execute(&call, cancel.child_token()).await;
            json!({ "content": out.content, "is_error": out.is_error })
        }
        other => json!({ "error": format!("unknown bridge op {other:?}") }),
    }
}

// ---------------------------------------------------------------- client

/// The `eidolon mcp` subcommand body: MCP over stdio, forwarded to [`serve`].
///
/// Synchronous and free of the async runtime on purpose — it is a pipe, and
/// a stalled MCP server stalls the CLI's whole turn.
pub fn run_client() {
    use std::io::{BufRead, Write};
    let socket = std::env::var(crate::MCP_SOCKET_ENV).ok();
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        mcp_log(&line);
        let Ok(req) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        // A request without an id is a notification: act, answer nothing.
        let Some(id) = req.get("id").cloned() else {
            continue;
        };
        let method = req
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let reply = match respond(method, req.get("params"), socket.as_deref()) {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(message) => {
                json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32603, "message": message } })
            }
        };
        if writeln!(out, "{reply}").is_err() || out.flush().is_err() {
            break;
        }
    }
}

/// Raw MCP request tracing: set `EIDOLON_MCP_LOG=/path` to append every
/// request line the CLI sends. The counterpart to `EIDOLON_WIRE_LOG`.
fn mcp_log(line: &str) {
    use std::io::Write as _;
    let Some(path) = std::env::var_os("EIDOLON_MCP_LOG") else {
        return;
    };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
}

fn respond(method: &str, params: Option<&Value>, socket: Option<&str>) -> Result<Value, String> {
    match method {
        "initialize" => {
            // Echo the client's protocol version when it names one; a server
            // that insists on its own is the usual cause of a failed
            // handshake.
            let version = params
                .and_then(|p| p.get("protocolVersion"))
                .and_then(Value::as_str)
                .unwrap_or(PROTOCOL)
                .to_string();
            Ok(json!({
                "protocolVersion": version,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": SERVER, "version": env!("CARGO_PKG_VERSION") },
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => {
            let reply = bridge(socket, &json!({ "op": "list" }))?;
            Ok(json!({ "tools": reply.get("tools").cloned().unwrap_or_else(|| json!([])) }))
        }
        "tools/call" => {
            let name = params
                .and_then(|p| p.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let input = params
                .and_then(|p| p.get("arguments"))
                .cloned()
                .unwrap_or_else(|| json!({}));
            // Claude Code passes its `tool_use` id here; carrying it through
            // keeps the harness's view of the call and the CLI's identical.
            let id = params
                .and_then(|p| p.pointer("/_meta/claudecode~1toolUseId"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let reply = bridge(
                socket,
                &json!({ "op": "call", "name": name, "input": input, "id": id }),
            )?;
            let text = reply
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default();
            // An error is reported as `isError`, not as a JSON-RPC error: it
            // is a result the model must read and act on, not a transport
            // failure.
            Ok(json!({
                "content": [ { "type": "text", "text": text } ],
                "isError": reply.get("is_error").and_then(Value::as_bool).unwrap_or(false),
            }))
        }
        other => Err(format!("unsupported method `{other}`")),
    }
}

fn bridge(socket: Option<&str>, request: &Value) -> Result<Value, String> {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    let path = socket.ok_or("no eidolon mcp socket configured")?;
    let mut s = UnixStream::connect(path).map_err(|e| format!("connecting to {path}: {e}"))?;
    // Generous on both directions: a tool call may sit behind an approval
    // dialog the person at the keyboard has not answered yet.
    s.set_read_timeout(Some(std::time::Duration::from_secs(3600)))
        .map_err(|e| e.to_string())?;
    s.set_write_timeout(Some(std::time::Duration::from_secs(3600)))
        .map_err(|e| e.to_string())?;
    s.write_all(request.to_string().as_bytes())
        .map_err(|e| e.to_string())?;
    s.shutdown(std::net::Shutdown::Write)
        .map_err(|e| e.to_string())?;
    let mut reply = Vec::new();
    s.read_to_end(&mut reply).map_err(|e| e.to_string())?;
    let reply: Value =
        serde_json::from_slice(&reply).map_err(|e| format!("parsing the bridge reply: {e}"))?;
    if let Some(e) = reply.get("error").and_then(Value::as_str) {
        return Err(e.to_string());
    }
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;
    use eidolon_core::policy::AllowAll;
    use eidolon_core::testing::AlwaysAsk;
    use eidolon_core::testing::ScriptedUser;
    use eidolon_core::tool::{CallContext, Tool, ToolManifest, ToolOutput};
    use eidolon_core::{EventBus, Session, ToolRegistry};
    use tokio::sync::Mutex;

    struct Echo(ToolManifest);

    #[async_trait::async_trait]
    impl Tool for Echo {
        fn manifest(&self) -> &ToolManifest {
            &self.0
        }
        async fn call(&self, input: Value, _: CallContext) -> anyhow::Result<ToolOutput> {
            Ok(ToolOutput::ok(
                input
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            ))
        }
    }

    fn registry() -> ToolRegistry {
        let mut r = ToolRegistry::new();
        r.register(Arc::new(Echo(ToolManifest {
            name: "echo".into(),
            description: "Echo the text back".into(),
            input_schema: json!({ "type": "object", "properties": { "text": { "type": "string" } }, "required": ["text"] }),
            approval: eidolon_core::policy::Approval::ReadOnly,
            prompt: None,
            render: None,
            deferred: false,
        })));
        r
    }

    async fn bridged(sock: &Path, req: Value) -> Value {
        let p = sock.to_str().unwrap().to_string();
        tokio::task::spawn_blocking(move || bridge(Some(&p), &req).unwrap())
            .await
            .unwrap()
    }

    fn dispatcher(
        dir: &Path,
        policy: Arc<dyn eidolon_core::policy::PolicyHook>,
        user: Arc<dyn eidolon_core::user::UserIo>,
    ) -> Arc<Dispatcher> {
        let session = Session::create(&dir.join("s.log"), "m", dir, None).unwrap();
        Arc::new(Dispatcher::new(
            registry(),
            policy,
            user,
            EventBus::default(),
            Arc::new(Mutex::new(session)),
            dir.to_path_buf(),
        ))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn list_and_call_go_through_the_dispatcher() {
        let dir = tempfile::tempdir().unwrap();
        let d = dispatcher(dir.path(), Arc::new(AllowAll), ScriptedUser::new(true));
        let sock = dir.path().join("m.sock");
        let task = serve(&sock, d.clone(), CancellationToken::new())
            .await
            .unwrap();

        let listed = bridged(&sock, json!({ "op": "list" })).await;
        assert_eq!(listed["tools"][0]["name"], "echo");
        // MCP spells it `inputSchema`; the manifest spells it `input_schema`.
        assert_eq!(listed["tools"][0]["inputSchema"]["type"], "object");

        let called = bridged(
            &sock,
            json!({ "op": "call", "name": "echo", "input": { "text": "hi" } }),
        )
        .await;
        assert_eq!(called["content"], "hi");
        assert_eq!(called["is_error"], false);

        // The bridge must NOT journal: the stream sink does, in stream order.
        // A record here would be an unpaired duplicate ahead of its own call.
        let session = d.session().lock().await;
        assert!(
            !session.branch().iter().any(|r| matches!(
                &r.kind,
                eidolon_core::session::RecordKind::ToolResult { .. }
            )),
            "the bridge journaled a tool result; the sink owns that"
        );
        task.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn policy_still_decides() {
        let dir = tempfile::tempdir().unwrap();
        // A policy that asks + a user who declines: the call must not run.
        let mut reg = registry();
        reg.register(Arc::new(Echo(ToolManifest {
            name: "mutate".into(),
            description: String::new(),
            input_schema: json!({ "type": "object" }),
            approval: eidolon_core::policy::Approval::Mutating,
            prompt: None,
            render: None,
            deferred: false,
        })));
        let session = Session::create(&dir.path().join("s.log"), "m", dir.path(), None).unwrap();
        let d = Arc::new(Dispatcher::new(
            reg,
            Arc::new(AlwaysAsk),
            ScriptedUser::new(false),
            EventBus::default(),
            Arc::new(Mutex::new(session)),
            dir.path().to_path_buf(),
        ));
        let sock = dir.path().join("m.sock");
        let task = serve(&sock, d, CancellationToken::new()).await.unwrap();

        let out = bridged(
            &sock,
            json!({ "op": "call", "name": "mutate", "input": {} }),
        )
        .await;
        assert_eq!(out["is_error"], true);
        assert!(
            out["content"].as_str().unwrap().contains("declined"),
            "{out}"
        );
        task.abort();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unknown_tool_is_an_error_result_not_a_transport_failure() {
        let dir = tempfile::tempdir().unwrap();
        let d = dispatcher(dir.path(), Arc::new(AllowAll), ScriptedUser::new(true));
        let sock = dir.path().join("m.sock");
        let task = serve(&sock, d, CancellationToken::new()).await.unwrap();
        let out = bridged(&sock, json!({ "op": "call", "name": "nope", "input": {} })).await;
        assert_eq!(out["is_error"], true);
        assert!(
            out["content"].as_str().unwrap().contains("unknown tool"),
            "{out}"
        );
        task.abort();
    }

    #[test]
    fn initialize_echoes_the_clients_protocol_version() {
        let r = respond(
            "initialize",
            Some(&json!({ "protocolVersion": "2025-06-18" })),
            None,
        )
        .unwrap();
        assert_eq!(r["protocolVersion"], "2025-06-18");
        assert_eq!(r["serverInfo"]["name"], SERVER);
        assert!(r["capabilities"]["tools"].is_object());
        // With no version named, we offer our own rather than nothing.
        let r = respond("initialize", None, None).unwrap();
        assert_eq!(r["protocolVersion"], PROTOCOL);
    }

    #[test]
    fn unsupported_methods_are_reported_as_such() {
        assert!(respond("resources/list", None, None).is_err());
        assert_eq!(respond("ping", None, None).unwrap(), json!({}));
    }
}
