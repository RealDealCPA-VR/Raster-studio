//! Package manifest — the first thing read on open, gating migrations and
//! carrying the package's integrity digest.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::hexid;

/// Current on-disk package layout version. Distinct from the *document* format
/// version: the package can gain files (previews, ai/) without the document
/// model changing, and vice versa.
///
/// # History
/// * `1` — pre-release. Manifest with no integrity data, no tile blobs, and a
///   `document_path` the loader trusted. Not readable by this build; see
///   [`MIN_SUPPORTED_MANIFEST_VERSION`].
/// * `2` — pixels are persisted (`tiles/`), assets are persisted (`assets/`), a
///   composite preview is written (`previews/`), and the manifest carries
///   [`Manifest::contents`] plus [`Manifest::integrity`].
pub const MANIFEST_VERSION: u32 = 2;

/// Oldest package layout this build reads.
///
/// Version 1 is refused rather than migrated, and deliberately: a v1 package
/// has no integrity data and stores no pixels, so "migrating" one would mean
/// producing a v2 package that claims verified contents it never had. No v1
/// package was ever shipped to a user.
pub const MIN_SUPPORTED_MANIFEST_VERSION: u32 = 2;

/// Size and BLAKE3 of one file in the package.
///
/// Recorded only for files that are **not** content-addressed. A tile or asset
/// blob is named by its own hash and is verified against that name when it is
/// read, so listing it here would add a second, redundant copy of the same
/// check and would make the manifest grow with the pixel count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDigest {
    pub size: u64,
    /// Lowercase hex, 64 characters.
    pub blake3: String,
}

impl FileDigest {
    pub fn of(bytes: &[u8]) -> Self {
        Self {
            size: bytes.len() as u64,
            blake3: hexid::to_hex(blake3::hash(bytes).as_bytes()),
        }
    }

    /// Whether `bytes` are the bytes this digest describes.
    pub fn matches(&self, bytes: &[u8]) -> bool {
        self.size == bytes.len() as u64 && *self == Self::of(bytes)
    }
}

/// `manifest.json` contents.
///
/// # What the integrity digest is and is not
///
/// [`Manifest::integrity`] is a BLAKE3 over the manifest's own fields and over
/// [`Manifest::contents`], which in turn holds a digest per non-content-addressed
/// file. Together they detect **corruption and tampering after the fact**: a
/// flipped bit in `document.msgpack`, a truncated preview, a swapped file.
///
/// They are **not a signature**. There is no key, so anyone who can rewrite a
/// file in the package can also recompute the digest. Treat a package that
/// verifies as intact, never as authentic — which is exactly why the loader's
/// path handling ([`crate::safepath`]) does its job *before* integrity is
/// consulted rather than trusting a package that passed the check.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// Package layout version. **Mandatory** for migrations.
    pub manifest_version: u32,
    /// Version string of the *application* that wrote this package.
    ///
    /// Supplied by the caller through [`crate::SaveOptions::app_version`]. It
    /// used to be `env!("CARGO_PKG_VERSION")` of this crate, which reported the
    /// version of the serialization library rather than of the program, and so
    /// was the same `0.1.0` for every build the user could ever run.
    pub app_version: String,
    /// Relative path to the serialized document within the package.
    ///
    /// **Diagnostic only.** The loader reads [`crate::DOCUMENT_FILE`] and uses
    /// this field solely to *reject* a package whose manifest disagrees. It is
    /// never joined onto the package directory — see [`crate::safepath`].
    pub document_path: String,
    /// Whether every asset's bytes live inside the package ("portable
    /// project"). Trivially true for a package with no assets.
    pub assets_collected: bool,
    /// Digest per non-content-addressed file, keyed by package-relative path.
    #[serde(default)]
    pub contents: BTreeMap<String, FileDigest>,
    /// BLAKE3 over the fields above. See the type's note on what this proves.
    #[serde(default)]
    pub integrity: String,
}

impl Manifest {
    /// Digest of everything above `integrity`, in a canonical, order-stable
    /// encoding that does not depend on the JSON writer.
    pub fn compute_integrity(&self) -> String {
        let mut h = blake3::Hasher::new();
        h.update(b"rstudio.package.integrity.v2\n");
        h.update(&self.manifest_version.to_le_bytes());
        h.update(self.app_version.as_bytes());
        h.update(b"\0");
        h.update(self.document_path.as_bytes());
        h.update(b"\0");
        h.update(&[u8::from(self.assets_collected)]);
        h.update(&(self.contents.len() as u64).to_le_bytes());
        // `BTreeMap` iterates in key order, so two manifests with equal
        // contents hash identically regardless of insertion order.
        for (path, d) in &self.contents {
            h.update(path.as_bytes());
            h.update(b"\0");
            h.update(&d.size.to_le_bytes());
            h.update(d.blake3.as_bytes());
            h.update(b"\n");
        }
        hexid::to_hex(h.finalize().as_bytes())
    }

    /// Stamp [`Manifest::integrity`] from the current field values.
    pub fn seal(&mut self) {
        self.integrity = self.compute_integrity();
    }

    /// Whether the recorded digest matches the fields it covers.
    pub fn verify_seal(&self) -> bool {
        // Constant-time comparison is pointless here — there is no secret — but
        // an empty `integrity` must never pass, and an empty string would
        // otherwise compare equal to an empty computed digest if the function
        // ever regressed.
        !self.integrity.is_empty() && self.integrity == self.compute_integrity()
    }
}

impl Default for Manifest {
    fn default() -> Self {
        let mut m = Self {
            manifest_version: MANIFEST_VERSION,
            app_version: crate::UNKNOWN_APP_VERSION.to_string(),
            document_path: crate::DOCUMENT_FILE.to_string(),
            assets_collected: false,
            contents: BTreeMap::new(),
            integrity: String::new(),
        };
        m.seal();
        m
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
        assert_eq!(back, m);
        assert!(back.verify_seal());
    }

    #[test]
    fn the_default_manifest_no_longer_records_this_crates_version_as_the_apps() {
        let m = Manifest::default();
        assert_ne!(
            m.app_version,
            env!("CARGO_PKG_VERSION"),
            "app_version must come from the application, not from project-format"
        );
    }

    #[test]
    fn editing_any_sealed_field_breaks_the_seal() {
        let base = Manifest::default();
        assert!(base.verify_seal());

        let mut path = base.clone();
        path.document_path = "../../etc/passwd".into();
        assert!(!path.verify_seal(), "a rewritten document_path must show");

        let mut app = base.clone();
        app.app_version = "9.9.9".into();
        assert!(!app.verify_seal());

        let mut collected = base.clone();
        collected.assets_collected = !collected.assets_collected;
        assert!(!collected.verify_seal());

        let mut contents = base.clone();
        contents
            .contents
            .insert("document.msgpack".into(), FileDigest::of(b"x"));
        assert!(!contents.verify_seal());

        let mut version = base.clone();
        version.manifest_version += 1;
        assert!(!version.verify_seal());
    }

    #[test]
    fn a_manifest_with_no_integrity_field_does_not_verify() {
        let m = Manifest {
            integrity: String::new(),
            ..Manifest::default()
        };
        assert!(!m.verify_seal(), "an absent digest is not a passing digest");
    }

    #[test]
    fn the_digest_does_not_depend_on_insertion_order() {
        let mut a = Manifest::default();
        a.contents.insert("b".into(), FileDigest::of(b"bb"));
        a.contents.insert("a".into(), FileDigest::of(b"aa"));
        let mut b = Manifest::default();
        b.contents.insert("a".into(), FileDigest::of(b"aa"));
        b.contents.insert("b".into(), FileDigest::of(b"bb"));
        assert_eq!(a.compute_integrity(), b.compute_integrity());
    }

    #[test]
    fn a_file_digest_notices_a_single_flipped_bit() {
        let d = FileDigest::of(b"payload");
        assert!(d.matches(b"payload"));
        assert!(!d.matches(b"payloae"));
        assert!(!d.matches(b"payload "));
    }
}
