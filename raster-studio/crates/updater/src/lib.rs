//! Signed update manifest handling.
//!
//! Updates are gated by an Ed25519 signature over the update manifest, using
//! the same public-key-in-app model as `licensing`. The app refuses to apply an
//! update whose manifest signature doesn't verify — a network CDN compromise
//! can't push a malicious build. Downloading/applying the payload is left to
//! the platform installer; this crate is the trust decision.

use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

/// Describes an available update.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpdateManifest {
    pub version: String,
    pub url: String,
    /// Hex BLAKE3 of the payload, verified after download.
    pub payload_hash_hex: String,
    pub notes: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum UpdateError {
    #[error("manifest signature invalid")]
    BadSignature,
    #[error("malformed signature")]
    MalformedSignature,
    #[error("serialization error: {0}")]
    Serialization(String),
}

/// Verify a manifest signature against the embedded public key.
pub fn verify_manifest(
    manifest: &UpdateManifest,
    signature_hex: &str,
    public_key: &VerifyingKey,
) -> Result<(), UpdateError> {
    let bytes =
        serde_json::to_vec(manifest).map_err(|e| UpdateError::Serialization(e.to_string()))?;
    let sig_bytes = unhex(signature_hex).ok_or(UpdateError::MalformedSignature)?;
    let sig_arr: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|_| UpdateError::MalformedSignature)?;
    public_key
        .verify(&bytes, &Signature::from_bytes(&sig_arr))
        .map_err(|_| UpdateError::BadSignature)
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use rand::rngs::OsRng;

    #[test]
    fn signed_manifest_verifies() {
        let sk = SigningKey::generate(&mut OsRng);
        let vk = sk.verifying_key();
        let m = UpdateManifest {
            version: "0.2.0".into(),
            url: "https://example.com/x".into(),
            payload_hash_hex: "abcd".into(),
            notes: "fixes".into(),
        };
        let sig = sk.sign(&serde_json::to_vec(&m).unwrap());
        let sig_hex: String = sig.to_bytes().iter().map(|b| format!("{b:02x}")).collect();
        assert!(verify_manifest(&m, &sig_hex, &vk).is_ok());

        let mut tampered = m.clone();
        tampered.url = "https://evil.example".into();
        assert_eq!(
            verify_manifest(&tampered, &sig_hex, &vk),
            Err(UpdateError::BadSignature)
        );
    }
}
