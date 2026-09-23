//! The one-click filters: Average, Blur, Blur More, Sharpen, Sharpen More and
//! Sharpen Edges.
//!
//! These are the Photopea/Photoshop rows that carry no dialog. Each is a
//! fixed small kernel, so a click does one predictable thing and the user
//! reaches for Gaussian Blur or Unsharp Mask when they want a control.
//!
//! All six work on premultiplied linear pixels. The blurs are normalised
//! kernels, so a constant image survives them (the crate invariant). The
//! sharpens add back what a [`blur`] removed and can overshoot, so they
//! re-clamp through [`clamp_premultiplied`] like the sharpens in
//! [`crate::sharpen`] do.

use crate::buffer::{clamp_premultiplied, FilterBuffer};
use crate::support::{fill_tiles, max_abs_diff, sub, EdgeMode};

/// The 3x3 binomial kernel [`blur`] uses: `[1 2 1] ⊗ [1 2 1] / 16`.
const BLUR_TAPS: [f32; 3] = [0.25, 0.5, 0.25];

/// The 5x5 binomial kernel [`blur_more`] uses: `[1 4 6 4 1] ⊗ [1 4 6 4 1] / 256`.
const BLUR_MORE_TAPS: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];

/// How much of the removed detail [`sharpen`] adds back.
const SHARPEN_AMOUNT: f32 = 1.0;

/// How much [`sharpen_more`] adds back — the same kernel, two and a half
/// times the strength.
const SHARPEN_MORE_AMOUNT: f32 = 2.5;

/// The local contrast, in linear premultiplied units, below which
/// [`sharpen_edges`] leaves a pixel exactly as it found it.
pub const SHARPEN_EDGES_THRESHOLD: f32 = 0.02;

/// Average: the whole buffer's mean colour, everywhere.
///
/// The mean is over premultiplied pixels, which is the colour the buffer
/// composites to when it is shrunk to one pixel. An empty buffer is returned
/// unchanged.
pub fn average(src: &FilterBuffer) -> FilterBuffer {
    if src.is_empty() {
        return src.clone();
    }
    let mut sum = [0.0f64; 4];
    for px in src.pixels() {
        for (acc, v) in sum.iter_mut().zip(px.iter()) {
            *acc += f64::from(*v);
        }
    }
    let n = src.len() as f64;
    let mean = [
        (sum[0] / n) as f32,
        (sum[1] / n) as f32,
        (sum[2] / n) as f32,
        (sum[3] / n) as f32,
    ];
    let (w, h) = src.dimensions();
    FilterBuffer::filled(w, h, mean).expect("the source had a valid size")
}

/// Blur: a fixed 3x3 binomial kernel.
pub fn blur(src: &FilterBuffer, edge: EdgeMode) -> FilterBuffer {
    separable(src, &BLUR_TAPS, edge)
}

/// Blur More: a fixed 5x5 binomial kernel, noticeably softer than [`blur`].
pub fn blur_more(src: &FilterBuffer, edge: EdgeMode) -> FilterBuffer {
    separable(src, &BLUR_MORE_TAPS, edge)
}

/// Sharpen: `src + (src - blur(src))`, clamped back into a valid pixel.
pub fn sharpen(src: &FilterBuffer, edge: EdgeMode) -> FilterBuffer {
    sharpen_by(src, SHARPEN_AMOUNT, edge)
}

/// Sharpen More: the same as [`sharpen`] at two and a half times the amount.
pub fn sharpen_more(src: &FilterBuffer, edge: EdgeMode) -> FilterBuffer {
    sharpen_by(src, SHARPEN_MORE_AMOUNT, edge)
}

/// Sharpen Edges: [`sharpen`] applied only where there is an edge to sharpen.
///
/// A pixel whose 3x3 neighbourhood differs from it by less than
/// [`SHARPEN_EDGES_THRESHOLD`] in every channel is copied through bit for bit
/// — not "sharpened by a small amount", copied — so flat areas, and the noise
/// in them, are exactly what they were.
pub fn sharpen_edges(src: &FilterBuffer, edge: EdgeMode) -> FilterBuffer {
    if src.is_empty() {
        return src.clone();
    }
    let blurred = blur(src, edge);
    let (w, h) = src.dimensions();
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let s = src.get(x, y);
        let mut contrast = 0.0f32;
        for oy in -1i64..=1 {
            for ox in -1i64..=1 {
                if ox == 0 && oy == 0 {
                    continue;
                }
                let n = src.at(x as i64 + ox, y as i64 + oy, edge);
                contrast = contrast.max(max_abs_diff(s, n));
            }
        }
        if contrast < SHARPEN_EDGES_THRESHOLD {
            return s;
        }
        add_back(s, blurred.get(x, y), SHARPEN_AMOUNT)
    });
    out
}

fn sharpen_by(src: &FilterBuffer, amount: f32, edge: EdgeMode) -> FilterBuffer {
    if src.is_empty() {
        return src.clone();
    }
    let blurred = blur(src, edge);
    let (w, h) = src.dimensions();
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        add_back(src.get(x, y), blurred.get(x, y), amount)
    });
    out
}

#[inline]
fn add_back(s: [f32; 4], blurred: [f32; 4], amount: f32) -> [f32; 4] {
    let d = sub(s, blurred);
    clamp_premultiplied([
        s[0] + d[0] * amount,
        s[1] + d[1] * amount,
        s[2] + d[2] * amount,
        s[3] + d[3] * amount,
    ])
}

/// A separable convolution with an odd, normalised tap list, horizontal then
/// vertical.
fn separable(src: &FilterBuffer, taps: &[f32], edge: EdgeMode) -> FilterBuffer {
    if src.is_empty() {
        return src.clone();
    }
    let half = (taps.len() / 2) as i64;
    let (w, h) = src.dimensions();
    let mut pass = src.same_size_blank();
    fill_tiles(w, h, pass.pixels_mut(), |x, y| {
        let mut acc = [0.0f32; 4];
        for (i, k) in taps.iter().enumerate() {
            let p = src.at(x as i64 + i as i64 - half, y as i64, edge);
            for (a, v) in acc.iter_mut().zip(p.iter()) {
                *a += v * k;
            }
        }
        acc
    });
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let mut acc = [0.0f32; 4];
        for (i, k) in taps.iter().enumerate() {
            let p = pass.at(x as i64, y as i64 + i as i64 - half, edge);
            for (a, v) in acc.iter_mut().zip(p.iter()) {
                *a += v * k;
            }
        }
        acc
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic noisy field around mid grey.
    fn noisy(w: u32, h: u32) -> FilterBuffer {
        let mut px = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let n = crate::rng::hash_unit(11, x as i64, y as i64) - 0.5;
                let v = 0.5 + 0.4 * n;
                px.push([v, v * 0.8, v * 0.6, 1.0]);
            }
        }
        FilterBuffer::from_pixels(w, h, px).unwrap()
    }

    /// One hard vertical edge between two flat plateaux.
    fn step(w: u32, h: u32) -> FilterBuffer {
        let mut px = Vec::new();
        for _ in 0..h {
            for x in 0..w {
                let v = if x < w / 2 { 0.2f32 } else { 0.7 };
                px.push([v, v, v, 1.0]);
            }
        }
        FilterBuffer::from_pixels(w, h, px).unwrap()
    }

    fn variance(buf: &FilterBuffer) -> f64 {
        let n = buf.len() as f64;
        let mean = buf.pixels().iter().map(|p| f64::from(p[0])).sum::<f64>() / n;
        buf.pixels()
            .iter()
            .map(|p| (f64::from(p[0]) - mean).powi(2))
            .sum::<f64>()
            / n
    }

    #[test]
    fn average_yields_a_flat_frame_equal_to_the_mean() {
        let src = noisy(23, 17);
        let n = src.len() as f64;
        let mut expected = [0.0f64; 4];
        for p in src.pixels() {
            for c in 0..4 {
                expected[c] += f64::from(p[c]) / n;
            }
        }
        let out = average(&src);
        assert_eq!(out.dimensions(), src.dimensions());
        let first = out.pixels()[0];
        for p in out.pixels() {
            assert_eq!(*p, first, "Average is not flat");
        }
        for c in 0..4 {
            assert!(
                (f64::from(first[c]) - expected[c]).abs() < 1e-5,
                "channel {c}: {} vs mean {}",
                first[c],
                expected[c]
            );
        }
        assert!(variance(&out) < 1e-12);
        assert!(
            variance(&src) > 1e-3,
            "the fixture must not already be flat"
        );
    }

    #[test]
    fn blur_more_reduces_variance_more_than_blur() {
        let src = noisy(40, 30);
        let v0 = variance(&src);
        let v1 = variance(&blur(&src, EdgeMode::Clamp));
        let v2 = variance(&blur_more(&src, EdgeMode::Clamp));
        assert!(v1 < v0, "Blur did not smooth: {v1} >= {v0}");
        assert!(v2 < v1, "Blur More is not softer than Blur: {v2} >= {v1}");
    }

    #[test]
    fn both_blurs_preserve_a_constant_under_every_edge_mode() {
        let px = [0.21, 0.34, 0.55, 0.8];
        for edge in [EdgeMode::Clamp, EdgeMode::Wrap, EdgeMode::Mirror] {
            let src = FilterBuffer::filled(9, 7, px).unwrap();
            for out in [blur(&src, edge), blur_more(&src, edge)] {
                for p in out.pixels() {
                    for c in 0..4 {
                        assert!((p[c] - px[c]).abs() < 1e-5, "{edge:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn sharpen_more_widens_an_edge_more_than_sharpen() {
        let src = step(16, 5);
        let (l, r) = (7u32, 8u32);
        let contrast = |b: &FilterBuffer| b.get(r, 2)[0] - b.get(l, 2)[0];
        let c0 = contrast(&src);
        let c1 = contrast(&sharpen(&src, EdgeMode::Clamp));
        let c2 = contrast(&sharpen_more(&src, EdgeMode::Clamp));
        assert!(
            c1 > c0 + 1e-3,
            "Sharpen did not raise the edge: {c1} vs {c0}"
        );
        assert!(c2 > c1 + 1e-3, "Sharpen More is not stronger: {c2} vs {c1}");
    }

    #[test]
    fn sharpen_edges_leaves_flat_regions_byte_identical_and_touches_the_edge() {
        let src = step(24, 6);
        let out = sharpen_edges(&src, EdgeMode::Clamp);
        let mut touched = 0;
        for y in 0..6 {
            for x in 0..24u32 {
                let (s, o) = (src.get(x, y), out.get(x, y));
                if (10..14).contains(&x) {
                    if s != o {
                        touched += 1;
                    }
                } else {
                    assert_eq!(s, o, "flat pixel ({x},{y}) was changed");
                }
            }
        }
        assert!(touched > 0, "the edge itself was not sharpened");
        // And the flat-with-tiny-noise case: below the threshold is untouched.
        let mut quiet = FilterBuffer::filled(12, 12, [0.4, 0.4, 0.4, 1.0]).unwrap();
        quiet.set(5, 5, [0.405, 0.4, 0.4, 1.0]);
        assert_eq!(
            sharpen_edges(&quiet, EdgeMode::Clamp).pixels(),
            quiet.pixels(),
            "sub-threshold noise was sharpened"
        );
    }

    #[test]
    fn nothing_panics_on_degenerate_buffers() {
        let one = FilterBuffer::filled(1, 1, [0.3, 0.2, 0.1, 1.0]).unwrap();
        let empty = FilterBuffer::transparent(0, 0).unwrap();
        for src in [&one, &empty] {
            let _ = average(src);
            let _ = blur(src, EdgeMode::Wrap);
            let _ = blur_more(src, EdgeMode::Mirror);
            let _ = sharpen(src, EdgeMode::Clamp);
            let _ = sharpen_more(src, EdgeMode::Clamp);
            let _ = sharpen_edges(src, EdgeMode::Clamp);
        }
        assert_eq!(average(&one).pixels(), one.pixels());
    }
}
