//! Tiles, mipmaps, codecs, and pixel formats — the substrate every raster
//! layer and composite is built from.
//!
//! # Tile-first strategy
//! We never model an image as a permanently resident full-canvas texture.
//! Pixels live in fixed-size [`Tile`]s (default [`TILE_SIZE`]), addressed by
//! [`TileCoord`], and content-addressed by [`TileHash`] so identical tiles are
//! stored once. A [`TileGrid`] binds those tiles to one mip level of one
//! image, tracks which part of each edge tile is real image data, and answers
//! "which tiles does this viewport touch?". Higher-level crates (`render`,
//! `asset-store`) build GPU/CPU caches on top of these primitives.
//!
//! # Correctness rules this crate enforces
//! * A tile's [`TileHash`] can never go stale: pixel bytes are only reachable
//!   through accessors that drop the cached hash.
//! * Images whose dimensions are not a multiple of [`TILE_SIZE`] round-trip
//!   exactly; padding in edge tiles is never mistaken for image content.
//! * Mip levels are filtered in linear, premultiplied space, so they neither
//!   darken nor bleed color out of transparent pixels.

pub mod codec;
pub mod format;
pub mod grid;
pub mod mipmap;
pub mod tile;

pub use format::PixelFormat;
pub use grid::{GridError, PixelRect, TileGrid};
pub use mipmap::{MipChain, MipError, MipLevel};
pub use tile::{Tile, TileCoord, TileError, TileHash, TILE_SIZE};
