//! GIMP's native document, `.xcf`.
//!
//! # What is read
//!
//! * Every XCF version GIMP writes (`gimp xcf file` and `v001`..`v0NN`),
//!   including the 64-bit offsets of version 11 and later.
//! * RGB and greyscale images at 8 bits per channel: GIMP's "8-bit
//!   perceptual" precision (versions 0-3, version 4's code 0, codes 150 and
//!   175) and "8-bit linear" (code 100, converted to sRGB as it is read).
//! * Uncompressed, RLE and zlib tiles.
//! * The layer tree: groups (by `PROP_ITEM_PATH`), offsets, opacity (integer
//!   and float), visibility, layer masks (applied when `PROP_APPLY_MASK`
//!   says so) and the basic blend modes - Normal, Multiply, Screen, Overlay,
//!   Soft light, Hard light, Difference, Addition, Subtract, Darken only,
//!   Lighten only, Divide, Dodge, Burn, Grain extract and Grain merge, in both
//!   their legacy and GIMP 2.10+ codes. [`read`] returns that tree
//!   ([`XcfDocument`]); [`decode`] flattens it.
//!
//! # What is not, and how it is reported
//!
//! * Indexed images and every precision above 8 bits are **refused by name**
//!   (the error says which), never approximated.
//! * Anything the flattening has to approximate is listed in
//!   [`XcfDocument::notes`]: a blend mode outside the list above (drawn as
//!   Normal), Dissolve (drawn as Normal), a pass-through group (its children
//!   drawn straight onto what is below, each with the group's opacity folded
//!   in), and a floating selection (skipped). Blending happens on the file's
//!   8-bit perceptual values with W3C source-over compositing, which is what
//!   GIMP's legacy modes do; GIMP 2.10's default modes blend in linear light,
//!   so a partially transparent pixel in one of those can differ slightly.
//! * Channels, paths, guides, parasites, text-layer text and layer effects
//!   are not read; text layers draw from their stored pixels.
//!
//! File > Open reaches this through the flat decoder, so a `.xcf` opens as
//! its flattened composite.
//!
//! # Untrusted input
//!
//! Every offset is checked against the file length before it is followed,
//! every count (properties, layers, tiles, group depth) is bounded, the canvas
//! and each layer go through [`ImportLimits`] before their buffers exist, and
//! the live allocation - one canvas per open group level plus the layer being
//! drawn - is checked against [`ImportLimits::max_alloc_bytes`] before each
//! buffer is reserved. RLE runs are bounds-checked against the tile they fill
//! and zlib output is capped at the tile's size.

use std::io::Read;

use super::{check_decode, malformed, rgba8_surface};
use crate::codec::{CodecError, DecodedSurface, ImageInfo, ImportFormat, ImportLimits};

const NAME: &str = "XCF";
const TILE: usize = 64;
/// More properties than any real item carries; a bound on a hostile loop.
const MAX_PROPERTIES: usize = 4096;
/// More layers than any real document; a bound before the list is walked.
const MAX_LAYERS: usize = 16_384;
/// Deepest group nesting accepted.
pub const MAX_GROUP_DEPTH: usize = 32;

const PROP_END: u32 = 0;
const PROP_COLORMAP: u32 = 1;
const PROP_FLOATING_SELECTION: u32 = 5;
const PROP_OPACITY: u32 = 6;
const PROP_MODE: u32 = 7;
const PROP_VISIBLE: u32 = 8;
const PROP_APPLY_MASK: u32 = 11;
const PROP_OFFSETS: u32 = 15;
const PROP_COMPRESSION: u32 = 17;
const PROP_GROUP_ITEM: u32 = 29;
const PROP_ITEM_PATH: u32 = 30;
const PROP_FLOAT_OPACITY: u32 = 33;

/// `true` when `head` opens with the XCF magic.
pub fn looks_like_xcf(head: &[u8]) -> bool {
    head.starts_with(b"gimp xcf ")
}

/// A layer's blend mode, as far as the flattener distinguishes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XcfMode {
    Normal,
    Multiply,
    Screen,
    Overlay,
    SoftLight,
    HardLight,
    Difference,
    Addition,
    Subtract,
    Darken,
    Lighten,
    Divide,
    Dodge,
    Burn,
    GrainExtract,
    GrainMerge,
    /// A group that composites its children straight onto what is below.
    PassThrough,
    /// Any other mode, drawn as Normal and listed in the notes. Holds GIMP's
    /// code.
    Unsupported(u32),
}

impl XcfMode {
    /// GIMP's `GimpLayerMode` code, legacy (0-22) and 2.10+ (23-61) alike.
    pub fn from_code(code: u32) -> Self {
        match code {
            0 | 28 => XcfMode::Normal,
            3 | 30 => XcfMode::Multiply,
            4 | 31 => XcfMode::Screen,
            23 => XcfMode::Overlay,
            // GIMP itself loads legacy Overlay as legacy Soft light: the old
            // Overlay was always Soft light's formula.
            5 | 19 | 45 => XcfMode::SoftLight,
            18 | 44 => XcfMode::HardLight,
            6 | 32 => XcfMode::Difference,
            7 | 33 => XcfMode::Addition,
            8 | 34 => XcfMode::Subtract,
            9 | 35 => XcfMode::Darken,
            10 | 36 => XcfMode::Lighten,
            15 | 41 => XcfMode::Divide,
            16 | 42 => XcfMode::Dodge,
            17 | 43 => XcfMode::Burn,
            20 | 46 => XcfMode::GrainExtract,
            21 | 47 => XcfMode::GrainMerge,
            61 => XcfMode::PassThrough,
            other => XcfMode::Unsupported(other),
        }
    }

    /// The separable blend function `B(backdrop, source)` on `0..=1` values.
    fn blend(self, b: f32, s: f32) -> f32 {
        let v = match self {
            XcfMode::Normal | XcfMode::PassThrough | XcfMode::Unsupported(_) => s,
            XcfMode::Multiply => b * s,
            XcfMode::Screen => b + s - b * s,
            XcfMode::Overlay => XcfMode::HardLight.blend(s, b),
            XcfMode::SoftLight => {
                // GIMP's soft light: (1 - b) * b * s + b * screen(b, s).
                let screen = 1.0 - (1.0 - b) * (1.0 - s);
                (1.0 - b) * b * s + b * screen
            }
            XcfMode::HardLight => {
                if s <= 0.5 {
                    b * 2.0 * s
                } else {
                    let s2 = 2.0 * s - 1.0;
                    b + s2 - b * s2
                }
            }
            XcfMode::Difference => (b - s).abs(),
            XcfMode::Addition => b + s,
            XcfMode::Subtract => b - s,
            XcfMode::Darken => b.min(s),
            XcfMode::Lighten => b.max(s),
            XcfMode::Divide => {
                if s <= 0.0 {
                    if b > 0.0 {
                        1.0
                    } else {
                        0.0
                    }
                } else {
                    b / s
                }
            }
            XcfMode::Dodge => {
                if s >= 1.0 {
                    1.0
                } else {
                    b / (1.0 - s)
                }
            }
            XcfMode::Burn => {
                if s <= 0.0 {
                    0.0
                } else {
                    1.0 - (1.0 - b) / s
                }
            }
            XcfMode::GrainExtract => b - s + 0.5,
            XcfMode::GrainMerge => b + s - 0.5,
        };
        v.clamp(0.0, 1.0)
    }
}

/// A layer or group, as the file describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct XcfLayer {
    pub name: String,
    /// Offset of the layer's top-left corner on the canvas.
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    /// `0.0..=1.0`.
    pub opacity: f32,
    pub visible: bool,
    pub mode: XcfMode,
    /// GIMP's raw mode code.
    pub mode_code: u32,
    /// `true` for a layer group; its pixels are its [`XcfLayer::children`].
    pub is_group: bool,
    /// A group's items, **top first**, as GIMP lists them.
    pub children: Vec<XcfLayer>,
    /// `true` when the layer carries a mask that is applied.
    pub has_applied_mask: bool,
    /// The layer's type code: 0 RGB, 1 RGBA, 2 grey, 3 grey + alpha.
    kind: u32,
    hierarchy: u64,
    mask: u64,
}

/// A parsed `.xcf`: the canvas and the layer tree, with pixels read on demand.
#[derive(Debug, Clone)]
pub struct XcfDocument<'a> {
    bytes: &'a [u8],
    pub version: u32,
    pub width: u32,
    pub height: u32,
    /// `true` for a greyscale image.
    pub grey: bool,
    /// `true` for GIMP's 8-bit linear precision.
    pub linear: bool,
    /// Top-level items, **top first**.
    pub layers: Vec<XcfLayer>,
    /// What the flattening approximates or leaves out; see the module docs.
    pub notes: Vec<String>,
    compression: u8,
}

/// A bounds-checked big-endian reader over the whole file.
struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
    wide: bool,
}

impl<'a> Reader<'a> {
    fn at(bytes: &'a [u8], pos: u64, wide: bool) -> Result<Self, CodecError> {
        let pos = usize::try_from(pos).map_err(|_| malformed(NAME, "an offset overflows"))?;
        if pos > bytes.len() {
            return Err(malformed(NAME, format!("offset {pos} is past the end of the file")));
        }
        Ok(Reader { bytes, pos, wide })
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], CodecError> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|e| *e <= self.bytes.len())
            .ok_or_else(|| malformed(NAME, "the file ends early"))?;
        let s = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    fn u32(&mut self) -> Result<u32, CodecError> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn i32(&mut self) -> Result<i32, CodecError> {
        Ok(self.u32()? as i32)
    }

    /// A file offset: 32 bits before version 11, 64 from then on.
    fn offset(&mut self) -> Result<u64, CodecError> {
        if self.wide {
            let b = self.take(8)?;
            Ok(u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
        } else {
            Ok(u64::from(self.u32()?))
        }
    }

    /// A GIMP string: a length that counts the terminating NUL, then bytes.
    fn string(&mut self) -> Result<String, CodecError> {
        let len = self.u32()? as usize;
        let raw = self.take(len)?;
        let raw = raw.strip_suffix(&[0]).unwrap_or(raw);
        Ok(String::from_utf8_lossy(raw).into_owned())
    }

    /// Walk a property list, handing each `(type, payload)` to `each`.
    fn properties(
        &mut self,
        version: u32,
        mut each: impl FnMut(u32, &'a [u8]) -> Result<(), CodecError>,
    ) -> Result<(), CodecError> {
        for _ in 0..MAX_PROPERTIES {
            let kind = self.u32()?;
            let mut size = self.u32()? as usize;
            if kind == PROP_END {
                return Ok(());
            }
            if kind == PROP_COLORMAP && version == 0 {
                // Version 0 wrote a wrong length for the colour map; GIMP
                // reads the colour count instead, and so does this.
                let save = self.pos;
                let n = self.u32()? as usize;
                self.pos = save;
                size = 4 + 3 * n.min(256);
            }
            let payload = self.take(size)?;
            each(kind, payload)?;
        }
        Err(malformed(NAME, "a property list does not end"))
    }
}

fn be_u32(payload: &[u8], at: usize) -> Option<u32> {
    payload
        .get(at..at + 4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// Header facts without reading any layer.
pub fn probe(bytes: &[u8], limits: ImportLimits) -> Result<ImageInfo, CodecError> {
    let (_, width, height, _, _) = header(bytes)?;
    limits.check_dimensions(width, height)?;
    Ok(super::info(width, height, ImportFormat::Xcf, false))
}

/// `(version, width, height, grey, linear)` from the fixed header, refusing
/// indexed images and every precision above 8 bits by name.
fn header(bytes: &[u8]) -> Result<(u32, u32, u32, bool, bool), CodecError> {
    if !looks_like_xcf(bytes) || bytes.len() < 14 {
        return Err(malformed(NAME, "no `gimp xcf` magic"));
    }
    let id = &bytes[9..14];
    let version = if id == b"file\0" {
        0
    } else if id[0] == b'v' && id[4] == 0 && id[1..4].iter().all(u8::is_ascii_digit) {
        id[1..4]
            .iter()
            .fold(0u32, |v, d| v * 10 + u32::from(d - b'0'))
    } else {
        return Err(malformed(NAME, "an unreadable version tag"));
    };
    let mut r = Reader::at(bytes, 14, false)?;
    let width = r.u32()?;
    let height = r.u32()?;
    let grey = match r.u32()? {
        0 => false,
        1 => true,
        2 => {
            return Err(CodecError::Unsupported(
                "indexed-colour XCF images are not supported; convert the image to RGB in \
                 GIMP first"
                    .into(),
            ))
        }
        other => return Err(malformed(NAME, format!("image type {other}"))),
    };
    let mut linear = false;
    if version >= 4 {
        let precision = r.u32()?;
        let ok = match version {
            4 => precision == 0,
            5 | 6 => {
                linear = precision == 100;
                matches!(precision, 100 | 150)
            }
            _ => {
                linear = precision == 100;
                matches!(precision, 100 | 150 | 175)
            }
        };
        if !ok {
            return Err(CodecError::Unsupported(format!(
                "XCF precision code {precision} (version {version}) is not supported: only \
                 8-bit images are read; convert the image to 8 bits per channel in GIMP first"
            )));
        }
    }
    Ok((version, width, height, grey, linear))
}

/// Parse the document structure. No pixels are read yet.
pub fn read(bytes: &[u8], limits: ImportLimits) -> Result<XcfDocument<'_>, CodecError> {
    let (version, width, height, grey, linear) = header(bytes)?;
    limits.check_dimensions(width, height)?;
    let wide = version >= 11;
    let mut r = Reader::at(bytes, if version >= 4 { 30 } else { 26 }, wide)?;

    let mut compression = 0u8;
    r.properties(version, |kind, payload| {
        if kind == PROP_COMPRESSION {
            compression = payload.first().copied().unwrap_or(0);
        }
        Ok(())
    })?;
    if compression > 2 {
        return Err(CodecError::Unsupported(format!(
            "XCF tile compression {compression} is not supported"
        )));
    }

    let mut offsets = Vec::new();
    loop {
        let offset = r.offset()?;
        if offset == 0 {
            break;
        }
        if offsets.len() >= MAX_LAYERS {
            return Err(CodecError::LimitExceeded(format!(
                "the XCF lists more than {MAX_LAYERS} layers"
            )));
        }
        offsets.push(offset);
    }

    let mut notes = Vec::new();
    let mut roots: Vec<XcfLayer> = Vec::new();
    for offset in offsets {
        let (layer, path, floating) = read_layer(bytes, offset, version, wide)?;
        if floating {
            notes.push(format!(
                "the floating selection {:?} was not drawn",
                layer.name
            ));
            continue;
        }
        match layer.mode {
            XcfMode::Unsupported(code) => notes.push(format!(
                "layer {:?} uses GIMP blend mode {code}, which is drawn as Normal",
                layer.name
            )),
            XcfMode::PassThrough => notes.push(format!(
                "group {:?} passes through; its children are drawn straight onto what is \
                 below it, each with the group's opacity folded in",
                layer.name
            )),
            _ if layer.mode_code == 1 => notes.push(format!(
                "layer {:?} uses Dissolve, which is drawn as Normal",
                layer.name
            )),
            _ => {}
        }
        insert(&mut roots, &path, layer, &mut notes)?;
    }
    if linear {
        notes.push("the image is 8-bit linear; it was converted to sRGB as it was read".into());
    }
    Ok(XcfDocument {
        bytes,
        version,
        width,
        height,
        grey,
        linear,
        layers: roots,
        notes,
        compression,
    })
}

/// Place `layer` under the group its item path names (all but the last
/// index), or at the top level when there is no path.
fn insert(
    roots: &mut Vec<XcfLayer>,
    path: &[u32],
    layer: XcfLayer,
    notes: &mut Vec<String>,
) -> Result<(), CodecError> {
    if path.len() > MAX_GROUP_DEPTH + 1 {
        return Err(CodecError::LimitExceeded(format!(
            "XCF groups nest deeper than {MAX_GROUP_DEPTH}"
        )));
    }
    let mut siblings = roots;
    if let Some((_, parents)) = path.split_last() {
        for index in parents {
            let found = siblings
                .get_mut(*index as usize)
                .filter(|g| g.is_group)
                .map(|g| &mut g.children);
            match found {
                Some(children) => siblings = children,
                None => {
                    notes.push(format!(
                        "layer {:?} names a group that does not exist; it was placed at the \
                         top level",
                        layer.name
                    ));
                    // Restart at the root: `siblings` cannot be re-borrowed
                    // from `roots` here, so return through a second call.
                    return insert_top(siblings_root(siblings), layer);
                }
            }
        }
    }
    siblings.push(layer);
    Ok(())
}

/// Helper for [`insert`]'s fallback; see there.
fn siblings_root(v: &mut Vec<XcfLayer>) -> &mut Vec<XcfLayer> {
    v
}

fn insert_top(v: &mut Vec<XcfLayer>, layer: XcfLayer) -> Result<(), CodecError> {
    v.push(layer);
    Ok(())
}

/// One layer record: `(layer, item path, is the floating selection)`.
fn read_layer(
    bytes: &[u8],
    offset: u64,
    version: u32,
    wide: bool,
) -> Result<(XcfLayer, Vec<u32>, bool), CodecError> {
    let mut r = Reader::at(bytes, offset, wide)?;
    let width = r.u32()?;
    let height = r.u32()?;
    let kind = r.u32()?;
    if kind > 3 {
        return Err(CodecError::Unsupported(format!(
            "XCF layer type {kind} (indexed) is not supported"
        )));
    }
    let name = r.string()?;
    let mut layer = XcfLayer {
        name,
        x: 0,
        y: 0,
        width,
        height,
        opacity: 1.0,
        visible: true,
        mode: XcfMode::Normal,
        mode_code: 0,
        is_group: false,
        children: Vec::new(),
        has_applied_mask: false,
        kind,
        hierarchy: 0,
        mask: 0,
    };
    let mut path = Vec::new();
    let mut floating = false;
    let mut apply_mask = false;
    r.properties(version, |prop, payload| {
        match prop {
            PROP_OPACITY => {
                if let Some(v) = be_u32(payload, 0) {
                    layer.opacity = (v.min(255) as f32) / 255.0;
                }
            }
            PROP_FLOAT_OPACITY => {
                if let Some(v) = be_u32(payload, 0) {
                    let f = f32::from_bits(v);
                    if f.is_finite() {
                        layer.opacity = f.clamp(0.0, 1.0);
                    }
                }
            }
            PROP_VISIBLE => layer.visible = be_u32(payload, 0).is_some_and(|v| v != 0),
            PROP_MODE => {
                if let Some(code) = be_u32(payload, 0) {
                    layer.mode_code = code;
                    layer.mode = XcfMode::from_code(code);
                }
            }
            PROP_OFFSETS => {
                if let (Some(x), Some(y)) = (be_u32(payload, 0), be_u32(payload, 4)) {
                    layer.x = x as i32;
                    layer.y = y as i32;
                }
            }
            PROP_APPLY_MASK => apply_mask = be_u32(payload, 0).is_some_and(|v| v != 0),
            PROP_GROUP_ITEM => layer.is_group = true,
            PROP_FLOATING_SELECTION => floating = true,
            PROP_ITEM_PATH => {
                path = payload
                    .chunks_exact(4)
                    .take(MAX_GROUP_DEPTH + 2)
                    .map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]]))
                    .collect();
            }
            _ => {}
        }
        Ok(())
    })?;
    layer.hierarchy = r.offset()?;
    layer.mask = r.offset()?;
    layer.has_applied_mask = apply_mask && layer.mask != 0;
    Ok((layer, path, floating))
}

impl XcfDocument<'_> {
    /// A layer's own pixels as straight sRGB RGBA8, `width * height * 4`
    /// bytes, with its mask applied when the file says to apply it. A group
    /// has none (its pixels are its children) and returns an empty buffer.
    pub fn layer_pixels(&self, layer: &XcfLayer, limits: ImportLimits) -> Result<Vec<u8>, CodecError> {
        if layer.is_group || layer.width == 0 || layer.height == 0 {
            return Ok(Vec::new());
        }
        limits.check_dimensions(layer.width, layer.height)?;
        let channels = match layer.kind {
            0 => 3,
            1 => 4,
            2 => 1,
            _ => 2,
        };
        let raw = self.read_hierarchy(layer.hierarchy, layer.width, layer.height, channels)?;
        let pixels = layer.width as usize * layer.height as usize;
        let mut rgba = Vec::with_capacity(pixels * 4);
        for px in raw.chunks_exact(channels) {
            let (rgb, a) = match channels {
                1 => ([px[0]; 3], 255),
                2 => ([px[0]; 3], px[1]),
                3 => ([px[0], px[1], px[2]], 255),
                _ => ([px[0], px[1], px[2]], px[3]),
            };
            rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], a]);
        }
        drop(raw);
        if self.linear {
            let lut = linear_to_srgb_lut();
            for px in rgba.chunks_exact_mut(4) {
                for c in &mut px[..3] {
                    *c = lut[*c as usize];
                }
            }
        }
        if layer.has_applied_mask {
            let mask = self.read_mask(layer)?;
            for (px, m) in rgba.chunks_exact_mut(4).zip(mask) {
                px[3] = ((u32::from(px[3]) * u32::from(m) + 127) / 255) as u8;
            }
        }
        Ok(rgba)
    }

    /// A layer mask's 8-bit plane, the layer's own size.
    fn read_mask(&self, layer: &XcfLayer) -> Result<Vec<u8>, CodecError> {
        let mut r = Reader::at(self.bytes, layer.mask, self.version >= 11)?;
        let width = r.u32()?;
        let height = r.u32()?;
        if (width, height) != (layer.width, layer.height) {
            return Err(malformed(NAME, "a layer mask is not the size of its layer"));
        }
        let _name = r.string()?;
        r.properties(self.version, |_, _| Ok(()))?;
        let hierarchy = r.offset()?;
        let mut plane = self.read_hierarchy(hierarchy, width, height, 1)?;
        if self.linear {
            let lut = linear_to_srgb_lut();
            for v in &mut plane {
                *v = lut[*v as usize];
            }
        }
        Ok(plane)
    }

    /// Read a hierarchy's top level into interleaved 8-bit samples.
    fn read_hierarchy(
        &self,
        offset: u64,
        width: u32,
        height: u32,
        channels: usize,
    ) -> Result<Vec<u8>, CodecError> {
        let wide = self.version >= 11;
        let mut r = Reader::at(self.bytes, offset, wide)?;
        let (hw, hh, bpp) = (r.u32()?, r.u32()?, r.u32()? as usize);
        if (hw, hh) != (width, height) || bpp != channels {
            return Err(malformed(
                NAME,
                format!(
                    "a hierarchy of {hw}x{hh}x{bpp} does not match its {width}x{height}x{channels} \
                     drawable"
                ),
            ));
        }
        let level = r.offset()?;
        let mut r = Reader::at(self.bytes, level, wide)?;
        if (r.u32()?, r.u32()?) != (width, height) {
            return Err(malformed(NAME, "a level does not match its hierarchy"));
        }
        let (w, h) = (width as usize, height as usize);
        let mut out = vec![0u8; w * h * channels];
        let (cols, rows) = (w.div_ceil(TILE), h.div_ceil(TILE));
        let max_tile_data = TILE * TILE * channels * 3 / 2;
        let mut offset = r.offset()?;
        if offset == 0 {
            // An empty level: GIMP leaves the drawable transparent.
            return Ok(out);
        }
        let mut tile = Vec::with_capacity(TILE * TILE * channels);
        for i in 0..cols * rows {
            if offset == 0 {
                return Err(malformed(NAME, "a level lists too few tiles"));
            }
            let next = r.offset()?;
            let start = usize::try_from(offset).unwrap_or(usize::MAX);
            if start > self.bytes.len() {
                return Err(malformed(NAME, "a tile offset is past the end of the file"));
            }
            let end = if next == 0 {
                start.saturating_add(max_tile_data)
            } else {
                usize::try_from(next).unwrap_or(usize::MAX)
            };
            if end < start || end - start > max_tile_data {
                return Err(malformed(NAME, "a tile's data length is out of range"));
            }
            let data = &self.bytes[start..end.min(self.bytes.len())];
            let (tx, ty) = ((i % cols) * TILE, (i / cols) * TILE);
            let (tw, th) = (TILE.min(w - tx), TILE.min(h - ty));
            let size = tw * th * channels;
            tile.clear();
            tile.resize(size, 0);
            match self.compression {
                0 => {
                    let src = data
                        .get(..size)
                        .ok_or_else(|| malformed(NAME, "an uncompressed tile is short"))?;
                    tile.copy_from_slice(src);
                }
                1 => rle_tile(data, &mut tile, tw * th, channels)?,
                _ => zlib_tile(data, &mut tile)?,
            }
            for row in 0..th {
                let dst = ((ty + row) * w + tx) * channels;
                let src = row * tw * channels;
                out[dst..dst + tw * channels].copy_from_slice(&tile[src..src + tw * channels]);
            }
            offset = next;
        }
        Ok(out)
    }

    /// Flatten the visible layers onto a transparent canvas: straight sRGB
    /// RGBA8, `width * height * 4` bytes.
    pub fn flatten(&self, limits: ImportLimits) -> Result<Vec<u8>, CodecError> {
        let canvas_bytes = u64::from(self.width) * u64::from(self.height) * 4;
        check_decode(limits, self.width, self.height, 4, 0)?;
        let mut canvas = vec![0f32; 0];
        drop(std::mem::take(&mut canvas));
        let mut out = vec![0u8; canvas_bytes as usize];
        self.composite(&self.layers, &mut out, 1.0, 1, limits)?;
        Ok(out)
    }

    /// Draw `items` (top first) onto `canvas`, bottom first. `level` is how
    /// many canvas-sized buffers are live, for the allocation check.
    fn composite(
        &self,
        items: &[XcfLayer],
        canvas: &mut [u8],
        opacity: f32,
        level: u64,
        limits: ImportLimits,
    ) -> Result<(), CodecError> {
        if level as usize > MAX_GROUP_DEPTH + 1 {
            return Err(CodecError::LimitExceeded(format!(
                "XCF groups nest deeper than {MAX_GROUP_DEPTH}"
            )));
        }
        let canvas_bytes = u64::from(self.width) * u64::from(self.height) * 4;
        for item in items.iter().rev() {
            if !item.visible {
                continue;
            }
            let item_opacity = item.opacity * opacity;
            if item.is_group {
                if item.mode == XcfMode::PassThrough {
                    self.composite(&item.children, canvas, item_opacity, level, limits)?;
                    continue;
                }
                limits.check_alloc(canvas_bytes.saturating_mul(level + 1))?;
                let mut group = vec![0u8; canvas.len()];
                self.composite(&item.children, &mut group, 1.0, level + 1, limits)?;
                let full = XcfLayer {
                    x: 0,
                    y: 0,
                    width: self.width,
                    height: self.height,
                    ..item.clone()
                };
                self.blend_onto(canvas, &group, &full, item_opacity);
                continue;
            }
            let layer_bytes = u64::from(item.width) * u64::from(item.height) * 4;
            limits.check_alloc(canvas_bytes.saturating_mul(level).saturating_add(layer_bytes))?;
            let pixels = self.layer_pixels(item, limits)?;
            if pixels.is_empty() {
                continue;
            }
            self.blend_onto(canvas, &pixels, item, item_opacity);
        }
        Ok(())
    }

    /// Source-over `src` (placed per `geometry`) onto `canvas` with
    /// `geometry.mode` at `opacity`, clipped to the canvas.
    fn blend_onto(&self, canvas: &mut [u8], src: &[u8], geometry: &XcfLayer, opacity: f32) {
        let (cw, ch) = (i64::from(self.width), i64::from(self.height));
        let (lw, lh) = (i64::from(geometry.width), i64::from(geometry.height));
        let (ox, oy) = (i64::from(geometry.x), i64::from(geometry.y));
        let x0 = ox.max(0);
        let y0 = oy.max(0);
        let x1 = (ox + lw).min(cw);
        let y1 = (oy + lh).min(ch);
        let mode = geometry.mode;
        for y in y0..y1 {
            for x in x0..x1 {
                let s = (((y - oy) * lw + (x - ox)) * 4) as usize;
                let d = ((y * cw + x) * 4) as usize;
                let sa = f32::from(src[s + 3]) / 255.0 * opacity;
                if sa <= 0.0 {
                    continue;
                }
                let ba = f32::from(canvas[d + 3]) / 255.0;
                let oa = sa + ba * (1.0 - sa);
                for c in 0..3 {
                    let cs = f32::from(src[s + c]) / 255.0;
                    let cb = f32::from(canvas[d + c]) / 255.0;
                    let mixed = (1.0 - ba) * cs + ba * mode.blend(cb, cs);
                    let co = (sa * mixed + ba * (1.0 - sa) * cb) / oa;
                    canvas[d + c] = (co * 255.0).round().clamp(0.0, 255.0) as u8;
                }
                canvas[d + 3] = (oa * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

/// Decode an RLE tile: each channel is a separate run-length stream filling
/// every `channels`-th byte of `tile`.
fn rle_tile(data: &[u8], tile: &mut [u8], pixels: usize, channels: usize) -> Result<(), CodecError> {
    let bogus = || malformed(NAME, "an RLE tile is damaged");
    let mut pos = 0usize;
    let mut next = |pos: &mut usize| -> Result<u8, CodecError> {
        let v = *data.get(*pos).ok_or_else(bogus)?;
        *pos += 1;
        Ok(v)
    };
    for channel in 0..channels {
        let mut filled = 0usize;
        while filled < pixels {
            let op = next(&mut pos)?;
            if op >= 128 {
                // A literal run of `256 - op` bytes, or a long one.
                let mut len = 256 - usize::from(op);
                if len == 128 {
                    len = (usize::from(next(&mut pos)?) << 8) | usize::from(next(&mut pos)?);
                }
                if filled + len > pixels {
                    return Err(bogus());
                }
                for _ in 0..len {
                    tile[filled * channels + channel] = next(&mut pos)?;
                    filled += 1;
                }
            } else {
                // A repeated byte, `op + 1` times, or a long run.
                let mut len = usize::from(op) + 1;
                if len == 128 {
                    len = (usize::from(next(&mut pos)?) << 8) | usize::from(next(&mut pos)?);
                }
                if filled + len > pixels {
                    return Err(bogus());
                }
                let v = next(&mut pos)?;
                for _ in 0..len {
                    tile[filled * channels + channel] = v;
                    filled += 1;
                }
            }
        }
    }
    Ok(())
}

/// Inflate a zlib tile into exactly `tile.len()` interleaved bytes.
fn zlib_tile(data: &[u8], tile: &mut [u8]) -> Result<(), CodecError> {
    let mut decoder = flate2::read::ZlibDecoder::new(data).take(tile.len() as u64);
    decoder
        .read_exact(tile)
        .map_err(|e| malformed(NAME, format!("a zlib tile is damaged: {e}")))
}

/// 8-bit linear to 8-bit sRGB.
fn linear_to_srgb_lut() -> [u8; 256] {
    let mut lut = [0u8; 256];
    for (i, v) in lut.iter_mut().enumerate() {
        let l = i as f64 / 255.0;
        let s = if l <= 0.003_130_8 {
            12.92 * l
        } else {
            1.055 * l.powf(1.0 / 2.4) - 0.055
        };
        *v = (s * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    lut
}

/// Decode a `.xcf` to its flattened composite.
pub fn decode(bytes: &[u8], limits: ImportLimits) -> Result<DecodedSurface, CodecError> {
    let doc = read(bytes, limits)?;
    let rgba = doc.flatten(limits)?;
    Ok(rgba8_surface(doc.width, doc.height, rgba, ImportFormat::Xcf))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::codec::SurfacePixels;
    use std::io::Write;

    /// One layer for the test writer.
    pub(crate) struct TestLayer {
        pub name: &'static str,
        pub x: i32,
        pub y: i32,
        pub width: u32,
        pub height: u32,
        /// 0 RGB, 1 RGBA, 2 grey, 3 grey + alpha.
        pub kind: u32,
        /// Interleaved samples, `width * height * channels`.
        pub pixels: Vec<u8>,
        pub opacity: u32,
        pub visible: bool,
        pub mode: u32,
        pub group: bool,
        pub path: Vec<u32>,
        pub mask: Option<Vec<u8>>,
    }

    impl TestLayer {
        pub fn rgba(name: &'static str, x: i32, y: i32, w: u32, h: u32, px: [u8; 4]) -> Self {
            TestLayer {
                name,
                x,
                y,
                width: w,
                height: h,
                kind: 1,
                pixels: px.repeat((w * h) as usize),
                opacity: 255,
                visible: true,
                mode: 28,
                group: false,
                path: Vec::new(),
                mask: None,
            }
        }
    }

    fn rle_encode(plane: &[u8]) -> Vec<u8> {
        // Short literal runs only, plus one long repeat when a plane is flat:
        // exercises both op families and the 16-bit length.
        let mut out = Vec::new();
        if plane.len() >= 128 && plane.iter().all(|b| *b == plane[0]) {
            out.push(127);
            out.extend_from_slice(&(plane.len() as u16).to_be_bytes());
            out.push(plane[0]);
            return out;
        }
        for chunk in plane.chunks(127) {
            if chunk.len() > 1 && chunk.iter().all(|b| *b == chunk[0]) {
                out.push((chunk.len() - 1) as u8);
                out.push(chunk[0]);
            } else {
                out.push((256 - chunk.len()) as u8);
                out.extend_from_slice(chunk);
            }
        }
        out
    }

    /// Write an XCF. `version` 0 writes `gimp xcf file` with 32-bit offsets;
    /// 11 writes `v011` with 64-bit offsets and a precision field.
    pub(crate) fn write_xcf(
        version: u32,
        width: u32,
        height: u32,
        grey: bool,
        compression: u8,
        layers: &[TestLayer],
    ) -> Vec<u8> {
        let wide = version >= 11;
        let mut f: Vec<u8> = Vec::new();
        if version == 0 {
            f.extend_from_slice(b"gimp xcf file\0");
        } else {
            f.extend_from_slice(format!("gimp xcf v{version:03}\0").as_bytes());
        }
        let u32w = |f: &mut Vec<u8>, v: u32| f.extend_from_slice(&v.to_be_bytes());
        u32w(&mut f, width);
        u32w(&mut f, height);
        u32w(&mut f, u32::from(grey));
        if version >= 4 {
            u32w(&mut f, 150);
        }
        u32w(&mut f, PROP_COMPRESSION);
        u32w(&mut f, 1);
        f.push(compression);
        u32w(&mut f, 0);
        u32w(&mut f, 0);
        let osize = if wide { 8 } else { 4 };
        // Offset table: layers + 0, then channels: 0.
        let table = f.len();
        f.resize(table + osize * (layers.len() + 2), 0);
        let put_offset = |f: &mut Vec<u8>, at: usize, v: u64| {
            if wide {
                f[at..at + 8].copy_from_slice(&v.to_be_bytes());
            } else {
                f[at..at + 4].copy_from_slice(&(v as u32).to_be_bytes());
            }
        };
        let write_hierarchy = |f: &mut Vec<u8>, w: u32, h: u32, ch: usize, px: &[u8]| -> u64 {
            let at = f.len() as u64;
            u32w(f, w);
            u32w(f, h);
            u32w(f, ch as u32);
            let level_slot = f.len();
            f.resize(level_slot + osize * 2, 0);
            let level = f.len() as u64;
            put_offset(f, level_slot, level);
            u32w(f, w);
            u32w(f, h);
            let (wu, hu) = (w as usize, h as usize);
            let (cols, rows) = (wu.div_ceil(TILE), hu.div_ceil(TILE));
            let slots = f.len();
            f.resize(slots + osize * (cols * rows + 1), 0);
            for i in 0..cols * rows {
                let (tx, ty) = ((i % cols) * TILE, (i / cols) * TILE);
                let (tw, th) = (TILE.min(wu - tx), TILE.min(hu - ty));
                let mut tile = Vec::new();
                for row in 0..th {
                    let s = ((ty + row) * wu + tx) * ch;
                    tile.extend_from_slice(&px[s..s + tw * ch]);
                }
                let here = f.len() as u64;
                put_offset(f, slots + i * osize, here);
                match compression {
                    0 => f.extend_from_slice(&tile),
                    1 => {
                        for c in 0..ch {
                            let plane: Vec<u8> = tile.iter().skip(c).step_by(ch).copied().collect();
                            f.extend(rle_encode(&plane));
                        }
                    }
                    _ => {
                        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
                        z.write_all(&tile).unwrap();
                        f.extend(z.finish().unwrap());
                    }
                }
            }
            at
        };
        for (i, l) in layers.iter().enumerate() {
            let here = f.len() as u64;
            put_offset(&mut f, table + i * osize, here);
            u32w(&mut f, l.width);
            u32w(&mut f, l.height);
            u32w(&mut f, l.kind);
            u32w(&mut f, l.name.len() as u32 + 1);
            f.extend_from_slice(l.name.as_bytes());
            f.push(0);
            for (prop, payload) in [
                (PROP_OPACITY, l.opacity.to_be_bytes().to_vec()),
                (PROP_VISIBLE, u32::from(l.visible).to_be_bytes().to_vec()),
                (PROP_MODE, l.mode.to_be_bytes().to_vec()),
                (
                    PROP_OFFSETS,
                    [l.x.to_be_bytes(), l.y.to_be_bytes()].concat(),
                ),
                (PROP_APPLY_MASK, u32::from(l.mask.is_some()).to_be_bytes().to_vec()),
            ] {
                u32w(&mut f, prop);
                u32w(&mut f, payload.len() as u32);
                f.extend_from_slice(&payload);
            }
            if l.group {
                u32w(&mut f, PROP_GROUP_ITEM);
                u32w(&mut f, 0);
            }
            if !l.path.is_empty() {
                u32w(&mut f, PROP_ITEM_PATH);
                u32w(&mut f, 4 * l.path.len() as u32);
                for p in &l.path {
                    u32w(&mut f, *p);
                }
            }
            u32w(&mut f, PROP_END);
            u32w(&mut f, 0);
            let slots = f.len();
            f.resize(slots + 2 * osize, 0);
            let ch = [3, 4, 1, 2][l.kind as usize];
            let h = write_hierarchy(&mut f, l.width, l.height, ch, &l.pixels);
            put_offset(&mut f, slots, h);
            if let Some(mask) = &l.mask {
                let m = f.len() as u64;
                u32w(&mut f, l.width);
                u32w(&mut f, l.height);
                u32w(&mut f, 5);
                f.extend_from_slice(b"mask\0");
                u32w(&mut f, PROP_END);
                u32w(&mut f, 0);
                let slot = f.len();
                f.resize(slot + osize, 0);
                let mh = write_hierarchy(&mut f, l.width, l.height, 1, mask);
                put_offset(&mut f, slot, mh);
                put_offset(&mut f, slots + osize, m);
            }
        }
        f
    }

    fn px(s: &DecodedSurface, x: u32, y: u32) -> [u8; 4] {
        let SurfacePixels::Rgba8(v) = &s.pixels else {
            panic!("8-bit")
        };
        let i = ((y * s.width + x) * 4) as usize;
        [v[i], v[i + 1], v[i + 2], v[i + 3]]
    }

    fn two_layers() -> Vec<TestLayer> {
        // Top first, as GIMP lists them.
        let mut top = TestLayer::rgba("top", 2, 1, 3, 2, [0, 0, 255, 255]);
        top.opacity = 128;
        let mut bottom = TestLayer::rgba("bottom", 0, 0, 70, 66, [255, 0, 0, 255]);
        bottom.kind = 0;
        bottom.pixels = [255u8, 0, 0].repeat(70 * 66);
        // A gradient on the bottom layer so tiles differ.
        for (i, p) in bottom.pixels.chunks_mut(3).enumerate() {
            p[1] = (i % 70) as u8;
        }
        vec![top, bottom]
    }

    #[test]
    fn layers_offsets_opacity_decode_under_every_compression_and_offset_width() {
        for version in [0, 11] {
            for compression in [0u8, 1, 2] {
                let file = write_xcf(version, 70, 66, false, compression, &two_layers());
                let s = decode(&file, ImportLimits::default())
                    .unwrap_or_else(|e| panic!("v{version} c{compression}: {e}"));
                assert_eq!((s.width, s.height), (70, 66));
                // Outside the top layer: the bottom layer, with its gradient.
                assert_eq!(px(&s, 0, 0), [255, 0, 0, 255]);
                assert_eq!(px(&s, 69, 65), [255, 69, 0, 255], "v{version} c{compression}");
                // Under the half-opaque top layer at (2,1)..(5,3): a mix.
                let [r, g, b, a] = px(&s, 3, 2);
                assert_eq!(a, 255);
                assert!((i32::from(r) - 127).abs() <= 1 && (i32::from(b) - 128).abs() <= 1);
                assert!(g <= 2, "{g}");
                assert_eq!(px(&s, 5, 2)[2], 0, "the top layer ends at x=5");
            }
        }
    }

    #[test]
    fn visibility_modes_masks_and_groups() {
        let mut hidden = TestLayer::rgba("hidden", 0, 0, 4, 4, [0, 255, 0, 255]);
        hidden.visible = false;
        let mut multiply = TestLayer::rgba("mul", 0, 0, 2, 4, [128, 128, 128, 255]);
        multiply.mode = 30;
        let mut masked = TestLayer::rgba("masked", 2, 0, 2, 4, [0, 0, 0, 255]);
        // Mask: left column hidden, right column shown.
        masked.mask = Some(vec![0, 255, 0, 255, 0, 255, 0, 255]);
        // A group holding one layer, at half opacity.
        let mut group = TestLayer::rgba("group", 0, 3, 4, 1, [0, 0, 0, 0]);
        group.group = true;
        group.opacity = 128;
        group.pixels = Vec::new();
        group.width = 0;
        group.height = 0;
        let mut inner = TestLayer::rgba("inner", 0, 3, 4, 1, [0, 0, 255, 255]);
        inner.path = vec![0, 0];
        let bottom = TestLayer::rgba("white", 0, 0, 4, 4, [255, 255, 255, 255]);
        let file = write_xcf(
            11,
            4,
            4,
            false,
            1,
            &[group, inner, hidden, multiply, masked, bottom],
        );
        let doc = read(&file, ImportLimits::default()).unwrap();
        assert_eq!(doc.layers.len(), 5, "the inner layer is inside the group");
        assert_eq!(doc.layers[0].children.len(), 1);
        assert_eq!(doc.layers[0].children[0].name, "inner");
        let s = decode(&file, ImportLimits::default()).unwrap();
        // Multiply 128 grey over white.
        assert_eq!(px(&s, 0, 0), [128, 128, 128, 255]);
        // The hidden green layer never shows; the mask hides x=2 and shows x=3.
        assert_eq!(px(&s, 2, 0), [255, 255, 255, 255]);
        assert_eq!(px(&s, 3, 0), [0, 0, 0, 255]);
        // Row 3: the group's blue at half opacity over the row below.
        let [r, _, b, _] = px(&s, 0, 3);
        assert!(b > 180 && r < 80, "{:?}", px(&s, 0, 3));
    }

    #[test]
    fn greyscale_images_decode() {
        let mut l = TestLayer::rgba("g", 0, 0, 2, 1, [0; 4]);
        l.kind = 3;
        l.pixels = vec![100, 255, 200, 0];
        let file = write_xcf(0, 2, 1, true, 1, &[l]);
        let s = decode(&file, ImportLimits::default()).unwrap();
        assert_eq!(px(&s, 0, 0), [100, 100, 100, 255]);
        assert_eq!(px(&s, 1, 0)[3], 0);
    }

    #[test]
    fn unsupported_features_are_refused_or_reported_by_name() {
        let mut file = write_xcf(11, 4, 4, false, 1, &[TestLayer::rgba("a", 0, 0, 4, 4, [1; 4])]);
        // Indexed images are refused by name.
        let mut indexed = file.clone();
        indexed[14 + 11] = 2;
        let err = decode(&indexed, ImportLimits::default()).unwrap_err();
        assert!(err.to_string().contains("indexed"), "{err}");
        // A 16-bit precision is refused by name.
        file[26..30].copy_from_slice(&250u32.to_be_bytes());
        let err = decode(&file, ImportLimits::default()).unwrap_err();
        assert!(err.to_string().contains("precision code 250"), "{err}");
        // An unknown blend mode is drawn as Normal and noted.
        let mut odd = TestLayer::rgba("odd", 0, 0, 1, 1, [9; 4]);
        odd.mode = 52;
        let file = write_xcf(0, 1, 1, false, 0, &[odd]);
        let doc = read(&file, ImportLimits::default()).unwrap();
        assert!(doc.notes.iter().any(|n| n.contains("blend mode 52")), "{:?}", doc.notes);
    }

    #[test]
    fn malformed_files_error_and_never_panic() {
        for version in [0, 11] {
            for compression in [0u8, 1, 2] {
                let good = write_xcf(version, 70, 66, false, compression, &two_layers());
                for n in (0..good.len()).step_by(7) {
                    let _ = decode(&good[..n], ImportLimits::default());
                }
                for i in (0..good.len()).step_by(3) {
                    let mut bad = good.clone();
                    bad[i] ^= 0x5a;
                    let _ = decode(&bad, ImportLimits::default());
                }
            }
        }
        // A huge canvas in a tiny file is refused before allocating.
        let mut huge = write_xcf(0, 1, 1, false, 0, &[]);
        huge[14..18].copy_from_slice(&60_000u32.to_be_bytes());
        huge[18..22].copy_from_slice(&60_000u32.to_be_bytes());
        assert!(decode(&huge, ImportLimits::default()).is_err());
        assert!(decode(b"gimp xcf ", ImportLimits::default()).is_err());
    }
}
