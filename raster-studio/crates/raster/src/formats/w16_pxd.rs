//! W16-L: Pixelmator Pro `.pxd`, opened as its **QuickLook preview**.
//!
//! A `.pxd` is a macOS package: a folder holding `metadata.info` (an SQLite
//! database: the document and its layer tree), `data/` (one file per raster
//! layer, named by UUID) and `QuickLook/` with two previews Pixelmator Pro
//! writes on every save, `Thumbnail.tiff` and the smaller `Icon.tiff`
//! (layout as documented by the `pxdlib` reverse-engineering notes). Off a
//! Mac, or sent as one file, the package travels **zipped**; that is the
//! file this reader takes, and it opens `QuickLook/Thumbnail.tiff` (else
//! `Icon.tiff`), decoded as the TIFF it is.
//!
//! The layers are **not** read: their tiles are in Pixelmator's own
//! undocumented format. A bare `metadata.info` (an SQLite file with no
//! QuickLook folder beside it) holds no picture and is refused by name.

use super::super::malformed;
use super::{decode_embedded, sqlite, zip};
use crate::codec::{CodecError, DecodedSurface, ImportFormat, ImportLimits};

const NAME: &str = "Pixelmator Pro";

/// The previews looked for, best first. A zip made of the package folder
/// puts the folder's own name in front, which [`preview`] allows for.
const PREVIEWS: [&str; 2] = ["QuickLook/Thumbnail.tiff", "QuickLook/Icon.tiff"];

/// The preview image stored in a zipped `.pxd` package.
pub fn preview(bytes: &[u8], limits: ImportLimits) -> Result<Vec<u8>, CodecError> {
    if sqlite::looks_like_sqlite(bytes) {
        return Err(CodecError::Unsupported(
            "this is a Pixelmator Pro metadata.info database on its own: the picture is in \
             the .pxd package's QuickLook and data folders, which were not included; open \
             the whole .pxd (zipped) instead"
                .into(),
        ));
    }
    if !zip::looks_like_zip(bytes) {
        return Err(malformed(
            NAME,
            "a .pxd is read as a zipped Pixelmator Pro package, and this is not a ZIP",
        ));
    }
    let entries = zip::entries(bytes, NAME)?;
    for wanted in PREVIEWS {
        let found = entries.iter().find(|e| {
            let n = e.name.replace('\\', "/");
            n.eq_ignore_ascii_case(wanted)
                || n.to_ascii_lowercase()
                    .ends_with(&format!("/{}", wanted.to_ascii_lowercase()))
        });
        if let Some(e) = found {
            return zip::read(bytes, e, limits.max_alloc_bytes, NAME);
        }
    }
    Err(CodecError::Unsupported(
        "this Pixelmator Pro file has no QuickLook preview (Thumbnail.tiff or Icon.tiff); \
         its layer data itself is not read"
            .into(),
    ))
}

/// Decode the preview.
pub fn decode(bytes: &[u8], limits: ImportLimits) -> Result<DecodedSurface, CodecError> {
    let tiff = preview(bytes, limits)?;
    decode_embedded(NAME, &tiff, limits, ImportFormat::Pxd)
}

#[cfg(test)]
mod tests {
    use super::super::test_util::{fuzz, ramp, zip};
    use super::*;
    use crate::codec::{decode_surface_bytes_as, encode, ExportFormat, SurfacePixels};

    #[test]
    fn a_zipped_pxd_opens_as_its_quicklook_thumbnail() {
        let px = ramp(5, 3);
        let tiff = encode(ExportFormat::Tiff, 5, 3, &px).unwrap();
        let icon = encode(ExportFormat::Tiff, 1, 1, &[1, 2, 3, 255]).unwrap();
        for prefix in ["", "Picture.pxd/"] {
            let file = zip(
                &[
                    (
                        &format!("{prefix}metadata.info"),
                        b"SQLite format 3\0".as_slice(),
                    ),
                    (&format!("{prefix}QuickLook/Icon.tiff"), &icon),
                    (&format!("{prefix}QuickLook/Thumbnail.tiff"), &tiff),
                ],
                true,
            );
            let s =
                decode_surface_bytes_as(&file, ImportLimits::default(), ImportFormat::Pxd).unwrap();
            assert_eq!(
                (s.width, s.height, s.source_format),
                (5, 3, ImportFormat::Pxd)
            );
            assert_eq!(s.pixels, SurfacePixels::Rgba8(px.clone()), "{prefix}");
        }
        assert_eq!(ImportFormat::from_extension("PXD"), Some(ImportFormat::Pxd));
    }

    #[test]
    fn a_pxd_without_a_preview_is_refused_by_name_and_damage_never_panics() {
        let file = zip(&[("metadata.info", b"x".as_slice())], false);
        let err =
            decode_surface_bytes_as(&file, ImportLimits::default(), ImportFormat::Pxd).unwrap_err();
        assert!(err.to_string().contains("QuickLook"), "{err}");
        let mut db = b"SQLite format 3\0".to_vec();
        db.resize(200, 0);
        let err =
            decode_surface_bytes_as(&db, ImportLimits::default(), ImportFormat::Pxd).unwrap_err();
        assert!(err.to_string().contains("metadata.info"), "{err}");
        let tiff = encode(ExportFormat::Tiff, 4, 4, &[7u8; 64]).unwrap();
        fuzz(
            &zip(&[("QuickLook/Thumbnail.tiff", &tiff)], true),
            ImportFormat::Pxd,
        );
    }
}
