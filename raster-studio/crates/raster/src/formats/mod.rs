//! W10-F: the containers this crate decodes (and, for some, encodes) with its
//! own code or a pure-Rust crate other than `image`.
//!
//! A child module of [`crate::codec`] (declared there with `#[path]`), so the
//! codec facade stays the one place the application talks to a decoder. Every
//! entry point here takes the file's bytes, already read through a bounded
//! reader by [`read_bounded`], and an [`ImportLimits`] that is checked against
//! the *declared* dimensions before any pixel buffer exists.
//!
//! | Format | Read | Write | Module |
//! | --- | --- | --- | --- |
//! | PPM / PGM / PBM (ASCII and binary) | yes, 1-16 bit | binary P6 / P5 / P4 | [`pnm`] |
//! | DDS | uncompressed RGB(A)/luminance, BC1, BC2, BC3 | uncompressed BGRA, BC3 | [`dds`] |
//! | GIMP XCF | 8-bit RGB/RGBA/grey layers, flattened (see [`xcf`]) | no | [`xcf`] |
//! | JPEG XL | yes (`jxl-oxide`) | no | [`jxl`] |
//! | AVIF | yes (`avif-parse` + `rav1d`) | yes (`image` over `ravif`) | [`avif`] |
//! | PSB | through the `psd` crate, as a layered document | no | - |
//!
//! HEIC is the one format of this wave with no reader. The only pure-Rust HEVC
//! decoders on crates.io are `heic` (AGPL-3.0-only or a commercial licence,
//! which a proprietary build cannot take) and `heic-rs` (a 0.1.1 release that
//! this wave did not evaluate for correctness or fuzz-safety); everything else
//! binds libheif / libde265 (C/C++). A `.heic` is refused by name, see
//! [`heic_refusal`], instead of reaching `image` and failing as "unknown
//! format".

use std::io::{BufRead, Read, Seek};

use super::{read_head, CodecError, DecodedSurface, ImageInfo, ImportFormat, ImportLimits};

pub mod avif;
pub mod dds;
pub mod jxl;
pub mod pnm;
pub mod xcf;

/// How many leading bytes the sniff looks at.
const SNIFF_BYTES: usize = 32;

/// The formats this module owns, identified by content.
pub fn sniff(head: &[u8]) -> Option<ImportFormat> {
    if pnm::looks_like_pnm(head) {
        Some(ImportFormat::Pnm)
    } else if dds::looks_like_dds(head) {
        Some(ImportFormat::Dds)
    } else if xcf::looks_like_xcf(head) {
        Some(ImportFormat::Xcf)
    } else if jxl::looks_like_jxl(head) {
        Some(ImportFormat::Jxl)
    } else if avif::looks_like_avif(head) {
        Some(ImportFormat::Avif)
    } else {
        None
    }
}

/// `true` when `head` is an ISOBMFF `ftyp` box naming a HEIF/HEIC brand (and
/// not an AVIF one).
pub fn looks_like_heic(head: &[u8]) -> bool {
    if head.len() < 12 || &head[4..8] != b"ftyp" {
        return false;
    }
    matches!(
        &head[8..12],
        b"heic" | b"heix" | b"heim" | b"heis" | b"hevc" | b"hevx" | b"mif1" | b"msf1"
    ) && !avif::looks_like_avif(head)
}

/// The refusal a HEIC file gets, naming why rather than "unknown format".
pub fn heic_refusal() -> CodecError {
    CodecError::Unsupported(
        "HEIC/HEIF is not supported: no pure-Rust HEVC decoder under a licence this \
         build can ship exists yet (see docs/parity-matrix.md); convert it to JPEG, PNG \
         or AVIF first"
            .into(),
    )
}

/// Which of this module's formats `source` holds, by content; the stream is
/// left where it was found. `Err` for a HEIC, which is refused by name.
pub(super) fn sniff_source<R: BufRead + Seek>(
    source: &mut R,
) -> Result<Option<ImportFormat>, CodecError> {
    let start = source.stream_position()?;
    let mut head = [0u8; SNIFF_BYTES];
    let filled = read_head(source, &mut head)?;
    source.seek(std::io::SeekFrom::Start(start))?;
    let head = &head[..filled];
    if looks_like_heic(head) {
        return Err(heic_refusal());
    }
    Ok(sniff(head))
}

/// Read the rest of `source`, refusing a file larger than any decode `limits`
/// allows could come from.
///
/// The bound is four times the decode allocation ceiling: an ASCII PPM spends
/// up to four characters per sample, which is the most any of these formats
/// spends per decoded byte. It bounds what the *read* may hold and is checked
/// as the read grows rather than trusted from a length field.
pub(super) fn read_bounded<R: Read>(
    source: R,
    limits: ImportLimits,
) -> Result<Vec<u8>, CodecError> {
    let cap = limits.max_alloc_bytes.saturating_mul(4);
    let mut data = Vec::new();
    source.take(cap.saturating_add(1)).read_to_end(&mut data)?;
    if data.len() as u64 > cap {
        return Err(CodecError::LimitExceeded(format!(
            "the file is larger than {cap} bytes"
        )));
    }
    Ok(data)
}

/// Header facts for one of this module's formats.
pub(super) fn probe<R: Read>(
    format: ImportFormat,
    source: R,
    limits: ImportLimits,
) -> Result<ImageInfo, CodecError> {
    let bytes = read_bounded(source, limits)?;
    match format {
        ImportFormat::Pnm => pnm::probe(&bytes, limits),
        ImportFormat::Dds => dds::probe(&bytes, limits),
        ImportFormat::Xcf => xcf::probe(&bytes, limits),
        ImportFormat::Jxl => jxl::probe(&bytes, limits),
        ImportFormat::Avif => avif::probe(&bytes, limits),
        other => Err(not_ours(other)),
    }
}

/// Decode one of this module's formats.
pub(super) fn decode<R: Read>(
    format: ImportFormat,
    source: R,
    limits: ImportLimits,
) -> Result<DecodedSurface, CodecError> {
    let bytes = read_bounded(source, limits)?;
    match format {
        ImportFormat::Pnm => pnm::decode(&bytes, limits),
        ImportFormat::Dds => dds::decode(&bytes, limits),
        ImportFormat::Xcf => xcf::decode(&bytes, limits),
        ImportFormat::Jxl => jxl::decode(&bytes, limits),
        ImportFormat::Avif => avif::decode(&bytes, limits),
        other => Err(not_ours(other)),
    }
}

fn not_ours(format: ImportFormat) -> CodecError {
    CodecError::Unsupported(format!("{} is not read by this module", format.name()))
}

/// A malformed-file error in this module's vocabulary.
pub(crate) fn malformed(format: &str, what: impl std::fmt::Display) -> CodecError {
    CodecError::Unsupported(format!("malformed {format} file: {what}"))
}

/// Check the dimensions, and the RGBA buffer the decode will allocate, before
/// it does. `bytes_per_pixel` is the storage size (4 for RGBA8, 8 for RGBA16);
/// `extra` is any further live allocation (a source plane, a layer).
pub(crate) fn check_decode(
    limits: ImportLimits,
    width: u32,
    height: u32,
    bytes_per_pixel: u64,
    extra: u64,
) -> Result<(), CodecError> {
    limits.check_dimensions(width, height)?;
    let out = u64::from(width)
        .saturating_mul(u64::from(height))
        .saturating_mul(bytes_per_pixel);
    limits.check_alloc(out.saturating_add(extra))
}

/// A straight-alpha sRGB RGBA8 surface.
pub(crate) fn rgba8_surface(
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    source_format: ImportFormat,
) -> DecodedSurface {
    DecodedSurface {
        width,
        height,
        pixels: super::SurfacePixels::Rgba8(rgba),
        color_space: color::ColorSpace::Srgb,
        icc_profile: None,
        source_format,
    }
}

/// Header facts for a decode with no ICC profile.
pub(crate) fn info(width: u32, height: u32, format: ImportFormat, sixteen: bool) -> ImageInfo {
    ImageInfo {
        width,
        height,
        format,
        pixel_format: if sixteen {
            super::PixelFormat::Rgba16
        } else {
            super::PixelFormat::Rgba8
        },
        icc_profile: None,
    }
}
