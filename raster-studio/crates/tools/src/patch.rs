//! Tile-aligned scratch surfaces: the bridge between "a gesture over a region"
//! and "one tile delta".
//!
//! Every pixel tool in this crate follows the same three steps:
//!
//! 1. **Load** the tiles covering the region it will touch into a working plane.
//! 2. **Edit** the plane — in linear, premultiplied light, always.
//! 3. **Commit** the plane back, once, producing a [`TileDelta`] that names
//!    only the tiles whose content actually changed.
//!
//! Step 3 is what makes "a stroke of N dabs is one undoable command" true: the
//! dabs accumulate in the plane and the plane is encoded exactly once, so the
//! number of dabs has no bearing on the number of commands or on how dark an
//! overlap gets.
//!
//! Two plane shapes, because there are two tile shapes.
//! [`ColorPatch`] holds `TILE_SIZE²` RGBA pixels per tile and is what a
//! [`PixelKey::Layer`] stores; [`CoveragePatch`] holds `TILE_SIZE²` 8-bit
//! samples per tile and is what a [`PixelKey::Mask`] stores. Mixing them up
//! would store a hash of four-byte pixels where the compositor expects
//! one-byte samples, which is exactly the mismatch
//! [`editor_core::CommandError::FillValueMismatch`] exists to prevent.

use color::{linear_to_srgb, premultiply, srgb8_to_linear, unpremultiply};
use editor_core::{PixelKey, TileDelta, TileEdit, MASK_TILE_BYTES};
use filters::FilterBuffer;
use glam::IVec2;
use raster::{PixelFormat, PixelRect, Tile, TileCoord, TileHash, TILE_SIZE};

use crate::error::ToolError;
use crate::tiles::TileAccess;

/// Largest number of tiles one tool operation may materialise at once.
///
/// 4096 tiles is a 16384×16384 pixel region — 1 GiB as `f32` RGBA, which is
/// already generous for a single gesture. The cap exists because a drag can
/// name an arbitrary rectangle and `vec![]` on an absurd one is a process
/// abort, not an error.
pub const MAX_PATCH_TILES: u64 = 4096;

/// The tile-aligned box that contains a pixel rect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TileBox {
    /// Left tile column.
    pub tx0: i32,
    /// Top tile row.
    pub ty0: i32,
    /// Tile columns.
    pub nx: u32,
    /// Tile rows.
    pub ny: u32,
}

impl TileBox {
    /// The smallest tile-aligned box containing `rect`.
    ///
    /// Refuses an empty rect ([`ToolError::Degenerate`]) and one that names
    /// more than [`MAX_PATCH_TILES`] tiles, before anything is allocated.
    pub fn covering(rect: PixelRect) -> Result<Self, ToolError> {
        if rect.is_empty() {
            return Err(ToolError::Degenerate);
        }
        let t = TILE_SIZE as i64;
        let x0 = rect.x.div_euclid(t);
        let x1 = (rect.right() - 1).div_euclid(t);
        let y0 = rect.y.div_euclid(t);
        let y1 = (rect.bottom() - 1).div_euclid(t);
        for v in [x0, x1, y0, y1] {
            if v < i32::MIN as i64 || v > i32::MAX as i64 {
                return Err(ToolError::RegionTooLarge {
                    tiles: u64::MAX,
                    max: MAX_PATCH_TILES,
                });
            }
        }
        let nx = (x1 - x0 + 1) as u64;
        let ny = (y1 - y0 + 1) as u64;
        let total = nx.saturating_mul(ny);
        if total > MAX_PATCH_TILES {
            return Err(ToolError::RegionTooLarge {
                tiles: total,
                max: MAX_PATCH_TILES,
            });
        }
        Ok(Self {
            tx0: x0 as i32,
            ty0: y0 as i32,
            nx: nx as u32,
            ny: ny as u32,
        })
    }

    /// Document-space pixel origin of the box.
    pub fn origin(self) -> IVec2 {
        IVec2::new(
            self.tx0.saturating_mul(TILE_SIZE as i32),
            self.ty0.saturating_mul(TILE_SIZE as i32),
        )
    }

    /// Width in pixels.
    pub fn width(self) -> u32 {
        self.nx * TILE_SIZE
    }

    /// Height in pixels.
    pub fn height(self) -> u32 {
        self.ny * TILE_SIZE
    }

    /// Total tiles.
    pub fn len(self) -> usize {
        (self.nx as usize) * (self.ny as usize)
    }

    pub fn is_empty(self) -> bool {
        self.len() == 0
    }

    /// The whole box as a pixel rect.
    pub fn rect(self) -> PixelRect {
        let o = self.origin();
        PixelRect::new(o.x as i64, o.y as i64, self.width(), self.height())
    }

    /// `(slot, coord)` for every tile, row-major.
    pub fn coords(self) -> impl Iterator<Item = (usize, TileCoord)> {
        let (tx0, ty0, nx, ny) = (self.tx0, self.ty0, self.nx, self.ny);
        (0..ny).flat_map(move |ty| {
            (0..nx).map(move |tx| {
                (
                    (ty * nx + tx) as usize,
                    TileCoord::new(tx0 + tx as i32, ty0 + ty as i32, 0),
                )
            })
        })
    }

    /// The slot a document-space point falls in, if it is inside the box.
    fn slot_of(self, p: IVec2) -> Option<usize> {
        let o = self.origin();
        let lx = p.x.checked_sub(o.x)?;
        let ly = p.y.checked_sub(o.y)?;
        if lx < 0 || ly < 0 || lx >= self.width() as i32 || ly >= self.height() as i32 {
            return None;
        }
        let tx = lx / TILE_SIZE as i32;
        let ty = ly / TILE_SIZE as i32;
        Some((ty as usize) * (self.nx as usize) + tx as usize)
    }
}

/// Decode one straight-alpha sRGB8 pixel into linear premultiplied light.
fn decode(px: [u8; 4]) -> [f32; 4] {
    premultiply([
        srgb8_to_linear(px[0]),
        srgb8_to_linear(px[1]),
        srgb8_to_linear(px[2]),
        px[3] as f32 / 255.0,
    ])
}

/// Encode linear premultiplied light back to straight-alpha sRGB8.
///
/// Colour goes through the transfer curve, alpha does not — alpha is a
/// coverage fraction and was never gamma encoded.
fn encode(px: [f32; 4]) -> [u8; 4] {
    let s = unpremultiply(px);
    let q = |v: f32| -> u8 {
        if !v.is_finite() {
            return 0;
        }
        (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
    };
    [
        q(linear_to_srgb(s[0].clamp(0.0, 1.0))),
        q(linear_to_srgb(s[1].clamp(0.0, 1.0))),
        q(linear_to_srgb(s[2].clamp(0.0, 1.0))),
        q(s[3]),
    ]
}

/// A tile-aligned plane of linear premultiplied RGBA covering a region of one
/// layer.
#[derive(Debug, Clone)]
pub struct ColorPatch {
    tb: TileBox,
    buf: FilterBuffer,
    dirty: Vec<bool>,
}

impl ColorPatch {
    /// Read every tile covering `rect` into a working plane.
    pub fn load(
        access: &dyn TileAccess,
        key: PixelKey,
        rect: PixelRect,
    ) -> Result<Self, ToolError> {
        let tb = TileBox::covering(rect)?;
        let mut buf = FilterBuffer::transparent(tb.width(), tb.height())?;
        let stride = tb.width() as usize;
        let ts = TILE_SIZE as usize;
        for (slot, coord) in tb.coords() {
            let Some(bytes) = access.tile_bytes(key, coord) else {
                continue;
            };
            if bytes.len() != Tile::byte_len(PixelFormat::Rgba8) {
                return Err(ToolError::Tile(raster::TileError::BadLength {
                    expected: Tile::byte_len(PixelFormat::Rgba8),
                    got: bytes.len(),
                }));
            }
            let tx = slot % tb.nx as usize;
            let ty = slot / tb.nx as usize;
            let px = buf.pixels_mut();
            for row in 0..ts {
                let dst = (ty * ts + row) * stride + tx * ts;
                let src = row * ts * 4;
                for i in 0..ts {
                    let b = &bytes[src + i * 4..src + i * 4 + 4];
                    px[dst + i] = decode([b[0], b[1], b[2], b[3]]);
                }
            }
        }
        let n = tb.len();
        Ok(Self {
            tb,
            buf,
            dirty: vec![false; n],
        })
    }

    pub fn tile_box(&self) -> TileBox {
        self.tb
    }

    /// The region this plane covers, in document pixels.
    pub fn rect(&self) -> PixelRect {
        self.tb.rect()
    }

    pub fn origin(&self) -> IVec2 {
        self.tb.origin()
    }

    pub fn width(&self) -> u32 {
        self.tb.width()
    }

    pub fn height(&self) -> u32 {
        self.tb.height()
    }

    pub fn contains(&self, p: IVec2) -> bool {
        self.tb.slot_of(p).is_some()
    }

    /// Row-major index of a document-space point, or `None` outside the plane.
    pub fn index_of(&self, p: IVec2) -> Option<usize> {
        let o = self.tb.origin();
        let lx = p.x.checked_sub(o.x)?;
        let ly = p.y.checked_sub(o.y)?;
        if lx < 0 || ly < 0 || lx >= self.width() as i32 || ly >= self.height() as i32 {
            return None;
        }
        Some(ly as usize * self.width() as usize + lx as usize)
    }

    fn index(&self, p: IVec2) -> Option<usize> {
        self.index_of(p)
    }

    /// One pixel, in linear premultiplied light. Outside the plane reads as
    /// fully transparent, which is the same thing an absent tile means.
    pub fn get(&self, p: IVec2) -> [f32; 4] {
        match self.index(p) {
            Some(i) => self.buf.pixels()[i],
            None => [0.0; 4],
        }
    }

    /// Write one pixel and mark its tile for re-encoding.
    pub fn set(&mut self, p: IVec2, px: [f32; 4]) {
        if let (Some(i), Some(slot)) = (self.index(p), self.tb.slot_of(p)) {
            self.buf.pixels_mut()[i] = px;
            self.dirty[slot] = true;
        }
    }

    pub fn buffer(&self) -> &FilterBuffer {
        &self.buf
    }

    /// Mutable access to the whole plane, for operations that rewrite it
    /// wholesale (a blur, a resample). Marks every tile dirty, because the
    /// caller could have touched any of them.
    pub fn buffer_mut(&mut self) -> &mut FilterBuffer {
        self.dirty.iter_mut().for_each(|d| *d = true);
        &mut self.buf
    }

    /// Replace the plane's pixels with `src`, which must be the same size.
    pub fn replace(&mut self, src: FilterBuffer) -> Result<(), ToolError> {
        if src.width() != self.width() || src.height() != self.height() {
            return Err(ToolError::Filter(filters::FilterError::BadLength {
                width: self.width(),
                height: self.height(),
                expected: (self.width() as usize) * (self.height() as usize),
                got: src.len(),
            }));
        }
        self.buf = src;
        self.dirty.iter_mut().for_each(|d| *d = true);
        Ok(())
    }

    /// Encode one tile back to straight-alpha sRGB8.
    fn encode_tile(&self, slot: usize) -> Vec<u8> {
        let ts = TILE_SIZE as usize;
        let stride = self.width() as usize;
        let tx = slot % self.tb.nx as usize;
        let ty = slot / self.tb.nx as usize;
        let mut out = vec![0u8; Tile::byte_len(PixelFormat::Rgba8)];
        let px = self.buf.pixels();
        for row in 0..ts {
            let src = (ty * ts + row) * stride + tx * ts;
            let dst = row * ts * 4;
            for i in 0..ts {
                out[dst + i * 4..dst + i * 4 + 4].copy_from_slice(&encode(px[src + i]));
            }
        }
        out
    }

    /// Encode the touched tiles and produce the delta that installs them.
    ///
    /// Only tiles that were written to are considered, and among those only the
    /// ones whose content hash actually changed produce an edit — so a stroke
    /// that grazes a tile without altering a pixel does not enlarge the undo
    /// entry, and a tile that ends up fully transparent is *removed* rather
    /// than stored as a transparent blob.
    pub fn commit(
        &self,
        access: &mut dyn TileAccess,
        key: PixelKey,
    ) -> Result<TileDelta, ToolError> {
        let mut edits = Vec::new();
        for (slot, coord) in self.tb.coords() {
            if !self.dirty[slot] {
                continue;
            }
            let bytes = self.encode_tile(slot);
            let after = if bytes.iter().all(|&b| b == 0) {
                None
            } else {
                Some(TileHash::of(&bytes))
            };
            if after == access.tile_hash(key, coord) {
                continue;
            }
            match after {
                Some(_) => {
                    let h = access.store(bytes);
                    edits.push(TileEdit::set(coord, h));
                }
                None => edits.push(TileEdit::clear(coord)),
            }
        }
        Ok(TileDelta::new(edits)?)
    }
}

/// A tile-aligned plane of 8-bit coverage covering a region of one mask.
#[derive(Debug, Clone)]
pub struct CoveragePatch {
    tb: TileBox,
    data: Vec<f32>,
    dirty: Vec<bool>,
}

impl CoveragePatch {
    /// Read every mask tile covering `rect` into a working plane.
    ///
    /// An absent mask tile reads as **zero coverage** — the layer hidden —
    /// which is the same value a present all-zero tile carries.
    pub fn load(
        access: &dyn TileAccess,
        key: PixelKey,
        rect: PixelRect,
    ) -> Result<Self, ToolError> {
        let tb = TileBox::covering(rect)?;
        let n = (tb.width() as usize) * (tb.height() as usize);
        let mut data = vec![0.0f32; n];
        let stride = tb.width() as usize;
        let ts = TILE_SIZE as usize;
        for (slot, coord) in tb.coords() {
            let Some(bytes) = access.tile_bytes(key, coord) else {
                continue;
            };
            if bytes.len() != MASK_TILE_BYTES {
                return Err(ToolError::Tile(raster::TileError::BadLength {
                    expected: MASK_TILE_BYTES,
                    got: bytes.len(),
                }));
            }
            let tx = slot % tb.nx as usize;
            let ty = slot / tb.nx as usize;
            for row in 0..ts {
                let dst = (ty * ts + row) * stride + tx * ts;
                for i in 0..ts {
                    data[dst + i] = bytes[row * ts + i] as f32 / 255.0;
                }
            }
        }
        let tiles = tb.len();
        Ok(Self {
            tb,
            data,
            dirty: vec![false; tiles],
        })
    }

    pub fn tile_box(&self) -> TileBox {
        self.tb
    }

    pub fn width(&self) -> u32 {
        self.tb.width()
    }

    pub fn height(&self) -> u32 {
        self.tb.height()
    }

    fn index(&self, p: IVec2) -> Option<usize> {
        let o = self.tb.origin();
        let lx = p.x.checked_sub(o.x)?;
        let ly = p.y.checked_sub(o.y)?;
        if lx < 0 || ly < 0 || lx >= self.width() as i32 || ly >= self.height() as i32 {
            return None;
        }
        Some(ly as usize * self.width() as usize + lx as usize)
    }

    /// Coverage at one point, `0.0..=1.0`. Outside the plane reads as zero.
    pub fn get(&self, p: IVec2) -> f32 {
        self.index(p).map(|i| self.data[i]).unwrap_or(0.0)
    }

    pub fn set(&mut self, p: IVec2, v: f32) {
        if let (Some(i), Some(slot)) = (self.index(p), self.tb.slot_of(p)) {
            self.data[i] = v.clamp(0.0, 1.0);
            self.dirty[slot] = true;
        }
    }

    fn encode_tile(&self, slot: usize) -> Vec<u8> {
        let ts = TILE_SIZE as usize;
        let stride = self.width() as usize;
        let tx = slot % self.tb.nx as usize;
        let ty = slot / self.tb.nx as usize;
        let mut out = vec![0u8; MASK_TILE_BYTES];
        for row in 0..ts {
            let src = (ty * ts + row) * stride + tx * ts;
            for i in 0..ts {
                out[row * ts + i] = (self.data[src + i].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            }
        }
        out
    }

    /// Encode the touched tiles.
    ///
    /// Unlike [`ColorPatch::commit`] an all-zero tile is stored explicitly
    /// rather than removed: for a mask, zero coverage is a *meaningful* value —
    /// the layer hidden — and removing the tile happens to mean the same thing,
    /// but storing it keeps the delta's intent legible and avoids the caller
    /// having to reason about which of the two it got.
    pub fn commit(
        &self,
        access: &mut dyn TileAccess,
        key: PixelKey,
    ) -> Result<TileDelta, ToolError> {
        let mut edits = Vec::new();
        for (slot, coord) in self.tb.coords() {
            if !self.dirty[slot] {
                continue;
            }
            let bytes = self.encode_tile(slot);
            let after = TileHash::of(&bytes);
            if Some(after) == access.tile_hash(key, coord) {
                continue;
            }
            let h = access.store(bytes);
            edits.push(TileEdit::set(coord, h));
        }
        Ok(TileDelta::new(edits)?)
    }
}

/// Read a rectangle of straight-alpha sRGB8 pixels out of a layer.
///
/// This is the shape [`selection::ImageView`] consumes, which is what the
/// magic wand, quick select, colour range and the magnetic lasso read. Pixels
/// outside any stored tile come back fully transparent.
pub fn read_rgba8(
    access: &dyn TileAccess,
    key: PixelKey,
    rect: PixelRect,
) -> Result<Vec<u8>, ToolError> {
    if rect.is_empty() {
        return Err(ToolError::Degenerate);
    }
    let area = (rect.width as u64) * (rect.height as u64);
    if area > MAX_PATCH_TILES * (TILE_SIZE as u64) * (TILE_SIZE as u64) {
        return Err(ToolError::RegionTooLarge {
            tiles: area / ((TILE_SIZE as u64) * (TILE_SIZE as u64)),
            max: MAX_PATCH_TILES,
        });
    }
    let mut out = vec![0u8; (area * 4) as usize];
    let t = TILE_SIZE as i64;
    let ts = TILE_SIZE as usize;
    for row in 0..rect.height as i64 {
        let y = rect.y + row;
        let ty = y.div_euclid(t);
        let ly = y.rem_euclid(t) as usize;
        for col in 0..rect.width as i64 {
            let x = rect.x + col;
            let tx = x.div_euclid(t);
            let coord = TileCoord::new(tx as i32, ty as i32, 0);
            let Some(bytes) = access.tile_bytes(key, coord) else {
                continue;
            };
            if bytes.len() != Tile::byte_len(PixelFormat::Rgba8) {
                continue;
            }
            let lx = x.rem_euclid(t) as usize;
            let si = (ly * ts + lx) * 4;
            let di = ((row * rect.width as i64 + col) * 4) as usize;
            out[di..di + 4].copy_from_slice(&bytes[si..si + 4]);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiles::MemoryTiles;
    use layer_model::LayerId;

    fn key() -> PixelKey {
        PixelKey::Layer(LayerId::new())
    }

    #[test]
    fn the_srgb8_round_trip_is_exact_for_every_opaque_byte() {
        // If it were not, committing a tile the tool merely *read* would
        // rewrite it and every stroke would dirty its whole neighbourhood.
        for v in 0..=255u8 {
            let px = decode([v, v, v, 255]);
            assert_eq!(encode(px), [v, v, v, 255], "byte {v} did not round-trip");
        }
    }

    #[test]
    fn a_patch_that_was_only_read_commits_nothing() {
        let mut tiles = MemoryTiles::new();
        let k = key();
        tiles.put_pixel(k, 10, 10, [200, 100, 50, 255]);
        let patch = ColorPatch::load(&tiles, k, PixelRect::new(0, 0, 64, 64)).unwrap();
        assert_eq!(
            patch.get(IVec2::new(10, 10))[3],
            1.0,
            "the stored pixel should have loaded"
        );
        let delta = patch.commit(&mut tiles, k).unwrap();
        assert!(delta.is_empty(), "a read-only patch must not emit edits");
    }

    #[test]
    fn writing_one_pixel_emits_exactly_one_tile_edit() {
        let mut tiles = MemoryTiles::new();
        let k = key();
        let mut patch = ColorPatch::load(&tiles, k, PixelRect::new(0, 0, 300, 300)).unwrap();
        assert_eq!(patch.tile_box().len(), 4, "300px spans two tiles each way");
        patch.set(IVec2::new(5, 5), decode([255, 0, 0, 255]));
        let delta = patch.commit(&mut tiles, k).unwrap();
        assert_eq!(delta.len(), 1);
        assert_eq!(delta.edits()[0].coord, TileCoord::new(0, 0, 0));
        assert!(delta.edits()[0].hash.is_some());
    }

    #[test]
    fn erasing_the_last_content_of_a_tile_removes_it_rather_than_storing_a_blank() {
        let mut tiles = MemoryTiles::new();
        let k = key();
        tiles.put_pixel(k, 1, 1, [255, 255, 255, 255]);
        let mut patch = ColorPatch::load(&tiles, k, PixelRect::new(0, 0, 16, 16)).unwrap();
        patch.set(IVec2::new(1, 1), [0.0; 4]);
        let delta = patch.commit(&mut tiles, k).unwrap();
        assert_eq!(delta.len(), 1);
        assert_eq!(delta.edits()[0].hash, None);
    }

    #[test]
    fn a_region_larger_than_the_cap_is_refused_before_allocating() {
        let err = TileBox::covering(PixelRect::new(0, 0, u32::MAX, u32::MAX)).unwrap_err();
        assert!(matches!(err, ToolError::RegionTooLarge { .. }));
        assert!(matches!(
            TileBox::covering(PixelRect::new(0, 0, 0, 10)),
            Err(ToolError::Degenerate)
        ));
    }

    #[test]
    fn a_coverage_patch_round_trips_a_mask_tile() {
        let mut tiles = MemoryTiles::new();
        let k = PixelKey::Mask(layer_model::MaskId::new());
        tiles.put(k, TileCoord::new(0, 0, 0), vec![128u8; MASK_TILE_BYTES]);
        let mut patch = CoveragePatch::load(&tiles, k, PixelRect::new(0, 0, 8, 8)).unwrap();
        assert!((patch.get(IVec2::new(4, 4)) - 128.0 / 255.0).abs() < 1e-6);
        // Untouched: nothing to commit.
        assert!(patch.commit(&mut tiles, k).unwrap().is_empty());
        patch.set(IVec2::new(4, 4), 1.0);
        let delta = patch.commit(&mut tiles, k).unwrap();
        assert_eq!(delta.len(), 1);
        assert!(
            delta.edits()[0].hash.is_some(),
            "a mask tile is stored, never removed"
        );
    }

    #[test]
    fn read_rgba8_stitches_tiles_and_leaves_gaps_transparent() {
        let mut tiles = MemoryTiles::new();
        let k = key();
        tiles.put_pixel(k, 255, 0, [10, 20, 30, 255]);
        tiles.put_pixel(k, 256, 0, [40, 50, 60, 255]);
        let px = read_rgba8(&tiles, k, PixelRect::new(254, 0, 4, 1)).unwrap();
        assert_eq!(&px[0..4], &[0, 0, 0, 0]);
        assert_eq!(&px[4..8], &[10, 20, 30, 255]);
        assert_eq!(&px[8..12], &[40, 50, 60, 255]);
        assert_eq!(&px[12..16], &[0, 0, 0, 0]);
    }
}
