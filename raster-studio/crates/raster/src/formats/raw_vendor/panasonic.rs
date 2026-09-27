//! W18-D: Panasonic RW2 / RWL (and Leica bodies built on them).
//!
//! An RW2 is a TIFF with the header magic `IIU\0` whose IFD0 carries the
//! Panasonic tags: `SensorWidth` / `SensorHeight` (`0x02` / `0x03`), the
//! active area's `SensorTopBorder` / `LeftBorder` / `BottomBorder` /
//! `RightBorder` (`0x04`-`0x07`), `CFAPattern` (`0x09`: 1 red-green /
//! green-blue, 2 green-red / blue-green, 3 green-blue / red-green, 4
//! blue-green / green-red), `BitsPerSample` (`0x0A`), `Compression`
//! (`0x0B`), `BlackLevelRed` / `Green` / `Blue` (`0x1C`-`0x1E`),
//! `WBRedLevel` / `Green` / `Blue` (`0x24`-`0x26`), `Orientation` and the
//! data at `RawDataOffset` (`0x118`, else `StripOffsets`).
//!
//! Data holding two bytes per pixel (to the end of the file) is read as
//! little-endian 16-bit words. Panasonic's packed RAW 1-4 compression
//! (`34316`, `34826`, `34828`, `34830`: every body's default) is refused by
//! name.

use crate::codec::{CodecError, ImportLimits};

use super::{
    balance, broken, budget, ifd0, not_decoded, orientation, rggb, tiff_any, unpack_plain, Mosaic,
    Pattern, Want, TAG_STRIP_OFFSETS,
};

const NAME: &str = "Panasonic RW2";

pub(super) fn read(bytes: &[u8], limits: ImportLimits, want: Want) -> Result<Mosaic, CodecError> {
    let tiff = tiff_any(bytes).ok_or_else(|| broken(NAME, "not a TIFF"))?;
    let ifd0 = ifd0(&tiff, NAME)?;
    let u = |tag: u16| ifd0.uint(&tiff, tag).map(|v| v as usize);
    let (width, height) = (u(0x02).unwrap_or(0), u(0x03).unwrap_or(0));
    budget(NAME, limits, width, height)?;
    let offset = u(0x118)
        .or_else(|| u(TAG_STRIP_OFFSETS))
        .ok_or_else(|| broken(NAME, "there is no RawDataOffset"))?;
    let data = bytes
        .get(offset..)
        .filter(|d| !d.is_empty())
        .ok_or_else(|| broken(NAME, "the raw data lies past the end of the file"))?;
    if data.len() < width * height * 2 {
        let what = match u(0x0B) {
            Some(c @ (34316 | 34826 | 34828 | 34830)) => format!("packed RAW compression {c}"),
            Some(c) => format!("raw compression {c}"),
            None => "packed raw data".to_string(),
        };
        return Err(not_decoded(NAME, &what));
    }
    let samples = if want == Want::Pixels {
        unpack_plain(NAME, data, width, height, 16, true)?
    } else {
        Vec::new()
    };
    let active = match (u(0x04), u(0x05), u(0x06), u(0x07)) {
        (Some(t), Some(l), Some(b), Some(r)) if t < b && l < r && b <= height && r <= width => {
            (t, l, b, r)
        }
        _ => (0, 0, height, width),
    };
    let codes = match u(0x09) {
        Some(2) => vec![1, 0, 2, 1],
        Some(3) => vec![1, 2, 0, 1],
        Some(4) => vec![2, 1, 1, 0],
        _ => rggb().codes,
    };
    let level = |tag: u16| u(tag).map(|v| v as f64);
    let black = match (level(0x1C), level(0x1D), level(0x1E)) {
        (Some(r), Some(g), Some(b)) => {
            // Per 2x2 cell, by the colour each cell holds.
            let by = [r, g, b];
            [0, 1, 2, 3].map(|i| by[usize::from(codes[i])])
        }
        _ => [0.0; 4],
    };
    let wb = match (level(0x24), level(0x25), level(0x26)) {
        (Some(r), Some(g), Some(b)) => balance(r, g, b),
        _ => None,
    };
    let bits = u(0x0A).unwrap_or(12).clamp(1, 16) as u32;
    Ok(Mosaic {
        name: NAME,
        width,
        height,
        samples,
        pattern: Pattern {
            rows: 2,
            cols: 2,
            codes,
        },
        active,
        black,
        white: None,
        full_scale: f64::from((1u32 << bits) - 1),
        wb,
        orientation: orientation(&tiff, &ifd0),
    })
}
