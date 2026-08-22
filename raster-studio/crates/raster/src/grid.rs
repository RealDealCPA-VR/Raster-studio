//! A bounded grid of fixed-size tiles covering one mip level of an image.
//!
//! # Partial edge tiles
//! A [`Tile`] is always a full `TILE_SIZE` square, but an image is rarely a
//! multiple of `TILE_SIZE`. Rather than pretend the padding is image content,
//! the grid records the image extent and reports, per tile, the sub-rect that
//! actually holds pixels ([`TileGrid::valid_rect`]). Flatten and build only
//! ever touch that sub-rect, so padding bytes never leak into an exported
//! image and never influence a round trip.
//!
//! # Sparsity
//! A coordinate inside the grid extent may have no tile. An absent tile reads
//! as fully transparent, which is what lets a layer occupy a large canvas
//! while storing only the tiles that were painted.

use std::collections::HashMap;

use crate::format::PixelFormat;
use crate::tile::{Tile, TileCoord, TileError, TILE_SIZE};

/// An axis-aligned rectangle in pixels.
///
/// The origin is signed so viewport rects that extend past the top-left of a
/// document are expressible; `width`/`height` are unsigned, so a rect is never
/// inside-out. A rect with a zero dimension is empty and overlaps nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixelRect {
    pub x: i64,
    pub y: i64,
    pub width: u32,
    pub height: u32,
}

impl PixelRect {
    pub const fn new(x: i64, y: i64, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// One past the last column covered by this rect.
    ///
    /// Saturates at `i64::MAX`: `x` is public and caller-supplied, so the sum
    /// is reachable input, and clamping keeps the reported edge past every
    /// addressable column instead of wrapping to a negative one.
    pub const fn right(&self) -> i64 {
        self.x.saturating_add(self.width as i64)
    }

    /// One past the last row covered by this rect. Saturates like
    /// [`PixelRect::right`].
    pub const fn bottom(&self) -> i64 {
        self.y.saturating_add(self.height as i64)
    }

    /// True when the rect covers no pixels at all.
    pub const fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// Failures when building or addressing a [`TileGrid`].
#[derive(Debug, thiserror::Error)]
pub enum GridError {
    #[error("image byte length mismatch: expected {expected}, got {got}")]
    BadImageLength { expected: usize, got: usize },
    /// `width * height * 4` does not fit in a `usize`, so the byte length can
    /// neither be computed nor allocated on this platform.
    #[error("a {width}x{height} RGBA8 image cannot be addressed on this platform")]
    ImageTooLarge { width: u32, height: u32 },
    #[error("tile coord ({x}, {y}) is outside a {tiles_x}x{tiles_y} tile grid")]
    CoordOutOfBounds {
        x: i32,
        y: i32,
        tiles_x: u32,
        tiles_y: u32,
    },
    #[error("tile coord is at mip level {got}, grid is at level {expected}")]
    LevelMismatch { expected: u8, got: u8 },
    #[error("tile format {got:?} does not match grid format {expected:?}")]
    FormatMismatch {
        expected: PixelFormat,
        got: PixelFormat,
    },
    #[error("operation requires an Rgba8 grid, this grid is {format:?}")]
    UnsupportedFormat { format: PixelFormat },
    #[error(transparent)]
    Tile(#[from] TileError),
}

/// Tiles covering a `width` x `height` image at one mip level.
#[derive(Debug, Clone, Default)]
pub struct TileGrid {
    format: PixelFormat,
    width: u32,
    height: u32,
    level: u8,
    tiles: HashMap<TileCoord, Tile>,
}

/// Number of tiles needed to cover `extent` pixels (0 pixels needs 0 tiles).
const fn tiles_for(extent: u32) -> u32 {
    extent.div_ceil(TILE_SIZE)
}

/// Byte length of a packed RGBA8 image, or [`GridError::ImageTooLarge`] when
/// the product does not fit in a `usize`.
///
/// Unchecked multiplication panics in debug and wraps in release, and a wrapped
/// length would let a far-too-short buffer pass the equality check below.
fn rgba8_len(width: u32, height: u32) -> Result<usize, GridError> {
    (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or(GridError::ImageTooLarge { width, height })
}

impl TileGrid {
    /// An empty grid covering a `width` x `height` image at mip level 0.
    pub fn new(width: u32, height: u32, format: PixelFormat) -> Self {
        Self::new_at_level(width, height, format, 0)
    }

    /// An empty grid covering a `width` x `height` image at `level`.
    ///
    /// `width`/`height` are the dimensions *at that level*, not at level 0.
    pub fn new_at_level(width: u32, height: u32, format: PixelFormat, level: u8) -> Self {
        Self {
            format,
            width,
            height,
            level,
            tiles: HashMap::new(),
        }
    }

    /// Split a packed RGBA8 image into tiles at mip level 0.
    ///
    /// Every covering tile is created, including partial edge tiles, whose
    /// padding bytes are zeroed. Zero, specifically: [`Tile::hash`] covers the
    /// whole `TILE_SIZE` square, so two edge tiles with identical image content
    /// only dedupe if their padding is a fixed value.
    ///
    /// `src` must be exactly `width * height * 4` bytes. A zero-area image
    /// yields a grid with no tiles.
    pub fn from_rgba8(width: u32, height: u32, src: &[u8]) -> Result<Self, GridError> {
        Self::from_rgba8_at_level(width, height, src, 0)
    }

    /// [`TileGrid::from_rgba8`] for a non-zero mip level.
    pub fn from_rgba8_at_level(
        width: u32,
        height: u32,
        src: &[u8],
        level: u8,
    ) -> Result<Self, GridError> {
        let expected = rgba8_len(width, height)?;
        if src.len() != expected {
            return Err(GridError::BadImageLength {
                expected,
                got: src.len(),
            });
        }
        let mut grid = Self::new_at_level(width, height, PixelFormat::Rgba8, level);
        if width == 0 || height == 0 {
            return Ok(grid);
        }

        let tile_stride = TILE_SIZE as usize * 4;
        for ty in 0..grid.tiles_y() {
            for tx in 0..grid.tiles_x() {
                let coord = TileCoord::new(tx as i32, ty as i32, level);
                let rect = grid
                    .valid_rect(coord)
                    .expect("coord generated from the grid extent is in bounds");
                let (ox, oy) = (tx * TILE_SIZE, ty * TILE_SIZE);

                let mut data = vec![0u8; Tile::byte_len(PixelFormat::Rgba8)];
                for row in 0..rect.height {
                    let s = ((oy + row) as usize * width as usize + ox as usize) * 4;
                    let d = row as usize * tile_stride;
                    let n = rect.width as usize * 4;
                    data[d..d + n].copy_from_slice(&src[s..s + n]);
                }
                grid.tiles
                    .insert(coord, Tile::from_bytes(PixelFormat::Rgba8, data)?);
            }
        }
        Ok(grid)
    }

    /// Reassemble a packed RGBA8 image of `width * height * 4` bytes.
    ///
    /// Absent tiles and edge padding read as fully transparent black.
    pub fn to_rgba8(&self) -> Result<Vec<u8>, GridError> {
        if self.format != PixelFormat::Rgba8 {
            return Err(GridError::UnsupportedFormat {
                format: self.format,
            });
        }
        let mut out = vec![0u8; rgba8_len(self.width, self.height)?];
        if self.width == 0 || self.height == 0 {
            return Ok(out);
        }

        let tile_stride = TILE_SIZE as usize * 4;
        for (&coord, tile) in &self.tiles {
            let Some(rect) = self.valid_rect(coord) else {
                continue;
            };
            let (ox, oy) = (coord.x as u32 * TILE_SIZE, coord.y as u32 * TILE_SIZE);
            let data = tile.data();
            for row in 0..rect.height {
                let d = ((oy + row) as usize * self.width as usize + ox as usize) * 4;
                let s = row as usize * tile_stride;
                let n = rect.width as usize * 4;
                out[d..d + n].copy_from_slice(&data[s..s + n]);
            }
        }
        Ok(out)
    }

    /// Storage format shared by every tile in the grid.
    pub const fn format(&self) -> PixelFormat {
        self.format
    }

    /// Mip level this grid describes.
    pub const fn level(&self) -> u8 {
        self.level
    }

    /// Image dimensions in pixels at this grid's mip level.
    pub const fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Number of tile columns covering the image.
    pub const fn tiles_x(&self) -> u32 {
        tiles_for(self.width)
    }

    /// Number of tile rows covering the image.
    pub const fn tiles_y(&self) -> u32 {
        tiles_for(self.height)
    }

    /// Number of tiles actually stored (never more than `tiles_x * tiles_y`).
    pub fn len(&self) -> usize {
        self.tiles.len()
    }

    /// True when no tile is stored. Note a grid can be non-empty in extent and
    /// still store nothing.
    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }

    /// True when `coord` is on this grid's level and inside its extent.
    pub fn contains_coord(&self, coord: TileCoord) -> bool {
        coord.level == self.level
            && coord.x >= 0
            && coord.y >= 0
            && (coord.x as u32) < self.tiles_x()
            && (coord.y as u32) < self.tiles_y()
    }

    /// The sub-rect of `coord` that holds image pixels, in tile-local pixels.
    ///
    /// `None` when the coord is outside the grid. Interior tiles report the
    /// full `TILE_SIZE` square; right/bottom edge tiles report less, and the
    /// remaining bytes of the tile are padding.
    pub fn valid_rect(&self, coord: TileCoord) -> Option<PixelRect> {
        if !self.contains_coord(coord) {
            return None;
        }
        let ox = coord.x as u32 * TILE_SIZE;
        let oy = coord.y as u32 * TILE_SIZE;
        Some(PixelRect::new(
            0,
            0,
            TILE_SIZE.min(self.width - ox),
            TILE_SIZE.min(self.height - oy),
        ))
    }

    /// The area `coord` occupies in image pixel space, clipped to the image.
    pub fn tile_image_rect(&self, coord: TileCoord) -> Option<PixelRect> {
        let local = self.valid_rect(coord)?;
        let (ox, oy) = coord.pixel_origin();
        Some(PixelRect::new(ox, oy, local.width, local.height))
    }

    /// Borrow the tile at `coord`, if one is stored.
    pub fn get(&self, coord: TileCoord) -> Option<&Tile> {
        self.tiles.get(&coord)
    }

    /// Mutably borrow the tile at `coord`, if one is stored.
    pub fn get_mut(&mut self, coord: TileCoord) -> Option<&mut Tile> {
        self.tiles.get_mut(&coord)
    }

    /// Store `tile` at `coord`, returning the tile it displaced.
    ///
    /// Rejects coords outside the grid extent, coords on another mip level,
    /// and tiles whose format differs from the grid's.
    pub fn insert(&mut self, coord: TileCoord, tile: Tile) -> Result<Option<Tile>, GridError> {
        if coord.level != self.level {
            return Err(GridError::LevelMismatch {
                expected: self.level,
                got: coord.level,
            });
        }
        if !self.contains_coord(coord) {
            return Err(GridError::CoordOutOfBounds {
                x: coord.x,
                y: coord.y,
                tiles_x: self.tiles_x(),
                tiles_y: self.tiles_y(),
            });
        }
        if tile.format() != self.format {
            return Err(GridError::FormatMismatch {
                expected: self.format,
                got: tile.format(),
            });
        }
        Ok(self.tiles.insert(coord, tile))
    }

    /// Drop the tile at `coord`; it then reads as fully transparent.
    pub fn remove(&mut self, coord: TileCoord) -> Option<Tile> {
        self.tiles.remove(&coord)
    }

    /// Fetch the tile at `coord`, creating a transparent one if absent.
    ///
    /// Errors on a coord this grid cannot address.
    pub fn get_or_insert_transparent(&mut self, coord: TileCoord) -> Result<&mut Tile, GridError> {
        if !self.tiles.contains_key(&coord) {
            self.insert(coord, Tile::transparent(self.format))?;
        }
        Ok(self
            .tiles
            .get_mut(&coord)
            .expect("just inserted or already present"))
    }

    /// Every stored tile, in unspecified order.
    pub fn iter(&self) -> impl Iterator<Item = (TileCoord, &Tile)> + '_ {
        self.tiles.iter().map(|(&c, t)| (c, t))
    }

    /// Coordinates of every tile whose extent overlaps `rect`, row-major.
    ///
    /// Yields coords regardless of whether a tile is stored there — callers
    /// that want only resident tiles use [`TileGrid::visible`]. Coords outside
    /// the grid extent are never yielded, so an off-canvas viewport yields
    /// nothing.
    pub fn visible_tiles(&self, rect: PixelRect) -> impl Iterator<Item = TileCoord> + '_ {
        let ts = TILE_SIZE as i64;
        let (mut min_tx, mut max_tx, mut min_ty, mut max_ty) = (0i64, -1i64, 0i64, -1i64);
        if !rect.is_empty() && self.tiles_x() > 0 && self.tiles_y() > 0 {
            min_tx = rect.x.div_euclid(ts).max(0);
            max_tx = (rect.right() - 1)
                .div_euclid(ts)
                .min(self.tiles_x() as i64 - 1);
            min_ty = rect.y.div_euclid(ts).max(0);
            max_ty = (rect.bottom() - 1)
                .div_euclid(ts)
                .min(self.tiles_y() as i64 - 1);
        }
        let level = self.level;
        (min_ty..=max_ty).flat_map(move |ty| {
            (min_tx..=max_tx).map(move |tx| TileCoord::new(tx as i32, ty as i32, level))
        })
    }

    /// Resident tiles overlapping `rect`, row-major.
    pub fn visible(&self, rect: PixelRect) -> impl Iterator<Item = (TileCoord, &Tile)> + '_ {
        self.visible_tiles(rect)
            .filter_map(move |c| self.tiles.get(&c).map(|t| (c, t)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic non-uniform test image so a mis-copied row is visible.
    fn ramp(width: u32, height: u32) -> Vec<u8> {
        let mut v = Vec::with_capacity(width as usize * height as usize * 4);
        for y in 0..height {
            for x in 0..width {
                v.push((x % 251) as u8);
                v.push((y % 241) as u8);
                v.push(((x ^ y) % 233) as u8);
                v.push(((x + y) % 199) as u8);
            }
        }
        v
    }

    #[test]
    fn round_trip_exact_multiple() {
        let (w, h) = (TILE_SIZE * 2, TILE_SIZE);
        let src = ramp(w, h);
        let grid = TileGrid::from_rgba8(w, h, &src).unwrap();
        assert_eq!(grid.len(), 2);
        assert_eq!(grid.to_rgba8().unwrap(), src);
    }

    #[test]
    fn round_trip_partial_edge_tiles() {
        // Neither dimension is a multiple of TILE_SIZE.
        let (w, h) = (TILE_SIZE + 37, TILE_SIZE * 2 + 1);
        let src = ramp(w, h);
        let grid = TileGrid::from_rgba8(w, h, &src).unwrap();
        assert_eq!((grid.tiles_x(), grid.tiles_y()), (2, 3));
        assert_eq!(grid.len(), 6);
        let back = grid.to_rgba8().unwrap();
        assert_eq!(back.len(), src.len());
        assert_eq!(back, src, "partial edge tiles must round-trip exactly");
    }

    #[test]
    fn round_trip_smaller_than_one_tile() {
        let (w, h) = (3, 5);
        let src = ramp(w, h);
        let grid = TileGrid::from_rgba8(w, h, &src).unwrap();
        assert_eq!(grid.len(), 1);
        assert_eq!(
            grid.valid_rect(TileCoord::new(0, 0, 0)).unwrap(),
            PixelRect::new(0, 0, 3, 5)
        );
        assert_eq!(grid.to_rgba8().unwrap(), src);
    }

    #[test]
    fn edge_tile_padding_is_not_image_data() {
        let (w, h) = (TILE_SIZE + 2, TILE_SIZE + 2);
        let src = ramp(w, h);
        let mut grid = TileGrid::from_rgba8(w, h, &src).unwrap();
        let edge = TileCoord::new(1, 1, 0);
        assert_eq!(
            grid.valid_rect(edge).unwrap(),
            PixelRect::new(0, 0, 2, 2),
            "the corner tile holds a 2x2 valid rect"
        );

        // Scribble over the padding region only: row 0, column 8, which is
        // outside the 2x2 valid rect.
        let tile = grid.get_mut(edge).unwrap();
        let px = 8 * 4;
        tile.data_mut()[px..px + 4].copy_from_slice(&[7, 7, 7, 7]);

        assert_eq!(
            grid.to_rgba8().unwrap(),
            src,
            "padding outside the valid rect must not reach the flattened image"
        );
    }

    #[test]
    fn zero_area_image_has_no_tiles() {
        let grid = TileGrid::from_rgba8(0, 0, &[]).unwrap();
        assert_eq!((grid.tiles_x(), grid.tiles_y()), (0, 0));
        assert_eq!(grid.len(), 0);
        assert!(grid.to_rgba8().unwrap().is_empty());
        assert!(grid.valid_rect(TileCoord::new(0, 0, 0)).is_none());
        assert_eq!(grid.visible_tiles(PixelRect::new(0, 0, 10, 10)).count(), 0);
    }

    #[test]
    fn zero_height_image_is_not_a_panic() {
        let grid = TileGrid::from_rgba8(64, 0, &[]).unwrap();
        assert_eq!(grid.len(), 0);
        assert!(grid.to_rgba8().unwrap().is_empty());
    }

    #[test]
    fn wrong_source_length_is_rejected() {
        // Too short: reading it would index out of bounds.
        assert!(matches!(
            TileGrid::from_rgba8(4, 4, &[0u8; 10]),
            Err(GridError::BadImageLength {
                expected: 64,
                got: 10
            })
        ));
        // Too long is just as wrong, and is the usual signature of a caller
        // passing the wrong stride or a stale dimension. Silently truncating
        // would tile the wrong pixels and report success.
        assert!(matches!(
            TileGrid::from_rgba8(4, 4, &[0u8; 100]),
            Err(GridError::BadImageLength {
                expected: 64,
                got: 100
            })
        ));
        assert!(matches!(
            TileGrid::from_rgba8_at_level(4, 4, &[0u8; 100], 2),
            Err(GridError::BadImageLength {
                expected: 64,
                got: 100
            })
        ));
        // A zero-area image takes no bytes, so a non-empty buffer is a mismatch.
        assert!(matches!(
            TileGrid::from_rgba8(0, 0, &[0u8; 4]),
            Err(GridError::BadImageLength {
                expected: 0,
                got: 4
            })
        ));
    }

    #[test]
    fn absent_tile_reads_as_transparent() {
        let (w, h) = (TILE_SIZE * 2, TILE_SIZE);
        let src = vec![255u8; w as usize * h as usize * 4];
        let mut grid = TileGrid::from_rgba8(w, h, &src).unwrap();
        assert!(grid.remove(TileCoord::new(1, 0, 0)).is_some());
        assert_eq!(grid.len(), 1);

        let back = grid.to_rgba8().unwrap();
        // Left half untouched, right half transparent.
        let row0 = &back[0..w as usize * 4];
        assert!(row0[..TILE_SIZE as usize * 4].iter().all(|&b| b == 255));
        assert!(row0[TILE_SIZE as usize * 4..].iter().all(|&b| b == 0));
    }

    #[test]
    fn visible_tiles_covers_only_overlapping_coords() {
        let grid = TileGrid::new(TILE_SIZE * 3, TILE_SIZE * 2, PixelFormat::Rgba8);

        // A rect entirely inside tile (1, 1).
        let one: Vec<_> = grid
            .visible_tiles(PixelRect::new(
                TILE_SIZE as i64 + 10,
                TILE_SIZE as i64 + 10,
                4,
                4,
            ))
            .collect();
        assert_eq!(one, vec![TileCoord::new(1, 1, 0)]);

        // A rect straddling the tile 0/1 boundary in both axes.
        // A rect straddling the tile 0/1 boundary in both axes. The order is
        // part of the contract: row-major, so the whole first tile row comes
        // before the second.
        let four: Vec<_> = grid
            .visible_tiles(PixelRect::new(
                TILE_SIZE as i64 - 1,
                TILE_SIZE as i64 - 1,
                2,
                2,
            ))
            .collect();
        assert_eq!(
            four,
            vec![
                TileCoord::new(0, 0, 0),
                TileCoord::new(1, 0, 0),
                TileCoord::new(0, 1, 0),
                TileCoord::new(1, 1, 0),
            ],
            "visible_tiles must yield row-major order"
        );
    }

    #[test]
    fn visible_tiles_treats_the_rect_end_as_exclusive() {
        // The grid is 3x2 tiles, so tiles exist beyond (0, 0) in both axes and
        // the `.min(tiles - 1)` clamp cannot mask an off-by-one here.
        let grid = TileGrid::new(TILE_SIZE * 3, TILE_SIZE * 2, PixelFormat::Rgba8);

        // right() and bottom() are ONE PAST the last covered pixel. A rect of
        // exactly [0, TILE_SIZE) x [0, TILE_SIZE) touches tile (0, 0) alone;
        // treating the end as inclusive would upload and draw a spurious extra
        // tile row and column for every viewport landing on a tile boundary.
        let exact: Vec<_> = grid
            .visible_tiles(PixelRect::new(0, 0, TILE_SIZE, TILE_SIZE))
            .collect();
        assert_eq!(exact, vec![TileCoord::new(0, 0, 0)]);

        // One pixel past the boundary genuinely reaches the next tile in both
        // axes, so the bound is not merely off in the other direction.
        let over: Vec<_> = grid
            .visible_tiles(PixelRect::new(0, 0, TILE_SIZE + 1, TILE_SIZE + 1))
            .collect();
        assert_eq!(
            over,
            vec![
                TileCoord::new(0, 0, 0),
                TileCoord::new(1, 0, 0),
                TileCoord::new(0, 1, 0),
                TileCoord::new(1, 1, 0),
            ]
        );

        // The same boundary, one tile in: a rect covering exactly tile column 1
        // and tile row 1 must not spill into column 2.
        let middle: Vec<_> = grid
            .visible_tiles(PixelRect::new(
                TILE_SIZE as i64,
                TILE_SIZE as i64,
                TILE_SIZE,
                TILE_SIZE,
            ))
            .collect();
        assert_eq!(middle, vec![TileCoord::new(1, 1, 0)]);
    }

    #[test]
    fn visible_tiles_is_row_major_over_a_wide_grid() {
        // 3x2 tiles, whole grid visible: a column-major walk would put
        // (0, 1) second instead of (1, 0).
        let grid = TileGrid::new(TILE_SIZE * 3, TILE_SIZE * 2, PixelFormat::Rgba8);
        let seen: Vec<_> = grid
            .visible_tiles(PixelRect::new(0, 0, TILE_SIZE * 3, TILE_SIZE * 2))
            .collect();
        assert_eq!(
            seen,
            vec![
                TileCoord::new(0, 0, 0),
                TileCoord::new(1, 0, 0),
                TileCoord::new(2, 0, 0),
                TileCoord::new(0, 1, 0),
                TileCoord::new(1, 1, 0),
                TileCoord::new(2, 1, 0),
            ]
        );
    }

    #[test]
    fn visible_at_the_edge_of_the_coordinate_space_does_not_overflow() {
        // PixelRect::x is public and signed, so right()/bottom() are reachable
        // sums; unchecked they panicked with "attempt to add with overflow".
        let grid = TileGrid::from_rgba8(
            TILE_SIZE,
            TILE_SIZE,
            &vec![7u8; TILE_SIZE as usize * TILE_SIZE as usize * 4],
        )
        .unwrap();

        assert_eq!(
            grid.visible_tiles(PixelRect::new(i64::MAX - 4, 0, 10, 10))
                .count(),
            0
        );
        assert_eq!(
            grid.visible_tiles(PixelRect::new(0, i64::MAX - 4, 10, 10))
                .count(),
            0
        );
        assert_eq!(
            grid.visible(PixelRect::new(i64::MAX, i64::MAX, 1, 1))
                .count(),
            0
        );
        assert_eq!(
            PixelRect::new(i64::MAX - 4, i64::MAX - 4, 10, 10).right(),
            i64::MAX
        );
        assert_eq!(
            PixelRect::new(i64::MAX - 4, i64::MAX - 4, 10, 10).bottom(),
            i64::MAX
        );

        // The far negative corner is just as reachable and must also yield
        // nothing rather than wrap into the grid.
        assert_eq!(
            grid.visible_tiles(PixelRect::new(i64::MIN, i64::MIN, 10, 10))
                .count(),
            0
        );
    }

    #[test]
    fn unaddressable_image_dimensions_are_an_error_not_an_overflow() {
        // width * height * 4 exceeds usize; the length check itself used to
        // panic before it could report anything.
        assert!(matches!(
            TileGrid::from_rgba8(u32::MAX, u32::MAX, &[]),
            Err(GridError::ImageTooLarge { .. })
        ));
        assert!(matches!(
            TileGrid::from_rgba8_at_level(u32::MAX, u32::MAX, &[], 2),
            Err(GridError::ImageTooLarge { .. })
        ));
        assert!(matches!(
            TileGrid::new(u32::MAX, u32::MAX, PixelFormat::Rgba8).to_rgba8(),
            Err(GridError::ImageTooLarge { .. })
        ));
    }

    #[test]
    fn edge_tile_padding_is_zero_not_arbitrary() {
        // Every image pixel here is non-zero, so any zero byte inside a tile
        // must be padding, and every padding byte must be zero: Tile::hash
        // covers the padding, so dedup depends on it being a fixed value.
        let (w, h) = (TILE_SIZE + 5, TILE_SIZE + 3);
        let src = vec![0xABu8; w as usize * h as usize * 4];
        let grid = TileGrid::from_rgba8(w, h, &src).unwrap();

        let corner = TileCoord::new(1, 1, 0);
        let rect = grid.valid_rect(corner).unwrap();
        assert_eq!(rect, PixelRect::new(0, 0, 5, 3));
        let data = grid.get(corner).unwrap().data();
        let stride = TILE_SIZE as usize * 4;

        for row in 0..TILE_SIZE as usize {
            let valid_bytes = if row < rect.height as usize {
                rect.width as usize * 4
            } else {
                0 // rows below the image are padding end to end
            };
            for (col, &b) in data[row * stride..(row + 1) * stride].iter().enumerate() {
                if col < valid_bytes {
                    assert_eq!(
                        b, 0xAB,
                        "image byte at row {row}, col {col} was overwritten"
                    );
                } else {
                    assert_eq!(b, 0, "padding at row {row}, col {col} must be zero");
                }
            }
        }

        // The same claim, stated as the property that depends on it: two grids
        // whose edge tiles hold the same image content hash identically.
        let other = TileGrid::from_rgba8(w, h, &src).unwrap();
        assert_eq!(
            grid.get(corner).unwrap().hash(),
            other.get(corner).unwrap().hash()
        );
    }

    #[test]
    fn visible_tiles_clamps_to_the_grid() {
        let grid = TileGrid::new(TILE_SIZE, TILE_SIZE, PixelFormat::Rgba8);
        // Viewport starts off-canvas to the top-left and overruns to the right.
        let seen: Vec<_> = grid
            .visible_tiles(PixelRect::new(-1000, -1000, 5000, 5000))
            .collect();
        assert_eq!(seen, vec![TileCoord::new(0, 0, 0)]);

        // Wholly off-canvas rects yield nothing, in either direction.
        assert_eq!(
            grid.visible_tiles(PixelRect::new(-500, -500, 100, 100))
                .count(),
            0
        );
        assert_eq!(
            grid.visible_tiles(PixelRect::new(TILE_SIZE as i64 * 4, 0, 100, 100))
                .count(),
            0
        );
        // An empty rect overlaps nothing even at a valid origin.
        assert_eq!(grid.visible_tiles(PixelRect::new(0, 0, 0, 10)).count(), 0);
    }

    #[test]
    fn visible_skips_absent_tiles() {
        let (w, h) = (TILE_SIZE * 2, TILE_SIZE);
        let mut grid = TileGrid::from_rgba8(w, h, &vec![1u8; w as usize * h as usize * 4]).unwrap();
        grid.remove(TileCoord::new(0, 0, 0));
        let seen: Vec<_> = grid
            .visible(PixelRect::new(0, 0, w, h))
            .map(|(c, _)| c)
            .collect();
        assert_eq!(seen, vec![TileCoord::new(1, 0, 0)]);
    }

    #[test]
    fn insert_rejects_out_of_bounds_and_mismatches() {
        let mut grid = TileGrid::new(TILE_SIZE, TILE_SIZE, PixelFormat::Rgba8);

        assert!(matches!(
            grid.insert(
                TileCoord::new(1, 0, 0),
                Tile::transparent(PixelFormat::Rgba8)
            ),
            Err(GridError::CoordOutOfBounds { .. })
        ));
        assert!(matches!(
            grid.insert(
                TileCoord::new(-1, 0, 0),
                Tile::transparent(PixelFormat::Rgba8)
            ),
            Err(GridError::CoordOutOfBounds { .. })
        ));
        assert!(matches!(
            grid.insert(
                TileCoord::new(0, 0, 1),
                Tile::transparent(PixelFormat::Rgba8)
            ),
            Err(GridError::LevelMismatch { .. })
        ));
        assert!(matches!(
            grid.insert(
                TileCoord::new(0, 0, 0),
                Tile::transparent(PixelFormat::Rgba16)
            ),
            Err(GridError::FormatMismatch { .. })
        ));
        assert_eq!(grid.len(), 0);

        assert!(grid
            .insert(
                TileCoord::new(0, 0, 0),
                Tile::transparent(PixelFormat::Rgba8)
            )
            .unwrap()
            .is_none());
        assert!(grid
            .insert(
                TileCoord::new(0, 0, 0),
                Tile::transparent(PixelFormat::Rgba8)
            )
            .unwrap()
            .is_some());
        assert_eq!(grid.len(), 1);
    }

    #[test]
    fn get_or_insert_transparent_creates_then_reuses() {
        let mut grid = TileGrid::new(TILE_SIZE, TILE_SIZE, PixelFormat::Rgba8);
        let coord = TileCoord::new(0, 0, 0);
        grid.get_or_insert_transparent(coord).unwrap().data_mut()[0] = 42;
        assert_eq!(grid.len(), 1);
        assert_eq!(grid.get_or_insert_transparent(coord).unwrap().data()[0], 42);
        assert!(grid
            .get_or_insert_transparent(TileCoord::new(9, 9, 0))
            .is_err());
    }

    #[test]
    fn edits_through_the_grid_change_the_tile_hash() {
        let (w, h) = (16, 16);
        let grid = TileGrid::from_rgba8(w, h, &ramp(w, h)).unwrap();
        let coord = TileCoord::new(0, 0, 0);
        let before = grid.get(coord).unwrap().hash();

        let mut grid = grid;
        grid.get_mut(coord).unwrap().data_mut()[0] ^= 0xff;
        assert_ne!(grid.get(coord).unwrap().hash(), before);
    }

    #[test]
    fn non_rgba8_grid_refuses_to_flatten() {
        let grid = TileGrid::new(4, 4, PixelFormat::RgbaF32);
        assert!(matches!(
            grid.to_rgba8(),
            Err(GridError::UnsupportedFormat { .. })
        ));
    }

    #[test]
    fn tile_image_rect_is_clipped_to_the_image() {
        let grid = TileGrid::new(TILE_SIZE + 5, TILE_SIZE + 5, PixelFormat::Rgba8);
        assert_eq!(
            grid.tile_image_rect(TileCoord::new(1, 1, 0)).unwrap(),
            PixelRect::new(TILE_SIZE as i64, TILE_SIZE as i64, 5, 5)
        );
        assert_eq!(
            grid.tile_image_rect(TileCoord::new(0, 0, 0)).unwrap(),
            PixelRect::new(0, 0, TILE_SIZE, TILE_SIZE)
        );
    }

    #[test]
    fn grid_at_a_mip_level_addresses_that_level() {
        let (w, h) = (TILE_SIZE + 1, 4);
        let src = ramp(w, h);
        let grid = TileGrid::from_rgba8_at_level(w, h, &src, 3).unwrap();
        assert_eq!(grid.level(), 3);
        assert_eq!(grid.to_rgba8().unwrap(), src);
        let coords: Vec<_> = grid.visible_tiles(PixelRect::new(0, 0, w, h)).collect();
        assert_eq!(
            coords,
            vec![TileCoord::new(0, 0, 3), TileCoord::new(1, 0, 3)]
        );
        assert!(grid.get(TileCoord::new(0, 0, 0)).is_none());

        // A coord addressed to another mip level names a different pixel scale
        // entirely, so every extent query must refuse it even though its (x, y)
        // is inside this grid. Without the level guard in `contains_coord`,
        // `valid_rect` would hand back a rect measured in the wrong units.
        let wrong_level = TileCoord::new(0, 0, 0);
        assert!(!grid.contains_coord(wrong_level));
        assert!(grid.valid_rect(wrong_level).is_none());
        assert!(grid.tile_image_rect(wrong_level).is_none());
        let wrong_level_edge = TileCoord::new(1, 0, 4);
        assert!(!grid.contains_coord(wrong_level_edge));
        assert!(grid.valid_rect(wrong_level_edge).is_none());
        assert!(grid.tile_image_rect(wrong_level_edge).is_none());

        // The same coords on this grid's own level all resolve, so the
        // assertions above are about the level and not about (x, y).
        for c in [TileCoord::new(0, 0, 3), TileCoord::new(1, 0, 3)] {
            assert!(grid.contains_coord(c));
            assert!(grid.valid_rect(c).is_some());
            assert!(grid.tile_image_rect(c).is_some());
        }
        assert_eq!(
            grid.valid_rect(TileCoord::new(1, 0, 3)).unwrap(),
            PixelRect::new(0, 0, 1, 4),
            "the level-3 edge tile holds a 1x4 valid rect"
        );
    }
}
