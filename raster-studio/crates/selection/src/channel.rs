//! Save selection as channel, load channel as selection.
//!
//! A saved selection is an 8-bit grayscale channel, and coverage is already
//! 8-bit grayscale, so the conversion is a copy with a rectangle attached —
//! *no* thresholding in either direction. That is the whole point: a feathered
//! selection saved to a channel and loaded back is the same feathered
//! selection, not a hard-edged approximation of it.
//!
//! Two shapes are offered. The flat one is a packed rectangle for callers that
//! already hold a buffer. The tiled one speaks the editor's own mask-channel
//! format — [`raster::TILE_SIZE`] squares of [`editor_core::MASK_TILE_BYTES`]
//! coverage samples, exactly what [`editor_core::PixelKey::Mask`] tiles hold —
//! and it is sparse: a tile with no coverage is not emitted at all, so saving a
//! small selection on a large canvas writes a handful of tiles.

use editor_core::{SelectionMask, MASK_TILE_BYTES};
use glam::IVec2;
use raster::{TileCoord, TILE_SIZE};

use crate::buf::{alloc_bytes, checked_samples, try_push, CoverageBuf};
use crate::error::SelectionOpError;
use crate::rect::Rect;

/// One tile of a saved selection channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MaskTile {
    pub coord: TileCoord,
    /// [`MASK_TILE_BYTES`] coverage samples, row-major.
    pub coverage: Vec<u8>,
}

/// Copy a selection's coverage into a packed `rect.width() * rect.height()`
/// grayscale buffer.
pub fn selection_to_channel(mask: &SelectionMask, rect: Rect) -> Result<Vec<u8>, SelectionOpError> {
    let n = checked_samples(rect)?;
    let mut out = alloc_bytes(n, 0)?;
    let w = rect.width() as usize;
    for y in 0..rect.height() as usize {
        let dy = rect.min().y + y as i32;
        for x in 0..w {
            out[y * w + x] = mask.coverage_at(IVec2::new(rect.min().x + x as i32, dy));
        }
    }
    Ok(out)
}

/// Load a packed grayscale channel as a selection.
pub fn channel_to_selection(
    origin: IVec2,
    width: u32,
    height: u32,
    channel: &[u8],
) -> Result<SelectionMask, SelectionOpError> {
    let rect = Rect::from_xywh(origin.x, origin.y, width, height);
    if rect.width() != width || rect.height() != height {
        return Err(SelectionOpError::CoordOutOfRange {
            x: origin.x,
            y: origin.y,
            width,
            height,
            limit: crate::COORD_LIMIT,
        });
    }
    // Copy through `alloc_bytes`, not `to_vec`: a channel loaded from a file is
    // caller-sized, and doubling it must be an error rather than an abort.
    let mut data = alloc_bytes(channel.len(), 0)?;
    data.copy_from_slice(channel);
    CoverageBuf::from_parts(rect, data)?.into_mask()
}

/// Split a selection into sparse mask tiles for the pixel store.
///
/// Only tiles that carry coverage are emitted, and only tiles that intersect
/// the selection's own bounds are even visited, so the cost is proportional to
/// the selection rather than to the canvas.
pub fn selection_to_mask_tiles(mask: &SelectionMask) -> Result<Vec<MaskTile>, SelectionOpError> {
    let content = match mask.bounds() {
        Some((min, max)) => Rect::new(min, max),
        None => return Ok(Vec::new()),
    };
    let ts = TILE_SIZE as i32;
    let tx0 = content.min().x.div_euclid(ts);
    let ty0 = content.min().y.div_euclid(ts);
    let tx1 = (content.max().x - 1).div_euclid(ts);
    let ty1 = (content.max().y - 1).div_euclid(ts);

    let mut out = Vec::new();
    for ty in ty0..=ty1 {
        for tx in tx0..=tx1 {
            let coord = TileCoord::new(tx, ty, 0);
            let (ox, oy) = coord.pixel_origin();
            let mut data = alloc_bytes(MASK_TILE_BYTES, 0)?;
            let mut any = false;
            for y in 0..TILE_SIZE as i64 {
                for x in 0..TILE_SIZE as i64 {
                    let p = IVec2::new(
                        crate::rect::clamp_coord(ox + x),
                        crate::rect::clamp_coord(oy + y),
                    );
                    let v = mask.coverage_at(p);
                    if v != 0 {
                        any = true;
                        data[y as usize * TILE_SIZE as usize + x as usize] = v;
                    }
                }
            }
            if any {
                try_push(
                    &mut out,
                    MaskTile {
                        coord,
                        coverage: data,
                    },
                )?;
            }
        }
    }
    Ok(out)
}

/// Reassemble a selection from mask tiles.
pub fn mask_tiles_to_selection(tiles: &[MaskTile]) -> Result<SelectionMask, SelectionOpError> {
    if tiles.is_empty() {
        return Ok(SelectionMask::new(IVec2::ZERO, 0, 0, Vec::new())?);
    }
    for t in tiles {
        if t.coverage.len() != MASK_TILE_BYTES {
            return Err(SelectionOpError::ImageSizeMismatch {
                width: TILE_SIZE,
                height: TILE_SIZE,
                expected: MASK_TILE_BYTES,
                got: t.coverage.len(),
            });
        }
    }
    let mut rect = Rect::EMPTY;
    for t in tiles {
        let (ox, oy) = t.coord.pixel_origin();
        let min = IVec2::new(crate::rect::clamp_coord(ox), crate::rect::clamp_coord(oy));
        rect = rect.union(Rect::from_xywh(min.x, min.y, TILE_SIZE, TILE_SIZE));
    }
    let mut buf = CoverageBuf::zeroed(rect)?;
    for t in tiles {
        let (ox, oy) = t.coord.pixel_origin();
        for y in 0..TILE_SIZE as i64 {
            for x in 0..TILE_SIZE as i64 {
                let v = t.coverage[y as usize * TILE_SIZE as usize + x as usize];
                if v != 0 {
                    buf.set(
                        IVec2::new(
                            crate::rect::clamp_coord(ox + x),
                            crate::rect::clamp_coord(oy + y),
                        ),
                        v,
                    );
                }
            }
        }
    }
    buf.into_mask()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::marquee::ellipse;

    #[test]
    fn a_feathered_selection_survives_a_flat_channel_round_trip() {
        let src =
            crate::modify::feather(&ellipse(Rect::from_xywh(4, 6, 20, 14)).unwrap(), 3.0).unwrap();
        assert!(
            src.coverage().iter().any(|&v| v > 0 && v < 255),
            "the fixture must be soft for this to prove anything"
        );
        let rect = Rect::of_mask(&src);
        let rect = rect.unwrap();
        let channel = selection_to_channel(&src, rect).unwrap();
        let back = channel_to_selection(rect.min(), rect.width(), rect.height(), &channel).unwrap();
        assert_eq!(back, src, "no thresholding in either direction");
    }

    #[test]
    fn a_channel_larger_than_the_selection_pads_with_zero() {
        let src = crate::marquee::rectangle(Rect::from_xywh(2, 2, 2, 2)).unwrap();
        let ch = selection_to_channel(&src, Rect::from_xywh(0, 0, 6, 6)).unwrap();
        assert_eq!(ch.len(), 36);
        assert_eq!(ch[0], 0);
        assert_eq!(ch[2 * 6 + 2], 255);
        assert_eq!(ch[4 * 6 + 4], 0);
        // And loading it back trims to the covered pixels again.
        let back = channel_to_selection(IVec2::ZERO, 6, 6, &ch).unwrap();
        assert_eq!(back, src);
    }

    #[test]
    fn a_channel_whose_length_disagrees_with_its_extent_is_refused() {
        assert!(matches!(
            channel_to_selection(IVec2::ZERO, 4, 4, &[0; 15]),
            Err(SelectionOpError::ImageSizeMismatch {
                expected: 16,
                got: 15,
                ..
            })
        ));
    }

    #[test]
    fn tiling_a_selection_is_sparse_and_round_trips() {
        // A selection straddling a tile boundary, on a canvas far from origin.
        let base = TILE_SIZE as i32;
        let src = crate::modify::feather(
            &ellipse(Rect::from_xywh(base - 10, base - 10, 20, 20)).unwrap(),
            2.0,
        )
        .unwrap();
        let tiles = selection_to_mask_tiles(&src).unwrap();
        assert_eq!(tiles.len(), 4, "it straddles exactly four tiles");
        for t in &tiles {
            assert_eq!(t.coverage.len(), MASK_TILE_BYTES);
        }
        let back = mask_tiles_to_selection(&tiles).unwrap();
        assert_eq!(back, src);
    }

    #[test]
    fn tiles_with_no_coverage_are_not_emitted_at_all() {
        // Two small patches three tiles apart diagonally: the bounding range is
        // 3x3 tiles, but seven of them hold nothing and must not be written.
        let ts = TILE_SIZE as i32;
        let a = crate::marquee::rectangle(Rect::from_xywh(5, 5, 2, 2)).unwrap();
        let b = crate::marquee::rectangle(Rect::from_xywh(2 * ts + 5, 2 * ts + 5, 2, 2)).unwrap();
        let both = crate::boolean::combine(&a, &b, crate::boolean::BooleanOp::Add).unwrap();

        let tiles = selection_to_mask_tiles(&both).unwrap();
        assert_eq!(
            tiles.len(),
            2,
            "the empty tiles between the two patches were written out"
        );
        assert_eq!(mask_tiles_to_selection(&tiles).unwrap(), both);
    }

    #[test]
    fn a_small_selection_on_a_huge_canvas_emits_one_tile() {
        let far = 1 << 20;
        let src = crate::marquee::rectangle(Rect::from_xywh(far + 5, far + 5, 3, 3)).unwrap();
        let tiles = selection_to_mask_tiles(&src).unwrap();
        assert_eq!(tiles.len(), 1);
        assert_eq!(
            tiles[0].coord,
            TileCoord::new(far / TILE_SIZE as i32, far / TILE_SIZE as i32, 0)
        );
        assert_eq!(mask_tiles_to_selection(&tiles).unwrap(), src);
    }

    #[test]
    fn an_empty_selection_produces_and_survives_no_tiles() {
        let empty = SelectionMask::new(IVec2::new(9, 9), 0, 0, Vec::new()).unwrap();
        assert!(selection_to_mask_tiles(&empty).unwrap().is_empty());
        assert!(mask_tiles_to_selection(&[]).unwrap().is_empty());
        assert!(matches!(
            mask_tiles_to_selection(&[MaskTile {
                coord: TileCoord::new(0, 0, 0),
                coverage: vec![0; 4],
            }]),
            Err(SelectionOpError::ImageSizeMismatch { .. })
        ));
    }
}
