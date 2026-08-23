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
    /// The largest 2D texture this **device** will create, as the device itself
    /// reports it — not [`wgpu::Limits::default`]'s 8192.
    ///
    /// Every texture size in the process is measured against this number. It is
    /// cached rather than read from `device.limits()` at each use so that the
    /// value a caller validated against and the value the device enforces are
    /// the same one.
    max_texture_dimension_2d: u32,
    /// Whatever the device last reported through the uncaptured-error handler.
    ///
    /// See [`GpuContext::take_last_error`]: wgpu's default handler is
    /// `panic!`, and this build aborts on panic, so leaving it in place makes
    /// every driver-side validation failure an immediate process death.
    last_error: Arc<Mutex<Option<String>>>,
    /// Lazily built mip pipelines, one per texture format. Keyed by format
    /// only, which is sound because the map belongs to a single device.
    mip_generators: Mutex<HashMap<wgpu::TextureFormat, Arc<MipGenerator>>>,
    /// How many entries [`GpuContext::mip_generator`] has had to build.
    mip_pipelines_built: AtomicU64,
}

impl GpuContext {
    /// The largest 2D texture this device will create, per side.
    ///
    /// Requested from the adapter rather than assumed: the WebGPU baseline
    /// [`wgpu::Limits::default`] promises only 8192, which a photograph from
    /// any current full-frame camera already exceeds (a Nikon Z8 JPEG is
    /// 8256x5504), while desktop hardware almost always allows 16384.
    /// **Nothing in this process may hand the GPU a texture wider or taller
    /// than this** — see [`crate::GpuTexture::from_rgba8_with`], which refuses
    /// rather than letting the driver raise an error the runtime turns into an
    /// abort.
    pub fn max_texture_dimension_2d(&self) -> u32 {
        self.max_texture_dimension_2d
    }

    /// Take the last error the device reported out of band, if any.
    ///
    /// wgpu calls the uncaptured-error handler for validation and out-of-memory
    /// failures that no `Result` carries — creating an oversized texture, a
    /// pipeline whose layout does not match, a buffer bigger than the device
    /// allows. The default handler panics; the release profile sets
    /// `panic = "abort"`, so the default handler is a process kill with no
    /// unwind, no dialog and no autosave flush. This context installs a handler
    /// that logs and records instead, which is what makes those failures
    /// something the shell can survive and report.
    pub fn take_last_error(&self) -> Option<String> {
        self.last_error
            .lock()
            .map(|mut slot| slot.take())
            .unwrap_or(None)
    }

    /// Peek at the last device error without clearing it.
    pub fn last_error(&self) -> Option<String> {
        self.last_error
            .lock()
            .map(|slot| slot.clone())
            .unwrap_or(None)
    }

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

        // Ask for the texture size the adapter actually offers instead of
        // taking the WebGPU baseline's 8192. Taken from the adapter rather than
        // hard-coded to 16384 because a device that offers less than the
        // baseline exists (some mobile and software backends), and asking for
        // more than is on offer fails the whole `request_device`.
        let required_limits = wgpu::Limits {
            max_texture_dimension_2d: adapter.limits().max_texture_dimension_2d,
            ..wgpu::Limits::default()
        };

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label: Some("raster-studio-device"),
                    required_features: wgpu::Features::empty(),
                    required_limits,
                    memory_hints: wgpu::MemoryHints::Performance,
                },
                None,
            )
            .await
            .context("failed to create GPU device")?;

        // Before anything is created on it: wgpu's default uncaptured-error
        // handler panics, and this build aborts on panic, so an oversized
        // texture or a mismatched pipeline would kill the process outright —
        // taking every other open document's unsaved work with it. Report
        // instead.
        let last_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let sink = Arc::clone(&last_error);
        device.on_uncaptured_error(Box::new(move |err| {
            let message = err.to_string();
            tracing::error!("wgpu device error: {message}");
            if let Ok(mut slot) = sink.lock() {
                *slot = Some(message);
            }
        }));

        let max_texture_dimension_2d = device.limits().max_texture_dimension_2d;

        Ok(Self {
            instance,
            adapter,
            device,
            queue,
            max_texture_dimension_2d,
            last_error,
            mip_generators: Mutex::new(HashMap::new()),
            mip_pipelines_built: AtomicU64::new(0),
        })
    }

    /// Convenience: build a headless context, blocking the current thread.
    pub fn headless_blocking() -> Result<Arc<Self>> {
        Ok(Arc::new(pollster::block_on(Self::headless())?))
    }
}
