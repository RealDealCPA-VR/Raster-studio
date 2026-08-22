//! Offline entitlement validation via Ed25519 signatures.
//!
//! Model: your release system holds the **private** signing key and issues a
//! signed [`Entitlement`]. The app embeds only the **public** key and verifies
//! entitlements locally — no license server, no network. A local trial state is
//! also supported so the app is usable before purchase.
//!
//! Security note: the private key must **never** ship in the app. Only the
//! public key is compiled in (or loaded from a bundled resource).

use std::time::{SystemTime, UNIX_EPOCH};

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

/// The signed claims describing what a user is entitled to.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entitlement {
    pub licensee: String,
    /// Product/edition identifier.
    pub product: String,
    /// Unix seconds after which the entitlement is expired (None = perpetual).
    pub expires_unix: Option<u64>,
    /// Major version this license is valid through (upgrade gating).
    pub max_major_version: u32,
}

/// A signed entitlement: the claims plus a detached Ed25519 signature.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedEntitlement {
    pub entitlement: Entitlement,
    /// Hex-encoded 64-byte signature over the canonical JSON of `entitlement`.
    pub signature_hex: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LicenseError {
    #[error("signature is invalid")]
    BadSignature,
    #[error("entitlement expired")]
    Expired,
    #[error("license does not cover version {0}")]
    VersionNotCovered(u32),
    #[error("malformed signature encoding")]
    MalformedSignature,
    #[error("serialization error: {0}")]
    Serialization(String),
}

/// Canonical bytes that get signed/verified (stable field order via struct).
fn canonical_bytes(e: &Entitlement) -> Result<Vec<u8>, LicenseError> {
    serde_json::to_vec(e).map_err(|err| LicenseError::Serialization(err.to_string()))
}

/// Sign an entitlement with the release private key. **Server-side only.**
pub fn sign(entitlement: &Entitlement, signing_key: &SigningKey) -> SignedEntitlement {
    let msg = canonical_bytes(entitlement).expect("entitlement serializes");
    let sig = signing_key.sign(&msg);
    SignedEntitlement {
        entitlement: entitlement.clone(),
        signature_hex: hex(sig.to_bytes().as_slice()),
    }
}

/// Verify a signed entitlement against the embedded public key, checking the
/// signature, expiry, and version coverage.
pub fn verify(
    signed: &SignedEntitlement,
    public_key: &VerifyingKey,
    app_major_version: u32,
) -> Result<(), LicenseError> {
    let sig_bytes = unhex(&signed.signature_hex).ok_or(LicenseError::MalformedSignature)?;
    let sig_arr: [u8; 64] = sig_bytes
        .try_into()
        .map_err(|_| LicenseError::MalformedSignature)?;
    let signature = Signature::from_bytes(&sig_arr);

    let msg = canonical_bytes(&signed.entitlement)?;
    public_key
        .verify(&msg, &signature)
        .map_err(|_| LicenseError::BadSignature)?;

    if let Some(exp) = signed.entitlement.expires_unix {
        if now_unix() > exp {
            return Err(LicenseError::Expired);
        }
    }
    if app_major_version > signed.entitlement.max_major_version {
        return Err(LicenseError::VersionNotCovered(app_major_version));
    }
    Ok(())
}

/// Local, unsigned trial state (stored in settings; not security-critical).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrialState {
    pub started_unix: u64,
    pub days: u64,
}

impl TrialState {
    pub fn start(days: u64) -> Self {
        Self {
            started_unix: now_unix(),
            days,
        }
    }
    pub fn is_active(&self) -> bool {
        now_unix() < self.started_unix + self.days * 86_400
    }
    pub fn days_remaining(&self) -> i64 {
        let end = self.started_unix + self.days * 86_400;
        ((end as i64) - (now_unix() as i64)).max(0) / 86_400
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
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
    use rand::rngs::OsRng;

    fn keypair() -> (SigningKey, VerifyingKey) {
        let sk = SigningKey::generate(&mut OsRng);
        let vk = sk.verifying_key();
        (sk, vk)
    }

    fn perpetual() -> Entitlement {
        Entitlement {
            licensee: "Jane Doe".into(),
            product: "raster-studio-pro".into(),
            expires_unix: None,
            max_major_version: 1,
        }
    }

    #[test]
    fn valid_signature_verifies() {
        let (sk, vk) = keypair();
        let signed = sign(&perpetual(), &sk);
        assert!(verify(&signed, &vk, 1).is_ok());
    }

    #[test]
    fn tampered_claims_fail() {
        let (sk, vk) = keypair();
        let mut signed = sign(&perpetual(), &sk);
        signed.entitlement.licensee = "Attacker".into();
        assert_eq!(verify(&signed, &vk, 1), Err(LicenseError::BadSignature));
    }

    #[test]
    fn wrong_key_fails() {
        let (sk, _) = keypair();
        let (_, other_vk) = keypair();
        let signed = sign(&perpetual(), &sk);
        assert_eq!(
            verify(&signed, &other_vk, 1),
            Err(LicenseError::BadSignature)
        );
    }

    #[test]
    fn expired_fails() {
        let (sk, vk) = keypair();
        let mut e = perpetual();
        e.expires_unix = Some(1); // 1970
        let signed = sign(&e, &sk);
        assert_eq!(verify(&signed, &vk, 1), Err(LicenseError::Expired));
    }

    #[test]
    fn version_gating() {
        let (sk, vk) = keypair();
        let signed = sign(&perpetual(), &sk);
        assert_eq!(
            verify(&signed, &vk, 2),
            Err(LicenseError::VersionNotCovered(2))
        );
    }

    #[test]
    fn trial_active_after_start() {
        let t = TrialState::start(14);
        assert!(t.is_active());
        assert!(t.days_remaining() >= 13);
    }
}
