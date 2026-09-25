//! W13-L: MP4 video export — AV1 in an ISO Base Media File (`.mp4`).
//!
//! # The encoder
//!
//! AV1 through `rav1e` 0.8.1 (BSD-2-Clause, pure Rust), the encoder that was
//! already in the tree behind `image`'s AVIF writer; it is named here
//! directly with default features off, so no `asm` (nasm) and no `threading`
//! (rayon) pool, and no C toolchain is needed. H.264 was not taken:
//! `openh264` binds Cisco's C library (a C compiler at build time) and the
//! pure-Rust H.264 encoders on crates.io are early releases this wave did not
//! evaluate.
//!
//! Frames are 8-bit 4:2:0, BT.709 limited range (tagged in the bitstream and
//! in a `colr` box), with transparency flattened onto white: a video has no
//! alpha. rav1e needs at least 16 x 16 pixels; a smaller frame is refused.
//!
//! # The container
//!
//! Written here box by box (no muxer crate): `ftyp`, then `moov` (one video
//! track: `tkhd`, `mdhd`, `hdlr`, `vmhd`, `dref`, and a sample table whose
//! `stsd` holds an `av01` entry with the encoder's own `av1C`), then one
//! `mdat` chunk. `moov` comes first, so a player can start before the whole
//! file arrives. Each frame is one sample with its own duration (`stts`, in
//! milliseconds), so an animation's per-frame delays survive exactly; the key
//! frames are listed in `stss`.
//!
//! [`probe`] reads the box structure back (size, sample count, durations,
//! codec) without decoding a picture: there is no AV1 *decoder* in this
//! build (see [`super::avif`]), and so no video import either — a video file
//! handed to File > Open is refused by name ([`video_refusal`]).

use rav1e::prelude::*;

use crate::codec::CodecError;

/// The smallest frame edge rav1e encodes (it refuses anything below 16).
pub const MIN_EDGE: u32 = 16;

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
    /// The sample entry's codec (`av01` for what this module writes).
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
}

/// `quality` (1..=100, like JPEG / AVIF) as rav1e's quantizer (255..=0).
fn quantizer(quality: u8) -> usize {
    let q = u32::from(quality.clamp(1, 100));
    ((100 - q) * 255 / 99) as usize
}

/// Encode `frames` (each `width * height * 4` bytes) as an MP4 of AV1.
pub fn encode(
    width: u32,
    height: u32,
    frames: &[Mp4Frame<'_>],
    quality: u8,
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
    let durations: Vec<u32> = frames.iter().map(|f| f.duration_ms.max(1)).collect();
    mux(width, height, &av1c, &samples, &durations)
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
    frame.planes[0].copy_from_raw_u8(&y, width, 1);
    frame.planes[1].copy_from_raw_u8(&chroma(&cb), cw, 1);
    frame.planes[2].copy_from_raw_u8(&chroma(&cr), cw, 1);
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

fn mux(
    width: u32,
    height: u32,
    av1c: &[u8],
    samples: &[(Vec<u8>, bool)],
    durations: &[u32],
) -> Result<Vec<u8>, CodecError> {
    let total: u64 = durations.iter().map(|d| u64::from(*d)).sum();
    let total = u32::try_from(total)
        .map_err(|_| CodecError::LimitExceeded("the video is too long".into()))?;
    let ftyp = bx(
        b"ftyp",
        &cat(&[
            b"isom".to_vec(),
            be32(&[0x200]),
            b"isomiso2av01mp41".to_vec(),
        ]),
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

        let mut entry = Vec::new();
        entry.extend_from_slice(&[0; 6]);
        entry.extend_from_slice(&1u16.to_be_bytes()); // data_reference_index
        entry.extend_from_slice(&[0; 16]); // pre_defined, reserved
        entry.extend_from_slice(&(width as u16).to_be_bytes());
        entry.extend_from_slice(&(height as u16).to_be_bytes());
        entry.extend_from_slice(&be32(&[0x0048_0000, 0x0048_0000, 0]));
        entry.extend_from_slice(&1u16.to_be_bytes()); // frame_count
        let mut name = [0u8; 32];
        let label = b"AV1 Coding";
        name[0] = label.len() as u8;
        name[1..=label.len()].copy_from_slice(label);
        entry.extend_from_slice(&name);
        entry.extend_from_slice(&0x0018u16.to_be_bytes());
        entry.extend_from_slice(&(-1i16).to_be_bytes());
        entry.extend_from_slice(&bx(b"av1C", av1c)?);
        // colr nclx: BT.709 primaries / transfer / matrix, limited range.
        entry.extend_from_slice(&bx(
            b"colr",
            &cat(&[b"nclx".to_vec(), vec![0, 1, 0, 1, 0, 1, 0]]),
        )?);
        let stsd = full(b"stsd", 0, &cat(&[be32(&[1]), bx(b"av01", &entry)?]))?;

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
    let has_av1c = entry
        .get(78..)
        .map(|rest| boxes(rest).is_ok_and(|b| b.iter().any(|(k, _)| k == b"av1C")))
        .unwrap_or(false);

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
        let bytes = encode(w, h, &input, 70).unwrap();
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
