//! `/hub/mesh`: the mesh view (slices R2b and R2c). Network-free: it reads Aoide's own files, asks the `aoide` CLI for `mesh --json` and
//! `mail outbox --json`, pings the daemon once, and never runs anything that touches a node.
//!
//! ```text
//!   GET /hub/mesh                                  the snapshot: available, aoide, daemon, registry, cache, graph, mesh, outbox, errors
//!   GET /hub/mesh/events[?audit=N][&daemon=M]      the two streams since byte N of the audit log and byte M of the daemon feed
//! ```
//!
//! **The two streams** are two named files, each read from its own cursor (see [`events`]): the audit log (`--aoide-audit-log`, else
//! `<aoide-home>/log`, aoide's own default) and aoided's events feed (`--aoide-daemon-events`, else `$AOIDE_DAEMON_EVENTS` when absolute, else
//! `$XDG_RUNTIME_DIR/aoide/events.jsonl`). Their records are merged by `ts`, and each carries the stream it came from. The runner gives aoide
//! `AOIDE_AUDIT_LOG` only for the explicit flag and never gives it the daemon's feed, and the start refuses two paths that are one file
//! ([`feeds_apart`]). The perch never opens either file for writing. But each call the perch makes is one aoide runs, and aoide appends its own
//! audit records for the commands it audits (the audit log is the flag's file, else aoide's default under `<aoide-home>`): so the audit log does
//! change while the perch runs. The snapshot's three calls are cached for 60 s ([`TTL`]), so any number of tabs cost at most one set a minute;
//! mail's calls (send, read, outbox) run once per request; the events poll runs no aoide call at all.
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
use std::os::unix::fs::MetadataExt as _;
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
/// are not the mesh's. Both streams keep to these.
const CLASSES: [&str; 2] = ["audit", "gate"];
/// The audit log's `door`, `status` and `command` values, through [`audit_keep`]. The docs name `cli`, `mcp`, `a2a` and `daemon` for a door;
/// `started` (aoided's start record, and `adapter.melete started`, observed in the real aoide 0.0.26 log), `ok`, `error`, `pending`,
/// `delivered` and `proposed` for a status; and a dotted command word (`mail.send`), at most [`COMMAND_MAX`].
const AUDIT_DOORS: [&str; 4] = ["cli", "mcp", "a2a", "daemon"];
const AUDIT_STATUS: [&str; 6] = ["started", "ok", "error", "pending", "delivered", "proposed"];
const COMMAND_MAX: usize = 64;
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
    /// What the events polls last saw of each feed file, by path (see [`tail`]): the next poll compares against it.
    feeds: Mutex<BTreeMap<PathBuf, Seen>>,
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
    home_of(opts.aoide_home.as_deref())
}

/// [`home`] for a flag that is not in an `Options` yet: `main.rs` checks the two feeds before it builds one.
pub fn home_of(flag: Option<&Path>) -> Option<PathBuf> {
    flag.map(Path::to_path_buf).or_else(default_home).filter(|home| home.is_absolute())
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
    crate::aoide::runtime_dir().filter(|dir| !dir.as_os_str().is_empty()).map(|dir| dir.join("aoide").join("aoided.sock"))
}

#[cfg(test)]
fn default_socket() -> Option<PathBuf> {
    Some(PathBuf::from(TEST_DEFAULT).join("aoided.sock"))
}

/// The audit log: `--aoide-audit-log`, else `<aoide-home>/log`. Aoide's own rule for the file it appends to is `$AOIDE_AUDIT_LOG`, else
/// `$AOIDE_ROOT/log` (when `$AOIDE_ROOT` is absolute), else `<home>/.aoide/log` (`docs/Aoide-Wiki/concepts/cli/Doors-and-Nodes.md`, and
/// `CLI-Reference.md`). The runner removes every ambient `AOIDE_*` from its calls and gives `AOIDE_ROOT` = `--aoide-home`, so under the perch
/// the `$AOIDE_AUDIT_LOG` step is the flag alone, and the file aoide appends to is the one this names. Absolute only: a relative flag names no
/// file, and `main.rs` refuses one at the start. The flag is read through [`crate::aoide::audit_flag`], the one reader the runner shares.
/// CONTRACTS.md gives no precedence for this path (its line 985 is a migration note); the order above is the wiki's, cited in the README.
pub fn audit_path(opts: &Options) -> Option<PathBuf> {
    audit_file(crate::aoide::audit_flag(opts).as_deref(), home(opts))
}

/// The audit log from its flag and aoide's home: the flag when it is given (absolute, or none), else `<home>/log`.
pub fn audit_file(flag: Option<&Path>, home: Option<PathBuf>) -> Option<PathBuf> {
    match flag {
        Some(path) => path.is_absolute().then(|| path.to_path_buf()),
        None => home.map(|home| home.join("log")),
    }
}

/// aoided's events feed: `--aoide-daemon-events`, else `$AOIDE_DAEMON_EVENTS` when absolute, else `$XDG_RUNTIME_DIR/aoide/events.jsonl`.
/// Absolute only. The XDG path is the perch's own choice (it mirrors [`default_socket`]); aoide's rule is `$AOIDE_DAEMON_EVENTS`, else a
/// sibling of the daemon socket, which is the same file only while the socket is the default one, so a `--aoide-daemon-socket` elsewhere
/// does not move this path. Nothing the runner runs is ever given this path.
pub fn daemon_path(opts: &Options) -> Option<PathBuf> {
    daemon_file(opts.aoide_daemon_events.as_deref())
}

/// The daemon's feed from its flag, or the default when there is none. Absolute or nothing, the flag included.
pub fn daemon_file(flag: Option<&Path>) -> Option<PathBuf> {
    match flag {
        Some(path) => path.is_absolute().then(|| path.to_path_buf()),
        None => default_daemon_path(),
    }
}

/// The default feed from the two variables the perch reads: `$AOIDE_DAEMON_EVENTS` when it is absolute, else `$XDG_RUNTIME_DIR/aoide/events.jsonl`
/// when the runtime dir is absolute. A relative value of either, and an empty one, names no file, so a path under the working directory is never
/// opened. The two readers are [`crate::aoide::runtime_dir`] and this function.
fn daemon_default(env_daemon: Option<PathBuf>, runtime_dir: Option<PathBuf>) -> Option<PathBuf> {
    env_daemon
        .filter(|path| path.is_absolute())
        .or_else(|| runtime_dir.filter(|dir| dir.is_absolute()).map(|dir| dir.join("aoide").join("events.jsonl")))
}

#[cfg(not(test))]
fn default_daemon_path() -> Option<PathBuf> {
    daemon_default(std::env::var_os("AOIDE_DAEMON_EVENTS").map(PathBuf::from), crate::aoide::runtime_dir())
}

/// The start's check that the two feeds are two files. Both are resolved as the view resolves them (the flags, else the defaults). When they are
/// one file, aoide's audit records would be appended into the feed aoided writes, so the start is refused and the sentence says how to fix it.
/// `main.rs` calls it before anything under `--home` is made. Only the paths are compared (and the device and inode, when both files are there);
/// no file is opened.
pub fn feeds_apart(aoide_home: Option<&Path>, audit_flag: Option<&Path>, daemon_flag: Option<&Path>) -> Result<(), String> {
    let (Some(audit), Some(daemon)) = (audit_file(audit_flag, home_of(aoide_home)), daemon_file(daemon_flag)) else { return Ok(()) };
    if same_file(&audit, &daemon)? {
        return Err(format!(
            "the audit log and aoided's events feed are the same file ({}): aoide would append its audit records into the feed aoided writes; \
             give one of them another path (--aoide-audit-log, --aoide-daemon-events, or AOIDE_DAEMON_EVENTS)",
            audit.display()
        ));
    }
    Ok(())
}

/// Whether two paths name one file: the same path once every link on the way is followed ([`resolved`]), or the same device and inode when both
/// exist (a hard link). A chain of links too long to follow is an error, and the start refuses on it.
fn same_file(a: &Path, b: &Path) -> Result<bool, String> {
    if resolved(a)? == resolved(b)? {
        return Ok(true);
    }
    Ok(match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => (a.dev(), a.ino()) == (b.dev(), b.ino()),
        _ => false,
    })
}

/// How many links a path may pass through before the start refuses it: a longer chain, or a loop, names no file the perch can tell apart.
const LINK_HOPS: usize = 40;

/// A path with its links followed, one at a time. A link that dangles is followed to the path it names, so a link to a feed that aoided has not
/// written yet is that path, and a relative target is relative to the link's own folder. Past [`LINK_HOPS`] links it is an error. The file at the
/// end is canonicalized, or, when it is not there, its folder is (which must be) and the name is joined to it.
fn resolved(path: &Path) -> Result<PathBuf, String> {
    let mut at = path.to_path_buf();
    let mut hops = 0;
    while std::fs::symlink_metadata(&at).is_ok_and(|meta| meta.is_symlink()) {
        if hops == LINK_HOPS {
            return Err(format!("{} is a chain of more than {LINK_HOPS} links, or a loop: the perch cannot tell which file it names", path.display()));
        }
        hops += 1;
        let target = std::fs::read_link(&at).map_err(|e| format!("{} is a link that cannot be read: {e}", at.display()))?;
        at = match at.parent() {
            Some(dir) if target.is_relative() => dir.join(&target),
            _ => target,
        };
    }
    Ok(match std::fs::canonicalize(&at) {
        Ok(real) => real,
        Err(_) => match (at.parent().and_then(|dir| std::fs::canonicalize(dir).ok()), at.file_name()) {
            (Some(dir), Some(name)) => dir.join(name),
            _ => at,
        },
    })
}

#[cfg(test)]
fn default_daemon_path() -> Option<PathBuf> {
    Some(PathBuf::from(TEST_DEFAULT).join("events.jsonl"))
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
    /// The url without its `user:pass@`, or absent: none in the file, or one that still has an `@` after the cut ([`without_userinfo`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
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

/// `url` without the `user:pass@` in front of its host, or `None` when an `@` is still in it after the cut. The cut is at the authority's last `@`
/// (the text after `://`, up to the first `/`, `?` or `#`). Fail closed: a value with no `://`, and one whose password has a `/` in it (which ends
/// the authority before the `@`), still has an `@` after the cut, so it is none. Nothing says which part before an `@` is a credential.
fn without_userinfo(url: &str) -> Option<String> {
    let cut = match url.split_once("://") {
        Some((scheme, rest)) => {
            let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
            let (authority, tail) = rest.split_at(end);
            match authority.rfind('@') {
                Some(at) => format!("{scheme}://{}{tail}", &authority[at + 1..]),
                None => url.to_string(),
            }
        }
        None => url.to_string(),
    };
    (!cut.contains('@')).then_some(cut)
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
            // The cut is made on the whole value and the clip after it, so a `@` past the 300th character is still cut away with its credential.
            url: entry.get("url").and_then(Value::as_str).and_then(without_userinfo).map(|url| url.chars().take(300).collect()),
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
///   registry   [{name, url?, verified, autogate, addedAt, via, allows, grants}]    the allowlist, in the file's order; `url` absent when none
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

/// Each stream's cursor, `None` for the first poll of that stream (which reads the newest window of it).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Cursors {
    audit: Option<u64>,
    daemon: Option<u64>,
}

/// `audit=<digits>` and `daemon=<digits>`, each at most once, in either order, and nothing else. No query at all is the first poll of both.
/// `None` is the 400: an unknown key (`after=` included), a repeat, a value that is not digits, or an empty pair.
fn cursors_asked(query: &str) -> Option<Cursors> {
    let mut cursors = Cursors::default();
    if query.is_empty() {
        return Some(cursors);
    }
    for pair in query.split('&') {
        let (key, text) = pair.split_once('=')?;
        let digits = !text.is_empty() && text.len() <= 20 && text.bytes().all(|b| b.is_ascii_digit());
        let offset: u64 = digits.then(|| text.parse().ok()).flatten()?;
        match key {
            "audit" if cursors.audit.is_none() => cursors.audit = Some(offset),
            "daemon" if cursors.daemon.is_none() => cursors.daemon = Some(offset),
            _ => return None,
        }
    }
    Some(cursors)
}

/// One record of a feed, or not. The daemon's feed keeps the fields [`feed_keep`] names; the audit log is an allowlist ([`audit_keep`]).
type Keep = fn(&Value) -> Option<Value>;

/// What a poll found of a feed file: its device and inode, and its length. The next poll compares against it, so a file that was replaced, or
/// cut shorter, is seen as a new feed even when its cursor is still inside the file.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Seen {
    dev: u64,
    ino: u64,
    len: u64,
}

/// What one poll of a feed found.
#[derive(Debug, PartialEq)]
struct Tail {
    /// The records that pass the stream's [`Keep`], oldest first. A line that is not an object is left out.
    events: Vec<Value>,
    /// Where the next poll of this stream starts: the end of the last whole line read.
    next: u64,
    /// The cursor was past the end of the file, or the file was not the one the last poll saw, or it was shorter than then: so this read
    /// started again at 0.
    reset: bool,
    /// The file as this poll found it. `None` when there is no file, so the last poll's identity stays the one to compare with.
    seen: Option<Seen>,
}

/// The answer's three parts for each stream: a cursor, a flag, or a path.
#[derive(Debug, PartialEq, Serialize)]
struct Streams<T> {
    audit: T,
    daemon: T,
}

/// One record of the answer and the stream it came from. `event` is the record as the stream keeps it, and is never altered.
#[derive(Debug, PartialEq, Serialize)]
struct Entry {
    stream: &'static str,
    event: Value,
}

/// `GET /hub/mesh/events`: the merged records, the two next cursors, the two resets, the two errors, and the two files as the page shows them.
#[derive(Debug, PartialEq, Serialize)]
struct Answer {
    events: Vec<Entry>,
    next: Streams<u64>,
    reset: Streams<bool>,
    /// A stream that could not be read (a link, a FIFO, a directory, an I/O error): the sentence for it. That stream has no events and keeps its
    /// cursor; the other stream answers as usual.
    error: Streams<Option<String>>,
    files: Streams<Option<String>>,
}

/// The fields of the daemon's feed that aoide's docs name, and so the only ones the page is ever given. At the top level: `v`, `kind` and
/// `source` (with `ts` and `class`, which are always kept). The payload of a `gate` record: `id`, `name`, `originAddr`, `url` and `direction`
/// (Aoide's CONTRACTS.md, at its repository root, "Pairing events feed (P-P5)": "payload carries fields BY NAME ONLY"). The payload of an
/// `audit` record:
/// `file` (docs/architecture/AOIDED.md, "The first producer: the hand-edit watcher"). Anything else, at either level, is dropped, so a field a
/// later aoided adds never reaches the page. A `url` is kept without its `user:pass@` ([`scrubbed`]).
const FEED_TOP: [&str; 3] = ["v", "kind", "source"];
const GATE_PAYLOAD: [&str; 5] = ["id", "name", "originAddr", "url", "direction"];
const AUDIT_PAYLOAD: [&str; 1] = ["file"];

/// The daemon's feed: a record of class `audit` or `gate` with an integer `ts`, through the fields above. `secret` names secrets and consumers,
/// and `notification` carries forwarded text: neither is ever forwarded. A record with no `ts` cannot be merged by time, and is dropped.
fn feed_keep(record: &Value) -> Option<Value> {
    let object = record.as_object()?;
    let class = object.get("class")?.as_str().filter(|class| CLASSES.contains(class))?;
    let ts = object.get("ts")?.as_u64()?;
    let mut kept = serde_json::Map::new();
    kept.insert("ts".into(), ts.into());
    kept.insert("class".into(), class.into());
    for key in FEED_TOP {
        if let Some(value) = object.get(key) {
            kept.insert(key.into(), value.clone());
        }
    }
    if let Some(payload) = object.get("payload").and_then(Value::as_object) {
        let names: &[&str] = if class == "gate" { &GATE_PAYLOAD } else { &AUDIT_PAYLOAD };
        let shown: serde_json::Map<String, Value> = names
            .iter()
            .filter_map(|name| payload.get(*name).and_then(|value| scrubbed(name, value)).map(|value| (name.to_string(), value)))
            .collect();
        kept.insert("payload".into(), Value::Object(shown));
    }
    Some(Value::Object(kept))
}

/// A payload field as the answer is given it, or none. A `url` is a string taken out of its `user:pass@` the way the registry's is
/// ([`without_userinfo`]), so a gate's credentials never reach the page. A `url` that is not a string, or that still has an `@`, is none, and the
/// field is left out of the record. Every other field is as the record has it.
fn scrubbed(name: &str, value: &Value) -> Option<Value> {
    match name {
        "url" => value.as_str().and_then(without_userinfo).map(Value::String),
        _ => Some(value.clone()),
    }
}

/// The audit log, through an allowlist, and failing closed. The docs (CONTRACTS.md and the Aoide wiki's `concepts/cli/Doors-and-Nodes.md`,
/// `entities/aoided.md`, `concepts/cli/CLI-Reference.md`) name an audit record's `class`, `door`, `command`, `status`, `message` and
/// `untrusted_data`. They do not name the type or unit of `ts`; the real aoide 0.0.26 log writes it as an integer (observed), and that is
/// all this reads. So:
///
/// - a record with no non-negative integer `ts` is dropped: it cannot be merged by time;
/// - a record whose `class` is not `audit` or `gate`, or whose `door`, `command` or `status` is present and not one of the words above, is dropped;
/// - what is kept is `ts`, `class`, `door`, `command` and `status`, and a `message` only for aoided's start records ([`start_record`]: `daemon` and
///   `adapter.melete` with status `started`, Doors-and-Nodes.md lines 91 and 213). `untrusted_data` is another's text and is never forwarded.
///   Every other record's `message` is left out, because it is text the docs do not promise to be free of a secret or a letter: `mail.read` writes
///   the letter's body into it (observed, cut at about 526 characters), and the secrets broker's own audit lines go to this same log, and they name
///   what they audit and never a value (Secrets-Broker.md lines 46-48 and 122). A record with no command or status, or a `message` that is not a
///   string, keeps no `message`.
fn audit_keep(record: &Value) -> Option<Value> {
    let object = record.as_object()?;
    let ts = object.get("ts")?.as_u64()?;
    let class = object.get("class")?.as_str().filter(|class| CLASSES.contains(class))?;
    let mut kept = serde_json::Map::new();
    kept.insert("ts".into(), ts.into());
    kept.insert("class".into(), class.into());
    if let Some(command) = object.get("command") {
        kept.insert("command".into(), command.as_str().filter(|command| command_ok(command))?.into());
    }
    if let Some(door) = object.get("door") {
        kept.insert("door".into(), door.as_str().filter(|door| AUDIT_DOORS.contains(door))?.into());
    }
    if let Some(status) = object.get("status") {
        kept.insert("status".into(), status.as_str().filter(|status| AUDIT_STATUS.contains(status))?.into());
    }
    let started = (kept.get("command").and_then(Value::as_str), kept.get("status").and_then(Value::as_str));
    let shown = object
        .get("message")
        .and_then(Value::as_str)
        .filter(|_| matches!(started, (Some(command), Some(status)) if start_record(command, status)));
    if let Some(message) = shown {
        kept.insert("message".into(), message.into());
    }
    Some(Value::Object(kept))
}

/// Whether a record is one of aoided's start records, the only ones whose `message` is kept: `daemon` (`daemon started`) and `adapter.melete`
/// (`adapter.melete started`), with status `started`. The command is matched without regard to case, so the rule does not depend on how it is spelt.
fn start_record(command: &str, status: &str) -> bool {
    status == "started" && ["daemon", "adapter.melete"].iter().any(|start| command.eq_ignore_ascii_case(start))
}

/// A dotted command word as the audit log writes one: a letter, then letters, digits, `.`, `/` or `-`, at most [`COMMAND_MAX`]. No space,
/// so a word with an argument in it is not one.
fn command_ok(command: &str) -> bool {
    command.len() <= COMMAND_MAX
        && command.starts_with(|c: char| c.is_ascii_alphabetic())
        && command.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '/' | '-'))
}

/// One poll of the feed at `path`, through `keep`. `after` is the byte offset of the last poll's `next`; `None` is the first poll, which
/// starts at the last 64 KiB (from the first whole line in it). At most [`EVENTS_WINDOW`] bytes of the file are read, and only whole lines are
/// used: a last line without its newline is read again next time, once it has one. A window with no newline at all is one line too long to
/// be an event, and is passed over.
fn tail(path: &Path, after: Option<u64>, keep: Keep, last: Option<Seen>) -> std::io::Result<Tail> {
    let (mut file, len) = match crate::rawfile::open_regular(path) {
        Ok(opened) => opened,
        // No feed yet is a feed with nothing in it, and a cursor from a feed that was there is past its end.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Tail { events: Vec::new(), next: 0, reset: after.is_some_and(|at| at > 0), seen: None });
        }
        Err(e) => return Err(e),
    };
    let meta = file.metadata()?;
    let seen = Seen { dev: meta.dev(), ino: meta.ino(), len };
    // A file other than the one the last poll read (replaced, or a new inode), or shorter than it was then (cut in place), is a new feed: a cursor
    // into the old one means nothing in it. The poll starts that stream again and says so. Missing a regrowth past the cursor between two polls
    // of the same file is the one case this cannot see (the size is only seen at a poll; `README.md` says so).
    let replaced = last.is_some_and(|last| (last.dev, last.ino) != (seen.dev, seen.ino) || len < last.len);
    let (mut start, reset) = match after {
        Some(_) if replaced => (0, true),
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
        .filter_map(|record| keep(&record))
        .collect();
    Ok(Tail { events, next: start + whole as u64, reset, seen: Some(seen) })
}

/// One stream's poll: at `path` when it has one, else an empty stream (no path is no file, and so nothing to read). `last` is what the last
/// poll of this file saw ([`tail`]).
fn poll(path: Option<&Path>, after: Option<u64>, keep: Keep, last: Option<Seen>) -> std::io::Result<Tail> {
    match path {
        Some(path) => tail(path, after, keep, last),
        None => Ok(Tail { events: Vec::new(), next: 0, reset: false, seen: None }),
    }
}

/// One stream's poll, or the sentence for why it could not be read. A stream that cannot be read is empty, keeps the cursor it was asked from
/// (0 for the first poll), and says why; the other stream is not affected.
fn settle(read: std::io::Result<Tail>, asked: Option<u64>, what: &str) -> (Tail, Option<String>) {
    match read {
        Ok(tail) => (tail, None),
        Err(why) => {
            tracing::warn!(error = %why, what, "perch: an events file was not read");
            let empty = Tail { events: Vec::new(), next: asked.unwrap_or(0), reset: false, seen: None };
            (empty, Some(format!("{what} could not be read as a plain file")))
        }
    }
}

/// The records' time, for the merge. A record with no numeric `ts` sorts as 0.
fn ts_of(event: &Value) -> u64 {
    event.get("ts").and_then(Value::as_u64).unwrap_or(0)
}

/// The two polls as one answer. The records of both, by `ts`; at a tie the audit log's first, then each stream's own file order (a
/// stable sort, so nothing is reordered within a stream that has the same `ts`).
fn answer(audit: Tail, daemon: Tail, files: Streams<Option<String>>, error: Streams<Option<String>>) -> Answer {
    let mut events: Vec<Entry> = audit
        .events
        .into_iter()
        .map(|event| Entry { stream: "audit", event })
        .chain(daemon.events.into_iter().map(|event| Entry { stream: "daemon", event }))
        .collect();
    events.sort_by_key(|entry| (ts_of(&entry.event), entry.stream != "audit"));
    Answer {
        events,
        next: Streams { audit: audit.next, daemon: daemon.next },
        reset: Streams { audit: audit.reset, daemon: daemon.reset },
        error,
        files,
    }
}

/// `GET /hub/mesh/events[?audit=N][&daemon=M]`: [`Answer`]. A stream whose file is not there yet is an empty one, and so is a stream with
/// no usable path (a relative flag, or no home to name a default under). A file that is not a plain file (a link, a FIFO, a directory) is not
/// read: that stream gets its sentence in `error`, and the other stream still answers. Only a bad query is a 400.
async fn events(ctx: &Ctx, query: &str) -> Response<BoxBody> {
    let Some(cursors) = cursors_asked(query) else {
        return problem(StatusCode::BAD_REQUEST, "expected no query, or audit=<offset> and/or daemon=<offset>, each once");
    };
    let (audit, daemon) = (audit_path(&ctx.opts), daemon_path(&ctx.opts));
    let files = Streams { audit: audit.as_deref().map(tilde), daemon: daemon.as_deref().map(tilde) };
    let known: BTreeMap<PathBuf, Seen> = (*ctx.mesh.feeds.lock().await).clone();
    let last = Streams {
        audit: audit.as_ref().and_then(|path| known.get(path)).copied(),
        daemon: daemon.as_ref().and_then(|path| known.get(path)).copied(),
    };
    let (audit_to_read, daemon_to_read) = (audit.clone(), daemon.clone());
    let read = tokio::task::spawn_blocking(move || {
        (
            poll(audit_to_read.as_deref(), cursors.audit, audit_keep, last.audit),
            poll(daemon_to_read.as_deref(), cursors.daemon, feed_keep, last.daemon),
        )
    })
    .await;
    let Ok((audit_read, daemon_read)) = read else {
        tracing::warn!("perch: the events read did not finish");
        return problem(StatusCode::BAD_GATEWAY, "the events could not be read");
    };
    let (audit_tail, audit_error) = settle(audit_read, cursors.audit, "the audit log");
    let (daemon_tail, daemon_error) = settle(daemon_read, cursors.daemon, "the daemon feed");
    {
        let mut feeds = ctx.mesh.feeds.lock().await;
        for (path, seen) in [(&audit, audit_tail.seen), (&daemon, daemon_tail.seen)] {
            if let (Some(path), Some(seen)) = (path, seen) {
                feeds.insert(path.clone(), seen);
            }
        }
    }
    let error = Streams { audit: audit_error, daemon: daemon_error };
    json(StatusCode::OK, &answer(audit_tail, daemon_tail, files, error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use serde_json::json;

    /// [`tail`] with the daemon feed's filter: what the feed tests below mean by a feed. No poll before it, so no identity to compare with.
    fn feed_tail(path: &Path, after: Option<u64>) -> std::io::Result<Tail> {
        tail(path, after, feed_keep, None)
    }

    /// [`tail`] with the audit log's allowlist.
    fn audit_tail(path: &Path, after: Option<u64>) -> std::io::Result<Tail> {
        tail(path, after, audit_keep, None)
    }

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

    /// The cut is at the authority's last `@`, and a value that still has an `@` after it is none (fail closed). Mutation: keep a value with an
    /// `@` left in it, and the cases marked `None` go red.
    #[test]
    fn a_url_loses_what_comes_before_the_at_sign_in_its_authority_and_an_at_left_over_drops_it() {
        assert_eq!(without_userinfo("ssh://khoa@osaka").as_deref(), Some("ssh://osaka"));
        assert_eq!(without_userinfo("http://u:p@host/x").as_deref(), Some("http://host/x"));
        assert_eq!(without_userinfo("http://a@b@host").as_deref(), Some("http://host"), "the authority's last @");
        assert_eq!(without_userinfo("http://127.0.0.1:8710").as_deref(), Some("http://127.0.0.1:8710"));
        assert_eq!(without_userinfo("http://host:8710/").as_deref(), Some("http://host:8710/"), "a plain url is kept");
        assert_eq!(without_userinfo("not a url").as_deref(), Some("not a url"));
        // An @ after the authority is still in the value, so it is dropped.
        assert_eq!(without_userinfo("http://user:pass@host:8710/x?a=b@c"), None);
        assert_eq!(without_userinfo("http://host/a@b"), None, "an @ in the path is dropped too");
        // No scheme: nothing says where the authority ends, so an @ is dropped.
        assert_eq!(without_userinfo("user:pass@box-a:8710/"), None);
        // A password with a `/` in it ends the authority before its @, so the cut finds none and the value is dropped.
        assert_eq!(without_userinfo("http://user:pa/ss@host/"), None);
    }

    /// A registry url that keeps an `@` is absent from the node, and nothing else of the node goes with it. A url the file does not have is
    /// absent too.
    #[test]
    fn a_registry_url_with_an_at_left_in_it_is_absent_and_the_rest_of_the_node_is_not() {
        let registry = project_registry(r#"{"nodes": [{"name": "a", "url": "user:pass@a:8710/", "verified": true}, {"name": "b"}]}"#).unwrap();
        assert_eq!((registry.nodes[0].url.clone(), registry.nodes[0].verified), (None, true));
        assert_eq!(registry.nodes[1].url, None, "no url in the file is no url");
        let shown = serde_json::to_value(&registry.nodes[0]).unwrap();
        assert!(shown.get("url").is_none() && !shown.to_string().contains("pass"), "{shown}");
        assert!(serde_json::to_value(&registry.nodes[1]).unwrap().get("url").is_none(), "the key is not there");
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

    /// Both streams as one answer: by `ts`, the audit log first at a tie, each stream in its own file order. What the audit log holds that
    /// is not on its allowlist (a `message`, a record with no `ts`) is not in the answer, and each stream keeps its own cursor.
    #[test]
    fn the_two_streams_merge_by_ts_then_audit_first_then_file_order() {
        let (dir, audit_file) = feed(&[
            r#"{"ts":10,"class":"audit","command":"a1","untrusted_data":"LETTER-SENTINEL"}"#,
            r#"{"class":"audit","command":"no-ts"}"#,
            r#"{"ts":12,"class":"audit","command":"a2"}"#,
            r#"{"ts":12,"class":"audit","command":"a3","untrusted_data":"FORWARDED-SENTINEL"}"#,
        ]);
        let daemon_file = dir.path().join("daemon.jsonl");
        std::fs::write(
            &daemon_file,
            concat!(
                "{\"v\":0,\"ts\":12,\"class\":\"gate\",\"kind\":\"d1\"}\n",
                "{\"v\":0,\"ts\":9,\"class\":\"secret\",\"kind\":\"no\"}\n",
                "{\"v\":0,\"ts\":11,\"class\":\"audit\",\"kind\":\"d2\"}\n",
            ),
        )
        .unwrap();
        let answer = answer(
            audit_tail(&audit_file, Some(0)).unwrap(),
            feed_tail(&daemon_file, Some(0)).unwrap(),
            Streams { audit: Some("a".into()), daemon: None },
            Streams { audit: None, daemon: None },
        );
        let order: Vec<(&str, &str)> =
            answer
                .events
                .iter()
                .map(|entry| (entry.stream, entry.event.get("command").or(entry.event.get("kind")).and_then(Value::as_str).unwrap()))
                .collect();
        assert_eq!(order, [("audit", "a1"), ("daemon", "d2"), ("audit", "a2"), ("audit", "a3"), ("daemon", "d1")]);
        let shown = serde_json::to_string(&answer).unwrap();
        assert!(!shown.contains("LETTER-SENTINEL") && !shown.contains("FORWARDED-SENTINEL"), "{shown}");
        let shown: Value = serde_json::from_str(&shown).unwrap();
        // The keys are as serde_json sorts them (not the struct's order): the five parts, and nothing else.
        let mut keys: Vec<&str> = shown.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, ["error", "events", "files", "next", "reset"]);
        assert_eq!(shown["error"], json!({"audit": null, "daemon": null}), "no stream failed");
        assert_eq!(shown["next"]["audit"], std::fs::metadata(&audit_file).unwrap().len());
        assert_eq!(shown["next"]["daemon"], std::fs::metadata(&daemon_file).unwrap().len());
        assert_eq!(shown["files"], json!({"audit": "a", "daemon": null}));
        assert_eq!(shown["events"][0]["stream"], "audit");
        assert_eq!(shown["events"][1]["event"]["kind"], "d2", "the feed's record is as it is");
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
        let got = feed_tail(&path, Some(0)).unwrap();
        assert_eq!(kinds(&got), ["a1", "g1", "a2"]);
        assert_eq!(got.next, std::fs::metadata(&path).unwrap().len());
        assert!(!got.reset);
        assert!(!format!("{:?}", got.events).contains("SECRET-NAME"));
    }

    /// The daemon feed's fields are the ones the docs name, by class, and nothing else: a free-text field a later aoided adds, at the top level or
    /// in a payload, is dropped; a record with no integer `ts` is dropped. Mutation: forward the whole record, or keep every payload field, and the
    /// sentinels below reach the answer.
    #[test]
    fn the_feed_keeps_the_fields_the_docs_name_and_no_other() {
        let gate = feed_keep(&json!({"v": 0, "ts": 3, "class": "gate", "kind": "pair-parked", "source": "a2a-door", "note": "TOP-SENTINEL",
            "payload": {"id": "abc12345", "name": "box-a", "originAddr": "10.0.0.5", "url": "http://box-a:8710/", "direction": "inbound",
                "sas": "SAS-SENTINEL", "free": "PAYLOAD-SENTINEL"}}));
        assert_eq!(
            gate,
            Some(json!({"v": 0, "ts": 3, "class": "gate", "kind": "pair-parked", "source": "a2a-door",
                "payload": {"id": "abc12345", "name": "box-a", "originAddr": "10.0.0.5", "url": "http://box-a:8710/", "direction": "inbound"}}))
        );
        let audit = feed_keep(&json!({"v": 0, "ts": 4, "class": "audit", "kind": "hand-edit", "source": "aoided",
            "payload": {"file": "sessions.json", "text": "PAYLOAD-SENTINEL", "name": "not an audit field"}}));
        assert_eq!(
            audit,
            Some(json!({"v": 0, "ts": 4, "class": "audit", "kind": "hand-edit", "source": "aoided", "payload": {"file": "sessions.json"}}))
        );
        // A gate's payload is not an audit's: a `name` there is not kept.
        assert_eq!(feed_keep(&json!({"ts": 5, "class": "audit", "payload": {"name": "x"}})), Some(json!({"ts": 5, "class": "audit", "payload": {}})));
        // A gate's url with a `user:pass@` is kept without it, as the registry's is: the password is in no kept field, and no byte of the answer.
        let creds = feed_keep(&json!({"ts": 6, "class": "gate", "kind": "pair-parked",
            "payload": {"url": "http://user:pass@box-a:8710/", "name": "box-a"}}));
        assert_eq!(creds, Some(json!({"ts": 6, "class": "gate", "kind": "pair-parked", "payload": {"url": "http://box-a:8710/", "name": "box-a"}})));
        // The url cases of fail-closed scrubbing: a credential with no scheme, a `/` in the password, a cut that is kept, a plain url kept, and a url
        // that is not text is none. A field that is none is left out of the record; the other payload fields stay.
        for (url, shown) in [
            (json!("user:pass@box-a:8710/"), None),
            (json!("http://user:pa/ss@host/"), None),
            (json!("http://u:p@host/x"), Some("http://host/x")),
            (json!("http://host:8710/"), Some("http://host:8710/")),
            (json!({"u": "user:pass@box-a"}), None),
        ] {
            let record = feed_keep(&json!({"ts": 6, "class": "gate", "payload": {"url": url.clone(), "name": "box-a"}})).unwrap();
            assert_eq!(record["payload"].get("url").and_then(Value::as_str), shown, "{url}");
            assert_eq!(record["payload"]["name"], "box-a", "{url}: the other field is kept");
        }
        let none = || Tail { events: Vec::new(), next: 0, reset: false, seen: None };
        let daemon = Tail { events: creds.into_iter().collect(), next: 0, reset: false, seen: None };
        let nothing = || Streams { audit: None, daemon: None };
        let served = serde_json::to_string(&answer(none(), daemon, nothing(), nothing())).unwrap();
        assert!(served.contains("http://box-a:8710/") && !served.contains("pass"), "{served}");
        for bad in [
            json!({"class": "gate", "kind": "no-ts"}),
            json!({"ts": "7", "class": "gate"}),
            json!({"ts": 7.5, "class": "gate"}),
            json!({"ts": -1, "class": "gate"}),
        ] {
            assert_eq!(feed_keep(&bad), None, "{bad}");
        }
        for other in [json!({"ts": 1, "class": "secret"}), json!({"ts": 1, "class": "notification"}), json!({"ts": 1})] {
            assert_eq!(feed_keep(&other), None, "{other}");
        }
    }

    /// The identity a poll keeps: a feed that was replaced by another file, or cut shorter than the last poll saw, starts again from 0 even when
    /// its cursor is still inside it. A file that only grew is read from the cursor as before, and with no last poll a cursor is just a cursor.
    /// Mutation: drop the replaced term from [`tail`] and the first reset below goes red.
    #[test]
    fn a_feed_replaced_or_cut_since_the_last_poll_starts_again_though_the_cursor_is_inside_it() {
        let (dir, path) = feed(&[r#"{"ts":1,"class":"audit","kind":"one"}"#, r#"{"ts":2,"class":"audit","kind":"two"}"#]);
        let first = feed_tail(&path, None).unwrap();
        let seen = first.seen.unwrap();
        let quiet = tail(&path, Some(first.next), feed_keep, Some(seen)).unwrap();
        assert_eq!((quiet.reset, quiet.events.len()), (false, 0), "nothing new, and no reset");
        // Replaced by another file, a new inode, longer than the cursor: the cursor is inside it, and it is still a new feed.
        let other = dir.path().join("replacement.jsonl");
        let replacement = concat!(
            "{\"ts\":3,\"class\":\"audit\",\"kind\":\"a\"}\n",
            "{\"ts\":4,\"class\":\"audit\",\"kind\":\"b\"}\n",
            "{\"ts\":5,\"class\":\"audit\",\"kind\":\"c\"}\n",
        );
        std::fs::write(&other, replacement).unwrap();
        std::fs::rename(&other, &path).unwrap();
        let replaced = tail(&path, Some(first.next), feed_keep, Some(seen)).unwrap();
        assert!(replaced.reset, "a new file is a reset");
        assert_eq!(kinds(&replaced), ["a", "b", "c"], "read from 0");
        // Cut in place to something shorter than the last poll saw: the same inode, and a reset.
        let now = replaced.seen.unwrap();
        let mut cut_file = std::fs::OpenOptions::new().write(true).truncate(true).open(&path).unwrap();
        std::io::Write::write_all(&mut cut_file, b"{\"ts\":6,\"class\":\"audit\",\"kind\":\"d\"}\n").unwrap();
        let cut = tail(&path, Some(replaced.next), feed_keep, Some(now)).unwrap();
        assert!(cut.reset, "shorter than the last poll is a reset");
        assert_eq!(kinds(&cut), ["d"]);
        // With no last poll the same cursor is only a cursor, inside the file: no reset.
        let no_memory = tail(&path, Some(1), feed_keep, None).unwrap();
        assert!(!no_memory.reset);
    }

    /// The start's check: two feeds are one file when their paths resolve to one, or when they are one inode under two names (a hard link or a
    /// link). Paths that are two files, a file that is not there yet under a folder that is, and a relative flag (which names no file) all pass.
    /// The audit log's default is `<home>/log`, so a feed flag at that path is refused too.
    #[test]
    fn a_start_with_the_two_feeds_as_one_file_is_refused_and_two_files_are_not() {
        let dir = tempfile::tempdir().unwrap();
        let (log, feed) = (dir.path().join("log"), dir.path().join("events.jsonl"));
        std::fs::write(&log, "").unwrap();
        std::fs::write(&feed, "").unwrap();
        assert!(feeds_apart(None, Some(log.as_path()), Some(feed.as_path())).is_ok(), "two files");
        assert!(feeds_apart(None, Some(log.as_path()), Some(log.as_path())).is_err(), "the same path");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&log, &link).unwrap();
        let why = feeds_apart(None, Some(log.as_path()), Some(link.as_path())).unwrap_err();
        assert!(why.contains("same file") && why.contains(&log.display().to_string()), "{why}");
        let hard = dir.path().join("hard");
        std::fs::hard_link(&feed, &hard).unwrap();
        assert!(feeds_apart(None, Some(hard.as_path()), Some(feed.as_path())).is_err(), "one inode, two names");
        let later = dir.path().join("later.jsonl");
        assert!(feeds_apart(None, Some(later.as_path()), Some(log.as_path())).is_ok(), "not there yet, under a folder that is");
        assert!(feeds_apart(None, Some(later.as_path()), Some(later.as_path())).is_err());
        assert!(feeds_apart(Some(dir.path()), None, Some(log.as_path())).is_err(), "the audit default is <home>/log");
        assert!(feeds_apart(None, Some(Path::new("log")), Some(log.as_path())).is_ok(), "a relative flag names no file");
        // A dangling link at the audit path names the feed that aoided has not written yet: the same file once the link is followed. Two links,
        // the second dangling, are the same; a chain of exactly LINK_HOPS links to another file is followed; one link more, and a loop, are refused.
        let unwritten = dir.path().join("unwritten.jsonl");
        let dangling = dir.path().join("dangling");
        std::os::unix::fs::symlink(&unwritten, &dangling).unwrap();
        assert!(feeds_apart(None, Some(dangling.as_path()), Some(unwritten.as_path())).is_err(), "a dangling link to the feed is the feed");
        let hop = dir.path().join("hop");
        std::os::unix::fs::symlink(&unwritten, &hop).unwrap();
        let two = dir.path().join("two");
        std::os::unix::fs::symlink(&hop, &two).unwrap();
        assert!(feeds_apart(None, Some(two.as_path()), Some(unwritten.as_path())).is_err(), "a chain of two links to the feed");
        assert!(feeds_apart(None, Some(two.as_path()), Some(later.as_path())).is_ok(), "a chain to another path is another file");
        // A relative link is relative to its own folder, not to the working directory: a dangling one beside the feed, one in a sub-folder that
        // reaches up a level, and a chain of two relative links. Mutation: resolve a relative target as it is (against the working directory), and
        // the three `is_err` cases below answer `Ok` instead.
        let rel = dir.path().join("rel");
        std::os::unix::fs::symlink("unwritten.jsonl", &rel).unwrap();
        assert!(feeds_apart(None, Some(rel.as_path()), Some(unwritten.as_path())).is_err(), "a relative dangling link names the feed in its folder");
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let up = sub.join("up");
        std::os::unix::fs::symlink("../unwritten.jsonl", &up).unwrap();
        assert!(feeds_apart(None, Some(up.as_path()), Some(unwritten.as_path())).is_err(), "../ counts from the link's own folder");
        assert!(feeds_apart(None, Some(up.as_path()), Some(later.as_path())).is_ok(), "a relative link to another path is another file");
        let rel_hop = dir.path().join("rel-hop");
        std::os::unix::fs::symlink("rel", &rel_hop).unwrap();
        assert!(feeds_apart(None, Some(rel_hop.as_path()), Some(unwritten.as_path())).is_err(), "a chain of two relative links is followed");
        let chain = |name: &str, links: usize, end: &Path| -> PathBuf {
            let mut target = end.to_path_buf();
            for n in (0..links).rev() {
                let link = dir.path().join(format!("{name}{n}"));
                std::os::unix::fs::symlink(&target, &link).unwrap();
                target = link;
            }
            target
        };
        let forty = chain("forty", LINK_HOPS, &log);
        assert!(feeds_apart(None, Some(forty.as_path()), Some(later.as_path())).is_ok(), "{LINK_HOPS} links are followed");
        let too_many = chain("many", LINK_HOPS + 1, &log);
        let why = feeds_apart(None, Some(too_many.as_path()), Some(later.as_path())).unwrap_err();
        assert!(why.contains("more than") && why.contains("links"), "{why}");
        let (loop_a, loop_b) = (dir.path().join("loop-a"), dir.path().join("loop-b"));
        std::os::unix::fs::symlink(&loop_b, &loop_a).unwrap();
        std::os::unix::fs::symlink(&loop_a, &loop_b).unwrap();
        assert!(feeds_apart(None, Some(loop_a.as_path()), Some(later.as_path())).is_err(), "a loop is not a file");
        assert_eq!(std::fs::read(&log).unwrap(), b"", "nothing was written");
    }

    /// The default feed comes from `$AOIDE_DAEMON_EVENTS` when it is absolute, else from the runtime dir when that is absolute. A relative value
    /// of either, and an empty one, names no file, so no path under the working directory is ever opened.
    #[test]
    fn the_default_feed_is_never_a_relative_path() {
        let path = |s: &str| Some(PathBuf::from(s));
        assert_eq!(daemon_default(path("/env/events.jsonl"), path("/run/u")), path("/env/events.jsonl"), "the absolute variable wins");
        assert_eq!(daemon_default(path("relative/events.jsonl"), path("/run/u")), path("/run/u/aoide/events.jsonl"), "a relative variable is none");
        assert_eq!(daemon_default(path(""), path("/run/u")), path("/run/u/aoide/events.jsonl"), "an empty variable is none");
        assert_eq!(daemon_default(None, path("run/u")), None, "a relative runtime dir names no file");
        assert_eq!(daemon_default(None, path("")), None);
        assert_eq!(daemon_default(None, None), None);
    }

    #[test]
    fn a_last_line_without_its_newline_waits_for_it() {
        let (_dir, path) = feed(&[r#"{"ts":1,"class":"audit","kind":"first"}"#]);
        let whole = std::fs::metadata(&path).unwrap().len();
        let mut file = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
        std::io::Write::write_all(&mut file, br#"{"ts":2,"class":"gate","kind":"sec"#).unwrap();
        let got = feed_tail(&path, Some(0)).unwrap();
        assert_eq!((kinds(&got), got.next), (vec!["first"], whole), "the half line is not read, and the cursor stays before it");
        std::io::Write::write_all(&mut file, b"ond\"}\n").unwrap();
        let got = feed_tail(&path, Some(whole)).unwrap();
        assert_eq!(kinds(&got), ["second"]);
        assert_eq!(got.next, std::fs::metadata(&path).unwrap().len());
        let again = feed_tail(&path, Some(got.next)).unwrap();
        assert_eq!((again.events.len(), again.next, again.reset), (0, got.next, false));
    }

    #[test]
    fn a_file_that_shrank_is_read_again_from_the_start() {
        let (_dir, path) = feed(&[r#"{"ts":1,"class":"audit","kind":"old-1"}"#, r#"{"ts":2,"class":"audit","kind":"old-2"}"#]);
        let before = feed_tail(&path, Some(0)).unwrap();
        // aoided truncates in place at its cap and goes on writing: the file is shorter than the cursor.
        std::fs::write(&path, "{\"ts\":3,\"class\":\"gate\",\"kind\":\"new\"}\n").unwrap();
        let got = feed_tail(&path, Some(before.next)).unwrap();
        assert!(got.reset);
        assert_eq!(kinds(&got), ["new"]);
        assert_eq!(got.next, std::fs::metadata(&path).unwrap().len());
        // A file that is gone after a cursor is the same fact; with no cursor there is nothing to reset.
        std::fs::remove_file(&path).unwrap();
        assert_eq!(feed_tail(&path, Some(5)).unwrap(), Tail { events: vec![], next: 0, reset: true, seen: None });
        assert_eq!(feed_tail(&path, Some(0)).unwrap(), Tail { events: vec![], next: 0, reset: false, seen: None });
        assert_eq!(feed_tail(&path, None).unwrap(), Tail { events: vec![], next: 0, reset: false, seen: None });
    }

    #[test]
    fn a_call_reads_at_most_a_window_and_the_calls_together_lose_nothing() {
        let line = |n: usize| format!("{{\"ts\":{n},\"class\":\"audit\",\"kind\":\"k{n:05}\",\"payload\":\"{}\"}}", "x".repeat(200));
        let all: Vec<String> = (0..1000).map(line).collect();
        let (_dir, path) = feed(&all.iter().map(String::as_str).collect::<Vec<_>>());
        let size = std::fs::metadata(&path).unwrap().len();
        assert!(size > 3 * EVENTS_WINDOW);
        let (mut after, mut seen, mut calls) = (0, Vec::new(), 0);
        while after < size {
            let got = feed_tail(&path, Some(after)).unwrap();
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
        let line = |n: usize| format!("{{\"ts\":{n},\"class\":\"gate\",\"kind\":\"k{n:05}\",\"payload\":\"{}\"}}", "y".repeat(200));
        let all: Vec<String> = (0..1000).map(line).collect();
        let (_dir, path) = feed(&all.iter().map(String::as_str).collect::<Vec<_>>());
        let size = std::fs::metadata(&path).unwrap().len();
        let got = feed_tail(&path, None).unwrap();
        assert_eq!(got.next, size);
        assert_eq!(got.events.last().unwrap()["kind"], "k00999");
        let first = got.events[0]["kind"].as_str().unwrap().to_string();
        assert!(first != "k00000", "the first poll is a window, not the file");
        let count = got.events.len();
        assert!(count > 100 && count < 1000);
        let n: usize = first[1..].parse().unwrap();
        assert_eq!(count, 1000 - n, "a run of lines with none missing in the middle");
        // A small file is read whole.
        let (_dir, path) = feed(&[r#"{"ts":1,"class":"audit","kind":"only"}"#]);
        assert_eq!(kinds(&feed_tail(&path, None).unwrap()), ["only"]);
    }

    #[test]
    fn a_line_longer_than_the_window_is_passed_over() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let mut text = format!("{{\"class\":\"audit\",\"kind\":\"{}\"}}", "z".repeat(EVENTS_WINDOW as usize + 10));
        text.push('\n');
        text.push_str("{\"ts\":2,\"class\":\"audit\",\"kind\":\"after\"}\n");
        std::fs::write(&path, &text).unwrap();
        let first = feed_tail(&path, Some(0)).unwrap();
        assert!(first.events.is_empty());
        assert_eq!(first.next, EVENTS_WINDOW, "the cursor moves on so the poll is not stuck behind it");
        let second = feed_tail(&path, Some(first.next)).unwrap();
        assert_eq!(kinds(&second), ["after"], "the rest of the long line is a fragment and is not an event");
    }

    #[test]
    fn a_feed_that_is_a_link_or_a_directory_is_not_read() {
        let (dir, path) = feed(&[r#"{"class":"audit","kind":"real"}"#]);
        let link = dir.path().join("link.jsonl");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(feed_tail(&link, Some(0)).is_err(), "O_NOFOLLOW: a link is not followed");
        assert!(feed_tail(dir.path(), Some(0)).is_err(), "a directory is not a feed");
    }

    fn cursors(audit: Option<u64>, daemon: Option<u64>) -> Option<Cursors> {
        Some(Cursors { audit, daemon })
    }

    #[test]
    fn the_events_query_is_each_stream_s_offset_once_and_nothing_else() {
        assert_eq!(cursors_asked(""), cursors(None, None), "no query is the first poll of both");
        assert_eq!(cursors_asked("audit=0"), cursors(Some(0), None));
        assert_eq!(cursors_asked("daemon=1234567"), cursors(None, Some(1_234_567)));
        assert_eq!(cursors_asked("audit=5&daemon=7"), cursors(Some(5), Some(7)));
        assert_eq!(cursors_asked("daemon=7&audit=5"), cursors(Some(5), Some(7)), "either order");
        for bad in [
            "after=0", "after=", "audit=", "audit=-1", "audit=1.5", "audit=1e3", "audit=%31", "audit=+1", "audit= 1", "audit=x", "audit",
            "AUDIT=1", "audit=1&audit=2", "daemon=1&daemon=1", "audit=1&after=1", "audit=1&x=1", "x=1", "audit=1&", "&audit=1", "audit=1&&daemon=2",
            "audit=99999999999999999999999", "audit=1=2",
        ] {
            assert_eq!(cursors_asked(bad), None, "{bad:?}");
        }
    }

    /// The audit log's allowlist: what is forwarded is `ts`, `class`, `door`, `command` and `status`, and a record outside it is dropped.
    #[test]
    fn the_audit_log_forwards_its_allowlist_and_drops_what_it_cannot_pin() {
        let kept = audit_keep(&json!({"ts": 1791366040, "door": "cli", "class": "audit", "command": "mail.send", "status": "ok",
            "message": "LETTER-SENTINEL body of a letter", "untrusted_data": "FORWARDED-SENTINEL", "admitted": false, "argv": ["--token=x"]}));
        assert_eq!(kept, Some(json!({"ts": 1791366040, "class": "audit", "door": "cli", "command": "mail.send", "status": "ok"})));
        // The gate's self-check: no door, no command, and nothing else but its class and status.
        let gate = audit_keep(&json!({"ts": 5, "class": "gate", "status": "proposed", "admitted": false}));
        assert_eq!(gate, Some(json!({"ts": 5, "class": "gate", "status": "proposed"})));
        // aoided's start record, as the real log writes it (door daemon, command daemon, status started): its message is kept, since it is a
        // start record.
        let started = audit_keep(&json!({"ts": 1791365990, "door": "daemon", "class": "audit", "command": "daemon", "status": "started",
            "message": "STARTED-SENTINEL on http://127.0.0.1:8710/"}));
        assert_eq!(
            started,
            Some(json!({"ts": 1791365990, "class": "audit", "door": "daemon", "command": "daemon", "status": "started",
                "message": "STARTED-SENTINEL on http://127.0.0.1:8710/"}))
        );
        let melete = json!({"ts": 2, "class": "audit", "command": "adapter.melete", "status": "started"});
        assert_eq!(audit_keep(&melete), Some(melete.clone()));
        // Each drop has its reason, and none of them is a secret or a letter that got through.
        let drops = [
            json!({"class": "audit", "command": "mail.send"}),
            json!({"ts": "1791366040", "class": "audit"}),
            json!({"ts": 1.5, "class": "audit"}),
            json!({"ts": -1, "class": "audit"}),
            json!({"ts": 1, "class": "secret", "command": "x"}),
            json!({"ts": 1, "class": "notification"}),
            json!({"ts": 1}),
            json!({"ts": 1, "class": "audit", "door": "non-cli"}),
            json!({"ts": 1, "class": "audit", "command": "node add osaka"}),
            json!({"ts": 1, "class": "audit", "command": "mail send"}),
            json!({"ts": 1, "class": "audit", "command": "-leading"}),
            json!({"ts": 1, "class": "audit", "command": 7}),
            json!({"ts": 1, "class": "audit", "status": "parked (superseding osaka)"}),
            json!({"ts": 1, "class": "audit", "status": "Started"}),
            json!({"ts": 1, "class": "audit", "door": "daemon", "command": "daemon", "status": "started now"}),
            json!({"ts": 1, "class": "audit", "status": null}),
            // `failed` is not a status the list names, so the whole record goes, message and all.
            json!({"ts": 1, "class": "audit", "door": "daemon", "command": "daemon", "status": "failed", "message": "FAILED-TEXT"}),
            json!("a string, not a record"),
            json!(["audit"]),
        ];
        for record in drops {
            assert_eq!(audit_keep(&record), None, "{record}");
        }
        // A secret's line keeps its command and status, and not its message, which names the secret.
        let secret_line =
            audit_keep(&json!({"ts": 9, "class": "audit", "door": "cli", "command": "secrets.resolve", "status": "ok", "message": "name=api-key"}));
        assert_eq!(secret_line, Some(json!({"ts": 9, "class": "audit", "door": "cli", "command": "secrets.resolve", "status": "ok"})));
        let only_a_door = audit_keep(&json!({"ts": 1, "class": "audit", "door": "cli"}));
        assert_eq!(only_a_door, Some(json!({"ts": 1, "class": "audit", "door": "cli"})), "an absent optional field is not in the record");
    }

    /// The message rule: a record's `message` is forwarded only for aoided's start records (`daemon` and `adapter.melete`, status `started`), and
    /// for no other record, mail's or anyone's (a letter, a secret's name, a failed run). The command is matched without regard to case. A record
    /// with no command or status keeps none, and a `message` that is not text is left out (the record stays).
    /// Mutation: forward `message` for every command, or for a start record whatever its status, and the cases below go red.
    #[test]
    fn a_message_reaches_an_answer_only_for_a_start_record() {
        for (command, status) in [("daemon", "started"), ("DAEMON", "started"), ("adapter.melete", "started"), ("Adapter.Melete", "started")] {
            let kept = audit_keep(&json!({"ts": 1, "class": "audit", "command": command, "status": status, "message": "STARTED-TEXT"}));
            assert_eq!(kept.as_ref().and_then(|kept| kept.get("message")), Some(&json!("STARTED-TEXT")), "{command} {status}");
        }
        for (command, status) in [
            ("mail.read", "ok"), ("mail.send", "ok"), ("mail.outbox", "ok"), ("MAIL.read", "ok"), ("Mail/read", "ok"),
            ("mail-read", "ok"), ("mail", "ok"), ("secrets.resolve", "ok"), ("secrets.put", "ok"), ("secrets.admin", "ok"),
            ("node-mail-x", "ok"), ("mailer.check", "ok"), ("a2a.serve", "ok"), ("send", "pending"), ("daemon", "error"),
            ("daemon", "ok"), ("adapter.melete", "error"),
        ] {
            let kept = audit_keep(&json!({"ts": 1, "class": "audit", "command": command, "status": status, "message": "LETTER-SENTINEL body"}));
            assert_eq!(kept, Some(json!({"ts": 1, "class": "audit", "command": command, "status": status})), "{command} {status}");
        }
        assert_eq!(audit_keep(&json!({"ts": 1, "class": "audit", "message": "no command"})), Some(json!({"ts": 1, "class": "audit"})));
        let no_status = audit_keep(&json!({"ts": 1, "class": "audit", "command": "daemon", "message": "no status"}));
        assert_eq!(no_status, Some(json!({"ts": 1, "class": "audit", "command": "daemon"})));
        let not_text = json!({"ts": 1, "class": "audit", "command": "daemon", "status": "started", "message": 7});
        assert_eq!(audit_keep(&not_text), Some(json!({"ts": 1, "class": "audit", "command": "daemon", "status": "started"})));
        assert!(start_record("daemon", "started") && start_record("adapter.melete", "started") && start_record("Daemon", "started"));
        assert!(!start_record("daemon", "failed") && !start_record("daemonx", "started"));
        assert!(!start_record("mail.read", "started") && !start_record("", "started"));
    }

    #[test]
    fn a_command_word_is_dotted_or_slashed_and_never_has_an_argument_in_it() {
        for ok in ["mail.send", "a2a.pairRequest", "session.pending.approve", "aoide/pairRequest", "send", "x-1"] {
            assert!(command_ok(ok), "{ok}");
        }
        for bad in ["", "1send", ".send", "mail send", "node add osaka", "mail.send\n", "a:b", "é", &"c".repeat(COMMAND_MAX + 1)] {
            assert!(!command_ok(bad), "{bad:?}");
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
            aoide_audit_log: None,
            aoide_daemon_events: None,
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
        // The two feeds default under the same stand-in folder, never under the real home or the real runtime dir.
        assert_eq!(audit_path(&opts).unwrap(), Path::new("/nonexistent-minerva-test/aoide/log"));
        assert_eq!(daemon_path(&opts).unwrap(), Path::new("/nonexistent-minerva-test/aoide/events.jsonl"));
        // A flag still wins, in this build too.
        let flagged = Options {
            aoide_home: Some(PathBuf::from("/given/aoide")),
            aoide_daemon_socket: Some(PathBuf::from("/given/aoided.sock")),
            aoide_audit_log: Some(PathBuf::from("/given/audit.log")),
            aoide_daemon_events: Some(PathBuf::from("/given/events.jsonl")),
            ..opts
        };
        assert_eq!(super::home(&flagged).unwrap(), Path::new("/given/aoide"));
        assert_eq!(daemon_socket(&flagged).unwrap(), Path::new("/given/aoided.sock"));
        assert_eq!(audit_path(&flagged).unwrap(), Path::new("/given/audit.log"));
        assert_eq!(daemon_path(&flagged).unwrap(), Path::new("/given/events.jsonl"));
    }

    /// The defaults of the two files, and the one rule that holds for both: a relative path names no file, and an explicit flag is never
    /// replaced by the default (a relative flag is none, not the default under the home).
    #[test]
    fn the_two_feeds_resolve_from_their_flags_and_their_defaults_and_only_absolute_paths_count() {
        let mut opts = unflagged();
        opts.aoide_home = Some(PathBuf::from("/given/aoide"));
        assert_eq!(audit_path(&opts), Some(PathBuf::from("/given/aoide/log")), "aoide's own default under --aoide-home");
        opts.aoide_audit_log = Some(PathBuf::from("relative/audit.log"));
        assert_eq!(audit_path(&opts), None, "a relative flag names no file, and does not fall back to the default");
        opts.aoide_audit_log = None;
        opts.aoide_home = Some(PathBuf::from("relative-home"));
        assert_eq!(audit_path(&opts), None, "a relative home has no log under it");
        opts.aoide_daemon_events = Some(PathBuf::from("relative/events.jsonl"));
        assert_eq!(daemon_path(&opts), None, "a relative daemon flag is none");
        opts.aoide_daemon_events = None;
        // The default is `$AOIDE_DAEMON_EVENTS` when absolute and `$XDG_RUNTIME_DIR/aoide/events.jsonl` otherwise. The unit tests see the
        // stand-in, whatever this process's environment is; the integration tests set the perch's own environment and check it there.
        assert_eq!(daemon_path(&opts), Some(PathBuf::from("/nonexistent-minerva-test/aoide/events.jsonl")));
    }

    /// The audit flag has one reader: the runner's `AOIDE_AUDIT_LOG` and the view's audit path are both taken from it, so they name the same
    /// file. A relative flag reaches the runner as it is (its calls are refused, not run with aoide's own default) and the view names no file.
    /// Mutation: have either side read the raw field, or fall back to the default for a relative flag, and one of these goes red.
    #[test]
    fn the_runner_and_the_view_read_the_audit_flag_from_one_place() {
        let mut opts = unflagged();
        opts.aoide = Some(PathBuf::from("/opt/aoide"));
        opts.aoide_audit_log = Some(PathBuf::from("/given/audit.log"));
        assert_eq!(crate::aoide::audit_flag(&opts), Some(PathBuf::from("/given/audit.log")));
        assert_eq!(audit_path(&opts), Some(PathBuf::from("/given/audit.log")));
        assert_eq!(Aoide::from_options(&opts).unwrap().audit_log, Some(PathBuf::from("/given/audit.log")));
        opts.aoide_audit_log = Some(PathBuf::from("relative/audit.log"));
        let relative = Some(PathBuf::from("relative/audit.log"));
        assert_eq!(Aoide::from_options(&opts).unwrap().audit_log, relative, "refused by the runner, not defaulted");
        assert_eq!(audit_path(&opts), None, "and the view names no file for it");
        opts.aoide_audit_log = None;
        assert_eq!(Aoide::from_options(&opts).unwrap().audit_log, None, "no flag, no AOIDE_AUDIT_LOG: aoide's own default");
        assert_eq!(audit_path(&opts), Some(PathBuf::from("/nonexistent-minerva-test/aoide/log")));
    }
}
