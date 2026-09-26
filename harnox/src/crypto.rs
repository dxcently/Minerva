//! The primitives every bearer-shaped secret is built from: 256 bits of OS
//! randomness, SHA-256 for at-rest hashing and PKCE, and the two encodings
//! (base64url, lower-case hex) those bytes travel in.
//!
//! One mint, one digest, one encoder each — so OAuth tokens, PKCE verifiers,
//! issued store secrets, gateway session ids, and upload-ticket capabilities
//! all share a single audited path instead of hand-rolling their own.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

/// SHA-256 of `bytes`.
pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().into()
}

/// 256 bits of OS randomness — the raw material for tokens, codes, PKCE
/// verifiers, client ids, request ids, and AEAD nonces.
///
/// Panics if the OS CSPRNG is unavailable: there is no safe fallback for a
/// secret, and a process that cannot draw randomness must not mint one.
pub fn random_bytes() -> [u8; 32] {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).expect("OS CSPRNG unavailable");
    buf
}

/// [`random_bytes`], base64url-encoded without padding (43 characters) — the
/// one mint behind every bearer-shaped secret: OAuth access/refresh tokens,
/// auth codes, client ids, issued store secrets, session ids, capability
/// tickets.
pub fn random_token() -> String {
    URL_SAFE_NO_PAD.encode(random_bytes())
}

/// Lower-case hex of arbitrary bytes — mostly digests headed for disk or a
/// filename.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn random_token_is_43_urlsafe_chars_and_fresh_each_time() {
        let a = random_token();
        let b = random_token();
        assert_eq!(a.len(), 43);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_ne!(a, b);
    }

    #[test]
    fn sha256_matches_a_known_vector() {
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn hex_is_lower_case_and_zero_padded() {
        assert_eq!(hex(&[0x00, 0x0f, 0xff]), "000fff");
    }
}
