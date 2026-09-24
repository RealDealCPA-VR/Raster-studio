//! JPEG XL (`.jxl`), read only, through `jxl-oxide` (pure Rust, MIT OR
//! Apache-2.0, default features off: no threads, no C colour engine).
//!
//! What is read: the first keyframe of a bare codestream or an ISOBMFF-boxed
//! file, with its orientation applied, as RGBA - 8-bit for an image whose
//! samples are 8 bits or fewer, 16-bit above that. Greyscale is widened to
//! RGB. The decoder is asked for sRGB output; an XYB-encoded image (every
//! lossy JPEG XL) converts to it exactly. When the file's colour cannot reach
//! sRGB without an ICC engine (a lossless image with its own ICC profile),
//! the pixels stay in the file's space and that profile is carried as the
//! surface's [`color::ColorSpace::IccProfile`], as every other codec here
//! does. A CMYK image is refused by name. There is no writer: no pure-Rust
//! JPEG XL encoder is in the tree.
//!
//! # Untrusted input
//!
//! The header's size goes through [`ImportLimits`] before a frame is decoded,
//! and `jxl-oxide`'s own allocation tracker is capped at the decode ceiling
//! so a crafted frame cannot allocate past it.

use jxl_oxide::{AllocTracker, EnumColourEncoding, JxlImage, RenderingIntent};

use super::{check_decode, info, malformed};
use crate::codec::{
    icc_profile_space, CodecError, DecodedSurface, ImageInfo, ImportFormat, ImportLimits,
    SurfacePixels,
};

const NAME: &str = "JPEG XL";

/// The ISOBMFF signature box a boxed JPEG XL file opens with.
const CONTAINER: [u8; 12] = [
    0, 0, 0, 0x0C, b'J', b'X', b'L', b' ', 0x0D, 0x0A, 0x87, 0x0A,
];

/// `true` for a bare codestream (`FF 0A`) or a boxed file.
pub fn looks_like_jxl(head: &[u8]) -> bool {
    head.starts_with(&[0xFF, 0x0A]) || head.starts_with(&CONTAINER)
}

/// sRGB's CICP code point: BT.709 primaries, sRGB transfer, identity
/// matrix, full range.
const SRGB_CICP: [u8; 4] = [1, 13, 0, 1];

fn open(bytes: &[u8], limits: ImportLimits) -> Result<JxlImage, CodecError> {
    if !looks_like_jxl(bytes) {
        return Err(malformed(NAME, "no JPEG XL signature"));
    }
    let cap = usize::try_from(limits.max_alloc_bytes).unwrap_or(usize::MAX);
    JxlImage::builder()
        .alloc_tracker(AllocTracker::with_limit(cap))
        .read(bytes)
        .map_err(|e| malformed(NAME, e))
}

fn sixteen_bit(image: &JxlImage) -> bool {
    image.image_header().metadata.bit_depth.bits_per_sample() > 8
}

/// Header facts without decoding a frame.
pub fn probe(bytes: &[u8], limits: ImportLimits) -> Result<ImageInfo, CodecError> {
    let image = open(bytes, limits)?;
    let (width, height) = (image.width(), image.height());
    limits.check_dimensions(width, height)?;
    Ok(info(width, height, ImportFormat::Jxl, sixteen_bit(&image)))
}

/// Decode the first keyframe.
pub fn decode(bytes: &[u8], limits: ImportLimits) -> Result<DecodedSurface, CodecError> {
    let mut image = open(bytes, limits)?;
    let (width, height) = (image.width(), image.height());
    let sixteen = sixteen_bit(&image);
    // The render holds four f32 planes on top of the output.
    let planes = u64::from(width) * u64::from(height) * 16;
    check_decode(limits, width, height, if sixteen { 8 } else { 4 }, planes)?;

    let format = image.pixel_format();
    if format.has_black() {
        return Err(CodecError::Unsupported(
            "CMYK JPEG XL images are not supported; convert the image to RGB first".into(),
        ));
    }
    let grey = format.is_grayscale();
    image.request_color_encoding(if grey {
        EnumColourEncoding::gray_srgb(RenderingIntent::Relative)
    } else {
        EnumColourEncoding::srgb(RenderingIntent::Relative)
    });
    if image.num_loaded_keyframes() == 0 {
        return Err(malformed(NAME, "the file holds no complete frame"));
    }
    let render = image.render_frame(0).map_err(|e| malformed(NAME, e))?;
    let mut stream = render.stream();
    let (sw, sh, channels) = (stream.width(), stream.height(), stream.channels() as usize);
    if (sw, sh) != (width, height) || !(1..=4).contains(&channels) {
        return Err(malformed(
            NAME,
            format!("the frame renders {sw}x{sh}x{channels} for a {width}x{height} image"),
        ));
    }
    let pixels = width as usize * height as usize;
    let widen = |px: &[u16], out: &mut [u16; 4], max: u16| match channels {
        1 => *out = [px[0], px[0], px[0], max],
        2 => *out = [px[0], px[0], px[0], px[1]],
        3 => *out = [px[0], px[1], px[2], max],
        _ => *out = [px[0], px[1], px[2], px[3]],
    };
    let surface_pixels = if sixteen {
        let mut raw = vec![0u16; pixels * channels];
        stream.write_to_buffer(&mut raw);
        let mut out = Vec::with_capacity(pixels * 4);
        let mut px = [0u16; 4];
        for s in raw.chunks_exact(channels) {
            widen(s, &mut px, u16::MAX);
            out.extend_from_slice(&px);
        }
        SurfacePixels::Rgba16(out)
    } else {
        let mut raw = vec![0u8; pixels * channels];
        stream.write_to_buffer(&mut raw);
        let mut out = Vec::with_capacity(pixels * 4);
        let mut px = [0u16; 4];
        for s in raw.chunks_exact(channels) {
            let wide = [
                u16::from(s[0]),
                u16::from(*s.get(1).unwrap_or(&0)),
                u16::from(*s.get(2).unwrap_or(&0)),
                u16::from(*s.get(3).unwrap_or(&0)),
            ];
            widen(&wide[..channels], &mut px, 255);
            out.extend(px.iter().map(|v| *v as u8));
        }
        SurfacePixels::Rgba8(out)
    };

    // Whatever the decoder could not convert stays in the file's space, and
    // the profile describing that space travels with the pixels.
    let (color_space, icc_profile) = match image.rendered_cicp() {
        Some(cicp) if cicp == SRGB_CICP => (color::ColorSpace::Srgb, None),
        _ => {
            let icc = image.rendered_icc();
            if icc.is_empty() || icc.len() > limits.max_icc_bytes {
                (color::ColorSpace::Srgb, None)
            } else {
                (icc_profile_space(&icc), Some(icc))
            }
        }
    };
    Ok(DecodedSurface {
        width,
        height,
        pixels: surface_pixels,
        color_space,
        icc_profile,
        source_format: ImportFormat::Jxl,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 6x4 lossless JPEG XL with alpha, written by libjxl (`ffmpeg -c:v
    /// libjxl -distance 0`) from the RGBA ramp [`fixture_pixel`] describes:
    /// an independent encoder, so this is not our own writer grading itself.
    const LOSSLESS_RGBA: &[u8] = include_bytes!("testdata/ramp_6x4_rgba_lossless.jxl");

    /// The pixel the lossless fixture holds at `(x, y)`.
    fn fixture_pixel(x: u32, y: u32) -> [u8; 4] {
        [
            (x * 40) as u8,
            (y * 60) as u8,
            200,
            255 - (x * 30 + y * 10) as u8,
        ]
    }

    #[test]
    fn a_lossless_rgba_fixture_decodes_exactly() {
        assert!(looks_like_jxl(LOSSLESS_RGBA));
        let info = probe(LOSSLESS_RGBA, ImportLimits::default()).unwrap();
        assert_eq!((info.width, info.height), (6, 4));
        let s = decode(LOSSLESS_RGBA, ImportLimits::default()).unwrap();
        assert_eq!((s.width, s.height), (6, 4));
        assert_eq!(s.color_space, color::ColorSpace::Srgb);
        let SurfacePixels::Rgba8(v) = s.pixels else {
            panic!("expected 8-bit")
        };
        for y in 0..4 {
            for x in 0..6 {
                let i = ((y * 6 + x) * 4) as usize;
                assert_eq!(v[i..i + 4], fixture_pixel(x, y), "({x},{y})");
            }
        }
    }

    #[test]
    fn malformed_files_error_and_never_panic() {
        assert!(decode(b"\xff\x0a", ImportLimits::default()).is_err());
        assert!(decode(b"not a jxl", ImportLimits::default()).is_err());
        for n in 0..LOSSLESS_RGBA.len() {
            let _ = decode(&LOSSLESS_RGBA[..n], ImportLimits::default());
        }
        for i in 0..LOSSLESS_RGBA.len() {
            let mut bad = LOSSLESS_RGBA.to_vec();
            bad[i] ^= 0x5a;
            let _ = decode(&bad, ImportLimits::default());
        }
        // A tight ceiling refuses before decoding.
        let tight = ImportLimits {
            max_width: 4,
            ..ImportLimits::default()
        };
        assert!(matches!(
            decode(LOSSLESS_RGBA, tight),
            Err(CodecError::LimitExceeded(_))
        ));
    }
}
