//! Render: filters that synthesise an image rather than transform one.
//!
//! Everything here is **deterministic in its seed**. The gradient noise behind
//! [`clouds`], [`difference_clouds`] and [`fibers`] is the crate's own
//! [`crate::rng::Perlin`], evaluated as a pure function of the pixel
//! coordinate — the same seed gives the same image on any machine, at any
//! thread count, and at any buffer size.
//!
//! Colours are given as **straight** linear RGBA, because that is how a user
//! thinks about a colour picker, and are premultiplied on the way into the
//! buffer. Gradients interpolate in *premultiplied* space: interpolating
//! straight colour towards a transparent stop drags that stop's meaningless
//! colour into the ramp and leaves the familiar dark or white fringe.

use color::premultiply;
use serde::{Deserialize, Serialize};

use crate::buffer::{clamp_premultiplied, FilterBuffer, FilterError};
use crate::rng::{hash_unit, Perlin};
use crate::support::{fill_tiles, lerp_px};

use core::f32::consts::TAU;

/// Parameters for [`clouds`] and [`difference_clouds`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CloudParams {
    pub seed: u64,
    /// Size of the largest feature, in pixels. Clamped to at least one.
    pub scale: f32,
    /// Octaves of detail. Clamped to `1 ..= 12`.
    pub octaves: u32,
    /// Amplitude ratio between successive octaves. Clamped to `0.01 ..= 1`.
    pub persistence: f32,
    /// Straight linear RGBA at the dark end.
    pub color_a: [f32; 4],
    /// Straight linear RGBA at the bright end.
    pub color_b: [f32; 4],
}

impl Default for CloudParams {
    fn default() -> Self {
        Self {
            seed: 0,
            scale: 64.0,
            octaves: 6,
            persistence: 0.5,
            color_a: [0.0, 0.0, 0.0, 1.0],
            color_b: [1.0, 1.0, 1.0, 1.0],
        }
    }
}

/// Fractal Perlin clouds.
///
/// The noise field is normalised so its range is `[-1, 1]` whatever the octave
/// count, then remapped to `[0, 1]` and used to mix `color_a` into `color_b`.
/// Because the normalisation happens *inside* the fractal sum, adding octaves
/// adds detail without changing the average brightness.
pub fn clouds(width: u32, height: u32, params: &CloudParams) -> Result<FilterBuffer, FilterError> {
    let mut out = FilterBuffer::transparent(width, height)?;
    if out.is_empty() {
        return Ok(out);
    }
    let perlin = Perlin::new(params.seed);
    let scale = if params.scale.is_finite() {
        params.scale.max(1.0)
    } else {
        64.0
    };
    let inv = 1.0 / scale;
    let a = premultiply(params.color_a);
    let b = premultiply(params.color_b);
    let (oct, persist) = (params.octaves, params.persistence);
    fill_tiles(width, height, out.pixels_mut(), |x, y| {
        let t =
            0.5 * (perlin.fbm((x as f32 + 0.5) * inv, (y as f32 + 0.5) * inv, oct, persist) + 1.0);
        lerp_px(a, b, t)
    });
    Ok(out)
}

/// Difference clouds: the absolute difference between the image and a fresh
/// cloud field.
///
/// The difference is taken on **straight** colour, then re-premultiplied,
/// because the operation is not linear in the pixel value: `|a*p - a*q|` only
/// equals `a*|p - q|` for a non-negative alpha, and taking the difference of
/// premultiplied channels would let a transparent pixel contribute colour.
/// Alpha is carried through from the source unchanged.
pub fn difference_clouds(src: &FilterBuffer, params: &CloudParams) -> FilterBuffer {
    if src.is_empty() {
        return src.clone();
    }
    let (w, h) = src.dimensions();
    let cloud = match clouds(w, h, params) {
        Ok(c) => c,
        // Same dimensions as an existing buffer, so this cannot overflow.
        Err(_) => return src.clone(),
    };
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let s = color::unpremultiply(src.get(x, y));
        let c = color::unpremultiply(cloud.get(x, y));
        premultiply([
            (s[0] - c[0]).abs(),
            (s[1] - c[1]).abs(),
            (s[2] - c[2]).abs(),
            s[3],
        ])
    });
    out
}

/// Parameters for [`fibers`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FiberParams {
    pub seed: u64,
    /// `0 ..= 1`. Higher values widen the individual fibres.
    pub variance: f32,
    /// `0 ..= 1`. Higher values stretch the fibres further along the vertical
    /// axis, making them look longer and straighter.
    pub strength: f32,
    /// Straight linear RGBA at the dark end.
    pub color_a: [f32; 4],
    /// Straight linear RGBA at the bright end.
    pub color_b: [f32; 4],
}

impl Default for FiberParams {
    fn default() -> Self {
        Self {
            seed: 0,
            variance: 0.35,
            strength: 0.5,
            color_a: [0.02, 0.02, 0.02, 1.0],
            color_b: [0.9, 0.9, 0.9, 1.0],
        }
    }
}

/// Vertical fibres, as in woven or brushed material.
///
/// Built from strongly anisotropic Perlin noise — high frequency across the
/// image, low frequency down it — plus a per-column phase offset so adjacent
/// fibres do not line up into visible bands.
pub fn fibers(width: u32, height: u32, params: &FiberParams) -> Result<FilterBuffer, FilterError> {
    let mut out = FilterBuffer::transparent(width, height)?;
    if out.is_empty() {
        return Ok(out);
    }
    let perlin = Perlin::new(params.seed);
    let variance = clamp01(params.variance, 0.35);
    let strength = clamp01(params.strength, 0.5);
    // Fibre width in pixels, and how far a fibre runs before it changes.
    let fibre_width = 1.0 + variance * 15.0;
    let fibre_length = fibre_width * (1.0 + strength * 40.0);
    let (fx, fy) = (1.0 / fibre_width, 1.0 / fibre_length);
    let a = premultiply(params.color_a);
    let b = premultiply(params.color_b);
    let seed = params.seed;
    fill_tiles(width, height, out.pixels_mut(), |x, y| {
        // Per-column phase: without it every column's noise starts in step and
        // the result reads as horizontal banding.
        let phase = hash_unit(seed ^ 0xF0E1_D2C3_B4A5_9687, x as i64, 0) * 64.0;
        let t =
            0.5 * (perlin.fbm((x as f32 + 0.5) * fx, (y as f32 + 0.5) * fy + phase, 3, 0.6) + 1.0);
        lerp_px(a, b, t)
    });
    Ok(out)
}

/// Parameters for [`lens_flare`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LensFlare {
    /// Light source position in pixel coordinates.
    pub center: (f32, f32),
    /// Overall intensity. Zero is the identity.
    pub brightness: f32,
    /// Radius of the central glow, in pixels.
    pub radius: f32,
    /// Number of ghost reflections spread along the axis through the frame
    /// centre. Clamped to `0 ..= 16`.
    pub ghosts: u32,
    /// Number of anamorphic streaks radiating from the core. Clamped to
    /// `0 ..= 32`.
    pub streaks: u32,
}

impl Default for LensFlare {
    fn default() -> Self {
        Self {
            center: (0.0, 0.0),
            brightness: 1.0,
            radius: 120.0,
            ghosts: 5,
            streaks: 6,
        }
    }
}

/// Lens flare: an emissive core, streaks, and ghost reflections.
///
/// The flare is **added** to the image, because a flare is light arriving at
/// the sensor, not a layer composited over it. Alpha rises with the added
/// light so the flare is visible over transparent regions too, and the result
/// is re-clamped into a valid premultiplied pixel.
///
/// Entirely deterministic: the ghost positions and sizes come from the
/// geometry, not from a random source. A non-positive brightness or radius is
/// the identity.
pub fn lens_flare(src: &FilterBuffer, params: &LensFlare) -> FilterBuffer {
    if src.is_empty()
        || !params.brightness.is_finite()
        || params.brightness <= 0.0
        || !params.radius.is_finite()
        || params.radius <= 0.0
    {
        return src.clone();
    }
    let (w, h) = src.dimensions();
    let (cx, cy) = params.center;
    let (fx, fy) = (w as f32 * 0.5, h as f32 * 0.5);
    let bright = params.brightness;
    let radius = params.radius;
    let ghosts = params.ghosts.min(16);
    let streaks = params.streaks.min(32);
    // Ghosts march along the line from the source through the frame centre.
    let (gx, gy) = (fx - cx, fy - cy);

    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        let (dx, dy) = (px - cx, py - cy);
        let r = (dx * dx + dy * dy).sqrt();

        // Core: a smooth inverse-square-ish falloff, finite at r = 0.
        let core = bright / (1.0 + (r / (radius * 0.25)) * (r / (radius * 0.25)));

        // Halo ring at the core radius.
        let ring = ((r - radius) / (radius * 0.18)).powi(2);
        let halo = bright * 0.35 * (-ring).exp();

        // Streaks: angular spikes, strongest near the core.
        let mut streak = 0.0f32;
        if streaks > 0 && r > 0.0 {
            let theta = dy.atan2(dx);
            let spikes = (theta * streaks as f32).cos().abs().powi(24);
            streak = bright * 0.5 * spikes / (1.0 + r / radius);
        }

        // Ghosts.
        let mut ghost = 0.0f32;
        for i in 1..=ghosts {
            let k = i as f32 / (ghosts as f32 + 1.0) * 2.0;
            let (ox, oy) = (cx + gx * k, cy + gy * k);
            let gr = ((px - ox) * (px - ox) + (py - oy) * (py - oy)).sqrt();
            let size = radius * (0.12 + 0.08 * i as f32);
            if gr < size {
                let t = 1.0 - gr / size;
                ghost += bright * 0.12 * t * t;
            }
        }

        let glow = core + halo + streak + ghost;
        let s = src.get(x, y);
        // Warm tint: a real flare is not neutral.
        let add = [glow, glow * 0.92, glow * 0.78];
        let alpha = (s[3] + glow).clamp(0.0, 1.0);
        clamp_premultiplied([s[0] + add[0], s[1] + add[1], s[2] + add[2], alpha])
    });
    out
}

/// Shape of a [`Gradient`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum GradientKind {
    /// Ramps along the line from `start` to `end`.
    #[default]
    Linear,
    /// Ramps outward from `start`, reaching the last stop at `end`.
    Radial,
    /// Sweeps once around `start`, starting along the direction of `end`.
    Angle,
    /// Like [`GradientKind::Linear`] but mirrored about `start`.
    Reflected,
    /// Concentric squares rotated to the `start`-`end` axis.
    Diamond,
}

/// One colour stop. `color` is **straight** linear RGBA.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GradientStop {
    /// Position along the ramp, clamped to `0 ..= 1`.
    pub position: f32,
    pub color: [f32; 4],
}

impl GradientStop {
    pub const fn new(position: f32, color: [f32; 4]) -> Self {
        Self { position, color }
    }
}

/// A gradient definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Gradient {
    pub kind: GradientKind,
    /// Where the ramp begins, in pixel coordinates.
    pub start: (f32, f32),
    /// Where the ramp ends, in pixel coordinates.
    pub end: (f32, f32),
    pub stops: Vec<GradientStop>,
}

impl Gradient {
    /// A two-stop gradient of the given kind.
    pub fn two_stop(
        kind: GradientKind,
        start: (f32, f32),
        end: (f32, f32),
        from: [f32; 4],
        to: [f32; 4],
    ) -> Self {
        Self {
            kind,
            start,
            end,
            stops: vec![GradientStop::new(0.0, from), GradientStop::new(1.0, to)],
        }
    }
}

/// Fill a buffer with a gradient.
///
/// Stops are sorted by position and evaluated in **premultiplied** space, so a
/// ramp to a transparent stop fades out cleanly instead of picking up that
/// stop's nominal colour. Positions outside `[0, 1]` are clamped, and a
/// position past the last stop holds that stop's colour.
///
/// Returns [`FilterError::EmptyGradient`] if there are no stops: there is no
/// defensible colour to fill with, and silently producing transparent black
/// would look like a working gradient that renders nothing.
pub fn gradient_fill(
    width: u32,
    height: u32,
    gradient: &Gradient,
) -> Result<FilterBuffer, FilterError> {
    if gradient.stops.is_empty() {
        return Err(FilterError::EmptyGradient);
    }
    let mut out = FilterBuffer::transparent(width, height)?;
    if out.is_empty() {
        return Ok(out);
    }
    let mut stops: Vec<(f32, [f32; 4])> = gradient
        .stops
        .iter()
        .map(|s| {
            let p = if s.position.is_finite() {
                s.position.clamp(0.0, 1.0)
            } else {
                0.0
            };
            (p, premultiply(s.color))
        })
        .collect();
    stops.sort_by(|a, b| a.0.total_cmp(&b.0));

    let (sx, sy) = gradient.start;
    let (ex, ey) = gradient.end;
    let (dx, dy) = (ex - sx, ey - sy);
    let len2 = dx * dx + dy * dy;
    let kind = gradient.kind;

    fill_tiles(width, height, out.pixels_mut(), |x, y| {
        let (px, py) = (x as f32 + 0.5 - sx, y as f32 + 0.5 - sy);
        let t = if len2 <= 0.0 {
            // Degenerate axis: the whole fill is the last stop.
            1.0
        } else {
            match kind {
                GradientKind::Linear => (px * dx + py * dy) / len2,
                GradientKind::Reflected => ((px * dx + py * dy) / len2).abs(),
                GradientKind::Radial => (px * px + py * py).sqrt() / len2.sqrt(),
                GradientKind::Angle => {
                    let base = dy.atan2(dx);
                    let mut a = py.atan2(px) - base;
                    while a < 0.0 {
                        a += TAU;
                    }
                    (a / TAU).min(1.0)
                }
                GradientKind::Diamond => {
                    let inv = 1.0 / len2.sqrt();
                    let (ux, uy) = (dx * inv, dy * inv);
                    let along = px * ux + py * uy;
                    let across = -px * uy + py * ux;
                    (along.abs() + across.abs()) * inv
                }
            }
        };
        sample_stops(&stops, t.clamp(0.0, 1.0))
    });
    Ok(out)
}

/// Colour of a sorted premultiplied stop list at `t` in `[0, 1]`.
fn sample_stops(stops: &[(f32, [f32; 4])], t: f32) -> [f32; 4] {
    if t <= stops[0].0 {
        return stops[0].1;
    }
    let last = stops[stops.len() - 1];
    if t >= last.0 {
        return last.1;
    }
    for pair in stops.windows(2) {
        let (p0, c0) = pair[0];
        let (p1, c1) = pair[1];
        if t <= p1 {
            let span = p1 - p0;
            let k = if span > 0.0 { (t - p0) / span } else { 0.0 };
            return lerp_px(c0, c1, k);
        }
    }
    last.1
}

#[inline]
fn clamp01(v: f32, fallback: f32) -> f32 {
    if v.is_finite() {
        v.clamp(0.0, 1.0)
    } else {
        fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A golden pin on the actual cloud *values*, not just on the fact that
    /// two runs agree. Determinism alone is satisfied by any noise function, so
    /// a change to the Perlin gradients, the octave normalisation, the
    /// half-pixel sample offset or the colour mix would slip past
    /// `clouds_are_deterministic_and_seed_dependent` unnoticed. These numbers
    /// come from the implementation and are here to break loudly if it moves.
    ///
    /// Integer hashing and f32 arithmetic make them reproducible across
    /// machines; the tolerance is a single-precision epsilon, not a fudge
    /// factor for platform drift.
    #[test]
    fn clouds_pin_their_values_for_a_fixed_seed() {
        let p = CloudParams {
            seed: 7,
            scale: 20.0,
            ..CloudParams::default()
        };
        let out = clouds(16, 16, &p).unwrap();
        // Default colours run black to white, so the channel *is* the field.
        for ((x, y), want) in [
            ((0u32, 0u32), 0.507_839_f32),
            ((5, 3), 0.550_505),
            ((15, 15), 0.596_258),
            ((8, 8), 0.619_487),
        ] {
            let got = out.get(x, y);
            assert!(
                (got[0] - want).abs() < 1e-5,
                "clouds({x},{y}) drifted: want {want}, got {got:?}"
            );
            assert!((got[3] - 1.0).abs() < 1e-6, "alpha at ({x},{y}): {got:?}");
        }

        // The same field, mixed into a different colour pair, must land at the
        // corresponding point on that ramp — this pins the premultiplied lerp
        // rather than the noise.
        let tinted = clouds(
            16,
            16,
            &CloudParams {
                color_a: [0.2, 0.0, 0.0, 1.0],
                color_b: [0.2, 0.8, 0.0, 1.0],
                ..p
            },
        )
        .unwrap();
        let t = 0.619_487_f32;
        let px = tinted.get(8, 8);
        assert!((px[0] - 0.2).abs() < 1e-5, "constant channel moved: {px:?}");
        assert!((px[1] - 0.8 * t).abs() < 1e-5, "ramp channel: {px:?}");
        assert!(px[2].abs() < 1e-6, "{px:?}");
    }

    #[test]
    fn clouds_are_deterministic_and_seed_dependent() {
        let p = CloudParams {
            seed: 7,
            scale: 20.0,
            ..CloudParams::default()
        };
        let a = clouds(64, 48, &p).unwrap();
        let b = clouds(64, 48, &p).unwrap();
        assert_eq!(a, b);
        let c = clouds(64, 48, &CloudParams { seed: 8, ..p }).unwrap();
        assert_ne!(a, c);
    }

    /// The value at a pixel must depend only on its coordinate, not on the
    /// buffer size — otherwise the parallel band decomposition is leaking into
    /// the result.
    #[test]
    fn cloud_values_do_not_depend_on_buffer_size() {
        let p = CloudParams {
            seed: 3,
            scale: 17.0,
            ..CloudParams::default()
        };
        // 300 rows spans several TILE_SIZE bands, 40 spans one.
        let big = clouds(300, 300, &p).unwrap();
        let small = clouds(40, 40, &p).unwrap();
        for y in 0..40 {
            for x in 0..40 {
                assert_eq!(big.get(x, y), small.get(x, y), "at {x},{y}");
            }
        }
    }

    #[test]
    fn clouds_stay_between_their_two_colours() {
        let p = CloudParams {
            seed: 11,
            scale: 12.0,
            color_a: [0.1, 0.2, 0.3, 1.0],
            color_b: [0.8, 0.7, 0.6, 1.0],
            ..CloudParams::default()
        };
        let out = clouds(128, 128, &p).unwrap();
        for px in out.pixels() {
            assert!((0.1 - 1e-5..=0.8 + 1e-5).contains(&px[0]), "{px:?}");
            assert!((0.2 - 1e-5..=0.7 + 1e-5).contains(&px[1]), "{px:?}");
            assert!((0.3 - 1e-5..=0.6 + 1e-5).contains(&px[2]), "{px:?}");
            assert!((px[3] - 1.0).abs() < 1e-6);
        }
    }

    /// Octave count adds detail, not brightness. An unnormalised fractal sum
    /// gets brighter with every octave.
    #[test]
    fn octave_count_does_not_shift_the_mean() {
        let base = CloudParams {
            seed: 5,
            scale: 24.0,
            ..CloudParams::default()
        };
        let mut means = Vec::new();
        for oct in [1u32, 3, 6, 10] {
            let out = clouds(
                160,
                160,
                &CloudParams {
                    octaves: oct,
                    ..base
                },
            )
            .unwrap();
            let mean: f64 =
                out.pixels().iter().map(|p| p[0] as f64).sum::<f64>() / out.len() as f64;
            means.push(mean);
        }
        for m in &means {
            assert!((m - 0.5).abs() < 0.08, "mean {m} drifted from mid grey");
        }
    }

    #[test]
    fn clouds_survive_degenerate_sizes_and_parameters() {
        assert!(clouds(0, 10, &CloudParams::default()).unwrap().is_empty());
        assert!(clouds(10, 0, &CloudParams::default()).unwrap().is_empty());
        assert_eq!(clouds(1, 1, &CloudParams::default()).unwrap().len(), 1);
        let wild = CloudParams {
            scale: f32::NAN,
            octaves: 0,
            persistence: f32::INFINITY,
            ..CloudParams::default()
        };
        let out = clouds(8, 8, &wild).unwrap();
        for px in out.pixels() {
            assert!(px.iter().all(|v| v.is_finite()), "{px:?}");
        }
    }

    #[test]
    fn difference_clouds_are_a_difference() {
        let src = FilterBuffer::filled(32, 32, [0.5, 0.5, 0.5, 1.0]).unwrap();
        let p = CloudParams {
            seed: 2,
            scale: 10.0,
            ..CloudParams::default()
        };
        let cloud = clouds(32, 32, &p).unwrap();
        let out = difference_clouds(&src, &p);
        for i in 0..src.len() {
            let expect = (0.5 - cloud.pixels()[i][0]).abs();
            assert!(
                (out.pixels()[i][0] - expect).abs() < 1e-5,
                "{} vs {expect}",
                out.pixels()[i][0]
            );
        }
        assert_eq!(out, difference_clouds(&src, &p), "must be reproducible");
        assert!(difference_clouds(&FilterBuffer::transparent(0, 4).unwrap(), &p).is_empty());
    }

    #[test]
    fn difference_clouds_preserve_alpha_and_stay_valid() {
        let src = FilterBuffer::filled(16, 16, [0.25, 0.1, 0.4, 0.5]).unwrap();
        let out = difference_clouds(&src, &CloudParams::default());
        for px in out.pixels() {
            assert!((px[3] - 0.5).abs() < 1e-6, "{px:?}");
            for c in 0..3 {
                assert!(px[c] >= -1e-6 && px[c] <= px[3] + 1e-6, "{px:?}");
            }
        }
    }

    #[test]
    fn fibers_are_vertically_elongated_and_deterministic() {
        let p = FiberParams {
            seed: 21,
            variance: 0.2,
            strength: 0.9,
            ..FiberParams::default()
        };
        let out = fibers(96, 96, &p).unwrap();
        assert_eq!(out, fibers(96, 96, &p).unwrap());
        assert_ne!(out, fibers(96, 96, &FiberParams { seed: 22, ..p }).unwrap());

        let mut across = 0.0f64;
        let mut along = 0.0f64;
        for y in 0..95u32 {
            for x in 0..95u32 {
                across += (out.get(x + 1, y)[0] - out.get(x, y)[0]).abs() as f64;
                along += (out.get(x, y + 1)[0] - out.get(x, y)[0]).abs() as f64;
            }
        }
        assert!(
            across > along * 3.0,
            "fibres should vary far more across than along: {across} vs {along}"
        );
    }

    #[test]
    fn fibers_survive_degenerate_sizes_and_parameters() {
        assert!(fibers(0, 4, &FiberParams::default()).unwrap().is_empty());
        assert_eq!(fibers(1, 1, &FiberParams::default()).unwrap().len(), 1);
        let wild = FiberParams {
            variance: f32::NAN,
            strength: f32::INFINITY,
            ..FiberParams::default()
        };
        for px in fibers(8, 8, &wild).unwrap().pixels() {
            assert!(px.iter().all(|v| v.is_finite()), "{px:?}");
        }
    }

    #[test]
    fn lens_flare_adds_light_at_the_source_and_is_deterministic() {
        let src = FilterBuffer::filled(64, 64, [0.1, 0.1, 0.1, 1.0]).unwrap();
        let p = LensFlare {
            center: (20.0, 20.0),
            brightness: 1.0,
            radius: 24.0,
            ..LensFlare::default()
        };
        let out = lens_flare(&src, &p);
        assert!(
            out.get(20, 20)[0] > src.get(20, 20)[0] + 0.3,
            "no glow at the source: {:?}",
            out.get(20, 20)
        );
        // Far from the flare the image is close to untouched.
        assert!(
            (out.get(62, 62)[0] - src.get(62, 62)[0]).abs() < 0.2,
            "{:?}",
            out.get(62, 62)
        );
        assert_eq!(out, lens_flare(&src, &p), "must be reproducible");
    }

    #[test]
    fn lens_flare_output_is_a_valid_premultiplied_image() {
        let src = FilterBuffer::filled(48, 48, [0.2, 0.1, 0.05, 0.4]).unwrap();
        let out = lens_flare(
            &src,
            &LensFlare {
                center: (24.0, 24.0),
                brightness: 8.0,
                radius: 30.0,
                ..LensFlare::default()
            },
        );
        for px in out.pixels() {
            assert!((0.0..=1.0).contains(&px[3]), "{px:?}");
            for c in 0..3 {
                assert!(px[c] >= 0.0 && px[c] <= px[3] + 1e-6, "{px:?}");
            }
        }
    }

    #[test]
    fn lens_flare_identity_and_degenerate_inputs() {
        let src = FilterBuffer::filled(8, 8, [0.3, 0.3, 0.3, 1.0]).unwrap();
        for b in [0.0f32, -1.0, f32::NAN] {
            assert_eq!(
                lens_flare(
                    &src,
                    &LensFlare {
                        brightness: b,
                        ..LensFlare::default()
                    }
                ),
                src
            );
        }
        assert_eq!(
            lens_flare(
                &src,
                &LensFlare {
                    radius: 0.0,
                    ..LensFlare::default()
                }
            ),
            src
        );
        let empty = FilterBuffer::transparent(0, 3).unwrap();
        assert!(lens_flare(&empty, &LensFlare::default()).is_empty());
        // No ghosts, no streaks: still valid.
        let bare = lens_flare(
            &src,
            &LensFlare {
                ghosts: 0,
                streaks: 0,
                ..LensFlare::default()
            },
        );
        assert!(bare
            .pixels()
            .iter()
            .all(|p| p.iter().all(|v| v.is_finite())));
    }

    #[test]
    fn linear_gradient_hits_its_endpoints_and_midpoint() {
        let from = [1.0, 0.0, 0.0, 1.0];
        let to = [0.0, 0.0, 1.0, 1.0];
        let g = Gradient::two_stop(GradientKind::Linear, (0.5, 0.5), (15.5, 0.5), from, to);
        let out = gradient_fill(16, 4, &g).unwrap();
        for c in 0..4 {
            assert!((out.get(0, 0)[c] - from[c]).abs() < 1e-5, "start");
            assert!((out.get(15, 0)[c] - to[c]).abs() < 1e-5, "end");
        }
        // Halfway along, the premultiplied mix is the average.
        let mid = out.get(8, 2);
        let t = 8.0 / 15.0;
        assert!((mid[0] - (1.0 - t)).abs() < 1e-4, "{mid:?}");
        assert!((mid[2] - t).abs() < 1e-4, "{mid:?}");
        // Constant down the perpendicular axis.
        for y in 0..4 {
            assert_eq!(out.get(7, y), out.get(7, 0));
        }
    }

    /// Interpolating towards a transparent stop must fade to nothing, not
    /// drift through that stop's nominal colour. In premultiplied space the
    /// colour channels fall with alpha, which is the property that matters.
    ///
    /// The transparent stop deliberately carries a *non-black* nominal colour
    /// (transparent red). Black is a fixed point of `premultiply`, so a black
    /// transparent stop cannot distinguish premultiplied interpolation from
    /// straight-alpha interpolation; transparent red can. Straight-alpha
    /// interpolation from opaque white to transparent red would keep the red
    /// channel near 1.0 while alpha falls to ~0.47 at the midpoint, which is an
    /// impossible premultiplied pixel (colour above alpha) and shows up as the
    /// familiar red fringe.
    #[test]
    fn gradient_to_transparent_fades_without_a_fringe() {
        let g = Gradient::two_stop(
            GradientKind::Linear,
            (0.5, 0.5),
            (15.5, 0.5),
            [1.0, 1.0, 1.0, 1.0],
            // Transparent RED: alpha 0, but a nominal colour that would leak.
            [1.0, 0.0, 0.0, 0.0],
        );
        let out = gradient_fill(16, 1, &g).unwrap();
        for x in 0..16u32 {
            let px = out.get(x, 0);
            // No channel may exceed alpha: that is what "premultiplied" means,
            // and it is exactly what a straight-alpha ramp would violate.
            for (c, v) in px.iter().take(3).enumerate() {
                assert!(
                    *v <= px[3] + 1e-5,
                    "channel {c} above alpha at x={x}: {px:?}"
                );
            }
            // Premultiplied white fading out: every colour channel tracks alpha.
            assert!((px[0] - px[3]).abs() < 1e-5, "fringe at {x}: {px:?}");
            assert!((px[1] - px[3]).abs() < 1e-5, "fringe at {x}: {px:?}");
            assert!((px[2] - px[3]).abs() < 1e-5, "fringe at {x}: {px:?}");
        }
        // Pin the midpoint numerically so a straight-alpha regression cannot
        // slip through on tolerance: alpha ~0.467 and red must match it, not 1.0.
        let mid = out.get(8, 0);
        assert!((mid[3] - 7.0 / 15.0).abs() < 1e-4, "midpoint alpha {mid:?}");
        assert!((mid[0] - mid[3]).abs() < 1e-5, "midpoint red {mid:?}");
        assert!(mid[0] < 0.6, "red leaked towards the stop colour: {mid:?}");
        assert!(out.get(0, 0)[3] > 0.99);
        assert!(out.get(15, 0)[3] < 0.01);
        // And the fully transparent end must be fully clear, not clear-red.
        assert!(out.get(15, 0)[0] < 0.01, "{:?}", out.get(15, 0));
    }

    #[test]
    fn every_gradient_kind_produces_a_distinct_field() {
        let from = [0.0, 0.0, 0.0, 1.0];
        let to = [1.0, 1.0, 1.0, 1.0];
        let mut seen: Vec<FilterBuffer> = Vec::new();
        for kind in [
            GradientKind::Linear,
            GradientKind::Radial,
            GradientKind::Angle,
            GradientKind::Reflected,
            GradientKind::Diamond,
        ] {
            let g = Gradient::two_stop(kind, (16.0, 16.0), (32.0, 16.0), from, to);
            let out = gradient_fill(32, 32, &g).unwrap();
            assert!(
                !seen.contains(&out),
                "{kind:?} produced the same field as another kind"
            );
            for px in out.pixels() {
                assert!(px.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)));
            }
            seen.push(out);
        }
    }

    #[test]
    fn reflected_gradient_is_symmetric_about_the_start() {
        let g = Gradient::two_stop(
            GradientKind::Reflected,
            // Anchored on a pixel *centre*, so pixels 16+d and 16-d really are
            // the same distance from the start.
            (16.5, 0.5),
            (31.5, 0.5),
            [0.0, 0.0, 0.0, 1.0],
            [1.0, 1.0, 1.0, 1.0],
        );
        let out = gradient_fill(32, 1, &g).unwrap();
        for d in 1..15u32 {
            let a = out.get(16 + d, 0);
            let b = out.get(16 - d, 0);
            assert!((a[0] - b[0]).abs() < 1e-5, "asymmetric at {d}: {a:?} {b:?}");
        }
    }

    #[test]
    fn unsorted_and_clamped_stops_still_produce_a_monotonic_ramp() {
        let g = Gradient {
            kind: GradientKind::Linear,
            start: (0.5, 0.5),
            end: (31.5, 0.5),
            stops: vec![
                GradientStop::new(1.5, [1.0, 1.0, 1.0, 1.0]),
                GradientStop::new(-0.5, [0.0, 0.0, 0.0, 1.0]),
                GradientStop::new(0.5, [0.5, 0.5, 0.5, 1.0]),
            ],
        };
        let out = gradient_fill(32, 1, &g).unwrap();
        let mut previous = -1.0f32;
        for x in 0..32u32 {
            let v = out.get(x, 0)[0];
            assert!(
                v >= previous - 1e-5,
                "not monotonic at {x}: {v} < {previous}"
            );
            previous = v;
        }
        assert!((out.get(0, 0)[0]).abs() < 1e-5);
        assert!((out.get(31, 0)[0] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn a_gradient_needs_at_least_one_stop() {
        let g = Gradient {
            kind: GradientKind::Linear,
            start: (0.0, 0.0),
            end: (1.0, 0.0),
            stops: Vec::new(),
        };
        assert_eq!(
            gradient_fill(4, 4, &g).unwrap_err(),
            FilterError::EmptyGradient
        );
    }

    #[test]
    fn a_degenerate_axis_and_a_single_stop_are_handled() {
        let g = Gradient::two_stop(
            GradientKind::Linear,
            (5.0, 5.0),
            (5.0, 5.0),
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 1.0, 0.0, 1.0],
        );
        let out = gradient_fill(4, 4, &g).unwrap();
        for px in out.pixels() {
            assert_eq!(
                *px,
                [0.0, 1.0, 0.0, 1.0],
                "degenerate axis is the last stop"
            );
        }
        let single = Gradient {
            kind: GradientKind::Radial,
            start: (0.0, 0.0),
            end: (10.0, 0.0),
            stops: vec![GradientStop::new(0.3, [0.2, 0.4, 0.6, 1.0])],
        };
        let flat = gradient_fill(4, 4, &single).unwrap();
        for px in flat.pixels() {
            assert_eq!(*px, [0.2, 0.4, 0.6, 1.0]);
        }
        assert!(gradient_fill(0, 4, &g).unwrap().is_empty());
    }
}
