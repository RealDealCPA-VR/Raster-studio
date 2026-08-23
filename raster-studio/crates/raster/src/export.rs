//! The export pipeline: linear premultiplied float in, an encoded file out.
//!
//! # Why this module exists
//!
//! The compositor's output is **linear, premultiplied** `f32` RGBA. A PNG on
//! disk holds **display-encoded, straight-alpha** integers. Those are two
//! different numbers for the same colour, and handing the first to
//! [`crate::codec::encode`] writes a file whose midtones are roughly twice as
//! bright as they should be and whose antialiased edges are dark-fringed.
//! There is nowhere else in the pipeline this conversion can happen, because
//! the codec facade deliberately knows nothing about colour and the compositor
//! deliberately knows nothing about files.
//!
//! The order of operations is fixed and matters:
//!
//! ```text
//! linear premultiplied f32
//!   → resample                 (filtered; premultiplied, so edges do not fringe)
//!   → flatten onto background  (containers with no alpha channel, and the
//!                               partially transparent pixels of a container
//!                               with only a transparent *index*, i.e. GIF)
//!   → un-premultiply           (colour.rs' shared epsilon, not an ad-hoc one)
//!   → linear → display encode  (color::from_linear, so the space is a parameter)
//!   → quantize to 8 or 16 bit
//!   → codec::encode_with
//! ```
//!
//! Resampling before encoding, and in premultiplied space, is what stops a
//! downscale from darkening and from dragging the colour of fully transparent
//! pixels into their opaque neighbours.
//!
//! # Colour spaces this build cannot transform
//!
//! [`ColorSpace::IccProfile`] has no implemented transform — no ICC engine is
//! linked — and `color`'s infallible [`color::to_linear`] / [`color::from_linear`]
//! return such a triple *unchanged*. Since import now produces that variant for
//! every file carrying a profile, using the infallible entry points here would
//! quietly label gamma-encoded samples "linear" and then re-encode them through
//! an sRGB preset, brightening every midtone. Every conversion in this module
//! therefore goes through [`color::try_to_linear`] / [`color::try_from_linear`]
//! and reports [`ExportError::Color`] instead.
//!
//! An ICC-tagged document is still exportable: [`ColorHandling::PassThrough`]
//! writes the file's own samples back out untouched, with the profile
//! re-embedded. That path is explicit, is rejected for any space that *does*
//! have a transform, and is the only way an unsupported space can reach an
//! encoder.
//!
//! The pass-through buffer is the one place gamma-encoded samples legitimately
//! sit inside a [`LinearImage`], so the buffer records that fact and
//! [`export`] refuses to pair it with a converting preset — otherwise the
//! double-encode above would simply have moved one call earlier. See
//! [`ExportError::ColorHandlingMismatch`].
//!
//! An embedded profile is likewise tied to the space actually written: one
//! [`ExportMetadata`] is offered to every preset in a batch, and a profile that
//! does not describe a given preset's [`ExportPreset::color_space`] is refused
//! rather than attached to a file it mislabels.
//!
//! # Untrusted input
//!
//! [`ExportPreset::name`] can come from a project file written by someone else.
//! [`ExportPreset::file_name`] therefore runs it through
//! [`sanitize_file_stem`], which can never produce a path separator, a `..`, a
//! leading dot, or a reserved device name — so a preset called
//! `../../../.ssh/authorized_keys` yields an inert file *name*, and joining it
//! onto an output directory stays inside that directory.

use std::path::{Path, PathBuf};

use color::{
    from_linear, premultiply, to_linear, try_from_linear, try_to_linear, unpremultiply, ColorSpace,
    UnsupportedColorSpace,
};

use crate::codec::{
    encode_to_path, encode_with, AlphaSupport, CodecError, EncodeOptions, EncodedPixels,
    ExportFormat,
};

/// Largest destination image the resampler will produce, in pixels.
///
/// A scale factor is a user-supplied number; multiplying an 8K canvas by an
/// unchecked float is how an export dialog turns into an out-of-memory abort.
/// The same ceiling bounds the resampler's *intermediate* buffer, which is a
/// different product entirely — see [`resample`].
pub const MAX_OUTPUT_PIXELS: u64 = 1 << 28;

/// Largest scale factor a preset may ask for.
pub const MAX_SCALE: f32 = 64.0;

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("codec: {0}")]
    Codec(#[from] CodecError),
    #[error("invalid image buffer: {0}")]
    Buffer(String),
    #[error("invalid preset '{name}': {reason}")]
    Preset { name: String, reason: String },
    #[error("output would be {0}")]
    TooLarge(String),
    #[error("two presets both export to '{0}'")]
    DuplicateOutput(String),
    /// The buffer's provenance and the preset's [`ColorHandling`] disagree.
    ///
    /// Separate from [`ExportError::Preset`] because the preset alone is not
    /// wrong: it is wrong *for this buffer*, and the fix is to pair them
    /// correctly rather than to change either one in isolation.
    #[error("preset '{preset}' cannot write this buffer: {reason}")]
    ColorHandlingMismatch { preset: String, reason: String },
    /// A conversion was asked for between the working space and a space this
    /// build has no transform for. Never an identity pass: an ICC-tagged
    /// document that reached an sRGB preset is a bug, not a colour.
    #[error("colour management: {0}")]
    Color(#[from] UnsupportedColorSpace),
}

/// An image in the compositor's working representation: linear, premultiplied
/// `f32` RGBA, row-major.
///
/// Values are deliberately unclamped — the working space is scene-referred, so
/// a highlight above `1.0` is meaningful right up until it is quantized.
///
/// The one exception to "linear" is a buffer produced by a `*_pass_through`
/// function, which is premultiplied but still carries the file's own encoding.
/// That is not a documentation convention the caller has to honour: the buffer
/// *records* which kind it is, [`resample`] and the flatten preserve the
/// record, and [`export`] refuses a buffer whose record disagrees with the
/// preset's [`ColorHandling`] — see [`LinearImage::is_pass_through`]. A
/// gamma-encoded buffer handed to an sRGB preset is
/// [`ExportError::ColorHandlingMismatch`], not a file with every midtone
/// brightened.
#[derive(Debug, Clone, PartialEq)]
pub struct LinearImage {
    width: u32,
    height: u32,
    pixels: Vec<f32>,
    /// `true` when the samples are the *file's* encoded values rather than
    /// linear ones. Set only by the `*_pass_through` constructors.
    pass_through: bool,
}

impl LinearImage {
    /// Wrap a premultiplied linear buffer, checking it against the dimensions.
    pub fn from_premultiplied(
        width: u32,
        height: u32,
        pixels: Vec<f32>,
    ) -> Result<Self, ExportError> {
        Self::build(width, height, pixels, false)
    }

    /// The shared constructor. `pass_through` is deliberately not a public
    /// parameter: a caller who could set it could re-label a linear buffer as
    /// gamma-encoded and reintroduce exactly the mismatch this field prevents.
    fn build(
        width: u32,
        height: u32,
        pixels: Vec<f32>,
        pass_through: bool,
    ) -> Result<Self, ExportError> {
        if width == 0 || height == 0 {
            return Err(ExportError::Buffer(format!(
                "a {width}x{height} image has no pixels"
            )));
        }
        // Saturating: `u32::MAX * u32::MAX * 4` does not fit in a `u64`, and a
        // debug build would panic on the overflow instead of rejecting the
        // dimensions.
        let expected = u64::from(width)
            .saturating_mul(u64::from(height))
            .saturating_mul(4);
        if pixels.len() as u64 != expected {
            return Err(ExportError::Buffer(format!(
                "{width}x{height} needs {expected} samples, got {}",
                pixels.len()
            )));
        }
        Ok(LinearImage {
            width,
            height,
            pixels,
            pass_through,
        })
    }

    /// Wrap a *straight*-alpha linear buffer, premultiplying it on the way in.
    pub fn from_straight(
        width: u32,
        height: u32,
        mut pixels: Vec<f32>,
    ) -> Result<Self, ExportError> {
        for px in pixels.chunks_exact_mut(4) {
            let out = premultiply([px[0], px[1], px[2], px[3]]);
            px.copy_from_slice(&out);
        }
        Self::from_premultiplied(width, height, pixels)
    }

    /// Whether these samples are the file's own encoded values rather than
    /// linear ones.
    ///
    /// `true` only for a buffer that came from
    /// [`linear_from_rgba8_pass_through`],
    /// [`linear_from_rgba16_pass_through`], or
    /// [`crate::codec::DecodedSurface::to_premultiplied_pass_through`], or from
    /// resampling or flattening one. Such a buffer can only be written through
    /// [`ColorHandling::PassThrough`]; every other buffer can only be written
    /// through [`ColorHandling::Convert`].
    pub fn is_pass_through(&self) -> bool {
        self.pass_through
    }

    /// A fully transparent image.
    pub fn transparent(width: u32, height: u32) -> Result<Self, ExportError> {
        let pixels = u64::from(width).saturating_mul(u64::from(height));
        if pixels > MAX_OUTPUT_PIXELS {
            return Err(ExportError::TooLarge(format!(
                "{width}x{height} = {pixels} pixels, limit is {MAX_OUTPUT_PIXELS}"
            )));
        }
        Self::from_premultiplied(width, height, vec![0.0; (pixels * 4) as usize])
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// The premultiplied linear samples, four per pixel, row-major.
    pub fn pixels(&self) -> &[f32] {
        &self.pixels
    }

    /// Consume the image, returning its samples.
    pub fn into_pixels(self) -> Vec<f32> {
        self.pixels
    }

    /// One pixel's four samples. Test-only: the pipeline works on whole
    /// buffers, and a per-pixel accessor in the hot loops would cost a bounds
    /// check per sample.
    #[cfg(test)]
    fn pixel(&self, x: usize, y: usize) -> [f32; 4] {
        let i = (y * self.width as usize + x) * 4;
        [
            self.pixels[i],
            self.pixels[i + 1],
            self.pixels[i + 2],
            self.pixels[i + 3],
        ]
    }
}

// ---------------------------------------------------------------- colour

/// How the exporter bridges the working buffer and the file's samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ColorHandling {
    /// Convert working-space pixels into [`ExportPreset::color_space`], and
    /// back on the way in. Requires that space to have an implemented
    /// transform.
    #[default]
    Convert,
    /// Apply no transfer-function conversion at all: the samples in the working
    /// buffer *are* the samples the file stores.
    ///
    /// This is how a document whose space has no implemented transform (an
    /// embedded ICC profile) is edited and written back without anything being
    /// invented. It is rejected for a space that *does* have a transform, where
    /// it could only mean skipping a conversion the exporter knows how to do.
    PassThrough,
}

/// A resolved conversion between the working space and one file space.
///
/// Resolving once is what keeps the per-pixel loop free of a per-pixel
/// `Result`: [`color::try_to_linear`] and [`color::try_from_linear`] are total
/// in their colour argument and fail only on the *space*, so a single probe
/// decides the whole image.
#[derive(Clone, Copy)]
enum Transform<'a> {
    Managed(&'a ColorSpace),
    PassThrough,
}

impl<'a> Transform<'a> {
    /// The managed transform for `space`, or [`ExportError::Color`] if this
    /// build has none.
    fn managed(space: &'a ColorSpace) -> Result<Self, ExportError> {
        try_to_linear(space, [0.0, 0.0, 0.0])?;
        try_from_linear(space, [0.0, 0.0, 0.0])?;
        Ok(Transform::Managed(space))
    }

    fn resolve(space: &'a ColorSpace, handling: ColorHandling) -> Result<Self, ExportError> {
        match handling {
            ColorHandling::Convert => Transform::managed(space),
            ColorHandling::PassThrough => Ok(Transform::PassThrough),
        }
    }

    /// File encoding to working space.
    #[inline]
    fn decode(self, rgb: [f32; 3]) -> [f32; 3] {
        match self {
            Transform::Managed(space) => to_linear(space, rgb),
            Transform::PassThrough => rgb,
        }
    }

    /// Working space to file encoding.
    #[inline]
    fn encode(self, rgb: [f32; 3]) -> [f32; 3] {
        match self {
            Transform::Managed(space) => from_linear(space, rgb),
            Transform::PassThrough => rgb,
        }
    }

    /// Whether a buffer produced by this transform holds the file's own
    /// encoded samples rather than linear ones.
    fn is_pass_through(self) -> bool {
        matches!(self, Transform::PassThrough)
    }
}

fn to_rgba8_with(image: &LinearImage, transform: Transform<'_>) -> Vec<u8> {
    let mut out = Vec::with_capacity(image.pixels.len());
    for px in image.pixels.chunks_exact(4) {
        let straight = unpremultiply([px[0], px[1], px[2], px[3]]);
        let encoded = transform.encode([straight[0], straight[1], straight[2]]);
        out.push(quantize8(encoded[0]));
        out.push(quantize8(encoded[1]));
        out.push(quantize8(encoded[2]));
        out.push(quantize8(straight[3]));
    }
    out
}

fn to_rgba16_with(image: &LinearImage, transform: Transform<'_>) -> Vec<u16> {
    let mut out = Vec::with_capacity(image.pixels.len());
    for px in image.pixels.chunks_exact(4) {
        let straight = unpremultiply([px[0], px[1], px[2], px[3]]);
        let encoded = transform.encode([straight[0], straight[1], straight[2]]);
        out.push(quantize16(encoded[0]));
        out.push(quantize16(encoded[1]));
        out.push(quantize16(encoded[2]));
        out.push(quantize16(straight[3]));
    }
    out
}

fn from_rgba8_with(
    width: u32,
    height: u32,
    rgba8: &[u8],
    transform: Transform<'_>,
) -> Result<LinearImage, ExportError> {
    let mut pixels = Vec::with_capacity(rgba8.len());
    for px in rgba8.chunks_exact(4) {
        let encoded = [
            f32::from(px[0]) / 255.0,
            f32::from(px[1]) / 255.0,
            f32::from(px[2]) / 255.0,
        ];
        let decoded = transform.decode(encoded);
        let alpha = f32::from(px[3]) / 255.0;
        pixels.extend_from_slice(&premultiply([decoded[0], decoded[1], decoded[2], alpha]));
    }
    LinearImage::build(width, height, pixels, transform.is_pass_through())
}

fn from_rgba16_with(
    width: u32,
    height: u32,
    rgba16: &[u16],
    transform: Transform<'_>,
) -> Result<LinearImage, ExportError> {
    let mut pixels = Vec::with_capacity(rgba16.len());
    for px in rgba16.chunks_exact(4) {
        let encoded = [
            f32::from(px[0]) / 65_535.0,
            f32::from(px[1]) / 65_535.0,
            f32::from(px[2]) / 65_535.0,
        ];
        let decoded = transform.decode(encoded);
        let alpha = f32::from(px[3]) / 65_535.0;
        pixels.extend_from_slice(&premultiply([decoded[0], decoded[1], decoded[2], alpha]));
    }
    LinearImage::build(width, height, pixels, transform.is_pass_through())
}

/// Straight-alpha display-encoded RGBA8 from linear premultiplied working
/// pixels.
///
/// This is the step whose absence silently ruins every export: without it the
/// premultiplied linear numbers are written to the file verbatim.
///
/// Fails with [`ExportError::Color`] when `space` has no implemented transform,
/// rather than writing the working numbers out as if they were already encoded.
pub fn rgba8_from_linear(image: &LinearImage, space: &ColorSpace) -> Result<Vec<u8>, ExportError> {
    Ok(to_rgba8_with(image, Transform::managed(space)?))
}

/// As [`rgba8_from_linear`], at 16 bits per channel.
pub fn rgba16_from_linear(
    image: &LinearImage,
    space: &ColorSpace,
) -> Result<Vec<u16>, ExportError> {
    Ok(to_rgba16_with(image, Transform::managed(space)?))
}

/// [`rgba8_from_linear`] with no colour conversion: the buffer's samples are
/// already in the file's encoding.
///
/// Only correct for a buffer that came in through
/// [`linear_from_rgba8_pass_through`] or
/// [`crate::codec::DecodedSurface::to_premultiplied_pass_through`], and only
/// when the file being written declares the very same space.
pub fn rgba8_from_linear_pass_through(image: &LinearImage) -> Vec<u8> {
    to_rgba8_with(image, Transform::PassThrough)
}

/// [`rgba16_from_linear`] with no colour conversion. See
/// [`rgba8_from_linear_pass_through`].
pub fn rgba16_from_linear_pass_through(image: &LinearImage) -> Vec<u16> {
    to_rgba16_with(image, Transform::PassThrough)
}

/// The decode-side inverse of [`rgba8_from_linear`].
pub fn linear_from_rgba8(
    width: u32,
    height: u32,
    rgba8: &[u8],
    space: &ColorSpace,
) -> Result<LinearImage, ExportError> {
    from_rgba8_with(width, height, rgba8, Transform::managed(space)?)
}

/// The decode-side inverse of [`rgba16_from_linear`].
pub fn linear_from_rgba16(
    width: u32,
    height: u32,
    rgba16: &[u16],
    space: &ColorSpace,
) -> Result<LinearImage, ExportError> {
    from_rgba16_with(width, height, rgba16, Transform::managed(space)?)
}

/// Premultiply file samples without converting them.
///
/// The result is a [`LinearImage`] by type only: its samples are premultiplied
/// but still carry the file's own transfer function. It may be resampled and
/// flattened — both are linear operations on whatever the samples are — but it
/// can only be written back out through [`ColorHandling::PassThrough`] into the
/// same space it came from. That is enforced, not merely documented: the buffer
/// reports [`LinearImage::is_pass_through`] and [`export`] rejects the mismatch
/// with [`ExportError::ColorHandlingMismatch`].
pub fn linear_from_rgba8_pass_through(
    width: u32,
    height: u32,
    rgba8: &[u8],
) -> Result<LinearImage, ExportError> {
    from_rgba8_with(width, height, rgba8, Transform::PassThrough)
}

/// [`linear_from_rgba8_pass_through`] at 16 bits per channel.
pub fn linear_from_rgba16_pass_through(
    width: u32,
    height: u32,
    rgba16: &[u16],
) -> Result<LinearImage, ExportError> {
    from_rgba16_with(width, height, rgba16, Transform::PassThrough)
}

/// Which pixels a flatten touches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FlattenMode {
    /// Every pixel becomes opaque. For containers with no alpha at all (JPEG).
    All,
    /// Partially transparent pixels are composited and become opaque; fully
    /// transparent pixels are left transparent. For containers that have a
    /// single transparent palette entry and no alpha channel (GIF).
    PartialOnly,
}

fn flatten_with(
    image: &LinearImage,
    background: [u8; 3],
    transform: Transform<'_>,
    mode: FlattenMode,
) -> LinearImage {
    let bg = transform.decode([
        f32::from(background[0]) / 255.0,
        f32::from(background[1]) / 255.0,
        f32::from(background[2]) / 255.0,
    ]);
    let mut pixels = Vec::with_capacity(image.pixels.len());
    for px in image.pixels.chunks_exact(4) {
        if mode == FlattenMode::PartialOnly && px[3] <= 0.0 {
            // A fully transparent pixel keeps the container's transparent
            // index rather than becoming an opaque patch of background.
            pixels.extend_from_slice(&[0.0, 0.0, 0.0, 0.0]);
            continue;
        }
        // `src` is already premultiplied, so source-over is src + bg*(1-a).
        let inv = 1.0 - px[3];
        pixels.push(px[0] + bg[0] * inv);
        pixels.push(px[1] + bg[1] * inv);
        pixels.push(px[2] + bg[2] * inv);
        pixels.push(1.0);
    }
    LinearImage {
        width: image.width,
        height: image.height,
        pixels,
        // Compositing does not change what the samples mean, so a
        // gamma-encoded buffer stays gamma-encoded and still cannot be handed
        // to a converting preset.
        pass_through: image.pass_through,
    }
}

/// Composite premultiplied working pixels over an opaque background.
///
/// `background` is a display-encoded triple in `space`, because that is what a
/// colour picker hands you; it is decoded here so the composite happens in the
/// same space as everything else.
///
/// Fails with [`ExportError::Color`] when `space` has no implemented transform.
pub fn flatten_onto(
    image: &LinearImage,
    background: [u8; 3],
    space: &ColorSpace,
    mode: FlattenMode,
) -> Result<LinearImage, ExportError> {
    Ok(flatten_with(
        image,
        background,
        Transform::managed(space)?,
        mode,
    ))
}

#[inline]
fn quantize8(v: f32) -> u8 {
    // `as` saturates, and NaN casts to 0; the clamp makes the intent explicit
    // rather than relying on that.
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

#[inline]
fn quantize16(v: f32) -> u16 {
    (v.clamp(0.0, 1.0) * 65_535.0 + 0.5) as u16
}

// -------------------------------------------------------------- resampling

/// Reconstruction filter used when a preset scales the image.
///
/// [`ResampleFilter::Nearest`] is included for completeness and for pixel-art
/// workflows; it is never the default, because on a downscale it point-samples
/// and aliases. Every other filter widens its kernel by the downscale factor,
/// which is what actually removes the frequencies the smaller grid cannot
/// represent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResampleFilter {
    /// Point sampling. Aliases on downscale. Pixel art only.
    Nearest,
    /// Tent filter. Cheap, soft.
    Triangle,
    /// Mitchell-Netravali with `B = C = 1/3`: the standard compromise between
    /// blur and ringing.
    Mitchell,
    /// Three-lobe Lanczos. Sharpest of the four, with mild ringing.
    Lanczos3,
}

impl ResampleFilter {
    /// Kernel support radius in destination-independent units.
    fn radius(self) -> f32 {
        match self {
            ResampleFilter::Nearest => 0.5,
            ResampleFilter::Triangle => 1.0,
            ResampleFilter::Mitchell => 2.0,
            ResampleFilter::Lanczos3 => 3.0,
        }
    }

    /// Whether the kernel widens on downscale (i.e. actually low-passes).
    fn widens(self) -> bool {
        !matches!(self, ResampleFilter::Nearest)
    }

    fn weight(self, x: f32) -> f32 {
        match self {
            ResampleFilter::Nearest => {
                if (-0.5..0.5).contains(&x) {
                    1.0
                } else {
                    0.0
                }
            }
            ResampleFilter::Triangle => {
                let a = x.abs();
                if a < 1.0 {
                    1.0 - a
                } else {
                    0.0
                }
            }
            ResampleFilter::Mitchell => mitchell(x, 1.0 / 3.0, 1.0 / 3.0),
            ResampleFilter::Lanczos3 => lanczos(x, 3.0),
        }
    }
}

fn mitchell(x: f32, b: f32, c: f32) -> f32 {
    let a = x.abs();
    let a2 = a * a;
    let a3 = a2 * a;
    if a < 1.0 {
        ((12.0 - 9.0 * b - 6.0 * c) * a3 + (-18.0 + 12.0 * b + 6.0 * c) * a2 + (6.0 - 2.0 * b))
            / 6.0
    } else if a < 2.0 {
        ((-b - 6.0 * c) * a3
            + (6.0 * b + 30.0 * c) * a2
            + (-12.0 * b - 48.0 * c) * a
            + (8.0 * b + 24.0 * c))
            / 6.0
    } else {
        0.0
    }
}

fn sinc(x: f32) -> f32 {
    if x.abs() < 1e-6 {
        1.0
    } else {
        let px = std::f32::consts::PI * x;
        px.sin() / px
    }
}

fn lanczos(x: f32, lobes: f32) -> f32 {
    if x.abs() >= lobes {
        0.0
    } else {
        sinc(x) * sinc(x / lobes)
    }
}

/// One destination sample's worth of source taps.
struct Taps {
    /// Index of the first source sample; may be negative before clamping.
    first: i64,
    weights: Vec<f32>,
}

/// Build the tap list for every destination index along one axis.
fn axis_taps(src: usize, dst: usize, filter: ResampleFilter) -> Vec<Taps> {
    let ratio = dst as f64 / src as f64;
    let filter_scale = if filter.widens() && ratio < 1.0 {
        1.0 / ratio
    } else {
        1.0
    };
    let support = f64::from(filter.radius()) * filter_scale;

    let mut out = Vec::with_capacity(dst);
    for i in 0..dst {
        // Destination sample `i` covers [i, i+1) and is centred at i+0.5;
        // mapping that centre back into source coordinates is what keeps the
        // result phase-correct rather than half a pixel off.
        let center = (i as f64 + 0.5) / ratio;
        let first = (center - support - 0.5).ceil() as i64;
        let last = (center + support - 0.5).floor() as i64;
        let mut weights = Vec::with_capacity((last - first + 1).max(1) as usize);
        let mut total = 0.0f32;
        for j in first..=last {
            let d = (j as f64 + 0.5 - center) / filter_scale;
            let w = filter.weight(d as f32);
            total += w;
            weights.push(w);
        }
        if total.abs() < 1e-9 {
            // Degenerate kernel (only possible for Nearest at an exact
            // boundary): fall back to the single nearest sample.
            let j = center.floor() as i64;
            out.push(Taps {
                first: j,
                weights: vec![1.0],
            });
            continue;
        }
        for w in &mut weights {
            *w /= total;
        }
        out.push(Taps { first, weights });
    }
    out
}

#[inline]
fn clamp_index(j: i64, len: usize) -> usize {
    j.clamp(0, len as i64 - 1) as usize
}

/// Which axis a separable resample filters first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PassOrder {
    /// `sw x sh -> dw x sh -> dw x dh`; the intermediate is `dw * sh`.
    HorizontalFirst,
    /// `sw x sh -> sw x dh -> dw x dh`; the intermediate is `sw * dh`.
    VerticalFirst,
}

/// Choose the pass order with the smaller intermediate buffer, and bound it.
///
/// The destination bound alone does **not** bound the intermediate: filtering
/// horizontally first allocates `dw * sh` pixels, a product of a destination
/// dimension and a *source* one. A 2x4096 source scaled to 65536x1 has a
/// 65 536-pixel destination and a 268-megapixel horizontal intermediate — four
/// orders of magnitude apart. Filtering vertically first makes that same job a
/// two-pixel intermediate, so the order is chosen rather than fixed, and
/// whichever is chosen is checked against [`MAX_OUTPUT_PIXELS`].
fn plan_passes(sw: u64, sh: u64, dw: u64, dh: u64) -> Result<PassOrder, ExportError> {
    let horizontal_first = dw.saturating_mul(sh);
    let vertical_first = sw.saturating_mul(dh);
    let (order, pixels) = if horizontal_first <= vertical_first {
        (PassOrder::HorizontalFirst, horizontal_first)
    } else {
        (PassOrder::VerticalFirst, vertical_first)
    };
    if pixels > MAX_OUTPUT_PIXELS {
        return Err(ExportError::TooLarge(format!(
            "resampling {sw}x{sh} to {dw}x{dh} needs a {pixels}-pixel intermediate, \
             limit is {MAX_OUTPUT_PIXELS}"
        )));
    }
    Ok(order)
}

/// Filter along x: `sw x rows` in, `dw x rows` out.
fn pass_x(src: &[f32], sw: usize, rows: usize, dw: usize, taps: &[Taps]) -> Vec<f32> {
    let mut out = vec![0.0f32; dw * rows * 4];
    for y in 0..rows {
        let row = y * sw * 4;
        for (x, tap) in taps.iter().enumerate() {
            let mut acc = [0.0f32; 4];
            for (k, &w) in tap.weights.iter().enumerate() {
                if w == 0.0 {
                    continue;
                }
                let sx = clamp_index(tap.first + k as i64, sw);
                let i = row + sx * 4;
                for c in 0..4 {
                    acc[c] += src[i + c] * w;
                }
            }
            let o = (y * dw + x) * 4;
            out[o..o + 4].copy_from_slice(&acc);
        }
    }
    out
}

/// Filter along y: `cols x sh` in, `cols x dh` out.
fn pass_y(src: &[f32], cols: usize, sh: usize, dh: usize, taps: &[Taps]) -> Vec<f32> {
    let mut out = vec![0.0f32; cols * dh * 4];
    for (y, tap) in taps.iter().enumerate() {
        for x in 0..cols {
            let mut acc = [0.0f32; 4];
            for (k, &w) in tap.weights.iter().enumerate() {
                if w == 0.0 {
                    continue;
                }
                let sy = clamp_index(tap.first + k as i64, sh);
                let i = (sy * cols + x) * 4;
                for c in 0..4 {
                    acc[c] += src[i + c] * w;
                }
            }
            let o = (y * cols + x) * 4;
            out[o..o + 4].copy_from_slice(&acc);
        }
    }
    out
}

/// Resample in linear premultiplied space with a real reconstruction filter.
///
/// Separable: one horizontal pass and one vertical pass, `O(w*h*(kx+ky))`
/// rather than `O(w*h*kx*ky)`. Out-of-range taps clamp to the edge sample, so
/// borders neither darken nor wrap. Both the destination and the intermediate
/// buffer are bounded by [`MAX_OUTPUT_PIXELS`]; see [`plan_passes`] for why
/// those are two different products.
pub fn resample(
    image: &LinearImage,
    dst_width: u32,
    dst_height: u32,
    filter: ResampleFilter,
) -> Result<LinearImage, ExportError> {
    if dst_width == 0 || dst_height == 0 {
        return Err(ExportError::Buffer(format!(
            "cannot resample to {dst_width}x{dst_height}"
        )));
    }
    let out_pixels = u64::from(dst_width) * u64::from(dst_height);
    if out_pixels > MAX_OUTPUT_PIXELS {
        return Err(ExportError::TooLarge(format!(
            "{dst_width}x{dst_height} = {out_pixels} pixels, limit is {MAX_OUTPUT_PIXELS}"
        )));
    }
    if dst_width == image.width && dst_height == image.height {
        return Ok(image.clone());
    }

    let order = plan_passes(
        u64::from(image.width),
        u64::from(image.height),
        u64::from(dst_width),
        u64::from(dst_height),
    )?;

    let (sw, sh) = (image.width as usize, image.height as usize);
    let (dw, dh) = (dst_width as usize, dst_height as usize);
    let xt = axis_taps(sw, dw, filter);
    let yt = axis_taps(sh, dh, filter);

    // The two orders are mathematically identical (a separable filter
    // commutes); only the intermediate's size differs.
    let out = match order {
        PassOrder::HorizontalFirst => {
            let mid = pass_x(&image.pixels, sw, sh, dw, &xt);
            pass_y(&mid, dw, sh, dh, &yt)
        }
        PassOrder::VerticalFirst => {
            let mid = pass_y(&image.pixels, sw, sh, dh, &yt);
            pass_x(&mid, sw, dh, dw, &xt)
        }
    };

    Ok(LinearImage {
        width: dst_width,
        height: dst_height,
        pixels: out,
        // Filtering does not change what the samples mean either.
        pass_through: image.pass_through,
    })
}

// ----------------------------------------------------------------- presets

/// Bits per channel in the written file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BitDepth {
    #[default]
    Eight,
    Sixteen,
}

/// One named export configuration.
///
/// A preset is the whole recipe: what container, how compressed, how big, how
/// resampled, whether metadata rides along, and what shows through where the
/// image is transparent but the container has no alpha.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportPreset {
    /// Human name; also the basis for [`ExportPreset::file_name`].
    pub name: String,
    pub format: ExportFormat,
    pub bit_depth: BitDepth,
    /// Multiplier applied to both axes. `1.0` writes the image at its own size.
    pub scale: f32,
    pub filter: ResampleFilter,
    /// Whether an available ICC profile is embedded.
    pub include_metadata: bool,
    /// Shown through transparency in containers that cannot store partial
    /// alpha. Encoded in [`ExportPreset::color_space`].
    pub background: [u8; 3],
    /// The space the file's pixels are encoded in.
    pub color_space: ColorSpace,
    /// Whether the working buffer is converted into `color_space` or written
    /// through untouched. See [`ColorHandling`].
    pub color_handling: ColorHandling,
}

impl Default for ExportPreset {
    fn default() -> Self {
        ExportPreset {
            name: "export".to_string(),
            format: ExportFormat::Png,
            bit_depth: BitDepth::Eight,
            scale: 1.0,
            filter: ResampleFilter::Lanczos3,
            include_metadata: true,
            background: [255, 255, 255],
            color_space: ColorSpace::Srgb,
            color_handling: ColorHandling::Convert,
        }
    }
}

impl ExportPreset {
    /// A preset with the given name and container, everything else default.
    pub fn new(name: impl Into<String>, format: ExportFormat) -> Self {
        ExportPreset {
            name: name.into(),
            format,
            ..Default::default()
        }
    }

    /// A preset that writes a document back in its own, untransformable space —
    /// the ICC case. Fails validation if `space` *does* have a transform.
    pub fn pass_through(name: impl Into<String>, format: ExportFormat, space: ColorSpace) -> Self {
        ExportPreset {
            name: name.into(),
            format,
            color_space: space,
            color_handling: ColorHandling::PassThrough,
            ..Default::default()
        }
    }

    pub fn with_scale(mut self, scale: f32) -> Self {
        self.scale = scale;
        self
    }

    pub fn with_filter(mut self, filter: ResampleFilter) -> Self {
        self.filter = filter;
        self
    }

    pub fn with_bit_depth(mut self, depth: BitDepth) -> Self {
        self.bit_depth = depth;
        self
    }

    pub fn with_background(mut self, background: [u8; 3]) -> Self {
        self.background = background;
        self
    }

    pub fn with_metadata(mut self, include: bool) -> Self {
        self.include_metadata = include;
        self
    }

    pub fn with_color_space(mut self, space: ColorSpace) -> Self {
        self.color_space = space;
        self
    }

    pub fn with_color_handling(mut self, handling: ColorHandling) -> Self {
        self.color_handling = handling;
        self
    }

    /// Reject a preset whose parameters cannot produce a file.
    pub fn validate(&self) -> Result<(), ExportError> {
        let reject = |reason: String| ExportError::Preset {
            name: self.name.clone(),
            reason,
        };
        self.format.validate().map_err(|e| reject(e.to_string()))?;
        if !self.scale.is_finite() || self.scale <= 0.0 {
            return Err(reject(format!(
                "scale must be a positive finite number, got {}",
                self.scale
            )));
        }
        if self.scale > MAX_SCALE {
            return Err(reject(format!(
                "scale {} exceeds the {MAX_SCALE}x limit",
                self.scale
            )));
        }
        if self.bit_depth == BitDepth::Sixteen && !self.format.supports_16_bit() {
            return Err(reject(format!(
                "{} cannot store 16 bits per channel",
                self.format.extension()
            )));
        }
        match self.color_handling {
            // Resolving the transform here is what stops an ICC-tagged
            // document reaching an encoder through an sRGB preset.
            ColorHandling::Convert => {
                Transform::managed(&self.color_space)?;
            }
            ColorHandling::PassThrough => {
                if self.color_space.is_transform_supported() {
                    return Err(reject(format!(
                        "pass-through is only for a space with no implemented transform; \
                         {} has one",
                        self.color_space.name()
                    )));
                }
            }
        }
        Ok(())
    }

    /// Destination size for a source of `width` x `height`.
    pub fn target_size(&self, width: u32, height: u32) -> Result<(u32, u32), ExportError> {
        self.validate()?;
        // `f64` throughout: `u32::MAX as f32 * 64.0` is not representable
        // exactly, and the whole point of this function is that the product
        // cannot silently wrap.
        let w = ((f64::from(width) * f64::from(self.scale)).round() as u64).max(1);
        let h = ((f64::from(height) * f64::from(self.scale)).round() as u64).max(1);
        if w > u64::from(u32::MAX)
            || h > u64::from(u32::MAX)
            || w.saturating_mul(h) > MAX_OUTPUT_PIXELS
        {
            return Err(ExportError::TooLarge(format!(
                "{w}x{h}, limit is {MAX_OUTPUT_PIXELS} pixels"
            )));
        }
        Ok((w as u32, h as u32))
    }

    /// A safe file name for this preset.
    ///
    /// Never contains a path separator, a `..`, or a leading dot, whatever the
    /// preset is called. See [`sanitize_file_stem`].
    pub fn file_name(&self) -> String {
        format!(
            "{}.{}",
            sanitize_file_stem(&self.name),
            self.format.extension()
        )
    }
}

/// Metadata offered to every preset in a batch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExportMetadata {
    /// ICC profile to embed where the container and the preset both allow it.
    pub icc_profile: Option<Vec<u8>>,
}

/// One finished export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedFile {
    /// Sanitised file name, from [`ExportPreset::file_name`].
    pub name: String,
    pub format: ExportFormat,
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
}

/// Quantized file samples, at the depth the preset asked for.
enum Quantized {
    Eight(Vec<u8>),
    Sixteen(Vec<u16>),
}

impl Quantized {
    fn as_pixels(&self) -> EncodedPixels<'_> {
        match self {
            Quantized::Eight(v) => EncodedPixels::Rgba8(v),
            Quantized::Sixteen(v) => EncodedPixels::Rgba16(v),
        }
    }
}

/// Everything an encoder needs, with no file written yet.
struct Prepared {
    width: u32,
    height: u32,
    pixels: Quantized,
    options: EncodeOptions,
}

/// Refuse a buffer whose samples are not what the preset says they are.
///
/// Both directions are errors, and both are silent corruption if they are not:
///
/// * A `*_pass_through` buffer (the file's own gamma-encoded samples, which is
///   the only way an ICC-tagged document enters this pipeline) handed to a
///   [`ColorHandling::Convert`] preset gets an sRGB encode applied to samples
///   that are already encoded — measured on a mid-grey: 128 in, 188 out, every
///   midtone brightened.
/// * A genuinely linear buffer handed to a [`ColorHandling::PassThrough`]
///   preset gets *no* encode where one is required — linear 0.5 written as
///   code 128 instead of 188, every midtone darkened.
///
/// [`LinearImage`] carries the answer itself, so this is a check on the buffer
/// and not on a convention the caller was asked to remember.
fn check_handling(image: &LinearImage, preset: &ExportPreset) -> Result<(), ExportError> {
    let reason = match (image.is_pass_through(), preset.color_handling) {
        (true, ColorHandling::Convert) => {
            "its samples are the source file's own encoded values, and this preset would \
             encode them a second time; use ExportPreset::pass_through in the source's own \
             space"
        }
        (false, ColorHandling::PassThrough) => {
            "its samples are linear working values, and this preset would write them with no \
             transfer function at all; use a converting preset"
        }
        _ => return Ok(()),
    };
    Err(ExportError::ColorHandlingMismatch {
        preset: preset.name.clone(),
        reason: reason.to_string(),
    })
}

/// The profile to embed, which must describe the space the file is written in.
///
/// [`ExportMetadata`] is offered to *every* preset in a batch, and one profile
/// cannot describe two different spaces. Embedding it unconditionally means the
/// second file claims a colour it does not contain, which a colour-managed
/// viewer then renders wrong — the same class of mislabeling the rest of this
/// module exists to prevent, so it is refused here rather than written.
///
/// The profile is embedded only where the file's space *is* the profile's
/// space: a [`ColorHandling::PassThrough`] preset, whose
/// [`ExportPreset::color_space`] is by construction the
/// [`ColorSpace::IccProfile`] the buffer came from, and whose `asset_hash` is
/// verified against the profile's actual content hash.
fn resolve_icc(
    preset: &ExportPreset,
    metadata: &ExportMetadata,
) -> Result<Option<Vec<u8>>, ExportError> {
    if !preset.include_metadata || !preset.format.supports_icc() {
        return Ok(None);
    }
    let Some(profile) = metadata.icc_profile.as_ref() else {
        return Ok(None);
    };
    let reject = |reason: String| {
        Err(ExportError::Preset {
            name: preset.name.clone(),
            reason,
        })
    };
    match &preset.color_space {
        ColorSpace::IccProfile { asset_hash } => {
            let actual = blake3::hash(profile).to_hex().to_string();
            if &actual != asset_hash {
                return reject(format!(
                    "the offered ICC profile hashes to {actual}, but the file is written in \
                     the profile addressed by {asset_hash}; embedding it would mislabel the file"
                ));
            }
            Ok(Some(profile.clone()))
        }
        built_in => reject(format!(
            "the file is written in {}, so the offered ICC profile does not describe it; \
             clear ExportMetadata::icc_profile or set include_metadata = false",
            built_in.name()
        )),
    }
}

/// Run one preset's colour pipeline over an already-scaled image.
fn prepare(
    scaled: &LinearImage,
    preset: &ExportPreset,
    metadata: &ExportMetadata,
) -> Result<Prepared, ExportError> {
    check_handling(scaled, preset)?;
    let transform = Transform::resolve(&preset.color_space, preset.color_handling)?;

    let flattened;
    let source = match preset.format.alpha_support() {
        AlphaSupport::Full => scaled,
        AlphaSupport::Binary => {
            flattened = flatten_with(
                scaled,
                preset.background,
                transform,
                FlattenMode::PartialOnly,
            );
            &flattened
        }
        AlphaSupport::None => {
            flattened = flatten_with(scaled, preset.background, transform, FlattenMode::All);
            &flattened
        }
    };

    let options = EncodeOptions {
        icc_profile: resolve_icc(preset, metadata)?,
    };

    let pixels = match preset.bit_depth {
        BitDepth::Eight => Quantized::Eight(to_rgba8_with(source, transform)),
        BitDepth::Sixteen => Quantized::Sixteen(to_rgba16_with(source, transform)),
    };

    Ok(Prepared {
        width: source.width,
        height: source.height,
        pixels,
        options,
    })
}

fn encode_scaled(
    scaled: &LinearImage,
    preset: &ExportPreset,
    metadata: &ExportMetadata,
) -> Result<ExportedFile, ExportError> {
    let p = prepare(scaled, preset, metadata)?;
    let bytes = encode_with(
        preset.format,
        p.width,
        p.height,
        p.pixels.as_pixels(),
        &p.options,
    )?;
    Ok(ExportedFile {
        name: preset.file_name(),
        format: preset.format,
        width: p.width,
        height: p.height,
        bytes,
    })
}

fn write_scaled(
    dir: &Path,
    scaled: &LinearImage,
    preset: &ExportPreset,
    metadata: &ExportMetadata,
) -> Result<PathBuf, ExportError> {
    let p = prepare(scaled, preset, metadata)?;
    // The name is sanitised, so this join cannot leave `dir`.
    let path = dir.join(preset.file_name());
    encode_to_path(
        &path,
        preset.format,
        p.width,
        p.height,
        p.pixels.as_pixels(),
        &p.options,
    )?;
    Ok(path)
}

/// Run one preset over a linear premultiplied image.
///
/// A preset at `scale = 1.0` encodes straight from `image`; nothing is copied
/// on the way, which matters because a float RGBA 8K frame is half a gigabyte.
pub fn export(
    image: &LinearImage,
    preset: &ExportPreset,
    metadata: &ExportMetadata,
) -> Result<ExportedFile, ExportError> {
    // Before the resample, so a mismatched pair costs nothing. `prepare`
    // checks again, because it is reachable from the batch paths too.
    check_handling(image, preset)?;
    let (w, h) = preset.target_size(image.width, image.height)?;
    if (w, h) == (image.width, image.height) {
        return encode_scaled(image, preset, metadata);
    }
    let scaled = resample(image, w, h, preset.filter)?;
    encode_scaled(&scaled, preset, metadata)
}

/// Validate a batch, then run `f` for each preset with the image it should
/// encode, sharing one resample between presets that ask for the same size.
///
/// # At most one scaled image is alive
///
/// Presets are *grouped* by `(width, height, filter)` and each group is run to
/// completion, so the scaled buffer is dropped before the next group's is
/// created. A cache keyed by size would share the resamples just as well but
/// would retain one float buffer per distinct size, and a float RGBA buffer is
/// bounded only by [`MAX_OUTPUT_PIXELS`] — 4 GiB each. Preset lists are
/// untrusted (see the module header), so both the preset count and the scale
/// factors are attacker-chosen, and N of them holding N buffers is an
/// unbounded allocation with two attacker-chosen factors in it.
///
/// Results keep the caller's preset order regardless of the order the groups
/// ran in.
fn for_each_scaled<T>(
    image: &LinearImage,
    presets: &[ExportPreset],
    metadata: &ExportMetadata,
    mut f: impl FnMut(&ExportPreset, &LinearImage) -> Result<T, ExportError>,
) -> Result<Vec<T>, ExportError> {
    for preset in presets {
        preset.validate()?;
        check_handling(image, preset)?;
        // Resolved up front as well as in `prepare`, so a batch whose metadata
        // does not describe every preset's space fails before it writes the
        // presets that happen to come first.
        resolve_icc(preset, metadata)?;
    }
    let mut seen: Vec<String> = Vec::with_capacity(presets.len());
    for preset in presets {
        let name = preset.file_name();
        if seen.contains(&name) {
            return Err(ExportError::DuplicateOutput(name));
        }
        seen.push(name);
    }

    // Sizes first, so an over-large preset is refused before any pixels move.
    let mut targets = Vec::with_capacity(presets.len());
    for preset in presets {
        targets.push(preset.target_size(image.width, image.height)?);
    }

    let source_size = (image.width, image.height);
    let mut out: Vec<Option<T>> = (0..presets.len()).map(|_| None).collect();
    for i in 0..presets.len() {
        if out[i].is_some() {
            continue;
        }
        // A 1:1 preset uses the source directly; nothing is scaled, and the
        // filter is irrelevant to which presets can share it.
        if targets[i] == source_size {
            for j in i..presets.len() {
                if out[j].is_none() && targets[j] == source_size {
                    out[j] = Some(f(&presets[j], image)?);
                }
            }
            continue;
        }
        let key = (targets[i].0, targets[i].1, presets[i].filter);
        let scaled = resample(image, key.0, key.1, key.2)?;
        for j in i..presets.len() {
            if out[j].is_none() && (targets[j].0, targets[j].1, presets[j].filter) == key {
                out[j] = Some(f(&presets[j], &scaled)?);
            }
        }
        // ...and `scaled` is dropped here, before the next group's resample.
    }
    Ok(out
        .into_iter()
        .map(|r| r.expect("every preset is assigned to exactly one group"))
        .collect())
}

/// Run several presets over one image in a single call.
///
/// Presets that ask for the same size and filter share a single resample, which
/// is the common case (a PNG and a JPEG at 1x) and by far the expensive step.
/// Two presets that would write the same file name are rejected rather than
/// silently overwriting each other.
///
/// Every output is held in memory at once. Use [`export_batch_to_dir`] for a
/// batch big enough for that to matter.
pub fn export_batch(
    image: &LinearImage,
    presets: &[ExportPreset],
    metadata: &ExportMetadata,
) -> Result<Vec<ExportedFile>, ExportError> {
    for_each_scaled(image, presets, metadata, |preset, scaled| {
        encode_scaled(scaled, preset, metadata)
    })
}

/// [`export_batch`] straight to files in `dir`, returning the paths written.
///
/// # What is and is not bounded
///
/// Each file is streamed to disk and dropped before the next preset runs, so a
/// twenty-preset batch never holds twenty encoded images. The larger buffers
/// are the *float* ones — a scaled [`LinearImage`] is 16 bytes a pixel, four
/// times the 8-bit buffer that gets encoded — and those are bounded too:
/// [`for_each_scaled`] groups presets by target size so exactly one scaled
/// image is alive at a time, whatever the preset list asks for. Peak footprint
/// is therefore the source buffer plus the largest single scaled buffer plus
/// one encode, and does not grow with the number of presets.
///
/// Every write is atomic — see [`crate::codec::encode_to_path`] — so a failure
/// part way through leaves the files already written intact and no half-written
/// file behind.
///
/// File names come from [`ExportPreset::file_name`] and are sanitised, so a
/// preset named by someone else cannot write outside `dir`.
pub fn export_batch_to_dir(
    dir: &Path,
    image: &LinearImage,
    presets: &[ExportPreset],
    metadata: &ExportMetadata,
) -> Result<Vec<PathBuf>, ExportError> {
    std::fs::create_dir_all(dir).map_err(|e| ExportError::Codec(CodecError::Io(e)))?;
    for_each_scaled(image, presets, metadata, |preset, scaled| {
        write_scaled(dir, scaled, preset, metadata)
    })
}

// ------------------------------------------------------------ name safety

/// Windows device names, which are reserved whatever extension follows them.
const RESERVED_STEMS: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Reduce an arbitrary string to a file-name stem that is safe to join onto a
/// directory.
///
/// The allowed set is ASCII alphanumerics plus `- _ ( )` and space. Everything
/// else — separators, dots, colons, NUL, control characters, non-ASCII — becomes
/// `_`. Because `.` is not in the allowed set, `..` cannot survive, no result
/// can be a hidden file, and no result can smuggle a second extension. Reserved
/// Windows device names and empty results fall back to `export`.
pub fn sanitize_file_stem(raw: &str) -> String {
    const MAX: usize = 64;
    let mut out = String::with_capacity(raw.len().min(MAX));
    for ch in raw.chars() {
        if out.len() >= MAX {
            break;
        }
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '(' | ')' | ' ') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    let trimmed = out.trim().to_string();
    if trimmed.is_empty() || RESERVED_STEMS.contains(&trimmed.to_ascii_lowercase().as_str()) {
        "export".to_string()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{
        decode_surface_bytes, encode_with, EncodeOptions, ImportLimits, SurfacePixels,
    };

    fn one_pixel(px: [f32; 4]) -> LinearImage {
        LinearImage::from_premultiplied(1, 1, px.to_vec()).unwrap()
    }

    /// A structurally valid, minimal ICC profile, small enough to compare byte
    /// for byte. Mirrors the one in `codec`'s tests.
    fn tiny_icc() -> Vec<u8> {
        let mut p = vec![0u8; 132];
        p[0..4].copy_from_slice(&132u32.to_be_bytes());
        p[4..8].copy_from_slice(b"RSTU");
        p[8..12].copy_from_slice(&0x0420_0000u32.to_be_bytes());
        p[12..16].copy_from_slice(b"mntr");
        p[16..20].copy_from_slice(b"RGB ");
        p[20..24].copy_from_slice(b"XYZ ");
        p[36..40].copy_from_slice(b"acsp");
        p
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "raster-export-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // ------------------------------------------------- the colour pipeline

    /// The bug this whole module exists to prevent, pinned to exact bytes.
    ///
    /// A linear premultiplied pixel is *not* the pixel a file stores. Writing
    /// the working buffer straight out — what `encode` did before this change —
    /// produces the "naive" numbers below, which are wrong in every channel.
    #[test]
    fn linear_premultiplied_exports_to_exact_srgb_straight_alpha_bytes() {
        // Half-transparent: premultiplied (0.25, 0.125, 0.5) at alpha 0.5 is
        // straight linear (0.5, 0.25, 1.0).
        let image = one_pixel([0.25, 0.125, 0.5, 0.5]);
        let bytes = rgba8_from_linear(&image, &ColorSpace::Srgb).unwrap();
        assert_eq!(
            bytes,
            vec![188, 137, 255, 128],
            "linear 0.5/0.25/1.0 at alpha 0.5 must encode as sRGB 188/137/255/128"
        );

        // What writing the working buffer verbatim would have produced.
        let naive: Vec<u8> = image
            .pixels()
            .iter()
            .map(|v| (v * 255.0 + 0.5) as u8)
            .collect();
        assert_eq!(naive, vec![64, 32, 128, 128]);
        assert_ne!(
            bytes, naive,
            "the un-premultiply and the transfer curve are not being applied"
        );

        // Opaque mid-grey: 18% linear is sRGB 118, not 46.
        assert_eq!(
            rgba8_from_linear(&one_pixel([0.18, 0.18, 0.18, 1.0]), &ColorSpace::Srgb).unwrap(),
            vec![118, 118, 118, 255]
        );
        // Deep shadow, on the curve's linear segment: 0.002 * 12.92 * 255.
        assert_eq!(
            rgba8_from_linear(&one_pixel([0.002, 0.002, 0.002, 1.0]), &ColorSpace::Srgb).unwrap(),
            vec![7, 7, 7, 255]
        );
        // The endpoints are exact, and out-of-range highlights clamp rather
        // than wrapping.
        assert_eq!(
            rgba8_from_linear(&one_pixel([0.0, 1.0, 4.0, 1.0]), &ColorSpace::Srgb).unwrap(),
            vec![0, 255, 255, 255]
        );
        // A fully transparent pixel is transparent black, not whatever colour
        // was left in the premultiplied buffer.
        assert_eq!(
            rgba8_from_linear(&one_pixel([0.0, 0.0, 0.0, 0.0]), &ColorSpace::Srgb).unwrap(),
            vec![0, 0, 0, 0]
        );
    }

    /// ...and the whole way through a real PNG, not just through the converter.
    #[test]
    fn a_full_export_writes_the_colour_managed_bytes() {
        let image = one_pixel([0.25, 0.125, 0.5, 0.5]);
        let file = export(
            &image,
            &ExportPreset::new("half", ExportFormat::Png),
            &ExportMetadata::default(),
        )
        .unwrap();
        let decoded = decode_surface_bytes(&file.bytes, ImportLimits::default()).unwrap();
        assert_eq!(
            decoded.pixels,
            SurfacePixels::Rgba8(vec![188, 137, 255, 128])
        );
    }

    #[test]
    fn sixteen_bit_export_keeps_more_than_eight_bits() {
        let image = one_pixel([0.18, 0.18, 0.18, 1.0]);
        let preset = ExportPreset::new("deep", ExportFormat::Png).with_bit_depth(BitDepth::Sixteen);
        let file = export(&image, &preset, &ExportMetadata::default()).unwrap();
        let decoded = decode_surface_bytes(&file.bytes, ImportLimits::default()).unwrap();
        let SurfacePixels::Rgba16(px) = decoded.pixels else {
            panic!("a 16-bit preset must write a 16-bit file");
        };
        // sRGB(0.18) = 0.46136; * 65535 = 30_235. The tolerance is one code,
        // because the last bit of a `powf` is not portable across libm builds.
        assert!(
            (i32::from(px[0]) - 30_235).abs() <= 1,
            "expected ~30235, got {}",
            px[0]
        );
        assert_eq!(px[3], 65_535);
        // The 8-bit path can only reach 118/255 = 30_270 after rescaling, so
        // the extra precision is real rather than a widened 8-bit value.
        assert_ne!(px[0] % 257, 0, "value is just an 8-bit code widened to 16");
    }

    #[test]
    fn the_colour_round_trip_is_stable() {
        // Every 8-bit code survives encode -> linear -> encode unchanged, which
        // is what makes an import/export cycle non-destructive.
        let straight: Vec<u8> = (0u8..=255).flat_map(|c| [c, c, c, 255]).collect();
        let linear = linear_from_rgba8(256, 1, &straight, &ColorSpace::Srgb).unwrap();
        let back = rgba8_from_linear(&linear, &ColorSpace::Srgb).unwrap();
        assert_eq!(back, straight);
    }

    /// The same round trip with alpha actually varying, so the premultiply and
    /// un-premultiply halves are exercised rather than merely present.
    #[test]
    fn the_colour_round_trip_is_stable_at_every_alpha() {
        // Alpha 0 is excluded on purpose: a transparent pixel carries no colour
        // to recover, and it is asserted separately below.
        let straight: Vec<u8> = (1u8..=255).flat_map(|a| [200, 128, 3, a]).collect();
        let linear = linear_from_rgba8(255, 1, &straight, &ColorSpace::Srgb).unwrap();
        // The buffer really is premultiplied: at alpha 1/255 the stored colour
        // is a 255th of the straight one.
        let first = linear.pixel(0, 0);
        let alpha = 1.0 / 255.0;
        assert!((first[3] - alpha).abs() < 1e-6);
        assert!(
            (first[0] - color::srgb_to_linear(200.0 / 255.0) * alpha).abs() < 1e-6,
            "not premultiplied: {first:?}"
        );

        let back = rgba8_from_linear(&linear, &ColorSpace::Srgb).unwrap();
        for (i, (got, want)) in back
            .chunks_exact(4)
            .zip(straight.chunks_exact(4))
            .enumerate()
        {
            assert_eq!(got[3], want[3], "alpha changed at {i}");
            for c in 0..3 {
                assert!(
                    (i32::from(got[c]) - i32::from(want[c])).abs() <= 1,
                    "alpha {} channel {c}: {got:?} vs {want:?}",
                    want[3]
                );
            }
        }

        // ...and a fully transparent pixel comes back transparent black.
        let clear = linear_from_rgba8(1, 1, &[200, 128, 3, 0], &ColorSpace::Srgb).unwrap();
        assert_eq!(clear.pixels(), &[0.0, 0.0, 0.0, 0.0]);
        assert_eq!(
            rgba8_from_linear(&clear, &ColorSpace::Srgb).unwrap(),
            vec![0, 0, 0, 0]
        );
    }

    #[test]
    fn the_output_space_is_a_parameter_not_an_assumption() {
        let image = one_pixel([0.5, 0.2, 0.1, 1.0]);
        let srgb = rgba8_from_linear(&image, &ColorSpace::Srgb).unwrap();
        let p3 = rgba8_from_linear(&image, &ColorSpace::DisplayP3).unwrap();
        let linear = rgba8_from_linear(&image, &ColorSpace::LinearSrgb).unwrap();
        assert_ne!(srgb, p3, "P3 encoding must differ from sRGB encoding");
        assert_ne!(srgb, linear);
        assert_eq!(linear, vec![128, 51, 26, 255]);
    }

    // ------------------------------------------------------- ICC profiles

    /// An ICC-tagged file has no implemented transform, and the working-space
    /// conversion must say so instead of handing back gamma-encoded numbers
    /// labelled "linear".
    ///
    /// Measured before the fix: sRGB code 128 in an ICC-tagged PNG arrived as
    /// 0.5019608 (the encoded number, not linear 0.2158) and re-exported
    /// through the default sRGB preset as 188 — every midtone grossly
    /// brightened, with no error anywhere.
    #[test]
    fn an_icc_tagged_import_is_a_typed_error_not_a_silent_identity() {
        let profile = tiny_icc();
        let bytes = encode_with(
            ExportFormat::Png,
            1,
            1,
            EncodedPixels::Rgba8(&[128, 128, 128, 255]),
            &EncodeOptions::with_icc(profile.clone()),
        )
        .unwrap();
        let surface = decode_surface_bytes(&bytes, ImportLimits::default()).unwrap();
        let space = surface.color_space.clone();
        assert!(matches!(space, ColorSpace::IccProfile { .. }));
        assert!(!space.is_transform_supported());

        // The decode side.
        let err = surface.to_linear_premultiplied().unwrap_err();
        assert!(
            matches!(&err, ExportError::Color(u) if u.space == space),
            "expected a typed colour error, got {err}"
        );
        assert!(linear_from_rgba8(1, 1, &[128, 128, 128, 255], &space).is_err());
        assert!(linear_from_rgba16(1, 1, &[0, 0, 0, 0], &space).is_err());

        // The encode side, including the flatten that a JPEG or GIF preset
        // would reach on the way.
        let working = one_pixel([0.2158, 0.2158, 0.2158, 1.0]);
        assert!(rgba8_from_linear(&working, &space).is_err());
        assert!(rgba16_from_linear(&working, &space).is_err());
        assert!(flatten_onto(&working, [0, 0, 0], &space, FlattenMode::All).is_err());

        // And a preset carrying that space cannot be run at all...
        let preset = ExportPreset::new("icc", ExportFormat::Png).with_color_space(space.clone());
        assert!(matches!(preset.validate(), Err(ExportError::Color(_))));
        assert!(export(&working, &preset, &ExportMetadata::default()).is_err());

        // ...nor can the mismatch the bug produced: an ICC document exported
        // through the default sRGB preset. The document never becomes a
        // `LinearImage` in the first place.
        assert_eq!(ExportPreset::default().color_space, ColorSpace::Srgb);
        assert!(surface.to_linear_premultiplied().is_err());
    }

    /// ...and an ICC-tagged document is still fully exportable in its own
    /// space: samples and profile both survive untouched.
    #[test]
    fn an_icc_tagged_surface_round_trips_in_its_own_space() {
        let profile = tiny_icc();
        let px: Vec<u8> = (0u8..16)
            .flat_map(|c| [c * 17, 255 - c * 17, 128, 255])
            .collect();
        let bytes = encode_with(
            ExportFormat::Png,
            4,
            4,
            EncodedPixels::Rgba8(&px),
            &EncodeOptions::with_icc(profile.clone()),
        )
        .unwrap();
        let surface = decode_surface_bytes(&bytes, ImportLimits::default()).unwrap();
        let space = surface.color_space.clone();

        // The pass-through buffer holds the file's own samples, not a
        // transformed version of them.
        let working = surface.to_premultiplied_pass_through();
        assert!((working.pixel(0, 0)[2] - 128.0 / 255.0).abs() < 1e-3);
        assert_eq!(rgba8_from_linear_pass_through(&working), px);

        let preset =
            ExportPreset::pass_through("icc", ExportFormat::Png, space.clone()).with_metadata(true);
        preset.validate().unwrap();
        let file = export(
            &working,
            &preset,
            &ExportMetadata {
                icc_profile: surface.icc_profile.clone(),
            },
        )
        .unwrap();
        let again = decode_surface_bytes(&file.bytes, ImportLimits::default()).unwrap();
        assert_eq!(again.pixels, SurfacePixels::Rgba8(px));
        assert_eq!(again.icc_profile.as_deref(), Some(profile.as_slice()));
        assert_eq!(again.color_space, space);
    }

    /// A buffer and a preset that disagree about what the samples mean is an
    /// error, in both directions.
    ///
    /// Measured before the fix: `export(&linear_from_rgba8_pass_through(1, 1,
    /// &[128,128,128,255]), &ExportPreset::new("oops", Png), &default())`
    /// returned `Ok` and the decoded file held `Rgba8([188,188,188,255])` —
    /// byte for byte the "every midtone brightened" bug this module was written
    /// to eliminate, moved one call earlier rather than removed. `LinearImage`
    /// carried no record of which producer made it and nothing checked.
    #[test]
    fn a_buffer_and_a_preset_that_disagree_about_the_samples_are_refused() {
        let gamma = linear_from_rgba8_pass_through(1, 1, &[128, 128, 128, 255]).unwrap();
        assert!(gamma.is_pass_through());

        let converting = ExportPreset::new("oops", ExportFormat::Png);
        assert_eq!(converting.color_handling, ColorHandling::Convert);
        let err = export(&gamma, &converting, &ExportMetadata::default()).unwrap_err();
        assert!(
            matches!(err, ExportError::ColorHandlingMismatch { .. }),
            "{err}"
        );

        // The exact file the unguarded path produced, pinned so the guard's
        // absence is a visible number and not an argument.
        assert_eq!(
            rgba8_from_linear(&gamma, &ColorSpace::Srgb).unwrap(),
            vec![188, 188, 188, 255],
            "a second sRGB encode of the file's own 128 is 188"
        );

        // The realistic route in: an ICC-tagged import, whose only entry into
        // the pipeline is `to_premultiplied_pass_through`.
        let (working, space, _, px) = icc_document();
        assert!(working.is_pass_through());
        assert!(matches!(
            export(
                &working,
                &ExportPreset::new("icc-as-srgb", ExportFormat::Png),
                &ExportMetadata::default()
            ),
            Err(ExportError::ColorHandlingMismatch { .. })
        ));

        // Scaling and flattening carry the record with them, so the mismatch
        // cannot be laundered by putting an operation in between.
        assert!(resample(&working, 3, 3, ResampleFilter::Lanczos3)
            .unwrap()
            .is_pass_through());
        assert!(flatten_with(
            &working,
            [0, 0, 0],
            Transform::PassThrough,
            FlattenMode::All
        )
        .is_pass_through());
        for preset in [
            ExportPreset::new("scaled", ExportFormat::Png).with_scale(2.0),
            ExportPreset::new("flattened", ExportFormat::Jpeg(90)),
            ExportPreset::new("both", ExportFormat::Gif).with_scale(0.5),
        ] {
            assert!(
                matches!(
                    export(&working, &preset, &ExportMetadata::default()),
                    Err(ExportError::ColorHandlingMismatch { .. })
                ),
                "{preset:?} accepted a gamma-encoded buffer"
            );
        }

        // The opposite mismatch: genuinely linear samples through a
        // pass-through preset, which would write linear 0.2158 as code 55
        // instead of 128 — every midtone darkened.
        let linear = one_pixel([0.2158, 0.2158, 0.2158, 1.0]);
        assert!(!linear.is_pass_through());
        let pass_preset = ExportPreset::pass_through("pt", ExportFormat::Png, space.clone());
        pass_preset.validate().unwrap();
        let err = export(&linear, &pass_preset, &ExportMetadata::default()).unwrap_err();
        assert!(
            matches!(err, ExportError::ColorHandlingMismatch { .. }),
            "{err}"
        );
        assert_eq!(
            rgba8_from_linear_pass_through(&linear),
            vec![55, 55, 55, 255],
            "writing linear samples with no transfer function is 55, not 128"
        );

        // A batch is refused before it writes anything.
        let dir = temp_dir("handling");
        let one = std::slice::from_ref(&converting);
        assert!(export_batch(&gamma, one, &ExportMetadata::default()).is_err());
        assert!(export_batch_to_dir(&dir, &gamma, one, &ExportMetadata::default()).is_err());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);

        // ...and each buffer paired with the preset that matches it still
        // writes exactly the samples it should.
        let file = export(&working, &pass_preset, &ExportMetadata::default()).unwrap();
        assert_eq!(
            decode_surface_bytes(&file.bytes, ImportLimits::default())
                .unwrap()
                .pixels,
            SurfacePixels::Rgba8(px)
        );
        let file = export(
            &linear,
            &ExportPreset::new("srgb", ExportFormat::Png),
            &ExportMetadata::default(),
        )
        .unwrap();
        assert_eq!(
            decode_surface_bytes(&file.bytes, ImportLimits::default())
                .unwrap()
                .pixels,
            SurfacePixels::Rgba8(vec![128, 128, 128, 255])
        );
    }

    /// Pass-through is not a general-purpose "skip the maths" switch: for a
    /// space that *does* have a transform it could only mean skipping it.
    #[test]
    fn pass_through_is_rejected_for_a_space_that_has_a_transform() {
        for space in [
            ColorSpace::Srgb,
            ColorSpace::LinearSrgb,
            ColorSpace::DisplayP3,
        ] {
            let preset = ExportPreset::pass_through("p", ExportFormat::Png, space.clone());
            let err = preset.validate().unwrap_err();
            assert!(
                matches!(err, ExportError::Preset { .. }),
                "{space:?}: {err}"
            );
        }
    }

    // ------------------------------------------------------------ flatten

    #[test]
    fn transparency_is_flattened_onto_the_background_for_jpeg() {
        // Fully transparent, exported to a container with no alpha.
        let image = one_pixel([0.0, 0.0, 0.0, 0.0]);
        for bg in [[255u8, 255, 255], [255, 0, 0], [0, 0, 0]] {
            let preset = ExportPreset::new("flat", ExportFormat::Jpeg(100)).with_background(bg);
            let file = export(&image, &preset, &ExportMetadata::default()).unwrap();
            let decoded = decode_surface_bytes(&file.bytes, ImportLimits::default()).unwrap();
            let SurfacePixels::Rgba8(px) = decoded.pixels else {
                unreachable!()
            };
            for c in 0..3 {
                assert!(
                    (px[c] as i32 - bg[c] as i32).abs() <= 3,
                    "background {bg:?} came out as {:?}",
                    &px[..3]
                );
            }
            assert_eq!(px[3], 255);
        }
    }

    /// GIF has no alpha channel — only one transparent palette entry — so a
    /// half-transparent pixel must be composited onto the preset background.
    ///
    /// Measured before the fix: `supports_alpha()` claimed GIF stored alpha, so
    /// the flatten was skipped and premultiplied (0.5, 0, 0, 0.5) came back as
    /// an opaque 255/0/0 with the background ignored entirely.
    #[test]
    fn gif_composites_partial_alpha_onto_the_background() {
        // Left: half-transparent red. Right: fully transparent.
        let image =
            LinearImage::from_premultiplied(2, 1, vec![0.5, 0.0, 0.0, 0.5, 0.0, 0.0, 0.0, 0.0])
                .unwrap();
        let background = [0u8, 255, 0];
        let preset = ExportPreset::new("g", ExportFormat::Gif).with_background(background);
        let file = export(&image, &preset, &ExportMetadata::default()).unwrap();
        let decoded = decode_surface_bytes(&file.bytes, ImportLimits::default()).unwrap();
        let SurfacePixels::Rgba8(px) = decoded.pixels else {
            unreachable!()
        };

        // Half red over green, composited in linear space: R = 0.5, G = 0.5,
        // B = 0, which is sRGB 188/188/0.
        for (c, want) in [188i32, 188, 0].into_iter().enumerate() {
            assert!(
                (i32::from(px[c]) - want).abs() <= 8,
                "expected ~188/188/0 over the green background, got {:?}",
                &px[..4]
            );
        }
        assert_eq!(px[3], 255, "a composited pixel must be opaque");
        // The un-composited answer, which is what the bug produced.
        assert!(
            px[1] > 64,
            "the background was ignored: {:?} is still pure red",
            &px[..4]
        );

        // ...and the fully transparent pixel keeps the transparent index.
        assert_eq!(px[7], 0, "a transparent pixel became opaque background");

        // The same image in a container that really does store alpha keeps its
        // partial alpha instead.
        let png = export(
            &image,
            &ExportPreset::new("p", ExportFormat::Png).with_background(background),
            &ExportMetadata::default(),
        )
        .unwrap();
        let decoded = decode_surface_bytes(&png.bytes, ImportLimits::default()).unwrap();
        assert_eq!(
            decoded.pixels,
            SurfacePixels::Rgba8(vec![255, 0, 0, 128, 0, 0, 0, 0])
        );
    }

    /// The alpha classification is a property of each container, not a guess.
    #[test]
    fn each_container_declares_what_it_can_do_with_alpha() {
        assert_eq!(ExportFormat::Png.alpha_support(), AlphaSupport::Full);
        assert_eq!(ExportFormat::Tiff.alpha_support(), AlphaSupport::Full);
        assert_eq!(ExportFormat::WebP.alpha_support(), AlphaSupport::Full);
        assert_eq!(ExportFormat::Bmp.alpha_support(), AlphaSupport::Full);
        assert_eq!(ExportFormat::Gif.alpha_support(), AlphaSupport::Binary);
        assert_eq!(ExportFormat::Jpeg(90).alpha_support(), AlphaSupport::None);
        // GIF is not alpha-capable, whatever it used to claim.
        assert!(!ExportFormat::Gif.supports_alpha());

        // Every format that claims full alpha really does round-trip a
        // half-transparent pixel.
        let image = one_pixel([0.5, 0.0, 0.0, 0.5]);
        for format in ExportFormat::ALL {
            if format.alpha_support() != AlphaSupport::Full {
                continue;
            }
            let file = export(
                &image,
                &ExportPreset::new("a", format),
                &ExportMetadata::default(),
            )
            .unwrap();
            let decoded = decode_surface_bytes(&file.bytes, ImportLimits::default()).unwrap();
            let SurfacePixels::Rgba8(px) = decoded.pixels else {
                unreachable!()
            };
            assert!(
                (i32::from(px[3]) - 128).abs() <= 1,
                "{format:?} claims full alpha but wrote {px:?}"
            );
        }
    }

    #[test]
    fn half_transparent_over_a_background_composites_in_linear_space() {
        // Straight linear white at alpha 0.5 over black: linear 0.5, which is
        // sRGB 188 — not the 128 an sRGB-space blend would give.
        let image = one_pixel([0.5, 0.5, 0.5, 0.5]);
        let flat = flatten_onto(&image, [0, 0, 0], &ColorSpace::Srgb, FlattenMode::All).unwrap();
        assert_eq!(
            rgba8_from_linear(&flat, &ColorSpace::Srgb).unwrap(),
            vec![188, 188, 188, 255]
        );
    }

    #[test]
    fn formats_with_alpha_are_not_flattened() {
        let image = one_pixel([0.0, 0.0, 0.0, 0.0]);
        let preset = ExportPreset::new("keep", ExportFormat::Png).with_background([255, 0, 0]);
        let file = export(&image, &preset, &ExportMetadata::default()).unwrap();
        let decoded = decode_surface_bytes(&file.bytes, ImportLimits::default()).unwrap();
        assert_eq!(decoded.pixels, SurfacePixels::Rgba8(vec![0, 0, 0, 0]));
    }

    // ---------------------------------------------------------- resampling

    /// Vertical stripes one pixel wide are pure Nyquist-rate detail: a correct
    /// downscale reproduces their *average*, and an aliasing one reproduces a
    /// moire pattern of near-black and near-white columns.
    ///
    /// The scale factor is deliberately non-integer (128 -> 47), because an
    /// integer factor can be got right by accident with a box filter.
    #[test]
    fn a_non_integer_downscale_does_not_alias() {
        const SRC: u32 = 128;
        const DST: u32 = 47;
        let mut px = Vec::with_capacity((SRC * SRC * 4) as usize);
        for _y in 0..SRC {
            for x in 0..SRC {
                let v = if x % 2 == 0 { 1.0f32 } else { 0.0 };
                px.extend_from_slice(&[v, v, v, 1.0]);
            }
        }
        let image = LinearImage::from_premultiplied(SRC, SRC, px).unwrap();

        // The band-limited answer: every frequency in the source is above the
        // destination's Nyquist rate, so only the DC term survives, and the DC
        // term of a 50% duty-cycle square wave is 0.5.
        const REFERENCE: f32 = 0.5;
        let deviation = |img: &LinearImage| -> f32 {
            let mut worst = 0.0f32;
            // Skip a 4px border: edge clamping legitimately biases it.
            for y in 4..(DST as usize - 4) {
                for x in 4..(DST as usize - 4) {
                    worst = worst.max((img.pixel(x, y)[0] - REFERENCE).abs());
                }
            }
            worst
        };

        // Lanczos and Mitchell have real stopband rejection; the tent filter
        // is a weaker low-pass and leaves a little more, which is why it is not
        // the default either. Both bounds are far below the point-sampled one.
        for (filter, tolerance) in [
            (ResampleFilter::Lanczos3, 0.02f32),
            (ResampleFilter::Mitchell, 0.02),
            (ResampleFilter::Triangle, 0.08),
        ] {
            let out = resample(&image, DST, DST, filter).unwrap();
            let d = deviation(&out);
            assert!(
                d < tolerance,
                "{filter:?} left {d} of aliasing; a filtered downscale must land on the average"
            );
        }

        // ...and the point-sampling filter, which is exactly what the task
        // forbids as a default, fails the same check badly. Without this the
        // assertions above could pass with any filter at all.
        let aliased = resample(&image, DST, DST, ResampleFilter::Nearest).unwrap();
        assert!(
            deviation(&aliased) > 0.4,
            "nearest-neighbour was supposed to alias; the test proves nothing"
        );

        // The default preset filter is one of the good ones.
        assert!(matches!(
            ExportPreset::default().filter,
            ResampleFilter::Lanczos3
        ));
    }

    /// A linear ramp is reproduced exactly by any correctly *phased* normalised
    /// symmetric kernel. This is the alignment check: half-pixel errors in the
    /// centre calculation show up here and nowhere else.
    #[test]
    fn resampling_a_ramp_matches_the_analytic_reference() {
        const SRC: u32 = 100;
        const DST: u32 = 37;
        let mut px = Vec::with_capacity((SRC * 4) as usize);
        for x in 0..SRC {
            // Sample centre x+0.5 maps to value (x+0.5)/SRC.
            let v = (x as f32 + 0.5) / SRC as f32;
            px.extend_from_slice(&[v, v, v, 1.0]);
        }
        let image = LinearImage::from_premultiplied(SRC, 1, px).unwrap();

        for filter in [
            ResampleFilter::Lanczos3,
            ResampleFilter::Mitchell,
            ResampleFilter::Triangle,
        ] {
            let out = resample(&image, DST, 1, filter).unwrap();
            for x in 6..(DST as usize - 6) {
                let want = (x as f32 + 0.5) / DST as f32;
                let got = out.pixel(x, 0)[0];
                assert!(
                    (got - want).abs() < 2e-3,
                    "{filter:?} column {x}: got {got}, reference {want}"
                );
            }
        }
    }

    #[test]
    fn resampling_preserves_a_flat_field_including_alpha() {
        let image =
            LinearImage::from_premultiplied(16, 16, [0.3, 0.4, 0.5, 0.6].repeat(16 * 16)).unwrap();
        for filter in [
            ResampleFilter::Nearest,
            ResampleFilter::Triangle,
            ResampleFilter::Mitchell,
            ResampleFilter::Lanczos3,
        ] {
            for (w, h) in [(5u32, 7u32), (16, 16), (40, 3)] {
                let out = resample(&image, w, h, filter).unwrap();
                assert_eq!((out.width(), out.height()), (w, h));
                for px in out.pixels().chunks_exact(4) {
                    for (got, want) in px.iter().zip([0.3, 0.4, 0.5, 0.6]) {
                        assert!(
                            (got - want).abs() < 1e-4,
                            "{filter:?} {w}x{h} shifted a flat field: {px:?}"
                        );
                    }
                }
            }
        }
    }

    /// Both separable orders are the same filter, so the answer must not depend
    /// on which one the planner picks.
    #[test]
    fn the_two_pass_orders_produce_the_same_image() {
        let mut px = Vec::new();
        for y in 0..17u32 {
            for x in 0..29u32 {
                let v = ((x * 7 + y * 13) % 11) as f32 / 11.0;
                px.extend_from_slice(&[v, v * 0.5, 1.0 - v, 1.0]);
            }
        }
        let image = LinearImage::from_premultiplied(29, 17, px).unwrap();
        for (dw, dh) in [(7u32, 41u32), (41, 7), (3, 3)] {
            let by_plan = resample(&image, dw, dh, ResampleFilter::Lanczos3).unwrap();
            // Recompute with the order the planner did *not* pick.
            let (sw, sh) = (29usize, 17usize);
            let xt = axis_taps(sw, dw as usize, ResampleFilter::Lanczos3);
            let yt = axis_taps(sh, dh as usize, ResampleFilter::Lanczos3);
            let other = match plan_passes(29, 17, u64::from(dw), u64::from(dh)).unwrap() {
                PassOrder::HorizontalFirst => {
                    let mid = pass_y(image.pixels(), sw, sh, dh as usize, &yt);
                    pass_x(&mid, sw, dh as usize, dw as usize, &xt)
                }
                PassOrder::VerticalFirst => {
                    let mid = pass_x(image.pixels(), sw, sh, dw as usize, &xt);
                    pass_y(&mid, dw as usize, sh, dh as usize, &yt)
                }
            };
            for (i, (a, b)) in by_plan.pixels().iter().zip(&other).enumerate() {
                assert!(
                    (a - b).abs() < 1e-5,
                    "{dw}x{dh} sample {i}: {a} vs {b} across pass orders"
                );
            }
        }
    }

    #[test]
    fn an_absurd_output_size_is_refused_rather_than_allocated() {
        let image = LinearImage::transparent(4, 4).unwrap();
        let err = resample(&image, 100_000, 100_000, ResampleFilter::Triangle).unwrap_err();
        assert!(matches!(err, ExportError::TooLarge(_)), "{err}");
        let err = ExportPreset::default()
            .with_scale(64.0)
            .target_size(60_000, 60_000)
            .unwrap_err();
        assert!(matches!(err, ExportError::TooLarge(_)), "{err}");
    }

    /// The destination bound does not bound the *intermediate*.
    ///
    /// Measured before the fix: a 2x4096 source (128 KB of pixels) resampled to
    /// 65536x1 — a 65 536-pixel destination, four orders of magnitude under the
    /// cap — allocated a 4.29 GB horizontal intermediate and took 9 seconds.
    /// Filtering vertically first makes the same job a two-pixel intermediate.
    #[test]
    fn a_tiny_source_cannot_force_a_giant_intermediate_buffer() {
        let image = LinearImage::transparent(2, 4096).unwrap();
        let (out, allocated) = crate::alloc_probe::measure(|| {
            resample(&image, 65_536, 1, ResampleFilter::Lanczos3).unwrap()
        });
        assert_eq!((out.width(), out.height()), (65_536, 1));
        // The destination is 65536 * 4 floats = 1 MiB; the taps add a little.
        // The horizontal-first intermediate would be 4.29 GB.
        assert!(
            allocated < (8 << 20),
            "resampling 2x4096 -> 65536x1 allocated {allocated} bytes; \
             the intermediate is unbounded"
        );

        // The planner picks the cheaper order, and says which.
        assert_eq!(
            plan_passes(2, 4096, 65_536, 1).unwrap(),
            PassOrder::VerticalFirst
        );
        assert_eq!(
            plan_passes(4096, 2, 1, 65_536).unwrap(),
            PassOrder::HorizontalFirst
        );

        // ...and when *both* orders would exceed the cap it is refused rather
        // than attempted. (Reachable only from a source already past the cap,
        // which is why this is checked on the planner directly.)
        let err = plan_passes(1 << 20, 1 << 20, 1 << 20, 1 << 20).unwrap_err();
        assert!(matches!(err, ExportError::TooLarge(_)), "{err}");
        assert!(plan_passes(u64::MAX, u64::MAX, u64::MAX, u64::MAX).is_err());
    }

    #[test]
    fn resampling_in_premultiplied_space_does_not_fringe() {
        // A transparent *red* pixel next to an opaque *white* one. Resampling
        // straight-alpha would drag the red into the white; premultiplied does
        // not, because the transparent pixel contributes nothing.
        let px = vec![
            0.0, 0.0, 0.0, 0.0, // transparent (premultiplied: no colour at all)
            1.0, 1.0, 1.0, 1.0, // opaque white
        ];
        let image = LinearImage::from_premultiplied(2, 1, px).unwrap();
        let out = resample(&image, 1, 1, ResampleFilter::Triangle).unwrap();
        let p = out.pixel(0, 0);
        assert!((p[3] - 0.5).abs() < 1e-5, "alpha should average to 0.5");
        // Un-premultiplying gives white back, not a pink.
        let straight = unpremultiply(p);
        for c in 0..3 {
            assert!((straight[c] - 1.0).abs() < 1e-4, "fringed: {straight:?}");
        }
    }

    // ------------------------------------------------------------- presets

    #[test]
    fn presets_scale_with_a_good_filter_by_default() {
        let image = LinearImage::transparent(200, 100).unwrap();
        let preset = ExportPreset::new("half", ExportFormat::Png).with_scale(0.5);
        let file = export(&image, &preset, &ExportMetadata::default()).unwrap();
        assert_eq!((file.width, file.height), (100, 50));
        assert_eq!(file.name, "half.png");
        let decoded = decode_surface_bytes(&file.bytes, ImportLimits::default()).unwrap();
        assert_eq!((decoded.width, decoded.height), (100, 50));

        // A scale that rounds, and one that would round to zero.
        assert_eq!(
            ExportPreset::default()
                .with_scale(0.333)
                .target_size(100, 10)
                .unwrap(),
            (33, 3)
        );
        assert_eq!(
            ExportPreset::default()
                .with_scale(0.001)
                .target_size(100, 100)
                .unwrap(),
            (1, 1),
            "a tiny scale must clamp to one pixel, not zero"
        );
    }

    #[test]
    fn an_invalid_preset_is_rejected_before_anything_is_encoded() {
        let image = LinearImage::transparent(4, 4).unwrap();
        let bad = [
            ExportPreset::new("q", ExportFormat::Jpeg(0)),
            ExportPreset::new("q", ExportFormat::Jpeg(101)),
            ExportPreset::default().with_scale(0.0),
            ExportPreset::default().with_scale(-1.0),
            ExportPreset::default().with_scale(f32::NAN),
            ExportPreset::default().with_scale(f32::INFINITY),
            ExportPreset::default().with_scale(1000.0),
            ExportPreset::new("j", ExportFormat::Jpeg(90)).with_bit_depth(BitDepth::Sixteen),
            ExportPreset::new("g", ExportFormat::Gif).with_bit_depth(BitDepth::Sixteen),
            ExportPreset::new("i", ExportFormat::Png).with_color_space(ColorSpace::IccProfile {
                asset_hash: "deadbeef".into(),
            }),
        ];
        for preset in bad {
            assert!(
                preset.validate().is_err(),
                "{preset:?} should not have validated"
            );
            assert!(export(&image, &preset, &ExportMetadata::default()).is_err());
        }
        // 16-bit is fine where the container supports it.
        for format in [ExportFormat::Png, ExportFormat::Tiff] {
            assert!(ExportPreset::new("d", format)
                .with_bit_depth(BitDepth::Sixteen)
                .validate()
                .is_ok());
        }
    }

    /// A document imported from an ICC-tagged file: the pass-through working
    /// buffer, the space it is in, the profile bytes, and the file's own
    /// samples.
    ///
    /// This is the *only* way an ICC-tagged document enters the pipeline, and
    /// therefore the only realistic source of a profile worth embedding.
    fn icc_document() -> (LinearImage, ColorSpace, Vec<u8>, Vec<u8>) {
        let profile = tiny_icc();
        let px: Vec<u8> = (0u8..16)
            .flat_map(|c| [c * 17, 255 - c * 17, 128, 255])
            .collect();
        let bytes = encode_with(
            ExportFormat::Png,
            4,
            4,
            EncodedPixels::Rgba8(&px),
            &EncodeOptions::with_icc(profile.clone()),
        )
        .unwrap();
        let surface = decode_surface_bytes(&bytes, ImportLimits::default()).unwrap();
        let space = surface.color_space.clone();
        assert!(matches!(space, ColorSpace::IccProfile { .. }));
        (surface.to_premultiplied_pass_through(), space, profile, px)
    }

    /// Metadata inclusion is the preset's decision — asserted on the one export
    /// where embedding a profile is truthful, which is the pass-through path:
    /// the file is written in the profile's own space, so the profile describes
    /// it. (What happens when it does *not* describe it is
    /// `a_profile_is_only_embedded_in_a_file_written_in_its_own_space`.)
    #[test]
    fn metadata_inclusion_is_a_preset_decision() {
        let (working, space, profile, px) = icc_document();
        let metadata = ExportMetadata {
            icc_profile: Some(profile.clone()),
        };

        let with = export(
            &working,
            &ExportPreset::pass_through("with", ExportFormat::Png, space.clone())
                .with_metadata(true),
            &metadata,
        )
        .unwrap();
        let decoded = decode_surface_bytes(&with.bytes, ImportLimits::default()).unwrap();
        assert_eq!(decoded.icc_profile.as_deref(), Some(profile.as_slice()));
        assert_eq!(decoded.color_space, space, "the file re-declares its space");
        assert_eq!(decoded.pixels, SurfacePixels::Rgba8(px));

        let without = export(
            &working,
            &ExportPreset::pass_through("without", ExportFormat::Png, space.clone())
                .with_metadata(false),
            &metadata,
        )
        .unwrap();
        let decoded = decode_surface_bytes(&without.bytes, ImportLimits::default()).unwrap();
        assert_eq!(decoded.icc_profile, None);

        // Asking a container that cannot carry a profile to embed one is not an
        // error; the profile is simply not written.
        let bmp = export(
            &working,
            &ExportPreset::pass_through("b", ExportFormat::Bmp, space).with_metadata(true),
            &metadata,
        )
        .unwrap();
        let decoded = decode_surface_bytes(&bmp.bytes, ImportLimits::default()).unwrap();
        assert_eq!(decoded.icc_profile, None);
    }

    /// An embedded profile must describe the space the file was written in.
    ///
    /// Measured before the fix: `prepare` embedded `metadata.icc_profile`
    /// whenever the preset asked for metadata and the container could carry it,
    /// ignoring `color_space` entirely. One `ExportMetadata` across an sRGB
    /// preset and a Display P3 preset therefore put the identical profile in
    /// both files — at most one of which can be truthful — and a colour-managed
    /// viewer renders the other wrong.
    #[test]
    fn a_profile_is_only_embedded_in_a_file_written_in_its_own_space() {
        let (working, space, profile, _) = icc_document();
        let metadata = ExportMetadata {
            icc_profile: Some(profile.clone()),
        };

        // A converting preset writes a built-in space, which this profile does
        // not describe. Refused, rather than attached to a file it mislabels.
        let linear = one_pixel([0.18, 0.18, 0.18, 1.0]);
        for target in [
            ColorSpace::Srgb,
            ColorSpace::DisplayP3,
            ColorSpace::LinearSrgb,
        ] {
            let preset =
                ExportPreset::new("tagged", ExportFormat::Png).with_color_space(target.clone());
            let err = export(&linear, &preset, &metadata).unwrap_err();
            assert!(
                matches!(err, ExportError::Preset { .. }),
                "{target:?} accepted a profile that does not describe it: {err}"
            );
            // ...with nothing offered, or nothing asked for, it exports fine.
            assert!(export(&linear, &preset, &ExportMetadata::default()).is_ok());
            assert!(export(&linear, &preset.clone().with_metadata(false), &metadata).is_ok());
        }

        // A batch spanning two different spaces cannot embed one profile in
        // both: the preset whose space the profile describes takes it, and the
        // other is refused before anything is written.
        let other = ColorSpace::IccProfile {
            asset_hash: blake3::hash(b"some other display's profile")
                .to_hex()
                .to_string(),
        };
        assert_ne!(other, space);
        let presets = [
            ExportPreset::pass_through("mine", ExportFormat::Png, space.clone()),
            ExportPreset::pass_through("theirs", ExportFormat::Png, other),
        ];
        let err = export_batch(&working, &presets, &metadata).unwrap_err();
        assert!(matches!(err, ExportError::Preset { .. }), "{err}");

        let dir = temp_dir("icc-batch");
        assert!(export_batch_to_dir(&dir, &working, &presets, &metadata).is_err());
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            0,
            "the matching preset was written before the batch was refused"
        );
        let _ = std::fs::remove_dir_all(&dir);

        // On its own the matching preset does embed it, so this is a check on
        // the relationship and not a blanket refusal.
        let file = export(&working, &presets[0], &metadata).unwrap();
        assert_eq!(
            decode_surface_bytes(&file.bytes, ImportLimits::default())
                .unwrap()
                .icc_profile
                .as_deref(),
            Some(profile.as_slice())
        );
    }

    #[test]
    fn a_batch_runs_every_preset_in_one_call() {
        let image = one_pixel([0.18, 0.18, 0.18, 1.0]);
        let presets = vec![
            ExportPreset::new("web", ExportFormat::Png),
            ExportPreset::new("web-2x", ExportFormat::Png).with_scale(2.0),
            ExportPreset::new("photo", ExportFormat::Jpeg(85)),
            ExportPreset::new("archive", ExportFormat::Tiff).with_bit_depth(BitDepth::Sixteen),
            ExportPreset::new("thumb", ExportFormat::WebP),
        ];
        let files = export_batch(&image, &presets, &ExportMetadata::default()).unwrap();
        assert_eq!(files.len(), 5);
        assert_eq!(
            files.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
            [
                "web.png",
                "web-2x.png",
                "photo.jpg",
                "archive.tif",
                "thumb.webp"
            ]
        );
        assert_eq!((files[1].width, files[1].height), (2, 2));
        for file in &files {
            let decoded = decode_surface_bytes(&file.bytes, ImportLimits::default()).unwrap();
            assert_eq!((decoded.width, decoded.height), (file.width, file.height));
        }
        // A batch produces exactly what the presets produce one at a time.
        for (preset, file) in presets.iter().zip(&files) {
            let single = export(&image, preset, &ExportMetadata::default()).unwrap();
            assert_eq!(&single, file, "batch differed from a single export");
        }
    }

    /// The same batch, written straight to disk, byte for byte.
    #[test]
    fn a_batch_can_write_straight_to_a_directory() {
        let dir = temp_dir("batch");
        let image = one_pixel([0.18, 0.18, 0.18, 1.0]);
        let presets = vec![
            ExportPreset::new("web", ExportFormat::Png),
            ExportPreset::new("web-2x", ExportFormat::Png).with_scale(2.0),
            ExportPreset::new("photo", ExportFormat::Jpeg(85)),
        ];
        let paths =
            export_batch_to_dir(&dir, &image, &presets, &ExportMetadata::default()).unwrap();
        let in_memory = export_batch(&image, &presets, &ExportMetadata::default()).unwrap();
        assert_eq!(paths.len(), 3);
        for (path, file) in paths.iter().zip(&in_memory) {
            assert_eq!(path.file_name().unwrap().to_str().unwrap(), file.name);
            assert_eq!(std::fs::read(path).unwrap(), file.bytes, "{path:?}");
        }
        // Nothing else was left in the directory — no temporary files.
        let entries: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries.len(), 3, "stray files: {entries:?}");

        // A hostile preset name is written inside the directory, not outside.
        let evil = [ExportPreset::new("../../escaped", ExportFormat::Png)];
        let paths = export_batch_to_dir(&dir, &image, &evil, &ExportMetadata::default()).unwrap();
        assert_eq!(paths[0].parent().unwrap(), dir);
        assert!(paths[0].exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A batch's peak memory must not grow with the number of presets.
    ///
    /// Measured before the fix: the resample cache was a `Vec` that was only
    /// ever pushed to, so N presets at N distinct target sizes held N float
    /// buffers at once — each bounded only by `MAX_OUTPUT_PIXELS * 16` = 4 GiB,
    /// with both the preset count and the scale factors coming from a project
    /// file someone else wrote. `export_batch_to_dir` was documented as the
    /// memory-bounded path while doing exactly this.
    ///
    /// Peak, not total: total allocation is the same either way, because the
    /// same N resamples happen. The question is how many are alive at once.
    #[test]
    fn a_batch_holds_one_scaled_image_at_a_time() {
        // 128x128 floats is 256 KiB in; each preset scales to roughly 512x512,
        // which is about 4 MiB of floats.
        let image = LinearImage::transparent(128, 128).unwrap();
        let presets_for = |n: usize| -> Vec<ExportPreset> {
            (0..n)
                .map(|i| {
                    ExportPreset::new(format!("p{i}"), ExportFormat::Png)
                        .with_scale(4.0 + i as f32 * 0.05)
                })
                .collect()
        };
        let peak_for = |n: usize| -> u64 {
            let presets = presets_for(n);
            let (files, peak) = crate::alloc_probe::measure_peak(|| {
                export_batch(&image, &presets, &ExportMetadata::default()).unwrap()
            });
            assert_eq!(files.len(), n);
            // Every preset really does ask for its own size, so none of them
            // share a resample and the grouping cannot hide behind reuse.
            let mut sizes: Vec<(u32, u32)> = files.iter().map(|f| (f.width, f.height)).collect();
            sizes.sort_unstable();
            sizes.dedup();
            assert_eq!(
                sizes.len(),
                n,
                "the presets must ask for {n} distinct sizes"
            );
            peak
        };

        let two = peak_for(2);
        let ten = peak_for(10);
        assert!(
            ten < two * 2,
            "peak went from {two} bytes at two presets to {ten} at ten; the scaled \
             images are being retained"
        );
        // ...and in absolute terms: ten retained 512x512 float buffers is
        // 42 MiB, one is 4 MiB.
        assert!(
            ten < (16 << 20),
            "a ten-preset batch peaked at {ten} bytes against a ~4 MiB scaled image"
        );

        // The same bound on the path whose documentation makes the claim.
        let dir = temp_dir("peak");
        let presets = presets_for(10);
        let (paths, peak) = crate::alloc_probe::measure_peak(|| {
            export_batch_to_dir(&dir, &image, &presets, &ExportMetadata::default()).unwrap()
        });
        assert_eq!(paths.len(), 10);
        assert!(
            peak < (16 << 20),
            "export_batch_to_dir peaked at {peak} bytes for ten presets"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Grouping presets by target size must not reorder the results.
    #[test]
    fn a_batch_returns_results_in_preset_order() {
        let image = LinearImage::transparent(8, 8).unwrap();
        // Interleaved sizes, with a repeat, so the groups do not run in the
        // order the presets are listed in.
        let presets = [
            ExportPreset::new("half-a", ExportFormat::Png).with_scale(0.5),
            ExportPreset::new("one-a", ExportFormat::Png),
            ExportPreset::new("double", ExportFormat::Png).with_scale(2.0),
            ExportPreset::new("half-b", ExportFormat::Bmp).with_scale(0.5),
            ExportPreset::new("one-b", ExportFormat::Bmp),
        ];
        let files = export_batch(&image, &presets, &ExportMetadata::default()).unwrap();
        assert_eq!(
            files.iter().map(|f| f.name.as_str()).collect::<Vec<_>>(),
            [
                "half-a.png",
                "one-a.png",
                "double.png",
                "half-b.bmp",
                "one-b.bmp"
            ]
        );
        assert_eq!(
            files
                .iter()
                .map(|f| (f.width, f.height))
                .collect::<Vec<_>>(),
            [(4, 4), (8, 8), (16, 16), (4, 4), (8, 8)]
        );
        // ...and each is what that preset produces on its own.
        for (preset, file) in presets.iter().zip(&files) {
            assert_eq!(
                &export(&image, preset, &ExportMetadata::default()).unwrap(),
                file
            );
        }
    }

    #[test]
    fn a_batch_refuses_to_overwrite_its_own_output() {
        let image = LinearImage::transparent(2, 2).unwrap();
        let presets = [
            ExportPreset::new("same", ExportFormat::Png),
            ExportPreset::new("same", ExportFormat::Png),
        ];
        let err = export_batch(&image, &presets, &ExportMetadata::default()).unwrap_err();
        assert!(matches!(err, ExportError::DuplicateOutput(n) if n == "same.png"));

        // ...including when two different names sanitise to the same thing.
        let presets = [
            ExportPreset::new("a/b", ExportFormat::Png),
            ExportPreset::new("a:b", ExportFormat::Png),
        ];
        assert!(export_batch(&image, &presets, &ExportMetadata::default()).is_err());

        // Different containers are not a collision.
        let presets = [
            ExportPreset::new("same", ExportFormat::Png),
            ExportPreset::new("same", ExportFormat::Bmp),
        ];
        assert!(export_batch(&image, &presets, &ExportMetadata::default()).is_ok());
    }

    #[test]
    fn one_bad_preset_fails_the_whole_batch_before_any_work() {
        let dir = temp_dir("bad-batch");
        let image = LinearImage::transparent(2, 2).unwrap();
        let presets = [
            ExportPreset::new("good", ExportFormat::Png),
            ExportPreset::new("bad", ExportFormat::Jpeg(0)),
        ];
        assert!(export_batch(&image, &presets, &ExportMetadata::default()).is_err());
        // ...and the on-disk batch writes nothing at all, not even the good one.
        assert!(export_batch_to_dir(&dir, &image, &presets, &ExportMetadata::default()).is_err());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // -------------------------------------------------------- name safety

    #[test]
    fn a_preset_name_can_never_escape_its_directory() {
        let hostile = [
            "../../../etc/passwd",
            "..\\..\\windows\\system32\\config\\sam",
            "/absolute/path",
            "C:\\Windows\\notepad",
            "..",
            ".",
            "...",
            ".hidden",
            "name\0with\0nul",
            "shell$(rm -rf)",
            "trailing/",
            "COM1",
            "nul",
            "",
            "   ",
            "\u{202e}gnp.exe",
        ];
        for raw in hostile {
            let stem = sanitize_file_stem(raw);
            assert!(!stem.is_empty(), "{raw:?} produced an empty stem");
            assert!(
                !stem.contains('/') && !stem.contains('\\'),
                "{raw:?} -> {stem:?} kept a separator"
            );
            assert!(!stem.contains(".."), "{raw:?} -> {stem:?} kept a ..");
            assert!(!stem.contains('.'), "{raw:?} -> {stem:?} kept a dot");
            assert!(!stem.contains('\0'), "{raw:?} -> {stem:?} kept a NUL");
            assert!(
                !stem.starts_with(' ') && !stem.ends_with(' '),
                "{raw:?} -> {stem:?} has edge whitespace"
            );
            assert!(
                !RESERVED_STEMS.contains(&stem.to_ascii_lowercase().as_str()),
                "{raw:?} -> {stem:?} is a reserved device name"
            );

            // And the name a preset actually writes is the stem plus exactly
            // one extension, so joining it onto a directory stays inside it.
            let name = ExportPreset::new(raw, ExportFormat::Png).file_name();
            let joined = std::path::Path::new("out").join(&name);
            assert_eq!(
                joined.components().count(),
                2,
                "{raw:?} -> {name:?} added path components"
            );
            assert_eq!(joined.parent(), Some(std::path::Path::new("out")));
        }
    }

    #[test]
    fn ordinary_names_survive_intact() {
        assert_eq!(sanitize_file_stem("web-2x"), "web-2x");
        assert_eq!(
            sanitize_file_stem("Hero Image (final)"),
            "Hero Image (final)"
        );
        assert_eq!(sanitize_file_stem("icon_512"), "icon_512");
        // Length is capped, so a preset name cannot blow past a filesystem's
        // component limit.
        assert!(sanitize_file_stem(&"a".repeat(500)).len() <= 64);
    }

    // ------------------------------------------------------------- buffers

    #[test]
    fn a_mismatched_buffer_is_rejected() {
        assert!(LinearImage::from_premultiplied(2, 2, vec![0.0; 8]).is_err());
        assert!(LinearImage::from_premultiplied(0, 2, vec![]).is_err());
        assert!(LinearImage::from_premultiplied(2, 2, vec![0.0; 16]).is_ok());
    }

    /// `u32::MAX * u32::MAX * 4` does not fit in a `u64`. Debug builds panic on
    /// an overflowing multiply, so the size checks have to saturate rather than
    /// wrap — otherwise the guard against an absurd size *is* the crash.
    #[test]
    fn absurd_dimensions_are_rejected_rather_than_overflowing() {
        assert!(LinearImage::from_premultiplied(u32::MAX, u32::MAX, vec![0.0; 4]).is_err());
        assert!(LinearImage::transparent(u32::MAX, u32::MAX).is_err());
        assert!(crate::codec::encode(ExportFormat::Png, u32::MAX, u32::MAX, &[0u8; 4]).is_err());
    }

    /// A 1:1 preset must not copy the working buffer just to hand it to the
    /// encoder. Float RGBA is 16 bytes a pixel, so the copy the resampler used
    /// to make was four times the size of the 8-bit buffer the encode needs.
    #[test]
    fn a_one_to_one_export_does_not_copy_the_working_buffer() {
        const N: u32 = 512;
        let image = LinearImage::from_premultiplied(N, N, vec![0.5; (N * N * 4) as usize]).unwrap();
        let floats = std::mem::size_of_val(image.pixels());
        assert_eq!(floats, 4 << 20);

        let (file, allocated) = crate::alloc_probe::measure(|| {
            export(
                &image,
                &ExportPreset::new("one-to-one", ExportFormat::Png),
                &ExportMetadata::default(),
            )
            .unwrap()
        });
        assert_eq!((file.width, file.height), (N, N));
        assert!(
            allocated < (floats / 2) as u64,
            "a 1:1 export allocated {allocated} bytes against a {floats}-byte working buffer"
        );
    }

    #[test]
    fn straight_alpha_input_is_premultiplied_on_the_way_in() {
        let straight = LinearImage::from_straight(1, 1, vec![1.0, 0.5, 0.0, 0.5]).unwrap();
        assert_eq!(straight.pixels(), &[0.5, 0.25, 0.0, 0.5]);
        // ...and comes back out identical.
        assert_eq!(
            rgba8_from_linear(&straight, &ColorSpace::LinearSrgb).unwrap(),
            vec![255, 128, 0, 128]
        );
    }
}
