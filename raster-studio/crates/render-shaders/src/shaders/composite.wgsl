// Layer compositing over a destination, with a selectable blend mode.
//
// Blend indices MUST match `layer_model::BlendMode::shader_index`:
//   0 Normal, 1 Multiply, 2 Screen, 3 Overlay, 4 Darken, 5 Lighten
//
// Operates in linear, premultiplied space; the caller is responsible for the
// linearization and (un)premultiplication passes around this shader.
//
// ORIENTATION CONVENTION (shared with quad.wgsl, do not diverge):
//   clip-space y = +1 is the TOP of the screen and maps to v = 0.0, the FIRST
//   row of the bound textures. quad.wgsl gets the same flip through its camera
//   affine; this shader applies it directly in `vs_main`.

struct Params {
    blend_index: u32,
    opacity: f32,
    _pad0: u32,
    _pad1: u32,
};

@group(0) @binding(0) var dst_tex: texture_2d<f32>;
@group(0) @binding(1) var src_tex: texture_2d<f32>;
@group(0) @binding(2) var samp: sampler;
@group(0) @binding(3) var<uniform> params: Params;

fn blend_rgb(base: vec3<f32>, src: vec3<f32>, mode: u32) -> vec3<f32> {
    switch mode {
        case 1u: { return base * src; }                                  // Multiply
        case 2u: { return 1.0 - (1.0 - base) * (1.0 - src); }            // Screen
        case 3u: {                                                       // Overlay
            let lo = 2.0 * base * src;
            let hi = 1.0 - 2.0 * (1.0 - base) * (1.0 - src);
            return select(hi, lo, base < vec3<f32>(0.5));
        }
        case 4u: { return min(base, src); }                              // Darken
        case 5u: { return max(base, src); }                              // Lighten
        default: { return src; }                                         // Normal
    }
}

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vidx: u32) -> VsOut {
    var verts = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(-1.0, 1.0),
        vec2<f32>(-1.0, 1.0),  vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
    );
    var out: VsOut;
    let p = verts[vidx];
    out.pos = vec4<f32>(p, 0.0, 1.0);
    out.uv = vec2<f32>(p.x * 0.5 + 0.5, 1.0 - (p.y * 0.5 + 0.5));
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let dst = textureSample(dst_tex, samp, in.uv);
    var src = textureSample(src_tex, samp, in.uv);
    src = src * params.opacity;

    // Un-premultiply to straight alpha so the separable blend functions see
    // real colors rather than alpha-attenuated ones.
    let base_rgb = select(dst.rgb / max(dst.a, 1e-5), vec3<f32>(0.0), dst.a < 1e-5);
    let src_rgb  = select(src.rgb / max(src.a, 1e-5), vec3<f32>(0.0), src.a < 1e-5);

    // W3C Compositing 1, §9.2: the blend function only applies in proportion to
    // how opaque the BACKDROP is:
    //     cs = (1 - alpha_b) * Cs + alpha_b * B(Cb, Cs)
    // Without this term a Multiply/Darken layer over an empty (fully
    // transparent) destination would evaluate B(0, Cs) = 0 and vanish.
    let blended = blend_rgb(base_rgb, src_rgb, params.blend_index);
    let cs = (1.0 - dst.a) * src_rgb + dst.a * blended;

    // Porter-Duff "over", emitting premultiplied color.
    let out_a = src.a + dst.a * (1.0 - src.a);
    let out_rgb = cs * src.a + base_rgb * dst.a * (1.0 - src.a);
    return vec4<f32>(out_rgb, out_a);
}
