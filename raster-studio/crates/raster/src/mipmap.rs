//! Mip level math, a linear-light premultiplied 2x downsample, and mip-chain
//! building.
//!
//! Every raster source and generated composite gets a mip chain so the render
//! graph can pick a level appropriate to the current zoom (avoids uploading /
//! sampling full-resolution tiles when zoomed out).
//!
//! # Why the filter is not a plain average of the stored bytes
//! Stored RGBA8 is sRGB-encoded with *straight* alpha. Averaging those bytes
//! directly is wrong twice over:
//! * averaging gamma-encoded values darkens every level (the mean of the
//!   encoded values is below the encoding of the mean);
//! * averaging straight-alpha RGB lets the color of fully transparent pixels
//!   bleed into the result.
//!
//! So the filter converts to linear, premultiplies, averages, un-premultiplies
//! and re-encodes. Alpha carries no transfer function and is averaged directly.
//!
//! # Odd dimensions
//! An even axis of `2n` pixels is a plain 2-tap box filter. An odd axis of
//! `2n + 1` pixels cannot be split into equal pairs, so it uses the standard
//! 3-tap polyphase kernel instead ([`axis_taps`]). Dropping the trailing
//! row/column would be cheaper but would discard 5 of the 9 source pixels of a
//! 3x3 image and shift content half a pixel toward the top-left at *every* odd
//! level, an error that accumulates down the chain.

/// Failures from the downsample / mip-chain functions.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MipError {
    #[error("cannot downsample a {width}x{height} image: both dimensions must be non-zero")]
    EmptyImage { width: u32, height: u32 },
    #[error("pixel buffer too small: expected {expected} bytes for {width}x{height}, got {got}")]
    BadLength {
        width: u32,
        height: u32,
        expected: usize,
        got: usize,
    },
    /// `width * height * 4` does not fit in a `usize`, so no buffer on this
    /// platform can hold the image and the length check itself would overflow.
    #[error("a {width}x{height} RGBA8 image cannot be addressed on this platform")]
    TooLarge { width: u32, height: u32 },
}

/// Byte length of a packed RGBA8 image, or `MipError::TooLarge` when the
/// product does not fit in a `usize`.
///
/// Computing this with plain `*` panics in debug and wraps in release for large
/// dimensions, which would let a far-too-short buffer pass the length check.
fn rgba8_len(width: u32, height: u32) -> Result<usize, MipError> {
    (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or(MipError::TooLarge { width, height })
}

/// Number of mip levels for an image of `width` x `height`, down to 1x1.
///
/// `floor(log2(max(width, height, 1))) + 1`, i.e. the level count a chain built
/// by repeated floor-halving actually produces.
pub fn level_count(width: u32, height: u32) -> u8 {
    let max_dim = width.max(height).max(1);
    (32 - (max_dim.leading_zeros())) as u8
}

/// Dimensions of `level` given base dimensions (each level halves, min 1).
///
/// `level` is clamped to 31: shifting a `u32` by 32 or more is undefined in
/// Rust and used to panic here, and every level past 31 is 1x1 anyway.
pub fn level_dimensions(width: u32, height: u32, level: u8) -> (u32, u32) {
    let shift = level.min(31) as u32;
    ((width >> shift).max(1), (height >> shift).max(1))
}

/// Encode a linear channel value back to an sRGB byte, rounded (not truncated).
///
/// Truncating here biases every level down by up to one code value and makes a
/// flat colour drift darker as the chain descends; rounding round-trips every
/// one of the 256 flat sRGB values exactly.
fn encode_channel(linear: f32) -> u8 {
    let s = color::linear_to_srgb(linear.clamp(0.0, 1.0));
    (s * 255.0).round().clamp(0.0, 255.0) as u8
}

/// Source samples along one axis that contribute to output index `d`, as
/// `(taps, count)` where each tap is `(source index, weight)`.
///
/// Three cases, all with weights summing to 1:
/// * `src_extent == 1`: a single tap of weight 1. The axis is preserved instead
///   of collapsing to zero, so `dst_extent` is also 1.
/// * `src_extent` even (`2n`, `dst_extent == n`): two taps at `2d` and `2d + 1`
///   of weight 1/2 — a plain box filter.
/// * `src_extent` odd (`2n + 1`, `dst_extent == n`): three taps at `2d`,
///   `2d + 1`, `2d + 2` weighted `(n - d)`, `n`, `(d + 1)` over `2n + 1`.
///
/// The odd case is the property that matters: summed over every `d`, each of the
/// `2n + 1` source pixels receives the same total weight `n / (2n + 1)`, so no
/// source pixel is dropped, and the per-output sampling centroids are symmetric
/// about the axis centre, so no half-pixel shift accumulates down a mip chain.
///
/// The largest source index produced is `src_extent - 1` in every case, so a
/// caller that sized its buffer for `src_extent` cannot be indexed out of
/// bounds.
fn axis_taps(d: u32, src_extent: u32, dst_extent: u32) -> ([(u32, f32); 3], usize) {
    if src_extent == 1 {
        return ([(0, 1.0), (0, 0.0), (0, 0.0)], 1);
    }
    if src_extent % 2 == 0 {
        return ([(d * 2, 0.5), (d * 2 + 1, 0.5), (0, 0.0)], 2);
    }
    let n = dst_extent as f32;
    let total = 2.0 * n + 1.0;
    let df = d as f32;
    (
        [
            (d * 2, (n - df) / total),
            (d * 2 + 1, n / total),
            (d * 2 + 2, (df + 1.0) / total),
        ],
        3,
    )
}

/// Downsample RGBA8 by 2x. Returns `(pixels, width, height)` of the half-size
/// image, each dimension `max(1, floor(d / 2))`.
///
/// Filtering happens in linear, premultiplied space and results are rounded, so
/// levels neither darken nor pick up color from transparent pixels. Even axes
/// are box-filtered; odd axes use the 3-tap kernel of [`axis_taps`], which
/// covers the whole source extent — no row or column is discarded and no
/// half-pixel shift accumulates. A 1-pixel axis is preserved rather than
/// collapsing to zero.
///
/// Errors when either dimension is zero, when `width * height * 4` exceeds
/// `usize`, or when `src` is shorter than `width * height * 4` bytes. It never
/// panics, for any dimensions and any slice length.
pub fn downsample_rgba8_2x(
    src: &[u8],
    width: u32,
    height: u32,
) -> Result<(Vec<u8>, u32, u32), MipError> {
    if width == 0 || height == 0 {
        return Err(MipError::EmptyImage { width, height });
    }
    let expected = rgba8_len(width, height)?;
    if src.len() < expected {
        return Err(MipError::BadLength {
            width,
            height,
            expected,
            got: src.len(),
        });
    }

    let lut = &color::SRGB8_TO_LINEAR;
    let dw = (width / 2).max(1);
    let dh = (height / 2).max(1);
    let mut out = vec![0u8; dw as usize * dh as usize * 4];

    for y in 0..dh {
        let (row_taps, row_n) = axis_taps(y, height, dh);
        for x in 0..dw {
            let (col_taps, col_n) = axis_taps(x, width, dw);
            // Premultiplied linear RGB + straight alpha, all in 0..=1.
            let mut acc = [0.0f32; 4];
            let mut weight_sum = 0.0f32;
            for &(sy, wy) in &row_taps[..row_n] {
                for &(sx, wx) in &col_taps[..col_n] {
                    let w = wx * wy;
                    let i = ((sy as usize * width as usize) + sx as usize) * 4;
                    let a = src[i + 3] as f32 / 255.0;
                    let aw = a * w;
                    acc[0] += lut[src[i] as usize] * aw;
                    acc[1] += lut[src[i + 1] as usize] * aw;
                    acc[2] += lut[src[i + 2] as usize] * aw;
                    acc[3] += aw;
                    weight_sum += w;
                }
            }
            // Normalising by the realised weight sum, rather than trusting it to
            // be exactly 1.0, keeps the even case bit-exact (the sum really is
            // 1.0 there) and absorbs f32 rounding in the odd case.
            let inv = 1.0 / weight_sum;
            let avg = [acc[0] * inv, acc[1] * inv, acc[2] * inv, acc[3] * inv];
            let straight = color::unpremultiply(avg);

            let o = ((y as usize * dw as usize) + x as usize) * 4;
            out[o] = encode_channel(straight[0]);
            out[o + 1] = encode_channel(straight[1]);
            out[o + 2] = encode_channel(straight[2]);
            out[o + 3] = (straight[3] * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
    Ok((out, dw, dh))
}

/// One level of a [`MipChain`]: packed RGBA8 plus its dimensions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MipLevel {
    pub width: u32,
    pub height: u32,
    /// Row-major RGBA8, `width * height * 4` bytes.
    pub rgba8: Vec<u8>,
}

/// A full mip pyramid, level 0 (the original) through 1x1.
///
/// `levels().len()` always equals [`level_count`] of the base dimensions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MipChain {
    levels: Vec<MipLevel>,
}

impl MipChain {
    /// Build every level from a packed RGBA8 image, halving down to 1x1.
    ///
    /// Level 0 borrows nothing — it holds a copy of `src` truncated to exactly
    /// `width * height * 4` bytes. Same error conditions as
    /// [`downsample_rgba8_2x`].
    pub fn build(src: &[u8], width: u32, height: u32) -> Result<Self, MipError> {
        if width == 0 || height == 0 {
            return Err(MipError::EmptyImage { width, height });
        }
        let expected = rgba8_len(width, height)?;
        if src.len() < expected {
            return Err(MipError::BadLength {
                width,
                height,
                expected,
                got: src.len(),
            });
        }

        let mut levels = vec![MipLevel {
            width,
            height,
            rgba8: src[..expected].to_vec(),
        }];
        let (mut w, mut h) = (width, height);
        while w > 1 || h > 1 {
            let prev = levels.last().expect("level 0 was pushed above");
            let (pixels, nw, nh) = downsample_rgba8_2x(&prev.rgba8, w, h)?;
            levels.push(MipLevel {
                width: nw,
                height: nh,
                rgba8: pixels,
            });
            w = nw;
            h = nh;
        }
        Ok(Self { levels })
    }

    /// All levels, index 0 being full resolution.
    pub fn levels(&self) -> &[MipLevel] {
        &self.levels
    }

    /// One level, or `None` past the end of the chain.
    pub fn level(&self, level: u8) -> Option<&MipLevel> {
        self.levels.get(level as usize)
    }

    /// Number of levels in the chain.
    pub fn len(&self) -> usize {
        self.levels.len()
    }

    /// Always false — a chain has at least level 0. Present so `len` does not
    /// trip the "has len, no is_empty" lint at call sites.
    pub fn is_empty(&self) -> bool {
        self.levels.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(out: &[u8], i: usize) -> [u8; 4] {
        [out[i * 4], out[i * 4 + 1], out[i * 4 + 2], out[i * 4 + 3]]
    }

    #[test]
    fn level_count_powers_of_two() {
        assert_eq!(level_count(256, 256), 9); // 256,128,...,1
        assert_eq!(level_count(1, 1), 1);
        assert_eq!(level_count(4096, 2048), 13);
    }

    #[test]
    fn level_dimensions_clamps_absurd_levels() {
        assert_eq!(level_dimensions(1024, 512, 0), (1024, 512));
        assert_eq!(level_dimensions(1024, 512, 3), (128, 64));
        // Level >= 32 used to shift a u32 by >= 32 and panic.
        assert_eq!(level_dimensions(1024, 512, 32), (1, 1));
        assert_eq!(level_dimensions(1024, 512, 255), (1, 1));
    }

    #[test]
    fn downsample_halves_and_averages() {
        // 2x2 solid gray -> 1x1 same gray, unchanged by the linear round trip.
        let src = vec![100u8; 16];
        let (out, w, h) = downsample_rgba8_2x(&src, 2, 2).unwrap();
        assert_eq!((w, h), (1, 1));
        assert_eq!(out, vec![100, 100, 100, 100]);
    }

    #[test]
    fn transparent_pixels_do_not_bleed_color() {
        // One opaque white pixel, three fully transparent black ones.
        // A straight-alpha average gives rgb ~64 (a dark grey halo);
        // premultiplied filtering keeps rgb at white and only alpha drops.
        #[rustfmt::skip]
        let src: Vec<u8> = vec![
            255, 255, 255, 255,   0, 0, 0, 0,
              0,   0,   0,   0,   0, 0, 0, 0,
        ];
        let (out, _, _) = downsample_rgba8_2x(&src, 2, 2).unwrap();
        let p = px(&out, 0);
        assert!(
            p[0] >= 254 && p[1] >= 254 && p[2] >= 254,
            "color must not be pulled toward transparent pixels, got {p:?}"
        );
        assert_eq!(p[3], 64, "alpha averages 255/4 = 63.75, rounded");
    }

    #[test]
    fn half_transparent_checkerboard_keeps_its_color() {
        // 4x4 checkerboard: opaque pure red alternating with transparent green.
        // Naively averaged, the reds get diluted and green leaks in.
        let mut src = Vec::new();
        for y in 0..4u32 {
            for x in 0..4u32 {
                if (x + y) % 2 == 0 {
                    src.extend_from_slice(&[200, 0, 0, 255]);
                } else {
                    src.extend_from_slice(&[0, 255, 0, 0]);
                }
            }
        }
        let (out, w, h) = downsample_rgba8_2x(&src, 4, 4).unwrap();
        assert_eq!((w, h), (2, 2));
        for i in 0..4 {
            let p = px(&out, i);
            assert_eq!(p[1], 0, "green from transparent pixels leaked: {p:?}");
            assert_eq!(p[2], 0);
            assert!(
                p[0] >= 199,
                "red must survive at full strength, got {}",
                p[0]
            );
            assert_eq!(
                p[3], 128,
                "half the samples are opaque: 255/2 rounds to 128"
            );
        }
    }

    #[test]
    fn gamma_checkerboard_does_not_darken() {
        // Opaque black/white checkerboard. Averaging the encoded bytes gives
        // ~128; averaging in linear space gives linear 0.5 -> sRGB ~188.
        #[rustfmt::skip]
        let src: Vec<u8> = vec![
            0, 0, 0, 255,   255, 255, 255, 255,
            255, 255, 255, 255,   0, 0, 0, 255,
        ];
        let (out, _, _) = downsample_rgba8_2x(&src, 2, 2).unwrap();
        let p = px(&out, 0);
        for c in &p[..3] {
            assert!(
                (187..=189).contains(c),
                "linear-space mean of black and white is ~188, got {c}"
            );
        }
        assert_eq!(p[3], 255);
    }

    #[test]
    fn averaging_rounds_instead_of_truncating() {
        // Alpha carries no transfer function, so it exercises rounding alone:
        // (0 + 0 + 255 + 255) / 4 = 127.5, which must round to 128, not 127.
        #[rustfmt::skip]
        let src: Vec<u8> = vec![
            10, 10, 10, 0,     10, 10, 10, 0,
            10, 10, 10, 255,   10, 10, 10, 255,
        ];
        let (out, _, _) = downsample_rgba8_2x(&src, 2, 2).unwrap();
        assert_eq!(px(&out, 0)[3], 128);
    }

    #[test]
    fn every_flat_rgb_value_survives_a_level_unchanged() {
        // The RGB rounding path. A uniform block has a mathematically exact
        // answer -- itself -- so any bias in the encode step shows up as an
        // off-by-one. Truncating instead of rounding regresses dozens of the
        // 256 code values (11 -> 10, 63 -> 62, 127 -> 126, 255 -> 254, ...).
        for v in 0u8..=255 {
            let px_in = [v, 255 - v, v.wrapping_mul(7), 255];
            let mut src = Vec::with_capacity(16);
            for _ in 0..4 {
                src.extend_from_slice(&px_in);
            }
            let (out, w, h) = downsample_rgba8_2x(&src, 2, 2).unwrap();
            assert_eq!((w, h), (1, 1));
            assert_eq!(
                px(&out, 0),
                px_in,
                "flat block of {px_in:?} must downsample to itself"
            );
        }
    }

    #[test]
    fn rounding_pins_the_known_off_by_one_values() {
        // Spot checks that fail individually under truncation, kept explicit so
        // a regression names the value rather than a loop index.
        for &v in &[11u8, 12, 31, 63, 97, 127, 187, 255] {
            let src = vec![v; 16];
            let (out, _, _) = downsample_rgba8_2x(&src, 2, 2).unwrap();
            assert_eq!(px(&out, 0)[0], v, "grey {v} must round-trip exactly");
        }
    }

    #[test]
    fn absurd_dimensions_are_an_error_not_an_arithmetic_overflow() {
        // width * height * 4 does not fit in a usize; computing it unchecked
        // panicked here before the length check could run.
        assert_eq!(
            downsample_rgba8_2x(&[], u32::MAX, u32::MAX),
            Err(MipError::TooLarge {
                width: u32::MAX,
                height: u32::MAX
            })
        );
        assert_eq!(
            MipChain::build(&[], u32::MAX, u32::MAX),
            Err(MipError::TooLarge {
                width: u32::MAX,
                height: u32::MAX
            })
        );
        // A merely huge-but-addressable request still reports a short buffer.
        assert!(matches!(
            downsample_rgba8_2x(&[0u8; 8], u32::MAX, 1),
            Err(MipError::BadLength { .. })
        ));
    }

    #[test]
    fn zero_dimensions_are_an_error_not_a_panic() {
        assert_eq!(
            downsample_rgba8_2x(&[], 0, 4),
            Err(MipError::EmptyImage {
                width: 0,
                height: 4
            })
        );
        assert_eq!(
            downsample_rgba8_2x(&[], 4, 0),
            Err(MipError::EmptyImage {
                width: 4,
                height: 0
            })
        );
        assert!(matches!(
            MipChain::build(&[], 0, 0),
            Err(MipError::EmptyImage { .. })
        ));
    }

    #[test]
    fn short_buffer_is_an_error_not_an_out_of_bounds_index() {
        // 4x4 needs 64 bytes; hand it 20.
        let err = downsample_rgba8_2x(&[0u8; 20], 4, 4).unwrap_err();
        assert_eq!(
            err,
            MipError::BadLength {
                width: 4,
                height: 4,
                expected: 64,
                got: 20
            }
        );
        assert!(matches!(
            MipChain::build(&[0u8; 20], 4, 4),
            Err(MipError::BadLength { .. })
        ));
    }

    #[test]
    fn single_pixel_and_single_row_survive() {
        let (out, w, h) = downsample_rgba8_2x(&[1, 2, 3, 255], 1, 1).unwrap();
        assert_eq!((w, h), (1, 1));
        assert_eq!(out, vec![1, 2, 3, 255]);

        // A 1-pixel axis is preserved alongside a shrinking one.
        let src = vec![9u8; 16];
        let (out, w, h) = downsample_rgba8_2x(&src, 1, 4).unwrap();
        assert_eq!((w, h), (1, 2));
        assert_eq!(out, vec![9u8; 8]);
    }

    #[test]
    fn axis_taps_cover_every_source_pixel_with_equal_weight() {
        // The kernel contract, stated directly: per output pixel the weights
        // sum to 1 (so a flat color cannot drift), and summed over the axis
        // every source pixel receives the same total weight (so none is dropped
        // and none is double-counted). Odd and even extents both, including the
        // 1-pixel degenerate case.
        for src_extent in 1u32..=33 {
            let dst_extent = (src_extent / 2).max(1);
            let mut per_source = vec![0.0f32; src_extent as usize];

            for d in 0..dst_extent {
                let (taps, n) = axis_taps(d, src_extent, dst_extent);
                let sum: f32 = taps[..n].iter().map(|&(_, w)| w).sum();
                assert!(
                    (sum - 1.0).abs() < 1e-6,
                    "weights for output {d} of a {src_extent}-pixel axis sum to {sum}, not 1"
                );
                for &(s, w) in &taps[..n] {
                    assert!(
                        s < src_extent,
                        "tap {s} is outside a {src_extent}-pixel axis"
                    );
                    assert!(w > 0.0, "a counted tap must carry weight");
                    per_source[s as usize] += w;
                }
            }

            let first = per_source[0];
            assert!(first > 0.0);
            for (i, &w) in per_source.iter().enumerate() {
                assert!(
                    (w - first).abs() < 1e-6,
                    "source pixel {i} of {src_extent} receives {w} but pixel 0 receives {first}"
                );
            }
        }
    }

    #[test]
    fn odd_axis_does_not_discard_the_trailing_column() {
        // 3x1 of [black, black, WHITE]. A filter that halves by dropping the
        // trailing column averages only the two blacks and reports pure black,
        // throwing the white pixel away. The 3-tap kernel weights all three
        // equally: linear 1/3 encodes to sRGB ~156.
        #[rustfmt::skip]
        let src: Vec<u8> = vec![
            0, 0, 0, 255,   0, 0, 0, 255,   255, 255, 255, 255,
        ];
        let (out, w, h) = downsample_rgba8_2x(&src, 3, 1).unwrap();
        assert_eq!((w, h), (1, 1));
        let p = px(&out, 0);
        assert_ne!(
            &p[..3],
            &[0, 0, 0],
            "the trailing column must contribute, got {p:?}"
        );
        for c in &p[..3] {
            assert!(
                (150..=162).contains(c),
                "linear mean of (0, 0, 1) is 1/3, which encodes to ~156, got {c}"
            );
        }
        assert_eq!(p[3], 255);
    }

    #[test]
    fn odd_axis_weights_every_source_pixel_equally() {
        // 5x1 ramp. Under a drop-the-trailing-column filter the 255 column is
        // never read, so the mean of the output is pulled down; here every
        // column contributes, so the two output pixels between them account for
        // the whole ramp and the brighter half stays brighter.
        #[rustfmt::skip]
        let src: Vec<u8> = vec![
            0, 0, 0, 255,   60, 60, 60, 255,   120, 120, 120, 255,
            180, 180, 180, 255,   255, 255, 255, 255,
        ];
        let (out, w, h) = downsample_rgba8_2x(&src, 5, 1).unwrap();
        assert_eq!((w, h), (2, 1));
        let (a, b) = (px(&out, 0), px(&out, 1));
        assert!(
            b[0] > 180,
            "the right output must carry the 255 column, got {b:?}"
        );
        assert!(a[0] < b[0], "the ramp must stay monotonic: {a:?} then {b:?}");
    }

    #[test]
    fn odd_axis_does_not_shift_content() {
        // A horizontally symmetric 5x1 image must downsample to a horizontally
        // symmetric 2x1 image. A filter whose window is centred at source 0.5
        // instead of the true output centre breaks the symmetry (it would
        // average [10, 40] and [90, 40]), which is exactly the half-pixel shift
        // that accumulates over a mip chain.
        #[rustfmt::skip]
        let src: Vec<u8> = vec![
            10, 10, 10, 255,   40, 40, 40, 255,   90, 90, 90, 255,
            40, 40, 40, 255,   10, 10, 10, 255,
        ];
        let (out, _, _) = downsample_rgba8_2x(&src, 5, 1).unwrap();
        assert_eq!(px(&out, 0), px(&out, 1), "a mirrored input must mirror out");

        // The same claim on the vertical axis, so a transposed bug is caught.
        let (out, w, h) = downsample_rgba8_2x(&src, 1, 5).unwrap();
        assert_eq!((w, h), (1, 2));
        assert_eq!(px(&out, 0), px(&out, 1));
    }

    #[test]
    fn odd_axis_preserves_a_flat_color() {
        // The 3-tap weights must sum to 1: if they did not, a uniform block
        // would drift brighter or darker at every odd level.
        for &(w, h) in &[(3u32, 1u32), (1, 3), (5, 7), (9, 9), (3, 3)] {
            let mut src = Vec::new();
            for _ in 0..(w * h) {
                src.extend_from_slice(&[70, 140, 210, 255]);
            }
            let (out, _, _) = downsample_rgba8_2x(&src, w, h).unwrap();
            for p in out.chunks_exact(4) {
                assert_eq!(p, [70, 140, 210, 255], "flat color drifted at {w}x{h}");
            }
        }
    }

    #[test]
    fn mip_chain_reaches_one_by_one() {
        let (w, h) = (8u32, 8u32);
        let chain = MipChain::build(&vec![128u8; (w * h * 4) as usize], w, h).unwrap();
        let dims: Vec<_> = chain.levels().iter().map(|l| (l.width, l.height)).collect();
        assert_eq!(dims, vec![(8, 8), (4, 4), (2, 2), (1, 1)]);
        assert_eq!(chain.len() as u8, level_count(w, h));
        assert!(!chain.is_empty());
    }

    #[test]
    fn mip_chain_length_matches_level_count_for_odd_and_oblong_sizes() {
        for &(w, h) in &[
            (1u32, 1u32),
            (1, 7),
            (3, 1),
            (5, 5),
            (7, 3),
            (100, 6),
            (37, 129),
            (64, 1),
        ] {
            let chain = MipChain::build(&vec![64u8; (w as usize * h as usize) * 4], w, h).unwrap();
            assert_eq!(
                chain.len() as u8,
                level_count(w, h),
                "chain length disagrees with level_count for {w}x{h}"
            );
            let last = chain.levels().last().unwrap();
            assert_eq!((last.width, last.height), (1, 1), "for {w}x{h}");
            for (i, l) in chain.levels().iter().enumerate() {
                assert_eq!(
                    (l.width, l.height),
                    level_dimensions(w, h, i as u8),
                    "level {i} of {w}x{h}"
                );
                assert_eq!(l.rgba8.len(), l.width as usize * l.height as usize * 4);
            }
        }
    }

    #[test]
    fn mip_chain_preserves_a_flat_color_at_every_level() {
        let (w, h) = (16u32, 16u32);
        let mut src = Vec::new();
        for _ in 0..(w * h) {
            src.extend_from_slice(&[30, 90, 200, 255]);
        }
        let chain = MipChain::build(&src, w, h).unwrap();
        for l in chain.levels() {
            for p in l.rgba8.chunks_exact(4) {
                assert_eq!(
                    p,
                    [30, 90, 200, 255],
                    "flat color drifted at {}x{}",
                    l.width,
                    l.height
                );
            }
        }
        assert_eq!(chain.level(0).unwrap().rgba8, src);
        assert!(chain.level(200).is_none());
    }

    #[test]
    fn mip_chain_of_fully_transparent_image_stays_transparent() {
        let chain = MipChain::build(&[0u8; 4 * 4 * 4], 4, 4).unwrap();
        for l in chain.levels() {
            assert!(l.rgba8.iter().all(|&b| b == 0));
        }
    }
}
