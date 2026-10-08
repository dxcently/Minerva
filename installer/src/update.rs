//! `minerva update`: fetch the newest release, verify it against the compiled-in public key, install it beside the
//! current one, flip `current`, keep the previous one. Refuses on any signature problem and on a Nix-managed install.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use std::time::Duration;

use crate::fetch::{MAX_BUNDLE, MAX_SMALL, Source};
use crate::install::{self, Layout};
use crate::keys::{self, Planned, Rotated, Trust};
use crate::manifest::Manifest;
use crate::version::is_newer;
use crate::{MANIFEST_NAME, manifest};

pub struct Opts<'a> {
    pub base: &'a str,
    /// Who may sign: the compiled-in key plus the keys dir (see `keys`).
    pub trust: &'a Trust,
    /// Unix seconds now (key and rotation expiry, and the 30-day freshness of the signed manifest).
    pub now: u64,
    /// The version running now (its `VERSION`, e.g. `0.1.0-gabc1234`).
    pub current: &'a str,
    /// Where this program lives (decides the prefix; also the Nix check).
    pub exe: &'a Path,
    pub rollback: bool,
    /// Only say whether a newer release exists. Writes nothing: a key rotation in the release is judged in memory only.
    pub check_only: bool,
}

/// What a release says, verified.
pub struct Verified {
    pub manifest: Manifest,
    /// The trust to judge the bundle by: `trust`, plus the release's rotation (in memory unless it was committed).
    pub trust: Trust,
    /// `Some(warning)` when the signed manifest is past its 30 days.
    pub stale: Option<String>,
    /// A rotation was written to the keys dir: (added, revoked).
    pub rotated: Option<(usize, usize)>,
}

/// The one way to read a release's front matter, for `update`, `update --check` and the daily notice:
/// 1. `keys.json` (+ `.minisig`) if present: planned against the local record, applied (`commit`) or only held in memory;
/// 2. `latest.json` + `.minisig`, verified with the keys that result;
/// 3. the manifest's `keys_serial`: a release that needs a rotation this machine does not have, and does not bring, is refused.
pub fn fetch_verified(src: &Source, trust: &Trust, now: u64, commit: bool, timeout: Option<Duration>) -> Result<Verified> {
    let today = keys::date_of(now);
    let mut eff = trust.clone();
    let mut have_serial = trust.state()?.serial;
    let mut rotated = None;
    if let Some(rot) = src.get_optional("keys.json", MAX_SMALL, timeout).context("fetching keys.json")? {
        let rsig = match src.get_optional("keys.json.minisig", MAX_SMALL, timeout).context("fetching the keys.json signature")? {
            Some(b) => Some(String::from_utf8(b).context("the keys.json signature is not text")?),
            None => None,
        };
        match keys::plan_rotation(trust, &rot, rsig.as_deref(), &today)? {
            Planned::AlreadyApplied => {}
            Planned::Apply(plan) => {
                have_serial = plan.serial();
                if commit {
                    if let Rotated::Applied(a, r) = keys::commit_rotation(trust, &plan)? {
                        rotated = Some((a.len(), r.len()));
                    }
                } else {
                    eff = trust.with_plan(&plan);
                }
            }
        }
    }
    let json = src.get(MANIFEST_NAME, MAX_SMALL, timeout).context("fetching the release manifest")?;
    let sig = String::from_utf8(src.get(&format!("{MANIFEST_NAME}.minisig"), MAX_SMALL, timeout).context("fetching the manifest signature")?)
        .context("the manifest signature is not text")?;
    eff.verify(&json, &sig, &today).context("the release manifest")?;
    let m = manifest::parse(&json)?;
    if let Some(need) = m.keys_serial
        && need > have_serial
    {
        bail!(
            "this release needs key rotation {need} (this machine has {have_serial}) and the release brings no keys.json for it; refusing. \
The maintainer must attach keys.json and keys.json.minisig to the release"
        );
    }
    let stale = m.stale(now);
    Ok(Verified { manifest: m, trust: eff, stale, rotated })
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    UpToDate,
    Available(String),
    Updated { from: String, to: String },
    RolledBack { from: String, to: String },
}

pub fn run(o: &Opts, out: &mut dyn Write) -> Result<Outcome> {
    if install::is_nix_managed(o.exe) {
        bail!(
            "this Minerva is managed by Nix ({}); it must not be rewritten from here. Update it with Nix:\n  nix profile upgrade <the minerva entry in `nix profile list`>\n(or bump the flake input, then rebuild)",
            o.exe.display()
        );
    }
    let layout = Layout::from_exe(o.exe)?;
    if o.rollback {
        let (from, to) = install::rollback(&layout)?;
        writeln!(out, "rolled back: {from} -> {to} (the other one is kept; `update --rollback` again flips it back)")?;
        notes(&layout, out)?;
        return Ok(Outcome::RolledBack { from, to });
    }

    let src = Source::new(o.base);
    let v = fetch_verified(&src, o.trust, o.now, !o.check_only, None)?;
    if let Some((a, r)) = v.rotated {
        writeln!(out, "key rotation applied: {a} added, {r} revoked (minerva keys list)")?;
    }
    if let Some(w) = &v.stale {
        writeln!(out, "note: {w}")?;
    }
    let m = v.manifest;
    let trust = v.trust;
    let today = keys::date_of(o.now);
    if !is_newer(&m.version, o.current)? {
        writeln!(out, "up to date ({}); the newest release is {}", o.current, m.full)?;
        return Ok(Outcome::UpToDate);
    }
    if o.check_only {
        writeln!(out, "Minerva {} is available (you have {}); run: minerva update", m.version, o.current)?;
        return Ok(Outcome::Available(m.version));
    }

    writeln!(out, "downloading {} ({} bytes)", m.bundle.name, m.bundle.size)?;
    let work = layout.opt().join(format!(".update-{}", std::process::id()));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).with_context(|| format!("creating {}", work.display()))?;
    let result = (|| -> Result<String> {
        let tgz = work.join(&m.bundle.name);
        src.get_to(&m.bundle.name, &tgz, MAX_BUNDLE)?;
        let bsig = String::from_utf8(src.get(&format!("{}.minisig", m.bundle.name), MAX_SMALL, None).context("fetching the bundle signature")?)
            .context("the bundle signature is not text")?;
        let bytes = fs::read(&tgz)?;
        trust.verify(&bytes, &bsig, &today).context("the bundle")?;
        let sum = hex(&Sha256::digest(&bytes));
        if !sum.eq_ignore_ascii_case(&m.bundle.sha256) || bytes.len() as u64 != m.bundle.size {
            bail!("the bundle does not match the signed manifest (sha-256 or size); refusing");
        }
        writeln!(out, "signatures ok, sha-256 ok")?;
        let top = install::extract_tar_gz(&bytes[..], &work.join("x"))?;
        let ver = fs::read_to_string(top.join("VERSION")).unwrap_or_default();
        if ver.trim() != m.full {
            bail!("the bundle says it is {:?}, the signed manifest says {:?}; refusing", ver.trim(), m.full);
        }
        let done = install::install_from_dir(&top, &layout)?;
        Ok(done.full)
    })();
    let _ = fs::remove_dir_all(&work);
    let to = result?;
    let from = o.current.to_string();
    writeln!(out, "installed {to}; the previous version is kept (minerva update --rollback)")?;
    notes(&layout, out)?;
    Ok(Outcome::Updated { from, to })
}

/// A running perch keeps running the old code: say so, never kill it.
fn notes(layout: &Layout, out: &mut dyn Write) -> Result<()> {
    if !perch_pids().is_empty() {
        writeln!(out, "a Minerva server is running; it keeps the old version until restarted: hoot stop web, then hoot (Linux: minerva stop, then minerva web)")?;
    }
    let _ = layout;
    Ok(())
}

/// Live perch pids from `$XDG_RUNTIME_DIR/minerva/perch-<port>.pid`.
pub fn perch_pids() -> Vec<u32> {
    let Some(rt) = std::env::var_os("XDG_RUNTIME_DIR") else { return Vec::new() };
    let dir: PathBuf = Path::new(&rt).join("minerva");
    let Ok(rd) = fs::read_dir(dir) else { return Vec::new() };
    rd.flatten()
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with("perch-") && n.ends_with(".pid")
        })
        .filter_map(|e| fs::read_to_string(e.path()).ok()?.trim().parse::<u32>().ok())
        .filter(|pid| Path::new(&format!("/proc/{pid}")).exists())
        .collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(test)]
mod tests;
