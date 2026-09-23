//! Converting layer tiles between 8 and 16 bits per channel.
//!
//! Image ▸ Mode ▸ 8/16 Bits/Channel rewrites every raster tile of a document
//! through these functions, and the paint path uses [`widen_rgba8_over`] to
//! land a tool's 8-bit output in a 16-bit document without dropping the deep
//! precision of the pixels the tool did not change.
//!
//! Both depths store straight-alpha, encoded samples in the same layout the
//! compositor reads (`compositor::composite`'s `fill_layer`): RGBA8 is one
//! byte a sample, RGBA16 is [`crate::rgba16_to_tile_bytes`]'s native-endian
//! `u16`s. The two are told apart by length, exactly as the compositor does.
//!
//! * 8 → 16 is lossless: each code is widened by bit repetition
//!   (`c * 257`, so 255 → 65535), the same rule the 16-bit New Document
//!   background uses, and it narrows back to the identical byte.
//! * 16 → 8 rounds to the nearest code, optionally with a 4×4 ordered dither
//!   on the colour channels (never on alpha) so a smooth 16-bit gradient does
//!   not band.

use crate::format::PixelFormat;
use crate::tile::Tile;

/// Bytes in an RGBA8 layer tile.
pub const RGBA8_TILE_BYTES: usize = Tile::byte_len(PixelFormat::Rgba8);
/// Bytes in an RGBA16 layer tile.
pub const RGBA16_TILE_BYTES: usize = Tile::byte_len(PixelFormat::Rgba16);

/// Widen one 8-bit code to 16 bits by bit repetition (255 → 65535).
pub const fn widen_sample(c: u8) -> u16 {
    c as u16 * 257
}

/// Round one 16-bit code to the nearest 8-bit code.
pub const fn narrow_sample(v: u16) -> u8 {
    ((v as u32 * 255 + 32_767) / 65_535) as u8
}

/// 4×4 Bayer thresholds, centred on zero, in 1/16ths of one 8-bit step.
const BAYER4: [[i32; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];

/// Narrow one 16-bit code with an ordered-dither offset for pixel `(x, y)`.
fn narrow_dithered(v: u16, x: usize, y: usize) -> u8 {
    // One 8-bit step is 257 sixteen-bit codes; the threshold shifts the
    // rounding point by (t - 7.5) / 16 of a step.
    let t = BAYER4[y & 3][x & 3];
    let shifted = i64::from(v) + ((2 * t - 15) as i64 * 257) / 32;
    narrow_sample(shifted.clamp(0, 65_535) as u16)
}

/// Widen an RGBA8 tile's bytes to an RGBA16 tile's bytes, losslessly.
///
/// Returns `None` when `bytes` is not an RGBA8 tile.
pub fn widen_rgba8_tile(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.len() != RGBA8_TILE_BYTES {
        return None;
    }
    Some(
        bytes
            .iter()
            .flat_map(|c| widen_sample(*c).to_ne_bytes())
            .collect(),
    )
}

/// Narrow an RGBA16 tile's bytes to an RGBA8 tile's bytes.
///
/// Rounds to the nearest code; with `dither` the colour channels get a 4×4
/// ordered dither keyed on the pixel's position in the tile (the tile edge is
/// a multiple of four, so the pattern is seamless across tiles). Alpha is
/// always rounded, never dithered. Returns `None` when `bytes` is not an
/// RGBA16 tile.
pub fn narrow_rgba16_tile(bytes: &[u8], dither: bool) -> Option<Vec<u8>> {
    if bytes.len() != RGBA16_TILE_BYTES {
        return None;
    }
    let ts = crate::TILE_SIZE as usize;
    let mut out = Vec::with_capacity(RGBA8_TILE_BYTES);
    for (i, px) in bytes.as_chunks::<8>().0.iter().enumerate() {
        let (x, y) = (i % ts, i / ts);
        for k in 0..4 {
            let v = u16::from_ne_bytes([px[2 * k], px[2 * k + 1]]);
            out.push(if dither && k < 3 {
                narrow_dithered(v, x, y)
            } else {
                narrow_sample(v)
            });
        }
    }
    Some(out)
}

/// The alpha of pixel `index` (row-major within the tile) of a layer colour
/// tile, whatever depth it is stored at, as a 16-bit code.
///
/// An RGBA16 tile ([`RGBA16_TILE_BYTES`] long) is read at its 16-bit alpha
/// sample; anything else is read as RGBA8 at a 4-byte stride and widened by
/// [`widen_sample`]. `None` when the pixel lies past the end of the bytes.
/// This is what the tile readers that only need coverage (tight ink bounds,
/// hit testing) use, so an RGBA16 tile is never mis-strided as RGBA8.
pub fn tile_alpha16(bytes: &[u8], index: usize) -> Option<u16> {
    if bytes.len() == RGBA16_TILE_BYTES {
        let at = index.checked_mul(8)?.checked_add(6)?;
        let pair = bytes.get(at..at + 2)?;
        return Some(u16::from_ne_bytes([pair[0], pair[1]]));
    }
    let at = index.checked_mul(4)?.checked_add(3)?;
    bytes.get(at).map(|&a| widen_sample(a))
}

/// A layer tile's bytes as RGBA8, whatever depth they are stored at.
///
/// The 8-bit readers of a document (whole-layer reads for filters,
/// transforms, fills and adjustments; content bounds; canvas fills) work in
/// RGBA8. An RGBA8 tile — or anything that is not an RGBA16 colour tile — is
/// borrowed as it is; an RGBA16 tile is rounded to the nearest 8-bit code
/// (no dither: a read must be deterministic, so the apply boundary's
/// [`widen_rgba8_over`] can recognise the pixels an edit left unchanged).
pub fn rgba8_view(bytes: &[u8]) -> std::borrow::Cow<'_, [u8]> {
    match narrow_rgba16_tile(bytes, false) {
        Some(narrowed) => std::borrow::Cow::Owned(narrowed),
        None => std::borrow::Cow::Borrowed(bytes),
    }
}

/// Land an 8-bit tile a tool produced in a 16-bit layer.
///
/// `new8` is the tool's RGBA8 output for one tile; `old16` is the RGBA16 tile
/// that was there before, if any. A pixel whose 8-bit value equals the
/// rounded old value is one the tool did not change, so it keeps its old
/// 16-bit value exactly; every other pixel is widened from the tool's output.
/// That is what keeps a brush dab from quantising the untouched rest of the
/// tile to eight bits.
///
/// Returns `None` when `new8` is not an RGBA8 tile. An `old16` of the wrong
/// length is treated as absent.
pub fn widen_rgba8_over(new8: &[u8], old16: Option<&[u8]>) -> Option<Vec<u8>> {
    if new8.len() != RGBA8_TILE_BYTES {
        return None;
    }
    let old16 = old16.filter(|o| o.len() == RGBA16_TILE_BYTES);
    let mut out = Vec::with_capacity(RGBA16_TILE_BYTES);
    for (i, px) in new8.as_chunks::<4>().0.iter().enumerate() {
        if let Some(old) = old16 {
            let o = &old[i * 8..i * 8 + 8];
            let unchanged = (0..4)
                .all(|k| narrow_sample(u16::from_ne_bytes([o[2 * k], o[2 * k + 1]])) == px[k]);
            if unchanged {
                out.extend_from_slice(o);
                continue;
            }
        }
        for c in px {
            out.extend_from_slice(&widen_sample(*c).to_ne_bytes());
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp8() -> Vec<u8> {
        (0..RGBA8_TILE_BYTES)
            .map(|i| (i * 31 % 256) as u8)
            .collect()
    }

    #[test]
    fn tile_alpha16_reads_each_depth_at_its_own_stride() {
        // Pixel 5 is the only one with alpha, in both depths.
        let mut t8 = vec![0u8; RGBA8_TILE_BYTES];
        t8[5 * 4..5 * 4 + 4].copy_from_slice(&[200, 100, 50, 128]);
        let t16 = widen_rgba8_tile(&t8).unwrap();
        for bytes in [&t8, &t16] {
            assert_eq!(tile_alpha16(bytes, 5), Some(widen_sample(128)));
            assert_eq!(tile_alpha16(bytes, 4), Some(0));
            assert_eq!(tile_alpha16(bytes, 6), Some(0));
        }
        // An RGBA16 tile read at RGBA8 stride would see pixel 5's red/green
        // bytes as alphas of other pixels (byte 43, pixel 10's RGBA8 alpha,
        // is pixel 5's green sample); the depth-aware read does not.
        assert_ne!(t16[10 * 4 + 3], 0);
        assert_eq!(tile_alpha16(&t16, 10), Some(0));
        let pixels = crate::TILE_SIZE as usize * crate::TILE_SIZE as usize;
        assert_eq!(tile_alpha16(&t16, pixels), None);
        assert_eq!(tile_alpha16(&t8, pixels), None);
    }

    #[test]
    fn every_8_bit_code_survives_a_widen_then_narrow() {
        for c in 0..=255u8 {
            assert_eq!(narrow_sample(widen_sample(c)), c);
        }
        let src = ramp8();
        let wide = widen_rgba8_tile(&src).unwrap();
        assert_eq!(wide.len(), RGBA16_TILE_BYTES);
        assert_eq!(narrow_rgba16_tile(&wide, false).unwrap(), src);
        // Widened codes sit exactly on the 8-bit grid, so dither cannot move
        // them either.
        assert_eq!(narrow_rgba16_tile(&wide, true).unwrap(), src);
    }

    #[test]
    fn narrowing_rounds_to_the_nearest_code_and_wrong_lengths_are_refused() {
        assert_eq!(narrow_sample(0), 0);
        assert_eq!(narrow_sample(65_535), 255);
        assert_eq!(narrow_sample(128), 0);
        assert_eq!(narrow_sample(129), 1);
        assert!(widen_rgba8_tile(&[0u8; 4]).is_none());
        assert!(narrow_rgba16_tile(&ramp8(), false).is_none());
        assert!(widen_rgba8_over(&[0u8; 8], None).is_none());
    }

    #[test]
    fn dither_spreads_a_between_codes_value_and_keeps_alpha_exact() {
        // 100.5 steps: plain rounding picks one code everywhere, the dither
        // mixes the two neighbours; alpha stays plain-rounded.
        let v = 100 * 257 + 128;
        let px: Vec<u8> = [v, v, v, 40_000u16]
            .iter()
            .flat_map(|s| s.to_ne_bytes())
            .collect();
        let tile: Vec<u8> = px.repeat(RGBA16_TILE_BYTES / 8);
        let plain = narrow_rgba16_tile(&tile, false).unwrap();
        let dith = narrow_rgba16_tile(&tile, true).unwrap();
        let reds = |t: &[u8]| {
            let mut s: Vec<u8> = t.chunks(4).map(|p| p[0]).collect();
            s.sort_unstable();
            s.dedup();
            s
        };
        assert_eq!(reds(&plain).len(), 1);
        assert_eq!(reds(&dith), vec![100, 101]);
        assert!(dith.chunks(4).all(|p| p[3] == narrow_sample(40_000)));
    }

    #[test]
    fn widening_over_keeps_untouched_deep_pixels_and_widens_changed_ones() {
        let mut old = vec![0u8; RGBA16_TILE_BYTES];
        for (i, s) in old.as_chunks_mut::<2>().0.iter_mut().enumerate() {
            *s = ((i * 7919 % 65_536) as u16).to_ne_bytes();
        }
        let mut new8 = narrow_rgba16_tile(&old, false).unwrap();
        // The tool changed pixel 5 only.
        new8[20..24].copy_from_slice(&[1, 2, 3, 255]);
        let merged = widen_rgba8_over(&new8, Some(&old)).unwrap();
        assert_eq!(merged[..40], old[..40]);
        assert_eq!(merged[48..], old[48..]);
        let p5: Vec<u16> = merged[40..48]
            .chunks(2)
            .map(|b| u16::from_ne_bytes([b[0], b[1]]))
            .collect();
        assert_eq!(p5, vec![257, 514, 771, 65_535]);
        // With nothing underneath, everything is widened.
        assert_eq!(
            widen_rgba8_over(&new8, None).unwrap(),
            widen_rgba8_tile(&new8).unwrap()
        );
    }
}
