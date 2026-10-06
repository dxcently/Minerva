//! The optional jev graph chooser: a Python + CPU-torch service (`jev/server.py`) behind `jev_choose` / `jev_entail`.
//! Not part of the default install: the installer and `minerva` OFFER it (default no); `minerva jev-setup` runs it any time.
//!
//! ```text
//!   <prefix>/opt/minerva/jev-venv/       the venv (outside the version dirs: an update does not rebuild it)
//!   <version>/jev/                       server.py, jevlike/, runs/*.pt, requirements.txt, requirements.pinned.txt
//!   ~/.config/eidolon/jev.token          0600, made here when missing (the plugin README's step 3)
//! ```
//! On Python 3.12+ the install is two steps, both `--require-hashes --no-deps --isolated`: `requirements.torch.txt` from the
//! PyTorch CPU index ONLY (`--index-url`), then `requirements.pinned.txt` (everything else) from PyPI ONLY. Every file is
//! pinned by sha-256, so neither index can substitute a package, and keeping the indexes apart means PyPI can never answer
//! for `torch` (nor the PyTorch index for anything else): there is no `--extra-index-url` to confuse. Pythons 3.10-3.11 get the
//! ranges in `requirements.txt` (unhashed, said out loud). The ~9 GB `openjev` NLI weights are NOT fetched (`get-openjev.sh`
//! does that by hand); only the 169 KB chooser runs without them.

use std::fs;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use base64::Engine;

use crate::install::Layout;
use crate::plugins::{self, JEV_TOKEN, PluginEnv};

pub const OFFER: &str = crate::JEV_OFFER;

pub fn venv_dir(l: &Layout) -> PathBuf {
    l.opt().join("jev-venv")
}

fn pidfile(l: &Layout) -> PathBuf {
    l.state_dir().join("jev.pid")
}

pub fn is_set_up(l: &Layout) -> bool {
    venv_dir(l).join(".minerva-jev").is_file()
}

fn token_path() -> Result<PathBuf> {
    let base = match std::env::var_os("XDG_CONFIG_HOME").filter(|d| Path::new(d).is_absolute()) {
        Some(x) => PathBuf::from(x),
        None => Path::new(&std::env::var_os("HOME").filter(|h| !h.is_empty()).context("HOME is not set")?).join(".config"),
    };
    Ok(base.join("eidolon/jev.token"))
}

fn python_version(py: &str) -> Result<(u32, u32)> {
    let o = Command::new(py)
        .args(["-c", "import sys; print(sys.version_info[0], sys.version_info[1])"])
        .output()
        .with_context(|| format!("{py} is not installed (apt install python3 python3-venv)"))?;
    let t = String::from_utf8_lossy(&o.stdout);
    let mut it = t.split_whitespace().map(|x| x.parse::<u32>().unwrap_or(0));
    Ok((it.next().unwrap_or(0), it.next().unwrap_or(0)))
}

pub struct SetupOpts<'a> {
    pub python: &'a str,
    pub dry_run: bool,
}

/// Build the venv, install the pinned requirements, make the token, grant it. Returns the lines to show.
pub fn setup(env: &PluginEnv, jev_src: &Path, o: &SetupOpts) -> Result<Vec<String>> {
    let l = &env.layout;
    let mut out = Vec::new();
    if crate::install::is_nix_managed(&l.prefix) {
        bail!("jev-setup is not available in a Nix install (jev-service is a separate Nix slice)");
    }
    let (maj, min) = python_version(o.python)?;
    if (maj, min) < (3, 10) {
        bail!("jev needs Python 3.10 or newer; {} is {maj}.{min}", o.python);
    }
    let hashed = (maj, min) >= (3, 12);
    let req = if hashed { "requirements.pinned.txt" } else { "requirements.txt" };
    let req_path = jev_src.join(req);
    let torch_path = jev_src.join("requirements.torch.txt");
    for p in if hashed { vec![&req_path, &torch_path] } else { vec![&req_path] } {
        if !p.is_file() {
            bail!("{} is missing; is this a Minerva bundle?", p.display());
        }
    }
    let venv = venv_dir(l);
    out.push(format!(
        "python {maj}.{min}; requirements: {}",
        if hashed { "requirements.torch.txt + requirements.pinned.txt (sha-256 pinned, torch from the PyTorch CPU index only, the rest from PyPI only)" } else { "requirements.txt (UNPINNED and not hash-checked: Python older than 3.12)" }
    ));
    out.push(format!("venv: {}", venv.display()));
    out.push(format!("token: {} (made when missing, mode 600)", token_path()?.display()));
    if o.dry_run {
        out.push("dry run: nothing was created".into());
        return Ok(out);
    }

    let marker = venv.join(".minerva-jev");
    if !venv.join("bin/python").exists() {
        let _ = fs::remove_dir_all(&venv);
        let st = Command::new(o.python).args(["-m", "venv"]).arg(&venv).stdin(Stdio::null()).status().context("running python -m venv")?;
        if !st.success() {
            let _ = fs::remove_dir_all(&venv);
            bail!("`python3 -m venv` failed. On Ubuntu/Debian: sudo apt install python3-venv, then run this again");
        }
    }
    let pip = |args: &[&str], file: &Path| -> Result<()> {
        let st = Command::new(venv.join("bin/pip"))
            .args(["install", "--isolated", "--no-cache-dir", "--disable-pip-version-check"])
            .args(args)
            .arg("-r")
            .arg(file)
            .stdin(Stdio::null())
            .status()
            .context("running pip")?;
        if !st.success() {
            bail!("pip could not install {} (network? disk space? a hash that no longer matches?); fix that and run minerva jev-setup again", file.display());
        }
        Ok(())
    };
    if hashed {
        pip(&["--require-hashes", "--no-deps", "--index-url", "https://download.pytorch.org/whl/cpu"], &torch_path)?;
        pip(&["--require-hashes", "--no-deps", "--index-url", "https://pypi.org/simple"], &req_path)?;
    } else {
        pip(&[], &req_path)?;
    }
    fs::write(&marker, format!("{req}\n"))?;
    out.push("venv ready".into());

    // the token the service and the two scorer tools share
    let tok = token_path()?;
    if !tok.exists() {
        fs::create_dir_all(tok.parent().unwrap())?;
        let mut raw = [0u8; 24];
        fs::File::open("/dev/urandom")?.read_exact(&mut raw)?;
        let text = base64::engine::general_purpose::STANDARD.encode(raw);
        let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tok).context("creating jev.token")?;
        std::io::Write::write_all(&mut f, text.as_bytes())?;
        out.push("token made".into());
    }
    if plugins::load_state(l).plugins.contains_key("jev") {
        for v in ["choose", "entail"] {
            let (ok, text) = env.eidolon(&["plugins", "grant", &format!("jev_{v}"), JEV_TOKEN])?;
            if !ok {
                bail!("granting jev_{v}: {}", text.trim());
            }
        }
        out.push("jev_choose and jev_entail may read the token".into());
    } else {
        out.push("note: the jev plugin here is not one Minerva installed, so no grants were made".into());
    }
    out.push("start it with: minerva services start (the entail guard also needs the openjev weights; see jev/get-openjev.sh)".into());
    Ok(out)
}

fn pid_alive(l: &Layout) -> Option<u32> {
    let pid: u32 = fs::read_to_string(pidfile(l)).ok()?.trim().parse().ok()?;
    let cmd = fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    String::from_utf8_lossy(&cmd).contains("server.py").then_some(pid)
}

/// Start the service from the venv (detached) when it is set up and nothing of ours is running.
pub fn start(l: &Layout, jev_src: &Path) -> Result<String> {
    if !is_set_up(l) {
        return Ok("jev service: not set up (minerva jev-setup)".into());
    }
    if let Some(p) = pid_alive(l) {
        return Ok(format!("jev service: already running (pid {p})"));
    }
    let token = fs::read_to_string(token_path()?).context("jev.token is missing; run minerva jev-setup")?;
    fs::create_dir_all(l.state_dir())?;
    let log = fs::OpenOptions::new().create(true).append(true).open(l.state_dir().join("jev.log"))?;
    let child = {
        use std::os::unix::process::CommandExt;
        Command::new(venv_dir(l).join("bin/python"))
            .args(["-u", "server.py"])
            .current_dir(jev_src)
            .env("EIDOLON_SERVICE_PORT", "8091")
            .env("EIDOLON_SERVICE_TOKEN", token.trim())
            .env("HF_HUB_OFFLINE", "1")
            .env("TRANSFORMERS_OFFLINE", "1")
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .process_group(0)
            .spawn()
            .context("starting the jev service")?
    };
    fs::write(pidfile(l), child.id().to_string())?;
    Ok(format!("jev service: started (pid {}, port 8091, log {})", child.id(), l.state_dir().join("jev.log").display()))
}

pub fn stop(l: &Layout) -> Result<String> {
    match pid_alive(l) {
        None => Ok("jev service: not running".into()),
        Some(pid) => {
            let _ = Command::new("kill").arg(pid.to_string()).status();
            let _ = fs::remove_file(pidfile(l));
            Ok(format!("jev service: stopped (pid {pid})"))
        }
    }
}

pub fn status(l: &Layout) -> String {
    match (is_set_up(l), pid_alive(l)) {
        (false, _) => "jev service: not set up (minerva jev-setup)".into(),
        (true, Some(p)) => format!("jev service: running (pid {p})"),
        (true, None) => "jev service: set up, not running".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offer_text_defaults_to_no_and_names_the_size() {
        assert!(OFFER.contains("Default: no"));
        assert!(OFFER.contains("1.2 GB"));
        assert!(OFFER.contains("minerva jev-setup"));
    }

    #[test]
    fn dry_run_creates_nothing() {
        let t = tempfile::tempdir().unwrap();
        let l = Layout::new(t.path().join("p"));
        let src = t.path().join("jev");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("requirements.pinned.txt"), "x\n").unwrap();fs::write(src.join("requirements.torch.txt"), "x\n").unwrap();
        fs::write(src.join("requirements.txt"), "x\n").unwrap();
        let env = PluginEnv { eidolon: "eidolon".into(), bin_dir: t.path().into(), src: t.path().into(), layout: l.clone() };
        if Command::new("python3").arg("--version").output().is_err() {
            return;
        }
        let lines = setup(&env, &src, &SetupOpts { python: "python3", dry_run: true }).unwrap();
        assert!(lines.iter().any(|x| x.contains("dry run")), "{lines:?}");
        assert!(!venv_dir(&l).exists() && !l.prefix.exists());
        assert!(!is_set_up(&l));
    }
}
