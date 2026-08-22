//! The `Document` — the authoritative, in-memory state of an open project.

use glam::UVec2;
use serde::{Deserialize, Serialize};

use color::ColorSpace;
use layer_model::LayerTree;

use crate::selection::Selection;

/// Monotonic format version. **Mandatory** — every persisted document records
/// it so `project-format` can run migrations. Bump on any breaking change.
pub const DOCUMENT_FORMAT_VERSION: u32 = 1;

/// Document-level metadata (size, color space, versioning).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentMeta {
    pub format_version: u32,
    /// Canvas size in pixels.
    pub size: UVec2,
    /// Working color space of the document.
    pub color_space: ColorSpace,
    pub title: String,
}

impl DocumentMeta {
    pub fn new(width: u32, height: u32, title: impl Into<String>) -> Self {
        Self {
            format_version: DOCUMENT_FORMAT_VERSION,
            size: UVec2::new(width, height),
            color_space: ColorSpace::Srgb,
            title: title.into(),
        }
    }
}

/// The full editable state of an open project.
///
/// Note: pixel tiles are *not* stored inline here — they live in the asset/tile
/// store and are referenced by hash. This keeps the document cheap to clone for
/// history snapshots and cheap to serialize.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub meta: DocumentMeta,
    pub layers: LayerTree,
    #[serde(skip)]
    pub selection: Selection,
}

impl Document {
    /// Create an empty document of the given size.
    pub fn new(width: u32, height: u32, title: impl Into<String>) -> Self {
        Self {
            meta: DocumentMeta::new(width, height, title),
            layers: LayerTree::new(),
            selection: Selection::None,
        }
    }

    pub fn width(&self) -> u32 {
        self.meta.size.x
    }

    pub fn height(&self) -> u32 {
        self.meta.size.y
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_document_has_format_version() {
        let d = Document::new(1920, 1080, "Untitled");
        assert_eq!(d.meta.format_version, DOCUMENT_FORMAT_VERSION);
        assert_eq!((d.width(), d.height()), (1920, 1080));
        assert!(d.layers.is_empty());
    }

    #[test]
    fn document_serde_roundtrip() {
        let d = Document::new(800, 600, "Test");
        let json = serde_json::to_string(&d).unwrap();
        let back: Document = serde_json::from_str(&json).unwrap();
        assert_eq!(back.meta.size, d.meta.size);
    }
}
