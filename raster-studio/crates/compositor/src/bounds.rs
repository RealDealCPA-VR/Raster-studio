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
use glam::{Affine2, Vec2};
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

/// Where level-0 stored tiles of `layer` land on the document — the
/// rectangles a presenter has to redraw when exactly those tiles were
/// rewritten (a brush stroke, or the undo of one).
///
/// `mask == false` names the layer's own tiles, mapped through the layer's
/// transform; `mask == true` names its mask's tiles, mapped through the same
/// pose the compositor samples the mask with (layer transform composed with
/// the mask's own). Each rectangle is then grown by the layer's effects'
/// reach, because repainting a pixel moves the drop shadow it casts. On an
/// untransformed, unstyled layer the answer is the tile rectangle itself.
///
/// `Ok(None)` when the answer is not a set of rectangles — the layer or its
/// mask is missing, a coordinate is not level 0, or the pose carries a tile
/// past the coordinate ceiling — and a caller treats that as "redraw
/// everything". Never a guess.
pub fn edited_tiles_on_document<S: TileSource + ?Sized>(
    doc: &Document,
    source: &S,
    layer: LayerId,
    mask: bool,
    coords: &[TileCoord],
    opts: CompositeOptions,
) -> Result<Option<Vec<PixelRect>>, CompositeError> {
    let ctx = Ctx::new(doc, source, 0, opts)?;
    let Some(layer_ref) = doc.layers.get(layer) else {
        return Ok(None);
    };
    let mut pose = finite_or_identity(layer_ref.transform);
    if mask {
        let Some(m) = layer_ref.mask.as_ref() else {
            return Ok(None);
        };
        pose *= finite_or_identity(*m.transform);
    }
    let identity = pose.abs_diff_eq(Affine2::IDENTITY, 1e-6);
    let reach = ctx.style_reach(layer_ref);
    let mut out = Vec::with_capacity(coords.len());
    for c in coords {
        if c.level != 0 {
            return Ok(None);
        }
        let (ox, oy) = c.pixel_origin();
        let local = PixelRect::new(ox, oy, TILE_SIZE, TILE_SIZE);
        let mapped = if identity {
            local
        } else {
            match mapped_rect(&pose, local) {
                Some(r) => r,
                None => return Ok(None),
            }
        };
        out.push(match reach {
            Some(margin) => ctx.style_rect(mapped, margin),
            None => mapped,
        });
    }
    Ok(Some(out))
}

/// A transform with a non-finite component is the identity, exactly as the
/// compositor's own `level_transform` treats it, so a corrupt matrix bounds
/// like "no transform" instead of poisoning the answer.
fn finite_or_identity(t: Affine2) -> Affine2 {
    if t.to_cols_array().iter().all(|v| v.is_finite()) {
        t
    } else {
        Affine2::IDENTITY
    }
}

/// The bounding box of `rect`'s image under `t`, widened by the two-pixel
/// bilinear margin the compositor's own `image_rect` uses: a destination
/// pixel can be reached by taps from up to a pixel outside the mapped shape.
/// `None` when the image does not fit the coordinate ceiling.
fn mapped_rect(t: &Affine2, rect: PixelRect) -> Option<PixelRect> {
    let (mut lo, mut hi) = (Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY));
    for (cx, cy) in [
        (rect.x, rect.y),
        (rect.right(), rect.y),
        (rect.x, rect.bottom()),
        (rect.right(), rect.bottom()),
    ] {
        let p = t.transform_point2(Vec2::new(cx as f32, cy as f32));
        if !p.x.is_finite() || !p.y.is_finite() {
            return None;
        }
        lo = lo.min(p);
        hi = hi.max(p);
    }
    let x0 = (lo.x.floor() as i64).saturating_sub(2);
    let y0 = (lo.y.floor() as i64).saturating_sub(2);
    let x1 = (hi.x.ceil() as i64).saturating_add(2);
    let y1 = (hi.y.ceil() as i64).saturating_add(2);
    let width = u32::try_from(x1.checked_sub(x0)?).ok()?;
    let height = u32::try_from(y1.checked_sub(y0)?).ok()?;
    Some(PixelRect::new(x0, y0, width, height))
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

/// A tile's alpha ink box, tile-local, from RGBA8 or RGBA16 bytes (the
/// depth is read by [`raster::tile_alpha16`], so a 16-bit layer's tile is
/// never mis-strided as RGBA8).
fn tile_alpha_ink(bytes: &[u8]) -> Option<TileInk> {
    let stride = TILE_SIZE as usize;
    let mut min_x = stride;
    let mut max_x = 0usize;
    let mut min_y = stride;
    let mut max_y = 0usize;
    let mut any = false;
    for y in 0..stride {
        for x in 0..stride {
            if raster::tile_alpha16(bytes, y * stride + x).is_some_and(|a| a != 0) {
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

    /// W4-F: a 16-bit layer's RGBA16 tile is read at its own 8-byte stride.
    /// Read as RGBA8, this tile gave `PixelRect { x: 0, y: 0, width: 32,
    /// height: 255 }` — the Move tool's tight bounds and the Properties
    /// panel's measurement were wrong for every 16-bit document.
    #[test]
    fn alpha_bounds_reads_an_rgba16_tile_at_sixteen_bits() {
        let mut t = TestDoc::linear(512, 256);
        let id = t.push_raster("Deep");
        let mut samples = Vec::with_capacity((TILE_SIZE * TILE_SIZE * 4) as usize);
        for _y in 0..TILE_SIZE {
            for x in 0..TILE_SIZE {
                if x < 16 {
                    samples.extend_from_slice(&[51_400, 25_700, 12_850, u16::MAX]);
                } else {
                    samples.extend_from_slice(&[0, 0, 0, 0]);
                }
            }
        }
        let hash = t.src.insert_bytes(raster::rgba16_to_tile_bytes(&samples));
        t.set_tile_hash(id, TileCoord::new(0, 0, 0), hash);
        let ink = alpha_bounds(&t.doc, &t.src, id, 0, CompositeOptions::default())
            .unwrap()
            .expect("there is visible ink");
        assert_eq!(ink, PixelRect::new(0, 0, 16, 256));
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

    /// The rectangles an edit's tiles dirty on the canvas: the tile itself on
    /// a plain layer, moved with the layer's transform, grown by its style,
    /// and never a guess where the pose is not a rectangle.
    #[test]
    fn edited_tiles_land_where_the_layer_puts_them_grown_by_its_style() {
        let mut t = TestDoc::linear(1024, 512);
        let id = t.push_raster("Ink");
        t.paint_tile(id, TileCoord::new(0, 0, 0), [255, 0, 0, 255]);
        let opts = CompositeOptions::default();
        let tile = TileCoord::new(1, 0, 0);
        let tile_rect = PixelRect::new(256, 0, 256, 256);

        // Untransformed, unstyled: exactly the tile.
        let plain = edited_tiles_on_document(&t.doc, &t.src, id, false, &[tile], opts)
            .unwrap()
            .expect("a rectangle answer");
        assert_eq!(plain, vec![tile_rect]);

        // Moved right by 300: the tile rectangle moves with it (plus the
        // compositor's two-pixel bilinear margin), so an undo of a dab on a
        // moved layer redraws where the dab is *seen*, not where it is stored.
        t.doc.layers.get_mut(id).unwrap().transform =
            Affine2::from_translation(Vec2::new(300.0, 0.0));
        let moved = edited_tiles_on_document(&t.doc, &t.src, id, false, &[tile], opts)
            .unwrap()
            .expect("a rectangle answer");
        assert_eq!(moved.len(), 1);
        let m = moved[0];
        assert!(
            m.x <= 556 && m.right() >= 812,
            "{m:?} does not cover the moved tile"
        );
        assert!(
            m.x >= 554 && m.right() <= 814,
            "{m:?} is wider than the margin allows"
        );
        assert!(m.y <= 0 && m.bottom() >= 256);

        // A drop shadow grows every rectangle by the effect's reach.
        t.doc.layers.get_mut(id).unwrap().transform = Affine2::IDENTITY;
        t.doc.layers.get_mut(id).unwrap().effects = layer_model::LayerEffects {
            drop_shadow: Some(layer_model::ShadowEffect::default()),
            ..Default::default()
        };
        let reach =
            crate::effects::reach(&t.doc.layers.get(id).unwrap().effects, 0).expect("reach");
        assert!(reach > 0);
        let styled = edited_tiles_on_document(&t.doc, &t.src, id, false, &[tile], opts)
            .unwrap()
            .expect("a rectangle answer");
        assert_eq!(styled.len(), 1);
        let s = styled[0];
        assert!(s.x < tile_rect.x && s.right() > tile_rect.right(), "{s:?}");
        assert!(
            s.x >= tile_rect.x - reach && s.right() <= tile_rect.right() + reach,
            "{s:?}"
        );

        // Mask tiles map through the mask's pose as well as the layer's.
        t.doc.layers.get_mut(id).unwrap().effects = layer_model::LayerEffects::default();
        assert!(
            edited_tiles_on_document(&t.doc, &t.src, id, true, &[tile], opts)
                .unwrap()
                .is_none(),
            "no mask, no rectangle"
        );
        let _mask = t.attach_mask(id);
        {
            let l = t.doc.layers.get_mut(id).unwrap();
            *l.mask.as_mut().unwrap().transform = Affine2::from_translation(Vec2::new(0.0, 256.0));
        }
        let masked = edited_tiles_on_document(&t.doc, &t.src, id, true, &[tile], opts)
            .unwrap()
            .expect("a rectangle answer");
        assert_eq!(masked.len(), 1);
        assert!(
            masked[0].y <= 256 && masked[0].bottom() >= 512,
            "{:?}",
            masked[0]
        );
        assert!(masked[0].y >= 254, "{:?}", masked[0]);

        // Not a rectangle answer: a mip tile, and a layer that is gone.
        assert!(edited_tiles_on_document(
            &t.doc,
            &t.src,
            id,
            false,
            &[TileCoord::new(0, 0, 1)],
            opts
        )
        .unwrap()
        .is_none());
        assert!(
            edited_tiles_on_document(&t.doc, &t.src, LayerId::new(), false, &[tile], opts)
                .unwrap()
                .is_none()
        );
    }
}
