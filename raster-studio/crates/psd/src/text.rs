//! `TySh` — the type-tool object setting, which is where a text layer keeps its
//! string.
//!
//! ```text
//! u16 version (1)
//! f64 × 6      transform: xx xy yx yy tx ty
//! u16 text version (50)   u32 descriptor version (16)   descriptor
//! u16 warp version (1)    u32 descriptor version (16)   descriptor
//! i32 × 4      left top right bottom
//! ```
//!
//! The string lives at key `Txt ` in the first descriptor. Everything about how
//! it is *set* — fonts, runs, kerning, justification — lives in an opaque
//! `EngineData` blob under `EngineData`, in a private textual format.
//!
//! # Why this module only reads
//!
//! [`parse`] extracts the transform and the string. There is deliberately no
//! "build a `TySh` from a string" counterpart: Photoshop discards a type layer
//! whose engine data does not describe every character run, so a synthesised
//! block would produce a file that opens with the text layer *missing* — worse
//! than not writing one. [`crate::write`] therefore writes back the bytes that
//! were read, which round-trips a text layer exactly.

use crate::bytes::{Cursor, Sink};
use crate::descriptor::{Descriptor, Value};
use crate::limits::ReadOptions;
use crate::model::TextData;

/// The identity transform, used when a block's transform cannot be read.
pub const IDENTITY: [f64; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// Parse a `TySh` payload, best-effort.
///
/// Always succeeds: `raw` is retained whatever happens, because a block this
/// crate cannot interpret still has to survive a save. Fields that could not be
/// read come back as [`IDENTITY`] and `None`.
pub fn parse(raw: &[u8], opts: &ReadOptions) -> TextData {
    let mut data = TextData {
        transform: IDENTITY,
        text: None,
        raw: raw.to_vec(),
    };
    let mut cur = Cursor::new(raw);
    if cur.u16().is_err() {
        return data;
    }
    let mut transform = [0.0f64; 6];
    for slot in transform.iter_mut() {
        match cur.f64() {
            Ok(v) => *slot = v,
            Err(_) => return data,
        }
    }
    data.transform = transform;

    // text version, then descriptor version.
    if cur.u16().is_err() || cur.u32().is_err() {
        return data;
    }
    if let Ok(desc) = Descriptor::read(&mut cur, opts) {
        data.text = desc.text("Txt ").map(str::to_owned);
    }
    data
}

/// The warp descriptor, when it can be read. Mostly useful for telling a warped
/// type layer from a flat one.
pub fn warp(raw: &[u8], opts: &ReadOptions) -> Option<Descriptor> {
    let mut cur = Cursor::new(raw);
    cur.u16().ok()?;
    for _ in 0..6 {
        cur.f64().ok()?;
    }
    cur.u16().ok()?;
    cur.u32().ok()?;
    Descriptor::read(&mut cur, opts).ok()?;
    cur.u16().ok()?;
    cur.u32().ok()?;
    Descriptor::read(&mut cur, opts).ok()
}

// -------------------------------------------------- card 079: synthesis

/// Escape a string for a PostScript literal inside engine data: backslashes
/// and parens are quoted, and every byte outside printable ASCII becomes a
/// `\ooo` octal escape. The engine data is a private textual format, so the
/// bytes the reader sees are exactly the bytes the writer meant.
fn ps_literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 8);
    for b in text.as_bytes() {
        match b {
            b'\\' => out.push_str("\\\\"),
            b'(' => out.push_str("\\("),
            b')' => out.push_str("\\)"),
            0x20..=0x7e => out.push(*b as char),
            _ => out.push_str(&format!("\\{:03o}", b)),
        }
    }
    out
}

/// The engine-data payload for the supported subset (card 079): one default
/// paragraph sheet, one style run spanning the whole string, the font and
/// size named, black fill. This is the shape Photoshop's own engine data
/// takes for a simple single-run point-text layer; a reader that re-typesets
/// the string needs the run array to cover every character, so the run
/// length is the string's character count.
fn engine_data(text: &str, font: &str, size_px: f64, transform: [f64; 6]) -> Vec<u8> {
    let chars = text.chars().count();
    let mut s = String::with_capacity(2048);
    let t = format!(
        "{} {} {} {} {} {}",
        transform[0], transform[1], transform[2], transform[3], transform[4], transform[5]
    );
    s.push_str("/EngineDict\n/Editor\n/Text (");
    s.push_str(&ps_literal(text));
    s.push_str(")\n/ParagraphRun\n<<\n/DefaultRunData\n<</ParagraphSheet\n<</DefaultStyleSheet\n<</Font\n/Name (");
    s.push_str(&ps_literal(font));
    s.push_str(")\n/Script 0\n/FontType 1\n>\n/FontSize ");
    s.push_str(&size_px.to_string());
    s.push_str("\n/FillColor\n<</Type 1\n/Values [ 0 0 0 ]\n>\n>\n/Justification 0\n/FirstLineIndent 0\n/StartIndent 0\n/EndIndent 0\n/SpaceBefore 0\n/SpaceAfter 0\n/LineSpacing ");
    s.push_str(&size_px.to_string());
    s.push_str("\n/AutoLeading 1\n/LeadingType 0\n/Tracking 0\n/HorizontalScale 100\n/Direction 2\n/CharacterDirection 0\n/LinkAlignment 2\n>\n>\n/StyleRun\n<</RunLength ");
    s.push_str(&chars.to_string());
    s.push_str("\n/RunData\n<</StyleSheet\n<</StyleSheetData\n<</Font\n/Name (");
    s.push_str(&ps_literal(font));
    s.push_str(")\n/Script 0\n/FontType 1\n>\n/FontSize ");
    s.push_str(&size_px.to_string());
    s.push_str(
        "\n/FillColor\n<</Type 1\n/Values [ 0 0 0 ]\n>\n>\n>\n>\n>\n>\n/RunArray\n<</RunLength ",
    );
    s.push_str(&chars.to_string());
    s.push_str("\n/RunData\n<</StyleSheet\n<</StyleSheetData\n<</Font\n/Name (");
    s.push_str(&ps_literal(font));
    s.push_str(")\n/Script 0\n/FontType 1\n>\n/FontSize ");
    s.push_str(&size_px.to_string());
    s.push_str(
        "\n/FillColor\n<</Type 1\n/Values [ 0 0 0 ]\n>\n>\n>\n>\n>\n>\n>\n/Render\n<</Transform (",
    );
    s.push_str(&t);
    s.push_str(")\n/FontSize ");
    s.push_str(&size_px.to_string());
    s.push_str("\n>\n");
    s.into_bytes()
}

/// Build a complete `TySh` block for the supported text subset (card 079):
/// the string in the text descriptor, the layer's transform, a no-warp
/// block, the layer rectangle, and a complete engine-data payload — one
/// paragraph, one style run covering every character, the named font at the
/// named size with a black fill.
///
/// This is what makes a type layer this build writes *editable* in another
/// application rather than merely present: the engine data describes every
/// character run, which is what a re-typesetting reader demands (a partial
/// payload makes Photoshop discard the layer — see the module doc).
///
/// Only the subset is claimed. Styling this build does not encode here
/// (per-span styles, custom kerning, paragraph boxes) stays covered by the
/// layer's raster fallback pixels, and the export report names the subset.
pub fn build(
    text: &str,
    transform: [f64; 6],
    bounds: (i32, i32, i32, i32),
    font: &str,
    size_px: f64,
) -> Vec<u8> {
    let mut s = Sink::new();
    s.u16(1);
    for v in transform {
        s.f64(v);
    }
    s.u16(50);
    s.u32(16);
    let mut d = Descriptor::new("TxLr");
    d.push("Txt ", Value::from(text)).unwrap();
    d.push(
        "textGridding",
        Value::Enumerated {
            type_id: "textGridding".into(),
            value: "None".into(),
        },
    )
    .unwrap();
    d.push(
        "EngineData",
        Value::RawData(engine_data(text, font, size_px, transform)),
    )
    .unwrap();
    d.write(&mut s).unwrap();
    s.u16(1);
    s.u32(16);
    let mut warp = Descriptor::new("warp");
    warp.push(
        "warpStyle",
        Value::Enumerated {
            type_id: "warpStyle".into(),
            value: "warpNone".into(),
        },
    )
    .unwrap();
    warp.write(&mut s).unwrap();
    s.i32(bounds.0);
    s.i32(bounds.1);
    s.i32(bounds.2);
    s.i32(bounds.3);
    s.into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytes::Sink;
    use crate::descriptor::Value;

    /// Build a `TySh` payload shaped exactly like the one Photoshop writes.
    fn fixture(text: &str, transform: [f64; 6]) -> Vec<u8> {
        let mut s = Sink::new();
        s.u16(1);
        for v in transform {
            s.f64(v);
        }
        s.u16(50);
        s.u32(16);
        let mut d = Descriptor::new("TxLr");
        d.push("Txt ", Value::from(text)).unwrap();
        d.push(
            "textGridding",
            Value::Enumerated {
                type_id: "textGridding".into(),
                value: "None".into(),
            },
        )
        .unwrap();
        d.push("EngineData", Value::RawData(b"<< /EngineDict >>".to_vec()))
            .unwrap();
        d.write(&mut s).unwrap();
        s.u16(1);
        s.u32(16);
        let mut warp = Descriptor::new("warp");
        warp.push(
            "warpStyle",
            Value::Enumerated {
                type_id: "warpStyle".into(),
                value: "warpNone".into(),
            },
        )
        .unwrap();
        warp.write(&mut s).unwrap();
        s.i32(0);
        s.i32(0);
        s.i32(200);
        s.i32(40);
        s.into_inner()
    }

    #[test]
    fn the_string_and_the_transform_come_back_out() {
        let t = [1.0, 0.0, 0.0, 1.0, 24.5, -8.0];
        let raw = fixture("Hello, world", t);
        let parsed = parse(&raw, &ReadOptions::default());
        assert_eq!(parsed.text.as_deref(), Some("Hello, world"));
        assert_eq!(parsed.transform, t);
        assert_eq!(parsed.raw, raw, "the block is preserved verbatim");
    }

    #[test]
    fn non_ascii_text_survives_the_utf16_round_trip() {
        let raw = fixture("Ελλάδα — 日本語 🎨", IDENTITY);
        let parsed = parse(&raw, &ReadOptions::default());
        assert_eq!(parsed.text.as_deref(), Some("Ελλάδα — 日本語 🎨"));
    }

    #[test]
    fn the_warp_descriptor_is_reachable_after_the_text_descriptor() {
        let raw = fixture("x", IDENTITY);
        let w = warp(&raw, &ReadOptions::default()).unwrap();
        assert_eq!(w.class_id, "warp");
        assert_eq!(
            w.get("warpStyle"),
            Some(&Value::Enumerated {
                type_id: "warpStyle".into(),
                value: "warpNone".into()
            })
        );
    }

    #[test]
    fn a_truncated_block_yields_defaults_and_keeps_its_bytes() {
        let raw = fixture("Hello", IDENTITY);
        for cut in 0..raw.len() {
            let parsed = parse(&raw[..cut], &ReadOptions::default());
            assert_eq!(parsed.raw, raw[..cut].to_vec());
            if cut < 2 {
                assert_eq!(parsed.transform, IDENTITY);
            }
            // Never a panic, and never a wrong string.
            if let Some(t) = &parsed.text {
                assert_eq!(t, "Hello");
            }
        }
    }

    #[test]
    fn garbage_bytes_parse_to_defaults_rather_than_panicking() {
        let junk: Vec<u8> = (0..512u32).map(|i| (i * 97 % 256) as u8).collect();
        let parsed = parse(&junk, &ReadOptions::default());
        assert_eq!(parsed.raw, junk);
        // Whether a warp descriptor happens to parse out of noise is not the
        // point; that the call returns at all is.
        let _ = warp(&junk, &ReadOptions::default());
    }

    #[test]
    fn a_descriptor_without_a_text_key_reports_no_text() {
        let mut s = Sink::new();
        s.u16(1);
        for v in IDENTITY {
            s.f64(v);
        }
        s.u16(50);
        s.u32(16);
        Descriptor::new("TxLr").write(&mut s).unwrap();
        let raw = s.into_inner();
        let parsed = parse(&raw, &ReadOptions::default());
        assert_eq!(parsed.text, None);
        assert_eq!(parsed.transform, IDENTITY);
    }
}

#[cfg(test)]
mod build_tests {
    use super::*;

    /// Card 079: the synthesized block round-trips through the parser — the
    /// string, the transform and the bounds come back exactly, and the
    /// engine data names the font and covers every character.
    #[test]
    fn the_built_type_block_round_trips_through_the_parser() {
        let t = [1.0, 0.0, 0.0, 1.0, 12.0, 34.0];
        let raw = build("SOLD TODAY", t, (5, 6, 105, 46), "Montserrat", 24.0);
        let parsed = parse(&raw, &ReadOptions::default());
        assert_eq!(parsed.text.as_deref(), Some("SOLD TODAY"));
        assert_eq!(parsed.transform, t);
        assert!(String::from_utf8_lossy(&parsed.raw).contains("/Name (Montserrat)"));
        assert!(String::from_utf8_lossy(&parsed.raw).contains("/FontSize 24"));
        assert!(String::from_utf8_lossy(&parsed.raw).contains("/RunLength 10"));
        // A reader that re-typesets needs every character covered: the style
        // run and the run array agree on the length, and the text is there.
        let engine = String::from_utf8_lossy(&parsed.raw);
        assert_eq!(engine.matches("/RunLength 10").count(), 2);
        assert!(engine.contains("/Text (SOLD TODAY)"));
    }

    /// Non-ASCII survives: the engine data escapes the bytes, the descriptor
    /// carries UTF-16.
    #[test]
    fn the_built_block_survives_non_ascii_text() {
        let raw = build("Ελλάδα", IDENTITY, (0, 0, 10, 10), "Sans", 12.0);
        let parsed = parse(&raw, &ReadOptions::default());
        assert_eq!(parsed.text.as_deref(), Some("Ελλάδα"));
        let engine = String::from_utf8_lossy(&parsed.raw);
        assert!(engine.contains("/RunLength 6"));
    }
}
