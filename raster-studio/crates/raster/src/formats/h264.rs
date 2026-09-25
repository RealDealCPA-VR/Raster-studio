//! W15-B: H.264 for MP4 export, through Cisco's OpenH264 (BSD-2-Clause),
//! compiled from source by `openh264-sys2` (the `cc` crate; no Cisco binary,
//! no runtime download).
//!
//! Declared from [`super::mp4`] (`#[path]`), which owns the container: this
//! module turns 4:2:0 planes into an H.264 elementary stream split the way an
//! MP4 `avc1` track stores it, the parameter sets (SPS / PPS) for the `avcC`
//! box and one length-prefixed sample per frame.
//!
//! The encoder is asked for the **High** profile (CAVLC entropy coding:
//! the Rust bindings expose no CABAC switch; High permits CAVLC), 8-bit
//! 4:2:0, BT.709 limited range signalled in the SPS VUI, one thread, frame
//! skipping off (every frame in is a frame out), and a quantizer band taken
//! from the Export As quality slider ([`qp_for_quality`]).
//!
//! Key frames are placed by **time**, not by a frame count: the first frame
//! is an IDR, and so is every frame that starts [`KEY_INTERVAL_SECONDS`] or
//! more after the previous IDR started (forced through
//! `Encoder::force_intra_frame`), so with mixed frame durations a seek point
//! still comes at most every 2 s, except that one frame shown longer than
//! 2 s cannot be split.
//!
//! The rate-control budget (4 bits a pixel a second at quality 100, scaled
//! with the quality, at the shortest frame's rate) is clamped to the chosen
//! H.264 level's maximum bit rate (Table A-1), which OpenH264 enforces when
//! it opens: without the clamp it refused 1080p60 and every 4K video.
//! OpenH264 encodes at most 3840 x 2160 (or 2160 x 3840); a larger frame is
//! refused by name, and AV1 (the other MP4 codec) takes it.

use openh264::encoder::{
    BitRate, Complexity, Encoder, EncoderConfig, FrameRate, FrameType, IntraFramePeriod, Level,
    Profile, QpRange, RateControlMode, UsageType, VuiConfig,
};
use openh264::formats::YUVSlices;
use openh264::{OpenH264API, Timestamp};

use crate::codec::CodecError;

/// The longest edge OpenH264 encodes (its level 5.2 limit).
pub const MAX_LONG_EDGE: u32 = 3840;
/// The longest *short* edge OpenH264 encodes.
pub const MAX_SHORT_EDGE: u32 = 2160;

/// At most this many seconds between two key frames (seek points).
pub const KEY_INTERVAL_SECONDS: u32 = 2;
const KEY_INTERVAL_MS: u64 = KEY_INTERVAL_SECONDS as u64 * 1000;

/// H.264 NAL unit types this module sorts on.
const NAL_IDR: u8 = 5;
const NAL_SPS: u8 = 7;
const NAL_PPS: u8 = 8;
/// Access unit delimiter: not stored in an MP4 sample.
const NAL_AUD: u8 = 9;

/// One encoded frame, ready for an MP4 sample: NAL units each prefixed by
/// its 4-byte big-endian length (the `avcC` says four), no start codes, no
/// parameter sets.
#[derive(Debug, Clone)]
pub struct H264Sample {
    pub data: Vec<u8>,
    /// Whether the frame is an IDR picture (a sync sample, `stss`).
    pub key: bool,
}

/// An encoded stream: the parameter sets and every frame's sample.
#[derive(Debug, Clone)]
pub struct H264Stream {
    pub sps: Vec<u8>,
    pub pps: Vec<u8>,
    pub samples: Vec<H264Sample>,
}

/// One 4:2:0 picture at an even `width x height`: `y` is `width * height`
/// bytes, `u` and `v` are `width/2 * height/2`.
#[derive(Debug, Clone, Copy)]
pub struct Planes<'a> {
    pub y: &'a [u8],
    pub u: &'a [u8],
    pub v: &'a [u8],
}

/// `quality` (1..=100) as the H.264 quantizer (0 best .. 51 worst): 100 is
/// QP 12, 80 about 19, 50 about 29, 1 is QP 45.
pub fn qp_for_quality(quality: u8) -> u8 {
    let q = u32::from(quality.clamp(1, 100));
    (12 + (100 - q) * 33 / 99) as u8
}

/// Whether OpenH264 can encode a `width x height` frame.
pub fn fits(width: u32, height: u32) -> bool {
    width.max(height) <= MAX_LONG_EDGE && width.min(height) <= MAX_SHORT_EDGE
}

/// Encode `frames` (all at the even `width x height`), each shown for its
/// `durations_ms`, at `quality` (1..=100).
pub fn encode(
    width: u32,
    height: u32,
    frames: &[Planes<'_>],
    durations_ms: &[u32],
    quality: u8,
) -> Result<H264Stream, CodecError> {
    if !width.is_multiple_of(2) || !height.is_multiple_of(2) || width == 0 || height == 0 {
        return Err(CodecError::InvalidParameter(format!(
            "H.264 4:2:0 needs an even frame size, not {width}x{height}"
        )));
    }
    if !fits(width, height) {
        return Err(CodecError::InvalidParameter(format!(
            "H.264 (OpenH264) encodes at most {MAX_LONG_EDGE}x{MAX_SHORT_EDGE} (or \
             {MAX_SHORT_EDGE}x{MAX_LONG_EDGE}) pixels, not {width}x{height}; choose the AV1 \
             codec for a larger video"
        )));
    }
    if frames.len() != durations_ms.len() || frames.is_empty() {
        return Err(CodecError::InvalidParameter(
            "an H.264 stream needs one duration per frame and at least one frame".into(),
        ));
    }
    let (w, h) = (width as usize, height as usize);
    for (i, f) in frames.iter().enumerate() {
        if f.y.len() != w * h || f.u.len() != w * h / 4 || f.v.len() != w * h / 4 {
            return Err(CodecError::BufferSize(format!(
                "frame {} is not a {width}x{height} 4:2:0 picture",
                i + 1
            )));
        }
    }

    // The frame rate the rate control budgets for: the shortest frame.
    let shortest = durations_ms.iter().copied().min().unwrap_or(1);
    let fps = (1000.0 / shortest.max(1) as f32).clamp(1.0, 120.0);
    let qp = qp_for_quality(quality);
    let (level, max_bps) = level_for(width, height, fps);
    let bps = budget_bps(width, height, quality, fps, max_bps);
    let config = EncoderConfig::new()
        .profile(Profile::High)
        .level(level)
        .usage_type(UsageType::CameraVideoRealTime)
        .rate_control_mode(RateControlMode::Bufferbased)
        .bitrate(BitRate::from_bps(bps))
        .max_frame_rate(FrameRate::from_hz(fps))
        .qp(QpRange::new(qp, (qp + 3).min(51)))
        .skip_frames(false)
        .complexity(Complexity::High)
        // No frame-counted period: key frames are forced by time below.
        .intra_frame_period(IntraFramePeriod::auto())
        .num_threads(1)
        .vui(VuiConfig::bt709());
    let mut encoder = Encoder::with_api_config(OpenH264API::from_source(), config)
        .map_err(|e| CodecError::Unsupported(format!("H.264 encoder: {e}")))?;

    let mut sps: Option<Vec<u8>> = None;
    let mut pps: Option<Vec<u8>> = None;
    let mut samples = Vec::with_capacity(frames.len());
    let mut at_ms = 0u64;
    let mut last_key_ms: Option<u64> = None;
    for (i, f) in frames.iter().enumerate() {
        let source = YUVSlices::new((f.y, f.u, f.v), (w, h), (w, w / 2, w / 2));
        if last_key_ms.is_some_and(|k| at_ms - k >= KEY_INTERVAL_MS) {
            encoder.force_intra_frame();
        }
        let started_ms = at_ms;
        let stream = encoder
            .encode_at(&source, Timestamp::from_millis(at_ms))
            .map_err(|e| CodecError::Unsupported(format!("H.264 encoder: {e}")))?;
        at_ms += u64::from(durations_ms[i]);
        if matches!(stream.frame_type(), FrameType::Skip | FrameType::Invalid) {
            return Err(CodecError::Unsupported(format!(
                "the H.264 encoder dropped frame {}",
                i + 1
            )));
        }
        let mut data = Vec::new();
        let mut key = false;
        for nal in split_annex_b(&stream.to_vec()) {
            let Some(kind) = nal.first().map(|b| b & 0x1F) else {
                continue;
            };
            match kind {
                NAL_SPS => keep_parameter_set(&mut sps, nal, "SPS")?,
                NAL_PPS => keep_parameter_set(&mut pps, nal, "PPS")?,
                NAL_AUD => {}
                _ => {
                    key |= kind == NAL_IDR;
                    let len = u32::try_from(nal.len()).map_err(|_| {
                        CodecError::LimitExceeded("an H.264 NAL unit exceeds 4 GiB".into())
                    })?;
                    data.extend_from_slice(&len.to_be_bytes());
                    data.extend_from_slice(nal);
                }
            }
        }
        if data.is_empty() {
            return Err(CodecError::Unsupported(format!(
                "the H.264 encoder returned no picture for frame {}",
                i + 1
            )));
        }
        if key {
            last_key_ms = Some(started_ms);
        } else if last_key_ms.is_none() {
            return Err(CodecError::Unsupported(
                "the H.264 encoder did not start on a key frame".into(),
            ));
        }
        samples.push(H264Sample { data, key });
    }
    let (Some(sps), Some(pps)) = (sps, pps) else {
        return Err(CodecError::Unsupported(
            "the H.264 encoder wrote no SPS / PPS".into(),
        ));
    };
    if sps.len() < 4 {
        return Err(CodecError::Unsupported("the H.264 SPS is truncated".into()));
    }
    Ok(H264Stream { sps, pps, samples })
}

/// Keep the first parameter set of a kind; a later one must repeat it (the
/// encoder keeps constant ids, and an `avcC` holds one of each).
fn keep_parameter_set(
    slot: &mut Option<Vec<u8>>,
    nal: &[u8],
    what: &str,
) -> Result<(), CodecError> {
    match slot {
        None => {
            *slot = Some(nal.to_vec());
            Ok(())
        }
        Some(first) if first.as_slice() == nal => Ok(()),
        Some(_) => Err(CodecError::Unsupported(format!(
            "the H.264 encoder changed its {what} mid-stream"
        ))),
    }
}

/// The rate-control budget in bits a second: 4 bits a pixel a second at
/// quality 100, scaled down with the quality, at `fps`, clamped to
/// `max_bps` (the level's maximum bit rate). Buffer-based rate control keeps
/// every picture's QP inside [qp, qp + 3] whatever the budget, so the clamp
/// changes the budget OpenH264 validates, not the quantizer band.
fn budget_bps(width: u32, height: u32, quality: u8, fps: f32, max_bps: u32) -> u32 {
    (u64::from(width) * u64::from(height) * u64::from(quality.clamp(1, 100)) / 25)
        .saturating_mul(fps.ceil() as u64)
        .clamp(64_000, u64::from(max_bps)) as u32
}

/// The smallest level whose limits hold `width x height` at `fps`, and that
/// level's maximum bit rate in bits a second (H.264 Table A-1 `MaxBR` x
/// 1000, the Baseline / Main VCL factor: below High's x1250 and below the
/// x1200 OpenH264 checks against, so OpenH264 accepts it).
fn level_for(width: u32, height: u32, fps: f32) -> (Level, u32) {
    // Macroblocks per frame and per second (H.264 Table A-1).
    let mbs = u64::from(width.div_ceil(16)) * u64::from(height.div_ceil(16));
    let rate = (mbs as f64 * f64::from(fps)).ceil() as u64;
    let table: [(u64, u64, u32, Level); 10] = [
        (396, 11_880, 4_000, Level::Level_2_1),
        (1_620, 20_250, 4_000, Level::Level_2_2),
        (1_620, 40_500, 10_000, Level::Level_3_0),
        (3_600, 108_000, 14_000, Level::Level_3_1),
        (5_120, 216_000, 20_000, Level::Level_3_2),
        (8_192, 245_760, 20_000, Level::Level_4_0),
        (8_704, 522_240, 50_000, Level::Level_4_2),
        (22_080, 589_824, 135_000, Level::Level_5_0),
        (36_864, 983_040, 240_000, Level::Level_5_1),
        (36_864, 2_073_600, 240_000, Level::Level_5_2),
    ];
    let (_, _, max_kbps, level) = table
        .iter()
        .copied()
        .find(|(fs, mbps, _, _)| mbs <= *fs && rate <= *mbps)
        .unwrap_or(table[table.len() - 1]);
    (level, max_kbps * 1000)
}

/// The NAL units of an Annex B byte stream, without their start codes.
pub fn split_annex_b(stream: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= stream.len() {
        if stream[i] == 0 && stream[i + 1] == 0 && stream[i + 2] == 1 {
            starts.push((i, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::with_capacity(starts.len());
    for (n, (_, body)) in starts.iter().enumerate() {
        let mut end = starts.get(n + 1).map_or(stream.len(), |s| s.0);
        // A 4-byte start code's leading zero belongs to the next code, and
        // trailing zeros are not part of a NAL unit.
        while end > *body && stream[end - 1] == 0 {
            end -= 1;
        }
        if end > *body {
            out.push(&stream[*body..end]);
        }
    }
    out
}

/// The `avcC` payload (ISO/IEC 14496-15 `AVCDecoderConfigurationRecord`)
/// for one SPS and one PPS, 4-byte NAL lengths.
pub fn avcc(sps: &[u8], pps: &[u8]) -> Result<Vec<u8>, CodecError> {
    let (Ok(sps_len), Ok(pps_len)) = (u16::try_from(sps.len()), u16::try_from(pps.len())) else {
        return Err(CodecError::Unsupported(
            "an H.264 parameter set exceeds 64 KiB".into(),
        ));
    };
    if sps.len() < 4 {
        return Err(CodecError::Unsupported("the H.264 SPS is truncated".into()));
    }
    let profile = sps[1];
    let mut out = vec![1, profile, sps[2], sps[3], 0xFC | 3, 0xE0 | 1];
    out.extend_from_slice(&sps_len.to_be_bytes());
    out.extend_from_slice(sps);
    out.push(1);
    out.extend_from_slice(&pps_len.to_be_bytes());
    out.extend_from_slice(pps);
    // The High-profile tail: 4:2:0, 8-bit luma and chroma, no SPS extension.
    if matches!(profile, 100 | 110 | 122 | 144) {
        out.extend_from_slice(&[0xFC | 1, 0xF8, 0xF8, 0]);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annex_b_splits_on_three_and_four_byte_start_codes() {
        let stream = [
            0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 4, 0,
        ];
        let nals = split_annex_b(&stream);
        assert_eq!(
            nals,
            vec![&[0x67, 1, 2][..], &[0x68, 3][..], &[0x65, 4][..]]
        );
        assert!(split_annex_b(&[]).is_empty());
        assert!(split_annex_b(&[0, 0, 1]).is_empty());
    }

    #[test]
    fn quality_maps_monotonically_onto_the_quantizer() {
        assert_eq!(qp_for_quality(100), 12);
        assert_eq!(qp_for_quality(1), 45);
        let qps: Vec<u8> = (1..=100).map(qp_for_quality).collect();
        assert!(qps.windows(2).all(|w| w[0] >= w[1]));
    }

    /// W15-B round 2: the budget never passes the level's maximum bit rate
    /// (the numbers the reviewer's refusals came from).
    #[test]
    fn the_budget_is_clamped_to_the_levels_maximum_bit_rate() {
        for (w, h, fps) in [
            (1920, 1080, 62.5f32),
            (3840, 2160, 30.0),
            (3840, 2160, 62.5),
            (3840, 2160, 120.0),
            (1280, 720, 60.0),
            (64, 64, 1.0),
        ] {
            let (_, max_bps) = level_for(w, h, fps);
            for q in [1, 50, 80, 100] {
                let bps = budget_bps(w, h, q, fps, max_bps);
                assert!(bps <= max_bps, "{w}x{h}@{fps} q{q}: {bps} > {max_bps}");
            }
        }
        assert_eq!(level_for(1920, 1080, 62.5).1, 50_000_000, "level 4.2");
        assert_eq!(level_for(3840, 2160, 30.0).1, 240_000_000, "level 5.1");
        // A small video's budget is not raised to the level's ceiling.
        assert_eq!(budget_bps(640, 480, 80, 30.0, 10_000_000), 10_000_000);
        assert_eq!(budget_bps(320, 240, 50, 30.0, 10_000_000), 4_608_000);
    }

    #[test]
    fn the_size_limit_is_openh264s() {
        assert!(fits(3840, 2160) && fits(2160, 3840));
        assert!(!fits(3842, 2160) && !fits(2162, 2162) && !fits(2160, 3842));
        let y = vec![0u8; 4];
        let c = vec![0u8; 1];
        let p = Planes {
            y: &y,
            u: &c,
            v: &c,
        };
        let err = encode(3, 2, &[p], &[10], 50).unwrap_err().to_string();
        assert!(err.contains("even"), "{err}");
        let err = encode(3842, 2, &[p], &[10], 50).unwrap_err().to_string();
        assert!(err.contains("AV1"), "{err}");
    }
}
