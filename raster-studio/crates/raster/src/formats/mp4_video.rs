//! W16-M: MP4 video *decoding*, for video layers (photopea.com/learn/video:
//! a media file opened or added to the timeline becomes a video layer).
//!
//! Declared from [`super`] (`formats/mp4.rs`, with `#[path]`) and reached as
//! `raster::codec::formats::mp4::video`.
//!
//! # What decodes
//!
//! The first video track of an ISO BMFF file (`.mp4`, `.m4v`, `.mov`) whose
//! sample entry is
//!
//! - `avc1` (H.264 with an `avcC` box), through Cisco's OpenH264 decoder
//!   (the same `openh264` crate and compiled-from-source library the
//!   exporter encodes with, BSD-2-Clause). OpenH264 decodes the profiles
//!   OpenH264 supports; a stream it cannot decode is an error naming it;
//! - `av01` (AV1), through `rusty_av1d` (BSD-2-Clause), the decoder the
//!   AVIF reader already runs, 8-bit 4:2:0 only.
//!
//! Any other codec (HEVC `hvc1` / `hev1`, VP9 `vp09`, MPEG-4 Part 2 `mp4v`,
//! ...) is refused by name, as are Matroska / WebM and AVI containers.
//!
//! # Where it runs
//!
//! Both decoders are native code (OpenH264 is C++; rusty_av1d can panic on
//! damaged input and the release profile aborts on a panic), so the editor
//! never calls [`decode_in_this_process`] itself: `app-shell`'s decode
//! worker runs it in a child process (`--decode-worker video`) and a crash
//! there is an error in the editor, not an exit.
//!
//! # Bounds
//!
//! Before any picture is decoded: the sample tables are bounded by
//! [`super::probe`] (every sample inside the file, at most
//! [`super::MAX_PROBE_SAMPLES`]); the frame count by [`MAX_VIDEO_FRAMES`];
//! the sample entry's coded size by the import limits; and the RGBA8 bytes
//! every frame together would hold by [`max_video_bytes`]. Each decoded
//! picture is checked again (its size must be the first picture's, inside
//! the limits) before it is converted.
//!
//! # Colour
//!
//! 4:2:0 Y'CbCr is converted to RGBA8 with the BT.709 matrix (what the
//! exporter writes and what HD video uses), limited range unless an AV1
//! sequence header says full range. Every frame is opaque.
//!
//! # Audio
//!
//! Not decoded: an audio track (AAC, MP3) is ignored. This build has no
//! permissively licensed pure-Rust AAC decoder (symphonia's is MPL-2.0), and
//! MP4 export writes no audio track either.

use crate::codec::CodecError;
use crate::ImportLimits;

use super::{boxes, child, malformed, probe, u32_at};

/// The most frames a video layer holds (an animation's bound, 1000).
pub const MAX_VIDEO_FRAMES: usize = crate::animation::MAX_ANIMATION_FRAMES;

/// The most RGBA8 bytes all of a video's decoded frames may hold together:
/// an animation's bound ([`crate::animation::MAX_ANIMATION_BYTES`], 1 GiB),
/// or the limits' allocation ceiling when that is lower.
pub fn max_video_bytes(limits: ImportLimits) -> u64 {
    crate::animation::MAX_ANIMATION_BYTES.min(limits.max_alloc_bytes)
}

/// The video codecs this build decodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec {
    H264,
    Av1,
}

impl VideoCodec {
    pub fn name(self) -> &'static str {
        match self {
            VideoCodec::H264 => "H.264",
            VideoCodec::Av1 => "AV1",
        }
    }
}

/// One decoded frame: opaque straight-alpha RGBA8, and how long it shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoFrame {
    pub rgba8: Vec<u8>,
    pub duration_ms: u32,
}

/// A decoded video track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedVideo {
    pub width: u32,
    pub height: u32,
    pub codec: VideoCodec,
    pub frames: Vec<VideoFrame>,
}

impl DecodedVideo {
    /// The whole length, in milliseconds.
    pub fn duration_ms(&self) -> u64 {
        self.frames.iter().map(|f| u64::from(f.duration_ms)).sum()
    }
}

/// The first video track's codec, configuration box and samples.
struct Track<'a> {
    codec: VideoCodec,
    config: &'a [u8],
    samples: Vec<&'a [u8]>,
    durations_ms: Vec<u32>,
    coded: (u32, u32),
}

/// The first sample entry's kind and payload, and the track's `stbl`.
type SampleTable<'a> = ([u8; 4], &'a [u8], &'a [u8]);

/// `(kind, payload)` of the first sample entry of the first video track,
/// and that track's sample table.
fn sample_table(bytes: &[u8]) -> Result<SampleTable<'_>, CodecError> {
    let top = boxes(bytes)?;
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
    let stbl = child(child(child(trak, b"mdia")?, b"minf")?, b"stbl")?;
    let stsd = child(stbl, b"stsd")?;
    let entries = boxes(stsd.get(8..).ok_or_else(|| malformed("stsd"))?)?;
    let (kind, entry) = *entries.first().ok_or_else(|| malformed("stsd is empty"))?;
    Ok((kind, entry, stbl))
}

/// Read the track: [`probe`] first (which checks that every sample lies in
/// the file and bounds the count), then the sample byte ranges.
fn track(bytes: &[u8]) -> Result<Track<'_>, CodecError> {
    let info = probe(bytes)?;
    let (kind, entry, stbl) = sample_table(bytes)?;
    let codec = match &kind {
        b"avc1" => VideoCodec::H264,
        b"av01" => VideoCodec::Av1,
        other => {
            let name = String::from_utf8_lossy(other).trim().to_string();
            let what = match other {
                b"hvc1" | b"hev1" => "HEVC (H.265)",
                b"vp09" | b"vp08" => "VP9 / VP8",
                b"mp4v" => "MPEG-4 Part 2",
                b"avc3" => "H.264 with in-band parameter sets",
                _ => "this codec",
            };
            return Err(CodecError::Unsupported(format!(
                "the video track is {what} (`{name}`); video layers decode H.264 (`avc1`) and \
                 AV1 (`av01`) only"
            )));
        }
    };
    let config_kind = match codec {
        VideoCodec::H264 => b"avcC",
        VideoCodec::Av1 => b"av1C",
    };
    let config = entry
        .get(78..)
        .and_then(|rest| {
            boxes(rest)
                .ok()?
                .into_iter()
                .find(|(k, _)| k == config_kind)
                .map(|b| b.1)
        })
        .ok_or_else(|| {
            malformed(format!(
                "the {} sample entry has no {} box",
                codec.name(),
                String::from_utf8_lossy(config_kind)
            ))
        })?;
    if info.frame_count as usize > MAX_VIDEO_FRAMES {
        return Err(CodecError::LimitExceeded(format!(
            "the video has {} frames, more than the {MAX_VIDEO_FRAMES} a video layer holds",
            info.frame_count
        )));
    }

    // The sample ranges, walked as `probe` walked them (it has checked each
    // one lies inside the file).
    let stsz = child(stbl, b"stsz")?;
    let uniform = u32_at(stsz, 4)?;
    let size = |i: usize| -> Result<u32, CodecError> {
        if uniform != 0 {
            Ok(uniform)
        } else {
            u32_at(stsz, 12 + i * 4)
        }
    };
    let stco = child(stbl, b"stco")?;
    let stsc = child(stbl, b"stsc")?;
    let runs: Vec<(u32, u32)> = (0..u32_at(stsc, 4)? as usize)
        .map(|i| Ok((u32_at(stsc, 8 + i * 12)?, u32_at(stsc, 12 + i * 12)?)))
        .collect::<Result<_, CodecError>>()?;
    let count = info.frame_count as usize;
    let mut samples = Vec::with_capacity(count);
    for c in 0..u32_at(stco, 4)? as usize {
        let chunk_no = c as u32 + 1;
        let per = runs
            .iter()
            .rev()
            .find(|(first, _)| *first <= chunk_no)
            .map(|r| r.1)
            .ok_or_else(|| malformed("stsc does not cover every chunk"))?;
        let mut at = u32_at(stco, 8 + c * 4)? as usize;
        for _ in 0..per {
            if samples.len() >= count {
                return Err(malformed("stsc names more samples than stsz"));
            }
            let n = size(samples.len())? as usize;
            let end = at
                .checked_add(n)
                .filter(|e| *e <= bytes.len())
                .ok_or_else(|| malformed("a sample lies past the end of the file"))?;
            samples.push(&bytes[at..end]);
            at = end;
        }
    }
    if samples.len() != count {
        return Err(malformed("the chunks do not hold every sample"));
    }
    let timescale = u64::from(info.timescale.max(1));
    let durations_ms = info
        .durations
        .iter()
        .map(|d| (u64::from(*d) * 1000 / timescale).clamp(1, u64::from(u32::MAX)) as u32)
        .collect();
    Ok(Track {
        codec,
        config,
        samples,
        durations_ms,
        coded: (info.coded_width, info.coded_height),
    })
}

fn check_size(limits: ImportLimits, width: u32, height: u32) -> Result<(), CodecError> {
    if width == 0 || height == 0 {
        return Err(malformed(format!("an empty {width}x{height} picture")));
    }
    if width > limits.max_width
        || height > limits.max_height
        || u64::from(width) * u64::from(height) > limits.max_pixels
    {
        return Err(CodecError::LimitExceeded(format!(
            "the video is {width}x{height}, past the {}x{} ({} pixel) limit",
            limits.max_width, limits.max_height, limits.max_pixels
        )));
    }
    Ok(())
}

/// Refuse `count` frames of `width x height` past [`max_video_bytes`].
fn check_budget(
    limits: ImportLimits,
    width: u32,
    height: u32,
    count: usize,
) -> Result<(), CodecError> {
    let bytes = u64::from(width) * u64::from(height) * 4 * count as u64;
    let budget = max_video_bytes(limits);
    if bytes > budget {
        return Err(CodecError::LimitExceeded(format!(
            "the video is {count} frames of {width}x{height} ({} MiB decoded), more than the \
             {} MiB a video layer holds; trim it or lower its resolution first",
            bytes >> 20,
            budget >> 20
        )));
    }
    Ok(())
}

/// Decode `bytes` (an MP4) here, in this process: every frame. Only the
/// decode worker calls this in the application (see the module docs).
pub fn decode_in_this_process(
    bytes: &[u8],
    limits: ImportLimits,
) -> Result<DecodedVideo, CodecError> {
    let window = decode_window_in_this_process(bytes, limits, 0, usize::MAX)?;
    Ok(DecodedVideo {
        width: window.width,
        height: window.height,
        codec: window.codec,
        frames: window.frames,
    })
}

/// W16-M: part of a video track, decoded on demand for the timeline time:
/// the whole track's frame timing, and the pictures of the frames
/// `first..first + frames.len()` only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoWindow {
    pub width: u32,
    pub height: u32,
    pub codec: VideoCodec,
    /// Every frame's duration, in milliseconds: the whole track's.
    pub durations_ms: Vec<u32>,
    /// The track index of `frames[0]`.
    pub first: usize,
    /// The decoded frames `first..first + frames.len()`.
    pub frames: Vec<VideoFrame>,
}

/// W16-M: decode the frames `first..first + count` of `bytes` (an MP4)
/// here, in this process (clamped to the track's end), for a video layer
/// that decodes on demand. The stream is decoded from its start up to the
/// window's last frame (a frame depends on the ones before it) and only the
/// window's pictures are kept, so the decoded-bytes budget
/// ([`max_video_bytes`]) bounds the window, not the whole video. Only the
/// decode worker calls this in the application (see the module docs).
pub fn decode_window_in_this_process(
    bytes: &[u8],
    limits: ImportLimits,
    first: usize,
    count: usize,
) -> Result<VideoWindow, CodecError> {
    let track = track(bytes)?;
    let total = track.samples.len();
    if total == 0 {
        return Err(malformed("the video holds no picture"));
    }
    if first >= total {
        return Err(malformed(format!(
            "frame {first} is past the video's {total} frames"
        )));
    }
    let end = first.saturating_add(count.max(1)).min(total);
    let want = end - first;
    let (cw, ch) = track.coded;
    check_size(limits, cw, ch)?;
    check_budget(limits, cw, ch, want)?;
    let mut frames: Vec<VideoFrame> = Vec::with_capacity(want);
    let mut size: Option<(u32, u32)> = None;
    let mut seen = 0usize;
    let durations = &track.durations_ms;
    let mut push = |w: u32, h: u32, rgba8: Vec<u8>| -> Result<(), CodecError> {
        match size {
            None => {
                check_size(limits, w, h)?;
                check_budget(limits, w, h, want)?;
                size = Some((w, h));
            }
            Some(s) if s != (w, h) => {
                return Err(malformed(format!(
                    "the picture size changes mid-stream ({}x{} to {w}x{h})",
                    s.0, s.1
                )))
            }
            Some(_) => {}
        }
        let index = seen;
        seen += 1;
        let Some(&duration_ms) = durations.get(index).filter(|_| index < end) else {
            return Err(malformed("the stream holds more pictures than samples"));
        };
        if index >= first {
            frames.push(VideoFrame { rgba8, duration_ms });
        }
        Ok(())
    };
    let samples = &track.samples[..end];
    match track.codec {
        VideoCodec::H264 => decode_h264(track.config, samples, limits, &mut push)?,
        VideoCodec::Av1 => decode_av1(samples, limits, &mut push)?,
    }
    let (width, height) = size.ok_or_else(|| malformed("the video holds no picture"))?;
    if seen != end {
        return Err(malformed(format!("{seen} of the {end} frames decoded")));
    }
    Ok(VideoWindow {
        width,
        height,
        codec: track.codec,
        durations_ms: track.durations_ms.clone(),
        first,
        frames,
    })
}

/// 4:2:0 planes to opaque RGBA8 through the BT.709 matrix, limited range
/// (`16..=235` luma, `16..=240` chroma) unless `full`.
#[allow(clippy::too_many_arguments)]
pub fn yuv420_to_rgba8(
    width: usize,
    height: usize,
    y: &[u8],
    y_stride: usize,
    u: &[u8],
    v: &[u8],
    c_stride: usize,
    full: bool,
) -> Result<Vec<u8>, CodecError> {
    let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
    let fits = |plane: &[u8], stride: usize, w: usize, h: usize| {
        h == 0 || (stride >= w && (h - 1) * stride + w <= plane.len())
    };
    if !fits(y, y_stride, width, height) || !fits(u, c_stride, cw, ch) || !fits(v, c_stride, cw, ch)
    {
        return Err(malformed("a decoded plane is shorter than its size"));
    }
    let (y_off, y_scale, c_scale) = if full {
        (0.0, 255.0, 255.0)
    } else {
        (16.0, 219.0, 224.0)
    };
    let mut out = vec![0u8; width * height * 4];
    for row in 0..height {
        for col in 0..width {
            let l = (f32::from(y[row * y_stride + col]) - y_off) / y_scale;
            let ci = (row / 2) * c_stride + col / 2;
            let cb = (f32::from(u[ci]) - 128.0) / c_scale;
            let cr = (f32::from(v[ci]) - 128.0) / c_scale;
            let r = l + 1.5748 * cr;
            let b = l + 1.8556 * cb;
            let g = (l - 0.2126 * r - 0.0722 * b) / 0.7152;
            let to8 = |c: f32| (c * 255.0).round().clamp(0.0, 255.0) as u8;
            let p = &mut out[(row * width + col) * 4..][..4];
            p.copy_from_slice(&[to8(r), to8(g), to8(b), 255]);
        }
    }
    Ok(out)
}

/// The SPS and PPS NAL units of an `avcC`, and its NAL length size.
fn avcc_parameter_sets(avcc: &[u8]) -> Result<(Vec<&[u8]>, usize), CodecError> {
    let bad = || malformed("the avcC box is damaged");
    if avcc.len() < 7 || avcc[0] != 1 {
        return Err(bad());
    }
    let length_size = usize::from(avcc[4] & 0x03) + 1;
    if length_size == 3 {
        return Err(bad());
    }
    let mut sets = Vec::new();
    let mut at = 5;
    for mask in [0x1F, 0xFF] {
        let n = *avcc.get(at).ok_or_else(bad)? & mask;
        at += 1;
        for _ in 0..n {
            let len = avcc
                .get(at..at + 2)
                .map(|b| usize::from(u16::from_be_bytes([b[0], b[1]])))
                .ok_or_else(bad)?;
            sets.push(avcc.get(at + 2..at + 2 + len).ok_or_else(bad)?);
            at += 2 + len;
        }
    }
    if sets.is_empty() {
        return Err(bad());
    }
    Ok((sets, length_size))
}

/// Append a length-prefixed sample's NAL units to `out` as Annex B.
fn sample_to_annex_b(
    sample: &[u8],
    length_size: usize,
    out: &mut Vec<u8>,
) -> Result<(), CodecError> {
    let mut at = 0;
    while at < sample.len() {
        let head = sample
            .get(at..at + length_size)
            .ok_or_else(|| malformed("a NAL length runs past its sample"))?;
        let n = head
            .iter()
            .fold(0usize, |acc, b| (acc << 8) | usize::from(*b));
        let body = sample
            .get(at + length_size..at + length_size + n)
            .ok_or_else(|| malformed("a NAL unit runs past its sample"))?;
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(body);
        at += length_size + n;
    }
    Ok(())
}

type Push<'a> = dyn FnMut(u32, u32, Vec<u8>) -> Result<(), CodecError> + 'a;

fn decode_h264(
    avcc: &[u8],
    samples: &[&[u8]],
    limits: ImportLimits,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    use openh264::decoder::{DecodedYUV, Decoder};
    use openh264::formats::YUVSource;
    let (sets, length_size) = avcc_parameter_sets(avcc)?;
    let fail = |e: openh264::Error| {
        CodecError::Unsupported(format!(
            "the H.264 decoder (OpenH264) could not decode this video: {e}"
        ))
    };
    let mut decoder = Decoder::new().map_err(fail)?;
    let convert = |yuv: &DecodedYUV<'_>| -> Result<(u32, u32, Vec<u8>), CodecError> {
        let (w, h) = yuv.dimensions();
        let (sy, su, sv) = yuv.strides();
        if su != sv {
            return Err(malformed("the chroma planes have different strides"));
        }
        let (w32, h32) = (
            u32::try_from(w).unwrap_or(u32::MAX),
            u32::try_from(h).unwrap_or(u32::MAX),
        );
        check_size(limits, w32, h32)?;
        let rgba = yuv420_to_rgba8(w, h, yuv.y(), sy, yuv.u(), yuv.v(), su, false)?;
        Ok((w32, h32, rgba))
    };
    let mut annex_b = Vec::new();
    for set in &sets {
        annex_b.extend_from_slice(&[0, 0, 0, 1]);
        annex_b.extend_from_slice(set);
    }
    for sample in samples {
        sample_to_annex_b(sample, length_size, &mut annex_b)?;
        if let Some(yuv) = decoder.decode(&annex_b).map_err(fail)? {
            let (w, h, rgba) = convert(&yuv)?;
            push(w, h, rgba)?;
        }
        annex_b.clear();
    }
    for yuv in decoder.flush_remaining().map_err(fail)? {
        let (w, h, rgba) = convert(&yuv)?;
        push(w, h, rgba)?;
    }
    Ok(())
}

fn decode_av1(
    samples: &[&[u8]],
    limits: ImportLimits,
    push: &mut Push<'_>,
) -> Result<(), CodecError> {
    use rusty_av1d::{Decoder, PlanarImageComponent, Rav1dError, Settings};
    let fail = |e: Rav1dError| {
        CodecError::Unsupported(format!("the AV1 decoder could not decode this video: {e}"))
    };
    let mut settings = Settings::new();
    settings.set_n_threads(1);
    settings.set_max_frame_delay(1);
    settings.set_frame_size_limit(u32::try_from(limits.max_pixels).unwrap_or(u32::MAX));
    let mut decoder = Decoder::with_settings(&settings).map_err(fail)?;
    let mut take = |picture: rusty_av1d::Picture| -> Result<(), CodecError> {
        if picture.bit_depth() != 8 || picture.pixel_layout() != rusty_av1d::PixelLayout::I420 {
            return Err(CodecError::Unsupported(
                "video layers decode 8-bit 4:2:0 AV1 only".into(),
            ));
        }
        let (w, h) = (picture.width(), picture.height());
        check_size(limits, w, h)?;
        let full = matches!(picture.color_range(), rusty_av1d::pixel::YUVRange::Full);
        let rgba = yuv420_to_rgba8(
            w as usize,
            h as usize,
            picture.plane(PlanarImageComponent::Y),
            picture.stride(PlanarImageComponent::Y) as usize,
            picture.plane(PlanarImageComponent::U),
            picture.plane(PlanarImageComponent::V),
            picture.stride(PlanarImageComponent::U) as usize,
            full,
        )?;
        push(w, h, rgba)
    };
    // Each round either takes a picture or feeds pending data; the bound
    // only stops a stream that never produces one from spinning.
    let rounds = samples.len().saturating_mul(8).saturating_add(64);
    let mut next = 0usize;
    let mut sent_all = false;
    for _ in 0..rounds {
        match decoder.get_picture() {
            Ok(p) => take(p)?,
            Err(Rav1dError::TryAgain) => {
                match decoder.send_pending_data() {
                    Ok(()) | Err(Rav1dError::TryAgain) => {}
                    Err(e) => return Err(fail(e)),
                }
                if let Some(sample) = samples.get(next) {
                    match decoder.send_data(sample.to_vec().into_boxed_slice(), None, None, None) {
                        Ok(()) | Err(Rav1dError::TryAgain) => {}
                        Err(e) => return Err(fail(e)),
                    }
                    next += 1;
                } else if sent_all {
                    return Ok(());
                } else {
                    sent_all = true;
                }
            }
            Err(e) => return Err(fail(e)),
        }
    }
    Err(malformed("the AV1 stream did not finish decoding"))
}

#[cfg(test)]
mod tests {
    use super::super::{encode, encode_with, Mp4Codec, Mp4Frame};
    use super::*;

    /// `n` opaque `w x h` frames, each a different flat colour in its left
    /// half and a grey right half, so a frame can be told apart by a pixel.
    fn clip(w: u32, h: u32, n: usize) -> Vec<Vec<u8>> {
        (0..n)
            .map(|i| {
                let mut px = Vec::with_capacity((w * h * 4) as usize);
                for _ in 0..h {
                    for x in 0..w {
                        if x < w / 2 {
                            px.extend_from_slice(&[
                                (40 + i * 60) as u8,
                                200 - (i * 50) as u8,
                                60,
                                255,
                            ]);
                        } else {
                            px.extend_from_slice(&[128, 128, 128, 255]);
                        }
                    }
                }
                px
            })
            .collect()
    }

    fn close(a: &[u8], b: &[u8], tolerance: i32) -> bool {
        a.iter()
            .zip(b)
            .all(|(x, y)| (i32::from(*x) - i32::from(*y)).abs() <= tolerance)
    }

    fn at(rgba: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * w + x) * 4) as usize;
        [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
    }

    /// An H.264 MP4 the exporter wrote decodes back to its frames: the
    /// count, the size, the durations, and each frame's colours.
    #[test]
    fn an_exported_h264_mp4_decodes_back_to_its_frames() {
        let (w, h) = (48, 32);
        let px = clip(w, h, 4);
        let delays = [100, 250, 100, 400];
        let input: Vec<Mp4Frame<'_>> = px
            .iter()
            .zip(delays)
            .map(|(p, d)| Mp4Frame {
                rgba8: p,
                duration_ms: d,
            })
            .collect();
        let bytes = encode(w, h, &input, 90).unwrap();
        let video = decode_in_this_process(&bytes, ImportLimits::default()).unwrap();
        assert_eq!(video.codec, VideoCodec::H264);
        assert_eq!((video.width, video.height), (w, h));
        assert_eq!(video.frames.len(), 4);
        assert_eq!(
            video
                .frames
                .iter()
                .map(|f| f.duration_ms)
                .collect::<Vec<_>>(),
            delays.to_vec()
        );
        assert_eq!(video.duration_ms(), 850);
        for (i, f) in video.frames.iter().enumerate() {
            let got = at(&f.rgba8, w, 8, 16);
            let want = at(&px[i], w, 8, 16);
            assert!(close(&got, &want, 8), "frame {i}: {got:?} vs {want:?}");
            assert!(close(&at(&f.rgba8, w, 40, 16), &[128, 128, 128, 255], 8));
            assert_eq!(got[3], 255, "opaque");
        }
    }

    /// W16-M: a window decodes only its frames: the whole track's timing,
    /// and the pictures `first..first + count` (clamped to the end), the
    /// same pictures a whole decode gives; a window past the end is an
    /// error, and the budget bounds the window, not the whole video.
    #[test]
    fn a_window_decodes_only_its_frames() {
        let (w, h) = (32, 32);
        let px = clip(w, h, 4);
        let input: Vec<Mp4Frame<'_>> = px
            .iter()
            .zip([100, 150, 200, 250])
            .map(|(p, d)| Mp4Frame {
                rgba8: p,
                duration_ms: d,
            })
            .collect();
        let bytes = encode(w, h, &input, 90).unwrap();
        let limits = ImportLimits::default();
        let win = decode_window_in_this_process(&bytes, limits, 2, 5).unwrap();
        assert_eq!(win.durations_ms, vec![100, 150, 200, 250]);
        assert_eq!((win.first, win.frames.len()), (2, 2), "clamped to the end");
        for (k, f) in win.frames.iter().enumerate() {
            let got = at(&f.rgba8, w, 4, 4);
            let want = at(&px[2 + k], w, 4, 4);
            assert!(
                close(&got, &want, 8),
                "frame {}: {got:?} vs {want:?}",
                2 + k
            );
        }
        let one = decode_window_in_this_process(&bytes, limits, 1, 1).unwrap();
        assert_eq!((one.first, one.frames.len()), (1, 1));
        assert_eq!(one.frames[0].duration_ms, 150);
        assert!(decode_window_in_this_process(&bytes, limits, 4, 1).is_err());
        // Two frames fit a budget that four do not.
        let tight = ImportLimits {
            max_alloc_bytes: u64::from(w * h * 4) * 2,
            ..limits
        };
        assert!(decode_in_this_process(&bytes, tight).is_err());
        assert_eq!(
            decode_window_in_this_process(&bytes, tight, 0, 2)
                .unwrap()
                .frames
                .len(),
            2
        );
    }

    /// The AV1 option decodes back too (through rusty_av1d).
    #[test]
    fn an_exported_av1_mp4_decodes_back_to_its_frames() {
        let (w, h) = (32, 32);
        let px = clip(w, h, 3);
        let input: Vec<Mp4Frame<'_>> = px
            .iter()
            .map(|p| Mp4Frame {
                rgba8: p,
                duration_ms: 200,
            })
            .collect();
        let bytes = encode_with(w, h, &input, 95, Mp4Codec::Av1).unwrap();
        let video = decode_in_this_process(&bytes, ImportLimits::default()).unwrap();
        assert_eq!(video.codec, VideoCodec::Av1);
        assert_eq!((video.width, video.height, video.frames.len()), (w, h, 3));
        for (i, f) in video.frames.iter().enumerate() {
            let got = at(&f.rgba8, w, 4, 4);
            let want = at(&px[i], w, 4, 4);
            assert!(close(&got, &want, 10), "frame {i}: {got:?} vs {want:?}");
        }
    }

    /// Limits are checked before a picture is decoded, and a damaged file
    /// is an error (this runs the decoders in this test process, so a
    /// crash would fail the run; the application runs them in the worker).
    #[test]
    fn limits_and_damage_are_errors() {
        let px = clip(32, 32, 3);
        let input: Vec<Mp4Frame<'_>> = px
            .iter()
            .map(|p| Mp4Frame {
                rgba8: p,
                duration_ms: 100,
            })
            .collect();
        let bytes = encode(32, 32, &input, 80).unwrap();
        let small = ImportLimits {
            max_width: 16,
            ..ImportLimits::default()
        };
        let err = decode_in_this_process(&bytes, small).unwrap_err();
        assert!(matches!(err, CodecError::LimitExceeded(_)), "{err}");
        let tight = ImportLimits {
            max_alloc_bytes: 32 * 32 * 4 * 2,
            ..ImportLimits::default()
        };
        let err = decode_in_this_process(&bytes, tight).unwrap_err();
        assert!(err.to_string().contains("3 frames"), "{err}");
        for cut in [0, 10, bytes.len() / 3, bytes.len() - 1] {
            assert!(decode_in_this_process(&bytes[..cut], ImportLimits::default()).is_err());
        }
        assert!(decode_in_this_process(b"not a video at all", ImportLimits::default()).is_err());
        // A codec this build does not decode is refused by name.
        let mut hevc = bytes.clone();
        let i = hevc.windows(4).position(|w| w == b"avc1").unwrap();
        // The first `avc1` is the ftyp brand; the sample entry follows.
        let j = i + 4 + hevc[i + 4..].windows(4).position(|w| w == b"avc1").unwrap();
        hevc[j..j + 4].copy_from_slice(b"hvc1");
        let err = decode_in_this_process(&hevc, ImportLimits::default()).unwrap_err();
        assert!(err.to_string().contains("HEVC"), "{err}");
    }

    #[test]
    fn bt709_limited_round_trips_through_the_exporters_conversion() {
        let (w, h) = (4usize, 2usize);
        // Pixel pairs share chroma, so use a flat colour per 2x2 block.
        let flat: Vec<u8> = (0..w * h)
            .flat_map(|i| {
                if (i % w) < 2 {
                    [250u8, 20, 30, 255]
                } else {
                    [10, 240, 20, 255]
                }
            })
            .collect();
        let [y, u, v] = super::super::rgba_to_yuv420(&flat, w, h);
        let back = yuv420_to_rgba8(w, h, &y, w, &u, &v, w / 2, false).unwrap();
        assert!(close(&back, &flat, 3), "{back:?}");
        assert!(yuv420_to_rgba8(w, h, &y[..3], w, &u, &v, w / 2, false).is_err());
    }
}
