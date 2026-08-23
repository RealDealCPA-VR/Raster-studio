//! PackBits, the run-length coding a `.psd` calls "RLE".
//!
//! A packet is a signed length byte `n`:
//!
//! * `0..=127` — the next `n + 1` bytes are literal;
//! * `-1..=-127` — the next byte repeats `1 - n` times;
//! * `-128` — a no-op, which real encoders never emit but real files contain.
//!
//! Decoding is where a hostile file gets its best shot at this crate: a two
//! byte packet can ask for 128 output bytes, so a small file can ask for a
//! large buffer. [`decode_into`] therefore takes an explicit `limit` and stops
//! at it rather than trusting the packets to add up.

use crate::error::{PsdError, PsdResult};

/// Longest run or literal one packet can carry.
pub const MAX_PACKET: usize = 128;

/// Decode packets from `src`, appending to `out`, refusing to write more than
/// `limit` bytes in total.
///
/// Returns the number of bytes appended. A packet that would push the output
/// past `limit` is an error, not a silent truncation: the caller knows exactly
/// how many bytes the row should be, and a row that disagrees means the file
/// is damaged.
pub fn decode_into(src: &[u8], out: &mut Vec<u8>, limit: usize) -> PsdResult<usize> {
    let start = out.len();
    let mut i = 0usize;
    while i < src.len() {
        let n = src[i] as i8;
        i += 1;
        if n >= 0 {
            let count = n as usize + 1;
            if i + count > src.len() {
                return Err(PsdError::Truncated {
                    needed: count,
                    available: src.len() - i,
                    at: i,
                });
            }
            if out.len() - start + count > limit {
                return Err(PsdError::BadRle {
                    row: 0,
                    expected: limit,
                    actual: out.len() - start + count,
                });
            }
            out.extend_from_slice(&src[i..i + count]);
            i += count;
        } else if n != -128 {
            let count = 1 - n as isize;
            debug_assert!((2..=MAX_PACKET as isize).contains(&count));
            let count = count as usize;
            if i >= src.len() {
                return Err(PsdError::Truncated {
                    needed: 1,
                    available: 0,
                    at: i,
                });
            }
            let value = src[i];
            i += 1;
            if out.len() - start + count > limit {
                return Err(PsdError::BadRle {
                    row: 0,
                    expected: limit,
                    actual: out.len() - start + count,
                });
            }
            out.resize(out.len() + count, value);
        }
        // n == -128 is a no-op packet.
    }
    Ok(out.len() - start)
}

/// Decode exactly `expected` bytes, or fail.
pub fn decode_exact(src: &[u8], expected: usize, row: usize) -> PsdResult<Vec<u8>> {
    let mut out = Vec::with_capacity(expected.min(1 << 16));
    let got = decode_into(src, &mut out, expected).map_err(|e| match e {
        PsdError::BadRle {
            expected, actual, ..
        } => PsdError::BadRle {
            row,
            expected,
            actual,
        },
        other => other,
    })?;
    if got != expected {
        return Err(PsdError::BadRle {
            row,
            expected,
            actual: got,
        });
    }
    Ok(out)
}

/// Longest output [`encode_into`] can produce for `n` input bytes.
///
/// Every 128 input bytes cost at most one extra length byte, and a final short
/// literal costs one more.
pub const fn max_encoded_len(n: usize) -> usize {
    n + n / MAX_PACKET + 1
}

/// PackBits-encode `src`, appending to `out`.
///
/// Runs of three or more identical bytes become repeat packets; everything else
/// travels as literals. Two-byte runs stay literal because encoding them costs
/// the same two bytes and would break up a longer literal packet.
pub fn encode_into(src: &[u8], out: &mut Vec<u8>) {
    let n = src.len();
    let mut i = 0usize;
    while i < n {
        let mut run = 1usize;
        while i + run < n && src[i + run] == src[i] && run < MAX_PACKET {
            run += 1;
        }
        if run >= 3 {
            out.push((257 - run) as u8);
            out.push(src[i]);
            i += run;
            continue;
        }
        // Literal packet: stop where a run of three begins, or at 128 bytes.
        let start = i;
        let mut j = i;
        while j < n {
            if j + 2 < n && src[j] == src[j + 1] && src[j + 1] == src[j + 2] {
                break;
            }
            j += 1;
            if j - start == MAX_PACKET {
                break;
            }
        }
        debug_assert!(j > start, "the literal scan always advances");
        out.push((j - start - 1) as u8);
        out.extend_from_slice(&src[start..j]);
        i = j;
    }
}

pub fn encode(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(max_encoded_len(src.len()));
    encode_into(src, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(src: &[u8]) {
        let packed = encode(src);
        assert!(
            packed.len() <= max_encoded_len(src.len()),
            "encoder exceeded its own worst case: {} > {}",
            packed.len(),
            max_encoded_len(src.len())
        );
        let out = decode_exact(&packed, src.len(), 0).unwrap();
        assert_eq!(out, src);
    }

    #[test]
    fn round_trips_runs_literals_and_the_boundaries_between_them() {
        round_trip(&[]);
        round_trip(&[7]);
        round_trip(&[7, 7]);
        round_trip(&[7, 7, 7]);
        round_trip(&[1, 2, 3, 4, 5]);
        round_trip(&[9; 300]);
        round_trip(&[9; 128]);
        round_trip(&[9; 129]);
        let mut mixed = vec![0u8; 200];
        for (i, b) in mixed.iter_mut().enumerate() {
            *b = if i % 17 < 5 { 42 } else { i as u8 };
        }
        round_trip(&mixed);
    }

    #[test]
    fn round_trips_every_length_up_to_past_two_packets() {
        for len in 0..300usize {
            let src: Vec<u8> = (0..len).map(|i| (i * 31 % 7) as u8).collect();
            round_trip(&src);
        }
    }

    #[test]
    fn a_run_longer_than_one_packet_is_split() {
        let packed = encode(&[5u8; 200]);
        // 128 + 72, two repeat packets of two bytes each.
        assert_eq!(packed.len(), 4);
        assert_eq!(decode_exact(&packed, 200, 0).unwrap(), vec![5u8; 200]);
    }

    #[test]
    fn the_noop_packet_is_skipped_rather_than_treated_as_a_literal() {
        // -128, then a two-byte literal.
        let packed = [0x80u8, 0x01, 0xAA, 0xBB];
        assert_eq!(decode_exact(&packed, 2, 0).unwrap(), vec![0xAA, 0xBB]);
    }

    /// The refusal has to happen *inside* the packet loop, not in
    /// [`decode_exact`]'s trailing length check. Both produce a `BadRle` with
    /// the same `row` and `expected`, so those two fields cannot tell them
    /// apart — which is how a missing in-loop guard hid behind this test's
    /// earlier version. Two things here can:
    ///
    /// * `actual` is the first over-long packet's end (`limit`-bounded) when the
    ///   guard fires, and the full expanded length when only the trailing check
    ///   does;
    /// * the bytes the decode asks the allocator for, which is the property that
    ///   actually matters — 1 000 two-byte repeat packets are 2 KiB of input
    ///   claiming 128 KiB of output, a 64x amplification a hostile file gets for
    ///   free if the row is expanded before it is checked.
    #[test]
    fn a_packet_that_overruns_the_expected_row_is_refused_not_truncated() {
        // A repeat packet asking for 128 bytes when the row is 4.
        let packed = [0x81u8, 0xFF];
        let err = decode_exact(&packed, 4, 3).unwrap_err();
        match err {
            PsdError::BadRle {
                row,
                expected,
                actual,
            } => assert_eq!((row, expected, actual), (3, 4, 128)),
            other => panic!("wrong error: {other}"),
        }

        // A whole row of them. Without the in-loop guard the output grows to
        // 128 000 bytes before anybody complains, and `actual` says so.
        let mut many = Vec::new();
        for _ in 0..1000 {
            many.extend_from_slice(&[0x81u8, 0xFF]);
        }
        let (res, allocated) = crate::probe::bytes_allocated_by(|| decode_exact(&many, 4, 3));
        match res.unwrap_err() {
            PsdError::BadRle {
                row,
                expected,
                actual,
            } => assert_eq!(
                (row, expected, actual),
                (3, 4, 128),
                "the row was expanded before it was refused"
            ),
            other => panic!("wrong error: {other}"),
        }
        assert!(
            allocated < 4096,
            "decoding the over-long row allocated {allocated} bytes; the packets \
             claim 128 000 and the row is 4"
        );
    }

    /// The same guard on the literal branch, which the repeat-packet test does
    /// not reach. A literal cannot amplify the way a repeat packet does — its
    /// bytes have to be in the file — but [`decode_into`] promises never to
    /// write past `limit`, and without the in-loop check a section full of
    /// literals expands in full before anybody objects.
    #[test]
    fn a_literal_packet_past_the_expected_row_is_refused_at_the_packet() {
        let mut packed = Vec::new();
        for _ in 0..10 {
            packed.push(0x7Fu8); // 128 literal bytes follow
            packed.extend_from_slice(&[0xAB; 128]);
        }
        let (res, allocated) = crate::probe::bytes_allocated_by(|| decode_exact(&packed, 4, 2));
        match res.unwrap_err() {
            PsdError::BadRle {
                row,
                expected,
                actual,
            } => assert_eq!(
                (row, expected, actual),
                (2, 4, 128),
                "the row was expanded before it was refused"
            ),
            other => panic!("wrong error: {other}"),
        }
        assert!(
            allocated < 512,
            "decoding the over-long row allocated {allocated} bytes for a 4 byte row"
        );
    }

    #[test]
    fn a_literal_packet_running_past_the_input_is_refused() {
        // Claims 128 literal bytes, supplies one.
        let packed = [0x7Fu8, 0xAA];
        assert!(matches!(
            decode_exact(&packed, 128, 0).unwrap_err(),
            PsdError::Truncated { .. }
        ));
    }

    #[test]
    fn a_repeat_packet_with_no_value_byte_is_refused() {
        let packed = [0xFEu8];
        assert!(matches!(
            decode_exact(&packed, 3, 0).unwrap_err(),
            PsdError::Truncated { .. }
        ));
    }

    #[test]
    fn a_short_row_is_refused_rather_than_zero_filled() {
        let packed = encode(&[1, 2, 3]);
        let err = decode_exact(&packed, 8, 1).unwrap_err();
        match err {
            PsdError::BadRle {
                row,
                expected,
                actual,
            } => assert_eq!((row, expected, actual), (1, 8, 3)),
            other => panic!("wrong error: {other}"),
        }
    }

    #[test]
    fn decoding_arbitrary_bytes_never_panics() {
        // Every two-byte packet header, decoded against a small limit.
        for a in 0..=255u8 {
            for b in 0..=255u8 {
                let _ = decode_exact(&[a, b, 0x11, 0x22, 0x33], 16, 0);
            }
        }
    }
}
