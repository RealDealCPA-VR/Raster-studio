//! W13-J: Filter ▸ 3D ▸ Normal Map (Photopea's *Generate Normals*) and
//! Texture Dilation, ported from Photopea.
//!
//! Tools for preparing textures for a 3-D renderer. The controls are
//! Photopea's (its `lightFilterGradient` and `Dila` descriptors and dialogs),
//! and so are the algorithms, read from Photopea's own filter code:
//!
//! * [`normal_map`] — *Blur* (0 to 100 px), *Scale* (0 to 200 %), *Invert*,
//!   and the weights of three *detail* bands, *High*, *Medium* and *Low* (0 to
//!   100 % each). The layer's luma is read as height at three scales — itself
//!   squared (High), blurred by 8 px (Medium) and by 16 px (Low) — mixed by
//!   the weights, and each pixel's surface normal is written as a colour:
//!   `x` in red, `y` in green, `z` in blue, each mapped from `[-1, 1]` to
//!   `[0, 255]` by `floor(127.5 + 127.5 v)`, so a flat surface is
//!   `(127, 127, 255)`. Alpha is kept.
//! * [`texture_dilation`] — *Crop* (0 to 20 px) and *Radius* (0 to 400 px).
//!   Every pixel within `crop + radius` of a covered pixel takes the colour of
//!   its **nearest** covered pixel, fully opaque; everything further away is
//!   cleared. A pixel counts as covered when its alpha is over 128/255, and
//!   *Crop* first shaves that many pixels off the covered areas' edges (whose
//!   colours are often contaminated by the background), so they are refilled
//!   from further in.
//!
//! Photopea blurs with three box passes approximating a Gaussian of the given
//! radius; [`box_gauss`] is that approximation, so the bands match its.

use color::{linear_to_srgb, premultiply, srgb_to_linear, unpremultiply};

use crate::buffer::FilterBuffer;

/// Largest Blur Photopea's Generate Normals dialog accepts, in pixels.
pub const MAX_NORMAL_BLUR: f32 = 100.0;
/// Largest Crop Photopea's Texture Dilation dialog accepts, in pixels.
pub const MAX_DILATION_CROP: u32 = 20;
/// Largest Radius Photopea's Texture Dilation dialog accepts, in pixels.
pub const MAX_DILATION_RADIUS: u32 = 400;

/// The widths of the `n` box passes that approximate a Gaussian of standard
/// deviation `sigma` (Photopea's, and the usual, construction).
fn box_widths(sigma: f32, n: u32) -> Vec<usize> {
    let n_f = n as f32;
    let ideal = (12.0 * sigma * sigma / n_f + 1.0).sqrt();
    let mut lo = ideal.floor() as i64;
    if lo % 2 == 0 {
        lo -= 1;
    }
    let lo = lo.max(1);
    let hi = lo + 2;
    let lo_f = lo as f32;
    let m = ((12.0 * sigma * sigma - n_f * lo_f * lo_f - 4.0 * n_f * lo_f - 3.0 * n_f)
        / (-4.0 * lo_f - 4.0))
        .round() as i64;
    (0..i64::from(n))
        .map(|i| if i < m { lo as usize } else { hi as usize })
        .collect()
}

/// One box pass of odd width `width` along rows (`horizontal`) or columns,
/// the edge value repeated outwards.
fn box_pass(plane: &[f32], w: usize, h: usize, width: usize, horizontal: bool) -> Vec<f32> {
    let r = (width / 2) as i64;
    if r == 0 {
        return plane.to_vec();
    }
    let (lines, len) = if horizontal { (h, w) } else { (w, h) };
    let at = |line: usize, i: i64| -> f32 {
        let i = i.clamp(0, len as i64 - 1) as usize;
        if horizontal {
            plane[line * w + i]
        } else {
            plane[i * w + line]
        }
    };
    let mut out = vec![0.0f32; plane.len()];
    let scale = 1.0 / (2 * r + 1) as f64;
    for line in 0..lines {
        let mut sum: f64 = (-r..=r).map(|i| f64::from(at(line, i))).sum();
        for i in 0..len as i64 {
            let v = (sum * scale) as f32;
            if horizontal {
                out[line * w + i as usize] = v;
            } else {
                out[i as usize * w + line] = v;
            }
            sum += f64::from(at(line, i + r + 1)) - f64::from(at(line, i - r));
        }
    }
    out
}

/// A Gaussian blur of standard deviation `sigma` over a `w x h` plane,
/// approximated by three box passes each way, the edges repeated. A zero or
/// non-finite `sigma` leaves the plane as it is.
pub fn box_gauss(plane: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    if !sigma.is_finite() || sigma <= 0.0 || w == 0 || h == 0 {
        return plane.to_vec();
    }
    let mut cur = plane.to_vec();
    for width in box_widths(sigma, 3) {
        cur = box_pass(&cur, w, h, width, true);
        cur = box_pass(&cur, w, h, width, false);
    }
    cur
}

/// Normal Map (Photopea's Generate Normals): see the module documentation.
///
/// `scale` and the three detail weights are fractions (`1.0` is 100 %).
/// Photopea's formula: with `a` the 0 to 255 luma (0.3 R + 0.59 G + 0.11 B of
/// the encoded colour), `k = 40 * scale / (high + medium + low)` (negated
/// when inverted), the height is `k * (low * blur16(a) + medium * blur8(a)) /
/// 255 + k * high * (a / 255)^2`, then blurred by `blur`; the slopes are
/// `0.3 * forward difference + 0.7 * backward difference` on each axis
/// (clamped at the edges), and the normal is the cross product of the two
/// tangents `(1, 0, sx)` and `(0, 1, sy)`, normalised. All three weights at
/// zero is a flat surface.
pub fn normal_map(
    src: &FilterBuffer,
    blur: f32,
    scale: f32,
    invert: bool,
    high: f32,
    medium: f32,
    low: f32,
) -> FilterBuffer {
    if src.is_empty() {
        return src.clone();
    }
    let clean = |v: f32, max: f32| {
        if v.is_finite() {
            v.clamp(0.0, max)
        } else {
            0.0
        }
    };
    let (high, medium, low) = (clean(high, 1.0), clean(medium, 1.0), clean(low, 1.0));
    let scale = clean(scale, 2.0) * if invert { -1.0 } else { 1.0 };
    let total = high + medium + low;
    let k = if total > 0.0 {
        40.0 * scale / total
    } else {
        0.0
    };
    let (w, h) = (src.width() as usize, src.height() as usize);
    let straight: Vec<[f32; 4]> = src.pixels().iter().map(|p| unpremultiply(*p)).collect();
    let luma: Vec<f32> = straight
        .iter()
        .map(|s| {
            let e = [0, 1, 2].map(|c| linear_to_srgb(s[c].clamp(0.0, 1.0)));
            255.0 * (0.3 * e[0] + 0.59 * e[1] + 0.11 * e[2])
        })
        .collect();
    let wide = box_gauss(&luma, w, h, 16.0);
    let mid = box_gauss(&luma, w, h, 8.0);
    let height: Vec<f32> = (0..w * h)
        .map(|i| {
            let fine = luma[i] / 255.0;
            k * low * wide[i] / 255.0 + k * medium * mid[i] / 255.0 + k * high * fine * fine
        })
        .collect();
    let height = box_gauss(&height, w, h, clean(blur, MAX_NORMAL_BLUR));
    let mut out = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let here = height[i];
            let right = height[if x + 1 == w { i } else { i + 1 }];
            let left = height[if x == 0 { i } else { i - 1 }];
            let down = height[if y + 1 == h { i } else { i + w }];
            let up = height[if y == 0 { i } else { i - w }];
            let sx = 0.3 * (right - here) + 0.7 * (here - left);
            let sy = 0.3 * (down - here) + 0.7 * (here - up);
            let la = (1.0 + sx * sx).sqrt();
            let lb = (1.0 + sy * sy).sqrt();
            // Tangents (1, 0, sx) / la and (0, 1, sy) / lb; their cross
            // product, as Photopea writes it.
            let n = [
                -(sx / la) * (1.0 / lb),
                -(1.0 / la) * (sy / lb),
                (1.0 / la) * (1.0 / lb),
            ];
            let byte = n.map(|v| (127.5 + v * 127.5).floor().clamp(0.0, 255.0) / 255.0);
            out.push(premultiply([
                srgb_to_linear(byte[0]),
                srgb_to_linear(byte[1]),
                srgb_to_linear(byte[2]),
                straight[i][3].clamp(0.0, 1.0),
            ]));
        }
    }
    FilterBuffer::from_pixels(src.width(), src.height(), out).expect("same size as the source")
}

/// For every pixel of a `w x h` mask, the index of the nearest `true` pixel
/// (Euclidean, between pixel centres) and the squared distance to it, or
/// `None` when the mask has no `true` pixel. An exact two-pass distance
/// transform (a column scan, then the lower envelope of parabolas along each
/// row) that carries the nearest pixel along — the construction Photopea's
/// own distance transform uses.
fn nearest_feature(mask: &[bool], w: usize, h: usize) -> Vec<Option<(usize, i64)>> {
    // Column pass: for each pixel, the row of the nearest `true` pixel in its
    // own column.
    let mut col_row: Vec<Option<usize>> = vec![None; w * h];
    for x in 0..w {
        let mut last: Option<usize> = None;
        for y in 0..h {
            if mask[y * w + x] {
                last = Some(y);
            }
            col_row[y * w + x] = last;
        }
        let mut next: Option<usize> = None;
        for y in (0..h).rev() {
            if mask[y * w + x] {
                next = Some(y);
            }
            let best = match (col_row[y * w + x], next) {
                (Some(a), Some(b)) => Some(if y - a <= b - y { a } else { b }),
                (a, b) => a.or(b),
            };
            col_row[y * w + x] = best;
        }
    }
    // Row pass: lower envelope of the parabolas (x - q)^2 + g(q)^2.
    let mut out = vec![None; w * h];
    let mut v: Vec<usize> = vec![0; w];
    let mut z: Vec<f64> = vec![0.0; w + 1];
    for y in 0..h {
        let g = |q: usize| -> Option<f64> {
            col_row[y * w + q].map(|r| {
                let d = r as f64 - y as f64;
                d * d
            })
        };
        let mut k: isize = -1;
        for q in 0..w {
            let Some(gq) = g(q) else { continue };
            loop {
                if k < 0 {
                    k = 0;
                    v[0] = q;
                    z[0] = f64::NEG_INFINITY;
                    z[1] = f64::INFINITY;
                    break;
                }
                let p = v[k as usize];
                let gp = g(p).expect("only filled columns are on the envelope");
                let s =
                    ((gq + (q * q) as f64) - (gp + (p * p) as f64)) / (2.0 * (q as f64 - p as f64));
                if s <= z[k as usize] {
                    k -= 1;
                    continue;
                }
                k += 1;
                v[k as usize] = q;
                z[k as usize] = s;
                z[k as usize + 1] = f64::INFINITY;
                break;
            }
        }
        if k < 0 {
            continue;
        }
        let mut j = 0usize;
        for x in 0..w {
            while z[j + 1] < x as f64 {
                j += 1;
            }
            let q = v[j];
            let r = col_row[y * w + q].expect("on the envelope");
            let (dx, dy) = (x as i64 - q as i64, y as i64 - r as i64);
            out[y * w + x] = Some((r * w + q, dx * dx + dy * dy));
        }
    }
    out
}

/// Texture Dilation: see the module documentation.
///
/// `crop` is clamped to `0 ..= 20` and `radius` to `0 ..= 400` pixels. Both
/// at zero, or a layer with no covered pixel, is returned unchanged.
pub fn texture_dilation(src: &FilterBuffer, crop: u32, radius: u32) -> FilterBuffer {
    let crop = crop.min(MAX_DILATION_CROP);
    let reach = i64::from(radius.min(MAX_DILATION_RADIUS) + crop);
    if src.is_empty() || reach == 0 {
        return src.clone();
    }
    let (w, h) = (src.width() as usize, src.height() as usize);
    let threshold = 128.0 / 255.0;
    let mut covered: Vec<bool> = src.pixels().iter().map(|p| p[3] > threshold).collect();
    if !covered.iter().any(|c| *c) {
        return src.clone();
    }
    if crop > 0 {
        // Shave `crop` pixels off every covered area's edge: a covered pixel
        // within `crop` of a clear one is dropped.
        let clear: Vec<bool> = covered.iter().map(|c| !c).collect();
        if clear.iter().any(|c| *c) {
            let near = nearest_feature(&clear, w, h);
            let c2 = i64::from(crop) * i64::from(crop);
            for (i, n) in near.iter().enumerate() {
                if let Some((_, d2)) = n {
                    if covered[i] && *d2 <= c2 {
                        covered[i] = false;
                    }
                }
            }
        }
        if !covered.iter().any(|c| *c) {
            return FilterBuffer::transparent(src.width(), src.height())
                .expect("same size as the source");
        }
    }
    let near = nearest_feature(&covered, w, h);
    let reach2 = reach * reach;
    let px = near
        .iter()
        .map(|n| match n {
            Some((j, d2)) if *d2 < reach2 => {
                let s = unpremultiply(src.pixels()[*j]);
                premultiply([s[0], s[1], s[2], 1.0])
            }
            _ => [0.0; 4],
        })
        .collect();
    FilterBuffer::from_pixels(src.width(), src.height(), px).expect("same size as the source")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grey_row(values: &[f32]) -> FilterBuffer {
        let px = values
            .iter()
            .map(|e| {
                let l = srgb_to_linear(*e);
                [l, l, l, 1.0]
            })
            .collect();
        FilterBuffer::from_pixels(values.len() as u32, 1, px).unwrap()
    }

    #[test]
    fn a_flat_surface_is_photopeas_flat_normal() {
        let flat = FilterBuffer::filled(6, 5, [0.3, 0.3, 0.3, 1.0]).unwrap();
        let out = normal_map(&flat, 0.0, 1.0, false, 1.0, 1.0, 1.0).to_rgba8();
        for p in out.chunks(4) {
            assert_eq!(p, [127, 127, 255, 255]);
        }
        // No detail at all is flat too, whatever the image.
        let ramp = grey_row(&[0.0, 0.5, 1.0]);
        let none = normal_map(&ramp, 0.0, 1.0, false, 0.0, 0.0, 0.0).to_rgba8();
        assert!(none.chunks(4).all(|p| p == [127, 127, 255, 255]));
    }

    #[test]
    fn the_high_band_is_squared_luma_and_invert_flips_it() {
        // Three pixels of encoded grey 0, 0.5, 1 and only High: heights are
        // 40 * (a / 255)^2 with a = 0, 127.5, 255.
        let ramp = grey_row(&[0.0, 0.5, 1.0]);
        let hts = [0.0f32, 0.25, 1.0].map(|v| 40.0 * v);
        let expect = |sign: f32| {
            let h: Vec<f32> = hts.iter().map(|v| v * sign).collect();
            let sx = 0.3 * (h[2] - h[1]) + 0.7 * (h[1] - h[0]);
            let la = (1.0 + sx * sx).sqrt();
            let byte = |v: f32| (127.5 + v * 127.5).floor() as u8;
            [byte(-sx / la), 127, byte(1.0 / la)]
        };
        let up = normal_map(&ramp, 0.0, 1.0, false, 1.0, 0.0, 0.0).to_rgba8();
        let down = normal_map(&ramp, 0.0, 1.0, true, 1.0, 0.0, 0.0).to_rgba8();
        let (e_up, e_down) = (expect(1.0), expect(-1.0));
        for c in 0..3 {
            assert!(
                (i32::from(up[4 + c]) - i32::from(e_up[c])).abs() <= 1,
                "{:?} vs {e_up:?}",
                &up[4..8]
            );
            assert!(
                (i32::from(down[4 + c]) - i32::from(e_down[c])).abs() <= 1,
                "{:?} vs {e_down:?}",
                &down[4..8]
            );
        }
        assert!(up[4] < 30 && down[4] > 225, "{} {}", up[4], down[4]);
        // Scale 0 is flat.
        let zero = normal_map(&ramp, 0.0, 0.0, false, 1.0, 1.0, 1.0).to_rgba8();
        assert!(zero.chunks(4).all(|p| p == [127, 127, 255, 255]));
    }

    #[test]
    fn box_gauss_keeps_a_constant_and_spreads_a_spike() {
        let flat = vec![0.4f32; 30];
        assert!(box_gauss(&flat, 6, 5, 3.0)
            .iter()
            .all(|v| (v - 0.4).abs() < 1e-6));
        let mut spike = vec![0.0f32; 21];
        spike[10] = 1.0;
        let out = box_gauss(&spike, 21, 1, 2.0);
        assert!(out[10] < 1.0 && out[9] > 0.0 && out[11] > 0.0);
        assert!((out[9] - out[11]).abs() < 1e-6, "symmetric");
        assert_eq!(box_widths(16.0, 3).len(), 3);
    }

    #[test]
    fn dilation_takes_the_nearest_covered_colour_within_the_reach() {
        // A 7x1 strip: red at x = 0, blue at x = 6, the rest clear.
        let mut src = FilterBuffer::transparent(7, 1).unwrap();
        src.set(0, 0, [1.0, 0.0, 0.0, 1.0]);
        src.set(6, 0, [0.0, 0.0, 1.0, 1.0]);
        // Radius 2: pixels 1 (d = 1) and 5 are filled; 2 (d = 2) is not, as
        // Photopea's test is d^2 < reach^2.
        let two = texture_dilation(&src, 0, 2);
        assert_eq!(two.get(1, 0), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(two.get(5, 0), [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(two.get(2, 0), [0.0; 4]);
        assert_eq!(two.get(3, 0), [0.0; 4]);
        // Radius 10: every pixel takes its nearest end (the middle ties to
        // the left, the column scan's and the envelope's first).
        let all = texture_dilation(&src, 0, 10);
        for x in 0..3 {
            assert_eq!(all.get(x, 0), [1.0, 0.0, 0.0, 1.0], "{x}");
        }
        for x in 4..7 {
            assert_eq!(all.get(x, 0), [0.0, 0.0, 1.0, 1.0], "{x}");
        }
        // A half-covered pixel (alpha 0.5, not over 128/255) is not a source:
        // it is overwritten, opaque, by its covered neighbour's colour.
        let mut soft = FilterBuffer::transparent(2, 1).unwrap();
        soft.set(0, 0, [0.0, 1.0, 0.0, 1.0]);
        soft.set(1, 0, [0.25, 0.0, 0.0, 0.5]);
        assert_eq!(
            texture_dilation(&soft, 0, 3).get(1, 0),
            [0.0, 1.0, 0.0, 1.0]
        );
        // A covered pixel far from everything is cleared if out of reach.
        let mut far = FilterBuffer::transparent(9, 1).unwrap();
        far.set(0, 0, [1.0, 0.0, 0.0, 1.0]);
        let out = texture_dilation(&far, 0, 3);
        assert_eq!(out.get(8, 0), [0.0; 4]);
        // Radius and crop zero, or nothing covered: unchanged.
        assert_eq!(texture_dilation(&src, 0, 0), src);
        let clear = FilterBuffer::transparent(3, 3).unwrap();
        assert_eq!(texture_dilation(&clear, 2, 5), clear);
    }

    #[test]
    fn crop_refills_a_contaminated_edge_from_further_in() {
        // A 6x1 strip: green, green, a dirty white edge, then clear.
        let mut src = FilterBuffer::transparent(6, 1).unwrap();
        src.set(0, 0, [0.0, 1.0, 0.0, 1.0]);
        src.set(1, 0, [0.0, 1.0, 0.0, 1.0]);
        src.set(2, 0, [1.0, 1.0, 1.0, 1.0]);
        // Without crop the white edge spreads.
        let plain = texture_dilation(&src, 0, 2);
        assert_eq!(plain.get(3, 0), [1.0, 1.0, 1.0, 1.0]);
        // Crop 1 shaves the white edge (it is 1 px from a clear pixel), and
        // green refills it and grows `radius + crop` = 3 px from x = 1.
        let cropped = texture_dilation(&src, 1, 2);
        for x in 0..4 {
            assert_eq!(cropped.get(x, 0), [0.0, 1.0, 0.0, 1.0], "{x}");
        }
        assert_eq!(cropped.get(4, 0), [0.0; 4]);
    }
}
