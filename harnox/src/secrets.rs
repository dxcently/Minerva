//! The custodied secret store — `secrets.enc` + `secret.key` in a directory
//! the consumer chooses (feature `secrets`).
//!
//! Design of record: the vault note *Secret Storage (design)* (2026-08-09).
//! The store defends against a compromised script / coding agent / installed
//! app and against a stolen backup or exfiltrated store file: `secrets.enc` is
//! meant to be backed up while `secret.key` is **excluded**, so a backup alone
//! decrypts nothing. It deliberately does NOT defend against root on the live
//! box — keyfile and ciphertext are co-resident.
//!
//! ## Core invariant: no read path
//!
//! There is no `secret_get` on any tool surface. An `external` secret's value
//! exits only through [`SecretStore::external_value`], a Rust-side injection
//! point the consumer must never expose to a script or a tool and must audit
//! as `(name, consumer)`, never the value; an `issued` token is generated
//! here, returned to the caller exactly once, and only its SHA-256 survives —
//! verification ([`SecretStore::verify`]) is a constant-time hash compare, not
//! a read. Scripts never hold a secret as a string.
//!
//! ## On-disk format
//!
//! `[1 byte version][24-byte XChaCha20 nonce][ciphertext + 16-byte Poly1305 tag]`
//! with the version byte passed as AAD, so a tampered version fails
//! authentication instead of being trusted. A fresh nonce is drawn from OS
//! randomness on every write. Writes are atomic: temp file (0600) → fsync →
//! rename ([`crate::fs::write_atomic_0600`]).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq as _;
use zeroize::Zeroizing;

use crate::crypto::{hex, random_token, sha256};
use crate::fs::write_atomic_0600;
use crate::time::now_secs;

/// The on-disk format version — the store's first byte, also fed to the AEAD
/// as associated data.
const FORMAT_VERSION: u8 = 1;

const NONCE_LEN: usize = 24;
const KEY_LEN: usize = 32;

/// Serializes every load→mutate→save cycle in this process so two concurrent
/// tool calls can't lose each other's write. Cross-process races are out of
/// scope: the daemon is the only writer.
static WRITE_LOCK: Mutex<()> = Mutex::new(());

/// Where a secret's material lives and how it may leave.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A value the operator pasted in (API key, service token). Stored
    /// encrypted; exits only via Rust-side injection, never over a tool.
    External,
    /// A token the store generated and handed out exactly once. Only its
    /// SHA-256 is stored — there is nothing to read even with the keyfile.
    Issued,
}

/// One stored secret. `value` is set for `External`, `sha256` (hex) for
/// `Issued` — never both. Issued hashes skip a KDF deliberately: they digest
/// high-entropy random tokens, not passwords.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    kind: Kind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sha256: Option<String>,
    created: u64,
    rotated: u64,
    /// Optional binding to a service definition; a store entry whose name
    /// matches a pinned service is injected for it regardless.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    service: Option<String>,
    /// Skills/apps allowed to consume this secret (external kind only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    grants: Vec<String>,
}

/// The decrypted plaintext: a serde map, JSON-encoded.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Data {
    entries: BTreeMap<String, Entry>,
}

/// What a listing shows: names + metadata, never values or hashes.
#[derive(Debug, Clone, Serialize)]
pub struct Meta {
    pub name: String,
    pub kind: Kind,
    pub created: u64,
    pub rotated: u64,
    pub service: Option<String>,
    pub grants: Vec<String>,
}

/// The store rooted at a directory. All operations load, act, and persist per
/// call: the file is small and always in page cache, and per-call loading is
/// what makes revocation instant with zero cache-coherence logic.
pub struct SecretStore {
    dir: PathBuf,
}

impl SecretStore {
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn store_path(&self) -> PathBuf {
        self.dir.join("secrets.enc")
    }

    fn key_path(&self) -> PathBuf {
        self.dir.join("secret.key")
    }

    /// The 32-byte store key: read `secret.key`, or generate it on first use
    /// (0600, atomic). Refuses a wrong-sized keyfile rather than deriving
    /// anything from it.
    fn load_or_create_key(&self) -> Result<Zeroizing<[u8; KEY_LEN]>> {
        let path = self.key_path();
        match std::fs::read(&path) {
            Ok(bytes) => {
                let buf = Zeroizing::new(bytes);
                if buf.len() != KEY_LEN {
                    bail!(
                        "{} is {} bytes, expected {KEY_LEN} — refusing to touch the store",
                        path.display(),
                        buf.len()
                    );
                }
                let mut key = Zeroizing::new([0u8; KEY_LEN]);
                key.copy_from_slice(&buf);
                Ok(key)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let mut key = Zeroizing::new([0u8; KEY_LEN]);
                getrandom::fill(key.as_mut()).context("OS CSPRNG unavailable")?;
                write_atomic_0600(&path, key.as_ref()).with_context(|| format!("writing {}", path.display()))?;
                Ok(key)
            }
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// Decrypt and deserialize the store; an absent `secrets.enc` is an empty
    /// store. An absent keyfile *with* a present store is an error (backup
    /// restored without its key), never a silent empty result.
    fn load(&self) -> Result<Data> {
        let path = self.store_path();
        let raw = match std::fs::read(&path) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Data::default()),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        if !self.key_path().exists() {
            bail!(
                "{} exists but {} is missing — a restored backup deliberately \
                 excludes the keyfile; without it the store is unrecoverable",
                path.display(),
                self.key_path().display()
            );
        }
        let key = self.load_or_create_key()?;
        if raw.len() < 1 + NONCE_LEN + 16 {
            bail!("{} is truncated ({} bytes)", path.display(), raw.len());
        }
        let version = raw[0];
        if version != FORMAT_VERSION {
            bail!("{} has unknown format version {version}", path.display());
        }
        let nonce = XNonce::from_slice(&raw[1..1 + NONCE_LEN]);
        let cipher = XChaCha20Poly1305::new_from_slice(key.as_ref()).expect("32-byte key");
        let plain = Zeroizing::new(
            cipher
                .decrypt(nonce, Payload { msg: &raw[1 + NONCE_LEN..], aad: &[version] })
                .map_err(|_| {
                    anyhow::anyhow!("decrypting {} failed — wrong keyfile or tampered store", path.display())
                })?,
        );
        serde_json::from_slice(&plain).context("deserializing the decrypted secret store")
    }

    /// Re-encrypt with a fresh nonce and write atomically (0600).
    fn save(&self, data: &Data) -> Result<()> {
        let key = self.load_or_create_key()?;
        let plain = Zeroizing::new(serde_json::to_vec(data)?);
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce).context("OS CSPRNG unavailable")?;
        let cipher = XChaCha20Poly1305::new_from_slice(key.as_ref()).expect("32-byte key");
        let ct = cipher
            .encrypt(XNonce::from_slice(&nonce), Payload { msg: &plain, aad: &[FORMAT_VERSION] })
            .map_err(|_| anyhow::anyhow!("encrypting the secret store failed"))?;
        let mut out = Vec::with_capacity(1 + NONCE_LEN + ct.len());
        out.push(FORMAT_VERSION);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        let path = self.store_path();
        write_atomic_0600(&path, &out).with_context(|| format!("writing {}", path.display()))
    }

    /// Store (or rotate — set *is* rotate) an external secret's value. The
    /// value transits the transport exactly once, on the way in.
    pub fn set(&self, name: &str, value: &str, service: Option<String>, grants: Vec<String>) -> Result<String> {
        validate_name(name)?;
        if value.trim().is_empty() {
            bail!("refusing to store an empty value for '{name}'");
        }
        let _guard = WRITE_LOCK.lock().unwrap();
        let mut data = self.load()?;
        let now = now_secs();
        let verb = match data.entries.get(name) {
            Some(prev) if prev.kind == Kind::Issued => {
                bail!("'{name}' is an issued token — rotate it with secret_issue, not secret_set")
            }
            Some(_) => "rotated",
            None => "stored",
        };
        let created = data.entries.get(name).map(|e| e.created).unwrap_or(now);
        data.entries.insert(
            name.to_string(),
            Entry {
                kind: Kind::External,
                value: Some(value.to_string()),
                sha256: None,
                created,
                rotated: now,
                service,
                grants,
            },
        );
        self.save(&data)?;
        Ok(format!("{verb} external secret '{name}' (value not retrievable — set again to rotate)"))
    }

    /// Generate a fresh random token under `name`, store only its SHA-256, and
    /// return the plaintext — the one time it ever exists outside the caller's
    /// hands. Re-issuing under an existing name is rotation; the old token
    /// stops verifying immediately.
    pub fn issue(&self, name: &str) -> Result<String> {
        validate_name(name)?;
        let _guard = WRITE_LOCK.lock().unwrap();
        let mut data = self.load()?;
        if data.entries.get(name).is_some_and(|e| e.kind == Kind::External) {
            bail!("'{name}' is an external secret — overwrite it with secret_set, not secret_issue");
        }
        let token = random_token();
        let now = now_secs();
        let created = data.entries.get(name).map(|e| e.created).unwrap_or(now);
        data.entries.insert(
            name.to_string(),
            Entry {
                kind: Kind::Issued,
                value: None,
                sha256: Some(hex(&sha256(token.as_bytes()))),
                created,
                rotated: now,
                service: None,
                grants: Vec::new(),
            },
        );
        self.save(&data)?;
        Ok(token)
    }

    /// Delete an entry — revocation for issued tokens, removal for external
    /// values. Errors on an unknown name so a typo can't read as success.
    pub fn delete(&self, name: &str) -> Result<String> {
        let _guard = WRITE_LOCK.lock().unwrap();
        let mut data = self.load()?;
        if data.entries.remove(name).is_none() {
            bail!("no secret named '{name}'");
        }
        self.save(&data)?;
        Ok(format!("deleted secret '{name}'"))
    }

    /// Names + metadata only — the rotation-age visibility surface. Values and
    /// hashes never appear here.
    pub fn list(&self) -> Result<Vec<Meta>> {
        let data = self.load()?;
        Ok(data
            .entries
            .into_iter()
            .map(|(name, e)| Meta {
                name,
                kind: e.kind,
                created: e.created,
                rotated: e.rotated,
                service: e.service,
                grants: e.grants,
            })
            .collect())
    }

    /// Whether an entry of any kind exists under `name` — for a gate deciding
    /// whether the store owns a credential or a legacy fallback still applies.
    pub fn has(&self, name: &str) -> bool {
        self.load().is_ok_and(|data| data.entries.contains_key(name))
    }

    /// Constant-time check of a presented token against an `issued` entry's
    /// stored hash. Verification is not reading — the no-read-path invariant
    /// holds. Unknown names, external entries, and malformed hashes all
    /// verify false.
    pub fn verify(&self, name: &str, presented: &str) -> bool {
        let Ok(data) = self.load() else { return false };
        let Some(entry) = data.entries.get(name) else { return false };
        let Some(stored) = entry.sha256.as_deref() else { return false };
        let presented_hex = hex(&sha256(presented.as_bytes()));
        stored.as_bytes().ct_eq(presented_hex.as_bytes()).into()
    }

    /// The internal injection path: an `external` entry's plaintext, for a
    /// Rust-side consumer (a bearer header the consumer's own code sets).
    /// **This is the only door a value leaves through.** Never surface it on
    /// a tool or to a script, and log every injection to the audit trail as
    /// `(name, consumer)` — never the value.
    pub fn external_value(&self, name: &str) -> Option<Zeroizing<String>> {
        let data = self.load().ok()?;
        let entry = data.entries.get(name)?;
        entry.value.clone().map(Zeroizing::new)
    }
}

/// Entry names are namespaced paths: bare (`github`, `vultr`) for
/// operator/service secrets, `app:<slug>/<key>` for app-owned ones. Keep the
/// charset boring so a name is always safe to log and render.
fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() || name.len() > 128 {
        bail!("secret name must be 1–128 characters");
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':' | '/')) {
        bail!("secret name '{name}' has characters outside [A-Za-z0-9._:/-]");
    }
    Ok(())
}

/// Validate the **key half** of an app-owned entry name — the `<key>` in
/// `app:<slug>/<key>`. Stricter than the full-name rule by exactly the two
/// characters that carry structure: a key may not contain `/` or `:`, so an
/// app declaring a credential key cannot climb out of its own namespace into
/// another app's or into a bare operator secret like `github`.
pub fn validate_secret_key(key: &str) -> Result<()> {
    if key.is_empty() || key.len() > 64 {
        bail!("secret key must be 1–64 characters");
    }
    if !key.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) {
        bail!("secret key '{key}' has characters outside [A-Za-z0-9._-]");
    }
    Ok(())
}

/// Render a listing for a tool reply: one line per entry, metadata only.
pub fn render(metas: &[Meta]) -> String {
    if metas.is_empty() {
        return "no secrets stored".to_string();
    }
    let now = now_secs();
    metas
        .iter()
        .map(|m| {
            let kind = match m.kind {
                Kind::External => "external",
                Kind::Issued => "issued",
            };
            let age_days = now.saturating_sub(m.rotated) / 86_400;
            let mut line = format!("- {} ({kind}, rotated {age_days}d ago", m.name);
            if let Some(service) = &m.service {
                line.push_str(&format!(", service {service}"));
            }
            if !m.grants.is_empty() {
                line.push_str(&format!(", granted to {}", m.grants.join(", ")));
            }
            line.push(')');
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> SecretStore {
        SecretStore::at(crate::testutil::tempdir("secrets"))
    }

    #[test]
    fn set_list_delete_roundtrip_without_exposing_the_value() {
        let s = store();
        s.set("github", "ghp_secret123", Some("github".into()), vec![]).unwrap();
        let metas = s.list().unwrap();
        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].name, "github");
        assert_eq!(metas[0].kind, Kind::External);
        // The rendered list never carries the value.
        assert!(!render(&metas).contains("ghp_secret123"));
        assert_eq!(s.external_value("github").as_deref().map(String::as_str), Some("ghp_secret123"));
        s.delete("github").unwrap();
        assert!(s.list().unwrap().is_empty());
        assert!(s.external_value("github").is_none());
    }

    #[test]
    fn the_ciphertext_never_contains_the_plaintext() {
        let s = store();
        s.set("vultr", "VULTRKEY-ABCDEF", None, vec![]).unwrap();
        let raw = std::fs::read(s.store_path()).unwrap();
        assert_eq!(raw[0], FORMAT_VERSION);
        let blob = String::from_utf8_lossy(&raw);
        assert!(!blob.contains("VULTRKEY"));
        assert!(!blob.contains("vultr"));
    }

    #[test]
    fn issue_returns_the_token_once_and_verifies_by_hash_only() {
        let s = store();
        let token = s.issue("app:agora/access_token").unwrap();
        assert!(s.verify("app:agora/access_token", &token));
        assert!(!s.verify("app:agora/access_token", "wrong"));
        // The store holds only the hash — nothing to read back.
        assert!(s.external_value("app:agora/access_token").is_none());
        // Re-issue rotates: the old token stops verifying.
        let newer = s.issue("app:agora/access_token").unwrap();
        assert!(!s.verify("app:agora/access_token", &token));
        assert!(s.verify("app:agora/access_token", &newer));
    }

    #[test]
    fn kinds_do_not_cross() {
        let s = store();
        s.set("github", "tok", None, vec![]).unwrap();
        assert!(s.issue("github").is_err());
        let _ = s.issue("gate").unwrap();
        assert!(s.set("gate", "v", None, vec![]).is_err());
        // Verify against an external entry is false, not an error.
        assert!(!s.verify("github", "tok"));
    }

    #[test]
    fn a_store_without_its_keyfile_refuses_loudly() {
        let s = store();
        s.set("github", "tok", None, vec![]).unwrap();
        std::fs::remove_file(s.key_path()).unwrap();
        let err = s.list().unwrap_err().to_string();
        assert!(err.contains("keyfile"), "unexpected error: {err}");
    }

    #[test]
    fn an_unknown_version_byte_is_refused() {
        let s = store();
        s.set("github", "tok", None, vec![]).unwrap();
        let mut raw = std::fs::read(s.store_path()).unwrap();
        raw[0] = 2;
        std::fs::write(s.store_path(), &raw).unwrap();
        assert!(s.list().is_err());
    }

    #[test]
    fn a_flipped_ciphertext_byte_fails_authentication() {
        let s = store();
        s.set("github", "tok", None, vec![]).unwrap();
        let mut raw = std::fs::read(s.store_path()).unwrap();
        let last = raw.len() - 1;
        raw[last] ^= 0x01;
        std::fs::write(s.store_path(), &raw).unwrap();
        let err = s.list().unwrap_err().to_string();
        assert!(err.contains("tampered"), "unexpected error: {err}");
    }

    #[test]
    fn set_rotates_in_place_and_keeps_created() {
        let s = store();
        s.set("github", "old", None, vec![]).unwrap();
        let created = s.list().unwrap()[0].created;
        let msg = s.set("github", "new", None, vec!["ci".into()]).unwrap();
        assert!(msg.contains("rotated"));
        let metas = s.list().unwrap();
        assert_eq!(metas[0].created, created);
        assert_eq!(metas[0].grants, vec!["ci".to_string()]);
        assert_eq!(s.external_value("github").as_deref().map(String::as_str), Some("new"));
    }

    #[test]
    fn names_are_validated() {
        let s = store();
        assert!(s.set("", "v", None, vec![]).is_err());
        assert!(s.set("bad name", "v", None, vec![]).is_err());
        assert!(s.set("app:slug/key", "v", None, vec![]).is_ok());
        assert!(validate_secret_key("key").is_ok());
        assert!(validate_secret_key("../github").is_err());
        assert!(validate_secret_key("app:x").is_err());
    }
}
