//! W16-L: Valve Texture Format (`.vtf`), versions 7.0-7.5.
//!
//! What opens is the largest mip level of frame 0, face 0, slice 0: the
//! texture itself, as the Source engine draws it.
//!
//! The header is little endian: `VTF\0`, the version (major 7, minor 0-5),
//! the header size, width and height, flags, the frame count and first
//! frame, reflectivity and bump scale, the high-resolution format, the mip
//! count, the low-resolution (thumbnail) format and size, then (7.2+) the
//! depth and (7.3+) a resource directory. Before 7.3 the thumbnail follows
//! the header and the high-resolution data follows it, mips smallest first,
//! each holding every frame, face and slice; from 7.3 the resource tagged
//! `0x30` says where the high-resolution data starts.
//!
//! Formats read: RGBA8888, ABGR8888, RGB888, BGR888, RGB565, I8, IA88, A8,
//! RGB888 / BGR888 blue-screen (the key colour becomes transparent),
//! ARGB8888, BGRA8888, DXT1, DXT3, DXT5, BGRX8888, BGR565, BGRX5551,
//! BGRA4444, DXT1 with one-bit alpha, BGRA5551, UV88, UVWQ8888,
//! RGBA16161616F (clipped to 0..1, 8 bits) and RGBA16161616 (16 bits).
//! P8 (paletted, never written by Valve's tools) is refused by name.

use super::super::{check_decode, info, malformed, rgba8_surface};
use super::{bytes_at, u16le, u32le};
use crate::codec::{CodecError, DecodedSurface, ImageInfo, ImportFormat, ImportLimits};

const NAME: &str = "VTF";
const ENVMAP: u32 = 0x4000;

/// `true` when `head` starts with `VTF\0`.
pub fn looks_like_vtf(head: &[u8]) -> bool {
    head.starts_with(b"VTF\0")
}

#[derive(Debug, Clone, Copy)]
struct Header {
    minor: u32,
    header_size: usize,
    width: u32,
    height: u32,
    flags: u32,
    frames: u64,
    first_frame: u16,
    format: u32,
    mips: u32,
    low_format: u32,
    low_w: u32,
    low_h: u32,
    depth: u64,
    /// 7.3+: where the high-resolution data starts.
    high_at: Option<usize>,
}

fn header(b: &[u8]) -> Result<Header, CodecError> {
    if !looks_like_vtf(b) {
        return Err(malformed(NAME, "it does not start with VTF"));
    }
    let major = u32le(b, 4, NAME)?;
    let minor = u32le(b, 8, NAME)?;
    if major != 7 || minor > 5 {
        return Err(CodecError::Unsupported(format!(
            "VTF version {major}.{minor} is not supported (7.0-7.5 are)"
        )));
    }
    let header_size = u32le(b, 12, NAME)? as usize;
    let width = u32::from(u16le(b, 16, NAME)?);
    let height = u32::from(u16le(b, 18, NAME)?);
    let flags = u32le(b, 20, NAME)?;
    let frames = u64::from(u16le(b, 24, NAME)?.max(1));
    let first_frame = u16le(b, 26, NAME)?;
    let format = u32le(b, 52, NAME)?;
    let mips = u32::from(bytes_at(b, 56, 1, NAME)?[0]).max(1);
    let low_format = u32le(b, 57, NAME)?;
    let low = bytes_at(b, 61, 2, NAME)?;
    let depth = if minor >= 2 {
        u64::from(u16le(b, 63, NAME)?.max(1))
    } else {
        1
    };
    let mut high_at = None;
    if minor >= 3 {
        let count = u32le(b, 68, NAME)? as usize;
        let mut found = None;
        for i in 0..count.min(64) {
            let at = 80 + i * 8;
            let entry = bytes_at(b, at, 8, NAME)?;
            if entry[..3] == [0x30, 0, 0] {
                found = Some(u32le(b, at + 4, NAME)? as usize);
            }
        }
        high_at =
            Some(found.ok_or_else(|| malformed(NAME, "it has no high-resolution image resource"))?);
    }
    if width == 0 || height == 0 {
        return Err(malformed(NAME, "the texture has no pixels"));
    }
    Ok(Header {
        minor,
        header_size,
        width,
        height,
        flags,
        frames,
        first_frame,
        format,
        mips,
        low_format,
        low_w: u32::from(low[0]),
        low_h: u32::from(low[1]),
        depth,
        high_at,
    })
}

/// Bytes one `w` x `h` image of `format` takes, or `None` for an unknown
/// format.
fn image_bytes(format: u32, w: u32, h: u32) -> Option<u64> {
    let (w, h) = (u64::from(w.max(1)), u64::from(h.max(1)));
    let blocks = w.div_ceil(4) * h.div_ceil(4);
    Some(match format {
        13 | 20 => blocks * 8,
        14 | 15 => blocks * 16,
        _ => w * h * bytes_per_pixel(format)?,
    })
}

fn bytes_per_pixel(format: u32) -> Option<u64> {
    Some(match format {
        0 | 1 | 11 | 12 | 16 | 23 => 4,
        2 | 3 | 9 | 10 => 3,
        4 | 6 | 17 | 18 | 19 | 21 | 22 => 2,
        5 | 7 | 8 => 1,
        24 | 25 => 8,
        _ => return None,
    })
}

fn format_name(format: u32) -> String {
    match format {
        7 => "P8 (paletted)".into(),
        other => format!("image format {other}"),
    }
}

/// Where mip 0 of frame 0, face 0, slice 0 starts, and its byte length.
fn locate(b: &[u8], h: &Header) -> Result<(usize, usize), CodecError> {
    let unknown =
        || CodecError::Unsupported(format!("VTF {} is not supported", format_name(h.format)));
    let faces: u64 = if h.flags & ENVMAP == 0 {
        1
    } else if h.minor < 5 && h.first_frame != 0xFFFF {
        7
    } else {
        6
    };
    let mut start = match h.high_at {
        Some(at) => at as u64,
        None => {
            let low = if h.low_format == u32::MAX || h.low_w == 0 || h.low_h == 0 {
                0
            } else {
                image_bytes(h.low_format, h.low_w, h.low_h)
                    .ok_or_else(|| malformed(NAME, "the thumbnail format is unknown"))?
            };
            h.header_size as u64 + low
        }
    };
    // Smallest mips first: skip every level but the largest.
    for level in (1..h.mips.min(16)).rev() {
        let size =
            image_bytes(h.format, h.width >> level, h.height >> level).ok_or_else(unknown)?;
        let depth = (h.depth >> level).max(1);
        start = start.saturating_add(size.saturating_mul(h.frames * faces * depth));
    }
    let len = image_bytes(h.format, h.width, h.height).ok_or_else(unknown)?;
    let end = start.saturating_add(len);
    if end > b.len() as u64 {
        return Err(malformed(NAME, "the image data runs past the file"));
    }
    Ok((start as usize, len as usize))
}

/// Header facts.
pub fn probe(bytes: &[u8], limits: ImportLimits) -> Result<ImageInfo, CodecError> {
    let h = header(bytes)?;
    limits.check_dimensions(h.width, h.height)?;
    Ok(info(h.width, h.height, ImportFormat::Vtf, h.format == 25))
}

fn expand(v: u16, bits: u32) -> u8 {
    let max = (1u32 << bits) - 1;
    ((u32::from(v) * 255 + max / 2) / max) as u8
}

fn rgb565(c: u16) -> [u8; 3] {
    [
        expand(c >> 11, 5),
        expand((c >> 5) & 0x3F, 6),
        expand(c & 0x1F, 5),
    ]
}

fn dxt_colors(block: &[u8], four: bool) -> [[u8; 4]; 16] {
    let c0 = u16::from_le_bytes([block[0], block[1]]);
    let c1 = u16::from_le_bytes([block[2], block[3]]);
    let (a, b) = (rgb565(c0), rgb565(c1));
    let mix = |p: u16, q: u16, d: u16| -> [u8; 3] {
        std::array::from_fn(|i| ((u16::from(a[i]) * p + u16::from(b[i]) * q + d / 2) / d) as u8)
    };
    let palette: [[u8; 4]; 4] = if four || c0 > c1 {
        let m = mix(2, 1, 3);
        let n = mix(1, 2, 3);
        [
            [a[0], a[1], a[2], 255],
            [b[0], b[1], b[2], 255],
            [m[0], m[1], m[2], 255],
            [n[0], n[1], n[2], 255],
        ]
    } else {
        let m = mix(1, 1, 2);
        [
            [a[0], a[1], a[2], 255],
            [b[0], b[1], b[2], 255],
            [m[0], m[1], m[2], 255],
            [0, 0, 0, 0],
        ]
    };
    let bits = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
    std::array::from_fn(|i| palette[((bits >> (i * 2)) & 3) as usize])
}

fn dxt5_alpha(block: &[u8]) -> [u8; 16] {
    let (a0, a1) = (u16::from(block[0]), u16::from(block[1]));
    let palette: [u8; 8] = std::array::from_fn(|i| {
        let i = i as u16;
        match i {
            0 => a0 as u8,
            1 => a1 as u8,
            _ if a0 > a1 => (((8 - i) * a0 + (i - 1) * a1) / 7) as u8,
            6 => 0,
            7 => 255,
            _ => (((6 - i) * a0 + (i - 1) * a1) / 5) as u8,
        }
    });
    let mut bits = 0u64;
    for (i, b) in block[2..8].iter().enumerate() {
        bits |= u64::from(*b) << (8 * i);
    }
    std::array::from_fn(|i| palette[((bits >> (i * 3)) & 7) as usize])
}

fn decode_blocks(format: u32, data: &[u8], w: u32, h: u32, out: &mut [u8]) {
    let size = if format == 13 || format == 20 { 8 } else { 16 };
    let bw = w.div_ceil(4) as usize;
    for (n, block) in data.chunks_exact(size).enumerate() {
        let (bx, by) = ((n % bw) as u32 * 4, (n / bw) as u32 * 4);
        let texels = match format {
            13 | 20 => dxt_colors(block, false),
            14 => {
                let mut t = dxt_colors(&block[8..], true);
                for (i, px) in t.iter_mut().enumerate() {
                    let nibble = (block[i / 2] >> ((i % 2) * 4)) & 0xF;
                    px[3] = nibble * 17;
                }
                t
            }
            _ => {
                let mut t = dxt_colors(&block[8..], true);
                let alpha = dxt5_alpha(block);
                for (px, a) in t.iter_mut().zip(alpha) {
                    px[3] = a;
                }
                t
            }
        };
        for (i, px) in texels.iter().enumerate() {
            let (x, y) = (bx + (i % 4) as u32, by + (i / 4) as u32);
            if x < w && y < h {
                let at = ((y * w + x) * 4) as usize;
                out[at..at + 4].copy_from_slice(px);
            }
        }
    }
}

fn half_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = i32::from((h >> 10) & 0x1F);
    let frac = f32::from(h & 0x3FF);
    sign * match exp {
        0 => frac * (2f32).powi(-24),
        31 => f32::INFINITY,
        e => (1.0 + frac / 1024.0) * (2f32).powi(e - 15),
    }
}

/// Decode the largest mip of frame 0.
pub fn decode(bytes: &[u8], limits: ImportLimits) -> Result<DecodedSurface, CodecError> {
    let h = header(bytes)?;
    let (w, ht) = (h.width, h.height);
    let sixteen = h.format == 25;
    check_decode(limits, w, ht, if sixteen { 8 } else { 4 }, 0)?;
    let (at, len) = locate(bytes, &h)?;
    let data = &bytes[at..at + len];
    let n = (w * ht) as usize;
    if sixteen {
        let px = data
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        let mut s = rgba8_surface(w, ht, Vec::new(), ImportFormat::Vtf);
        s.pixels = crate::codec::SurfacePixels::Rgba16(px);
        return Ok(s);
    }
    let mut out = vec![0u8; n * 4];
    match h.format {
        13 | 14 | 15 | 20 => decode_blocks(h.format, data, w, ht, &mut out),
        f => {
            let bpp = bytes_per_pixel(f).unwrap_or(1) as usize;
            if f == 7 {
                return Err(CodecError::Unsupported(format!(
                    "VTF {} is not supported",
                    format_name(f)
                )));
            }
            for (i, p) in data.chunks_exact(bpp).enumerate().take(n) {
                let le16 = || u16::from_le_bytes([p[0], p[1]]);
                let px: [u8; 4] = match f {
                    0 => [p[0], p[1], p[2], p[3]],
                    1 => [p[3], p[2], p[1], p[0]],
                    2 => [p[0], p[1], p[2], 255],
                    3 => [p[2], p[1], p[0], 255],
                    4 => {
                        let c = rgb565(le16());
                        [c[0], c[1], c[2], 255]
                    }
                    5 => [p[0], p[0], p[0], 255],
                    6 => [p[0], p[0], p[0], p[1]],
                    8 => [0, 0, 0, p[0]],
                    9 => {
                        let a = if p[..3] == [0, 0, 255] { 0 } else { 255 };
                        [p[0], p[1], p[2], a]
                    }
                    10 => {
                        let a = if p[..3] == [255, 0, 0] { 0 } else { 255 };
                        [p[2], p[1], p[0], a]
                    }
                    11 => [p[1], p[2], p[3], p[0]],
                    12 => [p[2], p[1], p[0], p[3]],
                    16 => [p[2], p[1], p[0], 255],
                    17 => {
                        let c = rgb565(le16());
                        [c[2], c[1], c[0], 255]
                    }
                    18 | 21 => {
                        let v = le16();
                        let a = if f == 21 && v & 0x8000 == 0 { 0 } else { 255 };
                        [
                            expand((v >> 10) & 0x1F, 5),
                            expand((v >> 5) & 0x1F, 5),
                            expand(v & 0x1F, 5),
                            a,
                        ]
                    }
                    19 => {
                        let v = le16();
                        [
                            expand((v >> 8) & 0xF, 4),
                            expand((v >> 4) & 0xF, 4),
                            expand(v & 0xF, 4),
                            expand(v >> 12, 4),
                        ]
                    }
                    22 => [p[0], p[1], 0, 255],
                    23 => [p[0], p[1], p[2], p[3]],
                    24 => {
                        let c = |k: usize| {
                            let v = half_to_f32(u16::from_le_bytes([p[k * 2], p[k * 2 + 1]]));
                            (v.clamp(0.0, 1.0) * 255.0).round() as u8
                        };
                        [c(0), c(1), c(2), c(3)]
                    }
                    _ => {
                        return Err(CodecError::Unsupported(format!(
                            "VTF {} is not supported",
                            format_name(f)
                        )))
                    }
                };
                out[i * 4..i * 4 + 4].copy_from_slice(&px);
            }
        }
    }
    Ok(rgba8_surface(w, ht, out, ImportFormat::Vtf))
}

#[cfg(test)]
mod tests {
    use super::super::test_util::fuzz;
    use super::*;
    use crate::codec::{decode_surface_bytes, probe_bytes, SurfacePixels};

    /// A VTF of `format` whose mips are `levels` (largest first), with a
    /// DXT1 4x4 thumbnail before them (versions < 7.3) or a resource
    /// directory (7.3+).
    fn vtf(minor: u32, w: u16, h: u16, format: u32, levels: &[Vec<u8>]) -> Vec<u8> {
        let header_size: u32 = if minor >= 3 { 96 } else { 80 };
        let mut b = Vec::new();
        b.extend_from_slice(b"VTF\0");
        b.extend_from_slice(&7u32.to_le_bytes());
        b.extend_from_slice(&minor.to_le_bytes());
        b.extend_from_slice(&header_size.to_le_bytes());
        b.extend_from_slice(&w.to_le_bytes());
        b.extend_from_slice(&h.to_le_bytes());
        b.extend_from_slice(&0u32.to_le_bytes()); // flags
        b.extend_from_slice(&1u16.to_le_bytes()); // frames
        b.extend_from_slice(&0u16.to_le_bytes()); // first frame
        b.extend_from_slice(&[0; 4]);
        b.extend_from_slice(&[0; 12]); // reflectivity
        b.extend_from_slice(&[0; 4]);
        b.extend_from_slice(&1f32.to_le_bytes());
        b.extend_from_slice(&format.to_le_bytes());
        b.push(levels.len() as u8);
        b.extend_from_slice(&13u32.to_le_bytes()); // thumbnail DXT1
        b.extend_from_slice(&[4, 4]);
        b.extend_from_slice(&1u16.to_le_bytes()); // depth
        b.extend_from_slice(&[0; 3]);
        let thumb = [0xAAu8; 8];
        if minor >= 3 {
            b.extend_from_slice(&2u32.to_le_bytes());
            b.extend_from_slice(&[0; 8]);
            let data_at = header_size + thumb.len() as u32;
            b.extend_from_slice(&[1, 0, 0, 0]);
            b.extend_from_slice(&header_size.to_le_bytes());
            b.extend_from_slice(&[0x30, 0, 0, 0]);
            b.extend_from_slice(&data_at.to_le_bytes());
        }
        b.resize(header_size as usize, 0);
        b.extend_from_slice(&thumb);
        for level in levels.iter().rev() {
            b.extend_from_slice(level);
        }
        b
    }

    #[test]
    fn uncompressed_formats_open_as_their_largest_mip() {
        // BGRA8888, 2x2, with its 1x1 mip first in the file.
        let top = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
        for minor in [1, 2, 3, 5] {
            let file = vtf(minor, 2, 2, 12, &[top.clone(), vec![99; 4]]);
            assert!(looks_like_vtf(&file));
            let s = decode_surface_bytes(&file, ImportLimits::default()).unwrap();
            assert_eq!(
                (s.width, s.height, s.source_format),
                (2, 2, ImportFormat::Vtf)
            );
            assert_eq!(
                s.pixels,
                SurfacePixels::Rgba8(vec![3, 2, 1, 4, 7, 6, 5, 8, 11, 10, 9, 12, 15, 14, 13, 16]),
                "7.{minor}"
            );
        }
        // RGB888 and RGB565 (pure red, pure green).
        let s = decode_surface_bytes(
            &vtf(2, 1, 1, 2, &[vec![10, 20, 30]]),
            ImportLimits::default(),
        )
        .unwrap();
        assert_eq!(s.pixels, SurfacePixels::Rgba8(vec![10, 20, 30, 255]));
        let s = decode_surface_bytes(
            &vtf(
                2,
                2,
                1,
                4,
                &[[0x00F8u16.swap_bytes(), 0x07E0]
                    .iter()
                    .flat_map(|v| v.to_le_bytes())
                    .collect()],
            ),
            ImportLimits::default(),
        )
        .unwrap();
        assert_eq!(
            s.pixels,
            SurfacePixels::Rgba8(vec![255, 0, 0, 255, 0, 255, 0, 255])
        );
        // RGBA16161616 stays 16-bit.
        let px: Vec<u8> = [1u16, 2, 3, 65535]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let file = vtf(2, 1, 1, 25, &[px]);
        let s = decode_surface_bytes(&file, ImportLimits::default()).unwrap();
        assert_eq!(s.pixels, SurfacePixels::Rgba16(vec![1, 2, 3, 65535]));
        assert!(matches!(
            probe_bytes(&file, ImportLimits::default())
                .unwrap()
                .pixel_format,
            crate::codec::PixelFormat::Rgba16
        ));
    }

    #[test]
    fn dxt1_and_dxt5_blocks_decode() {
        // DXT1: colour 0 red, colour 1 blue, indices 0,1,2,3 on the first row.
        let red = 0xF800u16;
        let blue = 0x001Fu16;
        let mut block = Vec::new();
        block.extend_from_slice(&red.to_le_bytes());
        block.extend_from_slice(&blue.to_le_bytes());
        block.extend_from_slice(&[0b1110_0100, 0, 0, 0]);
        let s = decode_surface_bytes(&vtf(2, 4, 4, 13, &[block.clone()]), ImportLimits::default())
            .unwrap();
        let SurfacePixels::Rgba8(px) = s.pixels else {
            panic!()
        };
        assert_eq!(&px[..4], &[255, 0, 0, 255]);
        assert_eq!(&px[4..8], &[0, 0, 255, 255]);
        assert_eq!(&px[8..12], &[170, 0, 85, 255]);
        assert_eq!(&px[12..16], &[85, 0, 170, 255]);
        assert_eq!(&px[16..20], &[255, 0, 0, 255]);
        // DXT5: alpha endpoints 255 / 0, first texel index 1 -> alpha 0.
        let mut b5 = vec![255, 0, 1, 0, 0, 0, 0, 0];
        b5.extend_from_slice(&block);
        let s = decode_surface_bytes(&vtf(2, 4, 4, 15, &[b5]), ImportLimits::default()).unwrap();
        let SurfacePixels::Rgba8(px) = s.pixels else {
            panic!()
        };
        assert_eq!(px[3], 0);
        assert_eq!(px[7], 255);
    }

    #[test]
    fn damaged_or_paletted_vtfs_error_and_never_panic() {
        let err = decode_surface_bytes(&vtf(2, 1, 1, 7, &[vec![0]]), ImportLimits::default())
            .unwrap_err();
        assert!(err.to_string().contains("P8"), "{err}");
        let file = vtf(
            3,
            8,
            8,
            15,
            &[vec![0x5A; 64], vec![0x33; 16], vec![1; 16], vec![2; 16]],
        );
        fuzz(&file, ImportFormat::Vtf);
        fuzz(
            &vtf(1, 4, 2, 0, &[vec![9; 32], vec![1; 8], vec![2; 4]]),
            ImportFormat::Vtf,
        );
        let tight = ImportLimits {
            max_width: 4,
            ..ImportLimits::default()
        };
        assert!(decode_surface_bytes(&file, tight).is_err());
    }
}
