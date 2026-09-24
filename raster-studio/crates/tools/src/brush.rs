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

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

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
    /// Stylus pressure scales each dab's alpha (Opacity from Pressure): a dab
    /// stamped at pressure `p` lays `p` times the alpha it would lay at full
    /// pressure. Off by default, and absent from older saved brushes, which
    /// therefore load with it off.
    #[serde(default)]
    pub opacity_pressure: bool,
    /// Dab size at zero pressure, as a fraction of `size`.
    pub min_size_ratio: f32,
    /// Skip anti-aliasing: every pixel is fully in or fully out (the pencil).
    pub aliased: bool,
    /// W9-E: the shape every dab stamps — the computed round tip, or a
    /// grayscale sample (Define Brush Preset, an imported `.abr`) scaled to
    /// `size` and rotated/squashed by `angle`/`roundness`. Absent from older
    /// saved brushes, which therefore load round.
    #[serde(default)]
    pub tip: BrushTip,
    /// W9-E: Photopea's Brush-panel dynamics (shape, scatter, colour,
    /// transfer). All off by default and absent from older saved brushes.
    #[serde(default)]
    pub dynamics: BrushDynamics,
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
            opacity_pressure: false,
            min_size_ratio: 0.1,
            aliased: false,
            tip: BrushTip::Round,
            dynamics: BrushDynamics::default(),
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
        self.dynamics = self.dynamics.validated()?;
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

    /// The alpha one dab lays down at this pressure: its [`Self::flow_at`],
    /// further scaled by the pressure itself when `opacity_pressure` is on.
    /// This is the value the stamp path writes into [`Dab::flow`].
    pub fn dab_alpha_at(&self, pressure: f32) -> f32 {
        let flow = self.flow_at(pressure);
        if self.opacity_pressure {
            flow * pressure.clamp(0.0, 1.0)
        } else {
            flow
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
    /// W9-E: the tip this dab stamps; see [`BrushSettings::tip`].
    pub tip: BrushTip,
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
    ///
    /// A sampled tip whose image is not registered in this process (a preset
    /// whose pixels were never loaded) falls back to the round tip rather
    /// than painting nothing.
    pub fn coverage_at(&self, p: Vec2) -> f32 {
        match self.sampled() {
            Some(tip) => self.sampled_coverage(&tip, p),
            None => self.falloff(self.norm(p)),
        }
    }

    /// The registered image behind a [`BrushTip::Sampled`] dab.
    fn sampled(&self) -> Option<Arc<SampledTip>> {
        match self.tip {
            BrushTip::Round => None,
            BrushTip::Sampled(id) => sampled_tip(id),
        }
    }

    /// Coverage of a sampled tip at `p`: the point is taken into the dab's
    /// rotated, squashed frame, where the tip image's longer side spans the
    /// diameter, and the image is sampled bilinearly there.
    fn sampled_coverage(&self, tip: &SampledTip, p: Vec2) -> f32 {
        let d = p - self.center;
        let (s, c) = self.angle.sin_cos();
        let x = d.x * c + d.y * s;
        let y = -d.x * s + d.y * c;
        let rx = self.radius.max(1e-4);
        let ry = (self.radius * self.roundness).max(1e-4);
        let long = tip.width.max(tip.height) as f32;
        let tx = x / rx * long * 0.5 + tip.width as f32 * 0.5;
        let ty = y / ry * long * 0.5 + tip.height as f32 * 0.5;
        tip.sample(tx, ty)
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
        if let Some(tip) = self.sampled() {
            // A sampled tip is its own shape; the round rim rules below do
            // not apply to it. Aliased, it is thresholded at half coverage.
            if self.aliased {
                let c = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
                return if self.sampled_coverage(&tip, c) >= 0.5 {
                    1.0
                } else {
                    0.0
                };
            }
            let mut sum = 0.0;
            for (dx, dy) in PIXEL_SAMPLES {
                sum += self.sampled_coverage(&tip, Vec2::new(x as f32 + dx, y as f32 + dy));
            }
            return sum * 0.25;
        }
        if self.aliased {
            let cp = self.center_pixel();
            if cp.x == x && cp.y == y {
                return 1.0;
            }
            let c = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
            return if self.norm(c) < 1.0 { 1.0 } else { 0.0 };
        }
        let mut sum = 0.0;
        for (dx, dy) in PIXEL_SAMPLES {
            sum += self.coverage_at(Vec2::new(x as f32 + dx, y as f32 + dy));
        }
        sum * 0.25
    }

    /// Half-open pixel bounds the dab can possibly touch.
    pub fn bounds(&self) -> (IVec2, IVec2) {
        // A sampled tip is a rectangle whose longer side is the diameter, so
        // rotated its corners reach up to sqrt(2) radii from the centre.
        let reach = match self.tip {
            BrushTip::Round => 1.0,
            BrushTip::Sampled(_) => std::f32::consts::SQRT_2,
        };
        let r = self.radius.max(0.0) * reach + 1.0;
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
    /// W9-E: the per-stroke generator the dynamics draw from, seeded from
    /// [`BrushDynamics::seed`] and the stroke's first point, so the same
    /// stroke replays to the same dabs.
    rng: StrokeRng,
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
            rng: StrokeRng::for_stroke(settings.dynamics.seed, pos),
            dabs: Vec::new(),
            raw: vec![pos],
        };
        me.stamp(pos, me.last_pressure, Vec2::X);
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

    /// Stamp at `center`, travelling along `dir` (a unit vector; scatter
    /// throws dabs across it).
    ///
    /// With every dynamic off this is exactly one dab of the settings' own
    /// shape. Otherwise it is `count` dabs (fewer by `count_jitter`), each
    /// with its own jittered size, angle, roundness and alpha and thrown
    /// off the path by `scatter` — all drawn from the stroke's seeded
    /// generator, in a fixed order, so a replay is identical.
    fn stamp(&mut self, center: Vec2, pressure: f32, dir: Vec2) {
        let s = self.settings;
        let base = Dab {
            center,
            radius: s.radius_at(pressure),
            hardness: s.hardness,
            angle: s.angle,
            roundness: s.roundness,
            flow: s.dab_alpha_at(pressure),
            aliased: s.aliased,
            tip: s.tip,
        };
        let d = s.dynamics;
        if d.is_off() {
            self.dabs.push(base);
            return;
        }
        let count_draw = self.rng.next_f32();
        let count = ((d.count.max(1) as f32) * (1.0 - d.count_jitter * count_draw))
            .round()
            .max(1.0) as u32;
        let perp = Vec2::new(-dir.y, dir.x);
        for _ in 0..count {
            let draws = [
                self.rng.next_f32(),
                self.rng.next_f32(),
                self.rng.next_f32(),
                self.rng.next_f32(),
                self.rng.next_f32(),
                self.rng.next_f32(),
                self.rng.next_f32(),
            ];
            let mut dab = base;
            let throw = d.scatter * s.size;
            dab.center += perp * throw * (2.0 * draws[0] - 1.0);
            if d.scatter_both_axes {
                dab.center += dir * throw * (2.0 * draws[1] - 1.0);
            }
            if d.size_jitter > 0.0 {
                dab.radius *= (1.0 - d.size_jitter * draws[2]).max(d.min_diameter);
            }
            dab.angle += d.angle_jitter * std::f32::consts::PI * (2.0 * draws[3] - 1.0);
            if d.roundness_jitter > 0.0 {
                dab.roundness *= (1.0 - d.roundness_jitter * draws[4]).max(d.min_roundness);
                dab.roundness = dab.roundness.max(0.01);
            }
            dab.flow *= (1.0 - d.flow_jitter * draws[5]) * (1.0 - d.opacity_jitter * draws[6]);
            self.dabs.push(dab);
        }
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
            self.stamp(from + dir * travelled, p0 + (pressure - p0) * t, dir);
        }
        self.since_dab += len - travelled;
        self.cursor = to;
        self.last_pressure = pressure;
    }
}

/// Where the four coverage samples of one pixel sit: the quarter points of
/// the pixel square.
const PIXEL_SAMPLES: [(f32, f32); 4] = [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)];

// ---------------------------------------------------------------------------
// W9-E: dynamics
// ---------------------------------------------------------------------------

/// Photopea's Brush panel, minus the tip: Shape Dynamics, Scattering, Color
/// Dynamics and Transfer. Every amount is a fraction `0..=1` unless noted,
/// and every one of them is zero — off — by default.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BrushDynamics {
    /// How far below full size a dab may randomly shrink.
    pub size_jitter: f32,
    /// The floor size jitter cannot go under, as a fraction of the size.
    pub min_diameter: f32,
    /// Random rotation, as a fraction of a half turn either way.
    pub angle_jitter: f32,
    /// How far below the brush's roundness a dab may randomly squash.
    pub roundness_jitter: f32,
    /// The floor roundness jitter cannot go under, as a fraction.
    pub min_roundness: f32,
    /// How far a dab is thrown off the path, in diameters (`0..=10`).
    pub scatter: f32,
    /// Throw along the path as well as across it.
    pub scatter_both_axes: bool,
    /// Dabs stamped at every spacing step (`1..=16`).
    pub count: u32,
    /// How much of `count` may randomly be left out.
    pub count_jitter: f32,
    /// Foreground-to-background mix, drawn once per stroke.
    pub fg_bg_jitter: f32,
    /// Hue shift, as a fraction of a half turn either way, per stroke.
    pub hue_jitter: f32,
    /// Saturation shift either way, per stroke.
    pub saturation_jitter: f32,
    /// Brightness shift either way, per stroke.
    pub brightness_jitter: f32,
    /// How much of a dab's alpha may randomly be withheld (Transfer: Opacity
    /// Jitter). Applied per dab, under the stroke's opacity ceiling.
    pub opacity_jitter: f32,
    /// Transfer: Flow Jitter, per dab.
    pub flow_jitter: f32,
    /// The brush's own seed; mixed with the stroke's first point to give
    /// each stroke its own — replayable — sequence.
    pub seed: u64,
}

impl Default for BrushDynamics {
    fn default() -> Self {
        Self {
            size_jitter: 0.0,
            min_diameter: 0.0,
            angle_jitter: 0.0,
            roundness_jitter: 0.0,
            min_roundness: 0.25,
            scatter: 0.0,
            scatter_both_axes: false,
            count: 1,
            count_jitter: 0.0,
            fg_bg_jitter: 0.0,
            hue_jitter: 0.0,
            saturation_jitter: 0.0,
            brightness_jitter: 0.0,
            opacity_jitter: 0.0,
            flow_jitter: 0.0,
            seed: 0,
        }
    }
}

impl BrushDynamics {
    /// Whether the per-dab dynamics are all off, so a spacing step is one
    /// plain dab. (Colour dynamics act per stroke and do not count here.)
    pub fn is_off(&self) -> bool {
        self.size_jitter == 0.0
            && self.angle_jitter == 0.0
            && self.roundness_jitter == 0.0
            && self.scatter == 0.0
            && self.count <= 1
            && self.opacity_jitter == 0.0
            && self.flow_jitter == 0.0
    }

    /// Whether any colour dynamic is on.
    pub fn has_color(&self) -> bool {
        self.fg_bg_jitter > 0.0
            || self.hue_jitter > 0.0
            || self.saturation_jitter > 0.0
            || self.brightness_jitter > 0.0
    }

    /// Refuse non-finite amounts; clamp the rest into their ranges.
    pub fn validated(mut self) -> Result<Self, ToolError> {
        for (what, v) in [
            ("size jitter", self.size_jitter),
            ("minimum diameter", self.min_diameter),
            ("angle jitter", self.angle_jitter),
            ("roundness jitter", self.roundness_jitter),
            ("minimum roundness", self.min_roundness),
            ("scatter", self.scatter),
            ("count jitter", self.count_jitter),
            ("foreground/background jitter", self.fg_bg_jitter),
            ("hue jitter", self.hue_jitter),
            ("saturation jitter", self.saturation_jitter),
            ("brightness jitter", self.brightness_jitter),
            ("opacity jitter", self.opacity_jitter),
            ("flow jitter", self.flow_jitter),
        ] {
            finite(what, v)?;
        }
        let unit = |v: f32| v.clamp(0.0, 1.0);
        self.size_jitter = unit(self.size_jitter);
        self.min_diameter = unit(self.min_diameter);
        self.angle_jitter = unit(self.angle_jitter);
        self.roundness_jitter = unit(self.roundness_jitter);
        self.min_roundness = unit(self.min_roundness);
        self.scatter = self.scatter.clamp(0.0, 10.0);
        self.count = self.count.clamp(1, 16);
        self.count_jitter = unit(self.count_jitter);
        self.fg_bg_jitter = unit(self.fg_bg_jitter);
        self.hue_jitter = unit(self.hue_jitter);
        self.saturation_jitter = unit(self.saturation_jitter);
        self.brightness_jitter = unit(self.brightness_jitter);
        self.opacity_jitter = unit(self.opacity_jitter);
        self.flow_jitter = unit(self.flow_jitter);
        Ok(self)
    }

    /// The colour a stroke starting at `start` paints with: `fg` (straight
    /// alpha, **linear** RGBA, as [`crate::ToolContext::foreground`] holds
    /// it) mixed toward `bg` and shifted in hue, saturation and brightness by
    /// the colour jitters, drawn from the same seed the stroke's dabs use.
    /// With every colour jitter off it is `fg` exactly.
    pub fn stroke_color(&self, fg: [f32; 4], bg: [f32; 4], start: Vec2) -> [f32; 4] {
        if !self.has_color() {
            return fg;
        }
        // A separate stream from the dabs' (a different salt), so turning a
        // colour jitter on does not reshuffle the shape of the stroke.
        let mut rng = StrokeRng::for_stroke(self.seed ^ 0xC010_C010_C010_C010, start);
        let mix = self.fg_bg_jitter * rng.next_f32();
        let mut rgb = [0.0f32; 3];
        for (i, c) in rgb.iter_mut().enumerate() {
            let linear = fg[i] + (bg[i] - fg[i]) * mix;
            *c = color::linear_to_srgb(linear.clamp(0.0, 1.0));
        }
        let mut hsv = color::model::rgb_to_hsv(rgb);
        hsv[0] =
            (hsv[0] + self.hue_jitter * 180.0 * (2.0 * rng.next_f32() - 1.0)).rem_euclid(360.0);
        hsv[1] = (hsv[1] + self.saturation_jitter * (2.0 * rng.next_f32() - 1.0)).clamp(0.0, 1.0);
        hsv[2] = (hsv[2] + self.brightness_jitter * (2.0 * rng.next_f32() - 1.0)).clamp(0.0, 1.0);
        let out = color::model::hsv_to_rgb(hsv);
        [
            color::srgb_to_linear(out[0]),
            color::srgb_to_linear(out[1]),
            color::srgb_to_linear(out[2]),
            fg[3],
        ]
    }
}

/// SplitMix64: small, fast, and — the point — the same sequence on every
/// machine for the same seed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StrokeRng(u64);

impl StrokeRng {
    /// The generator for a stroke that starts at `start` with brush seed
    /// `seed`.
    pub fn for_stroke(seed: u64, start: Vec2) -> Self {
        let pos = (u64::from(start.x.to_bits()) << 32) | u64::from(start.y.to_bits());
        let mut me = Self(seed ^ pos.rotate_left(17) ^ 0x9E37_79B9_7F4A_7C15);
        me.next_u64();
        me
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)`.
    pub fn next_f32(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }
}

// ---------------------------------------------------------------------------
// W9-E: sampled tips
// ---------------------------------------------------------------------------

/// The identity of a sampled tip: the content hash (BLAKE3, computed by
/// `asset_store::presets::tip_hash`) of its pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TipId(pub [u8; 32]);

/// The shape a brush stamps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BrushTip {
    /// The computed elliptical tip with its hardness falloff.
    #[default]
    Round,
    /// A grayscale image registered with [`register_sampled_tip`].
    Sampled(TipId),
}

/// The longest side a sampled tip may have (Photoshop's own limit is 5000).
pub const MAX_TIP_SIDE: u32 = 5000;

/// A grayscale tip image: one coverage byte per pixel, `255` = full paint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SampledTip {
    width: u32,
    height: u32,
    alpha: Vec<u8>,
}

impl SampledTip {
    /// A tip from `width * height` coverage bytes. Refuses an empty image,
    /// one past [`MAX_TIP_SIDE`], or a buffer of the wrong length.
    pub fn new(width: u32, height: u32, alpha: Vec<u8>) -> Result<Self, ToolError> {
        if width == 0 || height == 0 || width > MAX_TIP_SIDE || height > MAX_TIP_SIDE {
            return Err(ToolError::Degenerate);
        }
        if alpha.len() != width as usize * height as usize {
            return Err(ToolError::Degenerate);
        }
        Ok(Self {
            width,
            height,
            alpha,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn alpha(&self) -> &[u8] {
        &self.alpha
    }

    fn texel(&self, x: i64, y: i64) -> f32 {
        if x < 0 || y < 0 || x >= i64::from(self.width) || y >= i64::from(self.height) {
            return 0.0;
        }
        f32::from(self.alpha[y as usize * self.width as usize + x as usize]) / 255.0
    }

    /// Bilinear coverage at `(x, y)` in image pixels (pixel `i` is centred on
    /// `i + 0.5`); zero outside the image.
    pub fn sample(&self, x: f32, y: f32) -> f32 {
        if !x.is_finite() || !y.is_finite() {
            return 0.0;
        }
        let fx = x - 0.5;
        let fy = y - 0.5;
        let x0 = fx.floor();
        let y0 = fy.floor();
        let tx = fx - x0;
        let ty = fy - y0;
        let (x0, y0) = (x0 as i64, y0 as i64);
        let a = self.texel(x0, y0) * (1.0 - tx) + self.texel(x0 + 1, y0) * tx;
        let b = self.texel(x0, y0 + 1) * (1.0 - tx) + self.texel(x0 + 1, y0 + 1) * tx;
        a * (1.0 - ty) + b * ty
    }
}

type TipTable = RwLock<HashMap<TipId, Arc<SampledTip>>>;

fn tip_table() -> &'static TipTable {
    static TIPS: OnceLock<TipTable> = OnceLock::new();
    TIPS.get_or_init(|| RwLock::new(HashMap::new()))
}

thread_local! {
    /// The last tip looked up on this thread: a stroke stamps thousands of
    /// pixels of one tip, and this spares each of them the table's lock.
    static LAST_TIP: RefCell<Option<(TipId, Arc<SampledTip>)>> = const { RefCell::new(None) };
}

/// Make `tip` available to every dab whose [`BrushTip::Sampled`] names `id`.
///
/// The table is process-wide because a [`Dab`] is a small `Copy` value
/// carried through every stroke path; it names its tip, it cannot own it.
/// Ids are content hashes, so registering the same id twice is registering
/// the same pixels.
pub fn register_sampled_tip(id: TipId, tip: SampledTip) -> Arc<SampledTip> {
    let tip = Arc::new(tip);
    if let Ok(mut table) = tip_table().write() {
        table.insert(id, tip.clone());
    }
    tip
}

/// The registered tip `id`, if any.
pub fn sampled_tip(id: TipId) -> Option<Arc<SampledTip>> {
    let cached = LAST_TIP.with(|last| {
        last.borrow()
            .as_ref()
            .filter(|(k, _)| *k == id)
            .map(|(_, t)| t.clone())
    });
    if cached.is_some() {
        return cached;
    }
    let found = tip_table().read().ok()?.get(&id).cloned()?;
    LAST_TIP.with(|last| *last.borrow_mut() = Some((id, found.clone())));
    Some(found)
}

// ---------------------------------------------------------------------------
// W9-E: the brush library
// ---------------------------------------------------------------------------

/// Brushes made outside the Brushes panel — Edit > Define Brush Preset, a
/// `.abr` opened with File > Open — waiting to be listed in it.
///
/// Append-only and process-wide: the panel remembers how many it has taken
/// ([`library_since`]) and picks up the rest on its next frame, which is how a
/// brush the editor makes reaches a panel the editor does not own.
fn library() -> &'static Mutex<Vec<(String, BrushSettings)>> {
    static LIBRARY: OnceLock<Mutex<Vec<(String, BrushSettings)>>> = OnceLock::new();
    LIBRARY.get_or_init(|| Mutex::new(Vec::new()))
}

/// Offer a named brush to the Brushes panel.
pub fn publish_library_brush(name: impl Into<String>, settings: BrushSettings) {
    if let Ok(mut lib) = library().lock() {
        lib.push((name.into(), settings));
    }
}

/// How many brushes have been published so far.
pub fn library_len() -> usize {
    library().lock().map_or(0, |lib| lib.len())
}

/// The published brushes from index `seen` on, and the new count to
/// remember.
pub fn library_since(seen: usize) -> (Vec<(String, BrushSettings)>, usize) {
    match library().lock() {
        Ok(lib) => (lib.get(seen..).unwrap_or_default().to_vec(), lib.len()),
        Err(_) => (Vec::new(), seen),
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
    fn size_pressure_scales_the_stamped_dab_radius() {
        // The crux of native tablet pressure (S1.4 engine side): the emitter
        // maps each sample's pressure into a dab radius when size_pressure is
        // on, so a light stroke lands narrower than a firm one even though the
        // geometry is identical.
        let settings = BrushSettings {
            size: 20.0,
            size_pressure: true,
            min_size_ratio: 0.2,
            smoothing: 0.0,
            ..Default::default()
        };
        let radius_at = |p: f32| {
            let mut e = DabEmitter::begin(settings, Vec2::ZERO, p).unwrap();
            e.finish(Vec2::new(20.0, 0.0), p).unwrap();
            e.dabs()[0].radius
        };
        let full = radius_at(1.0);
        let light = radius_at(0.25);
        assert!(
            light < full,
            "light pressure must land narrower: {light} vs {full}"
        );
        // And the exact mapping: radius_at(1.0)==size/2, radius_at(0.0)==ratio*size/2.
        assert!((settings.radius_at(1.0) - 10.0).abs() < 1e-5);
        assert!((settings.radius_at(0.0) - 2.0).abs() < 1e-5);
        // Flow pressure likewise scales flow but not radius.
        let flow = BrushSettings {
            flow_pressure: true,
            flow: 0.5,
            ..settings
        };
        assert!((flow.flow_at(0.0) - 0.0).abs() < 1e-6);
        assert!((flow.flow_at(1.0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn opacity_pressure_scales_the_stamped_dab_alpha() {
        let settings = BrushSettings {
            size: 20.0,
            size_pressure: false,
            flow: 0.8,
            ..Default::default()
        };
        assert!(!settings.opacity_pressure, "off by default");
        let alpha =
            |s: BrushSettings, p: f32| DabEmitter::begin(s, Vec2::ZERO, p).unwrap().dabs()[0].flow;
        // Off: pressure does not touch the alpha.
        assert!((alpha(settings, 0.25) - 0.8).abs() < 1e-6);
        let on = BrushSettings {
            opacity_pressure: true,
            ..settings
        };
        assert!((alpha(on, 1.0) - 0.8).abs() < 1e-6);
        assert!((alpha(on, 0.25) - 0.2).abs() < 1e-6);
        assert_eq!(alpha(on, 0.0), 0.0);
        // A brush saved before the field existed loads with it off.
        let mut json = serde_json::to_value(settings).unwrap();
        json.as_object_mut().unwrap().remove("opacity_pressure");
        let back: BrushSettings = serde_json::from_value(json).unwrap();
        assert!(!back.opacity_pressure);
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
            tip: BrushTip::Round,
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
            tip: BrushTip::Round,
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
            tip: BrushTip::Round,
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
            tip: BrushTip::Round,
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
            tip: BrushTip::Round,
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

    // ---------------------------------------------------------------- W9-E

    fn jittery(dynamics: BrushDynamics) -> BrushSettings {
        BrushSettings {
            size: 20.0,
            spacing: 0.25,
            size_pressure: false,
            dynamics,
            ..Default::default()
        }
    }

    fn stroke(settings: BrushSettings) -> Vec<Dab> {
        let mut e = DabEmitter::begin(settings, Vec2::new(3.0, 50.0), 1.0).unwrap();
        for i in 1..20 {
            e.extend(Vec2::new(3.0 + i as f32 * 10.0, 50.0), 1.0)
                .unwrap();
        }
        e.finish(Vec2::new(203.0, 50.0), 1.0).unwrap();
        e.dabs().to_vec()
    }

    #[test]
    fn size_jitter_varies_the_dab_radius_and_a_seeded_stroke_replays_identically() {
        let settings = jittery(BrushDynamics {
            size_jitter: 0.8,
            min_diameter: 0.25,
            seed: 7,
            ..Default::default()
        });
        let dabs = stroke(settings);
        let radii: Vec<f32> = dabs.iter().map(|d| d.radius).collect();
        let lo = radii.iter().cloned().fold(f32::MAX, f32::min);
        let hi = radii.iter().cloned().fold(0.0f32, f32::max);
        assert!(hi - lo > 3.0, "radii barely varied: {lo}..{hi}");
        assert!(hi <= 10.0 + 1e-4, "jitter only shrinks: {hi}");
        assert!(lo >= 10.0 * 0.25 - 1e-4, "went under the floor: {lo}");
        // The same settings over the same path are the same dabs.
        assert_eq!(dabs, stroke(settings), "a replay differed");
        // A different seed is a different stroke.
        let other = BrushSettings {
            dynamics: BrushDynamics {
                seed: 8,
                ..settings.dynamics
            },
            ..settings
        };
        assert_ne!(dabs, stroke(other));
        // With the jitter off every dab is full size.
        assert!(stroke(jittery(BrushDynamics::default()))
            .iter()
            .all(|d| d.radius == 10.0));
    }

    #[test]
    fn scatter_throws_dabs_off_the_path_and_count_multiplies_them() {
        let plain = stroke(jittery(BrushDynamics::default()));
        assert!(plain.iter().all(|d| d.center.y == 50.0));
        let scattered = stroke(jittery(BrushDynamics {
            scatter: 1.0,
            count: 3,
            seed: 1,
            ..Default::default()
        }));
        let off = scattered
            .iter()
            .map(|d| (d.center.y - 50.0).abs())
            .fold(0.0f32, f32::max);
        assert!(off > 5.0, "scatter moved dabs at most {off}px off the path");
        assert!(
            scattered
                .iter()
                .all(|d| (d.center.y - 50.0).abs() <= 20.0 + 1e-3),
            "scatter 1.0 throws at most one diameter"
        );
        assert_eq!(scattered.len(), plain.len() * 3, "count is per step");
        // One axis only: along-path position is untouched.
        let xs: Vec<f32> = scattered.iter().map(|d| d.center.x).collect();
        assert!(xs.chunks(3).all(|c| c[0] == c[1] && c[1] == c[2]));
    }

    #[test]
    fn angle_roundness_and_transfer_jitter_vary_the_dab() {
        let dabs = stroke(jittery(BrushDynamics {
            angle_jitter: 1.0,
            roundness_jitter: 0.9,
            min_roundness: 0.2,
            flow_jitter: 0.5,
            opacity_jitter: 0.5,
            seed: 3,
            ..Default::default()
        }));
        let distinct = |f: &dyn Fn(&Dab) -> f32| {
            let mut v: Vec<u32> = dabs.iter().map(|d| f(d).to_bits()).collect();
            v.sort_unstable();
            v.dedup();
            v.len()
        };
        assert!(distinct(&|d| d.angle) > 10);
        assert!(distinct(&|d| d.roundness) > 10);
        assert!(distinct(&|d| d.flow) > 10);
        assert!(dabs.iter().all(|d| d.roundness >= 0.2 - 1e-6));
        assert!(dabs.iter().all(|d| (0.0..=1.0).contains(&d.flow)));
    }

    #[test]
    fn colour_dynamics_shift_the_stroke_colour_deterministically() {
        let fg = [0.8, 0.1, 0.1, 1.0];
        let bg = [0.0, 0.0, 0.9, 1.0];
        let off = BrushDynamics::default();
        assert_eq!(off.stroke_color(fg, bg, Vec2::ZERO), fg);
        let on = BrushDynamics {
            hue_jitter: 0.5,
            fg_bg_jitter: 1.0,
            seed: 11,
            ..Default::default()
        };
        let a = on.stroke_color(fg, bg, Vec2::new(4.0, 4.0));
        assert_eq!(a, on.stroke_color(fg, bg, Vec2::new(4.0, 4.0)));
        let colours: Vec<[f32; 4]> = (0..8)
            .map(|i| on.stroke_color(fg, bg, Vec2::new(i as f32, 0.0)))
            .collect();
        assert!(colours.iter().any(|c| *c != fg), "no stroke was recoloured");
        assert!(colours.windows(2).any(|w| w[0] != w[1]));
        assert!(colours
            .iter()
            .all(|c| c[3] == 1.0 && c[..3].iter().all(|v| (0.0..=1.0).contains(v))));
    }

    #[test]
    fn a_sampled_tip_stamps_its_own_shape() {
        // A 4x4 tip that is ink only in its left half: a vertical bar.
        let mut alpha = vec![0u8; 16];
        for y in 0..4 {
            alpha[y * 4] = 255;
            alpha[y * 4 + 1] = 255;
        }
        let id = TipId([0xB0; 32]);
        register_sampled_tip(id, SampledTip::new(4, 4, alpha).unwrap());
        let dab = Dab {
            center: Vec2::new(20.0, 20.0),
            radius: 8.0,
            hardness: 1.0,
            angle: 0.0,
            roundness: 1.0,
            flow: 1.0,
            aliased: false,
            tip: BrushTip::Sampled(id),
        };
        // Left of centre is ink, right is not — a round tip would paint both.
        assert!(dab.coverage_at(Vec2::new(15.0, 20.0)) > 0.9);
        assert!(
            dab.coverage_at(Vec2::new(15.0, 14.0)) > 0.9,
            "the bar is tall"
        );
        assert_eq!(dab.coverage_at(Vec2::new(25.0, 20.0)), 0.0);
        let round = Dab {
            tip: BrushTip::Round,
            ..dab
        };
        assert!(round.coverage_at(Vec2::new(25.0, 20.0)) > 0.9);
        // Rotated a quarter turn, the bar lies along the top instead.
        let turned = Dab {
            angle: std::f32::consts::FRAC_PI_2,
            ..dab
        };
        assert!(turned.coverage_at(Vec2::new(20.0, 15.0)) > 0.9);
        assert_eq!(turned.coverage_at(Vec2::new(20.0, 25.0)), 0.0);
        // Scaled by the radius: at half the radius the bar is half as far.
        let small = Dab { radius: 4.0, ..dab };
        assert_eq!(small.coverage_at(Vec2::new(15.0, 20.0)), 0.0);
        assert!(small.coverage_at(Vec2::new(18.0, 20.0)) > 0.9);
        // And the emitter carries the tip from the settings onto every dab.
        let settings = BrushSettings {
            tip: BrushTip::Sampled(id),
            ..Default::default()
        };
        let e = straight(settings, 30.0);
        assert!(e.dabs().iter().all(|d| d.tip == BrushTip::Sampled(id)));
        // An unregistered tip falls back to the round shape.
        let missing = Dab {
            tip: BrushTip::Sampled(TipId([0x5E; 32])),
            ..dab
        };
        assert_eq!(
            missing.coverage_at(Vec2::new(25.0, 20.0)),
            round.coverage_at(Vec2::new(25.0, 20.0))
        );
    }

    #[test]
    fn a_sampled_tip_refuses_bad_geometry_and_old_brushes_load_round() {
        assert!(SampledTip::new(0, 4, vec![]).is_err());
        assert!(SampledTip::new(2, 2, vec![0; 3]).is_err());
        assert!(SampledTip::new(MAX_TIP_SIDE + 1, 1, vec![0; MAX_TIP_SIDE as usize + 1]).is_err());
        let mut json = serde_json::to_value(BrushSettings::default()).unwrap();
        json.as_object_mut().unwrap().remove("tip");
        json.as_object_mut().unwrap().remove("dynamics");
        let back: BrushSettings = serde_json::from_value(json).unwrap();
        assert_eq!(back.tip, BrushTip::Round);
        assert_eq!(back.dynamics, BrushDynamics::default());
        let sampled = BrushSettings {
            tip: BrushTip::Sampled(TipId([9; 32])),
            ..Default::default()
        };
        let round_trip: BrushSettings =
            serde_json::from_str(&serde_json::to_string(&sampled).unwrap()).unwrap();
        assert_eq!(round_trip, sampled);
        // Nonsense dynamics are refused or clamped.
        assert!(BrushSettings {
            dynamics: BrushDynamics {
                scatter: f32::NAN,
                ..Default::default()
            },
            ..Default::default()
        }
        .validated()
        .is_err());
        let clamped = BrushSettings {
            dynamics: BrushDynamics {
                count: 999,
                size_jitter: 7.0,
                ..Default::default()
            },
            ..Default::default()
        }
        .validated()
        .unwrap();
        assert_eq!(clamped.dynamics.count, 16);
        assert_eq!(clamped.dynamics.size_jitter, 1.0);
    }
}
