//! W16-L: the formats the final parity audit found missing.
//!
//! A child of [`super`] (the `formats` module), so every reader here is
//! reached through the codec facade's own content sniff and extension hint,
//! like the W10-F / W11-H / W13 readers beside it.
//!
//! | Format | What opens | Module |
//! | --- | --- | --- |
//! | JPEG 2000 (`.jp2` / `.j2k` / `.jpf`) | the image, 8 bits per channel, grey / RGB / CMYK (converted) with alpha, through `hayro-jpeg2000` (pure Rust, `forbid(unsafe_code)`, SIMD off) | [`jp2`] |
//! | Clip Studio Paint `.clip` | the canvas preview PNG stored in the file's SQLite database (`CanvasPreview.ImageData`); the layers are **not** read | [`clip`] |
//! | Pixelmator Pro `.pxd` | a zipped `.pxd` package's `QuickLook/Thumbnail.tiff` (or `Icon.tiff`); the layers are **not** read | [`pxd`] |
//! | Valve `.vtf` | the largest mip of frame 0, face 0: RGBA / ABGR / ARGB / BGRA / BGRX 8888, RGB / BGR 888 (and blue-screen), RGB / BGR 565, BGRX / BGRA 5551, BGRA 4444, I8, IA88, A8, UV88, UVWQ8888, DXT1 / DXT3 / DXT5, RGBA16161616 (16-bit) and RGBA16161616F | [`vtf`] |
//! | FITS | the primary (or first image) HDU, BITPIX 8 / 16 / 32 / 64 / -32 / -64 with BSCALE / BZERO / BLANK, the first plane (or three planes as RGB), stretched linearly min..max to 16 bits, flipped so FITS's bottom row is at the bottom | [`fits`] |
//! | DICOM | the first frame, explicit / implicit VR little endian or RLE Lossless, MONOCHROME1 / 2 (8-16 bit, rescale slope / intercept, the file's window centre / width, else min..max) and 8-bit RGB / YBR_FULL; JPEG-family transfer syntaxes refused by name | [`dicom`] |
//! | AutoCAD DXF (ASCII) | LINE, LWPOLYLINE (with bulges), POLYLINE / VERTEX, CIRCLE, ARC, ELLIPSE, SPLINE (B-spline sampled), POINT, TEXT / MTEXT, drawn through `resvg`; binary DXF refused by name | [`dxf`] |
//! | CorelDRAW `.cdr` | the embedded thumbnail (a zipped X4+ file's `metadata/thumbnails/thumbnail.bmp` or `previews/thumbnail.png`; a RIFF file's `DISP` bitmap) | [`foreign`] |
//! | InDesign `.indd` | the JPEG thumbnail in its XMP packet | [`foreign`] |
//! | Affinity Photo, PaintTool SAI | **refused by name**, saying why | [`foreign`] |
//!
//! # Untrusted input
//!
//! Every reader takes the bytes [`super::read_bounded`] read and checks the
//! declared dimensions against [`ImportLimits`] before a pixel buffer exists;
//! every offset is checked before it is followed; every loop over a count
//! the file declares is bounded by the bytes that remain. Each module's
//! tests truncate and bit-flip a fixture through the whole decode.

use super::malformed;
use crate::codec::{CodecError, DecodedSurface, ImageInfo, ImportFormat, ImportLimits};

#[path = "w16_clip.rs"]
pub mod clip;
#[path = "w16_dicom.rs"]
pub mod dicom;
#[path = "w16_dxf.rs"]
pub mod dxf;
#[path = "w16_fits.rs"]
pub mod fits;
#[path = "w16_foreign.rs"]
pub mod foreign;
#[path = "w16_jp2.rs"]
pub mod jp2;
#[path = "w16_pxd.rs"]
pub mod pxd;
#[path = "w16_sqlite.rs"]
pub mod sqlite;
#[path = "w16_vtf.rs"]
pub mod vtf;
#[path = "w16_zip.rs"]
pub mod zip;

/// How many leading bytes [`sniff_prefix`] needs: a DICOM Part 10 file says
/// `DICM` at offset 128.
pub const SNIFF_PREFIX: usize = 132;

/// The formats this module owns that are recognised from the first 64 bytes.
pub fn sniff(head: &[u8]) -> Option<ImportFormat> {
    if jp2::looks_like_jp2(head) {
        Some(ImportFormat::Jp2)
    } else if clip::looks_like_clip(head) {
        Some(ImportFormat::Clip)
    } else if vtf::looks_like_vtf(head) {
        Some(ImportFormat::Vtf)
    } else if fits::looks_like_fits(head) {
        Some(ImportFormat::Fits)
    } else if dxf::looks_like_dxf(head) {
        Some(ImportFormat::Dxf)
    } else if foreign::looks_like_riff_cdr(head) {
        Some(ImportFormat::Cdr)
    } else if foreign::looks_like_indd(head) {
        Some(ImportFormat::Indd)
    } else {
        None
    }
}

/// The formats recognised from a longer prefix ([`SNIFF_PREFIX`] bytes).
pub fn sniff_prefix(prefix: &[u8]) -> Option<ImportFormat> {
    dicom::looks_like_dicom(prefix).then_some(ImportFormat::Dicom)
}

/// Whether `format` is one of this module's.
pub fn owns(format: ImportFormat) -> bool {
    matches!(
        format,
        ImportFormat::Jp2
            | ImportFormat::Clip
            | ImportFormat::Pxd
            | ImportFormat::Vtf
            | ImportFormat::Fits
            | ImportFormat::Dicom
            | ImportFormat::Dxf
            | ImportFormat::Cdr
            | ImportFormat::AfPhoto
            | ImportFormat::Sai
            | ImportFormat::Indd
    )
}

/// Header facts.
pub fn probe(
    format: ImportFormat,
    bytes: &[u8],
    limits: ImportLimits,
) -> Result<ImageInfo, CodecError> {
    match format {
        ImportFormat::Jp2 => jp2::probe(bytes, limits),
        ImportFormat::Vtf => vtf::probe(bytes, limits),
        ImportFormat::Fits => fits::probe(bytes, limits),
        ImportFormat::Dicom => dicom::probe(bytes, limits),
        ImportFormat::AfPhoto => Err(foreign::affinity_refusal()),
        ImportFormat::Sai => Err(foreign::sai_refusal()),
        // The container formats and DXF have no cheaper header than the
        // decode itself (a preview is itself a file to probe; a DXF's size
        // is its drawing's extent).
        other => {
            let s = decode(other, bytes, limits)?;
            Ok(super::info(
                s.width,
                s.height,
                other,
                matches!(s.pixels, crate::codec::SurfacePixels::Rgba16(_)),
            ))
        }
    }
}

/// Decode.
pub fn decode(
    format: ImportFormat,
    bytes: &[u8],
    limits: ImportLimits,
) -> Result<DecodedSurface, CodecError> {
    match format {
        ImportFormat::Jp2 => jp2::decode(bytes, limits),
        ImportFormat::Clip => clip::decode(bytes, limits),
        ImportFormat::Pxd => pxd::decode(bytes, limits),
        ImportFormat::Vtf => vtf::decode(bytes, limits),
        ImportFormat::Fits => fits::decode(bytes, limits),
        ImportFormat::Dicom => dicom::decode(bytes, limits),
        ImportFormat::Dxf => dxf::decode(bytes, limits),
        ImportFormat::Cdr => foreign::decode_cdr(bytes, limits),
        ImportFormat::Indd => foreign::decode_indd(bytes, limits),
        ImportFormat::AfPhoto => Err(foreign::affinity_refusal()),
        ImportFormat::Sai => Err(foreign::sai_refusal()),
        other => Err(CodecError::Unsupported(format!(
            "{} is not read by the W16 readers",
            other.name()
        ))),
    }
}

/// Decode an embedded image (a preview or thumbnail) through the codec
/// facade, by its content, and report it as `as_format`.
pub(crate) fn decode_embedded(
    name: &str,
    bytes: &[u8],
    limits: ImportLimits,
    as_format: ImportFormat,
) -> Result<DecodedSurface, CodecError> {
    let mut s = crate::codec::decode_surface_bytes(bytes, limits).map_err(|e| match e {
        CodecError::LimitExceeded(_) => e,
        other => malformed(
            name,
            format!("its embedded preview does not decode: {other}"),
        ),
    })?;
    s.source_format = as_format;
    Ok(s)
}

/// `len` bytes at `at`, or a malformed-file error naming `name`.
pub(crate) fn bytes_at<'a>(
    b: &'a [u8],
    at: usize,
    len: usize,
    name: &str,
) -> Result<&'a [u8], CodecError> {
    at.checked_add(len)
        .and_then(|end| b.get(at..end))
        .ok_or_else(|| malformed(name, "the file ends inside a record"))
}

pub(crate) fn u16le(b: &[u8], at: usize, name: &str) -> Result<u16, CodecError> {
    let s = bytes_at(b, at, 2, name)?;
    Ok(u16::from_le_bytes([s[0], s[1]]))
}

pub(crate) fn u32le(b: &[u8], at: usize, name: &str) -> Result<u32, CodecError> {
    let s = bytes_at(b, at, 4, name)?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

pub(crate) fn u64be(b: &[u8], at: usize, name: &str) -> Result<u64, CodecError> {
    let s = bytes_at(b, at, 8, name)?;
    let mut a = [0u8; 8];
    a.copy_from_slice(s);
    Ok(u64::from_be_bytes(a))
}

/// Shared test helpers: a ZIP writer and a fuzz loop.
#[cfg(test)]
pub(crate) mod test_util {
    use crate::codec::{ImportFormat, ImportLimits};
    use std::io::Write;

    /// A ZIP archive of `entries`, deflated when `deflate` (CRCs are zero:
    /// the readers here do not check them).
    pub fn zip(entries: &[(&str, &[u8])], deflate: bool) -> Vec<u8> {
        let mut out = Vec::new();
        let mut dir = Vec::new();
        for (name, data) in entries {
            let method: u16 = if deflate { 8 } else { 0 };
            let stored = if deflate {
                let mut e =
                    flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
                e.write_all(data).unwrap();
                e.finish().unwrap()
            } else {
                data.to_vec()
            };
            let local = out.len() as u32;
            out.extend_from_slice(b"PK\x03\x04");
            out.extend_from_slice(&[20, 0, 0, 0]);
            out.extend_from_slice(&method.to_le_bytes());
            out.extend_from_slice(&[0; 8]);
            out.extend_from_slice(&(stored.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&[0, 0]);
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(&stored);
            dir.extend_from_slice(b"PK\x01\x02");
            dir.extend_from_slice(&[20, 0, 20, 0, 0, 0]);
            dir.extend_from_slice(&method.to_le_bytes());
            dir.extend_from_slice(&[0; 8]);
            dir.extend_from_slice(&(stored.len() as u32).to_le_bytes());
            dir.extend_from_slice(&(data.len() as u32).to_le_bytes());
            dir.extend_from_slice(&(name.len() as u16).to_le_bytes());
            dir.extend_from_slice(&[0; 12]);
            dir.extend_from_slice(&local.to_le_bytes());
            dir.extend_from_slice(name.as_bytes());
        }
        let dir_at = out.len() as u32;
        out.extend_from_slice(&dir);
        out.extend_from_slice(b"PK\x05\x06");
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(dir.len() as u32).to_le_bytes());
        out.extend_from_slice(&dir_at.to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out
    }

    /// Truncate `file` at every length (stepping through a big file) and
    /// flip bytes throughout it, decoding and probing each through the
    /// facade with `format` as the hint: none may panic.
    pub fn fuzz(file: &[u8], format: ImportFormat) {
        let step = (file.len() / 700).max(1);
        let limits = ImportLimits::default();
        let mut cut = 0;
        while cut < file.len() {
            let _ = crate::codec::decode_surface_bytes_as(&file[..cut], limits, format);
            let _ = crate::codec::probe_bytes_as(&file[..cut], limits, format);
            cut += step;
        }
        let mut i = 0;
        while i < file.len() {
            for flip in [0xFFu8, 0x80, 0x01] {
                let mut bad = file.to_vec();
                bad[i] ^= flip;
                let _ = crate::codec::decode_surface_bytes_as(&bad, limits, format);
            }
            i += step;
        }
    }

    /// A little RGBA ramp.
    pub fn ramp(w: u32, h: u32) -> Vec<u8> {
        (0..w * h)
            .flat_map(|i| {
                [
                    (i * 13) as u8,
                    (i * 7 + 40) as u8,
                    200u8.wrapping_sub(i as u8),
                    255,
                ]
            })
            .collect()
    }
}
