//! W18-I: a style library with its blending options and its patterns.
//!
//! [`write_asl`](super::write_asl) writes each style's name and its effect
//! descriptor, and an empty pattern section. A Photoshop style also carries
//! its **Blending Options** — a `blendOptions` object in the `Styl`
//! descriptor, beside `Lefx`: blend mode `Md  ` (a `BlnM` enum), opacity
//! `Opct` and fill opacity `fillOpacity` (percentages) — and the library
//! carries the **patterns** its Pattern Overlays and pattern strokes name,
//! in the section before the styles, each laid out as one entry of a
//! document's `Patt` block (the layout [`psd::pattern::PatternLibrary`]
//! reads back).
//!
//! [`with_blend_options`] adds the object to a `Styl` descriptor;
//! [`write_asl_library`] writes the styles after the patterns.

use psd::bytes::{Cursor, Sink};
use psd::pattern::PsdPattern;
use psd::{Descriptor, Value};

use super::AslError;

/// A style's Blending Options.
#[derive(Clone, Debug, PartialEq)]
pub struct AslBlendOptions {
    /// The `BlnM` code of the blend mode (`Nrml`, `Mltp`, `Scrn`, …).
    pub mode: String,
    /// `0..=1`.
    pub opacity: f32,
    /// `0..=1`.
    pub fill_opacity: f32,
}

impl Default for AslBlendOptions {
    fn default() -> Self {
        Self {
            mode: "Nrml".to_string(),
            opacity: 1.0,
            fill_opacity: 1.0,
        }
    }
}

fn pct(v: f32) -> Value {
    Value::UnitFloat {
        unit: *b"#Prc",
        // Through f32's shortest decimal, so 0.1 is written 10%, not
        // 10.000000149%.
        value: (if v.is_finite() {
            v.clamp(0.0, 1.0)
        } else {
            1.0
        } * 100.0)
            .to_string()
            .parse()
            .unwrap_or(100.0),
    }
}

/// W18-I: `styl` (a style's `Styl` descriptor from its version word, as
/// [`super::AslStyle::style_descriptor`] holds it) with `options` as its
/// `blendOptions` object, replacing one it had.
pub fn with_blend_options(styl: &[u8], options: &AslBlendOptions) -> Result<Vec<u8>, AslError> {
    let unreadable = AslError::Truncated {
        what: "a style descriptor",
    };
    let mut cur = Cursor::new(styl);
    let version = cur.u32().map_err(|_| unreadable.clone())?;
    let mut descriptor =
        Descriptor::read(&mut cur, &psd::ReadOptions::default()).map_err(|_| unreadable.clone())?;
    let mut blend = Descriptor::new("blendOptions");
    let bad = |_| AslError::UnsupportedVersion {
        what: "blend mode key",
        version: 0,
    };
    blend
        .push(
            "Md  ",
            Value::Enumerated {
                type_id: "BlnM".to_string(),
                value: options.mode.clone(),
            },
        )
        .map_err(bad)?;
    blend.push("Opct", pct(options.opacity)).map_err(bad)?;
    blend
        .push("fillOpacity", pct(options.fill_opacity))
        .map_err(bad)?;
    descriptor.items.retain(|(k, _)| k != "blendOptions");
    descriptor
        .push("blendOptions", Value::Descriptor(blend))
        .map_err(bad)?;
    let mut sink = Sink::new();
    sink.u32(version);
    descriptor.write(&mut sink).map_err(bad)?;
    Ok(sink.into_inner())
}

/// W18-I: a style library of `(name, id, style descriptor)` styles, as
/// [`super::write_asl`] writes them, with `patterns` in its pattern
/// section (RGB, RLE rows, transparency in the user mask).
pub fn write_asl_library(styles: &[(&str, &str, &[u8])], patterns: &[PsdPattern]) -> Vec<u8> {
    let mut out = super::write_asl(styles);
    if patterns.is_empty() {
        return out;
    }
    // Header: u16 version, "8BSL", u16 pattern version, then the u32
    // pattern-section length (0 as `write_asl` leaves it) at byte 8.
    let section = psd::pattern::encode_block(patterns);
    let tail = out.split_off(12);
    out.truncate(8);
    out.extend_from_slice(&(section.len() as u32).to_be_bytes());
    out.extend_from_slice(&section);
    out.extend_from_slice(&tail);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_styl() -> Vec<u8> {
        let mut sink = Sink::new();
        sink.u32(16);
        let mut d = Descriptor::new("Styl");
        d.push("Lefx", Value::Descriptor(Descriptor::new("Lefx")))
            .unwrap();
        d.write(&mut sink).unwrap();
        sink.into_inner()
    }

    /// The blending options and the patterns survive a write and a read.
    #[test]
    fn a_library_carries_blending_options_and_patterns() {
        let styl = with_blend_options(
            &empty_styl(),
            &AslBlendOptions {
                mode: "Mltp".to_string(),
                opacity: 0.5,
                fill_opacity: 0.25,
            },
        )
        .unwrap();
        let pattern = PsdPattern {
            name: "Dots".to_string(),
            id: "p-1".to_string(),
            width: 2,
            height: 2,
            rgba8: vec![255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 255, 9, 9, 9, 0],
        };
        let bytes = write_asl_library(&[("Shade", "s-1", &styl)], std::slice::from_ref(&pattern));
        let lib = super::super::parse_asl(&bytes).unwrap();
        assert_eq!(lib.styles.len(), 1);
        assert_eq!(lib.styles[0].name, "Shade");

        let mut cur = Cursor::new(&lib.styles[0].style_descriptor);
        assert_eq!(cur.u32().unwrap(), 16);
        let d = Descriptor::read(&mut cur, &psd::ReadOptions::default()).unwrap();
        assert!(d.descriptor("Lefx").is_some(), "the effects are kept");
        let blend = d.descriptor("blendOptions").expect("blending options");
        assert_eq!(blend.number("Opct"), Some(50.0));
        assert_eq!(blend.number("fillOpacity"), Some(25.0));
        assert!(matches!(
            blend.get("Md  "),
            Some(Value::Enumerated { value, .. }) if value == "Mltp"
        ));

        let mut patterns = psd::pattern::PatternLibrary::default();
        let mut budget = psd::limits::Budget::new(1 << 20);
        patterns.read_block(&lib.patterns, &psd::ReadOptions::default(), &mut budget);
        assert_eq!(patterns.refused, Vec::<String>::new());
        assert_eq!(patterns.patterns, vec![pattern]);
    }

    /// Replacing the options leaves one `blendOptions`, the new one.
    #[test]
    fn blending_options_are_replaced_not_duplicated() {
        let once = with_blend_options(&empty_styl(), &AslBlendOptions::default()).unwrap();
        let twice = with_blend_options(
            &once,
            &AslBlendOptions {
                opacity: 0.1,
                ..AslBlendOptions::default()
            },
        )
        .unwrap();
        let mut cur = Cursor::new(&twice);
        cur.u32().unwrap();
        let d = Descriptor::read(&mut cur, &psd::ReadOptions::default()).unwrap();
        let count = d.items.iter().filter(|(k, _)| k == "blendOptions").count();
        assert_eq!(count, 1);
        assert_eq!(
            d.descriptor("blendOptions").unwrap().number("Opct"),
            Some(10.0)
        );
    }
}
