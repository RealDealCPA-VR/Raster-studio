//! W16-L: DICOM (medical imaging), its first frame.
//!
//! A DICOM Part 10 file is a 128-byte preamble, `DICM`, a file-meta group
//! (`0002,xxxx`, always explicit VR little endian) naming the transfer
//! syntax, then the data set. Read here:
//!
//! * transfer syntaxes implicit VR little endian (`1.2.840.10008.1.2`),
//!   explicit VR little endian (`...1.2.1`) and RLE Lossless (`...1.2.5`);
//!   the JPEG family (`...1.2.4.*`), deflate and big endian are refused by
//!   name;
//! * MONOCHROME1 / MONOCHROME2 at 8 or 16 bits allocated (any bits stored,
//!   signed or unsigned), through the modality rescale (slope, intercept)
//!   and the file's own first window (centre / width, the linear VOI
//!   function of PS3.3 C.11.2.1.2); with no window the frame's minimum and
//!   maximum are stretched; MONOCHROME1 is inverted. Opens as 16 bits;
//! * RGB and YBR_FULL at 8 bits, interleaved or planar. Opens as 8 bits.
//!
//! Sequences of undefined length are skipped item by item (bounded depth);
//! every element length is checked against the file before it is used. A
//! file without the preamble (a bare data set) opens when named `.dcm`.

use super::super::{check_decode, info, malformed, rgba8_surface};
use crate::codec::{
    CodecError, DecodedSurface, ImageInfo, ImportFormat, ImportLimits, SurfacePixels,
};

const NAME: &str = "DICOM";
const UNDEFINED: u32 = 0xFFFF_FFFF;
const MAX_DEPTH: usize = 16;

/// `true` when `prefix` holds `DICM` at offset 128.
pub fn looks_like_dicom(prefix: &[u8]) -> bool {
    prefix.get(128..132) == Some(b"DICM")
}

type Tag = (u16, u16);
const PIXEL_DATA: Tag = (0x7FE0, 0x0010);
const ITEM: Tag = (0xFFFE, 0xE000);
const ITEM_END: Tag = (0xFFFE, 0xE00D);
const SEQ_END: Tag = (0xFFFE, 0xE0DD);

#[derive(Debug, Clone, Copy, PartialEq)]
enum Pixels {
    Native,
    Rle,
}

#[derive(Debug, Default)]
struct Attrs {
    rows: u32,
    cols: u32,
    samples: u32,
    bits_alloc: u32,
    bits_stored: u32,
    signed: bool,
    photometric: String,
    planar: u32,
    center: Option<f64>,
    width: Option<f64>,
    slope: Option<f64>,
    intercept: Option<f64>,
}

struct Parsed<'a> {
    attrs: Attrs,
    pixels: Pixels,
    /// Native: the pixel data; RLE: the first fragment.
    data: &'a [u8],
}

fn u16_at(b: &[u8], at: usize) -> Result<u16, CodecError> {
    b.get(at..at + 2)
        .map(|s| u16::from_le_bytes([s[0], s[1]]))
        .ok_or_else(|| malformed(NAME, "the file ends inside an element"))
}

fn u32_at(b: &[u8], at: usize) -> Result<u32, CodecError> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| malformed(NAME, "the file ends inside an element"))
}

/// An element header at `at`: (tag, length or `UNDEFINED`, header size).
fn element(b: &[u8], at: usize, explicit: bool) -> Result<(Tag, u32, usize), CodecError> {
    let tag = (u16_at(b, at)?, u16_at(b, at + 2)?);
    if tag.0 == 0xFFFE || !explicit {
        return Ok((tag, u32_at(b, at + 4)?, 8));
    }
    let vr = b
        .get(at + 4..at + 6)
        .ok_or_else(|| malformed(NAME, "the file ends inside an element"))?;
    if matches!(
        vr,
        b"OB"
            | b"OD"
            | b"OF"
            | b"OL"
            | b"OV"
            | b"OW"
            | b"SQ"
            | b"UC"
            | b"UN"
            | b"UR"
            | b"UT"
            | b"SV"
            | b"UV"
    ) {
        Ok((tag, u32_at(b, at + 8)?, 12))
    } else if vr.iter().all(u8::is_ascii_uppercase) {
        Ok((tag, u32::from(u16_at(b, at + 6)?), 8))
    } else {
        Err(malformed(NAME, "an element has no value representation"))
    }
}

fn value(b: &[u8], at: usize, len: u32) -> Result<&[u8], CodecError> {
    at.checked_add(len as usize)
        .and_then(|end| b.get(at..end))
        .ok_or_else(|| malformed(NAME, "an element runs past the file"))
}

/// Skip the items of an undefined-length sequence starting at `at`; return
/// where the data set continues.
fn skip_sequence(
    b: &[u8],
    mut at: usize,
    explicit: bool,
    depth: usize,
) -> Result<usize, CodecError> {
    if depth > MAX_DEPTH {
        return Err(malformed(NAME, "sequences are nested too deeply"));
    }
    loop {
        let (tag, len, hdr) = element(b, at, explicit)?;
        at += hdr;
        if tag == SEQ_END {
            return Ok(at);
        }
        if tag != ITEM {
            return Err(malformed(
                NAME,
                "a sequence holds something other than items",
            ));
        }
        if len != UNDEFINED {
            value(b, at, len)?;
            at += len as usize;
            continue;
        }
        loop {
            let (tag, len, hdr) = element(b, at, explicit)?;
            at += hdr;
            if tag == ITEM_END {
                break;
            }
            if len == UNDEFINED {
                at = skip_sequence(b, at, explicit, depth + 1)?;
            } else {
                value(b, at, len)?;
                at += len as usize;
            }
        }
    }
}

fn text(v: &[u8]) -> String {
    String::from_utf8_lossy(v)
        .trim_matches(|c: char| c == '\0' || c.is_whitespace())
        .to_string()
}

fn first_number(v: &[u8]) -> Option<f64> {
    text(v).split('\\').next()?.trim().parse().ok()
}

fn parse(b: &[u8]) -> Result<Parsed<'_>, CodecError> {
    let mut at = if looks_like_dicom(b) { 132 } else { 0 };
    // The file meta group is always explicit VR little endian.
    let mut syntax = None;
    while u16_at(b, at).ok() == Some(0x0002) {
        let (tag, len, hdr) = element(b, at, true)?;
        if len == UNDEFINED {
            return Err(malformed(NAME, "a file meta element has no length"));
        }
        let v = value(b, at + hdr, len)?;
        if tag == (0x0002, 0x0010) {
            syntax = Some(text(v));
        }
        at += hdr + len as usize;
    }
    let (explicit, pixels) = match syntax.as_deref() {
        Some("1.2.840.10008.1.2") => (false, Pixels::Native),
        Some("1.2.840.10008.1.2.1") => (true, Pixels::Native),
        Some("1.2.840.10008.1.2.5") => (true, Pixels::Rle),
        Some(s) if s.starts_with("1.2.840.10008.1.2.4.") => {
            return Err(CodecError::Unsupported(format!(
                "JPEG-compressed DICOM (transfer syntax {s}) is not supported; \
                 uncompressed and RLE files are"
            )))
        }
        Some("1.2.840.10008.1.2.1.99") => {
            return Err(CodecError::Unsupported(
                "deflated DICOM (transfer syntax 1.2.840.10008.1.2.1.99) is not supported".into(),
            ))
        }
        Some("1.2.840.10008.1.2.2") => {
            return Err(CodecError::Unsupported(
                "big-endian DICOM (a retired transfer syntax) is not supported".into(),
            ))
        }
        Some(other) => {
            return Err(CodecError::Unsupported(format!(
                "DICOM transfer syntax {other} is not supported"
            )))
        }
        // No meta group: a bare data set; tell the VR form by its first element.
        None => {
            let vr = b.get(at + 4..at + 6).unwrap_or(b"");
            (
                vr.iter().all(u8::is_ascii_uppercase) && vr.len() == 2,
                Pixels::Native,
            )
        }
    };
    let mut a = Attrs {
        samples: 1,
        ..Attrs::default()
    };
    loop {
        let (tag, len, hdr) = element(b, at, explicit)?;
        at += hdr;
        if tag == PIXEL_DATA {
            let data = if len == UNDEFINED {
                if pixels != Pixels::Rle {
                    return Err(CodecError::Unsupported(
                        "this DICOM's pixel data is encapsulated (compressed) under a transfer \
                         syntax that says it is not"
                            .into(),
                    ));
                }
                first_fragment(b, at)?
            } else {
                if pixels == Pixels::Rle {
                    return Err(malformed(NAME, "RLE pixel data is not encapsulated"));
                }
                value(b, at, len)?
            };
            return Ok(Parsed {
                attrs: a,
                pixels,
                data,
            });
        }
        if len == UNDEFINED {
            at = skip_sequence(b, at, explicit, 0)?;
            continue;
        }
        let v = value(b, at, len)?;
        at += len as usize;
        let us = || {
            v.get(..2)
                .map(|s| u32::from(u16::from_le_bytes([s[0], s[1]])))
        };
        match tag {
            (0x0028, 0x0002) => a.samples = us().unwrap_or(1),
            (0x0028, 0x0004) => a.photometric = text(v),
            (0x0028, 0x0006) => a.planar = us().unwrap_or(0),
            (0x0028, 0x0010) => a.rows = us().unwrap_or(0),
            (0x0028, 0x0011) => a.cols = us().unwrap_or(0),
            (0x0028, 0x0100) => a.bits_alloc = us().unwrap_or(0),
            (0x0028, 0x0101) => a.bits_stored = us().unwrap_or(0),
            (0x0028, 0x0103) => a.signed = us() == Some(1),
            (0x0028, 0x1050) => a.center = first_number(v),
            (0x0028, 0x1051) => a.width = first_number(v),
            (0x0028, 0x1052) => a.intercept = first_number(v),
            (0x0028, 0x1053) => a.slope = first_number(v),
            _ => {}
        }
    }
}

/// The first fragment of encapsulated pixel data starting at `at` (after
/// the basic offset table).
fn first_fragment(b: &[u8], mut at: usize) -> Result<&[u8], CodecError> {
    let mut seen_table = false;
    loop {
        let (tag, len, hdr) = element(b, at, false)?;
        at += hdr;
        if tag == SEQ_END || tag != ITEM || len == UNDEFINED {
            return Err(malformed(NAME, "the encapsulated pixel data has no frame"));
        }
        let v = value(b, at, len)?;
        at += len as usize;
        if seen_table {
            return Ok(v);
        }
        seen_table = true;
    }
}

fn frame_dims(a: &Attrs) -> Result<(u32, u32), CodecError> {
    if a.rows == 0 || a.cols == 0 {
        return Err(malformed(NAME, "it has no Rows / Columns"));
    }
    Ok((a.cols, a.rows))
}

/// Header facts.
pub fn probe(bytes: &[u8], limits: ImportLimits) -> Result<ImageInfo, CodecError> {
    let p = parse(bytes)?;
    let (w, h) = frame_dims(&p.attrs)?;
    limits.check_dimensions(w, h)?;
    Ok(info(w, h, ImportFormat::Dicom, p.attrs.samples == 1))
}

/// PackBits, as DICOM's RLE segments use it, into exactly `n` bytes.
fn unpack_segment(seg: &[u8], n: usize) -> Result<Vec<u8>, CodecError> {
    let mut out = Vec::with_capacity(n);
    let mut i = 0;
    while out.len() < n && i < seg.len() {
        let c = seg[i] as i8;
        i += 1;
        if c >= 0 {
            let run = c as usize + 1;
            let lit = seg
                .get(i..i + run)
                .ok_or_else(|| malformed(NAME, "an RLE segment is cut short"))?;
            out.extend_from_slice(lit);
            i += run;
        } else if c != -128 {
            let run = 1 - c as isize;
            let byte = *seg
                .get(i)
                .ok_or_else(|| malformed(NAME, "an RLE segment is cut short"))?;
            out.extend(std::iter::repeat_n(byte, run as usize));
            i += 1;
        }
    }
    if out.len() < n {
        return Err(malformed(NAME, "an RLE segment decodes short"));
    }
    out.truncate(n);
    Ok(out)
}

/// Decode an RLE frame into native little-endian interleaved samples.
fn unrle(frame: &[u8], a: &Attrs, n: usize) -> Result<Vec<u8>, CodecError> {
    let bytes_per = (a.bits_alloc / 8) as usize;
    let want = a.samples as usize * bytes_per;
    let count = u32_at(frame, 0)? as usize;
    if count != want || count > 15 {
        return Err(malformed(
            NAME,
            "the RLE header's segment count does not fit the image",
        ));
    }
    let offsets: Vec<usize> = (0..count)
        .map(|i| u32_at(frame, 4 + i * 4).map(|v| v as usize))
        .collect::<Result<_, _>>()?;
    let mut segments = Vec::with_capacity(count);
    for (i, &start) in offsets.iter().enumerate() {
        let end = offsets.get(i + 1).copied().unwrap_or(frame.len());
        let seg = frame
            .get(start..end.max(start))
            .ok_or_else(|| malformed(NAME, "an RLE segment offset points past the frame"))?;
        segments.push(unpack_segment(seg, n)?);
    }
    // Segments run sample by sample, most significant byte first.
    let mut out = vec![0u8; n * want];
    for p in 0..n {
        for s in 0..a.samples as usize {
            for byte in 0..bytes_per {
                let seg = &segments[s * bytes_per + byte];
                out[p * want + s * bytes_per + (bytes_per - 1 - byte)] = seg[p];
            }
        }
    }
    Ok(out)
}

/// Decode the first frame.
pub fn decode(bytes: &[u8], limits: ImportLimits) -> Result<DecodedSurface, CodecError> {
    let p = parse(bytes)?;
    let a = &p.attrs;
    let (w, h) = frame_dims(a)?;
    let n = (w as u64 * h as u64) as usize;
    let grey = a.samples == 1;
    if grey && !matches!(a.photometric.as_str(), "MONOCHROME1" | "MONOCHROME2" | "") {
        return Err(CodecError::Unsupported(format!(
            "DICOM photometric interpretation {} is not supported",
            a.photometric
        )));
    }
    if !grey && (a.samples != 3 || !matches!(a.photometric.as_str(), "RGB" | "YBR_FULL")) {
        return Err(CodecError::Unsupported(format!(
            "DICOM photometric interpretation {} with {} samples is not supported",
            a.photometric, a.samples
        )));
    }
    let allowed = if grey { [8, 16] } else { [8, 8] };
    if !allowed.contains(&a.bits_alloc) {
        return Err(CodecError::Unsupported(format!(
            "DICOM with {} bits allocated per sample is not supported",
            a.bits_alloc
        )));
    }
    let frame_bytes = (n as u64)
        .saturating_mul(u64::from(a.samples))
        .saturating_mul(u64::from(a.bits_alloc / 8));
    check_decode(limits, w, h, 8, frame_bytes.saturating_mul(2))?;
    let native;
    let data: &[u8] = match p.pixels {
        Pixels::Native => p
            .data
            .get(..frame_bytes as usize)
            .ok_or_else(|| malformed(NAME, "the pixel data is shorter than one frame"))?,
        Pixels::Rle => {
            native = unrle(p.data, a, n)?;
            &native
        }
    };
    if !grey {
        let planar = a.planar == 1 && p.pixels == Pixels::Native;
        let mut out = vec![0u8; n * 4];
        for i in 0..n {
            let s = |c: usize| {
                if planar {
                    data[c * n + i]
                } else {
                    data[i * 3 + c]
                }
            };
            let (x, y, z) = (s(0), s(1), s(2));
            let rgb = if a.photometric == "YBR_FULL" {
                let (yy, cb, cr) = (f32::from(x), f32::from(y) - 128.0, f32::from(z) - 128.0);
                let c = |v: f32| v.round().clamp(0.0, 255.0) as u8;
                [
                    c(yy + 1.402 * cr),
                    c(yy - 0.344_136 * cb - 0.714_136 * cr),
                    c(yy + 1.772 * cb),
                ]
            } else {
                [x, y, z]
            };
            out[i * 4..i * 4 + 4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
        return Ok(rgba8_surface(w, h, out, ImportFormat::Dicom));
    }
    let stored = if a.bits_stored == 0 || a.bits_stored > a.bits_alloc {
        a.bits_alloc
    } else {
        a.bits_stored
    };
    let slope = a
        .slope
        .filter(|s| s.is_finite() && *s != 0.0)
        .unwrap_or(1.0);
    let intercept = a.intercept.filter(|s| s.is_finite()).unwrap_or(0.0);
    let values: Vec<f64> = (0..n)
        .map(|i| {
            let raw = if a.bits_alloc == 8 {
                u32::from(data[i])
            } else {
                u32::from(u16::from_le_bytes([data[i * 2], data[i * 2 + 1]]))
            };
            let mask = (1u32 << stored) - 1;
            let raw = raw & mask;
            let v = if a.signed && raw & (1 << (stored - 1)) != 0 {
                i64::from(raw) - (1i64 << stored)
            } else {
                i64::from(raw)
            };
            v as f64 * slope + intercept
        })
        .collect();
    let window = match (a.center, a.width) {
        (Some(c), Some(wd)) if c.is_finite() && wd.is_finite() && wd >= 1.0 => Some((c, wd)),
        _ => None,
    };
    let (lo, hi) = values
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), v| {
            (l.min(*v), h.max(*v))
        });
    let invert = a.photometric == "MONOCHROME1";
    let mut out = vec![0u16; n * 4];
    for (i, v) in values.iter().enumerate() {
        let t = match window {
            Some((c, wd)) => {
                if *v <= c - 0.5 - (wd - 1.0) / 2.0 {
                    0.0
                } else if *v > c - 0.5 + (wd - 1.0) / 2.0 {
                    1.0
                } else if wd > 1.0 {
                    ((v - (c - 0.5)) / (wd - 1.0) + 0.5).clamp(0.0, 1.0)
                } else {
                    1.0
                }
            }
            None if hi > lo => (v - lo) / (hi - lo),
            None => 0.5,
        };
        let t = if invert { 1.0 - t } else { t };
        let g = (t * 65535.0).round() as u16;
        out[i * 4..i * 4 + 4].copy_from_slice(&[g, g, g, 65535]);
    }
    let mut s = rgba8_surface(w, h, Vec::new(), ImportFormat::Dicom);
    s.pixels = SurfacePixels::Rgba16(out);
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::super::test_util::fuzz;
    use super::*;
    use crate::codec::{decode_surface_bytes, decode_surface_bytes_as, probe_bytes};

    fn el(out: &mut Vec<u8>, explicit: bool, g: u16, e: u16, vr: &[u8; 2], v: &[u8]) {
        out.extend_from_slice(&g.to_le_bytes());
        out.extend_from_slice(&e.to_le_bytes());
        let long = matches!(vr, b"OB" | b"OW" | b"SQ" | b"UN");
        if explicit || g == 2 {
            out.extend_from_slice(vr);
            if long {
                out.extend_from_slice(&[0, 0]);
                out.extend_from_slice(&(v.len() as u32).to_le_bytes());
            } else {
                out.extend_from_slice(&(v.len() as u16).to_le_bytes());
            }
        } else {
            out.extend_from_slice(&(v.len() as u32).to_le_bytes());
        }
        out.extend_from_slice(v);
    }

    fn pad(s: &str) -> Vec<u8> {
        let mut v = s.as_bytes().to_vec();
        if v.len() % 2 == 1 {
            v.push(b' ');
        }
        v
    }

    struct Img<'a> {
        syntax: &'a str,
        rows: u16,
        cols: u16,
        samples: u16,
        bits: u16,
        stored: u16,
        signed: bool,
        photometric: &'a str,
        window: Option<(&'a str, &'a str)>,
        rescale: Option<(&'a str, &'a str)>,
        /// Undefined-length sequence before the pixel data.
        sequence: bool,
    }

    fn dicom(img: &Img<'_>, pixels: &[u8], encapsulated: bool) -> Vec<u8> {
        let explicit = img.syntax != "1.2.840.10008.1.2";
        let mut b = vec![0u8; 128];
        b.extend_from_slice(b"DICM");
        let mut uid = img.syntax.as_bytes().to_vec();
        if uid.len() % 2 == 1 {
            uid.push(0);
        }
        el(&mut b, true, 2, 0x0010, b"UI", &uid);
        if img.sequence {
            // (0008,1140) SQ, undefined length, one undefined-length item.
            b.extend_from_slice(&0x0008u16.to_le_bytes());
            b.extend_from_slice(&0x1140u16.to_le_bytes());
            if explicit {
                b.extend_from_slice(b"SQ\0\0");
            }
            b.extend_from_slice(&UNDEFINED.to_le_bytes());
            b.extend_from_slice(&[0xFE, 0xFF, 0x00, 0xE0]);
            b.extend_from_slice(&UNDEFINED.to_le_bytes());
            el(&mut b, explicit, 0x0008, 0x1150, b"UI", &pad("1.2.3"));
            b.extend_from_slice(&[0xFE, 0xFF, 0x0D, 0xE0, 0, 0, 0, 0]);
            b.extend_from_slice(&[0xFE, 0xFF, 0xDD, 0xE0, 0, 0, 0, 0]);
        }
        el(
            &mut b,
            explicit,
            0x28,
            0x0002,
            b"US",
            &img.samples.to_le_bytes(),
        );
        el(&mut b, explicit, 0x28, 0x0004, b"CS", &pad(img.photometric));
        el(
            &mut b,
            explicit,
            0x28,
            0x0010,
            b"US",
            &img.rows.to_le_bytes(),
        );
        el(
            &mut b,
            explicit,
            0x28,
            0x0011,
            b"US",
            &img.cols.to_le_bytes(),
        );
        el(
            &mut b,
            explicit,
            0x28,
            0x0100,
            b"US",
            &img.bits.to_le_bytes(),
        );
        el(
            &mut b,
            explicit,
            0x28,
            0x0101,
            b"US",
            &img.stored.to_le_bytes(),
        );
        el(
            &mut b,
            explicit,
            0x28,
            0x0103,
            b"US",
            &u16::from(img.signed).to_le_bytes(),
        );
        if let Some((c, w)) = img.window {
            el(&mut b, explicit, 0x28, 0x1050, b"DS", &pad(c));
            el(&mut b, explicit, 0x28, 0x1051, b"DS", &pad(w));
        }
        if let Some((i, s)) = img.rescale {
            el(&mut b, explicit, 0x28, 0x1052, b"DS", &pad(i));
            el(&mut b, explicit, 0x28, 0x1053, b"DS", &pad(s));
        }
        if encapsulated {
            b.extend_from_slice(&0x7FE0u16.to_le_bytes());
            b.extend_from_slice(&0x0010u16.to_le_bytes());
            b.extend_from_slice(b"OB\0\0");
            b.extend_from_slice(&UNDEFINED.to_le_bytes());
            b.extend_from_slice(&[0xFE, 0xFF, 0x00, 0xE0, 0, 0, 0, 0]);
            b.extend_from_slice(&[0xFE, 0xFF, 0x00, 0xE0]);
            b.extend_from_slice(&(pixels.len() as u32).to_le_bytes());
            b.extend_from_slice(pixels);
            b.extend_from_slice(&[0xFE, 0xFF, 0xDD, 0xE0, 0, 0, 0, 0]);
        } else {
            el(&mut b, explicit, 0x7FE0, 0x0010, b"OW", pixels);
        }
        b
    }

    fn base(syntax: &str) -> Img<'_> {
        Img {
            syntax,
            rows: 1,
            cols: 4,
            samples: 1,
            bits: 16,
            stored: 12,
            signed: false,
            photometric: "MONOCHROME2",
            window: None,
            rescale: None,
            sequence: true,
        }
    }

    fn grey(s: &DecodedSurface) -> Vec<u16> {
        let SurfacePixels::Rgba16(px) = &s.pixels else {
            panic!("not 16-bit")
        };
        px.chunks(4).map(|p| p[0]).collect()
    }

    #[test]
    fn explicit_and_implicit_monochrome_decode_with_window_and_rescale() {
        let px: Vec<u8> = [0u16, 1000, 2000, 4095]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        // No window: min..max.
        for syntax in ["1.2.840.10008.1.2.1", "1.2.840.10008.1.2"] {
            let file = dicom(&base(syntax), &px, false);
            assert!(looks_like_dicom(&file));
            let s = decode_surface_bytes(&file, ImportLimits::default()).unwrap();
            assert_eq!(
                (s.width, s.height, s.source_format),
                (4, 1, ImportFormat::Dicom)
            );
            assert_eq!(grey(&s), [0, 16004, 32007, 65535], "{syntax}");
            let info = probe_bytes(&file, ImportLimits::default()).unwrap();
            assert_eq!((info.width, info.height), (4, 1));
        }
        // Window 1000 +/- 500 after rescale (slope 1, intercept -1024 would
        // move it; here intercept 0, slope 1): 0 -> black, 1000 -> mid,
        // 2000 and 4095 -> white. MONOCHROME1 inverts.
        let mut img = base("1.2.840.10008.1.2.1");
        img.window = Some(("1000", "1000\\2000"));
        img.rescale = Some(("0", "1"));
        let s = decode_surface_bytes(&dicom(&img, &px, false), ImportLimits::default()).unwrap();
        let g = grey(&s);
        assert_eq!((g[0], g[2], g[3]), (0, 65535, 65535));
        assert!((32700..32900).contains(&g[1]), "{g:?}");
        img.photometric = "MONOCHROME1";
        let s = decode_surface_bytes(&dicom(&img, &px, false), ImportLimits::default()).unwrap();
        assert_eq!(grey(&s)[0], 65535);
        // Signed 16-bit with a rescale intercept of -1024 (CT).
        let mut ct = base("1.2.840.10008.1.2.1");
        ct.signed = true;
        ct.stored = 16;
        ct.rescale = Some(("-1024", "1"));
        let px: Vec<u8> = [-2000i16, 0, 1024, 3000]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let s = decode_surface_bytes(&dicom(&ct, &px, false), ImportLimits::default()).unwrap();
        let g = grey(&s);
        assert_eq!((g[0], g[3]), (0, 65535));
        assert!(g[1] < g[2]);
    }

    #[test]
    fn rle_and_rgb_decode() {
        // 16-bit grey, RLE: MSB segment then LSB segment, 4 pixels.
        let vals = [0x0102u16, 0x0102, 0x0304, 0x0000];
        let msb = [0x01u8, 0x01, 0x03, 0x00];
        let lsb = [0x02u8, 0x02, 0x04, 0x00];
        let mut frame = vec![0u8; 64];
        frame[0] = 2;
        // Segment 1: a replicate run then literals; segment 2 literal.
        let seg1 = [0xFFu8, msb[0], 0x01, msb[2], msb[3]];
        let seg2 = [0x03u8, lsb[0], lsb[1], lsb[2], lsb[3]];
        frame[4..8].copy_from_slice(&64u32.to_le_bytes());
        frame[8..12].copy_from_slice(&(64 + seg1.len() as u32).to_le_bytes());
        frame.extend_from_slice(&seg1);
        frame.extend_from_slice(&seg2);
        let mut img = base("1.2.840.10008.1.2.5");
        img.stored = 16;
        let s = decode_surface_bytes(&dicom(&img, &frame, true), ImportLimits::default()).unwrap();
        let g = grey(&s);
        let expect: Vec<u16> = vals
            .iter()
            .map(|v| ((f64::from(*v) / f64::from(0x0304u16)) * 65535.0).round() as u16)
            .collect();
        assert_eq!(g, expect);
        // 8-bit RGB, interleaved and planar.
        let mut rgb = base("1.2.840.10008.1.2.1");
        rgb.samples = 3;
        rgb.bits = 8;
        rgb.stored = 8;
        rgb.cols = 2;
        rgb.photometric = "RGB";
        let s = decode_surface_bytes(
            &dicom(&rgb, &[1, 2, 3, 4, 5, 6], false),
            ImportLimits::default(),
        )
        .unwrap();
        assert_eq!(
            s.pixels,
            SurfacePixels::Rgba8(vec![1, 2, 3, 255, 4, 5, 6, 255])
        );
    }

    #[test]
    fn jpeg_dicom_is_refused_by_name_and_damage_never_panics() {
        let err = decode_surface_bytes(
            &dicom(&base("1.2.840.10008.1.2.4.50"), &[0; 8], true),
            ImportLimits::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("JPEG-compressed DICOM"), "{err}");
        // A bare data set (no preamble) opens when the name says DICOM.
        let file = dicom(
            &base("1.2.840.10008.1.2.1"),
            &[1, 0, 2, 0, 3, 0, 4, 0],
            false,
        );
        let bare = &file[132 + 8 + 20..];
        let s =
            decode_surface_bytes_as(bare, ImportLimits::default(), ImportFormat::Dicom).unwrap();
        assert_eq!(s.width, 4);
        fuzz(&file, ImportFormat::Dicom);
        let mut img = base("1.2.840.10008.1.2.5");
        img.stored = 16;
        let mut frame = vec![0u8; 64];
        frame[0] = 2;
        frame[4..8].copy_from_slice(&64u32.to_le_bytes());
        frame[8..12].copy_from_slice(&69u32.to_le_bytes());
        frame.extend_from_slice(&[0xFD, 7, 0xFD, 9, 0]);
        frame.extend_from_slice(&[0x03, 1, 2, 3, 4]);
        fuzz(&dicom(&img, &frame, true), ImportFormat::Dicom);
    }
}
