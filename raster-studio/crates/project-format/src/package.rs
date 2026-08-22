//! Atomic package read/write and format migration entry point.

use std::path::{Path, PathBuf};

use editor_core::Document;

use crate::manifest::{Manifest, MANIFEST_VERSION};

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("msgpack encode error: {0}")]
    MsgpackEncode(#[from] rmp_serde::encode::Error),
    #[error("msgpack decode error: {0}")]
    MsgpackDecode(#[from] rmp_serde::decode::Error),
    #[error("unsupported manifest version {found} (max supported {supported})")]
    UnsupportedVersion { found: u32, supported: u32 },
    #[error("not a .rstudio package: {0}")]
    NotAPackage(String),
}

/// Save `doc` to the package directory at `path`, atomically.
///
/// Strategy: write the whole package to a sibling temp dir, fsync files, then
/// rename it over the destination. A partially-written package can never
/// replace a good one.
pub fn save_project(path: &Path, doc: &Document) -> Result<(), ProjectError> {
    let tmp = temp_sibling(path);
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp)?;
    }
    std::fs::create_dir_all(&tmp)?;
    std::fs::create_dir_all(tmp.join("previews"))?;
    std::fs::create_dir_all(tmp.join("tiles"))?;
    std::fs::create_dir_all(tmp.join("assets"))?;
    std::fs::create_dir_all(tmp.join("ai"))?;

    // Manifest.
    let manifest = Manifest::default();
    let manifest_json = serde_json::to_vec_pretty(&manifest)?;
    write_and_sync(&tmp.join("manifest.json"), &manifest_json)?;

    // Document (MessagePack — compact, versioned via DocumentMeta.format_version).
    let doc_bytes = rmp_serde::to_vec_named(doc)?;
    write_and_sync(&tmp.join(&manifest.document_path), &doc_bytes)?;

    // Empty journal placeholder (appended to at runtime).
    write_and_sync(&tmp.join("commands.journal"), b"")?;

    // Atomic swap.
    if path.exists() {
        let old = temp_sibling_named(path, "old");
        std::fs::rename(path, &old)?;
        match std::fs::rename(&tmp, path) {
            Ok(()) => {
                let _ = std::fs::remove_dir_all(&old);
            }
            Err(e) => {
                // Roll back.
                let _ = std::fs::rename(&old, path);
                return Err(e.into());
            }
        }
    } else {
        std::fs::rename(&tmp, path)?;
    }
    Ok(())
}

/// Load a project package, running migrations if the manifest is older.
pub fn load_project(path: &Path) -> Result<Document, ProjectError> {
    let manifest_path = path.join("manifest.json");
    if !manifest_path.exists() {
        return Err(ProjectError::NotAPackage(path.display().to_string()));
    }
    let manifest: Manifest = serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
    if manifest.manifest_version > MANIFEST_VERSION {
        return Err(ProjectError::UnsupportedVersion {
            found: manifest.manifest_version,
            supported: MANIFEST_VERSION,
        });
    }

    let doc_bytes = std::fs::read(path.join(&manifest.document_path))?;
    let doc: Document = rmp_serde::from_slice(&doc_bytes)?;
    Ok(migrate(doc))
}

/// Apply in-memory document migrations. Currently a no-op passthrough; add
/// per-version steps here keyed on `doc.meta.format_version`.
fn migrate(doc: Document) -> Document {
    doc
}

fn write_and_sync(path: &Path, bytes: &[u8]) -> Result<(), ProjectError> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)?;
    f.write_all(bytes)?;
    f.flush()?;
    f.sync_all()?;
    Ok(())
}

fn temp_sibling(path: &Path) -> PathBuf {
    temp_sibling_named(path, "tmp")
}

fn temp_sibling_named(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_default();
    name.push(format!(".{suffix}"));
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use layer_model::Layer;

    #[test]
    fn save_then_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path().join("MyProject.rstudio");

        let mut doc = Document::new(1920, 1080, "MyProject");
        doc.layers.push_root(Layer::raster("Background")).unwrap();
        doc.layers.push_root(Layer::group("Group")).unwrap();

        save_project(&pkg, &doc).unwrap();
        assert!(pkg.join("manifest.json").exists());
        assert!(pkg.join("document.msgpack").exists());

        let loaded = load_project(&pkg).unwrap();
        assert_eq!(loaded.meta.size, doc.meta.size);
        assert_eq!(loaded.layers.len(), 2);
    }

    #[test]
    fn atomic_overwrite_preserves_on_second_save() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path().join("P.rstudio");
        let doc = Document::new(100, 100, "P");
        save_project(&pkg, &doc).unwrap();
        // Second save should succeed via the temp+rename path.
        save_project(&pkg, &doc).unwrap();
        assert!(load_project(&pkg).is_ok());
    }

    #[test]
    fn rejects_non_package() {
        let dir = tempfile::tempdir().unwrap();
        let err = load_project(dir.path());
        assert!(matches!(err, Err(ProjectError::NotAPackage(_))));
    }
}
