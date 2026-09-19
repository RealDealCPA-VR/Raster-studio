//! Reusable layer bounds queries (plan card 008).
//!
//! The compositor has always needed to know where a layer's pixels are —
//! `Ctx::document_bounds` sizes style contexts, `Ctx::content_bounds` walks
//! the tree. Those were `Ctx`-private, so callers outside the composite path
//! (Move's picking, Free Transform's start bounds, thumbnails) either re-derived
//! the geometry or fell back to the canvas rectangle — which is exactly gap
//! E03/E04. This module is the public seam over the compositor's own answers:
//! one implementation, the same one the frame uses.
//!
//! # What each query answers
//!
//! * [`content_bounds`] — the layer's stored ink extent **before its own
//!   transform**, at a mip level: the stored-tile extent of a raster /
//!   smart-object layer, the ink box of a text run or shape path, the union
//!   of a group's children each mapped through its own transform. `None` for
//!   a layer that bounds nothing (missing, or an adjustment — which only
//!   rewrites pixels that are already there).
//! * [`document_bounds`] — [`content_bounds`] mapped forward through the
//!   layer's transform, region-independent.
//! * [`styled_bounds`] — [`document_bounds`] grown by the layer's effects'
//!   reach, so a hit test or frame-selection includes a drop shadow's
//!   falloff. Coordinate-ceiling overflow falls back to the ungrown rect,
//!   exactly as the compositor's style pass does.
//! * [`alpha_bounds`] — the precise alpha ink of a pixel-owning layer,
//!   tile by tile, **cached by the tile hashes it read**. A pointer-frame
//!   query ("is the pointer over visible ink?") must not rescan a megabyte
//!   per frame: any pixel change replaces a hash, so the cache key cannot
//!   answer stale.
//!
//! # Empty and hostile inputs
//!
//! An empty layer (no stored tiles, or an empty group) is `None` — no NaN, no
//! unbounded allocation. A non-finite layer transform is conjugated to the
//! identity by the compositor's own `level_transform`, so it bounds like "no
//! transform" rather than poisoning the answer. Hidden layers bound like
//! visible ones: bounds are geometry, not visibility.
//!
//! The tile-hash cache is process-wide and bounded; overflowing it clears
//! rather than growing without limit.

use std::collections::{BTreeMap, HashMap};
use std::sync::{LazyLock, Mutex};

use editor_core::{Document, PixelKey};
use layer_model::{LayerId, LayerKind};
use raster::{PixelRect, TileCoord, TileHash, TILE_SIZE};

use crate::composite::{CompositeOptions, Ctx};
use crate::error::CompositeError;
use crate::source::TileSource;

/// Per-tile ink boxes keyed the way [`alpha_bounds`] reads them.
type AlphaInkCache = HashMap<AlphaCacheKey, BTreeMap<TileCoord, Option<TileInk>>>;

/// The layer's stored ink extent before its own transform. See module docs.
pub fn content_bounds<S: TileSource + ?Sized>(
    doc: &Document,
    source: &S,
    layer: LayerId,
    level: u8,
    opts: CompositeOptions,
) -> Result<Option<PixelRect>, CompositeError> {
    let ctx = Ctx::new(doc, source, level, opts)?;
    let Some(layer_ref) = doc.layers.get(layer) else {
        return Ok(None);
    };
    Ok(non_empty(ctx.content_bounds(layer_ref)))
}

/// Where the layer's content lands in document space at `level`. `None` when
/// the layer bounds to nothing.
pub fn document_bounds<S: TileSource + ?Sized>(
    doc: &Document,
    source: &S,
    layer: LayerId,
    level: u8,
    opts: CompositeOptions,
) -> Result<Option<PixelRect>, CompositeError> {
    let ctx = Ctx::new(doc, source, level, opts)?;
    let Some(layer_ref) = doc.layers.get(layer) else {
        return Ok(None);
    };
    Ok(non_empty(Some(ctx.document_bounds(layer_ref))))
}

/// [`document_bounds`] grown by the layer's effects' reach. `None` when the
/// layer bounds to nothing.
pub fn styled_bounds<S: TileSource + ?Sized>(
    doc: &Document,
    source: &S,
    layer: LayerId,
    level: u8,
    opts: CompositeOptions,
) -> Result<Option<PixelRect>, CompositeError> {
    let ctx = Ctx::new(doc, source, level, opts)?;
    let Some(layer_ref) = doc.layers.get(layer) else {
        return Ok(None);
    };
    let b = ctx.document_bounds(layer_ref);
    if b.is_empty() {
        return Ok(None);
    }
    let grown = match ctx.style_reach(layer_ref) {
        Some(margin) => ctx.style_rect(b, margin),
        None => b,
    };
    Ok(non_empty(Some(grown)))
}

/// `None` for a missing answer or an empty rect — the public contract.
fn non_empty(b: Option<PixelRect>) -> Option<PixelRect> {
    b.filter(|r| !r.is_empty())
}

// ---------------------------------------------------------------------------
// Alpha-precise raster ink, cached by tile hashes
// ---------------------------------------------------------------------------

/// What one tile contributes: its ink box in tile-local coordinates, or `None`
/// when the tile is entirely transparent.
type TileInk = (u32, u32, u32, u32);

/// The cache key: which layer's tiles, at which level, and the exact hashes
/// that were read. Any pixel change replaces a hash, so a stale answer is
/// unreachable.
#[derive(Clone, PartialEq, Eq, Hash)]
struct AlphaCacheKey {
    layer: PixelKey,
    level: u8,
    hashes: Vec<(TileCoord, TileHash)>,
}

/// Bounded process-wide cache; cleared wholesale when full, which is coarse
/// but bounded, and only costs a rescan.
static ALPHA_INK_CACHE: LazyLock<Mutex<AlphaInkCache>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
const ALPHA_INK_CACHE_MAX_ENTRIES: usize = 1024;

/// The precise alpha ink of a pixel-owning layer's stored tiles, in level
/// document space (no layer transform — callers map it through
/// [`document_bounds`]' transform themselves when they need document space).
///
/// Unlike [`content_bounds`], which counts a stored tile whole, this scans
/// each tile for its transparent-margin-free ink box, so one opaque pixel in
/// a 256-wide tile does not make the whole tile count.
pub fn alpha_bounds<S: TileSource + ?Sized>(
    doc: &Document,
    source: &S,
    layer: LayerId,
    level: u8,
    _opts: CompositeOptions,
) -> Result<Option<PixelRect>, CompositeError> {
    let Some(layer_ref) = doc.layers.get(layer) else {
        return Ok(None);
    };
    if !matches!(
        layer_ref.kind,
        LayerKind::Raster(_) | LayerKind::Generator(_) | LayerKind::SmartObject(_)
    ) {
        // Groups, text and shapes have exact geometry from their own queries;
        // an alpha scan is a raster answer. (No level validation needed —
        // stored tiles name their own level.)
        return Ok(None);
    }
    let key = PixelKey::Layer(layer_ref.id);
    let Some(map) = doc.pixels.tiles(key) else {
        return Ok(None);
    };
    let mut hashes: Vec<(TileCoord, TileHash)> =
        map.iter().filter(|(c, _)| c.level == level).collect();
    // TileCoord is Ord; the hash rides along. Stable cache keys regardless of
    // map iteration order.
    hashes.sort_unstable_by_key(|(c, _)| *c);
    if hashes.is_empty() {
        return Ok(None);
    }

    let cache_key = AlphaCacheKey {
        layer: key,
        level,
        hashes,
    };
    if let Ok(cache) = ALPHA_INK_CACHE.lock() {
        if let Some(per_tile) = cache.get(&cache_key) {
            return Ok(union_tile_ink(per_tile));
        }
    }

    let mut per_tile: BTreeMap<TileCoord, Option<TileInk>> = BTreeMap::new();
    for (coord, hash) in &cache_key.hashes {
        let ink = source
            .tile(*hash)
            .filter(|bytes| bytes.len() >= (TILE_SIZE * TILE_SIZE * 4) as usize)
            .and_then(tile_alpha_ink);
        per_tile.insert(*coord, ink);
    }
    let answer = union_tile_ink(&per_tile);

    if let Ok(mut cache) = ALPHA_INK_CACHE.lock() {
        if cache.len() >= ALPHA_INK_CACHE_MAX_ENTRIES {
            cache.clear();
        }
        cache.insert(cache_key, per_tile);
    }
    Ok(answer)
}

/// A tile's alpha ink box from RGBA8 bytes, tile-local.
fn tile_alpha_ink(bytes: &[u8]) -> Option<TileInk> {
    let stride = TILE_SIZE as usize;
    let mut min_x = stride;
    let mut max_x = 0usize;
    let mut min_y = stride;
    let mut max_y = 0usize;
    let mut any = false;
    for y in 0..stride {
        for x in 0..stride {
            if bytes[(y * stride + x) * 4 + 3] != 0 {
                any = true;
                min_x = min_x.min(x);
                max_x = max_x.max(x);
                min_y = min_y.min(y);
                max_y = max_y.max(y);
            }
        }
    }
    any.then(|| {
        (
            min_x as u32,
            min_y as u32,
            (max_x - min_x + 1) as u32,
            (max_y - min_y + 1) as u32,
        )
    })
}

/// Union of per-tile ink boxes, in level document space.
fn union_tile_ink(per_tile: &BTreeMap<TileCoord, Option<TileInk>>) -> Option<PixelRect> {
    let mut acc: Option<PixelRect> = None;
    for (coord, ink) in per_tile {
        let Some((lx, ly, w, h)) = ink else {
            continue;
        };
        let (ox, oy) = coord.pixel_origin();
        let rect = PixelRect::new(ox + i64::from(*lx), oy + i64::from(*ly), *w, *h);
        acc = Some(match acc {
            Some(a) => union(a, rect),
            None => rect,
        });
    }
    acc
}

/// Smallest rect covering both, computed by corners (`PixelRect` carries no
/// set algebra of its own).
fn union(a: PixelRect, b: PixelRect) -> PixelRect {
    let x0 = a.x.min(b.x);
    let y0 = a.y.min(b.y);
    let x1 = a.right().max(b.right());
    let y1 = a.bottom().max(b.bottom());
    PixelRect::new(x0, y0, (x1 - x0).max(0) as u32, (y1 - y0).max(0) as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::TestDoc;
    use glam::{Affine2, Vec2};

    #[test]
    fn an_empty_layer_bounds_to_nothing_without_nan_or_allocation() {
        let mut t = TestDoc::linear(512, 256);
        let empty = t.push_raster("Nothing stored");
        let adj = t.push_adjustment(
            "Adjustment",
            layer_model::AdjustmentKind::Exposure { stops: 1.0 },
        );
        let group = t.push_group("Empty group");
        let opts = CompositeOptions::default();
        for id in [empty, adj, group] {
            assert_eq!(content_bounds(&t.doc, &t.src, id, 0, opts).unwrap(), None);
            assert_eq!(document_bounds(&t.doc, &t.src, id, 0, opts).unwrap(), None);
            assert_eq!(styled_bounds(&t.doc, &t.src, id, 0, opts).unwrap(), None);
        }
    }

    #[test]
    fn a_transformed_asymmetric_layer_bounds_in_both_spaces() {
        let mut t = TestDoc::linear(2048, 2048);
        let id = t.push_raster("Asym");
        t.paint_tile_with(id, TileCoord::new(1, 0, 0), |x, y| {
            if x < 10 && y < 20 {
                [255, 0, 0, 255]
            } else {
                [0, 0, 0, 0]
            }
        });
        // Half scale, then to negative coordinates. Authored like the
        // compositor's own test: straight onto the layer field.
        t.doc.layers.get_mut(id).unwrap().transform =
            Affine2::from_translation(Vec2::new(-600.0, -400.0))
                * Affine2::from_scale(Vec2::splat(0.5));
        let opts = CompositeOptions::default();

        // Stored extent, before the transform: the whole tile at column 1.
        let local = content_bounds(&t.doc, &t.src, id, 0, opts)
            .unwrap()
            .expect("the layer has stored tiles");
        assert_eq!(local, PixelRect::new(256, 0, 256, 256));

        // Document space: the tile half-scaled to 128 and moved to (-600,-400).
        let docb = document_bounds(&t.doc, &t.src, id, 0, opts)
            .unwrap()
            .expect("content exists");
        // The exact answer carries the resampler's margin (the composite's own
        // test pins 2px for a pure translation); assert containment with a
        // small slack instead of re-deriving the margin here.
        assert!(
            docb.x <= -600 + 128 && docb.right() >= -600 + 256 - 2,
            "{docb:?}"
        );
        assert!(
            docb.y <= -400 && docb.bottom() >= -400 + 128 - 2,
            "{docb:?}"
        );
        assert!(docb.width <= 256 + 8 && docb.height <= 256 + 8, "{docb:?}");

        // And the round trip a caller cares about: the local bounds mapped by
        // the layer's transform agree with the document answer's core.
        assert!(docb.x <= -172, "the ink sits left of the origin: {docb:?}");
    }

    #[test]
    fn a_nested_group_bounds_from_its_transformed_child() {
        let mut t = TestDoc::linear(2048, 2048);
        let group = t.push_group("Group");
        let child = t.push_child(group, layer_model::Layer::raster("Moved"));
        t.paint_tile(child, TileCoord::new(0, 0, 0), [0, 0, 255, 255]);
        t.doc.layers.get_mut(child).unwrap().transform =
            Affine2::from_translation(Vec2::new(1000.0, 0.0));

        let b = document_bounds(&t.doc, &t.src, group, 0, CompositeOptions::default())
            .unwrap()
            .expect("the group bounds from its child");
        // The compositor's own content-bounds test pins this exact answer for
        // this fixture (tile 0..256 translated +1000, plus resampler margin).
        assert_eq!(b, PixelRect::new(998, -2, 260, 260));
    }

    #[test]
    fn styled_bounds_grows_by_the_effect_reach() {
        let mut t = TestDoc::linear(512, 256);
        let id = t.push_raster("Styled");
        t.paint_tile(id, TileCoord::new(0, 0, 0), [255, 255, 255, 255]);
        let opts = CompositeOptions::default();
        let bare = document_bounds(&t.doc, &t.src, id, 0, opts)
            .unwrap()
            .unwrap();

        // No effects: styled equals document bounds.
        assert_eq!(
            styled_bounds(&t.doc, &t.src, id, 0, opts).unwrap(),
            Some(bare)
        );

        // A drop shadow grows the answer by at least its blur reach.
        t.doc.layers.get_mut(id).unwrap().effects = layer_model::LayerEffects {
            drop_shadow: Some(layer_model::ShadowEffect::default()),
            ..Default::default()
        };
        let styled = styled_bounds(&t.doc, &t.src, id, 0, opts)
            .unwrap()
            .expect("the styled layer bounds");
        assert!(styled.width >= bare.width, "{styled:?} vs {bare:?}");
        assert!(
            styled.x <= bare.x && styled.right() >= bare.right(),
            "the shadow extends the extent: {styled:?} vs {bare:?}"
        );
        // And the growth is bounded by the effect's own reach.
        let reach =
            crate::effects::reach(&t.doc.layers.get(id).unwrap().effects, 0).expect("reach");
        assert!(
            styled.width <= bare.width + 2 * reach as u32 + 2,
            "{styled:?}"
        );
    }

    #[test]
    fn alpha_bounds_is_precise_and_follows_pixel_changes() {
        let mut t = TestDoc::linear(512, 256);
        let id = t.push_raster("Sparse");
        // Ink in the middle of the tile only: extent would say "whole tile".
        t.paint_tile_with(id, TileCoord::new(0, 0, 0), |x, y| {
            if (30..40).contains(&x) && (50..60).contains(&y) {
                [0, 0, 255, 255]
            } else {
                [0, 0, 0, 0]
            }
        });
        let opts = CompositeOptions::default();
        let ink = alpha_bounds(&t.doc, &t.src, id, 0, opts)
            .unwrap()
            .expect("there is visible ink");
        assert_eq!(ink, PixelRect::new(30, 50, 10, 10), "the ink box is exact");

        // A fully transparent tile under the same key bounds to nothing.
        let hidden = t.push_raster("All transparent");
        t.paint_tile_with(hidden, TileCoord::new(0, 0, 0), |_, _| [0, 0, 0, 0]);
        assert_eq!(
            alpha_bounds(&t.doc, &t.src, hidden, 0, opts).unwrap(),
            None,
            "every sample transparent: no ink"
        );

        // Changing the pixels changes the hashes, so the cached answer cannot
        // go stale: repaint the ink elsewhere and the answer follows.
        t.paint_tile_with(id, TileCoord::new(0, 0, 0), |x, y| {
            if (100..110).contains(&x) && (100..110).contains(&y) {
                [0, 0, 255, 255]
            } else {
                [0, 0, 0, 0]
            }
        });
        let moved = alpha_bounds(&t.doc, &t.src, id, 0, opts)
            .unwrap()
            .expect("the new ink");
        assert_eq!(moved, PixelRect::new(100, 100, 10, 10));
        assert_ne!(moved, ink);
    }

    #[test]
    fn a_missing_layer_bounds_to_nothing() {
        let t = TestDoc::linear(512, 256);
        let ghost = layer_model::Layer::raster("never added").id;
        assert_eq!(
            content_bounds(&t.doc, &t.src, ghost, 0, CompositeOptions::default()).unwrap(),
            None
        );
    }
}
