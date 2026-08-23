//! Codec facade: the one place the app talks to a third-party image codec.
//!
//! Everything above this module deals in [`DecodedSurface`] / [`EncodedPixels`]
//! and never names `image`, so adding or swapping a backing codec is a change
//! here and nowhere else.
//!
//! # What this module does and does not do
//!
//! This module moves **encoded** pixels: the bytes a file actually stores,
//! gamma-encoded and straight-alpha. It performs no colour management of its
//! own beyond carrying an ICC profile around. The compositor works in *linear
//! premultiplied* float, which is a different thing entirely, and handing those
//! floats straight to [`encode`] would produce a visibly wrong file. The
//! conversion between the two lives in [`crate::export`], and
//! [`DecodedSurface::to_linear_premultiplied`] is the decode-side inverse.
//!
//! # Untrusted input
//!
//! Import runs on files from other people. Every decode entry point takes an
//! [`ImportLimits`] and checks the header-declared dimensions *before* any
//! pixel buffer is allocated, so a four-byte width field cannot make the
//! process reserve gigabytes. [`ImportLimits::default`] is the policy for
//! untrusted content; [`ImportLimits::permissive`] exists for content the app
//! itself produced.
//!
//! Nothing here opens a path derived from file content: [`encode_to_path`]
//! writes exactly the path it is given, and the file *names* suggested by
//! export presets are sanitised in [`crate::export`].
//!
//! Nor does an export destroy what is already at that path if it fails:
//! [`encode_to_path`] encodes into a temporary file beside the destination and
//! renames it over the top only once the bytes are on disk.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Cursor, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use color::ColorSpace;
use image::{ExtendedColorType, ImageDecoder, ImageEncoder};

use crate::export::ExportError;
use crate::format::PixelFormat;

/// A decoded image in packed RGBA8, plus dimensions.
///
/// The convenience shape: always 8 bits per channel, whatever the file held.
/// Use [`decode_surface_bytes`] when a 16-bit source must stay 16-bit.
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    /// Row-major RGBA8, `width * height * 4` bytes, straight (non-premultiplied)
    /// alpha in the file's own encoding — *not* linear.
    pub rgba8: Vec<u8>,
    /// The colour space the pixels are encoded in. [`ColorSpace::IccProfile`]
    /// when the file carried a profile, with the profile bytes in
    /// [`DecodedImage::icc_profile`].
    pub color_space: ColorSpace,
    /// The raw embedded ICC profile, if the file had one.
    pub icc_profile: Option<Vec<u8>>,
}

/// Pixel storage of a decoded surface, at the depth the file actually used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurfacePixels {
    /// Row-major RGBA, 8 bits per channel.
    Rgba8(Vec<u8>),
    /// Row-major RGBA, 16 bits per channel, host endianness.
    Rgba16(Vec<u16>),
}

impl SurfacePixels {
    /// The [`PixelFormat`] this storage corresponds to.
    pub fn format(&self) -> PixelFormat {
        match self {
            SurfacePixels::Rgba8(_) => PixelFormat::Rgba8,
            SurfacePixels::Rgba16(_) => PixelFormat::Rgba16,
        }
    }

    /// Number of pixels held.
    pub fn pixel_count(&self) -> usize {
        match self {
            SurfacePixels::Rgba8(v) => v.len() / 4,
            SurfacePixels::Rgba16(v) => v.len() / 4,
        }
    }

    /// Down-convert to RGBA8, consuming the storage. 8-bit input is moved, not
    /// copied.
    pub fn into_rgba8(self) -> Vec<u8> {
        match self {
            SurfacePixels::Rgba8(v) => v,
            // 16 -> 8 is a *round*, not a truncation: `>> 8` maps 65535 to 255
            // but also maps 65280..=65535 to 255 while mapping 0..=255 to 0,
            // which loses a half-step of white and darkens the whole ramp.
            SurfacePixels::Rgba16(v) => v
                .into_iter()
                .map(|c| ((c as u32 * 255 + 32_767) / 65_535) as u8)
                .collect(),
        }
    }
}

/// A decoded image at the depth the file used, with its colour metadata intact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedSurface {
    pub width: u32,
    pub height: u32,
    /// Pixels, straight alpha, in the file's own encoding.
    pub pixels: SurfacePixels,
    /// The space [`DecodedSurface::pixels`] are encoded in.
    pub color_space: ColorSpace,
    /// The raw embedded ICC profile, if the file had one.
    pub icc_profile: Option<Vec<u8>>,
    /// The container format the pixels came out of.
    pub source_format: ImportFormat,
}

impl DecodedSurface {
    /// The storage format of [`DecodedSurface::pixels`].
    pub fn format(&self) -> PixelFormat {
        self.pixels.format()
    }

    /// Collapse to the 8-bit convenience shape.
    pub fn into_decoded_image(self) -> DecodedImage {
        DecodedImage {
            width: self.width,
            height: self.height,
            rgba8: self.pixels.into_rgba8(),
            color_space: self.color_space,
            icc_profile: self.icc_profile,
        }
    }

    /// Convert into the compositor's working representation: linear,
    /// premultiplied `f32` RGBA.
    ///
    /// This is the exact inverse of what [`crate::export`] does on the way out.
    ///
    /// A surface whose space is [`ColorSpace::IccProfile`] **fails** with
    /// [`crate::export::ExportError::Color`]: no ICC engine is linked, so there
    /// is no transform into the working space, and returning the file's own
    /// samples relabelled as linear would silently brighten every midtone the
    /// moment they were re-encoded. Use
    /// [`DecodedSurface::to_premultiplied_pass_through`] to edit and re-export
    /// such a document in its own space.
    pub fn to_linear_premultiplied(&self) -> Result<crate::export::LinearImage, ExportError> {
        match &self.pixels {
            SurfacePixels::Rgba8(v) => {
                crate::export::linear_from_rgba8(self.width, self.height, v, &self.color_space)
            }
            SurfacePixels::Rgba16(v) => {
                crate::export::linear_from_rgba16(self.width, self.height, v, &self.color_space)
            }
        }
    }

    /// Premultiply the file's samples without converting them.
    ///
    /// The result is a [`crate::export::LinearImage`] by type only: the samples
    /// still carry the file's own transfer function. It is the input half of a
    /// pass-through export — see [`crate::export::ColorHandling::PassThrough`] —
    /// and the only way to move an ICC-tagged document through this pipeline
    /// without inventing a transform for it.
    pub fn to_premultiplied_pass_through(&self) -> crate::export::LinearImage {
        let result = match &self.pixels {
            SurfacePixels::Rgba8(v) => {
                crate::export::linear_from_rgba8_pass_through(self.width, self.height, v)
            }
            SurfacePixels::Rgba16(v) => {
                crate::export::linear_from_rgba16_pass_through(self.width, self.height, v)
            }
        };
        result.expect("a decoded surface always has width*height*4 samples")
    }
}

/// Header facts about an image, obtained without decoding any pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageInfo {
    pub width: u32,
    pub height: u32,
    /// The container format.
    pub format: ImportFormat,
    /// The storage format a full decode would produce.
    pub pixel_format: PixelFormat,
    /// The raw embedded ICC profile, if the file has one.
    pub icc_profile: Option<Vec<u8>>,
}

/// Container formats the importer accepts.
///
/// Multi-image containers are read as a **single** image: the first frame of an
/// animated GIF or WebP, the first page of a multi-page TIFF, the first entry of
/// an ICO. Nothing in this crate models an image sequence, and silently
/// concatenating frames would be worse than taking the first one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImportFormat {
    Png,
    Jpeg,
    WebP,
    Tiff,
    /// Read as the first frame; animation is not modelled.
    Gif,
    Bmp,
    /// Read as the first entry in the icon directory.
    Ico,
    /// No magic number — see [`ImportFormat::is_self_identifying`].
    Tga,
}

impl ImportFormat {
    /// Every format the importer can read.
    pub const ALL: [ImportFormat; 8] = [
        ImportFormat::Png,
        ImportFormat::Jpeg,
        ImportFormat::WebP,
        ImportFormat::Tiff,
        ImportFormat::Gif,
        ImportFormat::Bmp,
        ImportFormat::Ico,
        ImportFormat::Tga,
    ];

    /// Short stable name for logs and UI.
    pub fn name(self) -> &'static str {
        match self {
            ImportFormat::Png => "PNG",
            ImportFormat::Jpeg => "JPEG",
            ImportFormat::WebP => "WebP",
            ImportFormat::Tiff => "TIFF",
            ImportFormat::Gif => "GIF",
            ImportFormat::Bmp => "BMP",
            ImportFormat::Ico => "ICO",
            ImportFormat::Tga => "TGA",
        }
    }

    /// Whether the format can be recognised from its leading bytes.
    ///
    /// TGA is the one that cannot: the format has no magic number at the start
    /// of the file, so a TGA can only be identified by its extension or by an
    /// explicit hint. Decoding one from an anonymous byte slice therefore
    /// requires [`decode_surface_bytes_as`].
    pub fn is_self_identifying(self) -> bool {
        !matches!(self, ImportFormat::Tga)
    }

    /// Match a file extension, case-insensitively and without a leading dot.
    pub fn from_extension(ext: &str) -> Option<Self> {
        Some(match ext.to_ascii_lowercase().as_str() {
            "png" | "apng" => ImportFormat::Png,
            "jpg" | "jpeg" | "jpe" | "jfif" => ImportFormat::Jpeg,
            "webp" => ImportFormat::WebP,
            "tif" | "tiff" => ImportFormat::Tiff,
            "gif" => ImportFormat::Gif,
            "bmp" | "dib" => ImportFormat::Bmp,
            "ico" | "cur" => ImportFormat::Ico,
            "tga" | "targa" | "icb" | "vda" | "vst" => ImportFormat::Tga,
            _ => return None,
        })
    }

    fn from_image(format: image::ImageFormat) -> Option<Self> {
        Some(match format {
            image::ImageFormat::Png => ImportFormat::Png,
            image::ImageFormat::Jpeg => ImportFormat::Jpeg,
            image::ImageFormat::WebP => ImportFormat::WebP,
            image::ImageFormat::Tiff => ImportFormat::Tiff,
            image::ImageFormat::Gif => ImportFormat::Gif,
            image::ImageFormat::Bmp => ImportFormat::Bmp,
            image::ImageFormat::Ico => ImportFormat::Ico,
            image::ImageFormat::Tga => ImportFormat::Tga,
            _ => return None,
        })
    }

    fn to_image(self) -> image::ImageFormat {
        match self {
            ImportFormat::Png => image::ImageFormat::Png,
            ImportFormat::Jpeg => image::ImageFormat::Jpeg,
            ImportFormat::WebP => image::ImageFormat::WebP,
            ImportFormat::Tiff => image::ImageFormat::Tiff,
            ImportFormat::Gif => image::ImageFormat::Gif,
            ImportFormat::Bmp => image::ImageFormat::Bmp,
            ImportFormat::Ico => image::ImageFormat::Ico,
            ImportFormat::Tga => image::ImageFormat::Tga,
        }
    }
}

/// The format hint taken from a path's extension, if it names one we read.
fn hint_from_path(path: &Path) -> Option<ImportFormat> {
    path.extension()
        .and_then(|e| e.to_str())
        .and_then(ImportFormat::from_extension)
}

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("image decode/encode failed: {0}")]
    Image(#[from] image::ImageError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("unsupported format for path: {0}")]
    Unsupported(String),
    /// The supplied pixel buffer does not match the stated dimensions.
    #[error("pixel buffer size mismatch: {0}")]
    BufferSize(String),
    /// A parameter the caller chose is out of range (e.g. JPEG quality).
    #[error("invalid parameter: {0}")]
    InvalidParameter(String),
    /// The file declares something larger than [`ImportLimits`] allows.
    #[error("import limit exceeded: {0}")]
    LimitExceeded(String),
}

/// Allocation bounds applied to untrusted input before any pixel buffer exists.
///
/// A decoder is handed a header written by whoever made the file. Left
/// unchecked, `width = 0x7fffffff` in a four-byte PNG field asks for a
/// multi-terabyte allocation. Every field here is checked against the
/// *declared* dimensions, before decoding, so a malicious header fails fast
/// with [`CodecError::LimitExceeded`] instead of aborting the process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportLimits {
    /// Largest accepted width, in pixels.
    pub max_width: u32,
    /// Largest accepted height, in pixels.
    pub max_height: u32,
    /// Largest accepted pixel count. Guards the `w * h` product, which two
    /// individually reasonable dimensions can still blow up.
    pub max_pixels: u64,
    /// Ceiling on what a decode may allocate for pixels, in bytes.
    ///
    /// Checked against what *this pipeline* will allocate, which is not the
    /// same number as the size of the file's own colour type: every decode is
    /// converted to RGBA8 or RGBA16, and during that conversion the source
    /// buffer and the converted one are both live. A 2048x2048 grayscale-8 PNG
    /// declares 4 MiB of source pixels and materialises a 16 MiB RGBA8 buffer
    /// on top of it, so the number checked here is the sum — see
    /// [`decode_alloc_bytes`].
    ///
    /// Handed to the backing decoder as well as checked here, but see
    /// [`ImportLimits::max_icc_bytes`] for one allocation it does not reach.
    pub max_alloc_bytes: u64,
    /// Ceiling on a **retained** ICC profile, in bytes. Real profiles are a few
    /// kilobytes; the field that declares one is 32 bits wide.
    ///
    /// # This is a retention filter, not an allocation bound
    ///
    /// The backing decoder materialises the whole profile — decompressing a
    /// PNG `iCCP` chunk in full — before this crate ever sees it, so by the
    /// time the length is compared the allocation has already happened. The
    /// effective ceiling on that allocation is the backing codec's own, not
    /// this field: `image` 0.25's `PngDecoder::set_limits` carries an upstream
    /// TODO saying it does not propagate limits into the `png` crate, so an
    /// `iCCP` chunk is inflated under `png`'s default budget however small a
    /// number is written here.
    ///
    /// Setting a small value therefore bounds what the *editor* carries
    /// around, and nothing else. `an_oversized_icc_profile_is_dropped_but_was_
    /// already_allocated` measures exactly that and will fail if the situation
    /// ever improves, at which point this doc is the thing to fix.
    ///
    /// A profile over the ceiling is dropped, not treated as a bad file: the
    /// pixels are unaffected and no ICC engine is linked to use the profile
    /// with anyway.
    pub max_icc_bytes: usize,
}

impl Default for ImportLimits {
    /// The desktop-editor policy: 65 535 pixels per side, 268 megapixels, four
    /// gibibytes of decoder allocation, 16 MiB of ICC.
    ///
    /// 268 Mpx is a 16 384 x 16 384 image, which comfortably covers stitched
    /// panoramas and gigapixel scans; four gibibytes is exactly what a 268 Mpx
    /// RGBA16 decode holds at its peak — 2 GiB of source samples plus the 2 GiB
    /// converted buffer, which is the sum [`decode_alloc_bytes`] computes.
    /// These are chosen so a *legitimate* file a person
    /// deliberately opened is never refused, while a header claiming
    /// `0x7fffffff` per side still fails before allocating anything.
    ///
    /// They are **not** tight enough for decoding images arriving unattended
    /// from a network — a few-kilobyte crafted PNG can still ask for a
    /// gigabyte here. Such a caller should construct its own limits rather than
    /// take this default.
    fn default() -> Self {
        ImportLimits {
            max_width: 65_535,
            max_height: 65_535,
            max_pixels: 1 << 28,
            max_alloc_bytes: 4u64 << 30,
            max_icc_bytes: 16 << 20,
        }
    }
}

impl ImportLimits {
    /// Bounds wide enough for content the app produced itself.
    ///
    /// Still bounded — "trusted" is not "unbounded" — but wide enough that a
    /// legitimate document never trips it.
    pub fn permissive() -> Self {
        ImportLimits {
            max_width: 300_000,
            max_height: 300_000,
            max_pixels: 1 << 32,
            max_alloc_bytes: 32u64 << 30,
            max_icc_bytes: 64 << 20,
        }
    }

    fn to_image_limits(self) -> image::Limits {
        let mut limits = image::Limits::no_limits();
        limits.max_image_width = Some(self.max_width);
        limits.max_image_height = Some(self.max_height);
        limits.max_alloc = Some(self.max_alloc_bytes);
        limits
    }

    fn check_dimensions(self, width: u32, height: u32) -> Result<(), CodecError> {
        if width == 0 || height == 0 {
            return Err(CodecError::LimitExceeded(format!(
                "image declares an empty dimension: {width}x{height}"
            )));
        }
        if width > self.max_width || height > self.max_height {
            return Err(CodecError::LimitExceeded(format!(
                "image is {width}x{height}, limit is {}x{}",
                self.max_width, self.max_height
            )));
        }
        let pixels = u64::from(width) * u64::from(height);
        if pixels > self.max_pixels {
            return Err(CodecError::LimitExceeded(format!(
                "image has {pixels} pixels, limit is {}",
                self.max_pixels
            )));
        }
        Ok(())
    }

    fn check_alloc(self, bytes: u64) -> Result<(), CodecError> {
        if bytes > self.max_alloc_bytes {
            return Err(CodecError::LimitExceeded(format!(
                "decoding needs {bytes} bytes, limit is {}",
                self.max_alloc_bytes
            )));
        }
        Ok(())
    }

    /// Decide whether to keep a profile the backing decoder has *already*
    /// allocated.
    ///
    /// `profile` arrives fully materialised — see [`ImportLimits::max_icc_bytes`],
    /// which explains at length why this cannot be an allocation bound. An
    /// over-large profile is dropped rather than failing the import: the pixels
    /// are still perfectly good, and no ICC engine is linked anyway.
    fn take_icc(self, profile: Option<Vec<u8>>) -> Option<Vec<u8>> {
        profile.filter(|p| !p.is_empty() && p.len() <= self.max_icc_bytes)
    }
}

/// The colour space to record for a source that carried `profile`.
///
/// A profile is referenced by content hash, matching how
/// [`ColorSpace::IccProfile`] is defined: identical profiles across many
/// imported files collapse to one asset.
fn color_space_for(profile: Option<&Vec<u8>>) -> ColorSpace {
    match profile {
        Some(bytes) => ColorSpace::IccProfile {
            asset_hash: blake3::hash(bytes).to_hex().to_string(),
        },
        None => ColorSpace::Srgb,
    }
}

/// Build a reader, letting content sniffing override the caller's hint.
///
/// The hint is set first and `with_guessed_format` second, so a file whose
/// *content* identifies it wins over whatever its extension claimed. That is
/// the safe precedence for untrusted input: a `.png` full of JPEG is decoded as
/// the JPEG it is, not as the PNG it pretends to be. The hint only decides
/// formats that carry no magic number at all — in practice TGA.
fn reader_for<R: BufRead + Seek>(
    source: R,
    limits: ImportLimits,
    hint: Option<ImportFormat>,
) -> Result<image::ImageReader<R>, CodecError> {
    let mut reader = image::ImageReader::new(source);
    if let Some(hint) = hint {
        reader.set_format(hint.to_image());
    }
    let mut reader = reader.with_guessed_format()?;
    reader.limits(limits.to_image_limits());
    Ok(reader)
}

fn import_format_of<R: BufRead + Seek>(
    reader: &image::ImageReader<R>,
) -> Result<ImportFormat, CodecError> {
    let format = reader
        .format()
        .ok_or_else(|| CodecError::Unsupported("could not identify the image format".into()))?;
    ImportFormat::from_image(format).ok_or_else(|| {
        CodecError::Unsupported(format!(
            "{format:?} is not one of the supported import formats"
        ))
    })
}

/// Read only the header of an image: dimensions, format and ICC profile.
///
/// This is the cheap half of a decode. For a multi-hundred-megabyte file it
/// touches a few kilobytes, which is what lets a caller decide whether to
/// commit to the full decode at all.
pub fn probe_reader<R: BufRead + Seek>(
    source: R,
    limits: ImportLimits,
) -> Result<ImageInfo, CodecError> {
    probe_reader_with_hint(source, limits, None)
}

/// [`probe_reader`], with a format hint for containers that cannot be sniffed.
pub fn probe_reader_with_hint<R: BufRead + Seek>(
    source: R,
    limits: ImportLimits,
    hint: Option<ImportFormat>,
) -> Result<ImageInfo, CodecError> {
    let reader = reader_for(source, limits, hint)?;
    let format = import_format_of(&reader)?;
    let mut decoder = reader.into_decoder()?;
    let (width, height) = decoder.dimensions();
    limits.check_dimensions(width, height)?;
    let pixel_format = storage_format_for(decoder.color_type());
    let icc = limits.take_icc(decoder.icc_profile()?);
    Ok(ImageInfo {
        width,
        height,
        format,
        pixel_format,
        icc_profile: icc,
    })
}

/// [`probe_reader`] over a file, reading incrementally rather than slurping it.
///
/// The extension supplies the format hint, so a `.tga` — which has no magic
/// number — probes correctly here even though it cannot be sniffed from bytes.
pub fn probe_path(path: &Path, limits: ImportLimits) -> Result<ImageInfo, CodecError> {
    probe_reader_with_hint(
        BufReader::new(File::open(path)?),
        limits,
        hint_from_path(path),
    )
}

/// [`probe_reader`] over an in-memory buffer.
pub fn probe_bytes(bytes: &[u8], limits: ImportLimits) -> Result<ImageInfo, CodecError> {
    probe_reader(Cursor::new(bytes), limits)
}

/// [`probe_bytes`] with an explicit container format.
pub fn probe_bytes_as(
    bytes: &[u8],
    limits: ImportLimits,
    format: ImportFormat,
) -> Result<ImageInfo, CodecError> {
    probe_reader_with_hint(Cursor::new(bytes), limits, Some(format))
}

/// Bytes a decode of `width` x `height` from `color_type` will hold at once.
///
/// Two buffers, not one. The backing decoder fills a buffer in the file's own
/// colour type (`source_bytes`, which is what `ImageDecoder::total_bytes`
/// reports), and this crate then converts that into the RGBA storage every
/// caller above it expects. During the conversion both are live, so the peak is
/// their sum — and for a narrow source it is dominated by the *converted* half:
/// an 8-bit grayscale image converts to RGBA8 at four times its own size, and
/// `total_bytes` alone under-counts the peak by that factor.
///
/// Saturating throughout: the dimensions have not necessarily been checked yet,
/// and a header claiming `0x7fffffff` per side must produce a number that fails
/// [`ImportLimits::check_alloc`] rather than an overflow panic.
fn decode_alloc_bytes(
    width: u32,
    height: u32,
    color_type: image::ColorType,
    source_bytes: u64,
) -> u64 {
    let converted = u64::from(width)
        .saturating_mul(u64::from(height))
        .saturating_mul(storage_format_for(color_type).bytes_per_pixel() as u64);
    source_bytes.saturating_add(converted)
}

/// Which storage depth a decoder's colour type maps onto.
fn storage_format_for(color_type: image::ColorType) -> PixelFormat {
    use image::ColorType::*;
    match color_type {
        L16 | La16 | Rgb16 | Rgba16 => PixelFormat::Rgba16,
        // 32-bit float sources are down-converted to 8-bit for now; nothing in
        // the pipeline stores `RgbaF32` yet, and claiming otherwise here would
        // hand callers a format the rest of the crate cannot round-trip.
        _ => PixelFormat::Rgba8,
    }
}

/// Decode from any seekable stream, preserving bit depth and ICC profile.
///
/// This is the streaming entry point: the file is pulled through `source`
/// incrementally, so decoding a 500 MB TIFF never holds 500 MB of *file* in
/// memory on top of the pixel buffer. The pixel buffer itself is bounded by
/// `limits`, checked against the declared dimensions before it is allocated.
pub fn decode_surface_reader<R: BufRead + Seek>(
    source: R,
    limits: ImportLimits,
) -> Result<DecodedSurface, CodecError> {
    decode_surface_reader_with_hint(source, limits, None)
}

/// [`decode_surface_reader`], with a format hint for containers that cannot be
/// sniffed from their leading bytes (see [`ImportFormat::is_self_identifying`]).
pub fn decode_surface_reader_with_hint<R: BufRead + Seek>(
    source: R,
    limits: ImportLimits,
    hint: Option<ImportFormat>,
) -> Result<DecodedSurface, CodecError> {
    let reader = reader_for(source, limits, hint)?;
    let source_format = import_format_of(&reader)?;
    let mut decoder = reader.into_decoder()?;

    let (width, height) = decoder.dimensions();
    limits.check_dimensions(width, height)?;
    // Not `decoder.total_bytes()`: that is the source colour type's size, and
    // the conversion below allocates an RGBA buffer on top of it.
    limits.check_alloc(decode_alloc_bytes(
        width,
        height,
        decoder.color_type(),
        decoder.total_bytes(),
    ))?;

    let icc_profile = limits.take_icc(decoder.icc_profile()?);
    let color_space = color_space_for(icc_profile.as_ref());
    let wants_16 = storage_format_for(decoder.color_type()) == PixelFormat::Rgba16;

    let dynamic = image::DynamicImage::from_decoder(decoder)?;
    let pixels = if wants_16 {
        SurfacePixels::Rgba16(dynamic.into_rgba16().into_raw())
    } else {
        SurfacePixels::Rgba8(dynamic.into_rgba8().into_raw())
    };

    // The decoder is the authority on the final size; a format with an
    // embedded sub-image (ICO) can legitimately differ from the container.
    let expected = u64::from(width) * u64::from(height);
    if pixels.pixel_count() as u64 != expected {
        return Err(CodecError::BufferSize(format!(
            "decoder produced {} pixels for a {width}x{height} image",
            pixels.pixel_count()
        )));
    }

    Ok(DecodedSurface {
        width,
        height,
        pixels,
        color_space,
        icc_profile,
        source_format,
    })
}

/// [`decode_surface_reader`] over a file path, hinted by the extension.
pub fn decode_surface_path(
    path: &Path,
    limits: ImportLimits,
) -> Result<DecodedSurface, CodecError> {
    decode_surface_reader_with_hint(
        BufReader::new(File::open(path)?),
        limits,
        hint_from_path(path),
    )
}

/// [`decode_surface_reader`] over an in-memory buffer.
pub fn decode_surface_bytes(
    bytes: &[u8],
    limits: ImportLimits,
) -> Result<DecodedSurface, CodecError> {
    decode_surface_reader(Cursor::new(bytes), limits)
}

/// [`decode_surface_bytes`] with an explicit container format.
///
/// Needed for TGA, which carries no signature; harmless for the rest, where
/// content sniffing still overrides the hint.
pub fn decode_surface_bytes_as(
    bytes: &[u8],
    limits: ImportLimits,
    format: ImportFormat,
) -> Result<DecodedSurface, CodecError> {
    decode_surface_reader_with_hint(Cursor::new(bytes), limits, Some(format))
}

/// Decode any supported raster file into RGBA8.
///
/// Applies [`ImportLimits::default`]; a 16-bit source is down-converted. Use
/// [`decode_surface_path`] to keep the depth and choose the limits.
pub fn decode_path(path: &Path) -> Result<DecodedImage, CodecError> {
    Ok(decode_surface_path(path, ImportLimits::default())?.into_decoded_image())
}

/// Decode from an in-memory buffer (used for embedded assets).
pub fn decode_bytes(bytes: &[u8]) -> Result<DecodedImage, CodecError> {
    Ok(decode_surface_bytes(bytes, ImportLimits::default())?.into_decoded_image())
}

/// What a container can store in the alpha channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AlphaSupport {
    /// A real alpha channel: every level of partial transparency survives.
    Full,
    /// One fully transparent palette entry and nothing in between (GIF).
    /// Partial alpha must be composited onto a background; alpha `0` can stay.
    Binary,
    /// No transparency at all (JPEG). Everything is composited.
    None,
}

/// Output format selector for encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExportFormat {
    /// PNG. Lossless, alpha, 8 or 16 bit, ICC.
    Png,
    /// JPEG at the given quality. **Must be `1..=100`**; [`encode`] rejects
    /// anything else rather than letting a codec clamp it silently.
    Jpeg(u8),
    /// WebP. Lossless only — the backing encoder has no lossy mode — so a
    /// quality knob here would be a lie.
    WebP,
    /// TIFF. Lossless, alpha, 8 or 16 bit, ICC.
    Tiff,
    /// GIF. Palettised to at most 256 colours with 1-bit alpha.
    Gif,
    /// BMP. Lossless, alpha, 8 bit, no ICC.
    Bmp,
}

impl ExportFormat {
    /// Every format the exporter can write.
    pub const ALL: [ExportFormat; 6] = [
        ExportFormat::Png,
        ExportFormat::Jpeg(90),
        ExportFormat::WebP,
        ExportFormat::Tiff,
        ExportFormat::Gif,
        ExportFormat::Bmp,
    ];

    /// The inclusive range a JPEG quality value must fall in.
    pub const JPEG_QUALITY_RANGE: std::ops::RangeInclusive<u8> = 1..=100;

    /// Build a JPEG format, rejecting an out-of-range quality at construction.
    pub fn jpeg(quality: u8) -> Result<Self, CodecError> {
        let format = ExportFormat::Jpeg(quality);
        format.validate()?;
        Ok(format)
    }

    /// Reject a format whose parameters are out of range.
    ///
    /// The variant is public, so `Jpeg(0)` and `Jpeg(200)` are constructible;
    /// this is what stops them reaching an encoder that would silently clamp.
    pub fn validate(self) -> Result<(), CodecError> {
        match self {
            ExportFormat::Jpeg(q) if !Self::JPEG_QUALITY_RANGE.contains(&q) => Err(
                CodecError::InvalidParameter(format!("JPEG quality must be 1..=100, got {q}")),
            ),
            _ => Ok(()),
        }
    }

    /// What the container can do with transparency.
    ///
    /// Three cases, not two: GIF has no alpha *channel*, but it does have one
    /// fully transparent palette entry, so it can keep a cut-out silhouette
    /// while every partially transparent pixel still has to be composited onto
    /// a background.
    pub fn alpha_support(self) -> AlphaSupport {
        match self {
            ExportFormat::Png | ExportFormat::Tiff | ExportFormat::WebP | ExportFormat::Bmp => {
                AlphaSupport::Full
            }
            ExportFormat::Gif => AlphaSupport::Binary,
            ExportFormat::Jpeg(_) => AlphaSupport::None,
        }
    }

    /// Whether the container can store an alpha *channel*, i.e. partial
    /// transparency. `false` means the exporter must flatten onto a background
    /// colour first — see [`ExportFormat::alpha_support`] for what GIF can
    /// still keep while doing so.
    pub fn supports_alpha(self) -> bool {
        matches!(self.alpha_support(), AlphaSupport::Full)
    }

    /// Whether the container can carry an embedded ICC profile.
    pub fn supports_icc(self) -> bool {
        matches!(
            self,
            ExportFormat::Png | ExportFormat::Jpeg(_) | ExportFormat::WebP | ExportFormat::Tiff
        )
    }

    /// Whether the container can store 16 bits per channel.
    pub fn supports_16_bit(self) -> bool {
        matches!(self, ExportFormat::Png | ExportFormat::Tiff)
    }

    /// The conventional file extension, without a dot.
    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Png => "png",
            ExportFormat::Jpeg(_) => "jpg",
            ExportFormat::WebP => "webp",
            ExportFormat::Tiff => "tif",
            ExportFormat::Gif => "gif",
            ExportFormat::Bmp => "bmp",
        }
    }

    /// The IANA media type.
    pub fn mime(self) -> &'static str {
        match self {
            ExportFormat::Png => "image/png",
            ExportFormat::Jpeg(_) => "image/jpeg",
            ExportFormat::WebP => "image/webp",
            ExportFormat::Tiff => "image/tiff",
            ExportFormat::Gif => "image/gif",
            ExportFormat::Bmp => "image/bmp",
        }
    }
}

/// Borrowed pixels handed to an encoder.
///
/// Borrowed, not owned, on purpose: an 8K RGBA8 frame is 132 MB, and the
/// previous implementation cloned it into an `ImageBuffer` before every single
/// encode. Every encoder in `image` writes from a `&[u8]`, so the clone bought
/// nothing.
#[derive(Debug, Clone, Copy)]
pub enum EncodedPixels<'a> {
    /// Row-major RGBA8, straight alpha, display-encoded.
    Rgba8(&'a [u8]),
    /// Row-major RGBA16, straight alpha, display-encoded, host endianness.
    Rgba16(&'a [u16]),
}

impl<'a> EncodedPixels<'a> {
    fn sample_count(&self) -> usize {
        match self {
            EncodedPixels::Rgba8(v) => v.len(),
            EncodedPixels::Rgba16(v) => v.len(),
        }
    }

    /// The RGBA8 samples, for a container that stores 8 bits per channel.
    ///
    /// 16-bit input is an *error* here rather than a silent down-conversion:
    /// [`ExportFormat::supports_16_bit`] exists to answer this question, and a
    /// caller who asked a BMP for 16 bits and got 8 without being told would
    /// have no way to know the depth they requested was not the depth written.
    fn require_rgba8(self, format: ExportFormat) -> Result<&'a [u8], CodecError> {
        match self {
            EncodedPixels::Rgba8(v) => Ok(v),
            EncodedPixels::Rgba16(_) => Err(depth_error(format)),
        }
    }
}

fn depth_error(format: ExportFormat) -> CodecError {
    CodecError::InvalidParameter(format!(
        "{} cannot store 16 bits per channel",
        format.extension()
    ))
}

/// RGB8 with the alpha channel dropped, for containers without alpha.
fn rgb8_from(rgba: &[u8]) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(rgba.len() / 4 * 3);
    for px in rgba.chunks_exact(4) {
        rgb.extend_from_slice(&px[..3]);
    }
    rgb
}

/// Side-channel data an encoder may embed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EncodeOptions {
    /// ICC profile to embed. Ignored by formats that cannot carry one.
    pub icc_profile: Option<Vec<u8>>,
}

impl EncodeOptions {
    /// Options that embed `profile`.
    pub fn with_icc(profile: Vec<u8>) -> Self {
        EncodeOptions {
            icc_profile: Some(profile),
        }
    }
}

fn check_buffer(width: u32, height: u32, pixels: EncodedPixels<'_>) -> Result<(), CodecError> {
    if width == 0 || height == 0 {
        return Err(CodecError::BufferSize(format!(
            "cannot encode a {width}x{height} image"
        )));
    }
    // Saturating: `u32::MAX * u32::MAX * 4` overflows a `u64`, and a debug
    // build would panic on that rather than rejecting the dimensions.
    let expected = u64::from(width)
        .saturating_mul(u64::from(height))
        .saturating_mul(4);
    if pixels.sample_count() as u64 != expected {
        return Err(CodecError::BufferSize(format!(
            "{}x{} needs {expected} samples, got {}",
            width,
            height,
            pixels.sample_count()
        )));
    }
    Ok(())
}

fn embed_icc<E: ImageEncoder>(
    encoder: &mut E,
    format: ExportFormat,
    options: &EncodeOptions,
) -> Result<(), CodecError> {
    if let Some(profile) = &options.icc_profile {
        if format.supports_icc() && !profile.is_empty() {
            encoder
                .set_icc_profile(profile.clone())
                .map_err(image::ImageError::Unsupported)?;
        }
    }
    Ok(())
}

/// Encode straight into a writer.
///
/// `Seek` is required because TIFF patches its own header offsets after the
/// image data is written.
pub fn encode_into<W: Write + Seek>(
    out: &mut W,
    format: ExportFormat,
    width: u32,
    height: u32,
    pixels: EncodedPixels<'_>,
    options: &EncodeOptions,
) -> Result<(), CodecError> {
    format.validate()?;
    check_buffer(width, height, pixels)?;

    match format {
        ExportFormat::Png => {
            let mut enc = image::codecs::png::PngEncoder::new(&mut *out);
            embed_icc(&mut enc, format, options)?;
            match pixels {
                EncodedPixels::Rgba8(v) => {
                    enc.write_image(v, width, height, ExtendedColorType::Rgba8)?
                }
                EncodedPixels::Rgba16(v) => enc.write_image(
                    bytemuck::cast_slice(v),
                    width,
                    height,
                    ExtendedColorType::Rgba16,
                )?,
            }
        }
        ExportFormat::Tiff => {
            let mut enc = image::codecs::tiff::TiffEncoder::new(&mut *out);
            embed_icc(&mut enc, format, options)?;
            match pixels {
                EncodedPixels::Rgba8(v) => {
                    enc.write_image(v, width, height, ExtendedColorType::Rgba8)?
                }
                EncodedPixels::Rgba16(v) => enc.write_image(
                    bytemuck::cast_slice(v),
                    width,
                    height,
                    ExtendedColorType::Rgba16,
                )?,
            }
        }
        ExportFormat::Jpeg(quality) => {
            // JPEG has no alpha channel. The straight-alpha RGB is written as
            // is; `crate::export` flattens onto a background first so the
            // discarded alpha is already 255.
            let rgb = rgb8_from(pixels.require_rgba8(format)?);
            let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut *out, quality);
            embed_icc(&mut enc, format, options)?;
            enc.write_image(&rgb, width, height, ExtendedColorType::Rgb8)?;
        }
        ExportFormat::WebP => {
            let rgba = pixels.require_rgba8(format)?;
            let mut enc = image::codecs::webp::WebPEncoder::new_lossless(&mut *out);
            embed_icc(&mut enc, format, options)?;
            enc.write_image(rgba, width, height, ExtendedColorType::Rgba8)?;
        }
        ExportFormat::Gif => {
            let rgba = pixels.require_rgba8(format)?;
            // Scoped: the GIF encoder writes the stream trailer when dropped.
            let mut enc = image::codecs::gif::GifEncoder::new(&mut *out);
            enc.encode(rgba, width, height, ExtendedColorType::Rgba8)?;
        }
        ExportFormat::Bmp => {
            let rgba = pixels.require_rgba8(format)?;
            let mut enc = image::codecs::bmp::BmpEncoder::new(&mut *out);
            enc.encode(rgba, width, height, ExtendedColorType::Rgba8)?;
        }
    }
    Ok(())
}

/// Encode display-encoded, straight-alpha pixels to the requested format.
///
/// The pixels are written as given: this function performs no colour
/// conversion. If they came out of the compositor they are linear and
/// premultiplied and must go through [`crate::export`] first.
pub fn encode_with(
    format: ExportFormat,
    width: u32,
    height: u32,
    pixels: EncodedPixels<'_>,
    options: &EncodeOptions,
) -> Result<Vec<u8>, CodecError> {
    let mut out = Cursor::new(Vec::new());
    encode_into(&mut out, format, width, height, pixels, options)?;
    Ok(out.into_inner())
}

/// Encode RGBA8 pixels to the requested format, returning the file bytes.
///
/// See [`encode_with`] for the colour-management caveat.
pub fn encode(
    format: ExportFormat,
    width: u32,
    height: u32,
    rgba8: &[u8],
) -> Result<Vec<u8>, CodecError> {
    encode_with(
        format,
        width,
        height,
        EncodedPixels::Rgba8(rgba8),
        &EncodeOptions::default(),
    )
}

/// Counter making concurrent temporary names in one directory unique.
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A temporary file that removes itself unless it is committed.
///
/// `Drop` rather than a cleanup call at each `?`: a panic in an encoder must
/// not leave a half-written file lying next to the user's export either.
struct TempFile {
    path: PathBuf,
    committed: bool,
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// The directory the temporary file must be created in, so that renaming it
/// over `path` never crosses a filesystem boundary.
fn parent_dir(path: &Path) -> PathBuf {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        // A bare file name means the current directory; `Path::new("")` would
        // put the temporary at a relative path instead.
        _ => PathBuf::from("."),
    }
}

/// Create a uniquely named, empty file in `dir`.
///
/// `create_new` means the open fails rather than truncating if the name is
/// somehow taken, so this can never clobber an unrelated file.
fn create_temp_in(dir: &Path) -> Result<(TempFile, File), CodecError> {
    for _ in 0..64 {
        let n = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!(".raster-export-{}-{n}.tmp", std::process::id()));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => {
                return Ok((
                    TempFile {
                        path,
                        committed: false,
                    },
                    file,
                ))
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(CodecError::Io(e)),
        }
    }
    Err(CodecError::Io(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not create a unique temporary file for the export",
    )))
}

/// Encode straight to a file, without ever materialising the whole file in
/// memory.
///
/// `path` is used exactly as given. Callers deriving a name from untrusted
/// content must sanitise it first — see [`crate::export::sanitize_file_stem`].
///
/// # The destination survives a failure
///
/// The bytes go into a temporary file in the *same directory* as `path` — same
/// filesystem, so the final `rename` is atomic and cannot fail half way — and
/// that file replaces `path` only once the encode has finished and the data is
/// flushed. A rejected parameter, a full disk, an encoder that refuses the
/// dimensions, or a panic all leave whatever was at `path` exactly as it was,
/// and leave no temporary behind.
///
/// Truncating first and encoding afterwards, which is what `File::create` does,
/// turns any of those into a destroyed file: an export that overwrites last
/// week's render with nothing at all.
pub fn encode_to_path(
    path: &Path,
    format: ExportFormat,
    width: u32,
    height: u32,
    pixels: EncodedPixels<'_>,
    options: &EncodeOptions,
) -> Result<(), CodecError> {
    // Cheap rejections first, so an obviously bad call does not even create a
    // temporary file. Correctness does not depend on this: everything below
    // is non-destructive until the rename.
    format.validate()?;
    check_buffer(width, height, pixels)?;

    let (mut temp, file) = create_temp_in(&parent_dir(path))?;

    let mut out = BufWriter::new(file);
    encode_into(&mut out, format, width, height, pixels, options)?;
    out.flush()?;
    let file = out
        .into_inner()
        .map_err(|e| CodecError::Io(e.into_error()))?;
    // The rename is only atomic with respect to *which* file the name points
    // at; without this the new file's contents can still be lost to a crash.
    file.sync_all()?;
    drop(file);

    std::fs::rename(&temp.path, path)?;
    temp.committed = true;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export;

    /// A structurally valid, minimal ICC profile: the 128-byte header followed
    /// by a zero tag count. Enough for a real parser to accept, and small
    /// enough to compare byte for byte.
    fn tiny_icc() -> Vec<u8> {
        let mut p = vec![0u8; 132];
        p[0..4].copy_from_slice(&132u32.to_be_bytes()); // profile size
        p[4..8].copy_from_slice(b"RSTU"); // preferred CMM
        p[8..12].copy_from_slice(&0x0420_0000u32.to_be_bytes()); // version 4.2
        p[12..16].copy_from_slice(b"mntr"); // device class
        p[16..20].copy_from_slice(b"RGB "); // data colour space
        p[20..24].copy_from_slice(b"XYZ "); // PCS
        p[36..40].copy_from_slice(b"acsp"); // required signature
        p[64..68].copy_from_slice(&0u32.to_be_bytes()); // rendering intent
        p[128..132].copy_from_slice(&0u32.to_be_bytes()); // tag count
        p
    }

    fn checker_rgba8(w: u32, h: u32) -> Vec<u8> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let on = (x + y) % 2 == 0;
                v.extend_from_slice(if on {
                    &[220u8, 40, 90, 255]
                } else {
                    &[10u8, 160, 240, 255]
                });
            }
        }
        v
    }

    /// TGA's 18-byte header. Every field in it is attacker-controlled with
    /// nothing to cross-check it against — the format has no magic number and
    /// no length prefix — which is why the hostile cases below poke at it
    /// directly.
    ///
    /// `image_type` 2 is uncompressed true-colour, 10 is RLE true-colour, 1 is
    /// colour-mapped.
    fn tga_header(image_type: u8, w: u16, h: u16, bpp: u8) -> Vec<u8> {
        let mut header = vec![0u8; 18];
        header[2] = image_type;
        header[12..14].copy_from_slice(&w.to_le_bytes());
        header[14..16].copy_from_slice(&h.to_le_bytes());
        header[16] = bpp;
        header[17] = 0x28; // 8 alpha bits, origin at top-left
        header
    }

    /// An ICO directory with `entries` declared and one 16-byte entry whose
    /// declared `size` and `offset` are the caller's to choose. Those two
    /// fields are ICO's whole attack surface: the payload they address is
    /// wherever they say it is, however far past the end of the file that is.
    ///
    /// `dim` is the entry's declared square size, in the single byte ICO gives
    /// it; the payload's own header has to agree with it.
    fn ico_with(entries: u16, dim: u8, size: u32, offset: u32, payload: &[u8]) -> Vec<u8> {
        let mut ico = Vec::new();
        ico.extend_from_slice(&0u16.to_le_bytes()); // reserved
        ico.extend_from_slice(&1u16.to_le_bytes()); // type: icon
        ico.extend_from_slice(&entries.to_le_bytes());
        ico.extend_from_slice(&[dim, dim, 0, 0]); // width, height, palette, reserved
        ico.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        ico.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        ico.extend_from_slice(&size.to_le_bytes());
        ico.extend_from_slice(&offset.to_le_bytes());
        ico.extend_from_slice(payload);
        ico
    }

    #[test]
    fn png_roundtrip() {
        let (w, h) = (2, 2);
        let px = vec![
            255u8, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ];
        let bytes = encode(ExportFormat::Png, w, h, &px).unwrap();
        let decoded = decode_bytes(&bytes).unwrap();
        assert_eq!((decoded.width, decoded.height), (w, h));
        assert_eq!(decoded.rgba8, px);
    }

    // ---------------------------------------------------------------- formats

    /// Every writable format survives a round trip, and the lossless ones do it
    /// pixel for pixel. WebP is in this list because the WebP encode path had
    /// never been exercised by a test at all.
    #[test]
    fn every_writable_format_roundtrips() {
        let (w, h) = (8u32, 6u32);
        let px = checker_rgba8(w, h);

        for format in ExportFormat::ALL {
            let bytes = encode(format, w, h, &px)
                .unwrap_or_else(|e| panic!("{format:?} failed to encode: {e}"));
            assert!(!bytes.is_empty(), "{format:?} produced no bytes");

            let decoded =
                decode_bytes(&bytes).unwrap_or_else(|e| panic!("{format:?} failed to decode: {e}"));
            assert_eq!(
                (decoded.width, decoded.height),
                (w, h),
                "{format:?} changed the dimensions"
            );

            match format {
                // Lossless, full colour, alpha preserved.
                ExportFormat::Png | ExportFormat::Tiff | ExportFormat::WebP | ExportFormat::Bmp => {
                    assert_eq!(decoded.rgba8, px, "{format:?} is supposed to be lossless");
                }
                // Palettised: only two colours are used, so a 256-entry palette
                // still reproduces them exactly.
                ExportFormat::Gif => {
                    assert_eq!(decoded.rgba8, px, "GIF lost a colour from a 2-colour image");
                }
                // Lossy and alpha-free.
                ExportFormat::Jpeg(_) => {
                    for (got, want) in decoded.rgba8.chunks_exact(4).zip(px.chunks_exact(4)) {
                        for c in 0..3 {
                            let delta = got[c] as i32 - want[c] as i32;
                            assert!(
                                delta.abs() <= 24,
                                "JPEG drifted too far: {got:?} vs {want:?}"
                            );
                        }
                        assert_eq!(got[3], 255);
                    }
                }
            }
        }
    }

    /// A WebP file really is a WebP file, not a PNG that happened to decode.
    #[test]
    fn webp_encode_produces_a_webp_container() {
        let px = checker_rgba8(4, 4);
        let bytes = encode(ExportFormat::WebP, 4, 4, &px).unwrap();
        assert_eq!(&bytes[0..4], b"RIFF", "not a RIFF container");
        assert_eq!(&bytes[8..12], b"WEBP", "not a WebP container");
        let info = probe_bytes(&bytes, ImportLimits::default()).unwrap();
        assert_eq!(info.format, ImportFormat::WebP);
    }

    /// The four read-only formats. ICO and TGA are hand-built here because
    /// neither is something this crate can write.
    #[test]
    fn read_only_formats_decode() {
        // --- TGA: 18-byte header, uncompressed 32-bit BGRA, top-left origin.
        let (w, h) = (2u32, 2u32);
        let mut tga = tga_header(2, w as u16, h as u16, 32);
        let want = [
            [10u8, 20, 30, 255],
            [200, 100, 50, 255],
            [0, 0, 0, 128],
            [255, 255, 255, 255],
        ];
        for px in want {
            tga.extend_from_slice(&[px[2], px[1], px[0], px[3]]); // BGRA
        }
        // TGA has no magic number, so an anonymous byte slice cannot be
        // sniffed as one — that is a property of the format, and the API says
        // so rather than pretending otherwise.
        assert!(!ImportFormat::Tga.is_self_identifying());
        assert!(decode_bytes(&tga).is_err());

        let decoded = decode_surface_bytes_as(&tga, ImportLimits::default(), ImportFormat::Tga)
            .expect("TGA must decode when the format is known");
        assert_eq!((decoded.width, decoded.height), (w, h));
        let SurfacePixels::Rgba8(px) = &decoded.pixels else {
            unreachable!()
        };
        assert_eq!(&px[..4], &want[0]);
        assert_eq!(&px[4..8], &want[1]);
        assert_eq!(
            probe_bytes_as(&tga, ImportLimits::default(), ImportFormat::Tga)
                .unwrap()
                .format,
            ImportFormat::Tga
        );

        // ...and a path with a `.tga` extension supplies the hint by itself.
        let dir = std::env::temp_dir().join(format!("raster-tga-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sample.tga");
        std::fs::write(&path, &tga).unwrap();
        let from_path = decode_surface_path(&path, ImportLimits::default()).unwrap();
        assert_eq!(from_path.source_format, ImportFormat::Tga);
        assert_eq!(from_path.pixels, decoded.pixels);
        assert_eq!(
            probe_path(&path, ImportLimits::default()).unwrap().format,
            ImportFormat::Tga
        );
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);

        // --- ICO wrapping a PNG.
        let inner = encode(ExportFormat::Png, 4, 4, &checker_rgba8(4, 4)).unwrap();
        // One entry, honestly sized, at offset 22 — immediately after the
        // 6-byte directory header and the single 16-byte entry.
        let ico = ico_with(1, 4, inner.len() as u32, 22, &inner);
        let decoded = decode_bytes(&ico).expect("ICO must decode");
        assert_eq!((decoded.width, decoded.height), (4, 4));
        assert_eq!(decoded.rgba8, checker_rgba8(4, 4));
        assert_eq!(
            probe_bytes(&ico, ImportLimits::default()).unwrap().format,
            ImportFormat::Ico
        );

        // --- GIF and BMP are covered as writable formats above; JPEG/PNG/
        // WebP/TIFF too. That is all eight importable containers.
        assert_eq!(ImportFormat::ALL.len(), 8);
    }

    // -------------------------------------------------------------------- ICC

    #[test]
    fn icc_profile_survives_decode_and_reencode() {
        let profile = tiny_icc();
        let (w, h) = (64u32, 64u32);
        let px = checker_rgba8(w, h);

        for format in [
            ExportFormat::Png,
            ExportFormat::Tiff,
            ExportFormat::Jpeg(92),
            ExportFormat::WebP,
        ] {
            let bytes = encode_with(
                format,
                w,
                h,
                EncodedPixels::Rgba8(&px),
                &EncodeOptions::with_icc(profile.clone()),
            )
            .unwrap_or_else(|e| panic!("{format:?}: {e}"));

            let decoded = decode_surface_bytes(&bytes, ImportLimits::default()).unwrap();
            assert_eq!(
                decoded.icc_profile.as_deref(),
                Some(profile.as_slice()),
                "{format:?} did not preserve the ICC profile"
            );
            // ...and the profile is reflected in the colour space, addressed by
            // content hash.
            let expected_hash = blake3::hash(&profile).to_hex().to_string();
            assert_eq!(
                decoded.color_space,
                ColorSpace::IccProfile {
                    asset_hash: expected_hash
                },
                "{format:?} did not record the profile in the colour space"
            );

            // Re-export it and it is still there: a full round trip.
            let again = encode_with(
                format,
                w,
                h,
                EncodedPixels::Rgba8(&px),
                &EncodeOptions {
                    icc_profile: decoded.icc_profile.clone(),
                },
            )
            .unwrap();
            let twice = decode_surface_bytes(&again, ImportLimits::default()).unwrap();
            assert_eq!(twice.icc_profile.as_deref(), Some(profile.as_slice()));
        }
    }

    /// A known, measured limitation, pinned so it cannot quietly get worse and
    /// so it is not mistaken for a bug in this crate.
    ///
    /// The backing `image` crate derives the TIFF reader's IFD-value budget
    /// from the size of the image's *own pixel buffer*, so a very small TIFF
    /// has no budget left to read a profile with and the tag is silently
    /// dropped. PNG and JPEG have no such coupling.
    #[test]
    fn a_tiny_tiff_loses_its_icc_profile_but_a_normal_one_does_not() {
        let profile = tiny_icc();
        let read_back = |w: u32, h: u32, format: ExportFormat| {
            let bytes = encode_with(
                format,
                w,
                h,
                EncodedPixels::Rgba8(&checker_rgba8(w, h)),
                &EncodeOptions::with_icc(profile.clone()),
            )
            .unwrap();
            decode_surface_bytes(&bytes, ImportLimits::default())
                .unwrap()
                .icc_profile
        };

        assert_eq!(
            read_back(4, 4, ExportFormat::Tiff),
            None,
            "the limitation is gone; simplify this test and the docs"
        );
        assert_eq!(
            read_back(64, 64, ExportFormat::Tiff).as_deref(),
            Some(profile.as_slice())
        );
        // The same tiny image keeps its profile in the formats that do not
        // share the coupling, which is what makes the cause specific to TIFF.
        assert_eq!(
            read_back(4, 4, ExportFormat::Png).as_deref(),
            Some(profile.as_slice())
        );
        assert_eq!(
            read_back(4, 4, ExportFormat::Jpeg(90)).as_deref(),
            Some(profile.as_slice())
        );
    }

    /// A file whose extension lies is decoded as what it actually is.
    #[test]
    fn content_sniffing_beats_a_wrong_format_hint() {
        let png = encode(ExportFormat::Png, 4, 4, &checker_rgba8(4, 4)).unwrap();
        let decoded =
            decode_surface_bytes_as(&png, ImportLimits::default(), ImportFormat::Bmp).unwrap();
        assert_eq!(
            decoded.source_format,
            ImportFormat::Png,
            "a wrong hint overrode the file's own signature"
        );
        assert_eq!(decoded.pixels, SurfacePixels::Rgba8(checker_rgba8(4, 4)));
    }

    #[test]
    fn no_profile_means_srgb_not_a_fabricated_one() {
        let bytes = encode(ExportFormat::Png, 4, 4, &checker_rgba8(4, 4)).unwrap();
        let decoded = decode_surface_bytes(&bytes, ImportLimits::default()).unwrap();
        assert_eq!(decoded.icc_profile, None);
        assert_eq!(decoded.color_space, ColorSpace::Srgb);
    }

    #[test]
    fn icc_is_dropped_by_formats_that_cannot_carry_it() {
        // Asking BMP to embed a profile is not an error; the profile is simply
        // not written, and that must not corrupt the file.
        let px = checker_rgba8(4, 4);
        let bytes = encode_with(
            ExportFormat::Bmp,
            4,
            4,
            EncodedPixels::Rgba8(&px),
            &EncodeOptions::with_icc(tiny_icc()),
        )
        .unwrap();
        let decoded = decode_surface_bytes(&bytes, ImportLimits::default()).unwrap();
        assert_eq!(decoded.icc_profile, None);
        assert_eq!(decoded.pixels, SurfacePixels::Rgba8(px));
    }

    #[test]
    fn an_absurdly_large_icc_profile_is_not_retained() {
        let limits = ImportLimits {
            max_icc_bytes: 64,
            ..ImportLimits::default()
        };
        let bytes = encode_with(
            ExportFormat::Png,
            4,
            4,
            EncodedPixels::Rgba8(&checker_rgba8(4, 4)),
            &EncodeOptions::with_icc(tiny_icc()),
        )
        .unwrap();
        let decoded = decode_surface_bytes(&bytes, limits).unwrap();
        assert_eq!(
            decoded.icc_profile, None,
            "a 132-byte profile beat a 64-byte cap"
        );
        assert_eq!(decoded.color_space, ColorSpace::Srgb);
    }

    // --------------------------------------------------------------- 16 bit

    #[test]
    fn sixteen_bit_png_decodes_without_precision_loss() {
        let (w, h) = (2u32, 2u32);
        // Values chosen so that an 8-bit round trip cannot reproduce them.
        let px: Vec<u16> = vec![
            65_535, 0, 1, 65_535, //
            12_345, 40_000, 257, 65_535, //
            30_001, 30_002, 30_003, 30_004, //
            7, 8, 9, 65_535,
        ];
        let bytes = encode_with(
            ExportFormat::Png,
            w,
            h,
            EncodedPixels::Rgba16(&px),
            &EncodeOptions::default(),
        )
        .unwrap();

        let info = probe_bytes(&bytes, ImportLimits::default()).unwrap();
        assert_eq!(info.pixel_format, PixelFormat::Rgba16);

        let decoded = decode_surface_bytes(&bytes, ImportLimits::default()).unwrap();
        assert_eq!(decoded.format(), PixelFormat::Rgba16);
        assert_eq!(decoded.pixels, SurfacePixels::Rgba16(px.clone()));

        // The convenience path is the one that loses precision, and it is only
        // reachable by asking for it.
        let eight = decode_bytes(&bytes).unwrap();
        assert_eq!(eight.rgba8.len(), (w * h * 4) as usize);
        assert_ne!(
            eight.rgba8[4], // 12_345 -> 48
            48 + 1,
            "sanity: the 8-bit view is a real conversion"
        );
        assert_eq!(eight.rgba8[4], ((12_345u32 * 255 + 32_767) / 65_535) as u8);
    }

    #[test]
    fn sixteen_bit_tiff_decodes_without_precision_loss() {
        let px: Vec<u16> = vec![65_535, 0, 33_333, 65_535, 1, 2, 3, 4];
        let bytes = encode_with(
            ExportFormat::Tiff,
            2,
            1,
            EncodedPixels::Rgba16(&px),
            &EncodeOptions::default(),
        )
        .unwrap();
        let decoded = decode_surface_bytes(&bytes, ImportLimits::default()).unwrap();
        assert_eq!(decoded.pixels, SurfacePixels::Rgba16(px));
    }

    #[test]
    fn an_eight_bit_source_stays_eight_bit() {
        let bytes = encode(ExportFormat::Png, 4, 4, &checker_rgba8(4, 4)).unwrap();
        let decoded = decode_surface_bytes(&bytes, ImportLimits::default()).unwrap();
        assert_eq!(decoded.format(), PixelFormat::Rgba8);
    }

    #[test]
    fn sixteen_bit_downconversion_is_a_round_not_a_shift() {
        // `>> 8` would send 65_535 -> 255 but also 128 -> 0 and 32_768 -> 128,
        // shifting the whole ramp down half a step.
        let px = SurfacePixels::Rgba16(vec![0, 32_768, 65_535, 257]).into_rgba8();
        assert_eq!(px, vec![0, 128, 255, 1]);
    }

    // ------------------------------------------------------------ parameters

    #[test]
    fn jpeg_quality_outside_1_to_100_is_rejected() {
        let px = checker_rgba8(2, 2);
        for bad in [0u8, 101, 200, 255] {
            let err = encode(ExportFormat::Jpeg(bad), 2, 2, &px)
                .expect_err("quality {bad} must be rejected");
            assert!(
                matches!(err, CodecError::InvalidParameter(_)),
                "quality {bad} produced the wrong error: {err}"
            );
            assert!(ExportFormat::jpeg(bad).is_err());
        }
        for good in [1u8, 50, 100] {
            assert!(ExportFormat::jpeg(good).is_ok());
            assert!(encode(ExportFormat::Jpeg(good), 2, 2, &px).is_ok());
        }
    }

    #[test]
    fn an_invalid_quality_does_not_truncate_the_destination_file() {
        let dir = std::env::temp_dir().join(format!("raster-codec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("existing.jpg");
        std::fs::write(&path, b"precious").unwrap();

        let err = encode_to_path(
            &path,
            ExportFormat::Jpeg(0),
            2,
            2,
            EncodedPixels::Rgba8(&checker_rgba8(2, 2)),
            &EncodeOptions::default(),
        )
        .expect_err("quality 0 must be rejected");
        assert!(matches!(err, CodecError::InvalidParameter(_)));
        assert_eq!(std::fs::read(&path).unwrap(), b"precious");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_buffer_that_does_not_match_the_dimensions_is_rejected() {
        let err = encode(ExportFormat::Png, 4, 4, &[0u8; 8]).unwrap_err();
        assert!(matches!(err, CodecError::BufferSize(_)), "{err}");
        let err = encode(ExportFormat::Png, 0, 4, &[]).unwrap_err();
        assert!(matches!(err, CodecError::BufferSize(_)), "{err}");
    }

    // ------------------------------------------------------------- to a path

    #[test]
    fn encode_to_path_writes_the_same_bytes_as_encode() {
        let dir = std::env::temp_dir().join(format!("raster-codec-path-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let px = checker_rgba8(8, 8);

        for format in ExportFormat::ALL {
            let path = dir.join(format!("out.{}", format.extension()));
            encode_to_path(
                &path,
                format,
                8,
                8,
                EncodedPixels::Rgba8(&px),
                &EncodeOptions::default(),
            )
            .unwrap_or_else(|e| panic!("{format:?}: {e}"));

            let from_file = std::fs::read(&path).unwrap();
            let in_memory = encode(format, 8, 8, &px).unwrap();
            assert_eq!(from_file, in_memory, "{format:?} differed on disk");

            // ...and the file on disk is decodable through the streaming path.
            let decoded = decode_surface_path(&path, ImportLimits::default()).unwrap();
            assert_eq!((decoded.width, decoded.height), (8, 8));
            let _ = std::fs::remove_file(&path);
        }
        let _ = std::fs::remove_dir(&dir);
    }

    // ------------------------------------------------------- untrusted input

    #[test]
    fn a_truncated_file_errors_instead_of_panicking() {
        let px = checker_rgba8(64, 64);
        for format in [
            ExportFormat::Png,
            ExportFormat::Jpeg(85),
            ExportFormat::WebP,
            ExportFormat::Tiff,
            ExportFormat::Gif,
            ExportFormat::Bmp,
        ] {
            let full = encode(format, 64, 64, &px).unwrap();
            // Several truncation points: mid-header, mid-metadata, mid-pixels.
            for fraction in [0.02f64, 0.25, 0.6, 0.95] {
                let cut = (full.len() as f64 * fraction) as usize;
                let truncated = &full[..cut];
                // Either an error or a successful partial decode is acceptable;
                // a panic or a hang is not, and neither is a silent success
                // that hands back an over-large buffer.
                if let Ok(surface) = decode_surface_bytes(truncated, ImportLimits::default()) {
                    assert_eq!(
                        surface.pixels.pixel_count() as u64,
                        u64::from(surface.width) * u64::from(surface.height),
                        "{format:?} at {fraction} returned a mis-sized buffer"
                    );
                }
            }
        }
    }

    /// ...including the two formats that are import-only, and therefore cannot
    /// be reached by truncating something this crate encoded.
    ///
    /// ICO and TGA are precisely the two parsers where "never trust a length or
    /// an offset from the file" has the most to do: an ICO entry carries a
    /// declared size and a declared offset with nothing to check them against,
    /// and a TGA header carries an id-field length, a colour-map length and
    /// (for image type 10) RLE run lengths, in a container with no magic number
    /// and no length prefix at all. `a_truncated_file_errors_instead_of_
    /// panicking` iterates `ExportFormat`, which has six writable containers
    /// and neither of these, so before this test nothing in the suite fed
    /// either parser a malformed byte.
    ///
    /// The contract is the same one: an error, or a decode whose buffer really
    /// is the size it claims. Never a panic, and never an over-large buffer.
    #[test]
    fn hostile_ico_and_tga_headers_error_instead_of_panicking() {
        let inner = encode(ExportFormat::Png, 4, 4, &checker_rgba8(4, 4)).unwrap();
        let check = |label: &str, bytes: &[u8], format: ImportFormat| {
            if let Ok(surface) = decode_surface_bytes_as(bytes, ImportLimits::default(), format) {
                assert_eq!(
                    surface.pixels.pixel_count() as u64,
                    u64::from(surface.width) * u64::from(surface.height),
                    "{label} returned a mis-sized buffer"
                );
                assert!(surface.width > 0 && surface.height > 0, "{label}");
            }
        };

        // --- ICO: the declared size/offset pair, in every hostile shape.
        let size = inner.len() as u32;
        check(
            "ico: entry offset past EOF",
            &ico_with(1, 4, size, 0xffff_0000, &inner),
            ImportFormat::Ico,
        );
        check(
            "ico: entry declares 4 GiB",
            &ico_with(1, 4, u32::MAX, 22, &inner),
            ImportFormat::Ico,
        );
        check(
            "ico: 65535 entries, one present",
            &ico_with(0xffff, 4, size, 22, &inner),
            ImportFormat::Ico,
        );
        check(
            "ico: offset overlaps the directory header",
            &ico_with(1, 4, size, 0, &inner),
            ImportFormat::Ico,
        );
        check(
            "ico: empty payload",
            &ico_with(1, 4, 0, 22, &[]),
            ImportFormat::Ico,
        );
        check(
            "ico: directory header only",
            &ico_with(1, 4, size, 22, &inner)[..6],
            ImportFormat::Ico,
        );

        // --- TGA: the declared lengths, in every hostile shape.
        // An RLE run packet claiming 128 pixels in a 2x2 image, with one
        // pixel's worth of payload behind it.
        let mut rle = tga_header(10, 2, 2, 32);
        rle.push(0x80 | 127);
        rle.extend_from_slice(&[1, 2, 3, 4]);
        check("tga: RLE run overruns the image", &rle, ImportFormat::Tga);

        // The id field declares 255 bytes that are not there.
        let mut id_overrun = tga_header(2, 2, 2, 32);
        id_overrun[0] = 255;
        id_overrun.extend_from_slice(&[0u8; 16]);
        check("tga: id field overruns", &id_overrun, ImportFormat::Tga);

        // A colour map of 65535 32-bit entries — 256 KiB — declared by a
        // 20-byte file.
        let mut colour_map = tga_header(1, 2, 2, 8);
        colour_map[1] = 1; // a colour map is present
        colour_map[5..7].copy_from_slice(&u16::MAX.to_le_bytes());
        colour_map[7] = 32; // bits per colour-map entry
        check(
            "tga: 65535-entry colour map",
            &colour_map,
            ImportFormat::Tga,
        );

        // Truncated part way through the pixel data.
        let mut full = tga_header(2, 8, 8, 32);
        full.extend_from_slice(&checker_rgba8(8, 8));
        check("tga: truncated mid-pixels", &full[..30], ImportFormat::Tga);
        check("tga: header only", &full[..18], ImportFormat::Tga);
        check("tga: half a header", &full[..9], ImportFormat::Tga);

        // A zero dimension, which would make every later product zero.
        check("tga: 0x0", &tga_header(2, 0, 0, 32), ImportFormat::Tga);
        // ...and a bit depth no true-colour TGA has.
        check(
            "tga: 7 bits per pixel",
            &tga_header(2, 2, 2, 7),
            ImportFormat::Tga,
        );
    }

    /// The dimension and allocation ceilings apply to the import-only formats
    /// too, and they bite *before* the buffer exists rather than after.
    ///
    /// An 18-byte TGA header can declare 65535x65535, which is 4.29 billion
    /// pixels and 17 GB of RGBA8. A 22-byte ICO directory can declare a 4 GiB
    /// entry. Neither number is checked by the container against anything, so
    /// the only thing standing between them and the allocator is
    /// [`ImportLimits`] — and "is checked" is only worth asserting if the
    /// measurement shows nothing large was allocated on the way to the refusal.
    #[test]
    fn an_import_only_header_cannot_ask_for_a_giant_allocation() {
        // 65535 * 65535 = 4_294_836_225 pixels against a 1<<28 ceiling.
        let tga = tga_header(2, 65_535, 65_535, 32);
        assert_eq!(tga.len(), 18, "the whole hostile file is 18 bytes");
        let (result, peak) = crate::alloc_probe::measure_peak(|| {
            decode_surface_bytes_as(&tga, ImportLimits::default(), ImportFormat::Tga)
        });
        // Which ceiling fired is asserted, not just that one did: the declared
        // *dimensions* are what must be caught, because they are checked before
        // the decoder is asked for a single pixel.
        let err = result.unwrap_err();
        assert!(
            matches!(&err, CodecError::LimitExceeded(m) if m.contains("4294836225 pixels")),
            "an 18-byte TGA declaring 17 GB of pixels was not refused on its dimensions: {err}"
        );
        assert!(
            peak < (1 << 20),
            "refusing the 17 GB TGA header still peaked at {peak} bytes"
        );

        // The same header under limits that *do* allow that many pixels is
        // still refused, by the allocation ceiling — a second, independent
        // bound, reached with nothing allocated either.
        let generous = ImportLimits {
            max_pixels: u64::MAX,
            ..ImportLimits::default()
        };
        let (result, peak) = crate::alloc_probe::measure_peak(|| {
            decode_surface_bytes_as(&tga, generous, ImportFormat::Tga)
        });
        let err = result.unwrap_err();
        assert!(
            matches!(&err, CodecError::LimitExceeded(m) if m.contains("decoding needs")),
            "the allocation ceiling did not catch a 17 GB TGA: {err}"
        );
        assert!(peak < (1 << 20), "peaked at {peak} bytes");

        // ICO: an entry declaring u32::MAX bytes at an offset past the end.
        let ico = ico_with(1, 4, u32::MAX, 0xffff_0000, &[]);
        assert!(ico.len() < 64);
        let (result, peak) = crate::alloc_probe::measure_peak(|| {
            decode_surface_bytes_as(&ico, ImportLimits::default(), ImportFormat::Ico)
        });
        assert!(result.is_err(), "{result:?}");
        assert!(
            peak < (1 << 20),
            "refusing a 4 GiB ICO entry peaked at {peak} bytes"
        );

        // ...and the probe is measuring this call rather than always reading
        // zero: the same decode of a legitimate 64x64 ICO does allocate.
        let inner = encode(ExportFormat::Png, 64, 64, &checker_rgba8(64, 64)).unwrap();
        let good = ico_with(1, 64, inner.len() as u32, 22, &inner);
        let (result, peak) = crate::alloc_probe::measure_peak(|| {
            decode_surface_bytes_as(&good, ImportLimits::default(), ImportFormat::Ico)
        });
        assert!(result.is_ok(), "{result:?}");
        assert!(
            peak >= 64 * 64 * 4,
            "a 64x64 ICO decode peaked at only {peak} bytes; the probe is not measuring"
        );
    }

    #[test]
    fn garbage_is_not_an_image() {
        for junk in [
            &b""[..],
            &b"\x00"[..],
            &b"not an image at all, just prose"[..],
            &[0xffu8; 512][..],
        ] {
            assert!(
                decode_bytes(junk).is_err(),
                "accepted {} bytes of junk",
                junk.len()
            );
            assert!(probe_bytes(junk, ImportLimits::default()).is_err());
        }
    }

    /// The header says 4096x4096; the limits say 16x16. Nothing is allocated.
    #[test]
    fn declared_dimensions_are_checked_before_any_pixel_buffer_exists() {
        let big = encode(ExportFormat::Png, 512, 512, &vec![0u8; 512 * 512 * 4]).unwrap();
        let limits = ImportLimits {
            max_width: 16,
            max_height: 16,
            ..ImportLimits::default()
        };
        let err = decode_surface_bytes(&big, limits).unwrap_err();
        assert!(
            matches!(err, CodecError::LimitExceeded(_) | CodecError::Image(_)),
            "{err}"
        );
        // The pixel-count limit catches dimensions that individually pass.
        let limits = ImportLimits {
            max_pixels: 1024,
            ..ImportLimits::default()
        };
        let err = decode_surface_bytes(&big, limits).unwrap_err();
        assert!(matches!(err, CodecError::LimitExceeded(_)), "{err}");
        // And the probe refuses on the same grounds without decoding.
        assert!(probe_bytes(&big, limits).is_err());
        // The default limits accept it.
        assert!(decode_surface_bytes(&big, ImportLimits::default()).is_ok());
    }

    #[test]
    fn the_allocation_ceiling_is_enforced() {
        let big = encode(ExportFormat::Png, 256, 256, &vec![7u8; 256 * 256 * 4]).unwrap();
        let limits = ImportLimits {
            max_alloc_bytes: 4096,
            ..ImportLimits::default()
        };
        let err = decode_surface_bytes(&big, limits).unwrap_err();
        assert!(
            matches!(err, CodecError::LimitExceeded(_) | CodecError::Image(_)),
            "{err}"
        );
    }

    /// A grayscale-8 PNG, whose source colour type is one byte per pixel.
    fn gray8_png(w: u32, h: u32) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(
                &vec![128u8; (w as usize) * (h as usize)],
                w,
                h,
                ExtendedColorType::L8,
            )
            .unwrap();
        out.into_inner()
    }

    /// `max_alloc_bytes` bounds what the *pipeline* allocates, not what the
    /// file's own colour type implies.
    ///
    /// Measured before the fix: this 2048x2048 grayscale PNG declares
    /// `total_bytes() = 4 MiB`, sailed through a 5 MiB ceiling, and then
    /// allocated 21 254 104 bytes converting to RGBA8 — four times the stated
    /// cap. The same pixel count as RGBA8 was correctly refused, so the field
    /// silently under-counted by the source-to-RGBA ratio, which is exactly the
    /// margin an unattended caller is told to tighten this field for.
    #[test]
    fn the_allocation_ceiling_counts_the_rgba_conversion_not_just_the_source() {
        const W: u32 = 2048;
        const H: u32 = 2048;
        const PIXELS: u64 = (W as u64) * (H as u64);

        // Between the 4 MiB grayscale source and the 16 MiB RGBA8 buffer that
        // both decodes materialise: it can only admit the grayscale file if the
        // conversion is not being counted.
        let limits = ImportLimits {
            max_alloc_bytes: 5 << 20,
            ..ImportLimits::default()
        };

        let gray = gray8_png(W, H);
        assert_eq!(
            probe_bytes(&gray, ImportLimits::default())
                .unwrap()
                .pixel_format,
            PixelFormat::Rgba8
        );
        // `let Err(..) else`, not `expect_err`: the success value is a
        // four-megapixel surface, and printing it is a 64 MB panic message.
        let Err(err) = decode_surface_bytes(&gray, limits) else {
            panic!("a grayscale source that converts to 16 MiB of RGBA8 must be refused");
        };
        assert!(matches!(err, CodecError::LimitExceeded(_)), "{err}");

        // The identical pixel count stored as RGBA8 is refused by the same
        // ceiling, which is the equivalence the fix establishes.
        let rgba = encode(ExportFormat::Png, W, H, &vec![9u8; (PIXELS * 4) as usize]).unwrap();
        let Err(err) = decode_surface_bytes(&rgba, limits) else {
            panic!("an RGBA8 source of the same pixel count must be refused too");
        };
        assert!(matches!(err, CodecError::LimitExceeded(_)), "{err}");

        // ...and the arithmetic is the sum of both live buffers, not either
        // one alone.
        assert_eq!(
            decode_alloc_bytes(W, H, image::ColorType::L8, PIXELS),
            PIXELS + PIXELS * 4
        );
        assert_eq!(
            decode_alloc_bytes(W, H, image::ColorType::L16, PIXELS * 2),
            PIXELS * 2 + PIXELS * 8
        );
        // A header claiming an absurd size produces a number that fails the
        // check rather than overflowing.
        assert_eq!(
            decode_alloc_bytes(u32::MAX, u32::MAX, image::ColorType::Rgba16, u64::MAX),
            u64::MAX
        );

        // Under a ceiling that admits the real peak, the same file decodes —
        // and really does allocate well past its own `total_bytes`.
        let generous = ImportLimits {
            max_alloc_bytes: 64 << 20,
            ..ImportLimits::default()
        };
        let (surface, allocated) =
            crate::alloc_probe::measure(|| decode_surface_bytes(&gray, generous).unwrap());
        assert_eq!(surface.pixels.pixel_count() as u64, PIXELS);
        assert_eq!(surface.format(), PixelFormat::Rgba8);
        assert!(
            allocated > (5 << 20),
            "decoding the grayscale file allocated only {allocated} bytes; the \
             measurement this test is built on no longer holds"
        );
    }

    #[test]
    fn probing_reads_the_header_only() {
        let px = checker_rgba8(32, 32);
        let bytes = encode(ExportFormat::Png, 32, 32, &px).unwrap();
        let info = probe_bytes(&bytes, ImportLimits::default()).unwrap();
        assert_eq!((info.width, info.height), (32, 32));
        assert_eq!(info.format, ImportFormat::Png);
        assert_eq!(info.pixel_format, PixelFormat::Rgba8);

        // A file whose header is intact but whose pixel data is gone still
        // probes, which is the whole point of a probe.
        let header_only = &bytes[..bytes.len() / 4];
        let info = probe_bytes(header_only, ImportLimits::default()).unwrap();
        assert_eq!((info.width, info.height), (32, 32));
        assert!(decode_surface_bytes(header_only, ImportLimits::default()).is_err());
    }

    // ------------------------------------------------------------- no copies

    /// Encoding must not clone the source buffer.
    ///
    /// The previous implementation called `rgba8.to_vec()` to build an
    /// `ImageBuffer`, so every export allocated a second full-size copy — 132 MB
    /// for an 8K frame. This measures allocation on the calling thread across
    /// the encode of a 16 MiB solid-colour image: solid colour compresses to a
    /// few kilobytes, so anything approaching the input size can only be a copy
    /// of the input.
    #[test]
    fn encoding_does_not_clone_the_source_buffer() {
        const W: u32 = 2048;
        const H: u32 = 2048;
        let px = vec![64u8; (W as usize) * (H as usize) * 4];
        assert_eq!(px.len(), 16 << 20);

        let (bytes, allocated) =
            crate::alloc_probe::measure(|| encode(ExportFormat::Png, W, H, &px).unwrap());

        assert!(!bytes.is_empty());
        assert!(
            allocated < (4 << 20),
            "encoding a {} MiB image allocated {} bytes; the source buffer is being cloned",
            px.len() >> 20,
            allocated
        );
    }

    // --------------------------------------------------------- colour bridge

    #[test]
    fn decode_to_linear_premultiplied_is_the_inverse_of_export() {
        // Alpha varies, so the premultiply half of the trip is exercised too.
        let mut px = checker_rgba8(4, 4);
        for (i, chunk) in px.chunks_exact_mut(4).enumerate() {
            chunk[3] = (i * 17) as u8;
        }
        let bytes = encode(ExportFormat::Png, 4, 4, &px).unwrap();
        let surface = decode_surface_bytes(&bytes, ImportLimits::default()).unwrap();
        let linear = surface.to_linear_premultiplied().unwrap();
        let back = export::rgba8_from_linear(&linear, &ColorSpace::Srgb).unwrap();
        for (i, (got, want)) in back.chunks_exact(4).zip(px.chunks_exact(4)).enumerate() {
            assert_eq!(got[3], want[3], "alpha changed at pixel {i}");
            if want[3] == 0 {
                continue; // a transparent pixel carries no colour to recover
            }
            for c in 0..3 {
                assert!(
                    (i32::from(got[c]) - i32::from(want[c])).abs() <= 1,
                    "pixel {i}: {got:?} vs {want:?}"
                );
            }
        }
    }

    /// An ICC-tagged surface has no transform into the working space, and the
    /// bridge says so instead of relabelling the file's own samples "linear".
    #[test]
    fn an_icc_tagged_surface_refuses_the_working_space_but_passes_through() {
        let profile = tiny_icc();
        let bytes = encode_with(
            ExportFormat::Png,
            1,
            1,
            EncodedPixels::Rgba8(&[128, 128, 128, 255]),
            &EncodeOptions::with_icc(profile),
        )
        .unwrap();
        let surface = decode_surface_bytes(&bytes, ImportLimits::default()).unwrap();
        assert!(matches!(surface.color_space, ColorSpace::IccProfile { .. }));
        assert!(surface.to_linear_premultiplied().is_err());

        // The pass-through buffer holds the file's number, 128/255, and not
        // the linear 0.2158 an sRGB decode would have produced.
        let working = surface.to_premultiplied_pass_through();
        assert!((working.pixels()[0] - 128.0 / 255.0).abs() < 1e-6);
        assert_eq!(
            export::rgba8_from_linear_pass_through(&working),
            vec![128, 128, 128, 255]
        );
    }

    // ---------------------------------------------------------- bit depth

    /// A container that cannot store 16 bits says so rather than quietly
    /// writing 8 and letting the caller believe otherwise.
    #[test]
    fn sixteen_bit_samples_are_refused_by_containers_that_cannot_store_them() {
        let px: Vec<u16> = vec![65_535, 30_000, 1, 65_535];
        for format in ExportFormat::ALL {
            let result = encode_with(
                format,
                1,
                1,
                EncodedPixels::Rgba16(&px),
                &EncodeOptions::default(),
            );
            if format.supports_16_bit() {
                let bytes = result.unwrap_or_else(|e| panic!("{format:?}: {e}"));
                let decoded = decode_surface_bytes(&bytes, ImportLimits::default()).unwrap();
                assert_eq!(decoded.pixels, SurfacePixels::Rgba16(px.clone()));
            } else {
                let err = result.expect_err("{format:?} claims it cannot store 16 bits");
                assert!(
                    matches!(err, CodecError::InvalidParameter(_)),
                    "{format:?}: {err}"
                );
            }
        }
    }

    // ------------------------------------------------- atomic path writes

    /// A failure *inside* the encoder must not cost the user the file that was
    /// already at the destination.
    ///
    /// 70000x1 passes `check_buffer` and is rejected by the GIF encoder, which
    /// cannot express a dimension above 65535 — a failure that happens after
    /// the point where a `File::create` implementation has already truncated
    /// the destination to zero bytes. Disk-full and a broken pipe land in the
    /// same place.
    #[test]
    fn a_failing_encode_leaves_the_existing_file_untouched() {
        let dir = std::env::temp_dir().join(format!("raster-atomic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("previous-render.gif");
        let precious = b"the render from last week, 36 bytes.";
        assert_eq!(precious.len(), 36);
        std::fs::write(&path, precious).unwrap();

        let px = vec![0u8; 70_000 * 4];
        let err = encode_to_path(
            &path,
            ExportFormat::Gif,
            70_000,
            1,
            EncodedPixels::Rgba8(&px),
            &EncodeOptions::default(),
        )
        .expect_err("the GIF encoder cannot write a 70000-pixel dimension");
        assert!(matches!(err, CodecError::Image(_)), "{err}");
        assert_eq!(
            std::fs::read(&path).unwrap(),
            precious,
            "a failed export destroyed the existing file"
        );

        // ...and no temporary file was left next to it.
        let entries: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, vec!["previous-render.gif".to_string()]);

        // The same is true of a rejected parameter...
        assert!(encode_to_path(
            &path,
            ExportFormat::Jpeg(0),
            2,
            2,
            EncodedPixels::Rgba8(&checker_rgba8(2, 2)),
            &EncodeOptions::default(),
        )
        .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), precious);

        // ...and of a mismatched buffer.
        assert!(encode_to_path(
            &path,
            ExportFormat::Gif,
            8,
            8,
            EncodedPixels::Rgba8(&[0u8; 12]),
            &EncodeOptions::default(),
        )
        .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), precious);

        // A *successful* write does replace it, with exactly the bytes
        // `encode` would have produced.
        let good = checker_rgba8(8, 8);
        encode_to_path(
            &path,
            ExportFormat::Gif,
            8,
            8,
            EncodedPixels::Rgba8(&good),
            &EncodeOptions::default(),
        )
        .unwrap();
        assert_eq!(
            std::fs::read(&path).unwrap(),
            encode(ExportFormat::Gif, 8, 8, &good).unwrap()
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    /// The temporary must land beside the destination, so the rename stays on
    /// one filesystem. A bare file name means the current directory, not an
    /// empty path — `Path::new("").join(x)` would silently produce a *relative*
    /// name and move the temporary somewhere else.
    #[test]
    fn the_temporary_is_created_beside_the_destination() {
        assert_eq!(parent_dir(Path::new("out.png")), PathBuf::from("."));
        assert_eq!(
            parent_dir(Path::new("renders/out.png")),
            PathBuf::from("renders")
        );
        let abs = std::env::temp_dir().join("out.png");
        assert_eq!(parent_dir(&abs), std::env::temp_dir());
    }

    // ------------------------------------------------ ICC allocation pin

    /// `max_icc_bytes` is a retention filter, not an allocation bound, and the
    /// doc on it says so. This measures that claim rather than asserting it in
    /// prose: a PNG carrying a one-megabyte `iCCP` chunk is decoded under a
    /// 1 KiB `max_icc_bytes`, and the megabyte is still allocated before the
    /// profile is dropped.
    ///
    /// If a future `image`/`png` release starts honouring the decoder limits
    /// for `iCCP`, this test fails — which is the signal to tighten the field
    /// and rewrite its documentation.
    #[test]
    fn an_oversized_icc_profile_is_dropped_but_was_already_allocated() {
        const PROFILE_BYTES: usize = 1 << 20;
        let mut profile = tiny_icc();
        profile.resize(PROFILE_BYTES, 0x5a);
        profile[0..4].copy_from_slice(&(PROFILE_BYTES as u32).to_be_bytes());

        let png = png_with_icc(&checker_rgba8(4, 4), 4, 4, &profile);
        let limits = ImportLimits {
            max_icc_bytes: 1024,
            ..ImportLimits::default()
        };

        let (decoded, allocated) =
            crate::alloc_probe::measure(|| decode_surface_bytes(&png, limits).unwrap());
        assert_eq!(
            decoded.icc_profile, None,
            "a 1 MiB profile beat a 1 KiB retention cap"
        );
        assert_eq!(decoded.color_space, ColorSpace::Srgb);
        assert_eq!(decoded.pixels, SurfacePixels::Rgba8(checker_rgba8(4, 4)));

        // Measured against the identical file with no `iCCP` chunk, so the
        // difference is the profile and not the 64-byte image around it.
        let plain = encode(ExportFormat::Png, 4, 4, &checker_rgba8(4, 4)).unwrap();
        let (_, baseline) =
            crate::alloc_probe::measure(|| decode_surface_bytes(&plain, limits).unwrap());
        assert!(
            allocated.saturating_sub(baseline) >= PROFILE_BYTES as u64,
            "the profile was not materialised by the backing decoder: decoding it \
             cost {allocated} bytes against {baseline} without it, so `max_icc_bytes` \
             may now be a real bound — tighten it and fix its documentation"
        );

        // Under a cap that admits it, the same file keeps the profile, so the
        // filter is a size test and not an unconditional drop.
        let permissive = ImportLimits {
            max_icc_bytes: PROFILE_BYTES,
            ..ImportLimits::default()
        };
        let kept = decode_surface_bytes(&png, permissive).unwrap();
        assert_eq!(kept.icc_profile.as_deref(), Some(profile.as_slice()));
    }

    /// Build a PNG carrying `profile` in an `iCCP` chunk.
    ///
    /// The chunk is written by hand because the encoder will not embed a
    /// profile this large, and because the point of the test is to control the
    /// *compressed* size: the zlib stream uses stored (uncompressed) deflate
    /// blocks, so a one-megabyte profile really is a one-megabyte chunk rather
    /// than a compression bomb.
    fn png_with_icc(rgba: &[u8], w: u32, h: u32, profile: &[u8]) -> Vec<u8> {
        fn crc32(bytes: &[u8]) -> u32 {
            let mut crc = 0xffff_ffffu32;
            for &b in bytes {
                crc ^= u32::from(b);
                for _ in 0..8 {
                    crc = if crc & 1 != 0 {
                        (crc >> 1) ^ 0xedb8_8320
                    } else {
                        crc >> 1
                    };
                }
            }
            !crc
        }
        fn adler32(bytes: &[u8]) -> u32 {
            let (mut a, mut b) = (1u32, 0u32);
            for &x in bytes {
                a = (a + u32::from(x)) % 65_521;
                b = (b + a) % 65_521;
            }
            (b << 16) | a
        }
        fn zlib_stored(data: &[u8]) -> Vec<u8> {
            // 0x78 0x01: deflate, 32 KiB window, no dictionary, and
            // 0x7801 % 31 == 0 as the format requires.
            let mut out = vec![0x78, 0x01];
            let mut rest = data;
            loop {
                let take = rest.len().min(65_535);
                let final_block = take == rest.len();
                out.push(u8::from(final_block)); // BTYPE 00, stored
                out.extend_from_slice(&(take as u16).to_le_bytes());
                out.extend_from_slice(&(!(take as u16)).to_le_bytes());
                out.extend_from_slice(&rest[..take]);
                rest = &rest[take..];
                if final_block {
                    break;
                }
            }
            out.extend_from_slice(&adler32(data).to_be_bytes());
            out
        }
        fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
            let mut body = Vec::with_capacity(4 + data.len());
            body.extend_from_slice(kind);
            body.extend_from_slice(data);
            let mut out = Vec::with_capacity(12 + data.len());
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            out.extend_from_slice(&body);
            out.extend_from_slice(&crc32(&body).to_be_bytes());
            out
        }

        let base = encode(ExportFormat::Png, w, h, rgba).unwrap();
        // Signature (8) + IHDR length/type/data/CRC (4 + 4 + 13 + 4).
        const AFTER_IHDR: usize = 8 + 4 + 4 + 13 + 4;
        assert_eq!(&base[12..16], b"IHDR");

        let mut iccp = Vec::new();
        iccp.extend_from_slice(b"probe"); // profile name
        iccp.push(0); // name terminator
        iccp.push(0); // compression method: deflate
        iccp.extend_from_slice(&zlib_stored(profile));

        let mut out = Vec::with_capacity(base.len() + iccp.len() + 12);
        out.extend_from_slice(&base[..AFTER_IHDR]);
        out.extend_from_slice(&chunk(b"iCCP", &iccp));
        out.extend_from_slice(&base[AFTER_IHDR..]);
        out
    }
}
