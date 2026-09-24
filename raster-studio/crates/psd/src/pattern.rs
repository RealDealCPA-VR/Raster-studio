//! W8-D: Photoshop pattern resources, and the pattern overlays and pattern
//! fill layers that name them.
//!
//! A document's patterns travel in document-level tagged blocks keyed
//! `Patt`, `Pat2` or `Pat3` (the three keys Photoshop has used for the same
//! layout). A pattern overlay (`patternFill` in an `lfx2` block; `PtFl` and
//! `PttR` are accepted too) and a pattern fill layer (the `PtFl` adjustment
//! key) do not carry pixels: their `Ptrn` object names a pattern by `Idnt`
//! (its unique id) and `Nm  ` (its name), and this module resolves that name
//! against the patterns the file carries.
//!
//! # The block layout (as implemented)
//!
//! A block is a run of patterns, each one:
//!
//! | Field | Size |
//! |---|---|
//! | length of the rest of this pattern | `u32` |
//! | version (1) | `u32` |
//! | image mode (1 greyscale, 3 RGB; indexed and others are refused by name) | `u32` |
//! | height, width | `u16`, `u16` |
//! | name | Unicode string |
//! | unique id | Pascal string |
//! | pixels: a virtual-memory array list | see below |
//!
//! padded so the next pattern starts on a 4-byte boundary. The array list is a
//! version word (3), a `u32` length, the pixel rectangle (top, left, bottom,
//! right as `i32`), a `u32` channel count `n`, then `n + 2` arrays — the
//! colour channels first, then the user mask (the pattern's transparency) and
//! the sheet mask. Each array is a `u32` "written" flag (0: absent, nothing
//! follows), a `u32` length (0: absent), then a `u32` depth, its own
//! rectangle, a `u16` depth, a `u8` compression (0 raw, 1 RLE: a `u16` byte
//! count per row followed by PackBits rows) and the data.
//!
//! # Untrusted input
//!
//! Every length is carved into a sub-cursor, so a pattern that lies about its
//! size damages only itself: it is refused (and named in
//! [`PatternLibrary::refused`]) while the patterns around it still load. The
//! pattern count, each edge ([`ReadOptions::max_dimension`] and
//! [`layer_model::effects::MAX_PATTERN_EDGE`]), each pattern's pixel count
//! ([`layer_model::effects::MAX_PATTERN_PIXELS`]), the channel count
//! ([`ReadOptions::max_channels_per_layer`]) and the total decoded bytes
//! ([`ReadOptions::max_decoded_bytes`], one [`Budget`] for the whole library)
//! are checked before anything is allocated.

use crate::bytes::{Cursor, Sink};
use crate::descriptor::{Descriptor, Value};
use crate::error::{PsdError, PsdResult};
use crate::limits::Budget;
use crate::model::{Adjustment, Effects, PsdFile};
use crate::ReadOptions;
use layer_model::effects::{
    PatternFill, PatternOverlayEffect, PatternTile, MAX_PATTERN_EDGE, MAX_PATTERN_PIXELS,
};
use layer_model::BlendMode;

/// The document-level tagged-block keys that carry patterns.
pub const PATTERN_BLOCK_KEYS: [[u8; 4]; 3] = [*b"Patt", *b"Pat2", *b"Pat3"];

/// The most patterns one file may define; the rest are refused by count.
pub const MAX_PATTERNS: usize = 1_024;

/// One pattern the file defines, decoded to straight-alpha RGBA8.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PsdPattern {
    pub name: String,
    /// The unique id a `Ptrn` reference names it by.
    pub id: String,
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, row-major.
    pub rgba8: Vec<u8>,
}

impl PsdPattern {
    /// The pattern as the W7-B effect's tile.
    pub fn tile(&self) -> Option<PatternTile> {
        PatternTile::new(
            self.name.clone(),
            self.width,
            self.height,
            self.rgba8.clone(),
        )
        .ok()
    }
}

/// Every pattern a file defines, and every one it defines but this reader
/// refused (with why), in file order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PatternLibrary {
    pub patterns: Vec<PsdPattern>,
    pub refused: Vec<String>,
}

impl PatternLibrary {
    /// Read the patterns out of `file`'s document-level `Patt`/`Pat2`/`Pat3`
    /// blocks. Never fails: a damaged pattern is named in
    /// [`PatternLibrary::refused`] and the rest still load.
    pub fn read(file: &PsdFile, opts: &ReadOptions) -> Self {
        let mut library = PatternLibrary::default();
        let mut budget = Budget::new(opts.max_decoded_bytes);
        for block in &file.extra {
            if PATTERN_BLOCK_KEYS.contains(&block.key) {
                library.read_block(&block.data, opts, &mut budget);
            }
        }
        library
    }

    /// Read one block's run of patterns into the library.
    pub fn read_block(&mut self, data: &[u8], opts: &ReadOptions, budget: &mut Budget) {
        let mut cur = Cursor::new(data);
        while cur.remaining() >= 4 {
            if self.patterns.len() + self.refused.len() >= MAX_PATTERNS {
                self.refused
                    .push(format!("patterns past the first {MAX_PATTERNS}"));
                return;
            }
            let Ok(len) = cur.u32() else { return };
            let Ok(mut one) = cur.sub(len as usize) else {
                self.refused
                    .push("a pattern whose length runs past its block".to_string());
                return;
            };
            match read_pattern(&mut one, opts, budget) {
                Ok(pattern) => self.patterns.push(pattern),
                Err(e) => self
                    .refused
                    .push(format!("a pattern could not be read: {e}")),
            }
            if cur.align_to(4).is_err() {
                return;
            }
        }
    }

    /// The pattern a `Ptrn` reference names: by unique id first, then by
    /// name (older writers leave the id empty).
    pub fn find(&self, reference: &PatternRef) -> Option<&PsdPattern> {
        if !reference.id.is_empty() {
            if let Some(p) = self.patterns.iter().find(|p| p.id == reference.id) {
                return Some(p);
            }
        }
        (!reference.name.is_empty())
            .then(|| self.patterns.iter().find(|p| p.name == reference.name))
            .flatten()
    }
}

/// A `Ptrn` object: the pattern a fill or an overlay draws.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PatternRef {
    pub name: String,
    pub id: String,
}

impl PatternRef {
    /// The `Ptrn` object of a pattern fill or overlay descriptor.
    pub fn of(d: &Descriptor) -> Option<Self> {
        let p = d.descriptor("Ptrn")?;
        let r = PatternRef {
            name: p.text("Nm  ").unwrap_or_default().to_string(),
            id: p.text("Idnt").unwrap_or_default().to_string(),
        };
        (!r.name.is_empty() || !r.id.is_empty()).then_some(r)
    }
}

/// Decode one pattern (the bytes after its length word).
fn read_pattern(
    cur: &mut Cursor<'_>,
    opts: &ReadOptions,
    budget: &mut Budget,
) -> PsdResult<PsdPattern> {
    let version = cur.u32()?;
    if version != 1 {
        return Err(PsdError::InvalidDocument(format!(
            "pattern version {version}"
        )));
    }
    let mode = cur.u32()?;
    let colours = match mode {
        1 => 1usize,
        3 => 3,
        2 => {
            return Err(PsdError::InvalidDocument(
                "an indexed-colour pattern".to_string(),
            ))
        }
        other => {
            return Err(PsdError::InvalidDocument(format!(
                "a pattern in image mode {other}"
            )))
        }
    };
    let _height = cur.u16()?;
    let _width = cur.u16()?;
    let name = cur.unicode_string(opts.max_name_units)?;
    let id = cur.pascal_string(1)?;

    // The virtual-memory array list.
    let list_version = cur.u32()?;
    if list_version != 3 {
        return Err(PsdError::InvalidDocument(format!(
            "pattern pixel list version {list_version}"
        )));
    }
    let list_len = cur.u32()? as usize;
    let mut list = cur.sub(list_len)?;
    let (width, height) = rect_size(&mut list)?;
    let max_edge = opts.max_dimension.min(MAX_PATTERN_EDGE);
    if width == 0 || height == 0 {
        return Err(PsdError::EmptyCanvas { width, height });
    }
    if width > max_edge || height > max_edge {
        return Err(PsdError::LimitExceeded {
            what: "pattern edge",
            value: u64::from(width.max(height)),
            max: u64::from(max_edge),
        });
    }
    let pixels = u64::from(width) * u64::from(height);
    if pixels > MAX_PATTERN_PIXELS {
        return Err(PsdError::LimitExceeded {
            what: "pattern pixels",
            value: pixels,
            max: MAX_PATTERN_PIXELS,
        });
    }
    let declared = list.u32()? as usize;
    if declared < colours || declared > opts.max_channels_per_layer {
        return Err(PsdError::LimitExceeded {
            what: "pattern channel count",
            value: declared as u64,
            max: opts.max_channels_per_layer as u64,
        });
    }
    let plane = pixels as usize;
    // Every plane this pattern keeps (colours + transparency) plus the RGBA
    // it becomes, drawn from the one budget before any of it is allocated.
    budget.take(pixels * (colours as u64 + 1) + pixels * 4)?;
    let mut colour: Vec<Vec<u8>> = Vec::with_capacity(colours);
    let mut alpha: Option<Vec<u8>> = None;
    for index in 0..declared + 2 {
        let keep = index < colours || index == declared;
        let data = read_array(&mut list, width, height, keep)?;
        if index < colours {
            colour.push(data.ok_or_else(|| {
                PsdError::InvalidDocument(format!("pattern colour channel {index} is absent"))
            })?);
        } else if index == declared {
            alpha = data;
        }
    }
    let mut rgba8 = Vec::with_capacity(plane * 4);
    for i in 0..plane {
        let (r, g, b) = if colours == 3 {
            (colour[0][i], colour[1][i], colour[2][i])
        } else {
            (colour[0][i], colour[0][i], colour[0][i])
        };
        let a = alpha.as_ref().map_or(255, |a| a[i]);
        rgba8.extend_from_slice(&[r, g, b, a]);
    }
    Ok(PsdPattern {
        name,
        id,
        width,
        height,
        rgba8,
    })
}

/// A `top, left, bottom, right` rectangle's width and height.
fn rect_size(cur: &mut Cursor<'_>) -> PsdResult<(u32, u32)> {
    let top = cur.i32()?;
    let left = cur.i32()?;
    let bottom = cur.i32()?;
    let right = cur.i32()?;
    let bad = || PsdError::BadRect {
        top,
        left,
        bottom,
        right,
    };
    let height = u32::try_from(i64::from(bottom) - i64::from(top)).map_err(|_| bad())?;
    let width = u32::try_from(i64::from(right) - i64::from(left)).map_err(|_| bad())?;
    Ok((width, height))
}

/// One array of the list: `None` when it is not written. Its plane is
/// decoded only when `keep` (the others are skipped whole).
fn read_array(
    list: &mut Cursor<'_>,
    width: u32,
    height: u32,
    keep: bool,
) -> PsdResult<Option<Vec<u8>>> {
    if list.u32()? == 0 {
        return Ok(None);
    }
    let len = list.u32()? as usize;
    if len == 0 {
        return Ok(None);
    }
    let mut array = list.sub(len)?;
    if !keep {
        return Ok(None);
    }
    let depth = array.u32()?;
    let (w, h) = rect_size(&mut array)?;
    let _depth16 = array.u16()?;
    let compression = array.u8()?;
    if depth != 8 {
        return Err(PsdError::UnsupportedDepth(
            depth.min(u32::from(u16::MAX)) as u16
        ));
    }
    if (w, h) != (width, height) {
        return Err(PsdError::ChannelSizeMismatch {
            what: "pattern channel",
            expected: width as usize * height as usize,
            actual: w as usize * h as usize,
        });
    }
    let (w, h) = (w as usize, h as usize);
    let plane = match compression {
        0 => array.take(w * h)?.to_vec(),
        1 => {
            let mut counts = Vec::with_capacity(h);
            for _ in 0..h {
                counts.push(array.u16()? as usize);
            }
            let mut plane = Vec::with_capacity(w * h);
            for (row, count) in counts.into_iter().enumerate() {
                let packed = array.take(count)?;
                plane.extend(crate::packbits::decode_exact(packed, w, row)?);
            }
            plane
        }
        other => return Err(PsdError::UnsupportedCompression(u16::from(other))),
    };
    Ok(Some(plane))
}

/// Write `patterns` as one `Patt` block's payload (RGB, RLE rows, the
/// transparency in the user mask) — the layout [`PatternLibrary::read_block`]
/// reads, for a writer that wants to carry patterns and for fixtures.
pub fn encode_block(patterns: &[PsdPattern]) -> Vec<u8> {
    let mut sink = Sink::new();
    for p in patterns {
        let slot = sink.begin_len();
        sink.u32(1);
        sink.u32(3);
        sink.u16(p.height.min(u32::from(u16::MAX)) as u16);
        sink.u16(p.width.min(u32::from(u16::MAX)) as u16);
        sink.unicode_string(&p.name);
        sink.pascal_string(&p.id, 1);
        sink.u32(3);
        let list = sink.begin_len();
        let rect = |sink: &mut Sink| {
            sink.i32(0);
            sink.i32(0);
            sink.i32(p.height as i32);
            sink.i32(p.width as i32);
        };
        rect(&mut sink);
        sink.u32(3);
        for channel in 0..5usize {
            if channel == 4 {
                sink.u32(0); // the sheet mask: not written
                continue;
            }
            sink.u32(1);
            let array = sink.begin_len();
            sink.u32(8);
            rect(&mut sink);
            sink.u16(8);
            sink.u8(1);
            let plane: Vec<u8> = p.rgba8.iter().skip(channel).step_by(4).copied().collect();
            let rows: Vec<Vec<u8>> = plane
                .chunks(p.width.max(1) as usize)
                .map(crate::packbits::encode)
                .collect();
            for row in &rows {
                sink.u16(row.len() as u16);
            }
            for row in &rows {
                sink.bytes(row);
            }
            sink.end_len(array);
        }
        sink.end_len(list);
        sink.end_len(slot);
        sink.align_to(4);
    }
    sink.into_inner()
}

/// The placement a pattern fill or overlay descriptor gives its pattern.
fn placement(d: &Descriptor, tile: PatternTile) -> PatternFill {
    let scale = match d.get("Scl ") {
        Some(Value::UnitFloat { value, .. }) | Some(Value::Double(value)) => *value / 100.0,
        _ => 1.0,
    };
    let offset = d
        .descriptor("phase")
        .map(|p| {
            [
                p.number("Hrzn").unwrap_or(0.0) as f32,
                p.number("Vrtc").unwrap_or(0.0) as f32,
            ]
        })
        .unwrap_or([0.0, 0.0]);
    let angle = d.number("Angl").unwrap_or(0.0) as f32;
    let link = match d.get("Algn") {
        Some(Value::Bool(b)) => *b,
        _ => true,
    };
    PatternFill {
        asset: None,
        tile: Some(tile),
        scale: if scale.is_finite() && scale > 0.0 {
            scale as f32
        } else {
            1.0
        },
        offset_px: offset.map(|v| if v.is_finite() { v } else { 0.0 }),
        angle_deg: if angle.is_finite() { angle } else { 0.0 },
        link_with_layer: link,
    }
}

/// A pattern fill layer's (`PtFl`) fill, resolved against `library`; `None`
/// when the adjustment is not a pattern fill or names a pattern the file does
/// not carry (or carries in a form this reader refused).
pub fn pattern_fill_layer(
    adjustment: &Adjustment,
    opts: &ReadOptions,
    library: &PatternLibrary,
) -> Option<PatternFill> {
    if adjustment.key != *b"PtFl" {
        return None;
    }
    let d = adjustment.descriptor(opts)?;
    let tile = library.find(&PatternRef::of(&d)?)?.tile()?;
    Some(placement(&d, tile))
}

/// The pattern overlay of an `lfx2` block, resolved against `library`;
/// `None` when there is none switched on, or its pattern cannot be resolved.
pub fn pattern_overlay(
    effects: &Effects,
    opts: &ReadOptions,
    library: &PatternLibrary,
) -> Option<PatternOverlayEffect> {
    let descriptor = effects.descriptor(opts)?;
    descriptor.items.iter().find_map(|(key, value)| {
        let Value::Descriptor(effect) = value else {
            return None;
        };
        if !matches!(key.as_str(), "patternFill" | "PtFl" | "PttR")
            || matches!(effect.get("enab"), Some(Value::Bool(false)))
        {
            return None;
        }
        let tile = library.find(&PatternRef::of(effect)?)?.tile()?;
        let blend_mode = match effect.get("Md  ") {
            Some(Value::Enumerated { value, .. }) => {
                crate::effects::blend_from_blnm(value).unwrap_or(BlendMode::Normal)
            }
            _ => BlendMode::Normal,
        };
        let opacity = match effect.get("Opct").or_else(|| effect.get("opacity")) {
            Some(Value::UnitFloat { value, .. }) => (*value / 100.0) as f32,
            _ => 1.0,
        };
        Some(PatternOverlayEffect {
            blend_mode,
            opacity: if opacity.is_finite() {
                opacity.clamp(0.0, 1.0)
            } else {
                1.0
            },
            pattern: placement(effect, tile),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2x2 RGB pattern with a transparent pixel, written byte by byte from
    /// the layout in the module docs (raw rows, not this module's encoder).
    fn hand_built_block() -> Vec<u8> {
        let mut b: Vec<u8> = Vec::new();
        let mut body: Vec<u8> = Vec::new();
        body.extend(1u32.to_be_bytes()); // version
        body.extend(3u32.to_be_bytes()); // RGB
        body.extend(2u16.to_be_bytes()); // height
        body.extend(2u16.to_be_bytes()); // width
        body.extend(2u32.to_be_bytes()); // name: 2 UTF-16 units
        body.extend([0, b'P', 0, b'1']);
        body.extend([3, b'a', b'b', b'c']); // id, Pascal
        body.extend(3u32.to_be_bytes()); // list version
        let mut list: Vec<u8> = Vec::new();
        for v in [0i32, 0, 2, 2] {
            list.extend(v.to_be_bytes());
        }
        list.extend(3u32.to_be_bytes()); // channels
        let planes: [[u8; 4]; 4] = [
            [255, 0, 10, 20],   // R
            [0, 255, 10, 20],   // G
            [0, 0, 10, 20],     // B
            [255, 255, 0, 128], // user mask
        ];
        for plane in planes {
            list.extend(1u32.to_be_bytes()); // written
            let mut array: Vec<u8> = Vec::new();
            array.extend(8u32.to_be_bytes());
            for v in [0i32, 0, 2, 2] {
                array.extend(v.to_be_bytes());
            }
            array.extend(8u16.to_be_bytes());
            array.push(0); // raw
            array.extend(plane);
            list.extend((array.len() as u32).to_be_bytes());
            list.extend(array);
        }
        list.extend(0u32.to_be_bytes()); // sheet mask: not written
        body.extend((list.len() as u32).to_be_bytes());
        body.extend(list);
        b.extend((body.len() as u32).to_be_bytes());
        b.extend(body);
        while !b.len().is_multiple_of(4) {
            b.push(0);
        }
        b
    }

    #[test]
    fn a_hand_built_pattern_block_decodes_to_its_rgba() {
        let mut library = PatternLibrary::default();
        let opts = ReadOptions::default();
        library.read_block(
            &hand_built_block(),
            &opts,
            &mut Budget::new(opts.max_decoded_bytes),
        );
        assert!(library.refused.is_empty(), "{:?}", library.refused);
        assert_eq!(library.patterns.len(), 1);
        let p = &library.patterns[0];
        assert_eq!((p.name.as_str(), p.id.as_str()), ("P1", "abc"));
        assert_eq!((p.width, p.height), (2, 2));
        assert_eq!(
            p.rgba8,
            vec![255, 0, 0, 255, 0, 255, 0, 255, 10, 10, 10, 0, 20, 20, 20, 128]
        );
        // The encoder writes what the reader reads.
        let mut again = PatternLibrary::default();
        again.read_block(
            &encode_block(&library.patterns),
            &opts,
            &mut Budget::new(opts.max_decoded_bytes),
        );
        assert_eq!(again.patterns, library.patterns);
    }

    /// Untrusted input: a pattern that lies about its length, one over the
    /// edge limit and one over the decode budget are each refused by name,
    /// and a good pattern after a refused one still loads.
    #[test]
    fn damaged_or_oversized_patterns_are_refused_and_the_rest_still_load() {
        let opts = ReadOptions::default();
        let good = hand_built_block();

        // Truncated: the length claims more than the block holds.
        let mut truncated = good.clone();
        truncated.truncate(good.len() - 8);
        let mut library = PatternLibrary::default();
        library.read_block(&truncated, &opts, &mut Budget::new(opts.max_decoded_bytes));
        assert!(library.patterns.is_empty());
        assert_eq!(library.refused.len(), 1, "{:?}", library.refused);

        // An edge past the limit, then the good one.
        let small = ReadOptions {
            max_dimension: 1,
            ..ReadOptions::default()
        };
        let mut library = PatternLibrary::default();
        library.read_block(&good, &small, &mut Budget::new(opts.max_decoded_bytes));
        assert!(library.patterns.is_empty());
        assert!(
            library.refused[0].contains("pattern edge"),
            "{:?}",
            library.refused
        );

        // The budget: a pattern needs more than is left.
        let mut library = PatternLibrary::default();
        library.read_block(&good, &opts, &mut Budget::new(8));
        assert!(library.patterns.is_empty());
        assert_eq!(library.refused.len(), 1);

        // A version this reader does not know, then a good pattern: the good
        // one still loads.
        let mut bad_then_good = good.clone();
        bad_then_good[7] = 9; // version word of the first pattern
        bad_then_good.extend(&good);
        let mut library = PatternLibrary::default();
        library.read_block(
            &bad_then_good,
            &opts,
            &mut Budget::new(opts.max_decoded_bytes),
        );
        assert_eq!(library.patterns.len(), 1);
        assert_eq!(library.refused.len(), 1);
    }
}
