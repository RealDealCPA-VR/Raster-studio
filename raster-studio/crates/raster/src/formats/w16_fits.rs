//! W16-L: FITS (Flexible Image Transport System), the astronomy format.
//!
//! A FITS file is a run of 2880-byte blocks. Each header is 80-character
//! `KEYWORD = value` cards ending at `END`; the data follows, padded to a
//! block. What opens is the primary HDU when it holds an image, else the
//! first `XTENSION= 'IMAGE'` extension:
//!
//! * `BITPIX` 8 (unsigned), 16, 32, 64 (signed, big endian) or -32 / -64
//!   (IEEE float), scaled to physical values by `BSCALE` and `BZERO`
//!   (which is how unsigned 16-bit data is stored); `BLANK` integers and
//!   non-finite floats open transparent;
//! * `NAXIS` 2, or 3 and more: the first plane, except `NAXIS3 = 3`, which
//!   opens as RGB;
//! * **auto-stretched**: the finite values' minimum and maximum map
//!   linearly onto 0..65535, so the image opens as a 16-bit document with
//!   the whole range visible (a constant image opens mid-grey);
//! * FITS stores the bottom row first (its origin is the lower-left
//!   pixel), so the rows are flipped to open the right way up.

use super::super::{check_decode, info, malformed, rgba8_surface};
use crate::codec::{
    CodecError, DecodedSurface, ImageInfo, ImportFormat, ImportLimits, SurfacePixels,
};

const NAME: &str = "FITS";
const BLOCK: usize = 2880;
const CARD: usize = 80;
/// Headers longer than this many blocks are refused (real ones are 1-10).
const MAX_HEADER_BLOCKS: usize = 1000;
/// HDUs walked looking for an image.
const MAX_HDUS: usize = 64;

/// `true` when `head` starts with the `SIMPLE  =` card.
pub fn looks_like_fits(head: &[u8]) -> bool {
    head.starts_with(b"SIMPLE  =")
}

#[derive(Debug, Clone, Default)]
struct Hdu {
    bitpix: i64,
    axes: Vec<u64>,
    bscale: f64,
    bzero: f64,
    blank: Option<i64>,
    pcount: u64,
    gcount: u64,
    is_image: bool,
    data_at: usize,
}

impl Hdu {
    fn data_bytes(&self) -> u64 {
        if self.axes.is_empty() {
            return 0;
        }
        let n = self.axes.iter().fold(1u64, |a, b| a.saturating_mul(*b));
        (self.bitpix.unsigned_abs() / 8)
            .saturating_mul(self.gcount.max(1))
            .saturating_mul(n.saturating_add(self.pcount))
    }
}

fn value(card: &str) -> &str {
    let v = card.get(10..).unwrap_or("").trim_start();
    // A quoted string ends at its closing quote (a slash inside it is not a
    // comment); anything else ends at the comment slash.
    if let Some(rest) = v.strip_prefix('\'') {
        return rest.split('\'').next().unwrap_or("").trim();
    }
    v.split('/').next().unwrap_or("").trim()
}

fn header_at(b: &[u8], start: usize, primary: bool) -> Result<Hdu, CodecError> {
    let mut hdu = Hdu {
        bscale: 1.0,
        gcount: 1,
        is_image: primary,
        ..Hdu::default()
    };
    let mut naxis = None;
    let mut at = start;
    for _ in 0..MAX_HEADER_BLOCKS * (BLOCK / CARD) {
        let card = b
            .get(at..at + CARD)
            .ok_or_else(|| malformed(NAME, "the header has no END card"))?;
        at += CARD;
        let card = String::from_utf8_lossy(card);
        let key = card.get(..8).unwrap_or("").trim_end();
        if key == "END" {
            let data_at = start + (at - start).div_ceil(BLOCK) * BLOCK;
            hdu.data_at = data_at;
            if naxis.is_none() {
                return Err(malformed(NAME, "the header has no NAXIS"));
            }
            return Ok(hdu);
        }
        if card.get(8..10) != Some("= ") {
            continue;
        }
        let v = value(&card);
        let int = || v.parse::<i64>().ok();
        let float = || v.replace(['D', 'd'], "E").parse::<f64>().ok();
        match key {
            "BITPIX" => {
                hdu.bitpix = int().ok_or_else(|| malformed(NAME, "BITPIX is not a number"))?
            }
            "NAXIS" => {
                let n = int().filter(|n| (0..=999).contains(n));
                naxis = Some(n.ok_or_else(|| malformed(NAME, "NAXIS is out of range"))?);
                hdu.axes = vec![0; naxis.unwrap_or(0) as usize];
            }
            "BSCALE" => hdu.bscale = float().unwrap_or(1.0),
            "BZERO" => hdu.bzero = float().unwrap_or(0.0),
            "BLANK" => hdu.blank = int(),
            "PCOUNT" => hdu.pcount = int().and_then(|v| u64::try_from(v).ok()).unwrap_or(0),
            "GCOUNT" => hdu.gcount = int().and_then(|v| u64::try_from(v).ok()).unwrap_or(1),
            "XTENSION" => hdu.is_image = v.eq_ignore_ascii_case("IMAGE"),
            k if k.starts_with("NAXIS") => {
                if let (Ok(i), Some(n)) = (k[5..].parse::<usize>(), int()) {
                    if let (Some(slot), Ok(n)) =
                        (hdu.axes.get_mut(i.wrapping_sub(1)), u64::try_from(n))
                    {
                        *slot = n;
                    }
                }
            }
            _ => {}
        }
    }
    Err(malformed(NAME, "the header is longer than any real one"))
}

/// The first HDU holding a 2D (or deeper) image.
fn image_hdu(b: &[u8]) -> Result<Hdu, CodecError> {
    if !looks_like_fits(b) {
        return Err(malformed(NAME, "it does not start with SIMPLE"));
    }
    let mut start = 0;
    for i in 0..MAX_HDUS {
        let hdu = header_at(b, start, i == 0)?;
        if hdu.is_image && hdu.axes.len() >= 2 && hdu.axes[0] > 0 && hdu.axes[1] > 0 {
            if !matches!(hdu.bitpix, 8 | 16 | 32 | 64 | -32 | -64) {
                return Err(CodecError::Unsupported(format!(
                    "FITS BITPIX {} is not a valid sample format",
                    hdu.bitpix
                )));
            }
            return Ok(hdu);
        }
        let next = (hdu.data_at as u64)
            .saturating_add(hdu.data_bytes().div_ceil(BLOCK as u64) * BLOCK as u64);
        if next >= b.len() as u64 {
            break;
        }
        start = next as usize;
    }
    Err(CodecError::Unsupported(
        "this FITS file holds no 2D image (only tables or empty HDUs)".into(),
    ))
}

fn dims(hdu: &Hdu) -> Result<(u32, u32, bool), CodecError> {
    let w = u32::try_from(hdu.axes[0]).map_err(|_| malformed(NAME, "NAXIS1 is too large"))?;
    let h = u32::try_from(hdu.axes[1]).map_err(|_| malformed(NAME, "NAXIS2 is too large"))?;
    let rgb = hdu.axes.get(2) == Some(&3);
    Ok((w, h, rgb))
}

/// Header facts.
pub fn probe(bytes: &[u8], limits: ImportLimits) -> Result<ImageInfo, CodecError> {
    let hdu = image_hdu(bytes)?;
    let (w, h, _) = dims(&hdu)?;
    limits.check_dimensions(w, h)?;
    Ok(info(w, h, ImportFormat::Fits, true))
}

fn sample(b: &[u8], bitpix: i64, blank: Option<i64>) -> Option<f64> {
    let v = match bitpix {
        8 => i64::from(b[0]),
        16 => i64::from(i16::from_be_bytes([b[0], b[1]])),
        32 => i64::from(i32::from_be_bytes([b[0], b[1], b[2], b[3]])),
        64 => {
            let mut a = [0u8; 8];
            a.copy_from_slice(&b[..8]);
            i64::from_be_bytes(a)
        }
        -32 => {
            let f = f32::from_be_bytes([b[0], b[1], b[2], b[3]]);
            return f.is_finite().then_some(f64::from(f));
        }
        _ => {
            let mut a = [0u8; 8];
            a.copy_from_slice(&b[..8]);
            let f = f64::from_be_bytes(a);
            return f.is_finite().then_some(f);
        }
    };
    (Some(v) != blank).then_some(v as f64)
}

/// Decode, stretched to 16 bits.
pub fn decode(bytes: &[u8], limits: ImportLimits) -> Result<DecodedSurface, CodecError> {
    let hdu = image_hdu(bytes)?;
    let (w, h, rgb) = dims(&hdu)?;
    let planes = if rgb { 3 } else { 1 };
    let size = (hdu.bitpix.unsigned_abs() / 8) as usize;
    let n = w as u64 * h as u64;
    let extra = n.saturating_mul(8 * planes as u64);
    check_decode(limits, w, h, 8, extra)?;
    let need = n.saturating_mul(planes as u64).saturating_mul(size as u64);
    let data = bytes
        .get(hdu.data_at..)
        .filter(|d| d.len() as u64 >= need)
        .ok_or_else(|| malformed(NAME, "the image data runs past the file"))?;
    let n = n as usize;
    let values: Vec<Option<f64>> = data
        .chunks_exact(size)
        .take(n * planes)
        .map(|s| sample(s, hdu.bitpix, hdu.blank).map(|v| hdu.bzero + hdu.bscale * v))
        .collect();
    let (lo, hi) = values
        .iter()
        .flatten()
        .filter(|v| v.is_finite())
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
            (lo.min(*v), hi.max(*v))
        });
    let span = hi - lo;
    let level = |v: Option<f64>| -> Option<u16> {
        let v = v.filter(|v| v.is_finite())?;
        Some(if span > 0.0 {
            (((v - lo) / span) * 65535.0).round().clamp(0.0, 65535.0) as u16
        } else {
            32768
        })
    };
    let mut out = vec![0u16; n * 4];
    for y in 0..h as usize {
        // FITS row 0 is the bottom row.
        let src_row = (h as usize - 1 - y) * w as usize;
        for x in 0..w as usize {
            let i = src_row + x;
            let o = (y * w as usize + x) * 4;
            let c: Vec<Option<u16>> = (0..planes).map(|p| level(values[p * n + i])).collect();
            if c.iter().all(Option::is_some) {
                let g = |k: usize| c[k.min(planes - 1)].unwrap_or(0);
                out[o..o + 4].copy_from_slice(&[g(0), g(1), g(2), 65535]);
            }
        }
    }
    let mut s = rgba8_surface(w, h, Vec::new(), ImportFormat::Fits);
    s.pixels = SurfacePixels::Rgba16(out);
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::super::test_util::fuzz;
    use super::*;
    use crate::codec::{decode_surface_bytes, probe_bytes};

    fn card(text: &str) -> Vec<u8> {
        let mut c = text.as_bytes().to_vec();
        c.resize(CARD, b' ');
        c
    }

    fn fits(bitpix: i64, axes: &[u64], extra: &[&str], data: &[u8]) -> Vec<u8> {
        let mut b = card("SIMPLE  =                    T / conforms");
        b.extend(card(&format!("BITPIX  = {bitpix:>20}")));
        b.extend(card(&format!("NAXIS   = {:>20}", axes.len())));
        for (i, a) in axes.iter().enumerate() {
            b.extend(card(&format!("NAXIS{}  = {a:>20}", i + 1)));
        }
        for e in extra {
            b.extend(card(e));
        }
        b.extend(card("END"));
        b.resize(b.len().div_ceil(BLOCK) * BLOCK, b' ');
        b.extend_from_slice(data);
        b.resize(b.len().div_ceil(BLOCK) * BLOCK, 0);
        b
    }

    fn grey(s: &DecodedSurface) -> Vec<u16> {
        let SurfacePixels::Rgba16(px) = &s.pixels else {
            panic!("not 16-bit")
        };
        px.chunks(4).map(|p| p[0]).collect()
    }

    #[test]
    fn integer_fits_stretch_and_flip() {
        // Unsigned 16-bit, stored signed with BZERO 32768: 2x2, bottom row
        // first in the file.
        let raw: Vec<u8> = [0u16, 100, 200, 300]
            .iter()
            .flat_map(|v| ((*v as i32 - 32768) as i16).to_be_bytes())
            .collect();
        let file = fits(
            16,
            &[2, 2],
            &[
                "BZERO   =                32768",
                "BSCALE  =                  1.0",
            ],
            &raw,
        );
        assert!(looks_like_fits(&file));
        let s = decode_surface_bytes(&file, ImportLimits::default()).unwrap();
        assert_eq!(
            (s.width, s.height, s.source_format),
            (2, 2, ImportFormat::Fits)
        );
        // Top row of the picture is the file's second row.
        assert_eq!(grey(&s), [43690, 65535, 0, 21845]);
        let info = probe_bytes(&file, ImportLimits::default()).unwrap();
        assert_eq!((info.width, info.height), (2, 2));
        // 8-bit, one row.
        let s = decode_surface_bytes(
            &fits(8, &[3, 1], &[], &[10, 20, 30]),
            ImportLimits::default(),
        )
        .unwrap();
        assert_eq!(grey(&s), [0, 32768, 65535]);
        // 32-bit with BLANK: the blank pixel is transparent.
        let raw: Vec<u8> = [5i32, -1, 15]
            .iter()
            .flat_map(|v| v.to_be_bytes())
            .collect();
        let s = decode_surface_bytes(
            &fits(32, &[3, 1], &["BLANK   =                   -1"], &raw),
            ImportLimits::default(),
        )
        .unwrap();
        let SurfacePixels::Rgba16(px) = &s.pixels else {
            panic!()
        };
        assert_eq!(
            px,
            &[0, 0, 0, 65535, 0, 0, 0, 0, 65535, 65535, 65535, 65535]
        );
    }

    #[test]
    fn float_and_rgb_fits_decode() {
        let raw: Vec<u8> = [0.5f32, f32::NAN, 1.5, 1.0]
            .iter()
            .flat_map(|v| v.to_be_bytes())
            .collect();
        let s =
            decode_surface_bytes(&fits(-32, &[4, 1], &[], &raw), ImportLimits::default()).unwrap();
        let SurfacePixels::Rgba16(px) = &s.pixels else {
            panic!()
        };
        assert_eq!(px[0], 0);
        assert_eq!(px[7], 0, "NaN is transparent");
        assert_eq!(px[8], 65535);
        assert_eq!(px[12], 32768);
        let raw: Vec<u8> = [2.0f64, 4.0].iter().flat_map(|v| v.to_be_bytes()).collect();
        let s =
            decode_surface_bytes(&fits(-64, &[2, 1], &[], &raw), ImportLimits::default()).unwrap();
        assert_eq!(grey(&s), [0, 65535]);
        // NAXIS3 = 3 opens as RGB: planes R, G, B of a 1x1 image.
        let s = decode_surface_bytes(
            &fits(8, &[1, 1, 3], &[], &[0, 255, 51]),
            ImportLimits::default(),
        )
        .unwrap();
        let SurfacePixels::Rgba16(px) = &s.pixels else {
            panic!()
        };
        assert_eq!(px, &[0, 65535, 13107, 65535]);
    }

    #[test]
    fn an_image_extension_after_an_empty_primary_opens() {
        let mut file = fits(8, &[], &["EXTEND  =                    T"], &[]);
        let mut ext = card("XTENSION= 'IMAGE   '           / Image extension");
        ext.extend(card("BITPIX  =                    8"));
        ext.extend(card("NAXIS   =                    2"));
        ext.extend(card("NAXIS1  =                    2"));
        ext.extend(card("NAXIS2  =                    1"));
        ext.extend(card("PCOUNT  =                    0"));
        ext.extend(card("GCOUNT  =                    1"));
        ext.extend(card("END"));
        ext.resize(BLOCK, b' ');
        ext.extend_from_slice(&[7, 9]);
        ext.resize(2 * BLOCK, 0);
        file.extend(ext);
        let s = decode_surface_bytes(&file, ImportLimits::default()).unwrap();
        assert_eq!((s.width, s.height), (2, 1));
        assert_eq!(grey(&s), [0, 65535]);
    }

    #[test]
    fn tables_only_or_damaged_fits_error_and_never_panic() {
        let err =
            decode_surface_bytes(&fits(8, &[], &[], &[]), ImportLimits::default()).unwrap_err();
        assert!(err.to_string().contains("no 2D image"), "{err}");
        let err = decode_surface_bytes(&fits(12, &[1, 1], &[], &[0, 0]), ImportLimits::default())
            .unwrap_err();
        assert!(err.to_string().contains("BITPIX"), "{err}");
        let raw: Vec<u8> = (0..16u8).collect();
        fuzz(
            &fits(16, &[2, 4], &["BZERO   =                32768"], &raw),
            ImportFormat::Fits,
        );
        let huge = fits(8, &[100_000, 100_000], &[], &[]);
        assert!(matches!(
            decode_surface_bytes(&huge, ImportLimits::default()),
            Err(CodecError::LimitExceeded(_))
        ));
    }
}
