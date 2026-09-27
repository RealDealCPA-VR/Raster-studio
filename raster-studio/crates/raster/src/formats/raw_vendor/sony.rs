//! W18-D: Sony ARW / SR2 / SRF.
//!
//! The raw image is the CFA `SubIFD` of IFD0. Two storages are read:
//!
//! - **ARW 2 ("cRAW")**, `Compression = 32767` with one byte per pixel, as
//!   Henry Dietz describes it ("Sony ARW2 Compression: Artifacts and
//!   Credible Repair", EI 2016): each row is cut into 32-pixel chunks; a
//!   chunk is two 16-byte blocks, the first holding the chunk's even
//!   columns, the second its odd ones (one CFA colour each). A block is 128
//!   bits read least-significant first from little-endian bytes: an 11-bit
//!   maximum, an 11-bit minimum, the 4-bit positions of the maximum and the
//!   minimum, then fourteen 7-bit deltas for the other pixels, each scaled
//!   by the smallest power of two `2^s` (`s <= 4`) with `max - min <
//!   128 * 2^s` and added to the minimum (clipped at 2047). The 11-bit
//!   values, doubled, index the tone curve: five linear segments whose
//!   step doubles (1, 2, 4, 8, 16) at the four thresholds `SonyToneCurve`
//!   (`0x7010`) records (each divided by four); without that tag the codes
//!   are kept as stored.
//! - **Uncompressed**, 16-bit words (little-endian) or rows packed
//!   MSB-first.
//!
//! Levels: the DNG `BlackLevel` / `WhiteLevel` of the raw IFD, else Sony's
//! `0x7310` (black, per cell) / `0x787F` (white) there, else black 512 (the
//! fixed offset the paper names, on the 14-bit scale) or 128 for 12-bit
//! data. The as-shot balance lives in Sony's encrypted `SR2Private` IFD,
//! which is not read: the balance is grey world. The CFA is the raw IFD's
//! TIFF/EP `CFAPattern`, else red-green / green-blue.
//!
//! Refused by name: ARW 1 (`32767` with less than a byte per pixel, the
//! DSLR-A100) and Sony's lossless-compressed ARW (`Compression = 7`).

use crate::codec::{CodecError, ImportLimits};

use super::{
    broken, budget, cells, ifd0, not_decoded, orientation, raw_ifd, rggb, strips, tiff_any,
    tiff_cfa, unpack_plain, Mosaic, Want, TAG_BITS, TAG_COMPRESSION, TAG_HEIGHT, TAG_WIDTH,
};

const NAME: &str = "Sony ARW";
const TAG_TONE_CURVE: u16 = 0x7010;
const TAG_SONY_BLACK: u16 = 0x7310;
const TAG_SONY_WHITE: u16 = 0x787F;

/// The ARW 2 tone curve over doubled 11-bit codes (4096 entries).
pub(super) fn tone_curve(thresholds: Option<&[u32]>) -> Vec<u32> {
    let mut curve: Vec<u32> = (0..4096).collect();
    let Some(t) = thresholds.filter(|t| t.len() == 4) else {
        return curve;
    };
    let mut knots = [0usize; 6];
    knots[5] = 4095;
    for i in 0..4 {
        knots[i + 1] = ((t[i] >> 2) & 0xFFF) as usize;
    }
    for i in 0..5 {
        for j in knots[i] + 1..=knots[i + 1].min(4095) {
            curve[j] = curve[j - 1] + (1 << i);
        }
    }
    curve
}

/// Decode ARW 2 rows (one byte per pixel) through `curve`.
pub(super) fn arw2(data: &[u8], w: usize, h: usize, curve: &[u32]) -> Result<Vec<u16>, CodecError> {
    let need = w
        .checked_mul(h)
        .ok_or_else(|| broken(NAME, "the image is too large"))?;
    if data.len() < need || curve.len() < 4096 {
        return Err(broken(
            NAME,
            "the compressed raw data is shorter than the image",
        ));
    }
    let mut out = vec![0u16; need];
    for y in 0..h {
        let row = &data[y * w..(y + 1) * w];
        let mut col = 0usize;
        while col + 32 <= w {
            for half in 0..2 {
                let at = col + half * 16;
                let mut block = [0u8; 16];
                block.copy_from_slice(&row[at..at + 16]);
                let v = u128::from_le_bytes(block);
                let max = (v & 0x7FF) as u32;
                let min = ((v >> 11) & 0x7FF) as u32;
                let imax = ((v >> 22) & 0xF) as usize;
                let imin = ((v >> 26) & 0xF) as usize;
                let mut sh = 0u32;
                while sh < 4 && (0x80u32 << sh) <= max.saturating_sub(min) {
                    sh += 1;
                }
                let mut bit = 30u32;
                for i in 0..16 {
                    let p = if i == imax {
                        max
                    } else if i == imin {
                        min
                    } else {
                        // A damaged block naming one position twice has
                        // no bits for its fifteenth delta: zero.
                        let d = (v.checked_shr(bit).unwrap_or(0) & 0x7F) as u32;
                        bit += 7;
                        ((d << sh) + min).min(0x7FF)
                    };
                    let code = curve[(p << 1) as usize];
                    out[y * w + col + half + 2 * i] = code.min(u32::from(u16::MAX)) as u16;
                }
            }
            col += 32;
        }
    }
    Ok(out)
}

pub(super) fn read(bytes: &[u8], limits: ImportLimits, want: Want) -> Result<Mosaic, CodecError> {
    let tiff = tiff_any(bytes).ok_or_else(|| broken(NAME, "not a TIFF"))?;
    let ifd0 = ifd0(&tiff, NAME)?;
    let raw = raw_ifd(&tiff, NAME)?;
    let width = raw.uint(&tiff, TAG_WIDTH).unwrap_or(0) as usize;
    let height = raw.uint(&tiff, TAG_HEIGHT).unwrap_or(0) as usize;
    budget(NAME, limits, width, height)?;
    let compression = raw.uint(&tiff, TAG_COMPRESSION).unwrap_or(1);
    let data = strips(&tiff, &raw, NAME)?;
    let pixels = width * height;
    let compressed = match compression {
        32767 if data.len() >= pixels.saturating_mul(2) => false,
        32767 if data.len() >= pixels => true,
        32767 => return Err(not_decoded(NAME, "ARW 1 compression (the DSLR-A100's)")),
        1 => false,
        7 => return Err(not_decoded(NAME, "lossless compression (Compression = 7)")),
        other => return Err(not_decoded(NAME, &format!("raw compression {other}"))),
    };
    let bits = raw.uint(&tiff, TAG_BITS).unwrap_or(14);
    let thresholds = raw.uints_of(&tiff, TAG_TONE_CURVE);
    let curve = tone_curve(thresholds.as_deref());
    let samples = match (want, compressed) {
        (Want::Header, _) => Vec::new(),
        (Want::Pixels, true) => arw2(&data, width, height, &curve)?,
        (Want::Pixels, false) => unpack_plain(NAME, &data, width, height, bits, tiff.le)?,
    };
    let (dng_black, dng_white) = super::dng_levels(&tiff, &raw);
    let black = dng_black
        .or_else(|| raw.floats_of(&tiff, TAG_SONY_BLACK).and_then(|b| cells(&b)))
        .unwrap_or(if compressed || bits >= 14 {
            [512.0; 4]
        } else {
            [128.0; 4]
        });
    let white = dng_white.or_else(|| {
        raw.floats_of(&tiff, TAG_SONY_WHITE)
            .and_then(|w| w.first().copied())
            .filter(|&w| w > 0.0)
    });
    let full_scale = if compressed {
        f64::from(curve[0x7FF << 1].min(u32::from(u16::MAX)))
    } else {
        f64::from((1u32 << bits.clamp(1, 16)) - 1)
    };
    Ok(Mosaic {
        name: NAME,
        width,
        height,
        samples,
        pattern: tiff_cfa(&tiff, &ifd0, &raw).unwrap_or_else(rggb),
        active: (0, 0, height, width),
        black,
        white,
        full_scale,
        wb: None,
        orientation: orientation(&tiff, &ifd0),
    })
}
