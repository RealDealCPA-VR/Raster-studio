//! W13-I: the Crop tool's Content-Aware option — the canvas a crop ADDS is
//! filled from the image.
//!
//! With Content-Aware on ([`tools::edit::CROP_CONTENT_AWARE_KEY`]) the crop
//! box may be dragged past the canvas, and a straightened crop uncovers
//! corners the old canvas never reached. Either way the new canvas has
//! pixels no old canvas pixel maps to. [`fill_new_area`] finds them on the
//! active pixel layer, in the layer's own pixel space (the crop moves layers
//! by their transforms, it does not resample them), and synthesises them
//! from the rest of the layer with the same PatchMatch fill Edit ▸ Fill ▸
//! Content-Aware runs ([`filters::content_aware_fill`], fixed seed). The
//! result is one [`Command::PaintTiles`] the commit route appends to the
//! crop's own transaction, so the crop and its fill are one Ctrl+Z.
//!
//! Limits, named rather than hidden: only the active layer is filled, and
//! only a raster layer of an 8-bit document (a 16- or 32-bit document crops
//! unfilled and the status line says so: the fill writes RGBA8 tiles, which
//! would round 16-bit codes and clip 32-bit HDR values); the synthesis runs on the
//! interaction thread (the crop waits for it); and the region is bounded by
//! the fill's own context limit, past which the crop lands unfilled and the
//! status line says why. A pixel the layer already holds past the old
//! canvas (content Reveal All would show) is kept, not overwritten.

use std::collections::HashMap;

use editor_core::{Command, PixelTarget, TileEdit};
use glam::{Affine2, Vec2};
use layer_model::LayerId;
use raster::{TileCoord, TILE_SIZE};
use tools::CropRequest;

use crate::doc::OpenDocument;

/// A layer's full transform: layer space to document space, through every
/// ancestor group (the same walk `crate::crop_apply` makes).
fn world_transform(doc: &OpenDocument, id: LayerId) -> Affine2 {
    let layers = &doc.document.layers;
    let mut t = layers
        .get(id)
        .map(|l| l.transform)
        .unwrap_or(Affine2::IDENTITY);
    let mut parent = layers.parent_of(id);
    while let Some(p) = parent {
        if let Some(group) = layers.get(p) {
            t = group.transform * t;
        }
        parent = layers.parent_of(p);
    }
    t
}

/// The pixel layer the fill paints: the active layer, when it is raster.
fn target_layer(doc: &OpenDocument) -> Result<LayerId, String> {
    let id = doc
        .document
        .active_layer()
        .ok_or("Content-Aware crop needs an active pixel layer")?;
    match doc.document.layers.get(id).map(|l| &l.kind) {
        Some(layer_model::LayerKind::Raster(_)) => Ok(id),
        _ => Err("Content-Aware crop fills a pixel layer; the active layer is not one".into()),
    }
}

/// The paint command that fills the canvas `req` adds on the active pixel
/// layer — `Ok(None)` when the crop adds no canvas the layer does not
/// already cover. Built from the document BEFORE the crop is applied (the
/// crop changes transforms only, so the layer's pixels are the same after).
///
/// `pending` is the rest of the crop's transaction — the commands the fill
/// is appended after. When Delete Cropped Pixels is on, it already rewrites
/// some of this layer's tiles (clearing what falls outside the new canvas);
/// a tile the fill touches starts from THAT result, not from the pre-crop
/// bytes, or the fill's whole-tile write would bring the cropped-away pixels
/// back. The synthesis still reads the layer as it was (the image the crop
/// was drawn on).
pub(crate) fn fill_new_area(
    doc: &mut OpenDocument,
    req: &CropRequest,
    pending: &[Command],
) -> Result<Option<Command>, String> {
    // 8-bit only: the synthesis reads tiles through the 8-bit view and
    // writes whole RGBA8 tiles, which would round 16-bit codes and clip a
    // 32-bit document's HDR values in every tile the fill touches.
    if doc.document.meta.bit_depth != 8 {
        return Err("Content-Aware crop fills 8-bit documents only".into());
    }
    let layer = target_layer(doc)?;
    let Some(plan) = crate::crop_apply::plan(req) else {
        return Ok(None);
    };
    let (old_w, old_h) = (doc.document.width() as f32, doc.document.height() as f32);
    let (new_w, new_h) = (plan.size.x as f32, plan.size.y as f32);
    let world = world_transform(doc, layer);
    let local_to_new = plan.to_new * world;
    let new_to_local = local_to_new.inverse();
    let doc_to_local = world.inverse();

    // The layer-space box holding the new canvas and the old one (the
    // context the synthesis reads).
    let corners = |m: Affine2, w: f32, h: f32| {
        [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)].map(|(x, y)| m.transform_point2(Vec2::new(x, y)))
    };
    let mut lo = Vec2::splat(f32::INFINITY);
    let mut hi = Vec2::splat(f32::NEG_INFINITY);
    for p in corners(new_to_local, new_w, new_h)
        .into_iter()
        .chain(corners(doc_to_local, old_w, old_h))
    {
        if !p.is_finite() {
            return Err("Content-Aware crop: the crop's geometry is not finite".into());
        }
        lo = lo.min(p);
        hi = hi.max(p);
    }
    let (x0, y0) = (lo.x.floor() as i64, lo.y.floor() as i64);
    let (x1, y1) = (hi.x.ceil() as i64, hi.y.ceil() as i64);
    let (w, h) = ((x1 - x0).max(0) as usize, (y1 - y0).max(0) as usize);
    if w == 0 || h == 0 {
        return Ok(None);
    }
    if w.saturating_mul(h) > filters::content_aware::MAX_CONTEXT_PIXELS * 4 {
        return Err("Content-Aware crop: the new canvas is too large to fill".into());
    }

    // The layer's own pixels over the box, straight from its tiles.
    let ts = TILE_SIZE as i64;
    let tiles = doc.document.layer_tiles(layer).cloned();
    let read_tile = |coord: TileCoord| -> Option<Vec<u8>> {
        let hash = tiles.as_ref()?.get(coord)?;
        let stored = compositor::TileSource::tile(&doc.tiles, hash)?;
        let bytes = raster::rgba8_view(stored);
        (bytes.len() == (ts * ts * 4) as usize).then(|| bytes.into_owned())
    };
    let mut cache: HashMap<(i32, i32), Option<Vec<u8>>> = HashMap::new();
    let mut rgba = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            let (lx, ly) = (x0 + x as i64, y0 + y as i64);
            let key = (lx.div_euclid(ts) as i32, ly.div_euclid(ts) as i32);
            let tile = cache
                .entry(key)
                .or_insert_with(|| read_tile(TileCoord::new(key.0, key.1, 0)));
            if let Some(bytes) = tile {
                let i = ((ly.rem_euclid(ts) * ts + lx.rem_euclid(ts)) * 4) as usize;
                rgba[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&bytes[i..i + 4]);
            }
        }
    }

    // The hole: layer pixels the new canvas shows, that no old canvas pixel
    // mapped to, and that the layer does not already hold.
    let mut hole = vec![false; w * h];
    let mut any = false;
    for y in 0..h {
        for x in 0..w {
            let local = Vec2::new((x0 + x as i64) as f32 + 0.5, (y0 + y as i64) as f32 + 0.5);
            let q = local_to_new.transform_point2(local);
            let in_new = q.x >= 0.0 && q.y >= 0.0 && q.x < new_w && q.y < new_h;
            let d = world.transform_point2(local);
            let in_old = d.x >= 0.0 && d.y >= 0.0 && d.x < old_w && d.y < old_h;
            if in_new && !in_old && rgba[(y * w + x) * 4 + 3] == 0 {
                hole[y * w + x] = true;
                any = true;
            }
        }
    }
    if !any {
        return Ok(None);
    }
    let source =
        filters::FilterBuffer::from_rgba8(w as u32, h as u32, &rgba).map_err(|e| e.to_string())?;
    let filled = filters::content_aware_fill(&source, &hole, filters::FillOptions::default())
        .map_err(|e| format!("Content-Aware crop fill refused: {e}"))?
        .to_rgba8();

    // What the crop's own edits leave in a tile of this layer: `None` when
    // `pending` does not touch it, `Some(None)` when it clears it.
    let pending_tile = |key: (i32, i32)| -> Option<Option<Vec<u8>>> {
        let coord = TileCoord::new(key.0, key.1, 0);
        let edit = pending.iter().rev().find_map(|command| match command {
            Command::PaintTiles {
                target: PixelTarget::Layer(id),
                delta,
            } if *id == layer => delta.get(coord),
            _ => None,
        })?;
        Some(edit.and_then(|hash| {
            let stored = compositor::TileSource::tile(&doc.tiles, hash)?;
            let bytes = raster::rgba8_view(stored);
            (bytes.len() == (ts * ts * 4) as usize).then(|| bytes.into_owned())
        }))
    };

    // Write the filled pixels back into their tiles, each tile starting from
    // what it holds once the rest of the crop has run.
    let mut touched: HashMap<(i32, i32), Vec<u8>> = HashMap::new();
    for y in 0..h {
        for x in 0..w {
            if !hole[y * w + x] {
                continue;
            }
            let (lx, ly) = (x0 + x as i64, y0 + y as i64);
            let key = (lx.div_euclid(ts) as i32, ly.div_euclid(ts) as i32);
            let bytes = touched.entry(key).or_insert_with(|| {
                pending_tile(key)
                    .unwrap_or_else(|| cache.get(&key).cloned().flatten())
                    .unwrap_or_else(|| vec![0u8; (ts * ts * 4) as usize])
            });
            let i = ((ly.rem_euclid(ts) * ts + lx.rem_euclid(ts)) * 4) as usize;
            let s = (y * w + x) * 4;
            bytes[i..i + 4].copy_from_slice(&filled[s..s + 4]);
        }
    }
    let mut keys: Vec<_> = touched.keys().copied().collect();
    keys.sort_unstable();
    let edits: Vec<TileEdit> = keys
        .into_iter()
        .map(|key| {
            let bytes = touched.remove(&key).unwrap_or_default();
            let hash = doc.tiles.insert_bytes(bytes);
            TileEdit::set(TileCoord::new(key.0, key.1, 0), hash)
        })
        .collect();
    Command::paint_tiles(PixelTarget::Layer(layer), edits)
        .map(Some)
        .map_err(|e| e.to_string())
}
