//! The `Document` — the authoritative, in-memory state of an open project.

use std::path::{Path, PathBuf};

use glam::UVec2;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize, Serializer};

use color::ColorSpace;
use layer_model::{LayerId, LayerTree};

use crate::pixels::{PixelKey, PixelStore, TileMap};
use crate::selection::Selection;

/// Monotonic format version. **Mandatory** — every persisted document records
/// it so `project-format` can run migrations. Bump on any breaking change.
///
/// # History
///
/// * `1` — initial.
/// * `2` — [`crate::Command::DeleteLayer`]'s inverse became
///   `Command::RestoreLayers`, carrying the whole detached subtree instead of a
///   single layer plus a follow-up move. A version-1 journal still replays
///   unchanged (its inverse was a `Transaction` of variants that all still
///   exist and still behave identically), so no migration step is needed; the
///   bump exists because a version-2 journal is *not* readable by version-1
///   code.
/// * `3` — pixels became editable. The document gained a [`PixelStore`], the
///   selection gained per-pixel coverage and is now persisted, `Document`
///   gained the active layer, [`crate::Command`] gained `PaintTiles`,
///   `FillRegion` and `ClearRegion`, and [`crate::LayerPatch`] grew to cover
///   the rest of `Layer` (mask, transform, locks, clipping, layer styles).
///   Older documents and journals still load: every added field defaults, and
///   no pre-existing variant changed shape.
pub const DOCUMENT_FORMAT_VERSION: u32 = 3;

/// Oldest format this build can still read. Everything from here up to
/// [`DOCUMENT_FORMAT_VERSION`] loads without a migration step.
pub const MIN_SUPPORTED_FORMAT_VERSION: u32 = 1;

/// Rejection of a document that cannot be loaded or a request that would leave
/// it inconsistent.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DocumentError {
    #[error(
        "document format version {found} is outside what this build reads ({min}..={max}); \
         it was written by a newer Raster Studio"
    )]
    UnsupportedFormatVersion { found: u32, min: u32, max: u32 },
    #[error("layer {0} is not in this document")]
    LayerNotFound(LayerId),
}

/// Document-level metadata (size, color space, versioning).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

/// Deserialization shadow of [`Document`]. Exists so `TryFrom` can gate the
/// format version *before* the value escapes into the editor, and so every
/// field added after version 1 can default.
#[derive(Deserialize)]
struct DocumentRepr {
    meta: DocumentMeta,
    #[serde(default)]
    layers: LayerTree,
    #[serde(default)]
    selection: Selection,
    #[serde(default)]
    pixels: PixelStore,
    #[serde(default)]
    active_layer: Option<LayerId>,
}

impl TryFrom<DocumentRepr> for Document {
    type Error = DocumentError;

    fn try_from(r: DocumentRepr) -> Result<Self, Self::Error> {
        let found = r.meta.format_version;
        if !(MIN_SUPPORTED_FORMAT_VERSION..=DOCUMENT_FORMAT_VERSION).contains(&found) {
            return Err(DocumentError::UnsupportedFormatVersion {
                found,
                min: MIN_SUPPORTED_FORMAT_VERSION,
                max: DOCUMENT_FORMAT_VERSION,
            });
        }
        // A stale active layer is dropped rather than fatal: it is a cursor,
        // not content, and refusing the file over it would cost the user the
        // whole document.
        let active_layer = r.active_layer.filter(|id| r.layers.contains(*id));
        Ok(Document {
            meta: r.meta,
            layers: r.layers,
            selection: r.selection,
            pixels: r.pixels,
            active_layer,
            dirty: false,
            path: None,
        })
    }
}

/// The full editable state of an open project.
///
/// Note: pixel tiles are *not* stored inline here — they live in the tile store
/// and are referenced by content hash through [`Document::pixels`]. This keeps
/// the document cheap to clone for history snapshots and cheap to serialize.
///
/// # Equality
/// `PartialEq` compares *document content*: metadata, layers, pixel
/// references, selection, and the active layer. It deliberately ignores
/// [`Document::is_dirty`] and [`Document::path`], which describe this editing
/// session rather than the document, and are not persisted either.
///
/// It is implemented by hand because [`LayerTree`] does not derive `PartialEq`;
/// the tree is compared through its depth-first order plus each [`Layer`]
/// (`layer_model::Layer` does derive it, and a group's `children` list lives on
/// the layer, so structure is covered).
///
/// # One view of the active layer
/// The `active_layer` field can hold an id whose layer has since left the tree —
/// nothing clears it when a layer is deleted, because the deletion is undoable
/// and the cursor should survive the undo. Equality and serialization therefore
/// both read it through [`Document::active_layer`], the same filtered accessor
/// every consumer uses, rather than through the raw field. Comparing the raw
/// field would make a document with a deleted active layer unequal to itself
/// after a save/load round trip, which would quietly weaken the
/// `assert_eq!(doc, before)` oracle that every atomicity test in this crate
/// depends on.
///
/// [`Layer`]: layer_model::Layer
#[derive(Debug, Clone, Deserialize)]
#[serde(try_from = "DocumentRepr")]
pub struct Document {
    pub meta: DocumentMeta,
    pub layers: LayerTree,
    /// The active selection. Persisted — see [`Selection`] — but omitted from
    /// the serialized form while there is no selection at all.
    ///
    /// The predicate is `is_none`, not `is_empty`: an empty-but-present
    /// selection means "no pixel is selected", while an absent one means "every
    /// pixel is", so dropping the empty case would change the meaning of the
    /// next fill across a save.
    pub selection: Selection,
    /// Content hashes of every layer's and mask's pixels. Omitted while empty
    /// so a vector-only document costs nothing for it.
    pub pixels: PixelStore,
    /// Raw cursor. Private, and read through [`Document::active_layer`], which
    /// filters an id whose layer has left the tree. Equality and serialization
    /// go through that same accessor — see the type's "One view of the active
    /// layer" note.
    active_layer: Option<LayerId>,
    /// Unsaved-changes flag. Session state, not document content: never
    /// serialized, and a freshly loaded document is clean.
    dirty: bool,
    /// Where this document was loaded from or last saved to. Not serialized —
    /// a file that records its own location is wrong the moment it is moved;
    /// the loader sets it.
    path: Option<PathBuf>,
}

/// Written by hand rather than derived so the active layer is serialized
/// through [`Document::active_layer`] — the filtered view — instead of the raw
/// field. A stale cursor is dropped on the way out exactly as it is dropped on
/// the way in ([`TryFrom<DocumentRepr>`]), which is what makes a save/load round
/// trip an identity on `PartialEq`.
///
/// `selection` and `pixels` are omitted while they carry nothing, and `dirty`
/// and `path` are session state and are never written.
impl Serialize for Document {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let selection = (!self.selection.is_none()).then_some(&self.selection);
        let pixels = (!self.pixels.is_empty()).then_some(&self.pixels);
        let active_layer = self.active_layer();
        let fields = 2
            + usize::from(selection.is_some())
            + usize::from(pixels.is_some())
            + usize::from(active_layer.is_some());
        let mut s = serializer.serialize_struct("Document", fields)?;
        s.serialize_field("meta", &self.meta)?;
        s.serialize_field("layers", &self.layers)?;
        if let Some(selection) = selection {
            s.serialize_field("selection", selection)?;
        }
        if let Some(pixels) = pixels {
            s.serialize_field("pixels", pixels)?;
        }
        if let Some(active_layer) = active_layer {
            s.serialize_field("active_layer", &active_layer)?;
        }
        s.end()
    }
}

impl PartialEq for Document {
    fn eq(&self, other: &Self) -> bool {
        if self.meta != other.meta
            || self.selection != other.selection
            || self.pixels != other.pixels
            // Through the accessor, not the field: a cursor left pointing at a
            // deleted layer reads as "no active layer" everywhere else, so it
            // must here too.
            || self.active_layer() != other.active_layer()
            || self.layers.len() != other.layers.len()
            || self.layers.root() != other.layers.root()
        {
            return false;
        }
        let order = self.layers.iter_depth_first();
        if order != other.layers.iter_depth_first() {
            return false;
        }
        order
            .into_iter()
            .all(|id| self.layers.get(id) == other.layers.get(id))
    }
}

impl Document {
    /// Create an empty document of the given size.
    pub fn new(width: u32, height: u32, title: impl Into<String>) -> Self {
        Self {
            meta: DocumentMeta::new(width, height, title),
            layers: LayerTree::new(),
            selection: Selection::None,
            pixels: PixelStore::default(),
            active_layer: None,
            dirty: false,
            path: None,
        }
    }

    pub fn width(&self) -> u32 {
        self.meta.size.x
    }

    pub fn height(&self) -> u32 {
        self.meta.size.y
    }

    /// The layer tools and panels act on, if any.
    ///
    /// Always a layer that is currently in the tree: an id that has been
    /// deleted (or undone away) resolves to `None` rather than to a dangling
    /// reference.
    pub fn active_layer(&self) -> Option<LayerId> {
        self.active_layer.filter(|id| self.layers.contains(*id))
    }

    /// Make `id` the active layer, or clear the active layer with `None`.
    ///
    /// Refuses an id that is not in the tree — silently accepting one would
    /// hand every tool a target that does not exist.
    pub fn set_active_layer(&mut self, id: Option<LayerId>) -> Result<(), DocumentError> {
        if let Some(id) = id {
            if !self.layers.contains(id) {
                return Err(DocumentError::LayerNotFound(id));
            }
        }
        self.active_layer = id;
        Ok(())
    }

    /// `true` when the document has changes that are not on disk.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Record that the document has changed. [`crate::History`] calls this on
    /// every successful apply, undo, and redo.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Record that the document matches what is on disk. `project-format`
    /// calls this after a successful save.
    pub fn mark_saved(&mut self) {
        self.dirty = false;
    }

    /// Where the document lives on disk, if it has ever been saved or loaded.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Record the document's location. Does not touch the dirty flag: saving to
    /// a new path is a save (`mark_saved`), opening a file is not.
    pub fn set_path(&mut self, path: Option<PathBuf>) {
        self.path = path;
    }

    /// Tile references of one layer's own pixels.
    pub fn layer_tiles(&self, layer: LayerId) -> Option<&TileMap> {
        self.pixels.tiles(PixelKey::Layer(layer))
    }

    /// Tile references of the mask attached to a layer.
    pub fn mask_tiles(&self, layer: LayerId) -> Option<&TileMap> {
        let mask = self.layers.get(layer)?.mask_id()?;
        self.pixels.tiles(PixelKey::Mask(mask))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use layer_model::Layer;

    #[test]
    fn new_document_has_format_version() {
        let d = Document::new(1920, 1080, "Untitled");
        assert_eq!(d.meta.format_version, DOCUMENT_FORMAT_VERSION);
        assert_eq!((d.width(), d.height()), (1920, 1080));
        assert!(d.layers.is_empty());
        assert!(!d.is_dirty());
        assert!(d.path().is_none());
        assert!(d.active_layer().is_none());
    }

    #[test]
    fn document_serde_roundtrip() {
        let mut d = Document::new(800, 600, "Test");
        let l = Layer::raster("L");
        let id = l.id;
        d.layers.push_root(l).unwrap();
        d.set_active_layer(Some(id)).unwrap();
        d.selection = Selection::Rect {
            min: glam::IVec2::new(1, 1),
            max: glam::IVec2::new(9, 9),
        };

        let json = serde_json::to_string(&d).unwrap();
        let back: Document = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d, "content must survive the round trip");
        assert_eq!(back.active_layer(), Some(id));
        assert_eq!(back.selection, d.selection);
    }

    #[test]
    fn a_selection_is_not_lost_on_save() {
        // It used to be `#[serde(skip)]`: every lasso died at save time.
        let mut d = Document::new(64, 64, "t");
        d.selection = Selection::Mask(
            crate::selection::SelectionMask::filled(glam::IVec2::new(4, 4), 8, 8).unwrap(),
        );
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains("selection"), "got {json}");
        let back: Document = serde_json::from_str(&json).unwrap();
        assert_eq!(back.selection, d.selection);
        assert_eq!(back.selection.bounds(), d.selection.bounds());
    }

    #[test]
    fn having_no_selection_costs_nothing_on_disk() {
        let d = Document::new(64, 64, "t");
        assert!(d.selection.is_none());
        let json = serde_json::to_string(&d).unwrap();
        assert!(!json.contains("selection"), "got {json}");
        assert!(!json.contains("pixels"), "got {json}");
    }

    #[test]
    fn an_empty_selection_does_not_come_back_as_no_selection() {
        // It used to be skipped by `is_empty`, so an empty marquee reloaded as
        // `Selection::None` and `coverage_at` flipped from 0.0 to 1.0: the next
        // fill would paint the whole layer instead of nothing.
        let probe = glam::IVec2::new(5, 5);
        for empty in [
            Selection::Rect {
                min: probe,
                max: probe,
            },
            Selection::Mask(crate::selection::SelectionMask::new(probe, 2, 2, vec![0; 4]).unwrap()),
        ] {
            let mut d = Document::new(64, 64, "t");
            d.selection = empty.clone();
            assert_eq!(d.selection.coverage_at(probe), 0.0);

            let json = serde_json::to_string(&d).unwrap();
            assert!(json.contains("selection"), "got {json}");
            let back: Document = serde_json::from_str(&json).unwrap();
            assert_eq!(back.selection, empty);
            assert_eq!(
                back.selection.coverage_at(probe),
                d.selection.coverage_at(probe),
                "coverage must not change meaning across a save"
            );
            assert_eq!(back, d);
        }
    }

    #[test]
    fn a_document_from_a_newer_build_is_refused_instead_of_half_read() {
        let mut d = Document::new(10, 10, "t");
        d.meta.format_version = DOCUMENT_FORMAT_VERSION + 1;
        let json = serde_json::to_string(&d).unwrap();
        let err = serde_json::from_str::<Document>(&json).unwrap_err();
        assert!(
            err.to_string().contains("newer Raster Studio"),
            "expected a version rejection, got {err}"
        );

        // Version 0 is not a version anyone wrote.
        d.meta.format_version = 0;
        let json = serde_json::to_string(&d).unwrap();
        assert!(serde_json::from_str::<Document>(&json).is_err());
    }

    #[test]
    fn every_supported_version_still_loads() {
        for v in MIN_SUPPORTED_FORMAT_VERSION..=DOCUMENT_FORMAT_VERSION {
            let mut d = Document::new(10, 10, "t");
            d.meta.format_version = v;
            let json = serde_json::to_string(&d).unwrap();
            let back: Document = serde_json::from_str(&json).unwrap();
            assert_eq!(back.meta.format_version, v);
        }
    }

    #[test]
    fn the_active_layer_can_only_name_a_live_layer() {
        let mut d = Document::new(10, 10, "t");
        let l = Layer::raster("L");
        let id = l.id;
        d.layers.push_root(l).unwrap();
        d.set_active_layer(Some(id)).unwrap();
        assert_eq!(d.active_layer(), Some(id));

        let ghost = LayerId::new();
        assert_eq!(
            d.set_active_layer(Some(ghost)).unwrap_err(),
            DocumentError::LayerNotFound(ghost)
        );
        assert_eq!(d.active_layer(), Some(id), "the refusal changed nothing");

        // Deleting the layer leaves the cursor pointing at nothing, which must
        // read as "no active layer", not as a dangling id.
        d.layers.remove(id).unwrap();
        assert_eq!(d.active_layer(), None);

        d.set_active_layer(None).unwrap();
        assert_eq!(d.active_layer(), None);
    }

    #[test]
    fn a_document_whose_active_layer_was_deleted_round_trips_equal_to_itself() {
        // The raw field keeps naming the deleted layer (the deletion is
        // undoable, so the cursor is not cleared), while every reader answers
        // `None`. Equality and serialization used to read the *field*, so this
        // document was not equal to itself after a save — which silently
        // weakens every `assert_eq!(doc, before)` atomicity oracle.
        let mut d = Document::new(10, 10, "t");
        let l = Layer::raster("L");
        let id = l.id;
        d.layers.push_root(l).unwrap();
        d.set_active_layer(Some(id)).unwrap();
        d.layers.remove(id).unwrap();
        assert_eq!(d.active_layer(), None, "the accessor filters the stale id");

        let json = serde_json::to_string(&d).unwrap();
        assert!(
            !json.contains("active_layer"),
            "a stale cursor must not be written out: {json}"
        );
        let back: Document = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d, "a save/load round trip must be an identity");
        assert_eq!(back.active_layer(), None);

        // And equality itself compares the filtered view: a document that never
        // had an active layer is equal to one whose active layer is gone.
        let fresh = Document::new(10, 10, "t");
        assert_eq!(d, fresh);
    }

    #[test]
    fn a_live_active_layer_is_still_written_and_read_back() {
        // The other side of the filter: a cursor pointing at a live layer must
        // survive the round trip, or the filtered view would be a way to lose
        // it.
        let mut d = Document::new(10, 10, "t");
        let l = Layer::raster("L");
        let id = l.id;
        d.layers.push_root(l).unwrap();
        d.set_active_layer(Some(id)).unwrap();

        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains("active_layer"), "got {json}");
        let back: Document = serde_json::from_str(&json).unwrap();
        assert_eq!(back.active_layer(), Some(id));
        assert_eq!(back, d);
    }

    #[test]
    fn a_stale_active_layer_is_dropped_on_load_rather_than_failing_the_file() {
        let mut d = Document::new(10, 10, "t");
        let l = Layer::raster("L");
        let id = l.id;
        d.layers.push_root(l).unwrap();
        d.set_active_layer(Some(id)).unwrap();
        let json = serde_json::to_string(&d).unwrap();
        // Strip the layer out of the serialized tree, keeping the cursor.
        let mut value: serde_json::Value = serde_json::from_str(&json).unwrap();
        value["layers"]["layers"] = serde_json::json!({});
        value["layers"]["root"] = serde_json::json!([]);
        let back: Document = serde_json::from_value(value).unwrap();
        assert_eq!(back.active_layer(), None);
    }

    #[test]
    fn dirty_and_path_are_session_state() {
        let mut d = Document::new(10, 10, "t");
        d.mark_dirty();
        d.set_path(Some(PathBuf::from("/tmp/x.rsp")));
        assert!(d.is_dirty());
        assert_eq!(d.path(), Some(Path::new("/tmp/x.rsp")));
        d.mark_saved();
        assert!(!d.is_dirty());

        // Neither crosses a save/load boundary, and neither takes part in
        // equality.
        let mut dirty = d.clone();
        dirty.mark_dirty();
        assert_eq!(dirty, d);
        let json = serde_json::to_string(&dirty).unwrap();
        let back: Document = serde_json::from_str(&json).unwrap();
        assert!(!back.is_dirty());
        assert!(back.path().is_none());
    }

    #[test]
    fn equality_sees_every_content_field() {
        let base = Document::new(10, 10, "t");

        let mut size = base.clone();
        size.meta.size.x = 11;
        assert_ne!(size, base);

        let mut layers = base.clone();
        layers.layers.push_root(Layer::raster("L")).unwrap();
        assert_ne!(layers, base);

        let mut renamed = layers.clone();
        renamed
            .layers
            .get_mut(renamed.layers.root()[0])
            .unwrap()
            .name = "other".into();
        assert_ne!(renamed, layers, "a layer's own fields must count");

        let mut active = layers.clone();
        active
            .set_active_layer(Some(active.layers.root()[0]))
            .unwrap();
        assert_ne!(active, layers);

        let mut sel = base.clone();
        sel.selection = Selection::Rect {
            min: glam::IVec2::ZERO,
            max: glam::IVec2::new(2, 2),
        };
        assert_ne!(sel, base);

        let mut px = base.clone();
        px.pixels.apply(
            PixelKey::Layer(LayerId::new()),
            &crate::pixels::TileDelta::single(crate::pixels::TileEdit::set(
                raster::TileCoord::new(0, 0, 0),
                raster::TileHash([1; 32]),
            )),
        );
        assert_ne!(px, base);

        assert_eq!(base.clone(), base);
    }
}
