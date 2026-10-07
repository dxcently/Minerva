//! `/hub/mesh`: the mesh view (slice R2b). Network-free: it reads Aoide's own files, asks the `aoide` CLI for `mesh --json` and
//! `mail outbox --json`, pings the daemon once, and never runs anything that touches a node.
//!
//! ```text
//!   GET /hub/mesh                  the snapshot: available, aoide, daemon, registry, cache, graph, mesh, outbox, errors
//!   GET /hub/mesh/events[?after=N] the audit and gate events since byte N of --aoide-events, at most 64 KiB of the file per call
//! ```
//!
//! `--aoide-events` is any JSON-lines file whose lines are objects with a `class` (aoide's audit log is one, and so is the feed aoided
//! keeps beside its socket). Only lines of class `audit` or `gate` are shown. The runner hands the same path to every call it makes as
//! `AOIDE_AUDIT_LOG` (R2a), so what the perch runs is audited there by aoide itself and the perch keeps no log of its own.
//!
//! **What is never done.** The only calls to Aoide are [`Shape::Version`], [`Shape::MeshJson`] and [`Shape::MailOutboxJson`], through
//! [`crate::aoide::Aoide`] (so never a `PATH` lookup, and nothing else is spellable: `node list`, `session`, `send`, `pair`, `spawn`,
//! `allow`, `node status`, `node pull`, `dispatch` and the A2A door have no variant). The daemon socket is sent `{"v":0,"op":"ping"}` and
//! nothing else, never `subscribe` or `dispatch`, and the events come from tailing the file. No request carries a word into any of it:
//! the one parameter, `after`, is digits.
//!
//! **The files** are read under `<aoide-home>/state/` (`--aoide-home`, else `~/.aoide`), each opened `O_NOFOLLOW | O_NONBLOCK`, checked to
//! be a regular file and read under a cap, so a link or a FIFO planted under a name is not read:
//!
//! - `nodes.json`, the registry, through an **allowlist**: `name`, `url` (without any `user:pass@`), `verified`, `autogate`, `addedAt`,
//!   `via`, and the capability words as `allows` (a flat list: the shape before P-CHARTER) and `grants` (mesh to words: per-mesh, after
//!   it). `pubkey`, `tokenFile`, `bearerSecret` and everything else are dropped, so a field a later Aoide adds is never forwarded by
//!   accident. A shape this code does not read is dropped and said in `errors`, because `schemaVersion` stayed `"0"` through every change
//!   and drift would otherwise be invisible.
//! - `node-cache/*.json`, at most 64 files of at most 64 KiB, as **text** with their age (remote-authored: never parsed here).
//! - `stage/graph.json`, at most 2 MiB, as counts and a list of nodes with an allowlist of fields (no `say`, `activity`, `socket`,
//!   `children`): the agents' words are not the mesh tab's to show.
//!
//! **The caches.** `mesh --json`, `mail outbox --json` and `--version` are each cached for 60 s ([`TTL`]; `--knob mesh_ttl_ms=N` shortens
//! it for a test), so any number of tabs cost one run of each. A failure is not cached. The lock is held across the run, so two tabs
//! opened together cost one run. The three run at once, so a snapshot waits for the slowest (10 s at most) and not for their sum.
//!
//! **`nodes(ctx)`** is what mail's send route asks: the registry's nodes with the meshes `mesh --json` names them in. That report lists
//! the rows that drift and not a clean bill per node (aoide 0.0.23 prints no row for a node that matches its declaration), so a healthy
//! node can have no meshes here; aoide's own refusal for a node trusted in more than one mesh (`data.meshes`) is what mail relies on then.

use std::collections::BTreeMap;
use std::future::Future;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::UnixStream;
use tokio::sync::Mutex;

use crate::aoide::{self, Aoide, Error, Reply, Shape};
use crate::serve::{json, problem, status, BoxBody, Ctx};
use crate::Options;

/// How long a run of `mesh --json`, `mail outbox --json` or `--version` is trusted.
pub const TTL: Duration = Duration::from_secs(60);
/// `--knob mesh_ttl_ms=N` replaces [`TTL`], so expiry can be proven without a minute's wait.
const KNOB: &str = "mesh_ttl_ms";
/// The daemon gets this long, from connecting to its answer.
const PING_WITHIN: Duration = Duration::from_secs(1);
/// A ping's answer is one line of this many bytes at most; the rest is not read.
const PING_LINE: u64 = 4096;
/// `nodes.json`: a few hundred bytes a node.
const REGISTRY_CAP: u64 = 1024 * 1024;
/// At most this many registry entries are read.
const MAX_NODES: usize = 256;
/// At most this many cache files are listed, each of at most [`CACHE_CAP`] bytes of text.
const CACHE_FILES: usize = 64;
const CACHE_CAP: usize = 64 * 1024;
/// A directory is looked at for at most this many entries, whatever is in it.
const SCAN_LIMIT: usize = 1000;
const GRAPH_CAP: u64 = 2 * 1024 * 1024;
/// At most this many graph nodes are listed; the counts are of all of them.
const GRAPH_NODES: usize = 500;
/// The events file is read this many bytes at a time.
const EVENTS_WINDOW: u64 = 64 * 1024;
/// The only classes the page is shown: `secret` names secrets and consumers, `notification` carries forwarded text, `rice` and `content`
/// are not the mesh's.
const CLASSES: [&str; 2] = ["audit", "gate"];
/// A capability word (`message`, `read`, `spawn`, and whatever a later Aoide adds) is this long at most, and a node has this many.
const WORD_MAX: usize = 32;
const WORDS_MAX: usize = 16;

const NO_AOIDE: &str = "Aoide is not available here: this perch has no aoide program (--aoide, or an executable ~/.local/bin/aoide) and \
there is no state folder to read";

/// One cached answer and when it was got. `Arc`, so a hit is a clone of a pointer.
struct Cached<T>(Mutex<Option<(Instant, Arc<T>)>>);

impl<T> Default for Cached<T> {
    fn default() -> Self {
        Self(Mutex::new(None))
    }
}

impl<T> Cached<T> {
    /// The answer got less than `ttl` ago, else the one `run` gives. Only a success is kept: a failure is asked for again next time.
    async fn get<E>(&self, ttl: Duration, run: impl Future<Output = Result<T, E>>) -> Result<Arc<T>, E> {
        let mut slot = self.0.lock().await;
        if let Some(value) = slot.as_ref().filter(|(at, _)| at.elapsed() < ttl).map(|(_, value)| value.clone()) {
            return Ok(value);
        }
        let value = Arc::new(run.await?);
        *slot = Some((Instant::now(), value.clone()));
        Ok(value)
    }
}

/// The mesh's caches. `Default` is how [`Ctx`] gets one before any hook runs.
#[derive(Default)]
pub struct State {
    version: Cached<Option<String>>,
    mesh: Cached<Value>,
    outbox: Cached<Value>,
}

fn ttl(opts: &Options) -> Duration {
    opts.knobs.get(KNOB).and_then(|ms| ms.parse().ok()).map(Duration::from_millis).unwrap_or(TTL)
}

/// `/hub/mesh` and everything under it. `rest` is `""` for the bare path and `events` for the tail. Only `GET` has a route here; a query
/// on the snapshot is a 400, as the meta routes' is.
pub async fn route(ctx: &Ctx, req: Request<Incoming>, rest: &str) -> Response<BoxBody> {
    let query = req.uri().query().unwrap_or("").to_string();
    let method = req.method().clone();
    drop(req);
    if method != Method::GET {
        return status(StatusCode::NOT_FOUND);
    }
    match rest {
        "" if !query.is_empty() => status(StatusCode::BAD_REQUEST),
        "" => snapshot(ctx).await,
        "events" => events(ctx, &query).await,
        _ => status(StatusCode::NOT_FOUND),
    }
}

// ---------------------------------------------------------------------------
// Where things are
// ---------------------------------------------------------------------------

/// What the perch's own unit tests get for an Aoide path nobody gave them: a folder that is not there, so no test that forgot a flag can
/// read the operator's `~/.aoide` or ping a real aoided. Only the unit tests of this crate see it (`cfg(test)`); the integration tests
/// run the real binary, whose environment is the harness's to set (they pass `--aoide-home` and a `HOME` of their own).
#[cfg(test)]
const TEST_DEFAULT: &str = "/nonexistent-minerva-test/aoide";

/// Aoide's home: `--aoide-home`, else `~/.aoide` (what aoide itself defaults to). Absolute or nothing.
pub fn home(opts: &Options) -> Option<PathBuf> {
    opts.aoide_home.clone().or_else(default_home).filter(|home| home.is_absolute())
}

#[cfg(not(test))]
fn default_home() -> Option<PathBuf> {
    std::env::var_os("HOME").filter(|home| !home.is_empty()).map(|home| PathBuf::from(home).join(".aoide"))
}

#[cfg(test)]
fn default_home() -> Option<PathBuf> {
    Some(PathBuf::from(TEST_DEFAULT))
}

fn state_dir(opts: &Options) -> Option<PathBuf> {
    home(opts).map(|home| home.join("state"))
}

/// `--aoide-daemon-socket`, else aoided's own default under `$XDG_RUNTIME_DIR`.
fn daemon_socket(opts: &Options) -> Option<PathBuf> {
    opts.aoide_daemon_socket.clone().or_else(default_socket).filter(|socket| socket.is_absolute())
}

#[cfg(not(test))]
fn default_socket() -> Option<PathBuf> {
    std::env::var_os("XDG_RUNTIME_DIR").filter(|dir| !dir.is_empty()).map(|dir| PathBuf::from(dir).join("aoide").join("aoided.sock"))
}

#[cfg(test)]
fn default_socket() -> Option<PathBuf> {
    Some(PathBuf::from(TEST_DEFAULT).join("aoided.sock"))
}

/// `--aoide-events` and nothing else. Aoide has two JSON-lines files of records with a `class` (the audit log, and the feed aoided keeps
/// beside its socket) and the flag does not say which, so no default is guessed: without it the tail is empty.
fn events_path(opts: &Options) -> Option<PathBuf> {
    opts.aoide_events.clone().filter(|path| path.is_absolute())
}

/// A path as the page shows it: `~` for the home directory.
fn tilde(path: &Path) -> String {
    tilde_in(std::env::var_os("HOME").map(PathBuf::from).as_deref(), path)
}

/// [`tilde`] for a given home. A home that is `/` is no home to abbreviate.
fn tilde_in(home: Option<&Path>, path: &Path) -> String {
    let home = home.filter(|home| home.parent().is_some());
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

// ---------------------------------------------------------------------------
// The calls to aoide, cached
// ---------------------------------------------------------------------------

/// `data` of an envelope, or the sentence for why there is none.
fn data_of(outcome: Result<Reply, Error>, what: &str) -> Result<Value, String> {
    let reply = outcome.map_err(|e| format!("aoide {what}: {e}"))?;
    reply.data().cloned().ok_or_else(|| format!("aoide {what} printed no data"))
}

async fn version_of(ctx: &Ctx, aoide: &Aoide) -> Result<Arc<Option<String>>, String> {
    ctx.mesh.version.get(ttl(&ctx.opts), async { aoide.version().await.map_err(|e| format!("aoide --version: {e}")) }).await
}

async fn mesh_of(ctx: &Ctx, aoide: &Aoide) -> Result<Arc<Value>, String> {
    ctx.mesh.mesh.get(ttl(&ctx.opts), async { data_of(aoide.run(&Shape::MeshJson).await, "mesh") }).await
}

async fn outbox_of(ctx: &Ctx, aoide: &Aoide) -> Result<Arc<Value>, String> {
    ctx.mesh.outbox.get(ttl(&ctx.opts), async { data_of(aoide.run(&Shape::MailOutboxJson).await, "mail outbox") }).await
}

/// Asks the daemon `{"v":0,"op":"ping"}` and nothing else, and how long it took when it said `ok`. `None` for no socket, no answer in
/// [`PING_WITHIN`], an answer that is not `{"ok":true,..}`. Only that boolean is read: the rest of the reply (a pid, a key) goes nowhere.
async fn ping(socket: &Path) -> Option<u64> {
    let begun = Instant::now();
    let asked = async {
        let mut stream = UnixStream::connect(socket).await.ok()?;
        stream.write_all(b"{\"v\":0,\"op\":\"ping\"}\n").await.ok()?;
        let mut line = Vec::new();
        tokio::io::BufReader::new((&mut stream).take(PING_LINE)).read_until(b'\n', &mut line).await.ok()?;
        let reply: Value = serde_json::from_slice(&line).ok()?;
        (reply.get("ok") == Some(&Value::Bool(true))).then_some(())
    };
    tokio::time::timeout(PING_WITHIN, asked).await.ok().flatten()?;
    Some(begun.elapsed().as_millis() as u64)
}

// ---------------------------------------------------------------------------
// Aoide's files
// ---------------------------------------------------------------------------

/// `path`'s text when it is a regular file of at most `cap` bytes (lossy UTF-8), `None` when it is not there, and a sentence about `what`
/// when it is something else. Nothing is followed or waited for: the open is `O_NOFOLLOW | O_NONBLOCK` and the size is the open file's.
fn read_text(path: &Path, cap: u64, what: &str) -> Result<Option<String>, String> {
    let (file, len) = match crate::rawfile::open_regular(path) {
        Ok(opened) => opened,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            tracing::warn!(error = %e, what, "perch: an aoide file was not read");
            return Err(format!("{what} could not be read as a plain file"));
        }
    };
    let over = || format!("{what} is over {} KiB", cap / 1024);
    if len > cap {
        return Err(over());
    }
    let mut bytes = Vec::new();
    // One byte past the cap: a file that grew since the `fstat` is still bounded.
    file.take(cap + 1).read_to_end(&mut bytes).map_err(|_| format!("{what} could not be read"))?;
    if bytes.len() as u64 > cap {
        return Err(over());
    }
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

/// At most `max` characters of a JSON string, and `None` for anything that is not one.
fn clip_str(value: Option<&Value>, max: usize) -> Option<String> {
    value?.as_str().map(|text| text.chars().take(max).collect())
}

/// One registered node as the page sees it. This struct is the allowlist: a field that is not here is not in any answer.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct NodeView {
    pub name: String,
    pub url: String,
    pub verified: bool,
    pub autogate: bool,
    #[serde(rename = "addedAt")]
    pub added_at: String,
    pub via: Option<String>,
    /// The capability words as one list: aoide before P-CHARTER (`allows`), and a flat `grants`.
    pub allows: Vec<String>,
    /// Words per mesh: the per-mesh `grants` of aoide after P-CHARTER (modelled from its announcement, not observed).
    pub grants: BTreeMap<String, Vec<String>>,
}

/// What reading `nodes.json` made of it: the nodes, and how many entries or fields were left out for a shape this code does not read.
#[derive(Debug, Default, PartialEq)]
struct Registry {
    nodes: Vec<NodeView>,
    skipped: usize,
    unread: usize,
}

/// `url` without a `user:pass@` in front of the host.
fn without_userinfo(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else { return url.to_string() };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    match authority.rfind('@') {
        Some(at) => format!("{scheme}://{}{tail}", &authority[at + 1..]),
        None => url.to_string(),
    }
}

fn word_ok(word: &str) -> bool {
    !word.is_empty()
        && word.len() <= WORD_MAX
        && word.starts_with(|c: char| c.is_ascii_lowercase())
        && word.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// A list of capability words, or `None` when it is anything else (one element that is not a word spoils the whole list).
fn words_of(value: &Value) -> Option<Vec<String>> {
    let list = value.as_array().filter(|list| list.len() <= WORDS_MAX)?;
    list.iter().map(|word| word.as_str().filter(|word| word_ok(word)).map(str::to_string)).collect()
}

/// Capability words: the flat ones, and the ones per mesh.
type Words = (Vec<String>, BTreeMap<String, Vec<String>>);

/// `allows` or `grants`: a flat list of words, or an object of mesh name to such a list. Anything else is `None`.
fn grants_of(value: &Value) -> Option<Words> {
    if let Some(words) = words_of(value) {
        return Some((words, BTreeMap::new()));
    }
    let object = value.as_object().filter(|object| object.len() <= WORDS_MAX)?;
    let mut per_mesh = BTreeMap::new();
    for (mesh, words) in object {
        if !aoide::name_ok(mesh) {
            return None;
        }
        per_mesh.insert(mesh.clone(), words_of(words)?);
    }
    Some((Vec::new(), per_mesh))
}

/// The registry file, through the allowlist. Both shapes are read: an object with `nodes` (or the pre-rename `peers`), or a bare list;
/// capability words flat (`allows`) or per mesh (`grants`). An entry without a usable name is skipped; a field that is not in a shape
/// above is left out and counted.
fn project_registry(text: &str) -> Result<Registry, String> {
    let parsed: Value = serde_json::from_str(text).map_err(|_| "the node registry is not JSON".to_string())?;
    let list = parsed.as_array().or_else(|| parsed.get("nodes").or_else(|| parsed.get("peers")).and_then(Value::as_array));
    let Some(list) = list else { return Err("the node registry is not in a shape this perch reads".to_string()) };
    let mut registry = Registry::default();
    for entry in list.iter().take(MAX_NODES) {
        let Some(name) = entry.get("name").and_then(Value::as_str).filter(|name| aoide::name_ok(name)) else {
            registry.skipped += 1;
            continue;
        };
        let flag = |key: &str| entry.get(key).and_then(Value::as_bool).unwrap_or(false);
        let mut view = NodeView {
            name: name.to_string(),
            url: without_userinfo(&clip_or_empty(entry.get("url"), 300)),
            verified: flag("verified"),
            autogate: flag("autogate"),
            added_at: clip_or_empty(entry.get("addedAt"), 64),
            via: clip_str(entry.get("via"), 200),
            allows: Vec::new(),
            grants: BTreeMap::new(),
        };
        for key in ["allows", "grants"] {
            match entry.get(key) {
                None | Some(Value::Null) => {}
                Some(value) => match grants_of(value) {
                    Some((words, per_mesh)) => {
                        for word in words {
                            if !view.allows.contains(&word) {
                                view.allows.push(word);
                            }
                        }
                        view.grants.extend(per_mesh);
                    }
                    None => registry.unread += 1,
                },
            }
        }
        registry.nodes.push(view);
    }
    registry.skipped += list.len().saturating_sub(MAX_NODES);
    Ok(registry)
}

fn clip_or_empty(value: Option<&Value>, max: usize) -> String {
    clip_str(value, max).unwrap_or_default()
}

fn read_registry(dir: &Path) -> Result<Registry, String> {
    match read_text(&dir.join("nodes.json"), REGISTRY_CAP, "the node registry")? {
        Some(text) => project_registry(&text),
        None => Ok(Registry::default()),
    }
}

/// One cached roster, as the text of its file. The text is another node's, so it is never parsed here and the page draws it as text.
#[derive(Debug, PartialEq, Serialize)]
struct CacheView {
    name: String,
    age_secs: u64,
    text: String,
    /// The file was longer than 64 KiB and the text is its start.
    cut: bool,
}

/// `node-cache/*.json`: the first [`CACHE_FILES`] by name, regular files only (a link is not followed), and whether there were more.
fn read_cache(dir: &Path) -> (Vec<CacheView>, bool) {
    let Ok(entries) = std::fs::read_dir(dir) else { return (Vec::new(), false) };
    let mut found: Vec<(String, PathBuf)> = Vec::new();
    for entry in entries.flatten().take(SCAN_LIMIT) {
        let path = entry.path();
        let Some(stem) = path.file_name().and_then(|name| name.to_str()).and_then(|name| name.strip_suffix(".json")) else { continue };
        let plain = std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_file());
        if !stem.is_empty() && plain {
            found.push((stem.chars().take(64).collect(), path));
        }
    }
    found.sort();
    let more = found.len() > CACHE_FILES;
    let mut views = Vec::new();
    for (name, path) in found.into_iter().take(CACHE_FILES) {
        let Ok((file, _)) = crate::rawfile::open_regular(&path) else { continue };
        let age_secs = file
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| SystemTime::now().duration_since(modified).ok())
            .map_or(0, |age| age.as_secs());
        let mut bytes = Vec::new();
        if file.take(CACHE_CAP as u64 + 1).read_to_end(&mut bytes).is_err() {
            continue;
        }
        let cut = bytes.len() > CACHE_CAP;
        bytes.truncate(CACHE_CAP);
        views.push(CacheView { name, age_secs, text: String::from_utf8_lossy(&bytes).into_owned(), cut });
    }
    (views, more)
}

/// The stage graph, counted and listed. Each node keeps these fields and no others.
const GRAPH_KEYS: [&str; 12] = ["id", "kind", "name", "agent", "state", "title", "petname", "role", "cwd", "startedAt", "model", "fetchedAt"];

#[derive(Debug, PartialEq, Serialize)]
struct GraphView {
    nodes: Vec<Value>,
    /// All the nodes there are, which is more than `nodes` holds when it was cut at 500.
    total: usize,
    kinds: BTreeMap<String, usize>,
    edges: usize,
}

fn project_graph(text: &str) -> Result<GraphView, String> {
    let parsed: Value = serde_json::from_str(text).map_err(|_| "the stage graph is not JSON".to_string())?;
    let Some(nodes) = parsed.get("nodes").and_then(Value::as_array) else {
        return Err("the stage graph is not in a shape this perch reads".to_string());
    };
    let mut view = GraphView { nodes: Vec::new(), total: nodes.len(), kinds: BTreeMap::new(), edges: 0 };
    view.edges = parsed.get("edges").and_then(Value::as_array).map_or(0, Vec::len);
    for node in nodes {
        let kind = clip_or_empty(node.get("kind"), 32);
        *view.kinds.entry(kind).or_default() += 1;
        if view.nodes.len() == GRAPH_NODES {
            continue;
        }
        let mut kept = serde_json::Map::new();
        for key in GRAPH_KEYS {
            if let Some(value) = clip_str(node.get(key), 200) {
                kept.insert(key.to_string(), Value::String(value));
            }
        }
        // A node's own cached graph nests as `children`: how many, and nothing of what they say.
        if let Some(children) = node.pointer("/children/nodes").and_then(Value::as_array) {
            kept.insert("children".to_string(), Value::from(children.len()));
        }
        view.nodes.push(Value::Object(kept));
    }
    Ok(view)
}

/// Everything under `state/` the snapshot reads, in one blocking pass.
#[derive(Default)]
struct Files {
    /// `state/` is there.
    exists: bool,
    registry: Vec<NodeView>,
    cache: Vec<CacheView>,
    graph: Option<GraphView>,
    /// Sentences about what could not be read, or was left out.
    notes: Vec<String>,
}

fn read_files(dir: &Path) -> Files {
    let mut files = Files { exists: dir.is_dir(), ..Files::default() };
    if !files.exists {
        return files;
    }
    match read_registry(dir) {
        Ok(registry) => {
            files.registry = registry.nodes;
            if registry.skipped > 0 {
                files.notes.push(format!("{} registry entries were left out: not a node name, or past the first {MAX_NODES}", registry.skipped));
            }
            if registry.unread > 0 {
                files.notes.push(format!("{} registry fields were left out: grants in a shape this perch does not read", registry.unread));
            }
        }
        Err(why) => files.notes.push(why),
    }
    let (cache, more) = read_cache(&dir.join("node-cache"));
    files.cache = cache;
    if more {
        files.notes.push(format!("the node cache holds more than {CACHE_FILES} files; the first {CACHE_FILES} are shown"));
    }
    // aoide 0.0.23 keeps the conducting stage under `state/stage/`.
    let graph_file = dir.join("stage").join("graph.json");
    match read_text(&graph_file, GRAPH_CAP, "the stage graph").and_then(|text| text.map(|text| project_graph(&text)).transpose()) {
        Ok(graph) => files.graph = graph,
        Err(why) => files.notes.push(why),
    }
    files
}

// ---------------------------------------------------------------------------
// GET /hub/mesh
// ---------------------------------------------------------------------------

/// `GET /hub/mesh`, always a 200 (what is missing is said in `errors` and in the empty or `null` part):
///
/// ```text
///   available  bool        an aoide program is configured, or there is a state folder to read
///   aoide      {path, version}|null      the program as a `~` path, and its --version (null when it did not say)
///   daemon     {socket, ping_ms}         the socket as a `~` path or null; milliseconds when aoided answered a ping, else null
///   registry   [{name, url, verified, autogate, addedAt, via, allows, grants}]      the allowlist, in the file's order
///   cache      [{name, age_secs, text, cut}]       node-cache/*.json as text, by name
///   graph      {nodes, total, kinds, edges}|null   the stage graph, counted and listed
///   mesh       `data` of `aoide mesh --json`|null     outbox   `data` of `aoide mail outbox --json`|null
///   errors     [sentence]
/// ```
async fn snapshot(ctx: &Ctx) -> Response<BoxBody> {
    let aoide = Aoide::from_options(&ctx.opts);
    let files = match state_dir(&ctx.opts) {
        Some(dir) => tokio::task::spawn_blocking(move || read_files(&dir)).await.unwrap_or_default(),
        None => Files::default(),
    };
    if aoide.is_err() && !files.exists {
        return json(StatusCode::OK, &unavailable());
    }
    let socket = daemon_socket(&ctx.opts);
    let mut errors = files.notes;
    let (version, mesh, outbox, ping_ms) = match &aoide {
        Ok(aoide) => {
            let (version, mesh, outbox, ping_ms) =
                tokio::join!(version_of(ctx, aoide), mesh_of(ctx, aoide), outbox_of(ctx, aoide), async {
                    match &socket {
                        Some(socket) => ping(socket).await,
                        None => None,
                    }
                });
            let mut got = |outcome: Result<Arc<Value>, String>| outcome.map_err(|why| errors.push(why)).ok();
            let (mesh, outbox) = (got(mesh), got(outbox));
            let version = version.map_err(|why| errors.push(why)).ok().and_then(|version| (*version).clone());
            (version, mesh, outbox, ping_ms)
        }
        Err(e) => {
            errors.push(e.to_string());
            (None, None, None, match &socket {
                Some(socket) => ping(socket).await,
                None => None,
            })
        }
    };
    let program = aoide.as_ref().ok().map(|aoide| serde_json::json!({ "path": tilde(&aoide.bin), "version": version }));
    json(
        StatusCode::OK,
        &serde_json::json!({
            "available": true,
            "aoide": program,
            "daemon": { "socket": socket.as_deref().map(tilde), "ping_ms": ping_ms },
            "registry": files.registry,
            "cache": files.cache,
            "graph": files.graph,
            "mesh": mesh.as_deref(),
            "outbox": outbox.as_deref(),
            "errors": errors,
        }),
    )
}

/// The answer of a perch with no aoide and no state: one sentence, and every part empty.
fn unavailable() -> Value {
    serde_json::json!({
        "available": false,
        "aoide": null,
        "daemon": { "socket": null, "ping_ms": null },
        "registry": [],
        "cache": [],
        "graph": null,
        "mesh": null,
        "outbox": null,
        "errors": [NO_AOIDE],
    })
}

// ---------------------------------------------------------------------------
// nodes, for mail
// ---------------------------------------------------------------------------

/// A node mail can name, and the meshes it can be reached through.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct NodeInfo {
    pub name: String,
    pub verified: bool,
    /// The meshes `mesh --json` has a row for this node in, in the order it lists them. Empty when none has (a node that matches its
    /// declaration has no row), or when `mesh --json` could not be had.
    pub meshes: Vec<String>,
}

/// The registered nodes with their meshes: what mail's send route checks a node and a mesh against. The mesh report is the cached one,
/// so asking costs at most one `mesh --json` a minute. No registry, no aoide, or a failed run is a shorter list, never an error.
pub async fn nodes(ctx: &Ctx) -> Vec<NodeInfo> {
    let Some(dir) = state_dir(&ctx.opts) else { return Vec::new() };
    let registry = tokio::task::spawn_blocking(move || read_registry(&dir)).await.ok().and_then(Result::ok).unwrap_or_default();
    let mesh = match Aoide::from_options(&ctx.opts) {
        Ok(aoide) => mesh_of(ctx, &aoide).await.ok(),
        Err(_) => None,
    };
    node_infos(&registry.nodes, mesh.as_deref())
}

fn node_infos(registry: &[NodeView], mesh: Option<&Value>) -> Vec<NodeInfo> {
    let meshes = mesh.map(meshes_by_node).unwrap_or_default();
    registry
        .iter()
        .map(|node| NodeInfo { name: node.name.clone(), verified: node.verified, meshes: meshes.get(&node.name).cloned().unwrap_or_default() })
        .collect()
}

/// `data` of `mesh --json` as node to the meshes whose `rows` name it: `data.report.sections[].{name, rows[].node}`, read defensively.
fn meshes_by_node(data: &Value) -> BTreeMap<String, Vec<String>> {
    let mut by_node: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let sections = data.pointer("/report/sections").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default();
    for section in sections {
        let Some(mesh) = section.get("name").and_then(Value::as_str).filter(|mesh| aoide::name_ok(mesh)) else { continue };
        for row in section.get("rows").and_then(Value::as_array).map(Vec::as_slice).unwrap_or_default() {
            let Some(node) = row.get("node").and_then(Value::as_str) else { continue };
            let meshes = by_node.entry(node.to_string()).or_default();
            if !meshes.iter().any(|known| known == mesh) {
                meshes.push(mesh.to_string());
            }
        }
    }
    by_node
}

// ---------------------------------------------------------------------------
// GET /hub/mesh/events
// ---------------------------------------------------------------------------

/// `after=<digits>` and nothing else, or no query at all (`Some(None)`: the newest events). `None` is the 400.
fn after_asked(query: &str) -> Option<Option<u64>> {
    if query.is_empty() {
        return Some(None);
    }
    let digits = query.strip_prefix("after=");
    let digits = digits.filter(|digits| !digits.is_empty() && digits.len() <= 20 && digits.bytes().all(|b| b.is_ascii_digit()))?;
    digits.parse().ok().map(Some)
}

/// What one poll of the feed found.
#[derive(Debug, PartialEq, Serialize)]
struct Tail {
    /// The feed's lines of class `audit` or `gate`, as the objects they are, oldest first. A line that is not an object is left out.
    events: Vec<Value>,
    /// Where the next poll starts (`?after=`): the end of the last whole line read.
    next: u64,
    /// The cursor was past the end of the file (it was truncated in place, or replaced), so this read started again at 0.
    reset: bool,
}

/// One poll of the feed at `path`. `after` is the byte offset of the last poll's `next`; `None` is the first poll, which starts at the
/// last 64 KiB (from the first whole line in it). At most [`EVENTS_WINDOW`] bytes of the file are read, and only whole lines are used: a
/// last line without its newline is read again next time, once it has one. A window with no newline at all is one line too long to be an
/// event, and is passed over.
fn tail(path: &Path, after: Option<u64>) -> std::io::Result<Tail> {
    let (mut file, len) = match crate::rawfile::open_regular(path) {
        Ok(opened) => opened,
        // No feed yet is a feed with nothing in it, and a cursor from a feed that was there is past its end.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Tail { events: Vec::new(), next: 0, reset: after.is_some_and(|at| at > 0) });
        }
        Err(e) => return Err(e),
    };
    let (mut start, reset) = match after {
        Some(at) if at > len => (0, true),
        Some(at) => (at, false),
        None => (len.saturating_sub(EVENTS_WINDOW), false),
    };
    file.seek(SeekFrom::Start(start))?;
    let mut window = Vec::new();
    (&file).take(EVENTS_WINDOW).read_to_end(&mut window)?;
    if after.is_none() && start > 0 {
        // The first poll starts at the window's edge, which is most likely the middle of a line: that one is not an event.
        let skip = window.iter().position(|&b| b == b'\n').map_or(window.len(), |at| at + 1);
        window.drain(..skip);
        start += skip as u64;
    }
    let whole = match window.iter().rposition(|&b| b == b'\n') {
        Some(at) => at + 1,
        None if window.len() as u64 >= EVENTS_WINDOW => window.len(),
        None => 0,
    };
    let events = window[..whole]
        .split(|&b| b == b'\n')
        .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
        .filter(|event| event.is_object() && event.get("class").and_then(Value::as_str).is_some_and(|class| CLASSES.contains(&class)))
        .collect();
    Ok(Tail { events, next: start + whole as u64, reset })
}

/// `GET /hub/mesh/events[?after=N]`: `{"events", "next", "reset"}` (see [`Tail`]). A feed that is not there yet is an empty one, and so is
/// a perch that could name no feed. A feed that is not a plain file is a 502.
async fn events(ctx: &Ctx, query: &str) -> Response<BoxBody> {
    let Some(after) = after_asked(query) else { return problem(StatusCode::BAD_REQUEST, "expected no query, or after=<offset>") };
    let Some(path) = events_path(&ctx.opts) else { return json(StatusCode::OK, &Tail { events: Vec::new(), next: 0, reset: false }) };
    match tokio::task::spawn_blocking(move || tail(&path, after)).await {
        Ok(Ok(tail)) => json(StatusCode::OK, &tail),
        outcome => {
            tracing::warn!(error = ?outcome.map(|read| read.err()), "perch: the events file was not read");
            problem(StatusCode::BAD_GATEWAY, "the events file could not be read as a plain file")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn names(nodes: &[NodeView]) -> Vec<&str> {
        nodes.iter().map(|node| node.name.as_str()).collect()
    }

    /// Every field of a registry entry that is a secret, or the name of one, and the fields around them.
    const BOTH_SHAPES: &str = r#"{
        "schemaVersion": "0",
        "nodes": [
            {"name": "osaka", "url": "http://127.0.0.1:8711", "verified": true, "allows": ["message", "read"], "via": "ssh://khoa@osaka",
             "addedAt": "2026-09-01T00:00:00Z", "pubkey": "PUBKEY-HEX", "tokenFile": "/home/u/.aoide/osaka.token", "bearerSecret": "BEARER-NAME",
             "hub": true},
            {"name": "yomi", "url": "http://yomi:8710", "verified": true, "autogate": true,
             "grants": {"alpha": ["message"], "beta": ["message", "spawn"]}, "addedAt": "2026-10-01T00:00:00Z"},
            {"name": "bridge", "url": "http://bridge:8710"}
        ]
    }"#;

    #[test]
    fn the_registry_is_the_allowlist_and_a_secret_field_is_not_on_it() {
        let registry = project_registry(BOTH_SHAPES).unwrap();
        let shown = serde_json::to_string(&registry.nodes).unwrap();
        for secret in ["PUBKEY-HEX", "BEARER-NAME", "osaka.token", "pubkey", "tokenFile", "bearerSecret", "hub"] {
            assert!(!shown.contains(secret), "{secret} reached the answer: {shown}");
        }
        let value = serde_json::to_value(&registry.nodes[0]).unwrap();
        let keys: std::collections::BTreeSet<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["addedAt", "allows", "autogate", "grants", "name", "url", "verified", "via"].into());
    }

    #[test]
    fn both_registry_shapes_are_read() {
        let registry = project_registry(BOTH_SHAPES).unwrap();
        assert_eq!(names(&registry.nodes), ["osaka", "yomi", "bridge"]);
        let [osaka, yomi, bridge] = &registry.nodes[..] else { panic!("three nodes") };
        // Before P-CHARTER: a flat list.
        assert_eq!((osaka.verified, osaka.allows.as_slice(), osaka.grants.is_empty()), (true, ["message".to_string(), "read".to_string()].as_slice(), true));
        assert_eq!(osaka.via.as_deref(), Some("ssh://khoa@osaka"));
        // After it: words per mesh.
        assert!(yomi.allows.is_empty());
        assert_eq!(yomi.grants["alpha"], ["message"]);
        assert_eq!(yomi.grants["beta"], ["message", "spawn"]);
        assert!(yomi.autogate && yomi.verified);
        // Neither: an unpaired node with defaults.
        assert_eq!((bridge.verified, bridge.autogate, bridge.via.clone(), bridge.added_at.as_str()), (false, false, None, ""));
        assert_eq!((registry.skipped, registry.unread), (0, 0));
        // The pre-rename top-level key, and a bare list.
        for text in [r#"{"peers": [{"name": "osaka"}]}"#, r#"[{"name": "osaka"}]"#] {
            assert_eq!(names(&project_registry(text).unwrap().nodes), ["osaka"], "{text}");
        }
        // `grants` as a flat list is the same words as `allows`.
        let flat = project_registry(r#"{"nodes": [{"name": "a", "allows": ["read"], "grants": ["read", "message"]}]}"#).unwrap();
        assert_eq!(flat.nodes[0].allows, ["read", "message"]);
    }

    #[test]
    fn what_the_registry_does_not_say_is_counted_and_never_guessed() {
        assert!(project_registry("not json").is_err());
        assert!(project_registry(r#"{"nodes": "osaka"}"#).is_err());
        assert!(project_registry(r#"{"version": 1}"#).is_err());
        let odd = r#"{"nodes": [
            {"name": "Bad Name"}, {"name": 7}, {"url": "x"}, {"name": "ok"},
            {"name": "a", "allows": {"alpha": "message"}},
            {"name": "b", "grants": [1, 2]},
            {"name": "c", "grants": {"Bad Mesh": ["read"]}},
            {"name": "d", "allows": ["Read Me"]},
            {"name": "e", "allows": null}
        ]}"#;
        let registry = project_registry(odd).unwrap();
        assert_eq!(names(&registry.nodes), ["ok", "a", "b", "c", "d", "e"]);
        assert_eq!(registry.skipped, 3, "three entries had no usable name");
        assert_eq!(registry.unread, 4, "four grants fields were in no shape this reads");
        assert!(registry.nodes.iter().all(|node| node.allows.is_empty() && node.grants.is_empty()));
        // Past the cap is counted, not read.
        let many = format!("[{}]", (0..MAX_NODES + 5).map(|n| format!("{{\"name\":\"n{n}\"}}")).collect::<Vec<_>>().join(","));
        let registry = project_registry(&many).unwrap();
        assert_eq!((registry.nodes.len(), registry.skipped), (MAX_NODES, 5));
    }

    #[test]
    fn a_url_loses_what_comes_before_the_at_sign_in_its_authority_and_nothing_else() {
        assert_eq!(without_userinfo("http://user:pass@host:8710/x?a=b@c"), "http://host:8710/x?a=b@c");
        assert_eq!(without_userinfo("ssh://khoa@osaka"), "ssh://osaka");
        assert_eq!(without_userinfo("http://host/a@b"), "http://host/a@b", "an @ in the path is the path's");
        assert_eq!(without_userinfo("http://127.0.0.1:8710"), "http://127.0.0.1:8710");
        assert_eq!(without_userinfo("not a url"), "not a url");
        assert_eq!(without_userinfo("http://a@b@host"), "http://host");
    }

    #[test]
    fn a_capability_word_is_lower_case_and_short() {
        for ok in ["message", "read", "spawn", "a", "x-1", "snake_case"] {
            assert!(word_ok(ok), "{ok}");
        }
        for bad in ["", "Read", "1st", "a b", "a/b", "é", &"w".repeat(WORD_MAX + 1)] {
            assert!(!word_ok(bad), "{bad:?}");
        }
        assert!(words_of(&serde_json::json!(["read", 1])).is_none(), "one bad element spoils the list");
        assert!(words_of(&serde_json::json!(vec!["read"; WORDS_MAX + 1])).is_none());
    }

    fn feed(lines: &[&str]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        std::fs::write(&path, lines.iter().map(|line| format!("{line}\n")).collect::<String>()).unwrap();
        (dir, path)
    }

    fn kinds(tail: &Tail) -> Vec<&str> {
        tail.events.iter().map(|event| event["kind"].as_str().unwrap()).collect()
    }

    #[test]
    fn the_tail_keeps_the_audit_and_gate_classes_and_nothing_else() {
        let (_dir, path) = feed(&[
            r#"{"v":0,"ts":1,"class":"audit","kind":"a1","source":"aoided","payload":{}}"#,
            r#"{"v":0,"ts":2,"class":"secret","kind":"s1","payload":{"secret":"SECRET-NAME"}}"#,
            r#"{"v":0,"ts":3,"class":"gate","kind":"g1","payload":{"node":"osaka"}}"#,
            r#"{"v":0,"ts":4,"class":"notification","kind":"n1","payload":{"body":"forwarded text"}}"#,
            r#"{"v":0,"ts":5,"class":"rice","kind":"r1"}"#,
            r#"{"v":0,"ts":6,"kind":"no-class"}"#,
            r#"{"v":0,"ts":7,"class":["audit"],"kind":"class-is-a-list"}"#,
            "not json at all",
            r#"["audit"]"#,
            r#"{"v":0,"ts":8,"class":"audit","kind":"a2"}"#,
        ]);
        let got = tail(&path, Some(0)).unwrap();
        assert_eq!(kinds(&got), ["a1", "g1", "a2"]);
        assert_eq!(got.next, std::fs::metadata(&path).unwrap().len());
        assert!(!got.reset);
        assert!(!serde_json::to_string(&got).unwrap().contains("SECRET-NAME"));
    }

    #[test]
    fn a_last_line_without_its_newline_waits_for_it() {
        let (_dir, path) = feed(&[r#"{"class":"audit","kind":"first"}"#]);
        let whole = std::fs::metadata(&path).unwrap().len();
        let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        std::io::Write::write_all(&mut file, br#"{"class":"gate","kind":"sec"#).unwrap();
        let got = tail(&path, Some(0)).unwrap();
        assert_eq!((kinds(&got), got.next), (vec!["first"], whole), "the half line is not read, and the cursor stays before it");
        std::io::Write::write_all(&mut file, b"ond\"}\n").unwrap();
        let got = tail(&path, Some(whole)).unwrap();
        assert_eq!(kinds(&got), ["second"]);
        assert_eq!(got.next, std::fs::metadata(&path).unwrap().len());
        let again = tail(&path, Some(got.next)).unwrap();
        assert_eq!((again.events.len(), again.next, again.reset), (0, got.next, false));
    }

    #[test]
    fn a_file_that_shrank_is_read_again_from_the_start() {
        let (_dir, path) = feed(&[r#"{"class":"audit","kind":"old-1"}"#, r#"{"class":"audit","kind":"old-2"}"#]);
        let before = tail(&path, Some(0)).unwrap();
        // aoided truncates in place at its cap and goes on writing: the file is shorter than the cursor.
        std::fs::write(&path, "{\"class\":\"gate\",\"kind\":\"new\"}\n").unwrap();
        let got = tail(&path, Some(before.next)).unwrap();
        assert!(got.reset);
        assert_eq!(kinds(&got), ["new"]);
        assert_eq!(got.next, std::fs::metadata(&path).unwrap().len());
        // A file that is gone after a cursor is the same fact; with no cursor there is nothing to reset.
        std::fs::remove_file(&path).unwrap();
        assert_eq!(tail(&path, Some(5)).unwrap(), Tail { events: vec![], next: 0, reset: true });
        assert_eq!(tail(&path, Some(0)).unwrap(), Tail { events: vec![], next: 0, reset: false });
        assert_eq!(tail(&path, None).unwrap(), Tail { events: vec![], next: 0, reset: false });
    }

    #[test]
    fn a_call_reads_at_most_a_window_and_the_calls_together_lose_nothing() {
        let line = |n: usize| format!("{{\"class\":\"audit\",\"kind\":\"k{n:05}\",\"payload\":\"{}\"}}", "x".repeat(200));
        let all: Vec<String> = (0..1000).map(line).collect();
        let (_dir, path) = feed(&all.iter().map(String::as_str).collect::<Vec<_>>());
        let size = std::fs::metadata(&path).unwrap().len();
        assert!(size > 3 * EVENTS_WINDOW);
        let (mut after, mut seen, mut calls) = (0, Vec::new(), 0);
        while after < size {
            let got = tail(&path, Some(after)).unwrap();
            assert!(got.next - after <= EVENTS_WINDOW, "a call read {} bytes", got.next - after);
            assert!(got.next > after, "every call moves on");
            seen.extend(got.events.iter().map(|event| event["kind"].as_str().unwrap().to_string()));
            (after, calls) = (got.next, calls + 1);
        }
        assert!(calls >= 4);
        assert_eq!(seen, (0..1000).map(|n| format!("k{n:05}")).collect::<Vec<_>>(), "in order, none twice, none lost");
    }

    #[test]
    fn the_first_poll_is_the_newest_window_from_a_whole_line() {
        let line = |n: usize| format!("{{\"class\":\"gate\",\"kind\":\"k{n:05}\",\"payload\":\"{}\"}}", "y".repeat(200));
        let all: Vec<String> = (0..1000).map(line).collect();
        let (_dir, path) = feed(&all.iter().map(String::as_str).collect::<Vec<_>>());
        let size = std::fs::metadata(&path).unwrap().len();
        let got = tail(&path, None).unwrap();
        assert_eq!(got.next, size);
        assert_eq!(got.events.last().unwrap()["kind"], "k00999");
        let first = got.events[0]["kind"].as_str().unwrap().to_string();
        assert!(first != "k00000", "the first poll is a window, not the file");
        let count = got.events.len();
        assert!(count > 100 && count < 1000);
        let n: usize = first[1..].parse().unwrap();
        assert_eq!(count, 1000 - n, "a run of lines with none missing in the middle");
        // A small file is read whole.
        let (_dir, path) = feed(&[r#"{"class":"audit","kind":"only"}"#]);
        assert_eq!(kinds(&tail(&path, None).unwrap()), ["only"]);
    }

    #[test]
    fn a_line_longer_than_the_window_is_passed_over() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let mut text = format!("{{\"class\":\"audit\",\"kind\":\"{}\"}}", "z".repeat(EVENTS_WINDOW as usize + 10));
        text.push('\n');
        text.push_str("{\"class\":\"audit\",\"kind\":\"after\"}\n");
        std::fs::write(&path, &text).unwrap();
        let first = tail(&path, Some(0)).unwrap();
        assert!(first.events.is_empty());
        assert_eq!(first.next, EVENTS_WINDOW, "the cursor moves on so the poll is not stuck behind it");
        let second = tail(&path, Some(first.next)).unwrap();
        assert_eq!(kinds(&second), ["after"], "the rest of the long line is a fragment and is not an event");
    }

    #[test]
    fn a_feed_that_is_a_link_or_a_directory_is_not_read() {
        let (dir, path) = feed(&[r#"{"class":"audit","kind":"real"}"#]);
        let link = dir.path().join("link.jsonl");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(tail(&link, Some(0)).is_err(), "O_NOFOLLOW: a link is not followed");
        assert!(tail(dir.path(), Some(0)).is_err(), "a directory is not a feed");
    }

    #[test]
    fn the_events_query_is_nothing_or_one_offset() {
        assert_eq!(after_asked(""), Some(None));
        assert_eq!(after_asked("after=0"), Some(Some(0)));
        assert_eq!(after_asked("after=1234567"), Some(Some(1_234_567)));
        for bad in ["after=", "after=-1", "after=1.5", "after=1e3", "after=%31", "after= 1", "after=1&after=2", "after=1&x=1", "x=1", "after", "AFTER=1", "after=99999999999999999999999"] {
            assert_eq!(after_asked(bad), None, "{bad:?}");
        }
    }

    fn cache_dir(files: &[(&str, &[u8])]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("node-cache");
        std::fs::create_dir(&cache).unwrap();
        for (name, bytes) in files {
            std::fs::write(cache.join(name), bytes).unwrap();
        }
        (dir, cache)
    }

    #[test]
    fn the_cache_is_text_by_name_with_an_age_and_is_bounded() {
        let hostile = "<script>alert(1)</script>\u{1b}[31m\u{1b}]0;title\u{7}";
        let big = vec![b'b'; CACHE_CAP + 100];
        let (dir, cache) = cache_dir(&[("yomi.json", hostile.as_bytes()), ("osaka.json", b"{\"graph\":{}}"), ("big.json", &big), ("notes.txt", b"no"), (".json", b"no")]);
        std::os::unix::fs::symlink(dir.path().join("node-cache/osaka.json"), cache.join("link.json")).unwrap();
        std::fs::create_dir(cache.join("dir.json")).unwrap();
        let old = SystemTime::now() - Duration::from_secs(300);
        std::fs::File::options().write(true).open(cache.join("osaka.json")).unwrap().set_modified(old).unwrap();
        let (views, more) = read_cache(&cache);
        assert!(!more);
        assert_eq!(views.iter().map(|view| view.name.as_str()).collect::<Vec<_>>(), ["big", "osaka", "yomi"], "regular .json files, by name");
        assert_eq!((views[0].text.len(), views[0].cut), (CACHE_CAP, true));
        assert_eq!((views[1].text.as_str(), views[1].cut), ("{\"graph\":{}}", false));
        assert!((300..310).contains(&views[1].age_secs), "{}", views[1].age_secs);
        assert_eq!(views[2].text, hostile, "text is the file's, untouched");
        assert!(views[2].age_secs < 60);
        // And as JSON it is a string: the escapes are the serializer's.
        let shown = serde_json::to_string(&views[2]).unwrap();
        assert!(!shown.contains('\u{1b}'), "{shown}");
        assert_eq!(serde_json::from_str::<Value>(&shown).unwrap()["text"], hostile);
    }

    #[test]
    fn more_than_sixty_four_cache_files_are_cut_and_said() {
        let files: Vec<(String, &[u8])> = (0..70).map(|n| (format!("n{n:02}.json"), b"{}".as_slice())).collect();
        let (_dir, cache) = cache_dir(&files.iter().map(|(name, bytes)| (name.as_str(), *bytes)).collect::<Vec<_>>());
        let (views, more) = read_cache(&cache);
        assert!(more);
        assert_eq!((views.len(), views[0].name.as_str(), views[63].name.as_str()), (CACHE_FILES, "n00", "n63"));
        assert_eq!(read_cache(&cache.join("absent")), (Vec::new(), false));
    }

    #[test]
    fn the_graph_is_counted_and_listed_without_what_the_agents_said() {
        let graph = serde_json::json!({
            "schemaVersion": "0",
            "nodes": [
                {"id": "project:p", "kind": "project", "name": "p", "path": "/p", "roots": ["/p"]},
                {"id": "session:s1", "kind": "session", "agent": "claude", "cwd": "/p", "state": "running", "startedAt": "t", "title": "a title", "petname": "brisk-fox",
                 "say": "SECRET WORDS", "activity": "SECRET COMMAND", "socket": "/run/s.sock", "pid": 7, "tool": "Bash", "windowAddress": "0xabc"},
                {"id": "node:osaka", "kind": "node", "name": "osaka", "url": "http://osaka:8710", "state": "fresh", "fetchedAt": "t2",
                 "children": {"nodes": [{"id": "session:remote", "say": "REMOTE WORDS"}, {"id": "x"}], "edges": []}},
                {"kind": 7}
            ],
            "edges": [{"from": "a", "to": "b", "kind": "spawned"}, {"from": "c", "to": "d", "kind": "anchors"}]
        });
        let view = project_graph(&graph.to_string()).unwrap();
        assert_eq!((view.total, view.edges), (4, 2));
        assert_eq!(view.kinds.iter().map(|(kind, n)| (kind.as_str(), *n)).collect::<Vec<_>>(), [("", 1), ("node", 1), ("project", 1), ("session", 1)]);
        let shown = serde_json::to_string(&view).unwrap();
        for hidden in ["SECRET", "REMOTE", "/run/s.sock", "windowAddress", "\"pid\"", "\"tool\"", "http://osaka", "\"path\""] {
            assert!(!shown.contains(hidden), "{hidden} reached the answer: {shown}");
        }
        assert_eq!(view.nodes[1]["petname"], "brisk-fox");
        assert_eq!(view.nodes[2]["children"], 2);
        assert_eq!(view.nodes[2]["fetchedAt"], "t2");
        assert!(project_graph("[]").is_err() && project_graph("{").is_err());
        // The list is cut and the counts are not.
        let many = serde_json::json!({"nodes": (0..GRAPH_NODES + 20).map(|n| serde_json::json!({"id": format!("s{n}"), "kind": "session"})).collect::<Vec<_>>()});
        let view = project_graph(&many.to_string()).unwrap();
        assert_eq!((view.nodes.len(), view.total, view.kinds["session"]), (GRAPH_NODES, GRAPH_NODES + 20, GRAPH_NODES + 20));
    }

    #[test]
    fn a_node_has_the_meshes_that_have_a_row_for_it() {
        let data = serde_json::json!({"report": {
            "sections": [
                {"name": "alpha", "rows": [{"node": "osaka", "class": "unverified"}, {"node": "yomi", "class": "via-mismatch"}, {"node": "osaka", "class": "again"}]},
                {"name": "beta", "rows": [{"node": "osaka", "class": "unverified"}]},
                {"name": "Bad Name", "rows": [{"node": "bridge"}]},
                {"name": "gamma", "rows": "not a list"},
                {"rows": [{"node": "bridge"}]},
                {"name": "delta", "rows": [{"class": "no node"}, {"node": 7}]}
            ],
            "undeclared": ["multi", "bridge"]
        }});
        let by_node = meshes_by_node(&data);
        assert_eq!(by_node["osaka"], ["alpha", "beta"]);
        assert_eq!(by_node["yomi"], ["alpha"]);
        assert_eq!(by_node.len(), 2, "{by_node:?}");
        assert!(meshes_by_node(&serde_json::json!({})).is_empty());
        assert!(meshes_by_node(&serde_json::json!({"report": {"sections": 7}})).is_empty());
        let registry = project_registry(r#"[{"name":"osaka","verified":true},{"name":"yomi"},{"name":"bridge","verified":true}]"#).unwrap();
        let infos = node_infos(&registry.nodes, Some(&data));
        let shown: Vec<(&str, bool, Vec<&str>)> = infos.iter().map(|n| (n.name.as_str(), n.verified, n.meshes.iter().map(String::as_str).collect())).collect();
        assert_eq!(shown, [("osaka", true, vec!["alpha", "beta"]), ("yomi", false, vec!["alpha"]), ("bridge", true, vec![])]);
        assert!(node_infos(&registry.nodes, None).iter().all(|n| n.meshes.is_empty()), "no report, no meshes, the same nodes");
    }

    #[tokio::test]
    async fn an_answer_is_kept_for_the_ttl_and_a_failure_is_not_kept() {
        let cached = Cached::<u8>::default();
        // The future is only polled when the cache runs it, so the count is of runs and not of futures built.
        let polls = AtomicUsize::new(0);
        let counted = |value: u8| {
            let polls = &polls;
            async move {
                polls.fetch_add(1, Ordering::SeqCst);
                Ok::<u8, &str>(value)
            }
        };
        let hour = Duration::from_secs(3600);
        assert_eq!(*cached.get(hour, counted(1)).await.unwrap(), 1);
        assert_eq!(*cached.get(hour, counted(2)).await.unwrap(), 1, "kept: the second run's answer is not used");
        assert_eq!(polls.load(Ordering::SeqCst), 1, "within the ttl nothing runs");
        assert_eq!(*cached.get(Duration::ZERO, counted(3)).await.unwrap(), 3);
        assert_eq!(polls.load(Ordering::SeqCst), 2, "past the ttl it runs");
        assert_eq!(*cached.get(hour, counted(4)).await.unwrap(), 3, "and what it gave is kept");
        // A failure is the caller's to see, is not kept, and leaves the last good answer where it was.
        let failing = cached.get(Duration::ZERO, async { Err::<u8, &str>("no") }).await;
        assert_eq!(failing.unwrap_err(), "no");
        assert_eq!(*cached.get(hour, counted(5)).await.unwrap(), 3);
        assert_eq!(polls.load(Ordering::SeqCst), 2);
        let fresh = Cached::<u8>::default();
        assert_eq!(fresh.get(hour, async { Err::<u8, &str>("no") }).await.unwrap_err(), "no");
        assert_eq!(*fresh.get(hour, counted(6)).await.unwrap(), 6, "nothing was kept from the failure");
    }

    #[test]
    fn a_path_under_the_home_is_shown_with_a_tilde() {
        let home = Some(Path::new("/home/u"));
        assert_eq!(tilde_in(home, Path::new("/home/u/.local/bin/aoide")), "~/.local/bin/aoide");
        assert_eq!(tilde_in(home, Path::new("/home/u")), "~");
        assert_eq!(tilde_in(home, Path::new("/home/user/x")), "/home/user/x", "a sibling that shares the prefix is not under it");
        assert_eq!(tilde_in(home, Path::new("/run/user/1000/aoide/aoided.sock")), "/run/user/1000/aoide/aoided.sock");
        assert_eq!(tilde_in(None, Path::new("/home/u/x")), "/home/u/x");
        assert_eq!(tilde_in(Some(Path::new("/")), Path::new("/etc/x")), "/etc/x", "a home of / abbreviates nothing");
    }

    /// Options as `main` builds them with no Aoide flag at all.
    fn unflagged() -> Options {
        Options {
            bind: "127.0.0.1:0".parse().unwrap(),
            ui_dir: None,
            roots: Vec::new(),
            eidolon: PathBuf::from("/nonexistent/eidolon"),
            sessions_dir: PathBuf::from("/nonexistent/sessions"),
            eidolon_config: None,
            scratch: None,
            other_sessions: None,
            modes: Default::default(),
            code_ttl: Duration::from_secs(60),
            plugins_dir: None,
            run_graphs: Vec::new(),
            jev_health: None,
            browser_health: None,
            aoide: None,
            aoide_home: None,
            aoide_daemon_socket: None,
            aoide_events: None,
            minerva: None,
            term_token_file: None,
            mail_token_file: None,
            agent_url_file: None,
            knobs: crate::Knobs::default(),
        }
    }

    /// A unit test that forgot to name an Aoide folder gets one that is not there, never the operator's `~/.aoide` or the runtime
    /// dir's aoided socket: no lib test can read real state or ping a real daemon by leaving a flag out.
    #[test]
    fn a_unit_test_with_no_aoide_flags_resolves_to_nothing_real() {
        let opts = unflagged();
        let (home, socket) = (home(&opts).unwrap(), daemon_socket(&opts).unwrap());
        assert_eq!(home, Path::new("/nonexistent-minerva-test/aoide"));
        assert_eq!(socket, Path::new("/nonexistent-minerva-test/aoide/aoided.sock"));
        assert_eq!(state_dir(&opts).unwrap(), Path::new("/nonexistent-minerva-test/aoide/state"));
        assert!(!home.exists() && !socket.exists(), "the stand-in paths must not be there");
        // Not under the real home or the real runtime dir, whatever this process's environment is.
        for (name, path) in [("HOME", &home), ("XDG_RUNTIME_DIR", &socket)] {
            if let Some(real) = std::env::var_os(name).map(PathBuf::from).filter(|real| real.parent().is_some()) {
                assert!(!path.starts_with(&real), "{path:?} is under ${name}");
            }
        }
        assert_eq!(events_path(&opts), None, "the events feed has no default at all");
        // A flag still wins, in this build too.
        let flagged = Options {
            aoide_home: Some(PathBuf::from("/given/aoide")),
            aoide_daemon_socket: Some(PathBuf::from("/given/aoided.sock")),
            ..opts
        };
        assert_eq!(super::home(&flagged).unwrap(), Path::new("/given/aoide"));
        assert_eq!(daemon_socket(&flagged).unwrap(), Path::new("/given/aoided.sock"));
    }
}
