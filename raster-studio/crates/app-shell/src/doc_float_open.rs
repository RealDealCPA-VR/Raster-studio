//! W11-H: an OpenEXR or Radiance HDR opens as a **32 Bits/Channel** document.
//!
//! Both roads File > Open takes build the document from the 8-bit
//! [`crate::import::DecodedImage`] first (the synchronous
//! [`OpenDocument::open_image`] and the off-thread import job, which ends in
//! [`OpenDocument::open_image_decoded`]). For a float file,
//! [`promote_float_source`] then replaces that layer's pixels with the
//! file's own samples as W10-H's `f32` tiles (`crate::depth32`): straight
//! alpha, sRGB-encoded with the curve extended past 1.0 (the same encoding
//! Image > Mode > 32 Bits/Channel keeps), so a highlight brighter than
//! diffuse white is kept, not clipped. The document is marked 32-bit, the
//! replacement is not an undo step, and the document is unmodified, exactly
//! as any other freshly opened file.
//!
//! The off-thread job decoded the file to 8 bits; the float samples are read
//! again here, on the interaction thread, bounded by the same import limits.

use std::path::Path;

use editor_core::Command;

use crate::doc::{DocumentError, OpenDocument};

/// Largest float file read for the 32-bit open: the import job's own ceiling.
const MAX_FLOAT_FILE_BYTES: u64 = 2 << 30;

/// The float format `path` holds, by content: `Some` for an OpenEXR or a
/// Radiance HDR, `None` for anything else (or an unreadable file, which the
/// ordinary open has already reported).
pub(crate) fn float_format(path: &Path) -> Option<raster::ImportFormat> {
    use raster::codec::formats::float::{looks_like_exr, looks_like_hdr};
    use std::io::Read;
    let mut head = [0u8; 16];
    let mut file = std::fs::File::open(path).ok()?;
    let mut filled = 0;
    while filled < head.len() {
        match file.read(&mut head[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(_) => return None,
        }
    }
    let head = &head[..filled];
    if looks_like_exr(head) {
        Some(raster::ImportFormat::Exr)
    } else if looks_like_hdr(head) {
        Some(raster::ImportFormat::Hdr)
    } else {
        None
    }
}

/// Straight linear RGBA as the `f32` tile encoding of a 32-bit document:
/// the sRGB curve, extended symmetrically past `0..=1` (nothing clipped but
/// alpha); a NaN or infinite sample is 0.
pub(crate) fn linear_to_document_f32(linear: &[f32]) -> Vec<f32> {
    let finite = |v: f32| if v.is_finite() { v } else { 0.0 };
    let mut out = Vec::with_capacity(linear.len());
    for px in linear.as_chunks::<4>().0 {
        for c in &px[..3] {
            out.push(color::linear_to_srgb(finite(*c)));
        }
        out.push(finite(px[3]).clamp(0.0, 1.0));
    }
    out
}

/// Make `open` — a document just built from `path`'s 8-bit decode — a
/// 32 Bits/Channel document holding the float file's unclipped samples.
/// `Ok(false)` (and nothing changed) when `path` is not an EXR or HDR.
pub(crate) fn promote_float_source(
    open: &mut OpenDocument,
    path: &Path,
) -> Result<bool, DocumentError> {
    let Some(format) = float_format(path) else {
        return Ok(false);
    };
    let declared = std::fs::metadata(path)
        .map_err(crate::import::ImportError::from)?
        .len();
    if declared > MAX_FLOAT_FILE_BYTES {
        return Err(DocumentError::Encode(raster::CodecError::LimitExceeded(
            format!("this file is {declared} bytes, more than the {MAX_FLOAT_FILE_BYTES} read"),
        )));
    }
    let bytes = std::fs::read(path).map_err(crate::import::ImportError::from)?;
    // 16 bytes a pixel on top of the decode: the f32 document buffer.
    let (width, height, linear) = raster::codec::formats::float::decode_linear(
        format,
        &bytes,
        raster::ImportLimits::default(),
        16,
    )?;
    if (width, height) != (open.document.width(), open.document.height()) {
        return Err(DocumentError::Encode(raster::CodecError::BufferSize(
            format!(
                "{}: the float samples are {width}x{height}, the opened image {}x{}",
                format.name(),
                open.document.width(),
                open.document.height()
            ),
        )));
    }
    let Some(layer) = open
        .document
        .active_layer()
        .or_else(|| open.document.layers.iter_depth_first().first().copied())
    else {
        return Ok(false);
    };
    let samples = linear_to_document_f32(&linear);
    let paint = crate::depth32::layer_rgbaf32_command(open, layer, &samples, "Open")
        .map_err(|e| DocumentError::Encode(raster::CodecError::BufferSize(e)))?;
    let from = open.document.meta.bit_depth;
    let command = Command::Transaction {
        label: "Open as 32 Bits/Channel".to_string(),
        commands: vec![Command::SetMetaBitDepth { from, to: 32 }, paint],
    };
    open.history.apply(&mut open.document, command)?;
    // Opening is not an edit: nothing to undo past, nothing unsaved.
    open.history.clear();
    open.document.mark_saved();
    // The History Brush's "as opened" state is the float document.
    open.opened = std::sync::Arc::new(tools::history_brush::HistorySource::of(&open.document));
    Ok(true)
}
