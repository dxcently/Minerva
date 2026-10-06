//! `latest.json`: what a release is. Signed (`latest.json.minisig`); the bundle is signed too, and its sha-256 is
//! pinned here, so a swapped bundle fails twice.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::version::check_dir_name;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileEntry {
    pub name: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Manifest {
    pub schema: u32,
    /// `0.1.0`
    pub version: String,
    /// `0.1.0-gabc1234`
    pub full: String,
    /// The Linux bundle `minerva update` installs.
    pub bundle: FileEntry,
    /// Everything else in the release (install.sh, minerva-install.exe, ...), for the people who download by hand.
    #[serde(default)]
    pub files: Vec<FileEntry>,
    /// Unix seconds this manifest was signed (`release-files.sh`, and again by `release-files.sh resign`).
    #[serde(default)]
    pub released: Option<u64>,
    /// `released` + 30 days. Past it the release info is "stale": a warning, never a refusal.
    #[serde(default)]
    pub expires: Option<u64>,
    /// The key-rotation serial this release needs the machine to have applied. If it is above the local one, `keys.json`
    /// must come with the release, else the update is refused (a withheld rotation cannot be skipped).
    #[serde(default)]
    pub keys_serial: Option<u64>,
}

/// How long a signed manifest counts as fresh.
pub const FRESH_SECS: u64 = 30 * 24 * 3600;

impl Manifest {
    /// `Some(warning)` when the signed freshness window has passed. Warn only; the update still works.
    pub fn stale(&self, now: u64) -> Option<String> {
        match (self.released, self.expires) {
            (Some(r), Some(e)) if now > e => {
                Some(format!("release info is stale (signed {}) -- the maintainer should re-sign", crate::version::date_of(r)))
            }
            _ => None,
        }
    }
}

pub fn parse(bytes: &[u8]) -> Result<Manifest> {
    let m: Manifest = serde_json::from_slice(bytes).context("the release manifest is not valid JSON of the expected shape")?;
    if m.schema != 1 {
        bail!("the release manifest has schema {}, this program understands 1; reinstall by hand", m.schema);
    }
    crate::version::parse_base(&m.version)?;
    check_dir_name(&m.full)?;
    for f in std::iter::once(&m.bundle).chain(m.files.iter()) {
        if f.name.is_empty() || f.name.contains(['/', '\\']) || f.name.starts_with('.') {
            bail!("the release manifest names a file with a path in it: {:?}", f.name);
        }
        if f.sha256.len() != 64 || !f.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
            bail!("the release manifest has a malformed sha-256 for {}", f.name);
        }
    }
    Ok(m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(name: &str) -> String {
        format!(
            r#"{{"schema":1,"version":"0.2.0","full":"0.2.0-gabc","bundle":{{"name":"{name}","sha256":"{}","size":3}}}}"#,
            "a".repeat(64)
        )
    }

    #[test]
    fn staleness_is_a_warning_with_the_signed_date() {
        let mut m = parse(sample("b.tar.gz").as_bytes()).unwrap();
        assert!(m.stale(9_999_999_999).is_none(), "no timestamps: nothing to be stale about");
        m.released = Some(1_791_269_513);
        m.expires = Some(1_791_269_513 + FRESH_SECS);
        assert!(m.stale(1_791_269_513 + FRESH_SECS).is_none());
        let w = m.stale(1_791_269_513 + FRESH_SECS + 1).unwrap();
        assert!(w.contains("stale") && w.contains("2026-10-06") && w.contains("re-sign"), "{w}");
    }

    #[test]
    fn parses_and_refuses_paths() {
        assert_eq!(parse(sample("b.tar.gz").as_bytes()).unwrap().full, "0.2.0-gabc");
        assert!(parse(sample("../b.tar.gz").as_bytes()).is_err());
        assert!(parse(b"{}").is_err());
    }
}
