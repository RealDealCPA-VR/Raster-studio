//! W18-H: Photoshop tool presets (`.tpl`), read as Photopea reads them.
//!
//! `8BTP`, a `u32` version (Photopea writes 3) and a `u32` (1), then
//! `8BIM` blocks, each a four-character key, a `u32` length and the payload,
//! padded to four bytes:
//!
//! | Key | Holds | Here |
//! |---|---|---|
//! | `tptp` | the presets: a `u32` count, then per preset its name (a Unicode string), a descriptor version (16) and one action descriptor whose class is the tool (`PbTl`, `ErTl`, ...) and whose items are the tool's options | read: [`ToolPresetEntry`] per preset |
//! | `tppa` | the patterns the presets use | counted, not read ([`TplFile::skipped`]) |
//! | `tpbd` | the sampled brush tips the presets use | counted, not read |
//! | `tpsh` | the custom shapes the presets use | counted, not read |
//! | `tpst` | the layer styles the presets use | counted, not read |
//!
//! This is the parsing half only. **Nothing in the application reads a
//! `.tpl` yet**: File > Open's resource route and the Tool Presets panel
//! (which keeps this build's own tool ids and option keys) have no mapping
//! from a Photoshop tool class and its descriptor, and those routes are
//! outside this reader.
//!
//! # Untrusted input
//!
//! Every read goes through `psd::bytes::Cursor`; each block is read inside
//! its declared length; the preset count is checked against
//! [`MAX_ENTRIES`] before anything is reserved; descriptors are read by the
//! `psd` crate's depth- and count-limited reader.

use psd::bytes::Cursor;
use psd::Descriptor;

use super::{check_count, ResourceError, MAX_ENTRIES};

/// Longest preset name read, in UTF-16 units.
const MAX_NAME_UNITS: usize = 1024;
/// Most blocks one file may hold.
const MAX_BLOCKS: usize = 4096;

/// One tool preset.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolPresetEntry {
    /// The name the user gave it.
    pub name: String,
    /// The tool, as Photoshop's class id (`PbTl` is the Brush Tool).
    pub tool_class: String,
    /// The tool's options, as saved.
    pub options: Descriptor,
}

/// A `.tpl` file.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TplFile {
    pub presets: Vec<ToolPresetEntry>,
    /// The blocks read past but not loaded, as `(key, count)` — the
    /// patterns, brush tips, shapes and styles the presets refer to, and
    /// any block key this reader does not know.
    pub skipped: Vec<(String, usize)>,
    /// Presets that could not be read, one sentence each.
    pub refused: Vec<String>,
}

/// Parse a `.tpl` file.
pub fn parse(bytes: &[u8]) -> Result<TplFile, ResourceError> {
    let mut cur = Cursor::new(bytes);
    if cur.tag().ok().as_ref() != Some(b"8BTP") {
        return Err(ResourceError::BadSignature {
            what: "tool preset",
        });
    }
    let version = cur.u32()?;
    if !(1..=16).contains(&version) {
        return Err(ResourceError::Unsupported {
            what: "tool preset file version",
            detail: version.to_string(),
        });
    }
    let _ = cur.u32()?;
    let mut file = TplFile::default();
    let mut blocks = 0usize;
    while cur.remaining() >= 12 {
        blocks += 1;
        check_count("block count", blocks, MAX_BLOCKS)?;
        let sig = cur.tag()?;
        if &sig != b"8BIM" {
            return Err(ResourceError::Malformed(format!(
                "block {blocks} does not start with 8BIM"
            )));
        }
        let key = cur.tag()?;
        let key_text = String::from_utf8_lossy(&key).into_owned();
        let len = cur.u32()? as usize;
        let mut body = cur.sub(len).map_err(|_| {
            ResourceError::Malformed(format!("the {key_text} block runs past the file"))
        })?;
        match &key {
            b"tptp" => read_presets(&mut body, &mut file)?,
            _ => {
                let count = if body.remaining() >= 4 {
                    body.u32().unwrap_or(0) as usize
                } else {
                    0
                };
                file.skipped.push((key_text, count));
            }
        }
        // Blocks are padded to four bytes.
        let pad = (4 - cur.pos() % 4) % 4;
        if cur.peek_tag() != Some(*b"8BIM") && cur.remaining() >= pad {
            cur.skip(pad)?;
        }
    }
    if file.presets.is_empty() {
        return Err(ResourceError::Empty);
    }
    Ok(file)
}

fn read_presets(body: &mut Cursor<'_>, file: &mut TplFile) -> Result<(), ResourceError> {
    let count = body.u32()? as usize;
    check_count("tool preset count", count, MAX_ENTRIES)?;
    let opts = psd::ReadOptions::default();
    file.presets.reserve(count.min(1024));
    for index in 0..count {
        let name = body.unicode_string(MAX_NAME_UNITS)?;
        let name = name.trim_end_matches('\0').to_string();
        let descriptor_version = body.u32()?;
        if descriptor_version != 16 {
            // The rest of the block cannot be walked past an unknown form.
            file.refused.push(format!(
                "preset {} ({name:?}) has descriptor version {descriptor_version}; it and the \
                 presets after it were not read",
                index + 1
            ));
            return Ok(());
        }
        let options = Descriptor::read(body, &opts)?;
        let name = if name.trim().is_empty() {
            format!("Tool Preset {}", index + 1)
        } else {
            name
        };
        file.presets.push(ToolPresetEntry {
            name,
            tool_class: options.class_id.clone(),
            options,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use psd::bytes::Sink;
    use psd::Value;

    fn unicode(sink: &mut Sink, s: &str) {
        let units: Vec<u16> = s.encode_utf16().chain([0]).collect();
        sink.u32(units.len() as u32);
        for u in units {
            sink.u16(u);
        }
    }

    fn block(out: &mut Vec<u8>, key: &[u8; 4], body: &[u8]) {
        out.extend(b"8BIM");
        out.extend(key);
        out.extend((body.len() as u32).to_be_bytes());
        out.extend_from_slice(body);
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
    }

    /// Photopea's writer: `8BTP`, 3, 1, then the blocks.
    fn tpl(presets: &[(&str, Descriptor)], extra: &[(&[u8; 4], Vec<u8>)]) -> Vec<u8> {
        let mut out = b"8BTP".to_vec();
        out.extend(3u32.to_be_bytes());
        out.extend(1u32.to_be_bytes());
        for (key, body) in extra {
            block(&mut out, key, body);
        }
        let mut sink = Sink::new();
        sink.u32(presets.len() as u32);
        for (name, d) in presets {
            unicode(&mut sink, name);
            sink.u32(16);
            d.write(&mut sink).unwrap();
        }
        block(&mut out, b"tptp", sink.as_slice());
        out
    }

    fn brush(size: f64) -> Descriptor {
        let mut d = Descriptor::new("PbTl");
        let mut tip = Descriptor::new("computedBrush");
        tip.push(
            "Dmtr",
            Value::UnitFloat {
                unit: *b"#Pxl",
                value: size,
            },
        )
        .unwrap();
        d.push("Brsh", Value::Descriptor(tip)).unwrap();
        d.push(
            "Md  ",
            Value::Enumerated {
                type_id: "BlnM".into(),
                value: "Mltp".into(),
            },
        )
        .unwrap();
        d.push("Opct", Value::Integer(40)).unwrap();
        d
    }

    #[test]
    fn presets_read_with_their_tool_and_options() {
        let bytes = tpl(
            &[("Soft 40", brush(40.0)), ("", Descriptor::new("ErTl"))],
            &[(b"tppa", 2u32.to_be_bytes().to_vec())],
        );
        let file = parse(&bytes).unwrap();
        assert_eq!(file.presets.len(), 2);
        let soft = &file.presets[0];
        assert_eq!(soft.name, "Soft 40");
        assert_eq!(soft.tool_class, "PbTl");
        assert_eq!(
            soft.options.descriptor("Brsh").unwrap().number("Dmtr"),
            Some(40.0)
        );
        assert_eq!(soft.options.number("Opct"), Some(40.0));
        assert_eq!(file.presets[1].name, "Tool Preset 2");
        assert_eq!(file.presets[1].tool_class, "ErTl");
        assert_eq!(file.skipped, vec![("tppa".to_string(), 2)]);
    }

    #[test]
    fn bad_files_are_refused_and_never_panic() {
        assert!(matches!(
            parse(b"8BPT\0\0\0\x03"),
            Err(ResourceError::BadSignature { .. })
        ));
        // No presets at all.
        let empty = tpl(&[], &[]);
        assert!(matches!(parse(&empty), Err(ResourceError::Empty)));
        let good = tpl(&[("A", brush(10.0)), ("B", brush(20.0))], &[]);
        for cut in 0..good.len() {
            let _ = parse(&good[..cut]);
        }
        for i in 0..good.len() {
            let mut f = good.clone();
            f[i] ^= 0x6B;
            let _ = parse(&f);
        }
        // A count past the limit is refused before anything is reserved.
        let mut huge = b"8BTP\0\0\0\x03\0\0\0\x01".to_vec();
        block(&mut huge, b"tptp", &u32::MAX.to_be_bytes());
        assert!(matches!(
            parse(&huge),
            Err(ResourceError::LimitExceeded { .. })
        ));
        // A block longer than the file.
        let mut long = b"8BTP\0\0\0\x03\0\0\0\x01".to_vec();
        long.extend(b"8BIMtptp\x7f\0\0\0\0\0\0\x01");
        assert!(parse(&long).is_err());
    }
}
