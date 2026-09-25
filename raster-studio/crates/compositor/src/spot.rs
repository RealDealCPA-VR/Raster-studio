//! W13X-4: spot channels composited over the image as ink.
//!
//! Every [`editor_core::spot::SpotChannel`] of the document is laid over the
//! composited layer stack, in list order, as a layer of its ink colour whose
//! alpha is the channel's coverage at that pixel and whose blend runs from
//! Multiply at 0% solidity (a transparent ink: it darkens what is printed
//! under it) to Normal at 100% (an opaque ink: it covers it) — Photoshop's
//! spot-channel preview. In W3C compositing terms, with backdrop `Cb` / `ab`
//! and ink `Cs` at coverage `as`:
//!
//! ```text
//! B(Cb, Cs) = (1 - s) * Cb * Cs + s * Cs
//! co = as * (1 - ab) * Cs + as * ab * B(Cb, Cs) + (1 - as) * cb
//! ao = as + ab * (1 - as)
//! ```
//!
//! on linear, premultiplied pixels (`cb = ab * Cb`), which needs no division.
//! Over a transparent backdrop the ink shows as itself, so a spot plate on an
//! empty document is visible.
//!
//! The pass reads the document's coverage at the centre of the document-space
//! block each pixel of the requested mip level stands for, and
//! [`hash_spot_inks`] keys a cached tile by exactly those samples, so a tile
//! is recomposited when — and only when — the ink under it changes.

use std::collections::hash_map::DefaultHasher;
use std::hash::Hash;

use color::{to_linear, ColorSpace};
use editor_core::Document;
use glam::IVec2;
use raster::PixelRect;

use crate::canvas::Canvas;

/// The document pixel the level-`level` pixel `(x, y)` samples.
fn doc_point(x: i64, y: i64, level: u8) -> IVec2 {
    let step = 1i64 << level;
    let clamp = |v: i64| v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32;
    IVec2::new(clamp(x * step + step / 2), clamp(y * step + step / 2))
}

/// Lay every spot channel of `doc` over `canvas` (linear, premultiplied,
/// at mip `level`).
pub(crate) fn lay_spot_inks(doc: &Document, space: &ColorSpace, level: u8, canvas: &mut Canvas) {
    if doc.spot_channels.is_empty() {
        return;
    }
    let rect = canvas.rect();
    let stride = i64::from(rect.width.max(1));
    for channel in &doc.spot_channels {
        let ink = to_linear(space, channel.ink.map(|v| f32::from(v) / 255.0));
        let s = channel.solidity_fraction();
        for (i, px) in canvas.pixels_mut().iter_mut().enumerate() {
            let x = rect.x + i as i64 % stride;
            let y = rect.y + i as i64 / stride;
            let a_s = channel.ink_at(doc_point(x, y, level));
            if a_s <= 0.0 {
                continue;
            }
            *px = ink_over(*px, ink, a_s, s);
        }
    }
}

/// One premultiplied pixel with ink `ink` (linear) at coverage `a_s` and
/// solidity `s` laid over it.
pub(crate) fn ink_over(backdrop: [f32; 4], ink: [f32; 3], a_s: f32, s: f32) -> [f32; 4] {
    let ab = backdrop[3];
    let mut out = [0.0; 4];
    for c in 0..3 {
        let cb = backdrop[c];
        let cs = ink[c];
        let blended = (1.0 - s) * cb * cs + s * ab * cs;
        out[c] = a_s * (1.0 - ab) * cs + a_s * blended + (1.0 - a_s) * cb;
    }
    out[3] = a_s + ab * (1.0 - a_s);
    out
}

/// Fold into a tile's cache key everything the spot pass reads for `rect`
/// (level-`level` pixels): each channel's ink and solidity and the coverage
/// samples it takes.
pub(crate) fn hash_spot_inks(doc: &Document, level: u8, rect: PixelRect, h: &mut DefaultHasher) {
    doc.spot_channels.len().hash(h);
    for channel in &doc.spot_channels {
        channel.ink.hash(h);
        channel.solidity.hash(h);
        for y in rect.y..rect.bottom() {
            for x in rect.x..rect.right() {
                channel.ink_at(doc_point(x, y, level)).to_bits().hash(h);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{composite_rect, CompositeOptions, MemoryTileSource, TileCompositor};
    use editor_core::spot::SpotChannel;
    use editor_core::Selection;

    fn doc_with_spot(solidity: u8) -> Document {
        let mut doc = Document::new(8, 8, "ink");
        doc.spot_channels.push(SpotChannel {
            coverage: Selection::Rect {
                min: IVec2::new(0, 0),
                max: IVec2::new(4, 8),
            },
            ..SpotChannel::empty("Green", [0, 255, 0], solidity)
        });
        doc
    }

    fn px(doc: &Document, x: i64, y: i64) -> [u8; 4] {
        let source = MemoryTileSource::new();
        let c = composite_rect(
            doc,
            &source,
            PixelRect::new(0, 0, 8, 8),
            0,
            CompositeOptions::default(),
        )
        .unwrap();
        let bytes = c.to_rgba8(&doc.meta.color_space);
        let i = ((y * 8 + x) * 4) as usize;
        [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]
    }

    #[test]
    fn a_spot_channel_composites_as_its_ink_where_it_covers_and_nowhere_else() {
        let doc = doc_with_spot(100);
        assert_eq!(px(&doc, 1, 1), [0, 255, 0, 255], "inked half");
        assert_eq!(px(&doc, 6, 1), [0, 0, 0, 0], "uninked half stays clear");
        assert_eq!(px(&Document::new(8, 8, "none"), 1, 1), [0, 0, 0, 0]);
    }

    #[test]
    fn solidity_runs_from_multiply_to_cover() {
        // Over opaque mid grey, a 0% green ink multiplies (red and blue go to
        // zero, green keeps the grey), a 100% one covers with pure green.
        let grey = [0.5, 0.5, 0.5, 1.0];
        let ink = [0.0, 1.0, 0.0];
        assert_eq!(ink_over(grey, ink, 1.0, 0.0), [0.0, 0.5, 0.0, 1.0]);
        assert_eq!(ink_over(grey, ink, 1.0, 1.0), [0.0, 1.0, 0.0, 1.0]);
        // No coverage leaves the backdrop exactly.
        assert_eq!(ink_over(grey, ink, 0.0, 0.5), grey);
        // Half coverage over nothing is half the ink.
        assert_eq!(ink_over([0.0; 4], ink, 0.5, 0.0), [0.0, 0.5, 0.0, 0.5]);
    }

    #[test]
    fn the_tile_cache_recomposites_when_the_ink_changes() {
        let source = MemoryTileSource::new();
        let mut cache = TileCompositor::new();
        let region = PixelRect::new(0, 0, 8, 8);
        let mut doc = doc_with_spot(100);
        let first = cache
            .composite_region(&doc, &source, region, 0, CompositeOptions::default())
            .unwrap();
        doc.spot_channels[0].ink = [255, 0, 0];
        let second = cache
            .composite_region(&doc, &source, region, 0, CompositeOptions::default())
            .unwrap();
        assert_ne!(first, second, "a cached tile kept the old ink");
        assert_eq!(
            second.to_rgba8(&doc.meta.color_space)[..4],
            [255, 0, 0, 255]
        );
    }
}
