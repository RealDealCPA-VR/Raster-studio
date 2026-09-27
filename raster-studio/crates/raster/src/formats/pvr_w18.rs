//! W18-H: a PowerVR texture container (`.pvr`, version 3) — Photopea
//! recognises it by its `PVR\x03` magic and opens the texture as a picture.
//!
//! The first mip level of the first surface, face and depth slice opens,
//! as straight-alpha RGBA8, from
//!
//! * **uncompressed** data described by the header's channel names and bit
//!   widths: byte-aligned channels (8, 16 or 32 bits each; unsigned
//!   integers, normalised or not, or 16- / 32-bit floats clamped to
//!   `0..=1`) in their stored order, and packed 8-, 16- and 32-bit pixels
//!   (RGB 565, RGBA 4444 / 5551, ...) whose first channel sits in the most
//!   significant bits; channels `r`, `g`, `b`, `a`, `l` (luminance), `i`
//!   (intensity) and `x` (padding);
//! * **PVRTC 4 bpp** (RGB and RGBA): the two block colours are upscaled
//!   bilinearly (wrapping, as the format does) and blended by each pixel's
//!   2-bit modulation, punch-through blocks included. The filtering is done
//!   in floating point, so a pixel can differ by a level or two from
//!   Imagination's integer reference decoder;
//! * **ETC1**;
//! * **BC1 / BC2 / BC3** (DXT1-DXT5; the premultiplied DXT2 / DXT4
//!   un-premultiplied).
//!
//! A premultiplied file (flag `0x02`) is un-premultiplied. The samples are
//! taken as stored whatever the header's colour space says (a linear-light
//! texture opens as its stored values, as Photopea shows it).
//!
//! # Refused by name
//!
//! PVRTC 2 bpp, PVRTC-II, ETC2 / EAC, ASTC, BC4-BC7 and the other
//! compressed formats; signed channel types; a big-endian (`\x03RVP`)
//! container.
//!
//! # Untrusted input
//!
//! The declared size goes through [`ImportLimits`] and the bytes it needs
//! are checked against what the file holds before the output is reserved;
//! every read is bounds-checked.

use super::{check_decode, malformed};
use crate::codec::{CodecError, ImportLimits};

const NAME: &str = "PVR";
const MAGIC: &[u8; 4] = b"PVR\x03";
const HEADER: usize = 52;

/// `true` when `head` opens with the version-3 magic.
pub fn looks_like_pvr(head: &[u8]) -> bool {
    head.starts_with(MAGIC)
}

/// One decoded texture.
#[derive(Debug, Clone, PartialEq)]
pub struct PvrImage {
    pub width: u32,
    pub height: u32,
    /// Row-major straight-alpha RGBA8.
    pub rgba: Vec<u8>,
    /// What the file holds that did not open (further mips, surfaces, faces
    /// or depth slices), one sentence each.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Format {
    /// Channel names and bit widths, first channel first.
    Plain {
        names: [u8; 4],
        bits: [u8; 4],
    },
    Pvrtc4,
    Etc1,
    Bc {
        version: u8,
        premultiplied: bool,
    },
}

fn u32_at(b: &[u8], at: usize) -> Result<u32, CodecError> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| malformed(NAME, "the header is cut short"))
}

fn compressed_name(id: u32) -> String {
    match id {
        0 | 1 => "PVRTC 2 bpp".into(),
        4 | 5 => "PVRTC-II".into(),
        12 | 13 => "BC4 / BC5".into(),
        14 | 15 => "BC6 / BC7".into(),
        16..=21 => "a YUV / packed-video format".into(),
        22..=26 => "ETC2".into(),
        27..=30 => "EAC".into(),
        _ => format!("compressed format {id}"),
    }
}

/// Decode the first image of a `.pvr`.
pub fn decode(bytes: &[u8], limits: ImportLimits) -> Result<PvrImage, CodecError> {
    if bytes.starts_with(b"\x03RVP") {
        return Err(CodecError::Unsupported(
            "a big-endian PVR container is not read by this build".into(),
        ));
    }
    if !looks_like_pvr(bytes) {
        return Err(malformed(NAME, "no PVR\\x03 signature"));
    }
    let flags = u32_at(bytes, 4)?;
    let (lo, hi) = (u32_at(bytes, 8)?, u32_at(bytes, 12)?);
    let channel_type = u32_at(bytes, 20)?;
    let height = u32_at(bytes, 24)?;
    let width = u32_at(bytes, 28)?;
    let depth = u32_at(bytes, 32)?;
    let surfaces = u32_at(bytes, 36)?;
    let faces = u32_at(bytes, 40)?;
    let mips = u32_at(bytes, 44)?;
    let meta = u32_at(bytes, 48)? as usize;
    if width == 0 || height == 0 {
        return Err(malformed(NAME, "the texture has no pixels"));
    }
    let premultiplied = flags & 2 != 0;
    let format = if hi == 0 {
        match lo {
            2 | 3 => Format::Pvrtc4,
            6 => Format::Etc1,
            7 => Format::Bc {
                version: 1,
                premultiplied: false,
            },
            8 | 9 => Format::Bc {
                version: 2,
                premultiplied: lo == 8,
            },
            10 | 11 => Format::Bc {
                version: 3,
                premultiplied: lo == 10,
            },
            other => {
                return Err(CodecError::Unsupported(format!(
                    "this PVR holds {}, which this build does not decode",
                    compressed_name(other)
                )))
            }
        }
    } else {
        Format::Plain {
            names: lo.to_le_bytes(),
            bits: hi.to_le_bytes(),
        }
    };
    check_decode(limits, width, height, 4, 0)?;
    let data = HEADER
        .checked_add(meta)
        .and_then(|at| bytes.get(at..))
        .ok_or_else(|| malformed(NAME, "the metadata runs past the file"))?;
    let (w, h) = (width as usize, height as usize);
    let mut rgba = match format {
        Format::Plain { names, bits } => plain(data, w, h, names, bits, channel_type)?,
        Format::Pvrtc4 => pvrtc4(data, w, h)?,
        Format::Etc1 => blocks(data, w, h, 8, etc1_block)?,
        Format::Bc { version, .. } => {
            let size = if version == 1 { 8 } else { 16 };
            blocks(data, w, h, size, |b| match version {
                1 => bc_colour(b, true),
                2 => bc2(b),
                _ => bc3(b),
            })?
        }
    };
    let bc_premultiplied = matches!(
        format,
        Format::Bc {
            premultiplied: true,
            ..
        }
    );
    if premultiplied || bc_premultiplied {
        unpremultiply(&mut rgba);
    }
    let mut notes = Vec::new();
    let extra = [
        (mips, "mip levels", "the largest"),
        (surfaces, "surfaces (array layers)", "the first"),
        (faces, "faces", "the first"),
        (depth, "depth slices", "the first"),
    ];
    for (count, what, which) in extra {
        if count > 1 {
            notes.push(format!("the texture holds {count} {what}; {which} opened"));
        }
    }
    Ok(PvrImage {
        width,
        height,
        rgba,
        notes,
    })
}

fn unpremultiply(rgba: &mut [u8]) {
    for px in rgba.as_chunks_mut::<4>().0 {
        let a = u32::from(px[3]);
        for c in &mut px[..3] {
            if let Some(v) = (u32::from(*c) * 255 + a / 2).checked_div(a) {
                *c = v.min(255) as u8;
            }
        }
    }
}

fn need(data: &[u8], bytes: u64) -> Result<(), CodecError> {
    if (data.len() as u64) < bytes {
        return Err(malformed(
            NAME,
            format!(
                "the image needs {bytes} bytes, the file holds {}",
                data.len()
            ),
        ));
    }
    Ok(())
}

fn half_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = i32::from((h >> 10) & 0x1f);
    let frac = f32::from(h & 0x3ff);
    match exp {
        0 => sign * frac * 2f32.powi(-24),
        31 => sign * f32::INFINITY,
        e => sign * (1.0 + frac / 1024.0) * 2f32.powi(e - 15),
    }
}

fn unit(v: f32) -> u8 {
    if v.is_nan() {
        0
    } else {
        (v.clamp(0.0, 1.0) * 255.0).round() as u8
    }
}

/// Scale an `n`-bit unsigned value to 8 bits.
fn widen(v: u32, n: u8) -> u8 {
    match n {
        0 => 0,
        8 => v as u8,
        n if n > 8 => (v >> (n - 8)) as u8,
        n => {
            let max = (1u32 << n) - 1;
            ((v * 255 + max / 2) / max) as u8
        }
    }
}

fn plain(
    data: &[u8],
    w: usize,
    h: usize,
    names: [u8; 4],
    bits: [u8; 4],
    channel_type: u32,
) -> Result<Vec<u8>, CodecError> {
    let channels: Vec<(u8, u8)> = names
        .iter()
        .zip(bits)
        .filter(|(n, b)| **n != 0 && *b != 0)
        .map(|(n, b)| (n.to_ascii_lowercase(), b))
        .collect();
    if channels.is_empty() {
        return Err(malformed(NAME, "the pixel format names no channels"));
    }
    for (name, _) in &channels {
        if !b"rgbalix".contains(name) {
            return Err(CodecError::Unsupported(format!(
                "this PVR holds a {:?} channel, which this build does not read",
                char::from(*name)
            )));
        }
    }
    let float = channel_type == 12;
    if matches!(channel_type, 1 | 3 | 5 | 7 | 9 | 11) {
        return Err(CodecError::Unsupported(
            "a PVR with signed channels is not read by this build".into(),
        ));
    }
    if channel_type > 12 {
        return Err(malformed(
            NAME,
            format!("unknown channel type {channel_type}"),
        ));
    }
    let total: u32 = channels.iter().map(|(_, b)| u32::from(*b)).sum();
    let aligned = channels.iter().all(|(_, b)| matches!(b, 8 | 16 | 32));
    if !total.is_multiple_of(8) || total > 128 || (!aligned && !matches!(total, 8 | 16 | 32)) {
        return Err(CodecError::Unsupported(format!(
            "a PVR pixel of {total} bits in this layout is not read by this build"
        )));
    }
    if float && !channels.iter().all(|(_, b)| matches!(b, 16 | 32)) {
        return Err(CodecError::Unsupported(
            "a floating-point PVR whose channels are not 16 or 32 bits is not read".into(),
        ));
    }
    let bpp = total as usize / 8;
    need(data, (w * h * bpp) as u64)?;
    let mut out = vec![0u8; w * h * 4];
    let mut values = [(0u8, 0u8); 4];
    for (i, px) in data[..w * h * bpp].chunks_exact(bpp).enumerate() {
        let mut n = 0;
        if aligned {
            let mut at = 0;
            for &(name, b) in &channels {
                let len = usize::from(b) / 8;
                let s = &px[at..at + len];
                let v = match (float, b) {
                    (true, 16) => unit(half_to_f32(u16::from_le_bytes([s[0], s[1]]))),
                    (true, _) => unit(f32::from_le_bytes([s[0], s[1], s[2], s[3]])),
                    (false, 8) => s[0],
                    (false, 16) => s[1],
                    (false, _) => s[3],
                };
                values[n] = (name, v);
                n += 1;
                at += len;
            }
        } else {
            let mut word = 0u32;
            for (k, byte) in px.iter().enumerate() {
                word |= u32::from(*byte) << (8 * k);
            }
            let mut shift = total;
            for &(name, b) in &channels {
                shift -= u32::from(b);
                let v = (word >> shift) & ((1u32 << b) - 1);
                values[n] = (name, widen(v, b));
                n += 1;
            }
        }
        let mut c = [0u8, 0, 0, 255];
        for &(name, v) in &values[..n] {
            match name {
                b'r' => c[0] = v,
                b'g' => c[1] = v,
                b'b' => c[2] = v,
                b'a' => c[3] = v,
                b'l' => c[..3].fill(v),
                b'i' => c = [v; 4],
                _ => {}
            }
        }
        out[i * 4..i * 4 + 4].copy_from_slice(&c);
    }
    Ok(out)
}

/// Decode 4x4 blocks of `size` bytes, row by row, into RGBA8.
fn blocks(
    data: &[u8],
    w: usize,
    h: usize,
    size: usize,
    decode: impl Fn(&[u8]) -> [[u8; 4]; 16],
) -> Result<Vec<u8>, CodecError> {
    let (bw, bh) = (w.div_ceil(4), h.div_ceil(4));
    need(data, (bw * bh * size) as u64)?;
    let mut out = vec![0u8; w * h * 4];
    for (i, block) in data[..bw * bh * size].chunks_exact(size).enumerate() {
        let texels = decode(block);
        let (bx, by) = ((i % bw) * 4, (i / bw) * 4);
        for (t, texel) in texels.iter().enumerate() {
            let (x, y) = (bx + t % 4, by + t / 4);
            if x < w && y < h {
                let o = (y * w + x) * 4;
                out[o..o + 4].copy_from_slice(texel);
            }
        }
    }
    Ok(out)
}

// ------------------------------------------------------------------- ETC1

const ETC1_MODIFIERS: [[i32; 4]; 8] = [
    [2, 8, -2, -8],
    [5, 17, -5, -17],
    [9, 29, -9, -29],
    [13, 42, -13, -42],
    [18, 60, -18, -60],
    [24, 80, -24, -80],
    [33, 106, -33, -106],
    [47, 183, -47, -183],
];

/// One ETC1 block (big-endian 64 bits), texels row-major.
fn etc1_block(b: &[u8]) -> [[u8; 4]; 16] {
    let flip = b[3] & 1 != 0;
    let diff = b[3] & 2 != 0;
    let tables = [usize::from(b[3] >> 5), usize::from((b[3] >> 2) & 7)];
    let mut base = [[0i32; 3]; 2];
    for c in 0..3 {
        if diff {
            let five = i32::from(b[c] >> 3);
            let delta = i32::from(b[c] & 7);
            let delta = if delta >= 4 { delta - 8 } else { delta };
            let second = (five + delta).clamp(0, 31);
            base[0][c] = (five << 3) | (five >> 2);
            base[1][c] = (second << 3) | (second >> 2);
        } else {
            base[0][c] = i32::from(b[c] >> 4) * 17;
            base[1][c] = i32::from(b[c] & 15) * 17;
        }
    }
    let msb = u32::from(u16::from_be_bytes([b[4], b[5]]));
    let lsb = u32::from(u16::from_be_bytes([b[6], b[7]]));
    let mut out = [[0u8; 4]; 16];
    for x in 0..4 {
        for y in 0..4 {
            let bit = x * 4 + y;
            let index = (((msb >> bit) & 1) << 1 | ((lsb >> bit) & 1)) as usize;
            let sub = usize::from(if flip { y >= 2 } else { x >= 2 });
            let m = ETC1_MODIFIERS[tables[sub]][index];
            let c = base[sub];
            out[y * 4 + x] = [
                (c[0] + m).clamp(0, 255) as u8,
                (c[1] + m).clamp(0, 255) as u8,
                (c[2] + m).clamp(0, 255) as u8,
                255,
            ];
        }
    }
    out
}

// ------------------------------------------------------------------- BC1-3

fn rgb565(c: u16) -> [u8; 3] {
    let r = u32::from(c >> 11);
    let g = u32::from((c >> 5) & 63);
    let b = u32::from(c & 31);
    [
        ((r * 255 + 15) / 31) as u8,
        ((g * 255 + 31) / 63) as u8,
        ((b * 255 + 15) / 31) as u8,
    ]
}

fn bc_colour(b: &[u8], allow_alpha: bool) -> [[u8; 4]; 16] {
    let (c0, c1) = (
        u16::from_le_bytes([b[0], b[1]]),
        u16::from_le_bytes([b[2], b[3]]),
    );
    let (a, z) = (rgb565(c0), rgb565(c1));
    let mix = |p: u32, q: u32, d: u32| -> [u8; 4] {
        let f = |i: usize| ((u32::from(a[i]) * p + u32::from(z[i]) * q) / d) as u8;
        [f(0), f(1), f(2), 255]
    };
    let four = c0 > c1 || !allow_alpha;
    let palette = if four {
        [mix(1, 0, 1), mix(0, 1, 1), mix(2, 1, 3), mix(1, 2, 3)]
    } else {
        [mix(1, 0, 1), mix(0, 1, 1), mix(1, 1, 2), [0, 0, 0, 0]]
    };
    let bits = u32::from_le_bytes([b[4], b[5], b[6], b[7]]);
    std::array::from_fn(|t| palette[((bits >> (2 * t)) & 3) as usize])
}

fn bc2(b: &[u8]) -> [[u8; 4]; 16] {
    let mut out = bc_colour(&b[8..16], false);
    for (t, texel) in out.iter_mut().enumerate() {
        let nibble = (b[t / 2] >> (4 * (t % 2))) & 15;
        texel[3] = nibble * 17;
    }
    out
}

fn bc3(b: &[u8]) -> [[u8; 4]; 16] {
    let mut out = bc_colour(&b[8..16], false);
    let (a0, a1) = (u32::from(b[0]), u32::from(b[1]));
    let palette: [u8; 8] = std::array::from_fn(|i| {
        let i = i as u32;
        match i {
            0 => a0 as u8,
            1 => a1 as u8,
            _ if a0 > a1 => ((a0 * (8 - i) + a1 * (i - 1)) / 7) as u8,
            6 => 0,
            7 => 255,
            _ => ((a0 * (6 - i) + a1 * (i - 1)) / 5) as u8,
        }
    });
    let mut bits = 0u64;
    for (k, byte) in b[2..8].iter().enumerate() {
        bits |= u64::from(*byte) << (8 * k);
    }
    for (t, texel) in out.iter_mut().enumerate() {
        texel[3] = palette[((bits >> (3 * t)) & 7) as usize];
    }
    out
}

// ------------------------------------------------------------------ PVRTC

/// The Morton index of block `(x, y)` in a `bw` x `bh` grid (both powers of
/// two), as the format lays blocks out.
fn twiddle(bw: usize, bh: usize, x: usize, y: usize) -> usize {
    let (min, mut rest) = if bh < bw { (bh, x) } else { (bw, y) };
    let (mut out, mut src, mut dst, mut shift) = (0usize, 1usize, 1usize, 0u32);
    while src < min {
        if y & src != 0 {
            out |= dst;
        }
        if x & src != 0 {
            out |= dst << 1;
        }
        src <<= 1;
        dst <<= 2;
        shift += 1;
    }
    rest >>= shift;
    out | (rest << (2 * shift))
}

/// Colour A (`high == false`) or B of a PVRTC colour word, as 8-bit RGBA.
fn pvrtc_colour(word: u32, high: bool) -> [f32; 4] {
    let x5 = |v: u32| ((v << 3) | (v >> 2)) as f32;
    let x4 = |v: u32| (v * 17) as f32;
    let x3 = |v: u32| ((v * 255 + 3) / 7) as f32;
    if high {
        let c = word >> 16;
        if c & 0x8000 != 0 {
            [x5((c >> 10) & 31), x5((c >> 5) & 31), x5(c & 31), 255.0]
        } else {
            [
                x4((c >> 8) & 15),
                x4((c >> 4) & 15),
                x4(c & 15),
                x3((c >> 12) & 7),
            ]
        }
    } else {
        let c = word & 0xffff;
        if c & 0x8000 != 0 {
            let b4 = (c >> 1) & 15;
            [
                x5((c >> 10) & 31),
                x5((c >> 5) & 31),
                x5((b4 << 1) | (b4 >> 3)),
                255.0,
            ]
        } else {
            let b3 = (c >> 1) & 7;
            [
                x4((c >> 8) & 15),
                x4((c >> 4) & 15),
                x3(b3),
                x3((c >> 12) & 7),
            ]
        }
    }
}

fn pvrtc4(data: &[u8], w: usize, h: usize) -> Result<Vec<u8>, CodecError> {
    let (bw, bh) = (w.div_ceil(4).max(2), h.div_ceil(4).max(2));
    if !bw.is_power_of_two() || !bh.is_power_of_two() {
        return Err(malformed(
            NAME,
            format!("a PVRTC texture must be a power of two on each side, not {w}x{h}"),
        ));
    }
    need(data, (bw * bh * 8) as u64)?;
    let word = |x: usize, y: usize| -> (u32, u32) {
        let at = twiddle(bw, bh, x % bw, y % bh) * 8;
        let s = &data[at..at + 8];
        (
            u32::from_le_bytes([s[0], s[1], s[2], s[3]]),
            u32::from_le_bytes([s[4], s[5], s[6], s[7]]),
        )
    };
    let mut out = vec![0u8; w * h * 4];
    for y in 0..h {
        for x in 0..w {
            // The colour images are sampled at block centres: the four
            // blocks around the pixel, weighted by its distance to each.
            let fx = (x as f32 - 1.5) / 4.0 + bw as f32;
            let fy = (y as f32 - 1.5) / 4.0 + bh as f32;
            let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
            let (tx, ty) = (fx - fx.floor(), fy - fy.floor());
            let mut ab = [[0f32; 4]; 2];
            for (dy, wy) in [(0, 1.0 - ty), (1, ty)] {
                for (dx, wx) in [(0, 1.0 - tx), (1, tx)] {
                    let (_, colours) = word(x0 + dx, y0 + dy);
                    for (slot, high) in [(0, false), (1, true)] {
                        let c = pvrtc_colour(colours, high);
                        for k in 0..4 {
                            ab[slot][k] += c[k] * wx * wy;
                        }
                    }
                }
            }
            let (modulation, colours) = word(x / 4, y / 4);
            let m = (modulation >> (2 * ((y % 4) * 4 + x % 4))) & 3;
            let punch = colours & 1 != 0;
            let (weight, clear) = match (punch, m) {
                (false, 0) | (true, 0) => (0.0, false),
                (false, 1) => (3.0 / 8.0, false),
                (false, 2) => (5.0 / 8.0, false),
                (true, 1) => (0.5, false),
                (true, 2) => (0.5, true),
                _ => (1.0, false),
            };
            let o = (y * w + x) * 4;
            for k in 0..4 {
                let v = ab[0][k] + (ab[1][k] - ab[0][k]) * weight;
                out[o + k] = v.round().clamp(0.0, 255.0) as u8;
            }
            if clear {
                out[o + 3] = 0;
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A version-3 header for a `w` x `h` texture of `format` (the 64-bit
    /// pixel format), then `data`.
    pub(crate) fn pvr(w: u32, h: u32, format: u64, channel_type: u32, data: &[u8]) -> Vec<u8> {
        let mut out = MAGIC.to_vec();
        // Flags.
        out.extend(0u32.to_le_bytes());
        out.extend(format.to_le_bytes());
        for v in [0u32, channel_type, h, w, 1, 1, 1, 1, 0] {
            out.extend(v.to_le_bytes());
        }
        out.extend_from_slice(data);
        out
    }

    fn plain_format(names: &[u8; 4], bits: [u8; 4]) -> u64 {
        u64::from(u32::from_le_bytes(*names)) | (u64::from(u32::from_le_bytes(bits)) << 32)
    }

    #[test]
    fn uncompressed_layouts_decode_to_their_colours() {
        // RGBA 8888, bytes in channel order.
        let px = [10u8, 20, 30, 40, 200, 100, 50, 255];
        let file = pvr(2, 1, plain_format(b"rgba", [8, 8, 8, 8]), 0, &px);
        let img = decode(&file, ImportLimits::default()).unwrap();
        assert_eq!((img.width, img.height), (2, 1));
        assert_eq!(img.rgba, px.to_vec());
        // RGB 565: red in the top five bits of a little-endian u16.
        let red: u16 = 0xF800;
        let blue: u16 = 0x001F;
        let data = [red.to_le_bytes(), blue.to_le_bytes()].concat();
        let file = pvr(2, 1, plain_format(b"rgb\0", [5, 6, 5, 0]), 0, &data);
        let img = decode(&file, ImportLimits::default()).unwrap();
        assert_eq!(img.rgba, vec![255, 0, 0, 255, 0, 0, 255, 255]);
        // RGBA 4444.
        let v: u16 = 0xF08C;
        let file = pvr(
            1,
            1,
            plain_format(b"rgba", [4, 4, 4, 4]),
            0,
            &v.to_le_bytes(),
        );
        let img = decode(&file, ImportLimits::default()).unwrap();
        assert_eq!(img.rgba, vec![255, 0, 136, 204]);
        // Luminance + alpha, and 32-bit floats clamped.
        let file = pvr(1, 1, plain_format(b"la\0\0", [8, 8, 0, 0]), 0, &[77, 128]);
        let img = decode(&file, ImportLimits::default()).unwrap();
        assert_eq!(img.rgba, vec![77, 77, 77, 128]);
        let floats: Vec<u8> = [0.5f32, 2.0, -1.0]
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        let file = pvr(1, 1, plain_format(b"rgb\0", [32, 32, 32, 0]), 12, &floats);
        let img = decode(&file, ImportLimits::default()).unwrap();
        assert_eq!(img.rgba, vec![128, 255, 0, 255]);
    }

    #[test]
    fn premultiplied_textures_are_unpremultiplied() {
        let mut file = pvr(
            1,
            1,
            plain_format(b"rgba", [8, 8, 8, 8]),
            0,
            &[64, 0, 0, 128],
        );
        file[4] = 2;
        let img = decode(&file, ImportLimits::default()).unwrap();
        assert_eq!(img.rgba, vec![128, 0, 0, 128]);
    }

    /// An ETC1 block in individual mode: both halves' base colours given as
    /// 4-bit values, codeword 0 (+-2 / +-8), every pixel on modifier index
    /// `index`.
    fn etc1_individual(left: [u8; 3], right: [u8; 3], flip: bool, index: u8) -> [u8; 8] {
        let mut b = [0u8; 8];
        for c in 0..3 {
            b[c] = (left[c] << 4) | right[c];
        }
        b[3] = u8::from(flip);
        let (msb, lsb) = (index >> 1, index & 1);
        let fill = |bit: u8| if bit == 1 { 0xFF } else { 0x00 };
        b[4] = fill(msb);
        b[5] = fill(msb);
        b[6] = fill(lsb);
        b[7] = fill(lsb);
        b
    }

    #[test]
    fn etc1_blocks_decode_both_halves_with_their_modifiers() {
        // Left half red (15, 0, 0) -> 255, right half blue; modifier +2.
        let block = etc1_individual([15, 0, 0], [0, 0, 8], false, 0);
        let file = pvr(4, 4, 6, 0, &block);
        let img = decode(&file, ImportLimits::default()).unwrap();
        let at = |x: usize, y: usize| &img.rgba[(y * 4 + x) * 4..(y * 4 + x) * 4 + 4];
        assert_eq!(at(0, 3), &[255, 2, 2, 255]);
        assert_eq!(at(3, 0), &[2, 2, 138, 255]);
        // Flipped: the top half is the first colour; modifier index 3 = -8.
        let block = etc1_individual([8, 8, 8], [4, 4, 4], true, 3);
        let img = decode(&pvr(4, 4, 6, 0, &block), ImportLimits::default()).unwrap();
        assert_eq!(&img.rgba[..4], &[128, 128, 128, 255]);
        assert_eq!(&img.rgba[(3 * 4) * 4..(3 * 4) * 4 + 4], &[60, 60, 60, 255]);
    }

    #[test]
    fn bc1_and_bc3_blocks_decode() {
        // BC1: colour 0 red, colour 1 blue, every texel index 1 (blue).
        let mut block = Vec::new();
        block.extend(0xF800u16.to_le_bytes());
        block.extend(0x001Fu16.to_le_bytes());
        block.extend(0x5555_5555u32.to_le_bytes());
        let img = decode(&pvr(4, 4, 7, 0, &block), ImportLimits::default()).unwrap();
        assert_eq!(&img.rgba[..4], &[0, 0, 255, 255]);
        // BC3: alpha endpoints 200 / 10, every alpha index 0 (200).
        let mut block = vec![200u8, 10, 0, 0, 0, 0, 0, 0];
        block.extend(0xF800u16.to_le_bytes());
        block.extend(0x001Fu16.to_le_bytes());
        block.extend(0u32.to_le_bytes());
        let img = decode(&pvr(4, 4, 11, 0, &block), ImportLimits::default()).unwrap();
        assert_eq!(&img.rgba[..4], &[255, 0, 0, 200]);
    }

    /// A PVRTC 4 bpp texture of `bw` x `bh` blocks, every block the same
    /// `(modulation, colour)` words.
    fn pvrtc(bw: usize, bh: usize, modulation: u32, colours: u32) -> Vec<u8> {
        let mut data = Vec::new();
        for _ in 0..bw * bh {
            data.extend(modulation.to_le_bytes());
            data.extend(colours.to_le_bytes());
        }
        pvr((bw * 4) as u32, (bh * 4) as u32, 3, 0, &data)
    }

    #[test]
    fn pvrtc_blends_its_two_colours_by_the_modulation() {
        // Colour A opaque red (RGB 554), colour B opaque blue (RGB 555).
        let a: u32 = 0x8000 | (31 << 10);
        let b: u32 = 0x8000 | 31;
        let colours = a | (b << 16);
        let img = decode(&pvrtc(2, 2, 0, colours), ImportLimits::default()).unwrap();
        assert!(img.rgba.chunks(4).all(|p| p == [255, 0, 0, 255]));
        let img = decode(&pvrtc(2, 2, u32::MAX, colours), ImportLimits::default()).unwrap();
        assert!(img.rgba.chunks(4).all(|p| p == [0, 0, 255, 255]));
        // Modulation 1 (3/8 of the way to B).
        let img = decode(&pvrtc(2, 2, 0x5555_5555, colours), ImportLimits::default()).unwrap();
        assert!(
            img.rgba.chunks(4).all(|p| p == [159, 0, 96, 255]),
            "{:?}",
            &img.rgba[..4]
        );
        // Punch-through blocks: modulation 2 is fully transparent.
        let img = decode(
            &pvrtc(2, 2, 0xAAAA_AAAA, colours | 1),
            ImportLimits::default(),
        )
        .unwrap();
        assert!(img.rgba.chunks(4).all(|p| p[3] == 0));
        // Non-power-of-two block grids are refused.
        let bad = pvrtc(3, 2, 0, colours);
        assert!(decode(&bad, ImportLimits::default()).is_err());
    }

    #[test]
    fn the_first_image_opens_and_the_rest_is_noted() {
        let mut file = pvr(
            1,
            1,
            plain_format(b"rgba", [8, 8, 8, 8]),
            0,
            &[1, 2, 3, 4, 9, 9, 9, 9],
        );
        file[44..48].copy_from_slice(&2u32.to_le_bytes());
        let img = decode(&file, ImportLimits::default()).unwrap();
        assert_eq!(img.rgba, vec![1, 2, 3, 4]);
        assert_eq!(img.notes.len(), 1, "{:?}", img.notes);
    }

    #[test]
    fn refusals_name_what_is_not_read() {
        let e = decode(&pvr(4, 4, 0, 0, &[0; 32]), ImportLimits::default()).unwrap_err();
        assert!(e.to_string().contains("PVRTC 2 bpp"), "{e}");
        let e = decode(&pvr(4, 4, 23, 0, &[0; 32]), ImportLimits::default()).unwrap_err();
        assert!(e.to_string().contains("ETC2"), "{e}");
        let e = decode(
            &pvr(1, 1, plain_format(b"rgba", [8, 8, 8, 8]), 1, &[0; 4]),
            ImportLimits::default(),
        )
        .unwrap_err();
        assert!(e.to_string().contains("signed"), "{e}");
        let mut be = pvr(1, 1, 7, 0, &[0; 8]);
        be[..4].copy_from_slice(b"\x03RVP");
        assert!(decode(&be, ImportLimits::default())
            .unwrap_err()
            .to_string()
            .contains("big-endian"));
    }

    #[test]
    fn malformed_files_error_and_never_panic() {
        let good = pvr(4, 4, plain_format(b"rgba", [8, 8, 8, 8]), 0, &[7; 64]);
        for cut in 0..good.len() {
            assert!(
                decode(&good[..cut], ImportLimits::default()).is_err(),
                "cut {cut}"
            );
        }
        // Huge dimensions, a huge metadata length, odd bit layouts.
        let mut huge = good.clone();
        huge[24..28].copy_from_slice(&u32::MAX.to_le_bytes());
        huge[28..32].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode(&huge, ImportLimits::default()).is_err());
        let mut meta = good.clone();
        meta[48..52].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode(&meta, ImportLimits::default()).is_err());
        for bits in [[3u8, 3, 3, 0], [8, 8, 8, 1], [64, 64, 0, 0], [0, 0, 0, 0]] {
            let f = pvr(2, 2, plain_format(b"rgba", bits), 0, &[1; 256]);
            let _ = decode(&f, ImportLimits::default());
        }
        let odd = pvr(2, 2, plain_format(b"rgzz", [8, 8, 8, 8]), 0, &[1; 16]);
        assert!(decode(&odd, ImportLimits::default()).is_err());
        // Every byte flipped in turn: an answer, never a panic.
        for i in 0..good.len() {
            let mut f = good.clone();
            f[i] ^= 0xA5;
            let _ = decode(&f, ImportLimits::default());
        }
        for id in 0..40u64 {
            let f = pvr(8, 8, id, 0, &[0x3C; 64]);
            let _ = decode(&f, ImportLimits::default());
        }
    }
}
