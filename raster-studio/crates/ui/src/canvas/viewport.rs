//! The canvas viewport: which part of the window the document actually gets,
//! and how logical points relate to physical pixels there.
//!
//! # The bug this type exists to kill
//!
//! egui lays panels out in **logical points**. The renderer draws into a
//! **physical pixel** surface. Handing the camera the full surface size — which
//! is what the shell did before this module existed — makes the image centre on
//! the middle of the *window* rather than the middle of the *free area*, so a
//! left dock of 240pt pushes the image 240pt to the right of where the user
//! sees the empty space, and a right dock hides the other end of it. On a 2x
//! display the error doubles, because the inset was never scaled.
//!
//! A [`Viewport`] carries all three facts at once — the surface size, the panel
//! insets, and the display scale — so nothing downstream has to remember to
//! convert. Every screen coordinate in this module tree is measured against one.

use glam::Vec2;

use super::geom::DocRect;

/// Space reserved by panels around the canvas, in logical points.
///
/// These are the *outer* chrome insets: docks, the options bar, the status bar.
/// Rulers are inset separately by [`Viewport::inset_by`], because a ruler is
/// drawn by the canvas itself and its thickness has to come off the image area
/// without being confused with a panel.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PanelInsets {
    pub left: f32,
    pub right: f32,
    pub top: f32,
    pub bottom: f32,
}

impl PanelInsets {
    /// No panels at all.
    pub const NONE: PanelInsets = PanelInsets {
        left: 0.0,
        right: 0.0,
        top: 0.0,
        bottom: 0.0,
    };

    /// Insets in points. Negative and non-finite values are treated as zero:
    /// a panel cannot reserve negative space, and letting a `NaN` through here
    /// would poison every coordinate conversion for the rest of the frame.
    pub fn new(left: f32, right: f32, top: f32, bottom: f32) -> Self {
        Self {
            left: sane(left),
            right: sane(right),
            top: sane(top),
            bottom: sane(bottom),
        }
    }

    /// Equal insets on every side.
    pub fn uniform(all: f32) -> Self {
        Self::new(all, all, all, all)
    }

    /// Total width taken by the left and right panels.
    pub fn horizontal(&self) -> f32 {
        self.left + self.right
    }

    /// Total height taken by the top and bottom panels.
    pub fn vertical(&self) -> f32 {
        self.top + self.bottom
    }

    /// The two insets combined, side by side.
    pub fn plus(&self, other: &PanelInsets) -> Self {
        Self::new(
            self.left + other.left,
            self.right + other.right,
            self.top + other.top,
            self.bottom + other.bottom,
        )
    }
}

fn sane(v: f32) -> f32 {
    if v.is_finite() {
        v.max(0.0)
    } else {
        0.0
    }
}

/// The region the document draws into, plus the display scale.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Viewport {
    /// The whole drawing surface, in logical points.
    surface_pt: Vec2,
    /// What the panels have taken, in logical points.
    insets: PanelInsets,
    /// Physical pixels per logical point. Always finite and positive.
    pixels_per_point: f32,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            surface_pt: Vec2::new(1280.0, 720.0),
            insets: PanelInsets::NONE,
            pixels_per_point: 1.0,
        }
    }
}

impl Viewport {
    /// The smallest display scale accepted. A zero or negative scale would make
    /// every point-to-pixel conversion degenerate, so it is clamped rather than
    /// propagated.
    pub const MIN_SCALE: f32 = 0.05;
    /// The largest display scale accepted.
    pub const MAX_SCALE: f32 = 16.0;

    /// A viewport over `surface_pt` logical points with the given panel insets.
    pub fn new(surface_pt: Vec2, insets: PanelInsets, pixels_per_point: f32) -> Self {
        Self {
            surface_pt: Vec2::new(sane(surface_pt.x), sane(surface_pt.y)),
            insets,
            pixels_per_point: if pixels_per_point.is_finite() {
                pixels_per_point.clamp(Self::MIN_SCALE, Self::MAX_SCALE)
            } else {
                1.0
            },
        }
    }

    /// A viewport described by the content rectangle rather than by insets —
    /// which is how egui reports it, as `ui.max_rect()` inside the central
    /// panel. The insets are derived, so [`Viewport::insets`] still answers.
    pub fn from_content_rect(surface_pt: Vec2, content: egui::Rect, pixels_per_point: f32) -> Self {
        let surface = Vec2::new(sane(surface_pt.x), sane(surface_pt.y));
        let insets = PanelInsets::new(
            content.min.x,
            surface.x - content.max.x,
            content.min.y,
            surface.y - content.max.y,
        );
        Self::new(surface, insets, pixels_per_point)
    }

    /// The whole window, in logical points.
    pub fn surface_pt(&self) -> Vec2 {
        self.surface_pt
    }

    /// The whole window, in physical pixels.
    pub fn surface_px(&self) -> Vec2 {
        self.surface_pt * self.pixels_per_point
    }

    /// What the panels reserved.
    pub fn insets(&self) -> PanelInsets {
        self.insets
    }

    /// Physical pixels per logical point.
    pub fn pixels_per_point(&self) -> f32 {
        self.pixels_per_point
    }

    /// A copy with additional space taken off the content area — what the
    /// rulers do to themselves before the image is laid out.
    pub fn inset_by(&self, extra: PanelInsets) -> Self {
        Self::new(
            self.surface_pt,
            self.insets.plus(&extra),
            self.pixels_per_point,
        )
    }

    /// A copy at a different display scale, same layout.
    pub fn with_scale(&self, pixels_per_point: f32) -> Self {
        Self::new(self.surface_pt, self.insets, pixels_per_point)
    }

    /// Top-left of the content area, in logical points.
    pub fn origin_pt(&self) -> Vec2 {
        Vec2::new(self.insets.left, self.insets.top)
    }

    /// Size of the content area, in logical points. Never negative: panels
    /// wider than the window clamp it to zero rather than inverting it.
    pub fn size_pt(&self) -> Vec2 {
        Vec2::new(
            (self.surface_pt.x - self.insets.horizontal()).max(0.0),
            (self.surface_pt.y - self.insets.vertical()).max(0.0),
        )
    }

    /// Centre of the content area, in logical points. **This** is what the
    /// image centres on — not the middle of the window.
    pub fn center_pt(&self) -> Vec2 {
        self.origin_pt() + self.size_pt() * 0.5
    }

    /// Far corner of the content area, in logical points.
    pub fn max_pt(&self) -> Vec2 {
        self.origin_pt() + self.size_pt()
    }

    /// Top-left of the content area, in physical pixels.
    pub fn origin_px(&self) -> Vec2 {
        self.origin_pt() * self.pixels_per_point
    }

    /// Size of the content area, in physical pixels.
    pub fn size_px(&self) -> Vec2 {
        self.size_pt() * self.pixels_per_point
    }

    /// Centre of the content area, in physical pixels.
    pub fn center_px(&self) -> Vec2 {
        self.center_pt() * self.pixels_per_point
    }

    /// The content area as an egui rectangle, for clipping and painting.
    pub fn content_rect(&self) -> egui::Rect {
        super::geom::to_egui_rect(self.origin_pt(), self.max_pt())
    }

    /// The content area as a document-space-shaped rectangle in points. Used
    /// where the same rectangle helpers are convenient on screen coordinates.
    pub fn content_bounds_pt(&self) -> DocRect {
        DocRect::new(self.origin_pt(), self.max_pt())
    }

    /// `true` when a screen-point position is inside the canvas rather than
    /// over a panel. Half-open, so a point exactly on the right or bottom edge
    /// belongs to the panel beyond it and never to both.
    pub fn contains_pt(&self, p: Vec2) -> bool {
        self.content_bounds_pt().contains(p)
    }

    /// `true` when the content area has no area to draw in — the window is
    /// collapsed, or the panels have eaten everything. Callers must not divide
    /// by the viewport size without checking this.
    pub fn is_degenerate(&self) -> bool {
        let s = self.size_pt();
        !(s.x > 0.0 && s.y > 0.0)
    }

    /// Logical points to physical pixels.
    pub fn to_px(&self, pt: Vec2) -> Vec2 {
        pt * self.pixels_per_point
    }

    /// Physical pixels to logical points.
    pub fn to_pt(&self, px: Vec2) -> Vec2 {
        px / self.pixels_per_point
    }

    /// The width of a one-physical-pixel line, in points, so hairlines stay
    /// crisp at any display scale.
    pub fn hairline_pt(&self) -> f32 {
        1.0 / self.pixels_per_point
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp() -> Viewport {
        Viewport::new(
            Vec2::new(1000.0, 800.0),
            PanelInsets::new(240.0, 300.0, 44.0, 28.0),
            2.0,
        )
    }

    #[test]
    fn the_content_area_is_the_window_minus_the_panels() {
        let v = vp();
        assert_eq!(v.origin_pt(), Vec2::new(240.0, 44.0));
        assert_eq!(v.size_pt(), Vec2::new(460.0, 728.0));
        assert_eq!(v.max_pt(), Vec2::new(700.0, 772.0));
    }

    /// The regression this whole type exists for: the centre the image is laid
    /// out around must be the centre of the *free* area, not of the window.
    #[test]
    fn the_centre_is_the_free_areas_centre_not_the_windows() {
        let v = vp();
        let window_centre = v.surface_pt() * 0.5;
        assert_eq!(v.center_pt(), Vec2::new(470.0, 408.0));
        assert_ne!(v.center_pt(), window_centre);
        // …and the same in physical pixels, scaled by the display scale.
        assert_eq!(v.center_px(), Vec2::new(940.0, 816.0));
        assert_eq!(v.size_px(), Vec2::new(920.0, 1456.0));
        assert_eq!(v.origin_px(), Vec2::new(480.0, 88.0));
    }

    #[test]
    fn insets_scale_with_the_display_and_are_not_left_in_points() {
        let one_x = Viewport::new(
            Vec2::new(1000.0, 800.0),
            PanelInsets::new(240.0, 0.0, 0.0, 0.0),
            1.0,
        );
        let two_x = one_x.with_scale(2.0);
        assert_eq!(one_x.origin_px(), Vec2::new(240.0, 0.0));
        assert_eq!(two_x.origin_px(), Vec2::new(480.0, 0.0));
    }

    #[test]
    fn a_point_over_a_panel_is_not_in_the_canvas() {
        let v = vp();
        assert!(v.contains_pt(Vec2::new(240.0, 44.0)));
        assert!(v.contains_pt(Vec2::new(699.9, 771.9)));
        // left dock, right dock, options bar, status bar
        assert!(!v.contains_pt(Vec2::new(239.9, 400.0)));
        assert!(!v.contains_pt(Vec2::new(700.0, 400.0)));
        assert!(!v.contains_pt(Vec2::new(400.0, 43.9)));
        assert!(!v.contains_pt(Vec2::new(400.0, 772.0)));
    }

    #[test]
    fn a_content_rect_and_insets_describe_the_same_viewport() {
        let v = vp();
        let rebuilt = Viewport::from_content_rect(
            v.surface_pt(),
            egui::Rect::from_min_max(egui::pos2(240.0, 44.0), egui::pos2(700.0, 772.0)),
            2.0,
        );
        assert_eq!(rebuilt.insets(), v.insets());
        assert_eq!(rebuilt.size_pt(), v.size_pt());
        assert_eq!(rebuilt.center_pt(), v.center_pt());
    }

    #[test]
    fn rulers_inset_the_canvas_without_being_confused_with_panels() {
        let v = vp();
        let with_rulers = v.inset_by(PanelInsets::new(16.0, 0.0, 16.0, 0.0));
        assert_eq!(with_rulers.origin_pt(), Vec2::new(256.0, 60.0));
        assert_eq!(with_rulers.size_pt(), Vec2::new(444.0, 712.0));
        // The window did not change size, only the area the image gets.
        assert_eq!(with_rulers.surface_pt(), v.surface_pt());
    }

    #[test]
    fn panels_wider_than_the_window_collapse_rather_than_invert() {
        let v = Viewport::new(
            Vec2::new(300.0, 200.0),
            PanelInsets::new(400.0, 400.0, 500.0, 0.0),
            1.0,
        );
        assert_eq!(v.size_pt(), Vec2::ZERO);
        assert!(v.is_degenerate());
        assert!(!v.contains_pt(v.center_pt()));
    }

    #[test]
    fn hostile_scales_and_insets_are_clamped_not_propagated() {
        let v = Viewport::new(
            Vec2::new(f32::NAN, 800.0),
            PanelInsets::new(-10.0, f32::INFINITY, f32::NAN, 5.0),
            0.0,
        );
        assert_eq!(v.insets(), PanelInsets::new(0.0, 0.0, 0.0, 5.0));
        assert!(v.surface_pt().x.is_finite());
        assert!(v.pixels_per_point() >= Viewport::MIN_SCALE);

        let huge = Viewport::new(Vec2::splat(100.0), PanelInsets::NONE, 1e9);
        assert_eq!(huge.pixels_per_point(), Viewport::MAX_SCALE);
        let nan_scale = Viewport::new(Vec2::splat(100.0), PanelInsets::NONE, f32::NAN);
        assert_eq!(nan_scale.pixels_per_point(), 1.0);
    }

    #[test]
    fn point_and_pixel_conversions_are_inverses() {
        let v = vp();
        let p = Vec2::new(123.5, -7.25);
        assert_eq!(v.to_pt(v.to_px(p)), p);
        assert_eq!(v.hairline_pt(), 0.5);
    }
}
