//! W9-M: placed (smart-object) layers and the embedded files they show.
//!
//! A smart-object layer is two things in a `.psd`:
//!
//! * a layer-level `SoLd` block (or the older `PlLd`) naming the placed file
//!   by a unique id and giving the four canvas corners its source rectangle
//!   is mapped onto, and
//! * a document-level `lnk2` block (older files: `lnk3`, `lnkD`) holding the
//!   placed files themselves, each keyed by that id.
//!
//! The layer's own channels still carry the rendered appearance, so a reader
//! that knows nothing of either block shows the right picture.
//!
//! # The layouts (as implemented)
//!
//! `SoLd`: the key `soLD`, a `u32` version (4), then a descriptor block (a
//! `u32` descriptor version, 16, and one descriptor) whose `Idnt` is the id,
//! `Trnf` the eight corner coordinates (top-left, top-right, bottom-right,
//! bottom-left, each `x, y`) and `Sz  ` the source size.
//!
//! `PlLd`: the key `plcL`, a `u32` version (3), the id as a Pascal string,
//! the page number, page count, anti-alias policy and layer type (four
//! `u32`s), the eight corner doubles, then the warp as a two-version
//! descriptor block. The source size is not in the fixed part; it is taken
//! from the linked file when it can be decoded.
//!
//! `lnk2` and friends: a run of entries, each a `u64` length, the entry, and
//! padding to a four-byte boundary. An entry is a type (`liFD` embedded data,
//! `liFE` external file, `liFA` alias), a `u32` version (1..=7), the id as a
//! Pascal string, the original file name as a Unicode string, the file type
//! and creator (`4` bytes each), a `u64` data length, a flag byte announcing
//! an optional "file open" descriptor block, then — for `liFD` — the file's
//! bytes, followed by version-dependent trailers (a child document id from
//! 5, a modification time from 6, a lock byte from 7).
//!
//! # Untrusted input
//!
//! Every entry is carved into a sub-cursor from its own declared length, so a
//! lying entry damages only itself and is named in [`LinkedFiles::refused`].
//! The entry count is capped ([`MAX_LINKED_FILES`]), every string is bounded
//! by [`ReadOptions::max_name_units`], and every copied payload is drawn from
//! one [`Budget`] of [`ReadOptions::max_decoded_bytes`] before it is copied.
//! Descriptors go through [`Descriptor::read`], which is depth- and
//! count-limited. Nothing here indexes: every read is a [`Cursor`] call.

use crate::bytes::{Cursor, Sink};
use crate::descriptor::{Descriptor, Value};
use crate::error::{PsdError, PsdResult};
use crate::limits::{Budget, ReadOptions};
use crate::model::{PsdFile, PsdLayer, TaggedBlock};

/// The document-level keys that carry placed files.
pub const LINKED_FILE_KEYS: [[u8; 4]; 3] = [*b"lnk2", *b"lnk3", *b"lnkD"];

/// The layer-level keys that make a layer a placed (smart-object) layer.
pub const PLACED_LAYER_KEYS: [[u8; 4]; 3] = [*b"SoLd", *b"SoLE", *b"PlLd"];

/// The most placed files one document may carry; the rest are refused.
pub const MAX_LINKED_FILES: usize = 4_096;

/// One placed file a document embeds (`liFD`) or names (`liFE`/`liFA`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedFile {
    /// The id a layer's `SoLd`/`PlLd` names it by.
    pub id: String,
    /// The file name it was placed from.
    pub name: String,
    /// Four-character file type, e.g. `png `.
    pub file_type: [u8; 4],
    pub creator: [u8; 4],
    /// The file's bytes. Empty for an external or alias entry, whose bytes
    /// live somewhere this reader does not follow.
    pub data: Vec<u8>,
    /// `true` for `liFD`: [`LinkedFile::data`] is the whole file.
    pub embedded: bool,
}

impl LinkedFile {
    /// An embedded file, its type taken from its first bytes.
    pub fn embedded(id: impl Into<String>, name: impl Into<String>, data: Vec<u8>) -> Self {
        LinkedFile {
            id: id.into(),
            name: name.into(),
            file_type: file_type_of(&data),
            creator: *b"8BIM",
            data,
            embedded: true,
        }
    }
}

/// The four-character type Photoshop records for a file with these bytes.
pub fn file_type_of(data: &[u8]) -> [u8; 4] {
    if data.starts_with(b"\x89PNG") {
        *b"png "
    } else if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        *b"JPEG"
    } else if data.starts_with(b"8BPS") {
        *b"8BPS"
    } else if data.starts_with(b"II*\0") || data.starts_with(b"MM\0*") {
        *b"TIFF"
    } else {
        *b"    "
    }
}

/// Every placed file a document carries, and the entries this reader refused
/// (with why), in file order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkedFiles {
    pub files: Vec<LinkedFile>,
    pub refused: Vec<String>,
}

impl LinkedFiles {
    /// Read `file`'s document-level `lnk2`/`lnk3`/`lnkD` blocks. Never fails:
    /// a damaged entry is named in [`LinkedFiles::refused`] and the rest load.
    pub fn read(file: &PsdFile, opts: &ReadOptions) -> Self {
        let mut out = LinkedFiles::default();
        let mut budget = Budget::new(opts.max_decoded_bytes);
        for block in &file.extra {
            if LINKED_FILE_KEYS.contains(&block.key) {
                out.read_block(&block.data, opts, &mut budget);
            }
        }
        out
    }

    /// Read one block's run of entries.
    pub fn read_block(&mut self, data: &[u8], opts: &ReadOptions, budget: &mut Budget) {
        let mut cur = Cursor::new(data);
        while cur.remaining() >= 8 {
            if self.files.len() + self.refused.len() >= MAX_LINKED_FILES {
                self.refused
                    .push(format!("placed files past the first {MAX_LINKED_FILES}"));
                return;
            }
            let Ok(len) = read_u64(&mut cur) else { return };
            let Some(len) = usize::try_from(len).ok().filter(|l| *l <= cur.remaining()) else {
                self.refused
                    .push("a placed file whose length runs past its block".to_string());
                return;
            };
            let Ok(mut one) = cur.sub(len) else { return };
            match read_entry(&mut one, opts, budget) {
                Ok(file) => self.files.push(file),
                Err(e) => self
                    .refused
                    .push(format!("a placed file could not be read: {e}")),
            }
            if cur.align_to(4).is_err() {
                return;
            }
        }
    }

    /// The file a placed layer names, by id.
    pub fn find(&self, id: &str) -> Option<&LinkedFile> {
        self.files.iter().find(|f| f.id == id)
    }
}

fn read_u64(cur: &mut Cursor<'_>) -> PsdResult<u64> {
    let hi = cur.u32()?;
    let lo = cur.u32()?;
    Ok((u64::from(hi) << 32) | u64::from(lo))
}

fn write_u64(sink: &mut Sink, v: u64) {
    sink.u32((v >> 32) as u32);
    sink.u32(v as u32);
}

fn read_entry(
    cur: &mut Cursor<'_>,
    opts: &ReadOptions,
    budget: &mut Budget,
) -> PsdResult<LinkedFile> {
    let kind = cur.tag()?;
    let version = cur.u32()?;
    if !(1..=8).contains(&version) {
        return Err(PsdError::InvalidDocument(format!(
            "placed-file entry version {version}"
        )));
    }
    let id = cur.pascal_string(1)?;
    let name = cur.unicode_string(opts.max_name_units)?;
    let file_type = cur.tag()?;
    let creator = cur.tag()?;
    let len = read_u64(cur)?;
    if cur.u8()? != 0 {
        // The optional "file open" descriptor: parsed (bounded) and dropped.
        let _version = cur.u32()?;
        Descriptor::read(cur, opts)?;
    }
    let embedded = kind == *b"liFD";
    let data = if embedded {
        let len = usize::try_from(len)
            .ok()
            .filter(|l| *l <= cur.remaining())
            .ok_or(PsdError::Truncated {
                needed: len.min(usize::MAX as u64) as usize,
                available: cur.remaining(),
                at: cur.offset(),
            })?;
        budget.take(len as u64)?;
        cur.take(len)?.to_vec()
    } else if kind == *b"liFE" || kind == *b"liFA" {
        Vec::new()
    } else {
        return Err(PsdError::InvalidDocument(format!(
            "placed-file entry type {}",
            crate::error::tag_name(kind)
        )));
    };
    Ok(LinkedFile {
        id,
        name,
        file_type,
        creator,
        data,
        embedded,
    })
}

/// Encode embedded files as one `lnk2` payload (version-7 `liFD` entries) —
/// the layout [`LinkedFiles::read_block`] reads. External entries are not
/// written: a caller that has no bytes has nothing to embed.
pub fn encode_linked_files(files: &[LinkedFile]) -> Vec<u8> {
    let mut sink = Sink::new();
    for file in files.iter().filter(|f| f.embedded) {
        let mut entry = Sink::new();
        entry.tag(b"liFD");
        entry.u32(7);
        entry.pascal_string(&file.id, 1);
        entry.unicode_string(&file.name);
        entry.tag(&file.file_type);
        entry.tag(&file.creator);
        write_u64(&mut entry, file.data.len() as u64);
        entry.u8(0); // no file-open descriptor
        entry.bytes(&file.data);
        entry.unicode_string(""); // version 5: child document id
        entry.f64(0.0); // version 6: asset modification time
        entry.u8(0); // version 7: unlocked
        let body = entry.into_inner();
        write_u64(&mut sink, body.len() as u64);
        sink.bytes(&body);
        sink.align_to(4);
    }
    sink.into_inner()
}

/// A placed layer's reference to its file and where the file lands.
#[derive(Debug, Clone, PartialEq)]
pub struct PlacedLayer {
    /// The [`LinkedFile::id`] this layer shows.
    pub id: String,
    /// Canvas corners of the source rectangle: top-left, top-right,
    /// bottom-right, bottom-left, each `x, y`.
    pub corners: [f64; 8],
    /// The source size in pixels, when the block records it (`SoLd` does,
    /// `PlLd` does not).
    pub size: Option<(f64, f64)>,
}

impl PlacedLayer {
    /// The placed-layer data of `layer`, from its `SoLd` (preferred) or
    /// `PlLd` block. `None` when there is neither, or the block does not
    /// parse.
    pub fn of(layer: &PsdLayer, opts: &ReadOptions) -> Option<Self> {
        let find = |key: &[u8; 4]| layer.extra.iter().find(|b| &b.key == key);
        find(b"SoLd")
            .or_else(|| find(b"SoLE"))
            .and_then(|b| parse_sold(&b.data, opts))
            .or_else(|| find(b"PlLd").and_then(|b| parse_plld(&b.data, opts)))
    }

    /// `true` when `layer` carries a placed-layer block at all, parseable or
    /// not.
    pub fn is_placed(layer: &PsdLayer) -> bool {
        layer
            .extra
            .iter()
            .any(|b| PLACED_LAYER_KEYS.contains(&b.key))
    }

    /// The layer-level `SoLd` block for this placement.
    pub fn to_block(&self) -> TaggedBlock {
        let (w, h) = self.size.unwrap_or((0.0, 0.0));
        let corners = || Value::List(self.corners.iter().map(|v| Value::Double(*v)).collect());
        let mut d = Descriptor::new("null");
        let mut push = |k: &str, v: Value| {
            let _ = d.push(k, v);
        };
        push("Idnt", Value::Text(self.id.clone()));
        push("placed", Value::Text(self.id.clone()));
        push("PgNm", Value::Integer(1));
        push("totalPages", Value::Integer(1));
        push("Annt", Value::Integer(16));
        push("Type", Value::Integer(2)); // raster
        push("Trnf", corners());
        push("nonAffineTransform", corners());
        let mut size = Descriptor::new("Pnt ");
        let _ = size.push("Wdth", Value::Double(w));
        let _ = size.push("Hght", Value::Double(h));
        push("Sz  ", Value::Descriptor(size));
        push(
            "Rslt",
            Value::UnitFloat {
                unit: *b"#Rsl",
                value: 72.0,
            },
        );
        push("comp", Value::Integer(-1));
        let mut s = Sink::new();
        s.tag(b"soLD");
        s.u32(4);
        s.u32(16);
        // Every key above is non-empty, which is the only thing `write`
        // refuses.
        let _ = d.write(&mut s);
        TaggedBlock::new(*b"SoLd", s.into_inner())
    }

    /// The legacy layer-level `PlLd` block for this placement, which older
    /// readers (and psd-tools' `transform_box`) take the corners from.
    /// Photoshop writes it beside `SoLd`.
    pub fn to_legacy_block(&self) -> TaggedBlock {
        let (w, h) = self.size.unwrap_or((0.0, 0.0));
        let px = |v: f64| Value::UnitFloat {
            unit: *b"#Pxl",
            value: v,
        };
        let mut bounds = Descriptor::new("Rctn");
        let _ = bounds.push("Top ", px(0.0));
        let _ = bounds.push("Left", px(0.0));
        let _ = bounds.push("Btom", px(h));
        let _ = bounds.push("Rght", px(w));
        let mut warp = Descriptor::new("warp");
        let _ = warp.push(
            "warpStyle",
            Value::Enumerated {
                type_id: "warpStyle".into(),
                value: "warpNone".into(),
            },
        );
        let _ = warp.push("warpValue", Value::Double(0.0));
        let _ = warp.push("warpPerspective", Value::Double(0.0));
        let _ = warp.push("warpPerspectiveOther", Value::Double(0.0));
        let _ = warp.push(
            "warpRotate",
            Value::Enumerated {
                type_id: "Ornt".into(),
                value: "Hrzn".into(),
            },
        );
        let _ = warp.push("bounds", Value::Descriptor(bounds));
        let _ = warp.push("uOrder", Value::Integer(4));
        let _ = warp.push("vOrder", Value::Integer(4));
        let mut s = Sink::new();
        s.tag(b"plcL");
        s.u32(3);
        s.pascal_string(&self.id, 1);
        s.u32(1); // page
        s.u32(1); // page count
        s.u32(16); // anti-alias policy
        s.u32(2); // layer type: raster
        for v in self.corners {
            s.f64(v);
        }
        s.u32(0); // warp version
        s.u32(16); // descriptor version
        let _ = warp.write(&mut s);
        TaggedBlock::new(*b"PlLd", s.into_inner())
    }
}

fn corners_of(d: &Descriptor) -> Option<[f64; 8]> {
    let Some(Value::List(items)) = d.get("Trnf") else {
        return None;
    };
    let mut out = [0.0f64; 8];
    if items.len() != 8 {
        return None;
    }
    for (slot, item) in out.iter_mut().zip(items) {
        *slot = match item {
            Value::Double(v) => *v,
            Value::UnitFloat { value, .. } => *value,
            Value::Integer(v) => f64::from(*v),
            _ => return None,
        };
        if !slot.is_finite() {
            return None;
        }
    }
    Some(out)
}

fn parse_sold(data: &[u8], opts: &ReadOptions) -> Option<PlacedLayer> {
    let mut cur = Cursor::new(data);
    let _key = cur.tag().ok()?;
    let _version = cur.u32().ok()?;
    let _descriptor_version = cur.u32().ok()?;
    let d = Descriptor::read(&mut cur, opts).ok()?;
    let id = d.text("Idnt")?.trim_end_matches('\0').to_string();
    let corners = corners_of(&d)?;
    let size = d.descriptor("Sz  ").and_then(|s| {
        let (w, h) = (s.number("Wdth")?, s.number("Hght")?);
        (w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0).then_some((w, h))
    });
    Some(PlacedLayer { id, corners, size })
}

fn parse_plld(data: &[u8], _opts: &ReadOptions) -> Option<PlacedLayer> {
    let mut cur = Cursor::new(data);
    let _key = cur.tag().ok()?;
    let _version = cur.u32().ok()?;
    let id = cur.pascal_string(1).ok()?;
    for _ in 0..4 {
        cur.u32().ok()?;
    }
    let mut corners = [0.0f64; 8];
    for slot in &mut corners {
        *slot = cur.f64().ok()?;
        if !slot.is_finite() {
            return None;
        }
    }
    Some(PlacedLayer {
        id,
        corners,
        size: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::PsdHeader;
    use crate::model::Rect;

    fn png_like(n: usize) -> Vec<u8> {
        let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
        v.extend((0..n).map(|i| i as u8));
        v
    }

    #[test]
    fn linked_files_round_trip_and_pad_every_entry_to_four_bytes() {
        let files = vec![
            LinkedFile::embedded("id-one", "logo.png", png_like(5)),
            LinkedFile::embedded("id-two", "photo.png", png_like(12)),
        ];
        let block = encode_linked_files(&files);
        assert_eq!(
            block.len() % 4,
            0,
            "psd-tools reads global blocks padded to 4"
        );
        let mut back = LinkedFiles::default();
        let opts = ReadOptions::default();
        back.read_block(&block, &opts, &mut Budget::new(opts.max_decoded_bytes));
        assert!(back.refused.is_empty(), "{:?}", back.refused);
        assert_eq!(back.files, files);
        assert_eq!(back.files[0].file_type, *b"png ");
        assert_eq!(back.find("id-two").unwrap().data, png_like(12));
    }

    #[test]
    fn a_placed_layer_block_round_trips_and_the_layer_reads_as_placed() {
        let placed = PlacedLayer {
            id: "abc".into(),
            corners: [4.0, 4.0, 20.0, 4.0, 20.0, 20.0, 4.0, 20.0],
            size: Some((8.0, 8.0)),
        };
        let mut layer = PsdLayer::raster("SO", Rect::sized(1, 1));
        assert!(!PlacedLayer::is_placed(&layer));
        layer.extra.push(placed.to_block());
        assert!(PlacedLayer::is_placed(&layer));
        assert_eq!(
            PlacedLayer::of(&layer, &ReadOptions::default()),
            Some(placed.clone())
        );
        // The legacy block alone carries the id and the corners.
        let mut legacy = PsdLayer::raster("SO", Rect::sized(1, 1));
        legacy.extra.push(placed.to_legacy_block());
        let back = PlacedLayer::of(&legacy, &ReadOptions::default()).unwrap();
        assert_eq!((back.id, back.corners), (placed.id, placed.corners));
    }

    #[test]
    fn a_legacy_plld_block_is_read() {
        let mut s = Sink::new();
        s.tag(b"plcL");
        s.u32(3);
        s.pascal_string("legacy", 1);
        for v in [1u32, 1, 16, 2] {
            s.u32(v);
        }
        for v in [0.0, 0.0, 10.0, 0.0, 10.0, 5.0, 0.0, 5.0] {
            s.f64(v);
        }
        let mut layer = PsdLayer::raster("old", Rect::sized(1, 1));
        layer.extra.push(TaggedBlock::new(*b"PlLd", s.into_inner()));
        let p = PlacedLayer::of(&layer, &ReadOptions::default()).unwrap();
        assert_eq!(p.id, "legacy");
        assert_eq!(p.corners[4..6], [10.0, 5.0]);
        assert_eq!(p.size, None);
    }

    #[test]
    fn a_lying_or_oversized_entry_is_refused_and_the_rest_still_load() {
        let opts = ReadOptions::default();
        let good = encode_linked_files(&[LinkedFile::embedded("g", "g.png", png_like(3))]);

        // An entry whose u64 length claims more than the block holds.
        let mut lying = good.clone();
        lying[7] = 0xFF;
        let mut back = LinkedFiles::default();
        back.read_block(&lying, &opts, &mut Budget::new(opts.max_decoded_bytes));
        assert!(back.files.is_empty());
        assert_eq!(back.refused.len(), 1, "{:?}", back.refused);

        // A payload bigger than the budget is refused before it is copied;
        // a good entry after it still loads.
        let mut two = encode_linked_files(&[LinkedFile::embedded("big", "b.png", png_like(64))]);
        two.extend(&good);
        let mut back = LinkedFiles::default();
        back.read_block(&two, &opts, &mut Budget::new(40));
        assert_eq!(back.files.len(), 1, "{:?}", back);
        assert_eq!(back.files[0].id, "g");
        assert_eq!(back.refused.len(), 1);

        // Garbage type word.
        let mut bad = good.clone();
        bad[8..12].copy_from_slice(b"zzzz");
        let mut back = LinkedFiles::default();
        back.read_block(&bad, &opts, &mut Budget::new(opts.max_decoded_bytes));
        assert!(back.files.is_empty());
        assert_eq!(back.refused.len(), 1);
    }

    /// psd-tools reads document-level blocks as padded to four bytes past
    /// their LENGTH; a block two past a multiple of four written with only
    /// the even pad made it lose the `lnk2` block after it.
    #[test]
    fn document_level_blocks_are_padded_to_four_so_the_next_one_is_found() {
        let mut file = PsdFile::new(PsdHeader::rgba8(2, 2));
        file.extra
            .push(TaggedBlock::new(*b"zzzz", vec![1, 2, 3, 4, 5, 6]));
        file.extra.push(TaggedBlock::new(
            *b"lnk2",
            encode_linked_files(&[LinkedFile::embedded("p", "p.png", png_like(1))]),
        ));
        let bytes = crate::write(&file).unwrap();
        let at = |needle: &[u8]| {
            bytes
                .windows(needle.len())
                .position(|w| w == needle)
                .unwrap()
        };
        let data = at(b"8BIMzzzz") + 12;
        assert_eq!(
            at(b"8BIMlnk2"),
            data + 8,
            "six data bytes, then two bytes of padding"
        );
        let back = crate::read(&bytes).unwrap();
        assert!(LinkedFiles::read(&back, &ReadOptions::default())
            .find("p")
            .is_some());
    }

    #[test]
    fn a_whole_document_carries_placed_files_through_write_and_read() {
        let mut file = PsdFile::new(PsdHeader::rgba8(4, 4));
        let mut layer = PsdLayer::raster("SO", Rect::sized(4, 4));
        layer.set_rgba8(&[9u8; 64]).unwrap();
        layer.extra.push(
            PlacedLayer {
                id: "doc-id".into(),
                corners: [0.0, 0.0, 4.0, 0.0, 4.0, 4.0, 0.0, 4.0],
                size: Some((2.0, 2.0)),
            }
            .to_block(),
        );
        file.layers.push(layer);
        file.extra.push(TaggedBlock::new(
            *b"lnk2",
            encode_linked_files(&[LinkedFile::embedded("doc-id", "s.png", png_like(7))]),
        ));
        let back = crate::read(&crate::write(&file).unwrap()).unwrap();
        let placed = PlacedLayer::of(&back.layers[0], &ReadOptions::default()).unwrap();
        let files = LinkedFiles::read(&back, &ReadOptions::default());
        assert_eq!(files.find(&placed.id).unwrap().data, png_like(7));
    }
}
