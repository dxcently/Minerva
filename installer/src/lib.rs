//! The Minerva installer and updater. See `installer/README.md`.
//!
//! Cross-platform pieces (version order, manifest, signature check, PATH list, WSL list parsing) compile everywhere
//! so the Windows exe and the Linux `minerva` share them; the file-system install/update logic is Unix-only and
//! lives in exactly one place (`install`, `update`), used by `minerva install` (which `install.sh` and the Windows exe
//! both call) and by `minerva update`.

pub mod manifest;
pub mod pathlist;
pub mod verify;
pub mod version;
pub mod wslparse;

#[cfg(unix)]
pub mod fetch;
#[cfg(unix)]
pub mod install;
#[cfg(unix)]
pub mod jev;
#[cfg(unix)]
pub mod keys;
#[cfg(unix)]
pub mod notice;
#[cfg(unix)]
pub mod plugins;
#[cfg(unix)]
pub mod update;

/// `0.1.0`: the version in the repo-root `VERSION` file (the one source of truth) at build time.
pub const BASE_VERSION: &str = env!("MINERVA_BASE_VERSION");
/// `0.1.0-gabc1234`: the base version plus the short git sha of the build.
pub const FULL_VERSION: &str = env!("MINERVA_FULL_VERSION");
/// Where releases are fetched from: GitHub redirects `latest/download/<asset>` to the newest release's asset.
pub const DEFAULT_BASE: &str = "https://github.com/dxcently/Minerva/releases/latest/download";
/// The optional jev step's offer, word for word on every platform (installer, `minerva install`, `minerva jev-setup`).
pub const JEV_OFFER: &str = "Set up the jev graph chooser? It lets Minerva's jev graphs (triage-linux, wiki-hop) make choices.
It needs Python 3.10+ with venv, downloads CPU torch and friends (about 1.2 GB on disk, a minute or two),
and lives in a venv under the install. Default: no. You can do it later with: minerva jev-setup (hoot jev-setup)";
/// The manifest asset's name; `<name>.minisig` is its signature.
pub const MANIFEST_NAME: &str = "latest.json";
