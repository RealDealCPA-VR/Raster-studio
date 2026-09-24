//! Filter ▸ Lens Correction: the Custom tab.
//!
//! Geometric distortion, chromatic aberration, vignette, and the transform
//! group (vertical and horizontal perspective, angle, scale), in one inverse
//! mapping: for each destination pixel the filter works out where in the
//! source it comes from and samples there bilinearly, through the caller's
//! [`EdgeMode`] for anything the correction pulls in from outside the frame
//! (Photoshop's "Edge Extension" is [`EdgeMode::Clamp`]).
//!
//! Coordinates are normalised about the image centre by the half-diagonal, so
//! the same settings do the same thing to a thumbnail preview and to the full
//! image.
//!
//! * **Distortion** (`-100..=100`): positive *applies* barrel distortion — the
//!   picture bulges and its corners are pulled in toward the centre — negative
//!   applies pincushion. To remove a lens's barrel, apply the negative.
//! * **Chromatic aberration** (`-100..=100` each): red/cyan scales the red
//!   channel's radial magnification against green, blue/yellow the blue's.
//!   Alpha comes from the green sample; the three samples are of the same
//!   premultiplied buffer, so on opaque pixels (the case aberration is about)
//!   the result is exact.
//! * **Vignette** amount (`-100..=100`, negative darkens the corners) with a
//!   midpoint (`0..=100`) that sets where the falloff starts.
//! * **Perspective**: a keystone homography, vertical and horizontal.
//! * **Angle** in degrees, and **scale** in percent (`100` is none).
//!
//! All controls at their neutral values return the source untouched.

use serde::{Deserialize, Serialize};

use crate::buffer::FilterBuffer;
use crate::support::{fill_tiles, smoothstep, EdgeMode};

/// The Custom tab of Lens Correction.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LensCorrection {
    /// Positive applies barrel, negative pincushion.
    pub distortion: f32,
    /// Red/cyan fringe: red channel magnification relative to green.
    pub ca_red_cyan: f32,
    /// Blue/yellow fringe: blue channel magnification relative to green.
    pub ca_blue_yellow: f32,
    /// Negative darkens the corners, positive lightens them.
    pub vignette_amount: f32,
    /// Where the vignette's falloff starts, `0..=100`.
    pub vignette_midpoint: f32,
    /// Keystone about the horizontal axis, `-100..=100`.
    pub vertical_perspective: f32,
    /// Keystone about the vertical axis, `-100..=100`.
    pub horizontal_perspective: f32,
    /// Rotation in degrees.
    pub angle: f32,
    /// Scale in percent, `50..=150`.
    pub scale: f32,
    /// How pixels pulled in from outside the frame are resolved.
    pub edge: EdgeMode,
}

impl Default for LensCorrection {
    fn default() -> Self {
        Self {
            distortion: 0.0,
            ca_red_cyan: 0.0,
            ca_blue_yellow: 0.0,
            vignette_amount: 0.0,
            vignette_midpoint: 50.0,
            vertical_perspective: 0.0,
            horizontal_perspective: 0.0,
            angle: 0.0,
            scale: 100.0,
            edge: EdgeMode::Clamp,
        }
    }
}

fn finite(v: f32, default: f32) -> f32 {
    if v.is_finite() {
        v
    } else {
        default
    }
}

impl LensCorrection {
    fn unit(v: f32) -> f32 {
        (finite(v, 0.0) / 100.0).clamp(-1.0, 1.0)
    }

    /// Whether the settings leave the geometry alone.
    fn geometry_is_identity(&self) -> bool {
        Self::unit(self.distortion) == 0.0
            && Self::unit(self.ca_red_cyan) == 0.0
            && Self::unit(self.ca_blue_yellow) == 0.0
            && Self::unit(self.vertical_perspective) == 0.0
            && Self::unit(self.horizontal_perspective) == 0.0
            && finite(self.angle, 0.0).rem_euclid(360.0) == 0.0
            && self.scale_factor() == 1.0
    }

    fn scale_factor(&self) -> f32 {
        finite(self.scale, 100.0).clamp(50.0, 150.0) / 100.0
    }

    /// Whether the settings change nothing at all.
    pub fn is_identity(&self) -> bool {
        self.geometry_is_identity() && Self::unit(self.vignette_amount) == 0.0
    }

    /// The source point, in normalised coordinates, for a destination point
    /// `(u, v)` — before chromatic aberration.
    fn source_of(&self, u: f32, v: f32) -> (f32, f32) {
        // Scale: a larger scale shows less of the source.
        let s = self.scale_factor();
        let (mut u, mut v) = (u / s, v / s);
        // Angle: the image turns by `angle`, so sample the reverse turn.
        let a = finite(self.angle, 0.0).to_radians();
        if a != 0.0 {
            let (sin, cos) = a.sin_cos();
            (u, v) = (u * cos + v * sin, -u * sin + v * cos);
        }
        // Perspective: a keystone homography.
        let kv = 0.5 * Self::unit(self.vertical_perspective);
        let kh = 0.5 * Self::unit(self.horizontal_perspective);
        if kv != 0.0 || kh != 0.0 {
            let w = (1.0 + kv * v + kh * u).max(0.05);
            (u, v) = (u / w, v / w);
        }
        // Distortion: sample further out as the radius grows, which pulls
        // outer content inward (barrel), or nearer in (pincushion).
        let k = 0.5 * Self::unit(self.distortion);
        if k != 0.0 {
            let r2 = u * u + v * v;
            let f = (1.0 + k * r2).max(0.05);
            (u, v) = (u * f, v * f);
        }
        (u, v)
    }

    /// Run the correction over `src`.
    pub fn apply(&self, src: &FilterBuffer) -> FilterBuffer {
        if src.is_empty() || self.is_identity() {
            return src.clone();
        }
        let (w, h) = src.dimensions();
        let cx = w as f32 * 0.5;
        let cy = h as f32 * 0.5;
        let half_diag = (cx * cx + cy * cy).sqrt().max(0.5);
        let geometry = !self.geometry_is_identity();
        let ca_r = 1.0 + 0.02 * Self::unit(self.ca_red_cyan);
        let ca_b = 1.0 + 0.02 * Self::unit(self.ca_blue_yellow);
        let amount = Self::unit(self.vignette_amount);
        let mid = (finite(self.vignette_midpoint, 50.0) / 100.0).clamp(0.0, 1.0);
        let start = 0.9 * mid;
        let edge = self.edge;
        let mut out = src.same_size_blank();
        fill_tiles(w, h, out.pixels_mut(), |x, y| {
            let u = (x as f32 + 0.5 - cx) / half_diag;
            let v = (y as f32 + 0.5 - cy) / half_diag;
            let mut px = if geometry {
                let (su, sv) = self.source_of(u, v);
                let at = |k: f32| {
                    src.sample_bilinear(cx + su * k * half_diag, cy + sv * k * half_diag, edge)
                };
                let g = at(1.0);
                let r = if ca_r != 1.0 { at(ca_r)[0] } else { g[0] };
                let b = if ca_b != 1.0 { at(ca_b)[2] } else { g[2] };
                [r, g[1], b, g[3]]
            } else {
                src.get(x, y)
            };
            if amount != 0.0 {
                let r = (u * u + v * v).sqrt();
                let fall = smoothstep(start, 1.0, r);
                let k = (1.0 + amount * fall).max(0.0);
                for c in px.iter_mut().take(3) {
                    *c *= k;
                }
            }
            px
        });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(w: u32, h: u32) -> FilterBuffer {
        let mut b = FilterBuffer::transparent(w, h).unwrap();
        for y in 0..h {
            for x in 0..w {
                let v = if (x / 4 + y / 4) % 2 == 0 { 0.9 } else { 0.1 };
                b.set(x, y, [v, v * 0.5, 1.0 - v, 1.0]);
            }
        }
        b
    }

    #[test]
    fn neutral_settings_are_identity() {
        let src = grid(33, 21);
        assert_eq!(LensCorrection::default().apply(&src), src);
        // A near-neutral setting runs the full resampling path and still
        // lands within one level.
        let out = LensCorrection {
            distortion: 1e-4,
            ..LensCorrection::default()
        }
        .apply(&src);
        for (a, b) in out.pixels().iter().zip(src.pixels()) {
            for c in 0..4 {
                assert!((a[c] - b[c]).abs() <= 1.0 / 255.0, "{a:?} vs {b:?}");
            }
        }
    }

    /// The centroid of the marker pixels (red above 0.5).
    fn marker(buf: &FilterBuffer) -> (f32, f32) {
        let (w, h) = buf.dimensions();
        let (mut sx, mut sy, mut n) = (0.0, 0.0, 0.0);
        for y in 0..h {
            for x in 0..w {
                let p = buf.get(x, y);
                if p[0] > 0.5 {
                    let wgt = p[0];
                    sx += (x as f32 + 0.5) * wgt;
                    sy += (y as f32 + 0.5) * wgt;
                    n += wgt;
                }
            }
        }
        assert!(n > 0.0, "the marker vanished");
        (sx / n, sy / n)
    }

    #[test]
    fn barrel_distortion_moves_corners_inward() {
        let (w, h) = (64u32, 48u32);
        let mut src = FilterBuffer::filled(w, h, [0.0, 0.0, 0.0, 1.0]).unwrap();
        // A marker near the top-left corner.
        for y in 4..8 {
            for x in 4..8 {
                src.set(x, y, [1.0, 1.0, 1.0, 1.0]);
            }
        }
        let centre = (w as f32 * 0.5, h as f32 * 0.5);
        let dist = |p: (f32, f32)| ((p.0 - centre.0).powi(2) + (p.1 - centre.1).powi(2)).sqrt();
        let before = dist(marker(&src));
        let barrel = LensCorrection {
            distortion: 80.0,
            ..LensCorrection::default()
        }
        .apply(&src);
        let after = dist(marker(&barrel));
        assert!(
            after < before - 2.0,
            "barrel pulls the corner in: {before} -> {after}"
        );
        let pincushion = LensCorrection {
            distortion: -80.0,
            ..LensCorrection::default()
        }
        .apply(&src);
        // Pincushion pushes it outward, possibly off the frame entirely.
        let out = pincushion
            .pixels()
            .iter()
            .any(|p| p[0] > 0.5)
            .then(|| dist(marker(&pincushion)));
        assert!(out.is_none_or(|d| d > before), "{out:?} vs {before}");
    }

    #[test]
    fn vignette_darkens_corners_not_the_centre() {
        let src = FilterBuffer::filled(40, 40, [0.5, 0.5, 0.5, 1.0]).unwrap();
        let out = LensCorrection {
            vignette_amount: -100.0,
            ..LensCorrection::default()
        }
        .apply(&src);
        assert!((out.get(20, 20)[0] - 0.5).abs() < 1e-4);
        assert!(out.get(0, 0)[0] < 0.2, "{:?}", out.get(0, 0));
    }

    #[test]
    fn chromatic_aberration_shifts_red_against_green() {
        let src = grid(48, 48);
        let out = LensCorrection {
            ca_red_cyan: 100.0,
            ..LensCorrection::default()
        }
        .apply(&src);
        let red_moved = out
            .pixels()
            .iter()
            .zip(src.pixels())
            .any(|(a, b)| (a[0] - b[0]).abs() > 0.1);
        let green_same = out
            .pixels()
            .iter()
            .zip(src.pixels())
            .all(|(a, b)| (a[1] - b[1]).abs() < 1e-4);
        assert!(red_moved && green_same);
    }

    #[test]
    fn transform_controls_move_pixels() {
        let src = grid(40, 30);
        for lc in [
            LensCorrection {
                vertical_perspective: 60.0,
                ..LensCorrection::default()
            },
            LensCorrection {
                horizontal_perspective: -60.0,
                ..LensCorrection::default()
            },
            LensCorrection {
                angle: 15.0,
                ..LensCorrection::default()
            },
            LensCorrection {
                scale: 130.0,
                ..LensCorrection::default()
            },
        ] {
            assert_ne!(lc.apply(&src), src, "{lc:?}");
        }
    }
}
