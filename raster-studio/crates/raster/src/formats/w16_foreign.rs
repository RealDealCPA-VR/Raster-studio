//! W16-L: other applications' native documents, opened as far as their
//! containers allow.
//!
//! * **CorelDRAW `.cdr`**: its embedded thumbnail. A CorelDRAW X4+ file is
//!   a ZIP holding `metadata/thumbnails/thumbnail.bmp` (and, in some
//!   versions, `previews/thumbnail.png`); an older one is a RIFF file
//!   (`RIFF....CDR?`) whose `DISP` chunk holds a Windows bitmap without its
//!   file header. The drawing itself (CorelDRAW's undocumented RIFF records)
//!   is not read.
//! * **InDesign `.indd`**: the JPEG thumbnail in the document's XMP packet
//!   (`xmpGImg:image`, base64). The layout itself is not read.
//! * **Affinity Photo `.afphoto`** and **PaintTool SAI `.sai`** are refused
//!   by name: Affinity's format is proprietary and undocumented, SAI's is an
//!   encrypted virtual file system, and neither exposes a preview a reader
//!   can find without decoding the whole format.

use super::super::malformed;
use super::{decode_embedded, u32le, zip};
use crate::codec::{CodecError, DecodedSurface, ImportFormat, ImportLimits};

const CDR: &str = "CorelDRAW";
const INDD: &str = "InDesign";

/// InDesign's database GUID, the first 16 bytes of every `.indd`.
const INDD_GUID: [u8; 16] = [
    0x06, 0x06, 0xED, 0xF5, 0xD8, 0x1D, 0x46, 0xE5, 0xBD, 0x31, 0xEF, 0xE7, 0xFE, 0x74, 0xB7, 0x1D,
];

/// `true` for a RIFF-based (pre-X4) CorelDRAW file.
pub fn looks_like_riff_cdr(head: &[u8]) -> bool {
    head.len() >= 12 && &head[..4] == b"RIFF" && head[8..11].eq_ignore_ascii_case(b"CDR")
}

/// `true` when `head` starts with InDesign's GUID.
pub fn looks_like_indd(head: &[u8]) -> bool {
    head.starts_with(&INDD_GUID)
}

/// The refusal an Affinity document gets.
pub fn affinity_refusal() -> CodecError {
    CodecError::Unsupported(
        "Affinity Photo / Designer documents are Serif's proprietary, undocumented format, \
         and no reader for them exists; export from Affinity as PSD (to keep the layers) or \
         PNG to open it here"
            .into(),
    )
}

/// The refusal a PaintTool SAI document gets.
pub fn sai_refusal() -> CodecError {
    CodecError::Unsupported(
        "PaintTool SAI documents are an encrypted, proprietary container with no published \
         format, and no reader for them exists; export from SAI as PSD (to keep the layers) \
         or PNG to open it here"
            .into(),
    )
}

/// A Windows bitmap body (a `BITMAPINFOHEADER` and what follows) with the
/// 14-byte `BM` file header it lacks put back in front.
fn bmp_with_file_header(dib: &[u8]) -> Result<Vec<u8>, CodecError> {
    let header = u32le(dib, 0, CDR)? as usize;
    if !matches!(header, 40 | 52 | 56 | 108 | 124) {
        return Err(malformed(CDR, "its DISP bitmap has an unknown header"));
    }
    let bits = u32::from(super::u16le(dib, 14, CDR)?);
    let compression = u32le(dib, 16, CDR)?;
    let used = u32le(dib, 32, CDR)? as usize;
    let colors = if used != 0 {
        used.min(256)
    } else if bits <= 8 {
        1 << bits
    } else {
        0
    };
    let masks = if header == 40 && compression == 3 {
        12
    } else {
        0
    };
    let offset = 14 + header + masks + colors * 4;
    let mut out = Vec::with_capacity(dib.len() + 14);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&((dib.len() + 14) as u32).to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&(offset as u32).to_le_bytes());
    out.extend_from_slice(dib);
    Ok(out)
}

/// The `DISP` chunk of a RIFF CorelDRAW file, walking `LIST` chunks.
fn riff_disp(b: &[u8]) -> Option<&[u8]> {
    fn walk(b: &[u8], depth: usize) -> Option<&[u8]> {
        let mut at = 0;
        while at + 8 <= b.len() && depth < 8 {
            let id = &b[at..at + 4];
            let len = u32::from_le_bytes([b[at + 4], b[at + 5], b[at + 6], b[at + 7]]) as usize;
            let data = b.get(at + 8..(at + 8).checked_add(len)?)?;
            if id == b"DISP" {
                return Some(data);
            }
            if id == b"LIST" && data.len() >= 4 {
                if let Some(found) = walk(&data[4..], depth + 1) {
                    return Some(found);
                }
            }
            at += 8 + len + (len & 1);
        }
        None
    }
    walk(b.get(12..)?, 0)
}

/// Decode a CorelDRAW file's thumbnail.
pub fn decode_cdr(bytes: &[u8], limits: ImportLimits) -> Result<DecodedSurface, CodecError> {
    let cap = limits.max_alloc_bytes;
    let image = if zip::looks_like_zip(bytes) {
        zip::first_of(
            bytes,
            &[
                "previews/thumbnail.png",
                "metadata/thumbnails/thumbnail.bmp",
                "metadata/thumbnails/thumbnail.png",
            ],
            cap,
            CDR,
        )?
        .map(|(_, data)| data)
    } else if looks_like_riff_cdr(bytes) {
        match riff_disp(bytes) {
            Some(disp) => {
                // The bitmap starts at the chunk, or after a 4-byte field.
                let body = [0usize, 4]
                    .into_iter()
                    .filter_map(|skip| disp.get(skip..))
                    .find(|d| {
                        d.len() >= 40
                            && matches!(d[0], 40 | 52 | 56 | 108 | 124)
                            && d[1..4] == [0, 0, 0]
                    })
                    .ok_or_else(|| malformed(CDR, "its DISP chunk holds no bitmap"))?;
                Some(bmp_with_file_header(body)?)
            }
            None => None,
        }
    } else {
        return Err(malformed(
            CDR,
            "it is neither a ZIP nor a RIFF CorelDRAW file",
        ));
    };
    let image = image.ok_or_else(|| {
        CodecError::Unsupported(
            "this CorelDRAW file has no embedded thumbnail, and CorelDRAW's drawing records \
             themselves are not read; export from CorelDRAW as SVG, PDF or PNG"
                .into(),
        )
    })?;
    decode_embedded(CDR, &image, limits, ImportFormat::Cdr)
}

fn base64(text: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut n) = (0u32, 0);
    let mut i = 0;
    while i < text.len() {
        let c = text[i];
        // XMP writes line breaks inside the value as `&#xA;`.
        if c == b'&' {
            if let Some(end) = text[i..].iter().position(|b| *b == b';') {
                i += end + 1;
                continue;
            }
        }
        i += 1;
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => continue,
        };
        acc = (acc << 6) | u32::from(v);
        n += 1;
        if n == 4 {
            out.extend_from_slice(&[(acc >> 16) as u8, (acc >> 8) as u8, acc as u8]);
            acc = 0;
            n = 0;
        }
    }
    match n {
        2 => out.push((acc >> 4) as u8),
        3 => out.extend_from_slice(&[(acc >> 10) as u8, (acc >> 2) as u8]),
        _ => {}
    }
    out
}

/// Every thumbnail in an InDesign file's XMP, decoded from base64.
fn indd_thumbnails(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    for (open, close) in [
        (
            b"<xmpGImg:image>".as_slice(),
            b"</xmpGImg:image>".as_slice(),
        ),
        (b"xmpGImg:image=\"".as_slice(), b"\"".as_slice()),
    ] {
        let mut from = 0;
        while let Some(at) = memchr::memmem::find(&bytes[from..], open) {
            let start = from + at + open.len();
            let Some(len) = memchr::memmem::find(&bytes[start..], close) else {
                break;
            };
            out.push(base64(&bytes[start..start + len]));
            from = start + len;
        }
    }
    out
}

/// Decode an InDesign file's XMP thumbnail (the largest, when several).
pub fn decode_indd(bytes: &[u8], limits: ImportLimits) -> Result<DecodedSurface, CodecError> {
    if !looks_like_indd(bytes) {
        return Err(malformed(INDD, "it does not start with InDesign's GUID"));
    }
    let best = indd_thumbnails(bytes)
        .into_iter()
        .filter(|t| t.starts_with(&[0xFF, 0xD8]) || t.starts_with(b"\x89PNG"))
        .max_by_key(Vec::len)
        .ok_or_else(|| {
            CodecError::Unsupported(
                "this InDesign file has no XMP thumbnail, and InDesign's layout itself is not \
                 read; export from InDesign as PDF to open its pages"
                    .into(),
            )
        })?;
    decode_embedded(INDD, &best, limits, ImportFormat::Indd)
}

#[cfg(test)]
mod tests {
    use super::super::test_util::{fuzz, ramp, zip};
    use super::*;
    use crate::codec::{
        decode_surface_bytes, decode_surface_bytes_as, encode, ExportFormat, SurfacePixels,
    };

    fn b64(data: &[u8]) -> String {
        const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut s = String::new();
        for chunk in data.chunks(3) {
            let v = (u32::from(chunk[0]) << 16)
                | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
                | u32::from(*chunk.get(2).unwrap_or(&0));
            for k in 0..4 {
                if k <= chunk.len() {
                    s.push(T[((v >> (18 - 6 * k)) & 63) as usize] as char);
                } else {
                    s.push('=');
                }
            }
            if s.len().is_multiple_of(76) {
                s.push_str("&#xA;");
            }
        }
        s
    }

    #[test]
    fn base64_round_trips() {
        for n in 0..10 {
            let data: Vec<u8> = (0..n).map(|i: u32| (i * 37) as u8).collect();
            assert_eq!(base64(b64(&data).as_bytes()), data, "{n}");
        }
    }

    #[test]
    fn a_zipped_cdr_opens_its_thumbnail_and_a_riff_cdr_its_disp_bitmap() {
        let px = ramp(4, 3);
        let bmp = encode(ExportFormat::Bmp, 4, 3, &px).unwrap();
        let file = zip(
            &[
                ("content/riffData.cdr", b"RIFF\0\0\0\0CDRX".as_slice()),
                ("metadata/thumbnails/thumbnail.bmp", &bmp),
            ],
            true,
        );
        let s = decode_surface_bytes_as(&file, ImportLimits::default(), ImportFormat::Cdr).unwrap();
        assert_eq!(
            (s.width, s.height, s.source_format),
            (4, 3, ImportFormat::Cdr)
        );
        let SurfacePixels::Rgba8(got) = &s.pixels else {
            panic!()
        };
        assert_eq!(
            got.chunks(4).map(|p| &p[..3]).collect::<Vec<_>>(),
            px.chunks(4).map(|p| &p[..3]).collect::<Vec<_>>()
        );
        // RIFF: the same bitmap minus its 14-byte file header, in a DISP
        // chunk inside a LIST.
        let dib = &bmp[14..];
        let mut disp = b"DISP".to_vec();
        disp.extend_from_slice(&(dib.len() as u32).to_le_bytes());
        disp.extend_from_slice(dib);
        if dib.len() % 2 == 1 {
            disp.push(0);
        }
        let mut list = b"LIST".to_vec();
        list.extend_from_slice(&((disp.len() + 4) as u32).to_le_bytes());
        list.extend_from_slice(b"doc ");
        list.extend_from_slice(&disp);
        let mut riff = b"RIFF".to_vec();
        riff.extend_from_slice(&((list.len() + 4) as u32).to_le_bytes());
        riff.extend_from_slice(b"CDR9");
        riff.extend_from_slice(&list);
        assert!(looks_like_riff_cdr(&riff));
        let s = decode_surface_bytes(&riff, ImportLimits::default()).unwrap();
        assert_eq!(
            (s.width, s.height, s.source_format),
            (4, 3, ImportFormat::Cdr)
        );
        fuzz(&riff, ImportFormat::Cdr);
        fuzz(&file, ImportFormat::Cdr);
    }

    #[test]
    fn an_indd_opens_its_xmp_thumbnail() {
        let px = ramp(8, 8);
        let jpeg = encode(ExportFormat::Jpeg(90), 8, 8, &px).unwrap();
        let mut file = INDD_GUID.to_vec();
        file.extend_from_slice(&[0; 200]);
        file.extend_from_slice(b"<x:xmpmeta><xmpGImg:format>JPEG</xmpGImg:format><xmpGImg:image>");
        file.extend_from_slice(b64(&jpeg).as_bytes());
        file.extend_from_slice(b"</xmpGImg:image></x:xmpmeta>");
        file.extend_from_slice(&[0; 100]);
        assert!(looks_like_indd(&file));
        let s = decode_surface_bytes(&file, ImportLimits::default()).unwrap();
        assert_eq!(
            (s.width, s.height, s.source_format),
            (8, 8, ImportFormat::Indd)
        );
        fuzz(&file, ImportFormat::Indd);
        let mut bare = INDD_GUID.to_vec();
        bare.extend_from_slice(&[0; 64]);
        let err = decode_surface_bytes(&bare, ImportLimits::default()).unwrap_err();
        assert!(err.to_string().contains("no XMP thumbnail"), "{err}");
    }

    #[test]
    fn affinity_and_sai_are_refused_by_name() {
        for (ext, name) in [("afphoto", "Affinity"), ("sai", "SAI")] {
            let format = ImportFormat::from_extension(ext).unwrap();
            let err =
                decode_surface_bytes_as(b"\0\xFFKAfake file", ImportLimits::default(), format)
                    .unwrap_err();
            assert!(err.to_string().contains(name), "{err}");
            assert!(err.to_string().contains("PSD"), "{err}");
            assert!(!format.is_decodable_here());
        }
    }
}
