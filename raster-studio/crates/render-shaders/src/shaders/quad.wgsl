// Fullscreen textured quad with a 2D camera transform.
//
// The camera is supplied as an affine mapping from normalized-device space to
// the source image's UV space, letting us pan and zoom by changing uniforms
// only (no vertex re-upload). Phase-0 canvas shader.

struct Camera {
    // Maps clip-space position (-1..1) to source UV (0..1).
    // stored as two rows of a 2x3 affine: [ax, bx, cx, ay, by, cy]
    m0: vec4<f32>,   // ax, bx, cx, ay
    m1: vec4<f32>,   // by, cy, _, _
};

@group(0) @binding(0) var src_tex: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;
@group(0) @binding(2) var<uniform> camera: Camera;

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

    // Outside the image: draw the transparency checkerboard.
    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) {
        let checker = checker_color(in.clip_xy);
        return vec4<f32>(checker, checker, checker, 1.0);
    }
    return textureSample(src_tex, src_sampler, uv);
}

fn checker_color(clip: vec2<f32>) -> f32 {
    let cell = 16.0;
    let gx = floor((clip.x * 0.5 + 0.5) * cell);
    let gy = floor((clip.y * 0.5 + 0.5) * cell);
    let odd = (gx + gy) - 2.0 * floor((gx + gy) * 0.5);
    return select(0.6, 0.75, odd < 0.5);
}
