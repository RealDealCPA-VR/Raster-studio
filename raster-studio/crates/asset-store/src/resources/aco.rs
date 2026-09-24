//! Photoshop colour swatches (`.aco`).
//!
//! A version 1 section — `u16` version (1), `u16` count, then per colour a
//! `u16` colour space and four `u16` components — optionally followed by a
//! version 2 section with the same colours plus a Unicode name each (a `u32`
//! count of UTF-16 units including a terminating NUL, then the units). When
//! the version 2 section is present and reads, its names are used;
//! otherwise the colours are named "Swatch N".
//!
//! Colour spaces: 0 RGB (`0..=65535`), 1 HSB (hue `0..=65535` for
//! `0..360°`), 2 CMYK (`0` is full ink, `65535` none), 7 Lab (L `0..=10000`,
//! a and b signed hundredths), 8 Grayscale (`0..=10000` ink coverage). Other
//! spaces (Pantone, Focoltone, Toyo, …) name a colour book and are refused
//! by name.

use psd::bytes::Cursor;

use super::{
    check_count, cmyk_to_rgb, gray_ink_to_rgb, hsb_to_rgb, lab_to_rgb, Loaded, ResourceError,
    SwatchResource, MAX_ENTRIES,
};

/// Longest swatch name read, in UTF-16 units.
const MAX_NAME_UNITS: usize = 1_024;

/// One colour as the file stores it: `(space, [w, x, y, z])`.
type Raw = (u16, [u16; 4]);

/// The colour in sRGB, or why not.
fn rgb_of((space, [w, x, y, z]): Raw) -> Result<[f32; 3], String> {
    let u = |v: u16| f64::from(v) / 65535.0;
    Ok(match space {
        0 => [u(w) as f32, u(x) as f32, u(y) as f32],
        1 => hsb_to_rgb(u(w) * 360.0, u(x), u(y)),
        // 0 is 100% ink in this format.
        2 => cmyk_to_rgb(1.0 - u(w), 1.0 - u(x), 1.0 - u(y), 1.0 - u(z)),
        7 => lab_to_rgb(
            f64::from(w) / 100.0,
            f64::from(x as i16) / 100.0,
            f64::from(y as i16) / 100.0,
        ),
        8 => gray_ink_to_rgb(f64::from(w.min(10_000)) / 10_000.0),
        other => {
            return Err(format!(
                "colour space {other} (a colour book) is not supported"
            ))
        }
    })
}

fn read_raw(cur: &mut Cursor<'_>) -> Result<Raw, ResourceError> {
    Ok((cur.u16()?, [cur.u16()?, cur.u16()?, cur.u16()?, cur.u16()?]))
}

/// Parse an `.aco` file.
pub fn parse(bytes: &[u8]) -> Result<Loaded<SwatchResource>, ResourceError> {
    let mut cur = Cursor::new(bytes);
    let version = cur
        .u16()
        .map_err(|_| ResourceError::BadSignature { what: "swatches" })?;
    if version != 1 && version != 2 {
        return Err(ResourceError::BadSignature { what: "swatches" });
    }
    // A file may be a lone version 2 section.
    let (v1, names) = if version == 1 {
        let count = cur.u16()? as usize;
        check_count("swatch count", count, MAX_ENTRIES)?;
        let mut v1 = Vec::with_capacity(count.min(cur.remaining() / 10));
        for _ in 0..count {
            v1.push(read_raw(&mut cur)?);
        }
        // The optional version 2 section: the same colours, named.
        let named = if cur.remaining() >= 4 && cur.u16().ok() == Some(2) {
            read_v2(&mut cur).ok().filter(|v2| v2.len() == v1.len())
        } else {
            None
        };
        match named {
            Some(v2) => (v2.iter().map(|(raw, _)| *raw).collect::<Vec<_>>(), Some(v2)),
            None => (v1, None),
        }
    } else {
        let v2 = read_v2(&mut cur)?;
        (v2.iter().map(|(raw, _)| *raw).collect(), Some(v2))
    };
    let mut loaded = Loaded::default();
    for (i, raw) in v1.into_iter().enumerate() {
        let name = names
            .as_ref()
            .and_then(|n| n.get(i))
            .map(|(_, name)| name.clone())
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| format!("Swatch {}", i + 1));
        match rgb_of(raw) {
            Ok([r, g, b]) => loaded.items.push(SwatchResource {
                name,
                rgba: [r, g, b, 1.0],
            }),
            Err(why) => loaded.refused.push(format!("{name}: {why}")),
        }
    }
    Ok(loaded)
}

/// A version 2 section, after its version word: `(colour, name)` pairs.
fn read_v2(cur: &mut Cursor<'_>) -> Result<Vec<(Raw, String)>, ResourceError> {
    let count = cur.u16()? as usize;
    check_count("swatch count", count, MAX_ENTRIES)?;
    // Each entry is at least 10 bytes of colour and 4 of name length.
    let mut out = Vec::with_capacity(count.min(cur.remaining() / 14));
    for _ in 0..count {
        let raw = read_raw(cur)?;
        let name = cur.unicode_string(MAX_NAME_UNITS)?;
        out.push((raw, name));
    }
    Ok(out)
}
