//! Pattern files (`.pat`).
//!
//! Two unrelated formats share the extension, and both are read:
//!
//! * **Photoshop** (`8BPT`): a `u16` version (1) and a `u32` pattern count,
//!   then the patterns, each laid out exactly like one entry of a document's
//!   `Patt` block (version, image mode, size, Unicode name, Pascal id, and a
//!   virtual-memory array list of channels). Some writers put the `Patt`
//!   block's `u32` length in front of each pattern and pad it to four bytes;
//!   both spellings are accepted. The pixels are decoded by the `psd` crate's
//!   own pattern reader — the one a `.psd`'s embedded patterns go through —
//!   with its limits and one decode budget for the whole file. Grayscale and
//!   RGB patterns load; indexed and other modes are refused by name.
//! * **GIMP** (`GPAT` at byte 20): a big-endian header (header size,
//!   version 1, width, height, bytes per pixel 1–4, `GPAT`, a UTF-8 name)
//!   and raw pixels.

use psd::bytes::Cursor;
use psd::limits::Budget;
use psd::pattern::PatternLibrary;

use super::{check_count, Loaded, PatternPreset, ResourceError, MAX_ENTRIES};

/// Largest GIMP pattern edge and pixel count accepted: the limits every
/// pattern tile in the application is held to.
const MAX_GIMP_EDGE: u32 = layer_model::effects::MAX_PATTERN_EDGE;
const MAX_GIMP_PIXELS: u64 = layer_model::effects::MAX_PATTERN_PIXELS;

/// Parse a `.pat` file.
pub fn parse(bytes: &[u8]) -> Result<Loaded<PatternPreset>, ResourceError> {
    if bytes.starts_with(b"8BPT") {
        return parse_photoshop(bytes);
    }
    if bytes.get(20..24) == Some(b"GPAT") {
        return parse_gimp(bytes).map(|p| Loaded {
            items: vec![p],
            refused: Vec::new(),
        });
    }
    Err(ResourceError::BadSignature { what: "pattern" })
}

/// The byte length of the pattern starting at `cur` (no length prefix),
/// found by walking its header; the cursor is advanced past it.
fn skeleton_len(cur: &mut Cursor<'_>, opts: &psd::ReadOptions) -> Result<usize, ResourceError> {
    let start = cur.pos();
    let _version = cur.u32()?;
    let mode = cur.u32()?;
    let _height = cur.u16()?;
    let _width = cur.u16()?;
    let _name = cur.unicode_string(opts.max_name_units)?;
    let _id = cur.pascal_string(1)?;
    if mode == 2 {
        return Err(ResourceError::Unsupported {
            what: "pattern",
            detail: "an indexed-colour pattern".into(),
        });
    }
    let _list_version = cur.u32()?;
    let list_len = cur.u32()? as usize;
    cur.skip(list_len)?;
    Ok(cur.pos() - start)
}

fn parse_photoshop(bytes: &[u8]) -> Result<Loaded<PatternPreset>, ResourceError> {
    let mut cur = Cursor::new(bytes);
    cur.skip(4)?;
    let version = cur.u16()?;
    if version != 1 {
        return Err(ResourceError::Unsupported {
            what: "pattern file version",
            detail: version.to_string(),
        });
    }
    let count = cur.u32()? as usize;
    check_count(
        "pattern count",
        count,
        MAX_ENTRIES.min(psd::pattern::MAX_PATTERNS),
    )?;
    let opts = psd::ReadOptions::default();
    let mut budget = Budget::new(opts.max_decoded_bytes);
    let mut library = PatternLibrary::default();
    for index in 0..count {
        if cur.remaining() < 8 {
            library
                .refused
                .push(format!("the file ends before pattern {}", index + 1));
            break;
        }
        // Which spelling: a pattern starts with its version word, 1; a
        // length-prefixed one has that word four bytes further on. Writers
        // pad between patterns to four bytes counted from different starts,
        // so up to three padding bytes are stepped over.
        let rest = cur.peek_rest();
        let word = |at: usize| {
            rest.get(at..at + 4)
                .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        };
        let plausible = |pad: usize| word(pad) == Some(1) || word(pad + 4) == Some(1);
        if let Some(pad) = (0..4).find(|pad| plausible(*pad)) {
            cur.skip(pad)?;
        }
        let rest = cur.peek_rest();
        let word = |at: usize| {
            rest.get(at..at + 4)
                .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        };
        let block: Vec<u8> = if word(0) == Some(1) {
            let body_start = cur.pos();
            let len = match skeleton_len(&mut cur, &opts) {
                Ok(len) => len,
                Err(e) => {
                    library.refused.push(format!("pattern {}: {e}", index + 1));
                    break;
                }
            };
            let body = &bytes[body_start..body_start + len];
            let mut block = Vec::with_capacity(len + 4);
            block.extend_from_slice(&(len as u32).to_be_bytes());
            block.extend_from_slice(body);
            block
        } else if word(4) == Some(1) {
            let len = cur.u32()? as usize;
            let Ok(body) = cur.take(len) else {
                library.refused.push(format!(
                    "pattern {} runs past the end of the file",
                    index + 1
                ));
                break;
            };
            let mut block = Vec::with_capacity(len + 4);
            block.extend_from_slice(&(len as u32).to_be_bytes());
            block.extend_from_slice(body);
            block
        } else {
            library.refused.push(format!(
                "pattern {} does not start where expected",
                index + 1
            ));
            break;
        };
        library.read_block(&block, &opts, &mut budget);
    }
    Ok(Loaded {
        items: library
            .patterns
            .into_iter()
            .map(|p| PatternPreset {
                name: if p.name.trim().is_empty() {
                    "Pattern".to_string()
                } else {
                    p.name
                },
                width: p.width,
                height: p.height,
                rgba8: p.rgba8,
            })
            .collect(),
        refused: library.refused,
    })
}

fn parse_gimp(bytes: &[u8]) -> Result<PatternPreset, ResourceError> {
    let mut cur = Cursor::new(bytes);
    let header = cur.u32()? as usize;
    let version = cur.u32()?;
    let width = cur.u32()?;
    let height = cur.u32()?;
    let depth = cur.u32()? as usize;
    if version != 1 {
        return Err(ResourceError::Unsupported {
            what: "GIMP pattern version",
            detail: version.to_string(),
        });
    }
    if !(1..=4).contains(&depth) {
        return Err(ResourceError::Unsupported {
            what: "GIMP pattern depth",
            detail: format!("{depth} bytes per pixel"),
        });
    }
    if width == 0 || height == 0 || width > MAX_GIMP_EDGE || height > MAX_GIMP_EDGE {
        return Err(ResourceError::LimitExceeded {
            what: "GIMP pattern edge",
            value: u64::from(width.max(height)),
            max: u64::from(MAX_GIMP_EDGE),
        });
    }
    let area = u64::from(width) * u64::from(height);
    if area > MAX_GIMP_PIXELS {
        return Err(ResourceError::LimitExceeded {
            what: "GIMP pattern pixels",
            value: area,
            max: MAX_GIMP_PIXELS,
        });
    }
    if header < 24 || header > bytes.len() {
        return Err(ResourceError::Malformed(
            "the GIMP pattern header size".into(),
        ));
    }
    let name = String::from_utf8_lossy(&bytes[24..header])
        .trim_end_matches('\0')
        .to_string();
    let pixels = (width as usize) * (height as usize);
    let data = bytes.get(header..header + pixels * depth).ok_or_else(|| {
        ResourceError::Malformed("the GIMP pattern's pixels are cut short".into())
    })?;
    let mut rgba8 = Vec::with_capacity(pixels * 4);
    for px in data.chunks_exact(depth) {
        rgba8.extend_from_slice(&match depth {
            1 => [px[0], px[0], px[0], 255],
            2 => [px[0], px[0], px[0], px[1]],
            3 => [px[0], px[1], px[2], 255],
            _ => [px[0], px[1], px[2], px[3]],
        });
    }
    Ok(PatternPreset {
        name: if name.trim().is_empty() {
            "Pattern".to_string()
        } else {
            name
        },
        width,
        height,
        rgba8,
    })
}
