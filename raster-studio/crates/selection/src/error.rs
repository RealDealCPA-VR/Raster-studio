//! One error type for every algorithm in this crate.
//!
//! Nothing here panics on caller input. A region too large to hold, a
//! non-finite radius, a seed outside the image and a corrupt mask all come back
//! as a value the caller can handle — an editor must not abort because a user
//! dragged a marquee across a 2-billion-pixel canvas.

/// Why a selection algorithm could not produce a mask.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SelectionOpError {
    /// The mask the result would have to be is not a legal
    /// [`editor_core::SelectionMask`].
    #[error(transparent)]
    Mask(#[from] editor_core::SelectionError),

    /// The region is describable but larger than
    /// [`editor_core::MAX_MASK_SAMPLES`].
    #[error("selection region {width}x{height} is larger than a selection mask may be")]
    RegionTooLarge { width: u32, height: u32 },

    /// The region is legal but this machine could not hold the working buffer.
    /// Reported instead of aborting the process in `handle_alloc_error`.
    #[error("could not allocate a {bytes}-byte working buffer for the selection")]
    OutOfMemory { bytes: usize },

    /// A pixel buffer whose length does not match the extent it claims.
    #[error("image is {width}x{height} but carries {got} bytes (expected {expected})")]
    ImageSizeMismatch {
        width: u32,
        height: u32,
        expected: usize,
        got: usize,
    },

    /// A wand / quick-select seed that is not inside the image.
    #[error("seed point ({x}, {y}) is outside the image")]
    SeedOutside { x: i32, y: i32 },

    /// A geometry or radius parameter that is NaN or infinite.
    #[error("{what} must be finite, got {value}")]
    NotFinite { what: &'static str, value: f32 },

    /// A radius past [`crate::MAX_RADIUS`]. Feather and smooth cost `O(radius)`
    /// per sample and expand grows its result on both axes, so an absurd radius
    /// is a hang rather than a result; [`crate::MAX_RADIUS`] documents the cost
    /// of each operation at the cap.
    #[error("{what} must be at most {max}, got {value}")]
    RadiusTooLarge {
        what: &'static str,
        value: f32,
        max: f32,
    },

    /// A mask whose storage rectangle reaches past [`crate::COORD_LIMIT`], the
    /// coordinate range these algorithms can grow and transform inside without
    /// overflowing.
    #[error("selection at ({x}, {y}) sized {width}x{height} leaves the +/-{limit} working grid")]
    CoordOutOfRange {
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        limit: i32,
    },

    /// A tile grid that could not be flattened into RGBA8.
    #[error("could not read the image: {0}")]
    Image(String),
}
