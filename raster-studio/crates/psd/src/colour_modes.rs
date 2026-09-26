//! W16-B: the colour modes that are not RGB — Bitmap, Indexed, CMYK, Lab,
//! Multichannel and Duotone — on the way in and out.
//!
//! This crate knows the *format*: where each mode keeps its samples and what
//! a sample value means. It does not own a colour model, so the conversions
//! between a mode and the editor's RGB working buffer take the colour science
//! from the caller ([`WorkingScience`] in, [`Separation`] out); the
//! application passes its own CMYK and Lab transforms, which is what makes a
//! file opened in CMYK and saved again come back as the same numbers.
//!
//! What each mode stores (Adobe's file-format specification):
//!
//! | Mode | Channels | A sample means |
//! |---|---|---|
//! | Bitmap (0) | 1, **1 bit**, rows padded to a byte | `1` black, `0` white |
//! | Greyscale (1) | 1 | `0` black .. max white |
//! | Indexed (2) | 1, 8-bit only | an index into the 768-byte planar palette in the colour mode data (256 reds, 256 greens, 256 blues); resource 1046 holds the colour count and 1047 the transparent index |
//! | CMYK (4) | 4 | ink, **inverted**: `0` is 100 % ink, max is none |
//! | Multichannel (7) | n inks, no transparency | inverted ink, per channel |
//! | Duotone (8) | 1 | the greyscale base; the inks and their curves are in the colour mode data |
//! | Lab (9) | 3 | `L` `0..=max` is 0..100; `a`, `b` are offset so the midpoint (128 or 32768) is 0 |
//!
//! Everything here is bounded: a palette is exactly 768 bytes or the file is
//! refused, a duotone record is parsed through checked reads and a malformed
//! one is reported rather than guessed, and every plane is length-checked
//! before it is converted.

use crate::error::{PsdError, PsdResult};
use crate::header::{ColorMode, Depth, PsdHeader};
use crate::limits::Budget;
use crate::model::CHANNEL_ALPHA;
use crate::model::{Channel, ImageResource, LayerKind, MergedImage, PsdFile, PsdLayer};

/// Image resource 1046: the number of colours an Indexed palette really uses.
pub const ID_INDEXED_COLOR_COUNT: u16 = 1046;
/// Image resource 1047: the palette index that is transparent.
pub const ID_TRANSPARENCY_INDEX: u16 = 1047;
/// An Indexed file's colour mode data: 256 reds, 256 greens, 256 blues.
pub const INDEXED_MODE_DATA_LEN: usize = 768;

/// Refuse colour mode data that cannot be what its mode says it is.
///
/// An Indexed file's pixels mean nothing without its palette, so a palette
/// that is not exactly 768 bytes is a malformed file, not a warning.
pub(crate) fn check_mode_data(header: &PsdHeader, data: &[u8]) -> PsdResult<()> {
    if header.color_mode == ColorMode::Indexed && data.len() != INDEXED_MODE_DATA_LEN {
        return Err(PsdError::SectionLengthMismatch {
            what: "Indexed colour mode data (a 768-byte palette)",
            declared: data.len() as u64,
            consumed: INDEXED_MODE_DATA_LEN as u64,
        });
    }
    Ok(())
}

/// Expand one plane of packed 1-bit Bitmap rows (`ceil(width / 8)` bytes a
/// row, most significant bit first) into 8-bit grey: a set bit is black (`0`),
/// a clear one white (`255`). The expansion is drawn from `budget` first.
pub fn unpack_bitmap(
    packed: &[u8],
    width: u32,
    height: u32,
    budget: &mut Budget,
) -> PsdResult<Vec<u8>> {
    let row_bytes = width.div_ceil(8) as usize;
    let expected = row_bytes
        .checked_mul(height as usize)
        .ok_or(PsdError::Overflow {
            what: "Bitmap plane size",
        })?;
    if packed.len() != expected {
        return Err(PsdError::ChannelSizeMismatch {
            what: "Bitmap plane",
            expected,
            actual: packed.len(),
        });
    }
    let total = (width as usize)
        .checked_mul(height as usize)
        .ok_or(PsdError::Overflow {
            what: "Bitmap pixel count",
        })?;
    budget.take(total as u64)?;
    let mut out = Vec::with_capacity(total);
    if row_bytes > 0 {
        for row in packed.chunks_exact(row_bytes) {
            for x in 0..width as usize {
                let byte = row.get(x / 8).copied().unwrap_or(0);
                let set = byte & (0x80 >> (x % 8)) != 0;
                out.push(if set { 0 } else { 255 });
            }
        }
    }
    budget.give(packed.len() as u64);
    Ok(out)
}

/// Pack 8-bit grey into 1-bit Bitmap rows (anything below 128 is black).
/// The inverse of [`unpack_bitmap`], for building Bitmap files in tests and
/// tools; the writer itself saves a Bitmap document as Greyscale.
pub fn pack_bitmap(grey: &[u8], width: u32, height: u32) -> Vec<u8> {
    let row_bytes = width.div_ceil(8) as usize;
    let mut out = vec![0u8; row_bytes * height as usize];
    for (y, row) in grey
        .chunks(width.max(1) as usize)
        .take(height as usize)
        .enumerate()
    {
        for (x, v) in row.iter().enumerate() {
            if *v < 128 {
                if let Some(b) = out.get_mut(y * row_bytes + x / 8) {
                    *b |= 0x80 >> (x % 8);
                }
            }
        }
    }
    out
}

/// An Indexed document's palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedTable {
    /// `1..=256` colours, in index order.
    pub colors: Vec<[u8; 3]>,
    /// The index that is transparent, when the file names one (resource 1047).
    pub transparent: Option<u8>,
}

impl IndexedTable {
    /// Read the palette from an Indexed file's colour mode data and its two
    /// resources. The colour count (1046) defaults to 256; a transparency
    /// index (1047) outside the palette is ignored.
    pub fn read(file: &PsdFile) -> PsdResult<Self> {
        let data = &file.color_mode_data;
        if data.len() != INDEXED_MODE_DATA_LEN {
            return Err(PsdError::SectionLengthMismatch {
                what: "Indexed colour mode data (a 768-byte palette)",
                declared: data.len() as u64,
                consumed: INDEXED_MODE_DATA_LEN as u64,
            });
        }
        let resource_u16 = |id: u16| {
            file.resources
                .iter()
                .find(|r| r.id == id)
                .and_then(|r| r.data.get(..2))
                .map(|b| u16::from_be_bytes([b[0], b[1]]))
        };
        let count = resource_u16(ID_INDEXED_COLOR_COUNT)
            .map(|c| usize::from(c).clamp(1, 256))
            .unwrap_or(256);
        let colors = (0..count)
            .map(|i| [data[i], data[256 + i], data[512 + i]])
            .collect();
        let transparent = resource_u16(ID_TRANSPARENCY_INDEX)
            .filter(|&t| usize::from(t) < count)
            .map(|t| t as u8);
        Ok(IndexedTable {
            colors,
            transparent,
        })
    }

    /// The colour at `index`; an index past the palette reads as black.
    pub fn lookup(&self, index: u8) -> [u8; 3] {
        self.colors
            .get(usize::from(index))
            .copied()
            .unwrap_or([0, 0, 0])
    }

    /// The 768-byte colour mode data for this palette (unused entries black).
    pub fn mode_data(&self) -> Vec<u8> {
        let mut out = vec![0u8; INDEXED_MODE_DATA_LEN];
        for (i, c) in self.colors.iter().take(256).enumerate() {
            out[i] = c[0];
            out[256 + i] = c[1];
            out[512 + i] = c[2];
        }
        out
    }

    /// The colour-count resource, and the transparency one when there is one.
    pub fn resources(&self) -> Vec<ImageResource> {
        let mut out = vec![ImageResource {
            id: ID_INDEXED_COLOR_COUNT,
            name: String::new(),
            data: (self.colors.len().clamp(1, 256) as u16)
                .to_be_bytes()
                .to_vec(),
        }];
        if let Some(t) = self.transparent {
            out.push(ImageResource {
                id: ID_TRANSPARENCY_INDEX,
                name: String::new(),
                data: u16::from(t).to_be_bytes().to_vec(),
            });
        }
        out
    }
}

/// One duotone ink's colour, as the record stores it (Adobe's colour
/// structure: a colour-space id and four 16-bit components).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InkColor {
    /// 0: 16-bit red, green, blue.
    Rgb([u16; 3]),
    /// 2: 16-bit cyan, magenta, yellow, black, **inverted** (`0` = 100 % ink).
    Cmyk([u16; 4]),
    /// 7: `L` `0..=10000`, `a` and `b` `-12800..=12700`.
    Lab { l: u16, a: i16, b: i16 },
    /// 8: grey `0..=10000` (`10000` is 100 % black).
    Gray(u16),
    /// Any other space (HSB, or a colour book such as Pantone, whose
    /// components are a book id rather than a colour). Kept, not rendered.
    Other { space: u16, components: [u16; 4] },
}

/// One ink of a duotone record.
#[derive(Debug, Clone, PartialEq)]
pub struct DuotoneInkRecord {
    pub color: InkColor,
    pub name: String,
    /// The ink curve as `[tint, ink]` pairs on `0..=1`, from the record's
    /// thirteen fixed tint positions (points marked unused are left out).
    pub curve: Vec<[f32; 2]>,
}

/// A Duotone document's ink record, decoded from the colour mode data.
///
/// Adobe documents the record only as "the duotone specification", to be
/// preserved by readers that treat the image as greyscale. The layout read
/// here is the Duotone Options (`.ado`) layout Photoshop uses for it:
///
/// ```text
/// u16 version (1)   u16 ink count (1..=4)
/// 4 x 10 bytes      ink colour (u16 space + 4 x u16)
/// 4 x 64 bytes      ink name (Pascal string, padded)
/// 4 x 28 bytes      ink curve (13 x i16 on 0..=1000, -1 unused; u16 flag)
/// ...               dot gain and overprint colours (kept, not read)
/// ```
///
/// Anything that does not fit that layout is reported (`Err` with the
/// reason) so the caller opens the greyscale base rather than guess inks.
#[derive(Debug, Clone, PartialEq)]
pub struct DuotoneRecord {
    pub inks: Vec<DuotoneInkRecord>,
}

/// The tint (input) positions of a duotone curve's thirteen points, in
/// percent.
pub const DUOTONE_CURVE_TINTS: [u16; 13] = [0, 5, 10, 20, 30, 40, 50, 60, 70, 80, 90, 95, 100];

const DUOTONE_INK_BLOCK: usize = 10;
const DUOTONE_NAME_BLOCK: usize = 64;
const DUOTONE_CURVE_BLOCK: usize = 28;
/// Version, count, then the colour, name and curve blocks of four inks.
pub const DUOTONE_MIN_LEN: usize =
    4 + 4 * (DUOTONE_INK_BLOCK + DUOTONE_NAME_BLOCK + DUOTONE_CURVE_BLOCK);

fn be16(data: &[u8], at: usize) -> Result<u16, String> {
    data.get(at..at + 2)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
        .ok_or_else(|| format!("the duotone record ends at byte {}", data.len()))
}

impl DuotoneRecord {
    /// Decode the record; `Err` names what did not fit.
    pub fn parse(data: &[u8]) -> Result<Self, String> {
        if data.len() < DUOTONE_MIN_LEN {
            return Err(format!(
                "the duotone record is {} bytes, shorter than the {DUOTONE_MIN_LEN} its inks need",
                data.len()
            ));
        }
        let version = be16(data, 0)?;
        if version != 1 {
            return Err(format!("the duotone record is version {version}, not 1"));
        }
        let count = usize::from(be16(data, 2)?);
        if !(1..=4).contains(&count) {
            return Err(format!("the duotone record names {count} inks, not 1 to 4"));
        }
        let colours_at = 4;
        let names_at = colours_at + 4 * DUOTONE_INK_BLOCK;
        let curves_at = names_at + 4 * DUOTONE_NAME_BLOCK;
        let mut inks = Vec::with_capacity(count);
        for i in 0..count {
            let c = colours_at + i * DUOTONE_INK_BLOCK;
            let space = be16(data, c)?;
            let comp = [
                be16(data, c + 2)?,
                be16(data, c + 4)?,
                be16(data, c + 6)?,
                be16(data, c + 8)?,
            ];
            let color = match space {
                0 => InkColor::Rgb([comp[0], comp[1], comp[2]]),
                2 => InkColor::Cmyk(comp),
                7 => InkColor::Lab {
                    l: comp[0],
                    a: comp[1] as i16,
                    b: comp[2] as i16,
                },
                8 => InkColor::Gray(comp[0]),
                space => InkColor::Other {
                    space,
                    components: comp,
                },
            };
            let n = names_at + i * DUOTONE_NAME_BLOCK;
            let len = usize::from(data[n]).min(DUOTONE_NAME_BLOCK - 1);
            let name: String = data[n + 1..n + 1 + len]
                .iter()
                .map(|&b| char::from(b))
                .collect();
            let k = curves_at + i * DUOTONE_CURVE_BLOCK;
            let mut curve = Vec::with_capacity(13);
            for (p, tint) in DUOTONE_CURVE_TINTS.iter().enumerate() {
                let raw = be16(data, k + 2 * p)? as i16;
                if raw == -1 {
                    continue;
                }
                if !(0..=1000).contains(&raw) {
                    return Err(format!(
                        "ink {} curve point {p} is {raw}, outside 0..=1000",
                        i + 1
                    ));
                }
                curve.push([f32::from(*tint) / 100.0, f32::from(raw) / 1000.0]);
            }
            if curve.len() < 2 {
                return Err(format!("ink {} curve has fewer than two points", i + 1));
            }
            inks.push(DuotoneInkRecord { color, name, curve });
        }
        Ok(DuotoneRecord { inks })
    }

    /// Encode in the layout [`DuotoneRecord::parse`] reads (unused inks and
    /// the trailing dot-gain / overprint block zeroed). A curve point is
    /// written at each of the thirteen tints the curve has a point at.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = vec![0u8; DUOTONE_MIN_LEN + 2 + 11 * DUOTONE_INK_BLOCK];
        out[0..2].copy_from_slice(&1u16.to_be_bytes());
        let count = self.inks.len().min(4);
        out[2..4].copy_from_slice(&(count as u16).to_be_bytes());
        let names_at = 4 + 4 * DUOTONE_INK_BLOCK;
        let curves_at = names_at + 4 * DUOTONE_NAME_BLOCK;
        for (i, ink) in self.inks.iter().take(4).enumerate() {
            let (space, comp) = match ink.color {
                InkColor::Rgb(c) => (0u16, [c[0], c[1], c[2], 0]),
                InkColor::Cmyk(c) => (2, c),
                InkColor::Lab { l, a, b } => (7, [l, a as u16, b as u16, 0]),
                InkColor::Gray(g) => (8, [g, 0, 0, 0]),
                InkColor::Other { space, components } => (space, components),
            };
            let c = 4 + i * DUOTONE_INK_BLOCK;
            out[c..c + 2].copy_from_slice(&space.to_be_bytes());
            for (j, v) in comp.iter().enumerate() {
                out[c + 2 + 2 * j..c + 4 + 2 * j].copy_from_slice(&v.to_be_bytes());
            }
            let n = names_at + i * DUOTONE_NAME_BLOCK;
            let name: Vec<u8> = ink
                .name
                .chars()
                .map(|ch| if ch.is_ascii() { ch as u8 } else { b'?' })
                .take(DUOTONE_NAME_BLOCK - 1)
                .collect();
            out[n] = name.len() as u8;
            out[n + 1..n + 1 + name.len()].copy_from_slice(&name);
            let k = curves_at + i * DUOTONE_CURVE_BLOCK;
            for (p, tint) in DUOTONE_CURVE_TINTS.iter().enumerate() {
                let t = f32::from(*tint) / 100.0;
                let point = ink.curve.iter().find(|q| (q[0] - t).abs() < 1e-4);
                let raw: i16 = match point {
                    Some(q) => (q[1].clamp(0.0, 1.0) * 1000.0).round() as i16,
                    None => -1,
                };
                out[k + 2 * p..k + 2 * p + 2].copy_from_slice(&raw.to_be_bytes());
            }
        }
        out
    }
}

/// The caller's colour science for [`to_working_rgb`].
pub struct WorkingScience<'a> {
    /// Ink coverage `0..=1` (cyan, magenta, yellow, black) to 8-bit sRGB.
    pub cmyk_to_rgb: &'a dyn Fn([f32; 4]) -> [u8; 3],
    /// `L` `0..=100`, `a` and `b` `-128..=127` to 8-bit sRGB.
    pub lab_to_rgb: &'a dyn Fn([f32; 3]) -> [u8; 3],
    /// A Duotone document's grey (`0` black .. `255` white) printed through
    /// its inks. `None` opens the greyscale base.
    pub duotone: Option<&'a [[u8; 3]; 256]>,
}

/// What [`to_working_rgb`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Normalised {
    /// The file's own mode.
    pub source: ColorMode,
    /// The file's own depth (the working copy is 8-bit when the source mode
    /// was converted).
    pub source_depth: Depth,
    /// The palette, for an Indexed file.
    pub indexed: Option<IndexedTable>,
    /// Multichannel channels past the ones the composite shows, which an RGB
    /// working copy has no place for.
    pub dropped_channels: usize,
}

/// One 16- or 32-bit sample narrowed to 8 bits (rounded).
fn narrow(plane: &[u8], depth: Depth) -> Vec<u8> {
    match depth {
        Depth::Eight => plane.to_vec(),
        Depth::Sixteen => plane
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| ((u32::from(u16::from_be_bytes(*c)) * 255 + 32767) / 65535) as u8)
            .collect(),
        Depth::ThirtyTwo => plane
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| {
                let v = f32::from_be_bytes(*c);
                let v = if v.is_finite() {
                    v.clamp(0.0, 1.0)
                } else {
                    0.0
                };
                (v * 255.0).round() as u8
            })
            .collect(),
    }
}

/// An 8-bit plane widened exactly to 16 bits (`v * 257`, big-endian).
fn widen16(plane: &[u8]) -> Vec<u8> {
    plane
        .iter()
        .flat_map(|v| (u16::from(*v) * 257).to_be_bytes())
        .collect()
}

/// Converted colour planes, and the alpha an Indexed transparent index makes.
type Converted = (Vec<Vec<u8>>, Option<Vec<u8>>);

/// How a source mode's colour samples become the working copy's.
struct Decode<'a> {
    source: ColorMode,
    /// Colour channels read per pixel.
    inputs: usize,
    /// Colour channels written per pixel: 1 (grey) or 3 (RGB).
    outputs: usize,
    science: &'a WorkingScience<'a>,
    indexed: Option<&'a IndexedTable>,
}

impl Decode<'_> {
    fn pixel(&self, s: &[u8]) -> [u8; 3] {
        let at = |i: usize| s.get(i).copied().unwrap_or(0);
        let ink = |i: usize| 1.0 - f32::from(at(i)) / 255.0;
        match self.source {
            ColorMode::Cmyk => (self.science.cmyk_to_rgb)([ink(0), ink(1), ink(2), ink(3)]),
            ColorMode::Lab => (self.science.lab_to_rgb)([
                f32::from(at(0)) * 100.0 / 255.0,
                f32::from(at(1)) - 128.0,
                f32::from(at(2)) - 128.0,
            ]),
            ColorMode::Indexed => self.indexed.map(|t| t.lookup(at(0))).unwrap_or([at(0); 3]),
            ColorMode::Multichannel if self.inputs >= 3 => {
                (self.science.cmyk_to_rgb)([ink(0), ink(1), ink(2), 0.0])
            }
            ColorMode::Duotone => match self.science.duotone {
                Some(lut) => lut[usize::from(at(0))],
                None => [at(0); 3],
            },
            _ => [at(0); 3],
        }
    }

    /// Convert `inputs` planes of `n` 8-bit samples; `None` when one is the
    /// wrong length. The optional second plane is the Indexed transparency.
    fn planes(&self, colour: &[&[u8]], n: usize) -> Option<Converted> {
        if colour.len() != self.inputs || colour.iter().any(|p| p.len() != n) {
            return None;
        }
        let mut out = vec![Vec::with_capacity(n); self.outputs];
        let transparent = self.indexed.and_then(|t| t.transparent);
        let mut alpha = transparent.map(|_| Vec::with_capacity(n));
        let mut s = [0u8; 4];
        for i in 0..n {
            for (c, plane) in colour.iter().enumerate().take(4) {
                s[c] = plane[i];
            }
            let rgb = self.pixel(&s[..colour.len().min(4)]);
            for (o, plane) in out.iter_mut().enumerate() {
                plane.push(rgb[o]);
            }
            if let (Some(a), Some(t)) = (alpha.as_mut(), transparent) {
                a.push(if s[0] == t { 0 } else { 255 });
            }
        }
        Some((out, alpha))
    }
}

/// Convert a file read in any mode into the editor's working form: RGB (or
/// greyscale for Bitmap and an un-inked Duotone) at 8 bits, in place.
///
/// RGB and Greyscale files are returned untouched, at their own depth. For
/// every other mode each layer's colour channels and the merged composite's
/// are decoded through the mode's rules (see the module table) and the
/// caller's [`WorkingScience`]; alpha, masks and extra channels are narrowed
/// to 8 bits and kept. An Indexed file's transparent index becomes an alpha
/// channel. A Multichannel composite shows its first three inks as C, M and
/// Y (or its one ink as grey); its other channels are counted in
/// [`Normalised::dropped_channels`].
///
/// The colour mode data and the Indexed resources are consumed: the working
/// copy is an ordinary RGB or greyscale file afterwards.
pub fn to_working_rgb(file: &mut PsdFile, science: &WorkingScience<'_>) -> PsdResult<Normalised> {
    let source = file.header.color_mode;
    let source_depth = file.header.depth;
    let mut result = Normalised {
        source,
        source_depth,
        indexed: None,
        dropped_channels: 0,
    };
    if matches!(source, ColorMode::Rgb | ColorMode::Grayscale) {
        return Ok(result);
    }
    let indexed = if source == ColorMode::Indexed {
        Some(IndexedTable::read(file)?)
    } else {
        None
    };
    let header_colour = usize::from(source.color_channels());
    // Multichannel: every merged channel is an ink; the composite shows three
    // of them as CMY, or one as grey.
    let (inputs, outputs, target) = match source {
        ColorMode::Multichannel if file.header.channels >= 3 => (3, 3, ColorMode::Rgb),
        ColorMode::Multichannel => (1, 1, ColorMode::Grayscale),
        ColorMode::Bitmap => (1, 1, ColorMode::Grayscale),
        ColorMode::Duotone if science.duotone.is_none() => (1, 1, ColorMode::Grayscale),
        ColorMode::Duotone => (1, 3, ColorMode::Rgb),
        other => (usize::from(other.color_channels()), 3, ColorMode::Rgb),
    };
    let decode = Decode {
        source,
        inputs,
        outputs,
        science,
        indexed: indexed.as_ref(),
    };
    let depth = source_depth;

    // Layers, iteratively (the tree came from a file).
    let mut stack: Vec<&mut PsdLayer> = file.layers.iter_mut().collect();
    while let Some(layer) = stack.pop() {
        convert_layer(layer, &decode, depth);
        if let LayerKind::Group(group) = &mut layer.kind {
            stack.extend(group.children.iter_mut());
        }
    }

    // The merged composite.
    let n = file.header.canvas_pixels() as usize;
    let declared = usize::from(file.header.channels);
    // Channels past the colour ones in the source.
    let consumed = if source == ColorMode::Multichannel {
        declared
    } else {
        header_colour
    };
    let extras_declared = declared.saturating_sub(consumed);
    if source == ColorMode::Multichannel {
        result.dropped_channels = declared.saturating_sub(inputs);
    }
    // An Indexed composite's first channel past the index is its
    // transparency (the header counts it as alpha): it becomes the working
    // alpha, combined with the transparent index when the file names both.
    let indexed_alpha = source == ColorMode::Indexed && extras_declared > 0;
    let with_alpha = indexed.as_ref().is_some_and(|t| t.transparent.is_some()) || indexed_alpha;
    if let Some(merged) = file.merged.take() {
        let planes: Vec<Vec<u8>> = merged.channels.iter().map(|p| narrow(p, depth)).collect();
        let colour: Vec<&[u8]> = planes.iter().take(inputs).map(Vec::as_slice).collect();
        let converted = decode.planes(&colour, n);
        let mut channels = Vec::with_capacity(outputs + 1 + extras_declared);
        match converted {
            Some((out, alpha)) => {
                channels.extend(out);
                if with_alpha {
                    let mut alpha = alpha.unwrap_or_else(|| vec![255u8; n]);
                    if let Some(file_alpha) = planes.get(1).filter(|_| indexed_alpha) {
                        for (a, f) in alpha.iter_mut().zip(file_alpha) {
                            *a = (*a).min(*f);
                        }
                    }
                    channels.push(alpha);
                }
            }
            None => {
                return Err(PsdError::ChannelSizeMismatch {
                    what: "merged colour channel",
                    expected: n,
                    actual: planes.first().map_or(0, Vec::len),
                });
            }
        }
        if source != ColorMode::Multichannel {
            channels.extend(
                planes
                    .into_iter()
                    .skip(consumed + usize::from(indexed_alpha)),
            );
        }
        file.merged = Some(MergedImage { channels });
    }
    let channels = outputs
        + usize::from(with_alpha)
        + if source == ColorMode::Multichannel {
            0
        } else {
            extras_declared - usize::from(indexed_alpha)
        };
    file.header = PsdHeader {
        channels: channels.min(56) as u16,
        depth: Depth::Eight,
        color_mode: target,
        ..file.header
    };
    file.color_mode_data.clear();
    file.resources
        .retain(|r| r.id != ID_INDEXED_COLOR_COUNT && r.id != ID_TRANSPARENCY_INDEX);
    result.indexed = indexed;
    Ok(result)
}

fn convert_layer(layer: &mut PsdLayer, decode: &Decode<'_>, depth: Depth) {
    let n = layer.bounds.width() as usize * layer.bounds.height() as usize;
    let bps = depth.bytes_per_sample();
    for channel in &mut layer.channels {
        if channel.data.len() == n * bps {
            channel.data = narrow(&channel.data, depth);
        }
    }
    if let Some(mask) = &mut layer.mask {
        let m = mask.bounds.width() as usize * mask.bounds.height() as usize;
        if mask.data.len() == m * bps {
            mask.data = narrow(&mask.data, depth);
        }
        if let Some(real) = &mut mask.real {
            let r = real.bounds.width() as usize * real.bounds.height() as usize;
            if real.data.len() == r * bps {
                real.data = narrow(&real.data, depth);
            }
        }
    }
    if n == 0 {
        return;
    }
    let ids: Vec<i16> = (0..decode.inputs as i16).collect();
    let colour: Option<Vec<&[u8]>> = ids
        .iter()
        .map(|id| layer.channel(*id).map(|c| c.data.as_slice()))
        .collect();
    let Some(colour) = colour else {
        return;
    };
    let Some((out, alpha)) = decode.planes(&colour, n) else {
        return;
    };
    // Every colour channel of the source goes (a Multichannel layer's extra
    // inks too); the working colour channels and alpha take their place.
    layer.channels.retain(|c| c.id < 0);
    if let Some(transparent) = alpha {
        match layer.channels.iter_mut().find(|c| c.id == CHANNEL_ALPHA) {
            Some(a) if a.data.len() == n => {
                for (v, t) in a.data.iter_mut().zip(transparent) {
                    *v = (*v).min(t);
                }
            }
            _ => layer
                .channels
                .push(Channel::new(CHANNEL_ALPHA, transparent)),
        }
    }
    for (id, plane) in out.into_iter().enumerate() {
        layer.channels.push(Channel::new(id as i16, plane));
    }
}

/// How [`from_working_rgb`] separates the working RGB into the target mode.
pub enum Separation<'a> {
    /// Rec. 601 luma.
    Grayscale,
    /// 8-bit sRGB to ink coverage `0..=1` (cyan, magenta, yellow, black).
    Cmyk(&'a mut dyn FnMut([u8; 3]) -> [f32; 4]),
    /// 8-bit sRGB to `L` `0..=100`, `a` and `b` in `-128..=127`.
    Lab(&'a dyn Fn([u8; 3]) -> [f32; 3]),
    /// A flat document mapped onto a palette: the table, and the index each
    /// opaque colour takes.
    Indexed(&'a IndexedTable, &'a mut dyn FnMut([u8; 3]) -> u8),
}

/// Rec. 601 luma, the formula Image > Mode > Grayscale uses.
pub fn luma601(rgb: [u8; 3]) -> u8 {
    (0.299 * f32::from(rgb[0]) + 0.587 * f32::from(rgb[1]) + 0.114 * f32::from(rgb[2]))
        .round()
        .clamp(0.0, 255.0) as u8
}

/// Rec. 601 luma of three big-endian 16-bit planes of `n` samples, as one
/// big-endian 16-bit plane.
fn luma601_16(planes: [&[u8]; 3], n: usize) -> Vec<u8> {
    let at = |p: &[u8], i: usize| f64::from(u16::from_be_bytes([p[2 * i], p[2 * i + 1]]));
    (0..n)
        .flat_map(|i| {
            let y = 0.299 * at(planes[0], i) + 0.587 * at(planes[1], i) + 0.114 * at(planes[2], i);
            (y.round().clamp(0.0, 65535.0) as u16).to_be_bytes()
        })
        .collect()
}

/// The inverse of [`to_working_rgb`] for the modes this crate writes:
/// convert an RGB file (8- or 16-bit, as the application builds it) into
/// Greyscale, CMYK or Lab — every layer's colour channels and the merged
/// composite, alpha, masks and extra channels untouched — or into Indexed,
/// which is flat by definition: the file must have no layers, and its
/// merged composite becomes one index plane (transparent pixels take the
/// table's transparent index) at 8 bits.
///
/// CMYK samples are written inverted (`0` = 100 % ink) and Lab ones
/// offset-encoded, the way Photoshop reads them. Colour conversion runs at
/// 8-bit precision and a 16-bit file is widened back exactly; a 16-bit
/// Greyscale separation keeps its 16-bit samples.
pub fn from_working_rgb(file: &mut PsdFile, separation: Separation<'_>) -> PsdResult<()> {
    if file.header.color_mode != ColorMode::Rgb {
        return Err(PsdError::InvalidDocument(format!(
            "only an RGB file can be separated, not a {:?} one",
            file.header.color_mode
        )));
    }
    let depth = file.header.depth;
    if depth == Depth::ThirtyTwo {
        return Err(PsdError::InvalidDocument(
            "a 32-bit file cannot be written in a print colour mode".into(),
        ));
    }
    let (target, outputs, mut separation) = match separation {
        Separation::Indexed(table, index_of) => return to_indexed(file, table, index_of),
        s @ Separation::Grayscale => (ColorMode::Grayscale, 1usize, s),
        s @ Separation::Cmyk(_) => (ColorMode::Cmyk, 4, s),
        s @ Separation::Lab(_) => (ColorMode::Lab, 3, s),
    };
    let mut encode = |rgb: [u8; 3]| -> [u8; 4] {
        match &mut separation {
            Separation::Grayscale => [luma601(rgb), 0, 0, 0],
            Separation::Cmyk(f) => {
                let ink = f(rgb);
                ink.map(|v| 255 - (v.clamp(0.0, 1.0) * 255.0).round() as u8)
            }
            Separation::Lab(f) => {
                let lab = f(rgb);
                [
                    (lab[0] * 255.0 / 100.0).round().clamp(0.0, 255.0) as u8,
                    (lab[1] + 128.0).round().clamp(0.0, 255.0) as u8,
                    (lab[2] + 128.0).round().clamp(0.0, 255.0) as u8,
                    0,
                ]
            }
            Separation::Indexed(..) => [0; 4],
        }
    };
    let bps = depth.bytes_per_sample();
    // A 16-bit greyscale document keeps its 16-bit samples: its luma is taken
    // at 16 bits, not through the 8-bit path the colour separations use.
    let grey16 = target == ColorMode::Grayscale && depth == Depth::Sixteen;
    let mut separate = |planes: [&[u8]; 3], n: usize| -> Option<Vec<Vec<u8>>> {
        if planes.iter().any(|p| p.len() != n * bps) {
            return None;
        }
        if grey16 {
            return Some(vec![luma601_16(planes, n)]);
        }
        let narrowed = planes.map(|p| narrow(p, depth));
        let mut out = vec![Vec::with_capacity(n); outputs];
        let [r, g, b] = &narrowed;
        for ((r, g), b) in r.iter().zip(g).zip(b) {
            let v = encode([*r, *g, *b]);
            for (o, plane) in out.iter_mut().enumerate() {
                plane.push(v[o]);
            }
        }
        if depth == Depth::Sixteen {
            for plane in &mut out {
                *plane = widen16(plane);
            }
        }
        Some(out)
    };

    let mut stack: Vec<&mut PsdLayer> = file.layers.iter_mut().collect();
    while let Some(layer) = stack.pop() {
        let n = layer.bounds.width() as usize * layer.bounds.height() as usize;
        if n > 0 {
            let planes = [0i16, 1, 2].map(|id| layer.channel(id).map(|c| c.data.as_slice()));
            if let [Some(r), Some(g), Some(b)] = planes {
                if let Some(out) = separate([r, g, b], n) {
                    layer.channels.retain(|c| !(0..=2).contains(&c.id));
                    for (id, plane) in out.into_iter().enumerate() {
                        layer.channels.push(Channel::new(id as i16, plane));
                    }
                }
            }
        }
        if let LayerKind::Group(group) = &mut layer.kind {
            stack.extend(group.children.iter_mut());
        }
    }
    let n = file.header.canvas_pixels() as usize;
    if let Some(merged) = &mut file.merged {
        if merged.channels.len() < 3 {
            return Err(PsdError::InvalidDocument(
                "the merged composite has fewer than three colour channels".into(),
            ));
        }
        let rest = merged.channels.split_off(3);
        let out = separate(
            [
                &merged.channels[0],
                &merged.channels[1],
                &merged.channels[2],
            ],
            n,
        )
        .ok_or_else(|| {
            PsdError::InvalidDocument("the merged composite is the wrong size".into())
        })?;
        merged.channels = out;
        merged.channels.extend(rest);
    }
    let channels = usize::from(file.header.channels).saturating_sub(3) + outputs;
    file.header.channels = channels.min(56) as u16;
    file.header.color_mode = target;
    Ok(())
}

fn to_indexed(
    file: &mut PsdFile,
    table: &IndexedTable,
    index_of: &mut dyn FnMut([u8; 3]) -> u8,
) -> PsdResult<()> {
    if !file.layers.is_empty() {
        return Err(PsdError::InvalidDocument(
            "an Indexed file is flat: merge the layers first".into(),
        ));
    }
    let depth = file.header.depth;
    let n = file.header.canvas_pixels() as usize;
    let merged = file.merged.as_ref().ok_or_else(|| {
        PsdError::InvalidDocument("an Indexed file needs its merged composite".into())
    })?;
    if merged.channels.len() < 3 {
        return Err(PsdError::InvalidDocument(
            "the merged composite has fewer than three colour channels".into(),
        ));
    }
    let planes: Vec<Vec<u8>> = merged.channels.iter().map(|p| narrow(p, depth)).collect();
    if planes.iter().any(|p| p.len() != n) {
        return Err(PsdError::InvalidDocument(
            "the merged composite is the wrong size".into(),
        ));
    }
    let has_alpha = file.header.has_alpha();
    let alpha = if has_alpha { planes.get(3) } else { None };
    let mut index = Vec::with_capacity(n);
    for i in 0..n {
        let transparent = alpha.is_some_and(|a| a[i] < 128);
        index.push(match (transparent, table.transparent) {
            (true, Some(t)) => t,
            _ => index_of([planes[0][i], planes[1][i], planes[2][i]]),
        });
    }
    let skip = if has_alpha { 4 } else { 3 };
    let mut channels = vec![index];
    channels.extend(planes.into_iter().skip(skip));
    file.header.channels = (channels.len().min(56)) as u16;
    file.header.depth = Depth::Eight;
    file.header.color_mode = ColorMode::Indexed;
    file.merged = Some(MergedImage { channels });
    file.color_mode_data = table.mode_data();
    file.resources
        .retain(|r| r.id != ID_INDEXED_COLOR_COUNT && r.id != ID_TRANSPARENCY_INDEX);
    file.resources.extend(table.resources());
    Ok(())
}

#[cfg(test)]
#[path = "colour_modes_tests.rs"]
mod tests;
