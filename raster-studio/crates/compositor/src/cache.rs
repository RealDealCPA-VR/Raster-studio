//! Cached, tile-parallel compositing with dirty-tile invalidation.
//!
//! # What makes the cache safe
//!
//! Every cached tile is keyed by a hash of *the inputs that produced it* — the
//! document's geometry and colour space, the compositing options, every layer
//! property that reaches the maths, and the content hash of each layer and mask
//! tile the traversal reads for that coordinate. A cached tile is reused only
//! when that key still matches, so a stale tile is a contradiction rather than
//! a race: if anything it depended on changed, the key changed.
//!
//! The explicit `invalidate_*` methods are therefore an **optimisation**, not
//! the correctness mechanism. They let a caller drop entries it knows are dead
//! so the key never has to be recomputed for them; forgetting to call one
//! costs a key computation, not a wrong pixel.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use rayon::prelude::*;

use editor_core::Document;
use layer_model::{LayerId, LayerKind};
use raster::{PixelRect, TileCoord, TILE_SIZE};

use crate::canvas::Canvas;
use crate::composite::{tile_rect, Ctx};
use crate::error::CompositeError;
use crate::source::TileSource;
use crate::CompositeOptions;

/// Default number of composited tiles held. At `TILE_SIZE` 256 and RGBA `f32`
/// one tile is 1 MiB, so this is a 128 MiB ceiling.
pub const DEFAULT_CACHE_TILES: usize = 128;

/// Hit/miss counters, so a caller (or a test) can prove that a recomposite
/// after a small edit only recomputed the tiles that changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
}

struct Entry {
    key: u64,
    canvas: Arc<Canvas>,
}

/// A compositor that remembers the tiles it has already produced.
///
/// Use this rather than [`crate::composite_region`] whenever the same document
/// is composited repeatedly — which is every interactive frame.
pub struct TileCompositor {
    cache: HashMap<TileCoord, Entry>,
    capacity: usize,
    stats: CacheStats,
}

impl Default for TileCompositor {
    fn default() -> Self {
        Self::new()
    }
}

impl TileCompositor {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_CACHE_TILES)
    }

    /// A compositor holding at most `capacity` composited tiles.
    ///
    /// When the cache grows past `capacity` it is trimmed to the tiles of the
    /// most recent request — a working-set policy, not an LRU. That is
    /// deliberate: the access pattern here is "the visible viewport", which is
    /// exactly a working set, and an LRU's per-entry bookkeeping would cost
    /// more than the hits it saved.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            cache: HashMap::new(),
            capacity: capacity.max(1),
            stats: CacheStats::default(),
        }
    }

    /// Composite `region` at `level`, reusing every cached tile whose inputs
    /// are unchanged and computing the rest in parallel.
    pub fn composite_region<S: TileSource + ?Sized>(
        &mut self,
        doc: &Document,
        source: &S,
        region: PixelRect,
        level: u8,
        opts: CompositeOptions,
    ) -> Result<Canvas, CompositeError> {
        let ctx = Ctx::new(doc, source, level, opts)?;
        let coords = ctx.tiles_covering(region);
        let produced = self.produce(&ctx, &coords)?;
        let mut out = Canvas::transparent(region)?;
        for (_, canvas) in &produced {
            out.blit_from(canvas);
        }
        self.store(&coords, produced);
        Ok(out)
    }

    /// Composite a single tile, returning the shared buffer the cache holds.
    pub fn composite_tile<S: TileSource + ?Sized>(
        &mut self,
        doc: &Document,
        source: &S,
        coord: TileCoord,
        opts: CompositeOptions,
    ) -> Result<Arc<Canvas>, CompositeError> {
        let ctx = Ctx::new(doc, source, coord.level, opts)?;
        let produced = self.produce(&ctx, &[coord])?;
        let canvas = produced
            .first()
            .map(|(_, c)| Arc::clone(c))
            .expect("one coordinate in, one tile out");
        self.store(&[coord], produced);
        Ok(canvas)
    }

    /// Compute (or reuse) every requested tile. Read-only against the cache so
    /// the whole set can run on the rayon pool.
    fn produce<S: TileSource + ?Sized>(
        &mut self,
        ctx: &Ctx<'_, S>,
        coords: &[TileCoord],
    ) -> Result<Vec<(u64, Arc<Canvas>)>, CompositeError> {
        let cache = &self.cache;
        let results: Vec<(bool, u64, Arc<Canvas>)> = coords
            .par_iter()
            .map(|&coord| {
                let key = ctx.tile_input_key(coord);
                if let Some(entry) = cache.get(&coord) {
                    if entry.key == key {
                        return Ok((true, key, Arc::clone(&entry.canvas)));
                    }
                }
                let canvas = ctx.composite_root(tile_rect(coord))?;
                Ok((false, key, Arc::new(canvas)))
            })
            .collect::<Result<Vec<_>, CompositeError>>()?;

        let mut out = Vec::with_capacity(results.len());
        for (hit, key, canvas) in results {
            if hit {
                self.stats.hits += 1;
            } else {
                self.stats.misses += 1;
            }
            out.push((key, canvas));
        }
        Ok(out)
    }

    fn store(&mut self, coords: &[TileCoord], produced: Vec<(u64, Arc<Canvas>)>) {
        for (coord, (key, canvas)) in coords.iter().zip(produced) {
            self.cache.insert(*coord, Entry { key, canvas });
        }
        if self.cache.len() > self.capacity {
            let keep: HashSet<TileCoord> = coords.iter().copied().collect();
            self.cache.retain(|c, _| keep.contains(c));
        }
    }

    /// Drop one cached tile. Returns whether anything was cached there.
    pub fn invalidate_tile(&mut self, coord: TileCoord) -> bool {
        self.cache.remove(&coord).is_some()
    }

    /// Drop every cached tile overlapping `rect` at `level`. Returns how many
    /// were dropped.
    pub fn invalidate_rect(&mut self, rect: PixelRect, level: u8) -> usize {
        if rect.is_empty() {
            return 0;
        }
        let t = TILE_SIZE as i64;
        let x0 = rect.x.div_euclid(t);
        let x1 = (rect.right() - 1).div_euclid(t);
        let y0 = rect.y.div_euclid(t);
        let y1 = (rect.bottom() - 1).div_euclid(t);
        let before = self.cache.len();
        self.cache.retain(|c, _| {
            c.level != level
                || (c.x as i64) < x0
                || (c.x as i64) > x1
                || (c.y as i64) < y0
                || (c.y as i64) > y1
        });
        before - self.cache.len()
    }

    /// Mark every tile a layer (and its descendants) can affect as dirty.
    ///
    /// Precise when every layer in the subtree sits at the identity transform
    /// and paints only where it stores pixels: exactly the tiles its stored
    /// layer and mask tiles touch are dropped, grown by any mask feather.
    ///
    /// Three things make that bound impossible, and each falls back to dropping
    /// **everything**:
    ///
    /// * a non-identity transform anywhere in the subtree — a transformed
    ///   layer's pixels can land on any tile at all;
    /// * an **adjustment** layer anywhere in the subtree — it stores no tiles
    ///   of its own and rewrites the backdrop beneath it, which is every tile
    ///   its scope covers;
    /// * an id the document does not hold.
    ///
    /// Returns how many cached tiles were dropped.
    pub fn invalidate_layer(&mut self, doc: &Document, id: LayerId) -> usize {
        let Some(dirty) = dirty_tiles(doc, id) else {
            let n = self.cache.len();
            self.invalidate_all();
            return n;
        };
        let before = self.cache.len();
        self.cache.retain(|c, _| !dirty.contains(c));
        before - self.cache.len()
    }

    /// Drop every cached tile.
    pub fn invalidate_all(&mut self) {
        self.cache.clear();
    }

    /// Number of composited tiles currently held.
    pub fn len(&self) -> usize {
        self.cache.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cache.is_empty()
    }

    pub fn stats(&self) -> CacheStats {
        self.stats
    }

    pub fn reset_stats(&mut self) {
        self.stats = CacheStats::default();
    }
}

/// Tiles a layer subtree can influence, or `None` when that cannot be bounded.
fn dirty_tiles(doc: &Document, id: LayerId) -> Option<HashSet<TileCoord>> {
    if !doc.layers.contains(id) {
        return None;
    }
    let mut out = HashSet::new();
    for lid in doc.layers.subtree_ids(id) {
        let layer = doc.layers.get(lid)?;
        if layer.transform != glam::Affine2::IDENTITY {
            return None;
        }
        if matches!(layer.kind, LayerKind::Adjustment(_)) {
            // An adjustment has no tiles of its own; what it changes is
            // whatever lies beneath it, anywhere its scope reaches.
            return None;
        }
        if matches!(layer.kind, LayerKind::Raster(_) | LayerKind::Generator(_)) {
            if let Some(map) = doc.layer_tiles(lid) {
                out.extend(map.iter().map(|(c, _)| c));
            }
        }
        if let Some(mask) = layer.mask.as_ref() {
            if let Some(map) = doc.pixels.tiles(editor_core::PixelKey::Mask(mask.id)) {
                // A feather reaches past the tile it is stored in.
                let reach = (mask.feather_px() / TILE_SIZE as f32).ceil() as i64 + 1;
                for (c, _) in map.iter() {
                    for dy in -reach..=reach {
                        for dx in -reach..=reach {
                            let (Ok(x), Ok(y)) = (
                                i32::try_from(c.x as i64 + dx),
                                i32::try_from(c.y as i64 + dy),
                            ) else {
                                continue;
                            };
                            out.insert(TileCoord::new(x, y, c.level));
                        }
                    }
                }
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::MemoryTileSource;
    use crate::testkit::{solid_layer, TestDoc};
    use layer_model::Layer;
    use raster::TileHash;

    #[test]
    fn a_repeated_composite_is_all_hits() {
        let mut t = TestDoc::new(512, 256);
        solid_layer(&mut t, "Red", [255, 0, 0, 255]);
        let (doc, src) = t.finish();

        let mut tc = TileCompositor::new();
        let region = PixelRect::new(0, 0, 512, 256);
        let first = tc
            .composite_region(&doc, &src, region, 0, CompositeOptions::default())
            .unwrap();
        assert_eq!(tc.stats(), CacheStats { hits: 0, misses: 2 });
        assert_eq!(tc.len(), 2);

        let second = tc
            .composite_region(&doc, &src, region, 0, CompositeOptions::default())
            .unwrap();
        assert_eq!(tc.stats(), CacheStats { hits: 2, misses: 2 });
        assert_eq!(first, second);
    }

    #[test]
    fn changing_a_layer_property_invalidates_by_key_alone() {
        // No `invalidate_*` call at all: the key notices.
        let mut t = TestDoc::new(256, 256);
        let id = solid_layer(&mut t, "Red", [255, 0, 0, 255]);
        let (mut doc, src) = t.finish();
        let mut tc = TileCompositor::new();
        let region = PixelRect::new(0, 0, 256, 256);
        let opaque = tc
            .composite_region(&doc, &src, region, 0, CompositeOptions::default())
            .unwrap();

        doc.layers.get_mut(id).unwrap().opacity = 0.5;
        tc.reset_stats();
        let faded = tc
            .composite_region(&doc, &src, region, 0, CompositeOptions::default())
            .unwrap();
        assert_eq!(tc.stats(), CacheStats { hits: 0, misses: 1 });
        assert_ne!(opaque, faded);
        assert!((faded.pixels()[0][3] - 0.5).abs() < 1e-6);
    }

    /// Composite `region` with a compositor that has never seen this document,
    /// so the answer owes nothing to any cache.
    fn cold(t: &TestDoc, region: PixelRect) -> Canvas {
        TileCompositor::new()
            .composite_region(&t.doc, &t.src, region, 0, CompositeOptions::default())
            .unwrap()
    }

    #[test]
    fn repainting_a_layer_tile_invalidates_by_key_alone() {
        // The other half of the key: not a layer *property* but the stored
        // bytes. Same layer id, same coordinate, new content hash, and no
        // `invalidate_*` call anywhere — the key is the only thing that can
        // notice, which is the whole promise the cache is built on.
        let mut t = TestDoc::linear(256, 256);
        let id = t.push_raster("Paint");
        t.paint_tile(id, TileCoord::new(0, 0, 0), [255, 0, 0, 255]);

        let region = PixelRect::new(0, 0, 256, 256);
        let mut tc = TileCompositor::new();
        let before = tc
            .composite_region(&t.doc, &t.src, region, 0, CompositeOptions::default())
            .unwrap();
        assert_eq!(before.pixels()[0], [1.0, 0.0, 0.0, 1.0]);

        t.paint_tile(id, TileCoord::new(0, 0, 0), [0, 0, 255, 255]);
        tc.reset_stats();
        let after = tc
            .composite_region(&t.doc, &t.src, region, 0, CompositeOptions::default())
            .unwrap();

        assert_eq!(tc.stats(), CacheStats { hits: 0, misses: 1 });
        assert_ne!(after, before, "the repaint must be visible");
        assert_eq!(after, cold(&t, region), "the cache served a stale tile");
        assert_eq!(after.pixels()[0], [0.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn repainting_a_mask_tile_invalidates_by_key_alone() {
        // Same again for `PixelKey::Mask`: one MaskId, one coordinate, only the
        // bytes behind it change.
        let mut t = TestDoc::linear(256, 256);
        let id = solid_layer(&mut t, "White", [255, 255, 255, 255]);
        let mask = t.attach_mask(id);
        t.paint_mask_tile(mask, TileCoord::new(0, 0, 0), 255);

        let region = PixelRect::new(0, 0, 256, 256);
        let mut tc = TileCompositor::new();
        let before = tc
            .composite_region(&t.doc, &t.src, region, 0, CompositeOptions::default())
            .unwrap();
        assert_eq!(before.pixels()[0], [1.0, 1.0, 1.0, 1.0]);

        t.paint_mask_tile(mask, TileCoord::new(0, 0, 0), 64);
        tc.reset_stats();
        let after = tc
            .composite_region(&t.doc, &t.src, region, 0, CompositeOptions::default())
            .unwrap();

        assert_eq!(tc.stats(), CacheStats { hits: 0, misses: 1 });
        assert_ne!(after, before, "the repaint must be visible");
        assert_eq!(after, cold(&t, region), "the cache served a stale tile");
        let k = 64.0 / 255.0;
        assert!(
            (after.pixels()[0][3] - k).abs() < 1e-6,
            "{:?}",
            after.pixels()[0]
        );
    }

    #[test]
    fn a_rename_does_not_evict_anything() {
        // Names never reach the maths, so a rename must not cost a recomposite.
        let mut t = TestDoc::new(256, 256);
        let id = solid_layer(&mut t, "Red", [255, 0, 0, 255]);
        let (mut doc, src) = t.finish();
        let mut tc = TileCompositor::new();
        let region = PixelRect::new(0, 0, 256, 256);
        tc.composite_region(&doc, &src, region, 0, CompositeOptions::default())
            .unwrap();

        doc.layers.get_mut(id).unwrap().name = "Crimson".into();
        tc.reset_stats();
        tc.composite_region(&doc, &src, region, 0, CompositeOptions::default())
            .unwrap();
        assert_eq!(tc.stats(), CacheStats { hits: 1, misses: 0 });
    }

    #[test]
    fn invalidating_a_layer_drops_only_the_tiles_it_covers() {
        let mut t = TestDoc::new(1024, 256);
        // A layer painted into tile (0,0) only.
        let id = t.push_raster("Patch");
        t.paint_tile(id, TileCoord::new(0, 0, 0), [0, 0, 255, 255]);
        let (doc, src) = t.finish();

        let mut tc = TileCompositor::new();
        let region = PixelRect::new(0, 0, 1024, 256);
        tc.composite_region(&doc, &src, region, 0, CompositeOptions::default())
            .unwrap();
        assert_eq!(tc.len(), 4);

        assert_eq!(tc.invalidate_layer(&doc, id), 1);
        assert_eq!(tc.len(), 3);
    }

    #[test]
    fn invalidating_a_transformed_layer_drops_everything() {
        let mut t = TestDoc::new(1024, 256);
        let id = t.push_raster("Moved");
        t.paint_tile(id, TileCoord::new(0, 0, 0), [0, 0, 255, 255]);
        let (mut doc, src) = t.finish();
        doc.layers.get_mut(id).unwrap().transform =
            glam::Affine2::from_translation(glam::Vec2::new(600.0, 0.0));

        let mut tc = TileCompositor::new();
        tc.composite_region(
            &doc,
            &src,
            PixelRect::new(0, 0, 1024, 256),
            0,
            CompositeOptions::default(),
        )
        .unwrap();
        assert_eq!(tc.len(), 4);
        assert_eq!(tc.invalidate_layer(&doc, id), 4);
        assert!(tc.is_empty());
    }

    #[test]
    fn invalidating_an_adjustment_layer_drops_everything_it_could_reach() {
        // An adjustment stores no tiles of its own, so bounding it by "the
        // tiles it holds" would drop nothing at all while it changes every
        // tile of the backdrop beneath it. The unbounded case has to take the
        // conservative path.
        let mut t = TestDoc::new(1024, 256);
        solid_layer(&mut t, "Grey", [128, 128, 128, 255]);
        let adj = t.push_adjustment(
            "Exposure",
            layer_model::AdjustmentKind::Exposure { stops: 1.0 },
        );
        let (doc, src) = t.finish();

        let mut tc = TileCompositor::new();
        tc.composite_region(
            &doc,
            &src,
            PixelRect::new(0, 0, 1024, 256),
            0,
            CompositeOptions::default(),
        )
        .unwrap();
        assert_eq!(tc.len(), 4);
        assert_eq!(tc.invalidate_layer(&doc, adj), 4);
        assert!(tc.is_empty());
    }

    #[test]
    fn invalidating_a_group_holding_an_adjustment_drops_everything_too() {
        // The same reach, one level down: the group's own tiles bound nothing
        // when a descendant's influence is unbounded.
        let mut t = TestDoc::new(1024, 256);
        solid_layer(&mut t, "Grey", [128, 128, 128, 255]);
        let group = t.push_group("Group");
        t.push_child(
            group,
            Layer::with_kind(
                "Exposure",
                LayerKind::Adjustment(layer_model::AdjustmentLayer {
                    kind: layer_model::AdjustmentKind::Exposure { stops: 1.0 },
                }),
            ),
        );
        t.set_group_blending(group, layer_model::GroupBlending::PassThrough);
        let (doc, src) = t.finish();

        let mut tc = TileCompositor::new();
        tc.composite_region(
            &doc,
            &src,
            PixelRect::new(0, 0, 1024, 256),
            0,
            CompositeOptions::default(),
        )
        .unwrap();
        assert_eq!(tc.len(), 4);
        assert_eq!(tc.invalidate_layer(&doc, group), 4);
        assert!(tc.is_empty());
    }

    #[test]
    fn invalidating_an_unknown_layer_is_conservative() {
        let mut t = TestDoc::new(256, 256);
        solid_layer(&mut t, "Red", [255, 0, 0, 255]);
        let (doc, src) = t.finish();
        let mut tc = TileCompositor::new();
        tc.composite_region(
            &doc,
            &src,
            PixelRect::new(0, 0, 256, 256),
            0,
            CompositeOptions::default(),
        )
        .unwrap();
        assert_eq!(tc.invalidate_layer(&doc, LayerId::new()), 1);
        assert!(tc.is_empty());
    }

    #[test]
    fn invalidate_rect_and_tile_target_the_right_entries() {
        let mut t = TestDoc::new(1024, 256);
        solid_layer(&mut t, "Red", [255, 0, 0, 255]);
        let (doc, src) = t.finish();
        let mut tc = TileCompositor::new();
        tc.composite_region(
            &doc,
            &src,
            PixelRect::new(0, 0, 1024, 256),
            0,
            CompositeOptions::default(),
        )
        .unwrap();
        assert_eq!(tc.len(), 4);

        assert!(tc.invalidate_tile(TileCoord::new(0, 0, 0)));
        assert!(!tc.invalidate_tile(TileCoord::new(0, 0, 0)));
        assert_eq!(tc.len(), 3);

        // A rect inside tiles 2 and 3 only.
        assert_eq!(tc.invalidate_rect(PixelRect::new(600, 0, 400, 10), 0), 2);
        assert_eq!(tc.len(), 1);
        // Wrong level: nothing matches.
        assert_eq!(tc.invalidate_rect(PixelRect::new(0, 0, 1024, 256), 1), 0);
    }

    #[test]
    fn capacity_trims_down_to_the_current_working_set() {
        let mut t = TestDoc::new(1024, 256);
        solid_layer(&mut t, "Red", [255, 0, 0, 255]);
        let (doc, src) = t.finish();
        let mut tc = TileCompositor::with_capacity(2);
        tc.composite_region(
            &doc,
            &src,
            PixelRect::new(0, 0, 1024, 256),
            0,
            CompositeOptions::default(),
        )
        .unwrap();
        // Four tiles were produced but only the request's own set is kept, and
        // the request *is* the working set, so all four survive this round.
        assert_eq!(tc.len(), 4);

        // A smaller follow-up request trims to itself.
        tc.composite_region(
            &doc,
            &src,
            PixelRect::new(0, 0, 10, 10),
            0,
            CompositeOptions::default(),
        )
        .unwrap();
        assert_eq!(tc.len(), 1);
    }

    #[test]
    fn a_missing_tile_in_the_source_is_transparent_not_an_error() {
        let mut t = TestDoc::new(256, 256);
        let id = t.push_raster("Ghost");
        // Reference a hash the source never learned about.
        t.set_tile_hash(id, TileCoord::new(0, 0, 0), TileHash([3; 32]));
        let (doc, _) = t.finish();
        let empty = MemoryTileSource::new();

        let mut tc = TileCompositor::new();
        let out = tc
            .composite_region(
                &doc,
                &empty,
                PixelRect::new(0, 0, 256, 256),
                0,
                CompositeOptions::default(),
            )
            .unwrap();
        assert!(out.pixels().iter().all(|p| *p == [0.0; 4]));
    }
}
