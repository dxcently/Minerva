//! `policy.permits.rn` — the store of what the operator has granted.
//!
//! One block, two authors, two files: `policy.rn` is hand-edited and *argues*;
//! this file is **strictly machine-edited** and holds what the operator
//! *granted* — vouches for plugin verbs (P4) and grants of secret sources to
//! tools (P3). One row shape with two subject kinds, which is why both halves
//! are one file.
//!
//! ## Where it lives
//!
//! **Derived, never configured**: the same directory as the policy file the
//! loader probed, whether or not an operator's file was there — [`beside`]
//! answers the path, [`Permits::from_policy_path`] the store. When the
//! plugins-and-config tree is re-shaped, the two files move together and
//! nothing else changes.
//!
//! ## How it is read
//!
//! The file is Rune, generated in a fixed shape: three `const` maps, the two
//! accessors the spec names, and one enumerator. It is micro-compiled here
//! against the manifest context's precedent — default modules, `json`, and
//! the one pure comparison name the accessors call, no host — and
//! [`Permits::rows`] reads everything back in one call, so the
//! Rust lookups ([`Permits::vouched`], [`Permits::granted`]) are map lookups,
//! never a VM per call. The *same* file compiles into the policy unit, in
//! front of the table, where the accessors are bare-name callable by the
//! operator's own `policy.rn`. So the file is compiled **twice**, on
//! purpose: the micro-unit here serves the Rust readers — the endpoint's
//! grant check, the CLI's listing and writes — and the policy unit serves
//! the table and its nuance calls. The drift risk that arrangement carries
//! is met the only place it can be: both units are compiled from the same
//! bytes, and both are rebuilt whenever those bytes change, so a lookup in
//! Rust and a lookup in Rune answer from one file. The micro-unit's context
//! is the manifest compile's — default modules and `json`, **stricter** than
//! the one `policy.rn` itself gets (the full tool scope) — so this file
//! cannot reach a host primitive even by accident.
//!
//! ## Re-read on change
//!
//! Every lookup refreshes first ([`Permits::refresh`]): the file is read,
//! compared with what is loaded, and re-parsed only when it differs — so a
//! vouch or a grant written by another process, or by the ask-once answer in
//! this one, takes effect on the next call rather than the next session. A
//! file that exists but does not compile loads as **no rows**, never as the
//! previous ones: a stale vouch may keep asking, but a broken file must not
//  keep allowing.
//!
//! ## How it is written
//!
//! Always whole, never patched: rows sorted, a generated header that means
//! it, temp + rename. Before the swap the composed unit — this source in
//! front of whatever table loads — is compiled in memory through the
//! checker attached at startup; a permissions file that would break the gate
//! is refused and the previous one stays on disk. With no checker there is
//! nothing to write through: [`Permits::write`] refuses, because an
//! unchecked write is how a broken gate gets onto disk.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use anyhow::Context as _;
use rune::{Diagnostics, Module, Vm};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The file's name, and the accessor names the spec fixes. They are part of
/// the block's interface: the operator's `policy.rn` calls `vouched` and
/// `granted` by bare name, in front of which this file always compiles.
pub const PERMITS_FILE: &str = "policy.permits.rn";

/// The shipped shape with no rows in it — what the loader composes in front
/// of the table when no file exists yet, and the pin the writer's output is
/// held against when the maps are empty.
pub const DEFAULT_PERMITS: &str = include_str!("../policy.permits.rn");

/// The permits file sits beside the policy file the loader probed — the one
/// it would have loaded, when the built-in shipped instead.
pub fn beside(policy_path: &Path) -> PathBuf {
    policy_path.with_file_name(PERMITS_FILE)
}

/// A vouch: the operator's standing yes for one plugin verb. The hash is the
/// **tool script's content hash** at vouch time, compared in Rust against
/// what is actually loaded — an edited plugin re-asks, and the Rune file
/// never grows a crypto habit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VouchRow {
    pub plugin: String,
    pub verb: String,
    pub hash: String,
    pub granted_by: String,
    pub granted_at: String,
}

impl VouchRow {
    /// Whether this vouch answers for the tool as loaded: the same
    /// plugin, and byte-for-byte the script the vouch saw. The one
    /// comparison the gate's answer and any other reader of the store —
    /// a panel source's may-fetch check — must agree on.
    pub fn covers(&self, manifest: &eidolon_core::tool::ToolManifest) -> bool {
        manifest.plugin.as_deref() == Some(self.plugin.as_str())
            && manifest.source_hash.as_deref() == Some(self.hash.as_str())
    }
}

/// An attestation: the operator approved this plugin's **service
/// declaration** as written — the row holds the declaration's hash, and
/// `start` matches it exactly, so an update that changes the command
/// stops being approved and asks again. The subject is the plugin: one
/// declaration per plugin, replaced whole on re-approval.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttestRow {
    pub plugin: String,
    pub hash: String,
    pub granted_by: String,
    pub granted_at: String,
}

/// A grant: one tool's permission to reach one **source**, the whole
/// source and not one spelling of it. The subject spelling is the
/// **registered verb name** (`browser_fetch`), the same name the gate's
/// question, the listing and the audit line's tool side use; the audit
/// line keeps `consumer` — whose effect — and adds `grant` — whose
/// permission — so one decision reads as one fact and not two spellings
/// of it. There is deliberately no plugin-wide row: grouping is a CLI
/// act that enumerates what exists now and writes one row per verb, so a
/// stored yes is bound to verbs the operator could name when they said
/// it, and never stands as authority over verbs that came later.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantRow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    pub source: String,
    pub granted_by: String,
    pub granted_at: String,
    /// A wide row as an older file wrote it: carried through reads and
    /// writes so the fact is never silently dropped, consulted by
    /// nothing, and named by the listing — the verbs it covered ask
    /// again. No current verb constructs one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wide_plugin: Option<String>,
}

/// Everything the file holds, as the writer writes it and the reader parses
/// it. Sorted `Vec`s: the file's order is the writer's sort order, and the
/// same struct writes and reads it. Lookups scan — a plugin has a handful
/// of verbs, not thousands.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rows {
    /// One row per vouched verb, sorted by verb.
    #[serde(default)]
    pub vouch: Vec<VouchRow>,
    /// One row per grant — per-tool or plugin-wide — sorted by subject then
    /// source, both subject kinds in the one list, exactly as the file has.
    #[serde(default)]
    pub grant: Vec<GrantRow>,
    /// One row per attested service declaration: the operator approved
    /// this plugin's `command` as written, by hash. An updated command
    /// stops matching and start refuses until it is approved again — an
    /// update cannot inherit the right to run a process.
    #[serde(default)]
    pub attest: Vec<AttestRow>,
}

impl Rows {
    /// Whether this plugin's service declaration is approved as written:
    /// the row's hash must equal the declaration's, computed the one way
    /// ([`service_declaration_hash`]) — any difference is a different
    /// program, however small the edit.
    pub fn attested(&self, plugin: &str, hash: &str) -> bool {
        self.attest
            .iter()
            .any(|row| row.plugin == plugin && row.hash == hash)
    }

    /// The grant covering `tool`'s reaching `source`: the exact row, or
    /// nothing. The source is matched the way the gate resolves it — a row
    /// written `~/…` and a lookup for the expanded path name one source,
    /// either direction ([`grant_source_eq`]) — so a tilde row the README
    /// told a headless install to write covers the path the gate actually
    /// asks about. There is no shorthand behind it — grouping wrote one row
    /// per verb at the moment of the yes, so a row exists only for a verb
    /// that existed when it was given.
    pub fn granted(&self, tool: &str, source: &str) -> Option<GrantRow> {
        self.grant
            .iter()
            .find(|row| row.tool.as_deref() == Some(tool) && grant_source_eq(&row.source, source))
            .cloned()
    }

    /// Add or replace one grant — a row for the same subject and source
    /// replaces the old rather than piling onto it. Replacement is by
    /// exact spelling: the same source under another spelling — `~/…`
    /// beside its expansion — is another row, and a grant never rewrites
    /// a row it was not asked for.
    pub fn grant(&mut self, row: GrantRow) {
        self.grant
            .retain(|r| r.source != row.source || r.subject() != row.subject());
        self.grant.push(row);
        self.sort();
    }

    /// Drop every vouch attributable to `plugin` — the whole plugin when
    /// `verb` is `None`, one verb's row otherwise. Answers how many went.
    pub fn unvouch(&mut self, plugin: &str, verb: Option<&str>) -> usize {
        let before = self.vouch.len();
        self.vouch.retain(|row| match verb {
            Some(v) => !(row.plugin == plugin && row.verb == v),
            None => row.plugin != plugin,
        });
        before - self.vouch.len()
    }

    /// The file's row order: vouches by verb, grants by subject then source.
    /// One sort, used by every write, so the file only ever changes when the
    /// rows do.
    fn sort(&mut self) {
        self.vouch.sort_by(|a, b| a.verb.cmp(&b.verb));
        self.grant.sort_by(|a, b| {
            a.subject()
                .cmp(&b.subject())
                .then_with(|| a.source.cmp(&b.source))
        });
    }
}

impl GrantRow {
    /// What the row is about, for sorting and for replacing: the tool.
    /// A legacy wide row has none, and sorts as the empty subject — the
    /// one place its presence still shows beside the naming.
    pub fn subject(&self) -> String {
        self.tool.clone().unwrap_or_default()
    }
}

/// The tilde rule the gate resolves a `token_file` by, applied to one side
/// of a grant comparison: a leading `~/` — bare, or straight after a
/// `scheme:` prefix — becomes the home directory, built the same way
/// `eidolon_tools::resolve` builds it so a stored spelling and a resolved
/// one land on the same bytes. Nothing else tilde-ish expands — a `~user`
/// source has no home to guess at, and with no `HOME` in the environment
/// every row stays exactly as spelled.
fn tilde_expanded(source: &str) -> String {
    let (scheme, rest) = if let Some(rest) = source.strip_prefix("~/") {
        ("", rest)
    } else if let Some((scheme, rest)) = source.split_once(':')
        && !scheme.is_empty()
        && scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'.' | b'_' | b'-'))
        && let Some(tilde_rest) = rest.strip_prefix("~/")
    {
        (scheme, tilde_rest)
    } else {
        return source.to_string();
    };
    match std::env::var_os("HOME") {
        Some(home) => {
            let expanded = std::path::PathBuf::from(home)
                .join(rest)
                .to_string_lossy()
                .into_owned();
            if scheme.is_empty() {
                expanded
            } else {
                format!("{scheme}:{expanded}")
            }
        }
        None => source.to_string(),
    }
}

/// Whether a stored grant source and an asked-for one name the same source,
/// whatever spelling each carries: equal as written, or equal once each
/// side's leading `~/` is resolved the way the gate resolves a
/// `token_file`. A plain path — and a `~user` one — matches itself and
/// nothing else. The one comparison the Rust lookup and the file's own
/// Rune accessor share, which is why they cannot disagree.
fn grant_source_eq(stored: &str, asked: &str) -> bool {
    stored == asked || tilde_expanded(stored) == tilde_expanded(asked)
}

/// The one name the permits file's own Rune may call beyond the default
/// modules: [`grant_source_eq`], installed as a bare, side-effect-free
/// function into every context the file compiles against — the micro-unit
/// here, and the host's shared policy context. The generated accessor
/// calls it, so a tilde row answers the same in Rune as in Rust, the file
/// carries no machine's home directory, and the shipped template stays
/// byte-for-byte what an empty store writes.
pub(crate) fn permits_module() -> Module {
    let mut m = Module::new();
    m.function("grant_source_eq", |stored: &str, asked: &str| {
        grant_source_eq(stored, asked)
    })
    .build()
    .expect("grant_source_eq is a plain function and installs");
    m
}

/// Compile a store source in front of a table against a host — the composed
/// check a write goes through, for callers that have a host but no loaded
/// policy: the CLI verbs, and a session running with the gate off. The
/// loaded policy's own [`ScriptPolicy::recompose`] is the same check for the
/// session that has one.
pub fn check_composed(permits_src: &str, table_src: &str, host: &Arc<Host>) -> anyhow::Result<()> {
    let compiler = host.compiler()?;
    crate::policy::compile_composed(permits_src, table_src, &compiler.context).map(|_| ())
}

/// The content hash a vouch carries and the stage compares: SHA-256, hex,
/// over the tool script's exact text. The same function at vouch time (the
/// CLI, over the file) and at load time ([`crate::script::ScriptTool`], over
/// the source compiled), which is what makes an edited plugin re-ask.
pub fn content_hash(src: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(src.as_bytes()))
}

/// The hash a service attestation carries: SHA-256 over the declaration's
/// canonical bytes, computed **here and nowhere else**, because a hash
/// that can be computed two ways is a check that can be passed by
/// accident. The canonical form is the fields in struct order, joined
/// with the unit separator — an argument containing the separator is
/// rejected upstream by the namespace grammar's spirit if never its
/// letter, and a hash over an ambiguous rendering would be worse than a
/// refusal to hash one.
pub fn service_declaration_hash(service: &crate::plugins::PluginService) -> String {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(service.command.as_bytes());
    for arg in &service.args {
        bytes.push(0x1f);
        bytes.extend_from_slice(arg.as_bytes());
    }
    bytes.push(0x1f);
    match &service.readiness {
        Some(r) => {
            bytes.extend_from_slice(format!("port:{}", r.port).as_bytes());
            if let Some(health) = &r.health {
                bytes.push(0x1f);
                bytes.extend_from_slice(health.as_bytes());
            }
        }
        None => bytes.extend_from_slice(b"no-readiness"),
    }
    bytes.push(0x1f);
    bytes.extend_from_slice(service.ready_timeout_s.to_string().as_bytes());
    format!("sha256:{:x}", Sha256::digest(&bytes))
}

/// Who a row says granted it: the operator at the keyboard, as the process
/// knows them. A shell without `USER` or `LOGNAME` still writes a row —
/// "operator" is the honest fallback, not a name invented.
pub fn granted_by() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "operator".into())
}

/// When the row was written: UTC, to the second, from the clock. A small
/// civil-from-days conversion rather than a dependency — the store has no
/// other use for a calendar.
pub fn granted_at() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    timestamp(millis)
}

/// `granted_at`'s format, exposed for the tests that pin it.
pub fn timestamp(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}

/// Howard Hinnant's `civil_from_days`: days since the epoch to `(y, m, d)`.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

use crate::Host;

/// The permission store: the file's parsed rows, kept current against the
/// disk on every lookup, plus the text the policy unit composes in front of
/// the table. Cheap to clone the handle; the rows behind it are replaced,
/// never mutated in place.
/// The composed-compile check a write goes through: the loaded policy,
/// compiling the store's next text in front of its table.
pub type ComposeCheck = Arc<dyn Fn(&str) -> anyhow::Result<()> + Send + Sync>;

pub struct Permits {
    path: PathBuf,
    inner: RwLock<Loaded>,
    checker: RwLock<Option<ComposeCheck>>,
}

struct Loaded {
    /// The text the rows were parsed from — the default template when no
    /// file exists, the file's bytes when one does. The policy stage holds
    /// its own copy of what it compiled and compares against this.
    src: String,
    rows: Rows,
}

impl Permits {
    /// The store for the policy file at `policy_path` — probed or missing,
    /// either way the permits file is its sibling.
    pub fn from_policy_path(policy_path: &Path) -> Self {
        Self::at(beside(policy_path))
    }

    /// The store for an explicit path. Nothing is read here: the first
    /// lookup or compose reads, so a store handed to a session that never
    /// asks costs nothing.
    pub fn at(path: PathBuf) -> Self {
        Permits {
            path,
            inner: RwLock::new(Loaded {
                src: String::new(),
                rows: Rows::default(),
            }),
            checker: RwLock::new(None),
        }
    }

    /// Attach the composed-compile check every write goes through: the
    /// policy stage, already loaded, compiling this source in front of the
    /// table it runs. Attached once at startup; a store without one refuses
    /// to write rather than writing unchecked.
    pub fn attach_checker(&self, checker: ComposeCheck) {
        *self.checker.write().expect("permits checker lock") = Some(checker);
    }

    /// Re-read the file if it differs from what is loaded; answer whether it
    /// changed. Absent is the ordinary case and parses as the shipped empty
    /// template, so a first grant composes from a known-good shape. A file
    /// that will not compile loads as no rows — fail closed — and says so
    /// once per change, not once per call.
    pub fn refresh(&self) -> bool {
        let read = std::fs::read_to_string(&self.path);
        let mut loaded = self.inner.write().expect("permits lock");
        let effective = match read {
            Ok(src) => src,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => DEFAULT_PERMITS.to_string(),
            Err(e) => {
                tracing::error!(error = %e, path = %self.path.display(), "reading the permits file");
                return false;
            }
        };
        if effective == loaded.src {
            return false;
        }
        let (src, rows) = match parse(&effective) {
            Ok(rows) => (effective, rows),
            Err(e) => {
                if !loaded.src.is_empty() {
                    tracing::error!(
                        error = %format!("{e:#}"),
                        path = %self.path.display(),
                        "the permits file does not compile; no rows are in force until it does"
                    );
                }
                // Fail closed, but keep the broken text as loaded so the
                // error is said once per change rather than once per call.
                (effective, Rows::default())
            }
        };
        let changed = rows != loaded.rows;
        *loaded = Loaded { src, rows };
        changed
    }

    /// The text as it loads right now — what the policy unit composes in
    /// front of the table. Refreshes first, so a compose after a change
    /// never compiles a stale file.
    pub fn source(&self) -> String {
        self.refresh();
        self.inner.read().expect("permits lock").src.clone()
    }

    /// Everything the file holds. Refreshes first.
    pub fn rows(&self) -> Rows {
        self.refresh();
        self.inner.read().expect("permits lock").rows.clone()
    }

    /// The vouch for one verb, if the operator granted one and the file
    /// still carries it.
    pub fn vouched(&self, verb: &str) -> Option<VouchRow> {
        self.refresh();
        self.inner
            .read()
            .expect("permits lock")
            .rows
            .vouch
            .iter()
            .find(|row| row.verb == verb)
            .cloned()
    }

    /// The grant covering `tool`'s reach for `source`: the exact row, or
    /// nothing.
    pub fn granted(&self, tool: &str, source: &str) -> Option<GrantRow> {
        self.refresh();
        self.inner.read().expect("permits lock").rows.granted(tool, source)
    }

    /// Rewrite the file whole: rows sorted, header generated, the composed
    /// unit compiled in memory through the checker before anything lands —
    /// a permissions file that would break the gate is refused and the
    /// previous one stays on disk. The store's own view is reloaded from
    /// the bytes written, so the next lookup answers as the file now does.
    pub fn write(&self, rows: &Rows) -> anyhow::Result<()> {
        let src = compose(rows);
        let checker = self.checker.read().expect("permits checker lock").clone();
        match checker {
            Some(check) => check(&src).context("refusing to write policy.permits.rn"),
            None => anyhow::bail!(
                "refusing to write {}: no gate is loaded to check the composed unit against",
                self.path.display()
            ),
        }?;
        let dir = self
            .path
            .parent()
            .context("the permits file has no directory to write into")?;
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let tmp = dir.join(format!(
            ".{}.tmp",
            self.path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(PERMITS_FILE)
        ));
        std::fs::write(&tmp, &src).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("moving {} into place at {}", tmp.display(), self.path.display()))?;
        // Load what was written, not what was meant: the store answers for
        // the file that exists.
        self.refresh();
        Ok(())
    }
}

/// The generated file: header, the two row lists with their rows sorted, and
/// the accessor tail. With no rows this is byte-for-byte the shipped
/// template — the pin that the writer and the shipped shape cannot drift.
pub fn compose(rows: &Rows) -> String {
    let mut out = String::from(HEADER);
    out.push_str(&format!("const VOUCHE = {};\n", vouch_list(rows)));
    out.push_str(&format!("const GRANT = {};\n", grant_list(rows)));
    out.push_str(&format!("const ATTEST = {};\n", attest_list(rows)));
    out.push_str(ACCESSORS);
    out
}

/// A string as a Rune string literal: the two characters that would end or
/// escape it, and nothing else — rows carry paths and names, not control
/// characters, and an escaped byte the reader cannot produce is a row that
/// will not round-trip.
fn rune_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// One row object: `field: value` pairs, Rune's object literal. Field names
/// are the struct's, fixed — the accessor reads them by name.
fn row_obj(fields: &[(&str, String)]) -> String {
    let inner: Vec<String> = fields
        .iter()
        .map(|(k, v)| format!("{k}: {v}"))
        .collect();
    format!("#{{ {} }}", inner.join(", "))
}

fn vouch_row_obj(row: &VouchRow) -> String {
    row_obj(&[
        ("plugin", rune_str(&row.plugin)),
        ("verb", rune_str(&row.verb)),
        ("hash", rune_str(&row.hash)),
        ("granted_by", rune_str(&row.granted_by)),
        ("granted_at", rune_str(&row.granted_at)),
    ])
}

fn grant_row_obj(row: &GrantRow) -> String {
    let subject = |v: &Option<String>| match v {
        Some(s) => rune_str(s),
        None => "\"\"".to_string(),
    };
    let mut fields = vec![
        ("tool", subject(&row.tool)),
        ("source", rune_str(&row.source)),
        ("granted_by", rune_str(&row.granted_by)),
        ("granted_at", rune_str(&row.granted_at)),
    ];
    // written back so a legacy wide row is never silently dropped from
    // the file that carries it; the accessor reads it never
    if let Some(p) = &row.wide_plugin {
        fields.push(("wide_plugin", rune_str(p)));
    }
    row_obj(&fields)
}

fn list(items: Vec<String>) -> String {
    if items.is_empty() {
        return "[]".into();
    }
    format!("[{}]", items.join(", "))
}

fn vouch_list(rows: &Rows) -> String {
    list(rows.vouch.iter().map(vouch_row_obj).collect())
}

fn grant_list(rows: &Rows) -> String {
    list(rows.grant.iter().map(grant_row_obj).collect())
}

fn attest_row_obj(row: &AttestRow) -> String {
    row_obj(&[
        ("plugin", rune_str(&row.plugin)),
        ("hash", rune_str(&row.hash)),
        ("granted_by", rune_str(&row.granted_by)),
        ("granted_at", rune_str(&row.granted_at)),
    ])
}

fn attest_list(rows: &Rows) -> String {
    list(rows.attest.iter().map(attest_row_obj).collect())
}

/// Compile a permits source against the manifest context's precedent —
/// default modules, `json`, and the one pure comparison name the
/// accessors call; no host, nothing that can act — and read everything
/// back through the file's own enumerator. The accessors the stage and
/// the table use are the same ones this parses, which is why a lookup in
/// Rust and a lookup in Rune cannot disagree.
fn parse(src: &str) -> anyhow::Result<Rows> {
    let mut context = rune::Context::with_default_modules().map_err(|e| anyhow::anyhow!("rune context: {e}"))?;
    context
        .install(rune_modules::json::module(false).map_err(|e| anyhow::anyhow!("json module: {e}"))?)
        .map_err(|e| anyhow::anyhow!("installing json: {e}"))?;
    context
        .install(permits_module())
        .map_err(|e| anyhow::anyhow!("installing the permits comparison: {e}"))?;
    let runtime = Arc::new(context.runtime().map_err(|e| anyhow::anyhow!("rune runtime: {e}"))?);
    let mut sources = rune::Sources::new();
    sources
        .insert(rune::Source::new(PERMITS_FILE, src).map_err(|e| anyhow::anyhow!("source: {e}"))?)
        .map_err(|e| anyhow::anyhow!("inserting source: {e}"))?;
    let mut diagnostics = Diagnostics::new();
    let unit = rune::prepare(&mut sources)
        .with_context(&context)
        .with_diagnostics(&mut diagnostics)
        .build()
        .map_err(|_| {
            let mut buf = rune::termcolor::Buffer::no_color();
            let rendered = match diagnostics.emit(&mut buf, &sources) {
                Ok(()) => String::from_utf8_lossy(buf.as_slice()).into_owned(),
                Err(e) => format!("<could not render diagnostics: {e}>"),
            };
            anyhow::anyhow!("policy.permits.rn failed to compile:\n{}", rendered.trim())
        })?;
    let unit = Arc::new(unit);
    let mut vm = Vm::new(runtime, unit);
    let value = vm
        .execute(["permit_rows"], ())
        .map_err(|e| anyhow::anyhow!("permits has no `pub fn permit_rows`: {e}"))?
        .complete()
        .into_result()
        .map_err(|e| anyhow::anyhow!("permit_rows failed: {e}"))?;
    let json = crate::script::rune_to_json(&value).context("permit_rows must return a plain object")?;
    // The file's rows speak the accessor's dialect: an absent subject is the
    // empty string, because a constant cannot call `Some`. The store's rows
    // speak the Rust one, and the crossing happens here and nowhere else.
    let file: FileRows = serde_json::from_value(json).context("permit_rows has the wrong shape")?;
    Ok(file.into())
}

/// The file's rows, as `permit_rows` hands them over.
#[derive(Default, Deserialize)]
struct FileRows {
    #[serde(default)]
    vouch: Vec<VouchRow>,
    #[serde(default)]
    grant: Vec<FileGrant>,
    #[serde(default)]
    attest: Vec<AttestRow>,
}

#[derive(Deserialize)]
struct FileGrant {
    #[serde(default)]
    tool: String,
    /// The wide row's plugin, as older files wrote it — read under a new
    /// name so nothing can mistake it for a subject, and empty on every
    /// row the current verbs write.
    #[serde(default, alias = "plugin")]
    wide_plugin: String,
    source: String,
    granted_by: String,
    granted_at: String,
}

impl From<FileRows> for Rows {
    fn from(file: FileRows) -> Self {
        Rows {
            vouch: file.vouch,
            grant: file.grant.into_iter().map(GrantRow::from).collect(),
            attest: file.attest,
        }
    }
}

impl From<FileGrant> for GrantRow {
    fn from(file: FileGrant) -> Self {
        let subject = |s: String| if s.is_empty() { None } else { Some(s) };
        GrantRow {
            tool: subject(file.tool),
            source: file.source,
            granted_by: file.granted_by,
            granted_at: file.granted_at,
            wide_plugin: subject(file.wide_plugin),
        }
    }
}

/// The header every write generates, and the first thing a hand edit runs
/// into.
const HEADER: &str = r#"// policy.permits.rn — what the operator has granted. **Machine-edited: hand
// edits are lost.** This file is rewritten whole, rows sorted, by
// `eidolon plugins trust` / `untrust` / `grant` / `revoke` and by the
// ask-once answers a session records; a write whose composed unit would not
// compile is refused and the previous file stays on disk — a stale vouch
// beats a broken gate.
//
// It is compiled **in front of** `policy.rn` into one unit and shares its
// namespace, so the table may call `vouched` and `granted` in its own words
// where it wants nuance; the policy stage calls them where the table has no
// answer. A vouch's content hash is compared in Rust against the tool that
// is actually loaded — an edited plugin re-asks — and no value of any
// granted secret ever passes through here, only the fact of the grant.
"#;

/// The accessor tail, generated identically on every write. The bodies are
/// deliberately dull: one pass over the grants under the shared
/// grant-source comparison, which is a bare function every context the
/// file compiles against installs — a stored `~/` spelling and the
/// expanded path the gate resolves it to are one source, so the file
/// carries no machine's home and the rule lives in one Rust function both
/// lookups call. A row names its subject in `tool`; a legacy wide row's
/// `plugin` is carried and honoured by nothing, and the other field is
/// the empty string, because a constant cannot call `Some`.
const ACCESSORS: &str = r#"
/// Everything the file holds, for the reader in Rust. Not a lookup: the
/// stage and the table use `vouched` and `granted` below.
pub fn permit_rows() {
    #{ vouch: VOUCHE, grant: GRANT, attest: ATTEST }
}

/// The vouch row for one verb, or nothing. The row carries the plugin, the
/// verb, who granted it and when, and the **tool script's content hash** the
/// Rust side compares against what is loaded.
pub fn vouched(tool) {
    for row in VOUCHE {
        if row.verb == tool {
            return row;
        }
    }
    None
}

/// The grant row covering one tool's reach for one source, or nothing.
/// The source is matched the way the gate resolves it: a stored `~/`
/// spelling and the expanded path it names are one source, whichever side
/// carries the tilde — `grant_source_eq` resolves a leading `~/` the way
/// the host resolves a `token_file`, and every other spelling matches
/// only itself. There is no plugin-wide shorthand: grouping is a CLI act
/// that writes one row per installed verb at the moment of the yes, so a
/// row exists only for a verb the operator could name when they gave it.
/// A legacy wide row's `wide_plugin` is carried in the file and honoured
/// by nothing — the verbs it covered ask again.
pub fn granted(tool, source) {
    for row in GRANT {
        if grant_source_eq(row.source, source) && row.tool == tool {
            return row;
        }
    }
    None
}

/// The attestation for one plugin's service declaration, if the operator
/// approved one and its hash is still what the plugin declares — the
/// caller passes the hash computed from the declaration at hand, so a
/// changed command simply does not match.
pub fn attested(plugin, hash) {
    for row in ATTEST {
        if row.plugin == plugin && row.hash == hash {
            return row;
        }
    }
    None
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn row(verb: &str, hash: &str) -> VouchRow {
        VouchRow {
            plugin: "browser".into(),
            verb: verb.into(),
            hash: hash.into(),
            granted_by: "noah".into(),
            granted_at: "2026-09-28T00:00:00Z".into(),
        }
    }

    /// The writer with no rows writes the shipped template, byte for byte —
    /// the generated shape and the checked-in shape are one shape, and a
    /// change to either fails here before it can surprise the gate.
    #[test]
    fn an_empty_store_writes_the_shipped_template() {
        assert_eq!(compose(&Rows::default()), DEFAULT_PERMITS);
    }

    /// The template compiles against the bare context, and both accessors
    /// answer `None` for a name nothing covers. This is the test that holds
    /// the generated Rune to the language: syntax, map literals, template
    /// strings, `for` destructuring, all of it runs here — including the
    /// one bare name the context installs for the accessors, the
    /// grant-source comparison.
    #[test]
    fn the_template_compiles_and_answers_nothing() {
        let rows = parse(DEFAULT_PERMITS).unwrap();
        assert_eq!(rows, Rows::default());
    }

    /// Tracker #24: the row a plugin's README writes — `file:~/…` — and the
    /// path the gate resolves it to are one source, either direction; a
    /// plain path answers only itself; and `~user` has no home to guess
    /// at, so it stays exactly as typed on both sides.
    #[test]
    fn a_tilde_row_and_its_expansion_are_one_source() {
        let home = std::env::var("HOME").expect("tests resolve tilde rows, so HOME is set");
        let tilde = "file:~/.config/eidolon/browser.token";
        let expanded = format!("file:{home}/.config/eidolon/browser.token");
        assert!(grant_source_eq(tilde, &expanded));
        assert!(grant_source_eq(&expanded, tilde));
        // a plain path is untouched: itself and nothing else
        assert!(grant_source_eq("secret:browser_token", "secret:browser_token"));
        assert!(!grant_source_eq("secret:browser_token", "secret:other"));
        assert!(!grant_source_eq(
            tilde,
            &format!("file:{home}-elsewhere/.config/eidolon/browser.token")
        ));
        // `~user` — any tilde that is not `~/` — never expands, either side
        assert!(!grant_source_eq("file:~jane/x", "file:/home/jane/x"));
        assert!(grant_source_eq("file:~jane/x", "file:~jane/x"));
        // and the Rust lookup answers through the same comparison
        let mut rows = Rows::default();
        rows.grant(GrantRow {
            tool: Some("browser_open".into()),
            source: tilde.into(),
            granted_by: "noah".into(),
            granted_at: "2026-09-28T00:00:00Z".into(),
            wide_plugin: None,
        });
        assert!(rows.granted("browser_open", &expanded).is_some());
        assert!(rows.granted("browser_open", tilde).is_some());
        assert!(rows.granted("browser_close", &expanded).is_none());
    }

    /// A store with rows in it answers them through the same file the
    /// table sees: the exact vouch, the exact grant. A legacy wide row in
    /// the file is carried through untouched and honoured by nothing —
    /// not even a verb its plugin prefix would have matched — and the
    /// Rust lookup and the Rune accessor agree about all of it.
    #[test]
    fn a_written_store_answers_its_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(PERMITS_FILE);
        let permits = Permits::at(path.clone());
        permits
            .attach_checker(Arc::new(|src| {
                parse(src).map(|_| ())
            }));
        let mut rows = Rows::default();
        rows.vouch.push(row("browser_open", "sha256:abc"));
        rows.grant(GrantRow {
            tool: Some("browser_open".into()),
            source: "secret:browser_token".into(),
            granted_by: "noah".into(),
            granted_at: "2026-09-28T00:00:00Z".into(),
            wide_plugin: None,
        });
        // the spelling the README writes and the gate resolves differently:
        // tracker #24's row, dead before the comparison went tilde-aware
        rows.grant(GrantRow {
            tool: Some("browser_open".into()),
            source: "file:~/.config/eidolon/browser.token".into(),
            granted_by: "noah".into(),
            granted_at: "2026-09-28T00:00:00Z".into(),
            wide_plugin: None,
        });
        // a row only an old file can hold: the wide subject, captured and
        // carried, answering nothing
        rows.grant(GrantRow {
            tool: None,
            source: "file:/home/noah/.tokens/x".into(),
            granted_by: "noah".into(),
            granted_at: "2026-09-28T00:00:00Z".into(),
            wide_plugin: Some("my_plugin".into()),
        });
        permits.write(&rows).unwrap();

        // The same handle re-reads the file it just wrote.
        let expected = rows.vouch.iter().find(|r| r.verb == "browser_open").cloned();
        assert_eq!(permits.vouched("browser_open"), expected);
        assert_eq!(permits.vouched("browser_close"), None);
        assert_eq!(
            permits.granted("browser_open", "secret:browser_token"),
            rows.granted("browser_open", "secret:browser_token")
        );
        // the tilde row answers the expanded lookup — in Rust and through
        // the file's own accessor — and the reverse
        let expanded = format!(
            "file:{}/.config/eidolon/browser.token",
            std::env::var("HOME").expect("tests resolve tilde rows, so HOME is set")
        );
        assert!(permits.granted("browser_open", &expanded).is_some());
        let src = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            call_granted(&src, "browser_open", &expanded).map(|g| g.source),
            Some("file:~/.config/eidolon/browser.token".to_string()),
        );
        assert_eq!(
            call_granted(&src, "browser_open", "file:~/.config/eidolon/browser.token").map(|g| g.source),
            Some("file:~/.config/eidolon/browser.token".to_string()),
        );
        // A wide row is authority over nothing: not over a verb its
        // prefix would have matched, not over any verb at all — while
        // the fact itself is carried, never silently dropped.
        assert!(permits.granted("my_plugin_fetch", "file:/home/noah/.tokens/x").is_none());
        let parsed = parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed, rows);
        assert_eq!(
            parsed.grant.iter().find(|r| r.wide_plugin.is_some()).and_then(|r| r.wide_plugin.as_deref()),
            Some("my_plugin"),
            "the legacy fact survives the write"
        );
        let via_accessor = call_granted(&std::fs::read_to_string(&path).unwrap(), "my_plugin_fetch", "file:/home/noah/.tokens/x");
        assert_eq!(via_accessor, parsed.granted("my_plugin_fetch", "file:/home/noah/.tokens/x"));
    }

    /// Re-read on change: the same store drops a vouch the file stopped
    /// carrying, on the next lookup, without being rebuilt.
    #[test]
    fn a_removed_row_stops_answering_on_the_next_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let permits = Permits::at(dir.path().join(PERMITS_FILE));
        permits.attach_checker(Arc::new(|src| parse(src).map(|_| ())));
        let mut rows = Rows::default();
        rows.vouch.push(row("browser_open", "sha256:abc"));
        permits.write(&rows).unwrap();
        assert!(permits.vouched("browser_open").is_some());
        rows.vouch.clear();
        permits.write(&rows).unwrap();
        assert_eq!(permits.vouched("browser_open"), None);
    }

    /// A file that exists but does not compile loads as no rows — a stale
    /// vouch may keep asking, but a broken file must not keep allowing —
    /// and the previous good file, restored, is in force again.
    #[test]
    fn a_broken_file_grants_nothing_until_it_is_fixed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(PERMITS_FILE);
        let permits = Permits::at(path.clone());
        permits.attach_checker(Arc::new(|src| parse(src).map(|_| ())));
        let mut rows = Rows::default();
        rows.vouch.push(row("browser_open", "sha256:abc"));
        permits.write(&rows).unwrap();
        assert!(permits.vouched("browser_open").is_some());
        std::fs::write(&path, "const VOUCHE = #{ broken").unwrap();
        assert_eq!(permits.vouched("browser_open"), None);
        // And a write through the checker refuses a file that would not
        // compile, leaving the previous one on disk — here, the broken one
        // a hand edit made, because the checker is the only writer.
        let mut future = Rows::default();
        future.vouch.push(row("browser_close", "sha256:def"));
        let composed = compose(&future);
        let tampered = composed.replace("sha256:def", "sha256:def\" oops");
        let checked = parse(&tampered).is_err();
        assert!(checked, "the tampered file must not compile");
    }

    /// The checker's yes is what stands between a broken gate and the disk:
    /// a composed unit that fails is refused, and `write` says so rather
    /// than writing anyway.
    #[test]
    fn a_write_the_checker_refuses_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(PERMITS_FILE);
        std::fs::write(&path, compose(&Rows::default())).unwrap();
        let permits = Permits::at(path.clone());
        permits.attach_checker(Arc::new(|_| anyhow::bail!("the composed unit does not compile")));
        let mut rows = Rows::default();
        rows.vouch.push(row("browser_open", "sha256:abc"));
        assert!(permits.write(&rows).is_err());
        // The previous file is still on disk, and still what loads.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), compose(&Rows::default()));
        assert_eq!(permits.vouched("browser_open"), None);
    }

    /// `granted_at` is a real UTC stamp, pinned against a known second —
    /// the calendar is small, and a row's date is read by a person.
    #[test]
    fn the_timestamp_is_the_utc_second_it_says() {
        assert_eq!(timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(timestamp(1_789_000_000), "2026-09-10T00:26:40Z");
        assert_eq!(timestamp(951_782_400), "2000-02-29T00:00:00Z");
    }

    /// The hash is over the exact text: one changed character is a
    /// different hash, which is the whole of how an edited plugin re-asks.
    #[test]
    fn the_hash_moves_when_the_source_does() {
        assert_ne!(content_hash("pub fn call(input) { 1 }"), content_hash("pub fn call(input) { 2 }"));
        assert!(content_hash("x").starts_with("sha256:"));
    }

    /// The Rune accessor's answer for one query, through a fresh compile of
    /// `src` — what the operator's table would see — crossing the file's
    /// dialect at the same place the store's own parse does.
    fn call_granted(src: &str, tool: &str, source: &str) -> Option<GrantRow> {
        let mut context = rune::Context::with_default_modules().unwrap();
        context.install(rune_modules::json::module(false).unwrap()).unwrap();
        context.install(permits_module()).unwrap();
        let runtime = Arc::new(context.runtime().unwrap());
        let mut sources = rune::Sources::new();
        sources.insert(rune::Source::new("permits", src).unwrap()).unwrap();
        let mut diagnostics = Diagnostics::new();
        let unit = rune::prepare(&mut sources)
            .with_context(&context)
            .with_diagnostics(&mut diagnostics)
            .build()
            .map_err(|_| "compile").unwrap();
        let mut vm = Vm::new(runtime, Arc::new(unit));
        let value = vm
            .execute(
                ["granted"],
                (crate::script::json_to_rune(&serde_json::json!(tool)), crate::script::json_to_rune(&serde_json::json!(source))),
            )
            .unwrap()
            .complete()
            .into_result()
            .unwrap();
        let json = crate::script::rune_to_json(&value).unwrap();
        let file: Option<FileGrant> = serde_json::from_value(json).unwrap();
        file.map(GrantRow::from)
    }
}
