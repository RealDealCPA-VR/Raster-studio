//! Color management scaffold.
//!
//! v1 exposes sRGB only, but the pipeline shape is fixed now so we never bake
//! sRGB assumptions into layer or shader APIs:
//!
//! ```text
//! source decode
//!   → source ICC to linear working RGB
//!   → linear-premultiplied compositing
//!   → display ICC / monitor transform
//!   → presentation
//! ```
//!
//! Every source and document carries a [`ColorSpace`]. Phase 3 swaps the
//! placeholder transforms here for a real ICC engine (LittleCMS binding or
//! equivalent) without changing callers.

use serde::{Deserialize, Serialize};

/// Color space metadata attached to sources and the document working space.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ColorSpace {
    /// sRGB with the standard IEC 61966-2-1 transfer function.
    #[default]
    Srgb,
    /// Linear (scene-referred) sRGB primaries. The internal working space.
    LinearSrgb,
    /// Display P3 (wide gamut). Phase 3.
    DisplayP3,
    /// An ICC profile referenced by content hash in the asset store.
    IccProfile { asset_hash: String },
}

/// sRGB electro-optical transfer function (gamma-encoded -> linear), per channel.
pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// Inverse of [`srgb_to_linear`] (linear -> gamma-encoded).
pub fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Premultiply straight-alpha RGBA (values in 0..=1) into premultiplied form.
///
/// Compositing is done in linear-premultiplied space; convert straight-alpha
/// source pixels with this before blending.
pub fn premultiply(rgba: [f32; 4]) -> [f32; 4] {
    let a = rgba[3];
    [rgba[0] * a, rgba[1] * a, rgba[2] * a, a]
}

/// Undo [`premultiply`], returning straight-alpha RGBA. Safe at `a == 0`.
pub fn unpremultiply(rgba: [f32; 4]) -> [f32; 4] {
    let a = rgba[3];
    if a <= f32::EPSILON {
        [0.0, 0.0, 0.0, 0.0]
    } else {
        [rgba[0] / a, rgba[1] / a, rgba[2] / a, a]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_linear_roundtrip() {
        for &c in &[0.0f32, 0.02, 0.5, 0.8, 1.0] {
            let round = linear_to_srgb(srgb_to_linear(c));
            assert!((round - c).abs() < 1e-5, "roundtrip failed for {c}");
        }
    }

    #[test]
    fn premultiply_roundtrip() {
        let px = [0.6, 0.3, 0.9, 0.5];
        let round = unpremultiply(premultiply(px));
        for i in 0..4 {
            assert!((round[i] - px[i]).abs() < 1e-6);
        }
    }

    #[test]
    fn unpremultiply_zero_alpha_is_transparent() {
        assert_eq!(unpremultiply([0.0, 0.0, 0.0, 0.0]), [0.0, 0.0, 0.0, 0.0]);
    }
}
