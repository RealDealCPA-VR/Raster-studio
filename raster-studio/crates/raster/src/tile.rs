//! Fixed-size tiles and their content-addressed identity.

use std::sync::OnceLock;

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
/// `(x * TILE_SIZE, y * TILE_SIZE)` in the coordinate space of `level`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
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

const HEX_DIGITS: [u8; 16] = *b"0123456789abcdef";

impl TileHash {
    /// Hash a raw byte buffer (BLAKE3).
    pub fn of(bytes: &[u8]) -> Self {
        TileHash(*blake3::hash(bytes).as_bytes())
    }

    /// Lowercase hex, suitable for content-addressed filenames.
    ///
    /// Always 64 characters, and allocates exactly once.
    pub fn to_hex(self) -> String {
        let mut bytes = [0u8; 64];
        for (i, b) in self.0.iter().enumerate() {
            bytes[i * 2] = HEX_DIGITS[(b >> 4) as usize];
            bytes[i * 2 + 1] = HEX_DIGITS[(b & 0x0f) as usize];
        }
        // SAFETY-free: every byte written comes from HEX_DIGITS, which is ASCII.
        String::from_utf8(bytes.to_vec()).expect("hex digits are ASCII")
    }
}

/// A single tile of pixel data plus its lazily cached content hash.
///
/// Invariants, enforced by the constructors and by [`Tile::data_mut`]:
/// * `data.len() == TILE_SIZE * TILE_SIZE * format.bytes_per_pixel()`.
/// * [`Tile::hash`] always reflects the current bytes — the cache is dropped
///   the moment a caller asks for mutable access, so a stale hash cannot be
///   observed even if the caller forgets or panics mid-edit.
///
/// A tile always stores a *full* `TILE_SIZE` square. Images whose dimensions
/// are not a multiple of `TILE_SIZE` are handled by
/// [`crate::grid::TileGrid`], which records the valid sub-rect of each edge
/// tile; the bytes outside that sub-rect are padding and carry no meaning.
#[derive(Debug)]
pub struct Tile {
    format: PixelFormat,
    /// Tightly packed pixels, row-major, `TILE_SIZE * TILE_SIZE` of them.
    data: Vec<u8>,
    hash: OnceLock<TileHash>,
}

impl Clone for Tile {
    fn clone(&self) -> Self {
        let hash = OnceLock::new();
        if let Some(h) = self.hash.get() {
            let _ = hash.set(*h);
        }
        Self {
            format: self.format,
            data: self.data.clone(),
            hash,
        }
    }
}

impl PartialEq for Tile {
    /// Compares storage format and pixel bytes; the hash cache is not state.
    fn eq(&self, other: &Self) -> bool {
        self.format == other.format && self.data == other.data
    }
}

impl Eq for Tile {}

impl Tile {
    /// Byte length a tile of `format` must have.
    pub const fn byte_len(format: PixelFormat) -> usize {
        TILE_SIZE as usize * TILE_SIZE as usize * format.bytes_per_pixel()
    }

    /// Build a tile from raw bytes, validating the length for the format.
    pub fn from_bytes(format: PixelFormat, data: Vec<u8>) -> Result<Self, TileError> {
        let expected = Self::byte_len(format);
        if data.len() != expected {
            return Err(TileError::BadLength {
                expected,
                got: data.len(),
            });
        }
        Ok(Self {
            format,
            data,
            hash: OnceLock::new(),
        })
    }

    /// A fully transparent tile in the given format.
    pub fn transparent(format: PixelFormat) -> Self {
        Self {
            format,
            data: vec![0u8; Self::byte_len(format)],
            hash: OnceLock::new(),
        }
    }

    /// Storage format of this tile. Immutable: changing it would invalidate
    /// the byte-length invariant.
    pub const fn format(&self) -> PixelFormat {
        self.format
    }

    /// Content hash of the current bytes (BLAKE3).
    ///
    /// Computed on first call and cached. [`Tile::data_mut`] drops the cache,
    /// so the value returned here always matches the bytes returned by
    /// [`Tile::data`].
    pub fn hash(&self) -> TileHash {
        *self.hash.get_or_init(|| TileHash::of(&self.data))
    }

    /// Read-only view of the pixel bytes.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Mutable view of the pixel bytes.
    ///
    /// The cached hash is invalidated *before* the slice is handed out, so it
    /// is recomputed on the next [`Tile::hash`] call no matter what the caller
    /// does with the slice. The length cannot change, so the byte-length
    /// invariant survives.
    pub fn data_mut(&mut self) -> &mut [u8] {
        self.hash = OnceLock::new();
        &mut self.data
    }

    /// Replace pixel data, invalidating the content hash. Length must match.
    pub fn set_data(&mut self, data: Vec<u8>) -> Result<(), TileError> {
        let expected = Self::byte_len(self.format);
        if data.len() != expected {
            return Err(TileError::BadLength {
                expected,
                got: data.len(),
            });
        }
        self.data = data;
        self.hash = OnceLock::new();
        Ok(())
    }

    /// Consume the tile, yielding its pixel bytes.
    pub fn into_data(self) -> Vec<u8> {
        self.data
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

    #[test]
    fn to_hex_matches_manual_encoding() {
        let mut raw = [0u8; 32];
        for (i, b) in raw.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(7).wrapping_add(0xa3);
        }
        let expected: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(TileHash(raw).to_hex(), expected);
    }

    #[test]
    fn data_mut_invalidates_the_cached_hash() {
        let mut t = Tile::transparent(PixelFormat::Rgba8);
        let before = t.hash();

        t.data_mut()[0] = 0xff;

        let after = t.hash();
        assert_ne!(before, after, "mutating pixels must change the hash");
        assert_eq!(
            after,
            TileHash::of(t.data()),
            "cached hash must match the current bytes"
        );
    }

    #[test]
    fn hash_is_stable_when_data_is_untouched() {
        let mut t = Tile::transparent(PixelFormat::Rgba8);
        let a = t.hash();
        // Taking a mutable view without writing must still agree with the bytes.
        let _ = t.data_mut();
        assert_eq!(t.hash(), a);
    }

    #[test]
    fn set_data_updates_the_hash() {
        let mut t = Tile::transparent(PixelFormat::Rgba8);
        let before = t.hash();
        t.set_data(vec![9u8; Tile::byte_len(PixelFormat::Rgba8)])
            .unwrap();
        assert_ne!(t.hash(), before);
        assert_eq!(t.hash(), TileHash::of(t.data()));
    }

    #[test]
    fn set_data_rejects_wrong_length() {
        let mut t = Tile::transparent(PixelFormat::Rgba8);
        let before = t.hash();
        assert!(matches!(
            t.set_data(vec![1u8; 5]),
            Err(TileError::BadLength { .. })
        ));
        assert_eq!(t.hash(), before, "a rejected write must not disturb state");
    }

    #[test]
    fn clone_carries_the_same_hash() {
        let mut a = Tile::transparent(PixelFormat::Rgba8);
        a.data_mut()[17] = 3;
        let b = a.clone();
        assert_eq!(a.hash(), b.hash());
        assert_eq!(a, b);
    }

    #[test]
    fn pixel_origin_scales_by_tile_size() {
        assert_eq!(
            TileCoord::new(2, 3, 0).pixel_origin(),
            (2 * TILE_SIZE as i64, 3 * TILE_SIZE as i64)
        );
    }
}
