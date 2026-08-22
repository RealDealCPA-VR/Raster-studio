//! GPU device/queue setup and per-surface configuration.

use std::sync::Arc;

use anyhow::{Context, Result};

/// Owns the wgpu instance, adapter, device and queue. One per application.
pub struct GpuContext {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

impl GpuContext {
    /// Create a headless context (no surface). Used by golden-image tests and
    /// as the base for a windowed context.
    pub async fn headless() -> Result<Self> {
        let instance = wgpu::Instance::default();
        Self::from_instance(instance, None).await
    }

    /// Create a context suitable for rendering to `surface`.
    pub async fn for_surface(
        instance: wgpu::Instance,
        surface: &wgpu::Surface<'_>,
    ) -> Result<Self> {
        Self::from_instance(instance, Some(surface)).await
    }

    async fn from_instance(
        instance: wgpu::Instance,
        compatible_surface: Option<&wgpu::Surface<'_>>,
    ) -> Result<Self> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface,
                force_fallback_adapter: false,
            })
            .await
            .context("no suitable GPU adapter found")?;

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("raster-studio-device"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                    memory_hints: wgpu::MemoryHints::Performance,
                },
                None,
            )
            .await
            .context("failed to create GPU device")?;

        Ok(Self {
            instance,
            adapter,
            device,
            queue,
        })
    }

    /// Convenience: build a headless context, blocking the current thread.
    pub fn headless_blocking() -> Result<Arc<Self>> {
        Ok(Arc::new(pollster::block_on(Self::headless())?))
    }
}
