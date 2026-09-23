//! Card 037: rendered-content hit testing.
//!
//! The Move tool's auto-select used to sample raw document-coordinate tiles
//! for every layer in the stack — which picked nothing on text or shape
//! layers, ignored visibility, opacity and masks, and happily sampled layers
//! whose bounds do not even contain the point. This module replaces that with
//! a bounded visible-content test:
//!
//! 1. **Bounds first.** Every candidate layer's stored extent is mapped into
//!    document space (`document_bounds`) and only layers whose bounds contain
//!    the point are sampled at all.
//! 2. **Visibility composes.** A layer is a candidate only when it and every
//!    ancestor are visible.
//! 3. **Alpha, opacity and masks gate the sample.** A raster or smart-object
//!    layer is hit when `alpha × opacity × mask coverage ≥ threshold` at the
//!    inverse-mapped point (linked masks ride the content's space; unlinked
//!    masks live in document space). Mask samples route through the mask's
//!    own `coverage` — enabled/inverted/density apply exactly as they do in
//!    the composite, and an absent mask tile is a raw 0.0 sample (hidden for
//!    a plain mask, revealed for an inverted one).
//! 4. **Vector kinds pick on bounds.** Text and shape ink is the shaped /
//!    geometric coverage — `document_bounds` for text is exactly the shaped
//!    run's ink box — so a point inside the bounds is a hit.
//! 5. **Adjustment and generator layers are never object-picked** (they have
//!    no content of their own), and **effect-only pixels are excluded by
//!    construction**: content bounds are the stored extent, not the styled
//!    (`styled_bounds`) extent that effect shadows inflate.
//! 6. **Locks do not gate picking** — picking is not editing. The Move tool
//!    refuses locked targets downstream, which is where the lock belongs.
//!
//! Groups are walked through, never picked: the hit is always a leaf layer,
//! which keeps group-vs-layer selection explicit (a group is selected from
//! the panel, its children from the canvas).
//!
//! Everything reads through the document's own pixel store and tile source —
//! no mutable tool access, so the query runs before the tool context is
//! built. Bounds are computed per query (each layer is visited once anyway);
//! a cross-query cache rides the transform-session work (card 039's geometry
//! publisher owns the session lifetime).

use compositor::{CompositeOptions, TileSource};
use editor_core::{PixelKey, PixelStore};
use glam::{IVec2, Vec2};
use layer_model::LayerKind;
use raster::TILE_SIZE;

use crate::interaction_geometry::{document_to_layer, document_to_mask, document_transform_of};

/// One canvas pick: the leaf layer under the point and whether it is a group
/// (always `false` today — groups are walked through — but the field keeps
/// group-vs-layer selection explicit for callers).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContentHit {
    /// The leaf layer the content belongs to.
    pub layer: layer_model::LayerId,
    /// `true` when the hit layer is a group container.
    pub is_group: bool,
}

/// The top-most visible-content leaf under `doc_pt`, or `None`.
///
/// `threshold` is the minimum effective alpha (`pixel alpha × layer opacity ×
/// mask coverage`) a hit must clear.
pub fn visible_content_at(
    doc: &editor_core::Document,
    pixels: &PixelStore,
    source: &dyn TileSource,
    doc_pt: Vec2,
    threshold: f32,
) -> Option<ContentHit> {
    for id in doc.layers.iter_depth_first() {
        let Some(layer) = doc.layers.get(id) else {
            continue;
        };
        // Visibility composes: the layer and every ancestor must be visible.
        if !layer.visible {
            continue;
        }
        let mut ancestor = doc.layers.parent_of(id);
        let mut hidden = false;
        while let Some(a) = ancestor {
            match doc.layers.get(a) {
                Some(l) if l.visible => ancestor = doc.layers.parent_of(a),
                Some(_) => {
                    hidden = true;
                    break;
                }
                None => break,
            }
        }
        if hidden {
            continue;
        }
        // Adjustment and generator layers have no pickable content; groups
        // are containers whose children are walked individually.
        if matches!(
            layer.kind,
            LayerKind::Adjustment(_) | LayerKind::Generator(_) | LayerKind::Group(_)
        ) {
            continue;
        }
        // Bounds candidate filter: only layers whose document-space extent
        // contains the point are sampled. Content bounds (not styled bounds)
        // keep effect-only shadow pixels out of the pick. The stored extent
        // lives in the layer's OWN space — compose it through the layer's
        // full placement (ancestors included, the same chain
        // document_to_layer inverts) or children of transformed groups
        // would reject points over their visible ink.
        let Some(own_bounds) =
            compositor::bounds::content_bounds(doc, source, id, 0, CompositeOptions::default())
                .ok()
                .flatten()
        else {
            continue;
        };
        let Some(placement) = document_transform_of(doc, id, 0).ok() else {
            continue;
        };
        let corners = [
            Vec2::new(own_bounds.x as f32, own_bounds.y as f32),
            Vec2::new(
                (own_bounds.x + own_bounds.width as i64) as f32,
                own_bounds.y as f32,
            ),
            Vec2::new(
                (own_bounds.x + own_bounds.width as i64) as f32,
                (own_bounds.y + own_bounds.height as i64) as f32,
            ),
            Vec2::new(
                own_bounds.x as f32,
                (own_bounds.y + own_bounds.height as i64) as f32,
            ),
        ];
        let placed: Vec<Vec2> = corners
            .iter()
            .map(|c| placement.transform_point2(*c))
            .collect();
        let min_x = placed.iter().map(|p| p.x).fold(f32::INFINITY, f32::min);
        let max_x = placed.iter().map(|p| p.x).fold(f32::NEG_INFINITY, f32::max);
        let min_y = placed.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
        let max_y = placed.iter().map(|p| p.y).fold(f32::NEG_INFINITY, f32::max);
        let in_bounds =
            doc_pt.x >= min_x && doc_pt.x < max_x && doc_pt.y >= min_y && doc_pt.y < max_y;
        if !in_bounds {
            continue;
        }
        // Vector kinds pick on bounds — their ink is the shaped coverage the
        // bounds already describe.
        if matches!(layer.kind, LayerKind::Text(_) | LayerKind::Shape(_)) {
            return Some(ContentHit {
                layer: id,
                is_group: false,
            });
        }
        // Raster and smart-object kinds sample alpha at the inverse-mapped
        // point, gated by the layer's opacity and its mask's coverage.
        let Some(local) = document_to_layer(doc, id, 0, doc_pt).ok() else {
            continue;
        };
        let pt = IVec2::new(local.x.floor() as i32, local.y.floor() as i32);
        let Some(alpha) = sample_alpha(pixels, source, id, pt) else {
            continue;
        };
        let mut effective = alpha * layer.effective_opacity();
        if let Some(mask) = &layer.mask {
            let Some(mask_pt) = document_to_mask(doc, id, 0, mask, doc_pt).ok() else {
                continue;
            };
            let mpt = IVec2::new(mask_pt.x.floor() as i32, mask_pt.y.floor() as i32);
            // The compositor's convention: an absent mask tile is a raw 0.0
            // sample fed through the mask's own coverage (so a plain mask
            // hides there, an inverted mask reveals, a disabled mask ignores
            // the whole thing), and density/inversion apply identically.
            let raw = sample_mask_coverage(pixels, source, mask.id, mpt).unwrap_or(0.0);
            effective *= mask.coverage(raw);
        }
        if effective < threshold {
            continue;
        }
        return Some(ContentHit {
            layer: id,
            is_group: false,
        });
    }
    None
}

/// The alpha byte at a layer-local pixel, `None` where nothing is stored.
fn sample_alpha(
    pixels: &PixelStore,
    source: &dyn TileSource,
    layer: layer_model::LayerId,
    pt: IVec2,
) -> Option<f32> {
    let coord = tile_coord_of(pt);
    let hash = pixels.tiles(PixelKey::Layer(layer))?.get(coord)?;
    let bytes = source.tile(hash)?;
    // Depth-aware: a 16-bit layer's RGBA16 tile is read at its own stride.
    let t = TILE_SIZE as i32;
    let index = (pt.y.rem_euclid(t) * t + pt.x.rem_euclid(t)) as usize;
    let alpha = raster::tile_alpha16(bytes, index)?;
    Some(f32::from(alpha) / 65_535.0)
}

/// A mask's coverage byte at mask-space coordinates, as a `0..=1` sample.
/// `None` where the mask stores nothing — the CALLER feeds that through the
/// mask's own `coverage` (a plain mask hides there, an inverted one reveals),
/// matching the compositor's absent-tile convention.
fn sample_mask_coverage(
    pixels: &PixelStore,
    source: &dyn TileSource,
    mask: layer_model::MaskId,
    pt: IVec2,
) -> Option<f32> {
    let coord = tile_coord_of(pt);
    let hash = pixels.tiles(PixelKey::Mask(mask))?.get(coord)?;
    let bytes = source.tile(hash)?;
    // Mask coverage tiles are one byte per pixel.
    Some(f32::from(tile_byte(bytes, pt, 1, 0)?) / 255.0)
}

fn tile_coord_of(pt: IVec2) -> raster::TileCoord {
    raster::TileCoord::new(
        pt.x.div_euclid(TILE_SIZE as i32),
        pt.y.div_euclid(TILE_SIZE as i32),
        0,
    )
}

/// One channel of the pixel at document-grid `pt` inside `bytes` (a full
/// tile's worth, RGBA or single-channel by `channel`).
fn tile_byte(bytes: &[u8], pt: IVec2, stride: usize, channel: usize) -> Option<u8> {
    let t = TILE_SIZE as i32;
    let lx = pt.x.rem_euclid(t) as usize;
    let ly = pt.y.rem_euclid(t) as usize;
    let index = (ly * t as usize + lx) * stride + channel;
    bytes.get(index).copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{DocumentId, OpenDocument};
    use compositor::MemoryTileSource;
    use editor_core::{Command, PixelKey, PixelTarget, TileEdit};
    use layer_model::Layer;

    // Card 037 tests: the bounded visible-content pick.

    /// A document with one ink patch on a raster layer at (24,24)..(40,40).
    fn doc_with_ink() -> (OpenDocument, layer_model::LayerId, MemoryTileSource) {
        let mut doc = OpenDocument::blank(DocumentId(7001), 96, 96, "pick", 8).unwrap();
        let layer = Layer::raster("Ink");
        let id = layer.id;
        doc.apply(Command::create_layer(layer)).unwrap();
        let mut bytes = vec![0u8; (raster::TILE_SIZE * raster::TILE_SIZE * 4) as usize];
        for y in 24..40usize {
            for x in 24..40usize {
                let i = (y * raster::TILE_SIZE as usize + x) * 4;
                bytes[i..i + 4].copy_from_slice(&[10, 60, 10, 255]);
            }
        }
        let mut source = MemoryTileSource::new();
        let hash = source.insert_bytes(bytes);
        doc.apply(
            Command::paint_tiles(
                PixelTarget::Layer(id),
                vec![TileEdit::set(raster::TileCoord::new(0, 0, 0), hash)],
            )
            .unwrap(),
        )
        .unwrap();
        (doc, id, source)
    }

    #[test]
    fn the_pick_answers_inside_the_ink_and_none_outside_it() {
        let (doc, id, source) = doc_with_ink();
        let hit = visible_content_at(
            &doc.document,
            &doc.document.pixels,
            &source,
            Vec2::new(30.0, 30.0),
            0.5,
        )
        .expect("the ink is picked");
        assert_eq!(hit.layer, id);
        assert!(!hit.is_group, "leaf layers pick as layers");
        assert!(
            visible_content_at(
                &doc.document,
                &doc.document.pixels,
                &source,
                Vec2::new(60.0, 60.0),
                0.5
            )
            .is_none(),
            "a point outside every layer's bounds is no pick"
        );
    }

    /// W4-F: the Move tool's auto-select on a 16-bit layer. The same ink,
    /// stored as an RGBA16 tile, picks where the ink is; read at an RGBA8
    /// stride, (30,30) sampled pixel (15,15)'s bytes — transparent — and the
    /// pick missed.
    #[test]
    fn the_pick_reads_an_rgba16_tile_at_sixteen_bits() {
        let (mut doc, id, mut source) = doc_with_ink();
        let coord = raster::TileCoord::new(0, 0, 0);
        let hash8 = doc.document.layer_tiles(id).unwrap().get(coord).unwrap();
        let deep = raster::widen_rgba8_tile(source.tile(hash8).unwrap()).unwrap();
        let hash16 = source.insert_bytes(deep);
        doc.apply(
            Command::paint_tiles(PixelTarget::Layer(id), vec![TileEdit::set(coord, hash16)])
                .unwrap(),
        )
        .unwrap();
        let hit = visible_content_at(
            &doc.document,
            &doc.document.pixels,
            &source,
            Vec2::new(30.0, 30.0),
            0.5,
        )
        .expect("the 16-bit ink is picked");
        assert_eq!(hit.layer, id);
        assert!(visible_content_at(
            &doc.document,
            &doc.document.pixels,
            &source,
            Vec2::new(60.0, 60.0),
            0.5
        )
        .is_none());
    }

    #[test]
    fn an_invisible_ancestor_hides_its_children_from_the_pick() {
        let (mut doc, id, source) = doc_with_ink();
        let group = Layer::group("Pack");
        let group_id = group.id;
        doc.apply(Command::create_layer(group)).unwrap();
        doc.apply(Command::MoveLayer {
            layer_id: id,
            parent: Some(group_id),
            index: 0,
        })
        .unwrap();
        let hit = visible_content_at(
            &doc.document,
            &doc.document.pixels,
            &source,
            Vec2::new(30.0, 30.0),
            0.5,
        );
        assert!(hit.is_some(), "a visible group passes its child through");

        doc.document.layers.get_mut(group_id).unwrap().visible = false;
        assert!(
            visible_content_at(
                &doc.document,
                &doc.document.pixels,
                &source,
                Vec2::new(30.0, 30.0),
                0.5
            )
            .is_none(),
            "an invisible ancestor hides the child"
        );
    }

    #[test]
    fn adjustment_layers_are_never_object_picked() {
        let (mut doc, id, source) = doc_with_ink();
        // An adjustment layer ABOVE the ink layer.
        let adj = Layer::with_kind(
            "Levels",
            layer_model::LayerKind::Adjustment(layer_model::AdjustmentLayer {
                kind: layer_model::AdjustmentKind::Levels {
                    black: 0.0,
                    white: 1.0,
                    gamma: 1.0,
                },
            }),
        );
        doc.apply(Command::create_layer(adj)).unwrap();
        let hit = visible_content_at(
            &doc.document,
            &doc.document.pixels,
            &source,
            Vec2::new(30.0, 30.0),
            0.5,
        )
        .expect("something is picked");
        assert_eq!(hit.layer, id, "the adjustment layer never shadows the pick");
    }

    #[test]
    fn a_semitransparent_layer_below_the_threshold_is_transparent_to_the_pick() {
        let (mut doc, id, source) = doc_with_ink();
        doc.document.layers.get_mut(id).unwrap().opacity = 0.25;
        let hit = visible_content_at(
            &doc.document,
            &doc.document.pixels,
            &source,
            Vec2::new(30.0, 30.0),
            0.5,
        );
        assert!(hit.is_none(), "0.25 opacity does not clear a 0.5 threshold");
        doc.document.layers.get_mut(id).unwrap().opacity = 0.75;
        assert!(
            visible_content_at(
                &doc.document,
                &doc.document.pixels,
                &source,
                Vec2::new(30.0, 30.0),
                0.5
            )
            .is_some(),
            "0.75 opacity clears the threshold"
        );
    }

    #[test]
    fn a_mask_hiding_the_point_hides_the_layer_from_the_pick() {
        let (mut doc, id, mut source) = doc_with_ink();
        // A mask that covers the whole canvas except the ink's left half: the
        // point (28,30) is masked out, (36,30) stays visible.
        let mut coverage = vec![0u8; (raster::TILE_SIZE * raster::TILE_SIZE) as usize];
        for y in 0..raster::TILE_SIZE as usize {
            for x in 32..raster::TILE_SIZE as usize {
                coverage[y * raster::TILE_SIZE as usize + x] = 255;
            }
        }
        let mask_hash = source.insert_bytes(coverage);
        let mask_id = layer_model::MaskId::new();
        doc.document.layers.get_mut(id).unwrap().mask = Some(layer_model::LayerMask::new(mask_id));
        doc.apply(
            Command::paint_tiles(
                // The mask target names the LAYER; the store keys by the
                // layer's mask id (stable across renames).
                PixelTarget::Mask(id),
                vec![TileEdit::set(raster::TileCoord::new(0, 0, 0), mask_hash)],
            )
            .unwrap(),
        )
        .unwrap();
        assert!(
            visible_content_at(
                &doc.document,
                &doc.document.pixels,
                &source,
                Vec2::new(28.0, 30.0),
                0.5
            )
            .is_none(),
            "the masked-out half does not pick"
        );
        assert!(
            visible_content_at(
                &doc.document,
                &doc.document.pixels,
                &source,
                Vec2::new(36.0, 30.0),
                0.5
            )
            .is_some(),
            "the revealed half picks"
        );
        // Silence the unused-key warning: the mask's key is read inside the pick.
        let _ = PixelKey::Mask(mask_id);
    }

    #[test]
    fn a_text_layer_is_picked_through_its_shaped_ink() {
        let mut doc = OpenDocument::blank(DocumentId(7002), 96, 96, "type", 8).unwrap();
        let layer = Layer::with_kind(
            "Headline",
            layer_model::LayerKind::Text(layer_model::TextLayer {
                text: "THUMBNAILS".to_string(),
                font_family: "DejaVu Sans".to_string(),
                size_px: 24.0,
                ..layer_model::TextLayer::default()
            }),
        );
        let id = layer.id;
        doc.apply(Command::create_layer(layer)).unwrap();
        // The shaped run sits at the layer's origin; a point inside its ink box
        // picks, a point far below does not.
        let hit = visible_content_at(
            &doc.document,
            &doc.document.pixels,
            &source_of(&doc),
            Vec2::new(8.0, 12.0),
            0.5,
        );
        assert_eq!(hit.expect("the headline picks").layer, id);
        assert!(
            visible_content_at(
                &doc.document,
                &doc.document.pixels,
                &source_of(&doc),
                Vec2::new(8.0, 80.0),
                0.5
            )
            .is_none(),
            "a point outside the shaped ink does not pick"
        );
    }

    fn source_of(doc: &OpenDocument) -> MemoryTileSource {
        doc.tiles.clone()
    }
}
