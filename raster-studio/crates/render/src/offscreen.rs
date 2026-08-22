//! Offscreen render targets with CPU readback.
//!
//! This is the seam that lets anything render without a window: golden-image
//! tests, thumbnailing, and (later) file export all render into an
//! [`OffscreenTarget`] and pull the result back with
//! [`OffscreenTarget::read_rgba8`].

use anyhow::{bail, Context, Result};

use crate::context::GpuContext;

/// A CPU-side RGBA8 image read back from the GPU.
///
/// Rows are tightly packed (`width * 4` bytes, no padding) and channel order is
/// always R, G, B, A regardless of the target's native format. Values are in
/// whatever encoding the target used: an `*-Srgb` target yields sRGB-encoded
/// bytes, a plain `*-Unorm` target yields the raw linear values written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Readback {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Readback {
    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// Tightly packed RGBA8 bytes, `width * height * 4` long.
    pub fn as_rgba8(&self) -> &[u8] {
        &self.pixels
    }

    /// Consume the readback, yielding its tightly packed RGBA8 bytes.
    pub fn into_rgba8(self) -> Vec<u8> {
        self.pixels
    }

    /// The pixel at `(x, y)`, with `(0, 0)` the TOP-LEFT of the target.
    ///
    /// # Panics
    /// If `x >= width` or `y >= height`.
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        assert!(
            x < self.width && y < self.height,
            "pixel ({x}, {y}) out of bounds for {}x{}",
            self.width,
            self.height
        );
        let i = ((y as usize) * (self.width as usize) + x as usize) * 4;
        [
            self.pixels[i],
            self.pixels[i + 1],
            self.pixels[i + 2],
            self.pixels[i + 3],
        ]
    }
}

/// A color texture that can be rendered into and copied back to the CPU.
///
/// The texture carries `RENDER_ATTACHMENT | COPY_SRC | TEXTURE_BINDING`, so it
/// can also be fed straight back into a later pass as a source.
pub struct OffscreenTarget {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
    format: wgpu::TextureFormat,
}

impl OffscreenTarget {
    /// Allocate a `width` x `height` target in `format`.
    ///
    /// `format` may be anything renderable, but [`Self::read_rgba8`] only
    /// supports the 8-bit RGBA/BGRA formats. Both dimensions must be non-zero.
    pub fn new(
        gpu: &GpuContext,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    ) -> Result<Self> {
        if width == 0 || height == 0 {
            bail!("offscreen target must be non-empty, got {width}x{height}");
        }
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen-target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Ok(Self {
            texture,
            view,
            width,
            height,
            format,
        })
    }

    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn format(&self) -> wgpu::TextureFormat {
        self.format
    }

    /// Copy the target back to CPU memory as tightly packed RGBA8.
    ///
    /// Blocks until the GPU has finished every command submitted so far. BGRA
    /// targets are swizzled to RGBA on the way out; any other format is an
    /// error rather than a silent misinterpretation of the bytes.
    pub fn read_rgba8(&self, gpu: &GpuContext) -> Result<Readback> {
        read_texture_rgba8(gpu, &self.texture, 0)
    }
}

/// Copy one mip level of `texture` back to CPU memory as tightly packed RGBA8.
///
/// `texture` must carry [`wgpu::TextureUsages::COPY_SRC`] and `mip_level` must
/// be within its chain. Blocks until every command submitted so far has retired,
/// so the caller does not have to fence. BGRA is swizzled to RGBA; any other
/// format is an error rather than a silent misinterpretation of the bytes.
///
/// This is the seam behind [`OffscreenTarget::read_rgba8`] and
/// [`crate::GpuTexture::read_level`]; it is public so export and golden-image
/// code can read back a texture it did not allocate through `OffscreenTarget`.
pub fn read_texture_rgba8(
    gpu: &GpuContext,
    texture: &wgpu::Texture,
    mip_level: u32,
) -> Result<Readback> {
    let swizzle_bgra = match texture.format() {
        wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Rgba8UnormSrgb => false,
        wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb => true,
        other => bail!("cannot read back {other:?}: only 8-bit RGBA/BGRA is supported"),
    };
    if mip_level >= texture.mip_level_count() {
        bail!(
            "mip level {mip_level} is out of range for a {}-level texture",
            texture.mip_level_count()
        );
    }

    let width = (texture.width() >> mip_level).max(1);
    let height = (texture.height() >> mip_level).max(1);

    let unpadded_bytes_per_row = width * 4;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(align) * align;

    let staging = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("offscreen-readback"),
        size: u64::from(padded_bytes_per_row) * u64::from(height),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });

    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("offscreen-readback-encoder"),
        });
    encoder.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture,
            mip_level,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyBuffer {
            buffer: &staging,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    gpu.queue.submit(Some(encoder.finish()));

    let slice = staging.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |res| {
        // The receiver is alive until the poll below returns; a send error can
        // only mean the caller was torn down, and there is nothing useful to do
        // about it here.
        let _ = tx.send(res);
    });
    gpu.device.poll(wgpu::Maintain::Wait);
    rx.recv()
        .context("readback callback never fired")?
        .context("failed to map readback buffer")?;

    let mut pixels = Vec::with_capacity((unpadded_bytes_per_row * height) as usize);
    {
        let mapped = slice.get_mapped_range();
        for row in 0..height as usize {
            let start = row * padded_bytes_per_row as usize;
            let end = start + unpadded_bytes_per_row as usize;
            pixels.extend_from_slice(&mapped[start..end]);
        }
    }
    staging.unmap();

    if swizzle_bgra {
        for px in pixels.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
    }

    Ok(Readback {
        width,
        height,
        pixels,
    })
}

#[cfg(test)]
mod tests {
    use super::Readback;

    fn sample() -> Readback {
        // 2x2, distinct per-pixel red channel so indexing errors are visible.
        Readback {
            width: 2,
            height: 2,
            pixels: vec![
                10, 0, 0, 255, // (0,0)
                20, 0, 0, 255, // (1,0)
                30, 0, 0, 255, // (0,1)
                40, 0, 0, 255, // (1,1)
            ],
        }
    }

    #[test]
    fn pixel_indexes_row_major_from_top_left() {
        let r = sample();
        assert_eq!(r.pixel(0, 0)[0], 10);
        assert_eq!(r.pixel(1, 0)[0], 20);
        assert_eq!(r.pixel(0, 1)[0], 30);
        assert_eq!(r.pixel(1, 1)[0], 40);
    }

    #[test]
    #[should_panic(expected = "out of bounds")]
    fn pixel_out_of_bounds_panics() {
        let _ = sample().pixel(2, 0);
    }
}
