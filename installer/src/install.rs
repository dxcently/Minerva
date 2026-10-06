//! The ONE install logic: lay a bundle down under `<prefix>/opt/minerva/<version>/`, flip `current`, keep `previous`.
//! `install.sh`, `minerva-install.exe` (inside WSL) and `minerva update` all end up in `install_from_dir`.
//!
//! ```text
//! <prefix>/                       default ~/.local
//!   opt/minerva/<full>/           one complete version (bin/ webui/ windows/ VERSION), marked by .minerva-complete
//!   opt/minerva/current  -> <full>      relative symlink, flipped atomically
//!   opt/minerva/previous -> <old>       the one kept for --rollback
//!   bin/minerva -> ../opt/minerva/current/bin/minerva
//!   state/minerva/update-check          the once-a-day notice cache
//! ```
//! Nothing outside `opt/minerva/` and those two files is ever created, changed or removed; in particular never
//! `~/.local/opt/eidolon*`, and never the Minerva home (`~/Minerva`: chats and settings).

use std::fs;
use std::io::Read;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::version::check_dir_name;

pub const COMPLETE: &str = ".minerva-complete";

#[derive(Debug, Clone)]
pub struct Layout {
    pub prefix: PathBuf,
}

impl Layout {
    pub fn new(prefix: impl Into<PathBuf>) -> Layout {
        Layout { prefix: prefix.into() }
    }

    /// `$HOME/.local`
    pub fn default_prefix() -> Result<PathBuf> {
        let home = std::env::var_os("HOME").filter(|h| !h.is_empty()).context("HOME is not set")?;
        Ok(Path::new(&home).join(".local"))
    }

    /// The layout an installed `minerva` binary lives in: `<prefix>/opt/minerva/<ver>/bin/minerva`.
    pub fn from_exe(exe: &Path) -> Result<Layout> {
        let real = exe.canonicalize().with_context(|| format!("resolving {}", exe.display()))?;
        let names: Vec<String> = real.ancestors().skip(1).take(4).map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()).collect();
        // ancestors().skip(1): bin, <ver>, minerva, opt
        if names.len() == 4 && names[0] == "bin" && names[2] == "minerva" && names[3] == "opt" {
            let prefix = real.ancestors().nth(5).context("no prefix above the install")?;
            return Ok(Layout::new(prefix));
        }
        bail!("{} is not an installed copy (expected <prefix>/opt/minerva/<version>/bin/minerva)", real.display())
    }

    pub fn opt(&self) -> PathBuf {
        self.prefix.join("opt/minerva")
    }
    pub fn version_dir(&self, full: &str) -> PathBuf {
        self.opt().join(full)
    }
    pub fn current(&self) -> PathBuf {
        self.opt().join("current")
    }
    pub fn previous(&self) -> PathBuf {
        self.opt().join("previous")
    }
    pub fn bin_link(&self) -> PathBuf {
        self.prefix.join("bin/minerva")
    }
    pub fn state_dir(&self) -> PathBuf {
        self.prefix.join("state/minerva")
    }

    /// The version `current` points at, if any (the symlink's own name, never a path).
    pub fn current_version(&self) -> Option<String> {
        link_name(&self.current())
    }
    pub fn previous_version(&self) -> Option<String> {
        link_name(&self.previous())
    }
}

fn link_name(link: &Path) -> Option<String> {
    let t = fs::read_link(link).ok()?;
    let n = t.file_name()?.to_string_lossy().into_owned();
    (check_dir_name(&n).is_ok() && link.parent()?.join(&n).join(COMPLETE).exists()).then_some(n)
}

/// A path that is, or lives inside, the Nix store: Nix owns it, we must not rewrite it.
pub fn is_nix_managed(p: &Path) -> bool {
    p.starts_with("/nix/store") || fs::canonicalize(p).map(|c| c.starts_with("/nix/store")).unwrap_or(false)
}

fn set_link(link: &Path, target: &str) -> Result<()> {
    let tmp = link.with_extension("new");
    let _ = fs::remove_file(&tmp);
    symlink(target, &tmp).with_context(|| format!("creating {}", tmp.display()))?;
    fs::rename(&tmp, link).with_context(|| format!("replacing {}", link.display()))
}

#[derive(Debug, PartialEq, Eq)]
pub struct Installed {
    pub full: String,
    pub previous: Option<String>,
    /// False when this exact version was already installed (a rerun).
    pub fresh: bool,
}

/// Install the unpacked bundle in `src` (it must contain `VERSION` and `bin/minerva`).
pub fn install_from_dir(src: &Path, layout: &Layout) -> Result<Installed> {
    let full = fs::read_to_string(src.join("VERSION")).with_context(|| format!("{} has no VERSION file; not a Minerva bundle", src.display()))?;
    let full = full.trim().to_string();
    check_dir_name(&full)?;
    for must in ["bin/minerva", "bin/perch", "bin/eidolon", "bin/perch-open.sh", "webui/term/index.html"] {
        if !src.join(must).exists() {
            bail!("the bundle in {} lacks {must}; refusing a partial bundle", src.display());
        }
    }
    let opt = layout.opt();
    fs::create_dir_all(&opt).with_context(|| format!("creating {}", opt.display()))?;
    let dest = layout.version_dir(&full);
    let fresh = !dest.join(COMPLETE).exists();
    if fresh {
        let part = opt.join(format!(".{full}.partial"));
        let _ = fs::remove_dir_all(&part);
        copy_tree(src, &part)?;
        fs::write(part.join(COMPLETE), b"")?;
        let _ = fs::remove_dir_all(&dest); // a half-written earlier attempt without the marker
        fs::rename(&part, &dest).with_context(|| format!("moving into {}", dest.display()))?;
    }
    let old = layout.current_version();
    if old.as_deref() != Some(full.as_str()) {
        if let Some(o) = &old {
            set_link(&layout.previous(), o)?;
        }
        set_link(&layout.current(), &full)?;
    }
    prune(layout)?;
    link_command(layout)?;
    Ok(Installed { full, previous: layout.previous_version(), fresh })
}

/// Flip back to `previous`; `previous` then names the version we left, so a second rollback undoes the first.
pub fn rollback(layout: &Layout) -> Result<(String, String)> {
    let cur = layout.current_version().context("nothing is installed")?;
    let prev = layout.previous_version().context("there is no previous version kept; nothing to roll back to")?;
    set_link(&layout.previous(), &cur)?;
    set_link(&layout.current(), &prev)?;
    Ok((cur, prev))
}

/// Keep `current` and `previous`; delete every other complete version dir and any leftover `.partial`.
/// Only directories that carry our `.minerva-complete` marker (or are our `.partial`) are ever removed.
fn prune(layout: &Layout) -> Result<()> {
    let keep: Vec<String> = [layout.current_version(), layout.previous_version()].into_iter().flatten().collect();
    for e in fs::read_dir(layout.opt())? {
        let e = e?;
        let name = e.file_name().to_string_lossy().into_owned();
        let p = e.path();
        if !e.file_type()?.is_dir() {
            continue;
        }
        if name.starts_with('.') && name.ends_with(".partial") {
            let _ = fs::remove_dir_all(&p);
        } else if p.join(COMPLETE).exists() && !keep.contains(&name) {
            fs::remove_dir_all(&p).with_context(|| format!("removing the old version {}", p.display()))?;
        }
    }
    Ok(())
}

/// `<prefix>/bin/minerva` -> `../opt/minerva/current/bin/minerva`. A file that is not our symlink is left alone.
fn link_command(layout: &Layout) -> Result<()> {
    let link = layout.bin_link();
    fs::create_dir_all(link.parent().unwrap())?;
    match fs::symlink_metadata(&link) {
        Ok(m) if !m.file_type().is_symlink() => {
            eprintln!("note: {} exists and is not a link; left alone (the command is {}/bin/minerva)", link.display(), layout.current().display());
            Ok(())
        }
        _ => set_link(&link, "../opt/minerva/current/bin/minerva"),
    }
}

/// Remove what the installer put there. Returns the paths removed. Never the Minerva home.
pub fn uninstall(layout: &Layout) -> Result<Vec<PathBuf>> {
    let mut gone = Vec::new();
    let opt = layout.opt();
    if opt.file_name().and_then(|n| n.to_str()) != Some("minerva") || opt.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()) != Some("opt") {
        bail!("refusing to remove {}", opt.display());
    }
    let link = layout.bin_link();
    if let Ok(t) = fs::read_link(&link) {
        if t.to_string_lossy().contains("opt/minerva/") {
            fs::remove_file(&link)?;
            gone.push(link);
        }
    }
    if opt.exists() {
        fs::remove_dir_all(&opt).with_context(|| format!("removing {}", opt.display()))?;
        gone.push(opt);
    }
    let st = layout.state_dir();
    if st.exists() {
        fs::remove_dir_all(&st)?;
        gone.push(st);
    }
    Ok(gone)
}

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for e in fs::read_dir(from)? {
        let e = e?;
        let (f, t) = (e.path(), to.join(e.file_name()));
        let ty = e.file_type()?;
        if ty.is_dir() {
            copy_tree(&f, &t)?;
        } else if ty.is_symlink() {
            symlink(fs::read_link(&f)?, &t)?;
        } else {
            fs::copy(&f, &t).with_context(|| format!("copying {}", f.display()))?; // keeps the mode (the executable bit)
        }
    }
    Ok(())
}

/// Unpack a `.tar.gz` into `dest` (the `tar` crate refuses `..` and absolute paths). Returns the single top directory.
pub fn extract_tar_gz(reader: impl Read, dest: &Path) -> Result<PathBuf> {
    fs::create_dir_all(dest)?;
    let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(reader));
    ar.set_preserve_permissions(true);
    ar.set_overwrite(true);
    ar.unpack(dest).context("unpacking the bundle")?;
    let mut tops = fs::read_dir(dest)?.collect::<std::io::Result<Vec<_>>>()?;
    tops.retain(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false));
    match tops.as_slice() {
        [one] => Ok(one.path()),
        _ => bail!("the bundle should hold exactly one top-level directory"),
    }
}

#[cfg(test)]
pub mod testbundle {
    use super::*;

    /// A minimal fake bundle directory with the files `install_from_dir` insists on.
    pub fn make(dir: &Path, full: &str) {
        for f in ["bin/minerva", "bin/perch", "bin/eidolon", "bin/perch-open.sh", "webui/term/index.html", "windows/hoot.ps1"] {
            let p = dir.join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, format!("{full} {f}")).unwrap();
        }
        fs::write(dir.join("VERSION"), format!("{full}\n")).unwrap();
    }

    /// The same, packed as `minerva-<full>-linux-x86_64.tar.gz` bytes (top dir `minerva-<full>`).
    pub fn targz(full: &str) -> Vec<u8> {
        let tmp = tempfile::tempdir().unwrap();
        let top = tmp.path().join(format!("minerva-{full}"));
        make(&top, full);
        let mut out = Vec::new();
        {
            let gz = flate2::write::GzEncoder::new(&mut out, flate2::Compression::fast());
            let mut b = tar::Builder::new(gz);
            b.append_dir_all(format!("minerva-{full}"), &top).unwrap();
            b.into_inner().unwrap().finish().unwrap();
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_rerun_upgrade_prune_rollback_uninstall() {
        let t = tempfile::tempdir().unwrap();
        let l = Layout::new(t.path().join("p"));
        fs::create_dir_all(l.prefix.join("opt/eidolon-master")).unwrap(); // someone else's: must survive everything
        let src = |v: &str| {
            let d = t.path().join(format!("src-{v}"));
            testbundle::make(&d, v);
            d
        };
        let a = install_from_dir(&src("0.1.0-ga"), &l).unwrap();
        assert!(a.fresh && a.previous.is_none());
        let again = install_from_dir(&src("0.1.0-ga"), &l).unwrap();
        assert!(!again.fresh);
        assert_eq!(l.current_version().as_deref(), Some("0.1.0-ga"));
        let b = install_from_dir(&src("0.2.0-gb"), &l).unwrap();
        assert_eq!(b.previous.as_deref(), Some("0.1.0-ga"));
        let _ = install_from_dir(&src("0.3.0-gc"), &l).unwrap();
        assert!(!l.version_dir("0.1.0-ga").exists(), "the second-oldest is pruned");
        assert!(l.version_dir("0.2.0-gb").exists());
        let (from, to) = rollback(&l).unwrap();
        assert_eq!((from.as_str(), to.as_str()), ("0.3.0-gc", "0.2.0-gb"));
        assert_eq!(l.current_version().as_deref(), Some("0.2.0-gb"));
        assert_eq!(fs::read_link(l.bin_link()).unwrap().to_str().unwrap(), "../opt/minerva/current/bin/minerva");
        assert!(l.bin_link().exists());
        let gone = uninstall(&l).unwrap();
        assert!(!gone.is_empty() && !l.opt().exists() && fs::symlink_metadata(l.bin_link()).is_err());
        assert!(l.prefix.join("opt/eidolon-master").exists());
    }

    #[test]
    fn refuses_partial_bundle_and_foreign_bin() {
        let t = tempfile::tempdir().unwrap();
        let l = Layout::new(t.path());
        let d = t.path().join("s");
        fs::create_dir_all(&d).unwrap();
        fs::write(d.join("VERSION"), "0.1.0\n").unwrap();
        assert!(install_from_dir(&d, &l).is_err());
        let d2 = t.path().join("s2");
        testbundle::make(&d2, "0.1.0");
        fs::create_dir_all(l.prefix.join("bin")).unwrap();
        fs::write(l.bin_link(), "mine").unwrap();
        install_from_dir(&d2, &l).unwrap();
        assert_eq!(fs::read_to_string(l.bin_link()).unwrap(), "mine");
    }

    #[test]
    fn layout_from_exe_and_nix() {
        let t = tempfile::tempdir().unwrap();
        let l = Layout::new(t.path().canonicalize().unwrap());
        let d = t.path().join("s");
        testbundle::make(&d, "0.1.0");
        install_from_dir(&d, &l).unwrap();
        let via_current = l.current().join("bin/minerva");
        assert_eq!(Layout::from_exe(&via_current).unwrap().prefix, l.prefix);
        assert!(Layout::from_exe(&d.join("bin/minerva")).is_err());
        assert!(is_nix_managed(Path::new("/nix/store/abc-minerva/bin/minerva")));
        assert!(!is_nix_managed(&via_current));
    }

    #[test]
    fn extract_round_trip() {
        let t = tempfile::tempdir().unwrap();
        let top = extract_tar_gz(&testbundle::targz("0.1.0-gx")[..], t.path()).unwrap();
        assert_eq!(top.file_name().unwrap(), "minerva-0.1.0-gx");
        assert!(top.join("bin/minerva").exists());
    }
}

/// The bundled Chromium needs glibc 2.35 or newer (Ubuntu 22.04+, Debian 12+). `ldd_text` is `ldd --version`'s output.
/// `Some(sentence)` when the host is known to be older (or not glibc); `None` when it is fine or cannot be told.
pub fn glibc_warning(ldd_text: &str) -> Option<String> {
    let first = ldd_text.lines().find(|l| !l.trim().is_empty())?;
    if first.to_ascii_lowercase().contains("musl") {
        return Some("this system uses musl, not glibc; the browser plugin's Chromium needs glibc 2.35+ (Ubuntu 22.04+ or Debian 12+) and will not run here. Everything else is installed.".into());
    }
    let ver = first.split_whitespace().last()?;
    let mut it = ver.split('.');
    let (maj, min): (u32, u32) = (it.next()?.parse().ok()?, it.next()?.parse().ok()?);
    if (maj, min) < (2, 35) {
        return Some(format!(
            "this system has glibc {maj}.{min}; the browser plugin's Chromium needs glibc 2.35+ (Ubuntu 22.04+ or Debian 12+) and will not run here. Everything else is installed."
        ));
    }
    None
}

#[cfg(test)]
mod glibc_tests {
    use super::glibc_warning;

    #[test]
    fn warns_below_2_35_only() {
        assert!(glibc_warning("ldd (Ubuntu GLIBC 2.31-0ubuntu9.16) 2.31\nCopyright").unwrap().contains("2.31"));
        assert!(glibc_warning("ldd (Debian GLIBC 2.28-10) 2.28").is_some());
        assert!(glibc_warning("ldd (Ubuntu GLIBC 2.35-0ubuntu3.8) 2.35").is_none());
        assert!(glibc_warning("ldd (GNU libc) 2.43").is_none());
        assert!(glibc_warning("musl libc (x86_64)\nVersion 1.2.4").unwrap().contains("musl"));
        assert!(glibc_warning("").is_none(), "unknown: say nothing");
        assert!(glibc_warning("something else entirely").is_none());
    }
}
