//! W11-H: Apple icon files (`.icns`), read only, by this crate's own parser.
//!
//! An `.icns` is a list of `(type, length, data)` entries, one per size (and
//! per density). What is read, choosing the **largest** entry that decodes:
//!
//! * PNG entries (`ic07`..`ic14`, `icp4`..`icp6`, and any other entry whose
//!   data is a PNG), decoded by the codec facade under the same limits;
//! * the 32-bit ARGB entries (`ic04` 16 px, `ic05` 32 px), PackBits-style
//!   run-length coded per channel;
//! * the classic 24-bit entries (`is32` 16, `il32` 32, `ih32` 48, `it32`
//!   128 px), run-length coded per channel, with their 8-bit alpha masks
//!   (`s8mk`, `l8mk`, `h8mk`, `t8mk`) when present, opaque when not.
//!
//! JPEG 2000 entries (some `ic08`..`ic10` in icons from 10.5 era tools) are
//! skipped: there is no JPEG 2000 decoder in the tree. A file with nothing
//! else is refused by name. The 1-bit and 8-bit palette icons (`ICN#`,
//! `icl8`...) are not read.
//!
//! # Untrusted input
//!
//! Every entry length is checked against what is left of the file before it
//! is sliced, entry sizes come from the entry type (not from the data), the
//! run-length decoder refuses output past the plane it was asked for, and a
//! PNG entry goes through the facade with the caller's [`ImportLimits`].

use super::{check_decode, info, malformed, rgba8_surface};
use crate::codec::{CodecError, DecodedSurface, ImageInfo, ImportFormat, ImportLimits};

const NAME: &str = "ICNS";

/// `true` for an `.icns` file.
pub fn looks_like_icns(head: &[u8]) -> bool {
    head.len() >= 8 && &head[..4] == b"icns"
}

/// How one entry's pixels are stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Png,
    Argb,
    /// 24-bit RGB; `mask` is the type code of its alpha mask.
    Rgb {
        mask: [u8; 4],
        it32: bool,
    },
}

#[derive(Debug, Clone, Copy)]
struct Entry<'a> {
    kind: Kind,
    side: u32,
    height: u32,
    data: &'a [u8],
}

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Every `(type, data)` entry, in file order.
type Entries<'a> = Vec<([u8; 4], &'a [u8])>;

fn entries(bytes: &[u8]) -> Result<Entries<'_>, CodecError> {
    if !looks_like_icns(bytes) {
        return Err(malformed(NAME, "no 'icns' signature"));
    }
    let declared = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
    let end = declared.min(bytes.len());
    let mut out = Vec::new();
    let mut at = 8usize;
    while at + 8 <= end {
        let kind: [u8; 4] = bytes[at..at + 4].try_into().expect("four bytes");
        let len =
            u32::from_be_bytes(bytes[at + 4..at + 8].try_into().expect("four bytes")) as usize;
        if len < 8 || len > end - at {
            return Err(malformed(
                NAME,
                format!("entry {kind:?} runs past the file"),
            ));
        }
        out.push((kind, &bytes[at + 8..at + len]));
        at += len;
        if out.len() > 4096 {
            return Err(malformed(NAME, "too many entries"));
        }
    }
    Ok(out)
}

/// The entries this reader can decode, largest first.
fn candidates(bytes: &[u8]) -> Result<Vec<Entry<'_>>, CodecError> {
    let list = entries(bytes)?;
    let mut out = Vec::new();
    for (kind, data) in &list {
        let data: &[u8] = data;
        if data.starts_with(&PNG_SIGNATURE) {
            // IHDR is the first chunk: width and height at bytes 16..24.
            if data.len() < 24 {
                continue;
            }
            let w = u32::from_be_bytes(data[16..20].try_into().expect("four"));
            let h = u32::from_be_bytes(data[20..24].try_into().expect("four"));
            out.push(Entry {
                kind: Kind::Png,
                side: w,
                height: h,
                data,
            });
            continue;
        }
        let (k, side) = match kind {
            b"ic04" if data.starts_with(b"ARGB") => (Kind::Argb, 16),
            b"ic05" if data.starts_with(b"ARGB") => (Kind::Argb, 32),
            b"is32" => (
                Kind::Rgb {
                    mask: *b"s8mk",
                    it32: false,
                },
                16,
            ),
            b"il32" => (
                Kind::Rgb {
                    mask: *b"l8mk",
                    it32: false,
                },
                32,
            ),
            b"ih32" => (
                Kind::Rgb {
                    mask: *b"h8mk",
                    it32: false,
                },
                48,
            ),
            b"it32" => (
                Kind::Rgb {
                    mask: *b"t8mk",
                    it32: true,
                },
                128,
            ),
            _ => continue,
        };
        out.push(Entry {
            kind: k,
            side,
            height: side,
            data,
        });
    }
    // Largest first; PNG before a same-size legacy entry.
    out.sort_by_key(|e| {
        (
            std::cmp::Reverse(u64::from(e.side) * u64::from(e.height)),
            e.kind != Kind::Png,
        )
    });
    if out.is_empty() {
        return Err(CodecError::Unsupported(
            "this .icns has no PNG, ARGB or 24-bit RGB image (JPEG 2000 and palette icons \
             are not read)"
                .into(),
        ));
    }
    Ok(out)
}

/// The icon run-length code: a control byte `n < 0x80` copies `n + 1`
/// literal bytes, `n >= 0x80` repeats the next byte `n - 125` times. Stops
/// at exactly `want` bytes; returns how much input it used.
fn unpack(src: &[u8], want: usize, out: &mut Vec<u8>) -> Result<usize, CodecError> {
    let start = out.len();
    let mut at = 0usize;
    while out.len() - start < want {
        let Some(&n) = src.get(at) else {
            return Err(malformed(NAME, "run-length data ends early"));
        };
        at += 1;
        let left = want - (out.len() - start);
        if n < 0x80 {
            let count = usize::from(n) + 1;
            let Some(run) = src.get(at..at + count) else {
                return Err(malformed(NAME, "a literal run ends early"));
            };
            if count > left {
                return Err(malformed(NAME, "a literal run overflows the plane"));
            }
            out.extend_from_slice(run);
            at += count;
        } else {
            let count = usize::from(n) - 125;
            let Some(&v) = src.get(at) else {
                return Err(malformed(NAME, "a repeat run ends early"));
            };
            if count > left {
                return Err(malformed(NAME, "a repeat run overflows the plane"));
            }
            at += 1;
            out.resize(out.len() + count, v);
        }
    }
    Ok(at)
}

/// Planes (R, G, B[, A]) of `plane` bytes each, run-length coded one after
/// the other.
fn unpack_planes(src: &[u8], plane: usize, planes: usize) -> Result<Vec<u8>, CodecError> {
    let mut out = Vec::with_capacity(plane * planes);
    let mut at = 0usize;
    for _ in 0..planes {
        at += unpack(&src[at.min(src.len())..], plane, &mut out)?;
    }
    Ok(out)
}

fn decode_entry(
    entry: &Entry<'_>,
    all: &[([u8; 4], &[u8])],
    limits: ImportLimits,
) -> Result<DecodedSurface, CodecError> {
    match entry.kind {
        Kind::Png => {
            let mut s =
                crate::codec::decode_surface_bytes_as(entry.data, limits, ImportFormat::Png)?;
            if s.source_format != ImportFormat::Png {
                return Err(malformed(
                    NAME,
                    "an entry that starts like a PNG is not one",
                ));
            }
            s.source_format = ImportFormat::Icns;
            Ok(s)
        }
        Kind::Argb => {
            let side = entry.side;
            let plane = (side * side) as usize;
            check_decode(limits, side, side, 4, plane as u64 * 4)?;
            let planes = unpack_planes(&entry.data[4..], plane, 4)?;
            let mut rgba = vec![0u8; plane * 4];
            for i in 0..plane {
                rgba[i * 4] = planes[plane + i];
                rgba[i * 4 + 1] = planes[2 * plane + i];
                rgba[i * 4 + 2] = planes[3 * plane + i];
                rgba[i * 4 + 3] = planes[i];
            }
            Ok(rgba8_surface(side, side, rgba, ImportFormat::Icns))
        }
        Kind::Rgb { mask, it32 } => {
            let side = entry.side;
            let plane = (side * side) as usize;
            check_decode(limits, side, side, 4, plane as u64 * 3)?;
            let mut data = entry.data;
            if it32 {
                // `it32` data opens with four bytes (zero in every file seen).
                data = data.get(4..).unwrap_or(&[]);
            }
            let mut rgba = vec![255u8; plane * 4];
            if data.len() == plane * 4 {
                // Uncompressed: one unused byte, then R, G, B.
                for i in 0..plane {
                    rgba[i * 4..i * 4 + 3].copy_from_slice(&data[i * 4 + 1..i * 4 + 4]);
                }
            } else {
                let planes = unpack_planes(data, plane, 3)?;
                for i in 0..plane {
                    rgba[i * 4] = planes[i];
                    rgba[i * 4 + 1] = planes[plane + i];
                    rgba[i * 4 + 2] = planes[2 * plane + i];
                }
            }
            if let Some((_, m)) = all.iter().find(|(k, _)| *k == mask) {
                if m.len() != plane {
                    return Err(malformed(NAME, "an alpha mask is the wrong size"));
                }
                for (i, a) in m.iter().enumerate() {
                    rgba[i * 4 + 3] = *a;
                }
            }
            Ok(rgba8_surface(side, side, rgba, ImportFormat::Icns))
        }
    }
}

/// Header facts: the largest entry's size.
pub fn probe(bytes: &[u8], limits: ImportLimits) -> Result<ImageInfo, CodecError> {
    let best = candidates(bytes)?[0];
    limits.check_dimensions(best.side, best.height)?;
    Ok(info(best.side, best.height, ImportFormat::Icns, false))
}

/// Decode the largest entry that decodes.
pub fn decode(bytes: &[u8], limits: ImportLimits) -> Result<DecodedSurface, CodecError> {
    let all = entries(bytes)?;
    let mut first_error = None;
    for entry in candidates(bytes)? {
        match decode_entry(&entry, &all, limits) {
            Ok(s) => return Ok(s),
            Err(e) => {
                first_error.get_or_insert(e);
            }
        }
    }
    Err(first_error.unwrap_or_else(|| malformed(NAME, "no entry decodes")))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::codec::{decode_surface_bytes, encode, probe_bytes, ExportFormat, SurfacePixels};

    /// Worst-case-legal run-length coding: literal runs of up to 128.
    fn pack(plane: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < plane.len() {
            // A repeat run where three or more bytes match.
            let mut run = 1;
            while i + run < plane.len() && plane[i + run] == plane[i] && run < 130 {
                run += 1;
            }
            if run >= 3 {
                out.push((run + 125) as u8);
                out.push(plane[i]);
                i += run;
            } else {
                let n = (plane.len() - i).min(128);
                out.push((n - 1) as u8);
                out.extend_from_slice(&plane[i..i + n]);
                i += n;
            }
        }
        out
    }

    pub(crate) fn icns(entries: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut body = Vec::new();
        for (kind, data) in entries {
            body.extend_from_slice(*kind);
            body.extend_from_slice(&((data.len() + 8) as u32).to_be_bytes());
            body.extend_from_slice(data);
        }
        let mut out = b"icns".to_vec();
        out.extend_from_slice(&((body.len() + 8) as u32).to_be_bytes());
        out.extend_from_slice(&body);
        out
    }

    fn px(i: usize) -> [u8; 4] {
        [(i * 7) as u8, (i * 3 + 1) as u8, 90, (i * 5) as u8]
    }

    fn argb_entry(side: usize) -> Vec<u8> {
        let n = side * side;
        let mut out = b"ARGB".to_vec();
        for ch in [3usize, 0, 1, 2] {
            let plane: Vec<u8> = (0..n).map(|i| px(i)[ch]).collect();
            out.extend(pack(&plane));
        }
        out
    }

    fn rgb_entry(side: usize) -> (Vec<u8>, Vec<u8>) {
        let n = side * side;
        let mut out = Vec::new();
        for ch in 0..3 {
            let plane: Vec<u8> = (0..n)
                .map(|i| if i % 9 < 4 { 17 } else { px(i)[ch] })
                .collect();
            out.extend(pack(&plane));
        }
        let mask = (0..n).map(|i| px(i)[3]).collect();
        (out, mask)
    }

    #[test]
    fn the_largest_entry_wins_and_every_encoding_decodes() {
        // A 48 px PNG beats a 32 px ARGB and a 16 px RGB.
        let png_px: Vec<u8> = (0..48 * 48).flat_map(px).collect();
        let png = encode(ExportFormat::Png, 48, 48, &png_px).unwrap();
        let (rgb, mask) = rgb_entry(16);
        let file = icns(&[
            (b"is32", rgb.clone()),
            (b"s8mk", mask.clone()),
            (b"ic05", argb_entry(32)),
            (b"icp6", png),
        ]);
        let info = probe_bytes(&file, ImportLimits::default()).unwrap();
        assert_eq!((info.width, info.format), (48, ImportFormat::Icns));
        let s = decode_surface_bytes(&file, ImportLimits::default()).unwrap();
        assert_eq!(
            (s.width, s.height, s.source_format),
            (48, 48, ImportFormat::Icns)
        );
        assert_eq!(s.pixels, SurfacePixels::Rgba8(png_px));

        // Only the ARGB and RGB entries: the 32 px ARGB one, exactly.
        let file = icns(&[
            (b"is32", rgb.clone()),
            (b"s8mk", mask.clone()),
            (b"ic05", argb_entry(32)),
        ]);
        let s = decode_surface_bytes(&file, ImportLimits::default()).unwrap();
        let want: Vec<u8> = (0..32 * 32).flat_map(px).collect();
        assert_eq!((s.width, s.pixels), (32, SurfacePixels::Rgba8(want)));

        // Only the 16 px RGB entry and its mask.
        let file = icns(&[(b"is32", rgb), (b"s8mk", mask)]);
        let s = decode_surface_bytes(&file, ImportLimits::default()).unwrap();
        let want: Vec<u8> = (0..256)
            .flat_map(|i| {
                let p = px(i);
                let c = |ch: usize| if i % 9 < 4 { 17 } else { p[ch] };
                [c(0), c(1), c(2), p[3]]
            })
            .collect();
        assert_eq!(s.pixels, SurfacePixels::Rgba8(want));
    }

    #[test]
    fn a_jpeg_2000_only_icon_is_refused_by_name() {
        let file = icns(&[(b"ic10", b"\0\0\0\x0cjP  \r\n\x87\n".to_vec())]);
        let err = decode_surface_bytes(&file, ImportLimits::default()).unwrap_err();
        assert!(err.to_string().contains("JPEG 2000"), "{err}");
    }

    #[test]
    fn damaged_icons_error_and_never_panic() {
        let png = encode(ExportFormat::Png, 20, 20, &[9u8; 20 * 20 * 4]).unwrap();
        let (rgb, mask) = rgb_entry(16);
        let file = icns(&[
            (b"is32", rgb),
            (b"s8mk", mask),
            (b"ic04", argb_entry(16)),
            (b"ic07", png),
        ]);
        for cut in 0..file.len() {
            let _ = decode_surface_bytes(&file[..cut], ImportLimits::default());
        }
        for i in 8..file.len() {
            let mut bad = file.clone();
            bad[i] ^= 0xFF;
            let _ = decode_surface_bytes(&bad, ImportLimits::default());
            let _ = probe_bytes(&bad, ImportLimits::default());
        }
        // An entry claiming more than the file holds is refused by name.
        let mut lying = icns(&[(b"is32", vec![0; 4])]);
        lying[12..16].copy_from_slice(&1_000u32.to_be_bytes());
        let err = decode_surface_bytes(&lying, ImportLimits::default()).unwrap_err();
        assert!(err.to_string().contains("ICNS"), "{err}");
    }
}
