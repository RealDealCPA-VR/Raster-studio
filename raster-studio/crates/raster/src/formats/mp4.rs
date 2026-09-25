//! W13-L: MP4 video export — H.264 (W15-B, the default) or AV1 in an ISO
//! Base Media File (`.mp4`).
//!
//! # The encoders
//!
//! W15-B: **H.264** ([`Mp4Codec::H264`], the default, what plays
//! everywhere) through Cisco's OpenH264 compiled from source ([`h264`]):
//! High profile, 8-bit 4:2:0, an `avc1` sample entry with an `avcC` box.
//! H.264 4:2:0 needs an even frame size, so an odd edge is padded by
//! repeating its last pixel column / row, and the file's size is that even
//! size. OpenH264 encodes at most 3840 x 2160; past that, AV1.
//!
//! **AV1** ([`Mp4Codec::Av1`], the option) through `rav1e` 0.8.1
//! (BSD-2-Clause, pure Rust), the encoder that was already in the tree behind
//! `image`'s AVIF writer; it is named here directly with default features
//! off, so no `asm` (nasm) and no `threading` (rayon) pool.
//!
//! Frames are 8-bit 4:2:0, BT.709 limited range (tagged in the bitstream and
//! in a `colr` box), with transparency flattened onto white: a video has no
//! alpha. Both need at least 16 x 16 pixels; a smaller frame is refused.
//!
//! # The container
//!
//! Written here box by box (no muxer crate): `ftyp`, then `moov` (one video
//! track: `tkhd`, `mdhd`, `hdlr`, `vmhd`, `dref`, and a sample table whose
//! `stsd` holds an `avc1` entry with its `avcC`, or an `av01` entry with the
//! encoder's own `av1C`), then one
//! `mdat` chunk. `moov` comes first, so a player can start before the whole
//! file arrives. Each frame is one sample with its own duration (`stts`, in
//! milliseconds), so an animation's per-frame delays survive exactly; the key
//! frames are listed in `stss`.
//!
//! [`probe`] reads the box structure back (size, sample count, durations,
//! codec) without decoding a picture. W16-M: [`video`] decodes an H.264 or
//! AV1 track to frames for a video layer (run in app-shell's decode worker);
//! the codec facade itself still refuses a video file by name
//! ([`video_refusal`]): a video is not a picture, it opens through the
//! video-layer route.

use rav1e::prelude::*;

use crate::codec::CodecError;

// W15-B: the H.264 encoder, a sibling file (`formats/h264.rs`) declared here
// so the MP4 module owns it.
#[path = "h264.rs"]
pub mod h264;

// W16-M: MP4 video decoding (H.264 through OpenH264's decoder, AV1 through
// rusty_av1d) for video layers, run by app-shell's decode worker.
#[path = "mp4_video.rs"]
pub mod video;

/// The smallest frame edge rav1e encodes (it refuses anything below 16).
pub const MIN_EDGE: u32 = 16;

/// W15-B: the video codec inside an exported MP4.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Mp4Codec {
    /// H.264 (OpenH264, High profile): the default, plays everywhere.
    #[default]
    H264,
    /// AV1 (rav1e): smaller files, newer players only.
    Av1,
}

/// The movie timescale: durations are written in milliseconds.
pub const TIMESCALE: u32 = 1000;

/// W13X-9: the most samples (frames) [`probe`] reads from one track: an
/// hour at 60 fps is 216 000, so a million is far past any real clip, and it
/// bounds what the sample tables may make `probe` allocate (a few MiB)
/// whatever count a damaged or hostile `stsz` names.
pub const MAX_PROBE_SAMPLES: u32 = 1_000_000;

/// The encoder speed preset (0 slowest .. 10 fastest). 8 keeps an export of
/// a few seconds interactive at the cost of a little compression.
const SPEED: u8 = 8;

/// One frame to encode: row-major straight-alpha RGBA8 and how long it shows.
#[derive(Debug, Clone, Copy)]
pub struct Mp4Frame<'a> {
    pub rgba8: &'a [u8],
    pub duration_ms: u32,
}

/// What [`probe`] reads back from a file's box structure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mp4Info {
    /// The track's display size (`tkhd`), in pixels.
    pub width: u32,
    pub height: u32,
    /// The sample entry's coded size (`stsd`).
    pub coded_width: u32,
    pub coded_height: u32,
    /// The sample entry's codec (`avc1` or `av01` for what this module
    /// writes).
    pub codec: [u8; 4],
    /// Samples (frames) in the track, from `stsz`.
    pub frame_count: u32,
    /// The sample durations from `stts`, in media timescale units.
    pub durations: Vec<u32>,
    /// The media timescale (`mdhd`).
    pub timescale: u32,
    /// The 1-based numbers of the key frames (`stss`).
    pub sync_samples: Vec<u32>,
    /// Whether the sample entry carries an `av1C` configuration box.
    pub has_av1c: bool,
    /// W15-B: whether the sample entry carries an `avcC` configuration box.
    pub has_avcc: bool,
}

/// `quality` (1..=100, like JPEG / AVIF) as rav1e's quantizer (255..=0).
fn quantizer(quality: u8) -> usize {
    let q = u32::from(quality.clamp(1, 100));
    ((100 - q) * 255 / 99) as usize
}

/// Encode `frames` (each `width * height * 4` bytes) as an MP4 in the
/// default codec (W15-B: H.264).
pub fn encode(
    width: u32,
    height: u32,
    frames: &[Mp4Frame<'_>],
    quality: u8,
) -> Result<Vec<u8>, CodecError> {
    encode_with(width, height, frames, quality, Mp4Codec::default())
}

/// W15-B: encode `frames` (each `width * height * 4` bytes) as an MP4 in
/// `codec`.
pub fn encode_with(
    width: u32,
    height: u32,
    frames: &[Mp4Frame<'_>],
    quality: u8,
    codec: Mp4Codec,
) -> Result<Vec<u8>, CodecError> {
    if !(1..=100).contains(&quality) {
        return Err(CodecError::InvalidParameter(format!(
            "MP4 quality must be 1..=100, got {quality}"
        )));
    }
    if frames.is_empty() {
        return Err(CodecError::InvalidParameter(
            "a video needs at least one frame".into(),
        ));
    }
    if width < MIN_EDGE || height < MIN_EDGE || width > 16_384 || height > 16_384 {
        return Err(CodecError::InvalidParameter(format!(
            "MP4 export needs a frame of {MIN_EDGE}x{MIN_EDGE} to 16384x16384 pixels, \
             not {width}x{height}"
        )));
    }
    let expected = width as usize * height as usize * 4;
    for (i, f) in frames.iter().enumerate() {
        if f.rgba8.len() != expected {
            return Err(CodecError::BufferSize(format!(
                "frame {} holds {} bytes, a {width}x{height} frame needs {expected}",
                i + 1,
                f.rgba8.len()
            )));
        }
    }

    let durations: Vec<u32> = frames.iter().map(|f| f.duration_ms.max(1)).collect();
    match codec {
        Mp4Codec::H264 => encode_h264(width, height, frames, &durations, quality),
        Mp4Codec::Av1 => encode_av1(width, height, frames, &durations, quality),
    }
}

/// W15-B: H.264 through OpenH264, an odd edge padded to even.
fn encode_h264(
    width: u32,
    height: u32,
    frames: &[Mp4Frame<'_>],
    durations: &[u32],
    quality: u8,
) -> Result<Vec<u8>, CodecError> {
    let (pw, ph) = (width.next_multiple_of(2), height.next_multiple_of(2));
    if !h264::fits(pw, ph) {
        return Err(CodecError::InvalidParameter(format!(
            "H.264 MP4 export encodes at most {}x{} (or {}x{}) pixels, not {width}x{height}; \
             choose the AV1 codec for a larger video",
            h264::MAX_LONG_EDGE,
            h264::MAX_SHORT_EDGE,
            h264::MAX_SHORT_EDGE,
            h264::MAX_LONG_EDGE
        )));
    }
    let planes: Vec<[Vec<u8>; 3]> = frames
        .iter()
        .map(|f| {
            let padded = pad_even(f.rgba8, width as usize, height as usize);
            rgba_to_yuv420(&padded, pw as usize, ph as usize)
        })
        .collect();
    let refs: Vec<h264::Planes<'_>> = planes
        .iter()
        .map(|[y, u, v]| h264::Planes { y, u, v })
        .collect();
    let stream = h264::encode(pw, ph, &refs, durations, quality)?;
    let entry = visual_sample_entry(
        pw,
        ph,
        b"AVC Coding",
        &bx(b"avcC", &h264::avcc(&stream.sps, &stream.pps)?)?,
    )?;
    let samples: Vec<(Vec<u8>, bool)> = stream
        .samples
        .into_iter()
        .map(|s| (s.data, s.key))
        .collect();
    mux(
        pw,
        ph,
        &bx(b"avc1", &entry)?,
        b"isomiso2avc1mp41",
        &samples,
        durations,
    )
}

/// W15-B: `rgba` (`width x height`) with its last column / row repeated
/// until both edges are even.
fn pad_even(rgba: &[u8], width: usize, height: usize) -> std::borrow::Cow<'_, [u8]> {
    let (pw, ph) = (width.next_multiple_of(2), height.next_multiple_of(2));
    if (pw, ph) == (width, height) {
        return std::borrow::Cow::Borrowed(rgba);
    }
    let mut out = Vec::with_capacity(pw * ph * 4);
    for row in 0..ph {
        let src = &rgba[row.min(height - 1) * width * 4..][..width * 4];
        out.extend_from_slice(src);
        if pw > width {
            out.extend_from_slice(&src[(width - 1) * 4..]);
        }
    }
    std::borrow::Cow::Owned(out)
}

/// AV1 through rav1e.
fn encode_av1(
    width: u32,
    height: u32,
    frames: &[Mp4Frame<'_>],
    durations: &[u32],
    quality: u8,
) -> Result<Vec<u8>, CodecError> {
    let q = quantizer(quality);
    let enc = EncoderConfig {
        width: width as usize,
        height: height as usize,
        bit_depth: 8,
        chroma_sampling: ChromaSampling::Cs420,
        pixel_range: PixelRange::Limited,
        color_description: Some(ColorDescription {
            color_primaries: ColorPrimaries::BT709,
            transfer_characteristics: TransferCharacteristics::BT709,
            matrix_coefficients: MatrixCoefficients::BT709,
        }),
        time_base: Rational::new(1, u64::from(TIMESCALE)),
        low_latency: true,
        quantizer: q,
        min_quantizer: q.min(255) as u8,
        ..EncoderConfig::with_speed_preset(SPEED)
    };
    let cfg = Config::new().with_encoder_config(enc).with_threads(1);
    let mut ctx: Context<u8> = cfg
        .new_context()
        .map_err(|e| CodecError::InvalidParameter(format!("AV1 encoder: {e}")))?;

    let mut samples: Vec<(Vec<u8>, bool)> = Vec::with_capacity(frames.len());
    for f in frames {
        let mut frame = ctx.new_frame();
        fill_yuv420(&mut frame, width as usize, height as usize, f.rgba8);
        let mut pending = Some(frame);
        while let Some(frame) = pending.take() {
            match ctx.send_frame(frame.clone()) {
                Ok(()) => {}
                Err(EncoderStatus::EnoughData) => {
                    drain(&mut ctx, &mut samples)?;
                    pending = Some(frame);
                }
                Err(e) => return Err(encoder_error(e)),
            }
        }
        drain(&mut ctx, &mut samples)?;
    }
    ctx.flush();
    drain(&mut ctx, &mut samples)?;
    if samples.len() != frames.len() {
        return Err(CodecError::Unsupported(format!(
            "the AV1 encoder returned {} frames for {}",
            samples.len(),
            frames.len()
        )));
    }
    let av1c = ctx.container_sequence_header();
    let entry = visual_sample_entry(width, height, b"AV1 Coding", &bx(b"av1C", &av1c)?)?;
    mux(
        width,
        height,
        &bx(b"av01", &entry)?,
        b"isomiso2av01mp41",
        &samples,
        durations,
    )
}

fn encoder_error(e: EncoderStatus) -> CodecError {
    CodecError::Unsupported(format!("AV1 encoder: {e}"))
}

/// Take every packet the encoder has ready, as `(sample, is_key)`.
fn drain(ctx: &mut Context<u8>, out: &mut Vec<(Vec<u8>, bool)>) -> Result<(), CodecError> {
    loop {
        match ctx.receive_packet() {
            Ok(packet) => {
                let key = packet.frame_type == FrameType::KEY;
                out.push((strip_temporal_delimiter(packet.data), key));
            }
            Err(EncoderStatus::Encoded) => continue,
            Err(EncoderStatus::NeedMoreData) | Err(EncoderStatus::LimitReached) => return Ok(()),
            Err(e) => return Err(encoder_error(e)),
        }
    }
}

/// AV1-in-ISOBMFF samples carry no temporal delimiter OBU; rav1e starts
/// every packet with one (`0x12 0x00`).
fn strip_temporal_delimiter(mut data: Vec<u8>) -> Vec<u8> {
    if data.starts_with(&[0x12, 0x00]) {
        data.drain(..2);
    }
    data
}

/// RGBA over white, to BT.709 limited-range Y'CbCr, chroma averaged 2x2.
fn fill_yuv420(frame: &mut Frame<u8>, width: usize, height: usize, rgba: &[u8]) {
    let cw = width.div_ceil(2);
    let [y, cb, cr] = rgba_to_yuv420(rgba, width, height);
    frame.planes[0].copy_from_raw_u8(&y, width, 1);
    frame.planes[1].copy_from_raw_u8(&cb, cw, 1);
    frame.planes[2].copy_from_raw_u8(&cr, cw, 1);
}

/// W15-B: RGBA over white as BT.709 limited-range 4:2:0 planes `[Y, Cb, Cr]`
/// (chroma averaged 2x2, `ceil(w/2) x ceil(h/2)`), shared by both encoders.
fn rgba_to_yuv420(rgba: &[u8], width: usize, height: usize) -> [Vec<u8>; 3] {
    let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
    let mut y = vec![0u8; width * height];
    let mut cb = vec![0f32; cw * ch];
    let mut cr = vec![0f32; cw * ch];
    let mut n = vec![0f32; cw * ch];
    for row in 0..height {
        for col in 0..width {
            let p = &rgba[(row * width + col) * 4..][..4];
            let a = f32::from(p[3]) / 255.0;
            let flat = |c: u8| (f32::from(c) / 255.0) * a + (1.0 - a);
            let (r, g, b) = (flat(p[0]), flat(p[1]), flat(p[2]));
            let luma = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            y[row * width + col] = (16.0 + 219.0 * luma).round().clamp(0.0, 255.0) as u8;
            let i = (row / 2) * cw + col / 2;
            cb[i] += (b - luma) / 1.8556;
            cr[i] += (r - luma) / 1.5748;
            n[i] += 1.0;
        }
    }
    let chroma = |v: &[f32]| -> Vec<u8> {
        v.iter()
            .zip(&n)
            .map(|(s, k)| (128.0 + 224.0 * s / k.max(1.0)).round().clamp(0.0, 255.0) as u8)
            .collect()
    };
    let (cb, cr) = (chroma(&cb), chroma(&cr));
    [y, cb, cr]
}

// ---------------------------------------------------------------- the muxer

fn bx(kind: &[u8; 4], payload: &[u8]) -> Result<Vec<u8>, CodecError> {
    let size = u32::try_from(payload.len() + 8)
        .map_err(|_| CodecError::LimitExceeded("an MP4 box cannot exceed 4 GiB".into()))?;
    let mut out = Vec::with_capacity(payload.len() + 8);
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    Ok(out)
}

/// A full box: version 0 and `flags` before the payload.
fn full(kind: &[u8; 4], flags: u32, payload: &[u8]) -> Result<Vec<u8>, CodecError> {
    let mut p = Vec::with_capacity(payload.len() + 4);
    p.extend_from_slice(&(flags & 0x00FF_FFFF).to_be_bytes());
    p.extend_from_slice(payload);
    bx(kind, &p)
}

fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}

const MATRIX: [u32; 9] = [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];

fn be32(v: &[u32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_be_bytes()).collect()
}

/// The body of a visual sample entry (`av01` / `avc1`): the fixed fields,
/// the codec's configuration box `config`, then a BT.709 limited-range
/// `colr`.
fn visual_sample_entry(
    width: u32,
    height: u32,
    label: &[u8],
    config: &[u8],
) -> Result<Vec<u8>, CodecError> {
    let mut entry = Vec::new();
    entry.extend_from_slice(&[0; 6]);
    entry.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
    entry.extend_from_slice(&[0; 16]); // pre_defined, reserved
    entry.extend_from_slice(&(width as u16).to_be_bytes());
    entry.extend_from_slice(&(height as u16).to_be_bytes());
    entry.extend_from_slice(&be32(&[0x0048_0000, 0x0048_0000, 0]));
    entry.extend_from_slice(&1u16.to_be_bytes()); // frame_count
    let mut name = [0u8; 32];
    name[0] = label.len() as u8;
    name[1..=label.len()].copy_from_slice(label);
    entry.extend_from_slice(&name);
    entry.extend_from_slice(&0x0018u16.to_be_bytes());
    entry.extend_from_slice(&(-1i16).to_be_bytes());
    entry.extend_from_slice(config);
    // colr nclx: BT.709 primaries / transfer / matrix, limited range.
    entry.extend_from_slice(&bx(
        b"colr",
        &cat(&[b"nclx".to_vec(), vec![0, 1, 0, 1, 0, 1, 0]]),
    )?);
    Ok(entry)
}

/// `sample_entry` is the whole `stsd` entry box; `brands` the `ftyp`
/// compatible brands.
fn mux(
    width: u32,
    height: u32,
    sample_entry: &[u8],
    brands: &[u8],
    samples: &[(Vec<u8>, bool)],
    durations: &[u32],
) -> Result<Vec<u8>, CodecError> {
    let total: u64 = durations.iter().map(|d| u64::from(*d)).sum();
    let total = u32::try_from(total)
        .map_err(|_| CodecError::LimitExceeded("the video is too long".into()))?;
    let ftyp = bx(
        b"ftyp",
        &cat(&[b"isom".to_vec(), be32(&[0x200]), brands.to_vec()]),
    )?;
    let data_len: usize = samples.iter().map(|s| s.0.len()).sum();

    // `stco` holds where the samples start, which depends on `moov`'s own
    // size; that size does not depend on the value, so build it twice.
    let moov_with = |offset: u32| -> Result<Vec<u8>, CodecError> {
        let mvhd = full(
            b"mvhd",
            0,
            &cat(&[
                be32(&[0, 0, TIMESCALE, total, 0x0001_0000]),
                vec![0x01, 0x00, 0, 0],
                be32(&[0, 0]),
                be32(&MATRIX),
                be32(&[0; 6]),
                be32(&[2]),
            ]),
        )?;
        let tkhd = full(
            b"tkhd",
            0x3,
            &cat(&[
                be32(&[0, 0, 1, 0, total, 0, 0]),
                vec![0; 8],
                be32(&MATRIX),
                be32(&[width << 16, height << 16]),
            ]),
        )?;
        let mdhd = full(
            b"mdhd",
            0,
            &cat(&[be32(&[0, 0, TIMESCALE, total]), vec![0x55, 0xC4, 0, 0]]),
        )?;
        let hdlr = full(
            b"hdlr",
            0,
            &cat(&[
                be32(&[0]),
                b"vide".to_vec(),
                be32(&[0, 0, 0]),
                b"VideoHandler\0".to_vec(),
            ]),
        )?;
        let vmhd = full(b"vmhd", 1, &[0; 8])?;
        let dref = full(b"dref", 0, &cat(&[be32(&[1]), full(b"url ", 1, &[])?]))?;
        let dinf = bx(b"dinf", &dref)?;

        let stsd = full(b"stsd", 0, &cat(&[be32(&[1]), sample_entry.to_vec()]))?;

        let mut runs: Vec<(u32, u32)> = Vec::new();
        for d in durations {
            match runs.last_mut() {
                Some((n, v)) if *v == *d => *n += 1,
                _ => runs.push((1, *d)),
            }
        }
        let mut stts = be32(&[runs.len() as u32]);
        for (n, d) in &runs {
            stts.extend_from_slice(&be32(&[*n, *d]));
        }
        let stts = full(b"stts", 0, &stts)?;
        let keys: Vec<u32> = samples
            .iter()
            .enumerate()
            .filter(|(_, s)| s.1)
            .map(|(i, _)| i as u32 + 1)
            .collect();
        let stss = full(b"stss", 0, &cat(&[be32(&[keys.len() as u32]), be32(&keys)]))?;
        let count = samples.len() as u32;
        let stsc = full(b"stsc", 0, &be32(&[1, 1, count, 1]))?;
        let sizes: Vec<u32> = samples.iter().map(|s| s.0.len() as u32).collect();
        let stsz = full(b"stsz", 0, &cat(&[be32(&[0, count]), be32(&sizes)]))?;
        let stco = full(b"stco", 0, &be32(&[1, offset]))?;
        let stbl = bx(b"stbl", &cat(&[stsd, stts, stss, stsc, stsz, stco]))?;
        let minf = bx(b"minf", &cat(&[vmhd, dinf, stbl]))?;
        let mdia = bx(b"mdia", &cat(&[mdhd, hdlr, minf]))?;
        let trak = bx(b"trak", &cat(&[tkhd, mdia]))?;
        bx(b"moov", &cat(&[mvhd, trak]))
    };
    let moov_len = moov_with(0)?.len();
    let offset = u32::try_from(ftyp.len() + moov_len + 8)
        .map_err(|_| CodecError::LimitExceeded("the MP4 header is too large".into()))?;
    let moov = moov_with(offset)?;
    let mdat_size = u32::try_from(data_len + 8)
        .map_err(|_| CodecError::LimitExceeded("the video data exceeds 4 GiB".into()))?;
    let mut out = Vec::with_capacity(ftyp.len() + moov.len() + data_len + 8);
    out.extend_from_slice(&ftyp);
    out.extend_from_slice(&moov);
    out.extend_from_slice(&mdat_size.to_be_bytes());
    out.extend_from_slice(b"mdat");
    for (s, _) in samples {
        out.extend_from_slice(s);
    }
    Ok(out)
}

// ---------------------------------------------------------------- the probe

fn malformed(what: impl std::fmt::Display) -> CodecError {
    super::malformed("MP4", what)
}

/// A run of boxes as `(kind, payload)`.
type Boxes<'a> = Vec<([u8; 4], &'a [u8])>;

/// The boxes directly inside `data`, as `(kind, payload)`.
fn boxes(data: &[u8]) -> Result<Boxes<'_>, CodecError> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at < data.len() {
        let head = data
            .get(at..at + 8)
            .ok_or_else(|| malformed("a box header runs past the end"))?;
        let size = u32::from_be_bytes([head[0], head[1], head[2], head[3]]) as usize;
        let kind = [head[4], head[5], head[6], head[7]];
        let (start, end) = match size {
            0 => (at + 8, data.len()),
            1 => {
                let big = data
                    .get(at + 8..at + 16)
                    .ok_or_else(|| malformed("a 64-bit box size runs past the end"))?;
                let size = u64::from_be_bytes(big.try_into().expect("8 bytes"));
                let size = usize::try_from(size).map_err(|_| malformed("a box is too large"))?;
                (
                    at + 16,
                    at.checked_add(size).ok_or_else(|| malformed("box size"))?,
                )
            }
            s if s < 8 => return Err(malformed("a box is smaller than its header")),
            s => (at + 8, at + s),
        };
        if end > data.len() || start > end {
            return Err(malformed("a box runs past the end of its parent"));
        }
        out.push((kind, &data[start..end]));
        at = end;
    }
    Ok(out)
}

fn child<'a>(data: &'a [u8], kind: &[u8; 4]) -> Result<&'a [u8], CodecError> {
    boxes(data)?
        .into_iter()
        .find(|(k, _)| k == kind)
        .map(|(_, p)| p)
        .ok_or_else(|| malformed(format!("no {} box", String::from_utf8_lossy(kind))))
}

fn u32_at(data: &[u8], at: usize) -> Result<u32, CodecError> {
    data.get(at..at + 4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| malformed("a field runs past the end of its box"))
}

fn u16_at(data: &[u8], at: usize) -> Result<u16, CodecError> {
    data.get(at..at + 2)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
        .ok_or_else(|| malformed("a field runs past the end of its box"))
}

/// Read an MP4's first video track back from its box structure, checking
/// that every sample the tables name lies inside the file.
pub fn probe(bytes: &[u8]) -> Result<Mp4Info, CodecError> {
    let top = boxes(bytes)?;
    if top.first().map(|b| &b.0) != Some(b"ftyp") {
        return Err(malformed("the file does not start with an ftyp box"));
    }
    let moov = top
        .iter()
        .find(|(k, _)| k == b"moov")
        .map(|b| b.1)
        .ok_or_else(|| malformed("no moov box"))?;
    let trak = boxes(moov)?
        .into_iter()
        .filter(|(k, _)| k == b"trak")
        .map(|b| b.1)
        .find(|t| {
            child(t, b"mdia")
                .and_then(|m| child(m, b"hdlr"))
                .is_ok_and(|h| h.get(8..12) == Some(b"vide"))
        })
        .ok_or_else(|| malformed("no video track"))?;
    let tkhd = child(trak, b"tkhd")?;
    if tkhd.first() != Some(&0) {
        return Err(malformed("only version-0 tkhd boxes are read"));
    }
    let width = u32_at(tkhd, 76)? >> 16;
    let height = u32_at(tkhd, 80)? >> 16;
    let mdia = child(trak, b"mdia")?;
    let mdhd = child(mdia, b"mdhd")?;
    let timescale = u32_at(mdhd, 12)?;
    let stbl = child(child(mdia, b"minf")?, b"stbl")?;

    let stsd = child(stbl, b"stsd")?;
    let entries = boxes(stsd.get(8..).ok_or_else(|| malformed("stsd"))?)?;
    let (codec, entry) = *entries.first().ok_or_else(|| malformed("stsd is empty"))?;
    let coded_width = u32::from(u16_at(entry, 24)?);
    let coded_height = u32::from(u16_at(entry, 26)?);
    let has_config = |kind: &[u8; 4]| {
        entry
            .get(78..)
            .map(|rest| boxes(rest).is_ok_and(|b| b.iter().any(|(k, _)| k == kind)))
            .unwrap_or(false)
    };
    let has_av1c = has_config(b"av1C");
    let has_avcc = has_config(b"avcC");

    let stsz = child(stbl, b"stsz")?;
    let uniform = u32_at(stsz, 4)?;
    let frame_count = u32_at(stsz, 8)?;
    // W13X-9: bound the count before anything is sized by it. Past the cap
    // it is refused outright; under it, every sample must still fit: a
    // uniform size puts `count * size` bytes in the file, a size table
    // needs four bytes per sample inside `stsz`.
    if frame_count > MAX_PROBE_SAMPLES {
        return Err(malformed(format!(
            "stsz names {frame_count} samples, more than the {MAX_PROBE_SAMPLES} this build reads"
        )));
    }
    let fits = if uniform != 0 {
        u64::from(frame_count) * u64::from(uniform) <= bytes.len() as u64
    } else {
        12 + u64::from(frame_count) * 4 <= stsz.len() as u64
    };
    if !fits {
        return Err(malformed(format!(
            "stsz names {frame_count} samples, more than the file holds"
        )));
    }
    let sizes: Vec<u32> = if uniform != 0 {
        vec![uniform; frame_count as usize]
    } else {
        (0..frame_count as usize)
            .map(|i| u32_at(stsz, 12 + i * 4))
            .collect::<Result<_, _>>()?
    };

    let stts = child(stbl, b"stts")?;
    let mut durations = Vec::new();
    for i in 0..u32_at(stts, 4)? as usize {
        let n = u32_at(stts, 8 + i * 8)?;
        let d = u32_at(stts, 12 + i * 8)?;
        if durations.len() + n as usize > frame_count as usize {
            return Err(malformed("stts names more samples than stsz"));
        }
        durations.extend(std::iter::repeat_n(d, n as usize));
    }
    if durations.len() != frame_count as usize {
        return Err(malformed("stts and stsz disagree on the sample count"));
    }
    let sync_samples = match child(stbl, b"stss") {
        Ok(stss) => (0..u32_at(stss, 4)? as usize)
            .map(|i| u32_at(stss, 8 + i * 4))
            .collect::<Result<_, _>>()?,
        Err(_) => (1..=frame_count).collect(),
    };

    // Every sample must lie inside the file: one chunk per stco entry, the
    // samples split among them by stsc.
    let stco = child(stbl, b"stco")?;
    let chunks: Vec<u64> = (0..u32_at(stco, 4)? as usize)
        .map(|i| u32_at(stco, 8 + i * 4).map(u64::from))
        .collect::<Result<_, _>>()?;
    let stsc = child(stbl, b"stsc")?;
    let runs: Vec<(u32, u32)> = (0..u32_at(stsc, 4)? as usize)
        .map(|i| Ok((u32_at(stsc, 8 + i * 12)?, u32_at(stsc, 12 + i * 12)?)))
        .collect::<Result<_, CodecError>>()?;
    let mut sample = 0usize;
    for (c, start) in chunks.iter().enumerate() {
        let chunk_no = c as u32 + 1;
        let per = runs
            .iter()
            .rev()
            .find(|(first, _)| *first <= chunk_no)
            .map(|r| r.1)
            .ok_or_else(|| malformed("stsc does not cover every chunk"))?;
        let mut at = *start;
        for _ in 0..per {
            let size = *sizes
                .get(sample)
                .ok_or_else(|| malformed("stsc names more samples than stsz"))?;
            at += u64::from(size);
            if at > bytes.len() as u64 {
                return Err(malformed("a sample lies past the end of the file"));
            }
            sample += 1;
        }
    }
    if sample != frame_count as usize {
        return Err(malformed("the chunks do not hold every sample"));
    }
    Ok(Mp4Info {
        width,
        height,
        coded_width,
        coded_height,
        codec,
        frame_count,
        durations,
        timescale,
        sync_samples,
        has_av1c,
        has_avcc,
    })
}

// ---------------------------------------------------------- video import

/// `true` for the head of a video file: an ISO BMFF `ftyp` naming a video
/// brand (MP4, MOV, M4V, 3GP), Matroska / WebM, or AVI. AVIF and HEIF
/// images share the `ftyp` box and are left to their own sniffs.
pub fn looks_like_video(head: &[u8]) -> bool {
    if head.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return true;
    }
    if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"AVI " {
        return true;
    }
    if head.len() < 12 || &head[4..8] != b"ftyp" {
        return false;
    }
    let brand = &head[8..12];
    matches!(
        brand,
        b"isom"
            | b"iso2"
            | b"iso4"
            | b"iso5"
            | b"iso6"
            | b"mp41"
            | b"mp42"
            | b"avc1"
            | b"av01"
            | b"M4V "
            | b"M4VP"
            | b"qt  "
            | b"3gp4"
            | b"3gp5"
            | b"3gp6"
            | b"3g2a"
            | b"dash"
            | b"MSNV"
    )
}

/// The refusal a video file gets from File > Open, naming why.
pub fn video_refusal() -> CodecError {
    CodecError::Unsupported(
        "opening video files (MP4, MOV, WebM, AVI) as video layers is not supported: this \
         build has no permissively licensed pure-Rust video decoder it can run safely (the \
         H.264 / HEVC / VP9 decoders are C libraries, some GPL/LGPL; rav1d, the pure-Rust \
         AV1 decoder, aborts the process on damaged input and this build aborts on a panic; \
         rav1d-safe is AGPL); export the frames as an animated GIF, APNG or WebP, or as \
         images, and open those instead"
            .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `n` frames of `w` x `h`, each a different solid colour with a
    /// half-transparent stripe.
    fn frames(w: u32, h: u32, n: usize) -> Vec<Vec<u8>> {
        (0..n)
            .map(|i| {
                let mut px = Vec::with_capacity((w * h * 4) as usize);
                for y in 0..h {
                    for _ in 0..w {
                        let a = if y < h / 4 { 128 } else { 255 };
                        px.extend_from_slice(&[(i * 70) as u8, 200 - (i * 40) as u8, 90, a]);
                    }
                }
                px
            })
            .collect()
    }

    #[test]
    fn an_exported_mp4_parses_back_with_its_frame_count_size_and_durations() {
        let (w, h) = (48, 34);
        let px = frames(w, h, 4);
        let delays = [100, 250, 100, 40];
        let input: Vec<Mp4Frame<'_>> = px
            .iter()
            .zip(delays)
            .map(|(p, d)| Mp4Frame {
                rgba8: p,
                duration_ms: d,
            })
            .collect();
        // W15-B: AV1 is the option now; `encode` writes H.264.
        let bytes = encode_with(w, h, &input, 70, Mp4Codec::Av1).unwrap();
        assert_eq!(&bytes[4..8], b"ftyp");
        assert!(!looks_like_video(&[]));
        assert!(
            looks_like_video(&bytes[..12]),
            "what it writes sniffs as video"
        );
        let top: Vec<[u8; 4]> = boxes(&bytes).unwrap().iter().map(|b| b.0).collect();
        assert_eq!(top, vec![*b"ftyp", *b"moov", *b"mdat"]);
        let info = probe(&bytes).unwrap();
        assert_eq!((info.width, info.height), (w, h));
        assert_eq!((info.coded_width, info.coded_height), (w, h));
        assert_eq!(&info.codec, b"av01");
        assert!(info.has_av1c);
        assert_eq!(info.frame_count, 4);
        assert_eq!(info.timescale, TIMESCALE);
        assert_eq!(info.durations, delays.to_vec());
        assert_eq!(
            info.sync_samples.first(),
            Some(&1),
            "the first frame is a key frame"
        );
    }

    #[test]
    fn a_one_frame_mp4_and_the_encoders_refusals() {
        let px = frames(16, 16, 1);
        let one = [Mp4Frame {
            rgba8: &px[0],
            duration_ms: 1000,
        }];
        let info = probe(&encode(16, 16, &one, 1).unwrap()).unwrap();
        assert_eq!((info.frame_count, info.durations.clone()), (1, vec![1000]));
        assert!(encode(15, 16, &[], 50).is_err(), "no frames");
        let small = frames(15, 16, 1);
        let err = encode(
            15,
            16,
            &[Mp4Frame {
                rgba8: &small[0],
                duration_ms: 10,
            }],
            50,
        )
        .unwrap_err();
        assert!(err.to_string().contains("16x16"), "{err}");
        assert!(encode(16, 16, &one, 0).is_err(), "quality 0");
        assert!(encode(16, 16, &one, 101).is_err(), "quality 101");
        let short = [Mp4Frame {
            rgba8: &px[0][..10],
            duration_ms: 10,
        }];
        assert!(encode(16, 16, &short, 50).is_err(), "wrong buffer size");
    }

    #[test]
    fn a_damaged_mp4_is_an_error_not_a_panic() {
        let px = frames(16, 16, 2);
        let input: Vec<Mp4Frame<'_>> = px
            .iter()
            .map(|p| Mp4Frame {
                rgba8: p,
                duration_ms: 50,
            })
            .collect();
        let bytes = encode(16, 16, &input, 50).unwrap();
        for cut in [0, 7, 20, bytes.len() / 2, bytes.len() - 1] {
            assert!(probe(&bytes[..cut]).is_err(), "cut at {cut}");
        }
        let mut flipped = bytes.clone();
        for i in (0..flipped.len()).step_by(7) {
            flipped[i] ^= 0xA5;
            let _ = probe(&flipped);
        }
    }

    /// W13X-9: a `stsz` naming an absurd sample count is refused before any
    /// table is sized by it (a uniform-size `stsz` of `u32::MAX` samples
    /// would otherwise ask for 16 GiB of sizes).
    #[test]
    fn probe_refuses_an_absurd_frame_count_before_allocating() {
        let px = frames(16, 16, 2);
        let input: Vec<Mp4Frame<'_>> = px
            .iter()
            .map(|p| Mp4Frame {
                rgba8: p,
                duration_ms: 50,
            })
            .collect();
        let bytes = encode(16, 16, &input, 50).unwrap();
        let at = bytes
            .windows(4)
            .position(|w| w == b"stsz")
            .expect("an stsz box");
        // kind, version + flags, uniform size, sample count.
        let patched = |uniform: u32, count: u32| {
            let mut b = bytes.clone();
            b[at + 8..at + 12].copy_from_slice(&uniform.to_be_bytes());
            b[at + 12..at + 16].copy_from_slice(&count.to_be_bytes());
            b
        };
        for (uniform, count) in [(1, u32::MAX), (0, u32::MAX), (1, MAX_PROBE_SAMPLES + 1)] {
            let err = probe(&patched(uniform, count)).unwrap_err().to_string();
            assert!(err.contains(&format!("{count} samples")), "{err}");
            assert!(err.contains("more than"), "{err}");
        }
        // Under the cap but more than the file can hold: refused too.
        let err = probe(&patched(1, MAX_PROBE_SAMPLES))
            .unwrap_err()
            .to_string();
        assert!(err.contains("more than the file holds"), "{err}");
        // A file big enough to hold the samples (a `free` box of padding
        // at the end) is still refused past the cap, by the cap.
        let mut padded = patched(1, MAX_PROBE_SAMPLES + 1);
        let pad = MAX_PROBE_SAMPLES as usize + 64;
        padded.extend_from_slice(&(pad as u32 + 8).to_be_bytes());
        padded.extend_from_slice(b"free");
        padded.resize(padded.len() + pad, 0);
        let err = probe(&padded).unwrap_err().to_string();
        assert!(
            err.contains(&format!("more than the {MAX_PROBE_SAMPLES}")),
            "{err}"
        );
        // The untouched file still probes, padded or not.
        assert_eq!(probe(&bytes).unwrap().frame_count, 2);
        let mut padded = bytes.clone();
        padded.extend_from_slice(&(pad as u32 + 8).to_be_bytes());
        padded.extend_from_slice(b"free");
        padded.resize(padded.len() + pad, 0);
        assert_eq!(probe(&padded).unwrap().frame_count, 2);
    }

    // ------------------------------------------------------ W15-B: H.264

    /// `n` frames of `w` x `h` with texture (a diagonal gradient, a moving
    /// square, a half-transparent band), so a PSNR means something.
    fn textured(w: u32, h: u32, n: usize) -> Vec<Vec<u8>> {
        (0..n)
            .map(|i| {
                let mut px = Vec::with_capacity((w * h * 4) as usize);
                for y in 0..h {
                    for x in 0..w {
                        let inside = (x as usize + 40 - (i * 3) % 40) % 40 < 14 && y > h / 3;
                        let r = ((x * 255) / w.max(1)) as u8;
                        let g = ((y * 255) / h.max(1)) as u8;
                        let b = if inside { 30 } else { 200 - (i * 10) as u8 };
                        let a = if y < h / 8 { 128 } else { 255 };
                        px.extend_from_slice(&[r, g, b, a]);
                    }
                }
                px
            })
            .collect()
    }

    fn mp4_frames<'a>(px: &'a [Vec<u8>], durations: &[u32]) -> Vec<Mp4Frame<'a>> {
        px.iter()
            .zip(durations)
            .map(|(p, d)| Mp4Frame {
                rgba8: p,
                duration_ms: *d,
            })
            .collect()
    }

    /// What the box walk finds in an `avc1` track: the `avcC` payload and
    /// every sample's bytes, located through `stsz` / `stsc` / `stco`.
    struct Avc {
        ftyp_brands: Vec<[u8; 4]>,
        avcc: Vec<u8>,
        entry_size: (u16, u16),
        samples: Vec<Vec<u8>>,
    }

    fn walk_avc1(bytes: &[u8]) -> Avc {
        let top = boxes(bytes).unwrap();
        let kinds: Vec<[u8; 4]> = top.iter().map(|b| b.0).collect();
        assert_eq!(kinds, vec![*b"ftyp", *b"moov", *b"mdat"]);
        let ftyp = top[0].1;
        let ftyp_brands = ftyp[8..]
            .chunks(4)
            .map(|c| [c[0], c[1], c[2], c[3]])
            .collect();
        let trak = child(top[1].1, b"trak").unwrap();
        let stbl = child(
            child(child(trak, b"mdia").unwrap(), b"minf").unwrap(),
            b"stbl",
        )
        .unwrap();
        let stsd = child(stbl, b"stsd").unwrap();
        let entries = boxes(&stsd[8..]).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(&entries[0].0, b"avc1", "the sample entry is avc1");
        let entry = entries[0].1;
        let entry_size = (u16_at(entry, 24).unwrap(), u16_at(entry, 26).unwrap());
        let avcc = child(&entry[78..], b"avcC").unwrap().to_vec();
        let stsz = child(stbl, b"stsz").unwrap();
        let count = u32_at(stsz, 8).unwrap() as usize;
        let sizes: Vec<usize> = (0..count)
            .map(|i| u32_at(stsz, 12 + i * 4).unwrap() as usize)
            .collect();
        let stco = child(stbl, b"stco").unwrap();
        assert_eq!(u32_at(stco, 4).unwrap(), 1, "one chunk");
        let mut at = u32_at(stco, 8).unwrap() as usize;
        let samples = sizes
            .iter()
            .map(|n| {
                let s = bytes[at..at + n].to_vec();
                at += n;
                s
            })
            .collect();
        Avc {
            ftyp_brands,
            avcc,
            entry_size,
            samples,
        }
    }

    /// The SPS and PPS inside an `avcC`, checking its fixed fields.
    fn avcc_parameter_sets(avcc: &[u8]) -> (Vec<u8>, Vec<u8>) {
        assert_eq!(avcc[0], 1, "configurationVersion");
        assert_eq!(avcc[4] & 0x03, 3, "4-byte NAL lengths");
        assert_eq!(avcc[5] & 0x1F, 1, "one SPS");
        let sps_len = u16::from_be_bytes([avcc[6], avcc[7]]) as usize;
        let sps = avcc[8..8 + sps_len].to_vec();
        assert_eq!(sps[0] & 0x1F, 7, "an SPS NAL");
        assert_eq!(
            (avcc[1], avcc[2], avcc[3]),
            (sps[1], sps[2], sps[3]),
            "profile / compatibility / level copied from the SPS"
        );
        let at = 8 + sps_len;
        assert_eq!(avcc[at], 1, "one PPS");
        let pps_len = u16::from_be_bytes([avcc[at + 1], avcc[at + 2]]) as usize;
        let pps = avcc[at + 3..at + 3 + pps_len].to_vec();
        assert_eq!(pps[0] & 0x1F, 8, "a PPS NAL");
        (sps, pps)
    }

    /// A length-prefixed sample's NAL units.
    fn sample_nals(sample: &[u8]) -> Vec<&[u8]> {
        let mut out = Vec::new();
        let mut at = 0;
        while at < sample.len() {
            let n = u32::from_be_bytes(sample[at..at + 4].try_into().unwrap()) as usize;
            out.push(&sample[at + 4..at + 4 + n]);
            at += 4 + n;
        }
        assert_eq!(at, sample.len(), "the lengths tile the sample exactly");
        out
    }

    /// Decode samples `0..=upto` with OpenH264's own decoder, from the
    /// `avcC` parameter sets; the last picture as `(w, h, [Y, U, V])`.
    fn decode_upto(avc: &Avc, upto: usize) -> (usize, usize, [Vec<u8>; 3]) {
        use openh264::formats::YUVSource;
        let (sps, pps) = avcc_parameter_sets(&avc.avcc);
        let mut annex_b = Vec::new();
        for nal in [&sps[..], &pps[..]] {
            annex_b.extend_from_slice(&[0, 0, 0, 1]);
            annex_b.extend_from_slice(nal);
        }
        let mut decoder = openh264::decoder::Decoder::new().unwrap();
        let mut last = None;
        for sample in &avc.samples[..=upto] {
            for nal in sample_nals(sample) {
                annex_b.extend_from_slice(&[0, 0, 0, 1]);
                annex_b.extend_from_slice(nal);
            }
            if let Some(yuv) = decoder.decode(&annex_b).unwrap() {
                let (w, h) = yuv.dimensions();
                let (sy, su, sv) = yuv.strides();
                let plane = |data: &[u8], stride: usize, pw: usize, ph: usize| -> Vec<u8> {
                    (0..ph)
                        .flat_map(|r| data[r * stride..r * stride + pw].iter().copied())
                        .collect()
                };
                last = Some((
                    w,
                    h,
                    [
                        plane(yuv.y(), sy, w, h),
                        plane(yuv.u(), su, w / 2, h / 2),
                        plane(yuv.v(), sv, w / 2, h / 2),
                    ],
                ));
            }
            annex_b.clear();
        }
        last.expect("the decoder returned a picture")
    }

    fn psnr(a: &[u8], b: &[u8]) -> f64 {
        assert_eq!(a.len(), b.len());
        let mse: f64 = a
            .iter()
            .zip(b)
            .map(|(x, y)| (f64::from(*x) - f64::from(*y)).powi(2))
            .sum::<f64>()
            / a.len() as f64;
        if mse == 0.0 {
            return f64::INFINITY;
        }
        10.0 * (255.0f64 * 255.0 / mse).log10()
    }

    /// PSNR of the decoded planes against the source's own 4:2:0 planes,
    /// all three together.
    fn yuv_psnr(decoded: &[Vec<u8>; 3], source: &[Vec<u8>; 3]) -> f64 {
        let a: Vec<u8> = decoded.concat();
        let b: Vec<u8> = source.concat();
        psnr(&a, &b)
    }

    /// W15-B: `encode` now writes H.264: an `avc1` track whose `avcC` holds
    /// the High-profile SPS / PPS, one length-prefixed sample per frame with
    /// the per-frame durations, and a first frame that OpenH264's decoder
    /// turns back into the source within 30 dB PSNR.
    #[test]
    fn an_h264_mp4_parses_back_and_its_first_frame_decodes_close_to_the_source() {
        let (w, h) = (64, 48);
        let px = textured(w, h, 5);
        let delays = [100, 250, 100, 40, 500];
        let bytes = encode(w, h, &mp4_frames(&px, &delays), 80).unwrap();
        assert_eq!(Mp4Codec::default(), Mp4Codec::H264, "H.264 is the default");
        assert!(looks_like_video(&bytes[..12]));

        let info = probe(&bytes).unwrap();
        assert_eq!(&info.codec, b"avc1");
        assert!(info.has_avcc && !info.has_av1c);
        assert_eq!((info.width, info.height), (w, h));
        assert_eq!((info.coded_width, info.coded_height), (w, h));
        assert_eq!(info.frame_count, 5);
        assert_eq!(info.durations, delays.to_vec());
        assert_eq!(info.timescale, TIMESCALE);
        assert_eq!(info.sync_samples.first(), Some(&1));

        let avc = walk_avc1(&bytes);
        assert!(avc.ftyp_brands.contains(b"avc1"), "{:?}", avc.ftyp_brands);
        assert!(!avc.ftyp_brands.contains(b"av01"));
        assert_eq!(avc.entry_size, (w as u16, h as u16));
        assert_eq!(avc.samples.len(), 5);
        let (sps, _) = avcc_parameter_sets(&avc.avcc);
        assert_eq!(sps[1], 100, "High profile (profile_idc 100)");
        // The High-profile tail: 4:2:0, 8-bit.
        assert_eq!(&avc.avcc[avc.avcc.len() - 4..], &[0xFD, 0xF8, 0xF8, 0]);
        for (i, s) in avc.samples.iter().enumerate() {
            for nal in sample_nals(s) {
                let kind = nal[0] & 0x1F;
                assert!(
                    !matches!(kind, 7 | 8),
                    "sample {i} carries a parameter set in-band"
                );
            }
        }
        assert!(
            sample_nals(&avc.samples[0])
                .iter()
                .any(|n| n[0] & 0x1F == 5),
            "the first sample is an IDR picture"
        );

        let (dw, dh, first) = decode_upto(&avc, 0);
        assert_eq!((dw, dh), (w as usize, h as usize));
        let source = rgba_to_yuv420(&px[0], w as usize, h as usize);
        let p = yuv_psnr(&first, &source);
        assert!(p > 30.0, "first frame PSNR {p:.2} dB");
        // And the last frame (a P frame chain) decodes as well.
        let (_, _, last) = decode_upto(&avc, 4);
        let p = yuv_psnr(&last, &rgba_to_yuv420(&px[4], w as usize, h as usize));
        assert!(p > 30.0, "last frame PSNR {p:.2} dB");
    }

    /// W15-B: an odd edge is padded to even by repeating the last column /
    /// row; the picture inside is the source's.
    #[test]
    fn an_odd_sized_h264_mp4_is_padded_to_even() {
        let (w, h) = (49, 35);
        let px = textured(w, h, 2);
        let bytes = encode(w, h, &mp4_frames(&px, &[100, 100]), 85).unwrap();
        let info = probe(&bytes).unwrap();
        assert_eq!((info.width, info.height), (50, 36));
        assert_eq!((info.coded_width, info.coded_height), (50, 36));
        let avc = walk_avc1(&bytes);
        let (dw, dh, first) = decode_upto(&avc, 0);
        assert_eq!((dw, dh), (50, 36));
        let padded = pad_even(&px[0], w as usize, h as usize);
        let source = rgba_to_yuv420(&padded, 50, 36);
        let p = yuv_psnr(&first, &source);
        assert!(p > 30.0, "PSNR {p:.2} dB");
        // The padding repeats the edge: the extra column is the last one.
        let row = |r: usize| &padded[r * 50 * 4..(r + 1) * 50 * 4];
        assert_eq!(&row(3)[49 * 4..], &row(3)[48 * 4..49 * 4]);
        assert_eq!(row(35), row(34));
    }

    /// The 1-based samples of `bytes` that hold an IDR picture, checked
    /// against `stss` (which must list exactly those), and the IDR rule by
    /// time: no frame starts [`h264::KEY_INTERVAL_SECONDS`] or more after
    /// the last IDR without being one itself.
    fn assert_key_frames_by_time(bytes: &[u8], durations: &[u32]) -> Vec<u32> {
        let info = probe(bytes).unwrap();
        let avc = walk_avc1(bytes);
        let idr: Vec<u32> = avc
            .samples
            .iter()
            .enumerate()
            .filter(|(_, s)| sample_nals(s).iter().any(|n| n[0] & 0x1F == 5))
            .map(|(i, _)| i as u32 + 1)
            .collect();
        assert_eq!(info.sync_samples, idr, "stss names exactly the IDRs");
        assert_eq!(idr.first(), Some(&1));
        let limit = u64::from(h264::KEY_INTERVAL_SECONDS) * 1000;
        let (mut at, mut last_key) = (0u64, 0u64);
        for (i, d) in durations.iter().enumerate() {
            if idr.contains(&(i as u32 + 1)) {
                last_key = at;
            } else {
                assert!(
                    at - last_key < limit,
                    "frame {} starts {} ms after the last key frame: {idr:?}",
                    i + 1,
                    at - last_key
                );
            }
            at += u64::from(*d);
        }
        idr
    }

    /// W15-B: a key frame at least every KEY_INTERVAL_SECONDS of video: at
    /// 2 fps that is every 4 frames, and `stss` lists exactly the samples
    /// holding an IDR.
    #[test]
    fn h264_key_frames_recur_and_stss_names_exactly_the_idr_samples() {
        let (w, h) = (32, 32);
        let px = textured(w, h, 9);
        let bytes = encode(w, h, &mp4_frames(&px, &[500; 9]), 60).unwrap();
        let idr = assert_key_frames_by_time(&bytes, &[500; 9]);
        assert_eq!(idr, vec![1, 5, 9]);
    }

    /// W15-B round 2: key frames are placed by time, not by a frame count at
    /// the shortest frame's rate: one 40 ms frame then ten 500 ms frames
    /// (5.04 s) still gets a key frame every 2 s (it had only the first).
    #[test]
    fn h264_key_frames_follow_time_with_mixed_durations() {
        let (w, h) = (32, 32);
        let mut delays = vec![40];
        delays.extend([500; 10]);
        let px = textured(w, h, delays.len());
        let bytes = encode(w, h, &mp4_frames(&px, &delays), 80).unwrap();
        let idr = assert_key_frames_by_time(&bytes, &delays);
        // Starts: 0, 40, 540, 1040, 1540, 2040 (key), .., 4040 (key), 4540.
        assert_eq!(idr, vec![1, 6, 10]);
    }

    /// W15-B round 2: the rate-control budget is clamped to the level's
    /// maximum bit rate, so the sizes and rates the timeline reaches open
    /// the encoder: 1080p at 60 fps (16 ms frames) and 4K at 30 fps and
    /// 60 fps, at the default quality 80 and at 100 (and 4K at 10 ms, an
    /// animated GIF's delay, past level 5.2's rate). Without the clamp
    /// OpenH264 refuses to open for 1080p60 and 4K video ("MaxSpatialBitrate
    /// .. should be larger than SpatialBitrate").
    #[test]
    fn h264_encodes_1080p60_and_4k_at_video_frame_rates() {
        for (w, h, ms, q) in [
            (1920, 1080, 16, 80),
            (1920, 1080, 16, 100),
            (3840, 2160, 33, 80),
            (3840, 2160, 40, 50),
            (3840, 2160, 16, 100),
            (2160, 3840, 33, 80),
            (3840, 2160, 10, 80),
        ] {
            let px = textured(w, h, 2);
            let bytes = encode(w, h, &mp4_frames(&px, &[ms, ms]), q)
                .unwrap_or_else(|e| panic!("{w}x{h} at {ms} ms, quality {q}: {e}"));
            let info = probe(&bytes).unwrap();
            assert_eq!(&info.codec, b"avc1");
            assert_eq!((info.width, info.height), (w, h));
            assert_eq!(info.frame_count, 2);
            assert_eq!(info.durations, vec![ms, ms]);
        }
        // And the 1080p60 file's first frame decodes close to the source.
        let (w, h) = (1920, 1080);
        let px = textured(w, h, 1);
        let bytes = encode(w, h, &mp4_frames(&px, &[16]), 80).unwrap();
        let (dw, dh, first) = decode_upto(&walk_avc1(&bytes), 0);
        assert_eq!((dw, dh), (1920, 1080));
        let p = yuv_psnr(&first, &rgba_to_yuv420(&px[0], 1920, 1080));
        assert!(p > 30.0, "1080p PSNR {p:.2} dB");
    }

    /// W15-B: the Export As quality slider reaches the H.264 quantizer: a
    /// higher quality is a larger file and a closer picture.
    #[test]
    fn the_h264_quality_slider_trades_size_for_fidelity() {
        let (w, h) = (64, 64);
        let px = textured(w, h, 1);
        let source = rgba_to_yuv420(&px[0], 64, 64);
        let run = |q: u8| {
            let bytes = encode(w, h, &mp4_frames(&px, &[1000]), q).unwrap();
            let (_, _, first) = decode_upto(&walk_avc1(&bytes), 0);
            (bytes.len(), yuv_psnr(&first, &source))
        };
        let (low_len, low_psnr) = run(10);
        let (high_len, high_psnr) = run(95);
        assert!(high_len > low_len, "{high_len} vs {low_len}");
        assert!(
            high_psnr > low_psnr + 3.0,
            "{high_psnr:.2} vs {low_psnr:.2}"
        );
    }

    /// W15-B: past OpenH264's limit the H.264 route refuses by name and
    /// points at AV1; the size gate is checked before any pixels are read.
    #[test]
    fn an_oversized_h264_export_names_the_av1_codec() {
        let px = vec![0u8; 16 * 4000 * 4];
        let frame = [Mp4Frame {
            rgba8: &px,
            duration_ms: 100,
        }];
        let err = encode(16, 4000, &frame, 50).unwrap_err().to_string();
        assert!(err.contains("AV1"), "{err}");
    }

    #[test]
    fn video_files_are_recognised_for_the_refusal() {
        assert!(looks_like_video(
            b"\x00\x00\x00\x18ftypmp42\x00\x00\x00\x00"
        ));
        assert!(looks_like_video(
            b"\x00\x00\x00\x14ftypqt  \x00\x00\x00\x00"
        ));
        assert!(looks_like_video(&[0x1A, 0x45, 0xDF, 0xA3, 0, 0, 0, 0]));
        assert!(looks_like_video(b"RIFF\x00\x00\x00\x00AVI LIST"));
        assert!(!looks_like_video(
            b"\x00\x00\x00\x1cftypavif\x00\x00\x00\x00"
        ));
        assert!(!looks_like_video(
            b"\x00\x00\x00\x1cftypheic\x00\x00\x00\x00"
        ));
        assert!(!looks_like_video(b"RIFF\x00\x00\x00\x00WEBPVP8 "));
        assert!(video_refusal().to_string().contains("video"));
    }
}
