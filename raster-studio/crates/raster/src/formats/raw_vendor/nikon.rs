//! W18-D: Nikon NEF / NRW.
//!
//! A NEF is a TIFF whose full-resolution CFA image is a `SubIFD` of IFD0.
//! Uncompressed data (`Compression = 1`) is read: two bytes per sample in
//! the file's byte order when the strip holds that many, else rows packed
//! MSB-first at `BitsPerSample` (10, 12 or 14). The CFA comes from the raw
//! IFD's TIFF/EP `CFAPattern` (red-green / green-blue when absent).
//!
//! The maker note is `Nikon\0` + version + two bytes, then a TIFF header of
//! its own (offsets relative to it): `BlackLevel` (`0x003D`, four shorts,
//! one per 2x2 cell) and `WB_RBLevels` (`0x000C`, rationals: red, then
//! blue, as multipliers) are read from it. With no black tag the black is
//! 0.
//!
//! Nikon's Huffman-coded compression (`Compression = 34713`, which the
//! "lossless compressed" and "compressed" settings write) is refused by
//! name: its code tables are not in the file, and the only descriptions of
//! them this project found are existing decoders' source code (dcraw /
//! LibRaw, the LGPL Rust readers) or write-ups taken from it (Laurent
//! Clevy's NEF page describes the `0x0096` curve by quoting dcraw), which
//! this project does not copy.

use crate::codec::{CodecError, ImportLimits};

use super::{
    broken, cells, ifd0, maker_note, not_decoded, raw_ifd, tiff_any, uncompressed_tiff, Mosaic,
    Tiff, Want, TAG_COMPRESSION,
};

const NAME: &str = "Nikon NEF";
const TAG_WB_RB_LEVELS: u16 = 0x000C;
const TAG_BLACK_LEVEL: u16 = 0x003D;

pub(super) fn read(bytes: &[u8], limits: ImportLimits, want: Want) -> Result<Mosaic, CodecError> {
    let tiff = tiff_any(bytes).ok_or_else(|| broken(NAME, "not a TIFF"))?;
    let ifd0 = ifd0(&tiff, NAME)?;
    let raw = raw_ifd(&tiff, NAME)?;
    match raw.uint(&tiff, TAG_COMPRESSION).unwrap_or(1) {
        1 => {}
        34713 => {
            return Err(not_decoded(
                NAME,
                "Nikon Huffman compression (the \"lossless compressed\" and \"compressed\" \
                 NEF settings)",
            ))
        }
        other => return Err(not_decoded(NAME, &format!("raw compression {other}"))),
    }
    let mut m = uncompressed_tiff(&tiff, &ifd0, &raw, limits, want, NAME)?;
    let note = maker_note(&tiff, &ifd0).and_then(|(at, len)| bytes.get(at..at + len));
    if let Some(note) = note {
        if let Some((black, wb)) = maker_levels(note) {
            if m.black == [0.0; 4] {
                if let Some(b) = black {
                    m.black = b;
                }
            }
            m.wb = wb;
        }
    }
    Ok(m)
}

/// Black per cell and the as-shot multipliers from a Nikon maker note.
#[allow(clippy::type_complexity)]
fn maker_levels(note: &[u8]) -> Option<(Option<[f64; 4]>, Option<[f64; 3]>)> {
    if !note.starts_with(b"Nikon\0") {
        return None;
    }
    let inner = note.get(10..)?;
    let tiff = super::tiff_any(inner)?;
    let ifd = tiff.ifd(tiff.first_ifd()?)?;
    let black = ifd
        .floats_of(&tiff, TAG_BLACK_LEVEL)
        .and_then(|b| cells(&b));
    let wb = rb_levels(&tiff, &ifd);
    Some((black, wb))
}

fn rb_levels(tiff: &Tiff, ifd: &super::Ifd) -> Option<[f64; 3]> {
    let v = ifd.floats_of(tiff, TAG_WB_RB_LEVELS)?;
    let (r, b) = (*v.first()?, *v.get(1)?);
    super::balance(r, 1.0, b)
}
