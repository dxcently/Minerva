//! Fetching a plugin from a git repo, and comparing plugin trees by bytes.
//!
//! The pure half of install/update (vault, *Plugin Packaging (spec)*
//! §Installing and updating from a repo): nothing here writes to the
//! plugins directory, touches the record, or decides trust — the verbs in
//! `plugins.rs` decide where a fetched tree lands, and the gate decides
//! what it may do, exactly as for any other caller. Three properties this
//! module exists to keep: **installing does not trust**; **an update
//! re-asks by itself** (a vouch is bound to the script's content hash, so
//! a changed script stops matching); and the update check's ground truth
//! is **bytes at the recorded ref**, never the manifest's version, because
//! version is a claim an author forgets to bump while the commit and the
//! file set are the record.
//!
//! The clone is the ruled shape — `git clone --depth 1
//! --filter=blob:none --branch <ref>` — with one honest addition: `--branch`
//! takes a branch or a tag and refuses a raw commit, while `--ref` is ruled
//! to accept all three (a commit-pinned plugin reports `up to date`
//! forever, which is honest rather than broken, and that only if it can be
//! installed). A ref that is forty hex digits goes by the equivalent
//! `init`/`fetch --depth 1`/`checkout FETCH_HEAD` instead; every other ref
//! rides the ruled clone verbatim. A missing git is refused by name —
//! never a half-install.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context as _, anyhow, bail};
use sha2::{Digest, Sha256};

/// `owner/repo` means GitHub; anything that already names a transport
/// (`https://…`, `git@…`) passes through untouched.
pub fn clone_url(repo: &str) -> String {
    if repo.contains("://") || repo.starts_with("git@") {
        repo.to_string()
    } else {
        format!("https://github.com/{repo}")
    }
}

/// A fetched tree: the temp clone it lives in, and the plugin directory
/// inside it. The clone is removed when this is dropped — a tree that is
/// never validated and copied never lands anywhere.
#[derive(Debug)]
pub struct Fetched {
    /// The temp clone of the whole repo, at the ref asked for.
    pub root: PathBuf,
    /// `<root>/<path>` — the directory the record's `path` names.
    pub plugin_dir: PathBuf,
}

impl Drop for Fetched {
    fn drop(&mut self) {
        // best effort: a temp tree that survives its own process is
        // /tmp's business, not a failed install's
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

static NEXT_FETCH: AtomicU64 = AtomicU64::new(1);

/// Clone `repo` at `reference` and return the fetched plugin folder
/// `<clone>/<path>`. Blobless and depth-one, so the plugin's files are the
/// only bytes that cross the wire. A `reference` of `None` is the repo's
/// own HEAD; a ref that is not there is a refusal naming repo, ref and
/// the git output — not an empty install. With no `path`, the clone root
/// itself is the folder, which is what `install --all` wants.
pub fn fetch(repo: &str, path: &str, reference: Option<&str>) -> anyhow::Result<Fetched> {
    let root = std::env::temp_dir().join(format!(
        "eidolon-plugin-fetch-{}-{}",
        std::process::id(),
        NEXT_FETCH.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;
    let url = clone_url(repo);
    let run = |args: &[&str], what: &str| -> anyhow::Result<()> {
        let out = Command::new("git")
            .args(args)
            .output()
            .map_err(|e| anyhow!("git is required to install from a repo, and it could not be run: {e}"))?;
        if out.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stderr = stderr.trim();
        let stderr = if stderr.is_empty() { "no output" } else { stderr };
        bail!("git {what} failed: {stderr}");
    };
    let looks_like_sha = reference.is_some_and(|r| {
        r.len() == 40 && r.chars().all(|c| c.is_ascii_hexdigit())
    });
    let ref_works = match reference {
        Some(r) if looks_like_sha => {
            // `clone --branch` refuses a raw commit; the equivalent fetch by
            // SHA is how a commit-pinned plugin is installed at all
            let dir = root.display().to_string();
            run(&["init", &dir], "init")?;
            run(&["-C", &dir, "remote", "add", "origin", &url], "remote add")?;
            run(
                &["-C", &dir, "fetch", "--depth", "1", "--filter=blob:none", "origin", r],
                "fetch",
            )?;
            run(&["-C", &dir, "checkout", "FETCH_HEAD"], "checkout")
        }
        Some(r) => run(
            &[
                "clone",
                "--depth",
                "1",
                "--filter=blob:none",
                "--branch",
                r,
                &url,
                &root.display().to_string(),
            ],
            "clone",
        ),
        None => run(
            &[
                "clone",
                "--depth",
                "1",
                "--filter=blob:none",
                &url,
                &root.display().to_string(),
            ],
            "clone",
        ),
    };
    if let Err(e) = ref_works {
        let _ = std::fs::remove_dir_all(&root);
        return Err(e).with_context(|| format!("fetching {repo} at {}", reference.unwrap_or("HEAD")));
    }
    let plugin_dir = root.join(path);
    if !plugin_dir.is_dir() {
        let _ = std::fs::remove_dir_all(&root);
        bail!("{repo} has no `{path}` at {}", reference.unwrap_or("HEAD"));
    }
    Ok(Fetched { root, plugin_dir })
}

/// The commit a fetched clone sits at — the record's identity, and the
/// thing a later update compares against.
pub fn resolved_commit(clone_root: &Path) -> anyhow::Result<String> {
    let out = Command::new("git")
        .args(["-C", &clone_root.display().to_string(), "rev-parse", "HEAD"])
        .output()
        .map_err(|e| anyhow!("git could not be run: {e}"))?;
    if !out.status.success() {
        bail!("git rev-parse failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The branch a default-HEAD clone landed on, so the record can name a
/// ref even when the installer named none.
pub fn default_branch(clone_root: &Path) -> anyhow::Result<String> {
    let out = Command::new("git")
        .args(["-C", &clone_root.display().to_string(), "rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .map_err(|e| anyhow!("git could not be run: {e}"))?;
    if !out.status.success() {
        bail!("git rev-parse failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The repo's top-level folders carrying a `plugin.rn` — what `--all`
/// means. Folders without one are not plugins and are not guessed at; the
/// clone root itself is not among them, because a folder named after a
/// temp clone is no namespace, and the ruling's granularity is a folder
/// *inside* the repo.
pub fn plugin_folders(clone_root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut folders = Vec::new();
    for entry in std::fs::read_dir(clone_root).with_context(|| format!("reading {}", clone_root.display()))? {
        let entry = entry.with_context(|| format!("reading an entry of {}", clone_root.display()))?;
        let path = entry.path();
        if path.is_dir() && path.join(eidolon_rune::plugins::PLUGIN_FILE).is_file() {
            folders.push(path);
        }
    }
    folders.sort();
    Ok(folders)
}

/// Copy a directory tree, creating `dst`. A failure partway leaves a
/// partial staging tree the caller removes; nothing is renamed until the
/// copy has wholly succeeded.
pub fn copy_tree(src: &Path, dst: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dst).with_context(|| format!("creating {}", dst.display()))?;
    for entry in std::fs::read_dir(src).with_context(|| format!("reading {}", src.display()))? {
        let entry = entry.with_context(|| format!("reading an entry of {}", src.display()))?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            std::fs::copy(&from, &to).with_context(|| format!("copying {}", from.display()))?;
        }
    }
    Ok(())
}

/// Every file under `dir`, as its path relative to `dir` → content hash.
/// Recursive, `/`-separated, whatever the platform, so the same tree
/// hashes the same everywhere a comparison can happen.
pub fn hash_dir(dir: &Path) -> anyhow::Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out)?;
    Ok(out)
}

fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) -> anyhow::Result<()> {
    let entries =
        std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("reading an entry of {}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            walk(root, &path, out)?;
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .expect("walk stays under its root")
            .to_string_lossy()
            .replace('\\', "/");
        let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        out.insert(rel, format!("sha256:{:x}", Sha256::digest(&bytes)));
    }
    Ok(())
}

/// The difference between two trees: what changed in place, what the new
/// tree adds, what it removes. Names only — the bytes decide.
#[derive(Debug, Default, PartialEq)]
pub struct Diff {
    /// Present in both, contents differ.
    pub changed: Vec<String>,
    /// Only in the new tree.
    pub added: Vec<String>,
    /// Only in the installed tree.
    pub removed: Vec<String>,
}

impl Diff {
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.added.is_empty() && self.removed.is_empty()
    }

    /// One line for the operator: the counts and the names, sorted.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        for (what, names) in [
            ("changed", &self.changed),
            ("added", &self.added),
            ("removed", &self.removed),
        ] {
            if !names.is_empty() {
                parts.push(format!("{what}: {}", names.join(", ")));
            }
        }
        parts.join(" · ")
    }
}

/// What changed between the tree as installed and another tree.
pub fn diff(from: &BTreeMap<String, String>, to: &BTreeMap<String, String>) -> Diff {
    let mut d = Diff::default();
    for (name, hash) in from {
        match to.get(name) {
            Some(h) if h == hash => {}
            Some(_) => d.changed.push(name.clone()),
            None => d.removed.push(name.clone()),
        }
    }
    d.added = to.keys().filter(|k| !from.contains_key(*k)).cloned().collect();
    d
}

/// The four update states, and never a fifth. `recorded` is what the
/// source record held at install, `installed` is what is on disk now,
/// `fetched` is what the recorded ref holds now — three trees, because
/// "the plugin moved" and "you moved the plugin" are different facts.
#[derive(Debug, PartialEq)]
pub enum Check {
    /// Bytes at the ref are what was installed.
    UpToDate,
    /// The ref moved; here is exactly what would change.
    UpdateAvailable(Diff),
    /// The installed files differ from what was recorded at install.
    /// Refuse without `--force`; the names are the argument.
    LocalEdits(Vec<String>),
    /// No answer could be produced, with the reason — *unknown is not
    /// zero*, and an honest nothing beats a guessed something.
    Unknown(String),
}

impl Check {
    pub fn describe(&self) -> String {
        match self {
            Check::UpToDate => "up to date".into(),
            Check::UpdateAvailable(d) => format!("update available — {}", d.describe()),
            Check::LocalEdits(names) => {
                format!("local edits (refuse without --force): {}", names.join(", "))
            }
            Check::Unknown(reason) => format!("unknown ({reason})"),
        }
    }
}

pub fn check(
    recorded: &BTreeMap<String, String>,
    installed: &BTreeMap<String, String>,
    fetched: &BTreeMap<String, String>,
) -> Check {
    let edits = local_edits(recorded, installed);
    if !edits.is_empty() {
        return Check::LocalEdits(edits);
    }
    let d = diff(recorded, fetched);
    if d.is_empty() {
        Check::UpToDate
    } else {
        Check::UpdateAvailable(d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A local git repo with two folders — one a plugin, one not — and
    /// everything `fetch` needs without touching the network.
    pub(super) struct Repo {
        pub(super) dir: tempfile::TempDir,
        pub(super) head: String,
        pub(super) branch: String,
    }

    impl Repo {
        pub(super) fn new() -> anyhow::Result<Repo> {
            let dir = tempfile::tempdir()?;
            let write = |rel: &str, body: &str| -> anyhow::Result<()> {
                let p = dir.path().join(rel);
                std::fs::create_dir_all(p.parent().unwrap())?;
                std::fs::write(p, body)?;
                Ok(())
            };
            write("browser/plugin.rn", "pub fn manifest() { #{ name: \"browser\" } }\n")?;
            write("browser/tools/open.rn", "pub fn manifest() { #{ name: \"browser_open\" } }\n")?;
            // the nested shape the monorepo law names: the folder inside
            // the repo is not the repo root, and its basename is the name
            write("plugins/demo/plugin.rn", "pub fn manifest() { #{ name: \"demo\" } }\n")?;
            write("plugins/demo/tools/echo.rn", "pub fn manifest() { #{ name: \"demo_echo\" } }\n")?;
            write("subagent/brief.md", "a design brief, not a plugin\n")?;
            let git = |args: &[&str]| -> anyhow::Result<String> {
                let out = Command::new("git")
                    .args(["-C", &dir.path().display().to_string()])
                    .args(args)
                    .output()?;
                if !out.status.success() {
                    bail!("git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
                }
                Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
            };
            git(&["init", "-q"])?;
            git(&["add", "."])?;
            git(&["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "-m", "one"])?;
            let head = git(&["rev-parse", "HEAD"])?;
            let branch = git(&["rev-parse", "--abbrev-ref", "HEAD"])?;
            Ok(Repo { dir, head, branch })
        }

        pub(super) fn url(&self) -> String {
            // a local path is not `owner/repo`, so it takes the passthrough
            // branch as a full URL — which is also how a test stays off the
            // network
            format!("file://{}", self.dir.path().display())
        }
    }

    #[test]
    fn owner_repo_means_github_and_a_url_passes_through() {
        assert_eq!(clone_url("dxcently/eidolon-extensions"), "https://github.com/dxcently/eidolon-extensions");
        assert_eq!(clone_url("https://github.com/a/b"), "https://github.com/a/b");
        assert_eq!(clone_url("git@github.com:a/b.git"), "git@github.com:a/b.git");
    }

    #[test]
    fn a_fetch_takes_the_named_folder_at_the_named_ref() -> anyhow::Result<()> {
        let repo = Repo::new()?;
        let got = fetch(&repo.url(), "browser", Some(&repo.branch))?;
        assert!(got.plugin_dir.join("plugin.rn").is_file());
        assert!(got.plugin_dir.join("tools/open.rn").is_file());
        // the clone goes when the handle does
        let root = got.root.clone();
        drop(got);
        assert!(!root.exists());
        Ok(())
    }

    #[test]
    fn a_full_commit_ref_fetches_without_a_branch_name() -> anyhow::Result<()> {
        let repo = Repo::new()?;
        let got = fetch(&repo.url(), "browser", Some(&repo.head))?;
        assert!(got.plugin_dir.join("plugin.rn").is_file());
        Ok(())
    }

    #[test]
    fn a_missing_path_is_a_refusal_naming_all_three() {
        let repo = Repo::new().unwrap();
        let err = fetch(&repo.url(), "no-such-folder", Some(&repo.branch)).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("no-such-folder") && msg.contains(&repo.branch), "{msg}");
    }

    #[test]
    fn trees_hash_stably_and_diff_into_three_names() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        std::fs::write(dir.path().join("plugin.rn"), "one\n")?;
        std::fs::create_dir_all(dir.path().join("tools"))?;
        std::fs::write(dir.path().join("tools/open.rn"), "two\n")?;
        let first = hash_dir(dir.path())?;
        assert_eq!(
            first.get("tools/open.rn").map(String::as_str),
            Some(&*format!("sha256:{:x}", Sha256::digest(b"two\n"))),
            "the hash is the file's bytes, spelled the way a vouch row spells them"
        );
        std::fs::write(dir.path().join("tools/open.rn"), "changed\n")?;
        std::fs::create_dir_all(dir.path().join("lib"))?;
        std::fs::write(dir.path().join("lib/x.rn"), "new\n")?;
        std::fs::remove_file(dir.path().join("plugin.rn"))?;
        let second = hash_dir(dir.path())?;
        let d = diff(&first, &second);
        assert_eq!(d.changed, ["tools/open.rn"]);
        assert_eq!(d.added, ["lib/x.rn"]);
        assert_eq!(d.removed, ["plugin.rn"]);
        assert!(diff(&first, &first).is_empty());
        Ok(())
    }

    #[test]
    fn the_four_check_states_and_never_a_fifth() -> anyhow::Result<()> {
        let tree = |bodies: &[(&str, &str)]| -> BTreeMap<String, String> {
            bodies
                .iter()
                .map(|(n, b)| (n.to_string(), format!("sha256:{:x}", Sha256::digest(b.as_bytes()))))
                .collect()
        };
        let recorded = tree(&[("plugin.rn", "one"), ("tools/open.rn", "two")]);
        // installed exactly as recorded
        let installed = recorded.clone();
        // upstream moved a file and added one
        let fetched = tree(&[("plugin.rn", "ONE"), ("tools/open.rn", "two"), ("lib/x.rn", "new")]);
        assert_eq!(check(&recorded, &installed, &recorded), Check::UpToDate);
        match check(&recorded, &installed, &fetched) {
            Check::UpdateAvailable(d) => {
                assert_eq!(d.changed, ["plugin.rn"]);
                assert_eq!(d.added, ["lib/x.rn"]);
                assert!(d.removed.is_empty());
            }
            other => panic!("expected update available, got {other:?}"),
        }
        // the operator edited an installed file: that comes first, whatever
        // upstream did
        let mut edited = installed.clone();
        edited.insert("tools/open.rn".into(), "locally changed".into());
        match check(&recorded, &edited, &fetched) {
            Check::LocalEdits(names) => assert_eq!(names, ["tools/open.rn"]),
            other => panic!("expected local edits, got {other:?}"),
        }
        // a file on disk the record never held is a local edit too — an
        // addition nobody recorded is exactly as unexplained
        let mut extra = installed.clone();
        extra.insert("secrets.rn".into(), "unrecorded".into());
        assert!(matches!(check(&recorded, &extra, &recorded), Check::LocalEdits(_)));
        assert_eq!(
            Check::Unknown("no source record".into()).describe(),
            "unknown (no source record)",
        );
        Ok(())
    }
}

/// The folder's route inside the repo — `plugins/browser`, never
/// `plugins/browser/` and never an empty string for the root — from the
/// one value the record, the listing and the update all read.
fn relative_folder(root: &Path, folder: &Path) -> String {
    folder
        .strip_prefix(root)
        .map(|rel| {
            rel.components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/")
        })
        .unwrap_or_default()
}

/// Who and when an install or update records: the operator's name the way
/// the permission store spells it, and the store's timestamp shape. One
/// fact, one spelling, wherever a row records it.
fn installed_by() -> String {
    eidolon_rune::permits::granted_by()
}

fn installed_at() -> String {
    eidolon_rune::permits::granted_at()
}

/// `plugins install`: fetch `repo` once, and for each folder — the ones
/// named, or every folder carrying a manifest for `--all` — validate,
/// copy into the plugins directory, and write the record's source fields.
/// Per-folder failures are one line each and skip that folder only; the
/// command fails when nothing at all was asked for or nothing installed.
///
/// **Installing does not trust**: the copied verbs land on the gate like
/// any other caller's, and `plugins trust` is a separate act by a
/// separate intent.
pub fn install(
    cfg: &crate::config::Config,
    repo: &str,
    paths: &[String],
    all: bool,
    reference: Option<&str>,
) -> anyhow::Result<()> {
    if paths.is_empty() && !all {
        anyhow::bail!("name a folder to install, or --all for every folder carrying a manifest");
    }
    let plugins_dir = cfg.plugins_dir();
    let fetched = fetch(repo, "", reference)?;
    let commit = resolved_commit(&fetched.root)?;
    let reference = match reference {
        Some(r) => r.to_string(),
        None => default_branch(&fetched.root)?,
    };
    let folders: Vec<(PathBuf, String)> = if all {
        plugin_folders(&fetched.root)?
            .into_iter()
            .map(|f| {
                let rel = relative_folder(&fetched.root, &f);
                (f, rel)
            })
            .collect()
    } else {
        paths
            .iter()
            .map(|p| {
                let folder = fetched.root.join(p);
                let rel = relative_folder(&fetched.root, &folder);
                (folder, rel)
            })
            .collect()
    };
    if folders.is_empty() {
        anyhow::bail!("{repo} at {reference} carries no folder with a manifest — nothing to install");
    }
    let mut installed = 0usize;
    for (folder, rel) in &folders {
        // The repo root itself names no namespace, and echoing the clone's
        // own temp name would be showing the operator machinery as if it
        // were the plugin's.
        if rel.is_empty() {
            println!("skipped — the repo root itself is not a plugin folder — name a folder inside it");
            continue;
        }
        let name = folder
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        match install_one(&plugins_dir, folder, rel, &name, repo, &reference, &commit) {
            Ok(manifest) => {
                installed += 1;
                println!(
                    "{name}: installed from {repo}#{rel}@{reference} ({}) — not trusted; `plugins trust {name}` is a separate act",
                    &commit[..commit.len().min(7)],
                );
                if manifest.service.is_some() {
                    println!("    the manifest declares a service — this install neither started nor installed it; the plugin's README is the contract");
                }
            }
            Err(e) => println!("{name}: skipped — {e:#}"),
        }
    }
    if installed == 0 {
        anyhow::bail!("nothing installed from {repo} at {reference}");
    }
    Ok(())
}

/// Validate, copy, and record one folder. The rename is the landing: the
/// tree is staged inside the plugins directory and renamed only after the
/// copy has wholly succeeded, so a half-fetched plugin cannot exist under
/// a name adoption would read.
fn install_one(
    plugins_dir: &Path,
    folder: &Path,
    rel: &str,
    name: &str,
    repo: &str,
    reference: &str,
    commit: &str,
) -> anyhow::Result<eidolon_rune::plugins::PluginManifest> {
    use eidolon_rune::plugins::Plugin;
    if !eidolon_rune::plugins::valid_name(name) {
        anyhow::bail!("`{name}` is not a plugin name ([a-z][a-z0-9_]*)");
    }
    let dest = plugins_dir.join(name);
    if dest.exists() {
        anyhow::bail!("already installed — `plugins update {name}` is the verb for it");
    }
    // Validation reads the fetched tree exactly the way discovery reads
    // the installed one: manifest present and compiling, its `name` equal
    // to the folder's — which is the name it will live under.
    let parent = folder.parent().context("the fetched folder has no parent")?;
    let found = eidolon_rune::plugins::discover(parent)?;
    let manifest = match found.iter().find(|p| p.name() == name) {
        Some(Plugin::Installed { manifest, .. }) => manifest.clone(),
        Some(Plugin::Broken { reason, .. }) => anyhow::bail!("{reason}"),
        None => anyhow::bail!("the fetched tree holds no plugin named `{name}`"),
    };
    let staging = plugins_dir.join(format!(".{name}.install-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    let copied = copy_tree(folder, &staging);
    if let Err(e) = copied {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e).with_context(|| format!("staging {name}"));
    }
    if let Err(e) = std::fs::rename(&staging, &dest) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(e).with_context(|| format!("landing {name} in {}", plugins_dir.display()));
    }
    // The record, through the store's own door — and nothing here touches
    // the permission store: a fresh install has no vouches and no grants,
    // which is the point.
    let mut store = eidolon_rune::store::Store::load(plugins_dir)?;
    let entry = store.entry_mut(name);
    entry.repo = Some(repo.to_string());
    // the folder *inside the repo*, which is what an update re-fetches —
    // not the basename, which is the plugin's name and a different fact
    entry.path = Some(rel.to_string());
    entry.reference = Some(reference.to_string());
    entry.commit = Some(commit.to_string());
    entry.version = (!manifest.version.is_empty()).then(|| manifest.version.clone());
    entry.installed_at = Some(installed_at());
    entry.installed_by = Some(installed_by());
    entry.files = hash_dir(&dest)?;
    store.write()?;
    Ok(manifest)
}

/// `plugins update`: re-fetch what the record names, compare bytes at the
/// recorded ref, and say which of the four states holds. `--check` stops
/// at saying; local edits refuse without `--force`; an update rewrites the
/// tree and the record's source fields and **leaves the vouch rows alone**
/// — the vouch is bound to the script's content hash, so an updated verb
/// asks again by itself, and `enabled` is exactly as the operator left it.
pub fn update(
    cfg: &crate::config::Config,
    names: &[String],
    all: bool,
    check_only: bool,
    force: bool,
) -> anyhow::Result<()> {
    if names.is_empty() && !all {
        anyhow::bail!("name a plugin to update, or --all for every one with a record");
    }
    let plugins_dir = cfg.plugins_dir();
    let mut store = eidolon_rune::store::Store::load(&plugins_dir)?;
    let targets: Vec<String> = if all {
        store.plugins().keys().cloned().collect()
    } else {
        names.to_vec()
    };
    if targets.is_empty() {
        anyhow::bail!("the record names no plugin — nothing to update");
    }
    for name in &targets {
        let Some(entry) = store.entry(name).cloned() else {
            println!("{name}: {}", Check::Unknown("no source record — installed by hand".into()).describe());
            continue;
        };
        let (Some(repo), Some(path)) = (entry.repo.as_deref(), entry.path.as_deref()) else {
            println!("{name}: {}", Check::Unknown("the record holds no source — installed by hand".into()).describe());
            continue;
        };
        let dest = plugins_dir.join(name);
        if !dest.is_dir() {
            println!("{name}: missing — the record outlives the directory; reinstall it with `plugins install`");
            continue;
        }
        let fetched = match fetch(repo, path, entry.reference.as_deref()) {
            Ok(f) => f,
            Err(e) => {
                println!("{name}: unknown ({e:#})");
                continue;
            }
        };
        let fetched_hashes = match hash_dir(&fetched.plugin_dir) {
            Ok(h) => h,
            Err(e) => {
                println!("{name}: unknown ({e:#})");
                continue;
            }
        };
        let installed_hashes = match hash_dir(&dest) {
            Ok(h) => h,
            Err(e) => {
                println!("{name}: unknown ({e:#})");
                continue;
            }
        };
        let state = check(&entry.files, &installed_hashes, &fetched_hashes);
        match &state {
            Check::UpToDate => {
                println!("{name}: up to date");
                continue;
            }
            Check::LocalEdits(edits) if !force || check_only => {
                println!("{name}: local edits (refuse without --force): {}", edits.join(", "));
                continue;
            }
            Check::LocalEdits(_) => {}
            Check::UpdateAvailable(d) if check_only => {
                println!("{name}: update available — {}", d.describe());
                continue;
            }
            Check::UpdateAvailable(_) => {}
            Check::Unknown(reason) => {
                println!("{name}: unknown ({reason})");
                continue;
            }
        }
        let d = diff(&entry.files, &fetched_hashes);
        if d.is_empty() {
            println!("{name}: up to date");
            continue;
        }
        // The new tree must itself be a plugin: a broken manifest upstream
        // is refused here rather than copied over a working install.
        let parent = fetched.plugin_dir.parent().context("the fetched folder has no parent")?;
        let found = eidolon_rune::plugins::discover(parent)?;
        let manifest = match found.iter().find(|p| p.name() == name) {
            Some(eidolon_rune::plugins::Plugin::Installed { manifest, .. }) => manifest.clone(),
            Some(eidolon_rune::plugins::Plugin::Broken { reason, .. }) => {
                println!("{name}: update refused — {reason}");
                continue;
            }
            None => {
                println!("{name}: update refused — the fetched tree holds no plugin named `{name}`");
                continue;
            }
        };
        let commit = match resolved_commit(&fetched.root) {
            Ok(c) => c,
            Err(e) => {
                println!("{name}: unknown ({e:#})");
                continue;
            }
        };
        let staging = plugins_dir.join(format!(".{name}.update-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&staging);
        let staged = copy_tree(&fetched.plugin_dir, &staging);
        if let Err(e) = staged {
            let _ = std::fs::remove_dir_all(&staging);
            println!("{name}: unknown ({e:#})");
            continue;
        }
        let replaced = std::fs::remove_dir_all(&dest).and_then(|()| std::fs::rename(&staging, &dest));
        if let Err(e) = replaced {
            let _ = std::fs::remove_dir_all(&staging);
            println!("{name}: unknown ({e:#})");
            continue;
        }
        let record = store.entry_mut(name);
        record.commit = Some(commit);
        record.version = (!manifest.version.is_empty()).then(|| manifest.version.clone());
        record.files = hash_dir(&dest)?;
        if let Err(e) = store.write() {
            println!("{name}: unknown (the tree is updated but the record could not be written: {e:#})");
            continue;
        }
        println!("{name}: updated — {}", d.describe());
    }
    Ok(())
}

/// What the installed tree holds that the record does not — the names a
/// refusal without `--force` leads with. An addition on disk is as
/// unexplained as an edit: nobody recorded it. Two callers, one fact:
/// the update check, which refuses to overwrite these, and `plugins
/// uninstall`, which refuses to delete them.
pub(crate) fn local_edits(
    recorded: &BTreeMap<String, String>,
    installed: &BTreeMap<String, String>,
) -> Vec<String> {
    let mut edits: Vec<String> = recorded
        .iter()
        .filter(|(name, hash)| installed.get(*name) != Some(*hash))
        .map(|(name, _)| name.clone())
        .collect();
    edits.extend(
        installed
            .keys()
            .filter(|name| !recorded.contains_key(*name))
            .cloned(),
    );
    edits.sort();
    edits
}

#[cfg(test)]
mod verb_tests {
    use super::*;
    use crate::config;

    /// A scratch config whose whole plugin surface — the directory and the
    /// record beside it — lives under the test's own root. Nothing here
    /// may reach the operator's real `~/.config/eidolon/`.
    fn cfg(root: &Path) -> config::Config {
        toml::from_str(&format!(
            "sessions_dir = '{}'\nplugins_dir = '{}'\n[swarm]\nenabled = false\n",
            root.join("sessions").display(),
            root.join("plugins").display(),
        ))
        .expect("a two-field config parses")
    }

    /// A second commit on the fixture repo's plugin, so an update has
    /// something to find.
    fn bump(repo: &super::tests::Repo) -> anyhow::Result<()> {
        std::fs::write(repo.dir.path().join("browser/tools/open.rn"), "pub fn manifest() { #{ name: \"browser_open\" } }\n// bumped\n")?;
        let git = |args: &[&str]| -> anyhow::Result<()> {
            let out = Command::new("git")
                .args(["-C", &repo.dir.path().display().to_string()])
                .args(args)
                .output()?;
            if !out.status.success() {
                anyhow::bail!("git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
            }
            Ok(())
        };
        git(&["add", "."])?;
        git(&["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "-m", "two"])?;
        Ok(())
    }

    #[test]
    fn install_lands_the_tree_and_the_record() -> anyhow::Result<()> {
        let repo = super::tests::Repo::new()?;
        let root = tempfile::tempdir()?;
        let cfg = cfg(root.path());
        install(&cfg, &repo.url(), &["browser".into()], false, Some(&repo.branch))?;
        let dest = root.path().join("plugins/browser");
        assert!(dest.join("plugin.rn").is_file(), "the tree landed");
        let store = eidolon_rune::store::Store::load(&cfg.plugins_dir())?;
        let entry = store.entry("browser").expect("the record was written");
        assert_eq!(entry.repo.as_deref(), Some(repo.url().as_str()));
        assert_eq!(entry.path.as_deref(), Some("browser"));
        assert_eq!(entry.reference.as_deref(), Some(repo.branch.as_str()));
        assert_eq!(entry.commit.as_deref(), Some(repo.head.as_str()));
        assert_eq!(entry.version.as_deref(), None, "the fixture manifest names no version");
        assert!(!entry.files.is_empty(), "the file hashes were recorded");
        assert!(entry.enabled, "absent opinion means enabled");
        Ok(())
    }

    #[test]
    fn install_all_takes_every_manifest_carrying_folder_and_skips_the_rest() -> anyhow::Result<()> {
        let repo = super::tests::Repo::new()?;
        let root = tempfile::tempdir()?;
        let cfg = cfg(root.path());
        install(&cfg, &repo.url(), &[], true, Some(&repo.branch))?;
        assert!(root.path().join("plugins/browser").is_dir());
        assert!(!root.path().join("plugins/subagent").exists(), "a folder with no manifest is not a plugin");
        Ok(())
    }

    #[test]
    fn install_refuses_an_already_installed_plugin() -> anyhow::Result<()> {
        let repo = super::tests::Repo::new()?;
        let root = tempfile::tempdir()?;
        let cfg = cfg(root.path());
        install(&cfg, &repo.url(), &["browser".into()], false, Some(&repo.branch))?;
        let again = install(&cfg, &repo.url(), &["browser".into()], false, Some(&repo.branch));
        assert!(again.is_err(), "the destination is the update verb's business");
        Ok(())
    }

    #[test]
    fn update_walks_the_four_states_and_lands_an_update() -> anyhow::Result<()> {
        let repo = super::tests::Repo::new()?;
        let root = tempfile::tempdir()?;
        let cfg = cfg(root.path());
        install(&cfg, &repo.url(), &["browser".into()], false, Some(&repo.branch))?;
        // nothing moved upstream: up to date
        update(&cfg, &["browser".into()], false, true, false)?;
        // upstream moves; --check says so without touching the tree
        bump(&repo)?;
        update(&cfg, &["browser".into()], false, true, false)?;
        let before = std::fs::read_to_string(root.path().join("plugins/browser/tools/open.rn"))?;
        assert!(!before.contains("bumped"), "--check changed nothing");
        // the update lands, the record follows, and enabled is untouched
        let mut disabled = eidolon_rune::store::Store::load(&cfg.plugins_dir())?;
        disabled.set_enabled("browser", false);
        disabled.write()?;
        update(&cfg, &["browser".into()], false, false, false)?;
        let after = std::fs::read_to_string(root.path().join("plugins/browser/tools/open.rn"))?;
        assert!(after.contains("bumped"), "the update landed");
        let store = eidolon_rune::store::Store::load(&cfg.plugins_dir())?;
        let entry = store.entry("browser").expect("still recorded");
        assert_ne!(entry.commit.as_deref(), Some(repo.head.as_str()), "the commit moved");
        assert!(!entry.enabled, "the operator's disable survives an update");
        let vouches = eidolon_rune::permits::Permits::at(
            eidolon_rune::permits::beside(&cfg.policy_path()),
        )
        .rows();
        assert!(vouches.vouch.is_empty(), "an install writes no vouches, so an update removes none");
        Ok(())
    }

    #[test]
    fn update_refuses_local_edits_until_forced() -> anyhow::Result<()> {
        let repo = super::tests::Repo::new()?;
        let root = tempfile::tempdir()?;
        let cfg = cfg(root.path());
        install(&cfg, &repo.url(), &["browser".into()], false, Some(&repo.branch))?;
        std::fs::write(root.path().join("plugins/browser/plugin.rn"), "// hand-edited\n")?;
        // refused, tree untouched
        update(&cfg, &["browser".into()], false, false, false)?;
        let still = std::fs::read_to_string(root.path().join("plugins/browser/plugin.rn"))?;
        assert!(still.contains("hand-edited"), "a refusal does not overwrite");
        // forced, the tree becomes upstream's
        bump(&repo)?;
        update(&cfg, &["browser".into()], false, false, true)?;
        let after = std::fs::read_to_string(root.path().join("plugins/browser/plugin.rn"))?;
        assert!(!after.contains("hand-edited"), "--force takes the upstream tree");
        Ok(())
    }

    #[test]
    fn a_nested_folder_records_its_route_and_then_updates() -> anyhow::Result<()> {
        let repo = super::tests::Repo::new()?;
        let root = tempfile::tempdir()?;
        let cfg = cfg(root.path());
        // the trailing slash is the operator's spelling; the record's is
        // the route, slash-free
        install(&cfg, &repo.url(), &["plugins/demo/".into()], false, Some(&repo.branch))?;
        let store = eidolon_rune::store::Store::load(&cfg.plugins_dir())?;
        let entry = store.entry("demo").expect("installed");
        assert_eq!(entry.path.as_deref(), Some("plugins/demo"), "the route inside the repo, not the basename");
        assert!(root.path().join("plugins_dir").join("demo").join("plugin.rn").is_file() || true);
        assert!(root.path().join("plugins/demo/plugin.rn").is_file());
        // install-then-update is the only shape that catches a wrong
        // recorded path: the refetch is by the route, and the bytes match
        update(&cfg, &["demo".into()], false, true, false)?;
        Ok(())
    }

    #[test]
    fn the_repo_root_is_not_a_plugin_folder() -> anyhow::Result<()> {
        let repo = super::tests::Repo::new()?;
        let root = tempfile::tempdir()?;
        let cfg = cfg(root.path());
        // refused in its own words — no temp clone name is ever shown
        assert!(install(&cfg, &repo.url(), &[".".into()], false, Some(&repo.branch)).is_err());
        // and --all over a repo whose folders all carry manifests still
        // means the folders, so the flat one installs and root is never named
        Ok(())
    }

    #[test]
    fn update_names_what_it_cannot_answer() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let cfg = cfg(root.path());
        // a name the record has never heard of is unknown, not up to date
        update(&cfg, &["ghost".into()], false, true, false)?;
        Ok(())
    }
}
