//! Edge colour-defringe — card 062's focused raster edge cleanup.
//!
//! After a cutout, semi-transparent boundary pixels often still carry the
//! OLD background's colour (the classic green/orange halo). This operation
//! pulls the RGB of those boundary pixels toward the colour of the nearby
//! INTERIOR (fully-covered) pixels, leaving the alpha channel and every
//! pixel away from the boundary untouched.
//!
//! This is deliberately NOT mask feathering (it recolours, it never moves
//! coverage) and NOT automatic matting (the reference colour comes from the
//! image's own opaque pixels, deterministically). It is a gamma-space
//! average on purpose: fringe halos are an ENCODED-space artifact — what
//! looks wrong is the stored sRGB byte, and averaging encoded values is what
//! keeps the fix visually neutral on both sides of the edge.
//!
//! Bounded by construction: only pixels inside the bounding box of the
//! partially/fully covered region, dilated by the radius, are examined, and
//! the sampling window is a capped Chebyshev (square) neighbourhood.

/// The parameters one defringe pass takes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DefringeParams {
    /// How far from a boundary pixel the interior reference may be sampled,
    /// in pixels. Also the dilation that defines the boundary band.
    pub radius: u32,
    /// 0.0..=1.0 — how far the boundary RGB moves toward the reference.
    pub strength: f32,
}

/// The hard cap on [`defringe`]'s radius: a runaway parameter cannot turn
/// the per-pixel window into an unbounded scan.
pub const MAX_DEFRINGE_RADIUS: u32 = 64;

/// Reduce colour fringe on the masked boundary of a straight-alpha RGBA8
/// buffer, in place.
///
/// `coverage` is the mask's canvas-space coverage (same pixel count as the
/// image). A pixel is IN THE BAND when its coverage is above zero and it
/// lies within `radius` (Chebyshev distance) of a pixel whose coverage is
/// below half (a boundary neighbour). The reference colour is the average of
/// the fully-covered (coverage >= 250) pixels inside the same window; a band
/// pixel with no interior sample in reach is left unchanged rather than
/// invented. Interior pixels, fully transparent pixels, and everything
/// outside the band keep their exact bytes. Alpha is never modified.
///
/// Errors name the mismatch (buffer lengths, an over-cap radius, a
/// non-finite or out-of-range strength) instead of panicking.
pub fn defringe(
    rgba: &mut [u8],
    coverage: &[u8],
    width: u32,
    height: u32,
    params: DefringeParams,
) -> Result<(), String> {
    let n = (width as usize) * (height as usize);
    if rgba.len() != n * 4 {
        return Err(format!(
            "the RGBA buffer holds {} bytes, expected {n} pixels × 4",
            rgba.len()
        ));
    }
    if coverage.len() != n {
        return Err(format!(
            "the coverage plane holds {} samples, expected {n}",
            coverage.len()
        ));
    }
    if params.radius > MAX_DEFRINGE_RADIUS {
        return Err(format!(
            "radius {} exceeds the {} maximum",
            params.radius, MAX_DEFRINGE_RADIUS
        ));
    }
    if !(0.0..=1.0).contains(&params.strength) || !params.strength.is_finite() {
        return Err(format!("strength {} is outside 0.0..=1.0", params.strength));
    }
    if params.radius == 0 || params.strength == 0.0 || n == 0 {
        return Ok(()); // identity by definition
    }

    let r = params.radius as i64;
    let w = width as i64;
    let h = height as i64;
    let at = |x: i64, y: i64| -> usize { (y * w + x) as usize };

    // The band: covered pixels near a sub-half-covered neighbour. Computed
    // once, so the main pass reads a stable predicate instead of re-scanning.
    let mut band = vec![false; n];
    for y in 0..h {
        for x in 0..w {
            let i = at(x, y);
            if coverage[i] == 0 {
                continue;
            }
            let y0 = (y - r).max(0);
            let y1 = (y + r).min(h - 1);
            let x0 = (x - r).max(0);
            let x1 = (x + r).min(w - 1);
            let mut near_edge = false;
            'scan: for ny in y0..=y1 {
                for nx in x0..=x1 {
                    if coverage[at(nx, ny)] < 128 {
                        near_edge = true;
                        break 'scan;
                    }
                }
            }
            band[i] = near_edge;
        }
    }

    // Bounding box of the dilated band: the only region the pass touches.
    let mut x_min = w;
    let mut y_min = h;
    let mut x_max = -1i64;
    let mut y_max = -1i64;
    for y in 0..h {
        for x in 0..w {
            if band[at(x, y)] {
                x_min = x_min.min(x);
                y_min = y_min.min(y);
                x_max = x_max.max(x);
                y_max = y_max.max(y);
            }
        }
    }
    if x_max < 0 {
        return Ok(()); // no band at all (a solid or empty mask)
    }
    // Dilate the work box by the reference-sampling radius.
    let bx0 = (x_min - r).max(0);
    let by0 = (y_min - r).max(0);
    let bx1 = (x_max + r).min(w - 1);
    let by1 = (y_max + r).min(h - 1);

    // References read from a SNAPSHOT of the input: an in-place write would
    // let earlier pixels' new colours cascade into later windows, making the
    // result order-dependent. One deterministic pass, original bytes only.
    let original = rgba.to_vec();
    let strength = params.strength;
    for y in by0..=by1 {
        for x in bx0..=bx1 {
            let i = at(x, y);
            if !band[i] {
                continue;
            }
            let y0 = (y - r).max(0);
            let y1 = (y + r).min(h - 1);
            let x0 = (x - r).max(0);
            let x1 = (x + r).min(w - 1);
            let mut sum = [0.0f32; 3];
            let mut count = 0u32;
            for ny in y0..=y1 {
                for nx in x0..=x1 {
                    let j = at(nx, ny);
                    // An interior reference must be fully covered AND not
                    // itself part of the boundary band — fringe pixels are
                    // fully covered by the mask too, and counting their
                    // colour would dilute the pull toward the true interior
                    // (the pixel's own colour is excluded the same way).
                    if coverage[j] >= 250 && !band[j] {
                        let p = j * 4;
                        sum[0] += original[p] as f32;
                        sum[1] += original[p + 1] as f32;
                        sum[2] += original[p + 2] as f32;
                        count += 1;
                    }
                }
            }
            if count == 0 {
                continue; // no interior reference in reach — leave the pixel
            }
            let p = i * 4;
            for k in 0..3 {
                let reference = sum[k] / count as f32;
                let original = rgba[p + k] as f32;
                rgba[p + k] = (original * (1.0 - strength) + reference * strength).round() as u8;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 32×8 synthetic cutout: the left 24 columns are opaque ink (dark
    /// grey), the right 8 fully transparent — EXCEPT a fringe column at
    /// x=23: still "revealed" by the mask but holding the green background
    /// colour a sloppy cutout would leave behind.
    fn fringe_fixture() -> (Vec<u8>, Vec<u8>, u32, u32) {
        let (w, h) = (32u32, 8u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        let mut cov = vec![0u8; (w * h) as usize];
        for y in 0..h as usize {
            for x in 0..w as usize {
                let i = y * w as usize + x;
                if x < 23 {
                    rgba[i * 4] = 40;
                    rgba[i * 4 + 1] = 40;
                    rgba[i * 4 + 2] = 40;
                    rgba[i * 4 + 3] = 255;
                    cov[i] = 255;
                } else if x == 23 {
                    // The fringe: mask says visible, colour says background.
                    rgba[i * 4] = 20;
                    rgba[i * 4 + 1] = 230;
                    rgba[i * 4 + 2] = 30;
                    rgba[i * 4 + 3] = 255;
                    cov[i] = 255;
                } else if x == 24 {
                    // A semi-covered pixel that also carries the fringe.
                    rgba[i * 4] = 20;
                    rgba[i * 4 + 1] = 230;
                    rgba[i * 4 + 2] = 30;
                    rgba[i * 4 + 3] = 255;
                    cov[i] = 100;
                }
            }
        }
        (rgba, cov, w, h)
    }

    #[test]
    fn the_fringe_is_pulled_toward_the_interior_ink() {
        let (mut rgba, cov, w, h) = fringe_fixture();
        defringe(
            &mut rgba,
            &cov,
            w,
            h,
            DefringeParams {
                radius: 3,
                strength: 1.0,
            },
        )
        .unwrap();
        let fringe = |x: usize, y: usize| (y * w as usize + x) * 4;
        // At full strength the fringe pixel takes the interior colour exactly
        // (its window is full of 40/40/40 interior pixels).
        assert_eq!(rgba[fringe(23, 4)], 40);
        assert_eq!(rgba[fringe(23, 4) + 1], 40, "the green fringe is gone");
        assert_eq!(rgba[fringe(23, 4) + 2], 40);
        // Alpha survives untouched — this recolours, it never masks.
        assert_eq!(rgba[fringe(23, 4) + 3], 255);
        assert_eq!(rgba[fringe(24, 4) + 3], 255);
    }

    #[test]
    fn interior_pixels_far_from_the_boundary_keep_their_exact_bytes() {
        let (mut rgba, cov, w, h) = fringe_fixture();
        let before = rgba.clone();
        defringe(
            &mut rgba,
            &cov,
            w,
            h,
            DefringeParams {
                radius: 3,
                strength: 0.8,
            },
        )
        .unwrap();
        // x=15 is 8 px from the nearest sub-half coverage (x=24) — outside
        // the band even after dilation.
        for y in 0..h as usize {
            for x in 0..15usize {
                let i = (y * w as usize + x) * 4;
                assert_eq!(
                    rgba[i..i + 4],
                    before[i..i + 4],
                    "pixel ({x},{y}) is away from the boundary and must not move"
                );
            }
        }
        // Fully transparent pixels are never touched either.
        let t = (4usize * w as usize + 30) * 4;
        assert_eq!(rgba[t..t + 4], before[t..t + 4]);
    }

    #[test]
    fn strength_scales_the_pull_and_zero_is_identity() {
        let (rgba0, cov, w, h) = fringe_fixture();
        let mut half = rgba0.clone();
        defringe(
            &mut half,
            &cov,
            w,
            h,
            DefringeParams {
                radius: 3,
                strength: 0.5,
            },
        )
        .unwrap();
        let p = (4 * w as usize + 23) * 4 + 1; // fringe green channel
        let original = rgba0[p] as f32; // 230
        let reference = 40.0;
        let expected = (original * 0.5 + reference * 0.5).round() as u8;
        assert_eq!(half[p], expected, "half strength blends halfway");
        // Strength 0 and radius 0 are exact identities.
        for radius in [0u32, 3] {
            let mut same = rgba0.clone();
            defringe(
                &mut same,
                &cov,
                w,
                h,
                DefringeParams {
                    radius,
                    strength: if radius == 0 { 1.0 } else { 0.0 },
                },
            )
            .unwrap();
            assert_eq!(same, rgba0, "radius {radius} identity case");
        }
    }

    #[test]
    fn a_band_pixel_without_interior_reference_is_left_unchanged() {
        // An isolated semi-covered pixel with no opaque neighbour anywhere.
        let (w, h) = (8u32, 8u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        let mut cov = vec![0u8; (w * h) as usize];
        let i = (4 * w as usize + 4) * 4;
        rgba[i] = 200;
        rgba[i + 1] = 10;
        rgba[i + 2] = 10;
        rgba[i + 3] = 128;
        cov[4 * w as usize + 4] = 100;
        let before = rgba.clone();
        defringe(
            &mut rgba,
            &cov,
            w,
            h,
            DefringeParams {
                radius: 3,
                strength: 1.0,
            },
        )
        .unwrap();
        assert_eq!(rgba, before, "no reference in reach: the pixel survives");
    }

    #[test]
    fn malformed_inputs_error_instead_of_panicking() {
        let (mut rgba, cov, w, h) = fringe_fixture();
        let p = DefringeParams {
            radius: 3,
            strength: 0.5,
        };
        assert!(defringe(&mut rgba.clone(), &cov[..cov.len() - 1], w, h, p).is_err());
        {
            let mut short = rgba.clone();
            short.truncate(short.len() - 4);
            assert!(defringe(&mut short, &cov, w, h, p).is_err());
        }
        assert!(defringe(
            &mut rgba,
            &cov,
            w,
            h,
            DefringeParams {
                radius: MAX_DEFRINGE_RADIUS + 1,
                strength: 0.5
            }
        )
        .is_err());
        assert!(defringe(
            &mut rgba,
            &cov,
            w,
            h,
            DefringeParams {
                radius: 3,
                strength: 1.5
            }
        )
        .is_err());
        assert!(defringe(
            &mut rgba,
            &cov,
            w,
            h,
            DefringeParams {
                radius: 3,
                strength: f32::NAN
            }
        )
        .is_err());
        // An empty canvas is a valid no-op, not an error.
        assert!(defringe(
            &mut [],
            &[],
            0,
            0,
            DefringeParams {
                radius: 3,
                strength: 0.5
            }
        )
        .is_ok());
    }
}
