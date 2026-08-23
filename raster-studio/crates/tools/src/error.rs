//! Everything a tool refuses, in one enum.
//!
//! Tools take raw gesture input, so every failure mode here is reachable from a
//! user's hand: a drag that collapses to zero area, a perspective drag that
//! makes the matrix singular, a flood fill seeded outside the canvas, a
//! marquee across a region too large to allocate. None of them may panic and
//! none of them may write NaN into a pixel — the whole point of routing them
//! through a `Result` is that the document is still editable afterwards.

use editor_core::{CommandError, PixelError};
use raster::TileError;

/// Why a tool could not turn a gesture into an edit.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    /// The gesture needs a layer to write to and there is none.
    #[error("no active layer to edit")]
    NoActiveLayer,

    /// The gesture produced a shape with no area (a zero-length drag, a
    /// zero-radius dab, a crop rect of zero width).
    #[error("the gesture has no area")]
    Degenerate,

    /// A parameter arrived as NaN or an infinity. Reported rather than
    /// clamped, because a silently clamped NaN is how a stroke ends up
    /// painting a plausible-looking wrong thing.
    #[error("{what} must be finite, got {value}")]
    NotFinite { what: &'static str, value: f32 },

    /// The region a tool would have to materialise is bigger than the cap in
    /// [`crate::patch::MAX_PATCH_TILES`]. Refused before allocating, because
    /// `handle_alloc_error` is an abort no editor can catch.
    #[error("operation covers {tiles} tiles, more than the {max} a tool may materialise at once")]
    RegionTooLarge { tiles: u64, max: u64 },

    /// The gesture would write colour into an 8-bit coverage mask.
    ///
    /// A mask stores how much of the layer shows through, not RGBA pixels, so
    /// a clone stamp, a dodge, a red-eye fix or a frequency-split heal has
    /// nothing to mean there. Refused rather than quietly retargeted at the
    /// layer, which would edit something the user was not looking at — and
    /// rather than committed anyway, which stores a four-byte-per-pixel tile in
    /// a one-byte-per-pixel slot that nothing downstream validates.
    ///
    /// This is the *narrow* half of the rule. Painting, filling, gradients,
    /// shapes and the free transform all mean something on coverage and go
    /// through [`crate::CoveragePatch`] instead of landing here.
    #[error("this tool has no meaning on a coverage mask")]
    UnsupportedOnMask,

    /// A seed point (flood fill, wand, clone source) lies outside the region
    /// being read.
    #[error("point ({x}, {y}) lies outside the region being sampled")]
    PointOutside { x: i32, y: i32 },

    /// The command the tool built was refused by the document.
    #[error(transparent)]
    Command(#[from] CommandError),

    #[error(transparent)]
    Pixel(#[from] PixelError),

    #[error(transparent)]
    Tile(#[from] TileError),

    #[error(transparent)]
    Selection(#[from] selection::SelectionOpError),

    #[error(transparent)]
    SelectionMask(#[from] editor_core::SelectionError),

    #[error(transparent)]
    Vector(#[from] vector::VectorError),

    #[error(transparent)]
    Filter(#[from] filters::FilterError),
}

impl ToolError {
    /// `true` when this is the "matrix cannot be inverted" refusal.
    ///
    /// A convenience for callers (and tests) that care about the singular
    /// transform case specifically; the variant itself lives in `editor-core`
    /// because that is where the invertibility rule is enforced.
    pub fn is_not_invertible(&self) -> bool {
        matches!(self, ToolError::Command(CommandError::NotInvertible))
    }

    /// Build the "not invertible" refusal without the caller having to reach
    /// into `editor-core`.
    pub fn not_invertible() -> Self {
        ToolError::Command(CommandError::NotInvertible)
    }
}

/// Refuse a non-finite scalar instead of letting it reach a pixel.
pub(crate) fn finite(what: &'static str, v: f32) -> Result<f32, ToolError> {
    if v.is_finite() {
        Ok(v)
    } else {
        Err(ToolError::NotFinite { what, value: v })
    }
}

/// Refuse a non-finite point.
pub(crate) fn finite_pt(what: &'static str, p: glam::Vec2) -> Result<glam::Vec2, ToolError> {
    finite(what, p.x)?;
    finite(what, p.y)?;
    Ok(p)
}
