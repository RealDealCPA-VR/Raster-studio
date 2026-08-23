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
/// * `2` — **never shipped.** No build ever wrote a version-2 document: the
///   change it was allocated for ([`crate::Command::DeleteLayer`]'s inverse
///   becoming `Command::RestoreLayers`, which carries the whole detached
///   subtree instead of a single layer plus a follow-up move) went out in the
///   same release as the version-3 changes below, so the number was consumed
///   without a format ever bearing it. It is listed rather than reused because
///   reusing a version number is how two different formats end up claiming to
///   be the same one.
/// * `3` — pixels became editable. The document gained a [`PixelStore`], the
///   selection gained per-pixel coverage and is now persisted, `Document`
///   gained the active layer, [`crate::Command`] gained `PaintTiles`,
///   `FillRegion` and `ClearRegion`, and [`crate::LayerPatch`] grew to cover
///   the rest of `Layer` (mask, transform, locks, clipping, layer styles).
///   This version also carries the change listed above under `2`:
///   `Command::RestoreLayers` is now the inverse of a delete.
///   Older documents and journals still load: every added field defaults, and
///   no pre-existing variant changed shape (a version-1 journal's delete
///   inverse was a `Transaction` of variants that all still exist and still
///   behave identically). The bump is one-way — a version-3 journal is not
///   readable by version-1 code.
pub const DOCUMENT_FORMAT_VERSION: u32 = 3;

/// Oldest format this build can still read. Everything from here up to
/// [`DOCUMENT_FORMAT_VERSION`] loads without a migration step.
pub const MIN_SUPPORTED_FORMAT_VERSION: u32 = 1;

/// Largest canvas side, in pixels, this build will accept.
///
/// The canvas size is the one number in a document that every downstream stage
/// sizes an allocation from — the compositor's canvas, the presenter's texture,
/// the exporter's buffer — so it is bounded *here*, at the point a document
/// enters the process, rather than at each of them. A file claiming
/// `u32::MAX` per side is rejected before anything tries to serve it.
///
/// `ui::dialogs::new_document::MAX_DIMENSION` is defined *as* this constant, so
/// the New Document dialog cannot describe a document the loader would refuse.
pub const MAX_CANVAS_DIMENSION: u32 = 300_000;

/// Largest canvas area, in pixels, this build will accept.
///
/// [`MAX_CANVAS_DIMENSION`] alone would permit 90 gigapixels; the area cap is
/// what actually keeps a document servable. One gigapixel is 4 GiB of RGBA8 —
/// large, deliberate, and finite.
pub const MAX_CANVAS_PIXELS: u64 = 1_000_000_000;

/// Whether `width` x `height` is a canvas this build will accept.
///
/// A zero-area canvas is *not* rejected: `0` is a legal (if useless) size the
/// document model already carries, and refusing it here would break documents
/// that round-trip one.
pub fn canvas_size_is_supported(width: u32, height: u32) -> bool {
    width <= MAX_CANVAS_DIMENSION
        && height <= MAX_CANVAS_DIMENSION
        && u64::from(width) * u64::from(height) <= MAX_CANVAS_PIXELS
}

/// Rejection of a document that cannot be loaded or a request that would leave
/// it inconsistent.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DocumentError {
    #[error(
        "document format version {found} is outside what this build reads ({min}..={max}); \
         it was written by a newer Raster Studio"
    )]
    UnsupportedFormatVersion { found: u32, min: u32, max: u32 },
    #[error(
        "canvas {width} x {height} is outside what this build can serve \
         (at most {max_dimension} per side and {max_pixels} pixels in total)"
    )]
    CanvasTooLarge {
        width: u32,
        height: u32,
        max_dimension: u32,
        max_pixels: u64,
    },
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
        // The canvas size is an allocation size for every stage downstream of
        // here, and nothing between the file and those stages checks it. A
        // `meta.size` of `[4294967295, 4294967295]` costs one JSON token to
        // write and would otherwise be handed straight to the compositor and
        // the GPU.
        let (width, height) = (r.meta.size.x, r.meta.size.y);
        if !canvas_size_is_supported(width, height) {
            return Err(DocumentError::CanvasTooLarge {
                width,
                height,
                max_dimension: MAX_CANVAS_DIMENSION,
                max_pixels: MAX_CANVAS_PIXELS,
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
///
/// # Requires a self-describing format
/// Because those three fields are omitted conditionally, the field *count*
/// varies with the document's content: a document with pixels and no selection
/// emits `[meta, layers, pixels, ...]`. That is only readable again by a
/// **name-keyed** encoding — `serde_json`, or the `rmp_serde::to_vec_named`
/// that `project-format::save_project` is required to keep using. Handed to a
/// positional encoder (`rmp_serde::to_vec`, `bincode`, `postcard`) the same
/// bytes deserialize by position, and [`DocumentRepr`] reads that [`PixelStore`]
/// into its `selection` field — silent corruption rather than an error.
///
/// So: **do not add a compact serializer for `Document` without first making
/// this impl emit all five fields unconditionally.** The constraint is
/// executable, not just written down here:
/// `the_serialized_form_needs_a_name_keyed_encoding` round-trips a document
/// through `rmp_serde::to_vec_named` and pins the omissions that make the field
/// count content-dependent, so the day this impl becomes position-stable that
/// test fails and points at this note.
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
    fn the_serialized_form_needs_a_name_keyed_encoding() {
        // `Serialize for Document` omits `selection`, `pixels` and
        // `active_layer` when they are empty, so the field count depends on the
        // content. This test is the executable record of what that costs.
        let mut d = Document::new(32, 32, "t");
        let l = Layer::raster("L");
        let id = l.id;
        d.layers.push_root(l).unwrap();
        d.pixels.apply(
            PixelKey::Layer(id),
            &crate::pixels::TileDelta::single(crate::pixels::TileEdit::set(
                raster::TileCoord::new(0, 0, 0),
                raster::TileHash([7; 32]),
            )),
        );
        assert!(d.selection.is_none(), "the field that gets omitted");

        // What `project-format::save_project` does, and the contract it has to
        // keep: named MessagePack round-trips exactly.
        let named = rmp_serde::to_vec_named(&d).unwrap();
        let back: Document = rmp_serde::from_slice(&named).unwrap();
        assert_eq!(back, d, "to_vec_named must be an identity");

        // And here is why it has to be *named*: this document emits three
        // fields, and the third one is `pixels`. A positional encoder writes
        // `[meta, layers, pixels]`; a positional reader assigns that third
        // element to `selection`, which is the next field in `DocumentRepr`.
        // Nothing about that read is loud — it is the wrong document, not an
        // error.
        let json = serde_json::to_string(&d).unwrap();
        assert!(json.contains(r#""pixels""#), "got {json}");
        assert!(
            !json.contains(r#""selection""#) && !json.contains(r#""active_layer""#),
            "the omissions are the whole point of this test: {json}"
        );
        // If these two assertions ever fail, the impl has become
        // position-stable: drop this test and the warning in its doc comment.
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
    fn an_absurd_canvas_size_is_refused_on_load() {
        // `meta.size` is the allocation size every stage downstream reads —
        // the compositor's canvas, the presenter's GPU texture, the exporter's
        // buffer — and nothing between the file and them checked it. Twelve
        // characters of JSON asked the process for 73 exabytes.
        let json = format!(
            r#"{{"meta":{{"format_version":{DOCUMENT_FORMAT_VERSION},"size":[4294967295,4294967295],"color_space":"Srgb","title":"t"}},"layers":{}}}"#,
            serde_json::to_string(&LayerTree::new()).unwrap()
        );
        let err = serde_json::from_str::<Document>(&json)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("outside what this build can serve"),
            "a 4294967295 x 4294967295 canvas was accepted: {err}"
        );

        // The area cap bites even when each side is legal on its own.
        let mut wide = Document::new(MAX_CANVAS_DIMENSION, MAX_CANVAS_DIMENSION, "t");
        let json = serde_json::to_string(&wide).unwrap();
        assert!(
            serde_json::from_str::<Document>(&json).is_err(),
            "{MAX_CANVAS_DIMENSION} squared is 90 gigapixels and must not load"
        );

        // ...and the largest canvas the limits allow still loads, so the check
        // is a bound and not a ban.
        wide.meta.size = UVec2::new(MAX_CANVAS_DIMENSION, 3_333);
        assert!(
            u64::from(MAX_CANVAS_DIMENSION) * 3_333 <= MAX_CANVAS_PIXELS,
            "the fixture must be inside the area cap"
        );
        let json = serde_json::to_string(&wide).unwrap();
        let back: Document = serde_json::from_str(&json).unwrap();
        assert_eq!(back.width(), MAX_CANVAS_DIMENSION);
        assert_eq!(back.height(), 3_333);

        // A zero-area document is legal, and refusing it here would break
        // documents that already round-trip one.
        let empty = Document::new(0, 0, "t");
        let json = serde_json::to_string(&empty).unwrap();
        assert!(serde_json::from_str::<Document>(&json).is_ok());
    }

    #[test]
    fn the_canvas_bound_is_exactly_what_it_says() {
        assert!(canvas_size_is_supported(MAX_CANVAS_DIMENSION, 1));
        assert!(!canvas_size_is_supported(MAX_CANVAS_DIMENSION + 1, 1));
        assert!(!canvas_size_is_supported(1, MAX_CANVAS_DIMENSION + 1));
        assert!(
            canvas_size_is_supported(31_622, 31_622),
            "just under a gigapixel"
        );
        assert!(!canvas_size_is_supported(31_624, 31_624), "just over");
        assert!(canvas_size_is_supported(0, 0));
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
