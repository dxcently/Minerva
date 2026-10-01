//! The record side of the plugin system: `plugins.toml`, the file beside
//! the plugins directory that carries what the *operator* has decided
//! about each plugin, as against what the *directory* decides is present.
//! The directory's contents are the truth about what exists — discovery
//! reads them and nothing else — and this file is the truth about what is
//! managed: an entry can disable a plugin (present, listed, not adopted)
//! and, once install lands, say where a plugin came from. A directory
//! with no entry here is enabled all the same — provenance `unmanaged —
//! installed by hand` — so deleting the file changes nothing, and a
//! hand-installed plugin behaves exactly as it always did.
//!
//! Operator-owned, harness-written: the `plugins enable` / `disable`
//! verbs rewrite it whole, rows sorted, the way the permission store is
//! written, and nothing else in the harness writes it. It is not part of
//! what Nix generates — it records the operator's decisions about
//! third-party code, not harness configuration. A broken file is an error
//! to the verbs (which write, and must not clobber a file they could not
//! read) and fails open to the readers (adoption, the ui merge), which
//! can write nothing and treat an unreadable record as no record.
//!
//! (vault, *Plugin Packaging (spec)*, §"The record is central" and
//! §"What the directory decides and what the file decides".)

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, anyhow};
use serde::{Deserialize, Serialize};

/// The store's file name. Always beside the plugins directory —
/// `<config>/plugins.toml` next to `<config>/plugins/` — so every reader
/// derives it the same one way and no configuration field can point one
/// reader's store somewhere another reader's is not.
pub const STORE_FILE: &str = "plugins.toml";

/// One plugin's record. A key with an empty table is meaningful — it
/// marks the plugin *managed*, whatever later fields add — so every
/// field defaults individually and absence of the struct is the only
/// `unmanaged`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Whether the plugin is adopted and its payloads load. Absent means
    /// enabled: an entry that only says "managed" does not disable.
    /// Disabling revokes nothing — the vouch and grant rows stay — so
    /// re-enabling asks nobody again.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Where an installed plugin came from, as the installer was told:
    /// `owner/repo` or a full git URL, and the folder inside the repo.
    /// Absent together with the fields below for a plugin the record
    /// marks managed without an install — the honest "installed by hand".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The ref the installer was given, or the default branch it resolved
    /// to. A branch can move; the commit is what was actually taken.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "ref")]
    pub reference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// The manifest's `version` when installed or updated — a claim, kept
    /// for the listing, never the update check's ground truth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_by: Option<String>,
    /// Every file as installed, path → content hash: what the update
    /// check compares the installed tree against to tell "the plugin
    /// moved" from "you moved the plugin".
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub files: BTreeMap<String, String>,
}

impl Entry {
    /// The listing's provenance line — `from owner/repo#path@ref (commit)`
    /// — or nothing for a row that records no source. A commit abbreviated
    /// the way git names commits, because the full one is one `git show`
    /// away and the line is for a person.
    pub fn provenance_line(&self) -> Option<String> {
        match (&self.repo, &self.path, &self.reference, &self.commit) {
            (Some(repo), Some(path), Some(reference), Some(commit)) => Some(format!(
                "from {repo}#{path}@{reference} ({})",
                &commit[..commit.len().min(7)]
            )),
            _ => None,
        }
    }
}

fn default_enabled() -> bool {
    true
}

/// The hand-written default exists so the serde default and the Rust one
/// cannot drift: an entry that never held an opinion is *enabled* — the
/// absent opinion means enabled, in a file and in memory alike. The
/// derived one would say `false`, and a row minted by `or_default` would
/// then disable a plugin nobody disabled.
impl Default for Entry {
    fn default() -> Self {
        Self {
            enabled: true,
            repo: None,
            path: None,
            reference: None,
            commit: None,
            version: None,
            installed_at: None,
            installed_by: None,
            files: BTreeMap::new(),
        }
    }
}

/// The file's serialized shape: one table per plugin, keyed by name —
/// the namespace, the same key `eidolon plugins` lists by.
#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    plugins: BTreeMap<String, Entry>,
}

/// The loaded record: the plugins' entries and the file they came from,
/// kept for the verbs' whole rewrite and the listing's provenance lines.
#[derive(Debug, Clone, Default)]
pub struct Store {
    path: PathBuf,
    plugins: BTreeMap<String, Entry>,
}

impl Store {
    /// Where the record for this plugins directory lives — its sibling
    /// file, one directory up from `plugins/`.
    pub fn beside(plugins_dir: &Path) -> PathBuf {
        plugins_dir.with_file_name(STORE_FILE)
    }

    /// Read the record. A missing file is the ordinary case — an empty
    /// store, everything unmanaged — and only a file that is there and
    /// cannot parse is an error: the verbs must not rewrite a record
    /// they read as noise, and the error names the file and the reason.
    pub fn load(plugins_dir: &Path) -> anyhow::Result<Self> {
        let path = Self::beside(plugins_dir);
        let plugins = match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str::<File>(&text)
                .map(|f| f.plugins)
                .with_context(|| format!("reading {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => {
                return Err(anyhow!("reading {}: {e}", path.display()));
            }
        };
        Ok(Store { path, plugins })
    }

    /// The readers' question: is this plugin adopted, do its payloads
    /// load? Fails open — a record that is missing or unreadable says
    /// nothing, and a plugin the directory holds is then enabled, the
    /// same as it was before the record existed. The readers never
    /// write, so an unreadable file costs nothing they could destroy.
    pub fn is_enabled(plugins_dir: &Path, name: &str) -> bool {
        match Self::load(plugins_dir) {
            Ok(store) => store.enabled(name),
            Err(_) => true,
        }
    }

    /// The entry for one plugin, or `None` when it is unmanaged — the
    /// listing's provenance line reads this directly.
    pub fn entry(&self, name: &str) -> Option<&Entry> {
        self.plugins.get(name)
    }

    /// Whether one plugin is enabled: an absent entry is enabled, the
    /// unmanaged case, and an entry without `enabled` is managed and on.
    pub fn enabled(&self, name: &str) -> bool {
        self.entry(name).is_none_or(|e| e.enabled)
    }

    /// The whole record, for the listing's missing-entry pass: entries
    /// whose directory is gone are named, never dropped.
    pub fn plugins(&self) -> &BTreeMap<String, Entry> {
        &self.plugins
    }

    /// Where the record lives, for messages that name it.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Set one plugin's enabled flag, marking it managed — a record is
    /// created for a plugin that had none, because `plugins disable`
    /// must survive the plugin being reinstalled by hand later.
    pub fn set_enabled(&mut self, name: &str, enabled: bool) {
        let entry = self.plugins.entry(name.to_string()).or_default();
        entry.enabled = enabled;
    }

    /// The row for one plugin, created managed-empty when absent — the
    /// installer's and updater's writer, which fills the source fields
    /// and leaves `enabled` exactly as the operator left it.
    pub fn entry_mut(&mut self, name: &str) -> &mut Entry {
        self.plugins.entry(name.to_string()).or_default()
    }

    /// Drop one plugin's row — the uninstaller's writer, and the **only**
    /// act that shrinks the record. `missing` is for a directory that
    /// vanished without an act; an uninstall is an act, and a record that
    /// went on calling it `missing` would be the listing explaining a
    /// decision the operator already made. Answers whether a row was
    /// there, so the caller can say which of the two it removed.
    pub fn forget(&mut self, name: &str) -> bool {
        self.plugins.remove(name).is_some()
    }

    /// Rewrite whole, rows sorted — a `BTreeMap`'s order — through a
    /// temp file and a rename, the permission store's discipline: a
    /// write that fails halfway leaves the previous record on disk.
    pub fn write(&self) -> anyhow::Result<()> {
        let text = toml::to_string_pretty(&File {
            plugins: self.plugins.clone(),
        })
        .context("rendering the plugin record")?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("creating {}", parent.display())
            })?;
        }
        let temp = self.path.with_extension("toml.new");
        std::fs::write(&temp, text).with_context(|| format!("writing {}", temp.display()))?;
        std::fs::rename(&temp, &self.path)
            .with_context(|| format!("moving {} into place", temp.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Host, register_plugins};

    /// A scratch plugins directory with its store beside it — never the
    /// operator's real home.
    fn scratch() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let plugins = dir.path().join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        (dir, plugins)
    }

    /// A missing record is the ordinary case: an empty store, everything
    /// enabled, and `is_enabled` — the readers' door — answering yes to
    /// a name it has never heard of.
    #[test]
    fn a_missing_record_enables_everything() {
        let (_dir, plugins) = scratch();
        let store = Store::load(&plugins).unwrap();
        assert!(store.plugins().is_empty());
        assert!(store.enabled("anything"));
        assert!(Store::is_enabled(&plugins, "anything"));
        assert_eq!(store.entry("anything"), None);
    }

    /// The verbs' round trip: disable, rewrite, read back — the flag on
    /// disk, the key marked managed, and the record sorted.
    #[test]
    fn a_disabled_entry_round_trips_through_the_file() {
        let (_dir, plugins) = scratch();
        let mut store = Store::load(&plugins).unwrap();
        store.set_enabled("pinger", false);
        store.set_enabled("alpha", true);
        store.write().unwrap();

        let text = std::fs::read_to_string(Store::beside(&plugins)).unwrap();
        assert!(text.contains("[plugins.alpha]"), "{text}");
        assert!(text.contains("[plugins.pinger]"), "{text}");
        assert!(text.contains("enabled = false"), "{text}");

        let store = Store::load(&plugins).unwrap();
        assert!(!store.enabled("pinger"));
        assert!(store.enabled("alpha"));
        assert_eq!(
            store.entry("pinger"),
            Some(&Entry {
                enabled: false,
                ..Entry::default()
            })
        );
    }

    /// A bare entry — a key with no `enabled` — is managed and enabled:
    /// marking a plugin managed is not disabling it.
    #[test]
    fn a_bare_entry_is_managed_and_enabled() {
        let (_dir, plugins) = scratch();
        std::fs::write(
            Store::beside(&plugins),
            "[plugins.\"pinger\"]\n",
        )
        .unwrap();
        let store = Store::load(&plugins).unwrap();
        assert_eq!(store.plugins().len(), 1);
        assert!(store.enabled("pinger"));
    }

    /// A record that cannot parse is an error to the verbs — they write,
    /// and must not clobber a file they read as noise — while the
    /// readers' door fails open: the plugin stays enabled, the same as
    /// before any record existed.
    #[test]
    fn a_broken_record_is_an_error_to_the_verbs_and_open_to_the_readers() {
        let (_dir, plugins) = scratch();
        std::fs::write(Store::beside(&plugins), "[plugins\n").unwrap();
        assert!(Store::load(&plugins).is_err());
        assert!(Store::is_enabled(&plugins, "pinger"));
    }

    /// A disabled plugin is present but not adopted: `register_plugins`
    /// skips it without a note — the listing is the operator's surface
    /// for the decision — and re-enabling it restores adoption, which is
    /// why disabling revokes nothing.
    #[test]
    fn a_disabled_plugin_is_not_adopted() {
        let dir = tempfile::tempdir().unwrap();
        let plugins = dir.path().join("plugins");
        for name in ["kept", "shut"] {
            let p = plugins.join(name);
            std::fs::create_dir_all(p.join("tools")).unwrap();
            std::fs::write(
                p.join("plugin.rn"),
                format!("pub fn manifest() {{ #{{ name: \"{name}\", description: \"t\" }} }}"),
            )
            .unwrap();
            std::fs::write(
                p.join("tools").join("ping.rn"),
                "pub fn manifest() { #{ name: \"ping\", description: \"t\", input_schema: #{ \"type\": \"object\" } } }\npub async fn call(input) { \"pong\" }",
            )
            .unwrap();
        }
        std::fs::write(
            Store::beside(&plugins),
            "[plugins.shut]\nenabled = false\n",
        )
        .unwrap();

        let host = std::sync::Arc::new(Host::new(dir.path().to_path_buf()));
        let mut registry = eidolon_core::tool::ToolRegistry::default();
        let notes = register_plugins(&mut registry, &host, &plugins);
        assert!(notes.is_empty(), "{notes:?}");
        assert!(registry.get("kept_ping").is_some());
        assert!(registry.get("shut_ping").is_none());

        // Re-enabled, the same directory adopts — nothing was revoked.
        std::fs::write(Store::beside(&plugins), "").unwrap();
        let mut registry = eidolon_core::tool::ToolRegistry::default();
        register_plugins(&mut registry, &host, &plugins);
        assert!(registry.get("shut_ping").is_some());
    }

    /// `forget` is the one writer that shrinks the record, and it answers
    /// whether it had anything to shrink — the caller says which of the
    /// two things it removed, so the listing cannot explain a decision the
    /// operator already made.
    #[test]
    fn forget_removes_one_row_and_says_whether_it_was_there() {
        let tmp = tempfile::tempdir().unwrap();
        let plugins = tmp.path().join("plugins");
        std::fs::create_dir_all(&plugins).unwrap();
        let mut store = Store::load(&plugins).unwrap();
        store.set_enabled("kept", false);
        store.set_enabled("gone", true);
        store.write().unwrap();

        assert!(store.forget("gone"), "a row was there");
        assert!(!store.forget("gone"), "and the second time it was not");
        assert!(!store.forget("never"), "nor is a name that was never recorded");
        store.write().unwrap();

        let read_back = Store::load(&plugins).unwrap();
        assert!(read_back.entry("gone").is_none());
        assert!(read_back.entry("never").is_none());
        assert_eq!(
            read_back.entry("kept").map(|e| e.enabled),
            Some(false),
            "the row beside it is untouched"
        );
    }
}
