//! Adobe Swatch Exchange (`.ase`).
//!
//! `ASEF`, a `u16` major and minor version (1.0), a `u32` block count, then
//! blocks: a `u16` type, a `u32` length and that many bytes. A colour entry
//! (`0x0001`) is a `u16` count of UTF-16 name units (including a NUL), the
//! name, a four-byte model (`RGB `, `CMYK`, `LAB `, `Gray`), its components
//! as big-endian `f32`, and a `u16` swatch type. Group start (`0xC001`) and
//! end (`0xC002`) blocks are skipped: the Swatches panel is one flat list.
//! Every block is read through its own length, so an unknown or damaged
//! block damages only itself.

use psd::bytes::Cursor;

use super::{
    check_count, cmyk_to_rgb, gray_ink_to_rgb, lab_to_rgb, Loaded, ResourceError, SwatchResource,
    MAX_ENTRIES,
};

const COLOR_ENTRY: u16 = 0x0001;

/// Longest swatch name read, in UTF-16 units.
const MAX_NAME_UNITS: usize = 1_024;

fn f32_of(cur: &mut Cursor<'_>) -> Result<f64, ResourceError> {
    let v = f64::from(f32::from_bits(cur.u32()?));
    if v.is_finite() {
        Ok(v)
    } else {
        Err(ResourceError::Malformed(
            "a non-finite colour component".into(),
        ))
    }
}

fn read_entry(block: &mut Cursor<'_>) -> Result<SwatchResource, ResourceError> {
    let units = block.u16()? as usize;
    if units > MAX_NAME_UNITS {
        return Err(ResourceError::LimitExceeded {
            what: "swatch name length",
            value: units as u64,
            max: MAX_NAME_UNITS as u64,
        });
    }
    let raw = block.take(units * 2)?;
    let utf16: Vec<u16> = raw
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_be_bytes(*c))
        .collect();
    let name = String::from_utf16_lossy(&utf16)
        .trim_end_matches('\0')
        .to_string();
    let model = block.tag()?;
    let rgb = match &model {
        b"RGB " => {
            let (r, g, b) = (f32_of(block)?, f32_of(block)?, f32_of(block)?);
            [
                r.clamp(0.0, 1.0) as f32,
                g.clamp(0.0, 1.0) as f32,
                b.clamp(0.0, 1.0) as f32,
            ]
        }
        b"CMYK" => cmyk_to_rgb(
            f32_of(block)?,
            f32_of(block)?,
            f32_of(block)?,
            f32_of(block)?,
        ),
        // L is stored as a fraction of 100; a and b as they are.
        b"LAB " => lab_to_rgb(f32_of(block)? * 100.0, f32_of(block)?, f32_of(block)?),
        // Grey is stored as lightness in Adobe's writers, 1.0 = white.
        b"Gray" => gray_ink_to_rgb(1.0 - f32_of(block)?),
        other => {
            return Err(ResourceError::Unsupported {
                what: "colour model",
                detail: String::from_utf8_lossy(other).into_owned(),
            })
        }
    };
    Ok(SwatchResource {
        name,
        rgba: [rgb[0], rgb[1], rgb[2], 1.0],
    })
}

/// Parse an `.ase` file.
pub fn parse(bytes: &[u8]) -> Result<Loaded<SwatchResource>, ResourceError> {
    let mut cur = Cursor::new(bytes);
    if cur.tag().ok().as_ref() != Some(b"ASEF") {
        return Err(ResourceError::BadSignature {
            what: "swatch exchange",
        });
    }
    let major = cur.u16()?;
    let _minor = cur.u16()?;
    if major != 1 {
        return Err(ResourceError::Unsupported {
            what: "swatch exchange version",
            detail: major.to_string(),
        });
    }
    let blocks = cur.u32()? as usize;
    // A block is at least six bytes of header.
    check_count("swatch exchange block count", blocks, MAX_ENTRIES * 4)?;
    let mut loaded = Loaded::default();
    for index in 0..blocks {
        if cur.is_empty() {
            loaded
                .refused
                .push(format!("the file ends before block {}", index + 1));
            break;
        }
        let kind = cur.u16()?;
        let len = cur.u32()? as usize;
        let Ok(mut block) = cur.sub(len) else {
            loaded
                .refused
                .push(format!("block {} runs past the end of the file", index + 1));
            break;
        };
        if kind != COLOR_ENTRY {
            continue;
        }
        if loaded.items.len() >= MAX_ENTRIES {
            loaded
                .refused
                .push(format!("swatches past the first {MAX_ENTRIES}"));
            break;
        }
        match read_entry(&mut block) {
            Ok(swatch) => loaded.items.push(swatch),
            Err(e) => loaded.refused.push(format!("swatch {}: {e}", index + 1)),
        }
    }
    Ok(loaded)
}
