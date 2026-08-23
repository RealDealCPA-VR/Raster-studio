//! The configurable document grid, and the pixel grid that appears at high
//! zoom.
//!
//! Both are generated as document coordinates, culled to what is actually
//! visible, and capped: a grid whose spacing has fallen below a pixel would
//! otherwise ask the painter for a hundred thousand hairlines and turn the
//! canvas into a solid block of ink. [`GridLines::suppressed`] records that a
//! grid was asked for but is too dense to be legible, so the UI can say so
//! instead of appearing to ignore the setting.

use glam::Vec2;

use super::camera::CanvasCamera;
use super::geom::{Axis, DocRect};
use super::viewport::Viewport;

/// The user's grid preferences.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridSettings {
    pub visible: bool,
    /// Distance between major grid lines, in document pixels.
    pub spacing_doc: f32,
    /// How many parts each major division is split into. `1` means no
    /// subdivisions.
    pub subdivisions: u32,
    /// Show single-pixel boundaries once the zoom makes them legible.
    pub pixel_grid: bool,
}

impl Default for GridSettings {
    fn default() -> Self {
        Self {
            visible: false,
            spacing_doc: 64.0,
            subdivisions: 4,
            pixel_grid: true,
        }
    }
}

impl GridSettings {
    /// Smallest and largest major spacing accepted, in document pixels.
    pub const MIN_SPACING: f32 = 0.01;
    pub const MAX_SPACING: f32 = 100_000.0;
    /// Most subdivisions a major division may carry.
    pub const MAX_SUBDIVISIONS: u32 = 100;

    /// The spacing, clamped into the legal range — the field is public, so a
    /// zero or a `NaN` from a preferences file has to be survivable.
    pub fn major_spacing(&self) -> f32 {
        if self.spacing_doc.is_finite() {
            self.spacing_doc.clamp(Self::MIN_SPACING, Self::MAX_SPACING)
        } else {
            Self::default().spacing_doc
        }
    }

    /// The minor spacing, or `None` when subdivisions are off.
    pub fn minor_spacing(&self) -> Option<f32> {
        let n = self.subdivisions.clamp(1, Self::MAX_SUBDIVISIONS);
        (n > 1).then(|| self.major_spacing() / n as f32)
    }
}

/// The lines one axis of a grid needs this frame.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GridLines {
    /// Document coordinates of the major lines on this axis.
    pub major: Vec<f32>,
    /// Document coordinates of the minor lines, majors excluded.
    pub minor: Vec<f32>,
    /// The grid was requested but is too dense at this zoom to draw. Nothing
    /// was generated; the UI should say the grid is hidden, not pretend it is
    /// off.
    pub suppressed: bool,
}

impl GridLines {
    pub fn is_empty(&self) -> bool {
        self.major.is_empty() && self.minor.is_empty()
    }
}

/// A line closer than this to its neighbour, in screen points, is not worth
/// drawing: two hairlines a point apart read as a smear, not as a grid.
///
/// [`design::Space::Hair`] is the smallest rung the spacing scale defines — the
/// sanctioned half-unit — which is exactly what a legibility floor wants. It is
/// spelled through the scale rather than as a bare number so a re-tuned grid
/// unit moves it too.
pub const MIN_LINE_GAP_PT: f32 = design::Space::Hair.units() * design::UNIT_PT;

/// The most lines one axis may contribute in a frame.
pub const MAX_LINES_PER_AXIS: usize = 2048;

/// Multiples of `step` covering `lo..=hi`, capped at [`MAX_LINES_PER_AXIS`].
fn multiples_in(lo: f32, hi: f32, step: f32) -> Option<Vec<f32>> {
    if !step.is_finite() || step <= 0.0 || !lo.is_finite() || !hi.is_finite() || hi < lo {
        return Some(Vec::new());
    }
    let first = (lo / step).ceil();
    let last = (hi / step).floor();
    let count = (last - first + 1.0).max(0.0);
    if !count.is_finite() || count as usize > MAX_LINES_PER_AXIS {
        return None;
    }
    let n = count as i64;
    let mut out = Vec::with_capacity(n.max(0) as usize);
    for i in 0..n {
        out.push((first + i as f32) * step);
    }
    Some(out)
}

/// The document grid on one axis, culled to the visible area.
pub fn grid_lines(
    camera: &CanvasCamera,
    viewport: &Viewport,
    settings: &GridSettings,
    axis: Axis,
) -> GridLines {
    if !settings.visible || viewport.is_degenerate() {
        return GridLines::default();
    }
    let scale = camera.scale_pt(viewport);
    if !scale.is_finite() || scale <= 0.0 {
        return GridLines::default();
    }
    let visible = camera.visible_doc_rect(viewport);
    let (lo, hi) = match axis {
        Axis::X => (visible.min.x, visible.max.x),
        Axis::Y => (visible.min.y, visible.max.y),
    };

    let major = settings.major_spacing();
    if major * scale < MIN_LINE_GAP_PT {
        return GridLines {
            suppressed: true,
            ..GridLines::default()
        };
    }
    let Some(major_lines) = multiples_in(lo, hi, major) else {
        return GridLines {
            suppressed: true,
            ..GridLines::default()
        };
    };

    let mut minor_lines = Vec::new();
    if let Some(minor) = settings.minor_spacing() {
        if minor * scale >= MIN_LINE_GAP_PT {
            if let Some(all) = multiples_in(lo, hi, minor) {
                let ratio = major / minor;
                minor_lines = all
                    .into_iter()
                    .filter(|v| {
                        let k = v / major;
                        // Keep only the ones that are not also major lines.
                        (k - k.round()).abs() > 0.5 / ratio.max(2.0)
                    })
                    .collect();
            }
        }
    }

    GridLines {
        major: major_lines,
        minor: minor_lines,
        suppressed: false,
    }
}

/// Physical pixels per document pixel below which single-pixel boundaries are
/// not drawn. Photoshop's threshold is the same idea: the grid must be at least
/// a few device pixels apart or it is noise.
pub const PIXEL_GRID_MIN_ZOOM: f32 = 8.0;

/// Whether the pixel grid should be drawn at this zoom.
pub fn pixel_grid_visible(settings: &GridSettings, camera: &CanvasCamera) -> bool {
    settings.pixel_grid && camera.zoom >= PIXEL_GRID_MIN_ZOOM
}

/// The single-pixel boundaries on one axis, culled to the visible area.
///
/// Empty when the zoom is below [`PIXEL_GRID_MIN_ZOOM`] — the pixel grid is a
/// zoom-gated affordance, not a setting the user has to keep toggling.
pub fn pixel_grid_lines(
    camera: &CanvasCamera,
    viewport: &Viewport,
    settings: &GridSettings,
    axis: Axis,
    canvas: DocRect,
) -> Vec<f32> {
    if !pixel_grid_visible(settings, camera) || viewport.is_degenerate() {
        return Vec::new();
    }
    let visible = camera.visible_doc_rect(viewport);
    let clipped = visible.intersect(&canvas);
    if clipped.is_empty() {
        return Vec::new();
    }
    let (lo, hi) = match axis {
        Axis::X => (clipped.min.x, clipped.max.x),
        Axis::Y => (clipped.min.y, clipped.max.y),
    };
    multiples_in(lo, hi, 1.0).unwrap_or_default()
}

/// Screen-point positions for a set of document coordinates on one axis.
///
/// Only meaningful on an axis-aligned view; the painter uses it for the grid,
/// which it draws as full-length screen lines. Under rotation the grid is drawn
/// from document endpoints instead — see [`crate::canvas::paint`].
pub fn to_screen_pt(
    camera: &CanvasCamera,
    viewport: &Viewport,
    axis: Axis,
    doc_values: &[f32],
) -> Vec<f32> {
    doc_values
        .iter()
        .map(|v| {
            let p = camera.screen_pt_of(viewport, axis.compose(*v, 0.0));
            axis.of(p)
        })
        .collect()
}

/// The two document-space endpoints of a grid line, spanning the visible area.
pub fn line_endpoints(visible: DocRect, axis: Axis, value: f32) -> (Vec2, Vec2) {
    match axis {
        Axis::X => (
            Vec2::new(value, visible.min.y),
            Vec2::new(value, visible.max.y),
        ),
        Axis::Y => (
            Vec2::new(visible.min.x, value),
            Vec2::new(visible.max.x, value),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::viewport::PanelInsets;

    fn vp() -> Viewport {
        Viewport::new(
            Vec2::new(1000.0, 800.0),
            PanelInsets::new(200.0, 100.0, 40.0, 20.0),
            2.0,
        )
    }

    fn cam(zoom: f32) -> CanvasCamera {
        CanvasCamera {
            center: Vec2::new(400.0, 300.0),
            zoom,
            ..CanvasCamera::default()
        }
    }

    #[test]
    fn an_invisible_grid_produces_nothing_and_is_not_reported_as_suppressed() {
        let s = GridSettings::default();
        assert!(!s.visible);
        let lines = grid_lines(&cam(1.0), &vp(), &s, Axis::X);
        assert!(lines.is_empty() && !lines.suppressed);
    }

    #[test]
    fn grid_lines_are_multiples_of_the_spacing_inside_the_visible_area() {
        let v = vp();
        let c = cam(1.0);
        let s = GridSettings {
            visible: true,
            spacing_doc: 100.0,
            subdivisions: 1,
            ..GridSettings::default()
        };
        let lines = grid_lines(&c, &v, &s, Axis::X);
        assert!(!lines.major.is_empty());
        let visible = c.visible_doc_rect(&v);
        for x in &lines.major {
            assert!((x / 100.0 - (x / 100.0).round()).abs() < 1e-3, "{x}");
            assert!(*x >= visible.min.x - 1e-3 && *x <= visible.max.x + 1e-3);
        }
        // Adjacent lines are exactly one spacing apart.
        for pair in lines.major.windows(2) {
            assert!((pair[1] - pair[0] - 100.0).abs() < 1e-2);
        }
    }

    #[test]
    fn subdivisions_appear_and_never_duplicate_a_major_line() {
        let v = vp();
        let c = cam(2.0);
        let s = GridSettings {
            visible: true,
            spacing_doc: 100.0,
            subdivisions: 4,
            ..GridSettings::default()
        };
        let lines = grid_lines(&c, &v, &s, Axis::X);
        assert!(!lines.minor.is_empty());
        for m in &lines.minor {
            let k = m / 100.0;
            assert!(
                (k - k.round()).abs() > 1e-3,
                "{m} is a major line and should not be in the minor set"
            );
            assert!((m / 25.0 - (m / 25.0).round()).abs() < 1e-3, "{m}");
        }
        assert!(!lines.suppressed);
    }

    #[test]
    fn a_grid_too_dense_to_read_is_suppressed_rather_than_drawn() {
        let v = vp();
        let s = GridSettings {
            visible: true,
            spacing_doc: 1.0,
            subdivisions: 1,
            ..GridSettings::default()
        };
        // 1 document pixel at 1/64 zoom is far under a screen point.
        let lines = grid_lines(&cam(1.0 / 64.0), &v, &s, Axis::X);
        assert!(lines.suppressed, "a sub-point grid must not be emitted");
        assert!(lines.is_empty());

        // Zoomed in, the same grid is fine.
        let visible_again = grid_lines(&cam(8.0), &v, &s, Axis::X);
        assert!(!visible_again.suppressed);
        assert!(!visible_again.major.is_empty());
    }

    #[test]
    fn subdivisions_drop_out_before_the_majors_do() {
        let v = vp();
        let s = GridSettings {
            visible: true,
            spacing_doc: 32.0,
            subdivisions: 100,
            ..GridSettings::default()
        };
        // Majors are 32 doc px, minors 0.32 — at scale_pt 0.5 that is 0.16pt.
        let lines = grid_lines(&cam(1.0), &v, &s, Axis::X);
        assert!(!lines.major.is_empty(), "majors still read");
        assert!(
            lines.minor.is_empty(),
            "minors are below the legibility floor"
        );
        assert!(!lines.suppressed);
    }

    #[test]
    fn hostile_settings_cannot_produce_an_unbounded_line_list() {
        let v = vp();
        for spacing in [0.0_f32, -5.0, f32::NAN, 1e-9] {
            let s = GridSettings {
                visible: true,
                spacing_doc: spacing,
                subdivisions: u32::MAX,
                ..GridSettings::default()
            };
            let lines = grid_lines(&cam(1.0), &v, &s, Axis::X);
            assert!(lines.major.len() <= MAX_LINES_PER_AXIS);
            assert!(lines.minor.len() <= MAX_LINES_PER_AXIS);
        }
    }

    #[test]
    fn the_pixel_grid_is_gated_on_zoom() {
        let v = vp();
        let s = GridSettings::default();
        assert!(s.pixel_grid);
        let canvas = DocRect::of_canvas(Vec2::new(800.0, 600.0));
        assert!(pixel_grid_lines(&cam(4.0), &v, &s, Axis::X, canvas).is_empty());
        assert!(!pixel_grid_visible(&s, &cam(4.0)));
        assert!(pixel_grid_visible(&s, &cam(PIXEL_GRID_MIN_ZOOM)));
        let lines = pixel_grid_lines(&cam(16.0), &v, &s, Axis::X, canvas);
        assert!(!lines.is_empty());
        for pair in lines.windows(2) {
            assert!((pair[1] - pair[0] - 1.0).abs() < 1e-3);
        }
        for x in &lines {
            assert!((x - x.round()).abs() < 1e-3, "{x} is not a pixel boundary");
        }
    }

    #[test]
    fn the_pixel_grid_stops_at_the_canvas_edge() {
        let v = vp();
        let s = GridSettings::default();
        let canvas = DocRect::new(Vec2::new(10.0, 10.0), Vec2::new(40.0, 40.0));
        let c = CanvasCamera {
            center: Vec2::new(25.0, 25.0),
            zoom: 16.0,
            ..CanvasCamera::default()
        };
        let lines = pixel_grid_lines(&c, &v, &s, Axis::X, canvas);
        assert!(!lines.is_empty());
        for x in &lines {
            assert!(*x >= 10.0 && *x <= 40.0, "{x} is outside the canvas");
        }
    }

    #[test]
    fn turning_the_pixel_grid_off_wins_over_the_zoom_gate() {
        let s = GridSettings {
            pixel_grid: false,
            ..GridSettings::default()
        };
        assert!(!pixel_grid_visible(&s, &cam(64.0)));
        assert!(pixel_grid_lines(
            &cam(64.0),
            &vp(),
            &s,
            Axis::X,
            DocRect::of_canvas(Vec2::splat(100.0))
        )
        .is_empty());
    }

    #[test]
    fn grid_coordinates_convert_to_the_same_screen_position_the_camera_gives() {
        let v = vp();
        let c = cam(3.0);
        let doc = [0.0_f32, 64.0, 128.0];
        let screen = to_screen_pt(&c, &v, Axis::X, &doc);
        for (d, s) in doc.iter().zip(screen) {
            assert!((c.screen_pt_of(&v, Vec2::new(*d, 0.0)).x - s).abs() < 1e-3);
        }
    }

    #[test]
    fn line_endpoints_span_the_visible_area_on_the_other_axis() {
        let visible = DocRect::new(Vec2::new(-10.0, -20.0), Vec2::new(30.0, 40.0));
        let (a, b) = line_endpoints(visible, Axis::X, 5.0);
        assert_eq!(a, Vec2::new(5.0, -20.0));
        assert_eq!(b, Vec2::new(5.0, 40.0));
        let (c, d) = line_endpoints(visible, Axis::Y, 5.0);
        assert_eq!(c, Vec2::new(-10.0, 5.0));
        assert_eq!(d, Vec2::new(30.0, 5.0));
    }

    #[test]
    fn spacing_and_subdivisions_are_clamped_not_trusted() {
        let s = GridSettings {
            spacing_doc: f32::NAN,
            subdivisions: 0,
            ..GridSettings::default()
        };
        assert_eq!(s.major_spacing(), GridSettings::default().spacing_doc);
        assert_eq!(s.minor_spacing(), None);
        let big = GridSettings {
            spacing_doc: 1e12,
            subdivisions: u32::MAX,
            ..GridSettings::default()
        };
        assert_eq!(big.major_spacing(), GridSettings::MAX_SPACING);
        assert_eq!(
            big.minor_spacing(),
            Some(GridSettings::MAX_SPACING / GridSettings::MAX_SUBDIVISIONS as f32)
        );
    }
}
