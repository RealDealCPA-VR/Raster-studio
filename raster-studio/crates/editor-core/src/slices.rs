//! W11-I: the document's slices — the Slice tool's regions with their
//! Slice Options — saved with the `.rstudio` document and restored with it,
//! as Photopea keeps slices in the file.
//!
//! The editor-core half is only the record: the rectangles and each slice's
//! name, URL and alt text, in slice order. The shell keeps the live set
//! (`app-shell`'s `SliceStore`) and writes it here whenever it changes;
//! opening a document reads it back.

use serde::{Deserialize, Serialize};

/// One slice: a document-space rectangle and its options.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentSlice {
    pub x: i64,
    pub y: i64,
    pub width: u32,
    pub height: u32,
    /// The slice's name (what its exported file is called).
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub url: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub alt: String,
}

#[cfg(test)]
mod tests {
    use super::DocumentSlice;
    use crate::Document;

    fn two_slices() -> Vec<DocumentSlice> {
        vec![
            DocumentSlice {
                x: 0,
                y: 0,
                width: 16,
                height: 8,
                name: "hero".into(),
                url: "https://example.com".into(),
                alt: "Hero banner".into(),
            },
            DocumentSlice {
                x: 16,
                y: 0,
                width: 16,
                height: 8,
                name: "slice_02".into(),
                ..DocumentSlice::default()
            },
        ]
    }

    #[test]
    fn slices_survive_a_save_and_load_of_the_document() {
        let mut doc = Document::new(32, 8, "sliced");
        doc.slices = two_slices();
        let json = serde_json::to_string(&doc).unwrap();
        assert!(json.contains("\"slices\""), "{json}");
        let back: Document = serde_json::from_str(&json).unwrap();
        assert_eq!(back.slices, two_slices());
        assert_eq!(back, doc, "a round trip is an identity");
    }

    #[test]
    fn a_document_without_slices_omits_the_field_and_an_old_file_reads_as_none() {
        let doc = Document::new(8, 8, "plain");
        let json = serde_json::to_string(&doc).unwrap();
        assert!(!json.contains("\"slices\""), "{json}");
        let back: Document = serde_json::from_str(&json).unwrap();
        assert!(back.slices.is_empty());
        // Slices are content: two documents that differ only in them differ.
        let mut sliced = doc.clone();
        sliced.slices = two_slices();
        assert_ne!(sliced, doc);
    }
}
