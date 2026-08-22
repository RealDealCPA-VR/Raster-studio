//! Package manifest — the first thing read on open, gating migrations.

use serde::{Deserialize, Serialize};

/// Current on-disk package layout version. Distinct from the *document* format
/// version: the package can gain files (previews, ai/) without the document
/// model changing, and vice versa.
pub const MANIFEST_VERSION: u32 = 1;

/// `manifest.json` contents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// Package layout version. **Mandatory** for migrations.
    pub manifest_version: u32,
    /// Application version string that wrote this package (diagnostics).
    pub app_version: String,
    /// Relative path to the serialized document within the package.
    pub document_path: String,
    /// Whether linked assets were collected/embedded ("portable project").
    pub assets_collected: bool,
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            manifest_version: MANIFEST_VERSION,
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            document_path: "document.msgpack".to_string(),
            assets_collected: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_roundtrip() {
        let m = Manifest::default();
        let s = serde_json::to_string_pretty(&m).unwrap();
        let back: Manifest = serde_json::from_str(&s).unwrap();
        assert_eq!(back.manifest_version, MANIFEST_VERSION);
    }
}
