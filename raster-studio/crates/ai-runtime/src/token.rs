//! Per-launch capability token. Generated fresh each time the sidecar starts;
//! every request to the runtime must carry it. This prevents other local
//! processes from submitting jobs to our ComfyUI instance.

use rand::RngCore;

/// A 256-bit random bearer token, hex-encoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityToken(String);

impl CapabilityToken {
    /// Generate a fresh random token from the OS CSPRNG.
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        CapabilityToken(bytes.iter().map(|b| format!("{b:02x}")).collect())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Constant-time-ish comparison for auth checks. (For real use, prefer a
    /// crate providing constant-time equality; this avoids the dep in scaffold.)
    pub fn matches(&self, presented: &str) -> bool {
        if self.0.len() != presented.len() {
            return false;
        }
        let mut diff = 0u8;
        for (a, b) in self.0.bytes().zip(presented.bytes()) {
            diff |= a ^ b;
        }
        diff == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_unique_and_long() {
        let a = CapabilityToken::generate();
        let b = CapabilityToken::generate();
        assert_ne!(a, b);
        assert_eq!(a.as_str().len(), 64);
    }

    #[test]
    fn matches_only_exact() {
        let t = CapabilityToken::generate();
        assert!(t.matches(t.as_str()));
        assert!(!t.matches("deadbeef"));
    }
}
