//! `/hub/meta/...`: the sources for the loaded and diff tabs (slice R3): the tools, plugins and policy rows eidolon reports, the guard's
//! status, the home's defaults, and a chat's git head read from `.git/HEAD` without running git.
//!
//! ```text
//!   GET /hub/meta/tools        [{name, approval?, plugin?, desc?}]   eidolon [--config C] tools     parsed, never --live          cached 10 min
//!   GET /hub/meta/plugins      {text}                                eidolon [--config C] plugins   verbatim, capped              cached 10 min
//!   GET /hub/meta/policy       {mode, file, rows, guard}             eidolon [--config C] policy-rows, C by ?mode=   cached 10 min per C
//!   GET /hub/meta/connectors   {mneme, melete}                       booleans: is the table in the config the doors run with
//!   GET /hub/meta/home         {default_mode, source}                what a new chat starts in, and whether minerva.toml said so
//!   GET /hub/meta/git?session  {branch} | {branch:null, sha}         <cwd>/.git/HEAD, no git run (git.rs)
//! ```
//!
//! **Everything about eidolon comes from eidolon's own commands**, run as the doors are (`--eidolon`, `--config`), never from its files.
//! Each run is bounded: [`titles::run`] closes stdin, kills the child on a timeout, and stops reading at a byte cap. A failure is a 502
//! (504 for a timeout) with a sentence and is **never cached**, so the next open asks again. The cache lock is held across the run, so two
//! tabs opened together cost one command. The text commands are shown as printed: the perch reads a plugin's name and the policy file's
//! path out of it and nothing else ([`parse`]).
//!
//! **The one exception is `connectors`**: no eidolon command says whether `[mneme]` or `[melete]` is configured, so the config the doors
//! run with (the Minerva home's generated `config.toml`) is parsed for the two tables, and only two booleans leave this module. A URL or
//! a token path in that file is never copied, and a parse error is logged without the text it stopped at.
//!
//! **`policy`'s `mode`** picks the config the same way a door does (`Registry::door_flags`): manual is `config-manual.toml`, auto and
//! yolo are `config.toml` (so they share one cached answer). The guard's own status rides along from [`crate::guard::status`], read on
//! every request because it is a value and not a command.
//!
//! The git route is the same shape of request as `/hub/tree`: the chat comes from [`tree::base`], its folder is judged against `--root`
//! on every request, and a full house of [`tree::MAX_LISTINGS`] slots is a 503.

mod git;
mod parse;

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use serde::Serialize;
use tokio::io::AsyncReadExt as _;
use tokio::sync::Mutex;

use crate::mode::Mode;
use crate::serve::{json, problem, status, BoxBody, Ctx};
use crate::titles::{self, RunError};
use crate::{files, guard, sessions, tree};

/// How long an answer is trusted.
pub const TTL: Duration = Duration::from_secs(600);
/// A listing is tens of milliseconds from eidolon's cache; a wedged one is not waited on longer than this.
const WITHIN: Duration = Duration::from_secs(20);
/// `--knob meta_timeout_ms=N` shortens [`WITHIN`], so a hung command can be proven without a 20 s test.
const KNOB: &str = "meta_timeout_ms";
/// `eidolon tools` prints every tool's JSON schema, and a Melete listing can be hundreds of them.
const TOOLS_CAP: usize = 8 * 1024 * 1024;
/// What `plugins` and `policy-rows` may print before the perch stops reading: tens of lines, not a megabyte.
const TEXT_RUN_CAP: usize = 1024 * 1024;
/// What the page is sent of either: the rest is cut, and the text says so.
const TEXT_CAP: usize = 64 * 1024;
/// The config read for `connectors`: a TOML file of a few hundred bytes.
const CONFIG_CAP: u64 = 1024 * 1024;

/// An answer and when it was got. `Arc`, so a hit is a clone of a pointer and the lock is not held while it is serialized.
type Entry<T> = (Instant, Arc<T>);

/// The cached `eidolon tools|plugins|policy-rows` answers. `Default` is how [`Ctx`] gets one.
#[derive(Default)]
pub struct State {
    tools: Mutex<Option<Entry<Vec<parse::Tool>>>>,
    plugins: Mutex<Option<Entry<Plugins>>>,
    /// By the config the command ran with (`None` is eidolon's own default): at most the two configs a perch has, and the default.
    policy: Mutex<HashMap<Option<PathBuf>, Entry<Policy>>>,
}

/// `eidolon plugins`: the text the page shows, and the names read out of it for [`parse::tools`].
struct Plugins {
    text: String,
    names: Vec<String>,
}

/// `eidolon policy-rows`: the text, and the file it says it compared as a `~` path.
struct Policy {
    file: Option<String>,
    rows: String,
}

/// The answer in `entry` when it is under [`TTL`] old at `now`.
fn fresh<T>(entry: Option<&Entry<T>>, now: Instant) -> Option<Arc<T>> {
    entry.filter(|(at, _)| now.saturating_duration_since(*at) < TTL).map(|(_, value)| value.clone())
}

/// Why a command's answer could not be had.
enum Fail {
    Run(RunError),
    /// It ran and printed something this perch does not read.
    Shape(String),
}

impl From<RunError> for Fail {
    fn from(error: RunError) -> Self {
        Fail::Run(error)
    }
}

/// The sentence for the page. `what` is the plural noun of the listing; the cause goes to the log, never to the response.
fn refused(what: &str, fail: Fail) -> Response<BoxBody> {
    match fail {
        Fail::Run(RunError::Timeout) => problem(StatusCode::GATEWAY_TIMEOUT, &format!("eidolon did not print its {what} in time")),
        Fail::Run(RunError::TooBig) => problem(StatusCode::BAD_GATEWAY, &format!("eidolon's {what} are over the size limit")),
        Fail::Run(RunError::Failed(why)) => {
            tracing::warn!(%why, "perch: eidolon's {what} could not be had");
            problem(StatusCode::BAD_GATEWAY, &format!("eidolon could not print its {what}"))
        }
        Fail::Shape(why) => {
            tracing::warn!(%why, "perch: eidolon's {what} are not what this perch reads");
            problem(StatusCode::BAD_GATEWAY, &format!("eidolon's {what} are not in a shape this perch reads"))
        }
    }
}

/// `/hub/meta/<what>`. `rest` is the path below `/hub/meta/` (`tools`, `policy`, `git`, ...), never empty. Only `GET` has a route here; a
/// query on a route that takes none is a 400, as the tree's is for a key it does not know.
pub async fn route(ctx: &Ctx, req: Request<Incoming>, rest: &str) -> Response<BoxBody> {
    let query = req.uri().query().unwrap_or("").to_string();
    let method = req.method().clone();
    drop(req);
    if method != Method::GET {
        return status(StatusCode::NOT_FOUND);
    }
    match rest {
        "policy" => policy(ctx, &query).await,
        "git" => git(ctx, &query).await,
        "tools" | "plugins" | "connectors" | "home" if !query.is_empty() => status(StatusCode::BAD_REQUEST),
        "tools" => tools(ctx).await,
        "plugins" => plugins(ctx).await,
        "connectors" => connectors(ctx).await,
        "home" => home(ctx).await,
        _ => status(StatusCode::NOT_FOUND),
    }
}

/// One run of eidolon as the doors run it: the binary the perch was given, `--config` first, stdin closed, bounded in time and size.
async fn eidolon(ctx: &Ctx, config: Option<&Path>, args: &[&str], cap: usize) -> Result<Vec<u8>, RunError> {
    let within = ctx.opts.knobs.get(KNOB).and_then(|ms| ms.parse().ok()).map(Duration::from_millis).unwrap_or(WITHIN);
    let args: Vec<OsString> = args.iter().map(OsString::from).collect();
    titles::run(&ctx.opts.eidolon, config, &args, cap, within).await
}

fn home_dir() -> Option<String> {
    std::env::var("HOME").ok()
}

// ---------------------------------------------------------------------------
// tools, plugins, policy
// ---------------------------------------------------------------------------

/// `GET /hub/meta/tools`: `[{"name", "approval"?, "plugin"?, "desc"?}]`, in the order eidolon printed them. Never `--live`: that asks
/// the Melete gateway over the network, and a tab that is opened must not.
async fn tools(ctx: &Ctx) -> Response<BoxBody> {
    match tools_cached(ctx).await {
        Ok(list) => json(StatusCode::OK, &*list),
        Err(fail) => refused("tools", fail),
    }
}

async fn tools_cached(ctx: &Ctx) -> Result<Arc<Vec<parse::Tool>>, Fail> {
    let mut slot = ctx.meta.tools.lock().await;
    if let Some(list) = fresh(slot.as_ref(), Instant::now()) {
        return Ok(list);
    }
    // The plugin names first (the lock order is tools, then plugins, and plugins never takes tools): eidolon prints no plugin on a
    // tool, only `<plugin>_<file>`. Best effort, because a tool list without its attribution is still a tool list.
    let names = plugins_cached(ctx).await.map(|plugins| plugins.names.clone()).unwrap_or_default();
    let out = eidolon(ctx, ctx.opts.eidolon_config.as_deref(), &["tools"], TOOLS_CAP).await?;
    let list = Arc::new(parse::tools(&out, &names).map_err(Fail::Shape)?);
    *slot = Some((Instant::now(), list.clone()));
    Ok(list)
}

/// `GET /hub/meta/plugins`: `{"text"}`, `eidolon plugins` as printed.
async fn plugins(ctx: &Ctx) -> Response<BoxBody> {
    match plugins_cached(ctx).await {
        Ok(plugins) => json(StatusCode::OK, &serde_json::json!({ "text": plugins.text })),
        Err(fail) => refused("plugins", fail),
    }
}

async fn plugins_cached(ctx: &Ctx) -> Result<Arc<Plugins>, Fail> {
    let mut slot = ctx.meta.plugins.lock().await;
    if let Some(plugins) = fresh(slot.as_ref(), Instant::now()) {
        return Ok(plugins);
    }
    let out = eidolon(ctx, ctx.opts.eidolon_config.as_deref(), &["plugins"], TEXT_RUN_CAP).await?;
    let plugins = Arc::new(Plugins { names: parse::plugin_names(&String::from_utf8_lossy(&out)), text: parse::text(&out, TEXT_CAP) });
    *slot = Some((Instant::now(), plugins.clone()));
    Ok(plugins)
}

/// The guard's status as the page reads it: `file` as a `~` path, and `why` only when there is something to say.
#[derive(Serialize)]
struct GuardView {
    on: bool,
    file: Option<String>,
    #[serde(skip_serializing_if = "String::is_empty")]
    why: String,
}

impl GuardView {
    fn of(now: guard::GuardStatus, home: Option<&str>) -> Self {
        let file = now.file.as_deref().and_then(Path::to_str).map(|file| parse::tilde(file, home));
        Self { on: now.on, file, why: now.why }
    }
}

/// `GET /hub/meta/policy[?mode=auto|manual|yolo]`: `{"mode", "file", "rows", "guard": {"on", "file", "why"?}}`. No `mode` is the mode a
/// new chat starts in. `file` is the policy file eidolon says it compared, `~` for the home, or `null` when its text names none.
///
/// ```text
///   400  a query that is not exactly `mode=<one of the three>`        409  manual, and no manual config could be made (the sentence)
///   502/504  eidolon's own failure                                    200  the answer
/// ```
async fn policy(ctx: &Ctx, query: &str) -> Response<BoxBody> {
    let Some(mode) = mode_asked(query, ctx.registry.default_mode) else {
        return problem(StatusCode::BAD_REQUEST, "expected no query, or mode=manual|auto|yolo");
    };
    let config = match ctx.registry.door_flags(mode) {
        Ok((config, _yolo)) => config,
        Err(why) => return problem(StatusCode::CONFLICT, &why),
    };
    let policy = match policy_cached(ctx, config).await {
        Ok(policy) => policy,
        Err(fail) => return refused("policy rows", fail),
    };
    let guard = GuardView::of(guard::status(), home_dir().as_deref());
    json(StatusCode::OK, &serde_json::json!({ "mode": mode, "file": policy.file, "rows": policy.rows, "guard": guard }))
}

/// `mode=<word>` and nothing else, or no query at all (`default`).
fn mode_asked(query: &str, default: Mode) -> Option<Mode> {
    if query.is_empty() {
        return Some(default);
    }
    let (key, value) = query.split_once('=')?;
    (files::decode(key)? == "mode").then(|| Mode::parse(&files::decode(value)?)).flatten()
}

async fn policy_cached(ctx: &Ctx, config: Option<&Path>) -> Result<Arc<Policy>, Fail> {
    let key = config.map(Path::to_path_buf);
    let mut cache = ctx.meta.policy.lock().await;
    if let Some(policy) = fresh(cache.get(&key), Instant::now()) {
        return Ok(policy);
    }
    let out = eidolon(ctx, config, &["policy-rows"], TEXT_RUN_CAP).await?;
    let rows = parse::text(&out, TEXT_CAP);
    let file = parse::policy_file(&rows).map(|file| parse::tilde(&file, home_dir().as_deref()));
    let policy = Arc::new(Policy { file, rows });
    cache.insert(key, (Instant::now(), policy.clone()));
    Ok(policy)
}

// ---------------------------------------------------------------------------
// connectors, home
// ---------------------------------------------------------------------------

/// `GET /hub/meta/connectors`: `{"mneme": bool, "melete": bool}`, whether the config the doors run with has the table. Nothing else of
/// the table (its URL, its token file) is read into the answer.
async fn connectors(ctx: &Ctx) -> Response<BoxBody> {
    let path = ctx.opts.eidolon_config.clone().unwrap_or_else(crate::config::member_config_path);
    match configured(&path).await {
        Ok((mneme, melete)) => json(StatusCode::OK, &serde_json::json!({ "mneme": mneme, "melete": melete })),
        Err(why) => {
            tracing::warn!(%why, "perch: the config could not be read for the connectors");
            problem(StatusCode::BAD_GATEWAY, "the eidolon config could not be read")
        }
    }
}

/// `(mneme, melete)`: whether each is a table in the TOML at `path`. A config that is not there has neither.
async fn configured(path: &Path) -> Result<(bool, bool), String> {
    let file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((false, false)),
        Err(e) => return Err(format!("opening it: {e}")),
    };
    let mut text = String::new();
    // One byte past the cap is how "over" is told from "exactly".
    file.take(CONFIG_CAP + 1).read_to_string(&mut text).await.map_err(|e| format!("reading it: {e}"))?;
    if text.len() as u64 > CONFIG_CAP {
        return Err("it is over the size limit".to_string());
    }
    // `message()`, not the error: a TOML error quotes the line it stopped at, and that line may be the connector's.
    let table = text.parse::<toml::Table>().map_err(|e| format!("parsing it: {}", e.message()))?;
    let has = |key: &str| table.get(key).is_some_and(toml::Value::is_table);
    Ok((has("mneme"), has("melete")))
}

/// `GET /hub/meta/home`: `{"default_mode", "source"}`. `default_mode` is what a new chat starts in now; `source` is `"minerva.toml"` when
/// that file names one and `"unset"` (which is yolo) when it does not, or when there is no home.
async fn home(ctx: &Ctx) -> Response<BoxBody> {
    let named = match ctx.registry.home().map(Path::to_path_buf) {
        Some(home) => {
            let read = tokio::task::spawn_blocking(move || crate::config::minerva_default_mode(&home)).await;
            read.ok().and_then(Result::ok).flatten().is_some()
        }
        None => false,
    };
    let source = if named { "minerva.toml" } else { "unset" };
    json(StatusCode::OK, &serde_json::json!({ "default_mode": ctx.registry.default_mode, "source": source }))
}

// ---------------------------------------------------------------------------
// git
// ---------------------------------------------------------------------------

/// `GET /hub/meta/git?session=<id>`: `{"branch": "<name>"}`, `{"branch": null, "sha": "<hex>"}` for a detached head, and `{"branch": null}`
/// for a folder that has no repository (or one this perch does not read: see git.rs). The chat is [`tree::base`]'s, so a live door's
/// folder and a saved chat's are served alike, with the same 404 and 409 for the same reasons, and the folder is judged against `--root`
/// now, whoever stored it. A refusal of the folder itself (a name the tree would not list) is the tree's empty 404.
async fn git(ctx: &Ctx, query: &str) -> Response<BoxBody> {
    let Some(id) = session_asked(query) else { return status(StatusCode::BAD_REQUEST) };
    if !sessions::id_ok(&id) {
        return status(StatusCode::NOT_FOUND);
    }
    let (root, saved) = match tree::base(ctx, &id) {
        Ok(tree::Base::Live(root)) => (root, false),
        Ok(tree::Base::Saved(root)) => (root, true),
        Err(refusal) => return *refusal,
    };
    // A slot before any work: the reads are on the blocking pool and the folder may be on a slow filesystem.
    let Some(permit) = ctx.tree_slots.take() else {
        return problem(StatusCode::SERVICE_UNAVAILABLE, "too many folder reads in flight; retry");
    };
    let registry = ctx.registry.clone();
    let read = tokio::task::spawn_blocking(move || -> Result<Option<git::Head>, ()> {
        let _slot = permit;
        let Some((root, floor)) = registry.tree_cwd(&root) else { return if saved { Err(()) } else { Ok(None) } };
        // The folder's own names are judged as a tree path is: a chat running in `~/.ssh` has no diff tab to show.
        let Some(root) = tree::resolve(&root, &floor, "") else { return Ok(None) };
        Ok(Some(git::head_of(&root)))
    });
    match read.await {
        Ok(Ok(Some(git::Head::Branch(branch)))) => json(StatusCode::OK, &serde_json::json!({ "branch": branch })),
        Ok(Ok(Some(git::Head::Detached(sha)))) => json(StatusCode::OK, &serde_json::json!({ "branch": null, "sha": sha })),
        Ok(Ok(Some(git::Head::Nothing))) => json(StatusCode::OK, &serde_json::json!({ "branch": null })),
        Ok(Err(())) => problem(StatusCode::NOT_FOUND, tree::SAVED_GONE),
        _ => status(StatusCode::NOT_FOUND),
    }
}

/// `session=<id>` and nothing else, decoded once. `None` is the 400.
fn session_asked(query: &str) -> Option<String> {
    if query.contains('&') {
        return None;
    }
    let (key, value) = query.split_once('=')?;
    (files::decode(key)? == "session").then(|| files::decode(value)).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_is_trusted_for_the_ttl_and_not_a_moment_longer() {
        let at = Instant::now();
        let entry: Entry<u8> = (at, Arc::new(7));
        assert_eq!(fresh(Some(&entry), at).as_deref(), Some(&7), "just made");
        assert_eq!(fresh(Some(&entry), at + TTL - Duration::from_millis(1)).as_deref(), Some(&7), "a moment before the ttl");
        assert!(fresh(Some(&entry), at + TTL).is_none(), "at the ttl");
        assert!(fresh(Some(&entry), at + TTL * 2).is_none());
        assert!(fresh::<u8>(None, at).is_none(), "nothing cached");
        // A clock that reads earlier than the entry (it cannot with `Instant`, but the subtraction must not panic) is fresh.
        assert_eq!(fresh(Some(&entry), at.checked_sub(Duration::from_secs(1)).unwrap_or(at)).as_deref(), Some(&7));
    }

    /// What the page reads for the guard line: on names the file as a `~` path and has no `why`; off has a `why` and no file; the stub
    /// (S0P-a's, `why: "not built yet"`) is the off case.
    ///
    /// Mutation: serialize `why` when empty, or leave the file absolute, and the first row goes red.
    #[test]
    fn the_guard_line_is_on_with_a_file_or_off_with_a_reason() {
        let on = guard::GuardStatus { on: true, file: Some("/home/u/.config/eidolon/minerva-guard.rn".into()), why: String::new() };
        assert_eq!(
            serde_json::to_string(&GuardView::of(on, Some("/home/u"))).unwrap(),
            "{\"on\":true,\"file\":\"~/.config/eidolon/minerva-guard.rn\"}"
        );
        let off = guard::GuardStatus { on: false, file: None, why: "the policy directory is not writable".into() };
        assert_eq!(
            serde_json::to_string(&GuardView::of(off, Some("/home/u"))).unwrap(),
            "{\"on\":false,\"file\":null,\"why\":\"the policy directory is not writable\"}"
        );
        let stub = guard::GuardStatus { on: false, file: None, why: "not built yet".into() };
        assert_eq!(serde_json::to_string(&GuardView::of(stub, None)).unwrap(), "{\"on\":false,\"file\":null,\"why\":\"not built yet\"}");
        let elsewhere = guard::GuardStatus { on: true, file: Some("/etc/eidolon/minerva-guard.rn".into()), why: String::new() };
        assert_eq!(GuardView::of(elsewhere, Some("/home/u")).file.as_deref(), Some("/etc/eidolon/minerva-guard.rn"), "outside home stays absolute");
    }

    #[test]
    fn the_policy_query_is_nothing_or_exactly_one_mode() {
        assert_eq!(mode_asked("", Mode::Auto), Some(Mode::Auto), "no query is the default");
        assert_eq!(mode_asked("mode=manual", Mode::Auto), Some(Mode::Manual));
        assert_eq!(mode_asked("mode=yolo", Mode::Auto), Some(Mode::Yolo));
        assert_eq!(mode_asked("%6dode=%61uto", Mode::Yolo), Some(Mode::Auto), "decoded once, as the tree's is");
        for bad in ["mode=", "mode=gated", "mode=auto&mode=auto", "mode=auto&x=1", "mod=auto", "auto", "mode", "mode=%zz", "mode=Auto", "x=mode=auto"] {
            assert_eq!(mode_asked(bad, Mode::Auto), None, "{bad:?}");
        }
    }

    #[test]
    fn the_git_query_is_exactly_one_session() {
        assert_eq!(session_asked("session=abc").as_deref(), Some("abc"));
        assert_eq!(session_asked("%73ession=a%2db").as_deref(), Some("a-b"));
        for bad in ["", "session", "id=abc", "session=a&session=b", "session=a&path=x", "session=%zz", "session=a&"] {
            assert_eq!(session_asked(bad), None, "{bad:?}");
        }
    }

    #[tokio::test]
    async fn a_connector_is_a_table_and_only_a_table_counts() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        let check = |text: &'static str| {
            let (config, text) = (config.clone(), text);
            async move {
                std::fs::write(&config, text).unwrap();
                configured(&config).await
            }
        };
        assert_eq!(check("").await, Ok((false, false)));
        assert_eq!(check("[mneme]\nurl = \"http://x\"\ntoken_file = \"/t\"\n").await, Ok((true, false)));
        assert_eq!(check("[melete]\nurl = \"http://y\"\n[mneme]\n").await, Ok((true, true)), "an empty table is configured");
        assert_eq!(check("mneme = \"http://x\"\nmelete = 1\n").await, Ok((false, false)), "a key that is not a table is not one");
        assert_eq!(check("[providers.mneme]\nx = 1\n").await, Ok((false, false)), "a table of another name's is not the top-level one");
        assert!(check("[mneme\nurl = \"http://secret.example/x\"\n").await.is_err());
        assert_eq!(configured(&dir.path().join("absent.toml")).await, Ok((false, false)), "no config, no connectors");
    }

    #[tokio::test]
    async fn a_config_error_does_not_quote_the_config() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        std::fs::write(&config, "[mneme]\ntoken_file = \"/home/u/.config/eidolon/mneme.token\" garbage\n").unwrap();
        let why = configured(&config).await.unwrap_err();
        assert!(!why.contains("token") && !why.contains("mneme") && !why.contains("/home"), "{why}");
    }
}
