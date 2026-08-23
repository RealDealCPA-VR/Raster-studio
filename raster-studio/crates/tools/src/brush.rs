//! The brush engine: how a pointer path becomes a sequence of dabs.
//!
//! Three ideas do all the work, and they are the three things a user feels.
//!
//! **Stamping.** A stroke is not a polygon; it is a rubber stamp pressed down
//! every `spacing × diameter` pixels along the path. Input samples arrive
//! sparsely — a fast flick may jump sixty pixels between two events — so the
//! engine walks the *segment between* samples and stamps along it, carrying the
//! leftover distance into the next segment. Nothing is ever dotted, and the dab
//! count depends on the path length rather than on how often the OS happened to
//! poll the stylus.
//!
//! **Flow is not opacity.** Flow is how much paint one dab lays down; opacity
//! is how dark the whole stroke may ever get. They are applied at different
//! places — flow inside [`crate::stroke::StrokeBuffer`] as dabs accumulate,
//! opacity once, when the finished stroke is composited — which is why a
//! low-flow airbrush builds up as you scrub over the same spot and a 50 %
//! opacity stroke stays at 50 % no matter how many times you cross it.
//!
//! **Stabilisation.** Raw stylus input is jittery. The engine low-passes the
//! position it stamps along, and pulls the filter to the true endpoint when the
//! pointer lifts so the stroke still ends where the hand did.

use glam::{IVec2, Vec2};
use serde::{Deserialize, Serialize};

use crate::error::{finite, ToolError};

/// Deterministic brush parameters: the same settings and the same input points
/// produce the same dabs, which is what makes a recorded stroke replayable.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BrushSettings {
    /// Diameter in document pixels.
    pub size: f32,
    /// Fraction of the radius that is at full strength before the falloff
    /// starts. `1.0` is a hard-edged (still anti-aliased) disc, `0.0` a pure
    /// gradient.
    pub hardness: f32,
    /// Distance between dabs as a fraction of the diameter.
    pub spacing: f32,
    /// Dab rotation in radians; only visible when `roundness < 1`.
    pub angle: f32,
    /// Minor/major axis ratio of the dab ellipse, `0..=1`.
    pub roundness: f32,
    /// Ceiling on the whole stroke's coverage.
    pub opacity: f32,
    /// How much paint a single dab lays down.
    pub flow: f32,
    /// Input stabilisation, `0.0` (raw) to just under `1.0` (very smooth).
    pub smoothing: f32,
    /// Stylus pressure scales dab size.
    pub size_pressure: bool,
    /// Stylus pressure scales dab flow.
    pub flow_pressure: bool,
    /// Dab size at zero pressure, as a fraction of `size`.
    pub min_size_ratio: f32,
    /// Skip anti-aliasing: every pixel is fully in or fully out (the pencil).
    pub aliased: bool,
}

impl Default for BrushSettings {
    fn default() -> Self {
        Self {
            size: 24.0,
            hardness: 0.8,
            spacing: 0.25,
            angle: 0.0,
            roundness: 1.0,
            opacity: 1.0,
            flow: 1.0,
            smoothing: 0.0,
            size_pressure: true,
            flow_pressure: false,
            min_size_ratio: 0.1,
            aliased: false,
        }
    }
}

impl BrushSettings {
    /// A hard, aliased, one-pixel-per-sample brush — the pencil.
    pub fn pencil(size: f32) -> Self {
        Self {
            size,
            hardness: 1.0,
            spacing: 0.1,
            aliased: true,
            size_pressure: false,
            ..Self::default()
        }
    }

    /// Reject values that would make the engine produce NaN geometry or loop
    /// forever, and clamp the ones with a meaningful saturating limit.
    pub fn validated(mut self) -> Result<Self, ToolError> {
        finite("brush size", self.size)?;
        finite("brush spacing", self.spacing)?;
        finite("brush hardness", self.hardness)?;
        finite("brush angle", self.angle)?;
        finite("brush roundness", self.roundness)?;
        finite("brush opacity", self.opacity)?;
        finite("brush flow", self.flow)?;
        finite("brush smoothing", self.smoothing)?;
        finite("brush min size ratio", self.min_size_ratio)?;
        if self.size <= 0.0 {
            return Err(ToolError::Degenerate);
        }
        self.hardness = self.hardness.clamp(0.0, 1.0);
        // A zero or negative spacing would stamp infinitely many dabs on a
        // finite path; a spacing above 10 diameters is indistinguishable from
        // "one dab" and keeps the arithmetic sane.
        self.spacing = self.spacing.clamp(0.01, 10.0);
        self.roundness = self.roundness.clamp(0.01, 1.0);
        self.opacity = self.opacity.clamp(0.0, 1.0);
        self.flow = self.flow.clamp(0.0, 1.0);
        // Never 1.0: a filter with coefficient 1 never converges on the input.
        self.smoothing = self.smoothing.clamp(0.0, 0.99);
        self.min_size_ratio = self.min_size_ratio.clamp(0.0, 1.0);
        Ok(self)
    }

    /// Distance between dab centres, in pixels.
    pub fn step(&self) -> f32 {
        (self.size * self.spacing).max(0.1)
    }

    /// The radius a dab gets at this pressure.
    pub fn radius_at(&self, pressure: f32) -> f32 {
        let p = pressure.clamp(0.0, 1.0);
        let scale = if self.size_pressure {
            self.min_size_ratio + (1.0 - self.min_size_ratio) * p
        } else {
            1.0
        };
        self.size * 0.5 * scale
    }

    /// The flow a dab gets at this pressure.
    pub fn flow_at(&self, pressure: f32) -> f32 {
        let p = pressure.clamp(0.0, 1.0);
        if self.flow_pressure {
            self.flow * p
        } else {
            self.flow
        }
    }
}

/// One stamp of the brush.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Dab {
    pub center: Vec2,
    pub radius: f32,
    pub hardness: f32,
    pub angle: f32,
    pub roundness: f32,
    /// How much coverage this single dab contributes, `0..=1`.
    pub flow: f32,
    pub aliased: bool,
}

impl Dab {
    /// Normalised distance from the centre in the dab's own elliptical frame:
    /// `1.0` is the rim, whatever the angle and roundness.
    fn norm(&self, p: Vec2) -> f32 {
        let d = p - self.center;
        let (s, c) = self.angle.sin_cos();
        let x = d.x * c + d.y * s;
        let y = -d.x * s + d.y * c;
        let rx = self.radius.max(1e-4);
        let ry = (self.radius * self.roundness).max(1e-4);
        ((x / rx).powi(2) + (y / ry).powi(2)).sqrt()
    }

    /// The hard core plus its falloff.
    ///
    /// The core is `hardness` of the radius and the rest is a smoothstep out to
    /// the rim. At `hardness == 1` the core would leave no room for a ramp, so
    /// it is capped one pixel short of the rim — that pixel is the
    /// anti-aliasing, and without it a hard brush stair-steps.
    fn falloff(&self, n: f32) -> f32 {
        if n >= 1.0 {
            return 0.0;
        }
        let aa = (1.0 / self.radius.max(0.5)).min(0.5);
        let inner = self.hardness.clamp(0.0, 1.0).min(1.0 - aa);
        if n <= inner {
            return 1.0;
        }
        let t = ((n - inner) / (1.0 - inner)).clamp(0.0, 1.0);
        1.0 - t * t * (3.0 - 2.0 * t)
    }

    /// Coverage at an exact point, before `flow`.
    pub fn coverage_at(&self, p: Vec2) -> f32 {
        self.falloff(self.norm(p))
    }

    /// The pixel the dab's centre falls inside.
    ///
    /// Floor, not round: pixel `(x, y)` owns the half-open square
    /// `[x, x+1) × [y, y+1)`, so the centre `(10.0, 10.0)` belongs to pixel
    /// `(10, 10)` and `(10.9, 10.9)` belongs to the same one.
    pub fn center_pixel(&self) -> IVec2 {
        IVec2::new(self.center.x.floor() as i32, self.center.y.floor() as i32)
    }

    /// Coverage of a whole pixel, before `flow`.
    ///
    /// Four samples inside the pixel, because a two-pixel brush sampled only at
    /// its centre is a rectangle. The pencil skips this: an aliased tool is
    /// *defined* by its hard pixel decision.
    ///
    /// That decision has to be *exclusive* on the rim. An inclusive `<= 1.0`
    /// test looks harmless until you notice where a one-pixel pencil actually
    /// lands: with radius `0.5`, a dab centred on a half-integer coordinate
    /// sits exactly `0.5` from the centres of the two pixels above and below
    /// it, both score `norm == 1.0`, and a line drawn along integer document
    /// coordinates — the ordinary mouse-driven case — comes out two pixels
    /// thick while the same line nudged half a pixel comes out one. A stroke's
    /// width must not depend on its sub-pixel phase.
    ///
    /// So the rim test is strict, and the pixel *containing* the centre is
    /// always stamped. The second half matters: with a strict test alone a
    /// sub-pixel dab centred on a pixel corner is `0.707 / 0.5 = 1.41` away
    /// from every neighbouring pixel centre and would paint nothing at all.
    /// Together the two rules give exactly one pixel per dab below about
    /// three-quarters of a pixel of radius, and a stable, never-empty footprint
    /// above it — including for the long thin ellipses a rotated, low-roundness
    /// pencil produces, which a blanket "small dabs are one pixel" rule would
    /// wrongly collapse to a dot.
    pub fn coverage_pixel(&self, x: i32, y: i32) -> f32 {
        if self.aliased {
            let cp = self.center_pixel();
            if cp.x == x && cp.y == y {
                return 1.0;
            }
            let c = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
            return if self.norm(c) < 1.0 { 1.0 } else { 0.0 };
        }
        const OFFS: [(f32, f32); 4] = [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)];
        let mut sum = 0.0;
        for (dx, dy) in OFFS {
            sum += self.coverage_at(Vec2::new(x as f32 + dx, y as f32 + dy));
        }
        sum * 0.25
    }

    /// Half-open pixel bounds the dab can possibly touch.
    pub fn bounds(&self) -> (IVec2, IVec2) {
        let r = self.radius.max(0.0) + 1.0;
        let lo = IVec2::new(
            (self.center.x - r).floor() as i32,
            (self.center.y - r).floor() as i32,
        );
        let hi = IVec2::new(
            (self.center.x + r).ceil() as i32 + 1,
            (self.center.y + r).ceil() as i32 + 1,
        );
        (lo, hi)
    }
}

/// Turns pointer samples into dabs: stabilises the path, then stamps along it
/// at a fixed spacing.
#[derive(Debug, Clone)]
pub struct DabEmitter {
    settings: BrushSettings,
    /// Low-passed position the stamping walks along.
    filtered: Vec2,
    /// Where the walk left off (the previous filtered sample).
    cursor: Vec2,
    /// Distance travelled since the last dab, carried across segments.
    since_dab: f32,
    last_pressure: f32,
    dabs: Vec<Dab>,
    /// The raw path, kept for tools whose algorithm wants the gesture rather
    /// than the stamps (quick select, magnetic lasso, patch).
    raw: Vec<Vec2>,
}

impl DabEmitter {
    /// Start a stroke at `pos`, stamping the first dab immediately.
    pub fn begin(settings: BrushSettings, pos: Vec2, pressure: f32) -> Result<Self, ToolError> {
        let settings = settings.validated()?;
        crate::error::finite_pt("stroke point", pos)?;
        let mut me = Self {
            settings,
            filtered: pos,
            cursor: pos,
            since_dab: 0.0,
            last_pressure: pressure.clamp(0.0, 1.0),
            dabs: Vec::new(),
            raw: vec![pos],
        };
        me.stamp(pos, me.last_pressure);
        Ok(me)
    }

    pub fn settings(&self) -> &BrushSettings {
        &self.settings
    }

    pub fn dabs(&self) -> &[Dab] {
        &self.dabs
    }

    pub fn raw_path(&self) -> &[Vec2] {
        &self.raw
    }

    fn stamp(&mut self, center: Vec2, pressure: f32) {
        self.dabs.push(Dab {
            center,
            radius: self.settings.radius_at(pressure),
            hardness: self.settings.hardness,
            angle: self.settings.angle,
            roundness: self.settings.roundness,
            flow: self.settings.flow_at(pressure),
            aliased: self.settings.aliased,
        });
    }

    /// Feed one pointer sample.
    pub fn extend(&mut self, pos: Vec2, pressure: f32) -> Result<(), ToolError> {
        crate::error::finite_pt("stroke point", pos)?;
        self.raw.push(pos);
        let a = 1.0 - self.settings.smoothing;
        self.filtered += (pos - self.filtered) * a;
        let target = self.filtered;
        self.walk_to(target, pressure.clamp(0.0, 1.0));
        Ok(())
    }

    /// Feed the final sample.
    ///
    /// The stabiliser lags behind the hand, so the filtered path would stop
    /// short of where the pointer lifted. The last segment is therefore walked
    /// to the *raw* position: a smoothed stroke still ends exactly where the
    /// user ended it.
    pub fn finish(&mut self, pos: Vec2, pressure: f32) -> Result<(), ToolError> {
        crate::error::finite_pt("stroke point", pos)?;
        self.raw.push(pos);
        self.filtered = pos;
        self.walk_to(pos, pressure.clamp(0.0, 1.0));
        Ok(())
    }

    /// Stamp along the segment from the cursor to `to`, carrying the leftover
    /// distance so spacing is uniform across segment boundaries.
    fn walk_to(&mut self, to: Vec2, pressure: f32) {
        let from = self.cursor;
        let seg = to - from;
        let len = seg.length();
        if !len.is_finite() {
            return;
        }
        if len <= f32::EPSILON {
            self.last_pressure = pressure;
            return;
        }
        let dir = seg / len;
        let step = self.settings.step();
        let p0 = self.last_pressure;
        let mut travelled = 0.0f32;
        loop {
            let need = step - self.since_dab;
            if travelled + need > len {
                break;
            }
            travelled += need;
            self.since_dab = 0.0;
            let t = travelled / len;
            self.stamp(from + dir * travelled, p0 + (pressure - p0) * t);
        }
        self.since_dab += len - travelled;
        self.cursor = to;
        self.last_pressure = pressure;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn straight(settings: BrushSettings, len: f32) -> DabEmitter {
        let mut e = DabEmitter::begin(settings, Vec2::ZERO, 1.0).unwrap();
        e.finish(Vec2::new(len, 0.0), 1.0).unwrap();
        e
    }

    #[test]
    fn spacing_produces_the_expected_dab_count_for_a_known_path_length() {
        // size 20, spacing 0.25 => a dab every 5px. 100px of path, plus the
        // one stamped at the very start, is 21.
        let s = BrushSettings {
            size: 20.0,
            spacing: 0.25,
            smoothing: 0.0,
            ..Default::default()
        };
        assert_eq!(s.step(), 5.0);
        assert_eq!(straight(s, 100.0).dabs().len(), 21);

        // Halving the spacing doubles the dabs.
        let tight = BrushSettings {
            spacing: 0.125,
            ..s
        };
        assert_eq!(straight(tight, 100.0).dabs().len(), 41);
    }

    #[test]
    fn spacing_is_carried_across_segments_so_a_fast_flick_is_not_dotted() {
        let s = BrushSettings {
            size: 20.0,
            spacing: 0.25,
            smoothing: 0.0,
            ..Default::default()
        };
        // One event per 3px: never a whole step, but the residual accumulates.
        let mut e = DabEmitter::begin(s, Vec2::ZERO, 1.0).unwrap();
        for i in 1..=10 {
            e.extend(Vec2::new(i as f32 * 3.0, 0.0), 1.0).unwrap();
        }
        // 30px of path at a 5px step: 6 dabs after the initial one.
        assert_eq!(e.dabs().len(), 7);
        // And they are evenly spaced, not clumped at event boundaries.
        for w in e.dabs().windows(2) {
            let d = (w[1].center - w[0].center).length();
            assert!((d - 5.0).abs() < 1e-3, "gap was {d}");
        }
    }

    #[test]
    fn pressure_changes_dab_size() {
        let s = BrushSettings {
            size: 40.0,
            size_pressure: true,
            min_size_ratio: 0.25,
            ..Default::default()
        };
        assert_eq!(s.radius_at(1.0), 20.0);
        assert_eq!(s.radius_at(0.0), 5.0);

        let mut e = DabEmitter::begin(s, Vec2::ZERO, 1.0).unwrap();
        e.finish(Vec2::new(100.0, 0.0), 0.0).unwrap();
        let first = e.dabs().first().unwrap().radius;
        let last = e.dabs().last().unwrap().radius;
        assert!(
            first > last,
            "radius should fall with pressure: {first} -> {last}"
        );
        assert!((first - 20.0).abs() < 1e-3);
        assert!(last < 6.0);

        // With pressure mapping off, the radius is constant.
        let flat = BrushSettings {
            size_pressure: false,
            ..s
        };
        let mut e2 = DabEmitter::begin(flat, Vec2::ZERO, 1.0).unwrap();
        e2.finish(Vec2::new(100.0, 0.0), 0.0).unwrap();
        assert_eq!(e2.dabs().first().unwrap().radius, 20.0);
        assert_eq!(e2.dabs().last().unwrap().radius, 20.0);
    }

    #[test]
    fn stabilisation_shortens_a_jittery_path_but_still_ends_where_the_hand_did() {
        let jitter: Vec<Vec2> = (0..40)
            .map(|i| Vec2::new(i as f32 * 2.0, if i % 2 == 0 { 6.0 } else { -6.0 }))
            .collect();

        let path_len = |smoothing: f32| -> (f32, Vec2) {
            let s = BrushSettings {
                size: 8.0,
                spacing: 0.25,
                smoothing,
                ..Default::default()
            };
            let mut e = DabEmitter::begin(s, jitter[0], 1.0).unwrap();
            for p in &jitter[1..jitter.len() - 1] {
                e.extend(*p, 1.0).unwrap();
            }
            e.finish(*jitter.last().unwrap(), 1.0).unwrap();
            let len: f32 = e
                .dabs()
                .windows(2)
                .map(|w| (w[1].center - w[0].center).length())
                .sum();
            (len, e.dabs().last().unwrap().center)
        };

        let (raw, raw_end) = path_len(0.0);
        let (smooth, smooth_end) = path_len(0.8);
        assert!(
            smooth < raw * 0.8,
            "stabilisation did not shorten the path: {smooth} vs {raw}"
        );
        // Both end at the last sample: the filter is pulled to the endpoint.
        let target = *jitter.last().unwrap();
        assert!((raw_end - target).length() < 5.0);
        assert!(
            (smooth_end - target).length() < 5.0,
            "smoothed stroke ended at {smooth_end:?}, not near {target:?}"
        );
    }

    #[test]
    fn a_dab_is_opaque_at_the_core_transparent_outside_and_soft_between() {
        let d = Dab {
            center: Vec2::new(0.0, 0.0),
            radius: 10.0,
            hardness: 0.5,
            angle: 0.0,
            roundness: 1.0,
            flow: 1.0,
            aliased: false,
        };
        assert_eq!(d.coverage_at(Vec2::ZERO), 1.0);
        assert_eq!(d.coverage_at(Vec2::new(4.0, 0.0)), 1.0);
        assert_eq!(d.coverage_at(Vec2::new(11.0, 0.0)), 0.0);
        let mid = d.coverage_at(Vec2::new(7.5, 0.0));
        assert!(mid > 0.1 && mid < 0.9, "falloff was {mid}");
        // Monotone outward.
        let mut prev = 1.0;
        for i in 0..=20 {
            let v = d.coverage_at(Vec2::new(i as f32 * 0.5, 0.0));
            assert!(v <= prev + 1e-6, "coverage rose at {i}");
            prev = v;
        }
    }

    #[test]
    fn roundness_and_angle_squash_and_rotate_the_dab() {
        let d = Dab {
            center: Vec2::ZERO,
            radius: 10.0,
            hardness: 1.0,
            angle: std::f32::consts::FRAC_PI_2,
            roundness: 0.2,
            flow: 1.0,
            aliased: true,
        };
        // Rotated a quarter turn: the long axis now runs vertically.
        assert!(d.coverage_at(Vec2::new(0.0, 9.0)) > 0.0);
        assert_eq!(d.coverage_at(Vec2::new(9.0, 0.0)), 0.0);
        assert!(d.coverage_at(Vec2::new(1.5, 0.0)) > 0.0);
    }

    #[test]
    fn an_aliased_dab_has_no_partial_pixels() {
        let d = Dab {
            center: Vec2::new(5.0, 5.0),
            radius: 3.0,
            hardness: 0.0,
            angle: 0.0,
            roundness: 1.0,
            flow: 1.0,
            aliased: true,
        };
        for y in 0..12 {
            for x in 0..12 {
                let c = d.coverage_pixel(x, y);
                assert!(c == 0.0 || c == 1.0, "pencil made {c} at ({x},{y})");
            }
        }
    }

    /// The rim test has to be exclusive, or a sub-pixel dab whose centre lands
    /// on a pixel boundary claims both sides of it and the pencil's width
    /// depends on where the stroke happens to sit within a pixel.
    #[test]
    fn a_sub_pixel_aliased_dab_marks_exactly_the_pixel_its_centre_is_in() {
        let at = |cx: f32, cy: f32| Dab {
            center: Vec2::new(cx, cy),
            radius: BrushSettings::pencil(1.0).radius_at(1.0),
            hardness: 1.0,
            angle: 0.0,
            roundness: 1.0,
            flow: 1.0,
            aliased: true,
        };
        assert_eq!(at(10.0, 10.0).radius, 0.5);

        // Sweep the centre across a whole pixel, including all four corners and
        // both boundary midpoints — the exact positions the inclusive test got
        // wrong.
        for i in 0..=10 {
            for j in 0..=10 {
                let cx = 10.0 + i as f32 * 0.1;
                let cy = 10.0 + j as f32 * 0.1;
                let d = at(cx, cy);
                let mut marked = Vec::new();
                let (lo, hi) = d.bounds();
                for y in lo.y..hi.y {
                    for x in lo.x..hi.x {
                        let c = d.coverage_pixel(x, y);
                        assert!(c == 0.0 || c == 1.0, "aliased dab made {c}");
                        if c > 0.0 {
                            marked.push((x, y));
                        }
                    }
                }
                assert_eq!(
                    marked,
                    vec![(cx.floor() as i32, cy.floor() as i32)],
                    "centre ({cx}, {cy}) marked {marked:?}"
                );
            }
        }
    }

    /// A larger aliased dab is still a disc, and a thin rotated one is still a
    /// streak rather than the single pixel a blanket "small dabs are one pixel"
    /// rule would collapse it to.
    #[test]
    fn a_larger_aliased_dab_keeps_its_shape() {
        let disc = Dab {
            center: Vec2::new(20.5, 20.5),
            radius: 4.0,
            hardness: 1.0,
            angle: 0.0,
            roundness: 1.0,
            flow: 1.0,
            aliased: true,
        };
        let count = |d: &Dab| {
            let (lo, hi) = d.bounds();
            let mut n = 0;
            for y in lo.y..hi.y {
                for x in lo.x..hi.x {
                    if d.coverage_pixel(x, y) > 0.0 {
                        n += 1;
                    }
                }
            }
            n
        };
        // Roughly pi*r^2 = 50 pixels; the exact figure depends on the rim rule,
        // so this only pins that it is a disc and not a dot or a square.
        let n = count(&disc);
        assert!((40..=60).contains(&n), "an r=4 aliased disc marked {n}");

        // Long and thin: 20 px along its major axis, a fraction of a pixel
        // across. It must still be a streak.
        let streak = Dab {
            center: Vec2::new(20.5, 20.5),
            radius: 10.0,
            roundness: 0.03,
            ..disc
        };
        let n = count(&streak);
        assert!(n >= 12, "a thin aliased streak collapsed to {n} pixels");
    }

    #[test]
    fn nonsense_settings_are_refused_or_clamped_rather_than_looping_forever() {
        assert!(BrushSettings {
            size: f32::NAN,
            ..Default::default()
        }
        .validated()
        .is_err());
        assert!(matches!(
            BrushSettings {
                size: 0.0,
                ..Default::default()
            }
            .validated(),
            Err(ToolError::Degenerate)
        ));
        let v = BrushSettings {
            spacing: 0.0,
            smoothing: 1.0,
            roundness: -3.0,
            ..Default::default()
        }
        .validated()
        .unwrap();
        assert!(v.spacing > 0.0 && v.smoothing < 1.0 && v.roundness > 0.0);
        assert!(
            DabEmitter::begin(BrushSettings::default(), Vec2::new(f32::NAN, 0.0), 1.0).is_err()
        );
    }
}
