//! 2D pan/zoom camera. Produces the affine that the quad shader uses to map
//! clip-space (-1..1) to source-image UV (0..1).

use glam::Vec2;

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
        self.zoom = (self.zoom * factor).clamp(0.01, 64.0);
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
    /// the shader's `Camera` uniform layout.
    pub fn clip_to_uv(&self) -> ([f32; 4], [f32; 4]) {
        // Visible image-space extent (half width/height) at current zoom.
        let half = self.viewport_size / (2.0 * self.zoom.max(1e-6));
        let min = self.center - half; // image px at clip (-1,-1)
        let max = self.center + half; // image px at clip (+1,+1)

        // Map clip x in [-1,1] -> image [min.x, max.x] -> uv /= image_size.
        // uv = (min + (clip*0.5+0.5)*(max-min)) / image_size
        let span = max - min;
        let ax = (span.x * 0.5) / self.image_size.x;
        let cx = (min.x + span.x * 0.5) / self.image_size.x;
        let ay = (span.y * 0.5) / self.image_size.y;
        let by_ = (span.y * 0.0) / self.image_size.y; // no shear
        let cy = (min.y + span.y * 0.5) / self.image_size.y;
        let _ = by_;
        // No cross terms (bx=by=0) for an axis-aligned camera.
        ([ax, 0.0, cx, ay], [0.0, cy, 0.0, 0.0])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cam() -> Camera {
        Camera::new(Vec2::new(4096.0, 2160.0), Vec2::new(1280.0, 720.0))
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
