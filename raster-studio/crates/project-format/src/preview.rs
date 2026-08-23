//! The composite preview written into `previews/`.
//!
//! `previews/` used to be created empty, so nothing — file browser, recent-files
//! list, "restore this version" dialog — could show what a project looks like
//! without opening it.
//!
//! # One compositor
//!
//! The preview is produced by [`compositor::composite_region`], the same
//! function that draws the canvas. A thumbnail rendered any other way would be
//! a second implementation of blending, masking and adjustment, and it would
//! drift from what the user sees.
//!
//! # Bounded memory
//!
//! A [`compositor::Canvas`] is 16 bytes per pixel (linear premultiplied `f32`),
//! so compositing a large document whole to make a 512-pixel thumbnail would
//! allocate gigabytes. The document is composited in **horizontal strips** of at
//! most [`MAX_STRIP_PIXELS`], each strip box-filtered straight into the
//! thumbnail accumulator. Peak allocation is one strip plus the accumulator,
//! independent of canvas height.
//!
//! Averaging happens in the canvas's own linear premultiplied space — averaging
//! gamma-encoded or straight-alpha samples would darken the result and drag
//! transparent pixels' colour into their neighbours.

use compositor::{Canvas, CompositeOptions};
use editor_core::Document;
use raster::codec::ExportFormat;
use raster::PixelRect;

use crate::error::ProjectError;
use crate::tiles::{AsTileSource, TileBytes};

/// Directory holding preview images.
pub const PREVIEWS_DIR: &str = "previews";
/// Package-relative path of the composite preview.
pub const PREVIEW_FILE: &str = "previews/preview.png";

/// Default longest edge of the preview, in pixels.
pub const DEFAULT_PREVIEW_MAX_EDGE: u32 = 512;

/// Most source pixels composited in one pass while building a preview.
const MAX_STRIP_PIXELS: u64 = 1 << 20;

/// A rendered preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    pub width: u32,
    pub height: u32,
    /// PNG file bytes.
    pub png: Vec<u8>,
}

/// Thumbnail dimensions for a canvas, preserving aspect ratio and never
/// upscaling.
fn thumb_size(width: u32, height: u32, max_edge: u32) -> (u32, u32) {
    let long = width.max(height) as u64;
    let target = (max_edge.max(1) as u64).min(long);
    let scale = |v: u32| -> u32 {
        let n = (v as u64 * target + long / 2) / long;
        n.max(1) as u32
    };
    (scale(width), scale(height))
}

/// Composite the document and box-filter it down to a thumbnail.
///
/// `None` for a zero-area canvas — there is nothing to show, and every
/// downstream reader treats an absent preview as "not available" already.
pub(crate) fn render(
    doc: &Document,
    tiles: &dyn TileBytes,
    max_edge: u32,
) -> Result<Option<Preview>, ProjectError> {
    render_with_strip_limit(doc, tiles, max_edge, MAX_STRIP_PIXELS)
}

/// [`render`] with the strip budget exposed, so the striping itself can be
/// tested rather than taken on faith.
fn render_with_strip_limit(
    doc: &Document,
    tiles: &dyn TileBytes,
    max_edge: u32,
    max_strip_pixels: u64,
) -> Result<Option<Preview>, ProjectError> {
    let (w, h) = (doc.width(), doc.height());
    if w == 0 || h == 0 {
        return Ok(None);
    }
    let (tw, th) = thumb_size(w, h, max_edge);
    let source = AsTileSource(tiles);
    let opts = CompositeOptions::default();

    let cells = tw as usize * th as usize;
    let mut sum = vec![[0f64; 4]; cells];
    let mut count = vec![0u32; cells];

    let rows_per_strip = ((max_strip_pixels / w as u64).max(1)).min(h as u64) as u32;
    let mut y = 0u32;
    while y < h {
        let rows = rows_per_strip.min(h - y);
        let strip = compositor::composite_region(
            doc,
            &source,
            PixelRect::new(0, y as i64, w, rows),
            0,
            opts,
        )
        .map_err(|e| ProjectError::Preview(e.to_string()))?;
        for ry in 0..rows {
            let sy = y + ry;
            let oy = (sy as u64 * th as u64 / h as u64) as usize;
            for sx in 0..w {
                let ox = (sx as u64 * tw as u64 / w as u64) as usize;
                let px = strip.get(sx as i64, sy as i64);
                let cell = oy * tw as usize + ox;
                for c in 0..4 {
                    sum[cell][c] += px[c] as f64;
                }
                count[cell] += 1;
            }
        }
        y += rows;
    }

    let pixels: Vec<[f32; 4]> = sum
        .iter()
        .zip(&count)
        .map(|(s, &n)| {
            if n == 0 {
                [0.0; 4]
            } else {
                let n = n as f64;
                [
                    (s[0] / n) as f32,
                    (s[1] / n) as f32,
                    (s[2] / n) as f32,
                    (s[3] / n) as f32,
                ]
            }
        })
        .collect();

    let canvas = Canvas::from_pixels(PixelRect::new(0, 0, tw, th), pixels)
        .map_err(|e| ProjectError::Preview(e.to_string()))?;
    let rgba8 = canvas.to_rgba8(&doc.meta.color_space);
    let png = raster::codec::encode(ExportFormat::Png, tw, th, &rgba8)
        .map_err(|e| ProjectError::Preview(e.to_string()))?;
    Ok(Some(Preview {
        width: tw,
        height: th,
        png,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiles::{solid_tile, NoTiles};
    use editor_core::{PixelKey, TileDelta, TileEdit};
    use raster::{TileCoord, TileHash};

    #[test]
    fn thumbnails_keep_their_aspect_ratio_and_never_upscale() {
        assert_eq!(thumb_size(1920, 1080, 512), (512, 288));
        assert_eq!(thumb_size(1080, 1920, 512), (288, 512));
        assert_eq!(thumb_size(64, 64, 512), (64, 64), "no upscaling");
        assert_eq!(thumb_size(10000, 1, 512), (512, 1), "never zero");
        assert_eq!(thumb_size(1, 10000, 512), (1, 512));
    }

    #[test]
    fn a_zero_area_document_has_no_preview() {
        let doc = Document::new(0, 0, "empty");
        assert!(render(&doc, &NoTiles, 512).unwrap().is_none());
    }

    #[test]
    fn the_preview_shows_the_painted_pixels() {
        // One opaque red tile filling a 256x256 canvas.
        let bytes = solid_tile([255, 0, 0, 255]);
        let hash = TileHash::of(&bytes);
        let mut doc = Document::new(256, 256, "red");
        let layer = layer_model::Layer::raster("L");
        let id = layer.id;
        doc.layers.push_root(layer).unwrap();
        doc.pixels.apply(
            PixelKey::Layer(id),
            &TileDelta::single(TileEdit::set(TileCoord::new(0, 0, 0), hash)),
        );

        let mut source = compositor::MemoryTileSource::new();
        source.insert_bytes(bytes);
        let preview = render(&doc, &source, 64).unwrap().unwrap();
        assert_eq!((preview.width, preview.height), (64, 64));

        let decoded = raster::codec::decode_bytes(&preview.png).unwrap();
        assert_eq!((decoded.width, decoded.height), (64, 64));
        assert_eq!(
            &decoded.rgba8[..4],
            &[255, 0, 0, 255],
            "a red document must produce a red thumbnail"
        );

        // ...and the same document with no tile bytes available is empty,
        // which is what makes the assertion above about pixels and not about
        // the encoder.
        let empty = render(&doc, &NoTiles, 64).unwrap().unwrap();
        let decoded = raster::codec::decode_bytes(&empty.png).unwrap();
        assert_eq!(&decoded.rgba8[..4], &[0, 0, 0, 0]);
    }

    #[test]
    fn striping_does_not_change_the_answer() {
        // Two tiles of different colours stacked vertically, so a thumbnail
        // that mixed up its strips would be visibly wrong rather than
        // accidentally right.
        let top = solid_tile([0, 128, 255, 255]);
        let bottom = solid_tile([255, 128, 0, 255]);
        let (th_hash, bh_hash) = (TileHash::of(&top), TileHash::of(&bottom));
        let mut doc = Document::new(256, 512, "stacked");
        let layer = layer_model::Layer::raster("L");
        let id = layer.id;
        doc.layers.push_root(layer).unwrap();
        doc.pixels.apply(
            PixelKey::Layer(id),
            &TileDelta::new([
                TileEdit::set(TileCoord::new(0, 0, 0), th_hash),
                TileEdit::set(TileCoord::new(0, 1, 0), bh_hash),
            ])
            .unwrap(),
        );
        let mut source = compositor::MemoryTileSource::new();
        source.insert_bytes(top);
        source.insert_bytes(bottom);

        // One pass over the whole canvas...
        let whole = render_with_strip_limit(&doc, &source, 32, u64::MAX)
            .unwrap()
            .unwrap();
        // ...versus four strips, two of which straddle a tile row boundary.
        let striped = render_with_strip_limit(&doc, &source, 32, 256 * 128)
            .unwrap()
            .unwrap();
        assert_eq!(whole, striped, "the strip budget changed the image");
        assert_eq!((whole.width, whole.height), (16, 32));

        let decoded = raster::codec::decode_bytes(&whole.png).unwrap();
        let row = |y: usize| -> [u8; 4] {
            let i = (y * whole.width as usize) * 4;
            decoded.rgba8[i..i + 4].try_into().unwrap()
        };
        assert_eq!(row(0), [0, 128, 255, 255]);
        assert_eq!(row(31), [255, 128, 0, 255]);
    }
}
