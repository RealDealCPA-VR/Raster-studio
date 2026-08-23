//! Every way a vector operation can refuse.
//!
//! The rule this crate follows is the same one the selection engine follows:
//! caller input never panics. Path data comes from files, from the pen tool and
//! from SVG strings, so NaN coordinates, absurd extents and malformed `d`
//! strings are all *reachable*, not hypothetical.
//!
//! # Refusing and degrading are both answers
//! Not every bad input is an error, and pretending otherwise would be worse for
//! an editor than the disease. A malformed `d` string is refused, because there
//! is no sensible partial reading of it and the user needs the offset. A
//! non-finite *coordinate* inside an otherwise good path is **dropped** instead:
//! it has no position, so it has no crossings, no length and no bounds, and
//! refusing the whole path would mean one poisoned control point could make a
//! document unopenable. Which of the two applies is documented on each
//! operation, and both are tested — [`VectorError`] is only ever returned for
//! the first kind.

use thiserror::Error;

/// The reason a vector operation could not produce a result.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum VectorError {
    /// SVG path data could not be parsed.
    #[error("invalid SVG path data at byte {offset}: {reason}")]
    Svg {
        /// Byte offset into the `d` string where parsing stopped.
        offset: usize,
        /// What the parser expected instead.
        reason: String,
    },

    /// A numeric parameter was outside its permitted range.
    #[error("{what} must be {expected}, got {value}")]
    InvalidParameter {
        /// Which parameter.
        what: &'static str,
        /// The constraint it violated, phrased for a message.
        expected: &'static str,
        /// The offending value.
        value: f64,
    },

    /// The requested coverage mask is larger than [`crate::MAX_MASK_SAMPLES`].
    ///
    /// Reachable from a shape whose bounds are enormous: without a cap, a
    /// `u32 * u32` product always fits a 64-bit `usize`, so nothing would be
    /// refused and the allocation would simply abort the process.
    #[error("a {width} x {height} coverage mask exceeds the {max}-sample limit")]
    RegionTooLarge {
        /// Requested width in pixels.
        width: u64,
        /// Requested height in pixels.
        height: u64,
        /// The cap that was exceeded.
        max: u64,
    },

    /// A rasteriser buffer sized by the caller's extent could not be allocated.
    ///
    /// `vec![v; n]` on an unaffordable `n` calls `handle_alloc_error`, which is
    /// an abort no editor can catch, let alone report. The three buffers whose
    /// length is a caller's extent multiplied out — the coverage mask, the
    /// row accumulator it is built from, and [`crate::CoverageMask::trimmed`]'s
    /// copy — are reserved through `try_reserve`, so an unaffordable one is
    /// this error instead of an abort.
    ///
    /// Nothing else in the crate returns it, because nothing else needs to:
    /// the other stages cap the *work* before allocating anything sized by it,
    /// and refuse with [`VectorError::TooComplex`] or
    /// [`VectorError::RegionTooLarge`].
    #[error("could not allocate {bytes} bytes")]
    OutOfMemory {
        /// Bytes that could not be reserved.
        bytes: usize,
    },

    /// A boolean operation's input was too complex to resolve within the
    /// crate's work limit.
    #[error("the operation needed more than {limit} {what}")]
    TooComplex {
        /// What ran out.
        what: &'static str,
        /// The limit that was hit.
        limit: usize,
    },
}
