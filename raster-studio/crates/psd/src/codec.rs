//! The four channel encodings, behind one pair of functions.
//!
//! Every channel in a `.psd` — layer channels, layer masks, and the merged
//! composite — is stored in one of four ways, selected by a `u16` immediately
//! before the data:
//!
//! | code | encoding |
//! |------|----------|
//! | 0 | raw, row-major, big-endian samples |
//! | 1 | RLE (PackBits) preceded by a per-row byte-count table |
//! | 2 | ZIP (zlib) |
//! | 3 | ZIP with per-row delta prediction |
//!
//! All four are lossless and must decode to identical bytes for the same image,
//! which is exactly what `four_encodings_agree_on_the_same_image` asserts.
//!
//! The merged composite differs from a layer channel in one structural way: its
//! compression code appears **once** for all channels, and in the RLE case a
//! **single** row-count table covers every row of every channel before any
//! packed data starts. Treating the composite as a sequence of independent
//! channels is the classic way to read a correct file as garbage, so
//! [`decode_merged`] and [`encode_merged`] are separate entry points rather
//! than a loop over [`decode_channel`].

use crate::bytes::{Cursor, Sink};
use crate::error::{PsdError, PsdResult};
use crate::header::Depth;
use crate::limits::Budget;
use crate::{packbits, zip};

/// How one channel's samples are stored.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize,
)]
pub enum Compression {
    Raw,
    #[default]
    Rle,
    Zip,
    ZipPrediction,
}

impl Compression {
    pub const ALL: [Compression; 4] = [
        Compression::Raw,
        Compression::Rle,
        Compression::Zip,
        Compression::ZipPrediction,
    ];

    pub const fn code(self) -> u16 {
        match self {
            Compression::Raw => 0,
            Compression::Rle => 1,
            Compression::Zip => 2,
            Compression::ZipPrediction => 3,
        }
    }

    pub fn from_code(code: u16) -> PsdResult<Self> {
        match code {
            0 => Ok(Compression::Raw),
            1 => Ok(Compression::Rle),
            2 => Ok(Compression::Zip),
            3 => Ok(Compression::ZipPrediction),
            other => Err(PsdError::UnsupportedCompression(other)),
        }
    }
}

/// The geometry of one channel: how many samples, how wide the rows are, and
/// how many bytes a sample takes.
///
/// Every size the decoders use comes from here, computed once with checked
/// arithmetic from dimensions that were already range-checked against
/// [`crate::limits::ReadOptions`]. Nothing downstream multiplies file-supplied
/// numbers again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelShape {
    pub width: usize,
    pub height: usize,
    pub depth: Depth,
}

impl ChannelShape {
    pub fn new(width: u32, height: u32, depth: Depth) -> Self {
        ChannelShape {
            width: width as usize,
            height: height as usize,
            depth,
        }
    }

    pub fn row_bytes(&self) -> PsdResult<usize> {
        self.width
            .checked_mul(self.depth.bytes_per_sample())
            .ok_or(PsdError::Overflow { what: "row bytes" })
    }

    /// Total decoded bytes for this channel.
    pub fn byte_len(&self) -> PsdResult<usize> {
        self.row_bytes()?
            .checked_mul(self.height)
            .ok_or(PsdError::Overflow {
                what: "channel byte length",
            })
    }

    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// Read the RLE byte-count table for `rows` rows.
fn read_rle_counts(cur: &mut Cursor<'_>, rows: usize) -> PsdResult<Vec<u32>> {
    let mut counts = Vec::with_capacity(rows.min(1 << 16));
    for _ in 0..rows {
        counts.push(u32::from(cur.u16()?));
    }
    Ok(counts)
}

/// Decode `rows` PackBits rows of `row_bytes` each, using an already-read table.
fn decode_rle_rows(
    cur: &mut Cursor<'_>,
    counts: &[u32],
    row_bytes: usize,
    out: &mut Vec<u8>,
) -> PsdResult<()> {
    for (row, &count) in counts.iter().enumerate() {
        let packed = cur.take(count as usize)?;
        let decoded = packbits::decode_exact(packed, row_bytes, row)?;
        out.extend_from_slice(&decoded);
    }
    Ok(())
}

/// A PackBits row count has to fit the `u16` the table stores it in.
///
/// It cannot for a very wide high-depth row: 30 000 samples of 32-bit data
/// pack to more than 65 535 bytes in the worst case. Photoshop uses ZIP for
/// those, and so should a caller — but the writer says so rather than
/// truncating the count and producing a file that decodes to noise.
fn rle_row_count(len: usize) -> PsdResult<u16> {
    u16::try_from(len).map_err(|_| {
        PsdError::InvalidDocument(format!(
            "a PackBits row packed to {len} bytes, which does not fit the 16-bit row-count \
             table; use ZIP compression for rows this wide"
        ))
    })
}

/// Decode one channel whose compression code has already been read.
///
/// `budget` is drawn against before the output buffer exists, so a channel that
/// declares more pixels than the read is allowed to produce costs a subtraction
/// rather than an allocation.
pub fn decode_channel(
    cur: &mut Cursor<'_>,
    compression: Compression,
    shape: ChannelShape,
    budget: &mut Budget,
) -> PsdResult<Vec<u8>> {
    let expected = shape.byte_len()?;
    budget.take(expected as u64)?;
    if expected == 0 {
        cur.skip_rest();
        return Ok(Vec::new());
    }
    let row_bytes = shape.row_bytes()?;
    match compression {
        Compression::Raw => Ok(cur.take(expected)?.to_vec()),
        Compression::Rle => {
            let counts = read_rle_counts(cur, shape.height)?;
            let mut out = Vec::with_capacity(expected);
            decode_rle_rows(cur, &counts, row_bytes, &mut out)?;
            Ok(out)
        }
        Compression::Zip | Compression::ZipPrediction => {
            // The zlib stream runs to the end of the channel's own bounded
            // section, so `peek_rest` here is already length-limited.
            let src = cur.peek_rest();
            let mut out = zip::inflate_exact(src, expected)?;
            cur.skip_rest();
            if compression == Compression::ZipPrediction {
                zip::unpredict(&mut out, shape.width, shape.height, shape.depth)?;
            }
            Ok(out)
        }
    }
}

/// Encode one channel. The returned bytes do **not** include the compression
/// code; the caller writes that.
pub fn encode_channel(
    data: &[u8],
    compression: Compression,
    shape: ChannelShape,
) -> PsdResult<Vec<u8>> {
    let expected = shape.byte_len()?;
    if data.len() != expected {
        return Err(PsdError::ChannelSizeMismatch {
            what: "channel being written",
            expected,
            actual: data.len(),
        });
    }
    if expected == 0 {
        return Ok(Vec::new());
    }
    let row_bytes = shape.row_bytes()?;
    match compression {
        Compression::Raw => Ok(data.to_vec()),
        Compression::Rle => {
            let mut counts = Vec::with_capacity(shape.height);
            let mut packed = Vec::with_capacity(data.len() / 2 + 16);
            for row in data.chunks(row_bytes) {
                let before = packed.len();
                packbits::encode_into(row, &mut packed);
                counts.push(rle_row_count(packed.len() - before)?);
            }
            let mut out = Vec::with_capacity(counts.len() * 2 + packed.len());
            for c in &counts {
                out.extend_from_slice(&c.to_be_bytes());
            }
            out.extend_from_slice(&packed);
            Ok(out)
        }
        Compression::Zip => zip::deflate(data),
        Compression::ZipPrediction => {
            let mut staged = data.to_vec();
            zip::predict(&mut staged, shape.width, shape.height, shape.depth)?;
            zip::deflate(&staged)
        }
    }
}

/// Decode the merged composite: one compression code, `channels` channels, and
/// in the RLE case one row-count table spanning all of them.
pub fn decode_merged(
    cur: &mut Cursor<'_>,
    compression: Compression,
    shape: ChannelShape,
    channels: usize,
    budget: &mut Budget,
) -> PsdResult<Vec<Vec<u8>>> {
    let expected = shape.byte_len()?;
    let total = (expected as u64)
        .checked_mul(channels as u64)
        .ok_or(PsdError::Overflow {
            what: "merged image byte length",
        })?;
    budget.take(total)?;
    if expected == 0 {
        cur.skip_rest();
        return Ok(vec![Vec::new(); channels]);
    }
    let row_bytes = shape.row_bytes()?;
    match compression {
        Compression::Rle => {
            let rows = shape
                .height
                .checked_mul(channels)
                .ok_or(PsdError::Overflow {
                    what: "merged image row count",
                })?;
            let counts = read_rle_counts(cur, rows)?;
            let mut out = Vec::with_capacity(channels);
            for c in 0..channels {
                let slice = &counts[c * shape.height..(c + 1) * shape.height];
                let mut chan = Vec::with_capacity(expected);
                decode_rle_rows(cur, slice, row_bytes, &mut chan)?;
                out.push(chan);
            }
            Ok(out)
        }
        Compression::Raw => {
            let mut out = Vec::with_capacity(channels);
            for _ in 0..channels {
                out.push(cur.take(expected)?.to_vec());
            }
            Ok(out)
        }
        Compression::Zip | Compression::ZipPrediction => {
            // Photoshop does not write a ZIP composite, but the code is legal
            // and other tools emit it: one zlib stream holding every channel.
            let src = cur.peek_rest();
            let mut all = zip::inflate_exact(src, expected * channels)?;
            cur.skip_rest();
            if compression == Compression::ZipPrediction {
                for chan in all.chunks_mut(expected) {
                    zip::unpredict(chan, shape.width, shape.height, shape.depth)?;
                }
            }
            Ok(all.chunks(expected).map(<[u8]>::to_vec).collect())
        }
    }
}

/// Write the merged composite, compression code included.
pub fn encode_merged(
    channels: &[Vec<u8>],
    compression: Compression,
    shape: ChannelShape,
    sink: &mut Sink,
) -> PsdResult<()> {
    let expected = shape.byte_len()?;
    for (i, chan) in channels.iter().enumerate() {
        if chan.len() != expected {
            return Err(PsdError::ChannelSizeMismatch {
                what: match i {
                    0 => "merged channel 0",
                    1 => "merged channel 1",
                    2 => "merged channel 2",
                    _ => "merged channel",
                },
                expected,
                actual: chan.len(),
            });
        }
    }
    sink.u16(compression.code());
    if expected == 0 {
        return Ok(());
    }
    let row_bytes = shape.row_bytes()?;
    match compression {
        Compression::Rle => {
            let mut counts: Vec<u16> = Vec::with_capacity(shape.height * channels.len());
            let mut packed = Vec::new();
            for chan in channels {
                for row in chan.chunks(row_bytes) {
                    let before = packed.len();
                    packbits::encode_into(row, &mut packed);
                    counts.push(rle_row_count(packed.len() - before)?);
                }
            }
            for c in counts {
                sink.u16(c);
            }
            sink.bytes(&packed);
        }
        Compression::Raw => {
            for chan in channels {
                sink.bytes(chan);
            }
        }
        Compression::Zip | Compression::ZipPrediction => {
            let mut all = Vec::with_capacity(expected * channels.len());
            for chan in channels {
                all.extend_from_slice(chan);
            }
            if compression == Compression::ZipPrediction {
                for chan in all.chunks_mut(expected) {
                    zip::predict(chan, shape.width, shape.height, shape.depth)?;
                }
            }
            sink.bytes(&zip::deflate(&all)?);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::Budget;

    fn image(width: usize, height: usize, depth: Depth) -> Vec<u8> {
        let n = width * height * depth.bytes_per_sample();
        (0..n)
            .map(|i| {
                // Deliberately mixed: long flat runs (which favour RLE) next to
                // noise (which does not), so no encoding gets an easy ride.
                if (i / 23) % 3 == 0 {
                    0xC7
                } else {
                    ((i * 61 + i / 7) % 253) as u8
                }
            })
            .collect()
    }

    #[test]
    fn four_encodings_agree_on_the_same_image_at_every_depth() {
        for depth in [Depth::Eight, Depth::Sixteen, Depth::ThirtyTwo] {
            let shape = ChannelShape {
                width: 37,
                height: 19,
                depth,
            };
            let original = image(shape.width, shape.height, depth);
            assert_eq!(original.len(), shape.byte_len().unwrap());

            for compression in Compression::ALL {
                let encoded = encode_channel(&original, compression, shape).unwrap();
                let mut budget = Budget::new(1 << 20);
                let mut cur = Cursor::new(&encoded);
                let decoded = decode_channel(&mut cur, compression, shape, &mut budget).unwrap();
                assert_eq!(
                    decoded, original,
                    "{compression:?} at {depth:?} did not round-trip"
                );
            }
        }
    }

    #[test]
    fn compression_codes_are_the_ones_the_format_defines() {
        assert_eq!(Compression::Raw.code(), 0);
        assert_eq!(Compression::Rle.code(), 1);
        assert_eq!(Compression::Zip.code(), 2);
        assert_eq!(Compression::ZipPrediction.code(), 3);
        for c in Compression::ALL {
            assert_eq!(Compression::from_code(c.code()).unwrap(), c);
        }
        assert!(matches!(
            Compression::from_code(4).unwrap_err(),
            PsdError::UnsupportedCompression(4)
        ));
    }

    /// The crate's headline claim is not "an over-large channel is refused" but
    /// "it is refused *before* the buffer that would hold it is reserved". A
    /// plain `unwrap_err` cannot tell those apart — both return the same error —
    /// so the refusal is measured, not just observed. Moving the allocation
    /// ahead of `budget.take` in [`decode_channel`] makes the byte count jump
    /// from a few dozen to 64 MiB and this test goes red.
    #[test]
    fn a_channel_larger_than_the_budget_is_refused_before_it_allocates() {
        let shape = ChannelShape {
            width: 4096,
            height: 4096,
            depth: Depth::ThirtyTwo,
        };
        // 64 MiB declared, 1 KiB allowed, and only four bytes of input exist.
        let mut budget = Budget::new(1024);
        let data = [0u8; 4];
        let mut cur = Cursor::new(&data);
        let (res, allocated) = crate::probe::bytes_allocated_by(|| {
            decode_channel(&mut cur, Compression::Raw, shape, &mut budget)
        });
        let err = res.unwrap_err();
        assert!(matches!(err, PsdError::BudgetExhausted { .. }), "{err}");
        assert!(
            allocated < 4096,
            "the refusal allocated {allocated} bytes; it must not reserve the \
             {} byte channel it is refusing",
            shape.byte_len().unwrap()
        );
    }

    #[test]
    fn an_rle_count_table_that_overruns_the_section_is_an_error() {
        let shape = ChannelShape {
            width: 8,
            height: 4,
            depth: Depth::Eight,
        };
        // Row counts claim 60 000 bytes per row; the section has none.
        let mut s = Sink::new();
        for _ in 0..4 {
            s.u16(60_000);
        }
        let buf = s.into_inner();
        let mut budget = Budget::new(1 << 20);
        let mut cur = Cursor::new(&buf);
        assert!(matches!(
            decode_channel(&mut cur, Compression::Rle, shape, &mut budget).unwrap_err(),
            PsdError::Truncated { .. }
        ));
    }

    #[test]
    fn a_zero_area_channel_decodes_to_nothing_without_dividing_by_zero() {
        let shape = ChannelShape {
            width: 0,
            height: 5,
            depth: Depth::Eight,
        };
        for compression in Compression::ALL {
            assert!(encode_channel(&[], compression, shape).unwrap().is_empty());
            let mut budget = Budget::new(1 << 20);
            let mut cur = Cursor::new(&[]);
            assert!(decode_channel(&mut cur, compression, shape, &mut budget)
                .unwrap()
                .is_empty());
        }
    }

    #[test]
    fn encoding_a_channel_of_the_wrong_size_is_refused() {
        let shape = ChannelShape {
            width: 4,
            height: 4,
            depth: Depth::Eight,
        };
        assert!(matches!(
            encode_channel(&[0u8; 15], Compression::Raw, shape).unwrap_err(),
            PsdError::ChannelSizeMismatch {
                expected: 16,
                actual: 15,
                ..
            }
        ));
    }

    #[test]
    fn merged_round_trips_in_every_encoding_with_one_shared_row_table() {
        let shape = ChannelShape {
            width: 13,
            height: 9,
            depth: Depth::Eight,
        };
        let channels: Vec<Vec<u8>> = (0..4)
            .map(|c| {
                let mut v = image(shape.width, shape.height, Depth::Eight);
                for b in v.iter_mut() {
                    *b = b.wrapping_add(c * 40);
                }
                v
            })
            .collect();

        for compression in Compression::ALL {
            let mut sink = Sink::new();
            encode_merged(&channels, compression, shape, &mut sink).unwrap();
            let buf = sink.into_inner();
            let mut cur = Cursor::new(&buf);
            let code = cur.u16().unwrap();
            assert_eq!(code, compression.code());
            let mut budget = Budget::new(1 << 20);
            let got = decode_merged(
                &mut cur,
                Compression::from_code(code).unwrap(),
                shape,
                4,
                &mut budget,
            )
            .unwrap();
            assert_eq!(got, channels, "{compression:?}");
        }
    }

    /// As above, measured rather than observed: 56 MiB declared, 1 MiB allowed,
    /// eight bytes of input. The refusal must cost a subtraction.
    #[test]
    fn a_merged_image_declaring_more_channels_than_fit_the_budget_is_refused() {
        let shape = ChannelShape {
            width: 1000,
            height: 1000,
            depth: Depth::Eight,
        };
        let mut budget = Budget::new(1 << 20);
        let data = [0u8; 8];
        let mut cur = Cursor::new(&data);
        let (res, allocated) = crate::probe::bytes_allocated_by(|| {
            decode_merged(&mut cur, Compression::Raw, shape, 56, &mut budget)
        });
        assert!(matches!(res.unwrap_err(), PsdError::BudgetExhausted { .. }));
        assert!(
            allocated < 4096,
            "the refusal allocated {allocated} bytes; it must not reserve the \
             composite it is refusing"
        );
    }
}
