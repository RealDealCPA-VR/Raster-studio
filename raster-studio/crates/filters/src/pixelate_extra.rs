//! Pixelate ▸ Facet, Fragment and Mezzotint.
//!
//! * [`facet`] and [`fragment`] work on premultiplied linear pixels: Facet
//!   *selects* an existing neighbour (so it can never invent a colour) and
//!   Fragment averages four shifted copies (a normalised kernel, so a constant
//!   survives).
//! * [`mezzotint`] is a two-tone screen defined on **gamma-encoded** luminance,
//!   like [`crate::pixelate::color_halftone`], and says so here: it is a model
//!   of ink coverage, and the eye judges "half the dots are black" against the
//!   encoded value, not linear light. Colour is unpremultiplied, screened, and
//!   premultiplied back.

use serde::{Deserialize, Serialize};

use color::{linear_srgb_luminance, linear_to_srgb, unpremultiply};

use crate::buffer::FilterBuffer;
use crate::rng::hash_unit;
use crate::support::{accumulate, fill_tiles, scale, EdgeMode};

/// How far, in pixels, each of [`fragment`]'s four copies is shifted along
/// both axes.
pub const FRAGMENT_OFFSET: i64 = 4;

/// Facet: clump neighbouring pixels into flat cells of a single colour.
///
/// Each pixel becomes the *medoid* of its 3x3 neighbourhood — the member
/// whose summed distance to the other eight is smallest. Because the answer is
/// always one of the input pixels, a flat area is untouched, an edge stays an
/// edge, and isolated grain is replaced by whatever its neighbours agree on.
/// Ties keep the centre pixel.
pub fn facet(src: &FilterBuffer, edge: EdgeMode) -> FilterBuffer {
    if src.is_empty() {
        return src.clone();
    }
    let (w, h) = src.dimensions();
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let mut window = [[0.0f32; 4]; 9];
        window[0] = src.get(x, y);
        let mut n = 1;
        for oy in -1i64..=1 {
            for ox in -1i64..=1 {
                if ox == 0 && oy == 0 {
                    continue;
                }
                window[n] = src.at(x as i64 + ox, y as i64 + oy, edge);
                n += 1;
            }
        }
        let mut best = 0;
        let mut best_cost = f32::INFINITY;
        for (i, candidate) in window.iter().enumerate() {
            let cost: f32 = window
                .iter()
                .map(|other| {
                    candidate
                        .iter()
                        .zip(other.iter())
                        .map(|(a, b)| (a - b).abs())
                        .sum::<f32>()
                })
                .sum();
            if cost < best_cost {
                best_cost = cost;
                best = i;
            }
        }
        window[best]
    });
    out
}

/// Fragment: the mean of four copies of the image, each shifted
/// [`FRAGMENT_OFFSET`] pixels along one diagonal.
///
/// A normalised four-tap kernel, so a flat frame is exactly flat afterwards.
pub fn fragment(src: &FilterBuffer, edge: EdgeMode) -> FilterBuffer {
    if src.is_empty() {
        return src.clone();
    }
    let d = FRAGMENT_OFFSET;
    let (w, h) = src.dimensions();
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let mut acc = [0.0f32; 4];
        for (ox, oy) in [(-d, -d), (d, -d), (-d, d), (d, d)] {
            accumulate(&mut acc, src.at(x as i64 + ox, y as i64 + oy, edge), 1.0);
        }
        scale(acc, 0.25)
    });
    out
}

/// The screen [`mezzotint`] lays down.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MezzotintType {
    FineDots,
    #[default]
    MediumDots,
    GrainyDots,
    CoarseDots,
    ShortLines,
    MediumLines,
    LongLines,
    ShortStrokes,
    MediumStrokes,
    LongStrokes,
}

impl MezzotintType {
    pub const ALL: &'static [MezzotintType] = &[
        MezzotintType::FineDots,
        MezzotintType::MediumDots,
        MezzotintType::GrainyDots,
        MezzotintType::CoarseDots,
        MezzotintType::ShortLines,
        MezzotintType::MediumLines,
        MezzotintType::LongLines,
        MezzotintType::ShortStrokes,
        MezzotintType::MediumStrokes,
        MezzotintType::LongStrokes,
    ];

    /// The cell one random threshold covers: `(along, across)` in pixels,
    /// and whether the cell runs along the diagonal.
    const fn cell(self) -> (i64, i64, bool) {
        match self {
            MezzotintType::FineDots => (1, 1, false),
            MezzotintType::MediumDots => (2, 2, false),
            MezzotintType::GrainyDots => (1, 1, false),
            MezzotintType::CoarseDots => (3, 3, false),
            MezzotintType::ShortLines => (4, 1, false),
            MezzotintType::MediumLines => (8, 1, false),
            MezzotintType::LongLines => (16, 1, false),
            MezzotintType::ShortStrokes => (4, 1, true),
            MezzotintType::MediumStrokes => (8, 1, true),
            MezzotintType::LongStrokes => (16, 1, true),
        }
    }
}

/// Mezzotint: a random two-tone screen.
///
/// Each pixel's encoded luminance is compared with a random threshold that
/// is constant over one screen cell (a dot, a run of pixels for the line
/// types, a diagonal run for the strokes); brighter than the threshold is
/// white, otherwise black. The result is monochrome — every pixel is either
/// black or white at the source's own alpha — and a mid-grey field comes out
/// as a roughly even mix of the two. The threshold is a pure function of the
/// seed and the cell, so the pattern is reproducible and tile-stable.
pub fn mezzotint(src: &FilterBuffer, kind: MezzotintType, seed: u64) -> FilterBuffer {
    if src.is_empty() {
        return src.clone();
    }
    let (along, across, diagonal) = kind.cell();
    let grainy = kind == MezzotintType::GrainyDots;
    let (w, h) = src.dimensions();
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let px = src.get(x, y);
        let alpha = px[3];
        let straight = unpremultiply(px);
        let lum = linear_srgb_luminance([straight[0], straight[1], straight[2]]).clamp(0.0, 1.0);
        let value = linear_to_srgb(lum);
        let (xi, yi) = (x as i64, y as i64);
        let (u, v) = if diagonal {
            (xi + yi, xi - yi)
        } else {
            (xi, yi)
        };
        let cell = (u.div_euclid(along), v.div_euclid(across));
        let mut threshold = hash_unit(seed, cell.0, cell.1);
        if grainy {
            // Two independent draws averaged: a threshold that clusters
            // around the middle, so mid tones break into grain and the
            // extremes stay solid.
            threshold = 0.5 * (threshold + hash_unit(seed ^ 0x9e37_79b9, cell.0, cell.1));
        }
        let on = if value > threshold { alpha } else { 0.0 };
        [on, on, on, alpha]
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noisy(w: u32, h: u32) -> FilterBuffer {
        let mut px = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let n = hash_unit(5, x as i64, y as i64);
                px.push([0.2 + 0.6 * n, 0.5, 0.8 - 0.6 * n, 1.0]);
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
    fn facet_picks_a_neighbour_and_clumps_grain() {
        let src = noisy(24, 20);
        let out = facet(&src, EdgeMode::Clamp);
        assert_ne!(out.pixels(), src.pixels(), "Facet changed nothing");
        assert!(variance(&out) < variance(&src));
        for y in 0..20u32 {
            for x in 0..24u32 {
                let o = out.get(x, y);
                let found = (-1i64..=1).any(|oy| {
                    (-1i64..=1).any(|ox| src.at(x as i64 + ox, y as i64 + oy, EdgeMode::Clamp) == o)
                });
                assert!(found, "({x},{y}) is a colour that was not in its window");
            }
        }
        let flat = FilterBuffer::filled(6, 6, [0.3, 0.2, 0.1, 0.9]).unwrap();
        assert_eq!(facet(&flat, EdgeMode::Wrap).pixels(), flat.pixels());
    }

    #[test]
    fn fragment_of_a_flat_frame_is_flat_and_of_an_edge_is_not() {
        let flat = FilterBuffer::filled(17, 11, [0.21, 0.34, 0.55, 0.8]).unwrap();
        for edge in [EdgeMode::Clamp, EdgeMode::Wrap, EdgeMode::Mirror] {
            let out = fragment(&flat, edge);
            for p in out.pixels() {
                for (v, want) in p.iter().zip(flat.pixels()[0].iter()) {
                    assert!((v - want).abs() < 1e-6, "{edge:?}");
                }
            }
        }
        let mut step = FilterBuffer::filled(20, 8, [0.1, 0.1, 0.1, 1.0]).unwrap();
        for y in 0..8 {
            for x in 10..20 {
                step.set(x, y, [0.9, 0.9, 0.9, 1.0]);
            }
        }
        let out = fragment(&step, EdgeMode::Clamp);
        assert_ne!(out.pixels(), step.pixels());
        // Two copies from each side meet at the edge: exactly the mean.
        assert!((out.get(10, 4)[0] - 0.5).abs() < 1e-6);
        assert!((out.get(9, 4)[0] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn mezzotint_is_a_monochrome_two_tone_pattern() {
        let grey = FilterBuffer::filled(32, 32, [0.2, 0.2, 0.2, 1.0]).unwrap();
        for kind in MezzotintType::ALL {
            let out = mezzotint(&grey, *kind, 9);
            let mut white = 0usize;
            for p in out.pixels() {
                assert_eq!(p[0], p[1], "{kind:?}");
                assert_eq!(p[1], p[2], "{kind:?}");
                assert_eq!(p[3], 1.0, "{kind:?}");
                assert!(p[0] == 0.0 || p[0] == 1.0, "{kind:?}: {p:?}");
                if p[0] == 1.0 {
                    white += 1;
                }
            }
            // Linear 0.2 encodes to about 0.48: roughly half the screen is on.
            assert!(
                (200..=800).contains(&white),
                "{kind:?}: {white} of 1024 pixels are white"
            );
            assert_eq!(
                mezzotint(&grey, *kind, 9).pixels(),
                out.pixels(),
                "{kind:?} is not deterministic"
            );
            assert_ne!(
                mezzotint(&grey, *kind, 10).pixels(),
                out.pixels(),
                "{kind:?} ignores the seed"
            );
        }
        // The line types run along a row: the threshold is constant across a
        // run, so a row of constant grey is constant across each run.
        let lines = mezzotint(&grey, MezzotintType::LongLines, 9);
        for run in 0..2u32 {
            let first = lines.get(run * 16, 3)[0];
            for x in run * 16..run * 16 + 16 {
                assert_eq!(lines.get(x, 3)[0], first);
            }
        }
        // Alpha is honoured: a half-covered pixel is black or half-white.
        let faint = FilterBuffer::filled(8, 8, [0.1, 0.1, 0.1, 0.5]).unwrap();
        for p in mezzotint(&faint, MezzotintType::FineDots, 1).pixels() {
            assert_eq!(p[3], 0.5);
            assert!(p[0] == 0.0 || p[0] == 0.5, "{p:?}");
        }
    }

    #[test]
    fn degenerate_buffers_do_not_panic() {
        let empty = FilterBuffer::transparent(0, 0).unwrap();
        let one = FilterBuffer::filled(1, 1, [0.4, 0.3, 0.2, 1.0]).unwrap();
        for src in [&empty, &one] {
            let _ = facet(src, EdgeMode::Mirror);
            let _ = fragment(src, EdgeMode::Wrap);
            let _ = mezzotint(src, MezzotintType::LongStrokes, 0);
        }
        assert_eq!(fragment(&one, EdgeMode::Clamp).pixels(), one.pixels());
    }
}
