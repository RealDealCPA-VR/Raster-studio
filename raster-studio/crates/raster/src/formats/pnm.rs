//! Netpbm: PBM (`P1`/`P4`), PGM (`P2`/`P5`) and PPM (`P3`/`P6`).
//!
//! Read: all six, ASCII and binary, any `maxval` from 1 to 65 535. A `maxval`
//! above 255 decodes to RGBA16; below it, samples are rescaled to the full
//! 8-bit range. PBM's `1` is **black**, as the format defines.
//!
//! Write: the binary forms only (`P6`, `P5`, `P4`) at 8 bits, which is what
//! every reader accepts. None of them has alpha; the exporter flattens first
//! ([`crate::codec::ExportFormat::alpha_support`] says `None`). A PGM stores
//! Rec. 601 luma of the encoded RGB; a PBM thresholds that luma at half.
//!
//! # Untrusted input
//!
//! The header is parsed token by token with every number range-checked; the
//! declared size goes through [`ImportLimits`] before the output is reserved,
//! and a binary body is length-checked against the header *before* that too,
//! so a 20-byte file declaring 60 000 x 60 000 costs nothing.

use super::{check_decode, info, malformed, rgba8_surface};
use crate::codec::SurfacePixels;
use crate::codec::{CodecError, DecodedSurface, ImageInfo, ImportFormat, ImportLimits};

const NAME: &str = "PNM";

/// Which Netpbm image an export writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PnmKind {
    /// `P6`: 8-bit RGB.
    Ppm,
    /// `P5`: 8-bit greyscale.
    Pgm,
    /// `P4`: 1-bit black and white.
    Pbm,
}

/// `true` when `head` opens with a Netpbm magic (`P1`..`P6`) followed by
/// whitespace or a comment.
pub fn looks_like_pnm(head: &[u8]) -> bool {
    head.len() >= 3
        && head[0] == b'P'
        && (b'1'..=b'6').contains(&head[1])
        && (head[2].is_ascii_whitespace() || head[2] == b'#')
}

/// A parsed header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Header {
    /// The magic digit, 1..=6.
    kind: u8,
    width: u32,
    height: u32,
    /// 1 for a bitmap.
    maxval: u32,
    /// Where the pixel data starts.
    data: usize,
}

/// A cursor over the header and ASCII body: skips whitespace and `#`
/// comments, reads unsigned decimal numbers.
struct Tokens<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Tokens<'a> {
    fn skip_space(&mut self) {
        while let Some(&b) = self.bytes.get(self.pos) {
            if b.is_ascii_whitespace() {
                self.pos += 1;
            } else if b == b'#' {
                while let Some(&c) = self.bytes.get(self.pos) {
                    self.pos += 1;
                    if c == b'\n' || c == b'\r' {
                        break;
                    }
                }
            } else {
                break;
            }
        }
    }

    /// The next number, refusing one past `max`.
    fn number(&mut self, what: &str, max: u32) -> Result<u32, CodecError> {
        self.skip_space();
        let start = self.pos;
        let mut value: u64 = 0;
        while let Some(&b) = self.bytes.get(self.pos) {
            if !b.is_ascii_digit() {
                break;
            }
            value = value * 10 + u64::from(b - b'0');
            if value > u64::from(max) {
                return Err(malformed(NAME, format!("{what} is larger than {max}")));
            }
            self.pos += 1;
        }
        if self.pos == start {
            return Err(malformed(NAME, format!("expected a number for {what}")));
        }
        Ok(value as u32)
    }

    /// The next PBM ASCII bit: a lone `0` or `1`, whitespace optional.
    fn bit(&mut self) -> Result<bool, CodecError> {
        self.skip_space();
        match self.bytes.get(self.pos) {
            Some(b'0') => {
                self.pos += 1;
                Ok(false)
            }
            Some(b'1') => {
                self.pos += 1;
                Ok(true)
            }
            Some(_) => Err(malformed(NAME, "a PBM pixel must be 0 or 1")),
            None => Err(malformed(NAME, "the pixel data ends early")),
        }
    }
}

fn parse_header(bytes: &[u8]) -> Result<Header, CodecError> {
    if !looks_like_pnm(bytes) {
        return Err(malformed(NAME, "no P1..P6 magic"));
    }
    let kind = bytes[1] - b'0';
    let mut t = Tokens { bytes, pos: 2 };
    let width = t.number("the width", u32::MAX)?;
    let height = t.number("the height", u32::MAX)?;
    let maxval = if kind == 1 || kind == 4 {
        1
    } else {
        let m = t.number("maxval", 65_535)?;
        if m == 0 {
            return Err(malformed(NAME, "maxval is 0"));
        }
        m
    };
    // Exactly one whitespace byte separates the header from a binary body.
    match bytes.get(t.pos) {
        Some(b) if b.is_ascii_whitespace() => t.pos += 1,
        None if width == 0 || height == 0 => {}
        _ => return Err(malformed(NAME, "the header does not end in whitespace")),
    }
    Ok(Header {
        kind,
        width,
        height,
        maxval,
        data: t.pos,
    })
}

/// Header facts without decoding the body.
pub fn probe(bytes: &[u8], limits: ImportLimits) -> Result<ImageInfo, CodecError> {
    let h = parse_header(bytes)?;
    limits.check_dimensions(h.width, h.height)?;
    Ok(info(h.width, h.height, ImportFormat::Pnm, h.maxval > 255))
}

/// Decode a PBM, PGM or PPM, ASCII or binary.
pub fn decode(bytes: &[u8], limits: ImportLimits) -> Result<DecodedSurface, CodecError> {
    let h = parse_header(bytes)?;
    let sixteen = h.maxval > 255;
    check_decode(limits, h.width, h.height, if sixteen { 8 } else { 4 }, 0)?;
    let (w, ht) = (h.width as usize, h.height as usize);
    let pixels = w * ht;
    let channels = match h.kind {
        3 | 6 => 3,
        _ => 1,
    };
    let body = &bytes[h.data..];

    // Bodies are length-checked before anything is reserved: a binary body
    // exactly, an ASCII one by the floor of one character per sample.
    let needed = match h.kind {
        4 => w.div_ceil(8) as u64 * ht as u64,
        5 | 6 => pixels as u64 * channels as u64 * if sixteen { 2 } else { 1 },
        _ => pixels as u64 * channels as u64,
    };
    if (body.len() as u64) < needed {
        return Err(malformed(
            NAME,
            format!(
                "the pixel data holds {} of at least {needed} bytes",
                body.len()
            ),
        ));
    }

    let mut source = Samples {
        kind: h.kind,
        sixteen,
        maxval: h.maxval,
        width: w,
        tokens: Tokens {
            bytes: body,
            pos: 0,
        },
        index: 0,
    };
    let full: u32 = if sixteen { 65_535 } else { 255 };
    // `(s * full + maxval/2) / maxval`, in u64 so the product cannot overflow.
    let scale = |s: u32| -> u32 {
        ((u64::from(s) * u64::from(full) + u64::from(h.maxval) / 2) / u64::from(h.maxval)) as u32
    };
    let mut next_rgb = || -> Result<[u32; 3], CodecError> {
        if channels == 3 {
            Ok([
                scale(source.next()?),
                scale(source.next()?),
                scale(source.next()?),
            ])
        } else {
            let v = scale(source.next()?);
            Ok([v, v, v])
        }
    };

    if sixteen {
        let mut out = Vec::with_capacity(pixels * 4);
        for _ in 0..pixels {
            let [r, g, b] = next_rgb()?;
            out.extend_from_slice(&[r as u16, g as u16, b as u16, u16::MAX]);
        }
        Ok(DecodedSurface {
            width: h.width,
            height: h.height,
            pixels: SurfacePixels::Rgba16(out),
            color_space: color::ColorSpace::Srgb,
            icc_profile: None,
            source_format: ImportFormat::Pnm,
        })
    } else {
        let mut out = Vec::with_capacity(pixels * 4);
        for _ in 0..pixels {
            let [r, g, b] = next_rgb()?;
            out.extend_from_slice(&[r as u8, g as u8, b as u8, 255]);
        }
        Ok(rgba8_surface(h.width, h.height, out, ImportFormat::Pnm))
    }
}

/// The body's samples in file order, each in `0..=maxval` (a bitmap's white
/// is 1, black 0). Binary reads are in bounds because the body was
/// length-checked against the header first; they still go through `get`.
struct Samples<'a> {
    kind: u8,
    sixteen: bool,
    maxval: u32,
    width: usize,
    tokens: Tokens<'a>,
    /// Samples read so far.
    index: usize,
}

impl Samples<'_> {
    fn next(&mut self) -> Result<u32, CodecError> {
        let i = self.index;
        self.index += 1;
        let short = || malformed(NAME, "the pixel data ends early");
        let body = self.tokens.bytes;
        let v = match self.kind {
            1 => u32::from(!self.tokens.bit()?),
            2 | 3 => self.tokens.number("a sample", 65_535)?,
            4 => {
                let (x, y) = (i % self.width, i / self.width);
                let byte = *body
                    .get(y * self.width.div_ceil(8) + x / 8)
                    .ok_or_else(short)?;
                u32::from(byte & (0x80 >> (x % 8)) == 0)
            }
            _ if self.sixteen => {
                let hi = *body.get(2 * i).ok_or_else(short)?;
                let lo = *body.get(2 * i + 1).ok_or_else(short)?;
                u32::from(u16::from_be_bytes([hi, lo]))
            }
            _ => u32::from(*body.get(i).ok_or_else(short)?),
        };
        Ok(v.min(self.maxval))
    }
}

/// Rec. 601 luma of encoded RGB, rounded.
fn luma(px: &[u8]) -> u8 {
    ((299 * u32::from(px[0]) + 587 * u32::from(px[1]) + 114 * u32::from(px[2]) + 500) / 1000) as u8
}

/// Encode straight RGBA8 as a binary Netpbm file. Alpha is dropped (the
/// exporter has already flattened).
pub fn encode(kind: PnmKind, width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    match kind {
        PnmKind::Ppm => {
            let mut out = format!("P6\n{width} {height}\n255\n").into_bytes();
            out.reserve(w * h * 3);
            for px in rgba.as_chunks::<4>().0 {
                out.extend_from_slice(&px[..3]);
            }
            out
        }
        PnmKind::Pgm => {
            let mut out = format!("P5\n{width} {height}\n255\n").into_bytes();
            out.reserve(w * h);
            out.extend(rgba.as_chunks::<4>().0.iter().map(|px| luma(px)));
            out
        }
        PnmKind::Pbm => {
            let mut out = format!("P4\n{width} {height}\n").into_bytes();
            let row = w.div_ceil(8);
            out.reserve(row * h);
            for y in 0..h {
                let mut packed = vec![0u8; row];
                for x in 0..w {
                    let i = (y * w + x) * 4;
                    if luma(&rgba[i..i + 4]) < 128 {
                        packed[x / 8] |= 0x80 >> (x % 8);
                    }
                }
                out.extend_from_slice(&packed);
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba8(s: DecodedSurface) -> Vec<u8> {
        match s.pixels {
            SurfacePixels::Rgba8(v) => v,
            SurfacePixels::Rgba16(_) => panic!("expected 8-bit"),
        }
    }

    fn sample_image() -> (u32, u32, Vec<u8>) {
        let (w, h) = (5u32, 3u32);
        let mut v = Vec::new();
        for y in 0..h {
            for x in 0..w {
                v.extend_from_slice(&[(x * 50) as u8, (y * 100) as u8, 7, 255]);
            }
        }
        (w, h, v)
    }

    #[test]
    fn ppm_round_trips_exactly() {
        let (w, h, px) = sample_image();
        let file = encode(PnmKind::Ppm, w, h, &px);
        assert!(file.starts_with(b"P6\n5 3\n255\n"));
        let back = decode(&file, ImportLimits::default()).unwrap();
        assert_eq!((back.width, back.height), (w, h));
        assert_eq!(rgba8(back), px);
    }

    #[test]
    fn pgm_round_trips_grey_exactly() {
        let (w, h) = (4u32, 2u32);
        let px: Vec<u8> = (0..8u8)
            .flat_map(|i| [i * 30, i * 30, i * 30, 255])
            .collect();
        let back =
            rgba8(decode(&encode(PnmKind::Pgm, w, h, &px), ImportLimits::default()).unwrap());
        assert_eq!(back, px);
    }

    #[test]
    fn pbm_round_trips_black_and_white_and_one_is_black() {
        // Width 10: rows are padded to two bytes each.
        let (w, h) = (10u32, 2u32);
        let px: Vec<u8> = (0..20u32)
            .flat_map(|i| {
                if i % 3 == 0 {
                    [0, 0, 0, 255]
                } else {
                    [255, 255, 255, 255]
                }
            })
            .collect();
        let file = encode(PnmKind::Pbm, w, h, &px);
        assert_eq!(file.len(), b"P4\n10 2\n".len() + 4);
        // Pixel 0 is black, so the first bit of the body is set.
        assert_eq!(file[b"P4\n10 2\n".len()] & 0x80, 0x80);
        assert_eq!(rgba8(decode(&file, ImportLimits::default()).unwrap()), px);
    }

    #[test]
    fn ascii_forms_decode_with_comments() {
        let p1 = b"P1\n# a comment\n3 2\n1 0 1\n010\n";
        let v = rgba8(decode(p1, ImportLimits::default()).unwrap());
        let firsts: Vec<u8> = v.chunks(4).map(|p| p[0]).collect();
        assert_eq!(firsts, vec![0, 255, 0, 255, 0, 255]);

        let p2 = b"P2 2 1 # inline\n 10\n0 10\n";
        let v = rgba8(decode(p2, ImportLimits::default()).unwrap());
        assert_eq!(v, vec![0, 0, 0, 255, 255, 255, 255, 255]);

        let p3 = b"P3\n1 1\n255\n12 34 56\n";
        let v = rgba8(decode(p3, ImportLimits::default()).unwrap());
        assert_eq!(v, vec![12, 34, 56, 255]);
    }

    #[test]
    fn a_sixteen_bit_pgm_decodes_to_rgba16() {
        let mut file = b"P5\n2 1\n65535\n".to_vec();
        file.extend_from_slice(&[0x12, 0x34, 0xff, 0xff]);
        let s = decode(&file, ImportLimits::default()).unwrap();
        assert_eq!(
            probe(&file, ImportLimits::default()).unwrap().pixel_format,
            crate::format::PixelFormat::Rgba16
        );
        let SurfacePixels::Rgba16(v) = s.pixels else {
            panic!("expected 16-bit")
        };
        assert_eq!(
            v,
            vec![0x1234, 0x1234, 0x1234, 65535, 65535, 65535, 65535, 65535]
        );
    }

    #[test]
    fn a_small_maxval_is_rescaled_to_full_range() {
        let file = b"P2\n2 1\n3\n0 3\n";
        let v = rgba8(decode(file, ImportLimits::default()).unwrap());
        assert_eq!((v[0], v[4]), (0, 255));
    }

    #[test]
    fn malformed_files_error_and_never_panic() {
        let cases: [&[u8]; 9] = [
            b"P6\n",
            b"P6\n2 2\n255\n\x01\x02",       // body short
            b"P6\n0 2\n255\n",               // empty dimension
            b"P5\n2 2\n0\n\x00\x00\x00\x00", // maxval 0
            b"P5\n2 2\n70000\n",             // maxval too big
            b"P2\n2 1\n255\n1 x\n",          // not a number
            b"P1\n2 1\n1 7\n",               // not a bit
            b"P6\n99999999999 2\n255\n",     // overflowing width
            b"P6\n60000 60000\n255\n",       // over the pixel limit, tiny file
        ];
        for bytes in cases {
            assert!(decode(bytes, ImportLimits::default()).is_err(), "{bytes:?}");
        }
        // Every truncation of a good file is an error or a decode, never a panic.
        let (w, h, px) = sample_image();
        for kind in [PnmKind::Ppm, PnmKind::Pgm, PnmKind::Pbm] {
            let good = encode(kind, w, h, &px);
            for n in 0..good.len() {
                let _ = decode(&good[..n], ImportLimits::default());
            }
        }
    }
}
