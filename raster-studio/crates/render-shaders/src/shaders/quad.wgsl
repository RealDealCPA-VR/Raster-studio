// Fullscreen textured quad with a 2D camera transform.
//
// The camera is supplied as an affine mapping from normalized-device space to
// the source image's UV space, letting us pan and zoom by changing uniforms
// only (no vertex re-upload). Phase-0 canvas shader.
//
// ORIENTATION CONVENTION (shared with composite.wgsl, do not diverge):
//   clip-space y = +1 is the TOP of the screen and maps to v = 0.0, which is
//   the FIRST row uploaded into the source texture (the top of the image).
//   quad.wgsl encodes that flip inside the camera affine (see
//   `render::Camera::clip_to_uv`, which emits a NEGATIVE clip.y coefficient for
//   v); composite.wgsl encodes the same flip directly in its vertex stage.
//
// COLOR CONVENTION: all shading below happens in LINEAR light, and every
// literal color is stored pre-linearized. What reaches the framebuffer must be
// sRGB-ENCODED either way:
//   * `*-Srgb` target  -> the hardware encodes; emit the linear value as is.
//   * plain unorm target -> nothing encodes; `srgb_encode` is 1.0 and this
//     shader applies the sRGB transfer function itself.
// Skipping that branch on a non-sRGB surface renders everything far too dark:
// the light checker cell would store its linear 0.522527 as byte 133 instead of
// the intended sRGB 0.75 -> byte 191.

struct Camera {
    // Maps clip-space position (-1..1) to source UV (0..1) as a 2x3 affine:
    //   u = ax*clip.x + bx*clip.y + cx
    //   v = ay*clip.x + by*clip.y + cy
    m0: vec4<f32>,   // ax, bx, cx, ay
    m1: vec4<f32>,   // by, cy, srgb_encode (0.0 or 1.0), _
};

@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;
@group(0) @binding(2) var<uniform> camera: Camera;

// Transparency checkerboard, in FRAMEBUFFER PIXELS so the cells keep a constant
// on-screen size under any window size, aspect ratio or zoom level.
const CHECKER_CELL_PX: f32 = 8.0;
// sRGB 0.60 and 0.75, pre-linearized for the sRGB render target.
const CHECKER_DARK: f32 = 0.318546;
const CHECKER_LIGHT: f32 = 0.522527;
// The flat pasteboard outside the document: sRGB 0x3C (a neutral grey just
// above the dark panels), pre-linearized.
const PASTEBOARD: f32 = 0.045031;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) clip_xy: vec2<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vidx: u32) -> VsOut {
    // Two triangles covering the whole screen.
    var verts = array<vec2<f32>, 6>(
        vec2<f32>(-1.0, -1.0), vec2<f32>(1.0, -1.0), vec2<f32>(-1.0, 1.0),
        vec2<f32>(-1.0, 1.0),  vec2<f32>(1.0, -1.0), vec2<f32>(1.0, 1.0),
    );
    var out: VsOut;
    let p = verts[vidx];
    out.pos = vec4<f32>(p, 0.0, 1.0);
    out.clip_xy = p;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Apply the affine camera to map clip position -> source UV.
    let ax = camera.m0.x; let bx = camera.m0.y; let cx = camera.m0.z;
    let ay = camera.m0.w; let by = camera.m1.x; let cy = camera.m1.y;
    let uv = vec2<f32>(
        ax * in.clip_xy.x + bx * in.clip_xy.y + cx,
        ay * in.clip_xy.x + by * in.clip_xy.y + cy,
    );

    // `textureSample` requires uniform control flow, so sample unconditionally
    // (clamped) and gate the contribution afterwards.
    let inside = uv.x >= 0.0 && uv.x <= 1.0 && uv.y >= 0.0 && uv.y <= 1.0;
    let src = textureSample(src_tex, src_sampler, clamp(uv, vec2<f32>(0.0), vec2<f32>(1.0)));

    // Photopea's pasteboard: a flat neutral grey OUTSIDE the document; the
    // checkerboard shows only through transparent pixels INSIDE it.
    let checker = checker_color(in.pos.xy);
    let backdrop = select(vec3(PASTEBOARD, PASTEBOARD, PASTEBOARD), checker, inside);
    let a = select(0.0, src.a, inside);
    let lit = mix(backdrop, src.rgb, a);
    let out_rgb = select(lit, linear_to_srgb(lit), camera.m1.z > 0.5);
    return vec4<f32>(out_rgb, 1.0);
}

/// sRGB OETF (IEC 61966-2-1), applied only when the target format does not do
/// it in hardware. Input is clamped: negative linear values have no encoding.
fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let x = clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
    let lo = x * 12.92;
    let hi = 1.055 * pow(x, vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, x <= vec3<f32>(0.0031308));
}

/// Checkerboard value for a framebuffer-pixel coordinate. Cell (0,0) is light.
fn checker_color(frag_px: vec2<f32>) -> vec3<f32> {
    let gx = floor(frag_px.x / CHECKER_CELL_PX);
    let gy = floor(frag_px.y / CHECKER_CELL_PX);
    let odd = (gx + gy) - 2.0 * floor((gx + gy) * 0.5);
    let v = select(CHECKER_DARK, CHECKER_LIGHT, odd < 0.5);
    return vec3<f32>(v, v, v);
}
