//! Document format gating and migration.
//!
//! `load_project` used to read `manifest.format_version` and then **ignore it**,
//! and `migrate()` was `fn migrate(doc: Document) -> Document { doc }`. Between
//! them, a document claiming any version at all was handed to the editor
//! unexamined.
//!
//! Two things happen here now, in this order:
//!
//! 1. **Gate.** The version is read out of the serialized document *before* the
//!    document itself is decoded, and a version outside
//!    [`MIN_DOCUMENT_VERSION`]`..=`[`MAX_DOCUMENT_VERSION`] is refused by name.
//!    Reading it first matters: it turns "a file from a newer build" into a
//!    sentence the user can act on instead of a msgpack decode error about a
//!    field that did not exist yet.
//! 2. **Migrate.** [`migrate`] walks the [`STEPS`] chain from the file's version
//!    to the current one and stamps the result, so everything downstream of a
//!    load sees exactly one format.
//!
//! # Why the two steps carry no data transformation
//!
//! `editor-core` documents the history: version `2` **was never shipped** (the
//! number was consumed by a change that went out with version 3), and every
//! field version 3 added — the pixel store, the persisted selection, the active
//! layer — has a serde default, so a version-1 document decodes into a
//! version-3 [`Document`] without help.
//!
//! What the 2→3 step does do is a repair rather than a translation: it clears
//! `pixels` and `selection`, because **no pre-version-3 build could write
//! either**. A version-1 document that carries them is damaged or forged, and
//! taking its word for a pixel store means loading tile references that no
//! version-1 package has blobs for.

use editor_core::{Document, DOCUMENT_FORMAT_VERSION, MIN_SUPPORTED_FORMAT_VERSION};
use serde::Deserialize;

use crate::error::ProjectError;

/// Oldest document format this build reads.
pub const MIN_DOCUMENT_VERSION: u32 = MIN_SUPPORTED_FORMAT_VERSION;
/// Newest document format this build reads — the one it writes.
pub const MAX_DOCUMENT_VERSION: u32 = DOCUMENT_FORMAT_VERSION;

/// One migration step: everything at `from` becomes `from + 1`.
struct Step {
    from: u32,
    apply: fn(&mut Document),
}

/// The migration chain, ascending and gapless. Checked by
/// `the_chain_covers_every_supported_version`.
const STEPS: &[Step] = &[
    Step {
        from: 1,
        // Version 2 never existed as an on-disk format; see the module docs.
        apply: |_doc| {},
    },
    Step {
        from: 2,
        apply: |doc| {
            // Neither field existed before version 3. Anything here came from a
            // damaged or hand-edited file, and a pixel store from a package
            // with no `tiles/` directory would just be a list of tiles that
            // cannot be resolved.
            doc.pixels = editor_core::PixelStore::default();
            doc.selection = editor_core::Selection::None;
        },
    },
];

/// Read `meta.format_version` out of a serialized document without decoding the
/// rest of it.
///
/// A struct with one field, so an unknown or newer field elsewhere in the
/// document cannot make the *version probe* fail — which would defeat the point
/// of probing.
#[derive(Deserialize)]
struct VersionProbe {
    meta: MetaProbe,
}

#[derive(Deserialize)]
struct MetaProbe {
    format_version: u32,
}

/// The format version a serialized document declares.
pub fn document_version(bytes: &[u8]) -> Result<u32, ProjectError> {
    let probe: VersionProbe = rmp_serde::from_slice(bytes)?;
    Ok(probe.meta.format_version)
}

/// Refuse a version this build cannot read, by name.
pub fn check_document_version(found: u32) -> Result<(), ProjectError> {
    if !(MIN_DOCUMENT_VERSION..=MAX_DOCUMENT_VERSION).contains(&found) {
        return Err(ProjectError::UnsupportedDocumentVersion {
            found,
            min: MIN_DOCUMENT_VERSION,
            max: MAX_DOCUMENT_VERSION,
        });
    }
    Ok(())
}

/// Bring a document loaded from format version `from` up to the current one.
///
/// Idempotent for a document already at the current version: the chain has no
/// steps left to run and the stamp is a no-op.
pub fn migrate(mut doc: Document, from: u32) -> Result<Document, ProjectError> {
    check_document_version(from)?;
    for step in STEPS.iter().filter(|s| s.from >= from) {
        (step.apply)(&mut doc);
    }
    // One format downstream of a load. Without this, a re-save of a migrated
    // document would write the *old* version number over new-format bytes.
    doc.meta.format_version = MAX_DOCUMENT_VERSION;
    Ok(doc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_core::{PixelKey, TileDelta, TileEdit};
    use raster::{TileCoord, TileHash};

    #[test]
    fn the_chain_covers_every_supported_version() {
        // A gap or a repeat would silently skip a migration.
        let expected: Vec<u32> = (MIN_DOCUMENT_VERSION..MAX_DOCUMENT_VERSION).collect();
        let actual: Vec<u32> = STEPS.iter().map(|s| s.from).collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn a_version_from_the_future_is_refused_by_name() {
        let err = check_document_version(999).unwrap_err();
        assert!(
            matches!(
                err,
                ProjectError::UnsupportedDocumentVersion { found: 999, .. }
            ),
            "{err}"
        );
        assert!(check_document_version(0).is_err(), "0 is nobody's version");
        for v in MIN_DOCUMENT_VERSION..=MAX_DOCUMENT_VERSION {
            check_document_version(v).unwrap();
        }
    }

    #[test]
    fn the_version_probe_reads_a_real_serialized_document() {
        let doc = Document::new(8, 8, "t");
        let bytes = rmp_serde::to_vec_named(&doc).unwrap();
        assert_eq!(document_version(&bytes).unwrap(), MAX_DOCUMENT_VERSION);
    }

    #[test]
    fn the_probe_ignores_everything_but_the_version() {
        // The probe has to survive a document whose *other* fields it does not
        // understand, or a file from a newer build would produce a decode error
        // instead of the version rejection the user needs to read.
        let value = serde_json::json!({
            "meta": { "format_version": 42, "unknown_meta_field": true },
            "layers": {"layers": {}, "root": []},
            "something_from_the_future": [1, 2, 3],
        });
        let bytes = rmp_serde::to_vec_named(&value).unwrap();
        assert_eq!(document_version(&bytes).unwrap(), 42);
    }

    #[test]
    fn migrating_stamps_the_current_version() {
        let mut doc = Document::new(8, 8, "t");
        doc.meta.format_version = 1;
        let out = migrate(doc, 1).unwrap();
        assert_eq!(out.meta.format_version, MAX_DOCUMENT_VERSION);
    }

    #[test]
    fn migrating_is_an_identity_for_a_current_document() {
        let mut doc = Document::new(8, 8, "t");
        let layer = layer_model::Layer::raster("L");
        let id = layer.id;
        doc.layers.push_root(layer).unwrap();
        doc.pixels.apply(
            PixelKey::Layer(id),
            &TileDelta::single(TileEdit::set(TileCoord::new(0, 0, 0), TileHash([5; 32]))),
        );
        let before = doc.clone();
        let after = migrate(doc, MAX_DOCUMENT_VERSION).unwrap();
        assert_eq!(after, before, "a current document must not be rewritten");
    }

    #[test]
    fn a_version_one_document_cannot_smuggle_in_a_pixel_store() {
        // Version 1 had no `pixels` field, so a version-1 document that carries
        // one is not a version-1 document. Loading it would reference tile
        // blobs a version-1 package never had.
        let mut doc = Document::new(8, 8, "t");
        let layer = layer_model::Layer::raster("L");
        let id = layer.id;
        doc.layers.push_root(layer).unwrap();
        doc.pixels.apply(
            PixelKey::Layer(id),
            &TileDelta::single(TileEdit::set(TileCoord::new(0, 0, 0), TileHash([9; 32]))),
        );
        doc.selection = editor_core::Selection::Rect {
            min: glam::IVec2::new(1, 1),
            max: glam::IVec2::new(4, 4),
        };
        assert!(!doc.pixels.is_empty());

        let out = migrate(doc, 1).unwrap();
        assert!(out.pixels.is_empty(), "a v1 pixel store must be dropped");
        assert!(out.selection.is_none());
        assert_eq!(out.layers.len(), 1, "the layers themselves survive");
    }
}
