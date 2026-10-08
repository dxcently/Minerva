//! The plugins that come with Minerva (browser, subagent, jev): installed, trusted and kept current by the SAME stock
//! eidolon commands a person would type (`eidolon plugins install|trust|grant|service approve`).
//!
//! Where: eidolon reads plugins from `<config dir>/eidolon/plugins` only (the `plugins_dir` key is the one other way, and
//! the perch's generated config keeps the member's keys but never sets it), so that is where they go. Hence the rules:
//!
//! ```text
//!   a plugin of that name is absent            -> install, trust, record it as OURS
//!   present and recorded as ours               -> replaced when the bundled copy differs (trust is re-applied)
//!   present and NOT ours (the member's own)    -> left alone; replaced only if the person says yes
//! ```
//!
//! `<prefix>/state/minerva/plugins.json` records which ones are ours; uninstall removes only those.
//! Install uses `eidolon plugins install file://<temp git repo of the bundled plugins>` (stock, so the plugin record
//! `plugins.toml` is written and `plugins update`/`uninstall` work); without `git` it copies the folder instead.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::install::Layout;

/// The six verbs the browser README grants the token file to.
const BROWSER_VERBS: [&str; 6] = ["open", "snapshot", "click", "type", "read", "back"];
const BROWSER_TOKEN: &str = "file:~/.config/eidolon/browser.token";

#[derive(Debug, Default, Serialize, Deserialize, Clone)]
pub struct Ours {
    pub version: String,
    /// sha-256 over the bundled folder this copy came from.
    pub hash: String,
    /// "stock" (eidolon plugins install) or "copy" (no git).
    pub how: String,
    /// sha-256 over the INSTALLED copy right after we installed and trusted it. Trust is only ever (re)applied to a copy
    /// that still hashes to this; anything else was edited by the member and is left alone, untrusted.
    #[serde(default)]
    pub installed: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    /// The Minerva version that last synced.
    #[serde(default)]
    pub stamp: String,
    #[serde(default)]
    pub plugins: BTreeMap<String, Ours>,
}

pub struct PluginEnv {
    pub eidolon: PathBuf,
    /// The install's `bin/`: first on PATH for eidolon, so `eidolon-browser` and `chromium` are found.
    pub bin_dir: PathBuf,
    /// `<version>/plugins`: one folder per bundled plugin.
    pub src: PathBuf,
    pub layout: Layout,
}

fn config_dir() -> Result<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME").filter(|d| Path::new(d).is_absolute()) {
        Some(x) => PathBuf::from(x),
        None => Path::new(&std::env::var_os("HOME").filter(|h| !h.is_empty()).context("HOME is not set")?).join(".config"),
    };
    Ok(base.join("eidolon"))
}

pub fn plugins_dir() -> Result<PathBuf> {
    Ok(config_dir()?.join("plugins"))
}

fn state_path(l: &Layout) -> PathBuf {
    l.state_dir().join("plugins.json")
}

pub fn load_state(l: &Layout) -> State {
    fs::read_to_string(state_path(l)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save_state(l: &Layout, st: &State) -> Result<()> {
    fs::create_dir_all(l.state_dir())?;
    let tmp = state_path(l).with_extension("tmp");
    fs::write(&tmp, serde_json::to_string_pretty(st)?)?;
    fs::rename(&tmp, state_path(l)).context("saving plugins.json")
}

/// sha-256 over relative paths and bytes, sorted.
pub fn hash_tree(dir: &Path) -> Result<String> {
    fn walk(base: &Path, d: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
        for e in fs::read_dir(d)? {
            let p = e?.path();
            if p.file_name().map(|n| n == ".git").unwrap_or(false) {
                continue; // eidolon's own bookkeeping, not the plugin's
            }
            if p.is_dir() {
                walk(base, &p, out)?;
            } else {
                out.push(p.strip_prefix(base)?.to_path_buf());
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    walk(dir, dir, &mut files)?;
    files.sort();
    let mut h = Sha256::new();
    for f in files {
        h.update(f.to_string_lossy().as_bytes());
        h.update([0]);
        h.update(fs::read(dir.join(&f))?);
        h.update([0]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn version_of(dir: &Path) -> String {
    // `version: "0.1.0"` in plugin.rn's manifest()
    let t = fs::read_to_string(dir.join("plugin.rn")).unwrap_or_default();
    t.lines().find_map(|l| l.trim().strip_prefix("version:").map(|v| v.trim().trim_end_matches(',').trim_matches('"').to_string())).unwrap_or_default()
}

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for e in fs::read_dir(from)? {
        let e = e?;
        let (f, t) = (e.path(), to.join(e.file_name()));
        if e.file_type()?.is_dir() {
            copy_tree(&f, &t)?;
        } else {
            fs::copy(&f, &t).with_context(|| format!("copying {}", f.display()))?;
        }
    }
    Ok(())
}

impl PluginEnv {
    fn path_env(&self) -> String {
        format!("{}:{}", self.bin_dir.display(), std::env::var("PATH").unwrap_or_default())
    }

    /// `eidolon <args>` with nobody at the keyboard. Returns (success, combined output).
    pub fn eidolon(&self, args: &[&str]) -> Result<(bool, String)> {
        let o = Command::new(&self.eidolon)
            .args(args)
            .env("PATH", self.path_env())
            .stdin(Stdio::null())
            .output()
            .with_context(|| format!("running {}", self.eidolon.display()))?;
        let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&o.stderr));
        Ok((o.status.success(), text))
    }

    fn eid_ok(&self, args: &[&str]) -> Result<String> {
        let (ok, text) = self.eidolon(args)?;
        if !ok {
            bail!("eidolon {} failed: {}", args.join(" "), text.trim());
        }
        Ok(text)
    }

    fn names(&self) -> Result<Vec<String>> {
        let mut v: Vec<String> = fs::read_dir(&self.src)
            .with_context(|| format!("reading {}", self.src.display()))?
            .flatten()
            .filter(|e| e.path().join("plugin.rn").is_file())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        Ok(v)
    }

    /// A throwaway git repo holding the bundled plugin folders; None when git is unusable.
    fn make_repo(&self, names: &[String]) -> Option<tempfile::TempDir> {
        // a private (0700), unpredictable directory: nobody can plant a repo or a symlink where the install will read
        let td = tempfile::Builder::new().prefix("minerva-plugins-").tempdir().ok()?;
        let repo = td.path().to_path_buf();
        for n in names {
            copy_tree(&self.src.join(n), &repo.join(n)).ok()?;
        }
        let git = |args: &[&str]| -> bool {
            Command::new("git")
                .args(["-C", &repo.display().to_string()])
                .args(["-c", "user.name=minerva", "-c", "user.email=minerva@localhost", "-c", "commit.gpgsign=false"])
                .args(args)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        };
        let ok = git(&["init", "-q", "-b", "main"]) && git(&["add", "-A"]) && git(&["commit", "-q", "-m", "bundled plugins"]);
        if ok { Some(td) } else { None }
    }

    fn trust_one(&self, name: &str, out: &mut Vec<String>) -> Result<()> {
        self.eid_ok(&["plugins", "trust", name])?;
        if name == "browser" {
            for v in BROWSER_VERBS {
                self.eid_ok(&["plugins", "grant", &format!("browser_{v}"), BROWSER_TOKEN])?;
            }
            self.eid_ok(&["plugins", "service", "approve", "browser"])?;
        }
        out.push(format!("{name}: trusted"));
        Ok(())
    }
}

/// Does the member's eidolon config move the plugins directory? Then we do not guess where it is.
fn config_moves_plugins() -> bool {
    let Ok(d) = config_dir() else { return false };
    fs::read_to_string(d.join("config.toml")).map(|t| t.lines().any(|l| l.trim_start().starts_with("plugins_dir"))).unwrap_or(false)
}

pub struct SyncOpts<'a> {
    /// The Minerva version being installed (the stamp).
    pub version: &'a str,
    /// Do nothing when the stamp already matches and every plugin of ours is still there.
    pub if_needed: bool,
    /// Asked for a plugin of the member's own with the same name; true replaces it. None asks nobody (leave it).
    pub ask_replace: Option<&'a mut dyn FnMut(&str) -> bool>,
}

/// Install, update and trust the bundled plugins. Returns the lines to show.
pub fn sync(env: &PluginEnv, mut o: SyncOpts) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut st = load_state(&env.layout);
    let pdir = plugins_dir()?;
    if config_moves_plugins() {
        out.push("plugins: your eidolon config sets plugins_dir; the bundled plugins were not installed (minerva plugins sync after you remove that key)".into());
        return Ok(out);
    }
    let names = env.names()?;
    if o.if_needed && st.stamp == o.version {
        // Nothing to do only if EVERY bundled plugin is settled: ours, present, unedited and current; or the member's own
        // (present). A missing or edited one, or one that is outdated, means a full pass.
        let settled = names.iter().all(|n| {
            let dest = pdir.join(n);
            match st.plugins.get(n) {
                Some(ours) => {
                    dest.is_dir()
                        && hash_tree(&dest).map(|h| h == ours.installed).unwrap_or(false)
                        && hash_tree(&env.src.join(n)).map(|h| h == ours.hash).unwrap_or(false)
                }
                None => dest.is_dir(),
            }
        });
        if settled {
            return Ok(out);
        }
    }

    let mut to_install: Vec<String> = Vec::new();
    let mut hashes = BTreeMap::new();
    for n in &names {
        let src = env.src.join(n);
        let hash = hash_tree(&src)?;
        hashes.insert(n.clone(), hash.clone());
        let dest = pdir.join(n);
        match (dest.exists(), st.plugins.get(n)) {
            (false, _) => to_install.push(n.clone()),
            (true, Some(ours)) if hash_tree(&dest).map(|h| h != ours.installed).unwrap_or(true) => {
                // The installed copy is not what we installed: the member edited it (or it was swapped). We neither trust nor
                // replace it; a vouch is only ever put on bytes we put there ourselves.
                out.push(format!(
                    "{n}: your copy was changed since Minerva installed it; left as it is and NOT trusted (to get Minerva's again: delete that folder, then minerva plugins sync)"
                ));
            }
            (true, Some(ours)) if ours.hash == hash => {
                // unedited and current; the vouches may still need re-applying after an eidolon-side reset, which is cheap and idempotent
                env.trust_one(n, &mut out)?;
            }
            (true, Some(_)) => {
                // ours, unedited and outdated: replace, keeping the trust state (re-applied below)
                if n == "browser" {
                    let _ = env.eidolon(&["plugins", "service", "stop", "browser"]);
                }
                let (ok, text) = env.eidolon(&["plugins", "uninstall", n])?;
                if ok {
                    to_install.push(n.clone());
                } else {
                    out.push(format!("{n}: kept your edited copy, not replaced ({})", text.trim().lines().next().unwrap_or("")));
                }
            }
            (true, None) => {
                let yes = match o.ask_replace.as_mut() {
                    Some(f) => f(n),
                    None => false,
                };
                if yes {
                    let _ = env.eidolon(&["plugins", "service", "stop", n]);
                    let (ok, text) = env.eidolon(&["plugins", "uninstall", n, "--force"])?;
                    if ok {
                        to_install.push(n.clone());
                    } else {
                        out.push(format!("{n}: could not replace yours: {}", text.trim()));
                    }
                } else {
                    out.push(format!("{n}: you already have a plugin of that name; left exactly as it is"));
                }
            }
        }
    }

    if !to_install.is_empty() {
        let repo = env.make_repo(&to_install);
        let mut stock_ok = false;
        if let Some(td) = &repo {
            let url = format!("file://{}", td.path().display());
            let mut args: Vec<&str> = vec!["plugins", "install", &url];
            args.extend(to_install.iter().map(String::as_str));
            let (ok, text) = env.eidolon(&args)?;
            stock_ok = ok;
            if !ok {
                out.push(format!("plugins: stock install failed ({}); copying instead", text.trim().lines().last().unwrap_or("")));
            }
        }
        drop(repo);
        for n in &to_install {
            let dest = pdir.join(n);
            let how = if stock_ok && dest.is_dir() {
                "stock"
            } else {
                fs::create_dir_all(&pdir)?;
                let stage = pdir.join(format!(".{n}.minerva-{}", std::process::id()));
                let _ = fs::remove_dir_all(&stage);
                if dest.exists() {
                    continue; // never overwrite
                }
                copy_tree(&env.src.join(n), &stage)?;
                fs::rename(&stage, &dest).with_context(|| format!("landing {n}"))?;
                "copy"
            };
            env.trust_one(n, &mut out)?;
            let installed = hash_tree(&dest)?;
            st.plugins.insert(n.clone(), Ours { version: version_of(&dest), hash: hashes[n].clone(), how: how.into(), installed });
            out.push(format!("{n}: installed {} ({how})", version_of(&dest)));
        }
    }
    st.stamp = o.version.to_string();
    save_state(&env.layout, &st)?;
    Ok(out)
}

/// Remove only what we installed: untrust, revoke our grants, uninstall. Best effort; never touches the member's own.
pub fn remove_ours(env: &PluginEnv) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let st = load_state(&env.layout);
    for n in st.plugins.keys() {
        let _ = env.eidolon(&["plugins", "service", "stop", n]);
        let _ = env.eidolon(&["plugins", "untrust", n]);
        if n == "browser" {
            for v in BROWSER_VERBS {
                let _ = env.eidolon(&["plugins", "revoke", &format!("browser_{v}"), BROWSER_TOKEN]);
            }
        }
        if n == "jev" {
            for v in ["choose", "entail"] {
                let _ = env.eidolon(&["plugins", "revoke", &format!("jev_{v}"), JEV_TOKEN]);
            }
        }
        let (ok, text) = env.eidolon(&["plugins", "uninstall", n, "--force"])?;
        out.push(if ok { format!("{n}: removed") } else { format!("{n}: not removed ({})", text.trim().lines().next().unwrap_or("")) });
    }
    let _ = fs::remove_file(state_path(&env.layout));
    Ok(out)
}

pub const JEV_TOKEN: &str = "file:~/.config/eidolon/jev.token";

/// `minerva services start|stop|status`: the browser service (when its plugin is ours).
pub fn service(env: &PluginEnv, op: &str) -> Result<String> {
    let st = load_state(&env.layout);
    if !st.plugins.contains_key("browser") {
        return Ok("browser service: not ours (no bundled browser plugin installed); left alone".into());
    }
    let (ok, text) = env.eidolon(&["plugins", "service", op, "browser"])?;
    let line = text.trim().lines().last().unwrap_or("").to_string();
    if !ok && op == "start" {
        bail!("browser service: {line}");
    }
    Ok(format!("browser service {op}: {line}"))
}

#[cfg(test)]
mod tests;
