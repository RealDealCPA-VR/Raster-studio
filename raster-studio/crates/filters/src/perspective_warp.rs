//! Edit ▸ Perspective Warp (W10-G): quads drawn over the layer, then moved.
//!
//! Photoshop's tool has two modes and so does this model:
//!
//! * **Layout** — the user draws one or more quads over the image, each
//!   covering a plane of it (the side of a building, a table top). A quad's
//!   drawn corners are its *source* corners.
//! * **Warp** — the user drags the corners. Where a corner ends up is its
//!   *destination*.
//!
//! Each quad is one projective map: the 3x3 [`Homography`] that sends its four
//! source corners exactly onto its four destination corners, solved as the
//! usual 8x8 linear system (the direct linear transform with `h33 = 1`, see
//! [`Homography::from_quads`]). [`PerspectiveWarp::apply`] resamples by
//! inverse mapping: for every output pixel inside a destination quad it asks
//! that quad's inverse homography where the pixel came from and samples the
//! source bilinearly there. A pixel covered by no destination quad keeps the
//! source pixel, except where a source quad's plane was lifted away and left
//! nothing behind, which becomes transparent — the plane moved, so it is no
//! longer there.
//!
//! A quad whose destination equals its source is the identity map, and the
//! warp of such a quad returns the source (to floating-point accuracy) — the
//! test `a_quad_warped_to_itself_is_the_identity` pins that.

use crate::support::EdgeMode;
use crate::FilterBuffer;

/// A quad's four corners, clockwise from the top left as drawn.
pub type Quad = [[f32; 2]; 4];

/// A projective map of the plane, row-major with `m[8]` the `h33` entry.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Homography {
    m: [f64; 9],
}

impl Homography {
    /// The identity map.
    pub const IDENTITY: Homography = Homography {
        m: [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
    };

    /// The homography sending `src[i]` to `dst[i]` for each of the four
    /// corners, or `None` when the corners are degenerate (three collinear,
    /// a repeated corner, non-finite input).
    ///
    /// Each correspondence `(x, y) -> (u, v)` gives two rows of the system
    /// `A h = b` over the eight unknowns `h11..h32`:
    ///
    /// ```text
    /// [x y 1 0 0 0 -u*x -u*y] h = u
    /// [0 0 0 x y 1 -v*x -v*y] h = v
    /// ```
    ///
    /// solved by Gaussian elimination with partial pivoting in `f64`.
    pub fn from_quads(src: &Quad, dst: &Quad) -> Option<Homography> {
        let mut a = [[0.0f64; 9]; 8];
        for i in 0..4 {
            let (x, y) = (f64::from(src[i][0]), f64::from(src[i][1]));
            let (u, v) = (f64::from(dst[i][0]), f64::from(dst[i][1]));
            if ![x, y, u, v].iter().all(|c| c.is_finite()) {
                return None;
            }
            a[2 * i] = [x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y, u];
            a[2 * i + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y, v];
        }
        let h = solve8(a)?;
        let m = [h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7], 1.0];
        Some(Homography { m })
    }

    /// Map a point. A point on the map's line at infinity has no image and
    /// answers `None`.
    pub fn apply(&self, p: [f64; 2]) -> Option<[f64; 2]> {
        let m = &self.m;
        let w = m[6] * p[0] + m[7] * p[1] + m[8];
        if w.abs() < 1e-12 {
            return None;
        }
        Some([
            (m[0] * p[0] + m[1] * p[1] + m[2]) / w,
            (m[3] * p[0] + m[4] * p[1] + m[5]) / w,
        ])
    }

    /// The inverse map, or `None` when this one is singular.
    pub fn inverse(&self) -> Option<Homography> {
        let m = &self.m;
        let c00 = m[4] * m[8] - m[5] * m[7];
        let c01 = m[5] * m[6] - m[3] * m[8];
        let c02 = m[3] * m[7] - m[4] * m[6];
        let det = m[0] * c00 + m[1] * c01 + m[2] * c02;
        if !det.is_finite() || det.abs() < 1e-18 {
            return None;
        }
        let inv = [
            c00,
            m[2] * m[7] - m[1] * m[8],
            m[1] * m[5] - m[2] * m[4],
            c01,
            m[0] * m[8] - m[2] * m[6],
            m[2] * m[3] - m[0] * m[5],
            c02,
            m[1] * m[6] - m[0] * m[7],
            m[0] * m[4] - m[1] * m[3],
        ];
        let s = 1.0 / det;
        Some(Homography {
            m: inv.map(|v| v * s),
        })
    }

    /// The nine entries, row-major.
    pub fn entries(&self) -> [f64; 9] {
        self.m
    }
}

/// Solve an 8x8 system given as an augmented 8x9 matrix.
fn solve8(mut a: [[f64; 9]; 8]) -> Option<[f64; 8]> {
    for col in 0..8 {
        let pivot = (col..8).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()))?;
        if a[pivot][col].abs() < 1e-10 {
            return None;
        }
        a.swap(col, pivot);
        for row in 0..8 {
            if row == col {
                continue;
            }
            let f = a[row][col] / a[col][col];
            if f == 0.0 {
                continue;
            }
            for k in col..9 {
                a[row][k] -= f * a[col][k];
            }
        }
    }
    let mut x = [0.0; 8];
    for i in 0..8 {
        x[i] = a[i][8] / a[i][i];
        if !x[i].is_finite() {
            return None;
        }
    }
    Some(x)
}

/// Whether `p` lies inside the (possibly non-convex) quad, by the even-odd
/// crossing rule.
pub fn quad_contains(quad: &Quad, p: [f32; 2]) -> bool {
    let mut inside = false;
    let mut j = 3;
    for i in 0..4 {
        let (a, b) = (quad[i], quad[j]);
        if (a[1] > p[1]) != (b[1] > p[1]) {
            let x = a[0] + (p[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0]);
            if p[0] < x {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// One plane of a Perspective Warp: where it was drawn, and where its
/// corners were dragged to.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct WarpQuad {
    /// The corners drawn in Layout mode.
    pub source: Quad,
    /// The corners after Warp mode moved them.
    pub target: Quad,
}

impl WarpQuad {
    /// A quad drawn and not yet moved.
    pub fn new(source: Quad) -> Self {
        Self {
            source,
            target: source,
        }
    }

    /// The axis-aligned rectangle `(x0, y0)`-`(x1, y1)` as a quad.
    pub fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        let (l, r) = (x0.min(x1), x0.max(x1));
        let (t, b) = (y0.min(y1), y0.max(y1));
        Self::new([[l, t], [r, t], [r, b], [l, b]])
    }

    /// Whether the corners never moved.
    pub fn is_identity(&self) -> bool {
        self.source == self.target
    }

    /// The map from source to target, or `None` for a degenerate quad.
    pub fn homography(&self) -> Option<Homography> {
        Homography::from_quads(&self.source, &self.target)
    }
}

/// A whole Perspective Warp: every quad, in drawing order.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct PerspectiveWarp {
    pub quads: Vec<WarpQuad>,
}

impl PerspectiveWarp {
    /// Whether nothing moved (or nothing was drawn).
    pub fn is_identity(&self) -> bool {
        self.quads.iter().all(WarpQuad::is_identity)
    }

    /// The warp scaled by `k` — the dialog edits a preview and the shell
    /// applies at full resolution.
    pub fn scaled(&self, k: f32) -> PerspectiveWarp {
        let s = |q: Quad| q.map(|p| [p[0] * k, p[1] * k]);
        PerspectiveWarp {
            quads: self
                .quads
                .iter()
                .map(|q| WarpQuad {
                    source: s(q.source),
                    target: s(q.target),
                })
                .collect(),
        }
    }

    /// Warp `src`. See the module documentation for the rule. A degenerate
    /// quad is skipped, never a panic.
    pub fn apply(&self, src: &FilterBuffer) -> FilterBuffer {
        let (w, h) = src.dimensions();
        let maps: Vec<(WarpQuad, Homography)> = self
            .quads
            .iter()
            .filter_map(|q| {
                let inv = q.homography()?.inverse()?;
                Some((*q, inv))
            })
            .collect();
        let mut out = src.clone();
        if maps.is_empty() {
            return out;
        }
        let width = w as usize;
        use rayon::prelude::*;
        out.pixels_mut()
            .par_chunks_mut(width.max(1))
            .enumerate()
            .for_each(|(y, row)| {
                for (x, px) in row.iter_mut().enumerate() {
                    let c = [x as f32 + 0.5, y as f32 + 0.5];
                    // The last-drawn quad is on top.
                    let hit = maps
                        .iter()
                        .rev()
                        .find(|(q, _)| quad_contains(&q.target, c));
                    if let Some((_, inv)) = hit {
                        *px = match inv.apply([f64::from(c[0]), f64::from(c[1])]) {
                            Some(s) => {
                                src.sample_bilinear(s[0] as f32, s[1] as f32, EdgeMode::Clamp)
                            }
                            None => [0.0; 4],
                        };
                    } else if maps.iter().any(|(q, _)| quad_contains(&q.source, c)) {
                        *px = [0.0; 4];
                    }
                }
            });
        let _ = h;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn textured(w: u32, h: u32) -> FilterBuffer {
        let mut b = FilterBuffer::transparent(w, h).unwrap();
        for y in 0..h {
            for x in 0..w {
                let v = ((x * 7 + y * 13) % 17) as f32 / 16.0;
                b.set(x, y, [v, 1.0 - v, (x % 5) as f32 / 4.0, 1.0]);
            }
        }
        b
    }

    #[test]
    fn a_quad_warped_to_itself_is_the_identity() {
        let src = textured(48, 40);
        let warp = PerspectiveWarp {
            quads: vec![WarpQuad::new([[4.0, 6.0], [40.0, 3.0], [44.0, 35.0], [7.0, 30.0]])],
        };
        assert!(warp.is_identity());
        let out = warp.apply(&src);
        for (a, b) in out.pixels().iter().zip(src.pixels()) {
            for c in 0..4 {
                assert!((a[c] - b[c]).abs() < 1e-4, "{a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn the_homography_sends_each_corner_to_its_target() {
        let src = [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]];
        let dst = [[2.0, 1.0], [12.0, 3.0], [9.0, 14.0], [-1.0, 9.0]];
        let h = Homography::from_quads(&src, &dst).unwrap();
        for i in 0..4 {
            let p = h
                .apply([f64::from(src[i][0]), f64::from(src[i][1])])
                .unwrap();
            assert!((p[0] - f64::from(dst[i][0])).abs() < 1e-9);
            assert!((p[1] - f64::from(dst[i][1])).abs() < 1e-9);
        }
        let back = h.inverse().unwrap().apply([2.0, 1.0]).unwrap();
        assert!(back[0].abs() < 1e-9 && back[1].abs() < 1e-9);
    }

    #[test]
    fn a_degenerate_quad_has_no_homography_and_warps_nothing() {
        let line = [[0.0, 0.0], [1.0, 1.0], [2.0, 2.0], [3.0, 3.0]];
        assert!(Homography::from_quads(&line, &line).is_none());
        let src = textured(8, 8);
        let warp = PerspectiveWarp {
            quads: vec![WarpQuad {
                source: line,
                target: [[0.0, 0.0], [5.0, 0.0], [5.0, 5.0], [0.0, 5.0]],
            }],
        };
        assert_eq!(warp.apply(&src), src);
    }

    #[test]
    fn moving_a_corner_moves_the_plane() {
        // A bright square in a quad; shifting the whole quad right by 6 moves
        // the square right by 6 and leaves the vacated strip transparent.
        let mut src = FilterBuffer::filled(32, 32, [0.0, 0.0, 0.0, 1.0]).unwrap();
        for y in 10..20 {
            for x in 10..20 {
                src.set(x, y, [1.0, 1.0, 1.0, 1.0]);
            }
        }
        let mut q = WarpQuad::rect(8.0, 8.0, 22.0, 22.0);
        q.target = q.source.map(|p| [p[0] + 6.0, p[1]]);
        let out = PerspectiveWarp { quads: vec![q] }.apply(&src);
        assert!((out.get(17, 15)[0] - 1.0).abs() < 1e-4, "square moved in");
        assert!((out.get(25, 15)[0] - 1.0).abs() < 1e-4, "square moved in");
        assert_eq!(out.get(11, 15)[3], 0.0, "the vacated strip is empty");
        assert_eq!(out.get(9, 15)[3], 0.0, "the vacated strip is empty");
        assert_eq!(out.get(2, 2), src.get(2, 2), "outside every quad is untouched");
    }
}
