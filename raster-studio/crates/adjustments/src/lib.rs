//! Parametric, non-destructive adjustments.
//!
//! Each adjustment is a pure function on a color value (operating in the
//! document's working space). These CPU reference implementations define the
//! *ground truth*; the GPU shaders in `render` must match them within tolerance
//! (validated by golden-image tests). Because adjustments are parametric they
//! remain fully editable — the roadmap's "adjustments remain editable after
//! save/reload" gate depends on this.

/// Levels: remap input black/white points with a gamma in between.
/// All values in 0..=1; `gamma > 0`.
pub fn levels(c: f32, black: f32, white: f32, gamma: f32) -> f32 {
    let denom = (white - black).max(1e-5);
    let normalized = ((c - black) / denom).clamp(0.0, 1.0);
    normalized.powf(1.0 / gamma.max(1e-5))
}

/// Exposure in stops: multiply linear value by 2^stops.
pub fn exposure(linear_c: f32, stops: f32) -> f32 {
    (linear_c * 2f32.powf(stops)).clamp(0.0, 1.0)
}

/// Saturation around luma (Rec. 709). `sat = 1.0` is identity, `0.0` grayscale.
pub fn saturation(rgb: [f32; 3], sat: f32) -> [f32; 3] {
    let luma = 0.2126 * rgb[0] + 0.7152 * rgb[1] + 0.0722 * rgb[2];
    [
        (luma + (rgb[0] - luma) * sat).clamp(0.0, 1.0),
        (luma + (rgb[1] - luma) * sat).clamp(0.0, 1.0),
        (luma + (rgb[2] - luma) * sat).clamp(0.0, 1.0),
    ]
}

/// Evaluate a monotonic curve defined by control points via linear
/// interpolation. `points` must be sorted by x (input), each in 0..=1.
pub fn curve(c: f32, points: &[[f32; 2]]) -> f32 {
    if points.is_empty() {
        return c;
    }
    if c <= points[0][0] {
        return points[0][1];
    }
    if c >= points[points.len() - 1][0] {
        return points[points.len() - 1][1];
    }
    for w in points.windows(2) {
        let (a, b) = (w[0], w[1]);
        if c >= a[0] && c <= b[0] {
            let t = (c - a[0]) / (b[0] - a[0]).max(1e-5);
            return a[1] + t * (b[1] - a[1]);
        }
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_identity() {
        // black=0, white=1, gamma=1 => identity
        for &c in &[0.0, 0.25, 0.5, 0.75, 1.0] {
            assert!((levels(c, 0.0, 1.0, 1.0) - c).abs() < 1e-5);
        }
    }

    #[test]
    fn levels_clamps() {
        assert_eq!(levels(0.0, 0.2, 0.8, 1.0), 0.0);
        assert_eq!(levels(1.0, 0.2, 0.8, 1.0), 1.0);
    }

    #[test]
    fn exposure_one_stop_doubles() {
        assert!((exposure(0.25, 1.0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn saturation_zero_is_gray() {
        let g = saturation([0.2, 0.5, 0.9], 0.0);
        assert!((g[0] - g[1]).abs() < 1e-6 && (g[1] - g[2]).abs() < 1e-6);
    }

    #[test]
    fn curve_interpolates_midpoint() {
        let pts = [[0.0, 0.0], [1.0, 1.0]];
        assert!((curve(0.5, &pts) - 0.5).abs() < 1e-6);
        let steep = [[0.0, 0.0], [0.5, 0.9], [1.0, 1.0]];
        assert!((curve(0.25, &steep) - 0.45).abs() < 1e-6);
    }
}
