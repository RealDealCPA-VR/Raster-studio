//! W11-H: Krita documents (`.kra`), read only, as their **merged image**.
//!
//! A `.kra` is a ZIP archive: a stored `mimetype` entry reading
//! `application/x-krita`, `maindoc.xml` (the layer tree), one file per layer
//! in Krita's own tiled, LZF-compressed pixel format, and `mergedimage.png`,
//! the flattened composite Krita writes on every save. What opens is that
//! PNG, decoded by the codec facade under the caller's limits. The layers are
//! **not** read: their pixel data is Krita's tiled format, not PNG, and
//! decoding it is not done here, so a `.kra` opens flattened.
//!
//! The ZIP reader is this module's own, and small: the end-of-central-
//! directory record, the central directory, and one local header; entries
//! stored (method 0) or deflated (method 8, `flate2`). ZIP64 archives and
//! encrypted entries are refused by name.
//!
//! # Untrusted input
//!
//! Every offset is checked against the file before it is followed, the
//! central directory is walked at most once per declared entry, and a
//! deflated entry is inflated through a reader capped at
//! [`ImportLimits::max_alloc_bytes`], whatever size the directory declares.

use std::io::Read;

use super::malformed;
use crate::codec::{CodecError, DecodedSurface, ImageInfo, ImportFormat, ImportLimits};

const NAME: &str = "Krita";

const LOCAL: [u8; 4] = *b"PK\x03\x04";
const CENTRAL: [u8; 4] = *b"PK\x01\x02";
const END: [u8; 4] = *b"PK\x05\x06";
const MIMETYPE: &[u8] = b"application/x-krita";

/// `true` when `head` is a ZIP whose first entry is a `mimetype` naming
/// Krita, which is how Krita writes every `.kra`.
pub fn looks_like_kra(head: &[u8]) -> bool {
    if head.len() < 30 || head[..4] != LOCAL {
        return false;
    }
    let name_len = usize::from(u16::from_le_bytes([head[26], head[27]]));
    let extra_len = usize::from(u16::from_le_bytes([head[28], head[29]]));
    let data = 30 + name_len + extra_len;
    head.get(30..30 + name_len) == Some(b"mimetype".as_slice())
        && head.get(data..data + MIMETYPE.len()) == Some(MIMETYPE)
}

fn le16(b: &[u8], at: usize) -> Result<usize, CodecError> {
    b.get(at..at + 2)
        .map(|s| usize::from(u16::from_le_bytes([s[0], s[1]])))
        .ok_or_else(|| malformed(NAME, "the archive ends inside a record"))
}

fn le32(b: &[u8], at: usize) -> Result<usize, CodecError> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize)
        .ok_or_else(|| malformed(NAME, "the archive ends inside a record"))
}

/// The bytes of the archive entry called `wanted`, inflated, or `None`
/// when the archive has no such entry.
pub(crate) fn entry(zip: &[u8], wanted: &str, cap: u64) -> Result<Option<Vec<u8>>, CodecError> {
    if zip.len() < 22 {
        return Err(malformed(NAME, "too short to be a ZIP archive"));
    }
    // The end record is the last 22 bytes plus a comment of up to 64 KiB.
    let floor = zip.len().saturating_sub(22 + 0xFFFF);
    let end = (floor..=zip.len().saturating_sub(22))
        .rev()
        .find(|&i| zip[i..i + 4] == END)
        .ok_or_else(|| malformed(NAME, "no ZIP end-of-directory record"))?;
    let count = le16(zip, end + 10)?;
    let dir_size = le32(zip, end + 12)?;
    let dir_at = le32(zip, end + 16)?;
    if count == 0xFFFF || dir_at == 0xFFFF_FFFF || dir_size == 0xFFFF_FFFF {
        return Err(CodecError::Unsupported(
            "ZIP64 Krita archives are not supported".into(),
        ));
    }
    let mut at = dir_at;
    for _ in 0..count {
        if zip.get(at..at + 4) != Some(CENTRAL.as_slice()) {
            return Err(malformed(NAME, "a central directory record is damaged"));
        }
        let flags = le16(zip, at + 8)?;
        let method = le16(zip, at + 10)?;
        let packed = le32(zip, at + 20)?;
        let name_len = le16(zip, at + 28)?;
        let extra_len = le16(zip, at + 30)?;
        let comment_len = le16(zip, at + 32)?;
        let local = le32(zip, at + 42)?;
        let name = zip
            .get(at + 46..at + 46 + name_len)
            .ok_or_else(|| malformed(NAME, "an entry name runs past the file"))?;
        at += 46 + name_len + extra_len + comment_len;
        if name != wanted.as_bytes() {
            continue;
        }
        if flags & 1 != 0 {
            return Err(CodecError::Unsupported(
                "encrypted Krita archives are not supported".into(),
            ));
        }
        if zip.get(local..local + 4) != Some(LOCAL.as_slice()) {
            return Err(malformed(NAME, "a local header is damaged"));
        }
        let data_at = local + 30 + le16(zip, local + 26)? + le16(zip, local + 28)?;
        let data = zip
            .get(data_at..data_at.saturating_add(packed))
            .ok_or_else(|| malformed(NAME, format!("{wanted} runs past the file")))?;
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
                    .map_err(|e| malformed(NAME, format!("{wanted} does not inflate: {e}")))?;
                if out.len() as u64 > cap {
                    return Err(CodecError::LimitExceeded(format!(
                        "{wanted} inflates past {cap} bytes"
                    )));
                }
                out
            }
            other => {
                return Err(CodecError::Unsupported(format!(
                    "ZIP compression method {other} in a Krita archive is not supported"
                )))
            }
        };
        return Ok(Some(bytes));
    }
    Ok(None)
}

fn merged_png(bytes: &[u8], limits: ImportLimits) -> Result<Vec<u8>, CodecError> {
    let mimetype = entry(bytes, "mimetype", 64)?;
    if mimetype.as_deref() != Some(MIMETYPE) {
        return Err(malformed(
            NAME,
            "the archive's mimetype is not application/x-krita",
        ));
    }
    entry(bytes, "mergedimage.png", limits.max_alloc_bytes)?.ok_or_else(|| {
        CodecError::Unsupported(
            "this .kra has no mergedimage.png (Krita's layer data itself is not read)".into(),
        )
    })
}

/// Header facts, from the merged PNG's header.
pub fn probe(bytes: &[u8], limits: ImportLimits) -> Result<ImageInfo, CodecError> {
    let png = merged_png(bytes, limits)?;
    let mut info = crate::codec::probe_bytes_as(&png, limits, ImportFormat::Png)?;
    if info.format != ImportFormat::Png {
        return Err(malformed(NAME, "mergedimage.png is not a PNG"));
    }
    info.format = ImportFormat::Kra;
    Ok(info)
}

/// Decode the merged image.
pub fn decode(bytes: &[u8], limits: ImportLimits) -> Result<DecodedSurface, CodecError> {
    let png = merged_png(bytes, limits)?;
    let mut s = crate::codec::decode_surface_bytes_as(&png, limits, ImportFormat::Png)?;
    if s.source_format != ImportFormat::Png {
        return Err(malformed(NAME, "mergedimage.png is not a PNG"));
    }
    s.source_format = ImportFormat::Kra;
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_surface_bytes, encode, probe_bytes, ExportFormat, SurfacePixels};
    use std::io::Write;

    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for b in bytes {
            crc ^= u32::from(*b);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xEDB8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    /// A ZIP the way Krita writes one: `mimetype` stored first, the rest
    /// deflated (when `deflate`).
    fn zip(entries: &[(&str, &[u8])], deflate: bool) -> Vec<u8> {
        let mut out = Vec::new();
        let mut dir = Vec::new();
        for (i, (name, data)) in entries.iter().enumerate() {
            let method: u16 = if deflate && i > 0 { 8 } else { 0 };
            let stored = if method == 8 {
                let mut e =
                    flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
                e.write_all(data).unwrap();
                e.finish().unwrap()
            } else {
                data.to_vec()
            };
            let local = out.len() as u32;
            let crc = crc32(data);
            let mut h = Vec::new();
            h.extend_from_slice(&LOCAL);
            h.extend_from_slice(&[20, 0, 0, 0]);
            h.extend_from_slice(&method.to_le_bytes());
            h.extend_from_slice(&[0; 4]);
            h.extend_from_slice(&crc.to_le_bytes());
            h.extend_from_slice(&(stored.len() as u32).to_le_bytes());
            h.extend_from_slice(&(data.len() as u32).to_le_bytes());
            h.extend_from_slice(&(name.len() as u16).to_le_bytes());
            h.extend_from_slice(&[0, 0]);
            h.extend_from_slice(name.as_bytes());
            out.extend(h);
            out.extend_from_slice(&stored);

            dir.extend_from_slice(&CENTRAL);
            dir.extend_from_slice(&[20, 0, 20, 0, 0, 0]);
            dir.extend_from_slice(&method.to_le_bytes());
            dir.extend_from_slice(&[0; 4]);
            dir.extend_from_slice(&crc.to_le_bytes());
            dir.extend_from_slice(&(stored.len() as u32).to_le_bytes());
            dir.extend_from_slice(&(data.len() as u32).to_le_bytes());
            dir.extend_from_slice(&(name.len() as u16).to_le_bytes());
            dir.extend_from_slice(&[0; 12]);
            dir.extend_from_slice(&local.to_le_bytes());
            dir.extend_from_slice(name.as_bytes());
        }
        let dir_at = out.len() as u32;
        out.extend_from_slice(&dir);
        out.extend_from_slice(&END);
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
        out.extend_from_slice(&(dir.len() as u32).to_le_bytes());
        out.extend_from_slice(&dir_at.to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out
    }

    fn kra(png: &[u8], deflate: bool) -> Vec<u8> {
        zip(
            &[
                ("mimetype", MIMETYPE),
                ("maindoc.xml", b"<DOC/>"),
                ("layers/layer1", b"VERSION 2\nTILEWIDTH 64\n"),
                ("mergedimage.png", png),
            ],
            deflate,
        )
    }

    #[test]
    fn a_kra_opens_as_its_merged_image() {
        let px: Vec<u8> = (0..6 * 4)
            .flat_map(|i| [i as u8 * 9, 3, 200, 255 - i as u8])
            .collect();
        let png = encode(ExportFormat::Png, 6, 4, &px).unwrap();
        for deflate in [false, true] {
            let file = kra(&png, deflate);
            assert!(looks_like_kra(&file));
            let info = probe_bytes(&file, ImportLimits::default()).unwrap();
            assert_eq!(
                (info.width, info.height, info.format),
                (6, 4, ImportFormat::Kra)
            );
            let s = decode_surface_bytes(&file, ImportLimits::default()).unwrap();
            assert_eq!(s.source_format, ImportFormat::Kra);
            assert_eq!(
                s.pixels,
                SurfacePixels::Rgba8(px.clone()),
                "deflate {deflate}"
            );
        }
    }

    #[test]
    fn a_kra_without_a_merged_image_or_not_krita_is_refused_by_name() {
        let file = zip(&[("mimetype", MIMETYPE), ("maindoc.xml", b"<DOC/>")], true);
        let err = decode_surface_bytes(&file, ImportLimits::default()).unwrap_err();
        assert!(err.to_string().contains("mergedimage.png"), "{err}");
        // An OpenRaster (or any other zip) named .kra is not Krita's.
        let other = zip(&[("mimetype", b"image/openraster")], false);
        let err = crate::codec::decode_surface_bytes_as(
            &other,
            ImportLimits::default(),
            ImportFormat::Kra,
        )
        .unwrap_err();
        assert!(err.to_string().contains("Krita"), "{err}");
    }

    #[test]
    fn damaged_archives_error_and_never_panic() {
        let png = encode(ExportFormat::Png, 5, 5, &[7u8; 100]).unwrap();
        let file = kra(&png, true);
        for cut in 0..file.len() {
            let _ = crate::codec::decode_surface_bytes_as(
                &file[..cut],
                ImportLimits::default(),
                ImportFormat::Kra,
            );
        }
        for i in 0..file.len() {
            let mut bad = file.clone();
            bad[i] ^= 0xFF;
            let _ = decode_surface_bytes(&bad, ImportLimits::default());
            let _ = probe_bytes(&bad, ImportLimits::default());
        }
        // A merged image past the allocation ceiling is refused while it
        // inflates, not after.
        let tight = ImportLimits {
            max_alloc_bytes: 16,
            ..ImportLimits::default()
        };
        assert!(matches!(
            decode_surface_bytes(&file, tight),
            Err(CodecError::LimitExceeded(_))
        ));
    }
}
