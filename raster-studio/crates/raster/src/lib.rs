//! Tiles, mipmaps, codecs, and pixel formats — the substrate every raster
//! layer and composite is built from.
//!
//! # Tile-first strategy
//! We never model an image as a permanently resident full-canvas texture.
//! Pixels live in fixed-size [`Tile`]s (default [`TILE_SIZE`]), addressed by
//! [`TileCoord`], and content-addressed by [`TileHash`] so identical tiles are
//! stored once. Higher-level crates (`render`, `asset-store`) build GPU/CPU
//! caches on top of these primitives.

pub mod codec;
pub mod format;
pub mod mipmap;
pub mod tile;

pub use format::PixelFormat;
pub use tile::{Tile, TileCoord, TileHash, TILE_SIZE};
