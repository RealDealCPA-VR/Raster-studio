//! Image filters: blur, sharpen, noise, distort, stylize, pixelate, render.
//!
//! Filters operate on premultiplied linear RGBA buffers produced by the
//! compositor, and are tile-parallel over `rayon`.
//!
//! Two invariants apply to every filter in this crate:
//!
//! - **Edge handling is explicit.** Each filter documents whether it clamps,
//!   wraps or mirrors at the boundary. Left implicit, this shows up as dark or
//!   bright borders.
//! - **Kernels are normalised.** A blur of a constant image returns that same
//!   constant; anything else means repeated application drifts in brightness.
