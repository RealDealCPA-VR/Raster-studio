//! W18-H: Matroska / WebM (`.mkv`, `.webm`) video *demuxing*, for video
//! layers: the first video track's frames, handed to the decoders the MP4
//! reader already runs (`rusty_av1d` for AV1, OpenH264 for H.264).
//!
//! A child of [`super`] (`mp4_video.rs`, declared there with `#[path]`):
//! [`super::decode_window_in_this_process`] asks [`looks_like_matroska`]
//! first and reads a Matroska file's track here, so everything after the
//! demux (the decode worker, the window, the budget, the colour conversion)
//! is the MP4 route's, and the application reaches it wherever it decodes a
//! video (File > Open, File > Place, the timeline's Add Media).
//!
//! # What demuxes
//!
//! The EBML header's DocType must be `matroska` or `webm`. The first track
//! whose `TrackType` is video, with `CodecID`:
//!
//! - `V_AV1` (AV1: each block is a temporal unit, as in an MP4 sample);
//! - `V_MPEG4/ISO/AVC` (H.264: `CodecPrivate` is the `avcC` record and each
//!   block holds length-prefixed NAL units, as in an MP4 sample).
//!
//! VP8 / VP9 (`V_VP8`, `V_VP9`, what most WebM files hold), HEVC and every
//! other codec are refused by name: this build has no permissively licensed
//! VP8 / VP9 decoder. A track with `ContentEncodings` (compressed or
//! encrypted blocks) and laced blocks are refused by name too.
//!
//! Frames are `SimpleBlock`s and `BlockGroup` `Block`s of that track in the
//! file's order (AV1 and H.264 as muxers write them: decode order). A
//! frame's duration is the gap to the next frame's timestamp (cluster
//! `Timecode` plus the block's offset, times `TimecodeScale`); the last
//! frame takes the track's `DefaultDuration`, else the gap before it, else
//! 40 ms.
//!
//! # Untrusted input
//!
//! Every element size is checked against the bytes that hold it before it
//! is followed; an unknown size is allowed only on the Segment and a
//! Cluster (live WebM), where it runs to the next top-level element; the
//! element walk, the nesting depth and the frame count are bounded (more
//! frames than a video layer holds is refused before any is decoded). No
//! allocation depends on a declared size: frames are slices of the file.

use crate::codec::CodecError;

use super::{malformed, Track, VideoCodec, MAX_VIDEO_FRAMES};

const EBML: u32 = 0x1A45_DFA3;
const DOC_TYPE: u32 = 0x4282;
const SEGMENT: u32 = 0x1853_8067;
const INFO: u32 = 0x1549_A966;
const TIMECODE_SCALE: u32 = 0x2A_D7B1;
const TRACKS: u32 = 0x1654_AE6B;
const TRACK_ENTRY: u32 = 0xAE;
const TRACK_NUMBER: u32 = 0xD7;
const TRACK_TYPE: u32 = 0x83;
const CODEC_ID: u32 = 0x86;
const CODEC_PRIVATE: u32 = 0x63A2;
const DEFAULT_DURATION: u32 = 0x23_E383;
const CONTENT_ENCODINGS: u32 = 0x6D80;
const VIDEO: u32 = 0xE0;
const PIXEL_WIDTH: u32 = 0xB0;
const PIXEL_HEIGHT: u32 = 0xBA;
const CLUSTER: u32 = 0x1F43_B675;
const CLUSTER_TIMECODE: u32 = 0xE7;
const SIMPLE_BLOCK: u32 = 0xA3;
const BLOCK_GROUP: u32 = 0xA0;
const BLOCK: u32 = 0xA1;

/// The top-level (Segment child) elements: where an unknown-size Cluster
/// ends.
const LEVEL1: [u32; 8] = [
    CLUSTER,
    0x1C53_BB6B, // Cues
    0x1254_C367, // Tags
    0x1941_A469, // Attachments
    0x1043_A770, // Chapters
    0x114D_9B74, // SeekHead
    INFO,
    TRACKS,
];

/// More elements than any real file's Segment walks; a bound on a hostile
/// loop of empty elements.
const MAX_ELEMENTS: usize = 1 << 22;

/// `true` when `head` opens with the EBML magic (Matroska, WebM).
pub fn looks_like_matroska(head: &[u8]) -> bool {
    head.starts_with(&[0x1A, 0x45, 0xDF, 0xA3])
}

/// An EBML element: its id, its payload and whether its size was unknown
/// (the payload then runs to the end of its parent).
struct Element<'a> {
    id: u32,
    data: &'a [u8],
    unknown: bool,
    /// The offset just past the element (past its header when the size is
    /// unknown), relative to the slice it was read from.
    end: usize,
    /// The offset of its payload.
    start: usize,
}

/// A variable-length integer at `at`: `(value with the marker stripped,
/// length, all value bits set)`.
fn vint(bytes: &[u8], at: usize, max_len: usize) -> Result<(u64, usize, bool), CodecError> {
    let first = *bytes
        .get(at)
        .ok_or_else(|| malformed("a Matroska element ends early"))?;
    let len = first.leading_zeros() as usize + 1;
    if len > max_len {
        return Err(malformed("a Matroska element has a bad length marker"));
    }
    let raw = bytes
        .get(at..at + len)
        .ok_or_else(|| malformed("a Matroska element ends early"))?;
    let mask = if len == 8 { 0 } else { 0xFFu8 >> len };
    let mut value = u64::from(first & mask);
    for b in &raw[1..] {
        value = (value << 8) | u64::from(*b);
    }
    let all_ones = value == (1u64 << (7 * len)) - 1;
    Ok((value, len, all_ones))
}

/// The element at `at` in `bytes`.
fn element(bytes: &[u8], at: usize) -> Result<Element<'_>, CodecError> {
    let first = *bytes
        .get(at)
        .ok_or_else(|| malformed("a Matroska element ends early"))?;
    let id_len = first.leading_zeros() as usize + 1;
    if id_len > 4 {
        return Err(malformed("a Matroska element has a bad id"));
    }
    let id_bytes = bytes
        .get(at..at + id_len)
        .ok_or_else(|| malformed("a Matroska element ends early"))?;
    let id = id_bytes.iter().fold(0u32, |v, b| (v << 8) | u32::from(*b));
    let (size, size_len, unknown) = vint(bytes, at + id_len, 8)?;
    let start = at + id_len + size_len;
    if unknown {
        return Ok(Element {
            id,
            data: &bytes[start..],
            unknown: true,
            end: start,
            start,
        });
    }
    let end = usize::try_from(size)
        .ok()
        .and_then(|s| start.checked_add(s))
        .filter(|e| *e <= bytes.len())
        .ok_or_else(|| malformed("a Matroska element runs past its parent"))?;
    Ok(Element {
        id,
        data: &bytes[start..end],
        unknown: false,
        end,
        start,
    })
}

/// Every child of `data`, in order; an unknown size is refused here (only
/// the Segment and a Cluster may have one, and they are walked apart).
fn children(data: &[u8]) -> Result<Vec<Element<'_>>, CodecError> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < data.len() {
        if out.len() >= MAX_ELEMENTS {
            return Err(CodecError::LimitExceeded(
                "a Matroska element holds too many children".into(),
            ));
        }
        let e = element(data, at)?;
        if e.unknown {
            return Err(malformed("a Matroska element has an unknown size"));
        }
        at = e.end;
        out.push(e);
    }
    Ok(out)
}

fn uint(data: &[u8]) -> Result<u64, CodecError> {
    if data.len() > 8 {
        return Err(malformed("a Matroska integer is longer than 8 bytes"));
    }
    Ok(data.iter().fold(0u64, |v, b| (v << 8) | u64::from(*b)))
}

fn child<'a, 'b>(list: &'b [Element<'a>], id: u32) -> Option<&'b Element<'a>> {
    list.iter().find(|e| e.id == id)
}

/// The chosen track's facts, from `Tracks`.
struct VideoTrack<'a> {
    number: u64,
    codec: VideoCodec,
    config: &'a [u8],
    coded: (u32, u32),
    default_ns: Option<u64>,
}

fn video_track<'a>(tracks: &'a [u8]) -> Result<VideoTrack<'a>, CodecError> {
    for entry in children(tracks)?
        .into_iter()
        .filter(|e| e.id == TRACK_ENTRY)
    {
        let fields = children(entry.data)?;
        let kind = child(&fields, TRACK_TYPE)
            .map(|e| uint(e.data))
            .transpose()?;
        if kind != Some(1) {
            continue;
        }
        let number = child(&fields, TRACK_NUMBER)
            .map(|e| uint(e.data))
            .transpose()?
            .ok_or_else(|| malformed("the video track has no number"))?;
        let id = child(&fields, CODEC_ID)
            .map(|e| {
                String::from_utf8_lossy(e.data)
                    .trim_end_matches('\0')
                    .to_string()
            })
            .unwrap_or_default();
        let codec = match id.as_str() {
            "V_AV1" => VideoCodec::Av1,
            "V_MPEG4/ISO/AVC" => VideoCodec::H264,
            other => {
                let what = match other {
                    "V_VP9" => "VP9",
                    "V_VP8" => "VP8",
                    "V_MPEGH/ISO/HEVC" => "HEVC (H.265)",
                    "V_THEORA" => "Theora",
                    _ => "this codec",
                };
                return Err(CodecError::Unsupported(format!(
                    "the video track is {what} (`{other}`); video layers decode AV1 (`V_AV1`) \
                     and H.264 (`V_MPEG4/ISO/AVC`) from Matroska / WebM only"
                )));
            }
        };
        if child(&fields, CONTENT_ENCODINGS).is_some() {
            return Err(CodecError::Unsupported(
                "the video track's blocks are compressed or encrypted (ContentEncodings), \
                 which video layers do not read"
                    .into(),
            ));
        }
        let config = child(&fields, CODEC_PRIVATE).map_or(&[][..], |e| e.data);
        if codec == VideoCodec::H264 && config.is_empty() {
            return Err(malformed("the H.264 track has no CodecPrivate (avcC)"));
        }
        let video = child(&fields, VIDEO)
            .map(|e| children(e.data))
            .transpose()?
            .unwrap_or_default();
        let dim = |id| -> Result<u32, CodecError> {
            let v = child(&video, id)
                .map(|e| uint(e.data))
                .transpose()?
                .unwrap_or(0);
            Ok(u32::try_from(v).unwrap_or(u32::MAX))
        };
        let coded = (dim(PIXEL_WIDTH)?, dim(PIXEL_HEIGHT)?);
        let default_ns = child(&fields, DEFAULT_DURATION)
            .map(|e| uint(e.data))
            .transpose()?
            .filter(|d| *d > 0);
        return Ok(VideoTrack {
            number,
            codec,
            config,
            coded,
            default_ns,
        });
    }
    Err(malformed("no video track"))
}

/// One block's frame when it belongs to `track`: `(relative timecode,
/// frame)`.
fn block(data: &[u8], track: u64) -> Result<Option<(i16, &[u8])>, CodecError> {
    let (number, len, _) = vint(data, 0, 8)?;
    if number != track {
        return Ok(None);
    }
    let head = data
        .get(len..len + 3)
        .ok_or_else(|| malformed("a Matroska block ends early"))?;
    let rel = i16::from_be_bytes([head[0], head[1]]);
    if (head[2] >> 1) & 3 != 0 {
        return Err(CodecError::Unsupported(
            "the video track's blocks are laced, which video layers do not read".into(),
        ));
    }
    Ok(Some((rel, &data[len + 3..])))
}

/// The frames of `track` in one Cluster's children, with their timestamps
/// in timecode units.
fn cluster_frames<'a>(
    items: &[Element<'a>],
    track: u64,
    out: &mut Vec<(i64, &'a [u8])>,
) -> Result<(), CodecError> {
    let base = child(items, CLUSTER_TIMECODE)
        .map(|e| uint(e.data))
        .transpose()?
        .unwrap_or(0);
    let base = i64::try_from(base).unwrap_or(i64::MAX);
    for item in items {
        let found = match item.id {
            SIMPLE_BLOCK => block(item.data, track)?,
            BLOCK_GROUP => {
                let group = children(item.data)?;
                match child(&group, BLOCK) {
                    Some(b) => block(b.data, track)?,
                    None => None,
                }
            }
            _ => None,
        };
        if let Some((rel, frame)) = found {
            if out.len() >= MAX_VIDEO_FRAMES {
                return Err(CodecError::LimitExceeded(format!(
                    "the video has more than the {MAX_VIDEO_FRAMES} frames a video layer holds"
                )));
            }
            out.push((base.saturating_add(i64::from(rel)), frame));
        }
    }
    Ok(())
}

/// The children of an unknown-size Cluster starting at `at` in `seg`: the
/// ones before the next top-level element, and where that element starts.
fn open_cluster(seg: &[u8], at: usize) -> Result<(Vec<Element<'_>>, usize), CodecError> {
    let mut items = Vec::new();
    let mut pos = at;
    while pos < seg.len() {
        if items.len() >= MAX_ELEMENTS {
            return Err(CodecError::LimitExceeded(
                "a Matroska cluster holds too many elements".into(),
            ));
        }
        let e = element(seg, pos)?;
        if LEVEL1.contains(&e.id) {
            break;
        }
        if e.unknown {
            return Err(malformed("a Matroska block has an unknown size"));
        }
        pos = e.end;
        items.push(e);
    }
    Ok((items, pos))
}

/// Read a Matroska / WebM file's first video track (see the module docs).
pub(super) fn track(bytes: &[u8]) -> Result<Track<'_>, CodecError> {
    let header = element(bytes, 0)?;
    if header.id != EBML || header.unknown {
        return Err(malformed("no EBML header"));
    }
    let doc_type = children(header.data)?
        .iter()
        .find(|e| e.id == DOC_TYPE)
        .map(|e| {
            String::from_utf8_lossy(e.data)
                .trim_end_matches('\0')
                .to_string()
        })
        .unwrap_or_default();
    if doc_type != "matroska" && doc_type != "webm" {
        return Err(CodecError::Unsupported(format!(
            "the EBML document type is {doc_type:?}, not Matroska or WebM"
        )));
    }
    let segment = element(bytes, header.end)?;
    if segment.id != SEGMENT {
        return Err(malformed("no Matroska Segment after the EBML header"));
    }
    let seg = segment.data;

    // Info and Tracks, and the Clusters' positions.
    let mut scale: u64 = 1_000_000;
    let mut track: Option<VideoTrack<'_>> = None;
    let mut clusters: Vec<(usize, bool, usize)> = Vec::new();
    let mut pos = 0;
    let mut walked = 0usize;
    while pos < seg.len() {
        walked += 1;
        if walked > MAX_ELEMENTS {
            return Err(CodecError::LimitExceeded(
                "the Matroska Segment holds too many elements".into(),
            ));
        }
        let e = element(seg, pos)?;
        match e.id {
            INFO if !e.unknown => {
                if let Some(s) = child(&children(e.data)?, TIMECODE_SCALE) {
                    scale = uint(s.data)?.max(1);
                }
            }
            TRACKS if !e.unknown && track.is_none() => track = Some(video_track(e.data)?),
            CLUSTER if e.unknown => {
                let (_, next) = open_cluster(seg, e.start)?;
                clusters.push((e.start, true, next));
                pos = next;
                continue;
            }
            CLUSTER => clusters.push((e.start, false, e.end)),
            _ if e.unknown => return Err(malformed("a Matroska element has an unknown size")),
            _ => {}
        }
        pos = e.end;
    }
    let track = track.ok_or_else(|| malformed("no Tracks element"))?;

    let mut frames: Vec<(i64, &[u8])> = Vec::new();
    for (start, unknown, end) in clusters {
        if unknown {
            let (items, _) = open_cluster(seg, start)?;
            cluster_frames(&items, track.number, &mut frames)?;
        } else {
            cluster_frames(&children(&seg[start..end])?, track.number, &mut frames)?;
        }
    }
    if frames.is_empty() {
        return Err(malformed("the video track holds no frame"));
    }

    // Durations: the gap to the next timestamp (see the module docs).
    let to_ms = |units: i64| -> u32 {
        let ns = i128::from(units) * i128::from(scale);
        (ns / 1_000_000).clamp(1, i128::from(u32::MAX)) as u32
    };
    let default_ms = track
        .default_ns
        .map(|ns| (ns / 1_000_000).clamp(1, u64::from(u32::MAX)) as u32);
    let mut durations_ms: Vec<u32> = frames
        .windows(2)
        .map(|w| {
            let gap = w[1].0.saturating_sub(w[0].0);
            if gap > 0 {
                to_ms(gap)
            } else {
                default_ms.unwrap_or(40)
            }
        })
        .collect();
    let last = default_ms
        .or_else(|| durations_ms.last().copied())
        .unwrap_or(40);
    durations_ms.push(last);

    Ok(Track {
        codec: track.codec,
        config: track.config,
        samples: frames.into_iter().map(|(_, f)| f).collect(),
        durations_ms,
        coded: track.coded,
    })
}

#[cfg(test)]
mod tests {
    use super::super::super::{encode, encode_with, Mp4Codec, Mp4Frame};
    use super::super::{decode_in_this_process, decode_window_in_this_process};
    use super::*;
    use crate::ImportLimits;

    fn id_bytes(id: u32) -> Vec<u8> {
        let b = id.to_be_bytes();
        let skip = b.iter().position(|x| *x != 0).unwrap_or(3);
        b[skip..].to_vec()
    }

    /// An element with an 8-byte size.
    fn el(id: u32, payload: &[u8]) -> Vec<u8> {
        let mut out = id_bytes(id);
        out.push(0x01);
        out.extend_from_slice(&(payload.len() as u64).to_be_bytes()[1..]);
        out.extend_from_slice(payload);
        out
    }

    /// An element whose size is unknown (all ones, one byte).
    fn el_unknown(id: u32, payload: &[u8]) -> Vec<u8> {
        let mut out = id_bytes(id);
        out.push(0xFF);
        out.extend_from_slice(payload);
        out
    }

    fn uint_el(id: u32, v: u64) -> Vec<u8> {
        el(id, &v.to_be_bytes())
    }

    fn simple_block(track: u8, rel: i16, frame: &[u8]) -> Vec<u8> {
        let mut p = vec![0x80 | track];
        p.extend_from_slice(&rel.to_be_bytes());
        p.push(0x80);
        p.extend_from_slice(frame);
        el(SIMPLE_BLOCK, &p)
    }

    /// A Matroska file holding `samples` (timestamps in ms) on track 1 of
    /// codec `codec_id`, with `config` as CodecPrivate, split into two
    /// clusters (the second with an unknown size when `live`); an audio
    /// track 2 comes first and carries a block of its own.
    fn mkv(
        doc_type: &str,
        codec_id: &str,
        config: &[u8],
        (w, h): (u32, u32),
        samples: &[(i16, &[u8])],
        live: bool,
    ) -> Vec<u8> {
        let header = el(EBML, &el(DOC_TYPE, doc_type.as_bytes()));
        let info = el(INFO, &uint_el(TIMECODE_SCALE, 1_000_000));
        let audio = el(
            TRACK_ENTRY,
            &[
                uint_el(TRACK_NUMBER, 2),
                uint_el(TRACK_TYPE, 2),
                el(CODEC_ID, b"A_OPUS"),
            ]
            .concat(),
        );
        let mut video_fields = vec![
            uint_el(TRACK_NUMBER, 1),
            uint_el(TRACK_TYPE, 1),
            el(CODEC_ID, codec_id.as_bytes()),
            el(
                VIDEO,
                &[
                    uint_el(PIXEL_WIDTH, u64::from(w)),
                    uint_el(PIXEL_HEIGHT, u64::from(h)),
                ]
                .concat(),
            ),
        ];
        if !config.is_empty() {
            video_fields.push(el(CODEC_PRIVATE, config));
        }
        let tracks = el(
            TRACKS,
            &[audio, el(TRACK_ENTRY, &video_fields.concat())].concat(),
        );
        let half = samples.len().div_ceil(2);
        let mut first = uint_el(CLUSTER_TIMECODE, 0);
        first.extend(simple_block(2, 0, b"audio"));
        for (t, s) in &samples[..half] {
            first.extend(simple_block(1, *t, s));
        }
        let base = samples.get(half).map_or(0, |s| s.0);
        let mut second = uint_el(CLUSTER_TIMECODE, base as u64);
        for (t, s) in &samples[half..] {
            // The second cluster's frames sit in BlockGroups.
            let mut p = vec![0x81];
            p.extend_from_slice(&(t - base).to_be_bytes());
            p.push(0);
            p.extend_from_slice(s);
            second.extend(el(BLOCK_GROUP, &el(BLOCK, &p)));
        }
        let mut body = [info, tracks, el(CLUSTER, &first)].concat();
        if live {
            body.extend(el_unknown(CLUSTER, &second));
            body.extend(el(0x1C53_BB6B, b""));
        } else {
            body.extend(el(CLUSTER, &second));
        }
        [header, el_unknown(SEGMENT, &body)].concat()
    }

    fn clip(w: u32, h: u32, n: usize) -> Vec<Vec<u8>> {
        (0..n)
            .map(|i| {
                (0..w * h)
                    .flat_map(|p| {
                        if p % w < w / 2 {
                            [(40 + i * 60) as u8, 200 - (i * 50) as u8, 60, 255]
                        } else {
                            [128, 128, 128, 255]
                        }
                    })
                    .collect()
            })
            .collect()
    }

    /// The MP4 exporter's samples and configuration, remuxed into Matroska.
    fn remux(mp4: &[u8], codec_id: &str, doc_type: &str, live: bool) -> Vec<u8> {
        let track = super::super::track(mp4).unwrap();
        let mut at = 0i16;
        let samples: Vec<(i16, &[u8])> = track
            .samples
            .iter()
            .zip(&track.durations_ms)
            .map(|(s, d)| {
                let t = at;
                at += *d as i16;
                (t, *s)
            })
            .collect();
        let config = if codec_id == "V_AV1" {
            &[][..]
        } else {
            track.config
        };
        mkv(doc_type, codec_id, config, track.coded, &samples, live)
    }

    #[test]
    fn an_av1_webm_decodes_to_the_same_frames_as_its_mp4() {
        let (w, h) = (32, 32);
        let px = clip(w, h, 3);
        let input: Vec<Mp4Frame<'_>> = px
            .iter()
            .zip([200, 100, 300])
            .map(|(p, d)| Mp4Frame {
                rgba8: p,
                duration_ms: d,
            })
            .collect();
        let mp4 = encode_with(w, h, &input, 95, Mp4Codec::Av1).unwrap();
        let from_mp4 = decode_in_this_process(&mp4, ImportLimits::default()).unwrap();
        for live in [false, true] {
            let webm = remux(&mp4, "V_AV1", "webm", live);
            assert!(looks_like_matroska(&webm));
            let video = decode_in_this_process(&webm, ImportLimits::default()).unwrap();
            assert_eq!(video.codec, VideoCodec::Av1);
            assert_eq!((video.width, video.height), (w, h));
            let durations: Vec<u32> = video.frames.iter().map(|f| f.duration_ms).collect();
            assert_eq!(durations, vec![200, 100, 100], "live={live}");
            for (a, b) in video.frames.iter().zip(&from_mp4.frames) {
                assert_eq!(a.rgba8, b.rgba8, "the same pictures as the MP4");
            }
            // A window decodes only its frames, with the whole timing.
            let win = decode_window_in_this_process(&webm, ImportLimits::default(), 1, 1).unwrap();
            assert_eq!((win.first, win.frames.len()), (1, 1));
            assert_eq!(win.frames[0].rgba8, from_mp4.frames[1].rgba8);
        }
    }

    #[test]
    fn an_h264_mkv_decodes_to_the_same_frames_as_its_mp4() {
        let (w, h) = (48, 32);
        let px = clip(w, h, 2);
        let input: Vec<Mp4Frame<'_>> = px
            .iter()
            .map(|p| Mp4Frame {
                rgba8: p,
                duration_ms: 250,
            })
            .collect();
        let mp4 = encode(w, h, &input, 90).unwrap();
        let from_mp4 = decode_in_this_process(&mp4, ImportLimits::default()).unwrap();
        let mkv = remux(&mp4, "V_MPEG4/ISO/AVC", "matroska", false);
        let video = decode_in_this_process(&mkv, ImportLimits::default()).unwrap();
        assert_eq!(video.codec, VideoCodec::H264);
        assert_eq!(video.frames.len(), 2);
        for (a, b) in video.frames.iter().zip(&from_mp4.frames) {
            assert_eq!(a.rgba8, b.rgba8);
        }
    }

    #[test]
    fn other_codecs_lacing_and_damage_are_refused_never_panicked_on() {
        let frame: &[u8] = &[1, 2, 3];
        let vp9 = mkv("webm", "V_VP9", &[], (16, 16), &[(0, frame)], false);
        let err = decode_in_this_process(&vp9, ImportLimits::default()).unwrap_err();
        assert!(err.to_string().contains("VP9"), "{err}");
        let other = mkv("mka", "V_AV1", &[], (16, 16), &[(0, frame)], false);
        assert!(decode_in_this_process(&other, ImportLimits::default()).is_err());
        let no_avcc = mkv(
            "matroska",
            "V_MPEG4/ISO/AVC",
            &[],
            (16, 16),
            &[(0, frame)],
            false,
        );
        let err = decode_in_this_process(&no_avcc, ImportLimits::default()).unwrap_err();
        assert!(err.to_string().contains("avcC"), "{err}");
        // A laced block.
        let mut laced = mkv("webm", "V_AV1", &[], (16, 16), &[(0, frame)], false);
        let i = laced
            .windows(4)
            .position(|w| w == [0x81, 0, 0, 0x80])
            .unwrap();
        laced[i + 3] = 0x82;
        let err = track(&laced).map(|_| ()).unwrap_err();
        assert!(err.to_string().contains("laced"), "{err}");
        // Too many frames is a limit, before anything decodes.
        let many: Vec<(i16, &[u8])> = (0..=MAX_VIDEO_FRAMES as i16).map(|t| (t, frame)).collect();
        let big = mkv("webm", "V_AV1", &[], (16, 16), &many, false);
        assert!(matches!(
            track(&big).map(|_| ()),
            Err(CodecError::LimitExceeded(_))
        ));
        // Truncations and bit flips through the demux: errors, never panics.
        let good = mkv(
            "webm",
            "V_AV1",
            &[],
            (16, 16),
            &[(0, frame), (40, frame), (80, frame)],
            true,
        );
        assert_eq!(track(&good).unwrap().samples.len(), 3);
        for n in 0..good.len() {
            let _ = track(&good[..n]);
        }
        for i in 0..good.len() {
            for bit in [0x01u8, 0x10, 0x80] {
                let mut bad = good.clone();
                bad[i] ^= bit;
                let _ = track(&bad);
            }
        }
    }
}
