//! Editor core: the [`Document`], the [`Command`] system, and the undo/redo
//! [`History`].
//!
//! The central invariant of the whole product — *the document remains editable
//! after every operation* — is enforced here. Every user-visible edit is a
//! deterministic [`Command`] that can `apply`, produce its `inverse`, be
//! serialized, and be replayed from a journal. Two properties make that
//! invariant hold rather than merely being asserted:
//!
//! * **Commands are atomic.** A command that fails leaves the document
//!   byte-identical to what it was, transactions included — they roll back the
//!   members that already applied. The one exception is
//!   [`CommandError::RollbackFailed`], which is that rollback *itself* failing:
//!   it reports that the guarantee could not be kept and that the document must
//!   be reloaded rather than edited on. [`History`] records an entry only on
//!   success, so there is never a mutation with no way back.
//! * **Pixels are references.** A raster edit is a *tile delta*: for each tile
//!   the edit touches, the content hash it carries afterwards. The bytes live
//!   in the tile store; the document holds hashes ([`crate::pixels`]). That is
//!   what makes a brush stroke across a hundred tiles one small, invertible
//!   command — and one undo step.
//!
//! This crate depends on `raster` for the vocabulary of tile identity and tile
//! geometry: [`raster::TileCoord`], [`raster::TileHash`], [`raster::PixelRect`],
//! [`raster::TILE_SIZE`], and [`raster::Tile`] with [`raster::PixelFormat`] —
//! the last two so a fill can compute the *same* hash the tile store computes
//! for the same bytes ([`crate::FillColor::solid_tile_hash`]). The identity of a
//! tile is defined once, in the crate that owns tiles, so a command and the
//! store it addresses cannot disagree about what a tile is. No rendering,
//! compositing, or pixel buffer of any kind enters this crate.

pub mod command;
pub mod document;
pub mod history;
pub mod pixels;
pub mod selection;

pub use command::{layer_class_name, resolve_target, Command, CommandError, LayerPatch, Patch};
pub use document::{
    canvas_size_is_supported, Document, DocumentError, DocumentMeta, Guide, GuideAxis, Guides,
    DOCUMENT_FORMAT_VERSION, MAX_CANVAS_DIMENSION, MAX_CANVAS_PIXELS, MIN_SUPPORTED_FORMAT_VERSION,
};
pub use history::{History, DEFAULT_HISTORY_LIMIT};
pub use pixels::{
    edge_tiles, Coverage, FillColor, FillValue, MaskCoverage, PixelError, PixelKey, PixelStore,
    PixelTarget, TileDelta, TileEdit, TileMap, MASK_TILE_BYTES,
};
pub use selection::{Selection, SelectionError, SelectionMask, MAX_MASK_SAMPLES};
