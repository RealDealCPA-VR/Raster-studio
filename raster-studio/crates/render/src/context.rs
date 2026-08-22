//! GPU device/queue setup and per-surface configuration.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};

use crate::texture::MipGenerator;

/// Owns the wgpu instance, adapter, device and queue. One per application.
pub struct GpuContext {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    /// Lazily built mip pipelines, one per texture format. Keyed by format
    /// only, which is sound because the map belongs to a single device.
    mip_generators: Mutex<HashMap<wgpu::TextureFormat, Arc<MipGenerator>>>,
    /// How many entries [`GpuContext::mip_generator`] has had to build.
    mip_pipelines_built: AtomicU64,
}

impl GpuContext {
    /// The [`MipGenerator`] for `format`, built once per context and reused.
    ///
    /// Constructing one compiles `mipmap.wgsl` and creates a render pipeline, so
    /// image upload must go through this rather than [`MipGenerator::new`].
    ///
    /// # Panics
    /// If another thread panicked while holding the cache lock.
    pub fn mip_generator(&self, format: wgpu::TextureFormat) -> Arc<MipGenerator> {
        let mut cache = self
            .mip_generators
            .lock()
            .expect("mip generator cache poisoned");
        Arc::clone(cache.entry(format).or_insert_with(|| {
            self.mip_pipelines_built.fetch_add(1, Ordering::Relaxed);
            Arc::new(MipGenerator::new(self, format))
        }))
    }

    /// How many mip pipelines this context has compiled.
    ///
    /// One per texture format ever mipmapped, for the life of the context. It
    /// must NOT grow with the number of images opened — that would mean an
    /// upload path is building its own [`MipGenerator`] instead of asking
    /// [`GpuContext::mip_generator`], and paying a WGSL compile per image.
    pub fn mip_pipelines_built(&self) -> u64 {
        self.mip_pipelines_built.load(Ordering::Relaxed)
    }

    /// Create a headless context (no surface). Used by golden-image tests and
    /// as the base for a windowed context.
    ///
    /// Falls back to a software adapter (WARP on Windows, lavapipe on Linux)
    /// when no hardware adapter is available, so CI machines without a GPU
    /// still get a working device. Returns `Err` only when *no* adapter at all
    /// can be found — callers that must degrade gracefully should treat that as
    /// "skip", not as a failure.
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
        let mut adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface,
                force_fallback_adapter: false,
            })
            .await;
        if adapter.is_none() {
            adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::LowPower,
                    compatible_surface,
                    force_fallback_adapter: true,
                })
                .await;
        }
        let adapter = adapter.context("no suitable GPU adapter found")?;

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
            mip_generators: Mutex::new(HashMap::new()),
            mip_pipelines_built: AtomicU64::new(0),
        })
    }

    /// Convenience: build a headless context, blocking the current thread.
    pub fn headless_blocking() -> Result<Arc<Self>> {
        Ok(Arc::new(pollster::block_on(Self::headless())?))
    }
}
