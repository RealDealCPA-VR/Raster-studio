//! Rejections raised by the compositor.

use layer_model::LayerId;

/// Something the compositor refuses to do, always *before* it allocates or
/// mutates anything.
///
/// Every variant is a caller mistake or a resource ceiling. Missing pixel data
/// is deliberately **not** an error: an absent tile reads as fully transparent
/// (for a layer) or as zero coverage (for a mask), exactly as `raster` and
/// `editor-core` document, so a partially-resident tile store composites what
/// it has instead of failing the frame.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CompositeError {
    /// The requested region — or an intermediate buffer it forced, such as the
    /// pre-image of a transformed layer — is larger than
    /// [`crate::MAX_CANVAS_PIXELS`].
    #[error("a composite of {pixels} pixels exceeds the {max}-pixel ceiling")]
    RegionTooLarge { pixels: u64, max: u64 },
    /// A pixel buffer handed to [`crate::Canvas::from_pixels`] does not match
    /// the rect it claims to cover.
    #[error("canvas needs {expected} pixels for its rect, got {got}")]
    PixelCountMismatch { expected: usize, got: usize },
    /// [`crate::composite_subtree`] was given an id the document does not hold.
    #[error("layer {0} is not in this document")]
    LayerNotFound(LayerId),
    /// The requested mip level is past the end of the document's mip chain.
    /// Level `n` exists only while `n < raster::mipmap::level_count(w, h)`.
    #[error("mip level {level} does not exist for a {width}x{height} document")]
    NoSuchLevel { level: u8, width: u32, height: u32 },
}
