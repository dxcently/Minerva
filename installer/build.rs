//! Compiles the update public key, the version and (for the Windows exe) the bundle path into the crate.
//!
//! - key:     `MINERVA_UPDATE_PUBKEY` (a file path) or `installer/minisign.pub`. A missing file, or the committed
//!            placeholder, FAILS the build, unless `MINERVA_ALLOW_PLACEHOLDER_KEY=1` (tests only: then every
//!            signature check fails closed).
//! - version: `MINERVA_VERSION_BASE`, else the repo-root `VERSION` file; plus the short git sha (`MINERVA_BUILD_SHA`,
//!            else `git rev-parse --short HEAD`) as `<base>-g<sha>`.
//! - bundle:  `MINERVA_BUNDLE` (a file path), embedded by the Windows exe; absent, an empty stand-in.

use std::path::PathBuf;
use std::process::Command;
use std::{env, fs};

fn main() {
    let dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    for v in ["MINERVA_UPDATE_PUBKEY", "MINERVA_ALLOW_PLACEHOLDER_KEY", "MINERVA_VERSION_BASE", "MINERVA_BUILD_SHA", "MINERVA_BUNDLE"] {
        println!("cargo:rerun-if-env-changed={v}");
    }
    println!("cargo:rerun-if-changed=build.rs");

    // The public key.
    let allow = env::var("MINERVA_ALLOW_PLACEHOLDER_KEY").as_deref() == Ok("1");
    let key_path = env::var_os("MINERVA_UPDATE_PUBKEY").map(PathBuf::from).unwrap_or_else(|| dir.join("minisign.pub"));
    println!("cargo:rerun-if-changed={}", key_path.display());
    let key = match fs::read_to_string(&key_path) {
        Ok(t) => t,
        Err(e) if allow => {
            println!("cargo:warning=update public key missing ({e}); placeholder compiled in, updates will be refused");
            "PLACEHOLDER\n".to_string()
        }
        Err(e) => panic!(
            "update public key not found at {} ({e}). Generate one with `minisign -G` and put the .pub file there \
             (or set MINERVA_UPDATE_PUBKEY=<file>). See installer/README.md.",
            key_path.display()
        ),
    };
    if key.contains("PLACEHOLDER") && !allow {
        panic!(
            "{} is still the placeholder. Put the real minisign PUBLIC key file there (installer/README.md, 'One-time signing \
             setup'), or set MINERVA_UPDATE_PUBKEY=<file>.",
            key_path.display()
        );
    }
    fs::write(out.join("update.pub"), key).unwrap();

    // The version.
    let version_file = dir.join("..").join("VERSION");
    println!("cargo:rerun-if-changed={}", version_file.display());
    let base = env::var("MINERVA_VERSION_BASE")
        .ok()
        .or_else(|| fs::read_to_string(&version_file).ok().map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "0.0.0".to_string());
    let sha = env::var("MINERVA_BUILD_SHA").ok().filter(|s| !s.is_empty()).or_else(|| {
        let o = Command::new("git").args(["rev-parse", "--short", "HEAD"]).current_dir(&dir).output().ok()?;
        o.status.success().then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
    });
    let full = match sha {
        Some(s) if !s.is_empty() => format!("{base}-g{s}"),
        _ => base.clone(),
    };
    println!("cargo:rustc-env=MINERVA_BASE_VERSION={base}");
    println!("cargo:rustc-env=MINERVA_FULL_VERSION={full}");

    // The bundle the Windows exe carries.
    let bundle = match env::var_os("MINERVA_BUNDLE") {
        Some(p) => {
            let p = PathBuf::from(p);
            println!("cargo:rerun-if-changed={}", p.display());
            p.canonicalize().unwrap_or_else(|e| panic!("MINERVA_BUNDLE {}: {e}", p.display()))
        }
        None => {
            let p = out.join("no-bundle.tar.gz");
            fs::write(&p, b"").unwrap();
            p
        }
    };
    println!("cargo:rustc-env=MINERVA_EMBED_BUNDLE={}", bundle.display());
}
