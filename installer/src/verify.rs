//! Signature check. The public key is compiled in (`build.rs`); nothing the network says can change it.

use anyhow::{Result, anyhow};
use minisign_verify::{PublicKey, Signature};

/// The operator's public key file, as text (two lines: an untrusted comment and the base64 key).
pub const PUBKEY: &str = include_str!(concat!(env!("OUT_DIR"), "/update.pub"));

/// Verify `data` against the minisign signature file text `sig`, with the key file text `pubkey`.
/// Refuses on any problem (a malformed key or signature, a wrong key, tampered data). Prehashed signatures are
/// accepted (what `minisign -S` writes); legacy ones are not.
pub fn verify(pubkey: &str, data: &[u8], sig: &str) -> Result<()> {
    let pk = PublicKey::decode(pubkey).map_err(|e| anyhow!("the compiled-in update public key is unusable ({e}); refusing"))?;
    let sg = Signature::decode(sig).map_err(|e| anyhow!("the signature file is malformed ({e}); refusing"))?;
    pk.verify(data, &sg, false).map_err(|e| anyhow!("signature check FAILED ({e}); refusing"))
}

#[cfg(test)]
pub mod testkeys {
    //! Throwaway keys for tests (never the operator's).

    /// (public key file text, secret key)
    pub fn keypair() -> (String, minisign::SecretKey) {
        let kp = minisign::KeyPair::generate_unencrypted_keypair().unwrap();
        (kp.pk.to_box().unwrap().to_string(), kp.sk)
    }

    pub fn sign(sk: &minisign::SecretKey, data: &[u8]) -> String {
        minisign::sign(None, sk, std::io::Cursor::new(data), None, None).unwrap().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::testkeys::{keypair, sign};
    use super::*;

    #[test]
    fn good_wrong_key_tampered_and_garbage() {
        let (pk, sk) = keypair();
        let (other_pk, _) = keypair();
        let sig = sign(&sk, b"hello");
        verify(&pk, b"hello", &sig).unwrap();
        assert!(verify(&pk, b"hellO", &sig).is_err());
        assert!(verify(&other_pk, b"hello", &sig).is_err());
        assert!(verify(&pk, b"hello", "garbage").is_err());
        assert!(verify("PLACEHOLDER\n", b"hello", &sig).is_err());
    }
}
