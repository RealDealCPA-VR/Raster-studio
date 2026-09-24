//! W11-H: Amiga / Deluxe Paint IFF images (`.iff`, `.ilbm`, `.lbm`), read
//! only, by this crate's own parser.
//!
//! What is read: an `FORM ILBM` (interleaved bitplanes) or `FORM PBM `
//! (Deluxe Paint's chunky variant), uncompressed or ByteRun1-compressed, with
//!
//! * 1 to 8 bitplanes through the `CMAP` palette, including Extra-Half-Brite
//!   (`CAMG` bit `0x80`: colours 32..63 are 0..31 at half brightness) and
//!   Hold-And-Modify (`CAMG` bit `0x800`, HAM6 and HAM8);
//! * 24 bitplanes (true colour) and 32 (true colour with alpha);
//! * masking 1 (a mask plane follows the colour planes of each row) and
//!   masking 2 (the `BMHD` transparent colour index is transparent).
//!
//! Planar rows are turned chunky here: plane `p`'s bit is bit `p` of the
//! pixel's value, most significant pixel first. A palette an old tool wrote
//! with 4-bit values in the high nibble is widened (`0xA0` becomes `0xAA`).
//! Only the first `FORM` of a `CAT`/`LIST` is not looked for: such a file is
//! refused by name, as are an `ACBM`, a `DEEP` or `RGBN` and compression
//! other than 0 and 1.
//!
//! # Untrusted input
//!
//! Chunk lengths are checked against the file before they are sliced, the
//! declared size goes through [`ImportLimits`] before the output exists, and
//! ByteRun1 refuses a run that would overflow the row it is decoding.

use super::{check_decode, info, malformed, rgba8_surface};
use crate::codec::{CodecError, DecodedSurface, ImageInfo, ImportFormat, ImportLimits};

const NAME: &str = "IFF";

/// `true` for a `FORM` of type `ILBM` or `PBM `.
pub fn looks_like_iff(head: &[u8]) -> bool {
    head.len() >= 12 && &head[..4] == b"FORM" && matches!(&head[8..12], b"ILBM" | b"PBM ")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Bmhd {
    width: u16,
    height: u16,
    planes: u8,
    masking: u8,
    compression: u8,
    transparent: u16,
}

#[derive(Debug, Default)]
struct Chunks<'a> {
    bmhd: Option<Bmhd>,
    cmap: &'a [u8],
    camg: u32,
    body: Option<&'a [u8]>,
    chunky: bool,
}

fn be16(b: &[u8]) -> u16 {
    u16::from_be_bytes([b[0], b[1]])
}

fn parse(bytes: &[u8]) -> Result<Chunks<'_>, CodecError> {
    if !looks_like_iff(bytes) {
        if bytes.len() >= 12 && &bytes[..4] == b"FORM" {
            return Err(CodecError::Unsupported(format!(
                "IFF FORM type {:?} is not an image this build reads (ILBM and PBM are)",
                String::from_utf8_lossy(&bytes[8..12])
            )));
        }
        return Err(malformed(NAME, "no FORM ILBM / PBM header"));
    }
    let declared = u32::from_be_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
    let end = declared.saturating_add(8).min(bytes.len());
    let mut chunks = Chunks {
        chunky: &bytes[8..12] == b"PBM ",
        ..Chunks::default()
    };
    let mut at = 12usize;
    while at + 8 <= end {
        let id = &bytes[at..at + 4];
        let len = u32::from_be_bytes(bytes[at + 4..at + 8].try_into().expect("four")) as usize;
        let data_at = at + 8;
        if len > end - data_at {
            return Err(malformed(
                NAME,
                format!("chunk {:?} runs past the file", String::from_utf8_lossy(id)),
            ));
        }
        let data = &bytes[data_at..data_at + len];
        match id {
            b"BMHD" => {
                if len < 20 {
                    return Err(malformed(NAME, "BMHD is shorter than 20 bytes"));
                }
                chunks.bmhd = Some(Bmhd {
                    width: be16(&data[0..2]),
                    height: be16(&data[2..4]),
                    planes: data[8],
                    masking: data[9],
                    compression: data[10],
                    transparent: be16(&data[12..14]),
                });
            }
            b"CMAP" => chunks.cmap = data,
            b"CAMG" if len >= 4 => {
                chunks.camg = u32::from_be_bytes(data[..4].try_into().expect("four"));
            }
            b"BODY" => {
                chunks.body = Some(data);
                break;
            }
            _ => {}
        }
        // Chunks are padded to an even length.
        at = data_at + len + (len & 1);
    }
    Ok(chunks)
}

fn header(c: &Chunks<'_>) -> Result<Bmhd, CodecError> {
    let h = c.bmhd.ok_or_else(|| malformed(NAME, "no BMHD chunk"))?;
    if c.chunky {
        if h.planes != 8 {
            return Err(malformed(NAME, "a PBM image must have 8 planes"));
        }
    } else if !matches!(h.planes, 1..=8 | 24 | 32) {
        return Err(CodecError::Unsupported(format!(
            "IFF images with {} bitplanes are not supported (1-8, 24 and 32 are)",
            h.planes
        )));
    }
    if h.compression > 1 {
        return Err(CodecError::Unsupported(format!(
            "IFF compression {} is not supported (0 and ByteRun1 are)",
            h.compression
        )));
    }
    Ok(h)
}

/// ByteRun1 (PackBits) into exactly `want` bytes; returns the input used.
fn byterun1(src: &[u8], want: usize, out: &mut Vec<u8>) -> Result<usize, CodecError> {
    let start = out.len();
    let mut at = 0usize;
    while out.len() - start < want {
        let Some(&n) = src.get(at) else {
            return Err(malformed(NAME, "compressed body ends early"));
        };
        at += 1;
        let left = want - (out.len() - start);
        let n = n as i8;
        if n >= 0 {
            let count = n as usize + 1;
            let run = src
                .get(at..at + count)
                .ok_or_else(|| malformed(NAME, "a literal run ends early"))?;
            if count > left {
                return Err(malformed(NAME, "a literal run overflows its row"));
            }
            out.extend_from_slice(run);
            at += count;
        } else if n != -128 {
            let count = (-(n as i16)) as usize + 1;
            let v = *src
                .get(at)
                .ok_or_else(|| malformed(NAME, "a repeat run ends early"))?;
            if count > left {
                return Err(malformed(NAME, "a repeat run overflows its row"));
            }
            at += 1;
            out.resize(out.len() + count, v);
        }
    }
    Ok(at)
}

/// The palette as RGB triples, widened from 4-bit nibbles if every entry
/// has a zero low nibble, with Extra-Half-Brite applied.
fn palette(c: &Chunks<'_>, h: &Bmhd) -> Vec<[u8; 3]> {
    let mut pal: Vec<[u8; 3]> = c.cmap.as_chunks::<3>().0.to_vec();
    if !pal.is_empty() && pal.iter().flatten().all(|v| v & 0x0F == 0) {
        for e in pal.iter_mut().flatten() {
            *e |= *e >> 4;
        }
    }
    let ehb = c.camg & 0x80 != 0 && h.planes == 6;
    if ehb {
        pal.resize(32, [0, 0, 0]);
        let half: Vec<[u8; 3]> = pal[..32].iter().map(|p| p.map(|v| v >> 1)).collect();
        pal.truncate(32);
        pal.extend(half);
    }
    pal
}

fn entry(pal: &[[u8; 3]], index: usize) -> [u8; 3] {
    pal.get(index).copied().unwrap_or([0, 0, 0])
}

/// Header facts.
pub fn probe(bytes: &[u8], limits: ImportLimits) -> Result<ImageInfo, CodecError> {
    let c = parse(bytes)?;
    let h = header(&c)?;
    let (w, ht) = (u32::from(h.width), u32::from(h.height));
    limits.check_dimensions(w, ht)?;
    Ok(info(w, ht, ImportFormat::Iff, false))
}

/// Decode to RGBA8.
pub fn decode(bytes: &[u8], limits: ImportLimits) -> Result<DecodedSurface, CodecError> {
    let c = parse(bytes)?;
    let h = header(&c)?;
    let body = c.body.ok_or_else(|| malformed(NAME, "no BODY chunk"))?;
    let (w, ht) = (usize::from(h.width), usize::from(h.height));
    let row_len = if c.chunky {
        (w + 1) & !1
    } else {
        let plane_row = w.div_ceil(16) * 2;
        plane_row * (usize::from(h.planes) + usize::from(h.masking == 1))
    };
    check_decode(
        limits,
        u32::from(h.width),
        u32::from(h.height),
        4,
        row_len as u64 * ht as u64,
    )?;

    // Every row, decompressed.
    let mut raw = Vec::with_capacity(row_len * ht);
    if h.compression == 1 {
        let mut at = 0usize;
        for _ in 0..ht {
            at += byterun1(&body[at.min(body.len())..], row_len, &mut raw)?;
        }
    } else {
        let need = row_len * ht;
        let data = body
            .get(..need)
            .ok_or_else(|| malformed(NAME, "the body is shorter than the image"))?;
        raw.extend_from_slice(data);
    }

    let pal = palette(&c, &h);
    let ham = !c.chunky && c.camg & 0x800 != 0 && matches!(h.planes, 6 | 8);
    let mut rgba = vec![0u8; w * ht * 4];
    let mut values = vec![0u32; w];
    for (y, row) in raw.chunks_exact(row_len).enumerate() {
        let mut opaque = vec![true; w];
        if c.chunky {
            for (x, v) in values.iter_mut().enumerate() {
                *v = u32::from(row[x]);
            }
        } else {
            let plane_row = w.div_ceil(16) * 2;
            values.iter_mut().for_each(|v| *v = 0);
            for p in 0..usize::from(h.planes) {
                let plane = &row[p * plane_row..(p + 1) * plane_row];
                for (x, v) in values.iter_mut().enumerate() {
                    if plane[x / 8] & (0x80 >> (x % 8)) != 0 {
                        *v |= 1 << p;
                    }
                }
            }
            if h.masking == 1 {
                let p = usize::from(h.planes);
                let plane = &row[p * plane_row..(p + 1) * plane_row];
                for (x, o) in opaque.iter_mut().enumerate() {
                    *o = plane[x / 8] & (0x80 >> (x % 8)) != 0;
                }
            }
        }
        let mut held = entry(&pal, 0);
        for x in 0..w {
            let v = values[x];
            let out = &mut rgba[(y * w + x) * 4..(y * w + x) * 4 + 4];
            let (rgb, alpha) = match h.planes {
                24 => ([v as u8, (v >> 8) as u8, (v >> 16) as u8], 255),
                32 if !c.chunky => ([v as u8, (v >> 8) as u8, (v >> 16) as u8], (v >> 24) as u8),
                planes if ham => {
                    let data_bits = u32::from(planes) - 2;
                    let data = v & ((1 << data_bits) - 1);
                    let wide = if data_bits == 4 {
                        (data * 17) as u8
                    } else {
                        ((data << 2) | (data >> 4)) as u8
                    };
                    match v >> data_bits {
                        0 => held = entry(&pal, data as usize),
                        1 => held[2] = wide,
                        2 => held[0] = wide,
                        _ => held[1] = wide,
                    }
                    (held, 255)
                }
                _ => {
                    let a = if h.masking == 2 && v == u32::from(h.transparent) {
                        0
                    } else {
                        255
                    };
                    (entry(&pal, v as usize), a)
                }
            };
            out[..3].copy_from_slice(&rgb);
            out[3] = if opaque[x] { alpha } else { 0 };
        }
    }
    Ok(rgba8_surface(
        u32::from(h.width),
        u32::from(h.height),
        rgba,
        ImportFormat::Iff,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_surface_bytes, probe_bytes, SurfacePixels};

    fn chunk(id: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(data);
        if data.len() % 2 == 1 {
            out.push(0);
        }
        out
    }

    fn bmhd(w: u16, h: u16, planes: u8, masking: u8, compression: u8, transparent: u16) -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(&w.to_be_bytes());
        d.extend_from_slice(&h.to_be_bytes());
        d.extend_from_slice(&[0; 4]);
        d.extend_from_slice(&[planes, masking, compression, 0]);
        d.extend_from_slice(&transparent.to_be_bytes());
        d.extend_from_slice(&[1, 1]);
        d.extend_from_slice(&w.to_be_bytes());
        d.extend_from_slice(&h.to_be_bytes());
        d
    }

    fn form(kind: &[u8; 4], chunks: &[Vec<u8>]) -> Vec<u8> {
        let body: Vec<u8> = chunks.concat();
        let mut out = b"FORM".to_vec();
        out.extend_from_slice(&((body.len() + 4) as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(&body);
        out
    }

    /// Interleave chunky `values` into planar rows (plus an optional mask
    /// plane), the way an ILBM stores them.
    fn planar(values: &[u32], w: usize, planes: usize, mask: Option<&[bool]>) -> Vec<u8> {
        let plane_row = w.div_ceil(16) * 2;
        let mut out = Vec::new();
        for row in 0..values.len() / w {
            for p in 0..planes {
                let mut bits = vec![0u8; plane_row];
                for x in 0..w {
                    if values[row * w + x] & (1 << p) != 0 {
                        bits[x / 8] |= 0x80 >> (x % 8);
                    }
                }
                out.extend(bits);
            }
            if let Some(mask) = mask {
                let mut bits = vec![0u8; plane_row];
                for x in 0..w {
                    if mask[row * w + x] {
                        bits[x / 8] |= 0x80 >> (x % 8);
                    }
                }
                out.extend(bits);
            }
        }
        out
    }

    /// ByteRun1 per row: literal runs only, the worst legal coding, plus
    /// one repeat run where a row starts with two equal bytes.
    fn byterun1_encode(raw: &[u8], row_len: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for row in raw.chunks(row_len) {
            let mut i = 0;
            while i < row.len() {
                let mut run = 1;
                while i + run < row.len() && row[i + run] == row[i] && run < 128 {
                    run += 1;
                }
                if run >= 2 {
                    out.push((-(run as i16 - 1)) as i8 as u8);
                    out.push(row[i]);
                    i += run;
                } else {
                    out.push(0);
                    out.push(row[i]);
                    i += 1;
                }
            }
        }
        out
    }

    const PAL: [[u8; 3]; 8] = [
        [0, 0, 0],
        [255, 0, 0],
        [0, 255, 0],
        [0, 0, 255],
        [10, 20, 30],
        [40, 50, 60],
        [70, 80, 90],
        [255, 255, 255],
    ];

    #[test]
    fn a_palette_ilbm_decodes_compressed_and_raw_with_its_mask() {
        let (w, h) = (19usize, 3usize);
        let values: Vec<u32> = (0..w * h).map(|i| (i % 8) as u32).collect();
        let mask: Vec<bool> = (0..w * h).map(|i| i % 5 != 0).collect();
        let raw = planar(&values, w, 3, Some(&mask));
        let row_len = w.div_ceil(16) * 2 * 4;
        let want: Vec<u8> = (0..w * h)
            .flat_map(|i| {
                let [r, g, b] = PAL[i % 8];
                [r, g, b, if mask[i] { 255 } else { 0 }]
            })
            .collect();
        for (compression, body) in [(0u8, raw.clone()), (1, byterun1_encode(&raw, row_len))] {
            let file = form(
                b"ILBM",
                &[
                    chunk(b"BMHD", &bmhd(w as u16, h as u16, 3, 1, compression, 0)),
                    chunk(b"CMAP", &PAL.concat()),
                    chunk(b"BODY", &body),
                ],
            );
            let info = probe_bytes(&file, ImportLimits::default()).unwrap();
            assert_eq!((info.width, info.format), (19, ImportFormat::Iff));
            let s = decode_surface_bytes(&file, ImportLimits::default()).unwrap();
            assert_eq!(s.source_format, ImportFormat::Iff);
            assert_eq!(
                s.pixels,
                SurfacePixels::Rgba8(want.clone()),
                "compression {compression}"
            );
        }
    }

    #[test]
    fn true_colour_ham6_and_transparent_index_decode() {
        // 24 planes: R in planes 0-7, G 8-15, B 16-23.
        let (w, h) = (5usize, 2usize);
        let rgb: Vec<[u8; 3]> = (0..w * h)
            .map(|i| [i as u8 * 20, 100, 255 - i as u8])
            .collect();
        let values: Vec<u32> = rgb
            .iter()
            .map(|p| u32::from(p[0]) | u32::from(p[1]) << 8 | u32::from(p[2]) << 16)
            .collect();
        let file = form(
            b"ILBM",
            &[
                chunk(b"BMHD", &bmhd(w as u16, h as u16, 24, 0, 0, 0)),
                chunk(b"BODY", &planar(&values, w, 24, None)),
            ],
        );
        let s = decode_surface_bytes(&file, ImportLimits::default()).unwrap();
        let want: Vec<u8> = rgb.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect();
        assert_eq!(s.pixels, SurfacePixels::Rgba8(want));

        // HAM6: palette 1 (red), then modify green to 0xF, then blue to 0x8.
        let values = vec![0b00_0001, 0b11_1111, 0b01_1000];
        let file = form(
            b"ILBM",
            &[
                chunk(b"BMHD", &bmhd(3, 1, 6, 0, 0, 0)),
                chunk(b"CMAP", &PAL.concat()),
                chunk(b"CAMG", &0x800u32.to_be_bytes()),
                chunk(b"BODY", &planar(&values, 3, 6, None)),
            ],
        );
        let s = decode_surface_bytes(&file, ImportLimits::default()).unwrap();
        assert_eq!(
            s.pixels,
            SurfacePixels::Rgba8(vec![255, 0, 0, 255, 255, 255, 0, 255, 255, 255, 136, 255])
        );

        // PBM (chunky) with masking 2: index 3 is transparent.
        let file = form(
            b"PBM ",
            &[
                chunk(b"BMHD", &bmhd(3, 1, 8, 2, 0, 3)),
                chunk(b"CMAP", &PAL.concat()),
                chunk(b"BODY", &[1, 3, 7, 0]),
            ],
        );
        let s = decode_surface_bytes(&file, ImportLimits::default()).unwrap();
        assert_eq!(
            s.pixels,
            SurfacePixels::Rgba8(vec![255, 0, 0, 255, 0, 0, 255, 0, 255, 255, 255, 255])
        );
    }

    #[test]
    fn damaged_iff_files_error_and_never_panic() {
        let (w, h) = (17usize, 4usize);
        let values: Vec<u32> = (0..w * h).map(|i| (i % 8) as u32).collect();
        let raw = planar(&values, w, 3, None);
        let file = form(
            b"ILBM",
            &[
                chunk(b"BMHD", &bmhd(w as u16, h as u16, 3, 0, 1, 0)),
                chunk(b"CMAP", &PAL.concat()),
                chunk(b"BODY", &byterun1_encode(&raw, w.div_ceil(16) * 2 * 3)),
            ],
        );
        decode_surface_bytes(&file, ImportLimits::default()).unwrap();
        for cut in 0..file.len() {
            let _ = decode_surface_bytes(&file[..cut], ImportLimits::default());
        }
        for i in 12..file.len() {
            for flip in [0x01, 0x80, 0xFF] {
                let mut bad = file.clone();
                bad[i] ^= flip;
                let _ = decode_surface_bytes(&bad, ImportLimits::default());
                let _ = probe_bytes(&bad, ImportLimits::default());
            }
        }
        // A 65535 x 65535 header over a tiny body fails on the limits or the
        // body, never by reserving the canvas.
        let huge = form(
            b"ILBM",
            &[
                chunk(b"BMHD", &bmhd(65_535, 65_535, 8, 0, 1, 0)),
                chunk(b"BODY", &[0x81, 0]),
            ],
        );
        assert!(decode_surface_bytes(&huge, ImportLimits::default()).is_err());
        // Other FORM types are named.
        let err = crate::codec::decode_surface_bytes_as(
            &form(b"8SVX", &[]),
            ImportLimits::default(),
            ImportFormat::Iff,
        )
        .unwrap_err();
        assert!(err.to_string().contains("8SVX"), "{err}");
    }
}
