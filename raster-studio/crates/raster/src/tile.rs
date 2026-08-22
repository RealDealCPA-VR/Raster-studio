//! Fixed-size tiles and their content-addressed identity.

use serde::{Deserialize, Serialize};

use crate::format::PixelFormat;

/// Edge length of a tile in pixels.
///
/// Start at 256; the render doc calls for benchmarking 512 for high-resolution
/// workloads. Keep this a single knob so the benchmark is a one-line change.
pub const TILE_SIZE: u32 = 256;

/// Address of a tile within a layer's tile grid, at a given mip level.
///
/// `(x, y)` are tile indices (not pixels): pixel origin is
/// `(x * TILE_SIZE, y * TILE_SIZE)` at `level == 0`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TileCoord {
    pub x: i32,
    pub y: i32,
    /// Mip level; 0 is full resolution, each level halves linear dimensions.
    pub level: u8,
}

impl TileCoord {
    pub const fn new(x: i32, y: i32, level: u8) -> Self {
        Self { x, y, level }
    }

    /// Pixel-space origin of this tile at its own mip level.
    pub const fn pixel_origin(self) -> (i64, i64) {
        (
            self.x as i64 * TILE_SIZE as i64,
            self.y as i64 * TILE_SIZE as i64,
        )
    }
}

/// A content hash uniquely identifying tile pixel data.
///
/// Two tiles with identical bytes share a hash, enabling deduplication in the
/// asset store and cheap "did this tile change?" checks for invalidation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TileHash(pub [u8; 32]);

impl TileHash {
    /// Hash a raw byte buffer (BLAKE3).
    pub fn of(bytes: &[u8]) -> Self {
        TileHash(*blake3::hash(bytes).as_bytes())
    }

    /// Lowercase hex, suitable for content-addressed filenames.
    pub fn to_hex(self) -> String {
        let mut s = String::with_capacity(64);
        for b in self.0 {
            s.push_str(&format!("{b:02x}"));
        }
        s
    }
}

/// A single tile of pixel data plus its cached content hash.
#[derive(Debug, Clone)]
pub struct Tile {
    pub format: PixelFormat,
    /// Tightly packed pixels, row-major, `TILE_SIZE * TILE_SIZE` of them.
    pub data: Vec<u8>,
    hash: TileHash,
}

impl Tile {
    /// Build a tile from raw bytes, validating the length for the format.
    pub fn from_bytes(format: PixelFormat, data: Vec<u8>) -> Result<Self, TileError> {
        let expected = TILE_SIZE as usize * TILE_SIZE as usize * format.bytes_per_pixel();
        if data.len() != expected {
            return Err(TileError::BadLength {
                expected,
                got: data.len(),
            });
        }
        let hash = TileHash::of(&data);
        Ok(Self { format, data, hash })
    }

    /// A fully transparent tile in the given format.
    pub fn transparent(format: PixelFormat) -> Self {
        let len = TILE_SIZE as usize * TILE_SIZE as usize * format.bytes_per_pixel();
        let data = vec![0u8; len];
        let hash = TileHash::of(&data);
        Self { format, data, hash }
    }

    /// Content hash, computed at construction and after [`Tile::set_data`].
    pub fn hash(&self) -> TileHash {
        self.hash
    }

    /// Replace pixel data, recomputing the content hash. Length must match.
    pub fn set_data(&mut self, data: Vec<u8>) -> Result<(), TileError> {
        let expected = TILE_SIZE as usize * TILE_SIZE as usize * self.format.bytes_per_pixel();
        if data.len() != expected {
            return Err(TileError::BadLength {
                expected,
                got: data.len(),
            });
        }
        self.hash = TileHash::of(&data);
        self.data = data;
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TileError {
    #[error("tile byte length mismatch: expected {expected}, got {got}")]
    BadLength { expected: usize, got: usize },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transparent_tile_is_deterministic() {
        let a = Tile::transparent(PixelFormat::Rgba8);
        let b = Tile::transparent(PixelFormat::Rgba8);
        assert_eq!(a.hash(), b.hash(), "identical tiles must share a hash");
    }

    #[test]
    fn bad_length_is_rejected() {
        let err = Tile::from_bytes(PixelFormat::Rgba8, vec![0; 10]);
        assert!(matches!(err, Err(TileError::BadLength { .. })));
    }

    #[test]
    fn hash_hex_is_64_chars() {
        let t = Tile::transparent(PixelFormat::Rgba8);
        assert_eq!(t.hash().to_hex().len(), 64);
    }
}
