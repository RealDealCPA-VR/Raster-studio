//! W13-C: camera RAW. **DNG is read** by this module's own code; the
//! proprietary RAW containers are recognised by content and **refused by
//! name**.
//!
//! # DNG
//!
//! A DNG is a TIFF. [`decode`] walks IFD0, its chain and its `SubIFDs`,
//! takes the full-resolution raw IFD (`NewSubFileType` 0, photometric CFA
//! `32803` or `LinearRaw` `34892`), and develops it:
//!
//! 1. **Samples**: strips or tiles, uncompressed (8/16-bit, or any width up
//!    to 16 bits packed MSB-first) or lossless JPEG (`Compression = 7`, see
//!    [`super::ljpeg`]). Lossy-JPEG, deflate/float and JPEG XL DNGs are
//!    refused by name.
//! 2. **Levels**: `LinearizationTable`, then `BlackLevel` (with its repeat
//!    pattern, `BlackLevelDeltaH` / `DeltaV`) is subtracted and the result
//!    scaled by `WhiteLevel`, inside the `ActiveArea`.
//! 3. **White balance** from `AsShotNeutral` (or, without one, the camera's
//!    response to D65 through the colour matrix), applied on the mosaic and
//!    clipped there, so a blown highlight stays white.
//! 4. **Demosaic**: for a 2x2 Bayer pattern, green is interpolated along the
//!    direction with the smaller gradient (Hamilton-Adams, with the
//!    second-derivative correction from the colour channel), then red and
//!    blue from the colour differences to green; any other pattern up to
//!    6x6 (X-Trans in a DNG) is averaged per colour over a 5x5 window.
//! 5. **Colour**: `AnalogBalance x CameraCalibration x ColorMatrix` (the
//!    matrix calibrated for D65 when one is, else `ColorMatrix2`, else
//!    `ColorMatrix1`; no interpolation by colour temperature) is combined
//!    with the sRGB primaries, row-normalised so a neutral stays neutral,
//!    and inverted: camera RGB to linear sRGB.
//! 6. **Tone**: `BaselineExposure` as a gain, clipping, then the sRGB
//!    transfer curve. There is no contrast S-curve (Adobe's default look is
//!    not reproduced); what is delivered is scene-linear sRGB, encoded.
//! 7. `DefaultCropOrigin` / `DefaultCropSize`, then the TIFF/EXIF
//!    `Orientation` of IFD0.
//!
//! The result is a 16-bit sRGB surface; File > Open makes it a 16 Bits/Channel
//! document. Not applied: `OpcodeList1-3` (lens corrections, gain maps),
//! `ForwardMatrix`, the DNG camera profile and `DefaultScale` (non-square
//! pixels).
//!
//! # Proprietary RAW
//!
//! Canon CR2 / CR3, Nikon NEF, Sony ARW, Fujifilm RAF, Olympus ORF,
//! Panasonic RW2 (and Pentax PEF / Samsung SRW) are identified from their
//! own signatures or, for the TIFF-shaped ones, from a CFA or vendor-coded
//! raw IFD with no `DNGVersion`, and refused with [`refusal`]. The Rust
//! crates that read them are `rawloader` and `rawler` (LGPL-2.1),
//! `quickraw` (LGPL-2.1) and `zenraw` (AGPL-3.0 or commercial): none is
//! permissive, and each vendor's compression is its own bounded-but-large
//! reverse-engineered codec this wave did not write.

use crate::codec::{
    CodecError, DecodedSurface, ImageInfo, ImportFormat, ImportLimits, SurfacePixels,
};

use super::{check_decode, info, ljpeg, malformed};

const NAME: &str = "DNG";

/// How many leading bytes [`identify`] is handed: enough for IFD0, the
/// `SubIFDs` it points at and their `Make` / `Compression` values in every
/// camera file this wave looked at.
pub const SNIFF_BYTES: usize = 256 * 1024;

/// What [`identify`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawKind {
    /// A DNG, which [`decode`] develops.
    Dng,
    /// A vendor RAW, named (`"Nikon NEF"`), which is refused.
    Proprietary(&'static str),
}

impl RawKind {
    /// The importer's name for it.
    pub fn format(self) -> ImportFormat {
        match self {
            RawKind::Dng => ImportFormat::Dng,
            RawKind::Proprietary(_) => ImportFormat::CameraRaw,
        }
    }
}

fn is_tiff(head: &[u8]) -> bool {
    head.starts_with(b"II*\0") || head.starts_with(b"MM\0*")
}

/// `true` when `head` (at least 16 bytes) could be a camera RAW, so the
/// sniff reads [`SNIFF_BYTES`] and asks [`identify`].
pub fn might_be_raw(head: &[u8]) -> bool {
    is_tiff(head) || vendor_signature(head).is_some()
}

/// Containers that say who made them in their first bytes.
fn vendor_signature(head: &[u8]) -> Option<&'static str> {
    if head.starts_with(b"FUJIFILMCCD-RAW") {
        Some("Fujifilm RAF")
    } else if head.starts_with(b"IIRO") || head.starts_with(b"IIRS") || head.starts_with(b"MMOR") {
        Some("Olympus ORF")
    } else if head.starts_with(b"IIU\0") {
        Some("Panasonic RW2")
    } else if head.len() >= 12 && &head[4..8] == b"ftyp" && &head[8..12] == b"crx " {
        Some("Canon CR3")
    } else if head.len() >= 10 && head.starts_with(b"II*\0") && &head[8..10] == b"CR" {
        Some("Canon CR2")
    } else {
        None
    }
}

/// Identify a camera RAW from the leading bytes of a file (a prefix is
/// enough: values past its end are simply not seen). `None` for anything
/// else, an ordinary TIFF included.
pub fn identify(head: &[u8]) -> Option<RawKind> {
    if let Some(name) = vendor_signature(head) {
        return Some(RawKind::Proprietary(name));
    }
    let tiff = Tiff::new(head)?;
    let first = tiff.first_ifd()?;
    let ifd0 = tiff.ifd(first)?;
    if ifd0.get(TAG_DNG_VERSION).is_some() {
        return Some(RawKind::Dng);
    }
    let make = ifd0
        .get(TAG_MAKE)
        .and_then(|e| tiff.bytes(e))
        .map(|b| String::from_utf8_lossy(b).to_ascii_uppercase())
        .unwrap_or_default();
    let raw_like = tiff.all_ifds(first).iter().any(|ifd| {
        let photometric = ifd.uint(&tiff, TAG_PHOTOMETRIC);
        let compression = ifd.uint(&tiff, TAG_COMPRESSION);
        photometric == Some(PHOTOMETRIC_CFA)
            || matches!(compression, Some(32767 | 32769 | 32770 | 34713 | 65000))
    });
    if !raw_like {
        return None;
    }
    let name = if make.starts_with("NIKON") {
        "Nikon NEF"
    } else if make.starts_with("SONY") {
        "Sony ARW"
    } else if make.starts_with("CANON") {
        "Canon CR2"
    } else if make.starts_with("PENTAX") || make.starts_with("RICOH") {
        "Pentax PEF"
    } else if make.starts_with("SAMSUNG") {
        "Samsung SRW"
    } else {
        "camera RAW"
    };
    Some(RawKind::Proprietary(name))
}

/// The refusal a proprietary RAW gets, naming the format and why.
pub fn refusal(bytes: &[u8]) -> CodecError {
    let name = match identify(bytes) {
        Some(RawKind::Proprietary(name)) => name,
        _ => "camera RAW",
    };
    CodecError::Unsupported(format!(
        "opening {name} files is not supported: this build reads DNG only. The Rust readers \
         for vendor RAW formats (rawloader, rawler, quickraw: LGPL-2.1; zenraw: AGPL-3.0) \
         are not permissively licensed, so none is linked; convert the file to DNG (for \
         example with Adobe DNG Converter) and open that"
    ))
}

// ---------------------------------------------------------------- TIFF ----

const TAG_NEW_SUBFILE_TYPE: u16 = 254;
const TAG_WIDTH: u16 = 256;
const TAG_HEIGHT: u16 = 257;
const TAG_BITS: u16 = 258;
const TAG_COMPRESSION: u16 = 259;
const TAG_PHOTOMETRIC: u16 = 262;
const TAG_MAKE: u16 = 271;
const TAG_STRIP_OFFSETS: u16 = 273;
const TAG_ORIENTATION: u16 = 274;
const TAG_SAMPLES: u16 = 277;
const TAG_ROWS_PER_STRIP: u16 = 278;
const TAG_STRIP_COUNTS: u16 = 279;
const TAG_PLANAR: u16 = 284;
const TAG_TILE_WIDTH: u16 = 322;
const TAG_TILE_LENGTH: u16 = 323;
const TAG_TILE_OFFSETS: u16 = 324;
const TAG_TILE_COUNTS: u16 = 325;
const TAG_SUB_IFDS: u16 = 330;
const TAG_CFA_REPEAT: u16 = 33421;
const TAG_CFA_PATTERN: u16 = 33422;
const TAG_DNG_VERSION: u16 = 50706;
const TAG_CFA_PLANE_COLOR: u16 = 50710;
const TAG_CFA_LAYOUT: u16 = 50711;
const TAG_LINEARIZATION: u16 = 50712;
const TAG_BLACK_REPEAT: u16 = 50713;
const TAG_BLACK_LEVEL: u16 = 50714;
const TAG_BLACK_DELTA_H: u16 = 50715;
const TAG_BLACK_DELTA_V: u16 = 50716;
const TAG_WHITE_LEVEL: u16 = 50717;
const TAG_CROP_ORIGIN: u16 = 50719;
const TAG_CROP_SIZE: u16 = 50720;
const TAG_COLOR_MATRIX_1: u16 = 50721;
const TAG_COLOR_MATRIX_2: u16 = 50722;
const TAG_CAMERA_CALIBRATION_1: u16 = 50723;
const TAG_CAMERA_CALIBRATION_2: u16 = 50724;
const TAG_ANALOG_BALANCE: u16 = 50727;
const TAG_AS_SHOT_NEUTRAL: u16 = 50728;
const TAG_BASELINE_EXPOSURE: u16 = 50730;
const TAG_ILLUMINANT_1: u16 = 50778;
const TAG_ILLUMINANT_2: u16 = 50779;
const TAG_ACTIVE_AREA: u16 = 50829;

const PHOTOMETRIC_CFA: u32 = 32803;
const PHOTOMETRIC_LINEAR_RAW: u32 = 34892;
/// EXIF `LightSource` code for D65.
const ILLUMINANT_D65: u32 = 21;

/// A bounded view of a TIFF: every read is a checked slice of `data`.
struct Tiff<'a> {
    data: &'a [u8],
    le: bool,
}

#[derive(Clone, Copy)]
struct Entry {
    tag: u16,
    kind: u16,
    count: u32,
    /// Where the value bytes are (inline in the entry, or at its offset).
    at: usize,
}

struct Ifd {
    entries: Vec<Entry>,
    next: u32,
}

fn type_size(kind: u16) -> Option<usize> {
    Some(match kind {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 | 13 => 4,
        5 | 10 | 12 => 8,
        _ => return None,
    })
}

impl<'a> Tiff<'a> {
    fn new(data: &'a [u8]) -> Option<Self> {
        if !is_tiff(data) {
            return None;
        }
        Some(Tiff {
            data,
            le: data[0] == b'I',
        })
    }

    fn u16_at(&self, at: usize) -> Option<u16> {
        let b = self.data.get(at..at.checked_add(2)?)?;
        Some(if self.le {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        })
    }

    fn u32_at(&self, at: usize) -> Option<u32> {
        let b = self.data.get(at..at.checked_add(4)?)?;
        let b = [b[0], b[1], b[2], b[3]];
        Some(if self.le {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    }

    fn first_ifd(&self) -> Option<usize> {
        self.u32_at(4).map(|o| o as usize)
    }

    fn ifd(&self, at: usize) -> Option<Ifd> {
        let n = usize::from(self.u16_at(at)?);
        if n == 0 || n > 4096 {
            return None;
        }
        let mut entries = Vec::with_capacity(n);
        for i in 0..n {
            let e = at + 2 + i * 12;
            let kind = self.u16_at(e + 2)?;
            let count = self.u32_at(e + 4)?;
            let inline = type_size(kind)
                .and_then(|s| s.checked_mul(count as usize))
                .is_some_and(|len| len <= 4);
            let at = if inline {
                e + 8
            } else {
                self.u32_at(e + 8)? as usize
            };
            entries.push(Entry {
                tag: self.u16_at(e)?,
                kind,
                count,
                at,
            });
        }
        let next = self.u32_at(at + 2 + n * 12).unwrap_or(0);
        Some(Ifd { entries, next })
    }

    /// IFD0, its chain and every `SubIFD` (two levels deep), each once; at
    /// most 64 of them.
    fn all_ifds(&self, first: usize) -> Vec<Ifd> {
        let mut seen: Vec<usize> = Vec::new();
        let mut out = Vec::new();
        let mut queue: Vec<(usize, u8)> = vec![(first, 0)];
        while let Some((at, depth)) = queue.pop() {
            if seen.contains(&at) || seen.len() >= 64 {
                continue;
            }
            seen.push(at);
            let Some(ifd) = self.ifd(at) else { continue };
            if depth < 2 {
                if let Some(subs) = ifd.get(TAG_SUB_IFDS).and_then(|e| self.uints(e)) {
                    queue.extend(subs.into_iter().take(16).map(|o| (o as usize, depth + 1)));
                }
            }
            if ifd.next != 0 && depth == 0 {
                queue.push((ifd.next as usize, 0));
            }
            out.push(ifd);
        }
        out
    }

    fn bytes(&self, e: Entry) -> Option<&'a [u8]> {
        let len = type_size(e.kind)?.checked_mul(e.count as usize)?;
        self.data.get(e.at..e.at.checked_add(len)?)
    }

    fn uints(&self, e: Entry) -> Option<Vec<u32>> {
        let b = self.bytes(e)?;
        let n = e.count as usize;
        Some(match e.kind {
            1 | 7 => b.iter().map(|&v| u32::from(v)).collect(),
            3 => (0..n)
                .map(|i| self.u16_at(e.at + i * 2).map(u32::from))
                .collect::<Option<_>>()?,
            4 | 13 => (0..n)
                .map(|i| self.u32_at(e.at + i * 4))
                .collect::<Option<_>>()?,
            _ => return None,
        })
    }

    fn floats(&self, e: Entry) -> Option<Vec<f64>> {
        let b = self.bytes(e)?;
        let n = e.count as usize;
        let v = |i: usize| -> Option<f64> {
            Some(match e.kind {
                1 | 7 => f64::from(b[i]),
                6 => f64::from(b[i] as i8),
                3 => f64::from(self.u16_at(e.at + i * 2)?),
                8 => f64::from(self.u16_at(e.at + i * 2)? as i16),
                4 => f64::from(self.u32_at(e.at + i * 4)?),
                9 => f64::from(self.u32_at(e.at + i * 4)? as i32),
                5 | 10 => {
                    let (num, den) = (self.u32_at(e.at + i * 8)?, self.u32_at(e.at + i * 8 + 4)?);
                    let (num, den) = if e.kind == 10 {
                        (f64::from(num as i32), f64::from(den as i32))
                    } else {
                        (f64::from(num), f64::from(den))
                    };
                    if den == 0.0 {
                        return None;
                    }
                    num / den
                }
                11 => f64::from(f32::from_bits(self.u32_at(e.at + i * 4)?)),
                12 => {
                    let hi = u64::from(self.u32_at(e.at + i * 8)?);
                    let lo = u64::from(self.u32_at(e.at + i * 8 + 4)?);
                    f64::from_bits(if self.le {
                        (lo << 32) | hi
                    } else {
                        (hi << 32) | lo
                    })
                }
                _ => return None,
            })
        };
        let out: Vec<f64> = (0..n).map(v).collect::<Option<_>>()?;
        out.iter().all(|f| f.is_finite()).then_some(out)
    }
}

impl Ifd {
    fn get(&self, tag: u16) -> Option<Entry> {
        self.entries.iter().copied().find(|e| e.tag == tag)
    }

    fn uint(&self, tiff: &Tiff, tag: u16) -> Option<u32> {
        self.uints_of(tiff, tag)?.first().copied()
    }

    fn uints_of(&self, tiff: &Tiff, tag: u16) -> Option<Vec<u32>> {
        tiff.uints(self.get(tag)?)
    }

    fn floats_of(&self, tiff: &Tiff, tag: u16) -> Option<Vec<f64>> {
        tiff.floats(self.get(tag)?)
    }
}

// ---------------------------------------------------------------- plan ----

/// Everything read from the tags, validated, before any pixel is touched.
struct Plan<'a> {
    tiff: Tiff<'a>,
    raw: Ifd,
    /// Full raw dimensions and samples per pixel (1 CFA, 3 linear).
    width: usize,
    height: usize,
    spp: usize,
    bits: u32,
    compression: u32,
    /// `(top, left, bottom, right)` inside the raw image.
    active: (usize, usize, usize, usize),
    /// `(x, y, w, h)` inside the active area.
    crop: (usize, usize, usize, usize),
    orientation: u32,
    /// CFA colour codes (0 red, 1 green, 2 blue), `rows x cols`; `None` for
    /// linear raw.
    cfa: Option<Pattern>,
    /// Camera-plane index for each of red, green, blue.
    planes: [usize; 3],
}

#[derive(Clone)]
struct Pattern {
    rows: usize,
    cols: usize,
    codes: Vec<u8>,
}

impl Pattern {
    fn at(&self, x: usize, y: usize) -> u8 {
        self.codes[(y % self.rows) * self.cols + x % self.cols]
    }

    fn is_bayer(&self) -> bool {
        self.rows == 2
            && self.cols == 2
            && ((self.codes[0] == 1 && self.codes[3] == 1 && self.codes[1] != self.codes[2])
                || (self.codes[1] == 1 && self.codes[2] == 1 && self.codes[0] != self.codes[3]))
    }
}

fn plan(bytes: &[u8], limits: ImportLimits) -> Result<Plan<'_>, CodecError> {
    let tiff = Tiff::new(bytes).ok_or_else(|| malformed(NAME, "not a TIFF"))?;
    let first = tiff.first_ifd().ok_or_else(|| malformed(NAME, "no IFD0"))?;
    let ifd0 = tiff
        .ifd(first)
        .ok_or_else(|| malformed(NAME, "IFD0 is damaged"))?;
    if ifd0.get(TAG_DNG_VERSION).is_none() {
        return Err(malformed(NAME, "IFD0 has no DNGVersion"));
    }
    let orientation = ifd0.uint(&tiff, TAG_ORIENTATION).unwrap_or(1);
    let orientation = if (1..=8).contains(&orientation) {
        orientation
    } else {
        1
    };
    let raw = tiff
        .all_ifds(first)
        .into_iter()
        .filter(|ifd| {
            matches!(
                ifd.uint(&tiff, TAG_PHOTOMETRIC),
                Some(PHOTOMETRIC_CFA | PHOTOMETRIC_LINEAR_RAW)
            ) && ifd.uint(&tiff, TAG_NEW_SUBFILE_TYPE).unwrap_or(0) == 0
        })
        .max_by_key(|ifd| {
            let w = u64::from(ifd.uint(&tiff, TAG_WIDTH).unwrap_or(0));
            w * u64::from(ifd.uint(&tiff, TAG_HEIGHT).unwrap_or(0))
        })
        .ok_or_else(|| malformed(NAME, "no full-resolution raw image"))?;
    let width = raw.uint(&tiff, TAG_WIDTH).unwrap_or(0) as usize;
    let height = raw.uint(&tiff, TAG_HEIGHT).unwrap_or(0) as usize;
    if width == 0 || height == 0 {
        return Err(malformed(NAME, "the raw image has no pixels"));
    }
    let photometric = raw.uint(&tiff, TAG_PHOTOMETRIC).unwrap_or(0);
    let spp = raw.uint(&tiff, TAG_SAMPLES).unwrap_or(1) as usize;
    match (photometric, spp) {
        (PHOTOMETRIC_CFA, 1) | (PHOTOMETRIC_LINEAR_RAW, 3) => {}
        _ => {
            return Err(CodecError::Unsupported(format!(
                "this DNG's raw image has {spp} samples per pixel for photometric \
                 {photometric}; only one-sample CFA and three-sample linear raw are read"
            )))
        }
    }
    if spp > 1 && raw.uint(&tiff, TAG_PLANAR).unwrap_or(1) != 1 {
        return Err(CodecError::Unsupported(
            "planar (separate-plane) linear DNGs are not read".into(),
        ));
    }
    let bit_list = raw.uints_of(&tiff, TAG_BITS).unwrap_or_else(|| vec![1]);
    let bits = bit_list.first().copied().unwrap_or(0);
    if bit_list.iter().any(|&b| b != bits) || !(1..=16).contains(&bits) {
        return Err(CodecError::Unsupported(format!(
            "DNG samples of {bits} bits are not read (1-16 bit integers are)"
        )));
    }
    let compression = raw.uint(&tiff, TAG_COMPRESSION).unwrap_or(1);
    match compression {
        1 | 7 => {}
        8 => {
            return Err(CodecError::Unsupported(
                "deflate-compressed (floating-point) DNGs are not read; uncompressed and \
                 lossless-JPEG DNGs are"
                    .into(),
            ))
        }
        34892 => {
            return Err(CodecError::Unsupported(
                "lossy-JPEG (\"lossy DNG\") raw data is not read; uncompressed and \
                 lossless-JPEG DNGs are"
                    .into(),
            ))
        }
        52546 => {
            return Err(CodecError::Unsupported(
                "JPEG XL-compressed DNG 1.7 raw data is not read; uncompressed and \
                 lossless-JPEG DNGs are"
                    .into(),
            ))
        }
        other => {
            return Err(CodecError::Unsupported(format!(
                "DNG compression {other} is not read; uncompressed and lossless-JPEG DNGs are"
            )))
        }
    }
    let active = match raw.uints_of(&tiff, TAG_ACTIVE_AREA) {
        Some(a) if a.len() == 4 => {
            let (t, l, b, r) = (a[0] as usize, a[1] as usize, a[2] as usize, a[3] as usize);
            if t >= b || l >= r || b > height || r > width {
                return Err(malformed(NAME, "ActiveArea lies outside the image"));
            }
            (t, l, b, r)
        }
        _ => (0, 0, height, width),
    };
    let (aw, ah) = (active.3 - active.1, active.2 - active.0);
    let crop = match (
        raw.floats_of(&tiff, TAG_CROP_ORIGIN),
        raw.floats_of(&tiff, TAG_CROP_SIZE),
    ) {
        (Some(o), Some(s)) if o.len() == 2 && s.len() == 2 => {
            let (x, y) = (
                o[0].round().max(0.0) as usize,
                o[1].round().max(0.0) as usize,
            );
            let (w, h) = (
                s[0].round().max(0.0) as usize,
                s[1].round().max(0.0) as usize,
            );
            if w > 0 && h > 0 && x.saturating_add(w) <= aw && y.saturating_add(h) <= ah {
                (x, y, w, h)
            } else {
                (0, 0, aw, ah)
            }
        }
        _ => (0, 0, aw, ah),
    };
    let (cfa, planes) = if photometric == PHOTOMETRIC_CFA {
        cfa_pattern(&tiff, &raw)?
    } else {
        (None, [0, 1, 2])
    };
    // Every buffer the develop holds at its peak: the raw samples (u16), the
    // normalised mosaic or planes (f32), three demosaiced planes (f32) and
    // the RGBA16 output checked by `check_decode` itself.
    let pixels = (width as u64).saturating_mul(height as u64);
    let extra = pixels.saturating_mul(spp as u64 * 6 + 12);
    let (w32, h32) = (
        u32::try_from(width).map_err(|_| malformed(NAME, "too wide"))?,
        u32::try_from(height).map_err(|_| malformed(NAME, "too tall"))?,
    );
    check_decode(limits, w32, h32, 8, extra)?;
    Ok(Plan {
        tiff,
        raw,
        width,
        height,
        spp,
        bits,
        compression,
        active,
        crop,
        orientation,
        cfa,
        planes,
    })
}

fn cfa_pattern(tiff: &Tiff, raw: &Ifd) -> Result<(Option<Pattern>, [usize; 3]), CodecError> {
    if raw.uint(tiff, TAG_CFA_LAYOUT).unwrap_or(1) != 1 {
        return Err(CodecError::Unsupported(
            "non-rectangular (staggered) CFA layouts are not read".into(),
        ));
    }
    let dims = raw
        .uints_of(tiff, TAG_CFA_REPEAT)
        .filter(|d| d.len() == 2)
        .ok_or_else(|| malformed(NAME, "the CFA has no CFARepeatPatternDim"))?;
    let (rows, cols) = (dims[0] as usize, dims[1] as usize);
    if !(1..=6).contains(&rows) || !(1..=6).contains(&cols) {
        return Err(CodecError::Unsupported(format!(
            "a {rows}x{cols} CFA pattern is not read (up to 6x6 is)"
        )));
    }
    let plane_colors = raw
        .uints_of(tiff, TAG_CFA_PLANE_COLOR)
        .unwrap_or_else(|| vec![0, 1, 2]);
    let mut sorted = plane_colors.clone();
    sorted.sort_unstable();
    if sorted != [0, 1, 2] {
        return Err(CodecError::Unsupported(
            "only red/green/blue sensors are read (this CFA has other colours)".into(),
        ));
    }
    let mut planes = [0usize; 3];
    for (plane, &code) in plane_colors.iter().enumerate() {
        planes[code as usize] = plane;
    }
    let codes = raw
        .uints_of(tiff, TAG_CFA_PATTERN)
        .filter(|c| c.len() == rows * cols)
        .ok_or_else(|| malformed(NAME, "the CFAPattern does not match its dimensions"))?;
    if codes.iter().any(|&c| c > 2) || (0..3).any(|c| !codes.contains(&c)) {
        return Err(malformed(
            NAME,
            "the CFAPattern is not a red/green/blue pattern",
        ));
    }
    let codes = codes.into_iter().map(|c| c as u8).collect();
    Ok((Some(Pattern { rows, cols, codes }), planes))
}

impl Plan<'_> {
    /// Output size after the crop and the orientation.
    fn output_size(&self) -> (usize, usize) {
        let (w, h) = (self.crop.2, self.crop.3);
        if self.orientation >= 5 {
            (h, w)
        } else {
            (w, h)
        }
    }
}

/// Header facts for a DNG: the developed size, 16-bit.
pub fn probe(bytes: &[u8], limits: ImportLimits) -> Result<ImageInfo, CodecError> {
    let plan = plan(bytes, limits)?;
    let (w, h) = plan.output_size();
    Ok(info(w as u32, h as u32, ImportFormat::Dng, true))
}

// ------------------------------------------------------------- samples ----

/// The raw samples, `width * height * spp`, from strips or tiles.
fn read_samples(plan: &Plan, limits: ImportLimits) -> Result<Vec<u16>, CodecError> {
    let (tiff, raw) = (&plan.tiff, &plan.raw);
    let (w, h, spp) = (plan.width, plan.height, plan.spp);
    let (block_w, block_h, offsets, counts) =
        if let Some(offsets) = raw.uints_of(tiff, TAG_TILE_OFFSETS) {
            let tw = raw.uint(tiff, TAG_TILE_WIDTH).unwrap_or(0) as usize;
            let tl = raw.uint(tiff, TAG_TILE_LENGTH).unwrap_or(0) as usize;
            if tw == 0 || tl == 0 {
                return Err(malformed(NAME, "a tiled raw image has no tile size"));
            }
            let counts = raw
                .uints_of(tiff, TAG_TILE_COUNTS)
                .ok_or_else(|| malformed(NAME, "TileByteCounts is missing"))?;
            (tw, tl, offsets, counts)
        } else {
            let offsets = raw
                .uints_of(tiff, TAG_STRIP_OFFSETS)
                .ok_or_else(|| malformed(NAME, "the raw image has no strips or tiles"))?;
            let counts = raw
                .uints_of(tiff, TAG_STRIP_COUNTS)
                .ok_or_else(|| malformed(NAME, "StripByteCounts is missing"))?;
            let rps = (raw.uint(tiff, TAG_ROWS_PER_STRIP).unwrap_or(u32::MAX) as usize).clamp(1, h);
            (w, rps, offsets, counts)
        };
    let across = w.div_ceil(block_w);
    let down = h.div_ceil(block_h);
    if offsets.len() < across * down || counts.len() < across * down {
        return Err(malformed(
            NAME,
            "fewer strips or tiles than the image needs",
        ));
    }
    let mut out = vec![0u16; w * h * spp];
    let stride = block_w
        .checked_mul(spp)
        .ok_or_else(|| malformed(NAME, "a tile is too wide"))?;
    for by in 0..down {
        for bx in 0..across {
            let k = by * across + bx;
            let (off, len) = (offsets[k] as usize, counts[k] as usize);
            let data = tiff
                .data
                .get(off..off.saturating_add(len))
                .ok_or_else(|| malformed(NAME, "a strip or tile lies past the end of the file"))?;
            let (x0, y0) = (bx * block_w, by * block_h);
            let rows = block_h.min(h - y0);
            let mut put = |r: usize, k: usize, v: u16| {
                let x = x0 + k / spp;
                if x < w {
                    out[((y0 + r) * w + x) * spp + k % spp] = v;
                }
            };
            if plan.compression == 7 {
                let cap = stride
                    .checked_mul(block_h)
                    .ok_or_else(|| malformed(NAME, "a tile is too large"))?;
                // The frame header is trusted only up to the tile's own size
                // and the decode budget.
                limits.check_alloc((cap as u64).saturating_mul(2))?;
                let frame = ljpeg::decode(data, cap)?;
                if frame.len() < rows * stride {
                    return Err(malformed(
                        NAME,
                        "a lossless-JPEG tile is smaller than its tile",
                    ));
                }
                for r in 0..rows {
                    for k in 0..stride {
                        put(r, k, frame[r * stride + k]);
                    }
                }
            } else {
                unpack(data, plan.bits, tiff.le, stride, rows, &mut put)?;
            }
        }
    }
    Ok(out)
}

/// Uncompressed samples: 8 bits as bytes, 16 in the file's byte order,
/// anything else packed MSB-first with every row starting on a byte.
fn unpack(
    data: &[u8],
    bits: u32,
    le: bool,
    stride: usize,
    rows: usize,
    put: &mut impl FnMut(usize, usize, u16),
) -> Result<(), CodecError> {
    let row_bytes = stride
        .checked_mul(bits as usize)
        .ok_or_else(|| malformed(NAME, "a strip or tile is too wide"))?
        .div_ceil(8);
    if row_bytes
        .checked_mul(rows)
        .is_none_or(|need| data.len() < need)
    {
        return Err(malformed(
            NAME,
            "an uncompressed strip or tile is truncated",
        ));
    }
    for r in 0..rows {
        let row = &data[r * row_bytes..(r + 1) * row_bytes];
        match bits {
            8 => {
                for (k, &v) in row.iter().enumerate() {
                    put(r, k, u16::from(v));
                }
            }
            16 => {
                for (k, &v) in row.as_chunks::<2>().0.iter().enumerate() {
                    put(
                        r,
                        k,
                        if le {
                            u16::from_le_bytes(v)
                        } else {
                            u16::from_be_bytes(v)
                        },
                    );
                }
            }
            _ => {
                let mut acc: u32 = 0;
                let mut have = 0u32;
                let mut bytes = row.iter();
                for k in 0..stride {
                    while have < bits {
                        acc = (acc << 8) | u32::from(*bytes.next().unwrap_or(&0));
                        have += 8;
                    }
                    have -= bits;
                    put(r, k, ((acc >> have) & ((1 << bits) - 1)) as u16);
                    acc &= (1u32 << have).wrapping_sub(1);
                }
            }
        }
    }
    Ok(())
}

// -------------------------------------------------------------- levels ----

/// Linearise, subtract black, scale by white: the active area as `spp`
/// interleaved `f32` samples in `0..` (not yet clipped).
fn normalise(plan: &Plan, samples: &[u16]) -> Result<Vec<f32>, CodecError> {
    let (tiff, raw) = (&plan.tiff, &plan.raw);
    let spp = plan.spp;
    let (top, left, bottom, right) = plan.active;
    let (aw, ah) = (right - left, bottom - top);
    let table: Option<Vec<f32>> = raw
        .uints_of(tiff, TAG_LINEARIZATION)
        .filter(|t| !t.is_empty())
        .map(|t| t.into_iter().map(|v| v as f32).collect());
    let (br, bc) = match raw.uints_of(tiff, TAG_BLACK_REPEAT) {
        Some(d) if d.len() == 2 && (1..=16).contains(&d[0]) && (1..=16).contains(&d[1]) => {
            (d[0] as usize, d[1] as usize)
        }
        _ => (1, 1),
    };
    let black = match raw.floats_of(tiff, TAG_BLACK_LEVEL) {
        None => vec![0.0; br * bc * spp],
        Some(b) if b.len() == br * bc * spp => b,
        Some(b) if b.len() == 1 => vec![b[0]; br * bc * spp],
        Some(_) => {
            return Err(malformed(
                NAME,
                "BlackLevel does not match its repeat pattern",
            ))
        }
    };
    let delta = |tag: u16, n: usize| -> Result<Vec<f64>, CodecError> {
        match raw.floats_of(tiff, tag) {
            None => Ok(vec![0.0; n]),
            Some(d) if d.len() == n => Ok(d),
            Some(_) => Err(malformed(
                NAME,
                "a BlackLevelDelta does not match the active area",
            )),
        }
    };
    let (dh, dv) = (delta(TAG_BLACK_DELTA_H, aw)?, delta(TAG_BLACK_DELTA_V, ah)?);
    let max_code = ((1u32 << plan.bits) - 1) as f64;
    let white = match raw.floats_of(tiff, TAG_WHITE_LEVEL) {
        None => vec![max_code; spp],
        Some(v) if v.len() == spp => v,
        Some(v) if v.len() == 1 => vec![v[0]; spp],
        Some(_) => return Err(malformed(NAME, "WhiteLevel does not match the samples")),
    };
    let mut out = Vec::with_capacity(aw * ah * spp);
    for y in 0..ah {
        for x in 0..aw {
            for s in 0..spp {
                let code = samples[((top + y) * plan.width + left + x) * spp + s];
                let v = match &table {
                    Some(t) => t[usize::from(code).min(t.len() - 1)],
                    None => f32::from(code),
                };
                let b = black[((y % br) * bc + x % bc) * spp + s] + dh[x] + dv[y];
                let range = (white[s] - b).max(1.0);
                out.push(((f64::from(v) - b) / range) as f32);
            }
        }
    }
    Ok(out)
}

// -------------------------------------------------------------- colour ----

type M3 = [[f64; 3]; 3];

/// Linear sRGB (D65) to CIE XYZ.
const SRGB_TO_XYZ: M3 = [
    [0.412_456_4, 0.357_576_1, 0.180_437_5],
    [0.212_672_9, 0.715_152_2, 0.072_175_0],
    [0.019_333_9, 0.119_192_0, 0.950_304_1],
];

fn mul(a: &M3, b: &M3) -> M3 {
    let mut m = [[0.0; 3]; 3];
    for (i, row) in m.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    m
}

fn apply(m: &M3, v: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2])
}

fn invert(m: &M3) -> Option<M3> {
    let c = |r: usize, s: usize| {
        let (r1, r2) = ((r + 1) % 3, (r + 2) % 3);
        let (s1, s2) = ((s + 1) % 3, (s + 2) % 3);
        m[r1][s1] * m[r2][s2] - m[r1][s2] * m[r2][s1]
    };
    let det = m[0][0] * c(0, 0) + m[0][1] * c(0, 1) + m[0][2] * c(0, 2);
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let mut out = [[0.0; 3]; 3];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = c(j, i) / det;
        }
    }
    Some(out)
}

fn matrix(values: &[f64]) -> Option<M3> {
    (values.len() == 9).then(|| [0, 1, 2].map(|r| [0, 1, 2].map(|c| values[r * 3 + c])))
}

/// White-balance multipliers per colour (red, green, blue; the smallest is
/// one) and the camera-RGB to linear-sRGB matrix.
fn colour(plan: &Plan) -> Result<([f64; 3], M3), CodecError> {
    let tiff = &plan.tiff;
    // Colour tags live in IFD0 by the DNG spec; some writers repeat them in
    // the raw IFD. Look in IFD0 first.
    let first = tiff.first_ifd().and_then(|o| tiff.ifd(o));
    let lookup = |tag: u16| -> Option<Vec<f64>> {
        first
            .as_ref()
            .and_then(|i| i.floats_of(tiff, tag))
            .or_else(|| plan.raw.floats_of(tiff, tag))
    };
    let illuminant = |tag: u16| lookup(tag).and_then(|v| v.first().copied());
    let second = lookup(TAG_COLOR_MATRIX_2).and_then(|v| matrix(&v));
    let use_second = second.is_some()
        && (illuminant(TAG_ILLUMINANT_2) == Some(f64::from(ILLUMINANT_D65))
            || illuminant(TAG_ILLUMINANT_1) != Some(f64::from(ILLUMINANT_D65)));
    let (cm, cc) = if use_second {
        (second, lookup(TAG_CAMERA_CALIBRATION_2))
    } else {
        (
            lookup(TAG_COLOR_MATRIX_1).and_then(|v| matrix(&v)),
            lookup(TAG_CAMERA_CALIBRATION_1),
        )
    };
    let cm = cm.ok_or_else(|| malformed(NAME, "no three-colour ColorMatrix1"))?;
    let identity: M3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    let cc = cc.and_then(|v| matrix(&v)).unwrap_or(identity);
    let ab = match lookup(TAG_ANALOG_BALANCE) {
        Some(v) if v.len() == 3 => [[v[0], 0.0, 0.0], [0.0, v[1], 0.0], [0.0, 0.0, v[2]]],
        _ => identity,
    };
    // Rows are camera planes; reorder them into red, green, blue.
    let plane_order = |m: M3| plan.planes.map(|p| m[p]);
    let xyz_to_cam = plane_order(mul(&mul(&ab, &cc), &cm));
    let mut cam_rgb = mul(&xyz_to_cam, &SRGB_TO_XYZ);
    for row in &mut cam_rgb {
        let sum: f64 = row.iter().sum();
        if !sum.is_finite() || sum.abs() < 1e-9 {
            return Err(malformed(NAME, "the colour matrix has a degenerate row"));
        }
        row.iter_mut().for_each(|v| *v /= sum);
    }
    let rgb_cam =
        invert(&cam_rgb).ok_or_else(|| malformed(NAME, "the colour matrix is singular"))?;
    let neutral = match lookup(TAG_AS_SHOT_NEUTRAL) {
        Some(n) if n.len() == 3 && n.iter().all(|&v| v > 0.0) => plan.planes.map(|p| n[p]),
        _ => apply(&xyz_to_cam, apply(&SRGB_TO_XYZ, [1.0; 3])),
    };
    if neutral.iter().any(|&v| !(v > 0.0 && v.is_finite())) {
        return Err(malformed(NAME, "the white balance is not positive"));
    }
    let inv = neutral.map(|v| 1.0 / v);
    let least = inv.iter().copied().fold(f64::INFINITY, f64::min);
    Ok((inv.map(|v| v / least), rgb_cam))
}

fn srgb_encode(v: f64) -> f64 {
    if v <= 0.003_130_8 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

// ------------------------------------------------------------ demosaic ----

/// Reflect `i` into `0..n` about the edges, keeping its parity (so a CFA
/// neighbour across the edge has the colour it would have had).
fn reflect(i: isize, n: usize) -> usize {
    if n <= 1 {
        return 0;
    }
    let period = 2 * (n as isize - 1);
    let m = i.rem_euclid(period);
    (if m >= n as isize { period - m } else { m }) as usize
}

/// Three planes (red, green, blue) from a white-balanced, clipped mosaic.
fn demosaic(cfa: &[f32], w: usize, h: usize, p: &Pattern) -> [Vec<f32>; 3] {
    let at = |x: isize, y: isize| -> (f32, u8) {
        let (x, y) = (reflect(x, w), reflect(y, h));
        (cfa[y * w + x], p.at(x, y))
    };
    let mut planes = [vec![0f32; w * h], vec![0f32; w * h], vec![0f32; w * h]];
    if !p.is_bayer() {
        for y in 0..h as isize {
            for x in 0..w as isize {
                let i = y as usize * w + x as usize;
                let (v, code) = at(x, y);
                let mut sum = [0f32; 3];
                let mut n = [0u32; 3];
                for dy in -2..=2 {
                    for dx in -2..=2 {
                        let (s, c) = at(x + dx, y + dy);
                        sum[c as usize] += s;
                        n[c as usize] += 1;
                    }
                }
                for c in 0..3 {
                    planes[c][i] = if c == usize::from(code) {
                        v
                    } else if n[c] > 0 {
                        sum[c] / n[c] as f32
                    } else {
                        0.0
                    };
                }
            }
        }
        return planes;
    }
    // Green everywhere: along the smoother direction, with the colour
    // channel's second derivative as the high-frequency correction.
    for y in 0..h as isize {
        for x in 0..w as isize {
            let i = y as usize * w + x as usize;
            let (c, code) = at(x, y);
            planes[1][i] = if code == 1 {
                c
            } else {
                let (l, r, u, d) = (
                    at(x - 1, y).0,
                    at(x + 1, y).0,
                    at(x, y - 1).0,
                    at(x, y + 1).0,
                );
                let (l2, r2, u2, d2) = (
                    at(x - 2, y).0,
                    at(x + 2, y).0,
                    at(x, y - 2).0,
                    at(x, y + 2).0,
                );
                let lap_h = 2.0 * c - l2 - r2;
                let lap_v = 2.0 * c - u2 - d2;
                let dh = (l - r).abs() + lap_h.abs();
                let dv = (u - d).abs() + lap_v.abs();
                let eh = (l + r) / 2.0 + lap_h / 4.0;
                let ev = (u + d) / 2.0 + lap_v / 4.0;
                let g = if dh < dv {
                    eh
                } else if dv < dh {
                    ev
                } else {
                    (eh + ev) / 2.0
                };
                g.clamp(0.0, 1.0)
            };
        }
    }
    // Red and blue: green plus the mean colour difference of the nearest
    // same-colour neighbours (two at a green site, four diagonals across).
    let green = planes[1].clone();
    let g_at = |x: isize, y: isize| green[reflect(y, h) * w + reflect(x, w)];
    for target in [0u8, 2] {
        for y in 0..h as isize {
            for x in 0..w as isize {
                let i = y as usize * w + x as usize;
                let (v, code) = at(x, y);
                planes[usize::from(target)][i] = if code == target {
                    v
                } else {
                    let mut sum = 0f32;
                    let mut n = 0u32;
                    for dy in -1..=1 {
                        for dx in -1..=1 {
                            let (s, c) = at(x + dx, y + dy);
                            if (dx, dy) != (0, 0) && c == target {
                                sum += s - g_at(x + dx, y + dy);
                                n += 1;
                            }
                        }
                    }
                    let diff = if n > 0 { sum / n as f32 } else { 0.0 };
                    (green[i] + diff).clamp(0.0, 1.0)
                };
            }
        }
    }
    planes
}

// -------------------------------------------------------------- decode ----

/// Develop a DNG into a 16-bit sRGB surface (see the module docs).
pub fn decode(bytes: &[u8], limits: ImportLimits) -> Result<DecodedSurface, CodecError> {
    let plan = plan(bytes, limits)?;
    let samples = read_samples(&plan, limits)?;
    let mut norm = normalise(&plan, &samples)?;
    drop(samples);
    let (wb, rgb_cam) = colour(&plan)?;
    let (aw, ah) = (plan.active.3 - plan.active.1, plan.active.2 - plan.active.0);
    let planes = match &plan.cfa {
        Some(p) => {
            for y in 0..ah {
                for x in 0..aw {
                    let v = &mut norm[y * aw + x];
                    *v = (*v * wb[usize::from(p.at(x, y))] as f32).clamp(0.0, 1.0);
                }
            }
            demosaic(&norm, aw, ah, p)
        }
        None => {
            let mut planes = [
                vec![0f32; aw * ah],
                vec![0f32; aw * ah],
                vec![0f32; aw * ah],
            ];
            for (i, px) in norm.as_chunks::<3>().0.iter().enumerate() {
                for c in 0..3 {
                    planes[c][i] = (px[plan.planes[c]] * wb[c] as f32).clamp(0.0, 1.0);
                }
            }
            planes
        }
    };
    drop(norm);
    let first = plan.tiff.first_ifd().and_then(|o| plan.tiff.ifd(o));
    let exposure = first
        .as_ref()
        .and_then(|i| i.floats_of(&plan.tiff, TAG_BASELINE_EXPOSURE))
        .and_then(|v| v.first().copied())
        .filter(|e| e.abs() <= 10.0)
        .unwrap_or(0.0);
    let gain = exposure.exp2();
    let (cx, cy, cw, ch) = plan.crop;
    let (ow, oh) = plan.output_size();
    let mut rgba = vec![0u16; ow * oh * 4];
    for oy in 0..oh {
        for ox in 0..ow {
            // Where this output pixel comes from inside the crop.
            let (sx, sy) = match plan.orientation {
                2 => (cw - 1 - ox, oy),
                3 => (cw - 1 - ox, ch - 1 - oy),
                4 => (ox, ch - 1 - oy),
                5 => (oy, ox),
                6 => (oy, ch - 1 - ox),
                7 => (cw - 1 - oy, ch - 1 - ox),
                8 => (cw - 1 - oy, ox),
                _ => (ox, oy),
            };
            let i = (cy + sy) * aw + cx + sx;
            let cam = [0, 1, 2].map(|c| f64::from(planes[c][i]));
            let rgb = apply(&rgb_cam, cam);
            let o = (oy * ow + ox) * 4;
            for c in 0..3 {
                let v = srgb_encode((rgb[c] * gain).clamp(0.0, 1.0));
                rgba[o + c] = (v * 65535.0 + 0.5) as u16;
            }
            rgba[o + 3] = u16::MAX;
        }
    }
    Ok(DecodedSurface {
        width: ow as u32,
        height: oh as u32,
        pixels: SurfacePixels::Rgba16(rgba),
        color_space: color::ColorSpace::Srgb,
        icc_profile: None,
        source_format: ImportFormat::Dng,
    })
}

#[doc(hidden)]
#[path = "raw_fixture.rs"]
pub mod fixture;

#[cfg(test)]
#[path = "raw_tests.rs"]
mod tests;
