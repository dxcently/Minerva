//! Melete from the desktop: the job harness's whole MCP surface as
//! deferred tools on this registry.
//!
//! Melete's connector publishes forty-odd tools — the triad (trigger,
//! observe, control), the schedulers, the library call, the docs — and
//! the vault decision that cut MCP as an *abstraction* for Mneme does not
//! transfer whole, because Mneme's surface really is two functions
//! (`RPC` and `dispatch`) with the vocabulary behind them, while Melete's
//! `lib_rpc` and `verba_dispatch` reach only its Rune libraries. What a
//! desktop session wants from Melete is the triad itself, and those are
//! tools with schemas of their own that the model has to be shown. So this
//! is MCP kept as the *transport* and the server's own manifests kept as
//! the interface: every tool Melete lists becomes a harness tool, name
//! prefixed, description and schema verbatim — and **deferred**, so the
//! whole listing costs nothing until a search or a call reaches for one
//! (`eidolon_core::tool_search`). Nothing is offered from the start:
//! [`DEFAULT_CORE`] is empty, because an offered tool has a standing
//! advantage over a deferred sibling under the same prefix, and a model that
//! reaches for the manual or the triad through `tool_search` is choosing the
//! tool it means. `[melete] core` names what to offer instead.
//!
//! ## The gate is here
//!
//! Melete's external connector runs *attended*: a call from an OAuth client
//! executes at once, nothing is held for Telegram, because the connector
//! was built for a person in a live chat. So the harness's own gate is the
//! only one, and [`approval_for`] is what it reads: observation is
//! read-only and silent; anything that starts, schedules or steers a job
//! is `mutating`, which `policy.rn` turns into a question — a job dispatch
//! from a terminal spends real tokens on a real box; cancelling or
//! removing is `destructive`. Under yolo those questions are waived, which
//! is what yolo means and is intended.
//!
//! ## Nothing on the way to the first frame waits on the network
//!
//! The list is read from `~/.cache/eidolon/mcp/melete/tools.json` at
//! startup and refreshed on a thread of its own when it is missing or a
//! day old, exactly as a provider script's `models()` is: the refresh
//! serves the *next* launch, `eidolon tools --live` primes the cache by
//! hand, and a first launch with no cache registers nothing from Melete
//! and says so. That is the same posture as the model catalog's, for the
//! same reason.
//!
//! ## Not a script
//!
//! A built-in is a `.rn` over a Rust primitive so an operator can replace
//! it; there is nothing here for a script to say — the name, the
//! description, the schema and the tier come from the server and a rule —
//! so [`MeleteTool`] is a native `Tool`, the way `choices_user` is. It still
//! goes through the one chokepoint; a native tool is only a problem when it
//! is a way *around* the dispatcher, and this one is a way to it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use eidolon_core::policy::Approval;
use eidolon_core::tool::{CallContext, Tool, ToolManifest, ToolOutput, ToolRegistry};

use crate::{Credential, McpClient, Server};

/// The Melete connector, spoken to over the same JSON-RPC `tools/call`
/// the Mneme client uses, behind the same passphrase-gated OAuth with the
/// `jobs.run` scope Melete's server issues.
#[derive(Clone, Debug)]
pub struct Melete {
    client: McpClient,
}

impl Melete {
    pub fn new(mcp_url: impl Into<String>, credential: Credential) -> Self {
        Melete {
            client: McpClient::new(Server::Melete, mcp_url, credential),
        }
    }

    /// One tool by its server-side name, the text content back.
    pub async fn call(&self, tool: &str, args: Value) -> Result<String> {
        self.client.call_tool(tool, args).await
    }

    /// Everything the connector lists, every page of it.
    pub async fn tools(&self) -> Result<Vec<Listed>> {
        self.client
            .list_tools()
            .await?
            .into_iter()
            .map(Listed::from_mcp)
            .collect()
    }
}

fn object_schema() -> Value {
    json!({ "type": "object" })
}

/// One tool as the connector lists it — MCP's `tools/list` shape with the
/// harness's field names, which is also what the cache holds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Listed {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "object_schema")]
    pub input_schema: Value,
}

impl Listed {
    fn from_mcp(v: Value) -> Result<Listed> {
        let name = v
            .get("name")
            .and_then(Value::as_str)
            .filter(|n| !n.is_empty())
            .context("a listed tool has no name")?
            .to_string();
        let description = v
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let input_schema = v
            .get("inputSchema")
            .cloned()
            .filter(Value::is_object)
            .unwrap_or_else(object_schema);
        Ok(Listed {
            name,
            description,
            input_schema,
        })
    }
}

/// The tier a Melete tool is declared at, from its name. The connector
/// carries no such flag, and its names are regular enough that a rule is
/// honest: `get_`/`list_`/`read_`/`check_` and the observers are
/// read-only, `cancel_`/`remove_`/`delete_` and the two that rewrite the
/// daemon or its store are destructive, and everything else — triggers,
/// schedulers, steering, edits, the shell — is mutating. `policy.rn` reads
/// the tier through the manifest; an operator who wants a trigger silent
/// edits the table, not this.
pub fn approval_for(name: &str) -> Approval {
    const READ_ONLY: &[&str] = &[
        "status",
        "job_status",
        "scorecard",
        "graph_view",
        "policy_analytics",
        "web_search",
        "web_fetch",
        "secret_list",
        "self_update_status",
    ];
    const DESTRUCTIVE: &[&str] = &["secret_delete", "self_update_now"];
    if READ_ONLY.contains(&name)
        || ["get_", "list_", "read_", "check_"]
            .iter()
            .any(|p| name.starts_with(p))
    {
        Approval::ReadOnly
    } else if DESTRUCTIVE.contains(&name)
        || ["cancel_", "remove_", "delete_"]
            .iter()
            .any(|p| name.starts_with(p))
    {
        Approval::Destructive
    } else {
        Approval::Mutating
    }
}

/// The tools offered without a search, by default: **none**.
///
/// It was a small core until 2026-09-14 — the manual first, because
/// `get_docs` is the connector's own discovery mechanism, written for the
/// model, then the spine of the triad. What that bought went unmeasured until
/// glm-5.3 was watched over a long session: an *offered* tool wins calls meant
/// for a deferred sibling under the same prefix, repeatedly and with
/// coherent-but-wrong arguments (bugs tracker, 2026-09-06 #1, wrong-door
/// selection). Deferring the whole listing removes the asymmetry instead of
/// guessing which half to privilege — every tool is then reached by name or by
/// search, and an empty `tool_search` query lists them all, so the manual is
/// one call away rather than free. `[melete] core` names what to offer for an
/// operator who wants the old shape back.
pub const DEFAULT_CORE: &[&str] = &[];

/// How the listing becomes registered tools.
#[derive(Clone, Debug)]
pub struct Options {
    /// Put in front of every name; `melete_` unless the operator says.
    pub prefix: String,
    /// The server-side names offered without a search.
    pub core: Vec<String>,
    /// How old a cached listing may be before a launch refreshes it.
    pub stale_after: Duration,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            prefix: "melete_".into(),
            core: DEFAULT_CORE.iter().map(|s| s.to_string()).collect(),
            stale_after: Duration::from_secs(24 * 60 * 60),
        }
    }
}

/// The harness's view of one listed tool.
pub fn manifest_for(listed: &Listed, opts: &Options) -> ToolManifest {
    ToolManifest {
        name: format!("{}{}", opts.prefix, listed.name),
        description: listed.description.clone(),
        input_schema: listed.input_schema.clone(),
        approval: approval_for(&listed.name),
        prompt: (listed.name == "get_docs").then(|| {
            let p = &opts.prefix;
            format!(
                "Melete runs detached jobs on its own box; Mneme stores vault knowledge. \
Do not delegate ordinary local work unless the task calls for Melete. \
Before first using Melete, call {p}get_docs once, then {p}read_doc for the relevant area; \
its live manual and discovered schemas outrank remembered interfaces. Use tool_search to find deferred tools. \
Use its dedicated tools, not invented shell wrappers or cron files. \
Before launching work that might already be running, inspect {p}status or {p}list_runs. \
A trigger returning run_id means started, not completed: observe {p}job_status before reporting success, \
and steer or stop the existing run rather than launching duplicates. \
Do not poll in a tight loop; progress is pushed to Telegram. \
Melete's lib_rpc and verba_dispatch cover libraries, not the entire job surface; \
do not copy Mneme's RPC argument shape into them. Read the scheduling, Rune, or coding-task docs before authoring in those areas."
            )
        }),
        render: None,
        deferred: !opts.core.iter().any(|c| c == &listed.name),
    }
}

/// One of Melete's tools on this registry: the server's manifest over the
/// one primitive, a `tools/call` by the unprefixed name.
pub struct MeleteTool {
    manifest: ToolManifest,
    remote: String,
    client: Arc<Melete>,
}

impl MeleteTool {
    pub fn new(listed: &Listed, opts: &Options, client: Arc<Melete>) -> Self {
        MeleteTool {
            manifest: manifest_for(listed, opts),
            remote: listed.name.clone(),
            client,
        }
    }
}

#[async_trait]
impl Tool for MeleteTool {
    fn manifest(&self) -> &ToolManifest {
        &self.manifest
    }

    async fn call(&self, input: Value, _: CallContext) -> Result<ToolOutput> {
        let input = if input.is_object() { input } else { json!({}) };
        match self.client.call(&self.remote, input).await {
            Ok(text) => Ok(ToolOutput::ok(text)),
            // The connector's own error is the useful thing to say; a
            // transport failure is reported the same way, as an error the
            // model can read, rather than as a dispatch failure.
            Err(e) => Ok(ToolOutput::error(format!("{e:#}"))),
        }
    }
}

/// The listing cache: one JSON file, written whole through a temp file
/// and a rename so a launch never reads half a listing.
pub mod cache {
    use super::Listed;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    /// `~/.cache/eidolon/mcp/melete/tools.json`, or `None` where there is
    /// no cache directory to speak of.
    pub fn path() -> Option<PathBuf> {
        Some(
            dirs::cache_dir()?
                .join("eidolon")
                .join("mcp")
                .join("melete")
                .join("tools.json"),
        )
    }

    /// The cached listing and its age.
    pub fn load(path: &Path) -> Option<(Vec<Listed>, Duration)> {
        let body = std::fs::read_to_string(path).ok()?;
        let listing: Vec<Listed> = serde_json::from_str(&body).ok()?;
        let age = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .unwrap_or(Duration::MAX);
        Some((listing, age))
    }

    pub fn store(path: &Path, listing: &[Listed]) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        std::fs::write(
            &tmp,
            serde_json::to_string_pretty(listing).unwrap_or_default(),
        )?;
        std::fs::rename(&tmp, path)
    }
}

/// Register every listed tool. Returns the registered names, prefixed.
pub fn register_listing(
    reg: &mut ToolRegistry,
    client: &Arc<Melete>,
    listing: &[Listed],
    opts: &Options,
) -> Vec<String> {
    let mut names = Vec::with_capacity(listing.len());
    for l in listing {
        let tool = MeleteTool::new(l, opts, client.clone());
        names.push(tool.manifest.name.clone());
        reg.register(Arc::new(tool));
    }
    names
}

/// Register Melete's tools from the cache, and refresh the cache on a
/// thread of its own when it is missing or stale. Returns one line per
/// thing worth saying — nothing when a fresh cache was there, which is
/// every launch but the first and the daily refresh.
pub fn register(
    reg: &mut ToolRegistry,
    client: Arc<Melete>,
    opts: &Options,
    cache_path: Option<&Path>,
) -> Vec<String> {
    let mut notes = Vec::new();
    let Some(path) = cache_path.map(Path::to_path_buf).or_else(cache::path) else {
        notes.push("no cache directory; Melete's tools are not registered (run `eidolon tools --live` where there is one)".into());
        return notes;
    };
    match cache::load(&path) {
        Some((listing, age)) => {
            register_listing(reg, &client, &listing, opts);
            if age > opts.stale_after {
                spawn_refresh(client, path);
            }
        }
        None => {
            notes.push(format!(
                "Melete's tool list is not cached yet; fetching it for the next launch ({}), or run `eidolon tools --live`",
                path.display()
            ));
            spawn_refresh(client, path);
        }
    }
    notes
}

/// Fetch the listing live and write the cache. What `eidolon tools
/// --live` does, and what the background refresh does on its own thread.
pub async fn refresh(client: &Melete, path: &Path) -> Result<Vec<Listed>> {
    let listing = client.tools().await.context("listing Melete's tools")?;
    if listing.is_empty() {
        bail!("Melete listed no tools");
    }
    cache::store(path, &listing).with_context(|| format!("writing {}", path.display()))?;
    Ok(listing)
}

/// [`refresh`] on a thread with a runtime of its own, so a launch never
/// waits on it. A failure is a debug line: the launch that spawned it
/// already has its answer, and the next one will try again.
fn spawn_refresh(client: Arc<Melete>, path: PathBuf) {
    let spawned = std::thread::Builder::new()
        .name("melete-tools".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::debug!(error = %e, "could not build a runtime for the Melete refresh");
                    return;
                }
            };
            if let Err(e) = rt.block_on(refresh(&client, &path)) {
                tracing::debug!(error = %e, "background refresh of Melete's tool list failed");
            }
        });
    if let Err(e) = spawned {
        tracing::debug!(error = %e, "could not spawn the Melete refresh thread");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn listed(name: &str) -> Listed {
        Listed {
            name: name.into(),
            description: format!("{name}. More about it."),
            input_schema: json!({ "type": "object", "properties": {} }),
        }
    }

    #[test]
    fn the_tier_follows_the_name() {
        assert_eq!(approval_for("get_docs"), Approval::ReadOnly);
        assert_eq!(approval_for("list_runs"), Approval::ReadOnly);
        assert_eq!(approval_for("job_status"), Approval::ReadOnly);
        assert_eq!(approval_for("check_drift"), Approval::ReadOnly);
        assert_eq!(approval_for("run_code_task"), Approval::Mutating);
        assert_eq!(approval_for("schedule_recurring"), Approval::Mutating);
        assert_eq!(approval_for("steer_run"), Approval::Mutating);
        assert_eq!(approval_for("lib_rpc"), Approval::Mutating);
        assert_eq!(approval_for("run_shell"), Approval::Mutating);
        assert_eq!(approval_for("cancel_scheduled"), Approval::Destructive);
        assert_eq!(approval_for("remove_mcp_connector"), Approval::Destructive);
        assert_eq!(approval_for("secret_delete"), Approval::Destructive);
        assert_eq!(approval_for("self_update_now"), Approval::Destructive);
        // Unknown names are mutating: a tool this rule has never heard of
        // is asked about rather than waved through.
        assert_eq!(approval_for("something_new"), Approval::Mutating);
    }

    #[test]
    fn nothing_is_offered_by_default_and_a_named_core_is() {
        let opts = Options::default();
        let docs = manifest_for(&listed("get_docs"), &opts);
        assert_eq!(docs.name, "melete_get_docs");
        assert!(docs.deferred, "the default offers nothing, the manual included");
        let sched = manifest_for(&listed("schedule_code_task"), &opts);
        assert!(sched.deferred);
        assert_eq!(sched.approval, Approval::Mutating);
        assert_eq!(sched.description, "schedule_code_task. More about it.");
        // The operator's own core and prefix.
        let opts = Options {
            prefix: "m_".into(),
            core: vec!["schedule_code_task".into()],
            ..Options::default()
        };
        let sched = manifest_for(&listed("schedule_code_task"), &opts);
        assert_eq!(sched.name, "m_schedule_code_task");
        assert!(!sched.deferred);
        assert!(manifest_for(&listed("get_docs"), &opts).deferred);
    }

    #[test]
    fn onboarding_is_once_under_the_manual_and_uses_the_configured_prefix() {
        for prefix in ["melete_", "m_", ""] {
            let opts = Options { prefix: prefix.into(), ..Options::default() };
            let docs = manifest_for(&listed("get_docs"), &opts);
            let prompt = docs.prompt.unwrap();
            assert!(prompt.contains(&format!("call {prefix}get_docs once")));
            assert!(prompt.contains(&format!("observe {prefix}job_status")));
            assert!(prompt.contains("started, not completed"));
            assert_eq!(docs.description, listed("get_docs").description);
            assert_eq!(docs.input_schema, listed("get_docs").input_schema);
            for name in ["read_doc", "run_code_task", "job_status", "lib_rpc"] {
                assert!(manifest_for(&listed(name), &opts).prompt.is_none(), "do not repeat the onboarding for {name}");
            }
        }
    }

    #[test]
    fn a_listing_round_trips_through_the_cache_and_registers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp").join("melete").join("tools.json");
        assert!(cache::load(&path).is_none());
        let listing = vec![listed("get_docs"), listed("run_code_task")];
        cache::store(&path, &listing).unwrap();
        let (back, age) = cache::load(&path).unwrap();
        assert_eq!(back, listing);
        assert!(age < Duration::from_secs(60));

        let client = Arc::new(Melete::new(
            "http://127.0.0.1:1/mcp",
            Credential::TokenFile(dir.path().join("none")),
        ));
        let mut reg = ToolRegistry::new();
        let notes = register(&mut reg, client.clone(), &Options::default(), Some(&path));
        assert!(notes.is_empty(), "{notes:?}");
        assert!(reg.get("melete_get_docs").is_some());
        assert!(reg.get("melete_run_code_task").is_some());
        assert_eq!(reg.len(), 2);
        let guidance = reg.guidance().unwrap();
        assert_eq!(guidance.matches("Melete runs detached jobs").count(), 1);
        assert!(!reg.guidance_for(&["melete_run_code_task".into()]).unwrap().contains("Melete runs detached jobs"));

        // No cache: nothing registered, and a note saying what will happen.
        let mut reg = ToolRegistry::new();
        let notes = register(
            &mut reg,
            client,
            &Options::default(),
            Some(&dir.path().join("absent.json")),
        );
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("not cached yet"));
        assert!(reg.is_empty());
    }

    #[test]
    fn a_listed_tool_without_a_schema_or_description_still_registers() {
        let l = Listed::from_mcp(json!({ "name": "status" })).unwrap();
        assert_eq!(l.input_schema, json!({ "type": "object" }));
        assert_eq!(l.description, "");
        assert!(Listed::from_mcp(json!({ "description": "nameless" })).is_err());
    }

    /// A stub connector: initialize → ok, initialized → 202, tools/list in
    /// two pages, tools/call → echoes the name and arguments.
    async fn stub() -> String {
        use axum::{Router, routing::post};
        let app = Router::new().route(
            "/mcp",
            post(move |axum::Json(req): axum::Json<Value>| async move {
                let ok = |v: Value| (axum::http::StatusCode::OK, [("content-type", "application/json")], v.to_string());
                match req["method"].as_str() {
                    Some("initialize") => ok(json!({"jsonrpc":"2.0","id":1,"result":{}})),
                    Some("notifications/initialized") => (axum::http::StatusCode::ACCEPTED, [("content-type", "application/json")], String::new()),
                    Some("tools/list") => {
                        let result = match req["params"]["cursor"].as_str() {
                            None => json!({ "tools": [
                                { "name": "get_docs", "description": "The manual.", "inputSchema": { "type": "object" } },
                                { "name": "run_code_task", "description": "A coding run.", "inputSchema": { "type": "object", "properties": { "repo": { "type": "string" } } } },
                            ], "nextCursor": "p2" }),
                            Some("p2") => json!({ "tools": [ { "name": "stop_run", "inputSchema": { "type": "object" } } ] }),
                            Some(other) => json!({ "tools": [], "error": other }),
                        };
                        ok(json!({"jsonrpc":"2.0","id":req["id"],"result":result}))
                    }
                    Some("tools/call") => {
                        let text = format!("{}:{}", req["params"]["name"].as_str().unwrap_or(""), req["params"]["arguments"]);
                        ok(json!({"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":text}]}}))
                    }
                    _ => (axum::http::StatusCode::BAD_REQUEST, [("content-type", "application/json")], "{}".into()),
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}/mcp")
    }

    #[tokio::test]
    async fn the_listing_is_paged_and_a_tool_calls_by_its_unprefixed_name() {
        let url = stub().await;
        let dir = tempfile::tempdir().unwrap();
        let tf = dir.path().join("tok");
        std::fs::write(&tf, "tok\n").unwrap();
        let client = Arc::new(Melete::new(url, Credential::TokenFile(tf)));

        let path = dir.path().join("tools.json");
        let listing = refresh(&client, &path).await.unwrap();
        let names: Vec<&str> = listing.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(
            names,
            ["get_docs", "run_code_task", "stop_run"],
            "both pages, in order"
        );
        assert!(path.exists(), "the refresh primed the cache");

        let mut reg = ToolRegistry::new();
        let registered = register_listing(&mut reg, &client, &listing, &Options::default());
        assert_eq!(
            registered,
            ["melete_get_docs", "melete_run_code_task", "melete_stop_run"]
        );
        let tool = reg.get("melete_run_code_task").unwrap().clone();
        let out = tool
            .call(
                json!({ "repo": "noah427/eidolon" }),
                CallContext {
                    cwd: dir.path().to_path_buf(),
                    cancel: Default::default(),
                    call_id: "test".into(),
                },
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.content);
        assert!(
            out.content.starts_with("run_code_task:"),
            "the server sees the unprefixed name: {}",
            out.content
        );
        assert!(out.content.contains("noah427/eidolon"));
    }
}
