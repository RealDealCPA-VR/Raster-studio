//! The in-memory document: layers, groups, masks, channels, resources.
//!
//! # Ordering
//!
//! [`PsdFile::layers`] and [`GroupData::children`] are **bottom-to-top**, the
//! order the file itself uses — index 0 is the layer furthest from the viewer.
//! `layer-model`'s tree is the other way round. Neither is more correct, but
//! silently mixing them reverses every document, so the direction is stated on
//! both fields and asserted by `layer_order_is_bottom_to_top`.
//!
//! # Groups
//!
//! A `.psd` has no nesting. A group is three things in the flat layer list: a
//! hidden "bounding section divider" record *below* the group's contents, then
//! the contents, then the record carrying the group's own name and blend mode
//! *above* them. [`crate::read`] turns that back into [`GroupData`] and
//! [`crate::write`] takes it apart again; this module only holds the result.

use layer_model::BlendMode;

use crate::descriptor::Descriptor;
use crate::error::{PsdError, PsdResult};
use crate::header::PsdHeader;
use crate::limits::ReadOptions;

/// Channel id for a layer's transparency (alpha).
pub const CHANNEL_ALPHA: i16 = -1;
/// Channel id for the user layer mask.
pub const CHANNEL_USER_MASK: i16 = -2;
/// Channel id for the "real" (vector-derived) user mask.
pub const CHANNEL_REAL_USER_MASK: i16 = -3;

/// A layer rectangle, stored the way the format stores it.
///
/// The order in the file is `top, left, bottom, right`, and the rectangle is
/// half-open: a one-pixel layer at the origin is `0, 0, 1, 1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Rect {
    pub top: i32,
    pub left: i32,
    pub bottom: i32,
    pub right: i32,
}

impl Rect {
    pub fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Rect {
            top,
            left,
            bottom,
            right,
        }
    }

    /// A rectangle at the origin with this size.
    pub fn sized(width: u32, height: u32) -> Self {
        Rect {
            top: 0,
            left: 0,
            bottom: height as i32,
            right: width as i32,
        }
    }

    /// Width in pixels, `0` when the rectangle is inside out.
    ///
    /// Computed in `i64` because `right - left` overflows `i32` for the
    /// rectangles a hostile file is free to write.
    pub fn width(&self) -> u32 {
        let w = i64::from(self.right) - i64::from(self.left);
        w.clamp(0, i64::from(u32::MAX)) as u32
    }

    pub fn height(&self) -> u32 {
        let h = i64::from(self.bottom) - i64::from(self.top);
        h.clamp(0, i64::from(u32::MAX)) as u32
    }

    pub fn is_empty(&self) -> bool {
        self.width() == 0 || self.height() == 0
    }

    /// Refuse a rectangle whose extent is beyond what the reader will act on.
    ///
    /// An inside-out rectangle is *not* refused: Photoshop writes `0,0,0,0` for
    /// group records and for layers with no pixels, and treating that as
    /// corruption would reject perfectly ordinary files. It reads as empty.
    pub fn validate(&self, max_dimension: u32) -> PsdResult<()> {
        if self.width() > max_dimension || self.height() > max_dimension {
            return Err(PsdError::BadRect {
                top: self.top,
                left: self.left,
                bottom: self.bottom,
                right: self.right,
            });
        }
        Ok(())
    }
}

/// One channel's decoded samples, big-endian at the document's bit depth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Channel {
    /// `0..` are colour channels; see [`CHANNEL_ALPHA`] and friends for the
    /// negative ids.
    pub id: i16,
    pub data: Vec<u8>,
}

impl Channel {
    pub fn new(id: i16, data: Vec<u8>) -> Self {
        Channel { id, data }
    }
}

/// The per-layer "locked" switches from the `lspf` block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Protection {
    pub transparency: bool,
    pub composite: bool,
    pub position: bool,
}

impl Protection {
    pub fn from_bits(bits: u32) -> Self {
        Protection {
            transparency: bits & 1 != 0,
            composite: bits & 2 != 0,
            position: bits & 4 != 0,
        }
    }

    pub fn to_bits(self) -> u32 {
        u32::from(self.transparency)
            | (u32::from(self.composite) << 1)
            | (u32::from(self.position) << 2)
    }

    pub fn is_default(self) -> bool {
        self == Protection::default()
    }
}

/// A layer mask: its own rectangle, its default colour outside that rectangle,
/// its flags, and its pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PsdMask {
    pub bounds: Rect,
    /// The value the mask takes outside [`PsdMask::bounds`] — `0` (hidden) or
    /// `255` (shown). Ignoring it turns "hide everything outside this box" into
    /// "show everything outside this box".
    pub default_color: u8,
    pub relative_to_layer: bool,
    pub disabled: bool,
    pub invert: bool,
    pub from_render: bool,
    /// Mask samples, `bounds.width() * bounds.height()` at the document depth.
    pub data: Vec<u8>,
    pub real: Option<RealMask>,
}

impl PsdMask {
    pub fn new(bounds: Rect, data: Vec<u8>) -> Self {
        PsdMask {
            bounds,
            default_color: 0,
            relative_to_layer: false,
            disabled: false,
            invert: false,
            from_render: false,
            data,
            real: None,
        }
    }
}

/// The second mask Photoshop keeps when a vector mask and a raster mask are
/// both present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RealMask {
    pub bounds: Rect,
    pub default_color: u8,
    pub relative_to_layer: bool,
    pub disabled: bool,
    pub invert: bool,
    pub data: Vec<u8>,
}

/// A tagged block preserved verbatim.
///
/// Anything this crate does not model is kept as bytes and written back
/// unchanged, so opening and saving a file does not quietly discard the parts
/// of it another application cares about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaggedBlock {
    /// `8BIM` or `8B64`.
    pub signature: [u8; 4],
    pub key: [u8; 4],
    pub data: Vec<u8>,
}

impl TaggedBlock {
    pub fn new(key: [u8; 4], data: Vec<u8>) -> Self {
        TaggedBlock {
            signature: *b"8BIM",
            key,
            data,
        }
    }
}

/// The adjustment or fill a layer carries instead of pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adjustment {
    pub key: [u8; 4],
    pub data: Vec<u8>,
}

/// Adjustment and fill keys this crate recognises by name.
///
/// Recognition drives one decision only — whether a tagged block belongs in
/// [`PsdLayer::adjustment`] or in the passthrough [`PsdLayer::extra`] list — so
/// an unrecognised adjustment still round-trips; it just is not labelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AdjustmentKey {
    SolidColorFill,
    GradientFill,
    PatternFill,
    BrightnessContrast,
    Levels,
    Curves,
    Exposure,
    Vibrance,
    HueSaturation,
    ColorBalance,
    BlackAndWhite,
    PhotoFilter,
    ChannelMixer,
    ColorLookup,
    Invert,
    Posterize,
    Threshold,
    GradientMap,
    SelectiveColor,
}

impl AdjustmentKey {
    /// Every key, paired with its four-character code.
    pub const ALL: [(AdjustmentKey, [u8; 4]); 19] = [
        (AdjustmentKey::SolidColorFill, *b"SoCo"),
        (AdjustmentKey::GradientFill, *b"GdFl"),
        (AdjustmentKey::PatternFill, *b"PtFl"),
        (AdjustmentKey::BrightnessContrast, *b"brit"),
        (AdjustmentKey::Levels, *b"levl"),
        (AdjustmentKey::Curves, *b"curv"),
        (AdjustmentKey::Exposure, *b"expA"),
        (AdjustmentKey::Vibrance, *b"vibA"),
        (AdjustmentKey::HueSaturation, *b"hue2"),
        (AdjustmentKey::ColorBalance, *b"blnc"),
        (AdjustmentKey::BlackAndWhite, *b"blwh"),
        (AdjustmentKey::PhotoFilter, *b"phfl"),
        (AdjustmentKey::ChannelMixer, *b"mixr"),
        (AdjustmentKey::ColorLookup, *b"clrL"),
        (AdjustmentKey::Invert, *b"nvrt"),
        (AdjustmentKey::Posterize, *b"post"),
        (AdjustmentKey::Threshold, *b"thrs"),
        (AdjustmentKey::GradientMap, *b"grdm"),
        (AdjustmentKey::SelectiveColor, *b"selc"),
    ];

    pub fn from_code(code: [u8; 4]) -> Option<Self> {
        Self::ALL.iter().find(|(_, c)| *c == code).map(|(k, _)| *k)
    }

    /// Written as an exhaustive match rather than a lookup in [`Self::ALL`], so
    /// that adding a variant is a compile error here instead of a panic at run
    /// time on the one adjustment nobody tested.
    pub const fn code(self) -> [u8; 4] {
        match self {
            AdjustmentKey::SolidColorFill => *b"SoCo",
            AdjustmentKey::GradientFill => *b"GdFl",
            AdjustmentKey::PatternFill => *b"PtFl",
            AdjustmentKey::BrightnessContrast => *b"brit",
            AdjustmentKey::Levels => *b"levl",
            AdjustmentKey::Curves => *b"curv",
            AdjustmentKey::Exposure => *b"expA",
            AdjustmentKey::Vibrance => *b"vibA",
            AdjustmentKey::HueSaturation => *b"hue2",
            AdjustmentKey::ColorBalance => *b"blnc",
            AdjustmentKey::BlackAndWhite => *b"blwh",
            AdjustmentKey::PhotoFilter => *b"phfl",
            AdjustmentKey::ChannelMixer => *b"mixr",
            AdjustmentKey::ColorLookup => *b"clrL",
            AdjustmentKey::Invert => *b"nvrt",
            AdjustmentKey::Posterize => *b"post",
            AdjustmentKey::Threshold => *b"thrs",
            AdjustmentKey::GradientMap => *b"grdm",
            AdjustmentKey::SelectiveColor => *b"selc",
        }
    }
}

/// `width * height * 4`, or `None` if that does not fit a `usize`.
///
/// A [`Rect`] is public and can be built by hand with `i32::MIN`/`i32::MAX`
/// corners, which multiply out past `usize` on the interleave path. The parser
/// range-checks rectangles on the way in; this is the guard for everything
/// else, so a bad rectangle declines instead of overflowing.
fn rgba_len(width: u32, height: u32) -> Option<usize> {
    (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)
}

impl Adjustment {
    pub fn kind(&self) -> Option<AdjustmentKey> {
        AdjustmentKey::from_code(self.key)
    }

    /// Parse the descriptor-shaped payload most modern adjustment keys carry.
    ///
    /// Returns `None` for the legacy fixed-layout keys (`brit`, `levl`, `curv`
    /// and friends written by very old Photoshop), which are not descriptors at
    /// all. Those keep their bytes and are written back unchanged.
    pub fn descriptor(&self, opts: &ReadOptions) -> Option<Descriptor> {
        let mut cur = crate::bytes::Cursor::new(&self.data);
        let _version = cur.u32().ok()?;
        Descriptor::read(&mut cur, opts).ok()
    }

    /// The fill colour of a `SoCo` solid-colour layer, as 0..=255 RGB.
    pub fn solid_color_rgb(&self, opts: &ReadOptions) -> Option<[f64; 3]> {
        if self.key != *b"SoCo" {
            return None;
        }
        let d = self.descriptor(opts)?;
        let c = d.descriptor("Clr ")?;
        Some([c.number("Rd  ")?, c.number("Grn ")?, c.number("Bl  ")?])
    }

    /// The legacy `brit` payload: brightness and contrast, each -100..=100.
    pub fn brightness_contrast(&self) -> Option<(i16, i16)> {
        if self.key != *b"brit" || self.data.len() < 4 {
            return None;
        }
        let b = i16::from_be_bytes([self.data[0], self.data[1]]);
        let c = i16::from_be_bytes([self.data[2], self.data[3]]);
        Some((b, c))
    }
}

/// Layer effects, preserved verbatim under the key they arrived with.
///
/// `lfx2` is the descriptor-based form every Photoshop since 6.0 writes;
/// `lrFX` is the fixed-layout form from before that. Both are kept as bytes so
/// a file that carries drop shadows still carries them after a save.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Effects {
    pub key: [u8; 4],
    pub data: Vec<u8>,
}

impl Effects {
    pub fn descriptor(&self, opts: &ReadOptions) -> Option<Descriptor> {
        if self.key != *b"lfx2" {
            return None;
        }
        let mut cur = crate::bytes::Cursor::new(&self.data);
        let _object_version = cur.u32().ok()?;
        let _descriptor_version = cur.u32().ok()?;
        Descriptor::read(&mut cur, opts).ok()
    }
}

/// A type layer's `TySh` block.
#[derive(Debug, Clone, PartialEq)]
pub struct TextData {
    /// The 2×3 affine placing the text on the canvas: `xx, xy, yx, yy, tx, ty`.
    pub transform: [f64; 6],
    /// The string, when the text descriptor could be parsed.
    pub text: Option<String>,
    /// The whole block verbatim. This is what gets written back: a `TySh`
    /// synthesised from scratch would need a complete engine-data payload, and
    /// a partial one makes Photoshop discard the layer.
    pub raw: Vec<u8>,
}

/// What a layer is.
#[derive(Debug, Clone, PartialEq)]
pub enum LayerKind {
    /// Anything that is not a group: pixels, an adjustment, a shape, text.
    Raster,
    Group(GroupData),
}

/// A group's contents and its two group-only properties.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct GroupData {
    /// Bottom-to-top, like [`PsdFile::layers`].
    pub children: Vec<PsdLayer>,
    /// Expanded in the layers panel.
    pub open: bool,
    /// Photoshop's "Pass Through": children blend against what is beneath the
    /// group rather than into an isolated buffer. Mutually exclusive with a
    /// blend mode — when this is set, [`PsdLayer::blend_mode`] is `Normal` and
    /// the file stores the key `'pass'`.
    pub pass_through: bool,
}

/// Dropping a group unlinks its whole subtree iteratively.
///
/// The compiler's own glue for `Vec<PsdLayer>` recurses once per nesting level,
/// so dropping a deeply nested tree overflows the stack — and unlike a panic,
/// that is an abort the host application cannot catch. It is also the one
/// consumer nobody calls on purpose: it runs on the way out of *every* function
/// that holds a tree, including the error paths that were meant to reject the
/// file in the first place. [`crate::read`] caps nesting so a parsed tree is
/// shallow, and this impl covers trees a caller assembles by hand.
///
/// The recursion lives in `PsdLayer` → `LayerKind::Group` → `GroupData` →
/// `Vec<PsdLayer>`; breaking it here breaks the whole cycle, and doing it here
/// rather than on `PsdLayer` keeps `PsdLayer { .. ..Default::default() }`
/// legal, which a manual `Drop` on `PsdLayer` would forbid.
impl Drop for GroupData {
    fn drop(&mut self) {
        let mut pending = std::mem::take(&mut self.children);
        while let Some(mut layer) = pending.pop() {
            if let LayerKind::Group(group) = &mut layer.kind {
                pending.append(&mut group.children);
            }
            // `layer` is dropped here with its own children already moved into
            // `pending`, so this impl runs again on an empty vector and stops.
        }
    }
}

/// One layer record.
#[derive(Debug, Clone, PartialEq)]
pub struct PsdLayer {
    pub name: String,
    pub bounds: Rect,
    pub blend_mode: BlendMode,
    /// 0..=255.
    pub opacity: u8,
    /// The separate "Fill" opacity, from `iOpa`. `None` means the file did not
    /// carry one, which is not the same as 255 — writing 255 back into a file
    /// that had no `iOpa` block adds a block Photoshop did not put there.
    pub fill_opacity: Option<u8>,
    /// This layer clips to the first non-clipping layer beneath it.
    pub clipping: bool,
    pub visible: bool,
    /// Bit 0 of the record flags. Distinct from [`Protection::transparency`],
    /// which is the `lspf` block — Photoshop writes both.
    pub transparency_protected: bool,
    /// Bit 4 of the record flags: the pixels do not contribute to the
    /// composite.
    pub pixel_data_irrelevant: bool,
    pub protection: Protection,
    pub layer_id: Option<u32>,
    /// The colour label shown in the layers panel, from `lclr`.
    pub sheet_color: Option<u16>,
    pub channels: Vec<Channel>,
    pub mask: Option<PsdMask>,
    /// The blending-ranges payload, verbatim.
    pub blending_ranges: Vec<u8>,
    pub adjustment: Option<Adjustment>,
    pub effects: Option<Effects>,
    pub text: Option<TextData>,
    /// Tagged blocks this crate does not model, preserved in file order.
    pub extra: Vec<TaggedBlock>,
    pub kind: LayerKind,
}

impl Default for PsdLayer {
    fn default() -> Self {
        PsdLayer {
            name: String::new(),
            bounds: Rect::default(),
            blend_mode: BlendMode::Normal,
            opacity: 255,
            fill_opacity: None,
            clipping: false,
            visible: true,
            transparency_protected: false,
            pixel_data_irrelevant: false,
            protection: Protection::default(),
            layer_id: None,
            sheet_color: None,
            channels: Vec::new(),
            mask: None,
            blending_ranges: Vec::new(),
            adjustment: None,
            effects: None,
            text: None,
            extra: Vec::new(),
            kind: LayerKind::Raster,
        }
    }
}

impl PsdLayer {
    /// An empty raster layer with the given bounds.
    pub fn raster(name: impl Into<String>, bounds: Rect) -> Self {
        PsdLayer {
            name: name.into(),
            bounds,
            ..Default::default()
        }
    }

    /// An empty, open, non-pass-through group.
    pub fn group(name: impl Into<String>) -> Self {
        PsdLayer {
            name: name.into(),
            kind: LayerKind::Group(GroupData {
                children: Vec::new(),
                open: true,
                pass_through: false,
            }),
            ..Default::default()
        }
    }

    pub fn is_group(&self) -> bool {
        matches!(self.kind, LayerKind::Group(_))
    }

    pub fn group_data(&self) -> Option<&GroupData> {
        match &self.kind {
            LayerKind::Group(g) => Some(g),
            LayerKind::Raster => None,
        }
    }

    pub fn group_data_mut(&mut self) -> Option<&mut GroupData> {
        match &mut self.kind {
            LayerKind::Group(g) => Some(g),
            LayerKind::Raster => None,
        }
    }

    /// Children, or an empty slice for a non-group.
    pub fn children(&self) -> &[PsdLayer] {
        self.group_data().map_or(&[], |g| g.children.as_slice())
    }

    pub fn push_child(&mut self, child: PsdLayer) -> PsdResult<()> {
        match self.group_data_mut() {
            Some(g) => {
                g.children.push(child);
                Ok(())
            }
            None => Err(PsdError::InvalidDocument(format!(
                "layer {:?} is not a group and cannot take a child",
                self.name
            ))),
        }
    }

    pub fn channel(&self, id: i16) -> Option<&Channel> {
        self.channels.iter().find(|c| c.id == id)
    }

    /// Replace the colour and alpha channels from interleaved 8-bit RGBA.
    ///
    /// The slice must be `bounds.width() * bounds.height() * 4` bytes.
    pub fn set_rgba8(&mut self, rgba: &[u8]) -> PsdResult<()> {
        let expected =
            rgba_len(self.bounds.width(), self.bounds.height()).ok_or(PsdError::Overflow {
                what: "layer pixel count",
            })?;
        if rgba.len() != expected {
            return Err(PsdError::ChannelSizeMismatch {
                what: "set_rgba8 input",
                expected,
                actual: rgba.len(),
            });
        }
        let n = expected / 4;
        let mut planes = vec![vec![0u8; n]; 4];
        for (i, px) in rgba.chunks_exact(4).enumerate() {
            for (p, plane) in planes.iter_mut().enumerate() {
                plane[i] = px[p];
            }
        }
        let alpha = planes.pop().expect("four planes were just built");
        // Alpha first, then R, G, B: the order Photoshop writes.
        self.channels = vec![Channel::new(CHANNEL_ALPHA, alpha)];
        for (i, plane) in planes.into_iter().enumerate() {
            self.channels.push(Channel::new(i as i16, plane));
        }
        Ok(())
    }

    /// Interleave the colour and alpha channels back into 8-bit RGBA.
    ///
    /// `None` when a channel is missing or the wrong size. A missing alpha
    /// channel is treated as fully opaque, which is what Photoshop does for a
    /// background layer.
    ///
    /// # Every check happens before the allocation
    ///
    /// A [`Rect`] read from a file may be as large as the reader's
    /// `max_dimension` — thirty thousand a side, nine hundred million pixels —
    /// while the layer carries no channels at all, which costs a hostile file
    /// nothing. Looking the planes up first means that layer declines in
    /// constant time; validating after `vec![255u8; total]` would reserve and
    /// zero 3.6 GB and only then discover there was nothing to put in it. This
    /// is a convenience a caller runs over [`PsdFile::all_layers`] straight
    /// after a parse, so it is outside the parser's own [`crate::limits::Budget`]
    /// and has to defend itself.
    pub fn rgba8(&self) -> Option<Vec<u8>> {
        let total = rgba_len(self.bounds.width(), self.bounds.height())?;
        let n = total / 4;
        let mut planes = [[].as_slice(); 3];
        for (c, plane) in planes.iter_mut().enumerate() {
            let found = self.channel(c as i16)?;
            if found.data.len() != n {
                return None;
            }
            *plane = found.data.as_slice();
        }
        let alpha = match self.channel(CHANNEL_ALPHA) {
            Some(a) if a.data.len() != n => return None,
            Some(a) => Some(a.data.as_slice()),
            None => None,
        };

        let mut out = vec![255u8; total];
        for (c, plane) in planes.iter().enumerate() {
            for (i, v) in plane.iter().enumerate() {
                out[i * 4 + c] = *v;
            }
        }
        if let Some(a) = alpha {
            for (i, v) in a.iter().enumerate() {
                out[i * 4 + 3] = *v;
            }
        }
        Some(out)
    }

    /// Depth-first walk over this layer and everything under it, bottom-to-top.
    ///
    /// Iterative on purpose: the tree is built from untrusted input, and a
    /// recursive walk over a deeply nested one is a stack overflow, which is an
    /// abort rather than an error.
    pub fn walk<'a>(&'a self, out: &mut Vec<&'a PsdLayer>) {
        let mut stack: Vec<&'a PsdLayer> = vec![self];
        while let Some(layer) = stack.pop() {
            out.push(layer);
            // Reversed, so that popping visits child 0 first and the result is
            // the same pre-order, bottom-to-top sequence recursion gave.
            for child in layer.children().iter().rev() {
                stack.push(child);
            }
        }
    }
}

/// The flattened composite Photoshop stores at the end of the file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MergedImage {
    /// One entry per [`PsdHeader::channels`], each `width * height` samples.
    pub channels: Vec<Vec<u8>>,
}

impl MergedImage {
    /// Build an 8-bit RGBA composite from interleaved pixels.
    pub fn from_rgba8(width: u32, height: u32, rgba: &[u8]) -> PsdResult<Self> {
        let expected = rgba_len(width, height).ok_or(PsdError::Overflow {
            what: "canvas pixel count",
        })?;
        if rgba.len() != expected {
            return Err(PsdError::ChannelSizeMismatch {
                what: "merged image input",
                expected,
                actual: rgba.len(),
            });
        }
        let n = expected / 4;
        let mut channels = vec![vec![0u8; n]; 4];
        for (i, px) in rgba.chunks_exact(4).enumerate() {
            for (c, plane) in channels.iter_mut().enumerate() {
                plane[i] = px[c];
            }
        }
        Ok(MergedImage { channels })
    }

    /// Interleave an 8-bit RGB(A) composite back into pixels.
    ///
    /// Like [`PsdLayer::rgba8`], every plane is found and length-checked before
    /// the output buffer is reserved, so a composite that cannot be built costs
    /// nothing however large the canvas claims to be.
    pub fn to_rgba8(&self, width: u32, height: u32) -> Option<Vec<u8>> {
        let total = rgba_len(width, height)?;
        let n = total / 4;
        if self.channels.len() < 3 {
            return None;
        }
        let mut planes = [[].as_slice(); 3];
        for (c, plane) in planes.iter_mut().enumerate() {
            let found = self.channels.get(c)?;
            if found.len() != n {
                return None;
            }
            *plane = found.as_slice();
        }
        let alpha = match self.channels.get(3) {
            Some(a) if a.len() != n => return None,
            Some(a) => Some(a.as_slice()),
            None => None,
        };

        let mut out = vec![255u8; total];
        for (c, plane) in planes.iter().enumerate() {
            for (i, v) in plane.iter().enumerate() {
                out[i * 4 + c] = *v;
            }
        }
        if let Some(a) = alpha {
            for (i, v) in a.iter().enumerate() {
                out[i * 4 + 3] = *v;
            }
        }
        Some(out)
    }
}

/// One entry from the image-resources section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageResource {
    pub id: u16,
    pub name: String,
    pub data: Vec<u8>,
}

/// A whole document.
#[derive(Debug, Clone, PartialEq)]
pub struct PsdFile {
    pub header: PsdHeader,
    /// Only non-empty for the colour modes this crate refuses, so in practice
    /// always empty — kept so a future mode does not need a new field.
    pub color_mode_data: Vec<u8>,
    pub resources: Vec<ImageResource>,
    /// Bottom-to-top.
    pub layers: Vec<PsdLayer>,
    /// The global layer mask info payload, verbatim.
    pub global_mask: Vec<u8>,
    /// Document-level tagged blocks, preserved in file order.
    pub extra: Vec<TaggedBlock>,
    /// The flattened composite. [`crate::write`] synthesises one when this is
    /// `None`, because many readers show nothing else.
    pub merged: Option<MergedImage>,
    /// Recoverable oddities noticed while reading. A file that produces
    /// warnings still parsed; the warnings say what was tolerated.
    pub warnings: Vec<String>,
}

impl PsdFile {
    pub fn new(header: PsdHeader) -> Self {
        PsdFile {
            header,
            color_mode_data: Vec::new(),
            resources: Vec::new(),
            layers: Vec::new(),
            global_mask: Vec::new(),
            extra: Vec::new(),
            merged: None,
            warnings: Vec::new(),
        }
    }

    /// Every layer, groups included, depth-first and bottom-to-top.
    pub fn all_layers(&self) -> Vec<&PsdLayer> {
        let mut out = Vec::new();
        for layer in &self.layers {
            layer.walk(&mut out);
        }
        out
    }

    /// The number of layer records the file will contain, dividers included.
    ///
    /// Iterative, because the tree can come from a file: see [`PsdLayer::walk`].
    pub fn record_count(&self) -> usize {
        let mut total = 0usize;
        let mut stack: Vec<&PsdLayer> = self.layers.iter().collect();
        while let Some(layer) = stack.pop() {
            match &layer.kind {
                LayerKind::Raster => total += 1,
                // The group record plus its hidden closing divider.
                LayerKind::Group(g) => {
                    total += 2;
                    stack.extend(g.children.iter());
                }
            }
        }
        total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_extent_does_not_overflow_on_a_hostile_rectangle() {
        let r = Rect {
            top: i32::MIN,
            left: i32::MIN,
            bottom: i32::MAX,
            right: i32::MAX,
        };
        assert_eq!(r.width(), u32::MAX);
        assert_eq!(r.height(), u32::MAX);
        assert!(r.validate(30_000).is_err());
    }

    #[test]
    fn an_inside_out_rect_reads_as_empty_rather_than_as_corruption() {
        let r = Rect {
            top: 10,
            left: 10,
            bottom: 4,
            right: 4,
        };
        assert_eq!((r.width(), r.height()), (0, 0));
        assert!(r.is_empty());
        r.validate(30_000).unwrap();
    }

    #[test]
    fn rect_is_half_open() {
        let r = Rect::new(0, 0, 1, 1);
        assert_eq!((r.width(), r.height()), (1, 1));
        assert_eq!(Rect::sized(640, 480), Rect::new(0, 0, 640, 480));
    }

    #[test]
    fn rgba_round_trips_through_planar_channels_in_photoshop_order() {
        let mut layer = PsdLayer::raster("l", Rect::sized(3, 2));
        let rgba: Vec<u8> = (0..24).collect();
        layer.set_rgba8(&rgba).unwrap();
        assert_eq!(
            layer.channels.iter().map(|c| c.id).collect::<Vec<_>>(),
            vec![CHANNEL_ALPHA, 0, 1, 2]
        );
        assert_eq!(layer.channel(0).unwrap().data, vec![0, 4, 8, 12, 16, 20]);
        assert_eq!(
            layer.channel(CHANNEL_ALPHA).unwrap().data,
            vec![3, 7, 11, 15, 19, 23]
        );
        assert_eq!(layer.rgba8().unwrap(), rgba);
    }

    #[test]
    fn set_rgba8_refuses_a_slice_that_does_not_match_the_bounds() {
        let mut layer = PsdLayer::raster("l", Rect::sized(3, 2));
        assert!(matches!(
            layer.set_rgba8(&[0u8; 20]).unwrap_err(),
            PsdError::ChannelSizeMismatch {
                expected: 24,
                actual: 20,
                ..
            }
        ));
    }

    #[test]
    fn a_layer_with_no_alpha_channel_reads_back_as_opaque() {
        let mut layer = PsdLayer::raster("l", Rect::sized(2, 1));
        layer.channels = vec![
            Channel::new(0, vec![1, 2]),
            Channel::new(1, vec![3, 4]),
            Channel::new(2, vec![5, 6]),
        ];
        assert_eq!(layer.rgba8().unwrap(), vec![1, 3, 5, 255, 2, 4, 6, 255]);
    }

    #[test]
    fn protection_bits_round_trip() {
        for bits in 0..8u32 {
            assert_eq!(Protection::from_bits(bits).to_bits(), bits);
        }
        assert!(Protection::from_bits(0).is_default());
        assert!(!Protection::from_bits(4).is_default());
    }

    #[test]
    fn adjustment_keys_round_trip_and_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for (key, code) in AdjustmentKey::ALL {
            assert!(seen.insert(code), "duplicate code for {key:?}");
            assert_eq!(AdjustmentKey::from_code(code), Some(key));
            assert_eq!(key.code(), code);
        }
        assert_eq!(AdjustmentKey::from_code(*b"zzzz"), None);
        // Pinned so that adding a variant to the enum without adding it to
        // `ALL` — which would make `from_code` blind to it — fails here.
        assert_eq!(AdjustmentKey::ALL.len(), 19);
    }

    #[test]
    fn a_rectangle_too_large_to_index_declines_instead_of_overflowing() {
        let huge = Rect {
            top: i32::MIN,
            left: i32::MIN,
            bottom: i32::MAX,
            right: i32::MAX,
        };
        let mut layer = PsdLayer::raster("huge", huge);
        assert_eq!(layer.rgba8(), None);
        assert!(matches!(
            layer.set_rgba8(&[]).unwrap_err(),
            PsdError::Overflow { .. }
        ));
        assert!(matches!(
            MergedImage::from_rgba8(u32::MAX, u32::MAX, &[]).unwrap_err(),
            PsdError::Overflow { .. }
        ));
        assert_eq!(
            MergedImage::default().to_rgba8(u32::MAX, u32::MAX),
            None,
            "the interleave must decline rather than reserve"
        );
    }

    /// The largest rectangle [`crate::limits::ReadOptions`] accepts by default,
    /// which is therefore a rectangle a hostile file is allowed to declare.
    const HUGE: Rect = Rect {
        top: 0,
        left: 0,
        bottom: 30_000,
        right: 30_000,
    };

    #[test]
    fn rgba8_finds_its_planes_before_it_reserves_the_output() {
        // 30 000 × 30 000 is exactly `max_dimension`, so the reader accepts a
        // layer with these bounds — and a layer record costs a hostile file
        // about sixty bytes whether or not it carries any channel data. Looking
        // the planes up after `vec![255u8; total]` reserves and memsets 3.6 GB
        // and only then discovers there was nothing to fill it with.
        let layer = PsdLayer::raster("huge", HUGE);
        assert!(layer.channels.is_empty());
        let (out, allocated) = crate::probe::bytes_allocated_by(|| layer.rgba8());
        assert_eq!(out, None);
        assert!(
            allocated < 4096,
            "rgba8 reserved {allocated} bytes before declining"
        );

        // Present but wrong-sized channels take the same early exit.
        let mut wrong = PsdLayer::raster("wrong", HUGE);
        wrong.channels = vec![
            Channel::new(0, vec![1, 2, 3]),
            Channel::new(1, vec![1, 2, 3]),
            Channel::new(2, vec![1, 2, 3]),
        ];
        let (out, allocated) = crate::probe::bytes_allocated_by(|| wrong.rgba8());
        assert_eq!(out, None);
        assert!(allocated < 4096, "{allocated} bytes");

        // A wrong-sized *alpha* channel is the last thing checked, and must
        // still be checked before the buffer exists.
        let mut bad_alpha = PsdLayer::raster("bad alpha", HUGE);
        let n = 30_000usize * 30_000;
        bad_alpha.channels = vec![
            Channel::new(0, vec![0; 0]),
            Channel::new(1, vec![0; 0]),
            Channel::new(2, vec![0; 0]),
            Channel::new(CHANNEL_ALPHA, vec![0; 1]),
        ];
        assert_ne!(n, 0);
        let (out, allocated) = crate::probe::bytes_allocated_by(|| bad_alpha.rgba8());
        assert_eq!(out, None);
        assert!(allocated < 4096, "{allocated} bytes");

        // The counter is not simply blind: a layer that *can* be interleaved
        // does allocate its output.
        let mut good = PsdLayer::raster("good", Rect::sized(64, 64));
        good.set_rgba8(&[7u8; 64 * 64 * 4]).unwrap();
        let (out, allocated) = crate::probe::bytes_allocated_by(|| good.rgba8());
        assert_eq!(out.unwrap().len(), 64 * 64 * 4);
        assert!(allocated >= 64 * 64 * 4, "only {allocated} bytes");
    }

    #[test]
    fn merged_to_rgba8_finds_its_planes_before_it_reserves_the_output() {
        // Same ordering rule, same reasoning, on the composite's interleave.
        let empty = MergedImage::default();
        let (out, allocated) = crate::probe::bytes_allocated_by(|| empty.to_rgba8(30_000, 30_000));
        assert_eq!(out, None);
        assert!(allocated < 4096, "{allocated} bytes");

        let short = MergedImage {
            channels: vec![vec![1, 2, 3], vec![4, 5, 6], vec![7, 8, 9]],
        };
        let (out, allocated) = crate::probe::bytes_allocated_by(|| short.to_rgba8(30_000, 30_000));
        assert_eq!(out, None);
        assert!(allocated < 4096, "{allocated} bytes");

        // Three good planes and a short alpha: the alpha check is last, and
        // still comes before the reservation.
        let bad_alpha = MergedImage {
            channels: vec![Vec::new(), Vec::new(), Vec::new(), vec![9]],
        };
        let (out, allocated) =
            crate::probe::bytes_allocated_by(|| bad_alpha.to_rgba8(30_000, 30_000));
        assert_eq!(out, None);
        assert!(allocated < 4096, "{allocated} bytes");
    }

    #[test]
    fn dropping_a_group_tree_deeper_than_the_stack_does_not_abort() {
        // The compiler's own drop glue for `Vec<PsdLayer>` recurses once per
        // level, so this many levels overflows the stack — an abort no caller
        // can catch. `GroupData` therefore drops its subtree iteratively.
        // Building the tree is already iterative: `push_child` only moves.
        let mut node = PsdLayer::group("leaf");
        for _ in 0..50_000 {
            let mut parent = PsdLayer::group("g");
            parent.push_child(node).expect("a group takes children");
            node = parent;
        }
        drop(node);
    }

    #[test]
    fn walking_and_counting_a_very_deep_tree_do_not_recurse() {
        // Deep enough that a recursive walk overflows an eight-megabyte stack
        // even when the optimiser has made each frame small.
        const DEPTH: usize = 400_000;
        let mut node = PsdLayer::raster("leaf", Rect::sized(1, 1));
        for _ in 0..DEPTH {
            let mut parent = PsdLayer::group("g");
            parent.push_child(node).expect("a group takes children");
            node = parent;
        }
        let mut file = PsdFile::new(PsdHeader::rgba8(1, 1));
        file.layers.push(node);
        // Every group plus the leaf.
        assert_eq!(file.all_layers().len(), DEPTH + 1);
        // Two records per group, one for the leaf.
        assert_eq!(file.record_count(), DEPTH * 2 + 1);
    }

    #[test]
    fn legacy_brightness_contrast_decodes_signed_values() {
        let a = Adjustment {
            key: *b"brit",
            data: vec![0xFF, 0xE2, 0x00, 0x1E, 0, 0, 0],
        };
        assert_eq!(a.brightness_contrast(), Some((-30, 30)));
        // A different key does not decode as `brit`.
        let b = Adjustment {
            key: *b"levl",
            data: vec![0xFF, 0xE2, 0x00, 0x1E],
        };
        assert_eq!(b.brightness_contrast(), None);
        // A truncated payload declines rather than indexing past the end.
        let c = Adjustment {
            key: *b"brit",
            data: vec![0x00],
        };
        assert_eq!(c.brightness_contrast(), None);
    }

    #[test]
    fn record_count_includes_the_hidden_divider_each_group_needs() {
        let mut file = PsdFile::new(PsdHeader::rgba8(4, 4));
        let mut outer = PsdLayer::group("outer");
        let mut inner = PsdLayer::group("inner");
        inner
            .push_child(PsdLayer::raster("leaf", Rect::sized(1, 1)))
            .unwrap();
        outer.push_child(inner).unwrap();
        file.layers.push(PsdLayer::raster("bg", Rect::sized(4, 4)));
        file.layers.push(outer);
        // bg + (outer, divider) + (inner, divider) + leaf
        assert_eq!(file.record_count(), 6);
        assert_eq!(file.all_layers().len(), 4);
    }

    #[test]
    fn pushing_a_child_onto_a_non_group_is_refused() {
        let mut leaf = PsdLayer::raster("leaf", Rect::sized(1, 1));
        assert!(leaf
            .push_child(PsdLayer::raster("x", Rect::sized(1, 1)))
            .is_err());
        assert!(leaf.children().is_empty());
    }

    #[test]
    fn layer_order_is_bottom_to_top() {
        let mut file = PsdFile::new(PsdHeader::rgba8(4, 4));
        file.layers
            .push(PsdLayer::raster("bottom", Rect::sized(1, 1)));
        file.layers.push(PsdLayer::raster("top", Rect::sized(1, 1)));
        let names: Vec<&str> = file.all_layers().iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, vec!["bottom", "top"]);
    }

    #[test]
    fn merged_rgba_round_trips() {
        let rgba: Vec<u8> = (0..48u32).map(|v| v as u8).collect();
        let m = MergedImage::from_rgba8(4, 3, &rgba).unwrap();
        assert_eq!(m.channels.len(), 4);
        assert_eq!(m.to_rgba8(4, 3).unwrap(), rgba);
        assert!(MergedImage::from_rgba8(4, 3, &rgba[..10]).is_err());
    }
}
