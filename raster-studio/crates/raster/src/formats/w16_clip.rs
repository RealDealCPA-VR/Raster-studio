//! W16-L: Clip Studio Paint `.clip`, opened as its **canvas preview**.
//!
//! A `.clip` is a chunk file: `CSFCHUNK`, the file size and the offset of
//! the first chunk (8 bytes each, big endian), then chunks of `CHNK`, a
//! four-letter type and an 8-byte big-endian length. `CHNKHead` holds the
//! header, each `CHNKExta` one block of external (layer tile) data, and
//! `CHNKSQLi` an SQLite database describing the document: its canvas, its
//! layer tree and, in the table `CanvasPreview`, a PNG of the flattened
//! canvas (`ImageData`) that Clip Studio writes on every save.
//!
//! What opens is that preview, read through the bounded SQLite reader in
//! [`super::sqlite`] and decoded by the codec facade. The layers are
//! **not** read: their pixels live in the `Exta` chunks in Clip Studio's
//! own tiled block format, whose layout is not publicly documented, so a
//! `.clip` opens flattened.

use super::super::malformed;
use super::sqlite::{Db, Value};
use super::{bytes_at, decode_embedded, u64be};
use crate::codec::{CodecError, DecodedSurface, ImportFormat, ImportLimits};

const NAME: &str = "Clip Studio";
const MAGIC: &[u8] = b"CSFCHUNK";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";

/// `true` when `head` starts with `CSFCHUNK`.
pub fn looks_like_clip(head: &[u8]) -> bool {
    head.starts_with(MAGIC)
}

/// One chunk of a `.clip`: its type and data.
pub type Chunk<'a> = ([u8; 4], &'a [u8]);

/// The chunks of a `.clip`.
pub fn chunks(bytes: &[u8]) -> Result<Vec<Chunk<'_>>, CodecError> {
    if !looks_like_clip(bytes) {
        return Err(malformed(NAME, "it does not start with CSFCHUNK"));
    }
    let first = u64be(bytes, 16, NAME)?;
    let mut at = usize::try_from(first).unwrap_or(usize::MAX).max(24);
    let mut out = Vec::new();
    while at < bytes.len() {
        let head = bytes_at(bytes, at, 16, NAME)?;
        if &head[..4] != b"CHNK" {
            return Err(malformed(NAME, "a chunk header is damaged"));
        }
        let mut kind = [0u8; 4];
        kind.copy_from_slice(&head[4..8]);
        let len = usize::try_from(u64be(bytes, at + 8, NAME)?).unwrap_or(usize::MAX);
        let data = bytes_at(bytes, at + 16, len, NAME)?;
        out.push((kind, data));
        at = at + 16 + len;
        if &kind == b"Foot" {
            break;
        }
    }
    Ok(out)
}

/// The canvas preview PNG stored in the file's database.
pub fn preview_png(bytes: &[u8], limits: ImportLimits) -> Result<Vec<u8>, CodecError> {
    let sqlite = chunks(bytes)?
        .into_iter()
        .find(|(k, _)| k == b"SQLi")
        .map(|(_, d)| d)
        .ok_or_else(|| malformed(NAME, "it has no CHNKSQLi database chunk"))?;
    let db = Db::open(sqlite, NAME)?;
    let cap = limits.max_alloc_bytes;
    let mut best: Option<Vec<u8>> = None;
    let mut keep = |blob: &[u8]| {
        if blob.starts_with(PNG) && best.as_ref().is_none_or(|b| blob.len() > b.len()) {
            best = Some(blob.to_vec());
        }
    };
    if let Some(table) = db.table("CanvasPreview")? {
        let column = table
            .columns
            .iter()
            .position(|c| c.eq_ignore_ascii_case("ImageData"));
        for row in db.rows(table.root, cap)? {
            match column.and_then(|c| row.get(c)) {
                Some(Value::Blob(b)) => keep(b),
                _ => row.iter().filter_map(Value::as_blob).for_each(&mut keep),
            }
        }
    }
    best.ok_or_else(|| {
        CodecError::Unsupported(
            "this .clip has no canvas preview (CanvasPreview.ImageData); Clip Studio's \
             layer data itself is not read"
                .into(),
        )
    })
}

/// Decode the canvas preview.
pub fn decode(bytes: &[u8], limits: ImportLimits) -> Result<DecodedSurface, CodecError> {
    let png = preview_png(bytes, limits)?;
    decode_embedded(NAME, &png, limits, ImportFormat::Clip)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::sqlite::writer;
    use super::super::test_util::{fuzz, ramp};
    use super::*;
    use crate::codec::{decode_surface_bytes, encode, probe_bytes, ExportFormat, SurfacePixels};

    pub fn clip_file(preview: &[u8]) -> Vec<u8> {
        let db = writer::build(&[
            (
                "Canvas",
                "CREATE TABLE Canvas (_PW_ID INTEGER PRIMARY KEY, MainId INTEGER, CanvasWidth REAL)",
                vec![vec![Value::Null, Value::Int(1), Value::Float(6.0)]],
            ),
            (
                "CanvasPreview",
                "CREATE TABLE CanvasPreview (_PW_ID INTEGER PRIMARY KEY, MainId INTEGER, \
                 CanvasId INTEGER, ImageType INTEGER, ImageWidth INTEGER, ImageHeight INTEGER, \
                 ImageData BLOB)",
                vec![vec![
                    Value::Null,
                    Value::Int(1),
                    Value::Int(1),
                    Value::Int(1),
                    Value::Int(6),
                    Value::Int(4),
                    Value::Blob(preview.to_vec()),
                ]],
            ),
        ]);
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&[0; 8]);
        out.extend_from_slice(&24u64.to_be_bytes());
        for (kind, data) in [
            (*b"Head", vec![0u8; 40]),
            (*b"Exta", vec![1u8; 64]),
            (*b"SQLi", db),
            (*b"Foot", Vec::new()),
        ] {
            out.extend_from_slice(b"CHNK");
            out.extend_from_slice(&kind);
            out.extend_from_slice(&(data.len() as u64).to_be_bytes());
            out.extend_from_slice(&data);
        }
        let len = out.len() as u64;
        out[8..16].copy_from_slice(&len.to_be_bytes());
        out
    }

    #[test]
    fn a_clip_opens_as_its_canvas_preview() {
        let px = ramp(6, 4);
        let png = encode(ExportFormat::Png, 6, 4, &px).unwrap();
        let file = clip_file(&png);
        assert!(looks_like_clip(&file));
        let s = decode_surface_bytes(&file, ImportLimits::default()).unwrap();
        assert_eq!(
            (s.width, s.height, s.source_format),
            (6, 4, ImportFormat::Clip)
        );
        assert_eq!(s.pixels, SurfacePixels::Rgba8(px));
        let info = probe_bytes(&file, ImportLimits::default()).unwrap();
        assert_eq!((info.width, info.format), (6, ImportFormat::Clip));
        assert_eq!(
            ImportFormat::from_extension("clip"),
            Some(ImportFormat::Clip)
        );
    }

    #[test]
    fn a_clip_without_a_preview_is_refused_by_name_and_damage_never_panics() {
        let file = clip_file(b"not a png");
        let err = decode_surface_bytes(&file, ImportLimits::default()).unwrap_err();
        assert!(err.to_string().contains("CanvasPreview"), "{err}");
        let png = encode(ExportFormat::Png, 5, 5, &[9u8; 100]).unwrap();
        fuzz(&clip_file(&png), ImportFormat::Clip);
    }
}
