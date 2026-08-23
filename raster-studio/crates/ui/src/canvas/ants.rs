//! The marching-ants selection outline.
//!
//! The boundary itself comes from [`selection::outline`], which walks the
//! coverage mask and hands back closed loops in document pixel-corner
//! coordinates. This module does the two things that are the *view's* job:
//! project those loops into screen points, and cut them into the alternating
//! dashes that crawl.
//!
//! Two colours are drawn, not one. A single-colour outline disappears wherever
//! the image underneath happens to match it, so the light run and the dark run
//! interleave along the same path — which is why [`AntsGeometry`] carries the
//! whole outline *and* the dashes that sit on top of it.
//!
//! The animation is a pure function of time: [`ants_phase`] turns a clock
//! reading into an offset along the path, so nothing has to accumulate state
//! between frames and a dropped frame cannot make the ants stutter.

use design::Space;
use glam::Vec2;
use selection::Polyline;

use super::camera::CanvasCamera;
use super::geom::DocRect;
use super::viewport::Viewport;

/// How the ants look and how fast they crawl.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AntsStyle {
    /// Length of one dash — and of one gap — in screen points.
    pub dash_pt: f32,
    /// How far the pattern travels per second, in screen points.
    pub speed_pt_per_sec: f32,
}

impl Default for AntsStyle {
    fn default() -> Self {
        Self {
            // One grid unit of dash, one of gap.
            dash_pt: Space::XSmall.pt(),
            // Three units a second: fast enough to read as motion, slow enough
            // not to draw the eye away from the image. A rate, not a spacing —
            // it is expressed on the grid only so the two stay in proportion.
            speed_pt_per_sec: Space::Medium.pt(),
        }
    }
}

impl AntsStyle {
    /// Smallest and largest dash accepted, in screen points: a quarter of a
    /// grid unit up to sixteen of them. Both come off the spacing scale, so a
    /// preferences file cannot ask for a dash that is invisible or for one
    /// longer than most selections.
    pub const MIN_DASH_PT: f32 = design::UNIT_PT * 0.25;
    pub const MAX_DASH_PT: f32 = design::UNIT_PT * 16.0;

    /// The dash length, clamped to something drawable.
    pub fn dash(&self) -> f32 {
        if self.dash_pt.is_finite() {
            self.dash_pt.clamp(Self::MIN_DASH_PT, Self::MAX_DASH_PT)
        } else {
            Self::default().dash_pt
        }
    }

    /// The full period of the pattern: one dash plus one gap.
    pub fn period(&self) -> f32 {
        self.dash() * 2.0
    }
}

/// The offset along the path at a given moment, wrapped into one period.
///
/// Pure, so the ants are identical on every machine at the same clock reading
/// and a skipped frame catches up rather than falling behind.
pub fn ants_phase(time_secs: f64, style: &AntsStyle) -> f32 {
    let period = style.period();
    if !time_secs.is_finite() {
        return 0.0;
    }
    let travelled = time_secs * f64::from(style.speed_pt_per_sec);
    let wrapped = travelled.rem_euclid(f64::from(period));
    wrapped as f32
}

/// The outline, ready to stroke.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AntsGeometry {
    /// Every loop of the boundary, in screen points. Stroked in the base
    /// colour, unbroken.
    pub outlines: Vec<Vec<Vec2>>,
    /// The dashes stroked over the base in the contrasting colour.
    pub dashes: Vec<[Vec2; 2]>,
}

impl AntsGeometry {
    pub fn is_empty(&self) -> bool {
        self.outlines.is_empty()
    }

    /// Total length of the dashed runs, in screen points. Used by the tests to
    /// pin the duty cycle.
    pub fn dashed_length(&self) -> f32 {
        self.dashes.iter().map(|[a, b]| (*b - *a).length()).sum()
    }
}

/// The most screen-space points one frame of ants may carry. A selection
/// traced at a huge zoom can otherwise produce more segments than there are
/// pixels on the display.
pub const MAX_SEGMENTS: usize = 20_000;

/// Project the outline and cut it into dashes.
///
/// Loops entirely outside the viewport are dropped, which is what keeps a
/// zoomed-in view of a complicated selection cheap.
pub fn build(
    loops: &[Polyline],
    camera: &CanvasCamera,
    viewport: &Viewport,
    style: &AntsStyle,
    phase: f32,
) -> AntsGeometry {
    let mut out = AntsGeometry::default();
    if viewport.is_degenerate() {
        return out;
    }
    // A dash of room around the viewport, so a loop whose corner is just off
    // screen still contributes the piece that is on screen.
    let clip = viewport.content_bounds_pt().expanded(style.period() * 2.0);
    let mut budget = MAX_SEGMENTS;

    for poly in loops {
        if poly.points.len() < 2 {
            continue;
        }
        let mut screen: Vec<Vec2> = poly
            .points
            .iter()
            .map(|p| camera.screen_pt_of(viewport, Vec2::new(p.x as f32, p.y as f32)))
            .collect();
        if poly.closed {
            // Repeat the first point so the walk closes the loop.
            if let Some(first) = screen.first().copied() {
                screen.push(first);
            }
        }
        if !screen.iter().all(|p| p.is_finite()) {
            continue;
        }
        let Some(bounds) = DocRect::of_points(&screen) else {
            continue;
        };
        // A degenerate box — a horizontal edge, or a loop collapsed to a point
        // — still has to survive the cull, hence the expansion.
        if bounds.expanded(1.0).intersect(&clip).is_empty() {
            continue;
        }
        if screen.len() > budget {
            break;
        }
        budget -= screen.len();
        dash_walk(&screen, style, phase, clip, &mut out.dashes);
        out.outlines.push(screen);
    }
    out
}

/// The parametric span of the segment `a -> b` that lies inside `rect`, or
/// `None` when it misses entirely. A standard slab clip.
fn clip_span(a: Vec2, b: Vec2, rect: DocRect) -> Option<(f32, f32)> {
    let d = b - a;
    let mut t0 = 0.0_f32;
    let mut t1 = 1.0_f32;
    for axis in 0..2 {
        let (p, dir, lo, hi) = if axis == 0 {
            (a.x, d.x, rect.min.x, rect.max.x)
        } else {
            (a.y, d.y, rect.min.y, rect.max.y)
        };
        if dir.abs() < 1e-9 {
            if p < lo || p > hi {
                return None;
            }
            continue;
        }
        let (mut near, mut far) = ((lo - p) / dir, (hi - p) / dir);
        if near > far {
            std::mem::swap(&mut near, &mut far);
        }
        t0 = t0.max(near);
        t1 = t1.min(far);
        if t0 > t1 {
            return None;
        }
    }
    Some((t0.clamp(0.0, 1.0), t1.clamp(0.0, 1.0)))
}

/// Cut a polyline into the "on" halves of the dash pattern.
///
/// The off-screen part of a segment is skipped by advancing the phase over it
/// rather than by stepping through it, so a selection whose loop runs a hundred
/// thousand points past the edge of the window costs the same as one that does
/// not — while the dashes that *are* on screen stay in the same places.
fn dash_walk(
    points: &[Vec2],
    style: &AntsStyle,
    phase: f32,
    clip: DocRect,
    out: &mut Vec<[Vec2; 2]>,
) {
    let dash = style.dash();
    let period = style.period();
    let mut travelled = if phase.is_finite() {
        phase.rem_euclid(period)
    } else {
        0.0
    };

    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let seg = b - a;
        let len = seg.length();
        if !len.is_finite() || len <= 0.0 {
            continue;
        }
        let dir = seg / len;
        let Some((t0, t1)) = clip_span(a, b, clip) else {
            travelled = (travelled + len).rem_euclid(period);
            continue;
        };
        let (start, end) = (t0 * len, t1 * len);
        travelled = (travelled + start).rem_euclid(period);
        let mut walked = start;
        while walked < end && out.len() < MAX_SEGMENTS {
            // Where in the period we are, and how much of this state is left.
            let within = travelled.rem_euclid(period);
            let on = within < dash;
            let remaining_state = if on { dash - within } else { period - within };
            let step = remaining_state.min(end - walked).max(f32::MIN_POSITIVE);
            if on {
                out.push([a + dir * walked, a + dir * (walked + step)]);
            }
            walked += step;
            travelled = (travelled + step).rem_euclid(period);
        }
        travelled = (travelled + (len - end).max(0.0)).rem_euclid(period);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::viewport::PanelInsets;
    use glam::IVec2;

    fn vp() -> Viewport {
        Viewport::new(
            Vec2::new(1000.0, 800.0),
            PanelInsets::new(200.0, 100.0, 40.0, 20.0),
            2.0,
        )
    }

    fn cam(zoom: f32) -> CanvasCamera {
        CanvasCamera {
            center: Vec2::new(50.0, 50.0),
            zoom,
            ..CanvasCamera::default()
        }
    }

    fn square(size: i32) -> Polyline {
        Polyline {
            points: vec![
                IVec2::new(0, 0),
                IVec2::new(size, 0),
                IVec2::new(size, size),
                IVec2::new(0, size),
            ],
            closed: true,
        }
    }

    #[test]
    fn the_outline_follows_the_camera() {
        let v = vp();
        let c = cam(2.0);
        let g = build(&[square(40)], &c, &v, &AntsStyle::default(), 0.0);
        assert_eq!(g.outlines.len(), 1);
        let ring = &g.outlines[0];
        // Closed loops repeat their first point so the last edge is drawn.
        assert_eq!(ring.len(), 5);
        assert_eq!(ring[0], ring[4]);
        assert_eq!(ring[0], c.screen_pt_of(&v, Vec2::ZERO));
        assert_eq!(ring[2], c.screen_pt_of(&v, Vec2::new(40.0, 40.0)));
    }

    #[test]
    fn an_open_polyline_is_not_closed_behind_the_users_back() {
        let v = vp();
        let open = Polyline {
            points: vec![IVec2::new(0, 0), IVec2::new(10, 0), IVec2::new(10, 10)],
            closed: false,
        };
        let g = build(&[open], &cam(2.0), &v, &AntsStyle::default(), 0.0);
        assert_eq!(g.outlines[0].len(), 3);
    }

    #[test]
    fn the_dashes_cover_about_half_the_perimeter() {
        let v = vp();
        let c = cam(2.0);
        let style = AntsStyle::default();
        let g = build(&[square(100)], &c, &v, &style, 0.0);
        // 100 document pixels at 1 point each (zoom 2, scale 2) -> 400pt loop.
        let perimeter = 4.0 * 100.0 * c.scale_pt(&v);
        let ratio = g.dashed_length() / perimeter;
        assert!(
            (ratio - 0.5).abs() < 0.05,
            "duty cycle is {ratio}, not about half"
        );
    }

    #[test]
    fn dash_lengths_never_exceed_one_dash() {
        let v = vp();
        let style = AntsStyle {
            dash_pt: 5.0,
            ..AntsStyle::default()
        };
        let g = build(&[square(60)], &cam(2.0), &v, &style, 1.7);
        assert!(!g.dashes.is_empty());
        for [a, b] in &g.dashes {
            let len = (*b - *a).length();
            assert!(len > 0.0 && len <= style.dash() + 1e-3, "{len}");
        }
    }

    /// The animation: advancing time moves the pattern along the path, and the
    /// phase wraps rather than growing without bound.
    #[test]
    fn the_phase_advances_with_time_and_wraps() {
        let style = AntsStyle {
            dash_pt: 4.0,
            speed_pt_per_sec: 8.0,
        };
        assert_eq!(style.period(), 8.0);
        assert_eq!(ants_phase(0.0, &style), 0.0);
        assert!((ants_phase(0.25, &style) - 2.0).abs() < 1e-4);
        // One full period per second at this speed.
        assert!(ants_phase(1.0, &style).abs() < 1e-3);
        assert!((ants_phase(1.25, &style) - 2.0).abs() < 1e-3);
        for t in [0.0, 0.3, 7.7, 1234.5, -3.2] {
            let p = ants_phase(t, &style);
            assert!((0.0..style.period()).contains(&p), "t={t} gave {p}");
        }
        assert_eq!(ants_phase(f64::NAN, &style), 0.0);
    }

    #[test]
    fn a_different_phase_puts_the_dashes_somewhere_else() {
        let v = vp();
        let c = cam(2.0);
        let style = AntsStyle::default();
        let a = build(&[square(60)], &c, &v, &style, 0.0);
        let b = build(&[square(60)], &c, &v, &style, style.dash());
        assert_ne!(a.dashes, b.dashes, "the ants did not move");
        // …but the outline underneath is identical.
        assert_eq!(a.outlines, b.outlines);
        // A whole period later they are back where they started.
        let c2 = build(&[square(60)], &c, &v, &style, style.period());
        assert_eq!(a.dashes.len(), c2.dashes.len());
        for (p, q) in a.dashes.iter().zip(&c2.dashes) {
            assert!((p[0] - q[0]).length() < 1e-2 && (p[1] - q[1]).length() < 1e-2);
        }
    }

    #[test]
    fn dash_size_is_measured_in_screen_points_so_zoom_does_not_change_it() {
        let v = vp();
        let style = AntsStyle::default();
        for zoom in [0.5_f32, 2.0, 16.0] {
            let g = build(&[square(40)], &cam(zoom), &v, &style, 0.0);
            for [a, b] in &g.dashes {
                assert!((*b - *a).length() <= style.dash() + 1e-3, "zoom {zoom}");
            }
        }
    }

    #[test]
    fn a_selection_far_off_screen_costs_nothing() {
        let v = vp();
        let far = Polyline {
            points: vec![
                IVec2::new(90_000, 90_000),
                IVec2::new(90_100, 90_000),
                IVec2::new(90_100, 90_100),
                IVec2::new(90_000, 90_100),
            ],
            closed: true,
        };
        let g = build(&[far], &cam(1.0), &v, &AntsStyle::default(), 0.0);
        assert!(g.is_empty());
        assert!(g.dashes.is_empty());
    }

    #[test]
    fn a_partly_visible_selection_is_still_drawn() {
        let v = vp();
        let c = cam(2.0);
        let straddling = Polyline {
            points: vec![
                IVec2::new(-5_000, 0),
                IVec2::new(60, 0),
                IVec2::new(60, 60),
                IVec2::new(-5_000, 60),
            ],
            closed: true,
        };
        let g = build(&[straddling], &c, &v, &AntsStyle::default(), 0.0);
        assert!(!g.is_empty());
    }

    #[test]
    fn degenerate_input_produces_nothing_rather_than_a_hang() {
        let v = vp();
        let style = AntsStyle::default();
        let single = Polyline {
            points: vec![IVec2::new(3, 3)],
            closed: true,
        };
        assert!(build(&[single], &cam(1.0), &v, &style, 0.0).is_empty());

        // A loop whose points all coincide has zero length: the walk must not
        // spin on a zero-length edge.
        let degenerate = Polyline {
            points: vec![IVec2::new(3, 3), IVec2::new(3, 3), IVec2::new(3, 3)],
            closed: true,
        };
        let g = build(&[degenerate], &cam(1.0), &v, &style, 0.0);
        assert!(g.dashes.is_empty());

        let collapsed = Viewport::new(Vec2::splat(50.0), PanelInsets::uniform(50.0), 1.0);
        assert!(build(&[square(10)], &cam(1.0), &collapsed, &style, 0.0).is_empty());
    }

    #[test]
    fn a_pathological_selection_is_capped() {
        let v = vp();
        // Many small loops, all on screen.
        let loops: Vec<Polyline> = (0..20_000).map(|_| square(30)).collect();
        let g = build(&loops, &cam(2.0), &v, &AntsStyle::default(), 0.0);
        let points: usize = g.outlines.iter().map(|o| o.len()).sum();
        assert!(points <= MAX_SEGMENTS, "{points} points slipped through");
        assert!(!g.outlines.is_empty());
    }

    #[test]
    fn a_hostile_style_is_clamped() {
        let bad = AntsStyle {
            dash_pt: f32::NAN,
            speed_pt_per_sec: 1.0,
        };
        assert_eq!(bad.dash(), AntsStyle::default().dash_pt);
        let tiny = AntsStyle {
            dash_pt: 0.0,
            speed_pt_per_sec: 1.0,
        };
        assert_eq!(tiny.dash(), 1.0);
        let huge = AntsStyle {
            dash_pt: 1e6,
            speed_pt_per_sec: 1.0,
        };
        assert_eq!(huge.dash(), 64.0);
    }
}
