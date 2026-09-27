//! W18-D: Olympus ORF.
//!
//! An ORF is a TIFF whose header magic is `IIRO` / `IIRS` / `MMOR` instead
//! of 42; IFD0 holds the raw image's size, `BitsPerSample` and strips. Data
//! holding two bytes per pixel is read as 16-bit words in the file's byte
//! order. Anything smaller is Olympus's compressed ORF (every body since the
//! early E-series), which is refused by name. The CFA is the EXIF
//! `CFAPattern` when written, else red-green / green-blue; black is the DNG
//! `BlackLevel` when written, else 0.

use crate::codec::{CodecError, ImportLimits};

use super::{
    broken, ifd0, not_decoded, raw_ifd, strips, tiff_any, uncompressed_tiff, Mosaic, Want,
    TAG_HEIGHT, TAG_WIDTH,
};

const NAME: &str = "Olympus ORF";

pub(super) fn read(bytes: &[u8], limits: ImportLimits, want: Want) -> Result<Mosaic, CodecError> {
    let tiff = tiff_any(bytes).ok_or_else(|| broken(NAME, "not a TIFF"))?;
    let ifd0 = ifd0(&tiff, NAME)?;
    let raw = raw_ifd(&tiff, NAME)?;
    let width = raw.uint(&tiff, TAG_WIDTH).unwrap_or(0) as usize;
    let height = raw.uint(&tiff, TAG_HEIGHT).unwrap_or(0) as usize;
    let data = strips(&tiff, &raw, NAME)?;
    if data.len() < width.saturating_mul(height).saturating_mul(2) {
        return Err(not_decoded(NAME, "Olympus compression"));
    }
    uncompressed_tiff(&tiff, &ifd0, &raw, limits, want, NAME)
}
