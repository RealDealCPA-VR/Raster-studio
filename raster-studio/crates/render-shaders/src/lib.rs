//! WGSL shader sources and pipeline metadata.
//!
//! Shaders are kept as embedded string constants so the renderer can create
//! pipelines without touching the filesystem at runtime (important for a
//! packaged desktop app). Add new `.wgsl` files under `shaders/` and expose
//! them here with `include_str!`.
//!
//! # Conventions the shaders in this crate all obey
//!
//! * **Orientation.** Clip-space `y = +1` is the top of the target and maps to
//!   `v = 0.0`, the first texel row of the source texture. `quad.wgsl` gets the
//!   flip from the camera affine; `composite.wgsl` applies it in its vertex
//!   stage; `mipmap.wgsl` needs no flip because it indexes texels directly
//!   (target texel `(x, y)` comes from source texels `(2x, 2y)..(2x+1, 2y+1)`).
//! * **Color.** Shading happens in LINEAR light and every literal color is
//!   stored pre-linearized. `quad.wgsl` emits the linear value for an
//!   `*-Srgb` target (hardware encodes) and applies the sRGB OETF itself for a
//!   plain unorm target, selected by the `srgb_encode` flag in its uniform.
//! * **Alpha.** Source textures and every mip level of them carry STRAIGHT
//!   (non-premultiplied) alpha. `mipmap.wgsl` premultiplies before averaging and
//!   un-premultiplies after, so minification does not drag transparent texels'
//!   black RGB into their opaque neighbours. `composite.wgsl` is the exception:
//!   its inputs and output are premultiplied, as documented in that file.

/// Fullscreen textured quad: samples a source texture with a camera transform.
/// Used for the Phase-0 canvas (display one image, pan/zoom).
pub const QUAD_WGSL: &str = include_str!("shaders/quad.wgsl");

/// Composite/blend shader. Selects a blend mode by index (matching
/// `layer_model::BlendMode::shader_index`).
pub const COMPOSITE_WGSL: &str = include_str!("shaders/composite.wgsl");

/// Bilinear mip-chain downsampler, one draw per generated level.
pub const MIPMAP_WGSL: &str = include_str!("shaders/mipmap.wgsl");

/// All shader sources, for pipeline warm-up and validation in tests.
pub const ALL: &[(&str, &str)] = &[
    ("quad", QUAD_WGSL),
    ("composite", COMPOSITE_WGSL),
    ("mipmap", MIPMAP_WGSL),
];

/// Edge length, in framebuffer pixels, of one transparency-checkerboard cell.
///
/// Mirrors `CHECKER_CELL_PX` in `quad.wgsl`; [`checker_constants_match_wgsl`]
/// keeps the two from drifting apart.
pub const CHECKER_CELL_PX: u32 = 8;

/// The checkerboard's light cell as it appears in an 8-bit sRGB framebuffer
/// (sRGB 0.75). Cell `(0, 0)` — the top-left of the target — is this one.
pub const CHECKER_LIGHT_SRGB_U8: u8 = 191;

/// The checkerboard's dark cell as it appears in an 8-bit sRGB framebuffer
/// (sRGB 0.60).
pub const CHECKER_DARK_SRGB_U8: u8 = 153;

/// The flat pasteboard outside the document, as it appears in an 8-bit sRGB
/// framebuffer (sRGB 0x3C — a neutral grey just above the dark panels).
pub const PASTEBOARD_SRGB_U8: u8 = 60;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shaders_are_non_empty() {
        for (name, src) in ALL {
            assert!(!src.trim().is_empty(), "shader {name} is empty");
        }
    }

    /// The Rust-side checker constants are what the GPU tests assert against;
    /// they are only correct if `quad.wgsl` still uses the matching literals.
    #[test]
    fn checker_constants_match_wgsl() {
        assert!(
            QUAD_WGSL.contains("const CHECKER_CELL_PX: f32 = 8.0;"),
            "quad.wgsl cell size no longer matches CHECKER_CELL_PX = {CHECKER_CELL_PX}"
        );
        assert!(
            QUAD_WGSL.contains("const PASTEBOARD: f32 = 0.045031;"),
            "quad.wgsl pasteboard grey no longer matches PASTEBOARD_SRGB_U8 = {PASTEBOARD_SRGB_U8}"
        );
        // sRGB 0.60 and 0.75 pre-linearized; see the sRGB EOTF.
        assert!(
            QUAD_WGSL.contains("const CHECKER_DARK: f32 = 0.318546;"),
            "quad.wgsl dark cell is no longer linearized sRGB 0.60"
        );
        assert!(
            QUAD_WGSL.contains("const CHECKER_LIGHT: f32 = 0.522527;"),
            "quad.wgsl light cell is no longer linearized sRGB 0.75"
        );
    }

    /// `composite.wgsl` derives its UVs in the vertex stage, so clip `+y` must
    /// map to `v = 0`.
    ///
    /// This is only a drift alarm on the source text. The BEHAVIOUR is pinned by
    /// `render`'s GPU tests (`composite_preserves_vertical_orientation` and
    /// `mip_levels_keep_top_to_bottom_order`), which render an asymmetric
    /// fixture and fail if either shader mirrors it.
    #[test]
    fn composite_flips_v() {
        assert!(
            strip_line_comments(COMPOSITE_WGSL).contains("1.0 - (p.y * 0.5 + 0.5)"),
            "composite.wgsl lost the v flip"
        );
    }

    /// `mipmap.wgsl` must average PREMULTIPLIED taps: a filtering `textureSample`
    /// blends RGB and A independently and darkens every partially transparent
    /// region. Behaviour is pinned by `render`'s
    /// `mip_downsample_is_alpha_weighted`.
    #[test]
    fn mipmap_does_not_use_a_filtering_sampler() {
        // Comments explain WHY the sampler is gone, so match on code only.
        let code = strip_line_comments(MIPMAP_WGSL);
        assert!(
            !code.contains("textureSample"),
            "mipmap.wgsl filters with a sampler again; that averages straight alpha"
        );
        assert!(
            code.contains("textureLoad"),
            "mipmap.wgsl no longer takes explicit texel loads"
        );
    }

    /// Drop `//` line comments so source-text checks cannot be satisfied — or
    /// broken — by prose.
    fn strip_line_comments(src: &str) -> String {
        src.lines()
            .map(|l| l.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n")
    }
}
