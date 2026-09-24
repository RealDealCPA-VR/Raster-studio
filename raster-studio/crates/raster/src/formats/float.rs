//! W11-H: the floating-point containers: OpenEXR (`.exr`, read and write) and
//! Radiance RGBE (`.hdr`, read).
//!
//! Both hold **linear-light** floating-point RGB, and an EXR's alpha is
//! *associated* (premultiplied), as the OpenEXR specification defines it.
//! The decoding is done by `image`'s `exr` (the pure-Rust `exr` crate) and
//! `hdr` codecs; this module owns what happens either side of them.
//!
//! # What a float file opens as
//!
//! File > Open makes an EXR or HDR a **32 Bits/Channel** document (W10-H's
//! `f32` tiles): the application reads [`decode_linear`]'s unclipped,
//! straight-alpha linear samples and stores them sRGB-encoded with the
//! curve extended past 1.0, so nothing brighter than diffuse white is lost
//! (`app-shell`'s `doc_float_open`).
//!
//! The [`DecodedSurface`] this module also returns (for the 8/16-bit
//! consumers: Place, the codec facade, a thumbnail) is display-encoded
//! 16-bit RGBA: the linear samples are un-premultiplied, run through the
//! sRGB curve and rounded to 16 bits. That surface is a *clip*: everything
//! from 0 to 1.0 is kept at 16-bit precision, anything brighter than 1.0 is
//! clipped to white, and a negative or NaN sample to black.
//!
//! # What an EXR export writes
//!
//! [`encode_exr`] takes the display-encoded 8- or 16-bit RGBA every other
//! encoder takes, undoes the sRGB curve, premultiplies, and writes 32-bit
//! float RGBA (`exr`'s default compression). A pixel that went out as 16-bit
//! sRGB comes back within one 16-bit code. [`encode_exr_linear`] writes a
//! 32-bit document's linear `f32` composite as it is, values above 1.0
//! included.
//!
//! # Untrusted input
//!
//! An EXR's header is read on its own first ([`exr::meta::MetaData`]), and
//! every part's data window *and* display window go through
//! [`ImportLimits`] before `image` reads a single block, so a header that
//! declares a vast data window behind a small display window costs nothing.
//! The float buffer and the 16-bit output are both counted against the
//! allocation ceiling before either exists.

use std::io::Cursor;

use image::{ImageDecoder, ImageEncoder};

use super::{check_decode, info, malformed};
use crate::codec::{
    CodecError, DecodedSurface, ImageInfo, ImportFormat, ImportLimits, SurfacePixels,
};

const EXR: &str = "OpenEXR";
const HDR: &str = "Radiance HDR";

/// The four bytes every OpenEXR file begins with.
pub const EXR_MAGIC: [u8; 4] = [0x76, 0x2F, 0x31, 0x01];

/// `true` for an OpenEXR file.
pub fn looks_like_exr(head: &[u8]) -> bool {
    head.starts_with(&EXR_MAGIC)
}

/// `true` for a Radiance RGBE file (`#?RADIANCE` or `#?RGBE`).
pub fn looks_like_hdr(head: &[u8]) -> bool {
    head.starts_with(b"#?RADIANCE") || head.starts_with(b"#?RGBE")
}

/// The sRGB transfer curve, linear to encoded, over `0..=1`.
fn srgb_encode(v: f32) -> f32 {
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// The sRGB transfer curve, encoded to linear, over `0..=1`.
fn srgb_decode(v: f32) -> f32 {
    if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// One `0..=1` value as the nearest 16-bit code; a NaN is 0.
fn to_code16(v: f32) -> u16 {
    if v.is_nan() {
        return 0;
    }
    (v.clamp(0.0, 1.0) * 65_535.0).round() as u16
}

/// Linear straight-alpha RGBA `f32` to display-encoded RGBA16, clipping
/// everything outside `0..=1` (see the module notes).
pub fn linear_to_rgba16(linear: &[f32]) -> Vec<u16> {
    let mut out = Vec::with_capacity(linear.len());
    for px in linear.as_chunks::<4>().0 {
        for c in &px[..3] {
            out.push(to_code16(srgb_encode(c.clamp(0.0, 1.0))));
        }
        out.push(to_code16(px[3]));
    }
    out
}

/// The header of an EXR, checked against `limits` window by window.
fn exr_dimensions(bytes: &[u8], limits: ImportLimits) -> Result<(u32, u32), CodecError> {
    if !looks_like_exr(bytes) {
        return Err(malformed(EXR, "no OpenEXR signature"));
    }
    let meta = exr::meta::MetaData::read_from_buffered(Cursor::new(bytes), false)
        .map_err(|e| malformed(EXR, e))?;
    let mut display = None;
    for header in &meta.headers {
        for size in [
            header.layer_size,
            header.shared_attributes.display_window.size,
        ] {
            let w = u32::try_from(size.width()).unwrap_or(u32::MAX);
            let h = u32::try_from(size.height()).unwrap_or(u32::MAX);
            limits.check_dimensions(w, h)?;
        }
        if display.is_none() {
            let size = header.shared_attributes.display_window.size;
            display = Some((size.width() as u32, size.height() as u32));
        }
    }
    display.ok_or_else(|| malformed(EXR, "the file has no parts"))
}

/// Header facts for an EXR or HDR.
pub fn probe(
    format: ImportFormat,
    bytes: &[u8],
    limits: ImportLimits,
) -> Result<ImageInfo, CodecError> {
    let (width, height) = match format {
        ImportFormat::Exr => exr_dimensions(bytes, limits)?,
        _ => {
            let decoder = hdr_decoder(bytes)?;
            decoder.dimensions()
        }
    };
    limits.check_dimensions(width, height)?;
    Ok(info(width, height, format, true))
}

fn hdr_decoder(bytes: &[u8]) -> Result<image::codecs::hdr::HdrDecoder<Cursor<&[u8]>>, CodecError> {
    if !looks_like_hdr(bytes) {
        return Err(malformed(HDR, "no #?RADIANCE / #?RGBE signature"));
    }
    image::codecs::hdr::HdrDecoder::new(Cursor::new(bytes)).map_err(|e| malformed(HDR, e))
}

/// Decode an EXR or HDR into **linear, straight-alpha** RGBA `f32`, unclipped.
pub fn decode_linear(
    format: ImportFormat,
    bytes: &[u8],
    limits: ImportLimits,
    extra_per_pixel: u64,
) -> Result<(u32, u32, Vec<f32>), CodecError> {
    let dynamic = match format {
        ImportFormat::Exr => {
            let (w, h) = exr_dimensions(bytes, limits)?;
            // The f32 RGBA read buffer, its conversion, and the caller's
            // output on top.
            check_decode(limits, w, h, 32 + extra_per_pixel, 0)?;
            let mut decoder = image::codecs::openexr::OpenExrDecoder::with_alpha_preference(
                Cursor::new(bytes),
                Some(true),
            )
            .map_err(|e| malformed(EXR, e))?;
            decoder
                .set_limits(limits.to_image_limits())
                .map_err(|e| malformed(EXR, e))?;
            image::DynamicImage::from_decoder(decoder).map_err(|e| malformed(EXR, e))?
        }
        _ => {
            let mut decoder = hdr_decoder(bytes)?;
            let (w, h) = decoder.dimensions();
            check_decode(limits, w, h, 12 + 16 + extra_per_pixel, 0)?;
            decoder
                .set_limits(limits.to_image_limits())
                .map_err(|e| malformed(HDR, e))?;
            image::DynamicImage::from_decoder(decoder).map_err(|e| malformed(HDR, e))?
        }
    };
    let (width, height) = (dynamic.width(), dynamic.height());
    let mut samples = dynamic.into_rgba32f().into_raw();
    if format == ImportFormat::Exr {
        // OpenEXR alpha is associated: straighten it.
        for px in samples.as_chunks_mut::<4>().0 {
            let a = px[3];
            if a > 0.0 && a.is_finite() {
                for c in &mut px[..3] {
                    *c /= a;
                }
            }
        }
    }
    Ok((width, height, samples))
}

/// Decode an EXR or HDR as a 16-bit display-encoded surface.
pub fn decode(
    format: ImportFormat,
    bytes: &[u8],
    limits: ImportLimits,
) -> Result<DecodedSurface, CodecError> {
    let (width, height, linear) = decode_linear(format, bytes, limits, 8)?;
    Ok(DecodedSurface {
        width,
        height,
        pixels: SurfacePixels::Rgba16(linear_to_rgba16(&linear)),
        color_space: color::ColorSpace::Srgb,
        icc_profile: None,
        source_format: format,
    })
}

/// Write display-encoded RGBA (8 or 16 bits a sample, straight alpha) as a
/// 32-bit float, premultiplied, linear-light OpenEXR.
pub fn encode_exr(
    width: u32,
    height: u32,
    samples: impl Iterator<Item = f32>,
) -> Result<Vec<u8>, CodecError> {
    let mut linear: Vec<f32> = Vec::with_capacity(width as usize * height as usize * 4);
    let mut px = [0f32; 4];
    for (i, v) in samples.enumerate() {
        px[i % 4] = v;
        if i % 4 == 3 {
            let a = px[3];
            for c in &px[..3] {
                linear.push(srgb_decode(*c) * a);
            }
            linear.push(a);
        }
    }
    write_rgba32f(width, height, &linear)
}

/// W11-H: write **linear-light, straight-alpha** RGBA `f32` (a 32 Bits/Channel
/// document's composite) as a premultiplied OpenEXR, unclipped: a sample
/// above 1.0 reaches the file as it is. A NaN or infinite sample is written
/// as 0; alpha is clamped to `0..=1`.
pub fn encode_exr_linear(width: u32, height: u32, straight: &[f32]) -> Result<Vec<u8>, CodecError> {
    let expected = width as usize * height as usize * 4;
    if straight.len() != expected {
        return Err(CodecError::InvalidParameter(format!(
            "{} samples for a {width}x{height} RGBA image, which needs {expected}",
            straight.len()
        )));
    }
    let finite = |v: f32| if v.is_finite() { v } else { 0.0 };
    let mut linear = Vec::with_capacity(expected);
    for px in straight.as_chunks::<4>().0 {
        let a = finite(px[3]).clamp(0.0, 1.0);
        for c in &px[..3] {
            linear.push(finite(*c) * a);
        }
        linear.push(a);
    }
    write_rgba32f(width, height, &linear)
}

/// Premultiplied linear RGBA `f32` as an OpenEXR file.
fn write_rgba32f(width: u32, height: u32, linear: &[f32]) -> Result<Vec<u8>, CodecError> {
    let mut out = Cursor::new(Vec::new());
    image::codecs::openexr::OpenExrEncoder::new(&mut out).write_image(
        bytemuck::cast_slice(linear),
        width,
        height,
        image::ExtendedColorType::Rgba32F,
    )?;
    Ok(out.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{
        decode_surface_bytes, encode, encode_into, probe_bytes, EncodeOptions, EncodedPixels,
        ExportFormat,
    };

    fn ramp(w: u32, h: u32) -> Vec<u8> {
        (0..w * h)
            .flat_map(|i| [(i * 37) as u8, (i * 11 + 5) as u8, 200, (255 - i * 3) as u8])
            .collect()
    }

    /// A Radiance file written by `image`'s own encoder (the RGBE writer),
    /// with one pixel brighter than diffuse white.
    fn hdr_file(w: u32, h: u32) -> Vec<u8> {
        let px: Vec<image::Rgb<f32>> = (0..w * h)
            .map(|i| match i {
                0 => image::Rgb([4.0, 0.0, 0.0]),
                1 => image::Rgb([0.214_041_14, 0.214_041_14, 0.214_041_14]),
                _ => image::Rgb([0.0, 0.5, 1.0]),
            })
            .collect();
        let mut out = Vec::new();
        image::codecs::hdr::HdrEncoder::new(&mut out)
            .encode(&px, w as usize, h as usize)
            .unwrap();
        out
    }

    #[test]
    fn an_exr_export_opens_again_within_one_16_bit_code() {
        let (w, h) = (7u32, 5u32);
        let rgba = ramp(w, h);
        let bytes = encode(ExportFormat::Exr, w, h, &rgba).unwrap();
        assert!(looks_like_exr(&bytes));
        let info = probe_bytes(&bytes, ImportLimits::default()).unwrap();
        assert_eq!(
            (info.width, info.height, info.format),
            (w, h, ImportFormat::Exr)
        );
        let s = decode_surface_bytes(&bytes, ImportLimits::default()).unwrap();
        assert_eq!(
            (s.width, s.height, s.source_format),
            (w, h, ImportFormat::Exr)
        );
        let SurfacePixels::Rgba16(back) = s.pixels else {
            panic!("an EXR opens at 16 bits")
        };
        for (i, (a, b)) in rgba.iter().zip(&back).enumerate() {
            let want = u32::from(*a) * 257;
            // Colour of a fully transparent pixel is not carried by a
            // premultiplied file; every other sample must round-trip.
            let alpha = rgba[i / 4 * 4 + 3];
            if alpha == 0 && i % 4 != 3 {
                continue;
            }
            assert!(
                want.abs_diff(u32::from(*b)) <= 64,
                "sample {i}: wrote {a} ({want}), read {b}"
            );
        }
    }

    #[test]
    fn a_16_bit_source_goes_out_to_exr_at_16_bit_precision() {
        let (w, h) = (3u32, 1u32);
        let deep: Vec<u16> = vec![
            1, 2, 3, 65_535, 30_001, 30_002, 30_003, 65_535, 65_535, 0, 12_345, 65_535,
        ];
        let mut out = Cursor::new(Vec::new());
        encode_into(
            &mut out,
            ExportFormat::Exr,
            w,
            h,
            EncodedPixels::Rgba16(&deep),
            &EncodeOptions::default(),
        )
        .unwrap();
        let s = decode_surface_bytes(&out.into_inner(), ImportLimits::default()).unwrap();
        let SurfacePixels::Rgba16(back) = s.pixels else {
            panic!()
        };
        for (a, b) in deep.iter().zip(&back) {
            assert!(a.abs_diff(*b) <= 1, "{a} came back as {b}");
        }
    }

    #[test]
    fn exr_alpha_is_premultiplied_in_the_file() {
        let bytes = encode(ExportFormat::Exr, 1, 1, &[255, 255, 255, 128]).unwrap();
        let (_, _, linear) = {
            // Read the raw associated samples through `image`, bypassing the
            // straightening this module does.
            let d = image::codecs::openexr::OpenExrDecoder::new(Cursor::new(&bytes[..])).unwrap();
            let img = image::DynamicImage::from_decoder(d).unwrap().into_rgba32f();
            (1, 1, img.into_raw())
        };
        let a = 128.0 / 255.0;
        assert!((linear[3] - a).abs() < 1e-6);
        assert!(
            (linear[0] - a).abs() < 1e-4,
            "{} is not premultiplied",
            linear[0]
        );
    }

    #[test]
    fn a_radiance_file_opens_clipped_above_diffuse_white() {
        let bytes = hdr_file(4, 2);
        assert!(looks_like_hdr(&bytes));
        let info = probe_bytes(&bytes, ImportLimits::default()).unwrap();
        assert_eq!(
            (info.width, info.height, info.format),
            (4, 2, ImportFormat::Hdr)
        );
        let s = decode_surface_bytes(&bytes, ImportLimits::default()).unwrap();
        assert_eq!(s.source_format, ImportFormat::Hdr);
        let SurfacePixels::Rgba16(px) = s.pixels else {
            panic!()
        };
        // 4.0 linear red is clipped to white red, not wrapped or scaled.
        assert_eq!(&px[0..4], &[65_535, 0, 0, 65_535]);
        // Linear 0.214 is sRGB mid-grey (code 128 of 255), within RGBE's
        // 8-bit mantissa.
        let mid = u32::from(px[4]);
        assert!(mid.abs_diff(128 * 257) < 400, "{mid}");
        // The unclipped samples are still there for a caller that wants them.
        let (_, _, linear) =
            decode_linear(ImportFormat::Hdr, &bytes, ImportLimits::default(), 0).unwrap();
        assert!(linear[0] > 3.9, "{}", linear[0]);
    }

    #[test]
    fn a_linear_export_keeps_values_above_diffuse_white() {
        // Straight linear RGBA: 6.5 red at half alpha, then an opaque
        // mid-grey with a NaN blue.
        let straight = [6.5, 0.25, 0.0, 0.5, 0.2, 0.2, f32::NAN, 1.0];
        let bytes = encode_exr_linear(2, 1, &straight).unwrap();
        let (w, h, back) =
            decode_linear(ImportFormat::Exr, &bytes, ImportLimits::default(), 0).unwrap();
        assert_eq!((w, h), (2, 1));
        for (i, (a, b)) in [6.5, 0.25, 0.0, 0.5, 0.2, 0.2, 0.0, 1.0]
            .iter()
            .zip(&back)
            .enumerate()
        {
            assert!((a - b).abs() < 1e-5, "sample {i}: wrote {a}, read {b}");
        }
        assert!(encode_exr_linear(2, 2, &straight).is_err(), "short buffer");
    }

    #[test]
    fn damaged_float_files_error_and_never_panic() {
        let exr = encode(ExportFormat::Exr, 6, 4, &ramp(6, 4)).unwrap();
        let hdr = hdr_file(6, 4);
        for file in [&exr, &hdr] {
            for cut in 0..file.len() {
                let _ = decode_surface_bytes(&file[..cut], ImportLimits::default());
                let _ = probe_bytes(&file[..cut], ImportLimits::default());
            }
            for i in 0..file.len() {
                let mut bad = file.clone();
                bad[i] ^= 0xA5;
                let _ = decode_surface_bytes(&bad, ImportLimits::default());
            }
        }
        // Truncated right after the magic is an error, by format name.
        let err = decode_surface_bytes(&exr[..8], ImportLimits::default()).unwrap_err();
        assert!(err.to_string().contains("OpenEXR"), "{err}");
    }

    #[test]
    fn a_vast_declared_exr_is_refused_before_it_is_read() {
        let bytes = encode(ExportFormat::Exr, 6, 4, &ramp(6, 4)).unwrap();
        let tight = ImportLimits {
            max_width: 5,
            ..ImportLimits::default()
        };
        assert!(matches!(
            decode_surface_bytes(&bytes, tight),
            Err(CodecError::LimitExceeded(_))
        ));
    }
}
