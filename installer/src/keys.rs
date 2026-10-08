//! Who may sign a release. The trusted set is:
//!
//! ```text
//!   the key compiled into this binary                      (build.rs; installer/minisign.pub)
//! + <keys dir>/*.pub                                       (added by hand, or by a signed rotation)
//! - every id, and every key with a revoked fingerprint, in <keys dir>/state.json "revoked" (a revoked key is never accepted again, compiled-in or not)
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

/// A fingerprint as state.json keeps it and as every comparison reads it: 16 lower-case hex digits, spaces and case ignored.
fn fp_hex(fingerprint: &str) -> String {
    fingerprint.chars().filter(|c| !c.is_whitespace()).collect::<String>().to_ascii_lowercase()
}

fn valid_fp(s: &str) -> bool {
    s.len() == 16 && s.bytes().all(|c| c.is_ascii_hexdigit())
}

/// Are these the same key bytes, whatever their ids? The fingerprint is the sha-256 of the 32 key bytes, so a fresh id does not change it.
fn same_key(a: &KeyInfo, b: &KeyInfo) -> bool {
    fp_hex(&a.fingerprint) == fp_hex(&b.fingerprint)
}

/// Write `dir/name` through a temp file in `dir`: created 0600, fsynced, renamed over the name, then `dir` fsynced.
fn write_private_in(dir: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    let tmp = dir.join(format!(".{name}.tmp"));
    let _ = fs::remove_file(&tmp);
    let mut f = fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&tmp).with_context(|| format!("writing {}", tmp.display()))?;
    f.write_all(bytes)?;
    f.sync_all()?;
    fs::rename(&tmp, dir.join(name)).with_context(|| format!("replacing {name}"))?;
    sync_dir(dir)
}

/// fsync a directory, so a rename made in it is on disk before anything relies on it.
fn sync_dir(dir: &Path) -> Result<()> {
    fs::File::open(dir).and_then(|d| d.sync_all()).with_context(|| format!("syncing {}", dir.display()))
}

/// The sentence for a key the record revokes: by its own id, or by its bytes under another id (the one the record names).
fn revoked_sentence(k: &KeyInfo, r: &Revoked) -> String {
    if r.id.eq_ignore_ascii_case(&k.id) {
        format!("key {} was revoked and is never accepted again", k.id)
    } else {
        format!("key {} has the key bytes (fingerprint {}) of the revoked key {}; a revoked key is never accepted again, whatever its id", k.id, k.fingerprint, r.id)
    }
}

/// Why a change to the trusted keys was refused, as a kind a program can branch on (`minerva keys add|remove --json`). The sentence a
/// person reads is the error's message; the kind does not change when its wording does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The fingerprint that was typed or passed is not the key's.
    Mismatch,
    /// The key was revoked (its id, or its key bytes under any id), and a revoked key is never accepted again.
    Revoked,
    /// The id is already used by a DIFFERENT key, or this key is already in the list (under its id, or its bytes under another id).
    Collision,
    /// The compiled-in key: already trusted, and it cannot be removed by hand.
    Compiled,
    /// The keys directory or its record cannot be relied on (not a private directory of yours; `state.json` missing, unreadable or corrupt).
    Unsafe,
    /// No key has that id.
    NotFound,
}

impl Refusal {
    pub fn kind(self) -> &'static str {
        match self {
            Refusal::Mismatch => "mismatch",
            Refusal::Revoked => "revoked",
            Refusal::Collision => "collision",
            Refusal::Compiled => "compiled",
            Refusal::Unsafe => "unsafe",
            Refusal::NotFound => "not-found",
        }
    }
}

/// The error behind a [`Refusal`]: it prints as its sentence, and survives any context added on top (`refusal` finds it again).
#[derive(Debug)]
pub struct Refused {
    pub why: Refusal,
    pub sentence: String,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.sentence)
    }
}

impl std::error::Error for Refused {}

pub fn refuse(why: Refusal, sentence: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(Refused { why, sentence: sentence.into() })
}

/// The refusal an error carries, if it is one.
pub fn refusal(e: &anyhow::Error) -> Option<&Refused> {
    e.downcast_ref::<Refused>()
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
    /// The key's fingerprint (16 lower-case hex digits, no spaces) when its bytes were known at the revocation. It is what refuses the
    /// same bytes under a fresh id. A record written before this field existed has none, and is matched by id only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fp: Option<String>,
}

impl State {
    /// The revocation that covers `k`: one with its id, or one with its key bytes (its fingerprint), whatever id it was made under.
    pub fn revocation_of(&self, k: &KeyInfo) -> Option<&Revoked> {
        let fp = fp_hex(&k.fingerprint);
        self.revoked.iter().find(|r| r.id.eq_ignore_ascii_case(&k.id) || r.fp.as_deref() == Some(fp.as_str()))
    }

    /// A revoked fingerprint must be 16 hex digits. Anything else in the record is refused, not guessed at.
    fn check_fingerprints(&mut self, dir: &Path) -> Result<()> {
        for r in &mut self.revoked {
            if let Some(fp) = r.fp.take() {
                let fp = fp_hex(&fp);
                if !valid_fp(&fp) {
                    return Err(refuse(
                        Refusal::Unsafe,
                        format!(
                            "{} has a revoked fingerprint for {} that is not 16 hex digits; refusing to guess which keys are revoked. {}",
                            dir.join("state.json").display(),
                            r.id,
                            RECOVER.replace("<dir>", &dir.display().to_string())
                        ),
                    ));
                }
                r.fp = Some(fp);
            }
        }
        Ok(())
    }
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

#[cfg(test)]
thread_local! {
    /// Test-only fault point: with it set, `ensure_dir` fails after the record is written and before the temp dir is renamed into place.
    static FAIL_BEFORE_RENAME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

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

    /// Is `k` the compiled-in key, by its id or by its key bytes?
    fn is_compiled(&self, k: &KeyInfo) -> bool {
        self.compiled_key().is_some_and(|c| c.id.eq_ignore_ascii_case(&k.id) || same_key(&c, k))
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
            return Err(refuse(
                Refusal::Unsafe,
                format!(
                    "{} is not a private directory of yours (owner, or mode {:o}), so its record of trusted and revoked keys cannot be relied on; refusing. Fix: chmod 700 it, or remove it",
                    self.dir.display(),
                    m.mode() & 0o777
                ),
            ));
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
                Ok(t) => serde_json::from_str::<State>(&t).map_err(|e| {
                    refuse(
                        Refusal::Unsafe,
                        format!("{} is corrupt ({e}); refusing to guess which keys are revoked. {}", f.display(), RECOVER.replace("<dir>", &self.dir.display().to_string())),
                    )
                })?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Err(refuse(
                        Refusal::Unsafe,
                        format!(
                            "{} is missing although {} exists: the record of revoked keys is gone; refusing. {}",
                            f.display(),
                            self.dir.display(),
                            RECOVER.replace("<dir>", &self.dir.display().to_string())
                        ),
                    ));
                }
                Err(e) => return Err(refuse(Refusal::Unsafe, format!("reading {}; refusing: {e}", f.display()))),
            }
        } else {
            State::default()
        };
        st.check_fingerprints(&self.dir)?;
        for r in &self.mem_revoked {
            match st.revoked.iter_mut().find(|x| x.id.eq_ignore_ascii_case(&r.id)) {
                // a preview that knows the fingerprint keeps it, even where the record has the id without one
                Some(x) => {
                    if x.fp.is_none() {
                        x.fp = r.fp.clone();
                    }
                }
                None => st.revoked.push(r.clone()),
            }
        }
        st.serial = st.serial.max(self.mem_serial);
        Ok(st)
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
            .filter(|k| st.revocation_of(k).is_none())
            .filter(|k| k.expires.as_deref().map(|e| e >= today).unwrap_or(true))
            .collect())
    }

    /// Verify `data` against `sig` with the trusted key the signature names. Returns that key's id.
    pub fn verify(&self, data: &[u8], sig: &str, today: &str) -> Result<String> {
        let id = sig_key_id(sig)?;
        let trusted = self.trusted(today)?;
        let Some(k) = trusted.iter().find(|k| k.id.eq_ignore_ascii_case(&id)) else {
            let st = self.state()?;
            let known = self.all().into_iter().find(|k| k.id.eq_ignore_ascii_case(&id));
            if st.revoked.iter().any(|r| r.id.eq_ignore_ascii_case(&id)) || known.as_ref().is_some_and(|k| st.revocation_of(k).is_some()) {
                bail!("signature check FAILED: the signing key {id} has been revoked; refusing");
            }
            if known.is_some() {
                bail!("signature check FAILED: the signing key {id} has expired; refusing");
            }
            bail!("signature check FAILED: the signing key {id} is not one this program trusts; refusing");
        };
        let pk = minisign_verify::PublicKey::from_base64(&k.b64).map_err(|e| anyhow!("a trusted key is unusable ({e})"))?;
        let sg = minisign_verify::Signature::decode(sig).map_err(|e| anyhow!("the signature file is malformed ({e}); refusing"))?;
        pk.verify(data, &sg, false).map_err(|e| anyhow!("signature check FAILED ({e}); refusing"))?;
        Ok(id)
    }

    /// Make the keys dir all at once. It is built in a temp dir beside it with its record in it (fsynced), the temp dir is renamed into
    /// place, and the parent is fsynced. A crash leaves either no keys dir (the next run removes the leftover and starts again) or a
    /// complete one: a dir with no `state.json` is never a state a crash can leave, so it never reads as "no keys".
    fn ensure_dir(&self) -> Result<()> {
        if !self.dir.exists() {
            let parent = self.dir.parent().context("the keys dir has no parent directory")?;
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
            let name = self.dir.file_name().context("the keys dir has no name")?.to_string_lossy().into_owned();
            let tmp = parent.join(format!(".{name}.new"));
            match fs::symlink_metadata(&tmp) {
                Ok(m) if m.is_dir() => fs::remove_dir_all(&tmp).with_context(|| format!("removing a leftover {}", tmp.display()))?,
                Ok(_) => fs::remove_file(&tmp).with_context(|| format!("removing a leftover {}", tmp.display()))?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e).with_context(|| format!("reading {}", tmp.display())),
            }
            fs::DirBuilder::new().mode(0o700).create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
            // A directory always has its record, so a missing one later means something deleted it.
            write_private_in(&tmp, "state.json", serde_json::to_string_pretty(&State::default())?.as_bytes())?;
            #[cfg(test)]
            if FAIL_BEFORE_RENAME.with(|f| f.get()) {
                bail!("injected fault: the keys dir was not renamed into place");
            }
            fs::rename(&tmp, &self.dir).with_context(|| format!("creating {}", self.dir.display()))?;
            sync_dir(parent)?;
        }
        if !self.dir_safe()? {
            bail!("{} is missing", self.dir.display());
        }
        Ok(())
    }

    fn write_private(&self, name: &str, bytes: &[u8]) -> Result<()> {
        write_private_in(&self.dir, name, bytes)
    }

    fn save_state(&self, st: &State) -> Result<()> {
        self.write_private("state.json", serde_json::to_string_pretty(st)?.as_bytes())
    }

    /// Refuse a key whose id is already known with different key bytes (a forged .pub that reuses a real key's id).
    fn check_id_collision(&self, k: &KeyInfo) -> Result<()> {
        for o in self.all() {
            if o.id.eq_ignore_ascii_case(&k.id) && o.b64 != k.b64 {
                return Err(refuse(
                    Refusal::Collision,
                    format!("key id {} is already in use by a DIFFERENT key (fingerprint {}); refusing this one (fingerprint {})", k.id, o.fingerprint, k.fingerprint),
                ));
            }
        }
        Ok(())
    }

    /// Add a key by hand (the caller has already had the person confirm the fingerprint). Refused, with nothing written: a key whose id
    /// or key bytes (under any id) were revoked; the compiled-in key; an id that a different key uses; a key already in the list, under
    /// its id or under another id. An existing key's record (its source, its expiry, who added it) is never rewritten.
    pub fn add_manual(&self, text: &str) -> Result<KeyInfo> {
        let k = parse_pub(text, Source::Manual)?;
        // the record first: an unreadable one refuses before anything else is looked at
        let st = self.state()?;
        if let Some(r) = st.revocation_of(&k) {
            return Err(refuse(Refusal::Revoked, revoked_sentence(&k, r)));
        }
        self.check_id_collision(&k)?;
        if let Some(c) = self.compiled_key().filter(|c| c.id.eq_ignore_ascii_case(&k.id) || same_key(c, &k)) {
            if st.revoked.iter().any(|r| r.id.eq_ignore_ascii_case(&c.id)) {
                return Err(refuse(Refusal::Revoked, format!("key {} is the compiled-in key, which was revoked and is never accepted again", c.id)));
            }
            let sentence = if c.id.eq_ignore_ascii_case(&k.id) {
                format!("key {} is already the compiled-in key", k.id)
            } else {
                format!("key {} has the key bytes of the compiled-in key {}, which is already trusted", k.id, c.id)
            };
            return Err(refuse(Refusal::Compiled, sentence));
        }
        for o in self.all() {
            // check_id_collision has refused any other bytes under this id, so a key with this id is this very key
            if o.id.eq_ignore_ascii_case(&k.id) {
                return Err(refuse(Refusal::Collision, format!("key {} is already in the list ({}); nothing changed", k.id, o.source.label())));
            }
            if same_key(&o, &k) {
                return Err(refuse(Refusal::Collision, format!("these key bytes (fingerprint {}) are already in the list under id {}; nothing changed", k.fingerprint, o.id)));
            }
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
        let k = all.iter().find(|k| k.id.eq_ignore_ascii_case(id)).ok_or_else(|| refuse(Refusal::NotFound, format!("no key {id} (try: minerva keys list)")))?.clone();
        if k.source == Source::Compiled {
            return Err(refuse(
                Refusal::Compiled,
                format!("key {} is compiled into this program; it cannot be removed by hand (a signed rotation can revoke it)", k.id),
            ));
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
    /// Revoked keys with their fingerprints, for a key this machine may never have held: its bytes then refuse the same key under a
    /// fresh id. Additive: an installer built before this field ignores it (serde takes no unknown-field error here) and revokes by id
    /// only, as it always did, so a file that carries it still applies everywhere.
    #[serde(default)]
    revoke_fp: Vec<RevokeFp>,
}

#[derive(Debug, Deserialize)]
struct RevokeFp {
    id: String,
    fp: String,
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

/// A key a plan revokes: its id, and its fingerprint when its bytes were known (the record keeps both).
#[derive(Debug, Clone)]
struct Revoke {
    id: String,
    fp: Option<String>,
}

/// A verified rotation, not yet written anywhere. `commit` writes it; `Trust::with_plan` applies it in memory only.
#[derive(Debug)]
pub struct Plan {
    serial: u64,
    signer: String,
    today: String,
    adds: Vec<(KeyInfo, Option<String>)>,
    /// Keys to revoke, including everything the revoked keys added earlier.
    revokes: Vec<Revoke>,
}

impl Plan {
    pub fn serial(&self) -> u64 {
        self.serial
    }

    /// Does this plan revoke `k`, by its id or by its key bytes?
    fn revokes_key(&self, k: &KeyInfo) -> bool {
        let fp = fp_hex(&k.fingerprint);
        self.revokes.iter().any(|r| r.id.eq_ignore_ascii_case(&k.id) || r.fp.as_deref() == Some(fp.as_str()))
    }
}

/// Add a revocation; a second one for the same id only fills in a fingerprint the first did not have.
fn push_revoke(revokes: &mut Vec<Revoke>, id: &str, fp: Option<String>) {
    match revokes.iter_mut().find(|r| r.id == id) {
        Some(r) => {
            if r.fp.is_none() {
                r.fp = fp;
            }
        }
        None => revokes.push(Revoke { id: id.to_string(), fp }),
    }
}

/// Collect the ids a file revokes, each with the fingerprint the file gives for it (if any). One id twice is one entry; two different
/// fingerprints for one id refuse the file.
fn want_revoke(wanted: &mut Vec<(String, Option<String>)>, id: String, given: Option<String>) -> Result<()> {
    match wanted.iter_mut().find(|(x, _)| *x == id) {
        Some((_, held)) => match (held.clone(), given) {
            (Some(a), Some(b)) if a != b => bail!("keys.json gives two different fingerprints for key {id}; refusing the whole file, nothing changed"),
            (None, b) => *held = b,
            _ => {}
        },
        None => wanted.push((id, given)),
    }
    Ok(())
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
        if let Some(e) = &a.expires
            && !valid_date(e)
        {
            bail!("keys.json: bad expiry {e:?} for key {}", k.id);
        }
        // a revoked key (its id, or its bytes under any id) is never accepted again: the whole file is refused, so nothing is half applied
        if st.revocation_of(&k).is_some() {
            return Err(refuse(
                Refusal::Revoked,
                format!("keys.json adds key {} (fingerprint {}), which was revoked and is never accepted again; refusing the whole file, nothing changed", k.id, k.fingerprint),
            ));
        }
        trust.check_id_collision(&k)?;
        if adds.iter().any(|(o, _)| o.id == k.id && o.b64 != k.b64) {
            bail!("keys.json adds two different keys with the id {}", k.id);
        }
        adds.push((k, a.expires.clone()));
    }
    // The fingerprint of each key revoked: the one the file gives in `revoke_fp`; else this file's own adds (a key that is only in this
    // file); else the keys dir or the compiled-in key; else the fingerprint this machine's record already has for that id (a revocation
    // listed again, or a rotation a crash cut short after the record was saved). A key the file names with none of those is refused,
    // the whole file with it. A given fingerprint that is not the one this machine holds under that id names another key, so the whole
    // file is refused.
    let known: Vec<KeyInfo> = adds.iter().map(|(k, _)| k.clone()).chain(trust.all()).collect();
    let fp_of = |id: &str| known.iter().find(|k| k.id.eq_ignore_ascii_case(id)).map(|k| fp_hex(&k.fingerprint));
    let recorded = |id: &str| st.revoked.iter().find(|r| r.id.eq_ignore_ascii_case(id)).and_then(|r| r.fp.clone());
    let mut wanted: Vec<(String, Option<String>)> = Vec::new();
    for r in &rot.revoke {
        if !valid_id(r) {
            bail!("keys.json: {r:?} is not a key id");
        }
        want_revoke(&mut wanted, r.to_ascii_uppercase(), None)?;
    }
    for r in &rot.revoke_fp {
        if !valid_id(&r.id) {
            bail!("keys.json: {:?} is not a key id", r.id);
        }
        let fp = fp_hex(&r.fp);
        if !valid_fp(&fp) {
            bail!("keys.json: the fingerprint given for key {} is not 16 hex digits; refusing the whole file, nothing changed", r.id);
        }
        want_revoke(&mut wanted, r.id.to_ascii_uppercase(), Some(fp))?;
    }
    let mut revokes: Vec<Revoke> = Vec::new();
    for (id, given) in wanted {
        let fp = match (given, fp_of(&id)) {
            (Some(g), Some(h)) if g != h => {
                bail!("keys.json revokes key {id} with fingerprint {g}, but this machine holds key {id} with fingerprint {h}; refusing the whole file, nothing changed")
            }
            // Named, with no fingerprint from the file, no key here and none in the record: its bytes cannot be checked, so the same bytes
            // under a fresh id would pass. Refused whole. (Keys a revoked key added are not named here: their fingerprint if this machine
            // holds their bytes, else the id alone.)
            (None, None) => Some(recorded(&id).ok_or_else(|| anyhow!("keys.json revokes {id} without its fingerprint; the maintainer lists every revoked key in revoke_fp"))?),
            (g, h) => g.or(h),
        };
        push_revoke(&mut revokes, &id, fp);
        // what the revoked key added before this file goes with it (keys added by this very file are the signer's
        // own succession, e.g. "A adds B and revokes A", and stay)
        for d in trust.descendants(&id) {
            let fp = fp_of(&d);
            push_revoke(&mut revokes, &d, fp);
        }
    }
    Ok(Planned::Apply(Plan { serial: rot.serial, signer, today: today.to_string(), adds, revokes }))
}

impl Trust {
    /// This trust plus `plan`, in memory only: what a check or the daily notice judges a release by without writing
    /// anything.
    pub fn with_plan(&self, plan: &Plan) -> Trust {
        let mut t = self.clone();
        for r in &plan.revokes {
            t.mem_revoked.push(Revoked { id: r.id.clone(), on: plan.today.clone(), by: plan.signer.clone(), fp: r.fp.clone() });
        }
        for (k, exp) in &plan.adds {
            if plan.revokes_key(k) || self.is_compiled(k) {
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
    for r in &plan.revokes {
        match st.revoked.iter_mut().find(|x| x.id.eq_ignore_ascii_case(&r.id)) {
            // a record from before fingerprints were kept gets the fingerprint now, when the key's bytes are known
            Some(x) => {
                if x.fp.is_none() {
                    x.fp = r.fp.clone();
                }
            }
            None => {
                st.revoked.push(Revoked { id: r.id.clone(), on: plan.today.clone(), by: plan.signer.clone(), fp: r.fp.clone() });
                revoked.push(r.id.clone());
            }
        }
    }
    // The revocations reach the record before any key file goes: a crash in between leaves files the record already refuses.
    trust.save_state(&st)?;
    for r in &plan.revokes {
        let _ = fs::remove_file(trust.dir.join(format!("{}.pub", r.id)));
        let _ = fs::remove_file(trust.dir.join(format!("{}.json", r.id)));
    }
    for (k, exp) in &plan.adds {
        if st.revocation_of(k).is_some() || trust.is_compiled(k) {
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
        // a later file that brings the revoked key back is refused whole, and records nothing (the serial stays at 1)
        let again = rotation(2, "2030-01-01", &[&k2], &[]);
        let e = apply_rotation(&tr, again.as_bytes(), &sign(&sk, again.as_bytes()), "2026-10-06").unwrap_err();
        assert_eq!(refusal(&e).map(|r| r.why.kind()), Some("revoked"), "{e:#}");
        assert_eq!(tr.state().unwrap().serial, 1);
        assert!(tr.trusted("2026-10-06").unwrap().iter().all(|k| k.id != id_of(&k2)));
    }

    /// `real`'s 32 key bytes under another id: the id bytes of the key file change, the key does not. What a person would be handed
    /// as "a new key" when the old one was revoked.
    pub fn with_fresh_id(real: &str) -> String {
        let mut r = B64.decode(parse_pub(real, Source::Manual).unwrap().b64).unwrap();
        for b in &mut r[2..10] {
            *b ^= 0x5a;
        }
        format!("untrusted comment: same bytes, fresh id\n{}\n", B64.encode(r))
    }

    /// `sig` with its key id (bytes 2..10 of its second line) replaced: what a forger does with a signature a leaked key made.
    fn sig_claiming(sig: &str, id: &str) -> String {
        let mut lines: Vec<String> = sig.lines().map(str::to_string).collect();
        let mut raw = B64.decode(&lines[1]).unwrap();
        for (i, b) in raw[2..10].iter_mut().enumerate() {
            *b = u8::from_str_radix(&id[2 * (7 - i)..2 * (7 - i) + 2], 16).unwrap();
        }
        lines[1] = B64.encode(raw);
        lines.join("\n") + "\n"
    }

    #[test]
    fn a_revoked_key_has_no_way_back_under_a_fresh_id() {
        // arm-2: the record keeps the revoked key's fingerprint, so the same bytes under another id are refused by hand and in a rotation
        let (pk, sk) = keypair();
        let (leaked, leaked_sk) = keypair();
        let (_t, tr) = trust_with(&pk);
        let add = rotation(1, "2030-01-01", &[&leaked], &[]);
        apply_rotation(&tr, add.as_bytes(), &sign(&sk, add.as_bytes()), "2026-10-06").unwrap();
        let revoke = rotation(2, "2030-01-01", &[], &[&id_of(&leaked)]);
        apply_rotation(&tr, revoke.as_bytes(), &sign(&sk, revoke.as_bytes()), "2026-10-06").unwrap();
        let fp = parse_pub(&leaked, Source::Manual).unwrap().fingerprint.replace(' ', "");
        assert_eq!(tr.state().unwrap().revoked.iter().find(|r| r.id == id_of(&leaked)).unwrap().fp.as_deref(), Some(fp.as_str()));
        // the key's file is gone, and its bytes come back under a fresh id
        assert!(!tr.dir.join(format!("{}.pub", id_of(&leaked))).exists());
        let fresh = with_fresh_id(&leaked);
        assert_ne!(id_of(&fresh), id_of(&leaked));
        assert_eq!(kind_of(tr.add_manual(&fresh)), "revoked");
        assert!(!tr.dir.join(format!("{}.pub", id_of(&fresh))).exists());
        let back = rotation(3, "2030-01-01", &[&fresh], &[]);
        let e = apply_rotation(&tr, back.as_bytes(), &sign(&sk, back.as_bytes()), "2026-10-06").unwrap_err();
        assert_eq!(refusal(&e).map(|r| r.why.kind()), Some("revoked"), "{e:#}");
        assert_eq!(tr.state().unwrap().serial, 2, "a refused file records nothing");
        assert!(!tr.dir.join(format!("{}.pub", id_of(&fresh))).exists());
        // the leaked secret cannot sign under the fresh id either: the claim fails and the id is not trusted
        let claim = sig_claiming(&sign(&leaked_sk, b"x"), &id_of(&fresh));
        let e = format!("{:#}", tr.verify(b"x", &claim, "2026-10-06").unwrap_err());
        assert!(e.contains("not one this program trusts") || e.contains("revoked"), "{e}");
    }

    #[test]
    fn a_revocation_fingerprint_in_the_file_refuses_a_key_this_machine_never_held() {
        // arm-2 for a key whose bytes were never on this machine: the record has no fingerprint of its own, so the signed file gives one
        let (pk, sk) = keypair();
        let (leaked, _) = keypair();
        let (_t, tr) = trust_with(&pk);
        let fp = fp_hex(&parse_pub(&leaked, Source::Manual).unwrap().fingerprint);
        let revoke = format!(r#"{{"schema":1,"serial":1,"expires":"2030-01-01","add":[],"revoke":[],"revoke_fp":[{{"id":"{}","fp":"{fp}"}}]}}"#, id_of(&leaked));
        apply_rotation(&tr, revoke.as_bytes(), &sign(&sk, revoke.as_bytes()), "2026-10-06").unwrap();
        assert_eq!(tr.state().unwrap().revoked.iter().find(|r| r.id == id_of(&leaked)).and_then(|r| r.fp.as_deref()), Some(fp.as_str()));
        let fresh = with_fresh_id(&leaked);
        assert_eq!(kind_of(tr.add_manual(&fresh)), "revoked");
        assert!(!tr.dir.join(format!("{}.pub", id_of(&fresh))).exists());
        let back = rotation(2, "2030-01-01", &[&fresh], &[]);
        let e = apply_rotation(&tr, back.as_bytes(), &sign(&sk, back.as_bytes()), "2026-10-06").unwrap_err();
        assert_eq!(refusal(&e).map(|r| r.why.kind()), Some("revoked"), "{e:#}");
        assert_eq!(tr.state().unwrap().serial, 1, "a refused file records nothing");
    }

    #[test]
    fn a_revocation_fingerprint_that_names_another_key_refuses_the_whole_file() {
        let (pk, sk) = keypair();
        let (known, _) = keypair();
        let (other, _) = keypair();
        let (_t, tr) = trust_with(&pk);
        let add = rotation(1, "2030-01-01", &[&known], &[]);
        apply_rotation(&tr, add.as_bytes(), &sign(&sk, add.as_bytes()), "2026-10-06").unwrap();
        let other_fp = fp_hex(&parse_pub(&other, Source::Manual).unwrap().fingerprint);
        let wrong = format!(r#"{{"schema":1,"serial":2,"expires":"2030-01-01","add":[],"revoke":[],"revoke_fp":[{{"id":"{}","fp":"{other_fp}"}}]}}"#, id_of(&known));
        let e = apply_rotation(&tr, wrong.as_bytes(), &sign(&sk, wrong.as_bytes()), "2026-10-06").unwrap_err();
        assert!(format!("{e:#}").contains("fingerprint"), "{e:#}");
        assert_eq!(tr.state().unwrap().serial, 1, "a refused file records nothing");
        assert!(tr.trusted("2026-10-06").unwrap().iter().any(|k| k.id == id_of(&known)), "the key it names is still trusted");
        // two different fingerprints for one id in one file: refused whole, whichever one is right
        let known_fp = fp_hex(&parse_pub(&known, Source::Manual).unwrap().fingerprint);
        let two = format!(
            r#"{{"schema":1,"serial":2,"expires":"2030-01-01","add":[],"revoke":[],"revoke_fp":[{{"id":"{k}","fp":"{known_fp}"}},{{"id":"{k}","fp":"{other_fp}"}}]}}"#,
            k = id_of(&known)
        );
        let e = apply_rotation(&tr, two.as_bytes(), &sign(&sk, two.as_bytes()), "2026-10-06").unwrap_err();
        assert!(format!("{e:#}").contains("two different fingerprints"), "{e:#}");
        assert_eq!(tr.state().unwrap().serial, 1, "a refused file records nothing");
        // a fingerprint that is not 16 hex digits is refused whole too, under any spelling
        for bad in ["0F7A 3A5C", "zzzzzzzzzzzzzzzz", "0F7A3A5C447EC7EF00"] {
            let json = format!(r#"{{"schema":1,"serial":2,"expires":"2030-01-01","add":[],"revoke":[],"revoke_fp":[{{"id":"{}","fp":"{bad}"}}]}}"#, id_of(&known));
            let e = apply_rotation(&tr, json.as_bytes(), &sign(&sk, json.as_bytes()), "2026-10-06").unwrap_err();
            assert!(format!("{e:#}").contains("16 hex digits"), "{bad}: {e:#}");
            assert_eq!(tr.state().unwrap().serial, 1, "{bad}: a refused file records nothing");
        }
    }

    #[test]
    fn a_fingerprint_given_later_reaches_the_record_and_the_preview() {
        // a record from before fingerprints were kept revokes the id with none (the one residual, and this machine never held the key);
        // a later signed file gives it: the in-memory preview and the record both keep the fingerprint, and the id is not revoked again
        let (pk, sk) = keypair();
        let (leaked, _) = keypair();
        let (_t, tr) = trust_with(&pk);
        fs::create_dir_all(&tr.dir).unwrap();
        fs::set_permissions(&tr.dir, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(tr.dir.join("state.json"), format!(r#"{{"serial":1,"revoked":[{{"id":"{}","on":"2026-01-01","by":"{}"}}]}}"#, id_of(&leaked), id_of(&pk))).unwrap();
        assert!(tr.state().unwrap().revoked.iter().all(|r| r.fp.is_none()));
        let fp = fp_hex(&parse_pub(&leaked, Source::Manual).unwrap().fingerprint);
        let second = format!(r#"{{"schema":1,"serial":2,"expires":"2030-01-01","add":[],"revoke":[],"revoke_fp":[{{"id":"{}","fp":"{fp}"}}]}}"#, id_of(&leaked));
        let Planned::Apply(plan) = plan_rotation(&tr, second.as_bytes(), Some(&sign(&sk, second.as_bytes())), "2026-10-06").unwrap() else {
            panic!("the second file was taken for already applied");
        };
        assert_eq!(tr.with_plan(&plan).state().unwrap().revoked[0].fp.as_deref(), Some(fp.as_str()), "the preview keeps the fingerprint");
        assert_eq!(commit_rotation(&tr, &plan).unwrap(), Rotated::Applied(vec![], vec![]), "the id was revoked already: nothing new");
        let rec = tr.state().unwrap();
        assert_eq!((rec.serial, rec.revoked.len(), rec.revoked[0].fp.as_deref()), (2, 1, Some(fp.as_str())));
    }

    #[test]
    fn a_rotation_that_revokes_a_key_this_machine_never_held_needs_its_fingerprint() {
        // a named id with no revoke_fp and no copy of its key here: the whole file is refused, and the record does not move
        let (pk, sk) = keypair();
        let (known, _) = keypair();
        let (never, _) = keypair();
        let (_t, tr) = trust_with(&pk);
        let add = rotation(1, "2030-01-01", &[&known], &[]);
        apply_rotation(&tr, add.as_bytes(), &sign(&sk, add.as_bytes()), "2026-10-06").unwrap();
        let bare = rotation(2, "2030-01-01", &[], &[&id_of(&never)]);
        let e = apply_rotation(&tr, bare.as_bytes(), &sign(&sk, bare.as_bytes()), "2026-10-06").unwrap_err();
        assert!(format!("{e:#}").contains(&format!("keys.json revokes {} without its fingerprint; the maintainer lists every revoked key in revoke_fp", id_of(&never))), "{e:#}");
        let st = tr.state().unwrap();
        assert_eq!((st.serial, st.revoked.len()), (1, 0), "a refused file records nothing");
        assert!(tr.trusted("2026-10-06").unwrap().iter().any(|k| k.id == id_of(&known)), "the key the file did not name is still trusted");
        // the same revocation with its fingerprint in revoke_fp is applied, the record keeps it, and the same bytes under a fresh id are refused
        let fp = fp_hex(&parse_pub(&never, Source::Manual).unwrap().fingerprint);
        let with_fp = format!(r#"{{"schema":1,"serial":2,"expires":"2030-01-01","add":[],"revoke":[],"revoke_fp":[{{"id":"{}","fp":"{fp}"}}]}}"#, id_of(&never));
        assert_eq!(apply_rotation(&tr, with_fp.as_bytes(), &sign(&sk, with_fp.as_bytes()), "2026-10-06").unwrap(), Rotated::Applied(vec![], vec![id_of(&never)]));
        assert_eq!(tr.state().unwrap().revoked.iter().find(|r| r.id == id_of(&never)).and_then(|r| r.fp.as_deref()), Some(fp.as_str()));
        assert_eq!(kind_of(tr.add_manual(&with_fresh_id(&never))), "revoked");
    }

    #[test]
    fn a_revocation_listed_again_without_its_fingerprint_keeps_the_one_it_was_given() {
        // a never-held id revoked WITH its fingerprint (serial 1), then listed again with none (serial 2): the record has the fingerprint, so
        // the file is accepted, the serial moves, no second entry is made, and the same bytes under a fresh id are still refused
        let (pk, sk) = keypair();
        let (never, _) = keypair();
        let (_t, tr) = trust_with(&pk);
        let fp = fp_hex(&parse_pub(&never, Source::Manual).unwrap().fingerprint);
        let first = format!(r#"{{"schema":1,"serial":1,"expires":"2030-01-01","add":[],"revoke":[],"revoke_fp":[{{"id":"{}","fp":"{fp}"}}]}}"#, id_of(&never));
        apply_rotation(&tr, first.as_bytes(), &sign(&sk, first.as_bytes()), "2026-10-06").unwrap();
        let again = rotation(2, "2030-01-01", &[], &[&id_of(&never)]);
        assert_eq!(apply_rotation(&tr, again.as_bytes(), &sign(&sk, again.as_bytes()), "2026-10-06").unwrap(), Rotated::Applied(vec![], vec![]));
        let st = tr.state().unwrap();
        assert_eq!((st.serial, st.revoked.len()), (2, 1), "the serial moved, and no second entry was made");
        assert_eq!(st.revoked[0].fp.as_deref(), Some(fp.as_str()), "the fingerprint is kept");
        assert_eq!(kind_of(tr.add_manual(&with_fresh_id(&never))), "revoked");
        let back = rotation(3, "2030-01-01", &[&with_fresh_id(&never)], &[]);
        let e = apply_rotation(&tr, back.as_bytes(), &sign(&sk, back.as_bytes()), "2026-10-06").unwrap_err();
        assert_eq!(refusal(&e).map(|r| r.why.kind()), Some("revoked"), "{e:#}");
        assert_eq!(tr.state().unwrap().serial, 2, "a refused file records nothing");
        // the crash window: the record was saved with the fingerprint and the serial did not move; the same file without one is accepted
        let (_t2, tr2) = trust_with(&pk);
        fs::create_dir_all(&tr2.dir).unwrap();
        fs::set_permissions(&tr2.dir, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(tr2.dir.join("state.json"), format!(r#"{{"serial":1,"revoked":[{{"id":"{}","on":"2026-10-06","by":"{}","fp":"{fp}"}}]}}"#, id_of(&never), id_of(&pk))).unwrap();
        assert_eq!(apply_rotation(&tr2, again.as_bytes(), &sign(&sk, again.as_bytes()), "2026-10-06").unwrap(), Rotated::Applied(vec![], vec![]));
        assert_eq!(tr2.state().unwrap().revoked[0].fp.as_deref(), Some(fp.as_str()));
    }

    #[test]
    fn a_held_key_revoked_without_a_fingerprint_is_recorded_with_its_own() {
        // the compiled-in key is on this machine: revoked by its id alone, the record still gets the fingerprint computed here
        let (pk, sk) = keypair();
        let (_t, tr) = trust_with(&pk);
        let json = rotation(1, "2030-01-01", &[], &[&id_of(&pk)]);
        apply_rotation(&tr, json.as_bytes(), &sign(&sk, json.as_bytes()), "2026-10-06").unwrap();
        let fp = fp_hex(&parse_pub(&pk, Source::Manual).unwrap().fingerprint);
        assert_eq!(tr.state().unwrap().revoked.iter().find(|r| r.id == id_of(&pk)).and_then(|r| r.fp.as_deref()), Some(fp.as_str()));
    }

    #[test]
    fn a_revoked_compiled_in_key_has_no_way_back_under_a_fresh_id() {
        let (pk, sk) = keypair();
        let (_t, tr) = trust_with(&pk);
        let r = rotation(1, "2030-01-01", &[], &[&id_of(&pk)]);
        apply_rotation(&tr, r.as_bytes(), &sign(&sk, r.as_bytes()), "2026-10-06").unwrap();
        assert_eq!(kind_of(tr.add_manual(&with_fresh_id(&pk))), "revoked");
        assert_eq!(kind_of(tr.add_manual(&pk)), "revoked");
    }

    #[test]
    fn trusted_never_lists_a_key_whose_fingerprint_is_revoked() {
        // a key planted by hand under a fresh id, with its bytes' fingerprint in the record: refused by trusted() and by verify()
        let (pk, _) = keypair();
        let (leaked, leaked_sk) = keypair();
        let (_t, tr) = trust_with(&pk);
        fs::create_dir_all(&tr.dir).unwrap();
        fs::set_permissions(&tr.dir, fs::Permissions::from_mode(0o700)).unwrap();
        let fp = parse_pub(&leaked, Source::Manual).unwrap().fingerprint.replace(' ', "");
        fs::write(
            tr.dir.join("state.json"),
            format!(r#"{{"serial":1,"revoked":[{{"id":"0123456789ABCDEF","on":"2026-01-01","by":"{}","fp":"{fp}"}}]}}"#, id_of(&pk)),
        )
        .unwrap();
        let fresh = with_fresh_id(&leaked);
        fs::write(tr.dir.join(format!("{}.pub", id_of(&fresh))), &fresh).unwrap();
        let trusted = tr.trusted("2026-10-06").unwrap();
        assert!(trusted.iter().all(|k| k.id != id_of(&fresh)), "{trusted:?}");
        assert_eq!(trusted.len(), 1, "only the compiled-in key is left");
        // the leaked key's real signature, claimed under the fresh id: the fresh id is not trusted, so it does not verify
        let claim = sig_claiming(&sign(&leaked_sk, b"x"), &id_of(&fresh));
        assert!(tr.verify(b"x", &claim, "2026-10-06").is_err());
    }

    #[test]
    fn a_fingerprint_that_is_not_sixteen_hex_digits_refuses_the_record() {
        let (pk, _) = keypair();
        let (_t, tr) = trust_with(&pk);
        fs::create_dir_all(&tr.dir).unwrap();
        fs::set_permissions(&tr.dir, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(tr.dir.join("state.json"), r#"{"serial":0,"revoked":[{"id":"0123456789ABCDEF","on":"x","by":"y","fp":"0F7A 3A5C"}]}"#).unwrap();
        assert_eq!(kind_of(tr.state()), "unsafe");
        // and a spelling with spaces and capitals is the same fingerprint
        fs::write(tr.dir.join("state.json"), r#"{"serial":0,"revoked":[{"id":"0123456789ABCDEF","on":"x","by":"y","fp":"0F7A 3A5C 447E C7EF"}]}"#).unwrap();
        assert_eq!(tr.state().unwrap().revoked[0].fp.as_deref(), Some("0f7a3a5c447ec7ef"));
    }

    #[test]
    fn add_manual_refuses_a_key_already_in_the_list_and_never_rewrites_its_record() {
        // a rotation key with an expiry, and a manual key: adding either again, under its id or under another id, is refused
        let (pk, sk) = keypair();
        let (rot_pk, _) = keypair();
        let (manual, _) = keypair();
        let (_t, tr) = trust_with(&pk);
        let json = format!(r#"{{"schema":1,"serial":1,"expires":"2030-01-01","add":[{{"key":"{}","expires":"2031-01-01"}}]}}"#, parse_pub(&rot_pk, Source::Manual).unwrap().b64);
        apply_rotation(&tr, json.as_bytes(), &sign(&sk, json.as_bytes()), "2026-10-06").unwrap();
        tr.add_manual(&manual).unwrap();
        let rot_id = id_of(&rot_pk);
        let meta_before = fs::read(tr.dir.join(format!("{rot_id}.json"))).unwrap();
        let e = tr.add_manual(&rot_pk).unwrap_err();
        assert_eq!(refusal(&e).map(|r| r.why.kind()), Some("collision"), "{e:#}");
        assert!(format!("{e:#}").contains("already in the list"), "{e:#}");
        assert_eq!(kind_of(tr.add_manual(&with_fresh_id(&rot_pk))), "collision");
        assert_eq!(kind_of(tr.add_manual(&manual)), "collision");
        assert_eq!(kind_of(tr.add_manual(&with_fresh_id(&manual))), "collision");
        // the rotation key kept its source, its expiry and who added it
        assert_eq!(fs::read(tr.dir.join(format!("{rot_id}.json"))).unwrap(), meta_before);
        let row = tr.all().into_iter().find(|k| k.id == rot_id).unwrap();
        assert_eq!((row.source, row.expires.as_deref()), (Source::Rotation, Some("2031-01-01")));
        // the compiled-in key under another id: already trusted, not added
        assert_eq!(kind_of(tr.add_manual(&with_fresh_id(&pk))), "compiled");
    }

    #[test]
    fn a_crash_while_the_keys_dir_is_made_leaves_nothing_that_reads_as_a_keys_dir() {
        // (a) a leftover temp dir from a crash before the rename: it holds a record that revokes something and it is not the keys dir
        let (pk, sk) = keypair();
        let (extra, _) = keypair();
        let (_t, tr) = trust_with(&pk);
        let tmp = tr.dir.parent().unwrap().join(".keys.new");
        fs::create_dir_all(&tmp).unwrap();
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(tmp.join("state.json"), r#"{"serial":9,"revoked":[{"id":"0123456789ABCDEF","on":"2026-01-01","by":"x"}]}"#).unwrap();
        assert_eq!(tr.state().unwrap().serial, 0, "no keys dir: the leftover is not the record");
        assert_eq!(tr.trusted("2026-10-06").unwrap().len(), 1, "trusts only the compiled-in key");
        tr.add_manual(&extra).unwrap();
        assert!(!tmp.exists(), "the leftover is removed when the dir is made");
        assert_eq!(tr.state().unwrap().serial, 0);
        assert!(tr.state().unwrap().revoked.is_empty(), "the leftover's revocation was never the record");
        // (b) a leftover that is a plain file, the same name: removed, not trusted
        fs::remove_dir_all(&tr.dir).unwrap();
        fs::write(&tmp, "junk").unwrap();
        tr.add_manual(&extra).unwrap();
        assert!(!tmp.exists() && tr.dir.join("state.json").is_file());
        // (c) a keys dir with no record (the shape a crash could leave before the rename existed): refused with the way out, and
        // nothing is trusted or verified on the strength of it
        fs::remove_file(tr.dir.join("state.json")).unwrap();
        let e = format!("{:#}", tr.trusted("2026-10-06").unwrap_err());
        assert!(e.contains("is missing although") && e.contains("To recover"), "{e}");
        let json = rotation(1, "2030-01-01", &[], &[]);
        assert!(apply_rotation(&tr, json.as_bytes(), &sign(&sk, json.as_bytes()), "2026-10-06").is_err());
    }

    #[test]
    fn a_fault_after_the_record_and_before_the_rename_leaves_no_keys_dir() {
        // the keys dir is built beside its place and renamed in. The fault sits after the record is written and before the rename: a
        // build made in place (no rename) leaves a dir here, and this test fails.
        let (pk, _) = keypair();
        let (extra, _) = keypair();
        let (_t, tr) = trust_with(&pk);
        let leftover = tr.dir.parent().unwrap().join(".keys.new");
        FAIL_BEFORE_RENAME.with(|f| f.set(true));
        let r = tr.add_manual(&extra);
        FAIL_BEFORE_RENAME.with(|f| f.set(false));
        let e = r.unwrap_err();
        assert!(format!("{e:#}").contains("injected fault"), "{e:#}");
        assert!(!tr.dir.exists(), "no keys dir was made");
        assert!(leftover.join("state.json").is_file(), "the record was written before the fault");
        let st = tr.state().unwrap();
        assert_eq!((st.serial, st.revoked.len()), (0, 0), "the fresh default, not a record that revokes or trusts anything");
        assert_eq!(tr.trusted("2026-10-06").unwrap().len(), 1, "trusts only the compiled-in key");
        // disarmed: the next run removes the leftover, makes the dir, and the key is added
        tr.add_manual(&extra).unwrap();
        assert!(!leftover.exists(), "the leftover is removed");
        assert!(tr.dir.join("state.json").is_file());
        assert_eq!(tr.trusted("2026-10-06").unwrap().len(), 2);
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
    fn a_key_a_revoked_key_added_keeps_its_fingerprint_only_while_its_bytes_are_held() {
        // A adds HELD and GONE; GONE's .pub is removed by hand before A is revoked: HELD is recorded with its fingerprint, GONE by id alone
        let (a_pk, a_sk) = keypair();
        let (held, _) = keypair();
        let (gone, _) = keypair();
        let (_t, tr) = trust_with(&a_pk);
        let r1 = rotation(1, "2030-01-01", &[&held, &gone], &[]);
        apply_rotation(&tr, r1.as_bytes(), &sign(&a_sk, r1.as_bytes()), "2026-10-06").unwrap();
        fs::remove_file(tr.dir.join(format!("{}.pub", id_of(&gone)))).unwrap();
        let r2 = rotation(2, "2030-01-01", &[], &[&id_of(&a_pk)]);
        apply_rotation(&tr, r2.as_bytes(), &sign(&a_sk, r2.as_bytes()), "2026-10-06").unwrap();
        let st = tr.state().unwrap();
        let recorded = |id: String| st.revoked.iter().find(|r| r.id == id).and_then(|r| r.fp.clone());
        assert_eq!(recorded(id_of(&held)), Some(fp_hex(&parse_pub(&held, Source::Manual).unwrap().fingerprint)));
        assert_eq!(recorded(id_of(&gone)), None, "its bytes are gone from this machine: the id alone");
        assert!(st.revoked.iter().any(|r| r.id == id_of(&gone)), "the id is still revoked");
        assert!(tr.trusted("2026-10-06").unwrap().is_empty(), "A is revoked, and nothing it added is trusted");
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

    fn kind_of<T: std::fmt::Debug>(r: Result<T>) -> &'static str {
        let e = r.unwrap_err();
        refusal(&e).unwrap_or_else(|| panic!("not a typed refusal: {e:#}")).why.kind()
    }

    #[test]
    fn every_refusal_carries_its_kind_through_added_context() {
        // `minerva keys add|remove --json` branches on these, never on the wording
        let (pk, _) = keypair();
        let (extra, _) = keypair();
        let (other, _) = keypair();
        let (_t, tr) = trust_with(&pk);
        assert_eq!(kind_of(tr.add_manual(&pk)), "compiled");
        assert_eq!(kind_of(tr.remove(&id_of(&pk))), "compiled");
        assert_eq!(kind_of(tr.remove("0123456789ABCDEF")), "not-found");
        tr.add_manual(&extra).unwrap();
        assert_eq!(kind_of(tr.add_manual(&forge_same_id(&extra, &other))), "collision");
        // revoked: the record names `other`, so it is refused however it is offered
        fs::write(
            tr.dir.join("state.json"),
            format!(r#"{{"serial":1,"revoked":[{{"id":"{}","on":"2026-01-01","by":"{}"}}]}}"#, id_of(&other), id_of(&pk)),
        )
        .unwrap();
        assert_eq!(kind_of(tr.add_manual(&other)), "revoked");
        // unsafe: a corrupt record, a missing one, an open directory
        fs::write(tr.dir.join("state.json"), "{ not json").unwrap();
        assert_eq!(kind_of(tr.add_manual(&other)), "unsafe");
        fs::remove_file(tr.dir.join("state.json")).unwrap();
        assert_eq!(kind_of(tr.state()), "unsafe");
        fs::write(tr.dir.join("state.json"), r#"{"serial":0,"revoked":[]}"#).unwrap();
        fs::set_permissions(&tr.dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(kind_of(tr.add_manual(&other)), "unsafe");
        // the sentence is still the message, with or without context on top
        let e = tr.add_manual(&other).unwrap_err();
        let wrapped = e.context("adding a key");
        assert!(refusal(&wrapped).is_some(), "context must not hide the refusal");
        assert!(format!("{wrapped:#}").contains("not a private directory"));
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
