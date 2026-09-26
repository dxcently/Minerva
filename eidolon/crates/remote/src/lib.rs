//! Typed, first-class access to Mneme — no MCP client, no per-tool schema
//! registration.
//!
//! The vault decision (2026-09-03, *Mneme/Melete/VV integration*) cut MCP
//! as an *abstraction*: the harness does not negotiate a tool list and
//! register 27 functions into the model's context. It calls two things,
//! `dispatch(utterance, bindings)` and `RPC({function, args})`, and those
//! are the whole surface. MCP survives only as the **transport** Mneme
//! happens to speak: a JSON-RPC `tools/call` over streamable HTTP, behind
//! the same passphrase-gated OAuth Melete uses (`harnox::oauth_client`).
//!
//! What [`Mneme::call_tool`] does, exactly (mirrors Melete's
//! `attachments::call_mneme_tool`, which is the proven recipe):
//!
//! 1. resolve the bearer — mint from the passphrase in a `0600` file (the
//!    minted token cached in memory only), or take a static one from a
//!    file or the named environment variable;
//! 2. `initialize` → capture `Mcp-Session-Id` → `notifications/initialized`
//!    (rmcp's stateful transport rejects a bare `tools/call` with 422);
//! 3. `tools/call` with the session id; the body may be JSON or an SSE
//!    frame with priming events, so both are parsed;
//! 4. a JSON-RPC `error` or a `result.isError` becomes an `Err` carrying the
//!    code and message; otherwise the concatenated text content is returned;
//! 5. on a 401/403 the cached token is dropped and the call retried once.
//!
//! [`McpClient`] is that recipe with the server's name on it; [`Mneme`] is
//! the two calls over it, and [`melete`] is the other server — the job
//! harness, whose listing becomes deferred tools on the registry rather
//! than two functions, for the reason that module gives.

use std::path::PathBuf;
use std::sync::{Arc, LazyLock};

use anyhow::{Context, bail};
use serde_json::Value;
use zeroize::Zeroizing;

use harnox::oauth_client::{ClientIdentity, TokenCache, CONNECT_TIMEOUT, oauth_base};

use eidolon_core::attribution::Attribution;

pub mod melete;
pub use melete::Melete;

static MNEME_IDENTITY: LazyLock<ClientIdentity> = LazyLock::new(|| ClientIdentity {
    client_name: "eidolon".into(),
    redirect_uri: "http://127.0.0.1/eidolon-cb".into(),
    scope: "vault.read".into(),
});
static MELETE_IDENTITY: LazyLock<ClientIdentity> = LazyLock::new(|| ClientIdentity {
    client_name: "eidolon".into(),
    redirect_uri: "http://127.0.0.1/eidolon-cb".into(),
    scope: "jobs.run".into(),
});
static CACHE: LazyLock<TokenCache> = LazyLock::new(TokenCache::new);

const PROTOCOL_VERSION: &str = "2025-06-18";

/// How the harness authenticates to one server.
#[derive(Clone, Debug)]
pub enum Credential {
    /// Operator passphrase in a file; the OAuth flow mints a bearer from it.
    PassphraseFile(PathBuf),
    /// A static bearer in a file (no minting).
    TokenFile(PathBuf),
    /// A static bearer in the named environment variable, read at
    /// resolution time (no minting). For projected sessions on fleet
    /// boxes the vault credential arrives via the process environment —
    /// never a file, never argv — so the token needs a non-file carrier.
    TokenEnv(String),
}

/// Which server a client speaks to. The two run the same OAuth server
/// (`harnox::oauth_server`, once vendored twice) and differ in exactly
/// what it is branded with — the scope a bearer is minted for — and in
/// the name their errors carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Server {
    Mneme,
    Melete,
}

impl Server {
    pub fn label(self) -> &'static str {
        match self {
            Server::Mneme => "Mneme",
            Server::Melete => "Melete",
        }
    }

    fn identity(self) -> &'static ClientIdentity {
        match self {
            Server::Mneme => &MNEME_IDENTITY,
            Server::Melete => &MELETE_IDENTITY,
        }
    }
}

/// Raw-wire tracing for the remote MCP clients, mirroring the provider
/// path's `EIDOLON_WIRE_LOG`. Set `EIDOLON_MCP_LOG=/path/to/file` to
/// append every JSON-RPC request body sent to Mneme or Melete, and the
/// response that came back. The remote path otherwise sends blind: when a
/// server reports parameters that differ from what the session journal
/// says left the harness (the 2026-09-06 `lib_rpc` stringified-`call`
/// mystery), this is the only place the actual bytes are visible.
fn mcp_log(tag: &str, text: &str) {
    use std::io::Write as _;
    let Some(path) = std::env::var_os("EIDOLON_MCP_LOG") else {
        return;
    };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "[{tag}] {text}");
    }
}

/// One MCP server as the harness speaks to it: `tools/call` and
/// `tools/list` over streamable HTTP, behind a bearer minted from the
/// passphrase or read from a file or the environment. The recipe in the
/// module docs, with the server's name on every error.
#[derive(Clone, Debug)]
pub struct McpClient {
    pub server: Server,
    pub mcp_url: String,
    pub credential: Credential,
}

impl McpClient {
    pub fn new(server: Server, mcp_url: impl Into<String>, credential: Credential) -> Self {
        McpClient {
            server,
            mcp_url: mcp_url.into(),
            credential,
        }
    }

    /// Resolve the bearer now, read fresh at call time and never logged:
    /// read the file, take the environment variable, or mint from the
    /// passphrase. The value is held in [`Zeroizing`] until the request
    /// drops it.
    async fn token(&self) -> anyhow::Result<Zeroizing<String>> {
        match &self.credential {
            Credential::TokenFile(p) => Ok(Zeroizing::new(read_secret(p)?)),
            Credential::TokenEnv(name) => {
                let v = std::env::var(name)
                    .with_context(|| format!("credential env var {name} is not set"))?;
                let v = v.trim().to_string();
                if v.is_empty() {
                    bail!("credential env var {name} is empty");
                }
                Ok(Zeroizing::new(v))
            }
            Credential::PassphraseFile(p) => {
                let pass = read_secret(p)?;
                let token = CACHE
                    .get_or_mint(&oauth_base(&self.mcp_url), &pass, self.server.identity())
                    .await
                    .with_context(|| format!("minting a {} token", self.server.label()))?;
                Ok(Zeroizing::new(token))
            }
        }
    }

    /// One JSON-RPC request on a fresh MCP session, re-minting the bearer
    /// once if the server rejects it. The `result` member comes back; a
    /// JSON-RPC `error` is an `Err` carrying its code and message.
    pub async fn request(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        let token = self.token().await?;
        match self.request_with(&token, method, params.clone()).await {
            Err(e)
                if looks_like_auth_rejection(&e)
                    && matches!(self.credential, Credential::PassphraseFile(_)) =>
            {
                tracing::info!(
                    server = self.server.label(),
                    "the server rejected the bearer; re-minting once"
                );
                CACHE.invalidate(&oauth_base(&self.mcp_url));
                let fresh = self.token().await?;
                self.request_with(&fresh, method, params).await
            }
            other => other,
        }
    }

    async fn request_with(
        &self,
        token: &str,
        method: &str,
        params: Value,
    ) -> anyhow::Result<Value> {
        let label = self.server.label();
        // OAuth legs are short; remote tools can legitimately take minutes.
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(std::time::Duration::from_secs(300))
            .build().context("building MCP HTTP client")?;
        let session = initialize(&client, &self.mcp_url, token, label).await?;
        let mut rb = client.post(&self.mcp_url).bearer_auth(token).header(
            reqwest::header::ACCEPT,
            "application/json, text/event-stream",
        );
        if let Some(id) = &session {
            rb = rb.header("Mcp-Session-Id", id);
        }
        let payload =
            serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": method, "params": params });
        mcp_log("request", &format!("{method} {}", payload));
        let resp = rb
            .json(&payload)
            .send()
            .await
            .with_context(|| format!("POST {} ({method})", self.mcp_url))?;
        let resp = resp.error_for_status().context("MCP request rejected")?;
        let value = read_rpc_response(resp, 2).await?;
        mcp_log("response", &format!("{method} {value}"));
        if let Some(err) = value.get("error") {
            let msg = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            match err.get("code").and_then(Value::as_i64) {
                Some(code) => bail!("{label} {method} failed: {msg} (code {code})"),
                None => bail!("{label} {method} failed: {msg}"),
            }
        }
        value
            .get("result")
            .cloned()
            .context("MCP response had no result")
    }

    /// The raw `tools/call` result: `content`, `isError` and
    /// `structuredContent`, with nothing folded away.
    ///
    /// [`McpClient::call_tool`] answers with the text of a successful call and
    /// an error for anything marked `isError` — which is what every
    /// model-facing caller wants, and exactly what a caller deciding *why* a
    /// call failed cannot use. Mneme puts a stable code beside the prose on a
    /// permanent failure, and a code is not prose.
    pub async fn call_tool_result(&self, tool: &str, arguments: Value) -> anyhow::Result<Value> {
        self.request(
            "tools/call",
            serde_json::json!({ "name": tool, "arguments": arguments }),
        )
        .await
        .map_err(|e| e.context(format!("{} tool `{tool}`", self.server.label())))
    }

    /// `tools/call`, the concatenated text content back; a result marked
    /// `isError` is an `Err` carrying that text.
    pub async fn call_tool(&self, tool: &str, arguments: Value) -> anyhow::Result<String> {
        let result = self.call_tool_result(tool, arguments).await?;
        let text = collect_content_text(&result);
        if result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            bail!(
                "{} tool `{tool}` reported an error: {}",
                self.server.label(),
                text.trim()
            );
        }
        Ok(text)
    }

    /// `tools/list`, every page of it, each tool as the server describes
    /// it (`name`, `description`, `inputSchema`).
    pub async fn list_tools(&self) -> anyhow::Result<Vec<Value>> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let params = match &cursor {
                Some(c) => serde_json::json!({ "cursor": c }),
                None => serde_json::json!({}),
            };
            let result = self.request("tools/list", params).await?;
            if let Some(page) = result.get("tools").and_then(Value::as_array) {
                tools.extend(page.iter().cloned());
            }
            match result
                .get("nextCursor")
                .and_then(Value::as_str)
                .filter(|c| !c.is_empty())
            {
                Some(next) if cursor.as_deref() != Some(next) => cursor = Some(next.to_string()),
                _ => return Ok(tools),
            }
        }
    }
}

/// Mneme, the vault server: its whole surface as two calls.
#[derive(Clone, Debug)]
pub struct Mneme {
    client: McpClient,
    /// The label every `RPC` this client sends is attributed with — see
    /// [`eidolon_core::attribution`]. Behind an `Arc` so that the copies
    /// handed to the tools' host and to the persona source share one cell:
    /// the client is built by config before the session exists, so the value
    /// arrives afterwards and has to be visible to every clone.
    attribution: Arc<std::sync::RwLock<Option<Arc<Attribution>>>>,
}

impl Mneme {
    pub fn new(mcp_url: impl Into<String>, credential: Credential) -> Self {
        Mneme {
            client: McpClient::new(Server::Mneme, mcp_url, credential),
            attribution: Arc::new(std::sync::RwLock::new(None)),
        }
    }

    /// Label this client's outbound calls with `attribution`. Set by the
    /// session that owns the client, once it knows its own id and working
    /// directory; the agent keeps the persona and model inside the cell
    /// current. Unset — the diagnostic clients in `eidolon tools`, a probe —
    /// is a call with no `attribution` argument at all, which is what the
    /// field's "omit freely" means.
    pub fn set_attribution(&self, attribution: Arc<Attribution>) {
        *self.attribution.write().unwrap() = Some(attribution);
    }

    /// `RPC({function, args})`, verbatim text back.
    ///
    /// The harness owns `args.attribution` here, one level below every
    /// caller: the model's `mneme_rpc` tool, a vault-channel command the loop
    /// resolved, and a user script's `mneme_call` all arrive through this
    /// function, and all of them get the same label stamped on. A value the
    /// model wrote is overwritten rather than passed along — the audit log
    /// records that field verbatim, so a model-chosen label is a claim about
    /// who wrote a note, and this is the only layer that can make it a fact.
    /// Discovery goes the same way: a schema this client fetches comes back
    /// with the field taken out of it. See [`hide_attribution`].
    pub async fn rpc(&self, function: &str, args: Value) -> anyhow::Result<String> {
        let mut args = args;
        self.attribute(&mut args);
        let reply = self
            .client
            .call_tool(
                "RPC",
                serde_json::json!({ "call": { "function": function, "args": args } }),
            )
            .await?;
        Ok(hide_attribution(function, reply))
    }

    /// Stamp the harness's label onto one call's arguments.
    ///
    /// Filled on *every* function rather than the ones whose schema declares
    /// the field: a function list on this side would be a second copy of
    /// Mneme's schema, stale the moment the server declared `attribution` on
    /// one more function, and the ones it does not declare ignore an unknown
    /// argument. A non-object `args` is left alone — there is no key to set,
    /// and the object is what Mneme's handlers take anyway.
    fn attribute(&self, args: &mut Value) {
        let Some(attribution) = self.attribution.read().unwrap().clone() else {
            return;
        };
        if let Some(object) = args.as_object_mut() {
            object.insert("attribution".into(), Value::String(attribution.label()));
        }
    }

    /// `dispatch(utterance, bindings)`, the JSON reply as text.
    ///
    /// Labelled by the harness exactly as [`Mneme::rpc`] is, and for the same
    /// reason: a dispatch that resolves to a write would otherwise be the one
    /// vault write with nobody's name on it. The argument is top level here
    /// rather than nested in a call envelope, which is the only difference the
    /// fill sees. Eidolon's model-facing dispatch tool was cut on 2026-09-14,
    /// so what usually reaches this is a user script — and the label still
    /// comes from the session, not from the script.
    pub async fn dispatch(
        &self,
        utterance: &str,
        bindings: Option<Value>,
    ) -> anyhow::Result<String> {
        let mut args = serde_json::json!({ "utterance": utterance });
        if let Some(b) = bindings {
            args["bindings"] = b;
        }
        self.attribute(&mut args);
        self.client.call_tool("dispatch", args).await
    }

    pub async fn call_tool(&self, tool: &str, arguments: Value) -> anyhow::Result<String> {
        self.client.call_tool(tool, arguments).await
    }

    /// `audit_tail`, the sample a park makes: has the vault's log moved past
    /// `since` under these filters?
    ///
    /// Two knobs of the function are deliberately never sent from here.
    /// **No `wait_ms`**: the same call is made at arm time, inline in the
    /// model's turn, where it has to return at once, and a request held open
    /// buys the same answer for a connection while a parked session is asleep
    /// anyway. **No `latch`**: a park is a read, and a sample that rewrites
    /// the audit manifest would be a write laundered through one.
    ///
    /// Only the two permanent answers are read out of the reply's
    /// `structuredContent` — a code, not the prose beside it. Any other
    /// application error is an `Err`, as is a transport failure: the caller
    /// is free to retry, and a park's deadline is what bounds it.
    pub async fn audit_tail(&self, q: &TailQuery) -> anyhow::Result<Tail> {
        let mut args = serde_json::json!({
            "vault": q.vault,
            "since_seq": q.since,
            "wait_ms": 0,
            "latch": false,
        });
        if let Some(p) = &q.path {
            args["path"] = Value::String(p.clone());
        }
        if let Some(w) = &q.written_by {
            args["written_by"] = Value::String(w.clone());
        }
        // Stamped exactly as `rpc` stamps it: a read still says who read.
        self.attribute(&mut args);
        let result = self
            .client
            .call_tool_result(
                "RPC",
                serde_json::json!({ "call": { "function": "audit_tail", "args": args } }),
            )
            .await
            .context("sampling the vault's audit log")?;

        if result.get("isError").and_then(Value::as_bool).unwrap_or(false) {
            let text = collect_content_text(&result);
            let code = result
                .get("structuredContent")
                .and_then(|s| s.get("code"))
                .and_then(Value::as_str);
            return match code {
                Some("no_function") | Some("unknown_vault") => {
                    Ok(Tail::Gone(text.trim().to_string()))
                }
                _ => bail!(
                    "{} refused the sample: {}",
                    self.client.server.label(),
                    text.trim()
                ),
            };
        }

        let body = collect_content_text(&result);
        let reply: Value = serde_json::from_str(&body)
            .with_context(|| format!("audit_tail replied with something that is not JSON: {}", body.trim()))?;
        // The one bit a park wants: does the filtered tail hold anything past
        // the cursor? A reply whose head moved while this filter matched
        // nothing is *still* — the cursor moving is not the event.
        let moved = reply
            .get("events")
            .and_then(Value::as_array)
            .is_some_and(|events| !events.is_empty());
        Ok(if moved { Tail::Moved } else { Tail::Still })
    }
}

/// One sample of the vault's log: what a `mneme_seq` park asks for.
///
/// `vault` and `since` travel together and neither is optional, because the
/// log's `seq` is per vault — a cursor without its vault is a number with no
/// meaning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TailQuery {
    /// The vault's name as `list_vaults` gives it: a registry name, not a
    /// folder.
    pub vault: String,
    /// The cursor: the `through_seq` a read returned.
    pub since: u64,
    /// Vault-relative path or folder prefix.
    pub path: Option<String>,
    /// Include-only substring of the writer's label.
    pub written_by: Option<String>,
}

/// What one sample came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tail {
    /// There is an event past the cursor under the filters: the wait can fire.
    Moved,
    /// There is not — yet.
    Still,
    /// The vault or the function is not there, and a later call will not find
    /// it. Permanent rather than transient, so a wait on it can never fire and
    /// should say so instead of running out a deadline.
    Gone(String),
}

/// Take the field the harness fills out of the schemas it hands the model.
///
/// Mneme renders `attribution` as a property, with a description, on every
/// function that takes one — so a model that fetches a schema to make a write
/// reads a paragraph about a field it must not set. The manifest used to say
/// so in prose instead, and that sentence was removed for the same reason: the
/// field is the harness's, so the harness takes it out of what its side is
/// shown, whether or not the server also stops rendering it. There is no read
/// exception any more: Mneme's audit filter used to live under this same key
/// and was renamed `written_by` (2026-09, settled with the Mneme side)
/// precisely because a key this layer overwrites can never carry a caller's
/// value. A schema from a not-yet-redeployed Mneme still describing
/// `attribution` on `audit_query` gets it hidden too — from a session client
/// that knob never worked (the stamp clobbered it on the way out), and
/// showing a knob that cannot work is how the trap stayed discoverable.
///
/// Best effort, deliberately: a reply that is not the JSON it looks like — a
/// paged `all` dump that stopped short, a shape from a later Mneme — comes
/// back untouched, which costs a few tokens in the model's context rather than
/// risking a mangled answer. A reply with nothing to hide is returned as the
/// exact bytes it arrived as, so the ordinary path stays verbatim.
fn hide_attribution(function: &str, reply: String) -> String {
    if function != "schema" {
        return reply;
    }
    let Ok(mut value) = serde_json::from_str::<Value>(&reply) else {
        return reply;
    };
    let Some(functions) = value.get_mut("functions").and_then(Value::as_array_mut) else {
        return reply;
    };
    let mut removed = false;
    for function in functions {
        let properties = function
            .get_mut("input_schema")
            .and_then(|schema| schema.get_mut("properties"))
            .and_then(Value::as_object_mut);
        if let Some(properties) = properties {
            removed |= properties.remove("attribution").is_some();
        }
    }
    if !removed {
        return reply;
    }
    serde_json::to_string_pretty(&value).unwrap_or(reply)
}

async fn initialize(
    client: &reqwest::Client,
    mcp_url: &str,
    token: &str,
    label: &str,
) -> anyhow::Result<Option<String>> {
    let init = client
        .post(mcp_url)
        .bearer_auth(token)
        .header(
            reqwest::header::ACCEPT,
            "application/json, text/event-stream",
        )
        .json(&serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "eidolon", "version": env!("CARGO_PKG_VERSION") }
            }
        }))
        .send()
        .await
        .with_context(|| format!("POST {mcp_url} (initialize)"))?;
    let init = init.error_for_status().context("MCP initialize rejected")?;
    let session = init
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let value = read_rpc_response(init, 1).await?;
    if value.get("error").is_some() || value.get("result").is_none() {
        bail!("{label} initialize failed: invalid JSON-RPC result");
    }
    let mut ack = client.post(mcp_url).bearer_auth(token).header(
        reqwest::header::ACCEPT,
        "application/json, text/event-stream",
    );
    if let Some(id) = &session {
        ack = ack.header("Mcp-Session-Id", id);
    }
    ack.json(&serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
        .send()
        .await
        .with_context(|| format!("POST {mcp_url} (initialized)"))?
        .error_for_status().context("MCP initialized acknowledgement rejected")?;
    Ok(session)
}

/// Stop at the matching response, not EOF: an SSE stream may remain open.
async fn read_rpc_response(mut response: reqwest::Response, id: u64) -> anyhow::Result<Value> {
    let sse = response.headers().get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok()).is_some_and(|v| v.starts_with("text/event-stream"));
    if !sse {
        let value = response.json::<Value>().await.context("reading MCP JSON response")?;
        if value.get("id").and_then(Value::as_u64) != Some(id) {
            bail!("MCP response id mismatch");
        }
        return Ok(value);
    }
    let mut pending = Vec::new();
    let mut data = String::new();
    while let Some(chunk) = response.chunk().await.context("reading MCP SSE response")? {
        pending.extend_from_slice(&chunk);
        while let Some(end) = pending.iter().position(|b| *b == b'\n') {
            let line: Vec<_> = pending.drain(..=end).collect();
            let line = std::str::from_utf8(&line).context("MCP SSE is not UTF-8")?
                .trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if let Ok(value) = serde_json::from_str::<Value>(&data)
                    && value.get("id").and_then(Value::as_u64) == Some(id)
                    && (value.get("result").is_some() || value.get("error").is_some())
                {
                    return Ok(value);
                }
                data.clear();
            } else if let Some(part) = line.strip_prefix("data:") {
                data.push_str(part.strip_prefix(' ').unwrap_or(part));
                data.push('\n');
            }
        }
    }
    bail!("MCP SSE ended before response id {id}")
}

/// The body may be plain JSON or an SSE frame; rmcp's stateful transport
/// also emits priming events with empty `data:`. Group lines into events,
/// keep the last one that parses as a JSON-RPC envelope.
pub fn parse_rpc_body(body: &str) -> anyhow::Result<Value> {
    if let Ok(v) = serde_json::from_str::<Value>(body.trim()) {
        return Ok(v);
    }
    let mut last = None;
    let mut data: Vec<&str> = Vec::new();
    let flush = |data: &mut Vec<&str>, last: &mut Option<Value>| {
        if !data.is_empty() {
            if let Ok(v) = serde_json::from_str::<Value>(data.join("\n").trim()) {
                *last = Some(v);
            }
            data.clear();
        }
    };
    for line in body.lines() {
        if line.trim().is_empty() {
            flush(&mut data, &mut last);
        } else if let Some(d) = line.strip_prefix("data:") {
            data.push(d.strip_prefix(' ').unwrap_or(d));
        }
    }
    flush(&mut data, &mut last);
    last.context("response was neither JSON nor an SSE data frame")
}

fn collect_content_text(result: &Value) -> String {
    let mut out = String::new();
    if let Some(items) = result.get("content").and_then(Value::as_array) {
        for item in items {
            if let Some(t) = item.get("text").and_then(Value::as_str) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(t);
            }
        }
    }
    if out.is_empty()
        && let Some(sc) = result.get("structuredContent")
    {
        out = sc.to_string();
    }
    out
}

fn looks_like_auth_rejection(e: &anyhow::Error) -> bool {
    e.chain().filter_map(|cause| cause.downcast_ref::<reqwest::Error>())
        .any(|e| matches!(e.status().map(|s| s.as_u16()), Some(401 | 403)))
}

fn read_secret(p: &std::path::Path) -> anyhow::Result<String> {
    let p = if let Ok(rest) = p.strip_prefix("~") {
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join(rest))
            .unwrap_or_else(|| p.to_path_buf())
    } else {
        p.to_path_buf()
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(md) = std::fs::metadata(&p)
            && md.permissions().mode() & 0o077 != 0
        {
            tracing::warn!(path = %p.display(), "credential file is readable by others; chmod 600 it");
        }
    }
    let s = std::fs::read_to_string(&p).with_context(|| format!("reading {}", p.display()))?;
    let s = s.trim().to_string();
    if s.is_empty() {
        bail!("{} is empty", p.display());
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn parses_sse_with_priming_events() {
        let body = "event: message\ndata:\n\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]}}\n\n";
        let v = parse_rpc_body(body).unwrap();
        assert_eq!(collect_content_text(&v["result"]), "hi");
    }

    /// A stub Mneme: initialize → session id, initialized → 202, tools/call
    /// → echoes the arguments as text (or an error for `RPC` on `boom`).
    async fn stub() -> (String, Arc<Mutex<Vec<Value>>>) {
        use axum::{Router, routing::post};
        let calls: Arc<Mutex<Vec<Value>>> = Arc::default();
        let seen = calls.clone();
        let app = Router::new().route(
            "/mcp",
            post(move |headers: axum::http::HeaderMap, axum::Json(req): axum::Json<Value>| {
                let seen = seen.clone();
                async move {
                    seen.lock().unwrap().push(req.clone());
                    let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("");
                    if auth != "Bearer tok" {
                        return (axum::http::StatusCode::UNAUTHORIZED, [("content-type", "application/json")], "{}".to_string());
                    }
                    match req["method"].as_str() {
                        Some("initialize") => (
                            axum::http::StatusCode::OK,
                            [("content-type", "application/json")],
                            serde_json::json!({"jsonrpc":"2.0","id":1,"result":{}}).to_string(),
                        ),
                        Some("notifications/initialized") => (axum::http::StatusCode::ACCEPTED, [("content-type", "application/json")], String::new()),
                        Some("tools/call") => {
                            let name = req["params"]["name"].as_str().unwrap_or("");
                            let args = &req["params"]["arguments"];
                            let is_err = args["call"]["function"] == "boom";
                            let text = format!("{name}:{args}");
                            (
                                axum::http::StatusCode::OK,
                                [("content-type", "text/event-stream")],
                                format!(
                                    "event: message\ndata: {}\n\n",
                                    serde_json::json!({"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":text}],"isError":is_err}})
                                ),
                            )
                        }
                        _ => (axum::http::StatusCode::BAD_REQUEST, [("content-type", "application/json")], "{}".into()),
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}/mcp"), calls)
    }

    #[tokio::test]
    async fn rpc_and_dispatch_go_through_tools_call() {
        let (url, calls) = stub().await;
        let dir = std::env::temp_dir().join(format!("eidolon-remote-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tf = dir.join("tok");
        std::fs::write(&tf, "tok\n").unwrap();
        let m = Mneme::new(url, Credential::TokenFile(tf));
        let out = m
            .rpc("read_note", serde_json::json!({"title":"x"}))
            .await
            .unwrap();
        assert!(out.starts_with("RPC:"), "{out}");
        assert!(out.contains("\"function\":\"read_note\""));
        let out = m
            .dispatch(
                "append $a to $b",
                Some(serde_json::json!({"a":"1","b":"n"})),
            )
            .await
            .unwrap();
        assert!(out.starts_with("dispatch:"), "{out}");
        assert!(out.contains("\"bindings\""));
        let err = m.rpc("boom", serde_json::json!({})).await.unwrap_err();
        assert!(format!("{err:#}").contains("reported an error"));
        let seen = calls.lock().unwrap();
        assert_eq!(
            seen.iter().filter(|c| c["method"] == "initialize").count(),
            3
        );
        assert!(seen.iter().any(|c| c["params"]["name"] == "RPC"));
    }

    /// The audit log records `attribution` verbatim, so the only thing that
    /// makes the field worth reading is that the *harness* writes it. A label
    /// the caller supplied is replaced, not passed through, and it is stamped
    /// on a function Mneme's schema does not declare the field for as readily
    /// as on one that does.
    #[tokio::test]
    async fn rpc_is_attributed_by_the_harness_over_a_callers_own_label() {
        let (url, calls) = stub().await;
        let dir = std::env::temp_dir().join(format!("eidolon-remote-attr-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tf = dir.join("tok");
        std::fs::write(&tf, "tok\n").unwrap();
        let m = Mneme::new(url, Credential::TokenFile(tf));
        m.set_attribution(eidolon_core::attribution::Attribution::new(
            "eidolon-756d",
            "~/eidolon",
        ));
        m.rpc(
            "edit_note",
            serde_json::json!({ "title": "x", "attribution": "chat-00009/meli" }),
        )
        .await
        .unwrap();
        m.rpc("read_note", serde_json::json!({ "title": "x" }))
            .await
            .unwrap();
        let seen = calls.lock().unwrap();
        let sent: Vec<&Value> = seen
            .iter()
            .filter(|c| c["params"]["name"] == "RPC")
            .map(|c| &c["params"]["arguments"]["call"]["args"]["attribution"])
            .collect();
        assert_eq!(sent.len(), 2);
        for label in sent {
            assert_eq!(*label, "session=eidolon-756d cwd=~/eidolon");
        }
    }

    /// The one call where the stamp's key used to cost the caller something:
    /// Mneme's audit filter shared the name and was renamed `written_by`
    /// (settled with the Mneme side, 2026-09) so a filter value could reach
    /// the wire at all. On the same call the caller's filter arrives intact
    /// while the caller's `attribution` — provenance, even on a read — is
    /// still replaced by the harness's label.
    #[tokio::test]
    async fn audit_query_written_by_reaches_the_wire_but_attribution_is_still_stamped() {
        let (url, calls) = stub().await;
        let dir = std::env::temp_dir().join(format!("eidolon-remote-audit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tf = dir.join("tok");
        std::fs::write(&tf, "tok\n").unwrap();
        let m = Mneme::new(url, Credential::TokenFile(tf));
        m.set_attribution(eidolon_core::attribution::Attribution::new(
            "eidolon-756d",
            "~/eidolon",
        ));
        m.rpc(
            "audit_query",
            serde_json::json!({
                "path": "wiki",
                "written_by": "session=eidolon-123",
                "attribution": "session=eidolon-123",
            }),
        )
        .await
        .unwrap();
        let seen = calls.lock().unwrap();
        let rpc = seen.iter().find(|c| c["params"]["name"] == "RPC").unwrap();
        let args = &rpc["params"]["arguments"]["call"]["args"];
        assert_eq!(
            args["written_by"], "session=eidolon-123",
            "the read filter is the caller's, not the stamp's"
        );
        assert_eq!(
            args["attribution"], "session=eidolon-756d cwd=~/eidolon",
            "provenance is still the harness's even on a read"
        );
    }
    #[tokio::test]
    async fn auth_rejections_keep_their_status_at_every_mcp_step() {
        use axum::{Router, routing::post};
        for stage in ["initialize", "notifications/initialized", "tools/call"] {
            let app = Router::new().route("/mcp", post(move |axum::Json(req): axum::Json<Value>| async move {
                if req["method"] == stage {
                    return (axum::http::StatusCode::UNAUTHORIZED, axum::Json(serde_json::json!({
                        "error":"invalid_token", "error_description":"the access token is invalid or expired"
                    })));
                }
                (axum::http::StatusCode::OK, axum::Json(serde_json::json!({"jsonrpc":"2.0","id":req["id"],"result":{}})))
            }));
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/mcp", listener.local_addr().unwrap());
            let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let client = McpClient::new(Server::Mneme, url, Credential::TokenFile("unused".into()));
            let err = client.request_with("test", "tools/call", serde_json::json!({})).await.unwrap_err();
            assert!(looks_like_auth_rejection(&err), "{stage}: {err:#}");
            task.abort();
        }
        // Tool text is not evidence of an HTTP authentication rejection.
        assert!(!looks_like_auth_rejection(&anyhow::anyhow!("tool said HTTP 401")));
    }

    /// The pid suffix keeps the parallel tests out of each other's
    /// process-global environment.
    #[tokio::test]
    async fn token_env_resolves_a_set_variable() {
        let name = format!("EIDOLON_REMOTE_TEST_TOKEN_{}", std::process::id());
        unsafe { std::env::set_var(&name, " tok-from-env\n") };
        let c = McpClient::new(Server::Mneme, "unused", Credential::TokenEnv(name.clone()));
        let tok = c.token().await.unwrap();
        assert_eq!(tok.as_str(), "tok-from-env");
        unsafe { std::env::remove_var(&name) };
    }

    #[tokio::test]
    async fn token_env_missing_names_the_variable() {
        let name = format!("EIDOLON_REMOTE_TEST_ABSENT_{}", std::process::id());
        unsafe { std::env::remove_var(&name) };
        let c = McpClient::new(Server::Mneme, "unused", Credential::TokenEnv(name.clone()));
        let err = c.token().await.unwrap_err();
        assert!(format!("{err:#}").contains(&name), "{err:#}");
    }

    #[tokio::test]
    async fn token_env_empty_or_whitespace_only_is_refused() {
        let name = format!("EIDOLON_REMOTE_TEST_EMPTY_{}", std::process::id());
        for value in ["", "   \t "] {
            unsafe { std::env::set_var(&name, value) };
            let c = McpClient::new(Server::Mneme, "unused", Credential::TokenEnv(name.clone()));
            let err = c.token().await.unwrap_err();
            let msg = format!("{err:#}");
            assert!(msg.contains(&name) && msg.contains("empty"), "{msg}");
        }
        unsafe { std::env::remove_var(&name) };
    }

    #[tokio::test]
    async fn sse_returns_matching_result_without_waiting_for_eof() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            assert!(socket.read(&mut request).await.unwrap() > 0);
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n").await.unwrap();
            // Split an event and a multibyte character across network chunks;
            // priming events and a different response id must be skipped.
            let event = "data:\r\n\r\ndata: {\"id\":99,\"result\":{}}\r\n\r\ndata: {\"id\":2,\n".to_string()
                + "data: \"result\":{\"text\":\"héllo\"}}\n\n";
            for byte in event.as_bytes() {
                socket.write_all(b"1\r\n").await.unwrap();
                socket.write_all(&[*byte]).await.unwrap();
                socket.write_all(b"\r\n").await.unwrap();
            }
            // No terminating chunk: the old resp.text() waits for this EOF.
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        });
        let response = reqwest::Client::new().get(url).send().await.unwrap();
        let started = std::time::Instant::now();
        let value = tokio::time::timeout(std::time::Duration::from_secs(2), read_rpc_response(response, 2))
            .await.expect("must not wait for stream EOF").unwrap();
        assert_eq!(value["result"]["text"], "héllo");
        eprintln!("SSE result returned in {:?}; server holds EOF for 60s", started.elapsed());
        task.abort();
    }

    /// Wall-clock regression measurement; opt-in because it takes 21 seconds.
    #[tokio::test]
    #[ignore = "21-second timeout comparison"]
    async fn slow_tool_outlives_the_oauth_timeout() {
        use axum::{Router, routing::post};
        let app = Router::new().route("/mcp", post(|axum::Json(req): axum::Json<Value>| async move {
            if req["method"] == "tools/call" {
                tokio::time::sleep(std::time::Duration::from_secs(21)).await;
            }
            axum::Json(serde_json::json!({"jsonrpc":"2.0","id":req["id"],"result":{}}))
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = McpClient::new(Server::Mneme, &url, Credential::TokenFile("unused".into()));
        let old = harnox::oauth_client::bounded_client(true).unwrap();
        let baseline = old.post(url).json(&serde_json::json!({"method":"tools/call","id":2}));
        let started = std::time::Instant::now();
        let (before, after) = tokio::join!(baseline.send(), client.request_with("test", "tools/call", serde_json::json!({})));
        assert!(before.unwrap_err().is_timeout());
        assert!(after.is_ok(), "{after:?}");
        eprintln!("slow tool: old OAuth client timed out (20s limit); new MCP client succeeded; elapsed={:?}", started.elapsed());
        task.abort();
    }

    /// The field the harness fills is not something the model should be shown,
    /// let alone taught: Mneme describes it on every function that takes one,
    /// so a model fetching a schema to make a write would read a paragraph
    /// about it — and could spend more writing one that gets replaced. Taken
    /// out of what this side hands over on every function: audit_query's read
    /// filter is `written_by` now, and a stray `attribution` there (an older
    /// server's schema) names a key this client overwrites, not a working knob.
    #[test]
    fn schema_replies_do_not_describe_the_field_the_harness_fills() {
        let reply = serde_json::json!({
            "count": 2,
            "functions": [
                {
                    "name": "edit_note",
                    "description": "Make a targeted in-place edit…",
                    "input_schema": {
                        "type": "object",
                        "properties": {
                            "attribution": { "type": ["string", "null"], "description": "Optional agent/session attribution…" },
                            "title": { "type": "string" },
                        },
                        "required": ["title"],
                    },
                },
                {
                    "name": "audit_query",
                    "input_schema": {
                        "type": "object",
                        "properties": {
                            "written_by": { "type": ["string", "null"] },
                            "attribution": { "type": ["string", "null"] },
                            "limit": { "type": ["integer", "null"] },
                        },
                    },
                },
            ],
            "total": 2,
        })
        .to_string();

        let hidden: Value = serde_json::from_str(&hide_attribution("schema", reply.clone())).unwrap();
        let write = &hidden["functions"][0]["input_schema"]["properties"];
        assert!(write.get("attribution").is_none(), "{hidden}");
        assert!(write.get("title").is_some(), "the rest of the schema survives");
        let audit = &hidden["functions"][1]["input_schema"]["properties"];
        assert!(
            audit.get("attribution").is_none(),
            "a key the harness overwrites is hidden even on a read: {hidden}"
        );
        assert!(
            audit.get("written_by").is_some() && audit.get("limit").is_some(),
            "the read filter that is not the stamp's key stays discoverable: {hidden}"
        );

        // Nothing to hide: the server's bytes come back exactly as they were.
        let listed = r#"{"count":1,"functions":[{"name":"read_note"}],"total":1}"#;
        assert_eq!(hide_attribution("schema", listed.to_string()), listed);
        // A reply that is not the JSON it looks like is left alone rather than
        // guessed at — a paged dump that stopped short, or a later Mneme.
        let torn = "[mneme] Lines 1–3 of 454. Continue: read_note …".to_string();
        assert_eq!(hide_attribution("schema", torn.clone()), torn);
        // And it is the schema reply, not every reply.
        assert_eq!(hide_attribution("list", reply.clone()), reply);
        assert_eq!(hide_attribution("read_note", reply.clone()), reply);
    }

    /// The natural-language door is labelled too — it can execute a write, and
    /// a write with nobody's name on it is the gap this closes. It is the same
    /// cell and the same rule as `rpc`, differing only in where the argument
    /// sits: top level on `dispatch`, inside the call envelope on `RPC`.
    #[tokio::test]
    async fn dispatch_is_attributed_by_the_harness_over_a_callers_own_label() {
        let (url, calls) = stub().await;
        let dir = std::env::temp_dir().join(format!("eidolon-remote-dispatch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tf = dir.join("tok");
        std::fs::write(&tf, "tok\n").unwrap();
        let m = Mneme::new(url, Credential::TokenFile(tf));
        assert!(
            m.rpc("list", serde_json::json!({})).await.is_ok(),
            "a client with no label set still reaches the vault"
        );
        m.set_attribution(eidolon_core::attribution::Attribution::new(
            "eidolon-756d",
            "~/eidolon",
        ));
        m.dispatch("append $a to $b", Some(serde_json::json!({ "a": "1", "b": "n" })))
            .await
            .unwrap();

        let seen = calls.lock().unwrap();
        let sent = seen
            .iter()
            .find(|c| c["params"]["name"] == "dispatch")
            .expect("the dispatch call reached the server");
        assert_eq!(sent["params"]["arguments"]["attribution"], "session=eidolon-756d cwd=~/eidolon");
        assert_eq!(
            sent["params"]["arguments"]["bindings"]["b"], "n",
            "bindings survive the fill: {sent}"
        );
        // The unattributed call went out with no such argument at all — the
        // field's "omit freely" is what an unlabelled client means.
        let rpc = seen
            .iter()
            .find(|c| c["params"]["name"] == "RPC")
            .expect("the earlier RPC reached the server");
        assert!(rpc["params"]["arguments"]["call"]["args"].get("attribution").is_none());
    }

    // ---- the vault's log, as a park samples it -------------------------

    /// The Mneme side's replies, **byte for byte** as `RPC` returns them in
    /// `content[0].text` (`wiki/systems/Mneme/The Audit Log`, section "The
    /// reply, byte for byte"; Mneme `81b4116`, 2026-09-22).
    const ONE_EVENT: &str = r#"{
  "baseline": false,
  "corrupt_lines": 0,
  "events": [
    {
      "kind": "rpc",
      "op": "create_note",
      "path": "wiki/b/Two.md",
      "seq": 2,
      "summary": "create note wiki/b/Two.md",
      "ts": 1790072680
    }
  ],
  "external_latched": 0,
  "gaps": [],
  "kind": "mneme.tail",
  "reconcile": false,
  "since_seq": 1,
  "through_seq": 2,
  "timed_out": false,
  "truncated": false
}"#;

    const NARROWED_AWAY: &str = r#"{
  "baseline": false,
  "corrupt_lines": 0,
  "events": [],
  "external_latched": 0,
  "gaps": [],
  "kind": "mneme.tail",
  "reconcile": false,
  "since_seq": 0,
  "through_seq": 2,
  "timed_out": false,
  "truncated": false
}"#;

    const TIMED_OUT: &str = r#"{
  "baseline": false,
  "corrupt_lines": 0,
  "events": [],
  "external_latched": 0,
  "gaps": [],
  "kind": "mneme.tail",
  "reconcile": false,
  "since_seq": 2,
  "through_seq": 2,
  "timed_out": true,
  "truncated": false
}"#;

    const EMPTY_LOG: &str = r#"{
  "baseline": false,
  "corrupt_lines": 0,
  "events": [],
  "external_latched": 0,
  "gaps": [],
  "kind": "mneme.tail",
  "reconcile": false,
  "since_seq": 0,
  "through_seq": 0,
  "timed_out": false,
  "truncated": false
}"#;

    fn text_reply(body: &str) -> Value {
        serde_json::json!({ "content": [{ "type": "text", "text": body }] })
    }

    /// A stub Mneme that answers every `tools/call` with one fixed `result`
    /// object, and remembers what was asked.
    async fn stub_reply(result: Value) -> (String, Arc<Mutex<Vec<Value>>>) {
        use axum::{Router, routing::post};
        let calls: Arc<Mutex<Vec<Value>>> = Arc::default();
        let seen = calls.clone();
        let app = Router::new().route(
            "/mcp",
            post(move |axum::Json(req): axum::Json<Value>| {
                let seen = seen.clone();
                let result = result.clone();
                async move {
                    seen.lock().unwrap().push(req.clone());
                    match req["method"].as_str() {
                        Some("initialize") => (
                            axum::http::StatusCode::OK,
                            [("content-type", "application/json")],
                            serde_json::json!({"jsonrpc":"2.0","id":1,"result":{}}).to_string(),
                        ),
                        Some("notifications/initialized") => (
                            axum::http::StatusCode::ACCEPTED,
                            [("content-type", "application/json")],
                            String::new(),
                        ),
                        Some("tools/call") => (
                            axum::http::StatusCode::OK,
                            [("content-type", "application/json")],
                            serde_json::json!({"jsonrpc":"2.0","id":2,"result":result}).to_string(),
                        ),
                        _ => (
                            axum::http::StatusCode::BAD_REQUEST,
                            [("content-type", "application/json")],
                            "{}".into(),
                        ),
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}/mcp"), calls)
    }

    async fn tail_client(result: Value) -> (Mneme, Arc<Mutex<Vec<Value>>>) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let (url, seen) = stub_reply(result).await;
        // One directory per client: tests run in one process, and a shared
        // token file would have one test truncating another's while it reads.
        let dir = std::env::temp_dir().join(format!(
            "eidolon-remote-tail-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let tf = dir.join("tok");
        std::fs::write(&tf, "tok\n").unwrap();
        (Mneme::new(url, Credential::TokenFile(tf)), seen)
    }

    fn cursor(since: u64, path: Option<&str>) -> TailQuery {
        TailQuery {
            vault: "default".into(),
            since,
            path: path.map(str::to_string),
            written_by: None,
        }
    }

    /// The whole reading: the leaf fires on an **event past the cursor**, and
    /// not on the cursor having moved. A reply whose filter matched nothing
    /// while the head advanced is the case a park sees most often, and the
    /// one a "did anything change?" sample would get wrong.
    #[tokio::test]
    async fn a_sample_reads_the_events_past_the_cursor_and_not_the_head() {
        let (m, seen) = tail_client(text_reply(ONE_EVENT)).await;
        assert_eq!(m.audit_tail(&cursor(1, Some("wiki/b"))).await.unwrap(), Tail::Moved);

        let (m, _) = tail_client(text_reply(NARROWED_AWAY)).await;
        assert_eq!(m.audit_tail(&cursor(0, Some("wiki/c"))).await.unwrap(), Tail::Still);

        let (m, _) = tail_client(text_reply(TIMED_OUT)).await;
        assert_eq!(m.audit_tail(&cursor(2, None)).await.unwrap(), Tail::Still);

        let (m, _) = tail_client(text_reply(EMPTY_LOG)).await;
        assert_eq!(m.audit_tail(&cursor(0, None)).await.unwrap(), Tail::Still);

        // What went out on the wire: one sample, which never holds a request
        // and never latches, stamped like every other call this client makes.
        let seen = seen.lock().unwrap();
        let sent = seen
            .iter()
            .find(|c| c["params"]["name"] == "RPC")
            .expect("the sample reached the server");
        let call = &sent["params"]["arguments"]["call"];
        assert_eq!(call["function"], "audit_tail");
        assert_eq!(call["args"]["vault"], "default");
        assert_eq!(call["args"]["since_seq"], 1);
        assert_eq!(call["args"]["path"], "wiki/b");
        assert_eq!(call["args"]["wait_ms"], 0, "a park's sample must return at once");
        assert_eq!(call["args"]["latch"], false, "a park's sample must not write");
        assert!(
            call["args"].get("written_by").is_none(),
            "a writer nobody named is not sent: {call}"
        );
    }

    /// The two permanent answers, read from the code beside the prose. An
    /// application error with no code is *not* classified from its text — it
    /// is an `Err`, so a park keeps waiting instead of being told its
    /// condition can never come true.
    #[tokio::test]
    async fn a_permanent_failure_is_gone_and_an_uncoded_one_is_retryable() {
        let no_function = serde_json::json!({
            "content": [{ "type": "text", "text": "RPC: no function named \"audit_tail\". Callable functions: …" }],
            "isError": true,
            "structuredContent": { "code": "no_function", "function": "audit_tail" },
        });
        let (m, _) = tail_client(no_function).await;
        match m.audit_tail(&cursor(1, None)).await.unwrap() {
            Tail::Gone(why) => assert!(why.contains("no function named"), "{why}"),
            other => panic!("expected a permanent refusal, got {other:?}"),
        }

        let unknown_vault = serde_json::json!({
            "content": [{ "type": "text", "text": "Unknown vault \"wiki\". Known vaults: default (default)." }],
            "isError": true,
            "structuredContent": { "code": "unknown_vault", "vault": "wiki", "known_vaults": ["default"] },
        });
        let (m, _) = tail_client(unknown_vault).await;
        match m.audit_tail(&cursor(1, None)).await.unwrap() {
            Tail::Gone(why) => assert!(why.contains("Unknown vault"), "{why}"),
            other => panic!("expected a permanent refusal, got {other:?}"),
        }

        let uncoded = serde_json::json!({
            "content": [{ "type": "text", "text": "the log is locked" }],
            "isError": true,
        });
        let (m, _) = tail_client(uncoded).await;
        let e = m.audit_tail(&cursor(1, None)).await.unwrap_err();
        assert!(format!("{e:#}").contains("the log is locked"), "{e:#}");
    }

}
