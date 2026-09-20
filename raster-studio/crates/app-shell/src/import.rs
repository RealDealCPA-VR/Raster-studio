//! Turning a decoded image into a document the editor can actually edit.
//!
//! # The bug this module exists to fix
//!
//! The Wave-0 shell created `Document::new(w, h, "Raster Studio")` — **zero
//! layers** — and held the opened picture separately as a loose GPU texture.
//! So the layers panel said "No layers yet" while a photograph filled the
//! window, adding a layer changed nothing on screen, and no tool could touch a
//! single pixel of the thing the user had opened. The image was not part of the
//! document at all.
//!
//! Here the image becomes exactly what any other raster content is: a
//! [`layer_model::Layer::raster`] whose pixels live in the tile store and are
//! referenced from the document by content hash. Everything downstream —
//! compositing, saving, undo, the brush — then works on it without knowing it
//! came from a file.
//!
//! # Why it is one transaction
//!
//! Creating the layer and filling it are a single
//! [`Command::Transaction`], so opening an image is one history entry: undoing
//! an import removes the layer *and* its pixels, and cannot leave an empty
//! layer behind.

use std::io::Read;
use std::path::Path;

use compositor::{MemoryTileSource, TileSource};
use editor_core::pixels::{PixelKey, PixelTarget, TileDelta, TileEdit, TileMap};
use editor_core::{Command, Document, History, MASK_TILE_BYTES};
use layer_model::{
    AdjustmentKind, BlendMode, ClippingMode, GroupBlending, GroupLayer, Layer, LayerId, LayerKind,
    LayerMask, LockState, MaskId, MaskKind, TextLayer,
};
use raster::{PixelFormat, TileCoord, TileGrid, TILE_SIZE};
use tools::text::{
    DEFAULT_FONT_FAMILY as TEXT_DEFAULT_FAMILY, DEFAULT_SIZE_PX as TEXT_DEFAULT_SIZE,
};

/// A decoded image on its way into a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    /// Row-major RGBA8, straight alpha.
    pub rgba8: Vec<u8>,
    /// The colour space the pixels are encoded in — `IccProfile` when the
    /// file carried a profile. Dropped at the app-shell boundary until this
    /// wave, which is why tagged files composited as sRGB.
    pub color_space: color::ColorSpace,
    /// The raw embedded ICC profile, if the file had one.
    pub icc_profile: Option<Vec<u8>>,
}

impl DecodedImage {
    /// Read and decode a file through the `raster` codec facade.
    pub fn decode_path(path: &Path) -> Result<DecodedImage, ImportError> {
        let decoded = raster::decode_path(path)?;
        Ok(DecodedImage {
            width: decoded.width,
            height: decoded.height,
            rgba8: decoded.rgba8,
            color_space: decoded.color_space,
            icc_profile: decoded.icc_profile,
        })
    }

    /// The name to give the layer and the document, taken from the file name.
    pub fn title_for(path: &Path) -> String {
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled".to_string())
    }
}

/// Why an image could not become a document.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error(transparent)]
    Decode(#[from] raster::CodecError),
    #[error("an image must have a non-zero width and height, got {width}x{height}")]
    EmptyImage { width: u32, height: u32 },
    #[error("the image does not hold {expected} bytes of RGBA8 ({found} found)")]
    PixelCount { expected: usize, found: usize },
    /// Card 047: the source's ICC profile cannot be transformed (not a
    /// matrix-shaper profile, or its bytes are missing). Placement refuses
    /// rather than silently treating the pixels as sRGB — the fallback the
    /// infallible entry points take is exactly what this error exists to
    /// surface.
    #[error("this image's colour profile ({name}) cannot be converted into the working space")]
    UnsupportedColorProfile { name: &'static str },
    #[error(transparent)]
    Grid(#[from] raster::GridError),
    #[error("building the import command failed: {0}")]
    Command(#[from] editor_core::CommandError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// The `.psd` is damaged, hostile, or in a variant this build does not read.
    #[error("this Photoshop document could not be read: {0}")]
    Psd(#[from] psd::PsdError),
    /// The file is larger than [`MAX_PSD_FILE_BYTES`]. A `.psd` is parsed as a
    /// whole buffer — its section offsets are not forward-only — so the file's
    /// own size is an allocation before any of the bounds the `psd` crate
    /// applies to what is *inside* it.
    #[error("this Photoshop document is {bytes} bytes, more than the {max} this build will read")]
    PsdTooLarge { bytes: u64, max: u64 },
    /// A canvas neither side can serve: zero-area, or past what a `.psd` can
    /// describe (30 000 a side — beyond that the format is `.psb`) or what this
    /// build will open.
    #[error("a {width}x{height} canvas cannot be exchanged as a .psd")]
    PsdCanvas { width: u32, height: u32 },
    /// A layer tree nested deeper than [`MAX_PSD_GROUP_DEPTH`].
    #[error("this document nests groups more than {max} deep, which a .psd cannot describe")]
    PsdTooDeep { max: usize },
    #[error("the layer tree could not be built: {0}")]
    Tree(#[from] layer_model::TreeError),
    /// Card 078: rendering a text/shape/smart-object layer's fallback pixels
    /// through the compositor failed.
    #[error(transparent)]
    Composite(#[from] compositor::CompositeError),
}

/// A document, its history, and the tile bytes its pixels live in.
///
/// These three travel together everywhere: the document holds hashes, so it is
/// meaningless without the source that resolves them.
#[derive(Debug)]
pub struct ImportedDocument {
    pub document: Document,
    pub history: History,
    pub tiles: MemoryTileSource,
    /// The raster layer the image became.
    pub layer: LayerId,
}

/// Build the command that adds `image` to `doc` as one raster layer, storing
/// its tiles into `tiles`.
///
/// Separated from [`document_from_image`] because this is also how an image is
/// imported into a document that already has content ("Place…"): the caller
/// runs the returned command through its own [`History`].
pub fn import_command(
    image: &DecodedImage,
    name: &str,
    tiles: &mut MemoryTileSource,
) -> Result<(Command, LayerId), ImportError> {
    if image.width == 0 || image.height == 0 {
        return Err(ImportError::EmptyImage {
            width: image.width,
            height: image.height,
        });
    }
    let expected = (image.width as usize)
        .saturating_mul(image.height as usize)
        .saturating_mul(4);
    if image.rgba8.len() != expected {
        return Err(ImportError::PixelCount {
            expected,
            found: image.rgba8.len(),
        });
    }

    let grid = TileGrid::from_rgba8(image.width, image.height, &image.rgba8)?;
    let layer = Layer::raster(name);
    let layer_id = layer.id;

    let mut edits = Vec::with_capacity(grid.len());
    for (coord, tile) in grid.iter() {
        debug_assert_eq!(tile.format(), PixelFormat::Rgba8);
        let hash = tiles.insert_bytes(tile.data().to_vec());
        edits.push(TileEdit::set(coord, hash));
    }

    let paint = Command::PaintTiles {
        target: PixelTarget::Layer(layer_id),
        delta: TileDelta::new(edits).map_err(editor_core::CommandError::from)?,
    };
    let command = Command::Transaction {
        label: format!("Open {name}"),
        // Order matters: the layer has to exist before its pixels can be
        // addressed. A transaction applies its members in order and rolls the
        // whole thing back if any one fails.
        commands: vec![Command::create_layer(layer), paint],
    };
    Ok((command, layer_id))
}

/// Build a whole document from one image: canvas the size of the image, one
/// raster layer holding it, that layer active.
pub fn document_from_image(
    image: &DecodedImage,
    title: &str,
    history_depth: usize,
) -> Result<ImportedDocument, ImportError> {
    let mut tiles = MemoryTileSource::new();
    let (command, layer) = import_command(image, title, &mut tiles)?;

    let mut document = Document::new(image.width, image.height, title);
    // A tagged file's colour space travels with it: the compositor applies
    // it (see `OpenDocument::icc_profile`) and the export path re-tags.
    document.meta.color_space = image.color_space.clone();
    let mut history = History::with_limit(history_depth);
    history.apply(&mut document, command)?;
    document
        .set_active_layer(Some(layer))
        .expect("the layer was just created in this document");
    // Opening a file is not an edit: the document on screen matches the file on
    // disk until the user does something.
    document.mark_saved();
    // ...and there is nothing to undo back *past* the import, because undoing
    // it would leave an empty canvas the user never asked for.
    history.clear();

    Ok(ImportedDocument {
        document,
        history,
        tiles,
        layer,
    })
}

/// An empty document with one raster layer — File ▸ New.
///
/// `background` decides what the layer starts with: nothing at all (the
/// transparency checkerboard) or a solid colour across the whole canvas at the
/// document's bit depth. The background is part of the document's *initial*
/// state, not its first undo step — the history is cleared either way, exactly
/// like an opened file.
pub fn blank_document(
    width: u32,
    height: u32,
    title: &str,
    history_depth: usize,
    background: BlankBackground,
) -> Result<ImportedDocument, ImportError> {
    if width == 0 || height == 0 {
        return Err(ImportError::EmptyImage { width, height });
    }
    let mut document = Document::new(width, height, title);
    let mut history = History::with_limit(history_depth);
    let layer = Layer::raster("Layer 1");
    let layer_id = layer.id;
    let mut tiles = MemoryTileSource::new();
    let command = match background {
        BlankBackground::Transparent => Command::create_layer(layer),
        BlankBackground::Solid { rgba8, depth } => Command::Transaction {
            label: format!("New {title}"),
            commands: vec![
                Command::create_layer(layer),
                solid_background_command(layer_id, width, height, rgba8, depth, &mut tiles)?,
            ],
        },
    };
    history.apply(&mut document, command)?;
    document
        .set_active_layer(Some(layer_id))
        .expect("the layer was just created in this document");
    history.clear();
    document.mark_saved();
    Ok(ImportedDocument {
        document,
        history,
        tiles,
        layer: layer_id,
    })
}

/// What a newly created document's base layer starts with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlankBackground {
    /// No pixels: the layer has no tiles, so every pixel composites as fully
    /// transparent and the canvas shows the transparency checkerboard.
    Transparent,
    /// One solid colour across the whole canvas, at the document's bit depth.
    Solid {
        /// Straight-alpha RGBA8; widened per channel for 16-bit.
        rgba8: [u8; 4],
        depth: raster::BitDepth,
    },
}

/// Fill a new layer with one solid colour, edge tiles included.
///
/// Every tile the canvas covers gets a full `TILE_SIZE` square of the colour
/// with the outside-the-canvas part zeroed, so an 8×8 document and a
/// 16000×16000 one cost the same code path and never materialise more than
/// one tile of pixels at a time.
fn solid_background_command(
    layer: LayerId,
    width: u32,
    height: u32,
    rgba8: [u8; 4],
    depth: raster::BitDepth,
    tiles: &mut MemoryTileSource,
) -> Result<Command, ImportError> {
    let (format, bytes_per_component) = match depth {
        raster::BitDepth::Eight => (PixelFormat::Rgba8, 1usize),
        raster::BitDepth::Sixteen => (PixelFormat::Rgba16, 2usize),
    };
    // 16-bit widens each 8-bit channel the same way every other 8→16
    // conversion in the workspace does: by bit repetition, 255 → 65535.
    let pixel: Vec<u8> = match depth {
        raster::BitDepth::Eight => rgba8.to_vec(),
        raster::BitDepth::Sixteen => rgba8
            .iter()
            .flat_map(|c| (u16::from(*c) * 257).to_ne_bytes())
            .collect(),
    };
    let ts = TILE_SIZE as usize;
    let tile_bytes = ts * ts * bytes_per_component * 4;
    let mut edits = Vec::new();
    let rect = raster::PixelRect::new(0, 0, width, height);
    for (coord, _coverage) in
        editor_core::pixels::tiles_covering(rect).map_err(|e| ImportError::Command(e.into()))?
    {
        // Pixels past the canvas edge stay transparent inside their tile.
        let visible_w = (width as i64 - i64::from(coord.x) * TILE_SIZE as i64).max(0) as usize;
        let visible_h = (height as i64 - i64::from(coord.y) * TILE_SIZE as i64).max(0) as usize;
        let mut data = vec![0u8; tile_bytes];
        let row_bytes = ts * bytes_per_component * 4;
        let visible_row_bytes = visible_w.min(ts) * bytes_per_component * 4;
        for ty in 0..visible_h.min(ts) {
            let start = ty * row_bytes;
            for chunk in data[start..start + visible_row_bytes].chunks_exact_mut(pixel.len()) {
                chunk.copy_from_slice(&pixel);
            }
        }
        let tile = raster::Tile::from_bytes(format, data).map_err(raster::GridError::from)?;
        let hash = tiles.insert_tile(&tile);
        edits.push(TileEdit::set(coord, hash));
    }
    Ok(Command::PaintTiles {
        target: PixelTarget::Layer(layer),
        delta: TileDelta::new(edits).map_err(editor_core::CommandError::from)?,
    })
}

/// The tile coordinates a document's layer covers — what the presenter uploads.
pub fn layer_tile_coords(doc: &Document, layer: LayerId) -> Vec<TileCoord> {
    doc.layer_tiles(layer)
        .map(|m| m.iter().map(|(c, _)| c).collect())
        .unwrap_or_default()
}

// ========================================================================= PSD
//
// A `.psd` is a *document*, not a picture. Reading one through
// [`DecodedImage::decode_path`] would hand back the merged composite and throw
// away the layer tree, the groups, the masks, the blend modes and the
// per-layer opacity — which is to say, everything the file was saved for. So
// PSD does not go through the flat codec facade at all (`raster::codec` refuses
// it by name and says so); it comes through here, where the `psd` crate's
// document tree is turned into a real [`Document`] with a real [`LayerTree`],
// and goes back out the same way.
//
// # The two models do not line up exactly
//
// They disagree in both directions, and the disagreements are *reported* rather
// than swallowed — see [`PsdNotes`]. A `.psd` has bit depths and colour modes
// this editor does not store, adjustment payloads it cannot evaluate, type
// layers it cannot re-typeset, and layer effects beyond the four this build
// maps (card 075: drop shadow, stroke, colour overlay, outer glow); this
// document model has an arbitrary affine per layer, a mask density and feather,
// and a blanket lock, none of which a `.psd` can carry. Silently dropping any
// of those is the failure mode that makes a round trip untrustworthy, so every
// one of them lands in a note the caller can put in front of the user.
//
// # Untrusted input
//
// A `.psd` arrives from somewhere else and every length in it was chosen by
// whoever wrote it. The `psd` crate validates before it allocates and bounds
// the whole parse against one budget; nothing here re-derives a size from the
// file and trusts it. The file's own length is bounded before it is read
// ([`MAX_PSD_FILE_BYTES`]), the canvas is checked against
// [`editor_core::canvas_size_is_supported`] before a document exists, the tree
// walk is iterative so nesting cannot overflow the stack, and every pixel copy
// below indexes only inside a rectangle this module intersected itself.
//
// [`LayerTree`]: layer_model::LayerTree

/// Largest `.psd` this build will read into memory.
///
/// The format is not streamable — the layer section's channel data is located
/// by lengths recorded earlier in the file, and the merged composite sits after
/// all of it — so opening one means holding it. Two gibibytes is far past any
/// real document and still finite.
pub const MAX_PSD_FILE_BYTES: u64 = 2 << 30;

/// Largest canvas edge a `.psd` can describe. Past this the format is `.psb`,
/// which this workspace neither reads nor writes.
pub const MAX_PSD_DIMENSION: u32 = 30_000;

/// Deepest group nesting exchanged with a `.psd`.
///
/// The same ceiling `psd::ReadOptions` applies on the way in, applied again on
/// the way out so a document assembled here cannot produce a file this build
/// would then refuse to reopen. Photoshop's own limit is ten.
pub const MAX_PSD_GROUP_DEPTH: usize = 64;

/// What a `.psd` carried that this document has no home for — or the reverse.
///
/// Empty is the good case. Everything in here is something the user would
/// otherwise discover much later, by finding it missing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PsdNotes {
    notes: Vec<String>,
    layers: Vec<PsdLayerReport>,
}

/// What became of one imported layer (card 077).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PsdLayerOutcome {
    /// The layer mapped with nothing lost worth reporting.
    Editable,
    /// The layer's pixels came through but part of it (text, a vector mask,
    /// an effect, a flag) was replaced by a raster or dropped.
    RasterFallback,
    /// The layer arrived empty — a kind this build has no home for, or an
    /// adjustment it cannot evaluate.
    Unsupported,
}

impl PsdLayerOutcome {
    /// The report's word for the outcome.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Editable => "editable",
            Self::RasterFallback => "raster fallback",
            Self::Unsupported => "unsupported",
        }
    }
}

/// One layer's import outcome with a human-readable reason (card 077).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PsdLayerReport {
    pub name: String,
    pub outcome: PsdLayerOutcome,
    /// What changed on this layer, empty when nothing did.
    pub detail: String,
}

impl PsdNotes {
    /// One line per thing that did not map, in the order it was noticed.
    pub fn notes(&self) -> &[String] {
        &self.notes
    }

    /// Per-layer outcomes, in import (bottom-to-top) order (card 077).
    pub fn layers(&self) -> &[PsdLayerReport] {
        &self.layers
    }

    pub fn is_empty(&self) -> bool {
        self.notes.is_empty()
    }

    /// The actionable report (card 077): what changed, per layer, and what
    /// the user should do about it. `None` when everything mapped — a fully
    /// supported file never sees this report.
    pub fn report(&self, source: Option<&Path>) -> Option<String> {
        if self.notes.is_empty() {
            return None;
        }
        let mut out = String::from("Some parts of this file did not map exactly:\n");
        for note in &self.notes {
            out.push_str("\n• ");
            out.push_str(note);
        }
        if !self.layers.is_empty() {
            out.push_str("\n\nPer layer:");
            for layer in &self.layers {
                out.push_str(&format!("\n• {} — {}", layer.name, layer.outcome.as_str()));
                if !layer.detail.is_empty() {
                    out.push_str(": ");
                    out.push_str(&layer.detail);
                }
            }
        }
        let origin = source
            .map(|p| format!("{}", p.display()))
            .unwrap_or_else(|| "file".into());
        out.push_str(&format!(
            "\n\nThe original file {origin} was not modified. Use File > Save As \
             with the native .rstudio format to keep working without further loss."
        ));
        Some(out)
    }

    /// The whole report as one sentence, for a status line. `None` when
    /// everything mapped.
    pub fn summary(&self) -> Option<String> {
        if self.notes.is_empty() {
            return None;
        }
        Some(self.notes.join("; "))
    }

    fn push(&mut self, note: impl Into<String>) {
        let note = note.into();
        tracing::info!("psd: {note}");
        self.notes.push(note);
    }
}

/// Per-layer counters, turned into a short note list at the end.
///
/// Collected rather than reported one by one because a file with two hundred
/// unmappable layers should produce one sentence, not two hundred.
#[derive(Debug, Default)]
struct Tally {
    adjustments: Vec<String>,
    type_layers: Vec<String>,
    editable_text: Vec<String>,
    effects: Vec<String>,
    /// (layer, named kinds) for effects the descriptor listed but this build
    /// does not map — or lists with required fields missing.
    unmapped_effects: Vec<(String, String)>,
    second_masks: Vec<String>,
    transformed: Vec<String>,
    /// Card 078: text/shape/smart-object layers whose rendered appearance was
    /// written as a raster layer's pixels (a .psd cannot carry them
    /// editably).
    raster_fallback: Vec<String>,
    mask_params: Vec<String>,
    vector_masks: Vec<String>,
    locked_all: Vec<String>,
    pass_through_blend: Vec<String>,
    color_labels: Vec<String>,
}

/// `“a”, “b” and 3 more` — enough to recognise, short enough for a status bar.
fn named(items: &[String]) -> String {
    let shown: Vec<String> = items.iter().take(2).map(|n| format!("“{n}”")).collect();
    match items.len().saturating_sub(shown.len()) {
        0 => shown.join(" and "),
        rest => format!("{} and {rest} more", shown.join(", ")),
    }
}

/// `satin and inner shadow` / `satin, inner glow and bevel` — the kinds in a
/// report note, in the order the file listed them.
fn kinds_phrase(kinds: &[String]) -> String {
    match kinds.len() {
        0 => String::new(),
        1 => kinds[0].clone(),
        _ => {
            let (last, head) = kinds.split_last().unwrap();
            format!("{} and {last}", head.join(", "))
        }
    }
}

impl Tally {
    /// Per-category lengths, in a fixed order — the diff between two of these
    /// says which categories a single layer's import grew (card 077).
    const CATEGORIES: usize = 13;

    fn signature(&self) -> [usize; Self::CATEGORIES] {
        [
            self.color_labels.len(),
            self.adjustments.len(),
            self.type_layers.len(),
            self.editable_text.len(),
            self.effects.len(),
            self.second_masks.len(),
            self.transformed.len(),
            self.raster_fallback.len(),
            self.mask_params.len(),
            self.vector_masks.len(),
            self.locked_all.len(),
            self.pass_through_blend.len(),
            self.unmapped_effects.len(),
        ]
    }

    /// What became of the layer whose import grew the tally from `before` to
    /// now: an outcome class plus the short labels of everything that moved.
    fn classify(&self, before: &[usize; Tally::CATEGORIES]) -> (PsdLayerOutcome, String) {
        const LABELS: [&str; Tally::CATEGORIES] = [
            "colour label dropped",
            "adjustment kept as an empty layer",
            "text imported as pixels",
            "editable text with a substituted font",
            "effects not imported",
            "second mask not imported",
            "transform baked into pixels",
            "text/shape/smart-object content written as raster pixels",
            "mask density/feather dropped",
            "vector mask rasterised",
            "lock dropped",
            "blend mode dropped for pass-through",
            "named effects not imported",
        ];
        let after = self.signature();
        let mut outcome = PsdLayerOutcome::Editable;
        let mut details: Vec<String> = Vec::new();
        for (i, (b, a)) in before.iter().zip(after.iter()).enumerate() {
            if a > b {
                // The unmapped-effects category names its kinds, so the
                // report says what was dropped, not just that something was.
                details.push(if i == 12 {
                    let kinds: Vec<&str> = self.unmapped_effects[*b..]
                        .iter()
                        .map(|(_, k)| k.as_str())
                        .collect();
                    format!("{} ({})", LABELS[i], kinds.join(", "))
                } else {
                    LABELS[i].to_string()
                });
                match i {
                    // A layer that arrives empty cannot be edited back into
                    // what it was.
                    1 => outcome = PsdLayerOutcome::Unsupported,
                    // Text that became pixels, a vector mask that became
                    // coverage, and a rendered text/shape/smart-object
                    // fallback are raster fallbacks by name.
                    2 | 7 | 9 if outcome == PsdLayerOutcome::Editable => {
                        outcome = PsdLayerOutcome::RasterFallback;
                    }
                    _ => {}
                }
            }
        }
        (outcome, details.join(", "))
    }

    fn record(&mut self, notes: &mut PsdNotes) {
        let entries: [(&[String], &str); 12] = [
            (
                &self.color_labels,
                "the colour label on {names} is not shown by this layers panel and was not kept",
            ),
            (
                &self.adjustments,
                "adjustment layer(s) this build cannot evaluate ({names}) were kept as empty \
                 layers; their effect is in the flattened image but not editable",
            ),
            (
                &self.type_layers,
                "type layer(s) ({names}) were imported as pixels; the text is no longer editable",
            ),
            (
                &self.editable_text,
                "type layer(s) ({names}) were imported as editable text with the default font, \
                 size and fill — the source font is not in this build's supported subset",
            ),
            (
                &self.effects,
                "layer effect(s) on {names} were not imported",
            ),
            (
                &self.second_masks,
                "{names} carried a second, vector-derived mask that was not imported",
            ),
            (
                &self.transformed,
                "{names} carry a transform a .psd cannot express; their pixels were written \
                 where they are stored",
            ),
            (
                &self.raster_fallback,
                "text, shape and smart-object layer(s) ({names}) cannot stay editable in a .psd; \
                 their rendered appearance was written as a raster layer's pixels",
            ),
            (
                &self.mask_params,
                "the mask density or feather on {names} was not written",
            ),
            (
                &self.vector_masks,
                "the vector mask on {names} was written as its rasterised coverage",
            ),
            (
                &self.locked_all,
                "the blanket lock on {names} has no .psd equivalent and was not written",
            ),
            (
                &self.pass_through_blend,
                "{names} pass through *and* carry a blend mode; a .psd stores only the \
                 pass-through",
            ),
        ];
        for (items, template) in entries {
            if !items.is_empty() {
                notes.push(template.replace("{names}", &named(items)));
            }
        }
        // Unmapped effect kinds are named per kind, grouped by signature so
        // layers with the same gap share one sentence.
        let mut by_kinds: std::collections::BTreeMap<&str, Vec<&str>> = Default::default();
        for (name, kinds) in &self.unmapped_effects {
            by_kinds.entry(kinds).or_default().push(name);
        }
        for (kinds, names) in by_kinds {
            let names: Vec<String> = names.into_iter().map(|s| s.to_string()).collect();
            notes.push(format!(
                "the {kinds} effect(s) on {} were not imported",
                named(&names)
            ));
        }
    }
}

/// A half-open rectangle in document pixels.
///
/// `i64` throughout: a `.psd` layer rectangle is a pair of `i32`s chosen by the
/// file, and `right - left` on those overflows `i32`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DocRect {
    x0: i64,
    y0: i64,
    x1: i64,
    y1: i64,
}

impl DocRect {
    const EMPTY: DocRect = DocRect {
        x0: 0,
        y0: 0,
        x1: 0,
        y1: 0,
    };

    fn canvas(width: u32, height: u32) -> Self {
        DocRect {
            x0: 0,
            y0: 0,
            x1: i64::from(width),
            y1: i64::from(height),
        }
    }

    fn from_psd(r: psd::Rect) -> Self {
        DocRect {
            x0: i64::from(r.left),
            y0: i64::from(r.top),
            x1: i64::from(r.left) + i64::from(r.width()),
            y1: i64::from(r.top) + i64::from(r.height()),
        }
    }

    /// The `.psd` spelling. Only ever called on a rectangle already clipped to
    /// a canvas, so the `i32` casts cannot lose anything.
    fn to_psd(self) -> psd::Rect {
        psd::Rect {
            left: self.x0 as i32,
            top: self.y0 as i32,
            right: self.x1 as i32,
            bottom: self.y1 as i32,
        }
    }

    fn is_empty(self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }

    fn width(self) -> u32 {
        (self.x1 - self.x0).clamp(0, i64::from(u32::MAX)) as u32
    }

    fn height(self) -> u32 {
        (self.y1 - self.y0).clamp(0, i64::from(u32::MAX)) as u32
    }

    fn clip(self, to: DocRect) -> Self {
        let out = DocRect {
            x0: self.x0.max(to.x0),
            y0: self.y0.max(to.y0),
            x1: self.x1.min(to.x1),
            y1: self.y1.min(to.y1),
        };
        if out.is_empty() {
            DocRect::EMPTY
        } else {
            out
        }
    }

    fn union(self, other: DocRect) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        DocRect {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }

    fn offset(self, dx: i64, dy: i64) -> Self {
        DocRect {
            x0: self.x0 + dx,
            y0: self.y0 + dy,
            x1: self.x1 + dx,
            y1: self.y1 + dy,
        }
    }

    /// Every level-0 tile this rectangle touches.
    fn tiles(self) -> Vec<TileCoord> {
        if self.is_empty() {
            return Vec::new();
        }
        let ts = i64::from(TILE_SIZE);
        let mut out = Vec::new();
        for ty in self.y0.div_euclid(ts)..=(self.y1 - 1).div_euclid(ts) {
            for tx in self.x0.div_euclid(ts)..=(self.x1 - 1).div_euclid(ts) {
                out.push(TileCoord::new(tx as i32, ty as i32, 0));
            }
        }
        out
    }
}

/// The rectangle a tile map covers, in the space its coordinates address.
fn tile_map_rect(map: &TileMap) -> DocRect {
    let ts = i64::from(TILE_SIZE);
    let mut out: Option<DocRect> = None;
    for (coord, _) in map.iter() {
        if coord.level != 0 {
            continue;
        }
        let (ox, oy) = coord.pixel_origin();
        let r = DocRect {
            x0: ox,
            y0: oy,
            x1: ox + ts,
            y1: oy + ts,
        };
        out = Some(match out {
            Some(acc) => acc.union(r),
            None => r,
        });
    }
    out.unwrap_or(DocRect::EMPTY)
}

/// The integer translation a layer transform amounts to, and whether that is
/// all it is.
///
/// A `.psd` layer has no transform: it has a rectangle. A pure integer
/// translation therefore folds into the rectangle exactly, and anything else —
/// rotation, scale, a sub-pixel shift — would have to be resampled, which is a
/// destructive edit this exporter will not make silently. Such a layer is
/// written where its pixels are stored, and the caller is told.
fn translation_of(transform: glam::Affine2) -> (i64, i64, bool) {
    let m = transform.to_cols_array();
    if !m.iter().all(|v| v.is_finite()) {
        return (0, 0, false);
    }
    let linear_is_identity = m[0] == 1.0 && m[1] == 0.0 && m[2] == 0.0 && m[3] == 1.0;
    let integral = m[4].fract() == 0.0 && m[5].fract() == 0.0;
    if linear_is_identity && integral {
        (m[4] as i64, m[5] as i64, true)
    } else {
        (0, 0, false)
    }
}

// ------------------------------------------------------------------ reading

/// `true` when `path` holds a Photoshop document.
///
/// By **content**, not by extension: a `.psd` renamed `.png` is still a
/// document, and a `.png` renamed `.psd` is still a picture. Getting this from
/// the name would send one of them down the wrong path and produce a confusing
/// error for a file that is perfectly readable the other way.
pub fn looks_like_psd(path: &Path) -> bool {
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = [0u8; 4];
    let mut filled = 0;
    while filled < head.len() {
        match file.read(&mut head[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(_) => return false,
        }
    }
    filled == head.len() && head == raster::codec::PSD_SIGNATURE
}

/// Read a `.psd` off disk, bounded by [`MAX_PSD_FILE_BYTES`].
///
/// The metadata length is a hint, not a licence: it is checked first so an
/// absurd file is refused without reading it, and then the read itself runs
/// through a `take` one byte past the ceiling, because a file being written (or
/// a pipe, or a device) can report a length it does not have.
pub fn read_psd_bytes(path: &Path) -> Result<Vec<u8>, ImportError> {
    let file = std::fs::File::open(path)?;
    let declared = file.metadata()?.len();
    if declared > MAX_PSD_FILE_BYTES {
        return Err(ImportError::PsdTooLarge {
            bytes: declared,
            max: MAX_PSD_FILE_BYTES,
        });
    }
    let mut bytes = Vec::new();
    // Not `with_capacity(declared)`: that reserves whatever the file claims.
    file.take(MAX_PSD_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_PSD_FILE_BYTES {
        return Err(ImportError::PsdTooLarge {
            bytes: bytes.len() as u64,
            max: MAX_PSD_FILE_BYTES,
        });
    }
    Ok(bytes)
}

/// A `.psd` turned into a document, and what did not survive the trip.
#[derive(Debug)]
pub struct PsdImport {
    pub imported: ImportedDocument,
    pub notes: PsdNotes,
    /// The file's own flattened composite as RGBA8, when it carries one
    /// (card 077). Kept so a caller may compare it against the reconstructed
    /// document — see [`PsdImport::compare_merged_preview`]. Not an authority:
    /// the preview is whatever wrote the file rendered, crude flatteners
    /// included.
    pub merged_preview: Option<Vec<u8>>,
}

/// How far this reconstruction is from the file's flattened preview
/// (card 077).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MergedPreviewDiff {
    /// Pixels compared (canvas area).
    pub pixels: usize,
    /// Pixels where any channel differs by more than the tolerance.
    pub differing: usize,
    /// Largest per-channel difference seen, 0–255.
    pub max_delta: u8,
}

impl PsdImport {
    /// Compare the file's flattened preview against this document's own
    /// composite. `None` when the file carries no preview (or is empty).
    /// The comparison is offered, not asserted: a preview written by a crude
    /// flattener legitimately differs from a correct reconstruction, so the
    /// numbers are evidence for a human, never a verdict on their own.
    pub fn compare_merged_preview(&self, tolerance: u8) -> Option<MergedPreviewDiff> {
        let rgba = self.merged_preview.as_ref()?;
        let doc = &self.imported.document;
        let canvas = compositor::composite_region(
            doc,
            &self.imported.tiles,
            raster::PixelRect::new(0, 0, doc.width(), doc.height()),
            0,
            compositor::CompositeOptions::default(),
        )
        .ok()?;
        let rendered = canvas.to_rgba8(&doc.meta.color_space);
        let differing = rgba
            .chunks_exact(4)
            .zip(rendered.chunks_exact(4))
            .filter(|(a, b)| {
                a.iter()
                    .zip(b.iter())
                    .any(|(x, y)| x.abs_diff(*y) > tolerance)
            })
            .count();
        let max_delta = rgba
            .chunks_exact(4)
            .zip(rendered.chunks_exact(4))
            .flat_map(|(a, b)| a.iter().zip(b.iter()).map(|(x, y)| x.abs_diff(*y)))
            .max()
            .unwrap_or(0);
        Some(MergedPreviewDiff {
            pixels: (doc.width() as usize) * (doc.height() as usize),
            differing,
            max_delta,
        })
    }
}

/// One PSD layer's colour and alpha, packed into the editor's RGBA8.
fn psd_layer_rgba(layer: &psd::PsdLayer, header: &psd::PsdHeader) -> Option<Vec<u8>> {
    let (width, height) = (layer.bounds.width(), layer.bounds.height());
    if width == 0 || height == 0 {
        return None;
    }
    let mut color = Vec::with_capacity(header.color_mode.channel_ids().len());
    for id in header.color_mode.channel_ids() {
        color.push(layer.channel(*id)?.data.as_slice());
    }
    let alpha = layer.channel(psd::CHANNEL_ALPHA).map(|c| c.data.as_slice());
    raster::codec::rgba8_from_planes(width, height, &color, alpha, header.depth.bits())
}

/// One PSD mask's samples as 8-bit coverage.
fn psd_mask_coverage(mask: &psd::PsdMask, depth: psd::Depth) -> Option<Vec<u8>> {
    let (width, height) = (mask.bounds.width(), mask.bounds.height());
    if width == 0 || height == 0 {
        return None;
    }
    raster::codec::gray8_from_plane(width, height, &mask.data, depth.bits())
}

/// The merged composite as RGBA8, whatever depth and colour mode it is in.
fn psd_merged_rgba(file: &psd::PsdFile) -> Option<Vec<u8>> {
    let merged = file.merged.as_ref()?;
    let colors = file.header.color_mode.color_channels() as usize;
    if merged.channels.len() < colors {
        return None;
    }
    let color: Vec<&[u8]> = merged.channels[..colors]
        .iter()
        .map(|c| c.as_slice())
        .collect();
    let alpha = merged.channels.get(colors).map(|c| c.as_slice());
    raster::codec::rgba8_from_planes(
        file.header.width,
        file.header.height,
        &color,
        alpha,
        file.header.depth.bits(),
    )
}

/// Cut an image that sits at `rect` in document space into level-0 tiles.
///
/// Card 076: the FULL extent is kept — `rect` may reach past the canvas and
/// the tiles outside it are stored anyway. A tile coordinate is a signed
/// `(x, y, level)` pair, so a layer hanging off the left edge has real
/// coordinates to live at, and the ink outside the canvas survives import:
/// moving the layer (or compositing an extended region) brings it into view.
/// Dropping it would be a destructive edit performed on open.
///
/// A tile that comes out entirely zero is *not* stored: an absent tile already
/// reads as fully transparent, so storing one would only cost a map entry.
fn tile_edits_for_rgba(
    rgba: &[u8],
    rect: psd::Rect,
    tiles: &mut MemoryTileSource,
) -> Vec<TileEdit> {
    let source = DocRect::from_psd(rect);
    let (w, h) = (i64::from(rect.width()), i64::from(rect.height()));
    if w == 0 || h == 0 || rgba.len() as u64 != (w as u64) * (h as u64) * 4 {
        return Vec::new();
    }
    let area = source;
    if area.is_empty() {
        return Vec::new();
    }

    let ts = i64::from(TILE_SIZE);
    let stride = TILE_SIZE as usize * 4;
    let mut out = Vec::new();
    for coord in area.tiles() {
        let (ox, oy) = coord.pixel_origin();
        let (cx0, cx1) = (area.x0.max(ox), area.x1.min(ox + ts));
        let (cy0, cy1) = (area.y0.max(oy), area.y1.min(oy + ts));
        let mut data = vec![0u8; stride * TILE_SIZE as usize];
        for y in cy0..cy1 {
            let src = (((y - source.y0) * w + (cx0 - source.x0)) as usize) * 4;
            let dst = ((y - oy) as usize) * stride + ((cx0 - ox) as usize) * 4;
            let n = ((cx1 - cx0) as usize) * 4;
            data[dst..dst + n].copy_from_slice(&rgba[src..src + n]);
        }
        if data.iter().all(|b| *b == 0) {
            continue;
        }
        let hash = tiles.insert_bytes(data);
        out.push(TileEdit::set(coord, hash));
    }
    out
}

/// Build coverage tiles for `coords`, filling from `coverage` inside `rect` and
/// with `default_color` outside it.
///
/// The default colour is why this cannot simply skip the tiles the mask
/// rectangle does not reach: a mask whose default is 255 *shows* everything
/// outside its own box, and an absent tile reads as zero coverage — the layer
/// fully hidden. Getting that backwards turns "hide this corner" into "hide
/// everything else".
fn tile_edits_for_coverage(
    coverage: Option<&[u8]>,
    rect: psd::Rect,
    default_color: u8,
    coords: &[TileCoord],
    tiles: &mut MemoryTileSource,
) -> Vec<TileEdit> {
    let source = DocRect::from_psd(rect);
    let w = i64::from(rect.width());
    let have = coverage
        .filter(|c| !source.is_empty() && c.len() as u64 == (w as u64) * u64::from(rect.height()));

    let ts = i64::from(TILE_SIZE);
    let stride = TILE_SIZE as usize;
    let mut out = Vec::new();
    for &coord in coords {
        let (ox, oy) = coord.pixel_origin();
        let mut data = vec![default_color; MASK_TILE_BYTES];
        if let Some(c) = have {
            let (cx0, cx1) = (source.x0.max(ox), source.x1.min(ox + ts));
            let (cy0, cy1) = (source.y0.max(oy), source.y1.min(oy + ts));
            for y in cy0..cy1 {
                if cx1 <= cx0 {
                    break;
                }
                let src = ((y - source.y0) * w + (cx0 - source.x0)) as usize;
                let dst = ((y - oy) as usize) * stride + (cx0 - ox) as usize;
                let n = (cx1 - cx0) as usize;
                data[dst..dst + n].copy_from_slice(&c[src..src + n]);
            }
        }
        // Zero coverage is exactly what an absent tile means already.
        if data.iter().all(|b| *b == 0) {
            continue;
        }
        let hash = tiles.insert_bytes(data);
        out.push(TileEdit::set(coord, hash));
    }
    out
}

/// One level of the tree walk: a parent and the children still to place.
struct Frame<'a> {
    parent: Option<LayerId>,
    /// Bottom-to-top, as the file stores them, popped from the back so the
    /// top-most is placed first — which is the order `LayerTree` indexes in.
    items: Vec<&'a psd::PsdLayer>,
    /// Next index under `parent`, counting from the top.
    index: usize,
}

/// The properties every PSD layer record carries, whatever kind it is.
fn layer_common(source: &psd::PsdLayer, tally: &mut Tally) -> Layer {
    let mut layer = Layer::raster(&source.name);
    layer.visible = source.visible;
    layer.opacity = f32::from(source.opacity) / 255.0;
    layer.fill_opacity = source.fill_opacity.map_or(1.0, |f| f32::from(f) / 255.0);
    layer.blend_mode = source.blend_mode;
    layer.clipping = if source.clipping {
        ClippingMode::ClipToBelow
    } else {
        ClippingMode::None
    };
    layer.locked = LockState {
        pixels: source.protection.composite,
        position: source.protection.position,
        // Photoshop writes transparency-lock twice: as a record flag and in the
        // `lspf` block. Either one means the same thing here.
        transparency: source.protection.transparency || source.transparency_protected,
        all: false,
    };
    if let Some(effects) = &source.effects {
        // Card 075: the four required effects decode into editable
        // parameters. A block that does not parse at all keeps the blanket
        // note; a block that parses but carries kinds (or required fields)
        // this build does not map keeps a note naming those kinds. The
        // verbatim `lfx2` bytes stay in the psd model either way — retention,
        // not rendering.
        match psd::import_effects(effects, &psd::ReadOptions::default()) {
            Some(imported) => {
                if !imported.effects.is_default() {
                    layer.effects = imported.effects;
                }
                if !imported.unmapped.is_empty() {
                    tally
                        .unmapped_effects
                        .push((source.name.clone(), kinds_phrase(&imported.unmapped)));
                }
            }
            None => tally.effects.push(source.name.clone()),
        }
    }
    if source.sheet_color.is_some_and(|c| c != 0) {
        tally.color_labels.push(source.name.clone());
    }
    if let Some(mask) = &source.mask {
        let mut attached = LayerMask::new(MaskId::new());
        attached.enabled = !mask.disabled;
        attached.inverted = mask.invert;
        // A `.psd` mask flag says "position relative to layer", which is the
        // *un*chained state in the panel; `linked` here is the chained one.
        attached.linked = !mask.relative_to_layer;
        // Card 076: the mask parameters travel too. Density is stored as a
        // `0..=255` byte — the same 8-bit scale the rest of the format uses,
        // mapped onto the model's `0.0..=1.0`; feather is an `f64` count of
        // PIXELS, which is already the model's unit (`feather_px`, document
        // pixels — a `.psd` mask has no space of its own beyond its layer's).
        // The setters only refuse non-finite values, so that is the one case
        // the mask_params note still names on import.
        if attached
            .set_density(f32::from(mask.density) / 255.0)
            .is_err()
            || attached.set_feather_px(mask.feather_px as f32).is_err()
        {
            tally.mask_params.push(source.name.clone());
        }
        layer.set_mask(attached);
        if mask.real.is_some() {
            tally.second_masks.push(source.name.clone());
        }
    }
    layer
}

/// Turn the bytes of a `.psd` into a document with its layer tree intact.
pub fn document_from_psd(
    bytes: &[u8],
    title: &str,
    history_depth: usize,
) -> Result<PsdImport, ImportError> {
    let file = psd::read(bytes)?;
    let header = file.header;
    let (width, height) = (header.width, header.height);
    if !editor_core::canvas_size_is_supported(width, height) {
        return Err(ImportError::PsdCanvas { width, height });
    }

    let mut notes = PsdNotes::default();
    let mut tally = Tally::default();
    for warning in &file.warnings {
        notes.push(format!("the file was read with a repair: {warning}"));
    }
    if header.depth != psd::Depth::Eight {
        notes.push(format!(
            "this is a {}-bit-per-channel document; Raster Studio edits 8, so its pixels were \
             converted down",
            header.depth.bits()
        ));
    }
    if header.color_mode == psd::ColorMode::Grayscale {
        notes.push("a greyscale document was opened as RGB");
    }
    // Card 076: the profile travels with the document. The pixels are NOT
    // transformed at import — they load verbatim into the tiles, and the
    // colour pipeline does the conversion at render, the same contract the
    // flat codecs follow: a matrix-shaper profile is converted by the
    // compositor, and a profile this engine cannot parse falls back to
    // identity (documented on `ColorSpace::is_transform_supported`), i.e. the
    // pixels are kept rather than silently reinterpreted. A profile that is
    // measurably sRGB is recorded as sRGB — treating its pixels as sRGB is
    // exact, so the bytes are redundant.
    let profile = psd::resource::icc_profile(&file.resources);
    let profile_space = match profile {
        Some(bytes)
            if color::icc::MatrixShaper::parse(bytes).is_ok_and(|p| p.is_srgb_equivalent()) =>
        {
            color::ColorSpace::Srgb
        }
        Some(bytes) => raster::icc_profile_space(bytes),
        None => color::ColorSpace::Srgb,
    };
    // The resolution resource is not content: every writer synthesises one, and
    // reporting it would put a note on the perfectly clean round trip of a file
    // this application wrote itself, which trains the user to ignore the notes.
    // A retained ICC profile is content-adjacent too — it rides in the
    // document's colour space, so it is no longer "left behind" and must not
    // be counted (or named) as dropped.
    let retained_profile = matches!(profile_space, color::ColorSpace::IccProfile { .. });
    let dropped_resources = file
        .resources
        .iter()
        .filter(|r| {
            r.id != psd::resource::ID_RESOLUTION_INFO
                && !(retained_profile && r.id == psd::resource::ID_ICC_PROFILE)
        })
        .count();
    if dropped_resources > 0 {
        let contents = if retained_profile {
            "guides, paths"
        } else {
            "guides, paths, the colour profile"
        };
        notes.push(format!(
            "{dropped_resources} image resource(s) — {contents} — are \
             not part of this document model and were left behind"
        ));
    }

    let mut document = Document::new(width, height, title);
    document.meta.color_space = profile_space;
    let mut tiles = MemoryTileSource::new();

    let mut stack = vec![Frame {
        parent: None,
        items: file.layers.iter().collect(),
        index: 0,
    }];
    while let Some(frame) = stack.last_mut() {
        let Some(source) = frame.items.pop() else {
            stack.pop();
            continue;
        };
        let parent = frame.parent;
        let index = frame.index;
        frame.index += 1;

        let before = tally.signature();
        let mut layer = layer_common(source, &mut tally);
        let mut wants_pixels = false;
        match &source.kind {
            psd::LayerKind::Group(group) => {
                layer.kind = LayerKind::Group(GroupLayer {
                    children: Vec::new(),
                    collapsed: !group.open,
                    blending: if group.pass_through {
                        GroupBlending::PassThrough
                    } else {
                        GroupBlending::Isolated
                    },
                });
            }
            psd::LayerKind::Raster => match &source.adjustment {
                // Invert is the one adjustment whose whole definition is its
                // name: there are no parameters to decode, so it maps exactly.
                Some(adjustment) if adjustment.key == *b"nvrt" => {
                    layer.kind = LayerKind::Adjustment(layer_model::AdjustmentLayer {
                        kind: AdjustmentKind::Invert,
                    });
                }
                Some(adjustment) => {
                    // The payload survives in the `psd` crate's model but this
                    // document has no vocabulary for it, and inventing one
                    // would put the wrong numbers behind a slider.
                    tally.adjustments.push(format!(
                        "{} ({})",
                        source.name,
                        psd::error::tag_name(adjustment.key)
                    ));
                }
                None => {
                    // A type layer's editable subset: a parseable `Txt ` string
                    // and a `TySh` transform (any affine). The string and the
                    // transform import as a real text layer; the font, size and
                    // fill do not — without the engine data the file does not
                    // name them, so the editor's new-text defaults stand in and
                    // the substitution is reported by name. The `TySh` bytes
                    // themselves stay in the `psd` model and survive a save
                    // verbatim (`psd::text` writes back what it read).
                    let parsed = source
                        .text
                        .as_ref()
                        .and_then(|t| t.text.clone().map(|text| (t.transform, text)));
                    if let Some((transform, text)) = parsed {
                        let [xx, xy, yx, yy, tx, ty] = transform;
                        layer.kind = LayerKind::Text(TextLayer {
                            text,
                            font_family: TEXT_DEFAULT_FAMILY.to_string(),
                            size_px: TEXT_DEFAULT_SIZE,
                            ..TextLayer::default()
                        });
                        layer.transform = glam::Affine2::from_cols_array(&[
                            xx as f32, xy as f32, yx as f32, yy as f32, tx as f32, ty as f32,
                        ]);
                        tally.editable_text.push(source.name.clone());
                    } else {
                        // No parseable string: everything editable about this
                        // type layer is locked inside the engine data, so the
                        // pixels are the only honest thing to keep.
                        wants_pixels = true;
                        if source.text.is_some() {
                            tally.type_layers.push(source.name.clone());
                        }
                    }
                }
            },
        }

        let id = document.layers.insert_at(layer, parent, index)?;

        let mut placed = DocRect::EMPTY;
        if wants_pixels {
            if let Some(rgba) = psd_layer_rgba(source, &header) {
                let edits = tile_edits_for_rgba(&rgba, source.bounds, &mut tiles);
                if !edits.is_empty() {
                    let delta = TileDelta::new(edits).map_err(editor_core::CommandError::from)?;
                    document.pixels.apply(PixelKey::Layer(id), &delta);
                }
                // The full extent, off-canvas parts included: the tile store
                // holds the ink wherever the file put it (card 076).
                placed = DocRect::from_psd(source.bounds);
            }
        }

        if let Some(mask) = &source.mask {
            // Card 076: no canvas clip here either — the mask's own box and
            // the layer's full extent can both reach past the canvas.
            let mask_area = DocRect::from_psd(mask.bounds);
            // Where the mask's own box does not reach, its default colour
            // decides — and a default of 255 has to be written out over
            // everything the layer covers, or the absent tiles would read as
            // "hidden" instead.
            let region = if mask.default_color == 0 {
                mask_area
            } else {
                mask_area.union(placed)
            };
            let coords = region.tiles();
            let coverage = psd_mask_coverage(mask, header.depth);
            let edits = tile_edits_for_coverage(
                coverage.as_deref(),
                mask.bounds,
                mask.default_color,
                &coords,
                &mut tiles,
            );
            if !edits.is_empty() {
                let mask_id = document
                    .layers
                    .get(id)
                    .and_then(Layer::mask_id)
                    .expect("layer_common attached a mask to this layer");
                let delta = TileDelta::new(edits).map_err(editor_core::CommandError::from)?;
                document.pixels.apply(PixelKey::Mask(mask_id), &delta);
            }
        }

        // Card 077: every layer lands in the report, classified by what its
        // import grew in the tally — editable, raster fallback, or unsupported.
        let (outcome, detail) = tally.classify(&before);
        notes.layers.push(PsdLayerReport {
            name: source.name.clone(),
            outcome,
            detail,
        });

        if let Some(group) = source.group_data() {
            stack.push(Frame {
                parent: Some(id),
                items: group.children.iter().collect(),
                index: 0,
            });
        }
    }

    if document.layers.is_empty() {
        // A flattened `.psd` — no layer section at all — is still a picture.
        // Photoshop writes these; so does every "save a copy" pipeline.
        let name = if psd_merged_rgba(&file).is_some() {
            "Background"
        } else {
            "Layer 1"
        };
        let layer = document.layers.push_root(Layer::raster(name))?;
        if let Some(rgba) = psd_merged_rgba(&file) {
            let edits = tile_edits_for_rgba(&rgba, psd::Rect::sized(width, height), &mut tiles);
            if !edits.is_empty() {
                let delta = TileDelta::new(edits).map_err(editor_core::CommandError::from)?;
                document.pixels.apply(PixelKey::Layer(layer), &delta);
            }
            notes.push("this file has no layers, so its flattened image became one layer");
        } else {
            notes.push("this file has neither layers nor a flattened image; the canvas is empty");
        }
    }

    tally.record(&mut notes);

    let order = document.layers.iter_depth_first();
    let active = order
        .iter()
        .copied()
        .find(|id| document.layers.get(*id).is_some_and(|l| !l.is_group()))
        .or_else(|| order.first().copied())
        .expect("the tree holds at least one layer");
    document
        .set_active_layer(Some(active))
        .expect("the active layer was taken from this tree");
    // Opening a file is not an edit.
    document.mark_saved();

    Ok(PsdImport {
        imported: ImportedDocument {
            document,
            history: History::with_limit(history_depth),
            tiles,
            layer: active,
        },
        notes,
        merged_preview: psd_merged_rgba(&file),
    })
}

// ------------------------------------------------------------------ writing

/// Reassemble a rectangle of a layer's pixels out of its tiles.
///
/// `rect` is in the space the tile coordinates address — layer space, which is
/// document space for the untransformed layers this exporter writes directly.
fn rgba_from_tiles(map: &TileMap, tiles: &MemoryTileSource, rect: DocRect) -> Vec<u8> {
    let (w, h) = (rect.width() as usize, rect.height() as usize);
    let mut out = vec![0u8; w * h * 4];
    if w == 0 || h == 0 {
        return out;
    }
    let ts = i64::from(TILE_SIZE);
    let stride = TILE_SIZE as usize * 4;
    for (coord, hash) in map.iter() {
        if coord.level != 0 {
            continue;
        }
        let Some(data) = tiles.tile(hash) else {
            continue;
        };
        if data.len() < stride * TILE_SIZE as usize {
            continue;
        }
        let (ox, oy) = coord.pixel_origin();
        let (cx0, cx1) = (rect.x0.max(ox), rect.x1.min(ox + ts));
        let (cy0, cy1) = (rect.y0.max(oy), rect.y1.min(oy + ts));
        for y in cy0..cy1 {
            if cx1 <= cx0 {
                break;
            }
            let src = ((y - oy) as usize) * stride + ((cx0 - ox) as usize) * 4;
            let dst = (((y - rect.y0) as usize) * w + (cx0 - rect.x0) as usize) * 4;
            let n = ((cx1 - cx0) as usize) * 4;
            out[dst..dst + n].copy_from_slice(&data[src..src + n]);
        }
    }
    out
}

/// [`rgba_from_tiles`] for a mask's one byte per pixel.
fn coverage_from_tiles(map: &TileMap, tiles: &MemoryTileSource, rect: DocRect) -> Vec<u8> {
    let (w, h) = (rect.width() as usize, rect.height() as usize);
    let mut out = vec![0u8; w * h];
    if w == 0 || h == 0 {
        return out;
    }
    let ts = i64::from(TILE_SIZE);
    let stride = TILE_SIZE as usize;
    for (coord, hash) in map.iter() {
        if coord.level != 0 {
            continue;
        }
        let Some(data) = tiles.tile(hash) else {
            continue;
        };
        if data.len() < MASK_TILE_BYTES {
            continue;
        }
        let (ox, oy) = coord.pixel_origin();
        let (cx0, cx1) = (rect.x0.max(ox), rect.x1.min(ox + ts));
        let (cy0, cy1) = (rect.y0.max(oy), rect.y1.min(oy + ts));
        for y in cy0..cy1 {
            if cx1 <= cx0 {
                break;
            }
            let src = ((y - oy) as usize) * stride + (cx0 - ox) as usize;
            let dst = ((y - rect.y0) as usize) * w + (cx0 - rect.x0) as usize;
            let n = (cx1 - cx0) as usize;
            out[dst..dst + n].copy_from_slice(&data[src..src + n]);
        }
    }
    out
}

/// `0.0..=1.0` as `0..=255`, exactly inverting the import's division.
fn to_byte(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// Shrink `rgba` (an image of `rect`'s size) to the smallest rectangle holding
/// a pixel that is not fully transparent. `None` when there is no such pixel.
///
/// A layer's tiles are 256 pixels on a side, so the rectangle they cover is
/// rounded out to tile boundaries. Writing *that* as the layer's `.psd`
/// rectangle is not wrong, but it is what makes Photoshop draw a selection
/// marquee around empty space and makes the file carry up to a quarter of a
/// megapixel of nothing per layer. A `.psd` layer rectangle is meant to be the
/// content's own bounding box, so that is what is written.
fn crop_to_content(rgba: &[u8], rect: DocRect) -> Option<(DocRect, Vec<u8>)> {
    let (w, h) = (rect.width() as usize, rect.height() as usize);
    if w == 0 || h == 0 || rgba.len() != w * h * 4 {
        return None;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0usize, 0usize);
    for y in 0..h {
        for x in 0..w {
            if rgba[(y * w + x) * 4 + 3] != 0 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
    }
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    if (x0, y0, x1, y1) == (0, 0, w, h) {
        return Some((rect, rgba.to_vec()));
    }
    let (cw, ch) = (x1 - x0, y1 - y0);
    let mut out = Vec::with_capacity(cw * ch * 4);
    for y in y0..y1 {
        let start = (y * w + x0) * 4;
        out.extend_from_slice(&rgba[start..start + cw * 4]);
    }
    Some((
        DocRect {
            x0: rect.x0 + x0 as i64,
            y0: rect.y0 + y0 as i64,
            x1: rect.x0 + x1 as i64,
            y1: rect.y0 + y1 as i64,
        },
        out,
    ))
}

/// One level of the document's tree as `.psd` layer records, bottom-to-top.
fn psd_layers_for(
    document: &Document,
    tiles: &MemoryTileSource,
    ids: &[LayerId],
    canvas: DocRect,
    depth: usize,
    tally: &mut Tally,
) -> Result<Vec<psd::PsdLayer>, ImportError> {
    if depth > MAX_PSD_GROUP_DEPTH {
        return Err(ImportError::PsdTooDeep {
            max: MAX_PSD_GROUP_DEPTH,
        });
    }
    // `ids` is top-most first; a `.psd` stores bottom-to-top.
    let mut out = Vec::with_capacity(ids.len());
    for &id in ids.iter().rev() {
        let Some(layer) = document.layers.get(id) else {
            continue;
        };
        let mut record = psd::PsdLayer::raster(&layer.name, psd::Rect::default());
        record.opacity = to_byte(layer.effective_opacity());
        let fill = layer.effective_fill_opacity();
        record.fill_opacity = (fill < 1.0).then(|| to_byte(fill));
        record.blend_mode = layer.blend_mode;
        record.visible = layer.visible;
        record.clipping = layer.is_clipping();
        record.protection = psd::Protection {
            transparency: layer.locked.transparency,
            composite: layer.locked.pixels,
            position: layer.locked.position,
        };
        record.transparency_protected = layer.locked.transparency;
        if layer.locked.all {
            tally.locked_all.push(layer.name.clone());
        }
        if !layer.effects.is_empty() {
            // Card 080: the four supported effects are written as real lfx2
            // descriptors — an independent reader can toggle and restyle
            // them. Kinds this writer cannot produce are named, as before.
            match psd::effects::export_effects(&layer.effects) {
                Some((data, unmapped)) => {
                    record.effects = Some(psd::Effects {
                        key: *b"lfx2",
                        data,
                    });
                    for kind in unmapped {
                        tally.unmapped_effects.push((layer.name.clone(), kind));
                    }
                }
                None => tally.effects.push(layer.name.clone()),
            }
        }

        let (dx, dy, expressible) = translation_of(layer.transform);
        let mut wants_pixels = false;
        let mut render_fallback = false;
        match &layer.kind {
            LayerKind::Group(group) => {
                let children =
                    psd_layers_for(document, tiles, &group.children, canvas, depth + 1, tally)?;
                let pass_through = group.blending == GroupBlending::PassThrough;
                if pass_through && layer.blend_mode != BlendMode::Normal {
                    tally.pass_through_blend.push(layer.name.clone());
                }
                record.kind = psd::LayerKind::Group(psd::GroupData {
                    children,
                    open: !group.collapsed,
                    pass_through,
                });
            }
            LayerKind::Adjustment(adjustment) => {
                record.pixel_data_irrelevant = true;
                if matches!(adjustment.kind, AdjustmentKind::Invert) {
                    record.adjustment = Some(psd::Adjustment {
                        key: *b"nvrt",
                        data: Vec::new(),
                    });
                } else {
                    // Card 078: an adjustment whose payload cannot be written
                    // gets NO invented pixels — an empty layer plus the note
                    // beats pixels that look evaluated but are not. Its
                    // appearance survives in the file's flattened preview.
                    tally.adjustments.push(layer.name.clone());
                }
            }
            LayerKind::Raster(_) | LayerKind::Generator(_) => wants_pixels = true,
            LayerKind::Text(_) | LayerKind::Shape(_) | LayerKind::SmartObject(_) => {
                // Card 078: these kinds carry editable content a .psd cannot
                // hold, but they can still LOOK right: the record's channels
                // get the layer's rendered appearance, with its real
                // transform, from the one compositor. (Card 079 adds the
                // editable text subset on top of this fallback.)
                tally.raster_fallback.push(layer.name.clone());
                wants_pixels = true;
                render_fallback = true;
            }
        }

        if wants_pixels {
            if render_fallback {
                // The record's channels must be the layer's CONTENT, not the
                // mask's result — the mask travels in its own channel below,
                // and writing both would make every reader apply it twice.
                // Render from a staged copy with the mask detached.
                let mut staged = document.clone();
                if let Some(stripped) = staged.layers.get_mut(id) {
                    stripped.mask = None;
                    // Card 080: the effects ride in the lfx2 descriptor, so
                    // the fallback pixels must not bake them too — a reader
                    // would draw every effect twice.
                    stripped.effects = layer_model::LayerEffects::default();
                }
                let canvas_rect = raster::PixelRect::new(0, 0, document.width(), document.height());
                let rendered = compositor::composite_subtree(
                    &staged,
                    tiles,
                    id,
                    canvas_rect,
                    0,
                    compositor::CompositeOptions::default(),
                )?;
                let rgba = rendered.to_rgba8(&document.meta.color_space);
                if let Some((bounds, cropped)) = crop_to_content(&rgba, canvas) {
                    record.bounds = bounds.to_psd();
                    record.set_rgba8(&cropped)?;
                }
            } else {
                if let Some(map) = document.layer_tiles(id) {
                    if !expressible {
                        tally.transformed.push(layer.name.clone());
                    }
                    let doc_area = tile_map_rect(map).offset(dx, dy).clip(canvas);
                    if !doc_area.is_empty() {
                        let source = doc_area.offset(-dx, -dy);
                        let rgba = rgba_from_tiles(map, tiles, source);
                        if let Some((bounds, cropped)) = crop_to_content(&rgba, doc_area) {
                            record.bounds = bounds.to_psd();
                            record.set_rgba8(&cropped)?;
                        }
                    }
                }
            }
        }

        if let Some(mask) = &layer.mask {
            if mask.kind == MaskKind::Vector {
                tally.vector_masks.push(layer.name.clone());
            }
            if mask.density() != 1.0 || mask.feather_px() != 0.0 {
                tally.mask_params.push(layer.name.clone());
            }
            // A linked mask travels with the layer; an unlinked one never moved.
            let (mdx, mdy) = if mask.linked { (dx, dy) } else { (0, 0) };
            if let Some(map) = document.pixels.tiles(PixelKey::Mask(mask.id)) {
                let doc_area = tile_map_rect(map).offset(mdx, mdy).clip(canvas);
                if !doc_area.is_empty() {
                    let source = doc_area.offset(-mdx, -mdy);
                    let mut written = psd::PsdMask::new(
                        doc_area.to_psd(),
                        coverage_from_tiles(map, tiles, source),
                    );
                    written.disabled = !mask.enabled;
                    written.invert = mask.inverted;
                    written.relative_to_layer = !mask.linked;
                    record.mask = Some(written);
                }
            }
        }

        out.push(record);
    }
    Ok(out)
}

/// Write `document` as a layered `.psd`, with `composite_rgba8` as the
/// flattened image every other reader shows.
///
/// The composite is passed in rather than derived here because this crate
/// already has the authoritative compositor behind [`crate::OpenDocument`];
/// letting the `psd` crate's fallback flattener produce one instead would put a
/// second, weaker compositor — one that ignores clipping, effects and
/// adjustments — into the save path.
pub fn psd_from_document(
    document: &Document,
    tiles: &MemoryTileSource,
    composite_rgba8: &[u8],
) -> Result<(Vec<u8>, PsdNotes), ImportError> {
    let (width, height) = (document.width(), document.height());
    if width == 0 || height == 0 || width > MAX_PSD_DIMENSION || height > MAX_PSD_DIMENSION {
        return Err(ImportError::PsdCanvas { width, height });
    }
    let expected = (width as usize) * (height as usize) * 4;
    if composite_rgba8.len() != expected {
        return Err(ImportError::PixelCount {
            expected,
            found: composite_rgba8.len(),
        });
    }

    let mut tally = Tally::default();
    let mut notes = PsdNotes::default();
    let canvas = DocRect::canvas(width, height);
    let mut file = psd::PsdFile::new(psd::PsdHeader::rgba8(width, height));
    file.merged = Some(psd::MergedImage::from_rgba8(
        width,
        height,
        composite_rgba8,
    )?);
    file.layers = psd_layers_for(
        document,
        tiles,
        document.layers.root(),
        canvas,
        0,
        &mut tally,
    )?;
    tally.record(&mut notes);
    Ok((psd::write(&file)?, notes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use compositor::{composite_region, CompositeOptions};
    use raster::{PixelRect, TILE_SIZE};

    /// A deterministic image with a different value in every channel of every
    /// pixel, so a transposed or shifted tile cannot pass by accident.
    fn probe_image(width: u32, height: u32) -> DecodedImage {
        let mut rgba8 = vec![0u8; (width as usize) * (height as usize) * 4];
        for y in 0..height {
            for x in 0..width {
                let i = ((y * width + x) * 4) as usize;
                rgba8[i] = (x % 251) as u8;
                rgba8[i + 1] = (y % 241) as u8;
                rgba8[i + 2] = ((x * 7 + y * 13) % 239) as u8;
                rgba8[i + 3] = 255;
            }
        }
        DecodedImage {
            width,
            height,
            rgba8,
            color_space: color::ColorSpace::Srgb,
            icc_profile: None,
        }
    }

    #[test]
    fn opening_an_image_produces_exactly_one_raster_layer() {
        let image = probe_image(300, 200);
        let imported = document_from_image(&image, "photo.png", 100).unwrap();

        assert_eq!(imported.document.layers.len(), 1);
        assert_eq!(imported.document.layers.root().len(), 1);
        let id = imported.document.layers.root()[0];
        assert_eq!(id, imported.layer);
        let layer = imported.document.layers.get(id).unwrap();
        assert!(
            matches!(layer.kind, layer_model::LayerKind::Raster(_)),
            "the image must be a raster layer, got {:?}",
            layer.kind
        );
        assert_eq!(layer.name, "photo.png");
        assert_eq!(imported.document.active_layer(), Some(id));
        assert_eq!(
            (imported.document.width(), imported.document.height()),
            (300, 200)
        );
        assert!(
            !imported.document.is_dirty(),
            "a just-opened file is not unsaved work"
        );
    }

    #[test]
    fn the_layers_tiles_are_the_source_pixels() {
        // 300x200 is deliberately not a multiple of TILE_SIZE: the edge tiles
        // are padded, and padding must not be mistaken for image content.
        let image = probe_image(300, 200);
        let imported = document_from_image(&image, "photo.png", 100).unwrap();

        let expected = TileGrid::from_rgba8(300, 200, &image.rgba8).unwrap();
        let map = imported
            .document
            .layer_tiles(imported.layer)
            .expect("the layer owns pixels");
        assert_eq!(map.len(), expected.len(), "one stored tile per grid tile");
        assert!(map.len() >= 2, "the probe must span several tiles");

        for (coord, tile) in expected.iter() {
            let hash = map.get(coord).expect("every grid tile is referenced");
            let bytes = compositor::TileSource::tile(&imported.tiles, hash)
                .expect("the hash resolves in the tile source");
            assert_eq!(
                bytes,
                tile.data(),
                "tile {coord:?} does not hold the source pixels"
            );
        }
    }

    #[test]
    fn compositing_the_document_reproduces_the_image() {
        // The end-to-end claim: what the canvas draws is the document, and the
        // document *is* the picture that was opened.
        let image = probe_image(300, 200);
        let imported = document_from_image(&image, "photo.png", 100).unwrap();

        let out = composite_region(
            &imported.document,
            &imported.tiles,
            PixelRect::new(0, 0, 300, 200),
            0,
            CompositeOptions::default(),
        )
        .unwrap();
        let rgba8 = out.to_rgba8(&imported.document.meta.color_space);
        assert_eq!(rgba8.len(), image.rgba8.len());

        // The compositor works in linear premultiplied f32 and encodes back to
        // 8 bit, so a value may move by one quantisation step; nothing more.
        let worst = rgba8
            .iter()
            .zip(&image.rgba8)
            .map(|(a, b)| (*a as i32 - *b as i32).abs())
            .max()
            .unwrap();
        assert!(worst <= 1, "composite differs from the source by {worst}");
    }

    #[test]
    fn an_image_smaller_than_one_tile_still_round_trips() {
        let image = probe_image(7, 3);
        let imported = document_from_image(&image, "tiny.png", 10).unwrap();
        let map = imported.document.layer_tiles(imported.layer).unwrap();
        assert_eq!(map.len(), 1);

        let out = composite_region(
            &imported.document,
            &imported.tiles,
            PixelRect::new(0, 0, 7, 3),
            0,
            CompositeOptions::default(),
        )
        .unwrap();
        let rgba8 = out.to_rgba8(&imported.document.meta.color_space);
        let worst = rgba8
            .iter()
            .zip(&image.rgba8)
            .map(|(a, b)| (*a as i32 - *b as i32).abs())
            .max()
            .unwrap();
        assert!(worst <= 1, "differs by {worst}");
    }

    #[test]
    fn an_exactly_tiled_image_has_no_padding_at_all() {
        let image = probe_image(TILE_SIZE, TILE_SIZE);
        let imported = document_from_image(&image, "square.png", 10).unwrap();
        let map = imported.document.layer_tiles(imported.layer).unwrap();
        assert_eq!(map.len(), 1);
        let hash = map.get(TileCoord::new(0, 0, 0)).unwrap();
        let bytes = compositor::TileSource::tile(&imported.tiles, hash).unwrap();
        assert_eq!(bytes, image.rgba8.as_slice());
    }

    #[test]
    fn identical_tiles_are_stored_once() {
        // Content addressing is the reason the tile source is a hash map: a
        // flat image is one blob however many tiles reference it.
        let image = DecodedImage {
            width: TILE_SIZE * 2,
            height: TILE_SIZE * 2,
            rgba8: vec![200u8; (TILE_SIZE as usize * 2) * (TILE_SIZE as usize * 2) * 4],
            color_space: color::ColorSpace::Srgb,
            icc_profile: None,
        };
        let imported = document_from_image(&image, "flat.png", 10).unwrap();
        assert_eq!(
            imported.document.layer_tiles(imported.layer).unwrap().len(),
            4,
            "four tile references"
        );
        assert_eq!(imported.tiles.len(), 1, "one distinct blob");
    }

    #[test]
    fn the_import_is_one_undoable_step_when_it_is_not_the_whole_document() {
        // `document_from_image` clears history (there is nothing sensible to
        // undo to), but placing an image into an open document must be one
        // step that takes the pixels with it.
        let mut tiles = MemoryTileSource::new();
        let image = probe_image(300, 200);
        let (cmd, layer) = import_command(&image, "placed", &mut tiles).unwrap();

        let mut doc = Document::new(400, 400, "canvas");
        let mut history = History::new();
        history.apply(&mut doc, cmd).unwrap();
        assert_eq!(doc.layers.len(), 1);
        assert!(doc.layer_tiles(layer).is_some());
        assert_eq!(history.undo_depth(), 1, "one history entry, not two");

        history.undo(&mut doc).unwrap();
        assert_eq!(doc.layers.len(), 0);
        assert!(
            doc.layer_tiles(layer).is_none(),
            "undo must take the pixels with the layer"
        );
    }

    #[test]
    fn a_degenerate_image_is_refused_with_a_reason() {
        let mut tiles = MemoryTileSource::new();
        let empty = DecodedImage {
            width: 0,
            height: 10,
            rgba8: Vec::new(),
            color_space: color::ColorSpace::Srgb,
            icc_profile: None,
        };
        let err = import_command(&empty, "x", &mut tiles).unwrap_err();
        assert!(err.to_string().contains("non-zero"), "{err}");

        let short = DecodedImage {
            width: 4,
            height: 4,
            rgba8: vec![0; 4],
            color_space: color::ColorSpace::Srgb,
            icc_profile: None,
        };
        let err = import_command(&short, "x", &mut tiles).unwrap_err();
        assert!(err.to_string().contains("RGBA8"), "{err}");
        assert!(tiles.is_empty(), "a refusal must store nothing");
    }

    #[test]
    fn a_blank_document_starts_with_one_empty_raster_layer() {
        let d = blank_document(800, 600, "Untitled", 50, BlankBackground::Transparent).unwrap();
        assert_eq!(d.document.layers.len(), 1);
        assert_eq!(d.document.active_layer(), Some(d.layer));
        assert!(d.document.layer_tiles(d.layer).is_none(), "no pixels yet");
        assert!(!d.document.is_dirty());
        assert!(blank_document(0, 10, "x", 1, BlankBackground::Transparent).is_err());
    }

    #[test]
    fn the_layer_tile_coords_cover_the_canvas() {
        let image = probe_image(300, 200);
        let imported = document_from_image(&image, "photo.png", 10).unwrap();
        let coords = layer_tile_coords(&imported.document, imported.layer);
        assert_eq!(coords.len(), 2, "300x200 spans two tiles across");
        assert!(coords.iter().all(|c| c.level == 0));
        assert!(layer_tile_coords(&imported.document, LayerId::new()).is_empty());
    }

    #[test]
    fn the_document_title_comes_from_the_file_name() {
        assert_eq!(
            DecodedImage::title_for(Path::new("/photos/holiday.png")),
            "holiday.png"
        );
        assert_eq!(DecodedImage::title_for(Path::new("/")), "Untitled");
    }

    // ------------------------------------------------------------------ PSD

    /// Deliberately not a multiple of `TILE_SIZE`, and wide enough to span two
    /// tiles: an importer that only ever exercises one tile proves nothing
    /// about placement.
    const PW: u32 = 300;
    const PH: u32 = 200;

    const RED: [u8; 4] = [200, 30, 30, 255];
    const GREEN: [u8; 4] = [30, 200, 60, 255];
    const BLUE: [u8; 4] = [10, 20, 240, 255];

    fn solid(rect: psd::Rect, rgba: [u8; 4]) -> Vec<u8> {
        rgba.repeat((rect.width() * rect.height()) as usize)
    }

    /// The mask rectangle in [`layered_psd`], and a coverage ramp over it that
    /// a flip or a transpose could not survive.
    const MASK_RECT: psd::Rect = psd::Rect {
        left: 10,
        top: 10,
        right: 110,
        bottom: 60,
    };

    fn mask_ramp() -> Vec<u8> {
        let (w, h) = (MASK_RECT.width() as usize, MASK_RECT.height() as usize);
        let mut out = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                out[y * w + x] = ((x * 2 + y) % 256) as u8;
            }
        }
        out
    }

    /// A layered fixture, written by this workspace's own `psd` writer — the
    /// same bytes Photoshop and Photopea are handed.
    ///
    /// Background at the bottom, a hidden Multiply group holding one clipped
    /// Screen layer, and a masked layer on top whose mask *shows* everything
    /// outside its own rectangle. Every property is set away from its default,
    /// so a field that failed to travel cannot pass by looking like the
    /// default it never left.
    fn layered_psd() -> Vec<u8> {
        let mut file = psd::PsdFile::new(psd::PsdHeader::rgba8(PW, PH));

        let canvas = psd::Rect::sized(PW, PH);
        let mut background = psd::PsdLayer::raster("Background", canvas);
        background.set_rgba8(&solid(canvas, RED)).unwrap();

        let inner_rect = psd::Rect::new(40, 20, 180, 150);
        let mut inner = psd::PsdLayer::raster("Inner", inner_rect);
        inner.set_rgba8(&solid(inner_rect, GREEN)).unwrap();
        inner.blend_mode = layer_model::BlendMode::Screen;
        inner.opacity = 200;
        inner.fill_opacity = Some(128);
        inner.clipping = true;
        inner.protection = psd::Protection {
            transparency: true,
            composite: false,
            position: true,
        };

        let mut group = psd::PsdLayer::group("Grp");
        group.blend_mode = layer_model::BlendMode::Multiply;
        group.opacity = 128;
        group.visible = false;
        group.push_child(inner).unwrap();

        let mut masked = psd::PsdLayer::raster("Masked", canvas);
        masked.set_rgba8(&solid(canvas, BLUE)).unwrap();
        let mut mask = psd::PsdMask::new(MASK_RECT, mask_ramp());
        // "Show everything outside the box" — the value that turns into "hide
        // everything outside the box" if the default colour is ignored.
        mask.default_color = 255;
        mask.invert = true;
        masked.mask = Some(mask);

        // Bottom-to-top, as the format stores them.
        file.layers = vec![background, group, masked];
        psd::write(&file).expect("the fixture must be writable")
    }

    /// The card-075 `lfx2` fixture, built through the public descriptor API:
    /// the four required effects (the outer glow deliberately disabled) plus
    /// a satin kind this build does not model, at 150 % scale. Values match
    /// the psd crate's card-073 builder so the two fixtures stay in step.
    fn card075_lfx2() -> Vec<u8> {
        let unit = |unit: &str, value: f64| psd::Value::UnitFloat {
            unit: unit.as_bytes().try_into().unwrap(),
            value,
        };
        let rgb = |r: f64, g: f64, b: f64| {
            let mut c = psd::Descriptor::new("RGBC");
            c.push("Rd  ", psd::Value::Double(r)).unwrap();
            c.push("Grn ", psd::Value::Double(g)).unwrap();
            c.push("Bl  ", psd::Value::Double(b)).unwrap();
            psd::Value::Descriptor(c)
        };
        let blnm = |v: &str| psd::Value::Enumerated {
            type_id: "BlnM".into(),
            value: v.into(),
        };
        let enumerated = |ty: &str, v: &str| psd::Value::Enumerated {
            type_id: ty.into(),
            value: v.into(),
        };

        let mut s = psd::bytes::Sink::new();
        s.u32(1);
        s.u32(16);
        let mut top = psd::Descriptor::new("Lfx2");
        top.push("masterFXSwitch", psd::Value::Bool(true)).unwrap();
        top.push("Scl ", unit("#Prc", 150.0)).unwrap();

        let mut drsh = psd::Descriptor::new("DrSh");
        drsh.push("enab", psd::Value::Bool(true)).unwrap();
        drsh.push("Md  ", blnm("Mltp")).unwrap();
        drsh.push("Clr ", rgb(0.0, 0.0, 0.0)).unwrap();
        drsh.push("opacity", unit("#Prc", 75.0)).unwrap();
        drsh.push("lagl", unit("#Ang", 130.0)).unwrap();
        drsh.push("uglg", psd::Value::Bool(false)).unwrap();
        drsh.push("Dstn", unit("#Pxl", 8.0)).unwrap();
        drsh.push("blur", unit("#Pxl", 16.0)).unwrap();
        drsh.push("Ckmt", unit("#Pxl", 4.0)).unwrap();
        drsh.push("layerConceals", psd::Value::Bool(false)).unwrap();
        top.push("DrSh", psd::Value::Descriptor(drsh)).unwrap();

        let mut frfx = psd::Descriptor::new("FrFX");
        frfx.push("enab", psd::Value::Bool(true)).unwrap();
        frfx.push("Md  ", blnm("Nrml")).unwrap();
        frfx.push("Clr ", rgb(255.0, 255.0, 255.0)).unwrap();
        frfx.push("Opct", unit("#Prc", 100.0)).unwrap();
        frfx.push("Sz  ", unit("#Pxl", 4.0)).unwrap();
        frfx.push("PntT", enumerated("FrFl", "SClr")).unwrap();
        frfx.push("Styl", enumerated("FStl", "OutF")).unwrap();
        top.push("FrFX", psd::Value::Descriptor(frfx)).unwrap();

        let mut sofi = psd::Descriptor::new("SoFi");
        sofi.push("enab", psd::Value::Bool(true)).unwrap();
        sofi.push("Md  ", blnm("Clr ")).unwrap();
        sofi.push("Clr ", rgb(220.0, 60.0, 30.0)).unwrap();
        sofi.push("Opct", unit("#Prc", 50.0)).unwrap();
        top.push("SoFi", psd::Value::Descriptor(sofi)).unwrap();

        let mut orgl = psd::Descriptor::new("OrGl");
        orgl.push("enab", psd::Value::Bool(false)).unwrap();
        orgl.push("Md  ", blnm("Scrn")).unwrap();
        orgl.push("Clr ", rgb(255.0, 255.0, 0.0)).unwrap();
        orgl.push("Opct", unit("#Prc", 60.0)).unwrap();
        orgl.push("blur", unit("#Pxl", 10.0)).unwrap();
        top.push("OrGl", psd::Value::Descriptor(orgl)).unwrap();

        top.push("ChFX", psd::Value::Descriptor(psd::Descriptor::new("ChFX")))
            .unwrap();
        top.write(&mut s).unwrap();
        s.into_inner()
    }

    /// A PSD whose one layer carries [`card075_lfx2`].
    fn styled_psd() -> Vec<u8> {
        let mut file = psd::PsdFile::new(psd::PsdHeader::rgba8(64, 64));
        let canvas = psd::Rect::sized(64, 64);
        let mut styled = psd::PsdLayer::raster("Styled", canvas);
        styled.set_rgba8(&solid(canvas, GREEN)).unwrap();
        styled.effects = Some(psd::Effects {
            key: *b"lfx2",
            data: card075_lfx2(),
        });
        file.layers = vec![styled];
        psd::write(&file).expect("the fixture must be writable")
    }

    fn names_of(doc: &Document, ids: &[LayerId]) -> Vec<String> {
        ids.iter()
            .filter_map(|id| doc.layers.get(*id).map(|l| l.name.clone()))
            .collect()
    }

    fn find(doc: &Document, name: &str) -> LayerId {
        doc.layers
            .iter_depth_first()
            .into_iter()
            .find(|id| doc.layers.get(*id).is_some_and(|l| l.name == name))
            .unwrap_or_else(|| panic!("no layer called {name}"))
    }

    /// One stored pixel of a layer, in document coordinates. Absent tiles read
    /// as fully transparent, which is what the compositor does.
    fn stored_pixel(
        doc: &Document,
        src: &MemoryTileSource,
        layer: LayerId,
        x: u32,
        y: u32,
    ) -> [u8; 4] {
        let Some(map) = doc.layer_tiles(layer) else {
            return [0; 4];
        };
        let coord = TileCoord::new((x / TILE_SIZE) as i32, (y / TILE_SIZE) as i32, 0);
        let Some(hash) = map.get(coord) else {
            return [0; 4];
        };
        let data = compositor::TileSource::tile(src, hash).expect("the hash resolves");
        let i = (((y % TILE_SIZE) * TILE_SIZE + (x % TILE_SIZE)) * 4) as usize;
        [data[i], data[i + 1], data[i + 2], data[i + 3]]
    }

    /// [`stored_pixel`] at signed coordinates — the off-canvas tiles that card
    /// 076 now keeps live exactly here.
    fn stored_pixel_signed(
        doc: &Document,
        src: &MemoryTileSource,
        layer: LayerId,
        x: i64,
        y: i64,
    ) -> [u8; 4] {
        let Some(map) = doc.layer_tiles(layer) else {
            return [0; 4];
        };
        let coord = TileCoord::new(
            x.div_euclid(i64::from(TILE_SIZE)) as i32,
            y.div_euclid(i64::from(TILE_SIZE)) as i32,
            0,
        );
        let Some(hash) = map.get(coord) else {
            return [0; 4];
        };
        let data = compositor::TileSource::tile(src, hash).expect("the hash resolves");
        let lx = x.rem_euclid(i64::from(TILE_SIZE)) as usize;
        let ly = y.rem_euclid(i64::from(TILE_SIZE)) as usize;
        let i = (ly * TILE_SIZE as usize + lx) * 4;
        [data[i], data[i + 1], data[i + 2], data[i + 3]]
    }

    /// One stored mask coverage sample, in document coordinates. Absent tiles
    /// read as zero — the layer fully hidden.
    fn stored_coverage(
        doc: &Document,
        src: &MemoryTileSource,
        layer: LayerId,
        x: u32,
        y: u32,
    ) -> u8 {
        let Some(map) = doc.mask_tiles(layer) else {
            return 0;
        };
        let coord = TileCoord::new((x / TILE_SIZE) as i32, (y / TILE_SIZE) as i32, 0);
        let Some(hash) = map.get(coord) else {
            return 0;
        };
        let data = compositor::TileSource::tile(src, hash).expect("the hash resolves");
        data[((y % TILE_SIZE) * TILE_SIZE + (x % TILE_SIZE)) as usize]
    }

    #[test]
    fn a_psd_opens_as_a_layer_tree_rather_than_a_flattened_picture() {
        let import = document_from_psd(&layered_psd(), "fixture.psd", 50).unwrap();
        let doc = &import.imported.document;
        assert_eq!((doc.width(), doc.height()), (PW, PH));

        // A `.psd` is stored bottom-to-top; the panel lists top-most first.
        let root = doc.layers.root().to_vec();
        assert_eq!(names_of(doc, &root), ["Masked", "Grp", "Background"]);

        let group_id = root[1];
        let group = doc.layers.get(group_id).unwrap();
        assert!(group.is_group(), "a group divider must rebuild a group");
        assert_eq!(group.blend_mode, layer_model::BlendMode::Multiply);
        assert!(!group.visible, "the group's hidden flag travelled");
        assert!((group.opacity - 128.0 / 255.0).abs() < 1e-6);
        assert_eq!(names_of(doc, group.children()), ["Inner"]);

        let inner_id = group.children()[0];
        let inner = doc.layers.get(inner_id).unwrap();
        assert_eq!(inner.blend_mode, layer_model::BlendMode::Screen);
        assert!((inner.opacity - 200.0 / 255.0).abs() < 1e-6);
        assert!((inner.fill_opacity - 128.0 / 255.0).abs() < 1e-6);
        assert!(inner.is_clipping(), "the clipping flag travelled");
        assert!(inner.locked.transparency && inner.locked.position);
        assert!(!inner.locked.pixels);

        // ...and the pixels are where the layer rectangle put them, not
        // smeared across the canvas.
        let tiles = &import.imported.tiles;
        assert_eq!(stored_pixel(doc, tiles, inner_id, 50, 30), GREEN);
        assert_eq!(stored_pixel(doc, tiles, inner_id, 179, 149), GREEN);
        assert_eq!(
            stored_pixel(doc, tiles, inner_id, 5, 5),
            [0; 4],
            "outside its rectangle the layer has nothing"
        );
        assert_eq!(stored_pixel(doc, tiles, inner_id, 180, 30), [0; 4]);
        assert_eq!(
            stored_pixel(doc, tiles, find(doc, "Background"), 290, 190),
            RED
        );
        assert_eq!(stored_pixel(doc, tiles, root[0], 290, 190), BLUE);

        // The mask: its ramp inside its own box, its default colour outside.
        let masked = doc.layers.get(root[0]).unwrap();
        let mask = masked.mask.as_ref().expect("the mask travelled");
        assert!(mask.enabled);
        assert!(mask.inverted, "the mask's invert flag travelled");
        assert_eq!(stored_coverage(doc, tiles, root[0], 15, 15), 15);
        // The far corner of the ramp: (99 * 2 + 49) % 256.
        assert_eq!(stored_coverage(doc, tiles, root[0], 109, 59), 247);
        assert_eq!(
            stored_coverage(doc, tiles, root[0], 250, 150),
            255,
            "a default colour of 255 shows everything outside the mask's box"
        );

        // Opening is not an edit, and there is nothing behind it to undo to.
        assert!(!doc.is_dirty());
        assert_eq!(import.imported.history.undo_depth(), 0);
        assert!(doc.active_layer().is_some());
        assert!(
            !doc.layers
                .get(doc.active_layer().unwrap())
                .unwrap()
                .is_group(),
            "the active layer must be one a tool can paint on"
        );
    }

    #[test]
    fn ignoring_a_masks_default_colour_would_be_visible_here() {
        // Mutation guard for the one line that is easy to drop: with the
        // default colour ignored, everything outside the mask's own rectangle
        // reads as zero coverage and the layer vanishes from most of the
        // canvas. 255 and 0 are the two answers, and they are opposite.
        let import = document_from_psd(&layered_psd(), "fixture.psd", 50).unwrap();
        let doc = &import.imported.document;
        let masked = doc.layers.root()[0];
        assert_ne!(
            stored_coverage(doc, &import.imported.tiles, masked, 250, 150),
            0
        );
    }

    /// Composite the whole canvas, the way the canvas view does.
    fn flatten(doc: &Document, tiles: &MemoryTileSource) -> Vec<u8> {
        composite_region(
            doc,
            tiles,
            PixelRect::new(0, 0, doc.width(), doc.height()),
            0,
            CompositeOptions::default(),
        )
        .unwrap()
        .to_rgba8(&doc.meta.color_space)
    }

    #[test]
    fn a_document_saved_as_a_psd_reopens_with_its_structure_intact() {
        let first = document_from_psd(&layered_psd(), "fixture.psd", 50).unwrap();
        let doc = &first.imported.document;
        let composite = flatten(doc, &first.imported.tiles);

        let (bytes, notes) = psd_from_document(doc, &first.imported.tiles, &composite).unwrap();
        assert!(notes.is_empty(), "nothing should have been lost: {notes:?}");
        // What we wrote is a real `.psd` by every check the reader makes.
        assert_eq!(&bytes[..4], b"8BPS");

        let again = document_from_psd(&bytes, "again.psd", 50).unwrap();
        let back = &again.imported.document;
        let tiles = &again.imported.tiles;

        assert_eq!((back.width(), back.height()), (PW, PH));
        let root = back.layers.root().to_vec();
        assert_eq!(names_of(back, &root), ["Masked", "Grp", "Background"]);

        let group = back.layers.get(root[1]).unwrap();
        assert!(group.is_group());
        assert_eq!(group.blend_mode, layer_model::BlendMode::Multiply);
        assert!(!group.visible);
        assert!((group.opacity - 128.0 / 255.0).abs() < 1e-6);
        assert_eq!(names_of(back, group.children()), ["Inner"]);

        let inner = back.layers.get(group.children()[0]).unwrap();
        assert_eq!(inner.blend_mode, layer_model::BlendMode::Screen);
        assert!(inner.is_clipping());
        assert!((inner.opacity - 200.0 / 255.0).abs() < 1e-6);
        assert!((inner.fill_opacity - 128.0 / 255.0).abs() < 1e-6);
        assert!(inner.locked.transparency && inner.locked.position);

        // Pixels, at document coordinates, in every layer.
        let inner_id = group.children()[0];
        assert_eq!(stored_pixel(back, tiles, inner_id, 50, 30), GREEN);
        assert_eq!(stored_pixel(back, tiles, inner_id, 179, 149), GREEN);
        assert_eq!(stored_pixel(back, tiles, inner_id, 5, 5), [0; 4]);
        assert_eq!(
            stored_pixel(back, tiles, find(back, "Background"), 290, 190),
            RED
        );
        assert_eq!(stored_pixel(back, tiles, root[0], 4, 4), BLUE);

        // The mask survived, ramp and all.
        assert!(back.layers.get(root[0]).unwrap().mask.is_some());
        assert_eq!(stored_coverage(back, tiles, root[0], 15, 15), 15);
        assert_eq!(stored_coverage(back, tiles, root[0], 250, 150), 255);

        // And the whole thing still composites to the same picture.
        assert_eq!(flatten(back, tiles), composite);
    }

    #[test]
    fn a_saved_psd_gives_its_layers_their_own_bounding_boxes() {
        // Tiles are 256 pixels square, so the rectangle a layer's tiles cover
        // is rounded out to tile boundaries. Writing that as the layer's `.psd`
        // rectangle makes Photoshop draw a marquee around empty space, so the
        // exporter crops to the content instead.
        let first = document_from_psd(&layered_psd(), "fixture.psd", 50).unwrap();
        let composite = flatten(&first.imported.document, &first.imported.tiles);
        let (bytes, _) =
            psd_from_document(&first.imported.document, &first.imported.tiles, &composite).unwrap();

        let file = psd::read(&bytes).unwrap();
        let inner = file
            .all_layers()
            .into_iter()
            .find(|l| l.name == "Inner")
            .expect("the inner layer survived");
        assert_eq!(
            inner.bounds,
            psd::Rect::new(40, 20, 180, 150),
            "the layer rectangle must be the content's, not the tile grid's"
        );
    }

    #[test]
    fn a_truncated_or_corrupt_psd_is_an_error_rather_than_a_blank_document() {
        let good = layered_psd();
        // Every prefix: a header cut in half, a header with no sections, a
        // layer section that stops mid-record, a file cut inside the composite.
        for cut in [0, 3, 13, 26, 40, 120, good.len() / 2, good.len() - 1] {
            let err = document_from_psd(&good[..cut], "cut.psd", 10)
                .expect_err("a truncated .psd must not open");
            assert!(
                matches!(err, ImportError::Psd(_)),
                "cut at {cut} gave {err}"
            );
        }

        // A plausible header over nonsense.
        let mut lying = good[..26].to_vec();
        lying.extend_from_slice(&[0xFF; 64]);
        assert!(document_from_psd(&lying, "lying.psd", 10).is_err());

        // Not a `.psd` at all.
        assert!(document_from_psd(b"not a psd at all", "x.psd", 10).is_err());
        assert!(document_from_psd(&[], "x.psd", 10).is_err());

        // A byte flipped in the middle either errors or reads; what it must
        // never do is panic, and it must never yield a document with no
        // layers, which is what "opened blank" looks like from the outside.
        for at in [30, 200, good.len() - 30] {
            let mut damaged = good.clone();
            damaged[at] ^= 0xFF;
            if let Ok(import) = document_from_psd(&damaged, "damaged.psd", 10) {
                assert!(
                    !import.imported.document.layers.is_empty(),
                    "byte {at}: a document that opens must hold something"
                );
            }
        }
    }

    #[test]
    fn a_flattened_psd_still_opens_as_one_layer_and_says_so() {
        let mut file = psd::PsdFile::new(psd::PsdHeader::rgba8(PW, PH));
        let canvas = psd::Rect::sized(PW, PH);
        file.merged = Some(psd::MergedImage::from_rgba8(PW, PH, &solid(canvas, BLUE)).unwrap());
        let bytes = psd::write(&file).unwrap();

        let import = document_from_psd(&bytes, "flat.psd", 10).unwrap();
        let doc = &import.imported.document;
        assert_eq!(doc.layers.len(), 1);
        let id = doc.layers.root()[0];
        assert_eq!(doc.layers.get(id).unwrap().name, "Background");
        assert_eq!(
            stored_pixel(doc, &import.imported.tiles, id, 290, 190),
            BLUE
        );
        assert!(
            import
                .notes
                .summary()
                .is_some_and(|s| s.contains("no layers")),
            "the user is told the file was flat: {:?}",
            import.notes
        );
    }

    #[test]
    fn what_a_psd_carries_and_this_document_cannot_is_reported_not_dropped() {
        let mut file = psd::PsdFile::new(psd::PsdHeader::rgba8(64, 64));
        let canvas = psd::Rect::sized(64, 64);

        let mut base = psd::PsdLayer::raster("Base", canvas);
        base.set_rgba8(&solid(canvas, RED)).unwrap();

        // An adjustment whose parameters this build has no vocabulary for.
        let mut curves = psd::PsdLayer::raster("Curves 1", psd::Rect::default());
        curves.adjustment = Some(psd::Adjustment {
            key: *b"curv",
            data: vec![0; 8],
        });
        curves.pixel_data_irrelevant = true;

        // ...and one it does: Invert has no parameters at all.
        let mut invert = psd::PsdLayer::raster("Invert 1", psd::Rect::default());
        invert.adjustment = Some(psd::Adjustment {
            key: *b"nvrt",
            data: Vec::new(),
        });
        invert.pixel_data_irrelevant = true;

        let mut styled = psd::PsdLayer::raster("Styled", canvas);
        styled.set_rgba8(&solid(canvas, GREEN)).unwrap();
        styled.effects = Some(psd::Effects {
            key: *b"lfx2",
            data: vec![0; 16],
        });
        styled.sheet_color = Some(2);

        file.layers = vec![base, curves, invert, styled];
        let bytes = psd::write(&file).unwrap();

        let import = document_from_psd(&bytes, "notes.psd", 10).unwrap();
        let doc = &import.imported.document;
        let told = import
            .notes
            .summary()
            .expect("this file loses things, so it must say so");

        assert!(told.contains("Curves 1"), "{told}");
        assert!(told.contains("curv"), "the key is named: {told}");
        assert!(!told.contains("Invert 1"), "Invert maps exactly: {told}");
        assert!(told.contains("Styled"), "effects and label: {told}");
        assert!(told.contains("effect"), "{told}");
        assert!(told.contains("colour label"), "{told}");

        // Invert really did become an editable adjustment layer.
        let invert = doc.layers.get(find(doc, "Invert 1")).unwrap();
        assert!(matches!(
            &invert.kind,
            LayerKind::Adjustment(a) if a.kind == AdjustmentKind::Invert
        ));
        // ...and the one that could not be mapped is still in the tree, so the
        // user can see it is there rather than wonder where it went.
        assert!(doc.layers.get(find(doc, "Curves 1")).is_some());
        assert_eq!(doc.layers.len(), 4);
    }

    // ------------------------------------------------------- card 077

    /// Card 077's acceptance fixture: one clean layer, one adjustment this
    /// build cannot evaluate, one exact mapping, and one layer whose named
    /// effect is dropped.
    fn mixed_fidelity_psd() -> Vec<u8> {
        let mut file = psd::PsdFile::new(psd::PsdHeader::rgba8(64, 64));
        let canvas = psd::Rect::sized(64, 64);

        let mut base = psd::PsdLayer::raster("Base", canvas);
        base.set_rgba8(&solid(canvas, RED)).unwrap();

        let mut curves = psd::PsdLayer::raster("Curves 1", psd::Rect::default());
        curves.adjustment = Some(psd::Adjustment {
            key: *b"curv",
            data: vec![0; 8],
        });
        curves.pixel_data_irrelevant = true;

        let mut styled = psd::PsdLayer::raster("Styled", canvas);
        styled.set_rgba8(&solid(canvas, GREEN)).unwrap();
        styled.effects = Some(psd::Effects {
            key: *b"lfx2",
            data: card075_lfx2(),
        });

        file.layers = vec![base, curves, styled];
        psd::write(&file).expect("the fixture must be writable")
    }

    #[test]
    fn the_fidelity_report_classifies_every_layer_and_says_what_to_do() {
        let import = document_from_psd(&mixed_fidelity_psd(), "mixed.psd", 10).unwrap();

        // Per-layer outcomes; the walk is bottom-to-top, so index by name.
        let layers = import.notes.layers();
        assert_eq!(layers.len(), 3, "every layer is in the report: {layers:?}");
        let of = |name: &str| layers.iter().find(|l| l.name == name).unwrap();
        let base = of("Base");
        assert_eq!(base.outcome, PsdLayerOutcome::Editable);
        assert!(base.detail.is_empty());
        let curves = of("Curves 1");
        assert_eq!(curves.outcome, PsdLayerOutcome::Unsupported);
        assert!(curves.detail.contains("empty layer"), "{layers:?}");
        let styled = of("Styled");
        assert!(styled.detail.contains("satin"), "{layers:?}");

        // The whole report: names the source, encourages native Save As.
        let report = import
            .notes
            .report(Some(Path::new("C:/art/mixed.psd")))
            .expect("a lossy import must produce a report");
        assert!(report.contains("C:/art/mixed.psd"), "{report}");
        assert!(report.contains(".rstudio"), "{report}");
        assert!(report.contains("Save As"), "{report}");
        assert!(report.contains("Base — editable"), "{report}");
        assert!(report.contains("Curves 1 — unsupported"), "{report}");
        assert!(report.contains("sat"), "{report}");
    }

    #[test]
    fn the_raster_fallback_is_actually_visible_not_a_report_about_nothing() {
        // The layer the report calls a fallback still carries its pixels:
        // the report explains a visual difference, it must be a real one.
        let import = document_from_psd(&mixed_fidelity_psd(), "mixed.psd", 10).unwrap();
        let doc = &import.imported.document;
        let tiles = &import.imported.tiles;
        let styled = find(doc, "Styled");
        assert_eq!(stored_pixel(doc, tiles, styled, 30, 30), GREEN);

        // The unsupported adjustment layer is honestly empty — and the report
        // says why, so the emptiness is explained rather than mysterious.
        let curves = find(doc, "Curves 1");
        assert!(matches!(
            &doc.layers.get(curves).unwrap().kind,
            LayerKind::Raster(_)
        ));
        assert_eq!(stored_pixel(doc, tiles, curves, 30, 30), [0; 4]);
    }

    #[test]
    fn a_fully_supported_file_is_quiet_and_every_layer_reads_editable() {
        let import = document_from_psd(&layered_psd(), "clean.psd", 50).unwrap();
        assert!(import.notes.is_empty(), "{:?}", import.notes);
        assert!(import.notes.report(None).is_none());
        for layer in import.notes.layers() {
            assert_eq!(layer.outcome, PsdLayerOutcome::Editable, "{layer:?}");
            assert!(layer.detail.is_empty(), "{layer:?}");
        }
    }

    #[test]
    fn the_merged_preview_is_kept_so_the_reconstruction_can_be_compared() {
        let import = document_from_psd(&layered_psd(), "preview.psd", 50).unwrap();
        let preview = import
            .merged_preview
            .as_ref()
            .expect("the file has a merged image");
        assert_eq!(preview.len(), PW as usize * PH as usize * 4);
        let diff = import
            .compare_merged_preview(8)
            .expect("a preview exists, so the comparison is offered");
        assert_eq!(diff.pixels, PW as usize * PH as usize);
        assert!(diff.differing <= diff.pixels);

        // A file with no composite offers no comparison at all.
        let mut import = import;
        import.merged_preview = None;
        assert!(import.compare_merged_preview(8).is_none());
    }

    #[test]
    fn the_four_required_effects_import_as_editable_parameters_and_the_rest_is_named() {
        let import = document_from_psd(&styled_psd(), "styled.psd", 10).unwrap();
        let doc = &import.imported.document;
        let styled = doc.layers.get(find(doc, "Styled")).unwrap();
        let e = &styled.effects;

        // Every mapped value, exactly as the fixture wrote them — colours
        // stored gamma-encoded in document space (decoded to linear at
        // render by the compositor), percentages in 0..1, pixels scaled by
        // 150 %, the
        // angle passed through unchanged (the compositor already uses
        // Photoshop's convention), the disabled glow absent.
        assert!(e.enabled, "the master switch is on");
        let s = e.drop_shadow.as_ref().expect("the drop shadow mapped");
        assert_eq!(s.blend_mode, BlendMode::Multiply);
        assert_eq!(s.color, [0.0, 0.0, 0.0, 1.0]);
        assert!((s.opacity - 0.75).abs() < 1e-6);
        assert!((s.angle_deg - 130.0).abs() < 1e-6);
        assert!(!s.use_global_light);
        assert!((s.distance_px - 12.0).abs() < 1e-6);
        assert!((s.size_px - 24.0).abs() < 1e-6);
        assert!((s.spread - 0.25).abs() < 1e-6);
        assert!((s.noise).abs() < 1e-6);
        assert!(!s.knockout);

        let k = e.stroke.as_ref().expect("the solid stroke mapped");
        assert_eq!(k.blend_mode, BlendMode::Normal);
        assert!((k.opacity - 1.0).abs() < 1e-6);
        assert!((k.size_px - 6.0).abs() < 1e-6);
        assert_eq!(k.position, layer_model::StrokePosition::Outside);
        assert!(!k.overprint);
        assert!(matches!(&k.fill, layer_model::FillStyle::Solid(c)
            if c.iter().zip([1.0f32, 1.0, 1.0, 1.0]).all(|(a, b)| (a - b).abs() < 1e-6)));

        let o = e.color_overlay.as_ref().expect("the colour overlay mapped");
        assert_eq!(o.blend_mode, BlendMode::Color);
        assert!((o.opacity - 0.5).abs() < 1e-6);
        assert!(matches!(&o.color, [r, g, b, 1.0]
            if (r - 220.0 / 255.0).abs() < 1e-3 && (g - 60.0 / 255.0).abs() < 1e-3 && (b - 30.0 / 255.0).abs() < 1e-3));

        assert!(e.outer_glow.is_none(), "a disabled effect is absent");

        // The honesty gate: the unmapped satin is named; the blanket
        // "layer effect(s) … were not imported" is gone.
        let told = import.notes.summary().expect("the satin must be named");
        assert!(
            told.contains("the satin effect(s) on \u{201c}Styled\u{201d} were not imported"),
            "{told}"
        );
        assert!(!told.contains("layer effect(s) on"), "{told}");

        // A native-package save/reopen preserves the imported effects.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("styled.rstudio");
        project_format::save_project_with(
            &path,
            &import.imported.document,
            &crate::doc::SourceTiles(&import.imported.tiles),
            &project_format::SaveOptions::new("test"),
        )
        .unwrap();
        let reopened = project_format::open_project(&path).unwrap().document;
        let back = reopened.layers.get(find(&reopened, "Styled")).unwrap();
        assert_eq!(back.effects, *e, "effects survive the native save");
    }

    #[test]
    fn a_layer_whose_effects_all_map_loses_the_effects_warning() {
        // The same fixture minus the satin and the disabled glow: every
        // effect left maps completely, so nothing is named at all.
        let mut file = psd::PsdFile::new(psd::PsdHeader::rgba8(64, 64));
        let canvas = psd::Rect::sized(64, 64);
        let mut styled = psd::PsdLayer::raster("Styled", canvas);
        styled.set_rgba8(&solid(canvas, GREEN)).unwrap();
        // Only a drop shadow and a colour overlay, both complete and enabled:
        // everything present maps, so nothing is named.
        let unit = |value: f64| psd::Value::UnitFloat {
            unit: *b"#Prc",
            value,
        };
        let mut s = psd::bytes::Sink::new();
        s.u32(1);
        s.u32(16);
        let mut top = psd::Descriptor::new("Lfx2");
        let mut drsh = psd::Descriptor::new("DrSh");
        drsh.push("enab", psd::Value::Bool(true)).unwrap();
        drsh.push(
            "Md  ",
            psd::Value::Enumerated {
                type_id: "BlnM".into(),
                value: "Mltp".into(),
            },
        )
        .unwrap();
        let mut black = psd::Descriptor::new("RGBC");
        black.push("Rd  ", psd::Value::Double(0.0)).unwrap();
        black.push("Grn ", psd::Value::Double(0.0)).unwrap();
        black.push("Bl  ", psd::Value::Double(0.0)).unwrap();
        drsh.push("Clr ", psd::Value::Descriptor(black)).unwrap();
        drsh.push("opacity", unit(75.0)).unwrap();
        drsh.push(
            "lagl",
            psd::Value::UnitFloat {
                unit: *b"#Ang",
                value: 120.0,
            },
        )
        .unwrap();
        drsh.push(
            "Dstn",
            psd::Value::UnitFloat {
                unit: *b"#Pxl",
                value: 8.0,
            },
        )
        .unwrap();
        drsh.push(
            "blur",
            psd::Value::UnitFloat {
                unit: *b"#Pxl",
                value: 16.0,
            },
        )
        .unwrap();
        top.push("DrSh", psd::Value::Descriptor(drsh)).unwrap();
        let mut sofi = psd::Descriptor::new("SoFi");
        sofi.push("enab", psd::Value::Bool(true)).unwrap();
        sofi.push(
            "Md  ",
            psd::Value::Enumerated {
                type_id: "BlnM".into(),
                value: "Nrml".into(),
            },
        )
        .unwrap();
        let mut red = psd::Descriptor::new("RGBC");
        red.push("Rd  ", psd::Value::Double(255.0)).unwrap();
        red.push("Grn ", psd::Value::Double(0.0)).unwrap();
        red.push("Bl  ", psd::Value::Double(0.0)).unwrap();
        sofi.push("Clr ", psd::Value::Descriptor(red)).unwrap();
        sofi.push("Opct", unit(100.0)).unwrap();
        top.push("SoFi", psd::Value::Descriptor(sofi)).unwrap();
        top.write(&mut s).unwrap();
        let data = s.into_inner();
        styled.effects = Some(psd::Effects {
            key: *b"lfx2",
            data,
        });
        file.layers = vec![styled];
        let bytes = psd::write(&file).unwrap();

        let import = document_from_psd(&bytes, "clean.psd", 10).unwrap();
        assert!(
            import.notes.is_empty(),
            "a fully mapped file says nothing: {:?}",
            import.notes
        );
        let doc = &import.imported.document;
        let styled = doc.layers.get(find(doc, "Styled")).unwrap();
        assert!(styled.effects.drop_shadow.is_some());
        assert_eq!(styled.effects.count(), 2);
    }

    #[test]
    fn a_deep_bit_depth_document_is_converted_and_the_user_is_told() {
        // A 16-bit document: every sample is two big-endian bytes, so a reader
        // that treats the planes as 8-bit would produce half-width garbage.
        let mut header = psd::PsdHeader::rgba8(4, 2);
        header.depth = psd::Depth::Sixteen;
        let mut file = psd::PsdFile::new(header);

        let rect = psd::Rect::sized(4, 2);
        let plane = |v: u16| -> Vec<u8> { v.to_be_bytes().repeat(8) };
        let mut layer = psd::PsdLayer::raster("Deep", rect);
        layer.channels = vec![
            psd::Channel::new(psd::CHANNEL_ALPHA, plane(0xFFFF)),
            psd::Channel::new(0, plane(0xFFFF)),
            psd::Channel::new(1, plane(0x8000)),
            psd::Channel::new(2, plane(0x0000)),
        ];
        file.layers = vec![layer];
        file.merged = Some(psd::MergedImage {
            channels: vec![plane(0xFFFF), plane(0x8000), plane(0), plane(0xFFFF)],
        });
        let bytes = psd::write(&file).unwrap();

        let import = document_from_psd(&bytes, "deep.psd", 10).unwrap();
        let doc = &import.imported.document;
        let id = doc.layers.root()[0];
        assert_eq!(
            stored_pixel(doc, &import.imported.tiles, id, 1, 1),
            [255, 128, 0, 255],
            "16-bit samples must round, not truncate"
        );
        assert!(
            import.notes.summary().is_some_and(|s| s.contains("16-bit")),
            "{:?}",
            import.notes
        );
    }

    /// Card 076: a layer hanging off the canvas keeps its FULL extent — the
    /// off-canvas ink is stored (negative tile coordinates) and nothing is
    /// reported, because nothing was dropped.
    #[test]
    fn a_psd_layer_hanging_off_the_canvas_keeps_its_full_extent() {
        let mut file = psd::PsdFile::new(psd::PsdHeader::rgba8(64, 64));
        let rect = psd::Rect::new(-20, -20, 30, 30);
        let mut over = psd::PsdLayer::raster("Over the edge", rect);
        over.set_rgba8(&solid(rect, GREEN)).unwrap();
        file.layers = vec![over];
        let bytes = psd::write(&file).unwrap();

        let import = document_from_psd(&bytes, "edge.psd", 10).unwrap();
        let doc = &import.imported.document;
        let id = doc.layers.root()[0];
        // The part inside the canvas landed at the right place...
        assert_eq!(stored_pixel(doc, &import.imported.tiles, id, 0, 0), GREEN);
        assert_eq!(stored_pixel(doc, &import.imported.tiles, id, 29, 29), GREEN);
        // ...and the part outside it is STILL THERE — document x −20 exists
        // in tile column −1, which the store addresses with signed coords.
        assert_eq!(
            stored_pixel_signed(doc, &import.imported.tiles, id, -20, -20),
            GREEN
        );
        assert_eq!(
            stored_pixel_signed(doc, &import.imported.tiles, id, -1, -1),
            GREEN
        );
        // No note: nothing was dropped, so nothing is named.
        assert!(import.notes.is_empty(), "{:?}", import.notes);
    }

    #[test]
    fn a_document_the_format_cannot_hold_is_refused_with_a_reason() {
        // A canvas past what a `.psd` can describe is a `.psb`, which nothing
        // here writes; saying so beats writing a file Photoshop cannot open.
        let doc = Document::new(MAX_PSD_DIMENSION + 1, 4, "huge");
        let err = psd_from_document(&doc, &MemoryTileSource::new(), &[]).unwrap_err();
        assert!(matches!(err, ImportError::PsdCanvas { .. }), "{err}");

        // A composite that is not the canvas is a caller bug, not a file one.
        let doc = Document::new(4, 4, "small");
        let err = psd_from_document(&doc, &MemoryTileSource::new(), &[0; 8]).unwrap_err();
        assert!(matches!(err, ImportError::PixelCount { .. }), "{err}");
    }

    #[test]
    fn a_transform_a_psd_cannot_express_is_reported_and_a_translation_is_folded_in() {
        // A `.psd` layer has a rectangle, not a matrix. An integer translation
        // is exactly a different rectangle; a rotation is a resample, and this
        // exporter will not make a destructive edit without saying so.
        let image = probe_image(64, 64);
        let mut imported = document_from_image(&image, "shifted", 10).unwrap();
        let id = imported.layer;
        imported.document.layers.get_mut(id).unwrap().transform =
            glam::Affine2::from_translation(glam::Vec2::new(10.0, 4.0));
        let composite = flatten(&imported.document, &imported.tiles);
        let (bytes, notes) =
            psd_from_document(&imported.document, &imported.tiles, &composite).unwrap();
        assert!(
            notes.is_empty(),
            "a whole-pixel move is expressible: {notes:?}"
        );
        let file = psd::read(&bytes).unwrap();
        assert_eq!(file.layers[0].bounds, psd::Rect::new(10, 4, 64, 64));

        let mut rotated = document_from_image(&image, "rotated", 10).unwrap();
        let id = rotated.layer;
        rotated.document.layers.get_mut(id).unwrap().transform = glam::Affine2::from_angle(0.5);
        let composite = flatten(&rotated.document, &rotated.tiles);
        let (_, notes) = psd_from_document(&rotated.document, &rotated.tiles, &composite).unwrap();
        assert!(
            notes.summary().is_some_and(|s| s.contains("transform")),
            "{notes:?}"
        );
    }

    #[test]
    fn reading_a_psd_off_disk_is_bounded_and_recognised_by_content() {
        let dir = tempfile::tempdir().unwrap();
        let psd_path = dir.path().join("real.psd");
        std::fs::write(&psd_path, layered_psd()).unwrap();
        assert!(looks_like_psd(&psd_path));
        assert_eq!(read_psd_bytes(&psd_path).unwrap(), layered_psd());

        // A PNG called `.psd` is still a PNG, and a `.psd` called `.png` is
        // still a document. The name decides nothing.
        let png = raster::encode(raster::ExportFormat::Png, 4, 4, &[128u8; 64]).unwrap();
        let lying_psd = dir.path().join("actually.psd");
        std::fs::write(&lying_psd, &png).unwrap();
        assert!(!looks_like_psd(&lying_psd));

        let lying_png = dir.path().join("actually.png");
        std::fs::write(&lying_png, layered_psd()).unwrap();
        assert!(looks_like_psd(&lying_png));

        // Nothing there, and something too short to have a signature.
        assert!(!looks_like_psd(&dir.path().join("nothing.psd")));
        let stub = dir.path().join("stub.psd");
        std::fs::write(&stub, b"8BP").unwrap();
        assert!(!looks_like_psd(&stub));
    }
    /// Card 076 — ICC retention: an embedded profile that is NOT sRGB
    /// (swapped primaries, identity tone curves — measurably not the sRGB
    /// transfer) is retained with the document: the pixels load verbatim and
    /// the profile bytes ride in `DocumentMeta::color_space`.
    fn swapped_primaries_profile() -> Vec<u8> {
        fn xyz(x: f64, y: f64, z: f64) -> Vec<u8> {
            let mut t = b"XYZ ".to_vec();
            t.extend_from_slice(&[0, 0, 0, 0]);
            for v in [x, y, z] {
                t.extend_from_slice(&((v * 65536.0).round() as i32).to_be_bytes());
            }
            t
        }
        fn identity_curve() -> Vec<u8> {
            let mut t = b"curv".to_vec();
            t.extend_from_slice(&[0, 0, 0, 0]);
            t.extend_from_slice(&[0, 0, 0, 0]); // count 0 = identity
            t
        }
        // sRGB's D50 primaries, red and green swapped: parses as a matrix
        // shaper, but is chromatically rotated away from sRGB.
        let sr = (0.4360, 0.2225, 0.0139);
        let sg = (0.3851, 0.7169, 0.0971);
        let sb = (0.1431, 0.0606, 0.7141);
        let mut out = vec![0u8; 128];
        out[4..8].copy_from_slice(b"acsp");
        out[12..16].copy_from_slice(b"mntr");
        out[16..20].copy_from_slice(b"RGB ");
        out[20..24].copy_from_slice(b"XYZ ");
        let tags: [(&[u8], Vec<u8>); 6] = [
            (b"rXYZ", xyz(sg.0, sg.1, sg.2)),
            (b"gXYZ", xyz(sr.0, sr.1, sr.2)),
            (b"bXYZ", xyz(sb.0, sb.1, sb.2)),
            (b"rTRC", identity_curve()),
            (b"gTRC", identity_curve()),
            (b"bTRC", identity_curve()),
        ];
        let mut offset: u32 = 132 + (tags.len() as u32) * 12;
        let mut table = Vec::new();
        for (sig, body) in &tags {
            table.extend_from_slice(sig);
            table.extend_from_slice(&offset.to_be_bytes());
            table.extend_from_slice(&(body.len() as u32).to_be_bytes());
            out.extend_from_slice(body);
            offset += body.len() as u32;
        }
        out.splice(128..128, table);
        out[128..132].copy_from_slice(&(tags.len() as u32).to_be_bytes());
        out
    }

    #[test]
    fn an_embedded_non_srgb_profile_is_retained_with_the_document() {
        let mut file = psd::PsdFile::new(psd::PsdHeader::rgba8(PW, PH));
        let canvas = psd::Rect::sized(PW, PH);
        let mut background = psd::PsdLayer::raster("Background", canvas);
        background.set_rgba8(&solid(canvas, RED)).unwrap();
        file.layers = vec![background];
        let profile = swapped_primaries_profile();
        file.resources.push(psd::ImageResource {
            id: psd::resource::ID_ICC_PROFILE,
            name: String::new(),
            data: profile.clone(),
        });
        file.resources.push(psd::ImageResource {
            id: 1069,
            name: String::new(),
            data: vec![1, 2, 3, 4],
        });

        let import = document_from_psd(&psd::write(&file).unwrap(), "tagged.psd", 50).unwrap();
        let doc = &import.imported.document;
        match &doc.meta.color_space {
            color::ColorSpace::IccProfile { profile: kept, .. } => {
                assert_eq!(kept, &profile, "the profile bytes ride with the document");
            }
            other => panic!("the profile must be retained, got {other:?}"),
        }
        // The profile is not among the resources named as left behind; the
        // other resource still is.
        let summary = import.notes.summary().unwrap();
        assert!(summary.contains("1 image resource(s)"), "{summary}");
        assert!(!summary.contains("the colour profile"), "{summary}");
    }

    #[test]
    fn a_profileless_psd_stays_srgb_and_still_names_left_behind_resources() {
        let import = document_from_psd(&layered_psd(), "plain.psd", 50).unwrap();
        assert_eq!(
            import.imported.document.meta.color_space,
            color::ColorSpace::Srgb
        );
    }

    /// Card 076 — full extents: a layer hanging off the left edge keeps its
    /// off-canvas ink in the tile store, and moving the layer brings it into
    /// view through the real transform path, instead of the import having
    /// dropped it.
    #[test]
    fn off_canvas_ink_survives_import_and_moves_into_view() {
        let mut file = psd::PsdFile::new(psd::PsdHeader::rgba8(PW, PH));
        // Half on the canvas, half off the left edge.
        let rect = psd::Rect::new(-64, 0, 64, 100);
        let mut hanging = psd::PsdLayer::raster("Hanging", rect);
        hanging.set_rgba8(&solid(rect, GREEN)).unwrap();
        file.layers = vec![hanging];

        let mut import = document_from_psd(&psd::write(&file).unwrap(), "hanging.psd", 50).unwrap();
        let doc = &import.imported.document;
        let id = doc.layers.root()[0];

        // The off-canvas half is really in the store: tile column -1.
        let map = doc.layer_tiles(id).unwrap();
        let off_hash = map
            .get(TileCoord::new(-1, 0, 0))
            .expect("the off-canvas tile is stored");
        let bytes = compositor::TileSource::tile(&import.imported.tiles, off_hash).unwrap();
        assert_ne!(
            bytes[192 * 4],
            0,
            "the off-canvas tile holds ink (tile-local x=192 = document x=-64)"
        );

        // The compositor deliberately clips its output to the canvas (an
        // edge tile's padding is not part of the image), so the off-canvas
        // ink proves it survived by RENDERING once the layer is moved into
        // view — the transform path samples the tile map's own bounds, which
        // include the negative column.
        let mut history = History::with_limit(10);
        history
            .apply(
                &mut import.imported.document,
                Command::TransformLayer {
                    layer_id: id,
                    matrix: [1.0, 0.0, 0.0, 1.0, 64.0, 0.0],
                },
            )
            .unwrap();
        let moved = composite_region(
            &import.imported.document,
            &import.imported.tiles,
            PixelRect::new(0, 0, 64, 100),
            0,
            CompositeOptions::default(),
        )
        .unwrap()
        .to_rgba8(&import.imported.document.meta.color_space);
        assert_eq!(
            moved[0], GREEN[0],
            "the former off-canvas ink moved into view"
        );
        // No note: nothing was dropped, so nothing is named.
        assert!(import.notes.is_empty(), "{:?}", import.notes);
    }

    /// Card 076 — mask parameters: density and feather land on the imported
    /// mask, in the model's units (density `0..=1`, feather document pixels).
    #[test]
    fn mask_density_and_feather_land_on_the_imported_mask() {
        let mut file = psd::PsdFile::new(psd::PsdHeader::rgba8(PW, PH));
        let rect = psd::Rect::sized(PW, PH);
        let mut masked = psd::PsdLayer::raster("Masked", rect);
        masked.set_rgba8(&solid(rect, BLUE)).unwrap();
        let mut mask = psd::PsdMask::new(MASK_RECT, mask_ramp());
        mask.density = 128; // → 128/255
        mask.feather_px = 6.0; // already pixel units on both sides
        masked.mask = Some(mask);
        file.layers = vec![masked];

        let import = document_from_psd(&psd::write(&file).unwrap(), "params.psd", 50).unwrap();
        let doc = &import.imported.document;
        let layer = doc.layers.get(doc.layers.root()[0]).unwrap();
        let mask = layer.mask.as_ref().expect("the mask is attached");
        assert!((mask.density() - 128.0 / 255.0).abs() < 1e-6);
        assert!((mask.feather_px() - 6.0).abs() < 1e-5);
        // The parameters mapped, so the mask_params note has nothing to say.
        assert!(import.notes.is_empty(), "{:?}", import.notes);
    }

    /// Card 072's honesty gate: the fidelity matrix
    /// (`docs/PSD-THUMBNAIL-SUPPORT.md`) must name every fallback note the
    /// import actually emits, the editable-preservation claims, and the
    /// failure-policy statement — a doc that drifts from `Tally::record`
    /// fails here instead of lying to the user.
    #[test]
    fn the_psd_support_matrix_names_every_fallback_the_import_emits() {
        let doc = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/PSD-THUMBNAIL-SUPPORT.md"
        ))
        .expect("the PSD support matrix exists");
        // Every Tally fallback note's FULL template text (unique strings, so
        // deleting any single note fails the gate rather than surviving via a
        // shared fragment).
        for phrase in [
            "the colour label on {names} is not shown by this layers panel and was not kept",
            "adjustment layer(s) this build cannot evaluate ({names}) were kept as empty              layers; their effect is in the flattened image but not editable",
            "type layer(s) ({names}) were imported as pixels; the text is no longer editable",
            "type layer(s) ({names}) were imported as editable text with the default font, size and fill — the source font is not in this build's supported subset",
            "layer effect(s) on {names} were not imported",
            "the {kinds} effect(s) on {names} were not imported",
            "{names} carried a second, vector-derived mask that was not imported",
            "{names} carry a transform a .psd cannot express; their pixels were written              where they are stored",
            "text, shape and smart-object layer(s) ({names}) cannot stay editable in a .psd;               their rendered appearance was written as a raster layer's pixels",
            "the mask density or feather on {names} was not written",
            "the vector mask on {names} was written as its rasterised coverage",
            "the blanket lock on {names} has no .psd equivalent and was not written",
            "{names} pass through *and* carry a blend mode; a .psd stores only the              pass-through",
        ] {
            // Rust's `\`-continuation collapses newline+indent into the
            // string, so compare with whitespace normalized on both sides.
            let flat = |t: &str| t.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(
                doc.contains(&flat(phrase)),
                "the matrix no longer names the fallback {:?}",
                flat(phrase)
            );
        }
        // The editable-preservation claims, the failure policy, and the
        // ICC-drop note the matrix quotes (a false claim here failed review).
        // Card 076 additions: the full-extents and ICC-retention claims, and
        // the mask-parameter mapping.
        for phrase in [
            "ClipToBelow",
            "Invert",
            "A correct merged preview is explicitly insufficient",
            "Original PSD bytes are never modified on import",
            "Nothing silent",
            "the colour profile — are not part of this document model",
            "full extents preserved",
            "Retained (metadata)",
            "The pixels are NOT transformed at import",
            "feather is stored in pixels, already the model's `feather_px` unit",
        ] {
            assert!(doc.contains(phrase), "the matrix lost the claim {phrase:?}");
        }
    }
    // ------------------------------------------------------- card 078

    /// A document with one shape layer — the fallback-render kind whose
    /// pixels come from the compositor, not the tile store.
    fn shape_document() -> (Document, MemoryTileSource, layer_model::LayerId) {
        let mut doc = Document::new(96, 64, "shapes");
        let tiles = MemoryTileSource::new();
        let layer = doc
            .layers
            .push_root(Layer::with_kind(
                "Badge",
                LayerKind::Shape(layer_model::ShapeLayer {
                    path_svg: "M 8 8 h 24 v 16 h -24 Z".into(),
                    fill: Some([1.0, 0.0, 0.0, 1.0]),
                    fill_rule: layer_model::ShapeFillRule::NonZero,
                    stroke: None,
                }),
            ))
            .unwrap();
        doc.layers.get_mut(layer).unwrap().transform =
            glam::Affine2::from_translation(glam::vec2(12.0, 10.0));
        (doc, tiles, layer)
    }

    /// Ink bounds of an independently decoded PSD layer's RGBA, in CANVAS
    /// coordinates (bounds.x0/y0 + the crop's own ink).
    fn decoded_ink_bounds(file: &psd::PsdFile, name: &str) -> Option<(i32, i32, i32, i32)> {
        let layer = file.layers.iter().find(|l| l.name == name)?;
        let rgba = psd_layer_rgba(layer, &file.header)?;
        let (w, h) = (layer.bounds.width(), layer.bounds.height());
        let mut bounds: Option<(i64, i64, i64, i64)> = None;
        for y in 0..h as i64 {
            for x in 0..w as i64 {
                if rgba[((y * w as i64 + x) * 4 + 3) as usize] != 0 {
                    let (gx, gy) = (
                        i64::from(layer.bounds.left) + x,
                        i64::from(layer.bounds.top) + y,
                    );
                    bounds = Some(match bounds {
                        None => (gx, gy, gx, gy),
                        Some((x0, y0, x1, y1)) => (x0, y0, x1.max(gx), y1.max(gy)),
                    });
                }
            }
        }
        bounds.map(|(x0, y0, x1, y1)| (x0 as i32, y0 as i32, x1 as i32, y1 as i32))
    }

    #[test]
    fn a_shape_layer_exports_its_rendered_appearance_at_its_transform() {
        let (doc, tiles, _) = shape_document();
        let composite = vec![0u8; 96 * 64 * 4];
        let (bytes, notes) = psd_from_document(&doc, &tiles, &composite).unwrap();

        // The fallback is named, not silent.
        let told = notes.summary().expect("the fallback must be named");
        assert!(told.contains("Badge"), "{told}");
        assert!(told.contains("raster layer's pixels"), "{told}");

        // An independent reader sees real pixels, at the transformed spot:
        // the 24x16 shape sits at (12+8, 10+8) .. (12+32, 10+24).
        let file = psd::read(&bytes).unwrap();
        let (x0, y0, x1, y1) =
            decoded_ink_bounds(&file, "Badge").expect("the shape's fallback pixels are visible");
        assert_eq!(
            (x0, y0),
            (20, 18),
            "the transform travelled into the pixels"
        );
        assert_eq!((x1, y1), (43, 33));
    }

    #[test]
    fn a_smart_object_exports_its_placed_appearance_through_its_transform(
    ) -> Result<(), Box<dyn std::error::Error>> {
        // An 8x8 embedded source placed at 2x scale: the compositor renders
        // a smart object from the layer's stored tiles through the layer
        // transform, so the exported channels must show the scaled and
        // translated appearance, not the raw stored tiles.
        let source_rgba = [200u8, 10, 10, 255].repeat(64);
        let mut doc = Document::new(64, 64, "placed");
        let mut tiles = MemoryTileSource::new();
        let asset = layer_model::AssetId::new();
        doc.set_asset_origin(layer_model::AssetRecord {
            id: asset,
            origin: layer_model::AssetOrigin::Embedded {
                name: "logo.png".into(),
                bytes: raster::encode(raster::ExportFormat::Png, 8, 8, &source_rgba).unwrap(),
            },
            source_size: Some((8, 8)),
        });
        let layer = doc
            .layers
            .push_root(Layer::with_kind(
                "Logo",
                LayerKind::SmartObject(layer_model::SmartObjectLayer {
                    asset,
                    linked: false,
                }),
            ))
            .unwrap();
        doc.layers.get_mut(layer).unwrap().transform =
            glam::Affine2::from_scale(glam::Vec2::new(2.0, 2.0))
                * glam::Affine2::from_translation(glam::vec2(4.0, 4.0));
        // The placed source's pixels, stored as the layer's tiles (what
        // placement does).
        let edits = tile_edits_for_rgba(&source_rgba, psd::Rect::new(0, 0, 8, 8), &mut tiles);
        let delta = TileDelta::new(edits)?;
        doc.pixels.apply(PixelKey::Layer(layer), &delta);

        let composite = vec![0u8; 64 * 64 * 4];
        let (bytes, _) = psd_from_document(&doc, &tiles, &composite).unwrap();
        let file = psd::read(&bytes).unwrap();
        let (x0, y0, x1, y1) =
            decoded_ink_bounds(&file, "Logo").expect("the smart object's fallback is visible");
        // 8x8 source at 2x from (4,4): the compositor's pixel-center
        // convention puts the solid span at (8,8)..(23,23) with the bilinear
        // edge falloff one pixel wider each way — 7..24 inclusive.
        assert_eq!((x0, y0), (7, 7));
        assert_eq!((x1 - x0 + 1, y1 - y0 + 1), (18, 18), "scaled ~2x");
        Ok(())
    }

    #[test]
    fn the_mask_travels_separately_so_the_fallback_is_not_double_masked(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (mut doc, mut tiles, layer) = shape_document();
        // A mask that hides the shape's left half.
        let mask_id = layer_model::MaskId::new();
        let mut c = vec![0u8; 96 * 64];
        for y in 0..64 {
            for x in 0..96 {
                c[y * 96 + x] = if x >= 48 { 255 } else { 0 };
            }
        }
        let edits = tile_edits_for_coverage(
            Some(&c),
            psd::Rect::sized(96, 64),
            0,
            &DocRect::canvas(96, 64).tiles(),
            &mut tiles,
        );
        let delta = TileDelta::new(edits)?;
        doc.pixels.apply(PixelKey::Mask(mask_id), &delta);
        doc.layers.get_mut(layer).unwrap().mask = Some(layer_model::LayerMask::new(mask_id));

        let composite = vec![0u8; 96 * 64 * 4];
        let (bytes, _) = psd_from_document(&doc, &tiles, &composite).unwrap();
        let file = psd::read(&bytes).unwrap();
        let record = file.layers.iter().find(|l| l.name == "Badge").unwrap();
        // The channels still hold ink where the mask hides it: the mask is
        // its own channel, not pre-applied.
        let rgba = psd_layer_rgba(record, &file.header).expect("channels exist");
        let shape_x_in_hidden_half = 20 - i64::from(record.bounds.left); // canvas x=20 is inside the shape, left of the mask edge
        assert!(
            (0..record.bounds.width() as i64).contains(&shape_x_in_hidden_half),
            "sanity: the hidden-side probe is inside the record"
        );
        let h = record.bounds.height() as i64;
        assert!(
            (0..h).any(|y| rgba
                [((y * record.bounds.width() as i64 + shape_x_in_hidden_half) * 4 + 3) as usize]
                != 0),
            "the fallback channels keep the ink the mask hides"
        );
        // ...and the mask channel really does hide that half.
        let mask = record.mask.as_ref().expect("the mask block travels");
        let cov = psd_mask_coverage(mask, file.header.depth).expect("mask samples decode");
        let probe = ((i64::from(18) - i64::from(mask.bounds.top)) * mask.bounds.width() as i64
            + (i64::from(20) - i64::from(mask.bounds.left))) as usize;
        assert_eq!(cov[probe], 0, "the mask hides the left half");
        Ok(())
    }

    #[test]
    fn supported_effects_export_as_editable_descriptors_and_the_rest_is_named(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut doc = Document::new(64, 64, "fx");
        let mut tiles = MemoryTileSource::new();
        let layer = doc.layers.push_root(Layer::raster("Shadowed")).unwrap();
        let rgba = [90u8, 30, 200, 255].repeat(16 * 16);
        let edits = tile_edits_for_rgba(&rgba, psd::Rect::new(0, 0, 16, 16), &mut tiles);
        let delta = TileDelta::new(edits)?;
        doc.pixels.apply(PixelKey::Layer(layer), &delta);
        doc.layers.get_mut(layer).unwrap().effects = layer_model::LayerEffects {
            drop_shadow: Some(layer_model::ShadowEffect {
                opacity: 0.75,
                ..Default::default()
            }),
            color_overlay: Some(layer_model::ColorOverlayEffect {
                blend_mode: BlendMode::Color,
                color: [220.0 / 255.0, 60.0 / 255.0, 30.0 / 255.0, 1.0],
                opacity: 0.5,
            }),
            satin: Some(layer_model::SatinEffect::default()),
            ..Default::default()
        };

        let composite = vec![0u8; 64 * 64 * 4];
        let (bytes, notes) = psd_from_document(&doc, &tiles, &composite).unwrap();
        // The kind this writer cannot produce is named, not silently dropped.
        let told = notes.summary().expect("the satin must be named");
        assert!(told.contains("satin"), "{told}");

        // An independent reader decodes the block back into editable
        // parameters through the same parser the import path uses.
        let file = psd::read(&bytes).unwrap();
        let record = file
            .layers
            .iter()
            .find(|l| l.name == "Shadowed")
            .expect("the layer is in the file");
        let fx = record.effects.as_ref().expect("the lfx2 block travels");
        let decoded = psd::effects::import_effects(fx, &psd::ReadOptions::default())
            .expect("the block decodes")
            .effects;
        let shadow = decoded
            .drop_shadow
            .as_ref()
            .expect("the shadow round-trips");
        assert!((shadow.opacity - 0.75).abs() < 1e-6);
        let overlay = decoded
            .color_overlay
            .as_ref()
            .expect("the overlay round-trips");
        assert_eq!(overlay.blend_mode, BlendMode::Color);
        assert!(decoded.satin.is_none(), "only the written kinds decode");
        Ok(())
    }

    #[test]
    fn the_fallback_pixels_do_not_bake_the_effects_the_descriptor_carries() {
        // Card 080's double-application guard: a rendered fallback carries
        // the layer's shape only; the shadow lives in the descriptor.
        let (mut doc, tiles, layer) = shape_document();
        doc.layers.get_mut(layer).unwrap().effects = layer_model::LayerEffects {
            drop_shadow: Some(layer_model::ShadowEffect {
                size_px: 16.0,
                distance_px: 8.0,
                ..Default::default()
            }),
            ..Default::default()
        };
        let composite = vec![0u8; 96 * 64 * 4];
        let (bytes, notes) = psd_from_document(&doc, &tiles, &composite).unwrap();
        // The descriptor is written: the only note is the shape's own
        // appearance fallback, never an effects note.
        assert!(
            notes
                .summary()
                .is_none_or(|t| t.contains("raster layer's pixels")),
            "{notes:?}"
        );
        let file = psd::read(&bytes).unwrap();
        assert!(file
            .layers
            .iter()
            .find(|l| l.name == "Badge")
            .unwrap()
            .effects
            .is_some());
        // ...and the channels hold exactly the shape's ink, no shadow reach.
        let (x0, y0, x1, y1) = decoded_ink_bounds(&file, "Badge").unwrap();
        assert_eq!((x0, y0, x1, y1), (20, 18, 43, 33));
    }
}
