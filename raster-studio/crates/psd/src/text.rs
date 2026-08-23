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

use crate::bytes::Cursor;
use crate::descriptor::Descriptor;
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
