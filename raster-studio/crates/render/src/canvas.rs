//! The Phase-0 canvas renderer: draws one image texture as a fullscreen quad
//! with a pan/zoom camera, over a transparency checkerboard that shows through
//! both outside the image bounds and behind transparent pixels inside them.

use bytemuck::{Pod, Zeroable};

use crate::camera::Camera;
use crate::context::GpuContext;
use crate::texture::GpuTexture;

/// GPU uniform matching `Camera` in `quad.wgsl`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct CameraUniform {
    m0: [f32; 4],
    m1: [f32; 4],
}

/// The backdrop behind the quad, as the sRGB value it should APPEAR as.
const CLEAR_SRGB: f64 = 0.1;

/// [`CLEAR_SRGB`] pre-linearized, i.e. `srgb_to_linear(0.1)`.
///
/// An `*-Srgb` target applies the sRGB transfer function to everything written
/// to it — including the clear value — so it must be handed this. Writing 0.1
/// there would show up as sRGB 0.35, more than three times as bright as
/// intended. A plain unorm target encodes nothing and takes [`CLEAR_SRGB`].
const CLEAR_LINEAR: f64 = 0.010_022_8;

/// Renders a single source texture with a camera transform.
pub struct Canvas {
    pipeline: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    camera_buf: wgpu::Buffer,
    bind_group_layout: wgpu::BindGroupLayout,
    bind_group: Option<wgpu::BindGroup>,
    format: wgpu::TextureFormat,
}

impl Canvas {
    /// Whether `format` can back a canvas target.
    ///
    /// The canvas shades in linear light and emits 8-bit *display* values, so
    /// the target must be an 8-bit RGBA/BGRA format. Both encodings are
    /// supported: an `*-Srgb` format gets the linear value and the hardware
    /// encodes it, a plain unorm format gets the sRGB encode applied inside the
    /// shader. Anything else (a float or 10-bit HDR target, a depth format)
    /// would receive values in the wrong space entirely.
    pub fn supports_target(format: wgpu::TextureFormat) -> bool {
        matches!(
            format,
            wgpu::TextureFormat::Rgba8Unorm
                | wgpu::TextureFormat::Rgba8UnormSrgb
                | wgpu::TextureFormat::Bgra8Unorm
                | wgpu::TextureFormat::Bgra8UnormSrgb
        )
    }

    /// Build the canvas pipeline for a given output texture format.
    ///
    /// # Panics
    /// If `output_format` is not accepted by [`Canvas::supports_target`].
    /// Failing loudly here beats silently rendering in the wrong color space.
    ///
    /// A caller that takes its format from adapter capabilities rather than
    /// choosing it should filter with [`Canvas::supports_target`] first, or use
    /// [`Canvas::try_new`] and degrade on the error. `app-shell` currently does
    /// neither: it picks `caps.formats.iter().find(|f| f.is_srgb())
    /// .unwrap_or(caps.formats[0])`, so an adapter exposing no sRGB surface
    /// format and an HDR format first would reach this panic.
    pub fn new(gpu: &GpuContext, output_format: wgpu::TextureFormat) -> Self {
        Self::try_new(gpu, output_format).expect("unsupported canvas target format")
    }

    /// Build the canvas pipeline, or report that `output_format` cannot back one.
    ///
    /// The fallible form of [`Canvas::new`], for callers whose format comes from
    /// adapter capabilities and who need to fail cleanly rather than abort.
    pub fn try_new(gpu: &GpuContext, output_format: wgpu::TextureFormat) -> anyhow::Result<Self> {
        anyhow::ensure!(
            Self::supports_target(output_format),
            "canvas target must be an 8-bit RGBA/BGRA format, got {output_format:?}"
        );
        let shader = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("quad-shader"),
                source: wgpu::ShaderSource::Wgsl(render_shaders::QUAD_WGSL.into()),
            });

        let bind_group_layout =
            gpu.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("canvas-bgl"),
                    entries: &[
                        wgpu::BindGroupLayoutEntry {
                            binding: 0,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Texture {
                                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                                view_dimension: wgpu::TextureViewDimension::D2,
                                multisampled: false,
                            },
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 1,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 2,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Buffer {
                                ty: wgpu::BufferBindingType::Uniform,
                                has_dynamic_offset: false,
                                min_binding_size: None,
                            },
                            count: None,
                        },
                    ],
                });

        let pipeline_layout = gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("canvas-pl"),
                bind_group_layouts: &[&bind_group_layout],
                push_constant_ranges: &[],
            });

        let pipeline = gpu
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("canvas-pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: "vs_main",
                    buffers: &[],
                    compilation_options: Default::default(),
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: "fs_main",
                    targets: &[Some(wgpu::ColorTargetState {
                        format: output_format,
                        // The shader composites the image over the checkerboard
                        // itself and emits an opaque color, so the
                        // fixed-function blender has nothing left to do.
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: Default::default(),
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview: None,
                cache: None,
            });

        let sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("canvas-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let camera_buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("canvas-camera"),
            size: std::mem::size_of::<CameraUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Ok(Self {
            pipeline,
            sampler,
            camera_buf,
            bind_group_layout,
            bind_group: None,
            format: output_format,
        })
    }

    /// Point the canvas at a source texture. Call when the open image changes.
    ///
    /// `source` should carry a full mip chain (the default for
    /// [`GpuTexture::from_rgba8`]); the canvas sampler uses trilinear
    /// minification and a single-level texture will alias when zoomed out.
    pub fn set_source(&mut self, gpu: &GpuContext, source: &GpuTexture) {
        let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("canvas-bg"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&source.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.camera_buf.as_entire_binding(),
                },
            ],
        });
        self.bind_group = Some(bind_group);
    }

    /// Upload the current camera transform.
    ///
    /// Also carries the target's color-space flag in `m1[2]`, which the camera
    /// itself knows nothing about: 1.0 asks `quad.wgsl` to apply the sRGB encode
    /// because the target format will not.
    pub fn update_camera(&self, gpu: &GpuContext, camera: &Camera) {
        let (m0, mut m1) = camera.clip_to_uv();
        m1[2] = if self.format.is_srgb() { 0.0 } else { 1.0 };
        let u = CameraUniform { m0, m1 };
        gpu.queue
            .write_buffer(&self.camera_buf, 0, bytemuck::bytes_of(&u));
    }

    /// Record a render pass drawing the quad into `target`.
    ///
    /// The pass always clears `target`, even before [`Canvas::set_source`] has
    /// been called, so the first frames show the backdrop rather than
    /// uninitialized memory.
    ///
    /// `target` must have the format this canvas was built for; the clear value
    /// is pre-linearized for an `*-Srgb` target and left as a display value for
    /// a plain unorm one, so the backdrop reads back as sRGB 0.1 either way.
    pub fn render(&self, encoder: &mut wgpu::CommandEncoder, target: &wgpu::TextureView) {
        let clear = if self.format.is_srgb() {
            CLEAR_LINEAR
        } else {
            CLEAR_SRGB
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("canvas-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: clear,
                        g: clear,
                        b: clear,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        let Some(bind_group) = &self.bind_group else {
            return; // cleared, but there is no source texture to draw yet
        };
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, bind_group, &[]);
        pass.draw(0..6, 0..1);
    }

    pub fn output_format(&self) -> wgpu::TextureFormat {
        self.format
    }
}

#[cfg(test)]
mod tests {
    use super::{Canvas, CLEAR_LINEAR, CLEAR_SRGB};

    /// sRGB EOTF, to check `CLEAR_LINEAR` really is `CLEAR_SRGB` linearized
    /// rather than a number someone typed.
    fn srgb_to_linear(c: f64) -> f64 {
        if c <= 0.040_45 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }

    #[test]
    fn clear_linear_is_the_display_value_linearized() {
        let expected = srgb_to_linear(CLEAR_SRGB);
        assert!(
            (CLEAR_LINEAR - expected).abs() < 1e-6,
            "CLEAR_LINEAR = {CLEAR_LINEAR}, srgb_to_linear({CLEAR_SRGB}) = {expected}"
        );
    }

    /// Both encodings of the 8-bit display formats are drawable; the shader
    /// branches on which one it got.
    #[test]
    fn eight_bit_display_formats_are_supported_targets() {
        for f in [
            wgpu::TextureFormat::Rgba8Unorm,
            wgpu::TextureFormat::Rgba8UnormSrgb,
            wgpu::TextureFormat::Bgra8Unorm,
            wgpu::TextureFormat::Bgra8UnormSrgb,
        ] {
            assert!(Canvas::supports_target(f), "{f:?} should be drawable");
        }
    }

    /// A float or 10-bit target would receive display-encoded values in a linear
    /// buffer; `Canvas::new` must refuse it rather than render the wrong colors.
    #[test]
    fn non_eight_bit_formats_are_rejected_targets() {
        for f in [
            wgpu::TextureFormat::Rgba16Float,
            wgpu::TextureFormat::Rgba32Float,
            wgpu::TextureFormat::Rgb10a2Unorm,
            wgpu::TextureFormat::R8Unorm,
        ] {
            assert!(!Canvas::supports_target(f), "{f:?} must not be drawable");
        }
    }
}
