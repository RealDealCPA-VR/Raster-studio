//! Screen-space line overlays drawn on top of the canvas.
//!
//! [`Canvas`](crate::Canvas) draws one texture and nothing else, which is
//! exactly right for the image — but a selection is not in the image. The
//! marching ants trace the *boundary between pixels*, they are measured in
//! framebuffer pixels rather than document ones (so the dashes keep their size
//! at any zoom), and they animate. None of that can live in a document-sized
//! texture, so it is a second pass: this one.
//!
//! # What this crate knows and what it does not
//!
//! Segments, widths and colours. It does **not** know what a selection is, how
//! an outline is traced or how fast ants crawl — `selection::outline` walks the
//! coverage mask and `ui::canvas::ants` cuts the result into dashes and
//! projects it through the camera. The application hands the answer here as
//! [`Segment`]s already in framebuffer pixels, which keeps this a line
//! renderer and not a second copy of the selection model.
//!
//! # Why a triangle per segment and not a line primitive
//!
//! `wgpu::PrimitiveTopology::LineList` draws exactly one pixel wide, ignores
//! the width you ask for, and is allowed to be implementation-defined about
//! which pixels it lights. The ants have to be legible over any image, which
//! means at least two device pixels and a second colour underneath — so each
//! segment is expanded into a quad on the CPU ([`segment_vertices`], a pure
//! function with tests that need no GPU) and drawn as two triangles.
//!
//! # Colour convention
//!
//! The same one the rest of the crate keeps: colours are LINEAR, and what
//! reaches the framebuffer is sRGB-encoded either by the hardware (an `*-Srgb`
//! target) or by the fragment shader (a plain unorm one), selected by the
//! `srgb_encode` flag in the uniform. See `render_shaders`' crate docs.

use anyhow::{bail, Result};
use bytemuck::{Pod, Zeroable};
use glam::Vec2;
use wgpu::util::DeviceExt;

use crate::canvas::Canvas;
use crate::context::GpuContext;

/// WGSL for the overlay pass.
///
/// Kept here rather than in `render-shaders` because it is this module's own
/// vertex layout — the constants in that crate are the ones two passes share.
const OVERLAY_WGSL: &str = r#"
struct Uniforms {
    // Framebuffer size in pixels, then the sRGB-encode flag (0 or 1).
    viewport: vec2<f32>,
    srgb_encode: f32,
    _pad: f32,
};

@group(0) @binding(0) var<uniform> u: Uniforms;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(@location(0) p: vec2<f32>, @location(1) color: vec4<f32>) -> VsOut {
    // Framebuffer pixels -> clip space. y = +1 is the TOP of the target, the
    // same orientation quad.wgsl and composite.wgsl keep, so pixel row 0 is
    // the top row.
    let vp = max(u.viewport, vec2<f32>(1.0, 1.0));
    var out: VsOut;
    out.pos = vec4<f32>(p.x / vp.x * 2.0 - 1.0, 1.0 - p.y / vp.y * 2.0, 0.0, 1.0);
    out.color = color;
    return out;
}

/// sRGB OETF (IEC 61966-2-1), applied only when the target format does not do
/// it in hardware. Identical to `quad.wgsl`'s, deliberately.
fn linear_to_srgb(c: vec3<f32>) -> vec3<f32> {
    let x = clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
    let lo = x * 12.92;
    let hi = 1.055 * pow(x, vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(hi, lo, x <= vec3<f32>(0.0031308));
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let rgb = select(in.color.rgb, linear_to_srgb(in.color.rgb), u.srgb_encode > 0.5);
    return vec4<f32>(rgb, in.color.a);
}
"#;

/// One straight run of the overlay, in **framebuffer pixels**.
///
/// `(0, 0)` is the top-left of the target, which is the unit `winit` reports a
/// cursor in and the unit [`crate::Camera`] measures a viewport in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Segment {
    pub a: Vec2,
    pub b: Vec2,
    /// Stroke width in framebuffer pixels.
    pub width_px: f32,
    /// Straight-alpha **linear** RGBA.
    pub color: [f32; 4],
}

impl Segment {
    pub fn new(a: Vec2, b: Vec2, width_px: f32, color: [f32; 4]) -> Self {
        Self {
            a,
            b,
            width_px,
            color,
        }
    }
}

/// One vertex of the expanded geometry: a framebuffer-pixel position and a
/// colour.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Pod, Zeroable)]
pub struct OverlayVertex {
    pub pos: [f32; 2],
    pub color: [f32; 4],
}

/// Narrowest and widest stroke the overlay will draw, in framebuffer pixels.
///
/// A zero or negative width is a line nobody can see and a NaN one is a
/// triangle the rasteriser may do anything with, so both are clamped rather
/// than passed on. The upper bound keeps a hostile style from filling the
/// window with one segment.
pub const MIN_WIDTH_PX: f32 = 1.0;
pub const MAX_WIDTH_PX: f32 = 64.0;

/// The most vertices one frame of overlay may hold.
///
/// `ui::canvas::ants` already caps the segments it produces; this is the
/// renderer's own bound, so a caller that builds its geometry some other way
/// cannot ask for a gigabyte of vertex buffer.
pub const MAX_VERTICES: usize = 6 * 65_536;

/// Expand line segments into the triangles that draw them.
///
/// Two triangles per segment, six vertices, appended to `out`. A segment whose
/// endpoints coincide, or which carries a non-finite coordinate, contributes
/// nothing: it has no direction, so it has no quad, and emitting a degenerate
/// one would hand the rasteriser NaN.
///
/// Pure, so the geometry is checked without a device — which is the half of
/// this module that can go wrong quietly.
pub fn segment_vertices(segments: &[Segment], out: &mut Vec<OverlayVertex>) {
    for seg in segments {
        if out.len() + 6 > MAX_VERTICES {
            return;
        }
        if !seg.a.is_finite() || !seg.b.is_finite() {
            continue;
        }
        let d = seg.b - seg.a;
        let len = d.length();
        // `is_finite` above already ruled a NaN length out, so this is only
        // asking whether the segment has any direction at all.
        if len <= 0.0 {
            continue;
        }
        let dir = d / len;
        // Left-hand normal, half a stroke wide on each side.
        let width = if seg.width_px.is_finite() {
            seg.width_px.clamp(MIN_WIDTH_PX, MAX_WIDTH_PX)
        } else {
            MIN_WIDTH_PX
        };
        let n = Vec2::new(-dir.y, dir.x) * (width * 0.5);
        let color = seg.color.map(|c| if c.is_finite() { c } else { 0.0 });
        let quad = [seg.a - n, seg.b - n, seg.a + n, seg.b + n];
        for i in [0usize, 1, 2, 2, 1, 3] {
            out.push(OverlayVertex {
                pos: [quad[i].x, quad[i].y],
                color,
            });
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct OverlayUniform {
    viewport: [f32; 2],
    srgb_encode: f32,
    _pad: f32,
}

/// The overlay pass: a vertex buffer of coloured triangles drawn over whatever
/// is already in the target.
///
/// The pass **loads** rather than clears, so it composes on top of the canvas
/// exactly as the egui pass does.
pub struct Overlay {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform: wgpu::Buffer,
    vertices: Option<wgpu::Buffer>,
    /// Capacity of `vertices`, in vertices.
    capacity: usize,
    count: u32,
    viewport: Vec2,
    srgb_encode: bool,
}

impl Overlay {
    /// Build the pass for `output_format`.
    ///
    /// # Panics
    /// If the format is not an 8-bit display format — the same rule
    /// [`Canvas::supports_target`] states, for the same reason: the shader
    /// hard-codes 8-bit display encoding. Use [`Overlay::try_new`] to get that
    /// as an error.
    pub fn new(gpu: &GpuContext, output_format: wgpu::TextureFormat) -> Self {
        Self::try_new(gpu, output_format).expect("unsupported overlay target format")
    }

    pub fn try_new(gpu: &GpuContext, output_format: wgpu::TextureFormat) -> Result<Self> {
        if !Canvas::supports_target(output_format) {
            bail!("{output_format:?} is not an 8-bit display format the overlay can draw into");
        }
        let shader = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("overlay"),
                source: wgpu::ShaderSource::Wgsl(OVERLAY_WGSL.into()),
            });
        let layout = gpu
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("overlay-bgl"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                }],
            });
        let srgb_encode = !output_format.is_srgb();
        let uniform = gpu
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("overlay-uniform"),
                contents: bytemuck::bytes_of(&OverlayUniform {
                    viewport: [1.0, 1.0],
                    srgb_encode: if srgb_encode { 1.0 } else { 0.0 },
                    _pad: 0.0,
                }),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            });
        let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("overlay-bg"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });
        let pipeline_layout = gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("overlay-pl"),
                bind_group_layouts: &[&layout],
                push_constant_ranges: &[],
            });
        let pipeline = gpu
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("overlay-pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: "vs_main",
                    compilation_options: Default::default(),
                    buffers: &[wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<OverlayVertex>() as wgpu::BufferAddress,
                        step_mode: wgpu::VertexStepMode::Vertex,
                        attributes: &[
                            wgpu::VertexAttribute {
                                offset: 0,
                                shader_location: 0,
                                format: wgpu::VertexFormat::Float32x2,
                            },
                            wgpu::VertexAttribute {
                                offset: 8,
                                shader_location: 1,
                                format: wgpu::VertexFormat::Float32x4,
                            },
                        ],
                    }],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: "fs_main",
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: output_format,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::TriangleList,
                    // A quad's winding flips with the direction its segment
                    // runs, so nothing may be culled.
                    cull_mode: None,
                    ..Default::default()
                },
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            });
        Ok(Self {
            pipeline,
            bind_group,
            uniform,
            vertices: None,
            capacity: 0,
            count: 0,
            viewport: Vec2::ONE,
            srgb_encode,
        })
    }

    /// The framebuffer size the pixel coordinates are measured against.
    pub fn set_viewport(&mut self, gpu: &GpuContext, size_px: Vec2) {
        let size = Vec2::new(
            if size_px.x.is_finite() {
                size_px.x
            } else {
                1.0
            }
            .max(1.0),
            if size_px.y.is_finite() {
                size_px.y
            } else {
                1.0
            }
            .max(1.0),
        );
        if size == self.viewport {
            return;
        }
        self.viewport = size;
        gpu.queue.write_buffer(
            &self.uniform,
            0,
            bytemuck::bytes_of(&OverlayUniform {
                viewport: [size.x, size.y],
                srgb_encode: if self.srgb_encode { 1.0 } else { 0.0 },
                _pad: 0.0,
            }),
        );
    }

    /// Replace the overlay's geometry. Reports how many vertices it holds now.
    ///
    /// The buffer is grown, never shrunk: the ants change every frame and a
    /// reallocation per frame is a stall the animation would show.
    pub fn set_segments(&mut self, gpu: &GpuContext, segments: &[Segment]) -> u32 {
        let mut verts = Vec::with_capacity(segments.len() * 6);
        segment_vertices(segments, &mut verts);
        self.count = verts.len() as u32;
        if verts.is_empty() {
            return 0;
        }
        if self.capacity < verts.len() || self.vertices.is_none() {
            let capacity = verts.len().next_power_of_two().min(MAX_VERTICES);
            self.vertices = Some(gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("overlay-vertices"),
                size: (capacity * std::mem::size_of::<OverlayVertex>()) as wgpu::BufferAddress,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            self.capacity = capacity;
        }
        let buffer = self.vertices.as_ref().expect("just built");
        gpu.queue
            .write_buffer(buffer, 0, bytemuck::cast_slice(&verts));
        self.count
    }

    /// Vertices currently held.
    pub fn vertex_count(&self) -> u32 {
        self.count
    }

    /// `true` when there is nothing to draw, so the caller can skip the pass.
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Draw over whatever is already in `target`.
    ///
    /// A no-op when [`Overlay::is_empty`] — an empty pass would still cost a
    /// render pass per frame on a document with no selection.
    pub fn render(&self, encoder: &mut wgpu::CommandEncoder, target: &wgpu::TextureView) {
        let (Some(buffer), false) = (self.vertices.as_ref(), self.is_empty()) else {
            return;
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("overlay"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                ops: wgpu::Operations {
                    // MUST be Load: the canvas pass drew the image already.
                    load: wgpu::LoadOp::Load,
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        pass.set_vertex_buffer(
            0,
            buffer.slice(
                ..(self.count as u64)
                    * (std::mem::size_of::<OverlayVertex>() as wgpu::BufferAddress),
            ),
        );
        pass.draw(0..self.count, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(a: (f32, f32), b: (f32, f32), w: f32) -> Segment {
        Segment::new(
            Vec2::new(a.0, a.1),
            Vec2::new(b.0, b.1),
            w,
            [1.0, 0.0, 0.0, 1.0],
        )
    }

    #[test]
    fn a_segment_becomes_two_triangles_straddling_its_own_line() {
        let mut out = Vec::new();
        // Horizontal, four pixels wide: the quad must span y = 8 +/- 2 and
        // nothing else.
        segment_vertices(&[seg((10.0, 10.0), (30.0, 10.0), 4.0)], &mut out);
        assert_eq!(out.len(), 6, "one segment is two triangles");
        let ys: Vec<f32> = out.iter().map(|v| v.pos[1]).collect();
        assert!(ys
            .iter()
            .all(|y| (*y - 8.0).abs() < 1e-4 || (*y - 12.0).abs() < 1e-4));
        assert!(ys.iter().any(|y| *y < 10.0) && ys.iter().any(|y| *y > 10.0));
        let xs: Vec<f32> = out.iter().map(|v| v.pos[0]).collect();
        assert!(xs
            .iter()
            .all(|x| (*x - 10.0).abs() < 1e-4 || (*x - 30.0).abs() < 1e-4));
        assert!(out.iter().all(|v| v.color == [1.0, 0.0, 0.0, 1.0]));
    }

    #[test]
    fn a_degenerate_or_non_finite_segment_draws_nothing() {
        let mut out = Vec::new();
        segment_vertices(
            &[
                seg((5.0, 5.0), (5.0, 5.0), 2.0),
                seg((f32::NAN, 0.0), (10.0, 10.0), 2.0),
                seg((0.0, 0.0), (f32::INFINITY, 0.0), 2.0),
            ],
            &mut out,
        );
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn a_hostile_width_is_clamped_rather_than_drawn() {
        for (asked, expected) in [
            (0.0_f32, MIN_WIDTH_PX),
            (1e9, MAX_WIDTH_PX),
            (-3.0, MIN_WIDTH_PX),
        ] {
            let mut out = Vec::new();
            segment_vertices(&[seg((0.0, 0.0), (10.0, 0.0), asked)], &mut out);
            assert_eq!(out.len(), 6);
            let spread = out
                .iter()
                .map(|v| v.pos[1])
                .fold(f32::NEG_INFINITY, f32::max)
                - out.iter().map(|v| v.pos[1]).fold(f32::INFINITY, f32::min);
            assert!(
                (spread - expected).abs() < 1e-3,
                "width {asked} produced a {spread}px stroke"
            );
        }
        // A NaN width falls back to the minimum rather than to a NaN triangle.
        let mut out = Vec::new();
        segment_vertices(&[seg((0.0, 0.0), (10.0, 0.0), f32::NAN)], &mut out);
        assert!(out
            .iter()
            .all(|v| v.pos[0].is_finite() && v.pos[1].is_finite()));
    }

    #[test]
    fn the_vertex_budget_is_a_hard_stop() {
        let many: Vec<Segment> = (0..(MAX_VERTICES / 6 + 100))
            .map(|i| seg((0.0, i as f32), (10.0, i as f32), 2.0))
            .collect();
        let mut out = Vec::new();
        segment_vertices(&many, &mut out);
        assert!(out.len() <= MAX_VERTICES, "{} vertices", out.len());
        assert!(!out.is_empty());
    }
}
