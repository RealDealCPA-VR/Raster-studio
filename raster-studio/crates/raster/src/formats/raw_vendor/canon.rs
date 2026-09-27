//! W18-D: Canon CR2, from Laurent Clevy's public description of the format
//! ("Understanding what is stored in a Canon RAW .CR2 file").
//!
//! A CR2 is a TIFF whose fourth IFD (IFD3) holds the raw image as one
//! lossless-JPEG frame (`Compression = 6`, `SOF3`, two or four
//! components). The decoded samples are laid out as vertical slices, left
//! to right: `cr2_slice` (tag `0xC640`) is `[n, w, last]`, `n` slices of
//! width `w` then one of `last`, each filled top to bottom before the next.
//! The sensor width is the sum of the slices (the frame's width times its
//! components when there are none); the height is what the samples leave.
//!
//! The maker note (an IFD at the EXIF `MakerNote`, file-relative offsets)
//! gives `SensorInfo` (`0x00E0`: the active area's left / top / right /
//! bottom borders, inclusive, and the black mask's) and `ColorBalance`
//! (`0x4001`: the as-shot RGGB levels at the short offset the table's
//! length selects: 25 for 582 entries, 34 for 653, 71 for 5120, else 63).
//! Black is the mean of the masked columns left of the active area per 2x2
//! cell (skipping the first two, which can carry a bright stripe); with no
//! `SensorInfo` it is 0. The CFA is red-green / green-blue, as on every EOS
//! body, but its phase at the sensor origin depends on the body's borders
//! (a 5D Mark II file starts one row off), so it is found from the data
//! ([`phase`]). sRAW / mRAW (subsampled YCbCr) are refused by name, as is
//! anything that is not lossless JPEG.

use crate::codec::{CodecError, ImportLimits};

use super::super::ljpeg;
use super::{
    balance, broken, budget, ifd0, maker_note, masked_black, not_decoded, orientation, rggb,
    strips, tiff_any, Mosaic, Pattern, Want,
};

const NAME: &str = "Canon CR2";
const TAG_CR2_SLICE: u16 = 0xC640;
const TAG_SRAW_TYPE: u16 = 0xC6C5;
const TAG_SENSOR_INFO: u16 = 0x00E0;
const TAG_COLOR_BALANCE: u16 = 0x4001;

/// A lossless-JPEG frame header: precision, height, width, components, and
/// whether every component is full resolution.
pub(super) struct Frame {
    pub precision: u32,
    pub height: usize,
    pub width: usize,
    pub components: usize,
    pub full_resolution: bool,
}

/// The `SOF3` of a lossless-JPEG stream, found by walking its marker
/// segments (bounded by the data).
pub(super) fn sof3(data: &[u8]) -> Option<Frame> {
    if !data.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut pos = 2usize;
    for _ in 0..64 {
        if *data.get(pos)? != 0xFF {
            return None;
        }
        while data.get(pos) == Some(&0xFF) {
            pos += 1;
        }
        let marker = *data.get(pos)?;
        pos += 1;
        if matches!(marker, 0xD8 | 0x01 | 0xD0..=0xD7) {
            continue;
        }
        if matches!(marker, 0xD9 | 0xDA) {
            return None;
        }
        let len = usize::from(u16::from_be_bytes([*data.get(pos)?, *data.get(pos + 1)?]));
        let seg = data.get(pos + 2..pos + len.max(2))?;
        if marker == 0xC3 {
            let n = usize::from(*seg.get(5)?);
            let specs = seg.get(6..6 + n * 3)?;
            return Some(Frame {
                precision: u32::from(seg[0]),
                height: usize::from(u16::from_be_bytes([seg[1], seg[2]])),
                width: usize::from(u16::from_be_bytes([seg[3], seg[4]])),
                components: n,
                full_resolution: specs.chunks(3).all(|c| c[1] == 0x11),
            });
        }
        pos += len.max(2);
    }
    None
}

pub(super) fn read(bytes: &[u8], limits: ImportLimits, want: Want) -> Result<Mosaic, CodecError> {
    let tiff = tiff_any(bytes).ok_or_else(|| broken(NAME, "not a TIFF"))?;
    let ifd0 = ifd0(&tiff, NAME)?;
    // IFD0 and its chain, in order.
    let mut chain = Vec::new();
    let mut at = tiff.first_ifd().unwrap_or(0);
    while chain.len() < 8 {
        let Some(ifd) = tiff.ifd(at) else { break };
        let next = ifd.next as usize;
        chain.push(ifd);
        if next == 0 || next == at {
            break;
        }
        at = next;
    }
    let k = chain
        .iter()
        .position(|i| i.get(TAG_CR2_SLICE).is_some())
        .or((chain.len() > 3).then_some(3))
        .ok_or_else(|| broken(NAME, "there is no raw IFD (IFD3)"))?;
    let raw = &chain[k];
    if raw.uint(&tiff, TAG_SRAW_TYPE) == Some(4) {
        return Err(not_decoded(NAME, "sRAW / mRAW (subsampled YCbCr) data"));
    }
    let compression = raw.uint(&tiff, super::TAG_COMPRESSION).unwrap_or(6);
    if compression != 6 {
        return Err(not_decoded(NAME, &format!("raw compression {compression}")));
    }
    let data = strips(&tiff, raw, NAME)?;
    let frame = sof3(&data)
        .ok_or_else(|| broken(NAME, "the raw data has no lossless-JPEG frame header"))?;
    if !frame.full_resolution {
        return Err(not_decoded(NAME, "sRAW / mRAW (subsampled YCbCr) data"));
    }
    if !(2..=16).contains(&frame.precision) || frame.components == 0 {
        return Err(broken(NAME, "the lossless-JPEG frame header is invalid"));
    }
    let total = frame
        .width
        .checked_mul(frame.components)
        .and_then(|v| v.checked_mul(frame.height))
        .filter(|&t| t > 0)
        .ok_or_else(|| broken(NAME, "the lossless-JPEG frame has no samples"))?;
    let widths: Vec<usize> = match raw.uints_of(&tiff, TAG_CR2_SLICE) {
        Some(s) if s.len() == 3 && (s[1] > 0 || s[2] > 0) => {
            if s[0] > 64 {
                return Err(broken(NAME, "cr2_slice declares too many slices"));
            }
            std::iter::repeat_n(s[1] as usize, s[0] as usize)
                .chain(std::iter::once(s[2] as usize))
                .filter(|&w| w > 0)
                .collect()
        }
        _ => vec![frame.width * frame.components],
    };
    let width: usize = widths.iter().sum();
    if width == 0 || !total.is_multiple_of(width) {
        return Err(broken(
            NAME,
            "cr2_slice does not match the lossless-JPEG frame",
        ));
    }
    let height = total / width;
    budget(NAME, limits, width, height)?;
    // SensorInfo: the active area and the black mask.
    let note = maker_note(&tiff, &ifd0).and_then(|(at, _)| tiff.ifd(at));
    let info = note
        .as_ref()
        .and_then(|n| n.uints_of(&tiff, TAG_SENSOR_INFO))
        .filter(|v| v.len() >= 9)
        .map(|v| v.into_iter().map(|x| x as usize).collect::<Vec<_>>());
    let active = match &info {
        Some(v) if v[5] < v[7] && v[6] < v[8] && v[7] < width && v[8] < height => {
            (v[6], v[5], v[8] + 1, v[7] + 1)
        }
        _ => (0, 0, height, width),
    };
    let wb = note
        .as_ref()
        .and_then(|n| n.uints_of(&tiff, TAG_COLOR_BALANCE))
        .filter(|c| c.len() > 500)
        .and_then(|c| {
            let at = match c.len() {
                582 => 25,
                653 => 34,
                5120 => 71,
                _ => 63,
            };
            let l = c.get(at..at + 4)?;
            let f = |i: usize| f64::from(l[i]);
            balance(f(0), (f(1) + f(2)) / 2.0, f(3))
        });
    let mut samples = Vec::new();
    let mut black = [0.0; 4];
    let mut pattern = rggb();
    if want == Want::Pixels {
        let stream = ljpeg::decode(&data, total).map_err(|e| broken(NAME, e))?;
        if stream.len() != total {
            return Err(broken(NAME, "the lossless-JPEG frame is short"));
        }
        samples = vec![0u16; total];
        let mut k = 0usize;
        let mut x0 = 0usize;
        for &sw in &widths {
            for y in 0..height {
                let row = y * width + x0;
                samples[row..row + sw].copy_from_slice(&stream[k..k + sw]);
                k += sw;
            }
            x0 += sw;
        }
        let (top, left, bottom, _) = active;
        // The black mask's own columns when SensorInfo gives a usable
        // range (several columns left of the image), else every masked
        // column but the first two.
        let declared = match &info {
            Some(v) if v.len() >= 13 && v[9] + 2 <= v[11] && v[11] < left => {
                masked_black(&samples, width, top..bottom, v[9]..v[11] + 1)
            }
            _ => None,
        };
        let border = (left >= 4)
            .then(|| masked_black(&samples, width, top..bottom, 2..left))
            .flatten();
        if let Some(b) = declared.or(border) {
            black = b;
        }
        pattern = phase(&samples, width, active, black, wb);
    }
    Ok(Mosaic {
        name: NAME,
        width,
        height,
        samples,
        pattern,
        active,
        black,
        white: None,
        full_scale: f64::from((1u32 << frame.precision) - 1),
        wb,
        orientation: orientation(&tiff, &ifd0),
    })
}

/// The CFA's phase at the sensor origin. The two green sites of a Bayer
/// block are the diagonal whose neighbours differ least. With green on the
/// anti-diagonal the pattern is red-green / green-blue, as on every EOS
/// body. With green on the main diagonal (the sensor read out one row or
/// column off, as some bodies' borders are) red is the site that, times the
/// as-shot red multiplier (and blue times blue), best matches green;
/// without an as-shot balance it is taken as green-blue / red-green. Both
/// use only the blocks with no sample within 10% of the brightest.
fn phase(
    samples: &[u16],
    width: usize,
    active: (usize, usize, usize, usize),
    black: [f64; 4],
    wb: Option<[f64; 3]>,
) -> Pattern {
    let (top, left, bottom, right) = active;
    let at = |y: usize, x: usize| f64::from(samples.get(y * width + x).copied().unwrap_or(0));
    // Blocks holding a sample near the brightest one are left out: a
    // clipped highlight is equal in every channel and would pull the
    // colour means together.
    let brightest = (top..bottom)
        .flat_map(|y| {
            samples
                .get(y * width + left..y * width + right)
                .unwrap_or(&[])
        })
        .copied()
        .max()
        .map_or(0.0, f64::from);
    let clip = brightest * 0.9;
    let (mut main, mut anti) = (0f64, 0f64);
    let mut sum = [0f64; 4];
    let mut n = 0f64;
    let mut y = top.next_multiple_of(2);
    while y + 1 < bottom {
        let mut x = left.next_multiple_of(2);
        while x + 1 < right {
            let v = [at(y, x), at(y, x + 1), at(y + 1, x), at(y + 1, x + 1)];
            x += 2;
            if v.iter().any(|&s| s >= clip) {
                continue;
            }
            main += (v[0] - v[3]).abs();
            anti += (v[1] - v[2]).abs();
            for c in 0..4 {
                sum[c] += v[c];
            }
            n += 1.0;
        }
        y += 2;
    }
    let pattern = |codes: [u8; 4]| Pattern {
        rows: 2,
        cols: 2,
        codes: codes.to_vec(),
    };
    if anti <= main || n == 0.0 {
        return rggb();
    }
    let m = [0, 1, 2, 3].map(|c| (sum[c] / n - black[c]).max(1.0));
    let g = (m[0] + m[3]) / 2.0;
    let Some([r, _, b]) = wb else {
        return pattern([1, 2, 0, 1]);
    };
    // Red at cell 1 (green-red / blue-green) or at cell 2.
    let err = |red: f64, blue: f64| (red * r / g).ln().abs() + (blue * b / g).ln().abs();
    if err(m[1], m[2]) < err(m[2], m[1]) {
        pattern([1, 0, 2, 1])
    } else {
        pattern([1, 2, 0, 1])
    }
}
