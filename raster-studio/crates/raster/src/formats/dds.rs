//! DirectDraw Surface (`.dds`).
//!
//! Read: the top mip level of the first surface of
//!
//! * uncompressed RGB / RGBA / BGRA / luminance / alpha-only data at 8, 16,
//!   24 or 32 bits per pixel, described by the header's channel masks;
//! * block-compressed `DXT1` (BC1, with its one-bit alpha), `DXT2`/`DXT3`
//!   (BC2) and `DXT4`/`DXT5` (BC3) - the premultiplied `DXT2`/`DXT4` are
//!   un-premultiplied on the way in;
//! * the same through a `DX10` extension header (`DXGI_FORMAT` BC1-BC3,
//!   R8G8B8A8, B8G8R8A8, B8G8R8X8, UNORM and SRGB alike).
//!
//! A cube map or texture array opens as its first face; a volume as its first
//! slice. Any other `DXGI_FORMAT` (BC4-BC7, float) is refused by name.
//!
//! Write: uncompressed 32-bit BGRA ([`DdsEncoding::Uncompressed`]) or BC3 /
//! `DXT5` ([`DdsEncoding::Bc3`]), one mip level, no DX10 header - the forms
//! every DDS reader accepts. The BC3 encoder fits each 4x4 block's colour
//! endpoints along the block's principal axis and its alpha endpoints to the
//! block's own range.
//!
//! # Untrusted input
//!
//! The declared size goes through [`ImportLimits`], and the data the declared
//! size needs is checked against what the file holds, both before the output
//! is reserved. Nothing indexes a slice that has not been length-checked.

use super::{check_decode, info, malformed, rgba8_surface};
use crate::codec::{CodecError, DecodedSurface, ImageInfo, ImportFormat, ImportLimits};

const NAME: &str = "DDS";
const MAGIC: &[u8; 4] = b"DDS ";
/// Magic + the 124-byte header.
const HEADER_END: usize = 128;
/// The DX10 extension header that follows when the FourCC is `DX10`.
const DX10_LEN: usize = 20;

const DDPF_ALPHAPIXELS: u32 = 0x1;
const DDPF_ALPHA: u32 = 0x2;
const DDPF_FOURCC: u32 = 0x4;
const DDPF_RGB: u32 = 0x40;
const DDPF_LUMINANCE: u32 = 0x2_0000;

/// How a DDS export stores its pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DdsEncoding {
    /// 32-bit BGRA, lossless.
    Uncompressed,
    /// BC3 (`DXT5`): 4:1 lossy block compression with smooth alpha.
    Bc3,
}

/// `true` when `head` opens with the `DDS ` magic.
pub fn looks_like_dds(head: &[u8]) -> bool {
    head.len() >= 4 && &head[..4] == MAGIC
}

fn u32_at(bytes: &[u8], at: usize) -> Result<u32, CodecError> {
    bytes
        .get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| malformed(NAME, "the header is truncated"))
}

/// The storage a surface uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layout {
    /// Block-compressed: 1 = BC1, 2 = BC2, 3 = BC3; `premultiplied` for
    /// DXT2/DXT4.
    Block { bc: u8, premultiplied: bool },
    /// Uncompressed, described by masks.
    Masks {
        bits: u32,
        r: u32,
        g: u32,
        b: u32,
        a: u32,
        luminance: bool,
    },
}

#[derive(Debug, Clone, Copy)]
struct Header {
    width: u32,
    height: u32,
    layout: Layout,
    data: usize,
}

fn parse_header(bytes: &[u8]) -> Result<Header, CodecError> {
    if !looks_like_dds(bytes) {
        return Err(malformed(NAME, "no DDS magic"));
    }
    if bytes.len() < HEADER_END {
        return Err(malformed(NAME, "the header is truncated"));
    }
    if u32_at(bytes, 4)? != 124 {
        return Err(malformed(NAME, "the header size is not 124"));
    }
    let height = u32_at(bytes, 12)?;
    let width = u32_at(bytes, 16)?;
    let pf_flags = u32_at(bytes, 80)?;
    let fourcc = bytes
        .get(84..88)
        .ok_or_else(|| malformed(NAME, "the header is truncated"))?;
    let bits = u32_at(bytes, 88)?;
    let masks = [
        u32_at(bytes, 92)?,
        u32_at(bytes, 96)?,
        u32_at(bytes, 100)?,
        u32_at(bytes, 104)?,
    ];
    let mut data = HEADER_END;
    let layout = if pf_flags & DDPF_FOURCC != 0 {
        match fourcc {
            b"DXT1" => Layout::Block {
                bc: 1,
                premultiplied: false,
            },
            b"DXT2" | b"DXT3" => Layout::Block {
                bc: 2,
                premultiplied: fourcc == b"DXT2",
            },
            b"DXT4" | b"DXT5" => Layout::Block {
                bc: 3,
                premultiplied: fourcc == b"DXT4",
            },
            b"DX10" => {
                data += DX10_LEN;
                let dxgi = u32_at(bytes, HEADER_END)?;
                dx10_layout(dxgi)?
            }
            other => {
                return Err(CodecError::Unsupported(format!(
                    "DDS compression {:?} is not supported (BC1-BC3 and uncompressed are)",
                    String::from_utf8_lossy(other)
                )))
            }
        }
    } else if pf_flags & (DDPF_RGB | DDPF_LUMINANCE | DDPF_ALPHA) != 0 {
        if !matches!(bits, 8 | 16 | 24 | 32) {
            return Err(malformed(NAME, format!("{bits} bits per pixel")));
        }
        let alpha = pf_flags & (DDPF_ALPHAPIXELS | DDPF_ALPHA) != 0;
        Layout::Masks {
            bits,
            r: masks[0],
            g: masks[1],
            b: masks[2],
            a: if alpha { masks[3] } else { 0 },
            luminance: pf_flags & DDPF_LUMINANCE != 0,
        }
    } else {
        return Err(malformed(NAME, "the pixel format names no layout"));
    };
    Ok(Header {
        width,
        height,
        layout,
        data,
    })
}

fn dx10_layout(dxgi: u32) -> Result<Layout, CodecError> {
    let masks = |r, g, b, a| Layout::Masks {
        bits: 32,
        r,
        g,
        b,
        a,
        luminance: false,
    };
    Ok(match dxgi {
        70..=72 => Layout::Block {
            bc: 1,
            premultiplied: false,
        },
        73..=75 => Layout::Block {
            bc: 2,
            premultiplied: false,
        },
        76..=78 => Layout::Block {
            bc: 3,
            premultiplied: false,
        },
        27..=29 => masks(0xff, 0xff00, 0xff_0000, 0xff00_0000),
        87 | 90 | 91 => masks(0xff_0000, 0xff00, 0xff, 0xff00_0000),
        88 | 92 | 93 => masks(0xff_0000, 0xff00, 0xff, 0),
        other => {
            return Err(CodecError::Unsupported(format!(
                "DDS DXGI_FORMAT {other} is not supported (BC1-BC3 and 8-bit RGBA/BGRA are)"
            )))
        }
    })
}

/// Bytes the top surface needs.
fn surface_bytes(h: &Header) -> u64 {
    let (w, ht) = (u64::from(h.width), u64::from(h.height));
    match h.layout {
        Layout::Block { bc, .. } => {
            let block = if bc == 1 { 8 } else { 16 };
            w.div_ceil(4) * ht.div_ceil(4) * block
        }
        Layout::Masks { bits, .. } => (w * u64::from(bits)).div_ceil(8) * ht,
    }
}

/// Header facts without decoding.
pub fn probe(bytes: &[u8], limits: ImportLimits) -> Result<ImageInfo, CodecError> {
    let h = parse_header(bytes)?;
    limits.check_dimensions(h.width, h.height)?;
    Ok(info(h.width, h.height, ImportFormat::Dds, false))
}

/// Decode the top mip level of the first surface to straight RGBA8.
pub fn decode(bytes: &[u8], limits: ImportLimits) -> Result<DecodedSurface, CodecError> {
    let h = parse_header(bytes)?;
    check_decode(limits, h.width, h.height, 4, 0)?;
    let needed = surface_bytes(&h);
    let body = bytes.get(h.data..).unwrap_or_default();
    if (body.len() as u64) < needed {
        return Err(malformed(
            NAME,
            format!("the surface needs {needed} bytes, the file holds {}", body.len()),
        ));
    }
    let (w, ht) = (h.width as usize, h.height as usize);
    let mut out = vec![0u8; w * ht * 4];
    match h.layout {
        Layout::Block { bc, premultiplied } => {
            let block_bytes = if bc == 1 { 8 } else { 16 };
            let bw = w.div_ceil(4);
            for (i, block) in body[..needed as usize].chunks_exact(block_bytes).enumerate() {
                let texels = match bc {
                    1 => decode_bc1(block),
                    2 => decode_bc2(block),
                    _ => decode_bc3(block),
                };
                let (bx, by) = ((i % bw) * 4, (i / bw) * 4);
                for (t, texel) in texels.iter().enumerate() {
                    let (x, y) = (bx + t % 4, by + t / 4);
                    if x < w && y < ht {
                        let o = (y * w + x) * 4;
                        out[o..o + 4].copy_from_slice(texel);
                    }
                }
            }
            if premultiplied {
                for px in out.chunks_exact_mut(4) {
                    let a = u32::from(px[3]);
                    if a > 0 {
                        for c in &mut px[..3] {
                            *c = ((u32::from(*c) * 255 + a / 2) / a).min(255) as u8;
                        }
                    }
                }
            }
        }
        Layout::Masks {
            bits,
            r,
            g,
            b,
            a,
            luminance,
        } => {
            let bpp = bits as usize / 8;
            let row = w * bpp;
            for y in 0..ht {
                for x in 0..w {
                    let at = y * row + x * bpp;
                    let mut v = 0u32;
                    for (k, byte) in body[at..at + bpp].iter().enumerate() {
                        v |= u32::from(*byte) << (8 * k);
                    }
                    let o = (y * w + x) * 4;
                    let (rr, gg, bb) = if luminance {
                        let l = channel(v, r);
                        (l, l, l)
                    } else {
                        (channel(v, r), channel(v, g), channel(v, b))
                    };
                    let aa = if a == 0 { 255 } else { channel(v, a) };
                    out[o..o + 4].copy_from_slice(&[rr, gg, bb, aa]);
                }
            }
        }
    }
    Ok(rgba8_surface(h.width, h.height, out, ImportFormat::Dds))
}

/// One channel out of a packed pixel, scaled to 8 bits. An empty mask reads
/// as 0.
fn channel(v: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    let max = u64::from(mask >> shift);
    let raw = u64::from((v & mask) >> shift);
    ((raw * 255 + max / 2) / max) as u8
}

fn expand565(c: u16) -> [u8; 3] {
    let r = ((c >> 11) & 31) as u8;
    let g = ((c >> 5) & 63) as u8;
    let b = (c & 31) as u8;
    [(r << 3) | (r >> 2), (g << 2) | (g >> 4), (b << 3) | (b >> 2)]
}

/// The palette of a BC1-style colour block. `four` forces the four-colour
/// mode, which BC2 and BC3 always use.
fn color_palette(c0: u16, c1: u16, four: bool) -> [[u8; 4]; 4] {
    let a = expand565(c0);
    let b = expand565(c1);
    let mix = |wa: u16, wb: u16, d: u16| -> [u8; 4] {
        let m = |i: usize| ((u16::from(a[i]) * wa + u16::from(b[i]) * wb + d / 2) / d) as u8;
        [m(0), m(1), m(2), 255]
    };
    if four || c0 > c1 {
        [
            [a[0], a[1], a[2], 255],
            [b[0], b[1], b[2], 255],
            mix(2, 1, 3),
            mix(1, 2, 3),
        ]
    } else {
        [
            [a[0], a[1], a[2], 255],
            [b[0], b[1], b[2], 255],
            mix(1, 1, 2),
            [0, 0, 0, 0],
        ]
    }
}

fn decode_color_block(block: &[u8], four: bool) -> [[u8; 4]; 16] {
    let c0 = u16::from_le_bytes([block[0], block[1]]);
    let c1 = u16::from_le_bytes([block[2], block[3]]);
    let indices = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
    let palette = color_palette(c0, c1, four);
    let mut out = [[0u8; 4]; 16];
    for (i, texel) in out.iter_mut().enumerate() {
        *texel = palette[((indices >> (2 * i)) & 3) as usize];
    }
    out
}

fn decode_bc1(block: &[u8]) -> [[u8; 4]; 16] {
    decode_color_block(block, false)
}

fn decode_bc2(block: &[u8]) -> [[u8; 4]; 16] {
    let mut out = decode_color_block(&block[8..16], true);
    for (i, texel) in out.iter_mut().enumerate() {
        let nibble = (block[i / 2] >> (4 * (i % 2))) & 0xf;
        texel[3] = nibble * 17;
    }
    out
}

fn alpha_palette(a0: u8, a1: u8) -> [u8; 8] {
    let (x, y) = (u16::from(a0), u16::from(a1));
    let mut p = [a0, a1, 0, 0, 0, 0, 0, 0];
    if a0 > a1 {
        for (i, v) in p.iter_mut().enumerate().skip(2) {
            let i = i as u16;
            *v = (((8 - i) * x + (i - 1) * y + 3) / 7) as u8;
        }
    } else {
        for (i, v) in p.iter_mut().enumerate().take(6).skip(2) {
            let i = i as u16;
            *v = (((6 - i) * x + (i - 1) * y + 2) / 5) as u8;
        }
        p[6] = 0;
        p[7] = 255;
    }
    p
}

fn decode_bc3(block: &[u8]) -> [[u8; 4]; 16] {
    let mut out = decode_color_block(&block[8..16], true);
    let palette = alpha_palette(block[0], block[1]);
    let mut bits = 0u64;
    for (k, b) in block[2..8].iter().enumerate() {
        bits |= u64::from(*b) << (8 * k);
    }
    for (i, texel) in out.iter_mut().enumerate() {
        texel[3] = palette[((bits >> (3 * i)) & 7) as usize];
    }
    out
}

/// Encode straight RGBA8 as a DDS file.
pub fn encode(encoding: DdsEncoding, width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let mut out = Vec::with_capacity(HEADER_END + w * h * 4);
    let put = |out: &mut Vec<u8>, v: u32| out.extend_from_slice(&v.to_le_bytes());
    out.extend_from_slice(MAGIC);
    put(&mut out, 124);
    let (flags, pitch) = match encoding {
        // CAPS | HEIGHT | WIDTH | PITCH | PIXELFORMAT
        DdsEncoding::Uncompressed => (0x100F, width * 4),
        // CAPS | HEIGHT | WIDTH | PIXELFORMAT | LINEARSIZE
        DdsEncoding::Bc3 => (0x8_1007, width.div_ceil(4) * height.div_ceil(4) * 16),
    };
    put(&mut out, flags);
    put(&mut out, height);
    put(&mut out, width);
    put(&mut out, pitch);
    put(&mut out, 0); // depth
    put(&mut out, 0); // mip count
    out.extend_from_slice(&[0u8; 44]); // reserved
    put(&mut out, 32); // pixel format size
    match encoding {
        DdsEncoding::Uncompressed => {
            put(&mut out, DDPF_RGB | DDPF_ALPHAPIXELS);
            put(&mut out, 0);
            put(&mut out, 32);
            put(&mut out, 0x00ff_0000);
            put(&mut out, 0x0000_ff00);
            put(&mut out, 0x0000_00ff);
            put(&mut out, 0xff00_0000);
        }
        DdsEncoding::Bc3 => {
            put(&mut out, DDPF_FOURCC);
            out.extend_from_slice(b"DXT5");
            for _ in 0..5 {
                put(&mut out, 0);
            }
        }
    }
    put(&mut out, 0x1000); // DDSCAPS_TEXTURE
    for _ in 0..4 {
        put(&mut out, 0);
    }
    debug_assert_eq!(out.len(), HEADER_END);
    match encoding {
        DdsEncoding::Uncompressed => {
            for px in rgba.chunks_exact(4) {
                out.extend_from_slice(&[px[2], px[1], px[0], px[3]]);
            }
        }
        DdsEncoding::Bc3 => {
            for by in (0..h).step_by(4) {
                for bx in (0..w).step_by(4) {
                    let mut block = [[0u8; 4]; 16];
                    for (t, texel) in block.iter_mut().enumerate() {
                        // Edge blocks repeat the last row / column.
                        let x = (bx + t % 4).min(w - 1);
                        let y = (by + t / 4).min(h - 1);
                        let i = (y * w + x) * 4;
                        texel.copy_from_slice(&rgba[i..i + 4]);
                    }
                    out.extend_from_slice(&encode_bc3_block(&block));
                }
            }
        }
    }
    out
}

fn to565(c: [f32; 3]) -> u16 {
    let q = |v: f32, max: f32| (v.clamp(0.0, 255.0) / 255.0 * max).round() as u16;
    (q(c[0], 31.0) << 11) | (q(c[1], 63.0) << 5) | q(c[2], 31.0)
}

/// One BC3 block: alpha endpoints at the block's range, colour endpoints at
/// the extremes of the block's projection onto its principal axis.
fn encode_bc3_block(texels: &[[u8; 4]; 16]) -> [u8; 16] {
    let mut out = [0u8; 16];

    // Alpha: eight-level mode spanning [min, max].
    let amax = texels.iter().map(|t| t[3]).max().unwrap_or(255);
    let amin = texels.iter().map(|t| t[3]).min().unwrap_or(255);
    out[0] = amax;
    out[1] = amin;
    let palette = alpha_palette(amax, amin);
    let mut bits = 0u64;
    for (i, t) in texels.iter().enumerate() {
        let best = (0..8u64)
            .min_by_key(|&k| (i32::from(palette[k as usize]) - i32::from(t[3])).abs())
            .unwrap_or(0);
        bits |= best << (3 * i);
    }
    out[2..8].copy_from_slice(&bits.to_le_bytes()[..6]);

    // Colour: principal axis by power iteration on the covariance.
    let px: Vec<[f32; 3]> = texels
        .iter()
        .map(|t| [f32::from(t[0]), f32::from(t[1]), f32::from(t[2])])
        .collect();
    let mut mean = [0f32; 3];
    for p in &px {
        for c in 0..3 {
            mean[c] += p[c] / 16.0;
        }
    }
    let mut cov = [[0f32; 3]; 3];
    for p in &px {
        let d = [p[0] - mean[0], p[1] - mean[1], p[2] - mean[2]];
        for (i, row) in cov.iter_mut().enumerate() {
            for (j, v) in row.iter_mut().enumerate() {
                *v += d[i] * d[j];
            }
        }
    }
    let mut axis = [1f32, 1.0, 1.0];
    for _ in 0..8 {
        let next = [
            cov[0][0] * axis[0] + cov[0][1] * axis[1] + cov[0][2] * axis[2],
            cov[1][0] * axis[0] + cov[1][1] * axis[1] + cov[1][2] * axis[2],
            cov[2][0] * axis[0] + cov[2][1] * axis[1] + cov[2][2] * axis[2],
        ];
        let len = (next[0] * next[0] + next[1] * next[1] + next[2] * next[2]).sqrt();
        if len < 1e-6 {
            break;
        }
        axis = [next[0] / len, next[1] / len, next[2] / len];
    }
    let project = |p: &[f32; 3]| {
        (p[0] - mean[0]) * axis[0] + (p[1] - mean[1]) * axis[1] + (p[2] - mean[2]) * axis[2]
    };
    let (mut lo, mut hi) = (f32::MAX, f32::MIN);
    for p in &px {
        let t = project(p);
        lo = lo.min(t);
        hi = hi.max(t);
    }
    let at = |t: f32| {
        [
            mean[0] + axis[0] * t,
            mean[1] + axis[1] * t,
            mean[2] + axis[2] * t,
        ]
    };
    let mut c0 = to565(at(hi));
    let mut c1 = to565(at(lo));
    if c0 < c1 {
        std::mem::swap(&mut c0, &mut c1);
    }
    let palette = color_palette(c0, c1, true);
    let mut indices = 0u32;
    for (i, t) in texels.iter().enumerate() {
        let best = (0..4u32)
            .min_by_key(|&k| {
                let p = palette[k as usize];
                (0..3)
                    .map(|c| (i32::from(p[c]) - i32::from(t[c])).pow(2))
                    .sum::<i32>()
            })
            .unwrap_or(0);
        indices |= best << (2 * i);
    }
    out[8..10].copy_from_slice(&c0.to_le_bytes());
    out[10..12].copy_from_slice(&c1.to_le_bytes());
    out[12..16].copy_from_slice(&indices.to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::SurfacePixels;

    fn rgba8(s: DecodedSurface) -> Vec<u8> {
        match s.pixels {
            SurfacePixels::Rgba8(v) => v,
            SurfacePixels::Rgba16(_) => panic!("expected 8-bit"),
        }
    }

    /// 7x5 (not a multiple of four) with a colour ramp and an alpha ramp.
    fn image() -> (u32, u32, Vec<u8>) {
        let (w, h) = (7u32, 5u32);
        let mut v = Vec::new();
        for y in 0..h {
            for x in 0..w {
                v.extend_from_slice(&[(x * 36) as u8, (y * 60) as u8, 200, (x * 40 + 10) as u8]);
            }
        }
        (w, h, v)
    }

    #[test]
    fn uncompressed_round_trips_exactly() {
        let (w, h, px) = image();
        let file = encode(DdsEncoding::Uncompressed, w, h, &px);
        assert_eq!(file.len(), HEADER_END + px.len());
        let back = decode(&file, ImportLimits::default()).unwrap();
        assert_eq!((back.width, back.height), (w, h));
        assert_eq!(rgba8(back), px);
    }

    #[test]
    fn bc3_round_trips_within_block_compression_error() {
        let (w, h, px) = image();
        let file = encode(DdsEncoding::Bc3, w, h, &px);
        // Two by two blocks of 16 bytes.
        assert_eq!(file.len(), HEADER_END + 4 * 16);
        assert_eq!(&file[84..88], b"DXT5");
        let back = rgba8(decode(&file, ImportLimits::default()).unwrap());
        let worst = px
            .iter()
            .zip(&back)
            .map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs())
            .max()
            .unwrap();
        assert!(worst <= 24, "BC3 error {worst} is larger than block compression explains");
        // A flat block survives exactly (up to 565 quantisation of the colour).
        let flat = [[40u8, 80, 120, 128]; 16];
        let block = encode_bc3_block(&flat);
        for t in decode_bc3(&block) {
            assert_eq!(t[3], 128);
            assert!((i32::from(t[0]) - 40).abs() <= 4 && (i32::from(t[1]) - 80).abs() <= 2);
        }
    }

    #[test]
    fn bc1_and_bc2_fixtures_decode() {
        // BC1 in three-colour mode (c0 <= c1): index 3 is transparent black.
        let red: u16 = 31 << 11;
        let blue: u16 = 31;
        let mut block = Vec::new();
        block.extend_from_slice(&blue.to_le_bytes()); // c0 = blue (smaller)
        block.extend_from_slice(&red.to_le_bytes()); // c1 = red
        // texel 0 -> index 0, texel 1 -> 1, texel 2 -> 2 (mid), texel 3 -> 3.
        block.extend_from_slice(&(0b11_10_01_00u32).to_le_bytes());
        let t = decode_bc1(&block);
        assert_eq!(t[0], [0, 0, 255, 255]);
        assert_eq!(t[1], [255, 0, 0, 255]);
        assert_eq!(t[2], [128, 0, 128, 255]);
        assert_eq!(t[3], [0, 0, 0, 0]);

        // BC2: explicit 4-bit alpha, colour always four-colour.
        let mut bc2 = vec![0u8; 16];
        bc2[0] = 0xf0; // texel 0 alpha 0, texel 1 alpha 15
        bc2[8..10].copy_from_slice(&red.to_le_bytes());
        bc2[10..12].copy_from_slice(&blue.to_le_bytes());
        let t = decode_bc2(&bc2);
        assert_eq!((t[0][3], t[1][3]), (0, 255));
        assert_eq!(t[0][..3], [255, 0, 0]);

        // Through a whole file with the DXT1 FourCC and a 16-bit 565 file.
        let mut file = encode(DdsEncoding::Bc3, 4, 4, &[0u8; 64]);
        file[84..88].copy_from_slice(b"DXT1");
        file.truncate(HEADER_END);
        file.extend_from_slice(&block);
        let px = rgba8(decode(&file, ImportLimits::default()).unwrap());
        assert_eq!(&px[..4], &[0, 0, 255, 255]);
    }

    #[test]
    fn uncompressed_masks_luminance_and_dx10_decode() {
        // 16-bit RGB565 through the masks path.
        let mut file = encode(DdsEncoding::Uncompressed, 1, 1, &[0; 4]);
        file.truncate(HEADER_END);
        file[80..84].copy_from_slice(&DDPF_RGB.to_le_bytes());
        file[88..92].copy_from_slice(&16u32.to_le_bytes());
        file[92..96].copy_from_slice(&0xf800u32.to_le_bytes());
        file[96..100].copy_from_slice(&0x07e0u32.to_le_bytes());
        file[100..104].copy_from_slice(&0x001fu32.to_le_bytes());
        file[104..108].copy_from_slice(&0u32.to_le_bytes());
        file.extend_from_slice(&0xf800u16.to_le_bytes());
        assert_eq!(rgba8(decode(&file, ImportLimits::default()).unwrap()), [255, 0, 0, 255]);

        // 8-bit luminance.
        let mut lum = file[..HEADER_END].to_vec();
        lum[80..84].copy_from_slice(&DDPF_LUMINANCE.to_le_bytes());
        lum[88..92].copy_from_slice(&8u32.to_le_bytes());
        lum[92..96].copy_from_slice(&0xffu32.to_le_bytes());
        lum.push(77);
        assert_eq!(rgba8(decode(&lum, ImportLimits::default()).unwrap()), [77, 77, 77, 255]);

        // DX10 R8G8B8A8_UNORM.
        let mut dx10 = file[..HEADER_END].to_vec();
        dx10[80..84].copy_from_slice(&DDPF_FOURCC.to_le_bytes());
        dx10[84..88].copy_from_slice(b"DX10");
        dx10.extend_from_slice(&28u32.to_le_bytes());
        dx10.extend_from_slice(&[0u8; 16]);
        dx10.extend_from_slice(&[1, 2, 3, 4]);
        assert_eq!(rgba8(decode(&dx10, ImportLimits::default()).unwrap()), [1, 2, 3, 4]);

        // An unsupported DXGI format is refused by name.
        let mut bc7 = dx10.clone();
        bc7[HEADER_END..HEADER_END + 4].copy_from_slice(&98u32.to_le_bytes());
        let err = decode(&bc7, ImportLimits::default()).unwrap_err();
        assert!(err.to_string().contains("DXGI_FORMAT 98"), "{err}");
    }

    #[test]
    fn premultiplied_dxt4_is_unpremultiplied() {
        let mut block = [0u8; 16];
        block[0] = 128; // a0
        block[1] = 128; // a1 -> every texel 128
        let grey: u16 = (15 << 11) | (31 << 5) | 15; // about half-intensity grey
        block[8..10].copy_from_slice(&grey.to_le_bytes());
        block[10..12].copy_from_slice(&grey.to_le_bytes());
        let mut file = encode(DdsEncoding::Bc3, 4, 4, &[0u8; 64]);
        file[84..88].copy_from_slice(b"DXT4");
        file.truncate(HEADER_END);
        file.extend_from_slice(&block);
        let px = rgba8(decode(&file, ImportLimits::default()).unwrap());
        // 123 premultiplied by 128/255 comes back near 245.
        assert!(px[0] > 230, "{:?}", &px[..4]);
        assert_eq!(px[3], 128);
    }

    #[test]
    fn malformed_files_error_and_never_panic() {
        let (w, h, px) = image();
        for encoding in [DdsEncoding::Uncompressed, DdsEncoding::Bc3] {
            let good = encode(encoding, w, h, &px);
            for n in 0..good.len() {
                assert!(
                    n == good.len() || decode(&good[..n], ImportLimits::default()).is_err(),
                    "{n}"
                );
            }
            // A huge declared size in a tiny file is refused before allocating.
            let mut huge = good.clone();
            huge[12..16].copy_from_slice(&60_000u32.to_le_bytes());
            huge[16..20].copy_from_slice(&60_000u32.to_le_bytes());
            assert!(decode(&huge, ImportLimits::default()).is_err());
            // Flipping every header byte never panics.
            for i in 4..HEADER_END {
                let mut bad = good.clone();
                bad[i] ^= 0xff;
                let _ = decode(&bad, ImportLimits::default());
            }
        }
        assert!(decode(b"DDS ", ImportLimits::default()).is_err());
    }
}
