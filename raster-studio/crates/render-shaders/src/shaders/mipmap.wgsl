// Mip-chain generation: draws one mip level as a 2x2 box downsample of the
// level above it. The bound source view MUST expose exactly one mip level, so
// the shader cannot read levels that have not been written yet.
//
// ALPHA CONVENTION (shared with quad.wgsl / composite.wgsl's inputs):
//   Level 0 holds STRAIGHT (non-premultiplied) alpha, and so must every level
//   this pass emits. Averaging straight alpha is wrong: the RGB of a fully
//   transparent texel is normally 0, so a plain filtered tap drags the color of
//   every neighbouring opaque texel toward black and each minification step
//   grows a dark fringe. The correct box filter premultiplies each contributing
//   texel, averages color and alpha separately, and un-premultiplies:
//       a  = mean(a_i)
//       c  = mean(c_i * a_i) / a          (0 when a is 0)
//   That is why the taps are `textureLoad`s and not one bilinear `textureSample`
//   — a filtering sampler blends RGB and A independently and cannot express it.
//
// ORIENTATION CONVENTION (shared with quad.wgsl / composite.wgsl):
//   Target texel (x, y) is built from source texels (2x, 2y)..(2x+1, 2y+1), so
//   row 0 of a level is always derived from row 0 of the level above. Level N
//   therefore has the same top-to-bottom order as level 0; a flip here would
//   mirror the image at every zoom threshold.
//
// KNOWN LIMIT: for an odd source extent the last row/column is dropped rather
// than blended in with a 3-tap weighting. The resulting half-texel bias is below
// the level's own texel size and only affects non-power-of-two images.
//
// COLOR: the source view keeps the texture's format, so an *-Srgb level decodes
// on load and re-encodes on store and the averaging happens in linear light,
// which is what a correct downsample requires.

@group(0) @binding(0) var src_tex: texture_2d<f32>;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vidx: u32) -> VsOut {
    var verts = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(-1.0, 1.0),
        vec2<f32>(-1.0, 1.0),  vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
    );
    var out: VsOut;
    out.pos = vec4<f32>(verts[vidx], 0.0, 1.0);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // `pos.xy` is the framebuffer coordinate of this target texel's center, with
    // (0.5, 0.5) at the TOP-LEFT texel, so flooring yields its integer index.
    let dst_px = vec2<i32>(floor(in.pos.xy));
    let last = vec2<i32>(textureDimensions(src_tex, 0)) - vec2<i32>(1, 1);
    let base = dst_px * 2;

    var sum_rgb = vec3<f32>(0.0);
    var sum_a = 0.0;
    for (var dy = 0; dy < 2; dy = dy + 1) {
        for (var dx = 0; dx < 2; dx = dx + 1) {
            let c = min(base + vec2<i32>(dx, dy), last);
            let t = textureLoad(src_tex, c, 0);
            sum_rgb = sum_rgb + t.rgb * t.a;
            sum_a = sum_a + t.a;
        }
    }

    let a = sum_a * 0.25;
    // Un-premultiply. The `max` keeps the division finite so `select` never has
    // to choose between a value and a NaN.
    let rgb = select(sum_rgb * 0.25 / max(a, 1e-5), vec3<f32>(0.0), a < 1e-5);
    return vec4<f32>(rgb, a);
}
