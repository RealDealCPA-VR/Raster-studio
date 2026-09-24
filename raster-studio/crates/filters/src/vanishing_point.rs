//! Vanishing Point: edit inside a perspective plane.
//!
//! A [`PerspectivePlane`] is four corners in image pixels — top-left,
//! top-right, bottom-right, bottom-left of a rectangle seen in perspective.
//! Its [`Homography`] maps the unit square `(u, v) in [0, 1]^2` onto that
//! quadrilateral, so plane coordinates are the rectangle's own coordinates:
//! equal steps in `u` are equal steps along the real surface, getting
//! shorter on screen as the surface recedes.
//!
//! Every edit is expressed in plane coordinates and rendered by inverse
//! mapping each destination pixel through the homography, which is what
//! makes it *perspective-correct*: a pasted image or a cloned patch shrinks
//! and foreshortens exactly as the plane does.
//!
//! * [`VanishingOp::Paste`] draws an image into a `u, v` rectangle of the
//!   plane (source-over).
//! * [`VanishingOp::Clone`] is one dab of the perspective clone stamp: the
//!   disc of plane radius `radius` around `to` receives what lies around
//!   `from`, both measured on the plane.
//!
//! A [`VanishingPointEdit`] is a plane plus its ops, applied in order to the
//! layer; the shell applies it as one undo step.

use serde::{Deserialize, Serialize};

use crate::buffer::FilterBuffer;
use crate::support::{EdgeMode, Interpolation, Sampling};

/// A 3x3 projective transform, row-major, acting on `(x, y, 1)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Homography(pub [f64; 9]);

impl Homography {
    /// The transform taking the unit square's corners `(0,0), (1,0), (1,1),
    /// (0,1)` to `quad[0..4]` in that order (Heckbert's square-to-quad).
    /// `None` when the quad is degenerate (three collinear corners).
    pub fn unit_square_to_quad(quad: [[f64; 2]; 4]) -> Option<Self> {
        let [[x0, y0], [x1, y1], [x2, y2], [x3, y3]] = quad;
        let sx = x0 - x1 + x2 - x3;
        let sy = y0 - y1 + y2 - y3;
        let (a, b, c, d, e, f, g, h);
        if sx.abs() < 1e-12 && sy.abs() < 1e-12 {
            // Affine.
            a = x1 - x0;
            b = x2 - x1;
            c = x0;
            d = y1 - y0;
            e = y2 - y1;
            f = y0;
            g = 0.0;
            h = 0.0;
        } else {
            let dx1 = x1 - x2;
            let dx2 = x3 - x2;
            let dy1 = y1 - y2;
            let dy2 = y3 - y2;
            let den = dx1 * dy2 - dx2 * dy1;
            if den.abs() < 1e-12 {
                return None;
            }
            g = (sx * dy2 - dx2 * sy) / den;
            h = (dx1 * sy - sx * dy1) / den;
            a = x1 - x0 + g * x1;
            b = x3 - x0 + h * x3;
            c = x0;
            d = y1 - y0 + g * y1;
            e = y3 - y0 + h * y3;
            f = y0;
        }
        let m = Homography([a, b, c, d, e, f, g, h, 1.0]);
        (m.det().abs() > 1e-12 && m.0.iter().all(|v| v.is_finite())).then_some(m)
    }

    fn det(&self) -> f64 {
        let m = &self.0;
        m[0] * (m[4] * m[8] - m[5] * m[7]) - m[1] * (m[3] * m[8] - m[5] * m[6])
            + m[2] * (m[3] * m[7] - m[4] * m[6])
    }

    /// Map a point; `None` when it lands on the line at infinity.
    pub fn map(&self, p: [f64; 2]) -> Option<[f64; 2]> {
        let m = &self.0;
        let w = m[6] * p[0] + m[7] * p[1] + m[8];
        if w.abs() < 1e-12 {
            return None;
        }
        Some([
            (m[0] * p[0] + m[1] * p[1] + m[2]) / w,
            (m[3] * p[0] + m[4] * p[1] + m[5]) / w,
        ])
    }

    /// The inverse transform (adjugate over determinant).
    pub fn inverse(&self) -> Option<Self> {
        let det = self.det();
        if det.abs() < 1e-18 || !det.is_finite() {
            return None;
        }
        let m = &self.0;
        let inv = [
            m[4] * m[8] - m[5] * m[7],
            m[2] * m[7] - m[1] * m[8],
            m[1] * m[5] - m[2] * m[4],
            m[5] * m[6] - m[3] * m[8],
            m[0] * m[8] - m[2] * m[6],
            m[2] * m[3] - m[0] * m[5],
            m[3] * m[7] - m[4] * m[6],
            m[1] * m[6] - m[0] * m[7],
            m[0] * m[4] - m[1] * m[3],
        ];
        Some(Homography(inv.map(|v| v / det)))
    }
}

/// A rectangle seen in perspective: its four corners in image pixels, in the
/// order top-left, top-right, bottom-right, bottom-left.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PerspectivePlane {
    pub corners: [[f32; 2]; 4],
}

impl PerspectivePlane {
    /// The axis-aligned plane inset by a quarter of each side — the default a
    /// fresh Vanishing Point dialog draws.
    pub fn centered(width: u32, height: u32) -> Self {
        let (w, h) = (width as f32, height as f32);
        PerspectivePlane {
            corners: [
                [w * 0.25, h * 0.25],
                [w * 0.75, h * 0.25],
                [w * 0.75, h * 0.75],
                [w * 0.25, h * 0.75],
            ],
        }
    }

    /// Whether the corners make a usable plane: finite, and a convex
    /// quadrilateral whose corners wind the same way (a bow-tie or a
    /// collapsed side is refused, as Photoshop draws such a plane red).
    pub fn is_valid(&self) -> bool {
        if !self.corners.iter().flatten().all(|v| v.is_finite()) {
            return false;
        }
        let c = self.corners.map(|p| [f64::from(p[0]), f64::from(p[1])]);
        let mut sign = 0.0f64;
        for i in 0..4 {
            let a = c[i];
            let b = c[(i + 1) % 4];
            let d = c[(i + 2) % 4];
            let cross = (b[0] - a[0]) * (d[1] - b[1]) - (b[1] - a[1]) * (d[0] - b[0]);
            if cross.abs() < 1e-6 {
                return false;
            }
            if sign == 0.0 {
                sign = cross.signum();
            } else if cross.signum() != sign {
                return false;
            }
        }
        self.homography().is_some()
    }

    /// Unit square to this plane.
    pub fn homography(&self) -> Option<Homography> {
        Homography::unit_square_to_quad(self.corners.map(|p| [f64::from(p[0]), f64::from(p[1])]))
    }

    /// Plane coordinates to image pixels.
    pub fn to_image(&self, uv: [f32; 2]) -> Option<[f32; 2]> {
        let p = self
            .homography()?
            .map([f64::from(uv[0]), f64::from(uv[1])])?;
        Some([p[0] as f32, p[1] as f32])
    }

    /// Image pixels to plane coordinates.
    pub fn to_plane(&self, p: [f32; 2]) -> Option<[f32; 2]> {
        let uv = self
            .homography()?
            .inverse()?
            .map([f64::from(p[0]), f64::from(p[1])])?;
        Some([uv[0] as f32, uv[1] as f32])
    }

    /// The grid Photoshop draws over a plane: `divisions + 1` lines in each
    /// direction, evenly spaced *on the plane* (so they bunch up with depth),
    /// as image-space segments.
    pub fn grid_lines(&self, divisions: u32) -> Vec<[[f32; 2]; 2]> {
        let n = divisions.clamp(1, 256);
        let mut out = Vec::new();
        for i in 0..=n {
            let t = i as f32 / n as f32;
            for (a, b) in [([t, 0.0], [t, 1.0]), ([0.0, t], [1.0, t])] {
                if let (Some(p), Some(q)) = (self.to_image(a), self.to_image(b)) {
                    out.push([p, q]);
                }
            }
        }
        out
    }

    /// The plane scaled about the origin, for mapping a plane laid out on a
    /// preview back onto the full-resolution image.
    pub fn scaled(&self, k: f32) -> Self {
        PerspectivePlane {
            corners: self.corners.map(|p| [p[0] * k, p[1] * k]),
        }
    }

    fn bounds(&self, w: u32, h: u32) -> (u32, u32, u32, u32) {
        let xs = self.corners.map(|p| p[0]);
        let ys = self.corners.map(|p| p[1]);
        let min = |v: [f32; 4]| v.iter().copied().fold(f32::MAX, f32::min);
        let max = |v: [f32; 4]| v.iter().copied().fold(f32::MIN, f32::max);
        let x0 = min(xs).floor().clamp(0.0, w as f32) as u32;
        let y0 = min(ys).floor().clamp(0.0, h as f32) as u32;
        let x1 = max(xs).ceil().clamp(0.0, w as f32) as u32;
        let y1 = max(ys).ceil().clamp(0.0, h as f32) as u32;
        (x0, y0, x1, y1)
    }
}

/// An image to paste, premultiplied linear like every [`FilterBuffer`].
pub type PasteImage = FilterBuffer;

/// One edit inside the plane.
#[derive(Debug, Clone, PartialEq)]
pub enum VanishingOp {
    /// Draw `image` into the plane rectangle `[u0, v0, u1, v1]`, source-over,
    /// resampled bilinearly through the homography.
    Paste { image: PasteImage, rect: [f32; 4] },
    /// One perspective clone-stamp dab: the plane disc of radius `radius`
    /// around `to` takes the pixels around `from`, with a soft rim over the
    /// outer fifth of the radius.
    Clone {
        from: [f32; 2],
        to: [f32; 2],
        radius: f32,
    },
}

/// A plane and its edits: the whole of one Vanishing Point session.
#[derive(Debug, Clone, PartialEq)]
pub struct VanishingPointEdit {
    pub plane: PerspectivePlane,
    pub ops: Vec<VanishingOp>,
}

impl VanishingPointEdit {
    /// Whether applying would change nothing: no ops, or no usable plane.
    pub fn is_identity(&self) -> bool {
        self.ops.is_empty() || !self.plane.is_valid()
    }

    /// Apply every op in order. An invalid plane is the identity.
    pub fn apply(&self, src: &FilterBuffer) -> FilterBuffer {
        let mut out = src.clone();
        if src.is_empty() || !self.plane.is_valid() {
            return out;
        }
        for op in &self.ops {
            out = apply_op(&out, &self.plane, op);
        }
        out
    }
}

fn apply_op(src: &FilterBuffer, plane: &PerspectivePlane, op: &VanishingOp) -> FilterBuffer {
    let (Some(fwd), Some(inv)) = (
        plane.homography(),
        plane.homography().and_then(|h| h.inverse()),
    ) else {
        return src.clone();
    };
    let (w, h) = src.dimensions();
    let (x0, y0, x1, y1) = plane.bounds(w, h);
    let mut out = src.clone();
    let bilinear = Sampling::new(EdgeMode::Clamp, Interpolation::Bilinear);
    for y in y0..y1 {
        for x in x0..x1 {
            let Some(uv) = inv.map([f64::from(x) + 0.5, f64::from(y) + 0.5]) else {
                continue;
            };
            let (u, v) = (uv[0] as f32, uv[1] as f32);
            if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
                continue;
            }
            let dst = src.get(x, y);
            let px = match op {
                VanishingOp::Paste { image, rect } => {
                    let [u0, v0, u1, v1] = *rect;
                    if image.is_empty() || u1 <= u0 || v1 <= v0 || u < u0 || u > u1 || v < v0 || v > v1
                    {
                        continue;
                    }
                    let (iw, ih) = image.dimensions();
                    let s = image.sample(
                        (u - u0) / (u1 - u0) * iw as f32,
                        (v - v0) / (v1 - v0) * ih as f32,
                        bilinear,
                    );
                    let k = 1.0 - s[3];
                    [
                        s[0] + dst[0] * k,
                        s[1] + dst[1] * k,
                        s[2] + dst[2] * k,
                        s[3] + dst[3] * k,
                    ]
                }
                VanishingOp::Clone { from, to, radius } => {
                    let r = *radius;
                    if !(r.is_finite() && r > 0.0) {
                        continue;
                    }
                    let d = ((u - to[0]).powi(2) + (v - to[1]).powi(2)).sqrt();
                    if d > r {
                        continue;
                    }
                    let weight = 1.0 - ((d - 0.8 * r) / (0.2 * r)).clamp(0.0, 1.0);
                    let su = u - to[0] + from[0];
                    let sv = v - to[1] + from[1];
                    let Some(sp) = fwd.map([f64::from(su), f64::from(sv)]) else {
                        continue;
                    };
                    let s = src.sample(sp[0] as f32, sp[1] as f32, bilinear);
                    [
                        dst[0] + (s[0] - dst[0]) * weight,
                        dst[1] + (s[1] - dst[1]) * weight,
                        dst[2] + (s[2] - dst[2]) * weight,
                        dst[3] + (s[3] - dst[3]) * weight,
                    ]
                }
            };
            out.set(x, y, px);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trapezoid: a floor receding toward the top of the image.
    fn floor() -> PerspectivePlane {
        PerspectivePlane {
            corners: [[40.0, 20.0], [80.0, 20.0], [110.0, 90.0], [10.0, 90.0]],
        }
    }

    #[test]
    fn the_homography_maps_the_unit_square_onto_the_corners() {
        let plane = floor();
        let h = plane.homography().unwrap();
        let unit = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
        for (uv, corner) in unit.iter().zip(plane.corners) {
            let p = h.map(*uv).unwrap();
            assert!((p[0] - f64::from(corner[0])).abs() < 1e-9, "{p:?}");
            assert!((p[1] - f64::from(corner[1])).abs() < 1e-9, "{p:?}");
        }
        // Round trip through the inverse.
        let back = plane.to_plane(plane.to_image([0.3, 0.7]).unwrap()).unwrap();
        assert!((back[0] - 0.3).abs() < 1e-5 && (back[1] - 0.7).abs() < 1e-5);
        // Perspective, not affine: the plane's centre is not the corners'
        // average — it sits nearer the far (short) edge.
        let centre = plane.to_image([0.5, 0.5]).unwrap();
        assert!(centre[1] < 55.0 - 1.0, "{centre:?}");
    }

    #[test]
    fn validity_refuses_bow_ties_and_collapsed_planes() {
        assert!(floor().is_valid());
        let mut bow = floor();
        bow.corners.swap(2, 3);
        assert!(!bow.is_valid());
        let flat = PerspectivePlane {
            corners: [[0.0, 0.0], [10.0, 0.0], [20.0, 0.0], [30.0, 0.0]],
        };
        assert!(!flat.is_valid());
        let nan = PerspectivePlane {
            corners: [[f32::NAN, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]],
        };
        assert!(!nan.is_valid());
    }

    #[test]
    fn a_paste_lands_with_the_planes_homography() {
        let src = FilterBuffer::filled(120, 100, [0.0, 0.0, 1.0, 1.0]).unwrap();
        let red = FilterBuffer::filled(8, 8, [1.0, 0.0, 0.0, 1.0]).unwrap();
        let plane = floor();
        let edit = VanishingPointEdit {
            plane,
            ops: vec![VanishingOp::Paste {
                image: red,
                rect: [0.5, 0.5, 1.0, 1.0],
            }],
        };
        let out = edit.apply(&src);
        let at = |uv: [f32; 2]| {
            let p = plane.to_image(uv).unwrap();
            out.get(p[0] as u32, p[1] as u32)
        };
        // Just inside each corner of the pasted rectangle: red. The corners
        // are where the homography puts them, not where an affine guess
        // would.
        for uv in [[0.53, 0.53], [0.97, 0.53], [0.97, 0.97], [0.53, 0.97]] {
            assert_eq!(at(uv), [1.0, 0.0, 0.0, 1.0], "inside at {uv:?}");
        }
        // Just outside the rectangle, on the plane: untouched.
        for uv in [[0.45, 0.6], [0.6, 0.45], [0.45, 0.45]] {
            assert_eq!(at(uv), [0.0, 0.0, 1.0, 1.0], "outside at {uv:?}");
        }
        // A pixel at the height of the corners' average lies on the plane
        // at v > 0.5 (perspective pushes the plane's centre toward the far
        // edge), so the paste reaches it.
        let mid = plane.to_plane([62.5, 55.5]).unwrap();
        assert!(mid[0] > 0.5 && mid[1] > 0.5, "{mid:?}");
        assert_eq!(out.get(62, 55)[0], 1.0);
        // Off the plane entirely: untouched.
        assert_eq!(out.get(2, 2), [0.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn a_clone_dab_copies_across_the_plane_with_perspective() {
        // Left half white, right half black.
        let mut src = FilterBuffer::filled(120, 100, [0.0, 0.0, 0.0, 1.0]).unwrap();
        for y in 0..100 {
            for x in 0..60 {
                src.set(x, y, [1.0, 1.0, 1.0, 1.0]);
            }
        }
        let plane = floor();
        let edit = VanishingPointEdit {
            plane,
            ops: vec![VanishingOp::Clone {
                from: [0.2, 0.6],
                to: [0.8, 0.6],
                radius: 0.1,
            }],
        };
        let out = edit.apply(&src);
        let p = plane.to_image([0.8, 0.6]).unwrap();
        assert!(src.get(p[0] as u32, p[1] as u32)[0] < 0.5, "setup: dark there");
        assert!(out.get(p[0] as u32, p[1] as u32)[0] > 0.99, "cloned white");
        // Outside the dab: untouched.
        let q = plane.to_image([0.8, 0.9]).unwrap();
        assert_eq!(out.get(q[0] as u32, q[1] as u32), src.get(q[0] as u32, q[1] as u32));
    }

    #[test]
    fn an_invalid_plane_or_no_ops_is_the_identity() {
        let src = FilterBuffer::filled(20, 20, [0.3, 0.3, 0.3, 1.0]).unwrap();
        let empty = VanishingPointEdit {
            plane: PerspectivePlane::centered(20, 20),
            ops: Vec::new(),
        };
        assert!(empty.is_identity());
        assert_eq!(empty.apply(&src), src);
        let mut bad = floor();
        bad.corners.swap(0, 1);
        let edit = VanishingPointEdit {
            plane: bad,
            ops: vec![VanishingOp::Clone {
                from: [0.1, 0.1],
                to: [0.5, 0.5],
                radius: 0.2,
            }],
        };
        assert!(edit.is_identity());
        assert_eq!(edit.apply(&src), src);
    }

    #[test]
    fn the_grid_is_even_on_the_plane_and_bunches_on_screen() {
        let plane = floor();
        let lines = plane.grid_lines(4);
        assert_eq!(lines.len(), 10);
        // Horizontal lines (v = const) get closer together toward the top.
        let ys: Vec<f32> = (0..=4)
            .map(|i| plane.to_image([0.5, i as f32 / 4.0]).unwrap()[1])
            .collect();
        let gaps: Vec<f32> = ys.windows(2).map(|w| w[1] - w[0]).collect();
        assert!(gaps[0] < gaps[3], "{gaps:?}");
    }
}
