//! WGSL shader sources and pipeline metadata.
//!
//! Shaders are kept as embedded string constants so the renderer can create
//! pipelines without touching the filesystem at runtime (important for a
//! packaged desktop app). Add new `.wgsl` files under `shaders/` and expose
//! them here with `include_str!`.

/// Fullscreen textured quad: samples a source texture with a camera transform.
/// Used for the Phase-0 canvas (display one image, pan/zoom).
pub const QUAD_WGSL: &str = include_str!("shaders/quad.wgsl");

/// Composite/blend shader. Selects a blend mode by index (matching
/// `layer_model::BlendMode::shader_index`).
pub const COMPOSITE_WGSL: &str = include_str!("shaders/composite.wgsl");

/// All shader sources, for pipeline warm-up and validation in tests.
pub const ALL: &[(&str, &str)] = &[("quad", QUAD_WGSL), ("composite", COMPOSITE_WGSL)];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shaders_are_non_empty() {
        for (name, src) in ALL {
            assert!(!src.trim().is_empty(), "shader {name} is empty");
        }
    }
}
