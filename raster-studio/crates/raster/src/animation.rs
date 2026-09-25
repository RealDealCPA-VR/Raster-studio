//! Animated images as frame sequences: animated GIF, APNG and animated WebP.
//!
//! # The convention (Photopea's)
//!
//! An animation is a stack of layers whose names start with
//! [`FRAME_LAYER_PREFIX`] (`_a_`), each carrying its own delay after a comma:
//! `_a_Frame 1,100` is a frame shown for 100 ms. Opening an animated file
//! turns every frame into one such layer; exporting a document that has them
//! writes one frame per `_a_` layer, in stacking order from the bottom up,
//! with every *other* layer as the document has it (shown or hidden in every
//! frame alike). This module owns the two
//! ends of that convention that do not need a document: decoding a file into
//! full-canvas frames and encoding full-canvas frames back into a file. The
//! layer side lives in `app-shell`'s importer.
//!
//! # Decoding
//!
//! [`decode_animation_bytes`] returns fully *composited* frames — every frame
//! the size of the canvas, with GIF disposal (keep / restore-to-background /
//! restore-to-previous), frame offsets and APNG / WebP blending already
//! applied — because a layer is a whole picture, not a patch meant to be
//! drawn over the previous one. The compositing is the backing `image`
//! decoders' own, which is what their `AnimationDecoder` iterators hand back.
//!
//! A file that is not animated (a still PNG, a one-frame GIF, a WebP without
//! an `ANIM` chunk) is `Ok(None)`: the flat decode path in [`crate::codec`]
//! owns it and nothing here changes what it produces.
//!
//! # Untrusted input
//!
//! Every frame is a full canvas of RGBA8, so the frame *count* multiplies the
//! canvas allocation. Both are bounded before they are paid for: the canvas by
//! [`ImportLimits`], the count by [`MAX_ANIMATION_FRAMES`], and the product by
//! [`MAX_ANIMATION_BYTES`] (and by [`ImportLimits::max_alloc_bytes`], if that
//! is smaller). A file past any of them is a [`CodecError::LimitExceeded`],
//! reported at the frame that would cross the line, before its buffer is kept.
//!
//! # Encoding
//!
//! [`encode_animation`] writes an infinitely looping animation:
//! * **GIF** through `image`'s GIF encoder (each frame palettised on its own;
//!   a frame with at most 256 colours keeps them exactly).
//! * **APNG**, written chunk by chunk here (`acTL` / `fcTL` / `fdAT`), because
//!   `image`'s PNG encoder writes a single image. Lossless RGBA8.
//! * **Animated WebP**, written chunk by chunk here (`VP8X` / `ANIM` /
//!   `ANMF`), with each frame's bitstream coming from `image`'s lossless
//!   WebP encoder — the same encoder a still `.webp` export uses.

use std::fs::File;
use std::io::{BufRead, BufReader, Cursor, Seek, Write};
use std::path::Path;

use color::ColorSpace;
use image::{AnimationDecoder, ExtendedColorType, ImageDecoder, ImageEncoder};

use crate::codec::{icc_profile_space, CodecError, ExportFormat, ImportFormat, ImportLimits};

/// The layer-name prefix that marks an animation frame (Photopea's `_a_`).
pub const FRAME_LAYER_PREFIX: &str = "_a_";

/// The delay a frame layer gets when its name carries none.
pub const DEFAULT_FRAME_DELAY_MS: u32 = 100;

/// Most frames an animated file may decode into. Each one becomes a layer.
pub const MAX_ANIMATION_FRAMES: usize = 1000;

/// Most bytes of decoded frames an animation may hold (every frame is a full
/// RGBA8 canvas). One gibibyte: 1000 frames of 512 x 512, or 64 of 2K.
pub const MAX_ANIMATION_BYTES: u64 = 1 << 30;

/// One full-canvas frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnimationFrame {
    /// Row-major RGBA8, straight alpha, the size of the canvas.
    pub rgba8: Vec<u8>,
    /// How long the frame is shown, in milliseconds.
    pub delay_ms: u32,
}

/// An animated file decoded into composited frames.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedAnimation {
    pub width: u32,
    pub height: u32,
    /// At least two frames: one frame is a still and is not returned here.
    pub frames: Vec<AnimationFrame>,
    /// The container the frames came from.
    pub format: ImportFormat,
    /// The embedded ICC profile (APNG `iCCP`, WebP `ICCP`), if any.
    pub icc_profile: Option<Vec<u8>>,
    /// The colour space the frames are encoded in.
    pub color_space: ColorSpace,
}

/// The layer name for a frame: `_a_<label>,<delay ms>`.
pub fn frame_layer_name(label: &str, delay_ms: u32) -> String {
    format!("{FRAME_LAYER_PREFIX}{label},{delay_ms}")
}

/// Read a frame layer's name back: `Some((label, delay_ms))` for an `_a_`
/// layer, `None` for any other. A name without a readable delay after its
/// last comma gets [`DEFAULT_FRAME_DELAY_MS`].
pub fn parse_frame_layer_name(name: &str) -> Option<(&str, u32)> {
    let rest = name.strip_prefix(FRAME_LAYER_PREFIX)?;
    if let Some((label, delay)) = rest.rsplit_once(',') {
        if let Ok(ms) = delay.trim().parse::<u32>() {
            return Some((label, ms));
        }
    }
    Some((rest, DEFAULT_FRAME_DELAY_MS))
}

/// Whether [`encode_animation`] can write `format` as an animation.
pub fn can_animate(format: ExportFormat) -> bool {
    matches!(
        format,
        ExportFormat::Gif
            | ExportFormat::Png
            | ExportFormat::WebP
            | ExportFormat::Mp4(_)
            | ExportFormat::Mp4Av1(_)
    )
}

fn image_limits(limits: ImportLimits) -> image::Limits {
    let mut out = image::Limits::no_limits();
    out.max_image_width = Some(limits.max_width);
    out.max_image_height = Some(limits.max_height);
    out.max_alloc = Some(limits.max_alloc_bytes);
    out
}

fn check_canvas(limits: ImportLimits, width: u32, height: u32) -> Result<(), CodecError> {
    if width == 0 || height == 0 {
        return Err(CodecError::LimitExceeded(format!(
            "animation declares an empty canvas: {width}x{height}"
        )));
    }
    if width > limits.max_width || height > limits.max_height {
        return Err(CodecError::LimitExceeded(format!(
            "animation is {width}x{height}, limit is {}x{}",
            limits.max_width, limits.max_height
        )));
    }
    let pixels = u64::from(width) * u64::from(height);
    if pixels > limits.max_pixels {
        return Err(CodecError::LimitExceeded(format!(
            "animation has {pixels} pixels per frame, limit is {}",
            limits.max_pixels
        )));
    }
    Ok(())
}

/// Decode an animated GIF, APNG or animated WebP into composited frames.
///
/// `Ok(None)` for anything that is not an animation of at least two frames —
/// a still image of any format, a one-frame GIF — so the caller's flat decode
/// stays the one road for stills.
pub fn decode_animation_bytes(
    bytes: &[u8],
    limits: ImportLimits,
) -> Result<Option<DecodedAnimation>, CodecError> {
    decode_animation_reader(Cursor::new(bytes), limits)
}

/// [`decode_animation_bytes`] over a file, streamed: a still PNG, WebP or any
/// other still is recognised from its header without reading its pixels.
pub fn decode_animation_path(
    path: &Path,
    limits: ImportLimits,
) -> Result<Option<DecodedAnimation>, CodecError> {
    decode_animation_reader(BufReader::new(File::open(path)?), limits)
}

/// [`decode_animation_bytes`] over any seekable stream. The container is
/// identified from its content, never from a file name.
pub fn decode_animation_reader<R: BufRead + Seek>(
    source: R,
    limits: ImportLimits,
) -> Result<Option<DecodedAnimation>, CodecError> {
    let reader = image::ImageReader::new(source).with_guessed_format()?;
    let Some(format) = reader.format() else {
        return Ok(None);
    };
    let source = reader.into_inner();
    let (width, height, frames, format, icc_profile) = match format {
        image::ImageFormat::Gif => {
            let mut decoder = image::codecs::gif::GifDecoder::new(source)?;
            decoder.set_limits(image_limits(limits))?;
            let (w, h) = decoder.dimensions();
            check_canvas(limits, w, h)?;
            let frames = collect_frames(decoder.into_frames(), w, h, limits)?;
            (w, h, frames, ImportFormat::Gif, None)
        }
        image::ImageFormat::Png => {
            let mut decoder =
                image::codecs::png::PngDecoder::with_limits(source, image_limits(limits))?;
            if !decoder.is_apng()? {
                return Ok(None);
            }
            let (w, h) = decoder.dimensions();
            check_canvas(limits, w, h)?;
            let icc = decoder.icc_profile()?;
            let frames = collect_frames(decoder.apng()?.into_frames(), w, h, limits)?;
            (w, h, frames, ImportFormat::Png, icc)
        }
        image::ImageFormat::WebP => {
            let mut decoder = image::codecs::webp::WebPDecoder::new(source)?;
            if !decoder.has_animation() {
                return Ok(None);
            }
            decoder.set_limits(image_limits(limits))?;
            let (w, h) = decoder.dimensions();
            check_canvas(limits, w, h)?;
            let icc = decoder.icc_profile()?;
            let frames = collect_frames(decoder.into_frames(), w, h, limits)?;
            (w, h, frames, ImportFormat::WebP, icc)
        }
        _ => return Ok(None),
    };
    if frames.len() < 2 {
        return Ok(None);
    }
    let icc_profile = icc_profile.filter(|p| !p.is_empty() && p.len() <= limits.max_icc_bytes);
    let color_space = match &icc_profile {
        Some(profile) => icc_profile_space(profile),
        None => ColorSpace::Srgb,
    };
    Ok(Some(DecodedAnimation {
        width,
        height,
        frames,
        format,
        icc_profile,
        color_space,
    }))
}

/// Pull every frame, bounding the count and the running total, and place any
/// frame the decoder did not already composite onto a full canvas.
fn collect_frames(
    frames: image::Frames<'_>,
    width: u32,
    height: u32,
    limits: ImportLimits,
) -> Result<Vec<AnimationFrame>, CodecError> {
    let frame_bytes = u64::from(width) * u64::from(height) * 4;
    let budget = MAX_ANIMATION_BYTES.min(limits.max_alloc_bytes);
    let mut out: Vec<AnimationFrame> = Vec::new();
    for frame in frames {
        let frame = frame?;
        if out.len() >= MAX_ANIMATION_FRAMES {
            return Err(CodecError::LimitExceeded(format!(
                "animation has more than {MAX_ANIMATION_FRAMES} frames"
            )));
        }
        let total = (out.len() as u64 + 1).saturating_mul(frame_bytes);
        if total > budget {
            return Err(CodecError::LimitExceeded(format!(
                "animation frames need more than {budget} bytes"
            )));
        }
        let (numer, denom) = frame.delay().numer_denom_ms();
        // Rounded to the nearest millisecond; a zero denominator reads as 0.
        let delay_ms = numer
            .saturating_add(denom / 2)
            .checked_div(denom)
            .unwrap_or(0);
        let (left, top) = (frame.left(), frame.top());
        let buffer = frame.into_buffer();
        let rgba8 = if (left, top) == (0, 0) && buffer.dimensions() == (width, height) {
            buffer.into_raw()
        } else {
            place_on_canvas(&buffer, left, top, width, height)
        };
        out.push(AnimationFrame { rgba8, delay_ms });
    }
    Ok(out)
}

/// Copy a sub-frame onto a transparent canvas at `(left, top)`, clipped.
fn place_on_canvas(
    buffer: &image::RgbaImage,
    left: u32,
    top: u32,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let mut canvas = vec![0u8; width as usize * height as usize * 4];
    for (x, y, px) in buffer.enumerate_pixels() {
        let (cx, cy) = (
            u64::from(left) + u64::from(x),
            u64::from(top) + u64::from(y),
        );
        if cx >= u64::from(width) || cy >= u64::from(height) {
            continue;
        }
        let i = (cy as usize * width as usize + cx as usize) * 4;
        canvas[i..i + 4].copy_from_slice(&px.0);
    }
    canvas
}

/// Encode full-canvas frames as a looping animated GIF, APNG or WebP.
///
/// Refuses any other format, an empty frame list, more than
/// [`MAX_ANIMATION_FRAMES`] frames, and a frame whose buffer is not
/// `width * height * 4` bytes.
pub fn encode_animation(
    format: ExportFormat,
    width: u32,
    height: u32,
    frames: &[AnimationFrame],
) -> Result<Vec<u8>, CodecError> {
    if !can_animate(format) {
        return Err(CodecError::InvalidParameter(format!(
            "{} cannot hold an animation",
            format.extension()
        )));
    }
    if frames.is_empty() {
        return Err(CodecError::InvalidParameter(
            "an animation needs at least one frame".into(),
        ));
    }
    if frames.len() > MAX_ANIMATION_FRAMES {
        return Err(CodecError::LimitExceeded(format!(
            "{} frames, limit is {MAX_ANIMATION_FRAMES}",
            frames.len()
        )));
    }
    if width == 0 || height == 0 {
        return Err(CodecError::BufferSize(format!(
            "cannot encode a {width}x{height} animation"
        )));
    }
    let expected = u64::from(width) * u64::from(height) * 4;
    for (i, frame) in frames.iter().enumerate() {
        if frame.rgba8.len() as u64 != expected {
            return Err(CodecError::BufferSize(format!(
                "frame {} holds {} bytes, a {width}x{height} frame needs {expected}",
                i + 1,
                frame.rgba8.len()
            )));
        }
    }
    match format {
        ExportFormat::Gif => encode_gif(width, height, frames),
        ExportFormat::Png => encode_apng(width, height, frames),
        // W13-L: MP4, every frame with its own duration; W15-B: H.264 by
        // default, AV1 when that codec was chosen.
        ExportFormat::Mp4(quality) | ExportFormat::Mp4Av1(quality) => {
            use crate::codec::formats::mp4::{encode_with, Mp4Codec, Mp4Frame};
            let frames: Vec<Mp4Frame<'_>> = frames
                .iter()
                .map(|f| Mp4Frame {
                    rgba8: &f.rgba8,
                    duration_ms: f.delay_ms,
                })
                .collect();
            let codec = if matches!(format, ExportFormat::Mp4Av1(_)) {
                Mp4Codec::Av1
            } else {
                Mp4Codec::H264
            };
            encode_with(width, height, &frames, quality, codec)
        }
        _ => encode_webp(width, height, frames),
    }
}

fn encode_gif(width: u32, height: u32, frames: &[AnimationFrame]) -> Result<Vec<u8>, CodecError> {
    let mut out = Vec::new();
    {
        // Scoped: the GIF encoder writes the trailer when dropped.
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut out);
        encoder.set_repeat(image::codecs::gif::Repeat::Infinite)?;
        for frame in frames {
            let buffer = image::RgbaImage::from_raw(width, height, frame.rgba8.clone())
                .ok_or_else(|| CodecError::BufferSize("frame buffer size".into()))?;
            encoder.encode_frame(image::Frame::from_parts(
                buffer,
                0,
                0,
                image::Delay::from_numer_denom_ms(frame.delay_ms, 1),
            ))?;
        }
    }
    Ok(out)
}

/// CRC-32 (IEEE, reflected), as PNG chunks carry it.
fn crc32(parts: &[&[u8]]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for part in parts {
        for &byte in *part {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
    }
    !crc
}

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) -> Result<(), CodecError> {
    let len = u32::try_from(data.len())
        .map_err(|_| CodecError::BufferSize("a PNG chunk cannot exceed 4 GiB".into()))?;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32(&[kind, data]).to_be_bytes());
    Ok(())
}

/// A frame's RGBA8 rows, each behind a filter-type byte (None), deflated.
fn png_image_data(width: u32, frame: &[u8]) -> Result<Vec<u8>, CodecError> {
    let row = width as usize * 4;
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    for line in frame.chunks_exact(row) {
        encoder.write_all(&[0])?;
        encoder.write_all(line)?;
    }
    Ok(encoder.finish()?)
}

/// An APNG frame delay as the `fcTL` fraction: milliseconds over 1000 when it
/// fits sixteen bits, hundredths over 100 past that (saturating).
fn apng_delay(ms: u32) -> (u16, u16) {
    match u16::try_from(ms) {
        Ok(ms) => (ms, 1000),
        Err(_) => (u16::try_from(ms / 10).unwrap_or(u16::MAX), 100),
    }
}

fn encode_apng(width: u32, height: u32, frames: &[AnimationFrame]) -> Result<Vec<u8>, CodecError> {
    let mut out = Vec::new();
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    // 8 bits, colour type 6 (RGBA), deflate, adaptive filtering, no interlace.
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    png_chunk(&mut out, b"IHDR", &ihdr)?;
    let mut actl = Vec::with_capacity(8);
    actl.extend_from_slice(&(frames.len() as u32).to_be_bytes());
    actl.extend_from_slice(&0u32.to_be_bytes()); // loop forever
    png_chunk(&mut out, b"acTL", &actl)?;
    let mut sequence = 0u32;
    for (i, frame) in frames.iter().enumerate() {
        let (num, den) = apng_delay(frame.delay_ms);
        let mut fctl = Vec::with_capacity(26);
        fctl.extend_from_slice(&sequence.to_be_bytes());
        fctl.extend_from_slice(&width.to_be_bytes());
        fctl.extend_from_slice(&height.to_be_bytes());
        fctl.extend_from_slice(&0u32.to_be_bytes());
        fctl.extend_from_slice(&0u32.to_be_bytes());
        fctl.extend_from_slice(&num.to_be_bytes());
        fctl.extend_from_slice(&den.to_be_bytes());
        // dispose_op NONE, blend_op SOURCE: every frame is a whole canvas.
        fctl.extend_from_slice(&[0, 0]);
        png_chunk(&mut out, b"fcTL", &fctl)?;
        sequence += 1;
        let data = png_image_data(width, &frame.rgba8)?;
        if i == 0 {
            // The first frame is also the default image, so a viewer that
            // does not know APNG shows it.
            png_chunk(&mut out, b"IDAT", &data)?;
        } else {
            let mut fdat = Vec::with_capacity(data.len() + 4);
            fdat.extend_from_slice(&sequence.to_be_bytes());
            fdat.extend_from_slice(&data);
            png_chunk(&mut out, b"fdAT", &fdat)?;
            sequence += 1;
        }
    }
    png_chunk(&mut out, b"IEND", &[])?;
    Ok(out)
}

fn push_u24(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes()[..3]);
}

fn riff_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) -> Result<(), CodecError> {
    let len = u32::try_from(data.len())
        .map_err(|_| CodecError::BufferSize("a WebP chunk cannot exceed 4 GiB".into()))?;
    out.extend_from_slice(kind);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(data);
    if data.len() % 2 == 1 {
        out.push(0);
    }
    Ok(())
}

/// The whole `VP8L` chunk (header, payload, padding) of a still lossless
/// WebP written by `image`'s encoder.
fn vp8l_chunk(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, CodecError> {
    let mut still = Vec::new();
    image::codecs::webp::WebPEncoder::new_lossless(&mut still).write_image(
        rgba,
        width,
        height,
        ExtendedColorType::Rgba8,
    )?;
    let malformed = || CodecError::Unsupported("the WebP encoder wrote no VP8L chunk".into());
    if still.len() < 12 || &still[0..4] != b"RIFF" || &still[8..12] != b"WEBP" {
        return Err(malformed());
    }
    let mut at = 12usize;
    while at + 8 <= still.len() {
        let kind = &still[at..at + 4];
        let len = u32::from_le_bytes(still[at + 4..at + 8].try_into().expect("four bytes"));
        let padded = (len as usize).saturating_add(len as usize & 1);
        let end = (at + 8).saturating_add(padded).min(still.len());
        if kind == b"VP8L" {
            return Ok(still[at..end].to_vec());
        }
        at = end;
    }
    Err(malformed())
}

fn encode_webp(width: u32, height: u32, frames: &[AnimationFrame]) -> Result<Vec<u8>, CodecError> {
    const MAX_SIDE: u32 = 1 << 14;
    if width > MAX_SIDE || height > MAX_SIDE {
        return Err(CodecError::InvalidParameter(format!(
            "an animated WebP is at most {MAX_SIDE} px a side, this is {width}x{height}"
        )));
    }
    let mut body = Vec::new();
    body.extend_from_slice(b"WEBP");
    let mut vp8x = Vec::with_capacity(10);
    // Flags: alpha (0x10) and animation (0x02).
    vp8x.extend_from_slice(&[0x12, 0, 0, 0]);
    push_u24(&mut vp8x, width - 1);
    push_u24(&mut vp8x, height - 1);
    riff_chunk(&mut body, b"VP8X", &vp8x)?;
    // Transparent background, loop forever.
    riff_chunk(&mut body, b"ANIM", &[0, 0, 0, 0, 0, 0])?;
    for frame in frames {
        let bitstream = vp8l_chunk(width, height, &frame.rgba8)?;
        let mut anmf = Vec::with_capacity(16 + bitstream.len());
        push_u24(&mut anmf, 0); // x / 2
        push_u24(&mut anmf, 0); // y / 2
        push_u24(&mut anmf, width - 1);
        push_u24(&mut anmf, height - 1);
        push_u24(&mut anmf, frame.delay_ms.min((1 << 24) - 1));
        // Do not blend (0x02), do not dispose: each frame is a whole canvas.
        anmf.push(0x02);
        anmf.extend_from_slice(&bitstream);
        riff_chunk(&mut body, b"ANMF", &anmf)?;
    }
    let len = u32::try_from(body.len())
        .map_err(|_| CodecError::BufferSize("a WebP file cannot exceed 4 GiB".into()))?;
    let mut out = Vec::with_capacity(body.len() + 8);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// W15-B round 2: both MP4 codecs animate: `Mp4` writes an H.264
    /// (`avc1`) track and `Mp4Av1` an AV1 (`av01`) track, each with every
    /// frame and its own delay.
    #[test]
    fn both_mp4_codecs_write_every_frame_with_its_delay() {
        use crate::codec::formats::mp4;
        let frames: Vec<AnimationFrame> = [(0u8, 100u32), (120, 250), (240, 40)]
            .iter()
            .map(|&(v, delay_ms)| AnimationFrame {
                rgba8: [v, 255 - v, 90, 255].repeat(32 * 32),
                delay_ms,
            })
            .collect();
        for (format, codec) in [
            (ExportFormat::Mp4(70), b"avc1"),
            (ExportFormat::Mp4Av1(70), b"av01"),
        ] {
            assert!(can_animate(format), "{format:?}");
            let bytes = encode_animation(format, 32, 32, &frames).unwrap();
            let info = mp4::probe(&bytes).unwrap();
            assert_eq!(&info.codec, codec, "{format:?}");
            assert_eq!(info.frame_count, 3, "{format:?}");
            assert_eq!(info.durations, vec![100, 250, 40], "{format:?}");
        }
    }

    const RED: [u8; 4] = [255, 0, 0, 255];
    const GREEN: [u8; 4] = [0, 255, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];
    const CLEAR: [u8; 4] = [0, 0, 0, 0];

    /// LZW for a 4-colour GIF (minimum code size 2) that never grows its
    /// table past three-bit codes: a clear code before every pixel.
    fn lzw_3bit(indices: &[u8]) -> Vec<u8> {
        let mut codes = Vec::new();
        for &index in indices {
            codes.push(4u8); // clear
            codes.push(index);
        }
        codes.push(5); // end of information
        let mut packed = Vec::new();
        let (mut acc, mut bits) = (0u32, 0u32);
        for code in codes {
            acc |= u32::from(code) << bits;
            bits += 3;
            while bits >= 8 {
                packed.push((acc & 0xFF) as u8);
                acc >>= 8;
                bits -= 8;
            }
        }
        if bits > 0 {
            packed.push(acc as u8);
        }
        packed
    }

    struct GifFrame {
        left: u16,
        top: u16,
        width: u16,
        height: u16,
        index: u8,
        disposal: u8,
        delay_cs: u16,
    }

    /// A GIF written byte by byte, so frames can carry offsets and disposal
    /// methods the `image` encoder never writes.
    fn hand_made_gif(width: u16, height: u16, frames: &[GifFrame]) -> Vec<u8> {
        let mut out = b"GIF89a".to_vec();
        out.extend_from_slice(&width.to_le_bytes());
        out.extend_from_slice(&height.to_le_bytes());
        out.extend_from_slice(&[0x91, 0, 0]); // 4-entry global table
        out.extend_from_slice(&[255, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0]);
        for f in frames {
            out.extend_from_slice(&[0x21, 0xF9, 4, f.disposal << 2]);
            out.extend_from_slice(&f.delay_cs.to_le_bytes());
            out.extend_from_slice(&[0, 0]);
            out.push(0x2C);
            for v in [f.left, f.top, f.width, f.height] {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out.push(0);
            out.push(2); // minimum code size
            let pixels = vec![f.index; usize::from(f.width) * usize::from(f.height)];
            let data = lzw_3bit(&pixels);
            for block in data.chunks(255) {
                out.push(block.len() as u8);
                out.extend_from_slice(block);
            }
            out.push(0);
        }
        out.push(0x3B);
        out
    }

    fn pixel(frame: &AnimationFrame, width: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * width + x) * 4) as usize;
        frame.rgba8[i..i + 4].try_into().unwrap()
    }

    fn solid(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
        rgba.repeat((width * height) as usize)
    }

    #[test]
    fn a_three_frame_gif_decodes_to_composited_frames_with_their_delays() {
        let gif = hand_made_gif(
            4,
            4,
            &[
                GifFrame {
                    left: 0,
                    top: 0,
                    width: 4,
                    height: 4,
                    index: 0,
                    disposal: 1, // keep
                    delay_cs: 10,
                },
                GifFrame {
                    left: 1,
                    top: 1,
                    width: 2,
                    height: 2,
                    index: 1,
                    disposal: 2, // restore to background
                    delay_cs: 20,
                },
                GifFrame {
                    left: 0,
                    top: 0,
                    width: 1,
                    height: 1,
                    index: 2,
                    disposal: 1,
                    delay_cs: 30,
                },
            ],
        );
        let anim = decode_animation_bytes(&gif, ImportLimits::default())
            .unwrap()
            .expect("three frames is an animation");
        assert_eq!((anim.width, anim.height), (4, 4));
        assert_eq!(anim.format, ImportFormat::Gif);
        let delays: Vec<u32> = anim.frames.iter().map(|f| f.delay_ms).collect();
        assert_eq!(delays, [100, 200, 300]);
        let [f1, f2, f3] = &anim.frames[..] else {
            panic!("expected three frames, got {}", anim.frames.len());
        };
        for y in 0..4 {
            for x in 0..4 {
                let centre = (1..3).contains(&x) && (1..3).contains(&y);
                assert_eq!(pixel(f1, 4, x, y), RED, "frame 1 ({x},{y})");
                // The 2x2 sub-frame sits at its offset over frame 1.
                let want2 = if centre { GREEN } else { RED };
                assert_eq!(pixel(f2, 4, x, y), want2, "frame 2 ({x},{y})");
                // Frame 2 restored its rectangle to the background before
                // frame 3 drew its one pixel at the origin.
                let want3 = if (x, y) == (0, 0) {
                    BLUE
                } else if centre {
                    CLEAR
                } else {
                    RED
                };
                assert_eq!(pixel(f3, 4, x, y), want3, "frame 3 ({x},{y})");
            }
        }
    }

    #[test]
    fn a_one_frame_gif_and_a_still_png_are_not_animations() {
        let gif = hand_made_gif(
            2,
            2,
            &[GifFrame {
                left: 0,
                top: 0,
                width: 2,
                height: 2,
                index: 0,
                disposal: 1,
                delay_cs: 0,
            }],
        );
        assert!(decode_animation_bytes(&gif, ImportLimits::default())
            .unwrap()
            .is_none());
        let png = crate::codec::encode(ExportFormat::Png, 2, 2, &solid(2, 2, RED)).unwrap();
        assert!(decode_animation_bytes(&png, ImportLimits::default())
            .unwrap()
            .is_none());
    }

    fn three_frames() -> Vec<AnimationFrame> {
        vec![
            AnimationFrame {
                rgba8: solid(3, 2, RED),
                delay_ms: 100,
            },
            AnimationFrame {
                rgba8: solid(3, 2, GREEN),
                delay_ms: 250,
            },
            AnimationFrame {
                rgba8: solid(3, 2, BLUE),
                delay_ms: 400,
            },
        ]
    }

    #[test]
    fn every_animated_format_round_trips_frames_and_delays() {
        let frames = three_frames();
        for format in [ExportFormat::Gif, ExportFormat::Png, ExportFormat::WebP] {
            let bytes = encode_animation(format, 3, 2, &frames).unwrap();
            let anim = decode_animation_bytes(&bytes, ImportLimits::default())
                .unwrap()
                .unwrap_or_else(|| panic!("{format:?} did not decode as an animation"));
            assert_eq!((anim.width, anim.height), (3, 2), "{format:?}");
            assert_eq!(anim.frames.len(), 3, "{format:?}");
            for (got, want) in anim.frames.iter().zip(&frames) {
                assert_eq!(got.rgba8, want.rgba8, "{format:?} pixels");
                // GIF stores hundredths of a second.
                assert_eq!(got.delay_ms, want.delay_ms / 10 * 10, "{format:?} delay");
            }
            // The flat decoder still reads the first frame of each.
            let still = crate::codec::decode_bytes(&bytes).unwrap();
            assert_eq!(still.rgba8, frames[0].rgba8, "{format:?} first frame");
        }
    }

    #[test]
    fn encoding_refuses_what_cannot_be_an_animation() {
        let frames = three_frames();
        assert!(encode_animation(ExportFormat::Jpeg(90), 3, 2, &frames).is_err());
        assert!(encode_animation(ExportFormat::Gif, 3, 2, &[]).is_err());
        assert!(encode_animation(ExportFormat::Gif, 4, 2, &frames).is_err());
    }

    #[test]
    fn a_frame_count_past_the_limit_is_refused() {
        let frame = AnimationFrame {
            rgba8: solid(1, 1, RED),
            delay_ms: 10,
        };
        let many = vec![frame; MAX_ANIMATION_FRAMES + 1];
        assert!(matches!(
            encode_animation(ExportFormat::Gif, 1, 1, &many),
            Err(CodecError::LimitExceeded(_))
        ));
        // Decoding is bounded the same way: a GIF of 1001 frames is refused.
        let frames: Vec<GifFrame> = (0..=MAX_ANIMATION_FRAMES)
            .map(|_| GifFrame {
                left: 0,
                top: 0,
                width: 1,
                height: 1,
                index: 0,
                disposal: 1,
                delay_cs: 1,
            })
            .collect();
        let gif = hand_made_gif(1, 1, &frames);
        assert!(matches!(
            decode_animation_bytes(&gif, ImportLimits::default()),
            Err(CodecError::LimitExceeded(_))
        ));
    }

    #[test]
    fn frame_layer_names_follow_photopeas_convention() {
        assert_eq!(frame_layer_name("Frame 1", 120), "_a_Frame 1,120");
        assert_eq!(
            parse_frame_layer_name("_a_Frame 1,120"),
            Some(("Frame 1", 120))
        );
        assert_eq!(
            parse_frame_layer_name("_a_walk"),
            Some(("walk", DEFAULT_FRAME_DELAY_MS))
        );
        assert_eq!(parse_frame_layer_name("Background"), None);
    }
}
