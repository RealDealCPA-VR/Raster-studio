//! ZIP (zlib) channel encoding, with and without Photoshop's delta prediction.
//!
//! Prediction is a per-row reversible transform applied *before* deflate so the
//! stream compresses better. It is depth-dependent, and the 32-bit case is not
//! a wider version of the 8-bit case — it is a different scheme:
//!
//! * **8-bit** — each byte holds the difference from the byte to its left.
//! * **16-bit** — each big-endian sample holds the difference from the sample
//!   to its left. The delta is on the *sample*, not on the byte, so it must be
//!   undone before the bytes mean anything.
//! * **32-bit** — the row is first split into four byte-planes (all the
//!   high bytes, then all the second bytes, and so on) and *then* delta-coded
//!   byte-wise across the whole planar row. Decoding undoes the delta first and
//!   re-interleaves second. Doing those two steps in the wrong order produces
//!   an image that looks like static, which is why each direction has its own
//!   test here.
//!
//! # Untrusted input
//!
//! A zlib stream is a decompression bomb waiting to happen: a few kilobytes can
//! inflate to gigabytes. [`inflate_exact`] never inflates into an unbounded
//! buffer — it reads through a `Read::take` capped one byte past the size the
//! channel's own geometry says it must be, so a bomb is refused after one extra
//! byte rather than after one extra gigabyte.

use std::io::{Read, Write};

use crate::error::{PsdError, PsdResult};
use crate::header::Depth;

/// Inflate `src`, requiring the result to be exactly `expected` bytes.
pub fn inflate_exact(src: &[u8], expected: usize) -> PsdResult<Vec<u8>> {
    let mut out = Vec::with_capacity(expected.min(1 << 20));
    let limit = expected as u64 + 1;
    let mut dec = flate2::read::ZlibDecoder::new(src).take(limit);
    dec.read_to_end(&mut out)
        .map_err(|e| PsdError::BadZip(e.to_string()))?;
    if out.len() != expected {
        return Err(PsdError::ChannelSizeMismatch {
            what: "zip channel",
            expected,
            actual: out.len(),
        });
    }
    Ok(out)
}

pub fn deflate(src: &[u8]) -> PsdResult<Vec<u8>> {
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(src)
        .map_err(|e| PsdError::BadZip(e.to_string()))?;
    enc.finish().map_err(|e| PsdError::BadZip(e.to_string()))
}

/// Undo the delta transform, in place, over `height` rows of `width` samples.
///
/// `data` must already be `height * width * depth.bytes_per_sample()` bytes;
/// callers get that from [`crate::codec::ChannelShape`], which computes it with
/// checked arithmetic from validated dimensions.
pub fn unpredict(data: &mut [u8], width: usize, height: usize, depth: Depth) -> PsdResult<()> {
    let row_bytes = row_bytes(width, height, depth, data.len())?;
    for row in data.chunks_mut(row_bytes) {
        match depth {
            Depth::Eight => unpredict_row_8(row),
            Depth::Sixteen => unpredict_row_16(row),
            Depth::ThirtyTwo => unpredict_row_32(row, width),
        }
    }
    Ok(())
}

/// Apply the delta transform, in place, ready for deflate.
pub fn predict(data: &mut [u8], width: usize, height: usize, depth: Depth) -> PsdResult<()> {
    let row_bytes = row_bytes(width, height, depth, data.len())?;
    for row in data.chunks_mut(row_bytes) {
        match depth {
            Depth::Eight => predict_row_8(row),
            Depth::Sixteen => predict_row_16(row),
            Depth::ThirtyTwo => predict_row_32(row, width),
        }
    }
    Ok(())
}

fn row_bytes(width: usize, height: usize, depth: Depth, len: usize) -> PsdResult<usize> {
    let row = width
        .checked_mul(depth.bytes_per_sample())
        .ok_or(PsdError::Overflow { what: "row bytes" })?;
    let total = row.checked_mul(height).ok_or(PsdError::Overflow {
        what: "channel bytes",
    })?;
    if total != len {
        return Err(PsdError::ChannelSizeMismatch {
            what: "predicted channel",
            expected: total,
            actual: len,
        });
    }
    // A zero-width row would make `chunks_mut(0)` panic.
    Ok(row.max(1))
}

fn unpredict_row_8(row: &mut [u8]) {
    for i in 1..row.len() {
        row[i] = row[i].wrapping_add(row[i - 1]);
    }
}

fn predict_row_8(row: &mut [u8]) {
    for i in (1..row.len()).rev() {
        row[i] = row[i].wrapping_sub(row[i - 1]);
    }
}

fn unpredict_row_16(row: &mut [u8]) {
    let n = row.len() / 2;
    for i in 1..n {
        let prev = u16::from_be_bytes([row[(i - 1) * 2], row[(i - 1) * 2 + 1]]);
        let here = u16::from_be_bytes([row[i * 2], row[i * 2 + 1]]);
        row[i * 2..i * 2 + 2].copy_from_slice(&here.wrapping_add(prev).to_be_bytes());
    }
}

fn predict_row_16(row: &mut [u8]) {
    let n = row.len() / 2;
    for i in (1..n).rev() {
        let prev = u16::from_be_bytes([row[(i - 1) * 2], row[(i - 1) * 2 + 1]]);
        let here = u16::from_be_bytes([row[i * 2], row[i * 2 + 1]]);
        row[i * 2..i * 2 + 2].copy_from_slice(&here.wrapping_sub(prev).to_be_bytes());
    }
}

/// Decode: undo the byte-wise delta over the whole planar row, then interleave
/// the four planes back into samples.
fn unpredict_row_32(row: &mut [u8], width: usize) {
    unpredict_row_8(row);
    // `width` arrives from the caller, and every index below is built from it.
    // `row_bytes` has already proved `row.len() == width * 4` for a 32-bit row,
    // so the clamp never bites — it is here because that proof lives in another
    // function, and this one is not entitled to a panic if it stops holding.
    let width = width.min(row.len() / 4);
    if width == 0 {
        return;
    }
    let planar = row.to_vec();
    for x in 0..width {
        for p in 0..4 {
            row[x * 4 + p] = planar[p * width + x];
        }
    }
}

/// Encode: split the row into four byte-planes, then delta over the result.
fn predict_row_32(row: &mut [u8], width: usize) {
    // Clamped for the same reason [`unpredict_row_32`] clamps.
    let width = width.min(row.len() / 4);
    if width != 0 {
        let interleaved = row.to_vec();
        for p in 0..4 {
            for x in 0..width {
                row[p * width + x] = interleaved[x * 4 + p];
            }
        }
    }
    predict_row_8(row);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_rows(width: usize, height: usize, depth: Depth) -> Vec<u8> {
        let n = width * height * depth.bytes_per_sample();
        (0..n).map(|i| ((i * 37 + i / 5) % 251) as u8).collect()
    }

    /// Every index in the 32-bit predictor is built from `width`, which arrives
    /// from the caller rather than from the row being transformed. `row_bytes`
    /// proves `row.len() == width * 4` first — but it does so in a *different*
    /// function, and the crate docs say no index in this crate rests on that
    /// kind of promise. Unclamped, both calls below are an index out of bounds.
    #[test]
    fn a_thirty_two_bit_row_shorter_than_its_width_is_transformed_short_not_panicked_on() {
        for width in [3usize, 100] {
            let mut row = vec![1u8, 2, 3, 4, 5, 6];
            unpredict_row_32(&mut row, width);
            assert_eq!(row.len(), 6, "nothing grew to meet the index");
            let mut row = vec![1u8, 2, 3, 4, 5, 6];
            predict_row_32(&mut row, width);
            assert_eq!(row.len(), 6);
        }
    }

    #[test]
    fn prediction_is_reversible_at_every_depth() {
        for depth in [Depth::Eight, Depth::Sixteen, Depth::ThirtyTwo] {
            let (w, h) = (11usize, 7usize);
            let original = sample_rows(w, h, depth);
            let mut data = original.clone();
            predict(&mut data, w, h, depth).unwrap();
            assert_ne!(data, original, "{depth:?}: prediction changed nothing");
            unpredict(&mut data, w, h, depth).unwrap();
            assert_eq!(data, original, "{depth:?}");
        }
    }

    #[test]
    fn eight_bit_prediction_is_a_left_delta_within_a_row_and_never_across_rows() {
        // Two rows of four. Each row starts over: the first byte of row two is
        // stored as itself, not as a difference from the last byte of row one.
        let mut data = vec![10u8, 12, 12, 20, 100, 90, 90, 90];
        predict(&mut data, 4, 2, Depth::Eight).unwrap();
        assert_eq!(data, vec![10, 2, 0, 8, 100, 246, 0, 0]);
    }

    #[test]
    fn sixteen_bit_prediction_works_on_samples_not_on_bytes() {
        // 0x0100, 0x0102 -> the delta is 2, which only shows up in the low byte
        // if the delta was taken on the sample.
        let mut data = vec![0x01, 0x00, 0x01, 0x02];
        predict(&mut data, 2, 1, Depth::Sixteen).unwrap();
        assert_eq!(data, vec![0x01, 0x00, 0x00, 0x02]);
        unpredict(&mut data, 2, 1, Depth::Sixteen).unwrap();
        assert_eq!(data, vec![0x01, 0x00, 0x01, 0x02]);
    }

    #[test]
    fn thirty_two_bit_prediction_deplanarises_in_the_right_order() {
        // Two samples: bytes A0 A1 A2 A3 / B0 B1 B2 B3.
        let original = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
        let mut data = original.clone();
        predict(&mut data, 2, 1, Depth::ThirtyTwo).unwrap();
        // Planar order is 1,5, 2,6, 3,7, 4,8; the byte delta of that is:
        assert_eq!(data, vec![1, 4, 253, 4, 253, 4, 253, 4]);
        unpredict(&mut data, 2, 1, Depth::ThirtyTwo).unwrap();
        assert_eq!(data, original);
    }

    #[test]
    fn zlib_round_trips() {
        let src = sample_rows(64, 64, Depth::Eight);
        let packed = deflate(&src).unwrap();
        assert_eq!(inflate_exact(&packed, src.len()).unwrap(), src);
    }

    #[test]
    fn a_zip_bomb_is_refused_after_one_byte_over_the_expected_size() {
        // 4 MiB of zeros compresses to a few kilobytes.
        let bomb = deflate(&vec![0u8; 4 << 20]).unwrap();
        assert!(bomb.len() < 64 * 1024, "fixture is not a bomb");
        let err = inflate_exact(&bomb, 100).unwrap_err();
        match err {
            PsdError::ChannelSizeMismatch {
                expected, actual, ..
            } => assert_eq!((expected, actual), (100, 101)),
            other => panic!("wrong error: {other}"),
        }
    }

    #[test]
    fn garbage_is_a_typed_error_rather_than_a_panic() {
        let err = inflate_exact(&[0xDE, 0xAD, 0xBE, 0xEF], 16).unwrap_err();
        assert!(matches!(err, PsdError::BadZip(_)), "{err}");
    }

    #[test]
    fn a_stream_shorter_than_expected_is_a_size_mismatch() {
        let packed = deflate(&[1u8, 2, 3]).unwrap();
        assert!(matches!(
            inflate_exact(&packed, 99).unwrap_err(),
            PsdError::ChannelSizeMismatch { .. }
        ));
    }

    #[test]
    fn prediction_on_a_wrongly_sized_buffer_is_refused_not_a_panic() {
        let mut data = vec![0u8; 5];
        assert!(matches!(
            predict(&mut data, 4, 2, Depth::Eight).unwrap_err(),
            PsdError::ChannelSizeMismatch { .. }
        ));
    }

    #[test]
    fn a_zero_width_channel_does_not_divide_by_zero() {
        let mut data: Vec<u8> = Vec::new();
        predict(&mut data, 0, 5, Depth::ThirtyTwo).unwrap();
        unpredict(&mut data, 0, 5, Depth::ThirtyTwo).unwrap();
        assert!(data.is_empty());
    }
}
