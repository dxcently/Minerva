//! Who may sign a release. The trusted set is:
//!
//! ```text
//!   the key compiled into this binary                      (build.rs; installer/minisign.pub)
//! + <keys dir>/*.pub                                       (added by hand, or by a signed rotation)
//! - every id in <keys dir>/state.json "revoked"            (a revoked key is never accepted again, compiled-in or not)
//! - keys past their expiry date
//! ```
//!
//! `<keys dir>` is `~/.config/minerva/update-keys` (0700; files 0600; `MINERVA_KEYS_DIR` overrides, for tests). A
//! directory or file anyone else can write is NOT trusted: the whole directory is ignored with a warning.
//!
//! A release may carry `keys.json` + `keys.json.minisig`: a rotation (add keys, revoke keys, with an expiry) signed by a
//! key that is trusted at that moment. The updater applies it before it verifies the manifest and the bundle.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Compiled,
    Manual,
    Rotation,
}

impl Source {
    pub fn label(&self) -> &'static str {
        match self {
            Source::Compiled => "compiled in",
            Source::Manual => "added by hand",
            Source::Rotation => "added by a signed rotation",
        }
    }
}

#[derive(Debug, Clone)]
pub struct KeyInfo {
    /// 16 upper-case hex digits, the way `minisign` prints a key id.
    pub id: String,
    /// The first 16 hex digits of the sha-256 of the 32 key bytes, in groups of four: what a person types back to confirm.
    pub fingerprint: String,
    /// The base64 line of the public key file.
    pub b64: String,
    pub comment: String,
    pub source: Source,
    pub expires: Option<String>,
}

fn hex_id(bytes8: &[u8]) -> String {
    // minisign prints the id as a little-endian u64
    bytes8.iter().rev().map(|b| format!("{b:02X}")).collect()
}

/// Parse a minisign public key file's text.
pub fn parse_pub(text: &str, source: Source) -> Result<KeyInfo> {
    let mut comment = String::new();
    let mut b64 = None;
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Some(c) = line.strip_prefix("untrusted comment:") {
            comment = c.trim().to_string();
        } else if b64.is_none() {
            b64 = Some(line.to_string());
        }
    }
    let b64 = b64.ok_or_else(|| anyhow!("no key in that file"))?;
    let raw = B64.decode(&b64).map_err(|_| anyhow!("that is not a minisign public key (bad base64)"))?;
    if raw.len() != 42 || &raw[..2] != b"Ed" {
        bail!("that is not a minisign public key (expected 42 bytes starting with Ed)");
    }
    let d = Sha256::digest(&raw[10..]);
    let hex: String = d.iter().take(8).map(|b| format!("{b:02x}")).collect();
    let fingerprint = hex.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).unwrap()).collect::<Vec<_>>().join(" ");
    Ok(KeyInfo { id: hex_id(&raw[2..10]), fingerprint, b64, comment, source, expires: None })
}

/// The key id a signature file says it was made with.
pub fn sig_key_id(sig_text: &str) -> Result<String> {
    let line = sig_text.lines().map(str::trim).filter(|l| !l.is_empty()).nth(1).ok_or_else(|| anyhow!("the signature file is malformed"))?;
    let raw = B64.decode(line).map_err(|_| anyhow!("the signature file is malformed (bad base64)"))?;
    if raw.len() != 74 || !(&raw[..2] == b"Ed" || &raw[..2] == b"ED") {
        bail!("the signature file is malformed (unexpected length)");
    }
    Ok(hex_id(&raw[2..10]))
}

pub fn valid_id(s: &str) -> bool {
    s.len() == 16 && s.chars().all(|c| c.is_ascii_hexdigit())
}

pub use crate::version::date_of;

pub fn today() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    date_of(now)
}

fn valid_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10 && b[4] == b'-' && b[7] == b'-' && b.iter().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    /// The serial of the last rotation applied; a rotation with a serial at or below it is already applied (or a replay).
    #[serde(default)]
    pub serial: u64,
    #[serde(default)]
    pub revoked: Vec<Revoked>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Revoked {
    pub id: String,
    pub on: String,
    pub by: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Meta {
    #[serde(default)]
    expires: Option<String>,
    source: String,
    /// The id of the key whose signature added this one ("manual" for a key added by hand). Revoking a key revokes
    /// every key it added, transitively (see `plan_rotation`).
    #[serde(default)]
    by: Option<String>,
}

const RECOVER: &str = "To recover: restore state.json from a backup; or, accepting that revoked keys are forgotten, move the directory aside \
(mv <dir> <dir>.old) and add your keys again with `minerva keys add`";

/// A rotation may move the serial by at most this much, so a leaked key cannot jump it to the end of the range and
/// make every later (real) rotation look "already applied".
pub const MAX_SERIAL_JUMP: u64 = 16;

/// The trust anchor and where the extra keys live.
#[derive(Debug, Clone)]
pub struct Trust {
    /// The compiled-in public key file text (may be the placeholder in a test build: then no compiled key).
    pub compiled: String,
    pub dir: PathBuf,
    /// A rotation applied in memory only (`--check`, the daily notice): never written to disk.
    mem_keys: Vec<KeyInfo>,
    mem_revoked: Vec<Revoked>,
    mem_serial: u64,
}

impl Trust {
    pub fn new(compiled: &str, dir: impl Into<PathBuf>) -> Trust {
        Trust { compiled: compiled.to_string(), dir: dir.into(), mem_keys: Vec::new(), mem_revoked: Vec::new(), mem_serial: 0 }
    }

    /// `$XDG_CONFIG_HOME/minerva/update-keys`, else `$HOME/.config/minerva/update-keys`. `MINERVA_KEYS_DIR` overrides it in
    /// test builds only (`cargo test`, or `--features test-overrides`); a release binary has no such back door.
    pub fn default_dir() -> Result<PathBuf> {
        #[cfg(any(test, feature = "test-overrides"))]
        if let Some(d) = std::env::var_os("MINERVA_KEYS_DIR").filter(|d| !d.is_empty()) {
            return Ok(PathBuf::from(d));
        }
        if let Some(x) = std::env::var_os("XDG_CONFIG_HOME").filter(|d| Path::new(d).is_absolute()) {
            return Ok(Path::new(&x).join("minerva/update-keys"));
        }
        let home = std::env::var_os("HOME").filter(|h| !h.is_empty()).context("HOME is not set")?;
        Ok(Path::new(&home).join(".config/minerva/update-keys"))
    }

    fn compiled_key(&self) -> Option<KeyInfo> {
        parse_pub(&self.compiled, Source::Compiled).ok()
    }

    /// Is the keys dir ours and closed to others? `Ok(false)` when it does not exist yet.
    fn dir_safe(&self) -> Result<bool> {
        let m = match fs::metadata(&self.dir) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e).with_context(|| format!("reading {}", self.dir.display())),
        };
        let me = unsafe { libc_geteuid() };
        if !m.is_dir() || m.uid() != me || m.mode() & 0o077 != 0 {
            bail!(
                "{} is not a private directory of yours (owner, or mode {:o}), so its record of trusted and revoked keys cannot be relied on; refusing. Fix: chmod 700 it, or remove it",
                self.dir.display(),
                m.mode() & 0o777
            );
        }
        Ok(true)
    }

    /// The record of applied rotations and revoked keys. A missing directory is a fresh start; a directory without
    /// `state.json`, an unreadable or a corrupt one is a hard error (silently forgetting revocations would let a revoked
    /// key sign again), as is a directory other people can write to.
    pub fn state(&self) -> Result<State> {
        let mut st = if self.dir_safe()? {
            let f = self.dir.join("state.json");
            match fs::read_to_string(&f) {
                Ok(t) => serde_json::from_str::<State>(&t)
                    .map_err(|e| anyhow!("{} is corrupt ({e}); refusing to guess which keys are revoked. {}", f.display(), RECOVER.replace("<dir>", &self.dir.display().to_string())))?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!(
                    "{} is missing although {} exists: the record of revoked keys is gone; refusing. {}",
                    f.display(),
                    self.dir.display(),
                    RECOVER.replace("<dir>", &self.dir.display().to_string())
                ),
                Err(e) => return Err(e).with_context(|| format!("reading {}; refusing", f.display())),
            }
        } else {
            State::default()
        };
        for r in &self.mem_revoked {
            if !st.revoked.iter().any(|x| x.id == r.id) {
                st.revoked.push(r.clone());
            }
        }
        st.serial = st.serial.max(self.mem_serial);
        Ok(st)
    }

    pub fn is_revoked(&self, id: &str) -> Result<bool> {
        Ok(self.state()?.revoked.iter().any(|r| r.id.eq_ignore_ascii_case(id)))
    }

    fn meta_of(&self, id: &str) -> Option<Meta> {
        fs::read_to_string(self.dir.join(format!("{id}.json"))).ok().and_then(|t| serde_json::from_str::<Meta>(&t).ok())
    }

    /// Every key we know of, revoked or expired ones included (flags are computed by the callers).
    pub fn all(&self) -> Vec<KeyInfo> {
        let mut out = Vec::new();
        if let Some(k) = self.compiled_key() {
            out.push(k);
        }
        match self.dir_safe() {
            Ok(true) => {
                let mut names: Vec<_> = fs::read_dir(&self.dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
                names.sort();
                for p in names.into_iter().filter(|p| p.extension().and_then(|e| e.to_str()) == Some("pub")) {
                    let Ok(text) = fs::read_to_string(&p) else { continue };
                    let Ok(mut k) = parse_pub(&text, Source::Manual) else { continue };
                    if let Some(m) = fs::read_to_string(p.with_extension("json")).ok().and_then(|t| serde_json::from_str::<Meta>(&t).ok()) {
                        k.expires = m.expires;
                        if m.source == "rotation" {
                            k.source = Source::Rotation;
                        }
                    }
                    if !out.iter().any(|o| o.id == k.id) {
                        out.push(k);
                    }
                }
            }
            Ok(false) => {}
            Err(e) => eprintln!("minerva: warning: {e:#}"),
        }
        for k in &self.mem_keys {
            if !out.iter().any(|o| o.id == k.id) {
                out.push(k.clone());
            }
        }
        out
    }

    /// The keys that may sign right now.
    pub fn trusted(&self, today: &str) -> Result<Vec<KeyInfo>> {
        let st = self.state()?;
        Ok(self
            .all()
            .into_iter()
            .filter(|k| !st.revoked.iter().any(|r| r.id.eq_ignore_ascii_case(&k.id)))
            .filter(|k| k.expires.as_deref().map(|e| e >= today).unwrap_or(true))
            .collect())
    }

    /// Verify `data` against `sig` with the trusted key the signature names. Returns that key's id.
    pub fn verify(&self, data: &[u8], sig: &str, today: &str) -> Result<String> {
        let id = sig_key_id(sig)?;
        let trusted = self.trusted(today)?;
        let Some(k) = trusted.iter().find(|k| k.id.eq_ignore_ascii_case(&id)) else {
            if self.is_revoked(&id)? {
                bail!("signature check FAILED: the signing key {id} has been revoked; refusing");
            }
            if self.all().iter().any(|k| k.id.eq_ignore_ascii_case(&id)) {
                bail!("signature check FAILED: the signing key {id} has expired; refusing");
            }
            bail!("signature check FAILED: the signing key {id} is not one this program trusts; refusing");
        };
        let pk = minisign_verify::PublicKey::from_base64(&k.b64).map_err(|e| anyhow!("a trusted key is unusable ({e})"))?;
        let sg = minisign_verify::Signature::decode(sig).map_err(|e| anyhow!("the signature file is malformed ({e}); refusing"))?;
        pk.verify(data, &sg, false).map_err(|e| anyhow!("signature check FAILED ({e}); refusing"))?;
        Ok(id)
    }

    fn ensure_dir(&self) -> Result<()> {
        if !self.dir.exists() {
            if let Some(p) = self.dir.parent() {
                fs::create_dir_all(p).with_context(|| format!("creating {}", p.display()))?;
            }
            fs::DirBuilder::new().mode(0o700).create(&self.dir).with_context(|| format!("creating {}", self.dir.display()))?;
            // A directory always has its record, so a missing one later means something deleted it.
            self.save_state(&State::default())?;
        }
        if !self.dir_safe()? {
            bail!("{} is missing", self.dir.display());
        }
        Ok(())
    }

    fn write_private(&self, name: &str, bytes: &[u8]) -> Result<()> {
        let tmp = self.dir.join(format!(".{name}.tmp"));
        let _ = fs::remove_file(&tmp);
        let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp).with_context(|| format!("writing {}", tmp.display()))?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&tmp, self.dir.join(name)).with_context(|| format!("replacing {name}"))
    }

    fn save_state(&self, st: &State) -> Result<()> {
        self.write_private("state.json", serde_json::to_string_pretty(st)?.as_bytes())
    }

    /// Refuse a key whose id is already known with different key bytes (a forged .pub that reuses a real key's id).
    fn check_id_collision(&self, k: &KeyInfo) -> Result<()> {
        for o in self.all() {
            if o.id.eq_ignore_ascii_case(&k.id) && o.b64 != k.b64 {
                bail!("key id {} is already in use by a DIFFERENT key (fingerprint {}); refusing this one (fingerprint {})", k.id, o.fingerprint, k.fingerprint);
            }
        }
        Ok(())
    }

    /// Add a key by hand (the caller has already had the person confirm the fingerprint).
    pub fn add_manual(&self, text: &str) -> Result<KeyInfo> {
        let k = parse_pub(text, Source::Manual)?;
        if self.is_revoked(&k.id)? {
            bail!("key {} was revoked and is never accepted again", k.id);
        }
        self.check_id_collision(&k)?;
        if self.compiled_key().map(|c| c.id == k.id).unwrap_or(false) {
            bail!("key {} is already the compiled-in key", k.id);
        }
        self.ensure_dir()?;
        let body = format!("untrusted comment: {}\n{}\n", if k.comment.is_empty() { "minisign public key (added by hand)" } else { &k.comment }, k.b64);
        self.write_private(&format!("{}.pub", k.id), body.as_bytes())?;
        self.write_private(&format!("{}.json", k.id), serde_json::to_string(&Meta { expires: None, source: "manual".into(), by: Some("manual".into()) })?.as_bytes())?;
        Ok(k)
    }

    /// Remove an added key. The compiled-in key cannot be removed here (a rotation revokes it).
    pub fn remove(&self, id: &str) -> Result<KeyInfo> {
        let all = self.all();
        let k = all.iter().find(|k| k.id.eq_ignore_ascii_case(id)).ok_or_else(|| anyhow!("no key {id} (try: minerva keys list)"))?.clone();
        if k.source == Source::Compiled {
            bail!("key {} is compiled into this program; it cannot be removed by hand (a signed rotation can revoke it)", k.id);
        }
        fs::remove_file(self.dir.join(format!("{}.pub", k.id)))?;
        let _ = fs::remove_file(self.dir.join(format!("{}.json", k.id)));
        Ok(k)
    }

    /// Ids of keys added (by a rotation) by `id`, directly or through other keys it added.
    fn descendants(&self, id: &str) -> Vec<String> {
        let mut by: Vec<(String, String)> = Vec::new();
        if let Ok(rd) = fs::read_dir(&self.dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) != Some("json") {
                    continue;
                }
                let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else { continue };
                if !valid_id(stem) {
                    continue;
                }
                if let Some(parent) = self.meta_of(stem).and_then(|m| m.by) {
                    by.push((stem.to_ascii_uppercase(), parent.to_ascii_uppercase()));
                }
            }
        }
        let mut out: Vec<String> = Vec::new();
        let mut frontier = vec![id.to_ascii_uppercase()];
        while let Some(cur) = frontier.pop() {
            for (child, parent) in &by {
                if *parent == cur && !out.contains(child) && !child.eq_ignore_ascii_case(id) {
                    out.push(child.clone());
                    frontier.push(child.clone());
                }
            }
        }
        out
    }
}

// geteuid without a libc dependency: std has no stable accessor, and the metadata of a file we just made tells the same.
unsafe fn libc_geteuid() -> u32 {
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    unsafe { geteuid() }
}

#[derive(Debug, Deserialize)]
struct Rotation {
    schema: u32,
    serial: u64,
    expires: String,
    #[serde(default)]
    add: Vec<AddEntry>,
    #[serde(default)]
    revoke: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct AddEntry {
    /// The base64 line of a minisign public key file.
    key: String,
    #[serde(default)]
    expires: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    comment: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Rotated {
    /// Applied: (ids added, ids revoked).
    Applied(Vec<String>, Vec<String>),
    /// Its serial is not above the one already applied: nothing changed.
    AlreadyApplied,
}

/// A verified rotation, not yet written anywhere. `commit` writes it; `Trust::with_plan` applies it in memory only.
#[derive(Debug)]
pub struct Plan {
    serial: u64,
    signer: String,
    today: String,
    adds: Vec<(KeyInfo, Option<String>)>,
    /// Ids to revoke, including everything the revoked keys added earlier.
    revokes: Vec<String>,
}

impl Plan {
    pub fn serial(&self) -> u64 {
        self.serial
    }
}

pub enum Planned {
    AlreadyApplied,
    Apply(Plan),
}

/// Check a `keys.json`. The order matters:
/// 1. its serial against the record: at or below what is applied it is a re-fetch of an old file, answered with
///    `AlreadyApplied` BEFORE looking at its signature or expiry (an applied revocation signed by the now-revoked key,
///    or an applied file past its date, must not block every later update);
/// 2. a serial more than `MAX_SERIAL_JUMP` above the record is refused (a leaked key must not be able to use up the range);
/// 3. only then the signature (a key trusted right now), the schema and the expiry.
pub fn plan_rotation(trust: &Trust, json: &[u8], sig: Option<&str>, today: &str) -> Result<Planned> {
    let rot: Rotation = serde_json::from_slice(json).context("keys.json is not valid")?;
    let st = trust.state()?;
    if rot.serial <= st.serial {
        return Ok(Planned::AlreadyApplied);
    }
    let sig = sig.ok_or_else(|| anyhow!("keys.json has no keys.json.minisig; refusing"))?;
    let signer = trust.verify(json, sig, today).context("keys.json")?;
    if rot.serial > st.serial.saturating_add(MAX_SERIAL_JUMP) {
        bail!("keys.json has serial {} but the last applied is {}; a jump of more than {MAX_SERIAL_JUMP} is refused", rot.serial, st.serial);
    }
    if rot.schema != 1 {
        bail!("keys.json has schema {}, this program understands 1", rot.schema);
    }
    if !valid_date(&rot.expires) {
        bail!("keys.json has no valid expiry date (YYYY-MM-DD)");
    }
    if rot.expires.as_str() < today {
        bail!("keys.json expired on {}; refusing a stale rotation", rot.expires);
    }
    // Validate everything before changing anything.
    let mut adds: Vec<(KeyInfo, Option<String>)> = Vec::new();
    for a in &rot.add {
        let k = parse_pub(&a.key, Source::Rotation)?;
        if let Some(e) = &a.expires {
            if !valid_date(e) {
                bail!("keys.json: bad expiry {e:?} for key {}", k.id);
            }
        }
        trust.check_id_collision(&k)?;
        if adds.iter().any(|(o, _)| o.id == k.id && o.b64 != k.b64) {
            bail!("keys.json adds two different keys with the id {}", k.id);
        }
        adds.push((k, a.expires.clone()));
    }
    let mut revokes: Vec<String> = Vec::new();
    for r in &rot.revoke {
        if !valid_id(r) {
            bail!("keys.json: {r:?} is not a key id");
        }
        let id = r.to_ascii_uppercase();
        if !revokes.contains(&id) {
            revokes.push(id.clone());
        }
        // what the revoked key added before this file goes with it (keys added by this very file are the signer's
        // own succession, e.g. "A adds B and revokes A", and stay)
        for d in trust.descendants(&id) {
            if !revokes.contains(&d) {
                revokes.push(d);
            }
        }
    }
    Ok(Planned::Apply(Plan { serial: rot.serial, signer, today: today.to_string(), adds, revokes }))
}

impl Trust {
    /// This trust plus `plan`, in memory only: what a check or the daily notice judges a release by without writing
    /// anything.
    pub fn with_plan(&self, plan: &Plan) -> Trust {
        let mut t = self.clone();
        for id in &plan.revokes {
            t.mem_revoked.push(Revoked { id: id.clone(), on: plan.today.clone(), by: plan.signer.clone() });
        }
        for (k, exp) in &plan.adds {
            if plan.revokes.contains(&k.id) || self.compiled_key().map(|c| c.id == k.id).unwrap_or(false) {
                continue;
            }
            let mut k = k.clone();
            k.expires = exp.clone();
            t.mem_keys.push(k);
        }
        t.mem_serial = plan.serial;
        t
    }
}

/// Write a planned rotation to the keys dir and the record. Returns (ids added, ids revoked).
pub fn commit_rotation(trust: &Trust, plan: &Plan) -> Result<Rotated> {
    // re-read from disk: a concurrent run may have applied it
    let mut st = trust.state()?;
    if plan.serial <= st.serial {
        return Ok(Rotated::AlreadyApplied);
    }
    trust.ensure_dir()?;
    let mut added = Vec::new();
    let mut revoked = Vec::new();
    for id in &plan.revokes {
        if !st.revoked.iter().any(|r| &r.id == id) {
            st.revoked.push(Revoked { id: id.clone(), on: plan.today.clone(), by: plan.signer.clone() });
            revoked.push(id.clone());
        }
        let _ = fs::remove_file(trust.dir.join(format!("{id}.pub")));
        let _ = fs::remove_file(trust.dir.join(format!("{id}.json")));
    }
    for (k, exp) in &plan.adds {
        if st.revoked.iter().any(|r| r.id == k.id) || trust.compiled_key().map(|c| c.id == k.id).unwrap_or(false) {
            continue; // revoked wins; the compiled-in key needs no file
        }
        let body = format!("untrusted comment: minisign public key (rotation {})\n{}\n", plan.serial, k.b64);
        trust.write_private(&format!("{}.pub", k.id), body.as_bytes())?;
        let meta = Meta { expires: exp.clone(), source: "rotation".into(), by: Some(plan.signer.clone()) };
        trust.write_private(&format!("{}.json", k.id), serde_json::to_string(&meta)?.as_bytes())?;
        added.push(k.id.clone());
    }
    st.serial = plan.serial;
    trust.save_state(&st)?;
    Ok(Rotated::Applied(added, revoked))
}

/// Verify and apply a signed `keys.json` (plan, then write). See `plan_rotation` for the rules.
pub fn apply_rotation(trust: &Trust, json: &[u8], sig: &str, today: &str) -> Result<Rotated> {
    match plan_rotation(trust, json, Some(sig), today)? {
        Planned::AlreadyApplied => Ok(Rotated::AlreadyApplied),
        Planned::Apply(p) => commit_rotation(trust, &p),
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use crate::verify::testkeys::{keypair, sign};

    pub fn trust_with(compiled: &str) -> (tempfile::TempDir, Trust) {
        let t = tempfile::tempdir().unwrap();
        let tr = Trust::new(compiled, t.path().join("keys"));
        (t, tr)
    }

    pub fn id_of(pubtext: &str) -> String {
        parse_pub(pubtext, Source::Manual).unwrap().id
    }

    pub fn rotation(serial: u64, expires: &str, add: &[&str], revoke: &[&str]) -> String {
        let adds: Vec<String> = add.iter().map(|p| format!(r#"{{"key":"{}"}}"#, parse_pub(p, Source::Manual).unwrap().b64)).collect();
        let revs: Vec<String> = revoke.iter().map(|r| format!("\"{r}\"")).collect();
        format!(r#"{{"schema":1,"serial":{serial},"expires":"{expires}","add":[{}],"revoke":[{}]}}"#, adds.join(","), revs.join(","))
    }

    #[test]
    fn ids_dates_and_parsing() {
        let (pk, sk) = keypair();
        let sig = sign(&sk, b"x");
        assert_eq!(id_of(&pk), sig_key_id(&sig).unwrap());
        assert_eq!(date_of(0), "1970-01-01");
        assert_eq!(date_of(1_791_269_513), "2026-10-06");
        assert!(parse_pub("garbage", Source::Manual).is_err());
        assert!(parse_pub("PLACEHOLDER\n", Source::Manual).is_err());
    }

    #[test]
    fn rotation_good_adds_and_revokes_and_is_idempotent() {
        let (old_pk, old_sk) = keypair();
        let (new_pk, new_sk) = keypair();
        let (_t, tr) = trust_with(&old_pk);
        let json = rotation(1, "2030-01-01", &[&new_pk], &[&id_of(&old_pk)]);
        let sig = sign(&old_sk, json.as_bytes());
        let r = apply_rotation(&tr, json.as_bytes(), &sig, "2026-10-06").unwrap();
        assert_eq!(r, Rotated::Applied(vec![id_of(&new_pk)], vec![id_of(&old_pk)]));
        // the new key signs; the old (compiled-in, now revoked) one does not
        tr.verify(b"hello", &sign(&new_sk, b"hello"), "2026-10-06").unwrap();
        let e = tr.verify(b"hello", &sign(&old_sk, b"hello"), "2026-10-06").unwrap_err();
        assert!(format!("{e:#}").contains("revoked"), "{e:#}");
        // a replay is a no-op
        assert_eq!(apply_rotation(&tr, json.as_bytes(), &sign(&new_sk, json.as_bytes()), "2026-10-06").unwrap(), Rotated::AlreadyApplied);
        // modes
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(&tr.dir).unwrap().permissions().mode() & 0o777, 0o700);
        let f = tr.dir.join(format!("{}.pub", id_of(&new_pk)));
        assert_eq!(fs::metadata(f).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn rotation_signed_by_an_unknown_or_revoked_key_is_refused() {
        let (pk, sk) = keypair();
        let (evil_pk, evil_sk) = keypair();
        let (_t, tr) = trust_with(&pk);
        let json = rotation(1, "2030-01-01", &[&evil_pk], &[]);
        let e = apply_rotation(&tr, json.as_bytes(), &sign(&evil_sk, json.as_bytes()), "2026-10-06").unwrap_err();
        assert!(format!("{e:#}").contains("not one this program trusts"), "{e:#}");
        assert!(!tr.dir.exists(), "nothing was written");
        // revoke the compiled key by a good rotation, then a rotation signed by it is refused
        let good = rotation(1, "2030-01-01", &[], &[&id_of(&pk)]);
        apply_rotation(&tr, good.as_bytes(), &sign(&sk, good.as_bytes()), "2026-10-06").unwrap();
        let later = rotation(2, "2030-01-01", &[&evil_pk], &[]);
        assert!(apply_rotation(&tr, later.as_bytes(), &sign(&sk, later.as_bytes()), "2026-10-06").is_err());
        // tampered
        let (_t2, tr2) = trust_with(&pk);
        assert!(apply_rotation(&tr2, b"{}", &sign(&sk, good.as_bytes()), "2026-10-06").is_err());
    }

    #[test]
    fn expired_rotation_and_expired_key() {
        let (pk, sk) = keypair();
        let (add_pk, add_sk) = keypair();
        let (_t, tr) = trust_with(&pk);
        let stale = rotation(1, "2020-01-01", &[], &[]);
        let e = apply_rotation(&tr, stale.as_bytes(), &sign(&sk, stale.as_bytes()), "2026-10-06").unwrap_err();
        assert!(format!("{e:#}").contains("expired"), "{e:#}");
        // a key added with an expiry stops working after it
        let json = format!(r#"{{"schema":1,"serial":1,"expires":"2030-01-01","add":[{{"key":"{}","expires":"2026-12-31"}}]}}"#, parse_pub(&add_pk, Source::Manual).unwrap().b64);
        apply_rotation(&tr, json.as_bytes(), &sign(&sk, json.as_bytes()), "2026-10-06").unwrap();
        tr.verify(b"a", &sign(&add_sk, b"a"), "2026-12-31").unwrap();
        let e = tr.verify(b"a", &sign(&add_sk, b"a"), "2027-01-01").unwrap_err();
        assert!(format!("{e:#}").contains("expired"));
    }

    #[test]
    fn a_revoked_key_cannot_come_back() {
        let (pk, sk) = keypair();
        let (k2, _) = keypair();
        let (_t, tr) = trust_with(&pk);
        let r = rotation(1, "2030-01-01", &[&k2], &[&id_of(&k2)]);
        apply_rotation(&tr, r.as_bytes(), &sign(&sk, r.as_bytes()), "2026-10-06").unwrap();
        assert!(tr.add_manual(&k2).unwrap_err().to_string().contains("revoked"));
        let again = rotation(2, "2030-01-01", &[&k2], &[]);
        apply_rotation(&tr, again.as_bytes(), &sign(&sk, again.as_bytes()), "2026-10-06").unwrap();
        assert!(tr.trusted("2026-10-06").unwrap().iter().all(|k| k.id != id_of(&k2)));
    }

    #[test]
    fn manual_add_list_remove() {
        let (pk, _) = keypair();
        let (extra, extra_sk) = keypair();
        let (_t, tr) = trust_with(&pk);
        let k = tr.add_manual(&extra).unwrap();
        assert_eq!(k.id, id_of(&extra));
        assert_eq!(tr.trusted("2026-10-06").unwrap().len(), 2);
        tr.verify(b"m", &sign(&extra_sk, b"m"), "2026-10-06").unwrap();
        assert!(tr.remove(&id_of(&pk)).is_err(), "the compiled-in key is not removable");
        tr.remove(&k.id).unwrap();
        assert_eq!(tr.trusted("2026-10-06").unwrap().len(), 1);
        assert!(tr.verify(b"m", &sign(&extra_sk, b"m"), "2026-10-06").is_err());
        assert!(tr.remove(&k.id).is_err());
    }

    #[test]
    fn an_open_keys_dir_is_refused_not_silently_ignored() {
        let (pk, _) = keypair();
        let (extra, extra_sk) = keypair();
        let (_t, tr) = trust_with(&pk);
        tr.add_manual(&extra).unwrap();
        fs::set_permissions(&tr.dir, fs::Permissions::from_mode(0o755)).unwrap();
        // ignoring it would forget the revocations: every use of the record is an error with a sentence
        let e = format!("{:#}", tr.trusted("2026-10-06").unwrap_err());
        assert!(e.contains("not a private directory") && e.contains("chmod 700"), "{e}");
        assert!(tr.verify(b"m", &sign(&extra_sk, b"m"), "2026-10-06").is_err());
    }

    #[test]
    fn state_json_missing_or_corrupt_is_a_hard_error_with_a_way_out() {
        // L5
        let (pk, sk) = keypair();
        let (extra, _) = keypair();
        let (_t, tr) = trust_with(&pk);
        // no directory yet: a fresh start
        assert_eq!(tr.state().unwrap().serial, 0);
        tr.add_manual(&extra).unwrap();
        assert!(tr.dir.join("state.json").is_file(), "a keys dir always has its record");
        // dir exists, state.json gone: refuse and say how to recover
        fs::remove_file(tr.dir.join("state.json")).unwrap();
        let e = format!("{:#}", tr.state().unwrap_err());
        assert!(e.contains("is missing although") && e.contains("To recover"), "{e}");
        let json = rotation(1, "2030-01-01", &[], &[]);
        assert!(apply_rotation(&tr, json.as_bytes(), &sign(&sk, json.as_bytes()), "2026-10-06").is_err());
        assert!(tr.verify(b"m", &sign(&sk, b"m"), "2026-10-06").is_err(), "no verification without the record");
        // corrupt
        fs::write(tr.dir.join("state.json"), "{ not json").unwrap();
        let e = format!("{:#}", tr.state().unwrap_err());
        assert!(e.contains("is corrupt") && e.contains("To recover"), "{e}");
    }

    #[test]
    fn a_huge_serial_is_refused_and_the_range_is_not_used_up() {
        // H2
        let (pk, sk) = keypair();
        let (evil, _) = keypair();
        let (_t, tr) = trust_with(&pk);
        let huge = rotation(u64::MAX, "2030-01-01", &[&evil], &[]);
        let e = format!("{:#}", apply_rotation(&tr, huge.as_bytes(), &sign(&sk, huge.as_bytes()), "2026-10-06").unwrap_err());
        assert!(e.contains("jump"), "{e}");
        let far = rotation(MAX_SERIAL_JUMP + 1, "2030-01-01", &[], &[]);
        assert!(apply_rotation(&tr, far.as_bytes(), &sign(&sk, far.as_bytes()), "2026-10-06").is_err());
        assert!(!tr.dir.exists(), "nothing was written");
        // the edge is allowed, and the next real rotation still applies
        let edge = rotation(MAX_SERIAL_JUMP, "2030-01-01", &[], &[]);
        apply_rotation(&tr, edge.as_bytes(), &sign(&sk, edge.as_bytes()), "2026-10-06").unwrap();
        let next = rotation(MAX_SERIAL_JUMP + 1, "2030-01-01", &[], &[]);
        assert!(matches!(apply_rotation(&tr, next.as_bytes(), &sign(&sk, next.as_bytes()), "2026-10-06").unwrap(), Rotated::Applied(..)));
    }

    #[test]
    fn revoking_a_key_revokes_everything_it_added_transitively() {
        // H2: A (leaked) adds N1, N1 adds N2 (all signed in turn); revoking A drops both
        let (a_pk, a_sk) = keypair();
        let (n1_pk, n1_sk) = keypair();
        let (n2_pk, n2_sk) = keypair();
        let (ok_pk, ok_sk) = keypair();
        let (_t, tr) = trust_with(&a_pk);
        let r1 = rotation(1, "2030-01-01", &[&n1_pk, &ok_pk], &[]);
        apply_rotation(&tr, r1.as_bytes(), &sign(&a_sk, r1.as_bytes()), "2026-10-06").unwrap();
        let r2 = rotation(2, "2030-01-01", &[&n2_pk], &[]);
        apply_rotation(&tr, r2.as_bytes(), &sign(&n1_sk, r2.as_bytes()), "2026-10-06").unwrap();
        // the meta records who added each key
        assert_eq!(tr.meta_of(&id_of(&n1_pk)).unwrap().by.unwrap(), id_of(&a_pk));
        assert_eq!(tr.meta_of(&id_of(&n2_pk)).unwrap().by.unwrap(), id_of(&n1_pk));
        // ok_pk was ALSO added by A; the operator re-adds trust in it explicitly by a second file after the revocation
        let r3 = rotation(3, "2030-01-01", &[], &[&id_of(&a_pk)]);
        let sig3 = sign(&n1_sk, r3.as_bytes()); // signed by a key that is still trusted at that moment
        match apply_rotation(&tr, r3.as_bytes(), &sig3, "2026-10-06").unwrap() {
            Rotated::Applied(_, revoked) => {
                for id in [&a_pk, &n1_pk, &n2_pk, &ok_pk] {
                    assert!(revoked.contains(&id_of(id)), "{revoked:?}");
                }
            }
            other => panic!("{other:?}"),
        }
        let trusted = tr.trusted("2026-10-06").unwrap();
        assert!(trusted.is_empty(), "{trusted:?}");
        assert!(tr.verify(b"x", &sign(&n2_sk, b"x"), "2026-10-06").is_err());
        assert!(tr.verify(b"x", &sign(&ok_sk, b"x"), "2026-10-06").is_err());
        let e = format!("{:#}", tr.verify(b"x", &sign(&n1_sk, b"x"), "2026-10-06").unwrap_err());
        assert!(e.contains("revoked"), "{e}");
    }

    #[test]
    fn a_key_added_by_the_same_file_that_revokes_the_signer_stays() {
        // the normal rotation: "A adds B and revokes A" must leave B trusted
        let (a_pk, a_sk) = keypair();
        let (b_pk, b_sk) = keypair();
        let (_t, tr) = trust_with(&a_pk);
        let r = rotation(1, "2030-01-01", &[&b_pk], &[&id_of(&a_pk)]);
        apply_rotation(&tr, r.as_bytes(), &sign(&a_sk, r.as_bytes()), "2026-10-06").unwrap();
        tr.verify(b"x", &sign(&b_sk, b"x"), "2026-10-06").unwrap();
        // ... and if A turns out to have been leaked, a later file naming A drops what A added
        let r2 = rotation(2, "2030-01-01", &[], &[&id_of(&a_pk)]);
        apply_rotation(&tr, r2.as_bytes(), &sign(&b_sk, r2.as_bytes()), "2026-10-06").unwrap();
        assert!(tr.verify(b"x", &sign(&b_sk, b"x"), "2026-10-06").is_err());
    }

    #[test]
    fn a_forged_pub_with_the_same_id_and_different_bytes_is_refused() {
        // M6
        let (pk, _) = keypair();
        let (real, _) = keypair();
        let (other, _) = keypair();
        let (_t, tr) = trust_with(&pk);
        tr.add_manual(&real).unwrap();
        let forged = forge_same_id(&real, &other);
        let fi = parse_pub(&forged, Source::Manual).unwrap();
        let ri = parse_pub(&real, Source::Manual).unwrap();
        assert_eq!(fi.id, ri.id);
        assert_ne!(fi.fingerprint, ri.fingerprint, "the fingerprint is what tells them apart");
        let e = format!("{:#}", tr.add_manual(&forged).unwrap_err());
        assert!(e.contains("DIFFERENT key"), "{e}");
        // also against the compiled-in key, and inside a rotation
        let e = format!("{:#}", tr.add_manual(&forge_same_id(&pk, &other)).unwrap_err());
        assert!(e.contains("DIFFERENT key"), "{e}");
    }

    #[test]
    fn fingerprint_is_sixteen_hex_digits_in_groups() {
        let (pk, _) = keypair();
        let f = parse_pub(&pk, Source::Manual).unwrap().fingerprint;
        let groups: Vec<&str> = f.split(' ').collect();
        assert_eq!(groups.len(), 4);
        assert!(groups.iter().all(|g| g.len() == 4 && g.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())), "{f}");
    }

    /// `real`'s id with `other`'s key bytes: what an attacker hands over as "the same key".
    pub fn forge_same_id(real: &str, other: &str) -> String {
        let r = B64.decode(parse_pub(real, Source::Manual).unwrap().b64).unwrap();
        let o = B64.decode(parse_pub(other, Source::Manual).unwrap().b64).unwrap();
        let mut f = r[..10].to_vec();
        f.extend_from_slice(&o[10..]);
        format!("untrusted comment: forged\n{}\n", B64.encode(f))
    }
}
