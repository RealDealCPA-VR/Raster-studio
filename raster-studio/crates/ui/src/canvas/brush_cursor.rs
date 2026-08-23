//! The brush cursor: an outline at the brush's true size and shape.
//!
//! The contract is one sentence: **the ring is the dab.** Its diameter on
//! screen is the brush diameter in document pixels times the current scale, at
//! every zoom, so what the user sees is exactly what the next click will paint.
//! A fixed-size cursor would be a lie the moment the zoom moved.
//!
//! Shape follows the brush too. A brush with `roundness < 1` is an ellipse, and
//! its `angle` turns it in *document* space — so the ring has to be rotated by
//! the brush angle **and** by the view rotation, and mirrored when the view is
//! flipped. Doing that by projecting the ellipse's own points through the
//! camera, rather than by drawing a circle and hoping, is what keeps it honest
//! under a rotated or flipped view.

use design::{Space, UNIT_PT};
use glam::Vec2;
use tools::BrushSettings;

use super::camera::CanvasCamera;
use super::viewport::Viewport;

/// How many points the outline is drawn with. Enough that a large ring has no
/// visible facets, few enough that it costs nothing.
pub const OUTLINE_SEGMENTS: usize = 64;

/// Below this diameter in screen points the ring is too small to aim with, so a
/// crosshair is drawn as well — the behaviour every editor has for tiny brushes.
pub const CROSSHAIR_BELOW_PT: f32 = Space::Small.units() * UNIT_PT;

/// Above this diameter in screen points the ring is not drawn at all: a ring
/// larger than the window is a full-screen circle that hides the image and
/// tells the user nothing.
///
/// A thousand grid units — a ceiling rather than a size, expressed on the grid
/// like every other measurement here so nothing in this module carries a bare
/// number.
pub const MAX_RING_PT: f32 = UNIT_PT * 1024.0;

/// The cursor for one frame, in screen points.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BrushCursor {
    /// The ring, as a closed loop of screen points. Empty when the ring is not
    /// drawn.
    pub outline: Vec<Vec2>,
    /// A small crosshair at the centre, drawn when the ring is too small to aim
    /// with or too large to be useful.
    pub crosshair: Option<[[Vec2; 2]; 2]>,
    /// Where the centre is, in screen points.
    pub center_pt: Vec2,
}

impl BrushCursor {
    pub fn is_empty(&self) -> bool {
        self.outline.is_empty() && self.crosshair.is_none()
    }

    /// The widest extent of the outline, in screen points — the diameter along
    /// the brush's major axis once the view has had its way with it.
    pub fn extent_pt(&self) -> f32 {
        let mut widest = 0.0_f32;
        for (i, a) in self.outline.iter().enumerate() {
            for b in &self.outline[i + 1..] {
                widest = widest.max((*b - *a).length());
            }
        }
        widest
    }
}

/// The brush diameter, in document pixels, that a dab will actually have.
///
/// Pressure scales the dab when the brush is set to let it, between
/// `min_size_ratio` and the full size — so the cursor has to take the current
/// pressure into account or it stops matching the dab mid-stroke.
pub fn effective_diameter(brush: &BrushSettings, pressure: f32) -> f32 {
    let size = if brush.size.is_finite() {
        brush.size.max(0.0)
    } else {
        0.0
    };
    if !brush.size_pressure {
        return size;
    }
    let p = if pressure.is_finite() {
        pressure.clamp(0.0, 1.0)
    } else {
        1.0
    };
    let min_ratio = if brush.min_size_ratio.is_finite() {
        brush.min_size_ratio.clamp(0.0, 1.0)
    } else {
        0.0
    };
    size * (min_ratio + (1.0 - min_ratio) * p)
}

/// The crosshair arms, in screen points.
fn crosshair_at(center: Vec2, arm_pt: f32) -> [[Vec2; 2]; 2] {
    [
        [
            center - Vec2::new(arm_pt, 0.0),
            center + Vec2::new(arm_pt, 0.0),
        ],
        [
            center - Vec2::new(0.0, arm_pt),
            center + Vec2::new(0.0, arm_pt),
        ],
    ]
}

/// Build the brush cursor for a pointer at `center_doc`.
///
/// The outline is produced by walking the dab's ellipse in **document** space
/// and projecting each point, so view rotation, view flip and the brush's own
/// angle all compose without a special case.
pub fn build(
    brush: &BrushSettings,
    pressure: f32,
    center_doc: Vec2,
    camera: &CanvasCamera,
    viewport: &Viewport,
) -> BrushCursor {
    let center_pt = camera.screen_pt_of(viewport, center_doc);
    let mut out = BrushCursor {
        center_pt,
        ..BrushCursor::default()
    };
    if viewport.is_degenerate() || !center_pt.is_finite() {
        return BrushCursor::default();
    }

    let diameter_doc = effective_diameter(brush, pressure);
    let scale = camera.scale_pt(viewport);
    if !scale.is_finite() || scale <= 0.0 {
        return out;
    }
    let diameter_pt = diameter_doc * scale;

    if diameter_pt <= 0.0 || diameter_pt > MAX_RING_PT {
        out.crosshair = Some(crosshair_at(center_pt, CROSSHAIR_BELOW_PT * 0.5));
        return out;
    }

    let rx = diameter_doc * 0.5;
    let roundness = if brush.roundness.is_finite() {
        brush.roundness.clamp(0.0, 1.0)
    } else {
        1.0
    };
    let ry = rx * roundness;
    let angle = if brush.angle.is_finite() {
        brush.angle
    } else {
        0.0
    };
    let (sin_a, cos_a) = angle.sin_cos();

    out.outline.reserve(OUTLINE_SEGMENTS);
    for i in 0..OUTLINE_SEGMENTS {
        let t = i as f32 / OUTLINE_SEGMENTS as f32 * std::f32::consts::TAU;
        let (s, c) = t.sin_cos();
        // The ellipse in the brush's own frame, turned by the brush angle.
        let local = Vec2::new(rx * c, ry * s);
        let turned = Vec2::new(
            local.x * cos_a - local.y * sin_a,
            local.x * sin_a + local.y * cos_a,
        );
        let p = camera.screen_pt_of(viewport, center_doc + turned);
        if !p.is_finite() {
            return BrushCursor::default();
        }
        out.outline.push(p);
    }

    if diameter_pt < CROSSHAIR_BELOW_PT {
        out.crosshair = Some(crosshair_at(center_pt, CROSSHAIR_BELOW_PT * 0.5));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::viewport::PanelInsets;
    use std::f32::consts::{FRAC_PI_2, FRAC_PI_4};

    fn vp(scale: f32) -> Viewport {
        Viewport::new(
            Vec2::new(1000.0, 800.0),
            PanelInsets::new(200.0, 100.0, 40.0, 20.0),
            scale,
        )
    }

    fn brush(size: f32) -> BrushSettings {
        BrushSettings {
            size,
            size_pressure: false,
            ..BrushSettings::default()
        }
    }

    /// The headline invariant: the ring is the dab, at every zoom and every
    /// display scale.
    #[test]
    fn the_outline_matches_the_brush_size_at_every_zoom() {
        for display_scale in [1.0_f32, 1.5, 2.0] {
            let v = vp(display_scale);
            for size in [1.0_f32, 8.0, 24.0, 300.0] {
                let b = brush(size);
                for zoom in [1.0 / 8.0, 0.5, 1.0, 4.0, 32.0] {
                    let cam = CanvasCamera {
                        center: Vec2::new(100.0, 100.0),
                        zoom,
                        ..CanvasCamera::default()
                    };
                    let want = size * cam.scale_pt(&v);
                    if want <= 0.0 || want > MAX_RING_PT {
                        continue;
                    }
                    let c = build(&b, 1.0, Vec2::new(100.0, 100.0), &cam, &v);
                    let got = c.extent_pt();
                    assert!(
                        (got - want).abs() <= want * 0.01,
                        "scale {display_scale}, size {size}, zoom {zoom}: \
                         ring is {got}pt, brush is {want}pt"
                    );
                }
            }
        }
    }

    #[test]
    fn the_ring_is_centred_on_the_pointer() {
        let v = vp(2.0);
        let cam = CanvasCamera {
            center: Vec2::new(100.0, 100.0),
            zoom: 4.0,
            ..CanvasCamera::default()
        };
        let at = Vec2::new(112.0, 93.0);
        let c = build(&brush(20.0), 1.0, at, &cam, &v);
        assert_eq!(c.center_pt, cam.screen_pt_of(&v, at));
        let mean: Vec2 = c.outline.iter().copied().sum::<Vec2>() / c.outline.len() as f32;
        assert!((mean - c.center_pt).length() < 0.05, "{mean:?}");
    }

    #[test]
    fn an_elliptical_brush_is_drawn_as_an_ellipse() {
        let v = vp(1.0);
        let cam = CanvasCamera {
            center: Vec2::ZERO,
            zoom: 1.0,
            ..CanvasCamera::default()
        };
        let b = BrushSettings {
            size: 100.0,
            roundness: 0.25,
            angle: 0.0,
            size_pressure: false,
            ..BrushSettings::default()
        };
        let c = build(&b, 1.0, Vec2::ZERO, &cam, &v);
        let xs: Vec<f32> = c.outline.iter().map(|p| p.x - c.center_pt.x).collect();
        let ys: Vec<f32> = c.outline.iter().map(|p| p.y - c.center_pt.y).collect();
        let width = xs.iter().fold(0.0_f32, |m, v| m.max(v.abs())) * 2.0;
        let height = ys.iter().fold(0.0_f32, |m, v| m.max(v.abs())) * 2.0;
        assert!((width - 100.0).abs() < 1.0, "{width}");
        assert!((height - 25.0).abs() < 1.0, "{height}");
    }

    #[test]
    fn the_brush_angle_turns_the_ellipse() {
        let v = vp(1.0);
        let cam = CanvasCamera {
            center: Vec2::ZERO,
            zoom: 1.0,
            ..CanvasCamera::default()
        };
        let b = BrushSettings {
            size: 100.0,
            roundness: 0.25,
            angle: FRAC_PI_2,
            size_pressure: false,
            ..BrushSettings::default()
        };
        let c = build(&b, 1.0, Vec2::ZERO, &cam, &v);
        let width = c
            .outline
            .iter()
            .fold(0.0_f32, |m, p| m.max((p.x - c.center_pt.x).abs()))
            * 2.0;
        // A quarter turn swaps the axes: the ellipse is now tall, not wide.
        assert!((width - 25.0).abs() < 1.0, "{width}");
    }

    /// A rotated view has to rotate the cursor, or an elliptical brush would
    /// paint at an angle the cursor does not show.
    #[test]
    fn the_ring_turns_with_the_view() {
        let v = vp(1.0);
        let b = BrushSettings {
            size: 100.0,
            roundness: 0.2,
            angle: 0.0,
            size_pressure: false,
            ..BrushSettings::default()
        };
        let upright = CanvasCamera {
            center: Vec2::ZERO,
            zoom: 1.0,
            ..CanvasCamera::default()
        };
        let turned = CanvasCamera {
            rotation: FRAC_PI_2,
            ..upright
        };
        let a = build(&b, 1.0, Vec2::ZERO, &upright, &v);
        let t = build(&b, 1.0, Vec2::ZERO, &turned, &v);
        let width = |c: &BrushCursor| {
            c.outline
                .iter()
                .fold(0.0_f32, |m, p| m.max((p.x - c.center_pt.x).abs()))
                * 2.0
        };
        assert!((width(&a) - 100.0).abs() < 1.0);
        assert!((width(&t) - 20.0).abs() < 1.0, "{}", width(&t));
        // The overall size is unchanged: only its orientation moved.
        assert!((a.extent_pt() - t.extent_pt()).abs() < 1.0);
    }

    #[test]
    fn a_flipped_view_mirrors_the_ring_without_resizing_it() {
        let v = vp(1.0);
        let b = BrushSettings {
            size: 80.0,
            roundness: 0.3,
            angle: FRAC_PI_4,
            size_pressure: false,
            ..BrushSettings::default()
        };
        let plain = CanvasCamera {
            center: Vec2::ZERO,
            zoom: 1.0,
            ..CanvasCamera::default()
        };
        let flipped = CanvasCamera {
            flip_x: true,
            ..plain
        };
        let a = build(&b, 1.0, Vec2::ZERO, &plain, &v);
        let f = build(&b, 1.0, Vec2::ZERO, &flipped, &v);
        assert!((a.extent_pt() - f.extent_pt()).abs() < 0.5);
        // Mirroring every point of one gives the other's point set.
        let mirrored: Vec<Vec2> = a
            .outline
            .iter()
            .map(|p| Vec2::new(2.0 * a.center_pt.x - p.x, p.y))
            .collect();
        for p in &mirrored {
            assert!(
                f.outline.iter().any(|q| (*q - *p).length() < 0.1),
                "{p:?} has no mirror image"
            );
        }
    }

    #[test]
    fn pressure_shrinks_the_ring_exactly_as_it_shrinks_the_dab() {
        let b = BrushSettings {
            size: 40.0,
            size_pressure: true,
            min_size_ratio: 0.25,
            ..BrushSettings::default()
        };
        assert_eq!(effective_diameter(&b, 1.0), 40.0);
        assert_eq!(effective_diameter(&b, 0.0), 10.0);
        assert_eq!(effective_diameter(&b, 0.5), 25.0);

        let v = vp(1.0);
        let cam = CanvasCamera {
            center: Vec2::ZERO,
            zoom: 1.0,
            ..CanvasCamera::default()
        };
        let full = build(&b, 1.0, Vec2::ZERO, &cam, &v);
        let light = build(&b, 0.5, Vec2::ZERO, &cam, &v);
        assert!((full.extent_pt() - 40.0).abs() < 0.5);
        assert!((light.extent_pt() - 25.0).abs() < 0.5);
    }

    #[test]
    fn a_brush_that_ignores_pressure_keeps_its_size() {
        let b = brush(30.0);
        for p in [0.0_f32, 0.5, 1.0] {
            assert_eq!(effective_diameter(&b, p), 30.0);
        }
    }

    #[test]
    fn a_tiny_ring_gets_a_crosshair_to_aim_with() {
        let v = vp(1.0);
        let cam = CanvasCamera {
            center: Vec2::ZERO,
            zoom: 1.0,
            ..CanvasCamera::default()
        };
        let small = build(&brush(3.0), 1.0, Vec2::ZERO, &cam, &v);
        assert!(!small.outline.is_empty());
        assert!(small.crosshair.is_some(), "a 3pt ring is unaimable alone");

        let big = build(&brush(40.0), 1.0, Vec2::ZERO, &cam, &v);
        assert!(big.crosshair.is_none());
    }

    #[test]
    fn an_absurd_ring_is_replaced_by_a_crosshair_rather_than_covering_the_image() {
        let v = vp(1.0);
        let cam = CanvasCamera {
            center: Vec2::ZERO,
            zoom: 256.0,
            ..CanvasCamera::default()
        };
        let c = build(&brush(500.0), 1.0, Vec2::ZERO, &cam, &v);
        assert!(c.outline.is_empty());
        assert!(c.crosshair.is_some());
    }

    #[test]
    fn a_zero_sized_brush_still_shows_where_the_pointer_is() {
        let v = vp(1.0);
        let cam = CanvasCamera::default();
        let c = build(&brush(0.0), 1.0, Vec2::ZERO, &cam, &v);
        assert!(c.outline.is_empty());
        assert!(c.crosshair.is_some());
        assert!(!c.is_empty());
    }

    #[test]
    fn hostile_brush_settings_cannot_produce_a_nan_ring() {
        let v = vp(1.0);
        let cam = CanvasCamera::default();
        let b = BrushSettings {
            size: f32::NAN,
            roundness: f32::NAN,
            angle: f32::INFINITY,
            min_size_ratio: f32::NAN,
            size_pressure: true,
            ..BrushSettings::default()
        };
        let c = build(&b, f32::NAN, Vec2::ZERO, &cam, &v);
        assert!(c.outline.iter().all(|p| p.is_finite()));
        assert!(effective_diameter(&b, f32::NAN).is_finite());
    }

    #[test]
    fn a_collapsed_viewport_or_dead_camera_draws_nothing() {
        let collapsed = Viewport::new(Vec2::splat(50.0), PanelInsets::uniform(50.0), 1.0);
        let cam = CanvasCamera::default();
        assert!(build(&brush(20.0), 1.0, Vec2::ZERO, &cam, &collapsed).is_empty());

        let v = vp(1.0);
        let dead = CanvasCamera {
            zoom: 0.0,
            ..CanvasCamera::default()
        };
        assert!(build(&brush(20.0), 1.0, Vec2::ZERO, &dead, &v)
            .outline
            .is_empty());
    }
}
