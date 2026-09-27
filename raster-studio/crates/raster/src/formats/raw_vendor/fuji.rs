//! W18-D: Fujifilm RAF.
//!
//! A RAF starts `FUJIFILMCCD-RAW `; at `0x54` a directory of big-endian
//! offset / length pairs gives the embedded JPEG, the metadata container
//! (`0x5C` / `0x60`) and the CFA section (`0x64` / `0x68`). The container
//! is a big-endian record count, then records of tag, size and data:
//! `RawImageCropTopLeft` (`0x0110`, top then left), `RawImageCroppedSize`
//! (`0x0111`, height then width), `XTransLayout` (`0x0131`, 36 colour
//! codes, 0 red / 1 green / 2 blue, stored last site first: reversed, they
//! are the 6x6 layout row-major from the raw image's origin, as the
//! per-site means of an X-T2 and an X-E2 file show) and `WB_GRGBLevels`
//! (`0x2FF0`).
//!
//! The CFA section is a TIFF whose IFD0 points (`0xF000`) at the `FujiIFD`:
//! `RawImageFullWidth` / `Height` (`0xF001` / `0xF002`), `BitsPerSample`
//! (`0xF003`), `StripOffsets` / `StripByteCounts` (`0xF007` / `0xF008`,
//! relative to that TIFF's header), `BlackLevel` (`0xF00A`) and
//! `WB_GRBLevels` (`0xF00E`). Data holding two bytes per pixel is read as
//! 16-bit words in that TIFF's byte order; anything smaller is Fujifilm's
//! compressed RAF (lossless or lossy), refused by name, as is a RAF with no
//! `FujiIFD` (the oldest bodies' layout). A pattern with no `XTransLayout`
//! is taken as red-green / green-blue.

use crate::codec::{CodecError, ImportLimits};

use super::{
    balance, broken, budget, cells, not_decoded, pattern, rggb, tiff_any, unpack_plain, Mosaic,
    Want,
};

const NAME: &str = "Fujifilm RAF";

fn be32(d: &[u8], at: usize) -> Option<usize> {
    let b = d.get(at..at + 4)?;
    Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
}

fn be16(d: &[u8], at: usize) -> Option<usize> {
    let b = d.get(at..at + 2)?;
    Some(usize::from(u16::from_be_bytes([b[0], b[1]])))
}

/// The metadata container's facts.
#[derive(Default)]
struct Records {
    crop_origin: Option<(usize, usize)>,
    crop_size: Option<(usize, usize)>,
    xtrans: Option<Vec<u32>>,
    grgb: Option<[f64; 4]>,
}

fn records(d: &[u8]) -> Records {
    let mut r = Records::default();
    let Some(count) = be32(d, 0) else { return r };
    let mut pos = 4usize;
    for _ in 0..count.min(512) {
        let (Some(tag), Some(size)) = (be16(d, pos), be16(d, pos + 2)) else {
            break;
        };
        let Some(v) = d.get(pos + 4..pos + 4 + size) else {
            break;
        };
        let pair = || Some((be16(v, 0)?, be16(v, 2)?));
        match tag {
            0x0110 => r.crop_origin = pair(),
            0x0111 => r.crop_size = pair(),
            // Stored last site first: reversed, the sensor's row-major layout.
            0x0131 if size == 36 => {
                r.xtrans = Some(v.iter().rev().map(|&c| u32::from(c)).collect())
            }
            0x2FF0 if size >= 8 => {
                r.grgb = (0..4)
                    .map(|i| be16(v, i * 2).map(|x| x as f64))
                    .collect::<Option<Vec<_>>>()
                    .map(|l| [l[0], l[1], l[2], l[3]]);
            }
            _ => {}
        }
        pos += 4 + size;
    }
    r
}

pub(super) fn read(bytes: &[u8], limits: ImportLimits, want: Want) -> Result<Mosaic, CodecError> {
    let (meta_at, meta_len, cfa_at, cfa_len) = (
        be32(bytes, 0x5C),
        be32(bytes, 0x60),
        be32(bytes, 0x64),
        be32(bytes, 0x68),
    );
    let (Some(meta_at), Some(meta_len), Some(cfa_at), Some(cfa_len)) =
        (meta_at, meta_len, cfa_at, cfa_len)
    else {
        return Err(broken(NAME, "the header is truncated"));
    };
    let meta = bytes
        .get(meta_at..meta_at.saturating_add(meta_len))
        .ok_or_else(|| broken(NAME, "the metadata container lies past the end of the file"))?;
    let rec = records(meta);
    let cfa = bytes
        .get(cfa_at..cfa_at.saturating_add(cfa_len))
        .filter(|c| !c.is_empty())
        .ok_or_else(|| broken(NAME, "the CFA section lies past the end of the file"))?;
    let Some(tiff) = tiff_any(cfa) else {
        return Err(not_decoded(
            NAME,
            "CFA section without a FujiIFD (the layout of the oldest bodies)",
        ));
    };
    let fuji = tiff
        .first_ifd()
        .and_then(|o| tiff.ifd(o))
        .and_then(|i| i.uint(&tiff, 0xF000))
        .and_then(|o| tiff.ifd(o as usize))
        .ok_or_else(|| {
            not_decoded(
                NAME,
                "CFA section without a FujiIFD (the layout of the oldest bodies)",
            )
        })?;
    let width = fuji.uint(&tiff, 0xF001).unwrap_or(0) as usize;
    let height = fuji.uint(&tiff, 0xF002).unwrap_or(0) as usize;
    let bits = fuji.uint(&tiff, 0xF003).unwrap_or(16);
    budget(NAME, limits, width, height)?;
    let off = fuji.uint(&tiff, 0xF007).unwrap_or(0) as usize;
    let len = fuji.uint(&tiff, 0xF008).unwrap_or(0) as usize;
    let data = cfa
        .get(off..off.saturating_add(len))
        .ok_or_else(|| broken(NAME, "the raw strip lies past the end of the CFA section"))?;
    if data.len() < width * height * 2 {
        return Err(not_decoded(NAME, "compressed raw data (lossless or lossy)"));
    }
    let samples = if want == Want::Pixels {
        unpack_plain(NAME, data, width, height, 16, tiff.le)?
    } else {
        Vec::new()
    };
    let active = match (rec.crop_origin, rec.crop_size) {
        (Some((top, left)), Some((h, w)))
            if h > 0 && w > 0 && top + h <= height && left + w <= width =>
        {
            (top, left, top + h, left + w)
        }
        _ => (0, 0, height, width),
    };
    let pat = match &rec.xtrans {
        Some(codes) => {
            pattern(6, 6, codes).ok_or_else(|| broken(NAME, "the XTransLayout is invalid"))?
        }
        None => rggb(),
    };
    let black = fuji
        .floats_of(&tiff, 0xF00A)
        .and_then(|b| cells(&b))
        .map(|b| {
            // A 6x6 (or longer) list: one level for all.
            if pat.rows == 2 {
                b
            } else {
                [b.iter().sum::<f64>() / 4.0; 4]
            }
        })
        .unwrap_or([0.0; 4]);
    let wb = fuji
        .floats_of(&tiff, 0xF00E)
        .filter(|l| l.len() >= 3)
        .and_then(|l| balance(l[1], l[0], l[2]))
        .or_else(|| {
            rec.grgb
                .and_then(|l| balance(l[1], (l[0] + l[2]) / 2.0, l[3]))
        });
    Ok(Mosaic {
        name: NAME,
        width,
        height,
        samples,
        pattern: pat,
        active,
        black,
        white: None,
        full_scale: f64::from((1u32 << bits.clamp(1, 16)) - 1),
        wb,
        orientation: 1,
    })
}
