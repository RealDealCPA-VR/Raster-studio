//! W10-H: 32 bits per channel — `f32` layer tiles.
//!
//! Image ▸ Mode ▸ 32 Bits/Channel stores every raster layer tile as
//! [`crate::PixelFormat::RgbaF32`]: four native-endian `f32` samples a pixel,
//! straight alpha, colour **in the document's encoding** (the same transfer
//! curve the 8- and 16-bit tiles use, so `1.0` is the 8-bit code 255), with
//! no upper bound on the colour channels: a value above `1.0` is HDR headroom
//! and is kept by every `f32` route. Photoshop's 32-bit mode stores linear
//! light instead; keeping the encoding means a 32-bit tile narrows to 16 or 8
//! bits by scaling alone, exactly like a 16-bit one, wherever it is read.
//!
//! The three depths are told apart by tile length, as the compositor does:
//! [`RGBAF32_TILE_BYTES`] is twice an RGBA16 tile.
//!
//! * 8/16 → 32 is lossless (`code / 255` or `code / 65535`).
//! * 32 → 16 clips to `0..=1` and rounds to the nearest code; 32 → 8 goes
//!   through 16 and may dither the colour channels
//!   ([`crate::depth::narrow_rgba16_tile`]). Values above `1.0` clip: this
//!   build has no HDR Toning on the way down.

use crate::depth::{
    narrow_rgba16_tile, narrow_sample, widen_sample, DepthSample, RGBA16_TILE_BYTES,
    RGBA8_TILE_BYTES,
};
use crate::format::PixelFormat;
use crate::tile::Tile;

/// Bytes in an RGBA `f32` layer tile.
pub const RGBAF32_TILE_BYTES: usize = Tile::byte_len(PixelFormat::RgbaF32);

/// Whether `bytes` is a 32-bit (`f32`) layer colour tile.
pub fn is_rgbaf32_tile(bytes: &[u8]) -> bool {
    bytes.len() == RGBAF32_TILE_BYTES
}

/// Pack `f32` samples into tile bytes: native-endian, four bytes a sample.
pub fn rgbaf32_to_tile_bytes(samples: &[f32]) -> Vec<u8> {
    samples.iter().flat_map(|v| v.to_ne_bytes()).collect()
}

/// The inverse of [`rgbaf32_to_tile_bytes`]. Trailing bytes short of a whole
/// sample are ignored.
pub fn tile_bytes_to_rgbaf32(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_ne_bytes(*b))
        .collect()
}

/// One `f32` sample as the nearest 16-bit code: clipped into `0..=1` (a
/// NaN is 0), then rounded.
pub fn f32_to_code16(v: f32) -> u16 {
    if v.is_nan() {
        return 0;
    }
    (v.clamp(0.0, 1.0) * 65_535.0).round() as u16
}

/// A layer colour tile's samples as `f32`, whatever depth it is stored at:
/// an `f32` tile as it is, an RGBA16 or RGBA8 tile scaled into `0..=1`
/// (lossless). `None` for anything that is not a colour tile.
pub fn rgbaf32_samples(bytes: &[u8]) -> Option<Vec<f32>> {
    match bytes.len() {
        RGBAF32_TILE_BYTES => Some(tile_bytes_to_rgbaf32(bytes)),
        RGBA16_TILE_BYTES => Some(
            crate::tile_bytes_to_rgba16(bytes)
                .into_iter()
                .map(|c| f32::from(c) / 65_535.0)
                .collect(),
        ),
        RGBA8_TILE_BYTES => Some(bytes.iter().map(|c| f32::from(*c) / 255.0).collect()),
        _ => None,
    }
}

/// Widen an RGBA8 or RGBA16 tile to an `f32` tile, losslessly. `None` for
/// an `f32` tile (already there) or anything that is not a colour tile.
pub fn widen_to_rgbaf32_tile(bytes: &[u8]) -> Option<Vec<u8>> {
    if is_rgbaf32_tile(bytes) {
        return None;
    }
    rgbaf32_samples(bytes).map(|s| rgbaf32_to_tile_bytes(&s))
}

/// An `f32` tile's samples as 16-bit codes ([`f32_to_code16`]); `None` when
/// `bytes` is not an `f32` tile.
pub fn rgbaf32_tile_to_rgba16(bytes: &[u8]) -> Option<Vec<u16>> {
    is_rgbaf32_tile(bytes).then(|| {
        tile_bytes_to_rgbaf32(bytes)
            .into_iter()
            .map(f32_to_code16)
            .collect()
    })
}

/// Narrow an `f32` tile to `to_bits` (16 or 8) tile bytes. 8 goes through
/// 16 and may dither the colour channels. `None` when `bytes` is not an
/// `f32` tile or `to_bits` is neither 8 nor 16.
pub fn narrow_rgbaf32_tile(bytes: &[u8], to_bits: u8, dither: bool) -> Option<Vec<u8>> {
    let sixteen = crate::rgba16_to_tile_bytes(&rgbaf32_tile_to_rgba16(bytes)?);
    match to_bits {
        16 => Some(sixteen),
        8 => narrow_rgba16_tile(&sixteen, dither),
        _ => None,
    }
}

/// Land an 8- or 16-bit tile a tool (or a 16-bit whole-layer edit) produced
/// in a 32-bit layer: the `f32` twin of [`crate::depth::widen_rgba8_over`].
///
/// Each output sample is widened, except where it equals the old `f32`
/// sample rounded (and clipped) to the output's depth; what that equality
/// can prove depends on the old sample:
///
/// * an old sample inside `0..=1` differs from the output by less than half
///   an output code, so it is kept (channel by channel): never further from
///   the truth than the output itself, whatever the edit did, a move
///   included;
/// * an old sample outside `0..=1` (HDR headroom, or below 0) was clipped
///   before the edit saw it, so a match only says the output is the clip
///   value, which a pixel an edit *moved* there can equally be. It is kept
///   only when `keep_clipped` is set, which the caller may do for an edit
///   that writes each pixel from that same pixel (a paint stroke), and only
///   when all four samples of the pixel match; otherwise the sample lands as
///   the clipped output, so the edit loses the headroom there rather than
///   leaving a stale value in place.
///
/// `None` when `new` is neither an RGBA8 nor an RGBA16 tile. An `old` that is
/// not an `f32` tile is treated as absent.
pub fn widen_over_rgbaf32(new: &[u8], old: Option<&[u8]>, keep_clipped: bool) -> Option<Vec<u8>> {
    let wide = new.len() == RGBA16_TILE_BYTES;
    if !wide && new.len() != RGBA8_TILE_BYTES {
        return None;
    }
    let new16: Vec<u16> = if wide {
        crate::tile_bytes_to_rgba16(new)
    } else {
        new.iter().map(|c| widen_sample(*c)).collect()
    };
    let old = old
        .filter(|o| is_rgbaf32_tile(o))
        .map(tile_bytes_to_rgbaf32);
    let mut out = Vec::with_capacity(new16.len());
    let matches = |o: f32, code: u16| {
        let old_code = f32_to_code16(o);
        if wide {
            old_code == code
        } else {
            widen_sample(narrow_sample(old_code)) == code
        }
    };
    for (i, px) in new16.as_chunks::<4>().0.iter().enumerate() {
        let Some(old) = &old else {
            out.extend(px.iter().map(|c| f32::from(*c) / 65_535.0));
            continue;
        };
        let o = &old[i * 4..i * 4 + 4];
        let pixel_unchanged = (0..4).all(|k| matches(o[k], px[k]));
        for k in 0..4 {
            let keep = matches(o[k], px[k])
                && ((0.0..=1.0).contains(&o[k]) || (keep_clipped && pixel_unchanged));
            out.push(if keep {
                o[k]
            } else {
                f32::from(px[k]) / 65_535.0
            });
        }
    }
    Some(rgbaf32_to_tile_bytes(&out))
}

/// `f32` as a whole-layer edit sample (32 Bits/Channel): the code is the
/// sample itself, `MAX` is `1.0`, and nothing is rounded or clipped — a
/// value above `1.0` is HDR headroom. A non-finite result is written as 0.
impl DepthSample for f32 {
    const MAX: f32 = 1.0;
    #[inline]
    fn to_f32(self) -> f32 {
        self
    }
    #[inline]
    fn from_f32_rounded(v: f32) -> Self {
        if v.is_finite() {
            v
        } else {
            0.0
        }
    }
}

/// Encode straight-alpha RGBA `f32` samples (`width × height × 4`) as a
/// 32-bit floating-point TIFF: File ▸ Export of a 32-bit document to `.tif`.
/// The samples are written as they are — nothing is clipped, so HDR values
/// above `1.0` reach the file.
pub fn encode_tiff_rgbaf32(
    width: u32,
    height: u32,
    samples: &[f32],
) -> Result<Vec<u8>, crate::CodecError> {
    use image::ImageEncoder;
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4));
    if expected != Some(samples.len()) {
        return Err(crate::CodecError::BufferSize(format!(
            "{width}x{height} RGBA f32 needs {expected:?} samples, got {}",
            samples.len()
        )));
    }
    let mut out = std::io::Cursor::new(Vec::new());
    image::codecs::tiff::TiffEncoder::new(&mut out).write_image(
        bytemuck::cast_slice(samples),
        width,
        height,
        image::ExtendedColorType::Rgba32F,
    )?;
    Ok(out.into_inner())
}

/// Read a TIFF back as straight RGBA `f32` samples at full precision,
/// nothing clipped: `(width, height, samples)`. A float TIFF keeps its
/// values; an integer one is scaled into `0..=1`.
pub fn decode_tiff_rgbaf32(bytes: &[u8]) -> Result<(u32, u32, Vec<f32>), crate::CodecError> {
    let image = image::load_from_memory_with_format(bytes, image::ImageFormat::Tiff)?;
    let (w, h) = (image.width(), image.height());
    Ok((w, h, image.into_rgba32f().into_raw()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::depth::{rgba16_samples, rgba8_view, tile_alpha16};

    fn f32_tile(px: [f32; 4]) -> Vec<u8> {
        rgbaf32_to_tile_bytes(&px.repeat(RGBAF32_TILE_BYTES / 16))
    }

    #[test]
    fn an_f32_tile_is_twice_an_rgba16_one_and_round_trips_its_samples() {
        assert_eq!(RGBAF32_TILE_BYTES, 2 * RGBA16_TILE_BYTES);
        let s: Vec<f32> = (0..RGBAF32_TILE_BYTES / 4)
            .map(|i| i as f32 * 0.37 - 3.0)
            .collect();
        assert_eq!(tile_bytes_to_rgbaf32(&rgbaf32_to_tile_bytes(&s)), s);
    }

    #[test]
    fn widening_8_and_16_bit_tiles_to_f32_is_lossless_both_ways() {
        let t8: Vec<u8> = (0..RGBA8_TILE_BYTES)
            .map(|i| (i * 31 % 256) as u8)
            .collect();
        let f = widen_to_rgbaf32_tile(&t8).unwrap();
        assert_eq!(f.len(), RGBAF32_TILE_BYTES);
        assert_eq!(narrow_rgbaf32_tile(&f, 8, false).unwrap(), t8);
        let t16 = crate::rgba16_to_tile_bytes(
            &(0..RGBA16_TILE_BYTES / 2)
                .map(|i| (i * 7919 % 65_536) as u16)
                .collect::<Vec<_>>(),
        );
        let f16 = widen_to_rgbaf32_tile(&t16).unwrap();
        assert_eq!(narrow_rgbaf32_tile(&f16, 16, false).unwrap(), t16);
        // An f32 tile is not widened again.
        assert!(widen_to_rgbaf32_tile(&f16).is_none());
    }

    #[test]
    fn narrowing_clips_hdr_and_the_shared_readers_read_f32_tiles_at_their_own_stride() {
        // 4.0 is HDR headroom: kept in f32, clipped to white on the way down.
        let tile = f32_tile([4.0, 0.5, -1.0, 0.25]);
        assert_eq!(
            rgbaf32_tile_to_rgba16(&tile).unwrap()[..4],
            [65_535, 32_768, 0, 16_384]
        );
        assert_eq!(rgba8_view(&tile)[..4], [255, 128, 0, 64]);
        assert_eq!(rgba8_view(&tile).len(), RGBA8_TILE_BYTES);
        assert_eq!(
            rgba16_samples(&tile).unwrap()[..4],
            [65_535, 32_768, 0, 16_384]
        );
        assert_eq!(tile_alpha16(&tile, 7), Some(16_384));
        assert_eq!(rgbaf32_samples(&tile).unwrap()[..4], [4.0, 0.5, -1.0, 0.25]);
        assert_eq!(f32_to_code16(f32::NAN), 0);
    }

    #[test]
    fn widening_over_keeps_untouched_f32_pixels_and_widens_changed_ones() {
        // Old: an HDR, between-codes pixel everywhere.
        let old = f32_tile([2.5, 0.123_456, 0.9, 1.0]);
        for depth in [8u8, 16] {
            let mut new = narrow_rgbaf32_tile(&old, depth, false).unwrap();
            // The edit changed pixel 3 only (to opaque black).
            let bpp = new.len() / (RGBA8_TILE_BYTES / 4);
            let black: Vec<u8> = if depth == 8 {
                vec![0, 0, 0, 255]
            } else {
                crate::rgba16_to_tile_bytes(&[0, 0, 0, 65_535])
            };
            new[3 * bpp..4 * bpp].copy_from_slice(&black);
            let merged =
                tile_bytes_to_rgbaf32(&widen_over_rgbaf32(&new, Some(&old), true).unwrap());
            assert_eq!(merged[..4], [2.5, 0.123_456, 0.9, 1.0], "{depth}-bit");
            assert_eq!(merged[12..16], [0.0, 0.0, 0.0, 1.0], "{depth}-bit");
            assert_eq!(merged[16..20], [2.5, 0.123_456, 0.9, 1.0], "{depth}-bit");
        }
        // Nothing underneath: everything widened.
        let all = widen_over_rgbaf32(&[7u8; RGBA8_TILE_BYTES], None, true).unwrap();
        assert!(tile_bytes_to_rgbaf32(&all)
            .iter()
            .all(|v| (*v - 7.0 / 255.0).abs() < 1e-7));
        assert!(widen_over_rgbaf32(&[0u8; 4], None, false).is_none());
    }

    #[test]
    fn without_keep_clipped_a_matching_hdr_pixel_lands_clipped_and_an_in_range_one_is_kept() {
        // Pixel 0 HDR (2.5 clips to 1.0), every other pixel in range.
        let mut samples = [0.25f32, 0.123_456, 0.9, 1.0].repeat(RGBAF32_TILE_BYTES / 16);
        samples[..4].copy_from_slice(&[2.5, 0.123_456, 0.9, 1.0]);
        let old = rgbaf32_to_tile_bytes(&samples);
        for depth in [8u8, 16] {
            // An edit whose output equals the clipped old tile: a move of
            // other clipped content onto pixel 0 looks exactly like this.
            let new = narrow_rgbaf32_tile(&old, depth, false).unwrap();
            let merged =
                tile_bytes_to_rgbaf32(&widen_over_rgbaf32(&new, Some(&old), false).unwrap());
            assert_eq!(
                merged[0], 1.0,
                "{depth}-bit: the HDR value is not left in place"
            );
            assert_eq!(merged[4..8], [0.25, 0.123_456, 0.9, 1.0], "{depth}-bit");
            // A paint stroke may keep it.
            let kept = tile_bytes_to_rgbaf32(&widen_over_rgbaf32(&new, Some(&old), true).unwrap());
            assert_eq!(kept[0], 2.5, "{depth}-bit");
        }
    }

    #[test]
    fn the_float_tiff_keeps_hdr_values_and_every_bit_of_precision() {
        let px = [4.0f32, 0.123_456_7, 1e-6, 0.5, 0.0, 1.0, 2.5, 1.0];
        let bytes = encode_tiff_rgbaf32(2, 1, &px).unwrap();
        let back = image::load_from_memory_with_format(&bytes, image::ImageFormat::Tiff).unwrap();
        assert_eq!(back.color(), image::ColorType::Rgba32F);
        assert_eq!(decode_tiff_rgbaf32(&bytes).unwrap(), (2, 1, px.to_vec()));
        assert!(encode_tiff_rgbaf32(3, 1, &px).is_err());
    }

    #[test]
    fn f32_edit_samples_are_neither_rounded_nor_clipped() {
        assert_eq!(f32::from_f32_rounded(3.25), 3.25);
        assert_eq!(f32::from_f32_rounded(-0.5), -0.5);
        assert_eq!(f32::from_f32_rounded(f32::INFINITY), 0.0);
        assert_eq!(0.123_f32.to_unit(), 0.123);
        assert_eq!(f32::from_unit(1.75), 1.75);
    }
}
