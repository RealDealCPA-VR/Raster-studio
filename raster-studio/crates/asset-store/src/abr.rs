//! W9-E: Photoshop brush files (`.abr`), version 6 and later.
//!
//! A v6+ file is a 4-byte header (`u16` version 6/7/10, `u16` subversion
//! 1/2) followed by `8BIM` sections. Only the `samp` section is read: it is a
//! run of sampled brushes, each a big-endian `u32` length (padded to a
//! multiple of four) and then an identifier block, the tip's bounds, its bit
//! depth, a compression flag, and the pixels — raw or PackBits-compressed per
//! scanline. The `desc` section (the brushes' dynamics and names, an Action
//! descriptor) is skipped: brushes are named after the file.
//!
//! **The file is untrusted input.** Every read goes through a cursor that
//! refuses to run past its slice, every length is checked against what is
//! actually left before anything is allocated, and the counts are capped
//! ([`MAX_ABR_BYTES`], [`MAX_BRUSHES`], [`MAX_SIDE`], and the decoded total
//! [`MAX_TOTAL_PIXELS`], taken before each tip is decoded). A malformed file is an
//! [`AbrError`], never a panic, and never an allocation the file's own bytes
//! chose without bound.

/// The largest `.abr` this reader will look at.
pub const MAX_ABR_BYTES: usize = 64 * 1024 * 1024;
/// The most brushes one file may yield.
pub const MAX_BRUSHES: usize = 1000;
/// The longest side one tip may have (Photoshop's limit).
pub const MAX_SIDE: u32 = 5000;
/// The most tip pixels one file may decode to, summed over its brushes.
/// PackBits turns two bytes into 128, so the file-size cap alone would let
/// a 64 MB file decode to gigabytes; this caps the output instead.
pub const MAX_TOTAL_PIXELS: usize = 256 * 1024 * 1024;

/// One sampled brush tip read from a file.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct AbrBrush {
    pub width: u32,
    pub height: u32,
    /// `width * height` coverage bytes, `255` = full paint.
    pub alpha8: Vec<u8>,
}

/// Why a file could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AbrError {
    #[error("the file is larger than the {MAX_ABR_BYTES}-byte limit")]
    TooLarge,
    #[error("the file ends in the middle of {0}")]
    Truncated(&'static str),
    #[error("brush file version {0} is not supported (only version 6 and later)")]
    UnsupportedVersion(u16),
    #[error("the file has no sampled brushes")]
    NoBrushes,
    #[error("a brush is malformed: {0}")]
    Malformed(&'static str),
    #[error("the file holds more than {MAX_BRUSHES} brushes")]
    TooManyBrushes,
    #[error("the brushes decode to more than {MAX_TOTAL_PIXELS} pixels")]
    TooManyPixels,
}

/// A bounds-checked big-endian reader over a slice.
struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.at
    }

    fn take(&mut self, n: usize, what: &'static str) -> Result<&'a [u8], AbrError> {
        if n > self.remaining() {
            return Err(AbrError::Truncated(what));
        }
        let out = &self.bytes[self.at..self.at + n];
        self.at += n;
        Ok(out)
    }

    fn u8(&mut self, what: &'static str) -> Result<u8, AbrError> {
        Ok(self.take(1, what)?[0])
    }

    fn u16(&mut self, what: &'static str) -> Result<u16, AbrError> {
        let b = self.take(2, what)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    fn u32(&mut self, what: &'static str) -> Result<u32, AbrError> {
        let b = self.take(4, what)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn i32(&mut self, what: &'static str) -> Result<i32, AbrError> {
        Ok(self.u32(what)? as i32)
    }
}

/// Read every sampled brush in a v6+ `.abr` file.
pub fn parse_abr(bytes: &[u8]) -> Result<Vec<AbrBrush>, AbrError> {
    if bytes.len() > MAX_ABR_BYTES {
        return Err(AbrError::TooLarge);
    }
    let mut c = Cursor::new(bytes);
    let version = c.u16("the header")?;
    if version < 6 {
        return Err(AbrError::UnsupportedVersion(version));
    }
    let subversion = c.u16("the header")?;
    if subversion != 1 && subversion != 2 {
        return Err(AbrError::Malformed("unknown subversion"));
    }
    let mut brushes = Vec::new();
    let mut budget = MAX_TOTAL_PIXELS;
    while c.remaining() >= 12 {
        let signature = c.take(4, "a section header")?;
        if signature != b"8BIM" && signature != b"8B64" {
            return Err(AbrError::Malformed("a section lacks its 8BIM signature"));
        }
        let key = c.take(4, "a section header")?;
        let len = c.u32("a section header")? as usize;
        let body = c.take(len, "a section")?;
        if key == b"samp" {
            read_samples(body, subversion, &mut budget, &mut brushes)?;
        }
        // Sections are padded to an even length.
        if len % 2 == 1 && c.remaining() > 0 {
            c.take(1, "section padding")?;
        }
    }
    if brushes.is_empty() {
        return Err(AbrError::NoBrushes);
    }
    Ok(brushes)
}

fn read_samples(
    section: &[u8],
    subversion: u16,
    budget: &mut usize,
    out: &mut Vec<AbrBrush>,
) -> Result<(), AbrError> {
    let mut c = Cursor::new(section);
    while c.remaining() >= 4 {
        let len = c.u32("a brush length")? as usize;
        let padded = len
            .checked_add(3)
            .map(|n| n & !3)
            .ok_or(AbrError::Malformed("a brush length overflows"))?;
        // The last brush's padding may be missing; never read past the end.
        let body = c.take(len, "a brush")?;
        let pad = (padded - len).min(c.remaining());
        c.take(pad, "brush padding")?;
        if out.len() >= MAX_BRUSHES {
            return Err(AbrError::TooManyBrushes);
        }
        out.push(read_brush(body, subversion, budget)?);
    }
    Ok(())
}

/// `budget` is what is left of [`MAX_TOTAL_PIXELS`]; this brush's pixels
/// are taken from it before anything is decoded or allocated.
fn read_brush(body: &[u8], subversion: u16, budget: &mut usize) -> Result<AbrBrush, AbrError> {
    let mut c = Cursor::new(body);
    // An identifier (a Pascal-ish key) and unknown fields: 47 bytes in
    // subversion 1, 301 in subversion 2 (the layout GIMP reads too).
    let skip = if subversion == 1 { 47 } else { 301 };
    c.take(skip, "a brush header")?;
    let top = c.i32("the brush bounds")?;
    let left = c.i32("the brush bounds")?;
    let bottom = c.i32("the brush bounds")?;
    let right = c.i32("the brush bounds")?;
    let depth = c.u16("the brush depth")?;
    let compression = c.u8("the brush compression")?;
    let width = i64::from(right) - i64::from(left);
    let height = i64::from(bottom) - i64::from(top);
    if width <= 0 || height <= 0 {
        return Err(AbrError::Malformed("the tip has no area"));
    }
    if width > i64::from(MAX_SIDE) || height > i64::from(MAX_SIDE) {
        return Err(AbrError::Malformed("the tip is larger than 5000 pixels"));
    }
    let (width, height) = (width as usize, height as usize);
    *budget = budget
        .checked_sub(width * height)
        .ok_or(AbrError::TooManyPixels)?;
    let bytes_per = match depth {
        8 => 1,
        16 => 2,
        _ => return Err(AbrError::Malformed("the tip's bit depth is not 8 or 16")),
    };
    let row = width * bytes_per;
    let plane = match compression {
        0 => {
            // Checked against what is left before anything is allocated.
            c.take(row * height, "the tip's pixels")?.to_vec()
        }
        1 => {
            let mut counts = Vec::with_capacity(height.min(c.remaining() / 2));
            for _ in 0..height {
                counts.push(c.u16("the scanline lengths")? as usize);
            }
            // PackBits expands at most 64x (two bytes to 128), so what is
            // left of the file bounds what is worth reserving.
            let mut plane =
                Vec::with_capacity((row * height).min(c.remaining().saturating_mul(64)));
            for count in counts {
                let packed = c.take(count, "a compressed scanline")?;
                unpack_bits(packed, row, &mut plane)?;
            }
            plane
        }
        _ => return Err(AbrError::Malformed("unknown compression")),
    };
    // A 16-bit tip keeps its high byte.
    let alpha8 = if bytes_per == 2 {
        plane.as_chunks::<2>().0.iter().map(|p| p[0]).collect()
    } else {
        plane
    };
    Ok(AbrBrush {
        width: width as u32,
        height: height as u32,
        alpha8,
    })
}

/// PackBits one scanline of exactly `row` bytes into `out`.
fn unpack_bits(mut packed: &[u8], row: usize, out: &mut Vec<u8>) -> Result<(), AbrError> {
    let start = out.len();
    while let Some((&n, rest)) = packed.split_first() {
        packed = rest;
        let n = n as i8;
        if n == -128 {
            continue;
        }
        let written = out.len() - start;
        if n < 0 {
            let run = 1 - n as isize;
            let (&value, rest) = packed
                .split_first()
                .ok_or(AbrError::Malformed("a scanline run is cut short"))?;
            packed = rest;
            if written + run as usize > row {
                return Err(AbrError::Malformed("a scanline overruns its width"));
            }
            out.resize(out.len() + run as usize, value);
        } else {
            let run = n as usize + 1;
            if run > packed.len() {
                return Err(AbrError::Malformed("a scanline literal is cut short"));
            }
            if written + run > row {
                return Err(AbrError::Malformed("a scanline overruns its width"));
            }
            out.extend_from_slice(&packed[..run]);
            packed = &packed[run..];
        }
    }
    if out.len() - start != row {
        return Err(AbrError::Malformed("a scanline is shorter than its width"));
    }
    Ok(())
}

/// Test support: a v6 `.abr` holding `tips` (raw when `rle` is false,
/// PackBits otherwise). Public so the application's own tests can build a
/// fixture the same way.
pub fn write_test_abr(tips: &[(u32, u32, Vec<u8>)], subversion: u16, rle: bool) -> Vec<u8> {
    let mut samp = Vec::new();
    for (w, h, alpha) in tips {
        let mut body = vec![0u8; if subversion == 1 { 47 } else { 301 }];
        for v in [0i32, 0, *h as i32, *w as i32] {
            body.extend_from_slice(&v.to_be_bytes());
        }
        body.extend_from_slice(&8u16.to_be_bytes());
        if rle {
            body.push(1);
            let rows: Vec<Vec<u8>> = alpha
                .chunks(*w as usize)
                .map(|r| {
                    // Literal runs of at most 128 bytes.
                    let mut packed = Vec::new();
                    for chunk in r.chunks(128) {
                        packed.push((chunk.len() - 1) as u8);
                        packed.extend_from_slice(chunk);
                    }
                    packed
                })
                .collect();
            for r in &rows {
                body.extend_from_slice(&(r.len() as u16).to_be_bytes());
            }
            for r in rows {
                body.extend_from_slice(&r);
            }
        } else {
            body.push(0);
            body.extend_from_slice(alpha);
        }
        samp.extend_from_slice(&(body.len() as u32).to_be_bytes());
        let len = body.len();
        samp.extend_from_slice(&body);
        samp.resize(samp.len() + ((4 - len % 4) % 4), 0);
    }
    let mut file = Vec::new();
    file.extend_from_slice(&6u16.to_be_bytes());
    file.extend_from_slice(&subversion.to_be_bytes());
    // A section this reader skips, then the samples.
    file.extend_from_slice(b"8BIMdesc");
    file.extend_from_slice(&2u32.to_be_bytes());
    file.extend_from_slice(&[0, 0]);
    file.extend_from_slice(b"8BIMsamp");
    file.extend_from_slice(&(samp.len() as u32).to_be_bytes());
    file.extend_from_slice(&samp);
    file
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disc(side: u32) -> Vec<u8> {
        let c = side as f32 / 2.0;
        (0..side * side)
            .map(|i| {
                let (x, y) = ((i % side) as f32 + 0.5, (i / side) as f32 + 0.5);
                if (x - c).hypot(y - c) < c {
                    255
                } else {
                    0
                }
            })
            .collect()
    }

    #[test]
    fn a_v6_file_imports_every_sampled_brush_raw_and_packed() {
        let tips = vec![
            (8, 8, disc(8)),
            (5, 3, (0..15).map(|i| i as u8 * 17).collect()),
            (200, 2, vec![128; 400]),
        ];
        for (sub, rle) in [(1, false), (2, false), (1, true), (2, true)] {
            let file = write_test_abr(&tips, sub, rle);
            let brushes = parse_abr(&file).unwrap_or_else(|e| panic!("sub {sub} rle {rle}: {e}"));
            assert_eq!(brushes.len(), 3, "sub {sub} rle {rle}");
            for (b, (w, h, a)) in brushes.iter().zip(&tips) {
                assert_eq!((b.width, b.height), (*w, *h));
                assert_eq!(&b.alpha8, a);
            }
        }
    }

    /// A PackBits "bomb": tips that are all 128-byte repeat runs, so each
    /// 5000x5000 tip is ~410 KB of file and 25 MB decoded. Eleven of them
    /// (4.5 MB, inside every per-file and per-brush cap) would decode to
    /// 275 MB; the total-pixel budget refuses the file instead.
    fn packbits_bomb(tips: usize, side: u32) -> Vec<u8> {
        let mut row = Vec::new();
        let mut left = side as usize;
        while left > 0 {
            let run = left.min(128);
            row.push((1 - run as i32) as i8 as u8);
            row.push(255);
            left -= run;
        }
        let mut samp = Vec::new();
        for _ in 0..tips {
            let mut body = vec![0u8; 47];
            for v in [0i32, 0, side as i32, side as i32] {
                body.extend_from_slice(&v.to_be_bytes());
            }
            body.extend_from_slice(&8u16.to_be_bytes());
            body.push(1);
            for _ in 0..side {
                body.extend_from_slice(&(row.len() as u16).to_be_bytes());
            }
            for _ in 0..side {
                body.extend_from_slice(&row);
            }
            samp.extend_from_slice(&(body.len() as u32).to_be_bytes());
            let len = body.len();
            samp.extend_from_slice(&body);
            samp.resize(samp.len() + ((4 - len % 4) % 4), 0);
        }
        let mut file = vec![0, 6, 0, 1];
        file.extend_from_slice(b"8BIMsamp");
        file.extend_from_slice(&(samp.len() as u32).to_be_bytes());
        file.extend_from_slice(&samp);
        file
    }

    #[test]
    fn a_packbits_bomb_is_refused_by_the_decoded_pixel_budget() {
        // The fixture really is well-formed: one of its tips parses.
        let one = parse_abr(&packbits_bomb(1, 64)).unwrap();
        assert_eq!((one[0].width, one[0].height), (64, 64));
        assert!(one[0].alpha8.iter().all(|&a| a == 255));

        let bomb = packbits_bomb(11, MAX_SIDE);
        assert!(bomb.len() < MAX_ABR_BYTES / 10, "{} bytes", bomb.len());
        assert!(11 * (MAX_SIDE as usize).pow(2) > MAX_TOTAL_PIXELS);
        // Compared without Debug-printing hundreds of MB of pixels.
        let result = parse_abr(&bomb).map(|b| b.len());
        assert_eq!(result, Err(AbrError::TooManyPixels));
    }

    #[test]
    fn a_packbits_run_expands() {
        let mut out = Vec::new();
        // -3 => repeat the next byte 4 times; 1 => copy 2 literals.
        unpack_bits(&[0xFD, 9, 1, 4, 5], 6, &mut out).unwrap();
        assert_eq!(out, vec![9, 9, 9, 9, 4, 5]);
    }

    #[test]
    fn malformed_files_are_errors_never_panics() {
        let good = write_test_abr(&[(8, 8, disc(8)), (4, 4, vec![255; 16])], 2, true);
        // Every truncation of a good file.
        for n in 0..good.len() {
            let _ = parse_abr(&good[..n]);
        }
        assert!(parse_abr(&good[..good.len() - 3]).is_err());
        // Every single-byte corruption of it.
        for i in 0..good.len() {
            for v in [0x00, 0x7F, 0x80, 0xFF] {
                let mut bad = good.clone();
                bad[i] = v;
                let _ = parse_abr(&bad);
            }
        }
        assert_eq!(parse_abr(&[]), Err(AbrError::Truncated("the header")));
        assert_eq!(
            parse_abr(&[0, 2, 0, 1]),
            Err(AbrError::UnsupportedVersion(2))
        );
        assert_eq!(parse_abr(&[0, 6, 0, 1]), Err(AbrError::NoBrushes));
        // A section that claims more bytes than the file has.
        let mut lying = vec![0, 6, 0, 1];
        lying.extend_from_slice(b"8BIMsamp");
        lying.extend_from_slice(&u32::MAX.to_be_bytes());
        assert!(matches!(parse_abr(&lying), Err(AbrError::Truncated(_))));
        // A tip claiming 5000x5000 raw pixels with none present is refused
        // before a 25 MB buffer is allocated for it.
        let mut huge = write_test_abr(&[(1, 1, vec![255])], 1, false);
        let bounds_at = 4 + 8 + 4 + 2 + 8 + 4 + 4 + 47;
        huge[bounds_at + 8..bounds_at + 12].copy_from_slice(&5000i32.to_be_bytes());
        huge[bounds_at + 12..bounds_at + 16].copy_from_slice(&5000i32.to_be_bytes());
        assert!(parse_abr(&huge).is_err());
        let mut wide = huge.clone();
        wide[bounds_at + 12..bounds_at + 16].copy_from_slice(&5001i32.to_be_bytes());
        assert_eq!(
            parse_abr(&wide),
            Err(AbrError::Malformed("the tip is larger than 5000 pixels"))
        );
        // A scanline that decodes past its width.
        assert!(unpack_bits(&[0x81, 7], 4, &mut Vec::new()).is_err());
        assert!(unpack_bits(&[5, 1, 2], 6, &mut Vec::new()).is_err());
    }
}
