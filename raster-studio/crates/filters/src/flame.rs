//! W13X-5: Filter ▸ Render ▸ Flame, drawn along the active path.
//!
//! Photopea's Flame (its `Flam` descriptor) renders only along a path: with
//! none it stops with "Make a path first" ([`NO_PATH`]). This one does the
//! same — [`flame`] refuses with [`NoPath`] when it is handed no usable path —
//! and takes Photopea's eighteen controls with their ranges and defaults
//! ([`FlameSettings`]): Type (six along-path modes), Length, Randomize
//! Length, Width, Angle, Interval, Adapt Interval for Loops, Color, Quality,
//! Turbulent, Jag, Opacity, Lines, Bottom, Style, Shape, Randomize Shape and
//! Random Seed.
//!
//! **The renderer is this build's, not a port of Photopea's.** The controls
//! mean what their names say, drawn with this crate's own seeded noise:
//!
//! * **One Flame Along Path**: one flame whose spine is each subpath, base at
//!   its start and tip at its end (Length, Angle and Interval do not apply,
//!   as in Photopea's dialog, which disables them for this type).
//! * **Multiple Flames Along Path**: a flame every Interval pixels, rising
//!   along the path's left normal (up, for a path drawn left to right).
//! * **Multiple Flames One Direction**: the same bases, every flame rising at
//!   Angle (0° is straight up, positive turns clockwise).
//! * **Multiple Flames Path Directed**: the left normal turned by Angle.
//! * **Multiple Flames Various Angle**: the left normal turned by Angle plus
//!   a seeded spread of up to 60° either way.
//! * **Candle Light**: one small candle flame (Width wide, 2.5 Widths tall)
//!   at each subpath's first point, rising straight up.
//!
//! Each flame is Lines strands spread across its Width, shaped by Shape
//! (Parallel, To the center, Spread, Oval, Pointing) and swayed by
//! Turbulent (a slow curve) and Jag (a fast one); Style Violent sways faster
//! and harder, Flat sways less and keeps its heat to the tip. Bottom
//! staggers the strands' bases (0 all level). Opacity is the translucent body
//! glowing between the strands. Quality is the stamping step. The heat is
//! coloured from Color through to a white core and **screened** over the
//! layer, so the filter only brightens, and alpha rises to at least the
//! flame's coverage. Pixels farther from every path than a flame can reach
//! are never touched.

use serde::{Deserialize, Serialize};

use crate::buffer::FilterBuffer;
use crate::rng::{Perlin, Rng};
use crate::support::fill_tiles;

/// Photopea's refusal when there is no path to burn along.
pub const NO_PATH: &str = "Make a path first";

/// The refusal [`flame`] answers when it has no path with any length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoPath;

impl std::fmt::Display for NoPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(NO_PATH)
    }
}

impl std::error::Error for NoPath {}

/// One flattened subpath, in the buffer's pixel space.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlamePath {
    pub points: Vec<[f32; 2]>,
    pub closed: bool,
}

/// Photopea's Flame Type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FlameType {
    OneAlongPath,
    MultipleAlongPath,
    MultipleOneDirection,
    MultiplePathDirected,
    MultipleVariousAngle,
    CandleLight,
}

impl FlameType {
    pub const ALL: [FlameType; 6] = [
        FlameType::OneAlongPath,
        FlameType::MultipleAlongPath,
        FlameType::MultipleOneDirection,
        FlameType::MultiplePathDirected,
        FlameType::MultipleVariousAngle,
        FlameType::CandleLight,
    ];
}

/// Photopea's Flame Style.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FlameStyle {
    Normal,
    Violent,
    Flat,
}

impl FlameStyle {
    pub const ALL: [FlameStyle; 3] = [FlameStyle::Normal, FlameStyle::Violent, FlameStyle::Flat];
}

/// Photopea's Flame Shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FlameShape {
    Parallel,
    ToCenter,
    Spread,
    Oval,
    Pointing,
}

impl FlameShape {
    pub const ALL: [FlameShape; 5] = [
        FlameShape::Parallel,
        FlameShape::ToCenter,
        FlameShape::Spread,
        FlameShape::Oval,
        FlameShape::Pointing,
    ];

    /// A strand's sideways offset at height `t` (0 base, 1 tip) for a strand
    /// whose base sits `b` pixels off the flame's axis.
    fn offset(self, b: f32, t: f32) -> f32 {
        match self {
            FlameShape::Parallel => b,
            FlameShape::ToCenter => b * (1.0 - t),
            FlameShape::Spread => b * (1.0 + 0.8 * t),
            FlameShape::Oval => b * (0.5 + 0.9 * (std::f32::consts::PI * t).sin()),
            FlameShape::Pointing => b * (1.0 - t) * (1.0 - t),
        }
    }
}

/// Photopea's eighteen Flame controls. Ranges are its dialog's; values out
/// of range are clamped when rendered.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FlameSettings {
    pub flame_type: FlameType,
    /// Pixels, 20..=1000.
    pub length: f32,
    pub randomize_length: bool,
    /// Pixels, 5..=600.
    pub width: f32,
    /// Degrees, 0..=360.
    pub angle: f32,
    /// Pixels between flames, 10..=200.
    pub interval: f32,
    /// On a closed subpath, stretch the interval so the flames fit evenly.
    pub adapt_interval: bool,
    /// Straight linear RGB.
    pub color: [f32; 3],
    /// 0 (Draft) ..= 4 (Fine).
    pub quality: u32,
    /// 0..=100.
    pub turbulent: f32,
    /// 0..=100.
    pub jag: f32,
    /// Percent, 0..=100.
    pub opacity: f32,
    /// Strands per flame, 2..=30.
    pub lines: u32,
    /// 0..=100.
    pub bottom: f32,
    pub style: FlameStyle,
    pub shape: FlameShape,
    pub randomize_shape: bool,
    /// 0..=100.
    pub seed: u32,
}

/// Photopea's default Flame colour, as sRGB bytes (255, 110, 28).
pub const DEFAULT_COLOR_SRGB8: [u8; 3] = [255, 110, 28];

impl Default for FlameSettings {
    /// Photopea's `Flam` descriptor defaults.
    fn default() -> Self {
        Self {
            flame_type: FlameType::OneAlongPath,
            length: 140.0,
            randomize_length: false,
            width: 100.0,
            angle: 0.0,
            interval: 100.0,
            adapt_interval: false,
            color: DEFAULT_COLOR_SRGB8.map(|c| color::srgb_to_linear(f32::from(c) / 255.0)),
            quality: 1,
            turbulent: 50.0,
            jag: 0.0,
            opacity: 25.0,
            lines: 10,
            bottom: 30.0,
            style: FlameStyle::Normal,
            shape: FlameShape::Parallel,
            randomize_shape: false,
            seed: 18,
        }
    }
}

/// Most flames one run places (a very long path at the smallest interval).
pub const MAX_FLAMES: usize = 4096;

fn finite_clamp(v: f32, lo: f32, hi: f32, fallback: f32) -> f32 {
    if v.is_finite() {
        v.clamp(lo, hi)
    } else {
        fallback
    }
}

impl FlameSettings {
    fn sanitized(&self) -> Self {
        let d = Self::default();
        Self {
            length: finite_clamp(self.length, 20.0, 1000.0, d.length),
            width: finite_clamp(self.width, 5.0, 600.0, d.width),
            angle: finite_clamp(self.angle, 0.0, 360.0, 0.0),
            interval: finite_clamp(self.interval, 10.0, 200.0, d.interval),
            color: self.color.map(|c| finite_clamp(c, 0.0, 1.0, 0.0)),
            quality: self.quality.min(4),
            turbulent: finite_clamp(self.turbulent, 0.0, 100.0, 0.0),
            jag: finite_clamp(self.jag, 0.0, 100.0, 0.0),
            opacity: finite_clamp(self.opacity, 0.0, 100.0, 0.0),
            lines: self.lines.clamp(2, 30),
            bottom: finite_clamp(self.bottom, 0.0, 100.0, 0.0),
            seed: self.seed.min(100),
            ..*self
        }
    }

    /// Linear colour of heat `t` in `[0, 1]`: the flame colour rising to it
    /// through the lower half, then on to white at the core.
    fn heat_color(&self, t: f32) -> [f32; 3] {
        let t = t.clamp(0.0, 1.0);
        if t < 0.5 {
            return self.color.map(|c| c * t * 2.0);
        }
        let k = ((t - 0.5) * 2.0).powf(1.5);
        self.color.map(|c| c + (1.0 - c) * k)
    }
}

/// Arc-length walk along one flattened subpath.
struct Walk {
    points: Vec<[f32; 2]>,
    lengths: Vec<f32>,
    closed: bool,
}

impl Walk {
    fn new(path: &FlamePath) -> Option<Self> {
        let mut points: Vec<[f32; 2]> = path
            .points
            .iter()
            .copied()
            .filter(|p| p[0].is_finite() && p[1].is_finite())
            .collect();
        points.dedup();
        let closed = path.closed && points.len() > 2;
        if closed {
            points.push(points[0]);
        }
        if points.len() < 2 {
            return None;
        }
        let mut lengths = vec![0.0f32];
        let mut total = 0.0;
        for pair in points.windows(2) {
            total += ((pair[1][0] - pair[0][0]).powi(2) + (pair[1][1] - pair[0][1]).powi(2)).sqrt();
            lengths.push(total);
        }
        (total > 0.0).then_some(Self {
            points,
            lengths,
            closed,
        })
    }

    fn length(&self) -> f32 {
        self.lengths.last().copied().unwrap_or(0.0)
    }

    /// The point and unit tangent `s` pixels along, clamped to the ends.
    fn at(&self, s: f32) -> ([f32; 2], [f32; 2]) {
        let s = s.clamp(0.0, self.length());
        let seg = match self.lengths.iter().position(|&l| l > s) {
            Some(0) => 0,
            Some(i) => i - 1,
            None => self.points.len() - 2,
        };
        let (a, b) = (self.points[seg], self.points[seg + 1]);
        let len = (self.lengths[seg + 1] - self.lengths[seg]).max(1e-6);
        let t = (s - self.lengths[seg]) / len;
        (
            [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t],
            [(b[0] - a[0]) / len, (b[1] - a[1]) / len],
        )
    }
}

/// The left normal of `tangent` in a y-down image: up for a path drawn left
/// to right.
fn left_normal(tangent: [f32; 2]) -> [f32; 2] {
    [tangent[1], -tangent[0]]
}

/// `v` turned `degrees` clockwise (on screen, y down).
fn turn(v: [f32; 2], degrees: f32) -> [f32; 2] {
    let (sin, cos) = degrees.to_radians().sin_cos();
    [v[0] * cos - v[1] * sin, v[0] * sin + v[1] * cos]
}

/// The heat plane and the stamp that fills it.
struct Heat {
    plane: Vec<f32>,
    w: u32,
    h: u32,
}

impl Heat {
    /// A soft disc of `value` at `c`, keeping the maximum. The profile is
    /// `(1 - (d/r)^2)^2`, zero at the rim.
    fn stamp(&mut self, c: [f32; 2], r: f32, value: f32) {
        let r = r.max(0.5);
        if !(c[0].is_finite() && c[1].is_finite()) || value <= 0.0 {
            return;
        }
        let x0 = (c[0] - r).floor().max(0.0) as u32;
        let y0 = (c[1] - r).floor().max(0.0) as u32;
        let x1 = ((c[0] + r).ceil().max(0.0) as u32).min(self.w);
        let y1 = ((c[1] + r).ceil().max(0.0) as u32).min(self.h);
        for y in y0..y1 {
            for x in x0..x1 {
                let (dx, dy) = (x as f32 + 0.5 - c[0], y as f32 + 0.5 - c[1]);
                let q = (dx * dx + dy * dy) / (r * r);
                if q < 1.0 {
                    let v = value * (1.0 - q) * (1.0 - q);
                    let cell = &mut self.plane[(y * self.w + x) as usize];
                    if v > *cell {
                        *cell = v;
                    }
                }
            }
        }
    }
}

/// How one run sways and fades, from Style, Turbulent, Jag and Quality.
struct Motion {
    /// Noise frequency of the slow sway along a strand.
    freq: f32,
    /// Sway amplitude, pixels per unit of height.
    sway: f32,
    /// Jag amplitude, pixels per unit of height.
    jag: f32,
    /// Heat falloff exponent towards the tip.
    decay: f32,
    /// Stamping step, pixels.
    step: f32,
}

impl Motion {
    fn of(s: &FlameSettings, width: f32) -> Self {
        let (freq, amp, decay) = match s.style {
            FlameStyle::Normal => (2.0, 1.0, 0.6),
            FlameStyle::Violent => (4.5, 1.7, 0.8),
            FlameStyle::Flat => (1.2, 0.5, 0.3),
        };
        Self {
            freq,
            sway: width * 0.6 * amp * s.turbulent / 100.0,
            jag: width * 0.25 * s.jag / 100.0,
            decay,
            step: [4.0, 2.0, 1.0, 0.75, 0.5][s.quality.min(4) as usize],
        }
    }

    /// The sideways drift of strand `strand` of flame `flame` at height `t`.
    fn drift(&self, noise: &Perlin, flame: u32, strand: u32, t: f32) -> f32 {
        let (f, k) = (flame as f32, strand as f32);
        let slow = noise.fbm(f * 7.31 + k * 1.7, t * self.freq, 2, 0.5) * self.sway;
        let fast = if self.jag > 0.0 {
            noise.fbm(f * 3.1 + k * 5.3 + 100.0, t * 12.0, 1, 0.5) * self.jag
        } else {
            0.0
        };
        (slow + fast) * t
    }
}

/// One flame of the Multiple / Candle types.
struct Placed {
    base: [f32; 2],
    dir: [f32; 2],
    len: f32,
    width: f32,
}

/// Filter ▸ Render ▸ Flame: render flames along `paths` (flattened
/// subpaths in the buffer's pixel space) with Photopea's controls, screened
/// over `src`. [`NoPath`] when no subpath has any length — Photopea's
/// "Make a path first".
pub fn flame(
    src: &FilterBuffer,
    settings: &FlameSettings,
    paths: &[FlamePath],
) -> Result<FilterBuffer, NoPath> {
    let walks: Vec<Walk> = paths.iter().filter_map(Walk::new).collect();
    if walks.is_empty() {
        return Err(NoPath);
    }
    if src.is_empty() {
        return Ok(src.clone());
    }
    let s = settings.sanitized();
    let (w, h) = src.dimensions();
    let mut heat = Heat {
        plane: vec![0.0; (w as usize) * (h as usize)],
        w,
        h,
    };
    let mut rng = Rng::new(u64::from(s.seed) ^ 0xF1A3_E5ED);
    let noise = Perlin::new(u64::from(s.seed));
    let n = s.lines;
    let strand_r = (s.width / n as f32 * 0.9).max(1.0);
    let lateral = |k: u32| (k as f32 / (n - 1) as f32 - 0.5) * s.width;
    let pick_shape = |rng: &mut Rng| {
        if s.randomize_shape {
            FlameShape::ALL[(rng.next_f32() * 5.0) as usize % 5]
        } else {
            s.shape
        }
    };

    if s.flame_type == FlameType::OneAlongPath {
        let motion = Motion::of(&s, s.width);
        for (fi, walk) in walks.iter().enumerate() {
            let total = walk.length();
            for k in 0..n {
                let shape = pick_shape(&mut rng);
                let b = lateral(k);
                let start = rng.next_f32() * s.bottom / 100.0 * 0.3 * total;
                let end = total * (0.85 + 0.15 * rng.next_f32());
                let mut d = start;
                while d <= end {
                    let t = d / total;
                    let (p, tan) = walk.at(d);
                    let nrm = left_normal(tan);
                    let off = shape.offset(b, t) + motion.drift(&noise, fi as u32, k, t);
                    let c = [p[0] + nrm[0] * off, p[1] + nrm[1] * off];
                    let r = strand_r * (1.0 - t).powf(0.7) + 0.5;
                    heat.stamp(c, r, (1.0 - t).powf(motion.decay));
                    d += motion.step;
                }
            }
            // The translucent body between the strands.
            if s.opacity > 0.0 {
                let mut d = 0.0;
                while d <= total {
                    let t = d / total;
                    let (p, _) = walk.at(d);
                    let r = 0.5 * s.width * (1.0 - t).powf(0.5) + 0.5;
                    heat.stamp(p, r, s.opacity / 100.0 * (1.0 - t).powf(motion.decay));
                    d += motion.step.max(r * 0.25);
                }
            }
        }
    } else {
        let mut placed: Vec<Placed> = Vec::new();
        for walk in &walks {
            let total = walk.length();
            if s.flame_type == FlameType::CandleLight {
                placed.push(Placed {
                    base: walk.at(0.0).0,
                    dir: [0.0, -1.0],
                    len: s.width * 2.5,
                    width: s.width,
                });
                continue;
            }
            let interval = if s.adapt_interval && walk.closed {
                total / (total / s.interval).round().max(1.0)
            } else {
                s.interval
            };
            let mut at = 0.0;
            // A closed loop's end is its start: do not burn there twice.
            while (at < total || (!walk.closed && at <= total)) && placed.len() < MAX_FLAMES {
                let (p, tan) = walk.at(at);
                let nrm = left_normal(tan);
                let dir = match s.flame_type {
                    FlameType::MultipleOneDirection => turn([0.0, -1.0], s.angle),
                    FlameType::MultiplePathDirected => turn(nrm, s.angle),
                    FlameType::MultipleVariousAngle => {
                        turn(nrm, s.angle + rng.next_signed() * 60.0)
                    }
                    _ => nrm,
                };
                let len = if s.randomize_length {
                    s.length * (0.5 + rng.next_f32())
                } else {
                    s.length
                };
                placed.push(Placed {
                    base: p,
                    dir,
                    len,
                    width: s.width,
                });
                at += interval;
            }
        }
        for (fi, f) in placed.iter().enumerate() {
            let motion = Motion::of(&s, f.width);
            let across = [-f.dir[1], f.dir[0]];
            let strand_r = (f.width / n as f32 * 0.9).max(1.0);
            for k in 0..n {
                let shape = pick_shape(&mut rng);
                let b = (k as f32 / (n - 1) as f32 - 0.5) * f.width;
                let centre = (2.0 * k as f32 / (n - 1) as f32 - 1.0).powi(2);
                let strand_len = f.len * (1.0 - 0.35 * centre) * (0.9 + 0.2 * rng.next_f32());
                let start = rng.next_f32() * s.bottom / 100.0 * 0.3 * strand_len;
                let mut d = start;
                while d <= strand_len {
                    let t = d / strand_len;
                    let off = shape.offset(b, t) + motion.drift(&noise, fi as u32, k, t);
                    let c = [
                        f.base[0] + f.dir[0] * d + across[0] * off,
                        f.base[1] + f.dir[1] * d + across[1] * off,
                    ];
                    let r = strand_r * (1.0 - t).powf(0.7) + 0.5;
                    heat.stamp(c, r, (1.0 - t).powf(motion.decay));
                    d += motion.step;
                }
            }
            if s.opacity > 0.0 {
                let mut d = 0.0;
                while d <= f.len {
                    let t = d / f.len;
                    let r = 0.5 * f.width * (1.0 - t).powf(0.5) + 0.5;
                    let c = [f.base[0] + f.dir[0] * d, f.base[1] + f.dir[1] * d];
                    heat.stamp(c, r, s.opacity / 100.0 * (1.0 - t).powf(motion.decay));
                    d += motion.step.max(r * 0.25);
                }
            }
        }
    }

    let mut out = src.same_size_blank();
    let plane = heat.plane;
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let t = plane[(y * w + x) as usize];
        let px = src.get(x, y);
        if t <= 0.0 {
            return px;
        }
        screen(px, s.heat_color(t), t.sqrt().min(1.0))
    });
    Ok(out)
}

/// Screen `light` (straight linear colour) at coverage `a` over a
/// premultiplied pixel.
fn screen(px: [f32; 4], light: [f32; 3], a: f32) -> [f32; 4] {
    let a = a.clamp(0.0, 1.0);
    let mut out = px;
    for c in 0..3 {
        let l = light[c] * a;
        out[c] = 1.0 - (1.0 - px[c].clamp(0.0, 1.0)) * (1.0 - l.clamp(0.0, 1.0));
    }
    out[3] = px[3].max(a);
    for c in 0..3 {
        out[c] = out[c].min(out[3]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn black(w: u32, h: u32) -> FilterBuffer {
        FilterBuffer::filled(w, h, [0.0, 0.0, 0.0, 1.0]).unwrap()
    }

    fn line(a: [f32; 2], b: [f32; 2]) -> Vec<FlamePath> {
        vec![FlamePath {
            points: vec![a, b],
            closed: false,
        }]
    }

    /// Distance from `p` to the segment `a`-`b`.
    fn to_segment(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let t = (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
        ((p[0] - a[0] - t * dx).powi(2) + (p[1] - a[1] - t * dy).powi(2)).sqrt()
    }

    #[test]
    fn no_path_is_photopeas_refusal() {
        let src = black(32, 32);
        let s = FlameSettings::default();
        assert_eq!(flame(&src, &s, &[]), Err(NoPath));
        // A subpath with no length is no path either.
        let dot = vec![FlamePath {
            points: vec![[5.0, 5.0], [5.0, 5.0]],
            closed: false,
        }];
        assert_eq!(flame(&src, &s, &dot), Err(NoPath));
        assert_eq!(NoPath.to_string(), "Make a path first");
    }

    /// Flames along a path paint near it and nowhere far from it, for every
    /// type: nothing lit farther than a flame's reach (length + width), and
    /// the path's own neighbourhood lit.
    #[test]
    fn a_flame_along_a_path_paints_near_the_path_and_not_far_from_it() {
        let src = black(256, 256);
        let (a, b) = ([60.0, 180.0], [200.0, 180.0]);
        let path = line(a, b);
        for flame_type in FlameType::ALL {
            let s = FlameSettings {
                flame_type,
                length: 40.0,
                width: 20.0,
                interval: 20.0,
                ..FlameSettings::default()
            };
            let out = flame(&src, &s, &path).expect("a path");
            let reach = match flame_type {
                FlameType::CandleLight => s.width * 2.5 + s.width,
                FlameType::OneAlongPath => s.width,
                _ => s.length * 1.3 + s.width,
            };
            let mut near = 0;
            for y in 0..256u32 {
                for x in 0..256u32 {
                    let p = [x as f32 + 0.5, y as f32 + 0.5];
                    let lit = out.get(x, y) != src.get(x, y);
                    let d = to_segment(p, a, b);
                    assert!(
                        !lit || d <= reach,
                        "{flame_type:?}: ({x}, {y}) lit {d} px from the path (reach {reach})"
                    );
                    if lit && d < 12.0 {
                        near += 1;
                    }
                }
            }
            assert!(
                near > 40,
                "{flame_type:?}: only {near} lit pixels by the path"
            );
            // Only brightens, and warm by default: red never below green.
            assert!(out.pixels().iter().all(|p| p[0] + 1e-6 >= p[1]));
        }
    }

    /// The flames follow the path's shape: a path in the top-left corner
    /// lights the top-left, and the same path moved to the bottom-right
    /// lights there instead.
    #[test]
    fn the_flames_go_where_the_path_goes() {
        let src = black(200, 200);
        let s = FlameSettings {
            flame_type: FlameType::MultipleAlongPath,
            length: 30.0,
            width: 16.0,
            interval: 10.0,
            ..FlameSettings::default()
        };
        let lit_in = |out: &FilterBuffer, x0: u32, y0: u32| {
            (y0..y0 + 100)
                .flat_map(|y| (x0..x0 + 100).map(move |x| (x, y)))
                .filter(|(x, y)| out.get(*x, *y) != src.get(*x, *y))
                .count()
        };
        let tl = flame(&src, &s, &line([10.0, 80.0], [90.0, 80.0])).unwrap();
        assert!(lit_in(&tl, 0, 0) > 200);
        assert_eq!(lit_in(&tl, 100, 100), 0);
        let br = flame(&src, &s, &line([110.0, 180.0], [190.0, 180.0])).unwrap();
        assert!(lit_in(&br, 100, 100) > 200);
        assert_eq!(lit_in(&br, 0, 0), 0);
    }

    #[test]
    fn the_same_settings_render_the_same_flames_and_the_seed_moves_them() {
        let src = black(96, 96);
        let path = line([10.0, 70.0], [86.0, 70.0]);
        let s = FlameSettings {
            width: 20.0,
            ..FlameSettings::default()
        };
        let a = flame(&src, &s, &path).unwrap();
        assert_eq!(a, flame(&src, &s, &path).unwrap());
        let other = FlameSettings { seed: 19, ..s };
        assert_ne!(a, flame(&src, &other, &path).unwrap());
    }

    #[test]
    fn the_defaults_are_photopeas_descriptor() {
        let d = FlameSettings::default();
        assert_eq!(d.flame_type, FlameType::OneAlongPath);
        assert_eq!(
            (d.length, d.width, d.angle, d.interval),
            (140.0, 100.0, 0.0, 100.0)
        );
        assert_eq!((d.quality, d.lines, d.seed), (1, 10, 18));
        assert_eq!(
            (d.turbulent, d.jag, d.opacity, d.bottom),
            (50.0, 0.0, 25.0, 30.0)
        );
        assert!(!d.randomize_length && !d.adapt_interval && !d.randomize_shape);
        assert!((d.color[0] - 1.0).abs() < 1e-6);
        assert!((d.color[1] - color::srgb_to_linear(110.0 / 255.0)).abs() < 1e-6);
    }
}
