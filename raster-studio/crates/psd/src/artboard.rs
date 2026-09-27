//! W18-B: Photoshop artboards — the `artb` / `artd` / `abdd` layer block.
//!
//! Photoshop marks an artboard as a group whose record carries one of three
//! tagged blocks (`artb` is the one it writes; `artd` and `abdd` are older and
//! alternate spellings of the same payload):
//!
//! ```text
//! u32 descriptor version (16)
//! descriptor "artboard" {
//!     artboardRect           Objc classFloatRect { Top  Left Btom Rght : doub }
//!     guideIndeces           VlLs (of long)
//!     artboardPresetName     TEXT
//!     Clr                    Objc RGBC { Rd  Grn  Bl  : doub, 0..=255 sRGB }
//!     artboardBackgroundType long   1 white, 2 black, 3 transparent, 4 Clr
//! }
//! ```
//!
//! The rect is in document pixels. Photoshop draws the background itself and
//! clips the group's contents to the rect; there is no background layer.
//!
//! Reading is bounded like every other descriptor ([`Descriptor::read`]:
//! depth and item ceilings) and never panics; a block that is not a readable
//! artboard is an error the caller can name, not a guess.

use crate::bytes::{Cursor, Sink};
use crate::descriptor::{Descriptor, Value};
use crate::error::{PsdError, PsdResult};
use crate::limits::ReadOptions;
use crate::model::{PsdLayer, TaggedBlock};

/// The three keys an artboard block is written under, `artb` first.
pub const KEYS: [[u8; 4]; 3] = [*b"artb", *b"artd", *b"abdd"];

/// What an artboard is filled with (`artboardBackgroundType`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ArtboardBackground {
    /// Type 1.
    White,
    /// Type 2.
    Black,
    /// Type 3.
    Transparent,
    /// Type 4: the `Clr ` colour, 8-bit sRGB channels (`0.0..=255.0`).
    Other([f64; 3]),
}

impl ArtboardBackground {
    /// The format's `artboardBackgroundType` number.
    pub fn code(self) -> i32 {
        match self {
            Self::White => 1,
            Self::Black => 2,
            Self::Transparent => 3,
            Self::Other(_) => 4,
        }
    }
}

/// One artboard as the `artb` block describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct PsdArtboard {
    /// Document-pixel edges.
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub background: ArtboardBackground,
    /// Photoshop's preset name ("iPhone 8", "Custom", ...); empty when none.
    pub preset_name: String,
}

impl PsdArtboard {
    /// `true` when every edge is finite and the rect has area.
    pub fn is_valid(&self) -> bool {
        [self.left, self.top, self.right, self.bottom]
            .iter()
            .all(|v| v.is_finite())
            && self.right > self.left
            && self.bottom > self.top
    }
}

fn invalid(what: &str) -> PsdError {
    PsdError::InvalidDocument(format!("artboard block: {what}"))
}

/// Parse an `artb` / `artd` / `abdd` payload.
pub fn parse(data: &[u8], opts: &ReadOptions) -> PsdResult<PsdArtboard> {
    let mut cur = Cursor::new(data);
    let version = cur.u32()?;
    if version != 16 {
        return Err(invalid(&format!("descriptor version {version}, not 16")));
    }
    let d = Descriptor::read(&mut cur, opts)?;
    let rect = d
        .descriptor("artboardRect")
        .ok_or_else(|| invalid("no artboardRect"))?;
    let edge = |key: &str| {
        rect.number(key)
            .filter(|v| v.is_finite())
            .ok_or_else(|| invalid(&format!("artboardRect has no finite {key:?}")))
    };
    let (top, left, bottom, right) = (edge("Top ")?, edge("Left")?, edge("Btom")?, edge("Rght")?);
    // Absent means white — Photopea and Photoshop both default to it.
    let kind = d.number("artboardBackgroundType").unwrap_or(1.0);
    let background = match kind as i64 {
        2 => ArtboardBackground::Black,
        3 => ArtboardBackground::Transparent,
        4 => {
            let clr = d.descriptor("Clr ");
            let ch = |key: &str| {
                clr.and_then(|c| c.number(key))
                    .filter(|v| v.is_finite())
                    .map_or(255.0, |v| v.clamp(0.0, 255.0))
            };
            ArtboardBackground::Other([ch("Rd  "), ch("Grn "), ch("Bl  ")])
        }
        // 1, and any number the format does not define.
        _ => ArtboardBackground::White,
    };
    Ok(PsdArtboard {
        left,
        top,
        right,
        bottom,
        background,
        preset_name: d.text("artboardPresetName").unwrap_or_default().to_string(),
    })
}

/// The `artb` payload for `board`.
pub fn build(board: &PsdArtboard) -> PsdResult<Vec<u8>> {
    if !board.is_valid() {
        return Err(invalid("the rect is empty or not finite"));
    }
    let mut rect = Descriptor::new("classFloatRect");
    rect.push("Top ", Value::Double(board.top))?;
    rect.push("Left", Value::Double(board.left))?;
    rect.push("Btom", Value::Double(board.bottom))?;
    rect.push("Rght", Value::Double(board.right))?;
    let [r, g, b] = match board.background {
        ArtboardBackground::Other(c) => c.map(|v| {
            if v.is_finite() {
                v.clamp(0.0, 255.0)
            } else {
                255.0
            }
        }),
        ArtboardBackground::Black => [0.0; 3],
        _ => [255.0; 3],
    };
    let mut clr = Descriptor::new("RGBC");
    clr.push("Rd  ", Value::Double(r))?;
    clr.push("Grn ", Value::Double(g))?;
    clr.push("Bl  ", Value::Double(b))?;
    let mut d = Descriptor::new("artboard");
    d.push("artboardRect", Value::Descriptor(rect))?;
    d.push("guideIndeces", Value::List(Vec::new()))?;
    d.push("artboardPresetName", Value::Text(board.preset_name.clone()))?;
    d.push("Clr ", Value::Descriptor(clr))?;
    d.push(
        "artboardBackgroundType",
        Value::Integer(board.background.code()),
    )?;
    let mut sink = Sink::new();
    sink.u32(16);
    d.write(&mut sink)?;
    Ok(sink.into_inner())
}

impl PsdLayer {
    /// W18-B: the artboard this (group) record is, from the first `artb`,
    /// `artd` or `abdd` block it carries. `None` when it carries none;
    /// `Some(Err)` when the block is there but unreadable.
    pub fn artboard(&self, opts: &ReadOptions) -> Option<PsdResult<PsdArtboard>> {
        KEYS.iter().find_map(|key| {
            self.extra
                .iter()
                .find(|b| &b.key == key)
                .map(|b| parse(&b.data, opts))
        })
    }

    /// W18-B: make this record the artboard `board` (an `artb` block,
    /// replacing any artboard block it had), or no artboard with `None`.
    pub fn set_artboard(&mut self, board: Option<&PsdArtboard>) -> PsdResult<()> {
        let data = board.map(build).transpose()?;
        self.extra.retain(|b| !KEYS.contains(&b.key));
        if let Some(data) = data {
            self.extra.push(TaggedBlock::new(*b"artb", data));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board(background: ArtboardBackground) -> PsdArtboard {
        PsdArtboard {
            left: 10.0,
            top: 20.0,
            right: 110.0,
            bottom: 70.0,
            background,
            preset_name: "Custom".into(),
        }
    }

    #[test]
    fn every_background_round_trips() {
        let opts = ReadOptions::default();
        for bg in [
            ArtboardBackground::White,
            ArtboardBackground::Black,
            ArtboardBackground::Transparent,
            ArtboardBackground::Other([12.0, 34.0, 56.0]),
        ] {
            let b = board(bg);
            assert_eq!(parse(&build(&b).unwrap(), &opts).unwrap(), b);
        }
    }

    /// The layout Photoshop writes, built key by key here rather than by
    /// [`build`]: four-character keys in their zero-length form, the long
    /// ones spelled out, `Clr ` present, background type 4.
    #[test]
    fn a_hand_built_photoshop_layout_artb_parses() {
        fn key(s: &mut Vec<u8>, k: &str) {
            if k.len() == 4 {
                s.extend_from_slice(&0u32.to_be_bytes());
            } else {
                s.extend_from_slice(&(k.len() as u32).to_be_bytes());
            }
            s.extend_from_slice(k.as_bytes());
        }
        fn doub(s: &mut Vec<u8>, k: &str, v: f64) {
            key(s, k);
            s.extend_from_slice(b"doub");
            s.extend_from_slice(&v.to_be_bytes());
        }
        fn class(s: &mut Vec<u8>, id: &str, count: u32) {
            s.extend_from_slice(&1u32.to_be_bytes()); // name: one unit
            s.extend_from_slice(&0u16.to_be_bytes()); // the NUL Photoshop writes
            key(s, id);
            s.extend_from_slice(&count.to_be_bytes());
        }
        let mut s = Vec::new();
        s.extend_from_slice(&16u32.to_be_bytes());
        class(&mut s, "artboard", 5);
        key(&mut s, "artboardRect");
        s.extend_from_slice(b"Objc");
        class(&mut s, "classFloatRect", 4);
        doub(&mut s, "Top ", 0.0);
        doub(&mut s, "Left", 1200.0);
        doub(&mut s, "Btom", 1334.0);
        doub(&mut s, "Rght", 1950.0);
        key(&mut s, "guideIndeces");
        s.extend_from_slice(b"VlLs");
        s.extend_from_slice(&0u32.to_be_bytes());
        key(&mut s, "artboardPresetName");
        s.extend_from_slice(b"TEXT");
        let name: Vec<u16> = "iPhone 8".encode_utf16().chain([0]).collect();
        s.extend_from_slice(&(name.len() as u32).to_be_bytes());
        for u in name {
            s.extend_from_slice(&u.to_be_bytes());
        }
        key(&mut s, "Clr ");
        s.extend_from_slice(b"Objc");
        class(&mut s, "RGBC", 3);
        doub(&mut s, "Rd  ", 255.0);
        doub(&mut s, "Grn ", 128.0);
        doub(&mut s, "Bl  ", 0.0);
        key(&mut s, "artboardBackgroundType");
        s.extend_from_slice(b"long");
        s.extend_from_slice(&4i32.to_be_bytes());

        let got = parse(&s, &ReadOptions::default()).unwrap();
        assert_eq!(
            (got.left, got.top, got.right, got.bottom),
            (1200.0, 0.0, 1950.0, 1334.0)
        );
        assert_eq!(
            got.background,
            ArtboardBackground::Other([255.0, 128.0, 0.0])
        );
        assert_eq!(got.preset_name.trim_end_matches('\0'), "iPhone 8");

        // Under any of the three keys, on a group record.
        for k in KEYS {
            let mut layer = PsdLayer::group("Artboard 1");
            layer.extra.push(TaggedBlock::new(k, s.clone()));
            let read = layer.artboard(&ReadOptions::default()).unwrap().unwrap();
            assert_eq!(read.background, got.background);
        }
    }

    #[test]
    fn hostile_payloads_are_errors_not_panics() {
        let opts = ReadOptions::default();
        assert!(parse(&[], &opts).is_err());
        assert!(parse(&[0, 0, 0, 16], &opts).is_err());
        assert!(parse(&[0, 0, 0, 15, 1, 2, 3], &opts).is_err());
        let good = build(&board(ArtboardBackground::White)).unwrap();
        for cut in 0..good.len() {
            let _ = parse(&good[..cut], &opts);
        }
        // No rect at all.
        let mut s = Sink::new();
        s.u32(16);
        Descriptor::new("artboard").write(&mut s).unwrap();
        assert!(parse(s.as_slice(), &opts).is_err());
        // An empty rect is refused on the way out.
        let mut empty = board(ArtboardBackground::White);
        empty.right = empty.left;
        assert!(build(&empty).is_err());
    }

    /// Through a whole file: the block rides on the group record (the one
    /// that closes the group), not on its bounding divider.
    #[test]
    fn an_artboard_group_survives_a_file_round_trip() {
        let mut file = crate::PsdFile::new(crate::PsdHeader::rgba8(64, 64));
        let mut group = PsdLayer::group("Artboard 1");
        let mut inside = PsdLayer::raster("Inside", crate::Rect::sized(2, 2));
        inside.set_rgba8(&[9, 8, 7, 255].repeat(4)).unwrap();
        group.push_child(inside).unwrap();
        let b = board(ArtboardBackground::Other([1.0, 2.0, 3.0]));
        group.set_artboard(Some(&b)).unwrap();
        file.layers.push(group);
        let back = crate::read(&crate::write(&file).unwrap()).unwrap();
        assert_eq!(back.layers.len(), 1);
        assert_eq!(back.layers[0].children().len(), 1);
        let read = back.layers[0].artboard(&ReadOptions::default()).unwrap();
        assert_eq!(read.unwrap(), b);
    }

    #[test]
    fn set_artboard_replaces_and_clears() {
        let mut layer = PsdLayer::group("A");
        layer.extra.push(TaggedBlock::new(*b"artd", vec![1, 2, 3]));
        layer
            .set_artboard(Some(&board(ArtboardBackground::Black)))
            .unwrap();
        assert_eq!(layer.extra.len(), 1);
        assert_eq!(&layer.extra[0].key, b"artb");
        layer.set_artboard(None).unwrap();
        assert!(layer.artboard(&ReadOptions::default()).is_none());
    }
}
