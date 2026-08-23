//! Gradients: five shapes over one multi-stop ramp, dithered.
//!
//! Two things here are easy to get wrong and very visible when they are.
//!
//! **Colour and opacity have separate stop lists.** They are separate in every
//! editor's gradient editor because they genuinely are separate: a
//! transparent-to-red gradient has one colour stop and two opacity stops, and
//! forcing them onto a shared list would make every colour stop carry an alpha
//! the user did not ask about.
//!
//! **Dithering.** A gradient across a wide area steps through very few distinct
//! 8-bit values, and the eye finds every one of those steps. An ordered dither
//! adds just under half an output level of patterned noise before the value is
//! quantised, which turns each hard band edge into a stipple the eye ignores.
//! It costs one addition per pixel and it is the difference between a sky that
//! looks painted and one that looks banded.

use color::{
    linear_to_srgb, premultiply, srgb_to_linear as srgb_to_linear_scalar, srgb_to_linear3,
};
use editor_core::{Command, Selection};
use glam::{IVec2, Vec2};
use raster::PixelRect;
use serde::{Deserialize, Serialize};

use crate::error::{finite, ToolError};
use crate::patch::ColorPatch;
use crate::tool::{PointerEvent, Tool, ToolContext, ToolId};

/// A colour stop, positioned in `0..=1` along the ramp.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ColorStop {
    pub position: f32,
    /// Straight-alpha **linear** RGB; the alpha channel of a colour stop is
    /// ignored — opacity comes from [`OpacityStop`].
    pub color: [f32; 3],
}

/// An opacity stop, positioned in `0..=1` along the ramp.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OpacityStop {
    pub position: f32,
    pub opacity: f32,
}

/// A multi-stop gradient ramp.
///
/// **Never empty.** Both stop lists are private and every constructor refuses
/// an empty one, *including deserialization*: `#[serde(try_from = ..)]` routes
/// a decoded preset back through [`GradientRamp::new`], so a hand-edited or
/// corrupt gradient preset is a deserialization error rather than a ramp that
/// panics the first time it is sampled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RampStops")]
pub struct GradientRamp {
    colors: Vec<ColorStop>,
    opacities: Vec<OpacityStop>,
}

/// The wire shape of a [`GradientRamp`]. Deserialization lands here first and
/// is converted through the checked constructor.
#[derive(Deserialize)]
struct RampStops {
    colors: Vec<ColorStop>,
    opacities: Vec<OpacityStop>,
}

impl TryFrom<RampStops> for GradientRamp {
    type Error = ToolError;

    fn try_from(v: RampStops) -> Result<Self, ToolError> {
        GradientRamp::new(v.colors, v.opacities)
    }
}

impl GradientRamp {
    /// Build a ramp, sorting both stop lists and refusing an empty one or a
    /// non-finite position.
    pub fn new(
        mut colors: Vec<ColorStop>,
        mut opacities: Vec<OpacityStop>,
    ) -> Result<Self, ToolError> {
        if colors.is_empty() || opacities.is_empty() {
            return Err(ToolError::Degenerate);
        }
        for c in &mut colors {
            finite("colour stop position", c.position)?;
            for v in c.color {
                finite("colour stop channel", v)?;
            }
            c.position = c.position.clamp(0.0, 1.0);
        }
        for o in &mut opacities {
            finite("opacity stop position", o.position)?;
            finite("opacity stop value", o.opacity)?;
            o.position = o.position.clamp(0.0, 1.0);
            o.opacity = o.opacity.clamp(0.0, 1.0);
        }
        colors.sort_by(|a, b| a.position.total_cmp(&b.position));
        opacities.sort_by(|a, b| a.position.total_cmp(&b.position));
        Ok(Self { colors, opacities })
    }

    /// The two-stop ramp a gradient tool starts with: foreground to background,
    /// both opaque.
    pub fn two(from: [f32; 3], to: [f32; 3]) -> Self {
        Self {
            colors: vec![
                ColorStop {
                    position: 0.0,
                    color: from,
                },
                ColorStop {
                    position: 1.0,
                    color: to,
                },
            ],
            opacities: vec![
                OpacityStop {
                    position: 0.0,
                    opacity: 1.0,
                },
                OpacityStop {
                    position: 1.0,
                    opacity: 1.0,
                },
            ],
        }
    }

    /// Foreground to fully transparent.
    pub fn to_transparent(from: [f32; 3]) -> Self {
        Self {
            colors: vec![ColorStop {
                position: 0.0,
                color: from,
            }],
            opacities: vec![
                OpacityStop {
                    position: 0.0,
                    opacity: 1.0,
                },
                OpacityStop {
                    position: 1.0,
                    opacity: 0.0,
                },
            ],
        }
    }

    pub fn color_stops(&self) -> &[ColorStop] {
        &self.colors
    }

    pub fn opacity_stops(&self) -> &[OpacityStop] {
        &self.opacities
    }

    /// Straight-alpha linear RGBA at `t`, clamped at both ends.
    ///
    /// Interpolation is in **linear light**, which is the whole reason the ramp
    /// stores linear values: mixing sRGB-encoded red and green halfway gives a
    /// muddy olive, mixing the light gives the yellow the eye expects.
    pub fn sample(&self, t: f32) -> [f32; 4] {
        let t = if t.is_finite() {
            t.clamp(0.0, 1.0)
        } else {
            0.0
        };
        let rgb = interpolate(
            &self.colors,
            t,
            |s| s.position,
            |a, b, k| {
                [
                    a.color[0] + (b.color[0] - a.color[0]) * k,
                    a.color[1] + (b.color[1] - a.color[1]) * k,
                    a.color[2] + (b.color[2] - a.color[2]) * k,
                ]
            },
        )
        .unwrap_or([0.0; 3]);
        let alpha = interpolate(
            &self.opacities,
            t,
            |s| s.position,
            |a, b, k| a.opacity + (b.opacity - a.opacity) * k,
        )
        .unwrap_or(0.0);
        [rgb[0], rgb[1], rgb[2], alpha]
    }
}

/// Piecewise-linear lookup shared by both stop lists.
///
/// Total: an empty list yields `None` rather than indexing. A
/// [`GradientRamp`] cannot hold one — every constructor, deserialization
/// included, refuses it — but this function is the last line before a raw
/// index, and the error module's rule is that nothing in this crate may panic.
fn interpolate<S: Copy, T>(
    stops: &[S],
    t: f32,
    pos: impl Fn(&S) -> f32,
    lerp: impl Fn(&S, &S, f32) -> T,
) -> Option<T> {
    let first = stops.first()?;
    let last = stops.last()?;
    if t <= pos(first) {
        return Some(lerp(first, first, 0.0));
    }
    if t >= pos(last) {
        return Some(lerp(last, last, 0.0));
    }
    for w in stops.windows(2) {
        let (a, b) = (&w[0], &w[1]);
        let (pa, pb) = (pos(a), pos(b));
        if t >= pa && t <= pb {
            let span = pb - pa;
            let k = if span <= f32::EPSILON {
                0.0
            } else {
                (t - pa) / span
            };
            return Some(lerp(a, b, k));
        }
    }
    Some(lerp(last, last, 0.0))
}

/// How the ramp parameter is derived from position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum GradientShape {
    #[default]
    Linear,
    Radial,
    /// Sweeps around the start point.
    Angle,
    /// Linear, mirrored about the start point.
    Reflected,
    /// Square rings around the start point, rotated to the drag direction.
    Diamond,
}

impl GradientShape {
    /// The ramp parameter at `p` for a drag from `start` to `end`.
    ///
    /// Always finite and always in `0..=1`: a zero-length drag is refused
    /// before this is ever called, so the divisors here cannot be zero.
    pub fn parameter(self, p: Vec2, start: Vec2, end: Vec2) -> f32 {
        let d = end - start;
        let len2 = d.length_squared().max(1e-12);
        let v = p - start;
        match self {
            GradientShape::Linear => (v.dot(d) / len2).clamp(0.0, 1.0),
            GradientShape::Reflected => (v.dot(d) / len2).abs().clamp(0.0, 1.0),
            GradientShape::Radial => (v.length() / len2.sqrt()).clamp(0.0, 1.0),
            GradientShape::Angle => {
                let a = v.y.atan2(v.x) - d.y.atan2(d.x);
                let tau = std::f32::consts::TAU;
                let mut t = a / tau;
                t -= t.floor();
                t.clamp(0.0, 1.0)
            }
            GradientShape::Diamond => {
                let len = len2.sqrt();
                let u = d / len;
                let x = v.dot(u);
                let y = v.dot(Vec2::new(-u.y, u.x));
                ((x.abs() + y.abs()) / len).clamp(0.0, 1.0)
            }
        }
    }
}

/// The classic 4×4 ordered-dither matrix, normalised to `0..1`.
const BAYER4: [[f32; 4]; 4] = [
    [0.0 / 16.0, 8.0 / 16.0, 2.0 / 16.0, 10.0 / 16.0],
    [12.0 / 16.0, 4.0 / 16.0, 14.0 / 16.0, 6.0 / 16.0],
    [3.0 / 16.0, 11.0 / 16.0, 1.0 / 16.0, 9.0 / 16.0],
    [15.0 / 16.0, 7.0 / 16.0, 13.0 / 16.0, 5.0 / 16.0],
];

fn bayer(x: i32, y: i32) -> f32 {
    BAYER4[(y.rem_euclid(4)) as usize][(x.rem_euclid(4)) as usize]
}

/// Everything a gradient gesture needs besides its two endpoints.
#[derive(Debug, Clone, PartialEq)]
pub struct GradientSettings {
    pub shape: GradientShape,
    pub ramp: GradientRamp,
    /// Break up 8-bit banding with an ordered dither.
    pub dither: bool,
    pub reverse: bool,
    /// Overall opacity of the gradient as a whole.
    pub opacity: f32,
}

impl Default for GradientSettings {
    fn default() -> Self {
        Self {
            shape: GradientShape::Linear,
            ramp: GradientRamp::two([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]),
            dither: true,
            reverse: false,
            opacity: 1.0,
        }
    }
}

/// Render a gradient into a patch over `rect`.
///
/// Exposed separately from the tool so a fill layer, a mask ramp or a scripted
/// batch can use the identical pixels.
pub fn render_gradient(
    patch: &mut ColorPatch,
    rect: PixelRect,
    start: Vec2,
    end: Vec2,
    settings: &GradientSettings,
    selection: &Selection,
) {
    let opacity = settings.opacity.clamp(0.0, 1.0);
    for y in rect.y..rect.bottom() {
        for x in rect.x..rect.right() {
            let p = IVec2::new(x as i32, y as i32);
            let clip = selection.coverage_at(p);
            if clip <= 0.0 {
                continue;
            }
            let pt = Vec2::new(x as f32 + 0.5, y as f32 + 0.5);
            let mut t = settings.shape.parameter(pt, start, end);
            if settings.reverse {
                t = 1.0 - t;
            }
            let mut c = settings.ramp.sample(t);
            if settings.dither {
                // Dither in the **output** domain, not in `t`.
                //
                // The banding a gradient shows is a quantisation artefact of
                // the 8-bit encoding, so the noise has to be just under one
                // encoded level — which is a different amount of `t` for every
                // ramp, and none at all for a ramp that spans two colours a
                // hair apart. Perturbing `t` instead would leave exactly the
                // shallow gradients that band worst completely undithered.
                let d = (bayer(p.x, p.y) - 0.5) / 255.0;
                for ch in c.iter_mut().take(3) {
                    let enc = linear_to_srgb(ch.clamp(0.0, 1.0)) + d;
                    *ch = srgb_to_linear_scalar(enc.clamp(0.0, 1.0));
                }
                c[3] = (c[3] + d).clamp(0.0, 1.0);
            }
            let a = c[3] * opacity * clip;
            let src = premultiply([c[0], c[1], c[2], a]);
            let dst = patch.get(p);
            patch.set(
                p,
                [
                    src[0] + dst[0] * (1.0 - a),
                    src[1] + dst[1] * (1.0 - a),
                    src[2] + dst[2] * (1.0 - a),
                    a + dst[3] * (1.0 - a),
                ],
            );
        }
    }
}

/// The gradient tool: drag to set the axis, release to commit.
pub struct GradientTool {
    pub settings: GradientSettings,
    start: Option<Vec2>,
    current: Option<Vec2>,
}

impl GradientTool {
    pub fn new(settings: GradientSettings) -> Self {
        Self {
            settings,
            start: None,
            current: None,
        }
    }

    /// The axis being dragged, for the on-canvas overlay.
    pub fn axis(&self) -> Option<(Vec2, Vec2)> {
        Some((self.start?, self.current?))
    }

    /// The region the gradient fills: the selection's box when there is one,
    /// the canvas otherwise, always clipped to the canvas.
    fn target_rect(&self, ctx: &ToolContext<'_>) -> Option<PixelRect> {
        let canvas = ctx.canvas;
        let r = match ctx.selection.bounds() {
            Some((min, max)) => PixelRect::new(
                min.x as i64,
                min.y as i64,
                (max.x - min.x).max(0) as u32,
                (max.y - min.y).max(0) as u32,
            ),
            None => canvas,
        };
        let x0 = r.x.max(canvas.x);
        let y0 = r.y.max(canvas.y);
        let x1 = r.right().min(canvas.right());
        let y1 = r.bottom().min(canvas.bottom());
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        Some(PixelRect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32))
    }
}

impl Default for GradientTool {
    fn default() -> Self {
        Self::new(GradientSettings::default())
    }
}

impl Tool for GradientTool {
    fn id(&self) -> ToolId {
        ToolId::Gradient
    }

    fn on_pointer_down(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        crate::error::finite_pt("gradient start", event.pos)?;
        self.start = Some(event.pos);
        self.current = Some(event.pos);
        Ok(())
    }

    fn on_pointer_move(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        if let Some(start) = self.start {
            // Shift constrains the axis to 45° increments.
            self.current = Some(if event.modifiers.shift {
                constrain_45(start, event.pos)
            } else {
                event.pos
            });
        }
        Ok(())
    }

    fn on_pointer_up(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        let Some(start) = self.start.take() else {
            return Ok(());
        };
        let end = if event.modifiers.shift {
            constrain_45(start, event.pos)
        } else {
            event.pos
        };
        self.current = None;
        crate::error::finite_pt("gradient end", end)?;
        if (end - start).length() < 1e-3 {
            return Err(ToolError::Degenerate);
        }
        let target = ctx.pixel_target()?;
        let key = ctx.pixel_key()?;
        let Some(rect) = self.target_rect(ctx) else {
            return Ok(());
        };
        let mut patch = ColorPatch::load(ctx.tiles, key, rect)?;
        render_gradient(&mut patch, rect, start, end, &self.settings, &ctx.selection);
        let delta = patch.commit(ctx.tiles, key)?;
        if !delta.is_empty() {
            ctx.emit(Command::PaintTiles { target, delta });
        }
        Ok(())
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.start = None;
        self.current = None;
    }

    fn is_active(&self) -> bool {
        self.start.is_some()
    }
}

/// Snap `to` onto the nearest 45° ray from `from`.
pub(crate) fn constrain_45(from: Vec2, to: Vec2) -> Vec2 {
    let d = to - from;
    let len = d.length();
    if len < 1e-6 {
        return to;
    }
    let step = std::f32::consts::FRAC_PI_4;
    let a = (d.y.atan2(d.x) / step).round() * step;
    from + Vec2::new(a.cos(), a.sin()) * len
}

/// Convert an sRGB-encoded 8-bit colour to the linear RGB a ramp stop holds.
pub fn stop_from_srgb8(rgb: [u8; 3]) -> [f32; 3] {
    srgb_to_linear3([
        rgb[0] as f32 / 255.0,
        rgb[1] as f32 / 255.0,
        rgb[2] as f32 / 255.0,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_linear_ramp_is_monotone_from_end_to_end() {
        let r = GradientRamp::two([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        let mut prev = -1.0;
        for i in 0..=100 {
            let t = i as f32 / 100.0;
            let v = r.sample(t)[0];
            assert!(v >= prev - 1e-6, "ramp fell at t={t}: {v} after {prev}");
            prev = v;
        }
        assert_eq!(r.sample(0.0)[0], 0.0);
        assert_eq!(r.sample(1.0)[0], 1.0);
        assert!((r.sample(0.5)[0] - 0.5).abs() < 1e-6);
        // Outside the domain clamps rather than extrapolating.
        assert_eq!(r.sample(-5.0)[0], 0.0);
        assert_eq!(r.sample(5.0)[0], 1.0);
        assert_eq!(r.sample(f32::NAN)[0], 0.0);
    }

    #[test]
    fn colour_and_opacity_stops_are_independent() {
        let ramp = GradientRamp::new(
            vec![ColorStop {
                position: 0.0,
                color: [1.0, 0.0, 0.0],
            }],
            vec![
                OpacityStop {
                    position: 0.25,
                    opacity: 1.0,
                },
                OpacityStop {
                    position: 0.75,
                    opacity: 0.0,
                },
            ],
        )
        .unwrap();
        // One colour stop: the colour is constant everywhere.
        assert_eq!(ramp.sample(0.0)[0], 1.0);
        assert_eq!(ramp.sample(1.0)[0], 1.0);
        // Opacity still ramps, and holds flat outside its own stops.
        assert_eq!(ramp.sample(0.1)[3], 1.0);
        assert!((ramp.sample(0.5)[3] - 0.5).abs() < 1e-6);
        assert_eq!(ramp.sample(0.9)[3], 0.0);
    }

    #[test]
    fn stops_are_sorted_and_an_empty_ramp_is_refused() {
        let r = GradientRamp::new(
            vec![
                ColorStop {
                    position: 1.0,
                    color: [1.0, 1.0, 1.0],
                },
                ColorStop {
                    position: 0.0,
                    color: [0.0, 0.0, 0.0],
                },
            ],
            vec![OpacityStop {
                position: 0.0,
                opacity: 1.0,
            }],
        )
        .unwrap();
        assert_eq!(r.color_stops()[0].position, 0.0);
        assert!(GradientRamp::new(Vec::new(), Vec::new()).is_err());
        assert!(GradientRamp::new(
            vec![ColorStop {
                position: f32::NAN,
                color: [0.0; 3]
            }],
            vec![OpacityStop {
                position: 0.0,
                opacity: 1.0
            }]
        )
        .is_err());
    }

    #[test]
    fn every_shape_stays_inside_the_unit_interval_and_differs_from_the_others() {
        let start = Vec2::new(10.0, 10.0);
        let end = Vec2::new(110.0, 10.0);
        let probe = Vec2::new(40.0, 60.0);
        let mut seen = Vec::new();
        for shape in [
            GradientShape::Linear,
            GradientShape::Radial,
            GradientShape::Angle,
            GradientShape::Reflected,
            GradientShape::Diamond,
        ] {
            for p in [probe, start, end, Vec2::new(-500.0, 900.0)] {
                let t = shape.parameter(p, start, end);
                assert!((0.0..=1.0).contains(&t), "{shape:?} produced {t}");
            }
            seen.push(shape.parameter(probe, start, end));
        }
        // Linear and reflected agree on the +x side; radial, angle and diamond
        // all disagree with them, which is the point of having five shapes.
        assert!((seen[0] - seen[3]).abs() < 1e-6);
        assert!((seen[0] - seen[1]).abs() > 1e-3);
        assert!((seen[0] - seen[2]).abs() > 1e-3);
        assert!((seen[0] - seen[4]).abs() > 1e-3);
    }

    #[test]
    fn a_reflected_gradient_is_symmetric_about_its_start() {
        let s = Vec2::new(50.0, 0.0);
        let e = Vec2::new(100.0, 0.0);
        let a = GradientShape::Reflected.parameter(Vec2::new(70.0, 0.0), s, e);
        let b = GradientShape::Reflected.parameter(Vec2::new(30.0, 0.0), s, e);
        assert!((a - b).abs() < 1e-6, "{a} vs {b}");
    }

    #[test]
    fn a_ramp_with_no_stops_cannot_be_deserialized() {
        // Presets and project files carry ramps, so this JSON is reachable from
        // any hand-edited or truncated file. It must be an error, not a panic
        // the first time someone drags the gradient tool.
        let err = serde_json::from_str::<GradientRamp>(r#"{"colors":[],"opacities":[]}"#)
            .expect_err("an empty ramp must not deserialize");
        assert!(
            err.to_string().contains("no area"),
            "the refusal should be the constructor's, got: {err}"
        );
        // One empty list is just as fatal as two: an empty opacity list would
        // index just as far out of bounds as an empty colour list.
        assert!(serde_json::from_str::<GradientRamp>(
            r#"{"colors":[{"position":0.0,"color":[1.0,0.0,0.0]}],"opacities":[]}"#
        )
        .is_err());
        assert!(serde_json::from_str::<GradientRamp>(
            r#"{"colors":[],"opacities":[{"position":0.0,"opacity":1.0}]}"#
        )
        .is_err());
    }

    #[test]
    fn a_ramp_survives_a_serde_round_trip_and_still_samples() {
        let ramp = GradientRamp::new(
            vec![
                ColorStop {
                    position: 1.0,
                    color: [0.0, 0.0, 1.0],
                },
                ColorStop {
                    position: 0.0,
                    color: [1.0, 0.0, 0.0],
                },
            ],
            vec![OpacityStop {
                position: 0.0,
                opacity: 0.25,
            }],
        )
        .unwrap();
        let json = serde_json::to_string(&ramp).unwrap();
        let back: GradientRamp = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ramp);
        assert_eq!(back.sample(0.0), [1.0, 0.0, 0.0, 0.25]);
        assert_eq!(back.sample(1.0), [0.0, 0.0, 1.0, 0.25]);
    }

    #[test]
    fn constrain_45_snaps_to_the_eight_rays() {
        let o = Vec2::ZERO;
        let p = constrain_45(o, Vec2::new(100.0, 10.0));
        assert!(p.y.abs() < 1e-3, "should have snapped to horizontal: {p:?}");
        let q = constrain_45(o, Vec2::new(100.0, 90.0));
        assert!(
            (q.x - q.y).abs() < 1e-2,
            "should have snapped to 45°: {q:?}"
        );
    }
}
