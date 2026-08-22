//! Blend modes. The early set matches the render doc's shader list; the enum is
//! open to extension for the full creative set in Phase 1.

use serde::{Deserialize, Serialize};

/// How a layer's pixels combine with the composite beneath it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum BlendMode {
    #[default]
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
}

impl BlendMode {
    /// Stable index used to select the matching GPU pipeline / WGSL branch.
    /// Keep in sync with `render-shaders`.
    pub const fn shader_index(self) -> u32 {
        match self {
            BlendMode::Normal => 0,
            BlendMode::Multiply => 1,
            BlendMode::Screen => 2,
            BlendMode::Overlay => 3,
            BlendMode::Darken => 4,
            BlendMode::Lighten => 5,
        }
    }

    /// Apply this blend mode to a single pair of **straight-alpha** channels
    /// (color only, in 0..=1). Reference implementation for golden-image tests;
    /// the GPU shaders must match these results within tolerance.
    pub fn blend_channel(self, base: f32, src: f32) -> f32 {
        match self {
            BlendMode::Normal => src,
            BlendMode::Multiply => base * src,
            BlendMode::Screen => 1.0 - (1.0 - base) * (1.0 - src),
            BlendMode::Overlay => {
                if base < 0.5 {
                    2.0 * base * src
                } else {
                    1.0 - 2.0 * (1.0 - base) * (1.0 - src)
                }
            }
            BlendMode::Darken => base.min(src),
            BlendMode::Lighten => base.max(src),
        }
    }

    pub const ALL: [BlendMode; 6] = [
        BlendMode::Normal,
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Overlay,
        BlendMode::Darken,
        BlendMode::Lighten,
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiply_darkens() {
        assert_eq!(BlendMode::Multiply.blend_channel(0.5, 0.5), 0.25);
    }

    #[test]
    fn screen_lightens() {
        assert_eq!(BlendMode::Screen.blend_channel(0.5, 0.5), 0.75);
    }

    #[test]
    fn darken_lighten_are_min_max() {
        assert_eq!(BlendMode::Darken.blend_channel(0.2, 0.8), 0.2);
        assert_eq!(BlendMode::Lighten.blend_channel(0.2, 0.8), 0.8);
    }

    #[test]
    fn shader_indices_unique() {
        let mut seen = std::collections::HashSet::new();
        for m in BlendMode::ALL {
            assert!(seen.insert(m.shader_index()), "duplicate shader index");
        }
    }
}
