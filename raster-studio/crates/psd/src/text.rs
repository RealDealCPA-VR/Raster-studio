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
//! it is *set* — fonts, runs, tracking, justification — lives in the
//! `EngineData` blob under `EngineData`, in the text engine's own
//! PostScript-like syntax, which [`crate::engine_data`] parses (bounded; it is
//! untrusted input) and writes.
//!
//! # Reading and writing
//!
//! [`parse`] extracts the transform and the string; [`engine_text`] reads the
//! engine data's style runs, paragraph runs and frame. [`crate::write`] writes
//! back the bytes that were read, which round-trips a type layer exactly.
//! [`build`] / [`build_styled`] synthesise a block for a layer this build
//! authored: the engine data they embed describes every character run (a
//! payload that does not makes Photoshop discard the layer).

use crate::bytes::{Cursor, Sink};
use crate::descriptor::{Descriptor, Value};
use crate::engine_data::{self, EngineDataError, EngineText};
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

/// The engine data of a `TySh` block, read by [`crate::engine_data::extract`].
///
/// `Ok(None)` when the block has no readable text descriptor, no
/// `EngineData` key, or engine data without an `EngineDict` — nothing to read
/// is not an error. `Err` when the engine data is there but malformed or past
/// a cap; the caller reports it and falls back.
pub fn engine_text(raw: &[u8], opts: &ReadOptions) -> Result<Option<EngineText>, EngineDataError> {
    let mut cur = Cursor::new(raw);
    let header = (|| -> Option<()> {
        cur.u16().ok()?;
        for _ in 0..6 {
            cur.f64().ok()?;
        }
        cur.u16().ok()?;
        cur.u32().ok()?;
        Some(())
    })();
    if header.is_none() {
        return Ok(None);
    }
    let Ok(desc) = Descriptor::read(&mut cur, opts) else {
        return Ok(None);
    };
    match desc.get("EngineData") {
        Some(Value::RawData(bytes)) => engine_data::extract(bytes),
        _ => Ok(None),
    }
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
    let engine = EngineText {
        text: text.to_owned(),
        style_runs: vec![engine_data::StyleRun {
            length: text.encode_utf16().count(),
            style: engine_data::CharStyle {
                font: Some(font.to_owned()),
                size: Some(size_px),
                fill: Some([0.0, 0.0, 0.0, 1.0]),
                ..engine_data::CharStyle::default()
            },
        }],
        ..EngineText::default()
    };
    build_styled(&engine, transform, bounds)
}

/// Build a complete `TySh` block from an [`EngineText`] — every style run,
/// the paragraph run, the point/box frame — so a styled layer's fonts, sizes
/// and fills travel per run. [`engine_text`] reads back what this writes.
pub fn build_styled(
    engine: &EngineText,
    transform: [f64; 6],
    bounds: (i32, i32, i32, i32),
) -> Vec<u8> {
    let text = engine.text.as_str();
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
    d.push("EngineData", Value::RawData(engine_data::write(engine)))
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
        // A reader that re-typesets needs every character covered: the run
        // lengths cover the string plus the engine's trailing ``, and the
        // engine data's own text is the string.
        let engine = String::from_utf8_lossy(&parsed.raw);
        assert_eq!(engine.matches("/RunLengthArray [ 11 ]").count(), 2);
        let e = engine_text(&raw, &ReadOptions::default())
            .unwrap()
            .expect("the engine data reads back");
        assert_eq!(e.text, "SOLD TODAY");
        assert_eq!(e.style_runs.len(), 1);
        assert_eq!(e.style_runs[0].length, 11);
        assert_eq!(e.style_runs[0].style.font.as_deref(), Some("Montserrat"));
        assert_eq!(e.style_runs[0].style.size, Some(24.0));
        assert_eq!(e.style_runs[0].style.fill, Some([0.0, 0.0, 0.0, 1.0]));
    }

    /// W9-C: a styled layer's runs travel through the block — two fonts, two
    /// sizes, two fills — and [`engine_text`] reads them back per run.
    #[test]
    fn a_styled_block_carries_every_run() {
        use crate::engine_data::{CharStyle, StyleRun};
        let engine = EngineText {
            text: "Red blue".into(),
            style_runs: vec![
                StyleRun {
                    length: 4,
                    style: CharStyle {
                        font: Some("Montserrat".into()),
                        size: Some(30.0),
                        fill: Some([1.0, 0.0, 0.0, 1.0]),
                        ..CharStyle::default()
                    },
                },
                StyleRun {
                    length: 4,
                    style: CharStyle {
                        font: Some("DejaVu Serif".into()),
                        size: Some(12.5),
                        fill: Some([0.0, 0.0, 1.0, 1.0]),
                        ..CharStyle::default()
                    },
                },
            ],
            ..EngineText::default()
        };
        let raw = build_styled(&engine, IDENTITY, (0, 0, 10, 10));
        assert_eq!(
            parse(&raw, &ReadOptions::default()).text.as_deref(),
            Some("Red blue")
        );
        let back = engine_text(&raw, &ReadOptions::default()).unwrap().unwrap();
        assert_eq!(back.text, "Red blue");
        let fonts: Vec<_> = back
            .style_runs
            .iter()
            .map(|r| (r.length, r.style.font.clone(), r.style.size, r.style.fill))
            .collect();
        assert_eq!(
            fonts,
            vec![
                (
                    4,
                    Some("Montserrat".into()),
                    Some(30.0),
                    Some([1.0, 0.0, 0.0, 1.0])
                ),
                (
                    5,
                    Some("DejaVu Serif".into()),
                    Some(12.5),
                    Some([0.0, 0.0, 1.0, 1.0])
                ),
            ],
            "the last run also covers the engine's trailing \r"
        );
    }

    /// A block whose engine data is malformed reports an error from
    /// [`engine_text`] while [`parse`] still returns the string.
    #[test]
    fn malformed_engine_data_is_an_error_not_a_panic() {
        let mut s = Sink::new();
        s.u16(1);
        for v in IDENTITY {
            s.f64(v);
        }
        s.u16(50);
        s.u32(16);
        let mut d = Descriptor::new("TxLr");
        d.push("Txt ", Value::from("x")).unwrap();
        d.push(
            "EngineData",
            Value::RawData(b"<< /EngineDict << /Editor (".to_vec()),
        )
        .unwrap();
        d.write(&mut s).unwrap();
        let raw = s.into_inner();
        assert_eq!(
            parse(&raw, &ReadOptions::default()).text.as_deref(),
            Some("x")
        );
        assert!(engine_text(&raw, &ReadOptions::default()).is_err());
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
