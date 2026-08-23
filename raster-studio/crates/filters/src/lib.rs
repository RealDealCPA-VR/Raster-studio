//! Image filters: blur, sharpen, noise, distort, stylize, pixelate, render.
//!
//! Filters operate on premultiplied linear RGBA buffers produced by the
//! compositor, and are tile-parallel over `rayon`.
//!
//! # The four invariants
//!
//! * **Linear light.** [`FilterBuffer`] holds scene-referred linear sRGB, so
//!   averaging pixels averages *light*. Exactly three filters leave linear
//!   space, because they are defined on gamma-encoded values, and each says so
//!   in its own documentation: [`stylize::solarize`],
//!   [`pixelate::color_halftone`], and the binning fallback inside
//!   [`noise::median`].
//! * **Premultiplied alpha.** A weighted average of premultiplied pixels is
//!   the correct composite of what they cover. Filters that are *not* linear
//!   in the pixel value — noise, the rank filters, solarize, difference clouds
//!   — unpremultiply, operate, and premultiply back, and say so.
//! * **Edge handling is explicit.** [`EdgeMode`] is the only way any filter
//!   reads outside the buffer, and every filter documents which mode it uses
//!   or accepts. There is deliberately no "transparent outside" mode: that is
//!   the compositor's decision, and baking it in here darkens every border.
//! * **Kernels are normalised.** A blur of a constant image returns that same
//!   constant, exactly, under every edge mode and at every size down to 1x1.
//!   Anything else means repeated application drifts in brightness. Every blur
//!   in [`blur`] is covered by that test.
//!
//! # Determinism
//!
//! Every random-looking filter is a pure function of its seed and the
//! destination coordinate — see [`rng`]. That is a correctness requirement,
//! not a convenience: the filters run in parallel over tile bands, so a
//! sequential random *stream* would give a different image depending on how
//! rayon happened to schedule the work.
//!
//! # Robustness
//!
//! No filter panics on a 1x1 image, a zero radius, an empty buffer, or a
//! non-finite parameter. Radii and sample counts are clamped to documented
//! maxima, so a runaway parameter cannot allocate or iterate without limit.
//!
//! "Bounded" means bounded, not cheap. The separable, sliding-window and
//! histogram filters are `O(1)` or `O(r)` per pixel and stay fast at the
//! maxima. The genuinely two-dimensional ones — [`blur::lens_blur`] and
//! [`blur::surface_blur`] — are `O(r^2)` per pixel by construction, so at
//! [`blur::MAX_BLUR_RADIUS`] a single call is millions of taps per pixel and
//! will take a very long time on a large image; `lens_blur` also allocates its
//! iris offsets, tens of megabytes at that radius. The clamp is there to stop
//! unbounded growth, not to make the maximum interactive. Callers driving
//! these from a UI should cap the radius well below the crate maximum.
//! [`stylize::oil_paint`] is `O(r^2)` too, but its own
//! [`stylize::MAX_STYLIZE_RADIUS`] of 64 keeps that in hand.
//!
//! ```
//! use filters::{blur, EdgeMode, FilterBuffer};
//!
//! // A flat field survives a blur untouched — normalisation and edge
//! // handling in one assertion.
//! let flat = FilterBuffer::filled(64, 64, [0.2, 0.3, 0.4, 1.0]).unwrap();
//! let blurred = blur::gaussian_blur(&flat, 4.0, EdgeMode::Clamp);
//! for px in blurred.pixels() {
//!     assert!((px[0] - 0.2).abs() < 1e-5);
//! }
//! ```

#![forbid(unsafe_code)]

pub mod blur;
pub mod buffer;
pub mod distort;
pub mod noise;
pub mod other;
pub mod pixelate;
pub mod render;
pub mod rng;
pub mod sharpen;
pub mod stylize;
pub mod support;

pub use buffer::{FilterBuffer, FilterError};
pub use support::{EdgeMode, Interpolation, Sampling};

pub use blur::{
    box_blur, gaussian_blur, lens_blur, motion_blur, radial_blur, surface_blur, RadialBlur,
    RadialBlurKind,
};
pub use distort::{
    pinch, polar_coordinates, ripple, shear, spherize, twirl, wave, zigzag, PolarMode, Wave,
    WaveKind, ZigZag, ZigZagKind,
};
pub use noise::{
    add_noise, despeckle, dust_and_scratches, median, reduce_noise, NoiseDistribution,
};
pub use other::{convolve, high_pass, maximum, minimum, offset, Kernel};
pub use pixelate::{color_halftone, crystallize, mosaic, pointillize};
pub use render::{
    clouds, difference_clouds, fibers, gradient_fill, lens_flare, CloudParams, FiberParams,
    Gradient, GradientKind, GradientStop, LensFlare,
};
pub use rng::{Perlin, Rng};
pub use sharpen::{smart_sharpen, unsharp_mask};
pub use stylize::{
    diffuse, emboss, find_edges, oil_paint, solarize, wind, DiffuseMode, WindDirection,
};
