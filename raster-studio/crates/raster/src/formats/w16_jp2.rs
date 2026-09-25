//! W16-L: JPEG 2000, a `.jp2` / `.jpf` container or a raw `.j2k` / `.j2c`
//! codestream, decoded by `hayro-jpeg2000` 0.3.1 (pure Rust, Apache-2.0 OR
//! MIT, `#![forbid(unsafe_code)]` in the crate; its `simd` feature is off,
//! so nothing under it uses `unsafe` either).
//!
//! The decoder yields 8 bits per channel whatever the file's precision, in
//! the file's colour space; grey and RGB open as they are (with alpha when
//! the file has it), CMYK is converted naively to RGB (no profile), an
//! embedded ICC profile is carried when it describes three channels.
//!
//! # Untrusted input
//!
//! The header (and so the declared size) is parsed before anything is
//! decoded, and checked against [`ImportLimits`] together with the
//! decoder's own buffer. The crate is tested by its authors on 20,000+ real
//! files and on the OpenJPEG conformance suite; this module's tests
//! truncate and bit-flip a fixture through the whole decode as well.

use hayro_jpeg2000::{ColorSpace, DecodeSettings, Image};

use super::super::{check_decode, info, malformed, rgba8_surface};
use crate::codec::{CodecError, DecodedSurface, ImageInfo, ImportFormat, ImportLimits};

const NAME: &str = "JPEG 2000";
const JP2: &[u8] = b"\x00\x00\x00\x0CjP  \r\n\x87\n";
const CODESTREAM: &[u8] = b"\xFF\x4F\xFF\x51";

/// `true` for a JP2 signature box or a raw codestream's SOC + SIZ.
pub fn looks_like_jp2(head: &[u8]) -> bool {
    head.starts_with(JP2) || head.starts_with(CODESTREAM)
}

fn open(bytes: &[u8]) -> Result<Image<'_>, CodecError> {
    Image::new(bytes, &DecodeSettings::default()).map_err(|e| malformed(NAME, e))
}

/// Header facts.
pub fn probe(bytes: &[u8], limits: ImportLimits) -> Result<ImageInfo, CodecError> {
    let image = open(bytes)?;
    limits.check_dimensions(image.width(), image.height())?;
    Ok(info(
        image.width(),
        image.height(),
        ImportFormat::Jp2,
        false,
    ))
}

/// Decode to RGBA8.
pub fn decode(bytes: &[u8], limits: ImportLimits) -> Result<DecodedSurface, CodecError> {
    let image = open(bytes)?;
    let (w, h) = (image.width(), image.height());
    let space = image.color_space().clone();
    let colour = usize::from(space.num_channels());
    let alpha = usize::from(image.has_alpha());
    let channels = colour + alpha;
    if !matches!(colour, 1 | 3 | 4) {
        return Err(CodecError::Unsupported(format!(
            "JPEG 2000 with {colour} colour channels is not supported"
        )));
    }
    let n = u64::from(w) * u64::from(h);
    check_decode(limits, w, h, 4, n.saturating_mul(channels as u64))?;
    let data = image.decode().map_err(|e| malformed(NAME, e))?;
    if data.len() as u64 != n * channels as u64 {
        return Err(malformed(
            NAME,
            "the decoder returned a buffer of the wrong size",
        ));
    }
    let mut out = Vec::with_capacity((n * 4) as usize);
    for p in data.chunks_exact(channels) {
        let a = if alpha == 1 { p[colour] } else { 255 };
        match colour {
            1 => out.extend_from_slice(&[p[0], p[0], p[0], a]),
            3 => out.extend_from_slice(&[p[0], p[1], p[2], a]),
            _ => {
                let k = 255 - u16::from(p[3]);
                let c = |v: u8| ((255 - u16::from(v)) * k / 255) as u8;
                out.extend_from_slice(&[c(p[0]), c(p[1]), c(p[2]), a]);
            }
        }
    }
    let mut s = rgba8_surface(w, h, out, ImportFormat::Jp2);
    if let ColorSpace::Icc {
        profile,
        num_channels: 3,
    } = space
    {
        s.color_space = crate::codec::icc_profile_space(&profile);
        s.icc_profile = Some(profile);
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::super::test_util::fuzz;
    use super::*;
    use crate::codec::{decode_surface_bytes, probe_bytes, SurfacePixels};

    // Written by OpenJPEG (through Pillow 12.1), lossless (5/3 wavelet):
    // the pixels below exactly.
    const RGB_JP2: &[u8] = include_bytes!("testdata/w16_ramp_6x4_rgb.jp2");
    const RGBA_J2K: &[u8] = include_bytes!("testdata/w16_ramp_5x3_rgba.j2k");
    const GREY_JP2: &[u8] = include_bytes!("testdata/w16_ramp_4x4_grey.jp2");

    fn ramp_rgb(w: u32, h: u32) -> Vec<u8> {
        (0..w * h)
            .flat_map(|i| [(i * 10) as u8, (255 - i * 9) as u8, (i * i) as u8, 255])
            .collect()
    }

    #[test]
    fn lossless_jpeg_2000_decodes_exactly() {
        assert!(looks_like_jp2(RGB_JP2) && looks_like_jp2(RGBA_J2K));
        let s = decode_surface_bytes(RGB_JP2, ImportLimits::default()).unwrap();
        assert_eq!(
            (s.width, s.height, s.source_format),
            (6, 4, ImportFormat::Jp2)
        );
        assert_eq!(s.pixels, SurfacePixels::Rgba8(ramp_rgb(6, 4)));
        let info = probe_bytes(RGB_JP2, ImportLimits::default()).unwrap();
        assert_eq!(
            (info.width, info.height, info.format),
            (6, 4, ImportFormat::Jp2)
        );
        // A raw codestream with alpha.
        let s = decode_surface_bytes(RGBA_J2K, ImportLimits::default()).unwrap();
        let want: Vec<u8> = (0..15u32)
            .flat_map(|i| {
                [
                    (i * 10) as u8,
                    (255 - i * 9) as u8,
                    (i * i) as u8,
                    (i * 17) as u8,
                ]
            })
            .collect();
        assert_eq!(s.pixels, SurfacePixels::Rgba8(want));
        // Grey.
        let s = decode_surface_bytes(GREY_JP2, ImportLimits::default()).unwrap();
        let want: Vec<u8> = (0..16u32)
            .flat_map(|i| [(i * 16) as u8; 3].into_iter().chain([255]))
            .collect();
        assert_eq!(s.pixels, SurfacePixels::Rgba8(want));
        for ext in ["jp2", "J2K", "jpf", "j2c"] {
            assert_eq!(ImportFormat::from_extension(ext), Some(ImportFormat::Jp2));
        }
    }

    #[test]
    fn damaged_jpeg_2000_errors_and_never_panics() {
        fuzz(RGB_JP2, ImportFormat::Jp2);
        fuzz(RGBA_J2K, ImportFormat::Jp2);
        let tight = ImportLimits {
            max_width: 5,
            ..ImportLimits::default()
        };
        assert!(matches!(
            decode_surface_bytes(RGB_JP2, tight),
            Err(CodecError::LimitExceeded(_))
        ));
    }
}
