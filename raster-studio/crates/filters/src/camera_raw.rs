//! Filter ▸ Camera Raw: the Basic panel and the parametric Tone Curve.
//!
//! Every control works on **straight linear** light (the buffer is
//! unpremultiplied first and premultiplied back at the end), so exposure is a
//! true multiplication of light: `+1` stop doubles every linear value. The
//! tonal controls (highlights, shadows, whites, blacks and the tone curve)
//! need a notion of "where on the tonal scale" a pixel sits, which linear
//! values do not give evenly; they measure it on a perceptual lightness
//! `p = L^(1/2.2)` of the pixel's luminance `L`, and then rescale all three
//! channels by the same factor so hue is kept. Nothing is clamped above `1.0`:
//! the working space is scene-referred.
//!
//! Every control at `0` is the identity, and [`CameraRaw::is_identity`] lets a
//! caller skip the pass entirely. The per-pixel stages are pure functions of
//! the pixel; texture and clarity are local-contrast stages and read a blurred
//! copy of the image, which is only built when one of them is non-zero.
//!
//! Stage order, as in the Basic panel: white balance, exposure, contrast,
//! highlights/shadows/whites/blacks, tone curve, texture and clarity, dehaze,
//! vibrance and saturation.

use color::{linear_srgb_luminance, premultiply, unpremultiply};
use serde::{Deserialize, Serialize};

use crate::blur::gaussian_blur;
use crate::buffer::FilterBuffer;
use crate::support::{fill_tiles, smoothstep, EdgeMode};

/// The Basic panel plus the parametric Tone Curve.
///
/// Slider ranges follow Camera Raw: exposure is in stops (`-5..=5`), every
/// other control is `-100..=100`. Values outside are clamped when applied.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CameraRaw {
    /// White-balance temperature: positive warms (more red, less blue).
    pub temperature: f32,
    /// White-balance tint: positive toward magenta, negative toward green.
    pub tint: f32,
    /// Exposure in stops.
    pub exposure: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    /// Fine-scale local contrast.
    pub texture: f32,
    /// Mid-scale local contrast, weighted to the midtones.
    pub clarity: f32,
    /// Positive removes atmospheric haze, negative adds it.
    pub dehaze: f32,
    /// Saturation that favours the less saturated colours.
    pub vibrance: f32,
    pub saturation: f32,
    /// Parametric tone curve regions, `-100..=100` each.
    pub curve_highlights: f32,
    pub curve_lights: f32,
    pub curve_darks: f32,
    pub curve_shadows: f32,
}

/// Mid grey, the pivot contrast turns about.
const MID_GREY: f32 = 0.18;

fn unit(v: f32, range: f32) -> f32 {
    if v.is_finite() {
        (v / range).clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

impl CameraRaw {
    /// Whether every control is at zero, i.e. the filter changes nothing.
    pub fn is_identity(&self) -> bool {
        [
            self.temperature,
            self.tint,
            self.exposure,
            self.contrast,
            self.highlights,
            self.shadows,
            self.whites,
            self.blacks,
            self.texture,
            self.clarity,
            self.dehaze,
            self.vibrance,
            self.saturation,
            self.curve_highlights,
            self.curve_lights,
            self.curve_darks,
            self.curve_shadows,
        ]
        .iter()
        .all(|v| !v.is_finite() || *v == 0.0)
    }

    /// The per-pixel stages before local contrast, on straight linear RGB.
    fn global(&self, rgb: [f32; 3]) -> [f32; 3] {
        // White balance: opposing multipliers, a stop at the slider's end.
        let t = unit(self.temperature, 100.0);
        let g = unit(self.tint, 100.0);
        let mut c = [
            rgb[0] * (0.5 * t).exp2(),
            rgb[1] * (-0.5 * g).exp2(),
            rgb[2] * (-0.5 * t).exp2(),
        ];
        // Exposure: stops of light.
        let ev = if self.exposure.is_finite() {
            self.exposure.clamp(-5.0, 5.0)
        } else {
            0.0
        };
        if ev != 0.0 {
            let k = ev.exp2();
            c = c.map(|v| v * k);
        }
        // Contrast: a power curve about mid grey, per channel.
        let k = unit(self.contrast, 100.0);
        if k != 0.0 {
            let gamma = 1.0 + 0.6 * k;
            c = c.map(|v| {
                if v <= 0.0 {
                    v
                } else {
                    MID_GREY * (v / MID_GREY).powf(gamma)
                }
            });
        }
        // Tonal ranges, on perceptual lightness, as a luminance ratio.
        let tonal = [
            unit(self.highlights, 100.0),
            unit(self.shadows, 100.0),
            unit(self.whites, 100.0),
            unit(self.blacks, 100.0),
            unit(self.curve_highlights, 100.0),
            unit(self.curve_lights, 100.0),
            unit(self.curve_darks, 100.0),
            unit(self.curve_shadows, 100.0),
        ];
        if tonal.iter().any(|v| *v != 0.0) {
            let l = linear_srgb_luminance(c).max(0.0);
            let p = l.powf(1.0 / 2.2);
            let q = self.tone(p, tonal).max(0.0);
            let l2 = q.powf(2.2);
            if l > 1e-6 {
                let ratio = l2 / l;
                c = c.map(|v| v * ratio);
            } else {
                // Black has no hue to keep: lifting it lifts a neutral.
                c = c.map(|v| v + l2);
            }
        }
        c
    }

    /// Perceptual lightness `p` through the tonal sliders and the curve.
    fn tone(&self, p: f32, s: [f32; 8]) -> f32 {
        let [hi, sh, wh, bl, ch, cl, cd, cs] = s;
        let x = p.clamp(0.0, 1.0);
        // Highlights and shadows: broad bells over the upper and lower half.
        let w_hi = smoothstep(0.35, 1.0, x) * (1.0 - smoothstep(0.95, 1.2, x)).max(0.3);
        let w_sh = 1.0 - smoothstep(0.0, 0.65, x);
        let w_sh = w_sh * smoothstep(-0.2, 0.1, x).max(0.3);
        // Whites and blacks: the extreme ends, which move the clipping points.
        let w_wh = smoothstep(0.6, 1.0, x);
        let w_bl = 1.0 - smoothstep(0.0, 0.4, x);
        // Tone-curve regions: quarter-wide bumps centred in each quarter.
        let bump = |centre: f32| {
            let d = ((x - centre) / 0.25).abs();
            if d >= 1.0 {
                0.0
            } else {
                0.5 + 0.5 * (std::f32::consts::PI * d).cos()
            }
        };
        p + 0.25 * (hi * w_hi + sh * w_sh)
            + 0.2 * (wh * w_wh + bl * w_bl)
            + 0.12 * (ch * bump(0.875) + cl * bump(0.625) + cd * bump(0.375) + cs * bump(0.125))
    }

    /// The per-pixel stages after local contrast.
    fn finish(&self, c: [f32; 3], haze_light: f32) -> [f32; 3] {
        let mut c = c;
        let d = unit(self.dehaze, 100.0);
        if d > 0.0 {
            // Dark-channel prior: the darkest channel estimates the haze.
            let dark = c[0].min(c[1]).min(c[2]).max(0.0);
            let t = (1.0 - 0.9 * d * dark / haze_light).max(0.1);
            c = c.map(|v| (v - haze_light) / t + haze_light);
            c = c.map(|v| v.max(0.0));
        } else if d < 0.0 {
            c = c.map(|v| v + (haze_light - v) * (-0.6 * d));
        }
        let vib = unit(self.vibrance, 100.0);
        let sat = unit(self.saturation, 100.0);
        if vib != 0.0 || sat != 0.0 {
            let l = linear_srgb_luminance(c);
            let max = c[0].max(c[1]).max(c[2]);
            let min = c[0].min(c[1]).min(c[2]);
            let current = if max > 1e-6 { (max - min) / max } else { 0.0 };
            let k = (1.0 + sat) * (1.0 + vib * (1.0 - current)).max(0.0);
            c = c.map(|v| l + (v - l) * k);
        }
        c
    }

    /// Run the whole panel over `src`.
    pub fn apply(&self, src: &FilterBuffer) -> FilterBuffer {
        if src.is_empty() || self.is_identity() {
            return src.clone();
        }
        let (w, h) = src.dimensions();
        // Stage 1: global, per pixel, kept premultiplied so the local
        // contrast blur averages coverage correctly.
        let mut stage = src.same_size_blank();
        fill_tiles(w, h, stage.pixels_mut(), |x, y| {
            let s = unpremultiply(src.get(x, y));
            if s[3] <= 0.0 {
                return [0.0; 4];
            }
            let c = self.global([s[0], s[1], s[2]]);
            premultiply([c[0], c[1], c[2], s[3]])
        });
        // Stage 2: local contrast.
        let texture = unit(self.texture, 100.0);
        let clarity = unit(self.clarity, 100.0);
        if texture != 0.0 || clarity != 0.0 {
            let long = w.max(h) as f32;
            let fine = (texture != 0.0).then(|| gaussian_blur(&stage, 2.0, EdgeMode::Clamp));
            let broad = (clarity != 0.0)
                .then(|| gaussian_blur(&stage, (long * 0.02).clamp(2.0, 64.0), EdgeMode::Clamp));
            let base = stage.clone();
            fill_tiles(w, h, stage.pixels_mut(), |x, y| {
                let s = base.get(x, y);
                let mut o = s;
                let mid = {
                    let st = unpremultiply(s);
                    let p = linear_srgb_luminance([st[0], st[1], st[2]])
                        .max(0.0)
                        .powf(1.0 / 2.2);
                    4.0 * p.clamp(0.0, 1.0) * (1.0 - p.clamp(0.0, 1.0))
                };
                if let Some(f) = &fine {
                    let b = f.get(x, y);
                    for ch in 0..3 {
                        o[ch] += texture * (s[ch] - b[ch]);
                    }
                }
                if let Some(f) = &broad {
                    let b = f.get(x, y);
                    for ch in 0..3 {
                        o[ch] += clarity * 0.8 * mid * (s[ch] - b[ch]);
                    }
                }
                for v in o.iter_mut().take(3) {
                    *v = v.max(0.0);
                }
                o
            });
        }
        // Stage 3: dehaze, vibrance, saturation.
        let haze_light = {
            let mut brightest = 0.0f32;
            for px in stage.pixels() {
                let s = unpremultiply(*px);
                brightest = brightest.max(linear_srgb_luminance([s[0], s[1], s[2]]));
            }
            brightest.clamp(0.25, 1.0)
        };
        let base = stage;
        let mut out = base.same_size_blank();
        fill_tiles(w, h, out.pixels_mut(), |x, y| {
            let s = unpremultiply(base.get(x, y));
            if s[3] <= 0.0 {
                return [0.0; 4];
            }
            let c = self.finish([s[0], s[1], s[2]], haze_light);
            premultiply([c[0], c[1], c[2], s[3]])
        });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp() -> FilterBuffer {
        let mut b = FilterBuffer::transparent(24, 12).unwrap();
        for y in 0..12 {
            for x in 0..24 {
                let v = x as f32 / 23.0;
                b.set(x, y, premultiply([v, 0.5 * v + 0.1, 1.0 - v, 0.75]));
            }
        }
        b
    }

    #[test]
    fn identity_settings_are_identity_within_one_level() {
        let src = ramp();
        let out = CameraRaw::default().apply(&src);
        for (a, b) in out.pixels().iter().zip(src.pixels()) {
            for c in 0..4 {
                assert!((a[c] - b[c]).abs() <= 1.0 / 255.0);
            }
        }
        // A near-zero control goes through the full pipeline (not the early
        // return), and that pass must be the identity too.
        let tiny = CameraRaw {
            curve_lights: 1e-6,
            ..CameraRaw::default()
        };
        let out = tiny.apply(&src);
        for (a, b) in out.pixels().iter().zip(src.pixels()) {
            for c in 0..4 {
                assert!((a[c] - b[c]).abs() <= 1.0 / 255.0, "{a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn exposure_plus_one_doubles_linear_values() {
        let src = ramp();
        let out = CameraRaw {
            exposure: 1.0,
            ..CameraRaw::default()
        }
        .apply(&src);
        for (a, b) in out.pixels().iter().zip(src.pixels()) {
            for c in 0..3 {
                assert!((a[c] - 2.0 * b[c]).abs() < 1e-5, "{a:?} vs {b:?}");
            }
            assert_eq!(a[3], b[3], "alpha is untouched");
        }
    }

    #[test]
    fn every_control_moves_the_image() {
        let src = ramp();
        let set: [fn(&mut CameraRaw); 17] = [
            |c| c.temperature = 50.0,
            |c| c.tint = 50.0,
            |c| c.exposure = -1.0,
            |c| c.contrast = 60.0,
            |c| c.highlights = -80.0,
            |c| c.shadows = 80.0,
            |c| c.whites = 60.0,
            |c| c.blacks = -60.0,
            |c| c.texture = 80.0,
            |c| c.clarity = 80.0,
            |c| c.dehaze = 60.0,
            |c| c.vibrance = 80.0,
            |c| c.saturation = -80.0,
            |c| c.curve_highlights = 80.0,
            |c| c.curve_lights = 80.0,
            |c| c.curve_darks = 80.0,
            |c| c.curve_shadows = 80.0,
        ];
        // Texture and clarity need detail to act on.
        let mut detailed = src.clone();
        for y in 0..12 {
            for x in 0..24 {
                if (x + y) % 3 == 0 {
                    detailed.set(x, y, [0.1, 0.1, 0.1, 1.0]);
                }
            }
        }
        for (i, f) in set.iter().enumerate() {
            let mut c = CameraRaw::default();
            f(&mut c);
            assert!(!c.is_identity());
            let out = c.apply(&detailed);
            let moved = out
                .pixels()
                .iter()
                .zip(detailed.pixels())
                .any(|(a, b)| (0..3).any(|k| (a[k] - b[k]).abs() > 1e-3));
            assert!(moved, "control {i} changed nothing");
        }
    }

    #[test]
    fn saturation_minus_100_is_grey() {
        let src = ramp();
        let out = CameraRaw {
            saturation: -100.0,
            ..CameraRaw::default()
        }
        .apply(&src);
        for p in out.pixels() {
            assert!((p[0] - p[1]).abs() < 1e-5 && (p[1] - p[2]).abs() < 1e-5);
        }
    }

    #[test]
    fn warm_temperature_raises_red_and_lowers_blue() {
        let grey = FilterBuffer::filled(2, 2, [0.3, 0.3, 0.3, 1.0]).unwrap();
        let p = CameraRaw {
            temperature: 60.0,
            ..CameraRaw::default()
        }
        .apply(&grey)
        .get(0, 0);
        assert!(p[0] > 0.3 && p[2] < 0.3, "{p:?}");
    }
}
