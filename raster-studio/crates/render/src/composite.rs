//! One layer-over-destination compositing pass (`composite.wgsl`).

use bytemuck::{Pod, Zeroable};

use crate::context::GpuContext;

/// GPU uniform matching `Params` in `composite.wgsl`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Pod, Zeroable)]
struct ParamsUniform {
    blend_index: u32,
    opacity: f32,
    _pad0: u32,
    _pad1: u32,
}

/// How one layer combines with what is already on the canvas.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompositeParams {
    /// Blend function index. MUST match `layer_model::BlendMode::shader_index`:
    /// 0 Normal, 1 Multiply, 2 Screen, 3 Overlay, 4 Darken, 5 Lighten. Any
    /// other value falls back to Normal in the shader.
    pub blend_index: u32,
    /// Layer opacity in `0.0..=1.0`, applied to the source before blending.
    pub opacity: f32,
}

impl Default for CompositeParams {
    fn default() -> Self {
        Self {
            blend_index: 0,
            opacity: 1.0,
        }
    }
}

/// Composites a source layer over a destination into a third texture.
///
/// Inputs and output are premultiplied; the destination and source must be
/// distinct textures from the target, because the shader samples both while the
/// target is bound as a render attachment.
pub struct CompositePass {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    params_buf: wgpu::Buffer,
}

impl CompositePass {
    /// Build the composite pipeline for a given output texture format.
    pub fn new(gpu: &GpuContext, output_format: wgpu::TextureFormat) -> Self {
        let shader = gpu
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("composite-shader"),
                source: wgpu::ShaderSource::Wgsl(render_shaders::COMPOSITE_WGSL.into()),
            });

        let texture_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };

        let bind_group_layout =
            gpu.device
                .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("composite-bgl"),
                    entries: &[
                        texture_entry(0),
                        texture_entry(1),
                        wgpu::BindGroupLayoutEntry {
                            binding: 2,
                            visibility: wgpu::ShaderStages::FRAGMENT,
                            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                            count: None,
                        },
                        wgpu::BindGroupLayoutEntry {
                            binding: 3,
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

        let layout = gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("composite-pl"),
                bind_group_layouts: &[&bind_group_layout],
                push_constant_ranges: &[],
            });

        let pipeline = gpu
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("composite-pipeline"),
                layout: Some(&layout),
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
                        // The shader performs the whole composite itself, so the
                        // fixed-function blender must not touch the result.
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

        // Nearest: the pass is 1:1 with the target, so filtering would only
        // blur the layer against itself.
        let sampler = gpu.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("composite-sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let params_buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("composite-params"),
            size: std::mem::size_of::<ParamsUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            bind_group_layout,
            sampler,
            params_buf,
        }
    }

    /// Record `src` composited over `dst` into `target`.
    ///
    /// All three views must be the same size; `target` must not alias `dst` or
    /// `src`. The params write is queued immediately, so a single encoder may
    /// only hold one recorded composite per distinct `params` value.
    pub fn render(
        &self,
        gpu: &GpuContext,
        encoder: &mut wgpu::CommandEncoder,
        dst: &wgpu::TextureView,
        src: &wgpu::TextureView,
        target: &wgpu::TextureView,
        params: CompositeParams,
    ) {
        let uniform = ParamsUniform {
            blend_index: params.blend_index,
            opacity: params.opacity,
            _pad0: 0,
            _pad1: 0,
        };
        gpu.queue
            .write_buffer(&self.params_buf, 0, bytemuck::bytes_of(&uniform));

        let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("composite-bg"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(dst),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(src),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.params_buf.as_entire_binding(),
                },
            ],
        });

        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("composite-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.draw(0..6, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use super::CompositeParams;

    #[test]
    fn default_params_are_opaque_normal() {
        let p = CompositeParams::default();
        assert_eq!(p.blend_index, 0);
        assert_eq!(p.opacity, 1.0);
    }
}
