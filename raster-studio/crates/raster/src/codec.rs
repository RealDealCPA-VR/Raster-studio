//! Codec abstraction. All decode/encode goes through this facade so the rest
//! of the app never depends on a specific codec crate — swap or add formats
//! (TIFF, WebP, PSD later) behind this boundary.

use std::path::Path;

/// A decoded image in packed RGBA8, plus dimensions.
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    /// Row-major RGBA8, `width * height * 4` bytes.
    pub rgba8: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    #[error("image decode/encode failed: {0}")]
    Image(#[from] image::ImageError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("unsupported format for path: {0}")]
    Unsupported(String),
}

/// Decode any supported raster file into RGBA8.
pub fn decode_path(path: &Path) -> Result<DecodedImage, CodecError> {
    let img = image::open(path)?.to_rgba8();
    let (width, height) = img.dimensions();
    Ok(DecodedImage {
        width,
        height,
        rgba8: img.into_raw(),
    })
}

/// Decode from an in-memory buffer (used for embedded assets).
pub fn decode_bytes(bytes: &[u8]) -> Result<DecodedImage, CodecError> {
    let img = image::load_from_memory(bytes)?.to_rgba8();
    let (width, height) = img.dimensions();
    Ok(DecodedImage {
        width,
        height,
        rgba8: img.into_raw(),
    })
}

/// Output format selector for encoding.
#[derive(Debug, Clone, Copy)]
pub enum ExportFormat {
    Png,
    /// JPEG at the given quality (1..=100).
    Jpeg(u8),
    WebP,
}

/// Encode RGBA8 pixels to the requested format, returning the file bytes.
pub fn encode(
    format: ExportFormat,
    width: u32,
    height: u32,
    rgba8: &[u8],
) -> Result<Vec<u8>, CodecError> {
    use image::{ImageBuffer, Rgba};
    let buf: ImageBuffer<Rgba<u8>, _> = ImageBuffer::from_raw(width, height, rgba8.to_vec())
        .ok_or_else(|| CodecError::Unsupported("buffer size mismatch".into()))?;

    let mut out = std::io::Cursor::new(Vec::new());
    match format {
        ExportFormat::Png => {
            buf.write_to(&mut out, image::ImageFormat::Png)?;
        }
        ExportFormat::Jpeg(q) => {
            // JPEG has no alpha; flatten onto the RGB channels.
            let rgb = image::DynamicImage::ImageRgba8(buf).to_rgb8();
            let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, q);
            enc.encode_image(&rgb)?;
        }
        ExportFormat::WebP => {
            buf.write_to(&mut out, image::ImageFormat::WebP)?;
        }
    }
    Ok(out.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_roundtrip() {
        let (w, h) = (2, 2);
        let px = vec![
            255u8, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ];
        let bytes = encode(ExportFormat::Png, w, h, &px).unwrap();
        let decoded = decode_bytes(&bytes).unwrap();
        assert_eq!((decoded.width, decoded.height), (w, h));
        assert_eq!(decoded.rgba8, px);
    }
}
