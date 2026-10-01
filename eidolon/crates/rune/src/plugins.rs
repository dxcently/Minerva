//! Plugin discovery: the installed-plugins directory and its manifests.
//!
//! A plugin is a directory under the plugins directory, named for the
//! namespace it contributes under, and the directory's contents are
//! authoritative: `plugin.rn` at the root carrying `pub fn manifest()`,
//! and any of the payload directories beside it. The manifest does not
//! enumerate the payloads — a declaration that lists its own payloads is
//! a second place for the listing to be wrong — so discovery reads the
//! directory for what is present and the manifest only for what the
//! plugin says about itself. (vault, *Plugin Packaging (spec)*.)
//!
//! Discovery is lazy by contract: nothing in the startup path calls into
//! this module. `eidolon plugins` and adoption read it on demand. A
//! manifest compiles against a fresh default-modules context plus `json`
//! — no host, no eidolon primitives — because a manifest is metadata: it
//! never runs again, and no primitive belongs in it.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, anyhow};
use rune::{Context, Vm};
use serde::Deserialize;

use crate::script::{compile_source, rune_to_json};

/// The manifest file every plugin carries at its root.
pub const PLUGIN_FILE: &str = "plugin.rn";

/// The payload directories a plugin may carry, in presentation order.
/// `lib/` is shared Rune compiled in front of the payloads that name it;
/// the rest hold the payloads themselves.
pub const PAYLOAD_DIRS: &[&str] = &["lib", "tools", "ui", "workflows"];

/// What `manifest()` returns. `name` is required and must equal the
/// directory name — two identities for one plugin is how a listing and a
/// log come to disagree. Everything else defaults: a plugin with no
/// description is undersold, not broken.
#[derive(Debug, Clone, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    /// What the plugin says about itself: payload names it carries,
    /// secrets it wants, policy it wants installed. **Nothing believes a
    /// claim and nothing gates on one**: they exist to be shown to a
    /// person, and an effect is gated by the one table like any other
    /// caller's. Keys are free-form; they print in sorted order.
    #[serde(default)]
    pub claims: BTreeMap<String, Vec<String>>,
    /// The daemon this plugin's tools reach, when it ships one: what to run
    /// and how to tell it is up. **A declaration only — nothing here runs
    /// it.** The operator does, with `eidolon plugins service start`, which
    /// asks once and holds the answer against the hash of this block, so an
    /// update that changes the command asks again. (vault, *Plugin
    /// Packaging (spec)*, "Services v1".)
    #[serde(default)]
    pub service: Option<PluginService>,
}

/// A service a plugin's tools reach through `api_request`.
#[derive(Debug, Clone, Deserialize)]
pub struct PluginService {
    /// The program. With no `/` it is resolved on `$PATH`; with one it is a
    /// path, and `{dir}` may stand for the plugin's own directory — so a
    /// plugin can ship its own launcher and need no install step.
    pub command: String,
    /// The arguments, as **an argv list, never a string a shell
    /// interprets**. `{dir}` in any element expands to the plugin directory
    /// as one element, so a path containing a space cannot become two.
    #[serde(default)]
    pub args: Vec<String>,
    /// How to tell it is up. Absent, an operator `start` returns as soon as
    /// the child is spawned and `status` says so rather than guessing.
    #[serde(default)]
    pub readiness: Option<Readiness>,
    /// How long readiness has to arrive before the start is called failed.
    #[serde(default = "default_ready_timeout_s")]
    pub ready_timeout_s: u64,
}

/// Readiness for a service that speaks on a port: a TCP connect, and — when
/// a path is named — a 2xx from `http://127.0.0.1:<port><health>`.
#[derive(Debug, Clone, Deserialize)]
pub struct Readiness {
    pub port: u16,
    #[serde(default)]
    pub health: Option<String>,
}

fn default_ready_timeout_s() -> u64 {
    30
}

/// One entry of discovery: a plugin whose manifest compiled, or the
/// reason one did not. Broken is a per-plugin outcome, never a command
/// failure — the rest of the directory still lists.
#[derive(Debug)]
pub enum Plugin {
    Installed {
        dir: PathBuf,
        manifest: PluginManifest,
    },
    Broken {
        dir: PathBuf,
        reason: String,
    },
}

impl Plugin {
    /// The directory's name — the plugin's namespace either way.
    pub fn name(&self) -> &str {
        let dir = match self {
            Self::Installed { dir, .. } | Self::Broken { dir, .. } => dir,
        };
        dir.file_name().and_then(|n| n.to_str()).unwrap_or("<unnamed>")
    }
}

/// A plugin's name — its namespace — is `[a-z][a-z0-9_]*`: what
/// `eidolon plugins` prints, what a run's pin records, what a log line
/// can be grepped for.
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    if !matches!(chars.next(), Some(c) if c.is_ascii_lowercase()) {
        return false;
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Read the plugins directory: one entry per subdirectory, sorted by
/// name. A missing directory is no plugins, not an error — most installs
/// have none. Only the manifest is compiled here, never a payload.
pub fn discover(plugins_dir: &Path) -> anyhow::Result<Vec<Plugin>> {
    let read = match fs::read_dir(plugins_dir) {
        Ok(read) => read,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(e).with_context(|| format!("reading {}", plugins_dir.display()));
        }
    };
    let mut plugins = Vec::new();
    for entry in read {
        let path = entry
            .with_context(|| format!("reading {}", plugins_dir.display()))?
            .path();
        if path.is_dir() {
            plugins.push(read_plugin(&path));
        }
    }
    plugins.sort_by(|a, b| a.name().cmp(b.name()));
    Ok(plugins)
}

fn read_plugin(dir: &Path) -> Plugin {
    let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if !valid_name(name) {
        return Plugin::Broken {
            dir: dir.to_path_buf(),
            reason: format!("directory name `{name}` is not [a-z][a-z0-9_]*"),
        };
    }
    if !dir.join(PLUGIN_FILE).is_file() {
        return Plugin::Broken {
            dir: dir.to_path_buf(),
            reason: format!("no {PLUGIN_FILE} at the root"),
        };
    }
    match load_manifest(dir) {
        Ok(manifest) if manifest.name == name => Plugin::Installed {
            dir: dir.to_path_buf(),
            manifest,
        },
        Ok(manifest) => Plugin::Broken {
            dir: dir.to_path_buf(),
            reason: format!("manifest says name `{}`, directory is `{name}`", manifest.name),
        },
        Err(e) => Plugin::Broken {
            dir: dir.to_path_buf(),
            reason: format!("{e:#}"),
        },
    }
}

/// Compile `plugin.rn` and run its `manifest()`. The recipe is the host
/// compiler's minus the eidolon module: a manifest that reaches for a
/// primitive is a category error, and the compile says so.
fn load_manifest(dir: &Path) -> anyhow::Result<PluginManifest> {
    let path = dir.join(PLUGIN_FILE);
    let src = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let mut context = Context::with_default_modules().map_err(|e| anyhow!("rune context: {e}"))?;
    context
        .install(rune_modules::json::module(false).map_err(|e| anyhow!("json module: {e}"))?)
        .map_err(|e| anyhow!("installing json: {e}"))?;
    let runtime = Arc::new(context.runtime().map_err(|e| anyhow!("rune runtime: {e}"))?);
    let unit = compile_source(&context, &path.display().to_string(), &src)?;
    let mut vm = Vm::new(runtime, unit.clone());
    let value = vm
        .execute(["manifest"], ())
        .map_err(|e| anyhow!("script has no `pub fn manifest()`: {e}"))?
        .complete()
        .into_result()
        .map_err(|e| anyhow!("manifest() failed: {e}"))?;
    let json = rune_to_json(&value).context("manifest() must return a plain object")?;
    serde_json::from_value(json).context("manifest() has the wrong shape")
}

/// Which payload directories are present with at least one `*.rn` in
/// them. Read from disk: the directory's contents are the truth, and the
/// manifest's claims are what the plugin *says*, not what is here.
pub fn payloads_present(dir: &Path) -> Vec<&'static str> {
    PAYLOAD_DIRS
        .iter()
        .filter(|d| has_rn_source(dir.join(d)))
        .copied()
        .collect()
}

fn has_rn_source(dir: PathBuf) -> bool {
    fs::read_dir(dir)
        .map(|read| {
            read.filter_map(|e| e.ok())
                .any(|e| e.path().extension().is_some_and(|x| x == "rn"))
        })
        .unwrap_or(false)
}
