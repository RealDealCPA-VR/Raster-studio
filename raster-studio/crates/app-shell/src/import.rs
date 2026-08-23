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

use std::path::Path;

use compositor::MemoryTileSource;
use editor_core::pixels::{PixelTarget, TileDelta, TileEdit};
use editor_core::{Command, Document, History};
use layer_model::{Layer, LayerId};
use raster::{PixelFormat, TileCoord, TileGrid};

/// A decoded image on its way into a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    /// Row-major RGBA8, straight alpha.
    pub rgba8: Vec<u8>,
}

impl DecodedImage {
    /// Read and decode a file through the `raster` codec facade.
    pub fn decode_path(path: &Path) -> Result<DecodedImage, ImportError> {
        let decoded = raster::decode_path(path)?;
        Ok(DecodedImage {
            width: decoded.width,
            height: decoded.height,
            rgba8: decoded.rgba8,
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
    #[error(transparent)]
    Grid(#[from] raster::GridError),
    #[error("building the import command failed: {0}")]
    Command(#[from] editor_core::CommandError),
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

/// An empty document with one transparent raster layer — File ▸ New.
pub fn blank_document(
    width: u32,
    height: u32,
    title: &str,
    history_depth: usize,
) -> Result<ImportedDocument, ImportError> {
    if width == 0 || height == 0 {
        return Err(ImportError::EmptyImage { width, height });
    }
    let mut document = Document::new(width, height, title);
    let mut history = History::with_limit(history_depth);
    let layer = Layer::raster("Layer 1");
    let layer_id = layer.id;
    history.apply(&mut document, Command::create_layer(layer))?;
    document
        .set_active_layer(Some(layer_id))
        .expect("the layer was just created in this document");
    history.clear();
    document.mark_saved();
    Ok(ImportedDocument {
        document,
        history,
        tiles: MemoryTileSource::new(),
        layer: layer_id,
    })
}

/// The tile coordinates a document's layer covers — what the presenter uploads.
pub fn layer_tile_coords(doc: &Document, layer: LayerId) -> Vec<TileCoord> {
    doc.layer_tiles(layer)
        .map(|m| m.iter().map(|(c, _)| c).collect())
        .unwrap_or_default()
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
        };
        let err = import_command(&empty, "x", &mut tiles).unwrap_err();
        assert!(err.to_string().contains("non-zero"), "{err}");

        let short = DecodedImage {
            width: 4,
            height: 4,
            rgba8: vec![0; 4],
        };
        let err = import_command(&short, "x", &mut tiles).unwrap_err();
        assert!(err.to_string().contains("RGBA8"), "{err}");
        assert!(tiles.is_empty(), "a refusal must store nothing");
    }

    #[test]
    fn a_blank_document_starts_with_one_empty_raster_layer() {
        let d = blank_document(800, 600, "Untitled", 50).unwrap();
        assert_eq!(d.document.layers.len(), 1);
        assert_eq!(d.document.active_layer(), Some(d.layer));
        assert!(d.document.layer_tiles(d.layer).is_none(), "no pixels yet");
        assert!(!d.document.is_dirty());
        assert!(blank_document(0, 10, "x", 1).is_err());
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
}
