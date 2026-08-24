//! The authoritative CPU tile compositor.
//!
//! This crate answers one question: given an [`editor_core::Document`] and a
//! region, what pixels does the user see? Everything else in the editor —
//! the GPU presenter, export, thumbnails, golden-image tests — consumes this
//! answer rather than computing its own.
//!
//! Having exactly one implementation is deliberate. A GPU compositor running
//! alongside a CPU one is two sources of truth that drift; here the GPU only
//! presents tiles this crate produced.
//!
//! # Working space
//!
//! Compositing happens in **linear, premultiplied** `f32`. Inputs are
//! converted on the way in and results converted back on the way out, so the
//! blend, mask and adjustment maths never sees gamma-encoded values.
//!
//! Concretely, and this is the invariant every function here upholds:
//!
//! * A [`Canvas`] — the working buffer, and everything this crate returns —
//!   holds linear premultiplied RGBA. Nothing else is ever stored in one.
//! * Stored pixels are straight-alpha 8-bit RGBA encoded in the *document's*
//!   [`color::ColorSpace`]. They are decoded with [`color::to_linear`] and
//!   premultiplied at the moment they are read.
//! * Results go back out through [`Canvas::to_rgba8`], which unpremultiplies,
//!   encodes with [`color::from_linear`], and quantizes — the only place in
//!   the crate that produces gamma-encoded values.
//! * The two documented exceptions, both because the operation is *defined* on
//!   encoded values rather than on light: [`BlendSpace::Encoded`] (opt-in, for
//!   reproducing another editor's look) and four of the five adjustments (see
//!   [`adjust`], which tabulates exactly which and why).
//!
//! Alpha is premultiplied for the usual reason and one specific one: bilinear
//! resampling of a transformed layer averages neighbouring samples, and
//! averaging *straight* colour across a transparent edge drags a transparent
//! pixel's meaningless RGB into the result as a dark halo.
//!
//! # Shape of the API
//!
//! ```no_run
//! use compositor::{composite_region, CompositeOptions, MemoryTileSource};
//! use editor_core::Document;
//! use raster::PixelRect;
//!
//! let doc = Document::new(1920, 1080, "Untitled");
//! let tiles = MemoryTileSource::new();
//! let viewport = PixelRect::new(0, 0, 800, 600);
//! let image = composite_region(&doc, &tiles, viewport, 0, CompositeOptions::default())?;
//! let rgba8 = image.to_rgba8(&doc.meta.color_space);
//! # Ok::<(), compositor::CompositeError>(())
//! ```
//!
//! `(document, region, level) -> pixels`. The document holds tile *hashes*, so
//! the second argument is a [`TileSource`] that resolves them to bytes; a hash
//! the source cannot resolve reads as transparent rather than failing the
//! frame. For repeated composites — which is every interactive frame — use
//! [`TileCompositor`], which caches tiles keyed by a hash of the inputs that
//! produced them.
//!
//! # Not yet
//!
//! Honest gaps, each of which changes what a document looks like and none of
//! which is silently approximated:
//!
//! * **A pattern fill** ([`layer_model::PatternFill`]) names an `AssetId`, and
//!   this crate has no asset store to resolve one against, so a pattern overlay
//!   — and a glow or stroke filled with a pattern — draws nothing. Solid and
//!   gradient fills draw. See the [`effects`] module docs for the rest of the
//!   layer-style gaps: contours, glow jitter, stroke overprint, the three bevel
//!   techniques, and the clamp on how far an effect may reach.
//! * **Smart-object layers** have no rasterizer here, so they contribute
//!   nothing and cannot serve as a clipping base. Text and shape layers do
//!   render — see [`text`] and [`shape`] for the limits of each.
//! * **A vector layer mask** ([`layer_model::MaskKind::Vector`]) has no
//!   rasterizer here either. Reading its (non-existent) coverage tiles would
//!   report zero everywhere and hide the layer completely, so a vector mask
//!   with no rasterized tiles is **ignored** instead — the layer renders as if
//!   unmasked, including its density and inversion, which is wrong in the
//!   direction that keeps the user's content on screen. Tiles stored under the
//!   mask's id are used whatever the kind says, so a rasterizer filling them in
//!   needs no change here.
//! * **Blend mode on an adjustment layer** is ignored; an adjustment is a
//!   weighted replacement of the backdrop's colour, never an `over`.
//! * **Mip tiles are read, not built.** Asking for level `n` reads the tiles
//!   stored at level `n` (see `editor_core::pixels`, whose deltas address a
//!   caller-chosen level); it does not downsample level 0 on the fly. Building
//!   the chain is `raster::MipChain`'s job.
//! * **Selection** does not mask the composite. A selection scopes *edits*, not
//!   what the document looks like.
//! * **A mask feather is clamped** to the reach whose sampled buffer can still
//!   be allocated (thousands of pixels at level 0, so this is an absurd-value
//!   guard rather than a limit anyone meets). Refusing the frame over a slider
//!   would be the worse answer. The clamp is part of the tile cache key, so a
//!   clamped feather still caches and is still region-independent.
//!
//! What is *not* on this list, because it is bounded rather than approximated:
//! a heavily minified layer. Its pre-image grows with the inverse of its scale,
//! and the compositor samples only what the layer actually stores and splits
//! the destination when even that is too large — see the [`composite`] module
//! docs. The answer is the same as an unbounded implementation's; only the peak
//! allocation differs.

#![forbid(unsafe_code)]

pub mod adjust;
pub mod blending;
pub mod cache;
pub mod canvas;
pub mod composite;
pub mod effects;
pub mod error;
pub mod shape;
pub mod source;
pub mod text;

#[cfg(test)]
mod testkit;
#[cfg(test)]
mod tests;

pub use adjust::{apply_adjustment, PreparedAdjustment};
pub use blending::{blend_atop, blend_over, dissolve_noise, BlendContext, BlendSpace};
pub use cache::{CacheStats, TileCompositor, DEFAULT_CACHE_TILES};
pub use canvas::{Canvas, MAX_CANVAS_PIXELS};
pub use composite::{
    composite_rect, composite_region, composite_subtree, composite_tile, tile_rect,
    CompositeOptions, MAX_PREIMAGE_PIXELS, TAP_WINDOW,
};
pub use effects::MAX_REACH;
pub use error::CompositeError;
pub use shape::MAX_SHAPE_PIXELS;
pub use source::{MemoryTileSource, TileSource};
pub use text::{font_families, load_font, no_fonts};
