//! 2D pan/zoom camera. Produces the affine that the quad shader uses to map
//! clip-space (-1..1) to source-image UV (0..1).

use glam::Vec2;

/// Smallest zoom a camera may be driven to: one image pixel per hundred screen
/// pixels of the whole picture. Below it a document is a dot.
///
/// Published rather than kept as a literal inside [`Camera::zoom_at`] because a
/// host that sets [`Camera::zoom`] directly — a typed zoom level, a Navigator
/// slider — must clamp to the same range a wheel gesture does, or the two
/// routes to the same number disagree.
pub const MIN_ZOOM: f32 = 0.01;
/// Largest zoom a camera may be driven to: sixty-four screen pixels per image
/// pixel, which is where a single pixel fills a small window.
pub const MAX_ZOOM: f32 = 64.0;

/// A pannable, zoomable 2D view of an image of known pixel size.
#[derive(Debug, Clone)]
pub struct Camera {
    /// Center of the view, in image pixel coordinates.
    pub center: Vec2,
    /// Zoom factor: screen pixels per image pixel (1.0 = 100%).
    pub zoom: f32,
    /// Size of the image being viewed, in pixels.
    pub image_size: Vec2,
    /// Size of the viewport (surface), in pixels.
    pub viewport_size: Vec2,
}

impl Camera {
    pub fn new(image_size: Vec2, viewport_size: Vec2) -> Self {
        Self {
            center: image_size * 0.5,
            zoom: 1.0,
            image_size,
            viewport_size,
        }
    }

    /// Pan by a delta given in *screen* pixels.
    pub fn pan_screen(&mut self, delta_px: Vec2) {
        self.center -= delta_px / self.zoom.max(1e-6);
    }

    /// Zoom toward a screen-space anchor (e.g. the cursor) by a multiplier.
    pub fn zoom_at(&mut self, anchor_screen: Vec2, factor: f32) {
        let before = self.screen_to_image(anchor_screen);
        self.zoom = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let after = self.screen_to_image(anchor_screen);
        // Keep the anchor point stationary in image space.
        self.center += before - after;
    }

    /// Scale the view so the whole image fits the viewport.
    pub fn fit(&mut self) {
        let sx = self.viewport_size.x / self.image_size.x.max(1.0);
        let sy = self.viewport_size.y / self.image_size.y.max(1.0);
        self.zoom = sx.min(sy);
        self.center = self.image_size * 0.5;
    }

    /// Convert a screen-space point to image-space pixels.
    pub fn screen_to_image(&self, screen: Vec2) -> Vec2 {
        let from_center = screen - self.viewport_size * 0.5;
        self.center + from_center / self.zoom.max(1e-6)
    }

    /// The affine used by `quad.wgsl`: maps clip-space (-1..1) to UV (0..1).
    ///
    /// Returned as two rows `[ax, bx, cx, ay]` and `[by, cy, 0, 0]` matching
    /// the shader's `Camera` uniform layout, which evaluates
    ///
    /// ```text
    /// u = ax*clip.x + bx*clip.y + cx
    /// v = ay*clip.x + by*clip.y + cy
    /// ```
    ///
    /// Constraints this must satisfy:
    ///
    /// * `u` depends on `clip.x` only and `v` on `clip.y` only — the camera is
    ///   axis aligned, so both cross terms (`bx`, `ay`) are exactly zero. A
    ///   non-zero `ay` smears one texture row across every screen row.
    /// * `by` is NEGATIVE: clip `y = +1` is the top of the screen and must map
    ///   to `v = 0`, the first (top) row of the source texture. See the
    ///   orientation convention documented in `render_shaders`.
    /// * `image_size` components are treated as at least 1 px, so a degenerate
    ///   image cannot produce a non-finite affine.
    ///
    /// `m0[1]`, `m0[3]` and `m1[2..]` are the unused slots of the 2x3 affine.
    /// `Canvas::update_camera` overwrites `m1[2]` with the target's
    /// sRGB-encode flag before upload; the camera itself has no opinion on
    /// color spaces.
    pub fn clip_to_uv(&self) -> ([f32; 4], [f32; 4]) {
        let image = Vec2::new(self.image_size.x.max(1.0), self.image_size.y.max(1.0));
        // Visible image-space extent (half width/height) at current zoom.
        let half = self.viewport_size / (2.0 * self.zoom.max(1e-6));

        // clip.x in [-1,1] -> image x in [center.x - half.x, center.x + half.x],
        // then divided by the image width to land in UV.
        let ax = half.x / image.x;
        let cx = self.center.x / image.x;
        // clip.y is flipped: +1 (screen top) -> center.y - half.y (image top).
        let by = -half.y / image.y;
        let cy = self.center.y / image.y;

        ([ax, 0.0, cx, 0.0], [by, cy, 0.0, 0.0])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cam() -> Camera {
        Camera::new(Vec2::new(4096.0, 2160.0), Vec2::new(1280.0, 720.0))
    }

    /// Evaluate the affine exactly as `quad.wgsl`'s fragment stage does.
    fn shader_uv(c: &Camera, clip: Vec2) -> Vec2 {
        let (m0, m1) = c.clip_to_uv();
        let (ax, bx, cx, ay) = (m0[0], m0[1], m0[2], m0[3]);
        let (by, cy) = (m1[0], m1[1]);
        Vec2::new(
            ax * clip.x + bx * clip.y + cx,
            ay * clip.x + by * clip.y + cy,
        )
    }

    #[test]
    fn clip_center_maps_to_camera_center_in_uv() {
        let mut c = cam();
        c.zoom = 0.5;
        c.center = Vec2::new(1000.0, 800.0);
        let uv = shader_uv(&c, Vec2::ZERO);
        assert!((uv.x - 1000.0 / 4096.0).abs() < 1e-6, "u = {}", uv.x);
        assert!((uv.y - 800.0 / 2160.0).abs() < 1e-6, "v = {}", uv.y);
    }

    #[test]
    fn u_ignores_clip_y_and_v_ignores_clip_x() {
        let mut c = cam();
        c.zoom = 0.37;
        c.center = Vec2::new(1234.0, 567.0);
        let a = shader_uv(&c, Vec2::new(0.5, -1.0));
        let b = shader_uv(&c, Vec2::new(0.5, 1.0));
        assert!(
            (a.x - b.x).abs() < 1e-6,
            "u drifted with clip.y: {a:?} {b:?}"
        );

        let l = shader_uv(&c, Vec2::new(-1.0, 0.25));
        let r = shader_uv(&c, Vec2::new(1.0, 0.25));
        assert!(
            (l.y - r.y).abs() < 1e-6,
            "v drifted with clip.x: {l:?} {r:?}"
        );
    }

    /// Regression: the old affine put the v scale on the clip.x coefficient, so
    /// v was constant down every column and every screen row sampled the same
    /// texture row.
    #[test]
    fn v_actually_varies_down_the_screen() {
        let mut c = cam();
        c.fit();
        let top = shader_uv(&c, Vec2::new(0.0, 1.0));
        let bottom = shader_uv(&c, Vec2::new(0.0, -1.0));
        assert!(
            (top.y - bottom.y).abs() > 0.1,
            "v is constant down the screen: top={} bottom={}",
            top.y,
            bottom.y
        );
    }

    /// clip y = +1 is the top of the screen and must show the TOP of the image.
    #[test]
    fn v_is_flipped_so_image_is_not_upside_down() {
        let mut c = cam();
        c.fit();
        let top = shader_uv(&c, Vec2::new(0.0, 1.0));
        let bottom = shader_uv(&c, Vec2::new(0.0, -1.0));
        assert!(
            top.y < bottom.y,
            "image renders upside down: top v={} bottom v={}",
            top.y,
            bottom.y
        );
    }

    /// A square image fitted into a square viewport must map the clip corners
    /// exactly onto the UV corners, with (−1,+1) at (0,0).
    #[test]
    fn fitted_square_maps_clip_corners_to_uv_corners() {
        let mut c = Camera::new(Vec2::splat(256.0), Vec2::splat(64.0));
        c.fit();
        let tl = shader_uv(&c, Vec2::new(-1.0, 1.0));
        let br = shader_uv(&c, Vec2::new(1.0, -1.0));
        assert!(tl.abs_diff_eq(Vec2::ZERO, 1e-6), "top-left uv = {tl:?}");
        assert!(br.abs_diff_eq(Vec2::ONE, 1e-6), "bottom-right uv = {br:?}");
    }

    #[test]
    fn zero_sized_image_yields_finite_affine() {
        let c = Camera::new(Vec2::ZERO, Vec2::new(800.0, 600.0));
        let (m0, m1) = c.clip_to_uv();
        for v in m0.iter().chain(m1.iter()) {
            assert!(v.is_finite(), "non-finite affine term {v}");
        }
    }

    #[test]
    fn screen_center_maps_to_camera_center() {
        let c = cam();
        let img = c.screen_to_image(c.viewport_size * 0.5);
        assert!((img - c.center).length() < 1e-3);
    }

    #[test]
    fn zoom_at_keeps_anchor_stationary() {
        let mut c = cam();
        let anchor = Vec2::new(300.0, 200.0);
        let before = c.screen_to_image(anchor);
        c.zoom_at(anchor, 2.0);
        let after = c.screen_to_image(anchor);
        assert!((before - after).length() < 1e-2, "anchor drifted");
    }

    #[test]
    fn fit_sets_zoom_to_min_ratio() {
        let mut c = cam();
        c.fit();
        assert!((c.zoom - (1280.0 / 4096.0)).abs() < 1e-4);
    }

    #[test]
    fn pan_moves_center_inversely_to_zoom() {
        let mut c = cam();
        c.zoom = 2.0;
        let start = c.center;
        c.pan_screen(Vec2::new(100.0, 0.0));
        assert!((c.center.x - (start.x - 50.0)).abs() < 1e-3);
    }
}
