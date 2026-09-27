//! W18-D: the vendor camera RAWs, read by this crate's own code (no
//! dependency) from the public descriptions of each container, and developed
//! through the DNG path's demosaic and tone stages ([`super::demosaic`],
//! [`super::render`]).
//!
//! # What decodes, per vendor
//!
//! | Vendor | Decoded | Refused by name |
//! | --- | --- | --- |
//! | Canon CR2 ([`canon`]) | lossless JPEG (ITU T.81 process 14) in vertical slices (`cr2_slice`, tag `0xC640`), 2 or 4 components; the sensor borders from the maker note's `SensorInfo`; black from the masked border; the as-shot balance from `ColorBalance` (`0x4001`) | sRAW / mRAW (YCbCr, subsampled); CR3 (ISO BMFF + CRX codec) |
//! | Nikon NEF / NRW ([`nikon`]) | uncompressed (`Compression = 1`): 16-bit words or packed MSB-first 10/12/14-bit rows; black (`0x003D`) and the as-shot balance (`WB_RBLevels`, `0x000C`) from the maker note | Nikon's Huffman-coded compression (`Compression = 34713`, lossless and lossy) |
//! | Sony ARW / SR2 / SRF ([`sony`]) | the curve-compressed ARW 2 (`Compression = 32767`, 8 bits per pixel: 16-pixel blocks of max / min / positions / 7-bit deltas, then the tone curve from `SonyToneCurve`, `0x7010`); uncompressed 16-bit words or packed rows | ARW 1 (DSLR-A100); Sony's lossless-compressed ARW (`Compression = 7`) |
//! | Fujifilm RAF ([`fuji`]) | uncompressed raw in the `FujiIFD` (`0xF000`) of the CFA section, Bayer or X-Trans (6x6 `XTransLayout`, `0x0131`); the crop from `RawImageCropTopLeft` / `RawImageCroppedSize`; black (`0xF00A`) and the as-shot balance (`WB_GRBLevels`, `0xF00E`) | Fujifilm's compressed RAF (lossless or lossy); RAFs without a `FujiIFD` |
//! | Olympus ORF ([`olympus`]) | uncompressed 16-bit words | Olympus's compressed ORF (every recent body) |
//! | Panasonic RW2 / RWL ([`panasonic`]) | uncompressed 16-bit words; the sensor borders, CFA, black and white-balance tags of the Panasonic IFD0 | Panasonic's packed RAW 1-4 compression (`34316`, `34826`, `34828`, `34830`) |
//! | Pentax PEF, Samsung SRW, other TIFF-shaped RAWs | uncompressed, as for NEF | their vendor compressions |
//!
//! The gating tests build each container synthetically from the same
//! public descriptions (see `tests.rs`). The ignored
//! `develop_real_camera_files_from_a_directory` was run by hand on public
//! sample files (2026-09-26): two CR2s (5472x3648, 5616x3744), uncompressed
//! 12- and 14-bit D800 NEFs, a curve-compressed and an uncompressed ARW,
//! two uncompressed RAFs (6000x4000, 4896x3264) and an E-1 ORF developed;
//! a lossless-compressed NEF, a compressed RAF and a packed RW2 were refused
//! by name. None of those files is in the repository.
//!
//! # Levels and colour
//!
//! Black comes from the file's own tags where one is read (the DNG
//! `BlackLevel` in the raw IFD first, then the vendor tag named above, then
//! the masked border for CR2), else the documented default named per
//! vendor. White is the DNG `WhiteLevel` (or a vendor tag) where present;
//! otherwise the image's own maximum when at least sixteen (and at least
//! 0.01% of) samples pile up there (a clipped highlight), else full scale.
//! White balance is the file's as-shot balance where read, else grey world
//! (each colour's mean over unclipped CFA blocks matched to green).
//!
//! No vendor file carries a DNG-style colour matrix, and the per-camera
//! tables other readers ship are not reproduced: the generic matrix is the
//! identity, i.e. white-balanced camera RGB is taken as linear sRGB. Colours
//! are therefore less saturated than Photopea's (which ships per-camera
//! matrices); the tone curve is the DNG path's (no contrast S-curve).
//!
//! Every read is a checked slice of the file; declared sizes are checked
//! against the import limits before any buffer exists; damaged files are
//! errors, never panics (see the fuzz test in `tests.rs`), so these decoders
//! run in the calling process rather than the decode worker.

use std::borrow::Cow;

use crate::codec::{CodecError, DecodedSurface, ImageInfo, ImportFormat, ImportLimits};

use super::{
    check_decode, demosaic, info, render, Ifd, Pattern, RawKind, Tiff, M3, TAG_BITS,
    TAG_BLACK_LEVEL, TAG_CFA_PATTERN, TAG_CFA_REPEAT, TAG_COMPRESSION, TAG_HEIGHT,
    TAG_NEW_SUBFILE_TYPE, TAG_ORIENTATION, TAG_PHOTOMETRIC, TAG_SAMPLES, TAG_STRIP_COUNTS,
    TAG_STRIP_OFFSETS, TAG_TILE_OFFSETS, TAG_WHITE_LEVEL, TAG_WIDTH,
};

mod canon;
mod fuji;
mod nikon;
mod olympus;
mod panasonic;
mod sony;

#[cfg(test)]
mod tests;

const TAG_EXIF_IFD: u16 = 34665;
const TAG_MAKER_NOTE: u16 = 37500;
const TAG_EXIF_CFA_PATTERN: u16 = 41730;

/// How far a read goes: the header facts only, or the samples too.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Want {
    Header,
    Pixels,
}

/// A vendor raw image, read and checked, ready for [`develop`].
pub(super) struct Mosaic {
    pub name: &'static str,
    pub width: usize,
    pub height: usize,
    /// `width * height` samples, row-major; empty for [`Want::Header`].
    pub samples: Vec<u16>,
    /// The CFA, phased at the raw image's origin.
    pub pattern: Pattern,
    /// `(top, left, bottom, right)`, bottom and right exclusive.
    pub active: (usize, usize, usize, usize),
    /// Black per 2x2 cell, indexed `(y % 2) * 2 + x % 2` in raw coordinates.
    pub black: [f64; 4],
    /// The saturation level, when the file says it.
    pub white: Option<f64>,
    /// The largest code the storage can hold (full scale).
    pub full_scale: f64,
    /// As-shot multipliers for red, green, blue, when the file says them.
    pub wb: Option<[f64; 3]>,
    pub orientation: u32,
}

impl Mosaic {
    fn output_size(&self) -> (usize, usize) {
        let (t, l, b, r) = self.active;
        let (w, h) = (r - l, b - t);
        if self.orientation >= 5 {
            (h, w)
        } else {
            (w, h)
        }
    }
}

/// Develop a vendor RAW (see the module docs).
pub fn decode(bytes: &[u8], limits: ImportLimits) -> Result<DecodedSurface, CodecError> {
    develop(read(bytes, limits, Want::Pixels)?)
}

/// Header facts for a vendor RAW: the developed size, 16-bit.
pub fn probe(bytes: &[u8], limits: ImportLimits) -> Result<ImageInfo, CodecError> {
    let m = read(bytes, limits, Want::Header)?;
    let (w, h) = m.output_size();
    Ok(info(w as u32, h as u32, ImportFormat::CameraRaw, true))
}

fn read(bytes: &[u8], limits: ImportLimits, want: Want) -> Result<Mosaic, CodecError> {
    let name = match super::identify(bytes) {
        Some(RawKind::Proprietary(name)) => name,
        _ => return Err(super::refusal(bytes)),
    };
    let m = match name {
        "Canon CR2" => canon::read(bytes, limits, want)?,
        "Canon CR3" => {
            return Err(not_decoded(
                name,
                "CRX compression (Canon's own wavelet codec inside an ISO BMFF container)",
            ))
        }
        "Nikon NEF" => nikon::read(bytes, limits, want)?,
        "Sony ARW" => sony::read(bytes, limits, want)?,
        "Fujifilm RAF" => fuji::read(bytes, limits, want)?,
        "Olympus ORF" => olympus::read(bytes, limits, want)?,
        "Panasonic RW2" => panasonic::read(bytes, limits, want)?,
        other => generic(bytes, limits, want, other)?,
    };
    validate(&m, want)?;
    Ok(m)
}

fn validate(m: &Mosaic, want: Want) -> Result<(), CodecError> {
    let (t, l, b, r) = m.active;
    if m.width == 0 || m.height == 0 || t >= b || l >= r || b > m.height || r > m.width {
        return Err(broken(m.name, "the active area lies outside the image"));
    }
    let p = &m.pattern;
    if !(1..=6).contains(&p.rows)
        || !(1..=6).contains(&p.cols)
        || p.codes.len() != p.rows * p.cols
        || p.codes.iter().any(|&c| c > 2)
        || (0..3).any(|c| !p.codes.contains(&c))
    {
        return Err(broken(m.name, "the CFA is not a red/green/blue pattern"));
    }
    if want == Want::Pixels && Some(m.samples.len()) != m.width.checked_mul(m.height) {
        return Err(broken(m.name, "the samples do not fill the image"));
    }
    Ok(())
}

/// A vendor RAW this build recognises but whose compression it does not
/// decode, named.
pub(super) fn not_decoded(name: &str, what: &str) -> CodecError {
    CodecError::Unsupported(format!(
        "opening this {name} file is not supported: its {what} is not decoded by this build, \
         which reads DNG and the uncompressed or openly documented vendor encodings. Convert \
         the file to DNG (for example with Adobe DNG Converter) and open that"
    ))
}

/// A damaged vendor RAW, named; the advice covers a camera file this reader
/// misjudges.
pub(super) fn broken(name: &str, what: impl std::fmt::Display) -> CodecError {
    CodecError::Unsupported(format!(
        "malformed {name} file: {what}. If this is a camera's own file, convert it to DNG (for \
         example with Adobe DNG Converter) and open that"
    ))
}

/// The allocation budget for a raw of `w x h`: the samples (u16), the
/// normalised mosaic (f32), three planes (f32) and the RGBA16 output.
pub(super) fn budget(
    name: &str,
    limits: ImportLimits,
    w: usize,
    h: usize,
) -> Result<(), CodecError> {
    let (w32, h32) = (
        u32::try_from(w).map_err(|_| broken(name, "too wide"))?,
        u32::try_from(h).map_err(|_| broken(name, "too tall"))?,
    );
    if w == 0 || h == 0 {
        return Err(broken(name, "the raw image has no pixels"));
    }
    let pixels = (w as u64).saturating_mul(h as u64);
    check_decode(limits, w32, h32, 8, pixels.saturating_mul(2 + 4 + 12))
}

// ---------------------------------------------------------------- TIFF ----

/// A TIFF, including the vendor variants whose header magic is not 42
/// (`IIRO` / `IIRS` / `MMOR` for Olympus, `IIU\0` for Panasonic).
pub(super) fn tiff_any(data: &[u8]) -> Option<Tiff<'_>> {
    let head = data.get(..4)?;
    let le = match head {
        b"II*\0" | b"IIRO" | b"IIRS" | b"IIU\0" => true,
        b"MM\0*" | b"MMOR" => false,
        _ => return None,
    };
    Some(Tiff { data, le })
}

/// IFD0 of a TIFF.
pub(super) fn ifd0(tiff: &Tiff, name: &str) -> Result<Ifd, CodecError> {
    tiff.first_ifd()
        .and_then(|o| tiff.ifd(o))
        .ok_or_else(|| broken(name, "IFD0 is missing or damaged"))
}

/// The TIFF `Orientation` of IFD0, 1-8.
pub(super) fn orientation(tiff: &Tiff, ifd0: &Ifd) -> u32 {
    ifd0.uint(tiff, TAG_ORIENTATION)
        .filter(|o| (1..=8).contains(o))
        .unwrap_or(1)
}

/// The EXIF IFD that IFD0 points at.
pub(super) fn exif_ifd(tiff: &Tiff, ifd0: &Ifd) -> Option<Ifd> {
    tiff.ifd(ifd0.uint(tiff, TAG_EXIF_IFD)? as usize)
}

/// Where the EXIF `MakerNote` lies: `(offset, length)` in the file.
pub(super) fn maker_note(tiff: &Tiff, ifd0: &Ifd) -> Option<(usize, usize)> {
    let e = exif_ifd(tiff, ifd0)?.get(TAG_MAKER_NOTE)?;
    let len = e.count as usize;
    tiff.data.get(e.at..e.at.checked_add(len)?)?;
    Some((e.at, len))
}

/// The raw IFD of a TIFF-shaped vendor file: the largest full-resolution
/// IFD with strips or tiles that is a CFA, is vendor-compressed, or holds
/// one sample of more than eight bits.
pub(super) fn raw_ifd(tiff: &Tiff, name: &str) -> Result<Ifd, CodecError> {
    let first = tiff
        .first_ifd()
        .ok_or_else(|| broken(name, "there is no IFD0"))?;
    tiff.all_ifds(first)
        .into_iter()
        .filter(|ifd| {
            let photometric = ifd.uint(tiff, TAG_PHOTOMETRIC);
            let compression = ifd.uint(tiff, TAG_COMPRESSION).unwrap_or(1);
            let spp = ifd.uint(tiff, TAG_SAMPLES).unwrap_or(1);
            let bits = ifd.uint(tiff, TAG_BITS).unwrap_or(0);
            let has_data =
                ifd.get(TAG_STRIP_OFFSETS).is_some() || ifd.get(TAG_TILE_OFFSETS).is_some();
            has_data
                && ifd.uint(tiff, TAG_NEW_SUBFILE_TYPE).unwrap_or(0) & 1 == 0
                && (photometric == Some(super::PHOTOMETRIC_CFA)
                    || matches!(
                        compression,
                        32767 | 32769 | 32770 | 32772 | 34713 | 65000 | 65535
                    )
                    || (spp == 1 && bits > 8))
        })
        .max_by_key(|ifd| {
            let w = u64::from(ifd.uint(tiff, TAG_WIDTH).unwrap_or(0));
            w * u64::from(ifd.uint(tiff, TAG_HEIGHT).unwrap_or(0))
        })
        .ok_or_else(|| broken(name, "no raw image IFD with strips"))
}

/// The bytes of an IFD's strips, in order (one strip borrowed, several
/// joined; every strip must lie inside the file).
pub(super) fn strips<'a>(
    tiff: &Tiff<'a>,
    ifd: &Ifd,
    name: &str,
) -> Result<Cow<'a, [u8]>, CodecError> {
    let offsets = ifd
        .uints_of(tiff, TAG_STRIP_OFFSETS)
        .filter(|o| !o.is_empty())
        .ok_or_else(|| broken(name, "the raw image has no strips"))?;
    let counts = ifd
        .uints_of(tiff, TAG_STRIP_COUNTS)
        .filter(|c| c.len() == offsets.len())
        .ok_or_else(|| broken(name, "StripByteCounts is missing"))?;
    let piece = |k: usize| -> Result<&'a [u8], CodecError> {
        let (off, len) = (offsets[k] as usize, counts[k] as usize);
        tiff.data
            .get(off..off.saturating_add(len))
            .ok_or_else(|| broken(name, "a strip lies past the end of the file"))
    };
    if offsets.len() == 1 {
        return Ok(Cow::Borrowed(piece(0)?));
    }
    let mut out = Vec::new();
    for k in 0..offsets.len() {
        out.extend_from_slice(piece(k)?);
    }
    Ok(Cow::Owned(out))
}

/// Uncompressed samples: 16-bit words in the given byte order when the data
/// holds two bytes per sample, else rows packed MSB-first (every row on a
/// byte boundary).
pub(super) fn unpack_plain(
    name: &str,
    data: &[u8],
    w: usize,
    h: usize,
    bits: u32,
    le: bool,
) -> Result<Vec<u16>, CodecError> {
    if !(8..=16).contains(&bits) {
        return Err(not_decoded(name, &format!("{bits}-bit sample storage")));
    }
    let n = w
        .checked_mul(h)
        .ok_or_else(|| broken(name, "the image is too large"))?;
    let mut out = vec![0u16; n];
    let words = n.saturating_mul(2);
    let (bits, le) = if data.len() >= words {
        (16, le)
    } else {
        (bits, le)
    };
    let row_bytes = w.saturating_mul(bits as usize).div_ceil(8);
    if row_bytes
        .checked_mul(h)
        .is_none_or(|need| data.len() < need)
    {
        return Err(broken(
            name,
            "the uncompressed raw data is shorter than the image",
        ));
    }
    super::unpack(data, bits, le, w, h, &mut |r, k, v| {
        if let Some(s) = out.get_mut(r * w + k) {
            *s = v;
        }
    })?;
    Ok(out)
}

/// The 2x2 CFA a vendor TIFF declares: TIFF/EP `CFARepeatPatternDim` +
/// `CFAPattern` in the raw IFD, else the EXIF `CFAPattern`, else `None`.
pub(super) fn tiff_cfa(tiff: &Tiff, ifd0: &Ifd, raw: &Ifd) -> Option<Pattern> {
    if let (Some(dims), Some(codes)) = (
        raw.uints_of(tiff, TAG_CFA_REPEAT),
        raw.uints_of(tiff, TAG_CFA_PATTERN),
    ) {
        if dims.len() == 2 {
            if let Some(p) = pattern(dims[0] as usize, dims[1] as usize, &codes) {
                return Some(p);
            }
        }
    }
    let e = exif_ifd(tiff, ifd0)?.get(TAG_EXIF_CFA_PATTERN)?;
    let b = tiff.bytes(e)?;
    let head = b.get(..4)?;
    // Two shorts (columns, rows) whose byte order writers disagree on.
    for le in [tiff.le, !tiff.le] {
        let s = |i: usize| {
            let v = [head[i], head[i + 1]];
            usize::from(if le {
                u16::from_le_bytes(v)
            } else {
                u16::from_be_bytes(v)
            })
        };
        let (cols, rows) = (s(0), s(2));
        let codes: Vec<u32> = b[4..].iter().map(|&c| u32::from(c)).collect();
        if let Some(p) = pattern(rows, cols, &codes) {
            return Some(p);
        }
    }
    None
}

/// A red/green/blue pattern of `rows x cols` codes, or `None`.
pub(super) fn pattern(rows: usize, cols: usize, codes: &[u32]) -> Option<Pattern> {
    let ok = (1..=6).contains(&rows)
        && (1..=6).contains(&cols)
        && codes.len() == rows * cols
        && codes.iter().all(|&c| c <= 2)
        && (0..3).all(|c| codes.contains(&c));
    ok.then(|| Pattern {
        rows,
        cols,
        codes: codes.iter().map(|&c| c as u8).collect(),
    })
}

/// Red-green / green-blue, the pattern assumed where a file names none.
pub(super) fn rggb() -> Pattern {
    Pattern {
        rows: 2,
        cols: 2,
        codes: vec![0, 1, 1, 2],
    }
}

/// The DNG `BlackLevel` / `WhiteLevel` some vendor files write in their raw
/// IFD: black per 2x2 cell (one value, or four), white.
pub(super) fn dng_levels(tiff: &Tiff, raw: &Ifd) -> (Option<[f64; 4]>, Option<f64>) {
    let black = raw.floats_of(tiff, TAG_BLACK_LEVEL).and_then(|b| cells(&b));
    let white = raw
        .floats_of(tiff, TAG_WHITE_LEVEL)
        .and_then(|w| w.first().copied())
        .filter(|&w| w > 0.0);
    (black, white)
}

/// Four per-cell values from one (all alike), four (one per cell) or any
/// other count (their mean).
pub(super) fn cells(v: &[f64]) -> Option<[f64; 4]> {
    if v.is_empty() || v.iter().any(|x| !x.is_finite() || *x < 0.0) {
        return None;
    }
    Some(match v.len() {
        4 => [v[0], v[1], v[2], v[3]],
        _ => [v.iter().sum::<f64>() / v.len() as f64; 4],
    })
}

/// The mean of the samples in `rows x cols` per 2x2 cell (raw coordinates),
/// when every cell has one.
pub(super) fn masked_black(
    samples: &[u16],
    width: usize,
    rows: std::ops::Range<usize>,
    cols: std::ops::Range<usize>,
) -> Option<[f64; 4]> {
    let mut sum = [0f64; 4];
    let mut n = [0u64; 4];
    for y in rows {
        for x in cols.clone() {
            let v = *samples.get(y.checked_mul(width)?.checked_add(x)?)?;
            let c = (y % 2) * 2 + x % 2;
            sum[c] += f64::from(v);
            n[c] += 1;
        }
    }
    (n.iter().all(|&k| k > 0)).then(|| [0, 1, 2, 3].map(|c| sum[c] / n[c] as f64))
}

/// Multipliers `[r, 1, b]` from levels `(r, g, b)`, when they are sane.
pub(super) fn balance(r: f64, g: f64, b: f64) -> Option<[f64; 3]> {
    let wb = [r / g, 1.0, b / g];
    wb.iter()
        .all(|m| m.is_finite() && (0.05..=20.0).contains(m))
        .then_some(wb)
}

// ------------------------------------------------------------- generic ----

/// Pentax PEF, Samsung SRW and any other TIFF-shaped RAW: the raw IFD,
/// uncompressed only.
fn generic(
    bytes: &[u8],
    limits: ImportLimits,
    want: Want,
    name: &'static str,
) -> Result<Mosaic, CodecError> {
    let tiff = tiff_any(bytes).ok_or_else(|| broken(name, "not a TIFF"))?;
    let ifd0 = ifd0(&tiff, name)?;
    let raw = raw_ifd(&tiff, name)?;
    let compression = raw.uint(&tiff, TAG_COMPRESSION).unwrap_or(1);
    if compression != 1 {
        return Err(not_decoded(
            name,
            &format!("vendor compression {compression}"),
        ));
    }
    uncompressed_tiff(&tiff, &ifd0, &raw, limits, want, name)
}

/// The uncompressed TIFF raw shared by NEF, ARW, PEF and SRW: size, bits,
/// strips, CFA (RGGB when undeclared), the DNG levels when written, the
/// orientation.
pub(super) fn uncompressed_tiff(
    tiff: &Tiff,
    ifd0: &Ifd,
    raw: &Ifd,
    limits: ImportLimits,
    want: Want,
    name: &'static str,
) -> Result<Mosaic, CodecError> {
    let width = raw.uint(tiff, TAG_WIDTH).unwrap_or(0) as usize;
    let height = raw.uint(tiff, TAG_HEIGHT).unwrap_or(0) as usize;
    budget(name, limits, width, height)?;
    let bits = raw.uint(tiff, TAG_BITS).unwrap_or(16);
    if raw.uint(tiff, TAG_SAMPLES).unwrap_or(1) != 1 {
        return Err(not_decoded(name, "multi-sample (non-mosaic) raw data"));
    }
    let samples = if want == Want::Pixels {
        let data = strips(tiff, raw, name)?;
        unpack_plain(name, &data, width, height, bits, tiff.le)?
    } else {
        Vec::new()
    };
    let (black, white) = dng_levels(tiff, raw);
    Ok(Mosaic {
        name,
        width,
        height,
        samples,
        pattern: tiff_cfa(tiff, ifd0, raw).unwrap_or_else(rggb),
        active: (0, 0, height, width),
        black: black.unwrap_or([0.0; 4]),
        white,
        full_scale: f64::from((1u32 << bits.clamp(1, 16)) - 1),
        wb: None,
        orientation: orientation(tiff, ifd0),
    })
}

// ------------------------------------------------------------- develop ----

/// The saturation level: the file's, else the image's maximum when a
/// highlight piles up there, else full scale.
fn white_level(m: &Mosaic) -> f64 {
    if let Some(w) = m.white {
        return w;
    }
    let (t, l, b, r) = m.active;
    let mut max = 0u16;
    let mut count = 0usize;
    for y in t..b {
        for &v in m
            .samples
            .get(y * m.width + l..y * m.width + r)
            .unwrap_or(&[])
        {
            if v > max {
                max = v;
                count = 1;
            } else if v == max {
                count += 1;
            }
        }
    }
    let n = (b - t) * (r - l);
    let black = m.black.iter().copied().fold(0.0, f64::max);
    if count >= 16 && count.saturating_mul(10_000) >= n && f64::from(max) > black + 1.0 {
        f64::from(max)
    } else {
        m.full_scale
    }
}

/// Grey-world multipliers: each colour's mean matched to green's, over the
/// CFA blocks (2x2; 3x3 for a 6x6 pattern) with no sample at or above 0.95,
/// so a highlight clipped in one channel does not skew the others.
fn grey_world(norm: &[f32], w: usize, p: &Pattern) -> [f64; 3] {
    let mut sum = [0f64; 3];
    let mut n = [0u64; 3];
    let block = if p.rows == 6 { 3 } else { p.rows.max(p.cols) };
    let h = norm.len() / w.max(1);
    for by in (0..h.saturating_sub(block - 1)).step_by(block) {
        for bx in (0..w.saturating_sub(block - 1)).step_by(block) {
            let cells = (0..block).flat_map(|dy| (0..block).map(move |dx| (bx + dx, by + dy)));
            if cells.clone().any(|(x, y)| norm[y * w + x] >= 0.95) {
                continue;
            }
            for (x, y) in cells {
                let c = usize::from(p.at(x, y));
                sum[c] += f64::from(norm[y * w + x]);
                n[c] += 1;
            }
        }
    }
    let mean = [0, 1, 2].map(|c| if n[c] > 0 { sum[c] / n[c] as f64 } else { 0.0 });
    if mean.iter().any(|&m| m <= 1e-6) {
        return [1.0; 3];
    }
    [mean[1] / mean[0], 1.0, mean[1] / mean[2]]
}

const IDENTITY: M3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// Levels, white balance, the shared demosaic and the shared render.
fn develop(m: Mosaic) -> Result<DecodedSurface, CodecError> {
    let (t, l, b, r) = m.active;
    let (aw, ah) = (r - l, b - t);
    let white = white_level(&m);
    let p = Pattern {
        rows: m.pattern.rows,
        cols: m.pattern.cols,
        codes: (0..m.pattern.rows * m.pattern.cols)
            .map(|k| m.pattern.at(k % m.pattern.cols + l, k / m.pattern.cols + t))
            .collect(),
    };
    let mut norm = Vec::with_capacity(aw * ah);
    for y in t..b {
        for x in l..r {
            let v = f64::from(m.samples.get(y * m.width + x).copied().unwrap_or(0));
            let black = m.black[(y % 2) * 2 + x % 2];
            let range = (white - black).max(1.0);
            norm.push(((v - black) / range).max(0.0) as f32);
        }
    }
    drop(m.samples);
    let wb = m.wb.unwrap_or_else(|| grey_world(&norm, aw, &p));
    let least = wb.iter().copied().fold(f64::INFINITY, f64::min);
    let wb = if least.is_finite() && least > 0.0 {
        wb.map(|v| v / least)
    } else {
        [1.0; 3]
    };
    for (i, v) in norm.iter_mut().enumerate() {
        let c = usize::from(p.at(i % aw, i / aw));
        *v = (*v * wb[c] as f32).clamp(0.0, 1.0);
    }
    let planes = demosaic(&norm, aw, ah, &p);
    drop(norm);
    Ok(render(
        &planes,
        aw,
        (0, 0, aw, ah),
        m.orientation,
        &IDENTITY,
        1.0,
        ImportFormat::CameraRaw,
    ))
}
