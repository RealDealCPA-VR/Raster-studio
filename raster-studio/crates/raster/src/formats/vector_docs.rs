//! W13-D: the document formats File > Open reads through a **preview** the
//! file carries, and the one it refuses by name.
//!
//! | Format | What opens | What does not |
//! | --- | --- | --- |
//! | EPS | W15-E: the **PostScript artwork**, run by the bounded interpreter in [`super::postscript`] (paths, fills, strokes, clips, images, text in a fallback font); when that fails or draws nothing, the embedded TIFF preview of a DOS EPS, else its WMF preview (drawn by [`super::metafile`]), else an EPSI hex preview, with the reason in the note | what the interpreter reports as not drawn (smooth shading, patterns, embedded Type 1 outlines, unknown operators; see [`super::postscript`]); an EPS it cannot draw and with no preview is refused with the interpreter's reason |
//! | Paint.NET PDN | here, the flattened thumbnail PNG in the XML header (Paint.NET writes it at most 256 px a side); File > Open reads the **layers** first through [`pdn::read_layers`] (W13X-7) and comes here only when they cannot be read | a layer layout [`pdn`] cannot follow (see its docs): the thumbnail opens and the caller says why |
//! | Sketch | here, `previews/preview.png`, the page preview Sketch saves; File > Open reads the **layers** first through [`design_files`] (W13X-8) and comes here only when they cannot be read | nothing more on this flat route |
//! | Adobe XD | here, the archive's `preview.png` (or `thumbnail.png`); File > Open reads the layers first ([`design_files`]) | nothing more on this flat route |
//! | Figma FIG | here, a ZIP-packaged `.fig`'s `thumbnail.png`; File > Open reads the layers first ([`design_fig`]), a bare `fig-kiwi` canvas included | on this flat route a bare `fig-kiwi` canvas has no preview: **refused**, saying so |
//!
//! Every preview is decoded by the codec facade under the caller's
//! [`ImportLimits`], and [`decode_described`] returns, beside the pixels, the
//! sentence that says which preview it is, so the application can tell the
//! user they are looking at a preview and not the artwork.
//!
//! # Untrusted input
//!
//! The ZIP reader here checks every offset against the file before it
//! follows it, walks the central directory at most once per declared entry,
//! refuses ZIP64 and encrypted archives by name, and inflates through a
//! reader capped at the limits' allocation ceiling. The EPS header's offsets
//! and lengths are checked against the file; an EPSI preview's declared size
//! is checked against the limits before its buffer exists.

use std::io::Read;

use super::{check_decode, malformed, metafile, rgba8_surface};
use crate::codec::{
    decode_surface_bytes_as, CodecError, DecodedSurface, ImageInfo, ImportFormat, ImportLimits,
};

// W13X-8: Sketch, Adobe XD and Figma read as layers (File > Open's route);
// this module keeps the flat preview for every other caller.
#[path = "design_fig.rs"]
pub mod design_fig;
#[path = "design_files.rs"]
pub mod design_files;

const DOS_EPS: [u8; 4] = [0xC5, 0xD0, 0xD3, 0xC6];
const ZIP_LOCAL: [u8; 4] = *b"PK\x03\x04";
const ZIP_CENTRAL: [u8; 4] = *b"PK\x01\x02";
const ZIP_END: [u8; 4] = *b"PK\x05\x06";
const XD_MIMETYPE: &[u8] = b"application/vnd.adobe.sparkler";
const FIG_KIWI: &[u8] = b"fig-kiwi";

/// `true` for a DOS EPS binary header or a PostScript `%!PS` start (an
/// EPS, or an Illustrator file older than the PDF-compatible format).
pub fn looks_like_eps(head: &[u8]) -> bool {
    head.starts_with(&DOS_EPS) || head.starts_with(b"%!PS")
}

/// `true` for a Paint.NET `PDN3` file.
pub fn looks_like_pdn(head: &[u8]) -> bool {
    head.starts_with(b"PDN3")
}

/// `true` for an Adobe XD archive: a ZIP whose first entry is a `mimetype`
/// naming XD's "sparkler" project type. The sniff window may cut the name
/// short, so at least `application/vnd.adobe` of it must be there and match.
pub fn looks_like_xd(head: &[u8]) -> bool {
    first_zip_entry(head).is_some_and(|(name, data)| {
        let seen = data.len().min(XD_MIMETYPE.len());
        name == b"mimetype" && seen >= 20 && data[..seen] == XD_MIMETYPE[..seen]
    })
}

/// `true` for a bare Figma canvas (`fig-kiwi`).
pub fn looks_like_fig_kiwi(head: &[u8]) -> bool {
    head.starts_with(FIG_KIWI)
}

fn first_zip_entry(head: &[u8]) -> Option<(&[u8], &[u8])> {
    if head.len() < 30 || head[..4] != ZIP_LOCAL {
        return None;
    }
    let name_len = usize::from(u16::from_le_bytes([head[26], head[27]]));
    let extra_len = usize::from(u16::from_le_bytes([head[28], head[29]]));
    let name = head.get(30..30 + name_len)?;
    let data = head.get(30 + name_len + extra_len..)?;
    Some((name, data))
}

// ------------------------------------------------------------------- base64

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Standard base64 with padding.
pub(crate) fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(B64[(n >> (18 - 6 * i)) as usize & 63]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Decode base64, skipping whitespace; `None` on any other stray byte.
pub(crate) fn base64_decode(text: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &c in text {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            c if c.is_ascii_whitespace() => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------- ZIP

fn le16(b: &[u8], at: usize, name: &str) -> Result<usize, CodecError> {
    b.get(at..at + 2)
        .map(|s| usize::from(u16::from_le_bytes([s[0], s[1]])))
        .ok_or_else(|| malformed(name, "the archive ends inside a record"))
}

fn le32(b: &[u8], at: usize, name: &str) -> Result<usize, CodecError> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize)
        .ok_or_else(|| malformed(name, "the archive ends inside a record"))
}

/// The bytes of the archive entry called `wanted`, inflated, or `None` when
/// the archive has no such entry. `name` is the format, for messages.
pub(crate) fn zip_entry(
    zip: &[u8],
    wanted: &str,
    cap: u64,
    name: &str,
) -> Result<Option<Vec<u8>>, CodecError> {
    if zip.len() < 22 {
        return Err(malformed(name, "too short to be a ZIP archive"));
    }
    let floor = zip.len().saturating_sub(22 + 0xFFFF);
    let end = (floor..=zip.len() - 22)
        .rev()
        .find(|&i| zip[i..i + 4] == ZIP_END)
        .ok_or_else(|| malformed(name, "no ZIP end-of-directory record"))?;
    let count = le16(zip, end + 10, name)?;
    let dir_size = le32(zip, end + 12, name)?;
    let dir_at = le32(zip, end + 16, name)?;
    if count == 0xFFFF || dir_at == 0xFFFF_FFFF || dir_size == 0xFFFF_FFFF {
        return Err(CodecError::Unsupported(format!(
            "ZIP64 {name} archives are not supported"
        )));
    }
    let mut at = dir_at;
    for _ in 0..count {
        if zip.get(at..at + 4) != Some(ZIP_CENTRAL.as_slice()) {
            return Err(malformed(name, "a central directory record is damaged"));
        }
        let flags = le16(zip, at + 8, name)?;
        let method = le16(zip, at + 10, name)?;
        let packed = le32(zip, at + 20, name)?;
        let name_len = le16(zip, at + 28, name)?;
        let extra_len = le16(zip, at + 30, name)?;
        let comment_len = le16(zip, at + 32, name)?;
        let local = le32(zip, at + 42, name)?;
        let entry_name = zip
            .get(at + 46..at + 46 + name_len)
            .ok_or_else(|| malformed(name, "an entry name runs past the file"))?;
        at += 46 + name_len + extra_len + comment_len;
        if entry_name != wanted.as_bytes() {
            continue;
        }
        if flags & 1 != 0 {
            return Err(CodecError::Unsupported(format!(
                "encrypted {name} archives are not supported"
            )));
        }
        if zip.get(local..local + 4) != Some(ZIP_LOCAL.as_slice()) {
            return Err(malformed(name, "a local header is damaged"));
        }
        let data_at = local + 30 + le16(zip, local + 26, name)? + le16(zip, local + 28, name)?;
        let data = zip
            .get(data_at..data_at.saturating_add(packed))
            .ok_or_else(|| malformed(name, format!("{wanted} runs past the file")))?;
        let bytes = match method {
            0 => {
                if data.len() as u64 > cap {
                    return Err(CodecError::LimitExceeded(format!(
                        "{wanted} is larger than {cap} bytes"
                    )));
                }
                data.to_vec()
            }
            8 => {
                let mut out = Vec::new();
                flate2::read::DeflateDecoder::new(data)
                    .take(cap.saturating_add(1))
                    .read_to_end(&mut out)
                    .map_err(|e| malformed(name, format!("{wanted} does not inflate: {e}")))?;
                if out.len() as u64 > cap {
                    return Err(CodecError::LimitExceeded(format!(
                        "{wanted} inflates past {cap} bytes"
                    )));
                }
                out
            }
            other => {
                return Err(CodecError::Unsupported(format!(
                    "{wanted} uses ZIP compression method {other}, which this build does not read"
                )))
            }
        };
        return Ok(Some(bytes));
    }
    Ok(None)
}

/// The first of `candidates` the archive holds, decoded as PNG.
fn zip_preview(
    zip: &[u8],
    candidates: &[&str],
    limits: ImportLimits,
    format: ImportFormat,
) -> Result<(DecodedSurface, String), CodecError> {
    for wanted in candidates {
        if let Some(png) = zip_entry(zip, wanted, limits.max_alloc_bytes, format.name())? {
            let mut surface = decode_surface_bytes_as(&png, limits, ImportFormat::Png)?;
            surface.source_format = format;
            return Ok((surface, (*wanted).to_string()));
        }
    }
    Err(CodecError::Unsupported(format!(
        "this {} file has no preview image ({}), and its vector artwork is not read by this build",
        format.name(),
        candidates.join(" / ")
    )))
}

// ---------------------------------------------------------------------- EPS

fn eps_note(what: &str, why: &str) -> String {
    format!("EPS: this is the file's embedded {what} preview, not the PostScript artwork (the PostScript interpreter could not draw it: {why})")
}

fn decode_eps(bytes: &[u8], limits: ImportLimits) -> Result<(DecodedSurface, String), CodecError> {
    const NAME: &str = "EPS";
    let section = |off_at: usize, len_at: usize| -> Result<Option<&[u8]>, CodecError> {
        let off = le32(bytes, off_at, NAME)?;
        let len = le32(bytes, len_at, NAME)?;
        if off == 0 || len == 0 {
            return Ok(None);
        }
        bytes
            .get(off..off.saturating_add(len))
            .map(Some)
            .ok_or_else(|| malformed(NAME, "a section runs past the end of the file"))
    };
    let dos = bytes.starts_with(&DOS_EPS);
    let postscript: &[u8] = if dos {
        section(4, 8)?.unwrap_or(&[])
    } else {
        bytes
    };
    // W15-E: the artwork first; a preview only when the PostScript cannot
    // be drawn, and then the note says why.
    let why = match super::postscript::render(postscript, limits) {
        Ok(drawn) => return Ok(drawn),
        Err(e) => e.to_string(),
    };
    if dos {
        if let Some(tiff) = section(20, 24)? {
            let mut s = decode_surface_bytes_as(tiff, limits, ImportFormat::Tiff)?;
            s.source_format = ImportFormat::Eps;
            return Ok((s, eps_note("TIFF", &why)));
        }
        if let Some(wmf) = section(12, 16)? {
            let mut s = metafile::decode_wmf(wmf, limits)?;
            s.source_format = ImportFormat::Eps;
            return Ok((s, eps_note("WMF", &why)));
        }
    }
    if let Some(s) = epsi_preview(postscript, limits)? {
        return Ok((s, eps_note("EPSI", &why)));
    }
    Err(CodecError::Unsupported(format!(
        "this EPS / PostScript file could not be drawn by the PostScript interpreter ({why}) \
         and has no embedded preview (TIFF, WMF or EPSI) to show instead; save it as PDF"
    )))
}

/// The EPSI preview (`%%BeginPreview: w h depth lines`, hex rows in comment
/// lines, `%%EndPreview`), as grey. Samples follow the PostScript `image`
/// operator's convention: 0 is black, the first row is the top.
fn epsi_preview(ps: &[u8], limits: ImportLimits) -> Result<Option<DecodedSurface>, CodecError> {
    const NAME: &str = "EPSI";
    let marker = b"%%BeginPreview:";
    let Some(start) = ps.windows(marker.len()).position(|w| w == marker) else {
        return Ok(None);
    };
    let rest = &ps[start + marker.len()..];
    let line_end = rest
        .iter()
        .position(|b| *b == b'\n' || *b == b'\r')
        .unwrap_or(rest.len());
    let header = std::str::from_utf8(&rest[..line_end])
        .map_err(|_| malformed(NAME, "the preview header is not text"))?;
    let nums: Vec<u32> = header
        .split_whitespace()
        .take(3)
        .map(|t| t.parse::<u32>())
        .collect::<Result<_, _>>()
        .map_err(|_| malformed(NAME, "the preview header is not three numbers"))?;
    let [width, height, depth] = nums[..] else {
        return Err(malformed(NAME, "the preview header is not three numbers"));
    };
    if !matches!(depth, 1 | 2 | 4 | 8) {
        return Err(malformed(NAME, format!("a {depth}-bit preview")));
    }
    check_decode(limits, width, height, 4, 0)?;
    let row_bytes = (width as usize * depth as usize).div_ceil(8);
    let need = row_bytes * height as usize;
    let mut data = Vec::with_capacity(need);
    let mut high: Option<u8> = None;
    let body = &rest[line_end..];
    let end = body
        .windows(b"%%EndPreview".len())
        .position(|w| w == b"%%EndPreview")
        .unwrap_or(body.len());
    for &c in &body[..end] {
        if data.len() == need {
            break;
        }
        let v = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            _ => continue,
        };
        match high.take() {
            Some(h) => data.push(h << 4 | v),
            None => high = Some(v),
        }
    }
    if data.len() < need {
        return Err(malformed(
            NAME,
            "the preview has fewer rows than it declares",
        ));
    }
    let max = (1u32 << depth) - 1;
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    for row in data.chunks_exact(row_bytes) {
        for x in 0..width as usize {
            let bit = x * depth as usize;
            let byte = row[bit / 8];
            let shift = 8 - depth as usize - bit % 8;
            let v = (u32::from(byte) >> shift) & max;
            let g = (v * 255 / max) as u8;
            rgba.extend_from_slice(&[g, g, g, 255]);
        }
    }
    Ok(Some(rgba8_surface(width, height, rgba, ImportFormat::Eps)))
}

// ---------------------------------------------------------------------- PDN

/// W13X-7: the layers of a `.pdn` (the .NET object graph after the header).
#[path = "pdn.rs"]
pub mod pdn;

fn decode_pdn(bytes: &[u8], limits: ImportLimits) -> Result<(DecodedSurface, String), CodecError> {
    const NAME: &str = "Paint.NET";
    if !looks_like_pdn(bytes) || bytes.len() < 7 {
        return Err(malformed(NAME, "no PDN3 header"));
    }
    let len = usize::from(bytes[4]) | usize::from(bytes[5]) << 8 | usize::from(bytes[6]) << 16;
    let xml = bytes
        .get(7..7 + len)
        .ok_or_else(|| malformed(NAME, "the header runs past the end of the file"))?;
    let attr = |name: &[u8]| -> Option<&[u8]> {
        let at = xml.windows(name.len()).position(|w| w == name)? + name.len();
        let end = xml[at..].iter().position(|b| *b == b'"')?;
        Some(&xml[at..at + end])
    };
    let thumb = xml
        .windows(6)
        .position(|w| w == b"<thumb")
        .and_then(|at| {
            let tail = &xml[at..];
            let key = b"png=\"";
            let p = tail.windows(key.len()).position(|w| w == key)? + key.len();
            let end = tail[p..].iter().position(|b| *b == b'"')?;
            Some(&tail[p..p + end])
        })
        .ok_or_else(|| {
            CodecError::Unsupported(
                "this Paint.NET file has no thumbnail to fall back on; save it as PNG or PSD \
                 from Paint.NET"
                    .into(),
            )
        })?;
    let png = base64_decode(thumb).ok_or_else(|| malformed(NAME, "the thumbnail is not base64"))?;
    let mut surface = decode_surface_bytes_as(&png, limits, ImportFormat::Png)?;
    surface.source_format = ImportFormat::Pdn;
    let dims = attr(b"width=\"")
        .zip(attr(b"height=\""))
        .map(|(w, h)| {
            format!(
                " of the {}x{} image",
                String::from_utf8_lossy(w),
                String::from_utf8_lossy(h)
            )
        })
        .unwrap_or_default();
    // W13X-7: File > Open reads the layers first (`pdn::read_layers`); the
    // caller that lands here says why they could not be read.
    let note = format!("Paint.NET: this is the file's flattened thumbnail{dims}, not its layers");
    Ok((surface, note))
}

// ----------------------------------------------------------- Sketch, XD, FIG

fn decode_zip_doc(
    format: ImportFormat,
    bytes: &[u8],
    limits: ImportLimits,
) -> Result<(DecodedSurface, String), CodecError> {
    if format == ImportFormat::Fig && looks_like_fig_kiwi(bytes) {
        return Err(fig_refusal());
    }
    if !bytes.starts_with(&ZIP_LOCAL) {
        return Err(match format {
            ImportFormat::Fig => fig_refusal(),
            _ => malformed(format.name(), "not a ZIP archive"),
        });
    }
    let candidates: &[&str] = match format {
        ImportFormat::Sketch => &["previews/preview.png"],
        ImportFormat::Xd => &["preview.png", "thumbnail.png"],
        _ => &["thumbnail.png"],
    };
    let (surface, entry) = zip_preview(bytes, candidates, limits, format)?;
    // W13X-8: File > Open reads the layers ([`design_files`]); this flat
    // route (thumbnails, Place, the codec facade) is the preview.
    let note = format!(
        "{}: this is the file's embedded preview image ({entry}), not its layers",
        format.name()
    );
    Ok((surface, note))
}

/// The refusal a bare Figma canvas gets.
pub fn fig_refusal() -> CodecError {
    CodecError::Unsupported(
        "this Figma .fig file is a bare fig-kiwi canvas (Figma's own binary format) with no \
         preview image to show here; File > Open reads its layers, or export the frames from \
         Figma as PNG, SVG or PDF"
            .into(),
    )
}

// -------------------------------------------------------------------- entry

/// Decode `bytes` as `format`, with the sentence that says what was opened
/// (which preview, and what was not read).
pub fn decode_described(
    format: ImportFormat,
    bytes: &[u8],
    limits: ImportLimits,
) -> Result<(DecodedSurface, String), CodecError> {
    match format {
        ImportFormat::Eps => decode_eps(bytes, limits),
        ImportFormat::Pdn => decode_pdn(bytes, limits),
        ImportFormat::Sketch | ImportFormat::Xd | ImportFormat::Fig => {
            decode_zip_doc(format, bytes, limits)
        }
        other => Err(CodecError::Unsupported(format!(
            "{} is not read by this module",
            other.name()
        ))),
    }
}

/// Header facts: the preview is decoded to learn its size.
pub(super) fn probe(
    format: ImportFormat,
    bytes: &[u8],
    limits: ImportLimits,
) -> Result<ImageInfo, CodecError> {
    let (s, _) = decode_described(format, bytes, limits)?;
    Ok(super::info(s.width, s.height, format, false))
}

/// The flat codec's decode.
pub(super) fn decode(
    format: ImportFormat,
    bytes: &[u8],
    limits: ImportLimits,
) -> Result<DecodedSurface, CodecError> {
    decode_described(format, bytes, limits).map(|(s, _)| s)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::codec::{decode_surface_bytes, encode, ExportFormat, SurfacePixels};

    fn solid(w: u32, h: u32, c: [u8; 4]) -> Vec<u8> {
        (0..w * h).flat_map(|_| c).collect()
    }

    fn px(s: &DecodedSurface, x: u32, y: u32) -> [u8; 4] {
        let SurfacePixels::Rgba8(p) = &s.pixels else {
            panic!("8-bit expected")
        };
        let i = ((y * s.width + x) * 4) as usize;
        [p[i], p[i + 1], p[i + 2], p[i + 3]]
    }

    /// A stored (method 0) ZIP of `entries`, first entry first.
    pub(crate) fn build_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, data) in entries {
            let local = out.len() as u32;
            out.extend_from_slice(&ZIP_LOCAL);
            out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            out.extend_from_slice(&[0; 4]); // crc (not checked)
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&(name.len() as u16).to_le_bytes());
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(data);
            central.extend_from_slice(&ZIP_CENTRAL);
            central.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            central.extend_from_slice(&[0; 4]);
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(data.len() as u32).to_le_bytes());
            central.extend_from_slice(&(name.len() as u16).to_le_bytes());
            central.extend_from_slice(&[0; 12]);
            central.extend_from_slice(&local.to_le_bytes());
            central.extend_from_slice(name.as_bytes());
        }
        let dir_at = out.len() as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(&ZIP_END);
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(central.len() as u32).to_le_bytes());
        out.extend_from_slice(&dir_at.to_le_bytes());
        out.extend_from_slice(&[0; 2]);
        out
    }

    fn png(w: u32, h: u32, c: [u8; 4]) -> Vec<u8> {
        encode(ExportFormat::Png, w, h, &solid(w, h, c)).unwrap()
    }

    #[test]
    fn base64_round_trips() {
        for n in 0..20u8 {
            let data: Vec<u8> = (0..n).map(|i| i.wrapping_mul(37)).collect();
            assert_eq!(
                base64_decode(base64_encode(&data).as_bytes()).unwrap(),
                data
            );
        }
        assert_eq!(base64_encode(b"Man"), "TWFu");
        assert_eq!(base64_decode(b"TW Fu\n").unwrap(), b"Man");
        assert!(base64_decode(b"TW*u").is_none());
    }

    pub(crate) fn dos_eps_with_tiff() -> Vec<u8> {
        let tiff = encode(ExportFormat::Tiff, 3, 2, &solid(3, 2, [20, 90, 200, 255])).unwrap();
        let ps = b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 3 2\n";
        let mut v = DOS_EPS.to_vec();
        let ps_at = 30u32;
        let tiff_at = ps_at + ps.len() as u32;
        for x in [ps_at, ps.len() as u32, 0, 0, tiff_at, tiff.len() as u32] {
            v.extend_from_slice(&x.to_le_bytes());
        }
        v.extend_from_slice(&[0xFF, 0xFF]);
        v.extend_from_slice(ps);
        v.extend_from_slice(&tiff);
        v
    }

    #[test]
    fn a_dos_eps_opens_its_tiff_preview_and_says_so() {
        let eps = dos_eps_with_tiff();
        let s = decode_surface_bytes(&eps, ImportLimits::default()).unwrap();
        assert_eq!(
            (s.width, s.height, s.source_format),
            (3, 2, ImportFormat::Eps)
        );
        assert_eq!(px(&s, 1, 1), [20, 90, 200, 255]);
        let (_, note) = decode_described(ImportFormat::Eps, &eps, ImportLimits::default()).unwrap();
        assert!(note.contains("TIFF preview"), "{note}");
        assert!(note.contains("not the PostScript"), "{note}");
    }

    #[test]
    fn a_dos_eps_with_a_wmf_preview_draws_it() {
        let wmf = super::metafile::tests::sample_wmf();
        let mut v = DOS_EPS.to_vec();
        for x in [0u32, 0, 30, wmf.len() as u32, 0, 0] {
            v.extend_from_slice(&x.to_le_bytes());
        }
        v.extend_from_slice(&[0xFF, 0xFF]);
        v.extend_from_slice(&wmf);
        let (s, note) = decode_described(ImportFormat::Eps, &v, ImportLimits::default()).unwrap();
        assert_eq!(
            (s.width, s.height, s.source_format),
            (40, 20, ImportFormat::Eps)
        );
        assert_eq!(px(&s, 5, 10), [255, 0, 0, 255]);
        assert!(note.contains("WMF preview"), "{note}");
    }

    #[test]
    fn an_epsi_preview_is_read_and_a_bare_eps_is_refused_by_name() {
        // 4x2, 1 bit: row 0 = 1010 (white black white black), row 1 = 0000.
        let eps = b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 4 2\n\
%%BeginPreview: 4 2 1 2\n% A0\n% 00\n%%EndPreview\nshowpage\n";
        let s = decode_surface_bytes(eps, ImportLimits::default()).unwrap();
        assert_eq!(
            (s.width, s.height, s.source_format),
            (4, 2, ImportFormat::Eps)
        );
        assert_eq!(px(&s, 0, 0), [255, 255, 255, 255]);
        assert_eq!(px(&s, 1, 0), [0, 0, 0, 255]);
        assert_eq!(px(&s, 2, 1), [0, 0, 0, 255]);
        let bare = b"%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 4 2\nnewpath 0 0 moveto\n";
        let err = decode_surface_bytes(bare, ImportLimits::default()).unwrap_err();
        assert!(err.to_string().contains("no embedded preview"), "{err}");
        assert!(err.to_string().contains("drew nothing"), "{err}");
    }

    pub(crate) fn pdn_file(thumb_png: &[u8]) -> Vec<u8> {
        let xml = format!(
            "<pdnImage width=\"800\" height=\"600\" layers=\"3\"><custom><thumb png=\"{}\" /></custom></pdnImage>",
            base64_encode(thumb_png)
        );
        let mut v = b"PDN3".to_vec();
        let n = xml.len() as u32;
        v.extend_from_slice(&n.to_le_bytes()[..3]);
        v.extend_from_slice(xml.as_bytes());
        v.extend_from_slice(&[0, 1, 0, 0, 0, 0xFF, 0xFF]); // start of the object graph
        v
    }

    #[test]
    fn a_pdn_opens_its_thumbnail_and_says_the_layers_are_not_read() {
        let pdn = pdn_file(&png(8, 6, [250, 10, 10, 255]));
        let s = decode_surface_bytes(&pdn, ImportLimits::default()).unwrap();
        assert_eq!(
            (s.width, s.height, s.source_format),
            (8, 6, ImportFormat::Pdn)
        );
        assert_eq!(px(&s, 4, 3), [250, 10, 10, 255]);
        let (_, note) = decode_described(ImportFormat::Pdn, &pdn, ImportLimits::default()).unwrap();
        assert!(note.contains("thumbnail of the 800x600 image"), "{note}");
        assert!(note.contains("not its layers"), "{note}");
    }

    #[test]
    fn sketch_xd_and_zipped_fig_open_their_previews() {
        let limits = ImportLimits::default();
        let sketch = build_zip(&[
            ("document.json", b"{}"),
            ("previews/preview.png", &png(5, 4, [1, 2, 3, 255])),
        ]);
        let s =
            crate::codec::decode_surface_bytes_as(&sketch, limits, ImportFormat::Sketch).unwrap();
        assert_eq!(
            (s.width, s.height, s.source_format),
            (5, 4, ImportFormat::Sketch)
        );
        let xd = build_zip(&[
            ("mimetype", b"application/vnd.adobe.sparkler.project+dcxucf"),
            ("preview.png", &png(7, 3, [9, 9, 9, 255])),
        ]);
        // XD is recognised by content, no hint needed.
        let s = decode_surface_bytes(&xd, limits).unwrap();
        assert_eq!(
            (s.width, s.height, s.source_format),
            (7, 3, ImportFormat::Xd)
        );
        let (_, note) = decode_described(ImportFormat::Xd, &xd, limits).unwrap();
        assert!(note.contains("preview image (preview.png)"), "{note}");
        let fig = build_zip(&[
            ("canvas.fig", b"fig-kiwi"),
            ("thumbnail.png", &png(2, 2, [0, 0, 0, 255])),
        ]);
        let s = crate::codec::decode_surface_bytes_as(&fig, limits, ImportFormat::Fig).unwrap();
        assert_eq!(
            (s.width, s.height, s.source_format),
            (2, 2, ImportFormat::Fig)
        );
    }

    #[test]
    fn a_bare_fig_is_refused_by_name_and_a_previewless_archive_says_why() {
        let limits = ImportLimits::default();
        let kiwi = b"fig-kiwi\x0f\0\0\0rest of the canvas";
        let err = decode_surface_bytes(kiwi, limits).unwrap_err();
        assert!(err.to_string().contains("fig-kiwi"), "{err}");
        assert!(err.to_string().contains("export"), "{err}");
        let no_preview = build_zip(&[("document.json", b"{}")]);
        let err = crate::codec::decode_surface_bytes_as(&no_preview, limits, ImportFormat::Sketch)
            .unwrap_err();
        assert!(err.to_string().contains("no preview image"), "{err}");
    }

    #[test]
    fn malformed_documents_error_and_never_panic() {
        let limits = ImportLimits::default();
        let samples: Vec<(ImportFormat, Vec<u8>)> = vec![
            (ImportFormat::Eps, dos_eps_with_tiff()),
            (ImportFormat::Pdn, pdn_file(&png(3, 3, [1, 1, 1, 255]))),
            (
                ImportFormat::Sketch,
                build_zip(&[("previews/preview.png", &png(3, 3, [1, 1, 1, 255]))]),
            ),
            (
                ImportFormat::Xd,
                build_zip(&[
                    ("mimetype", b"application/vnd.adobe.sparkler.project+dcxucf"),
                    ("preview.png", &png(3, 3, [1, 1, 1, 255])),
                ]),
            ),
        ];
        for (format, bytes) in &samples {
            for cut in 0..bytes.len() {
                let _ = decode_described(*format, &bytes[..cut], limits);
            }
            let mut flipped = bytes.clone();
            for i in (0..flipped.len()).step_by(3) {
                flipped[i] ^= 0x5A;
                let _ = decode_described(*format, &flipped, limits);
                flipped[i] ^= 0x5A;
            }
        }
        // An EPS section pointing past the file.
        let mut eps = dos_eps_with_tiff();
        eps[20..24].copy_from_slice(&0xFFFF_0000u32.to_le_bytes());
        assert!(decode_described(ImportFormat::Eps, &eps, limits).is_err());
        // An EPSI preview declaring more than the limits allow.
        let big = b"%!PS\n%%BeginPreview: 900000 900000 8 1\n% 00\n%%EndPreview\n";
        assert!(matches!(
            decode_described(ImportFormat::Eps, big, limits),
            Err(CodecError::LimitExceeded(_))
        ));
        // A PDN header longer than the file.
        assert!(decode_described(ImportFormat::Pdn, b"PDN3\xFF\xFF\x00<pdnImage", limits).is_err());
    }
}
