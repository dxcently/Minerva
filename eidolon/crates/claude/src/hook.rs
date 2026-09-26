//! The pre-tool hook, both ends.
//!
//! **Server** ([`serve`]): a unix socket the driver listens on for the life
//! of one turn. Each connection is one hook invocation: the CLI's
//! `PreToolUse` payload (`tool_name`, `tool_input`, `tool_use_id`) comes
//! in, [`Dispatcher::adjudicate`] decides, and a Claude Code hook decision
//! goes back. Anything that goes wrong is a deny.
//!
//! **Client** ([`run_client`]): what `eidolon hook` does — read stdin,
//! connect to `$EIDOLON_HOOK_SOCKET`, forward, print the decision. If the
//! socket is missing or unreachable, print a deny. Synchronous and
//! dependency-free on purpose: it runs once per tool call in a fresh
//! process, and it must never hang the CLI.

use std::path::Path;
use std::sync::Arc;

use anyhow::Context as _;
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

use eidolon_core::dispatch::Dispatcher;
use eidolon_core::ipc;
use eidolon_core::tool::{CallOrigin, ToolCall, ToolManifest};

/// Listen on `path`; returns the task so the driver can abort it.
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
                // A read that fails — or a peer offering more than
                // `MAX_REQUEST` — gets no decision and no reply; the
                // connection just goes away, which the client reads as an
                // empty, unparsable reply.
                let Ok(buf) = ipc::read_request(&mut stream).await else { return };
                let decision = decide(&d, &buf, &c).await;
                let _ = stream.write_all(decision.to_string().as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    }))
}

async fn decide(d: &Dispatcher, payload: &[u8], cancel: &CancellationToken) -> Value {
    let payload: Value = match serde_json::from_slice(payload) {
        Ok(v) => v,
        Err(e) => return deny(&format!("unreadable hook payload: {e}")),
    };
    let name = payload
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if name.is_empty() {
        return deny("hook payload has no tool_name");
    }
    // Our own tools, arriving back through the CLI's MCP client, are
    // adjudicated *and* executed by `Dispatcher::dispatch`. Asking here as
    // well would put the same call in front of the user twice.
    if crate::is_eidolon_tool(&name) {
        return json!({ "hookSpecificOutput": { "hookEventName": "PreToolUse", "permissionDecision": "allow", "permissionDecisionReason": "eidolon tool; adjudicated at dispatch" } });
    }
    let input = payload.get("tool_input").cloned().unwrap_or(Value::Null);
    let id = payload
        .get("tool_use_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("hook_{}", crate::now_ms()));
    let call = ToolCall {
        id,
        name: name.clone(),
        input,
        origin: CallOrigin::Model,
    };
    let manifest = ToolManifest::synthetic(&name, crate::approval_for(&name));
    match d.adjudicate(&call, &manifest, cancel).await {
        Ok(()) => {
            json!({ "hookSpecificOutput": { "hookEventName": "PreToolUse", "permissionDecision": "allow", "permissionDecisionReason": "eidolon policy" } })
        }
        Err(reason) => deny(&reason),
    }
}

fn deny(reason: &str) -> Value {
    json!({ "hookSpecificOutput": { "hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": format!("eidolon: {reason}") } })
}

/// The `eidolon hook` subcommand body. Never returns an error to the
/// caller: every failure prints a deny and exits 0, because a non-zero
/// hook exit is itself interpreted by the CLI and we want one behaviour.
pub fn run_client() {
    use std::io::{Read, Write};
    let mut payload = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut payload);
    let out = match std::env::var(crate::HOOK_SOCKET_ENV) {
        Err(_) => deny("no hook socket configured"),
        Ok(path) => match forward(&path, &payload) {
            Ok(v) => v,
            Err(e) => deny(&format!("hook socket error: {e}")),
        },
    };
    println!("{out}");
    let _ = std::io::stdout().flush();
}

fn forward(path: &str, payload: &[u8]) -> anyhow::Result<Value> {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    let mut s = UnixStream::connect(path).with_context(|| format!("connecting to {path}"))?;
    // Both directions: the request is small, but a server wedged before its
    // first read would otherwise hold the write forever.
    s.set_read_timeout(Some(std::time::Duration::from_secs(600)))?;
    s.set_write_timeout(Some(std::time::Duration::from_secs(600)))?;
    s.write_all(payload)?;
    s.shutdown(std::net::Shutdown::Write)?;
    let mut reply = Vec::new();
    s.read_to_end(&mut reply)?;
    serde_json::from_slice(&reply).context("parsing the hook decision")
}

#[cfg(test)]
mod tests {
    use super::*;
    use eidolon_core::policy::AllowAll;
    use eidolon_core::testing::AlwaysAsk;
    use eidolon_core::testing::ScriptedUser;
    use eidolon_core::{EventBus, Session, ToolRegistry};
    use tokio::sync::Mutex;

    async fn fwd(sock: &std::path::Path, payload: &'static [u8]) -> Value {
        let p = sock.to_str().unwrap().to_string();
        tokio::task::spawn_blocking(move || forward(&p, payload).unwrap())
            .await
            .unwrap()
    }

    fn dispatcher(
        dir: &std::path::Path,
        name: &str,
        policy: Arc<dyn eidolon_core::policy::PolicyHook>,
        confirms: bool,
    ) -> Arc<Dispatcher> {
        let session = Session::create(&dir.join(format!("{name}.log")), "m", dir, None).unwrap();
        let user = ScriptedUser::new(confirms);
        Arc::new(Dispatcher::new(
            ToolRegistry::new(),
            policy,
            user,
            EventBus::default(),
            Arc::new(Mutex::new(session)),
            dir.to_path_buf(),
        ))
    }

    /// The hook carries the policy's verdict, whatever it is. The shipped
    /// hook allows everything; this covers the escalation path a real
    /// classifier will use.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_hook_reports_whatever_policy_decided() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("allow.sock");
        let task = serve(
            &sock,
            dispatcher(dir.path(), "allow", Arc::new(AllowAll), true),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let out = fwd(
            &sock,
            br#"{"tool_name":"Bash","tool_input":{"command":"rm -rf /"},"tool_use_id":"t1"}"#,
        )
        .await;
        assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "allow");
        task.abort();

        // A hook that escalates, and a user who says no.
        let sock = dir.path().join("ask.sock");
        let task = serve(
            &sock,
            dispatcher(dir.path(), "ask", Arc::new(AlwaysAsk), false),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let out = fwd(
            &sock,
            br#"{"tool_name":"Bash","tool_input":{"command":"rm -rf /"},"tool_use_id":"t2"}"#,
        )
        .await;
        assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "deny");
        assert!(
            out["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .unwrap()
                .contains("declined")
        );
        task.abort();
    }

    /// The chokepoint fails closed: an unreadable payload is never allowed
    /// through just because policy would have allowed it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_malformed_payload_is_denied_even_under_allow_all() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("h.sock");
        let task = serve(
            &sock,
            dispatcher(dir.path(), "junk", Arc::new(AllowAll), true),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            fwd(&sock, b"not json").await["hookSpecificOutput"]["permissionDecision"],
            "deny"
        );
        assert_eq!(
            fwd(&sock, br#"{"tool_input":{}}"#).await["hookSpecificOutput"]["permissionDecision"],
            "deny"
        );
        task.abort();
    }

    /// Our own MCP tools are adjudicated at dispatch, so the hook must let
    /// them past rather than asking a second time.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn eidolon_mcp_tools_skip_the_hook() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("h.sock");
        // A policy that would escalate, and a user who declines: if the hook
        // consulted them, this would come back denied.
        let task = serve(
            &sock,
            dispatcher(dir.path(), "mcp", Arc::new(AlwaysAsk), false),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let out = fwd(&sock, br#"{"tool_name":"mcp__eidolon__bash","tool_input":{"command":"ls"},"tool_use_id":"t1"}"#).await;
        assert_eq!(out["hookSpecificOutput"]["permissionDecision"], "allow");
        task.abort();
    }
}
