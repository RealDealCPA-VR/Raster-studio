//! Photoshop custom shapes (`.csh`).
//!
//! `cush`, a `u32` version (2) and a `u32` shape count, then per shape: a
//! Unicode name, padding to a four-byte file offset, a `u32` shape version
//! (1), a `u32` byte length, and inside that length a Pascal id, the shape's
//! bounds (four `u32`: top, left, bottom, right) and Photoshop path records —
//! 26 bytes each, the same records a vector mask stores. Record 0/3 opens a
//! closed/open subpath of N knots; 1, 2, 4 and 5 are its knots (three
//! points, each a vertical then a horizontal signed 8.24 fixed-point
//! number: the incoming control, the anchor, the outgoing control); 6, 7
//! and 8 (fill rule, clipboard, initial fill) carry no geometry and are
//! skipped. Every shape is read through its own length, so one damaged
//! shape is refused by name while the rest still load.

use psd::bytes::Cursor;

use super::{
    check_count, KnotResource, Loaded, ResourceError, ShapeResource, SubpathResource, MAX_ENTRIES,
    MAX_KNOTS,
};

/// Longest shape name read, in UTF-16 units.
const MAX_NAME_UNITS: usize = 1_024;

/// Bytes in one path record.
const RECORD: usize = 26;

/// Parse a `.csh` file.
pub fn parse(bytes: &[u8]) -> Result<Loaded<ShapeResource>, ResourceError> {
    let mut cur = Cursor::new(bytes);
    if cur.tag().ok().as_ref() != Some(b"cush") {
        return Err(ResourceError::BadSignature {
            what: "custom shape",
        });
    }
    let version = cur.u32()?;
    if version != 2 {
        return Err(ResourceError::Unsupported {
            what: "custom shape file version",
            detail: version.to_string(),
        });
    }
    let count = cur.u32()? as usize;
    check_count("custom shape count", count, MAX_ENTRIES)?;
    let mut knots_left = MAX_KNOTS;
    let mut loaded = Loaded::default();
    for index in 0..count {
        let name = match cur.unicode_string(MAX_NAME_UNITS) {
            Ok(name) => name,
            Err(_) => {
                loaded
                    .refused
                    .push(format!("the file ends before shape {}", index + 1));
                break;
            }
        };
        let name = if name.trim().is_empty() {
            format!("Shape {}", index + 1)
        } else {
            name
        };
        cur.align_to(4)?;
        let shape_version = cur.u32()?;
        let len = cur.u32()? as usize;
        let Ok(mut body) = cur.sub(len) else {
            loaded
                .refused
                .push(format!("{name}: runs past the end of the file"));
            break;
        };
        if shape_version != 1 {
            loaded
                .refused
                .push(format!("{name}: shape version {shape_version}"));
            continue;
        }
        match read_shape(&mut body, name.clone(), &mut knots_left) {
            Ok(shape) => loaded.items.push(shape),
            Err(e) => loaded.refused.push(format!("{name}: {e}")),
        }
    }
    Ok(loaded)
}

/// An 8.24 fixed-point path coordinate.
fn fixed(cur: &mut Cursor<'_>) -> Result<f64, ResourceError> {
    Ok(f64::from(cur.i32()?) / f64::from(1u32 << 24))
}

/// A point as the record stores it (vertical first), as `(x, y)`.
fn point(cur: &mut Cursor<'_>) -> Result<[f64; 2], ResourceError> {
    let y = fixed(cur)?;
    let x = fixed(cur)?;
    Ok([x, y])
}

fn read_shape(
    body: &mut Cursor<'_>,
    name: String,
    knots_left: &mut usize,
) -> Result<ShapeResource, ResourceError> {
    let id = body.pascal_string(1)?;
    // The bounds: the path records are what the shape draws.
    body.skip(16)?;
    let mut subpaths: Vec<SubpathResource> = Vec::new();
    // Knots still owed to the open subpath.
    let mut owed = 0usize;
    while body.remaining() >= RECORD {
        let mut record = body.sub(RECORD)?;
        let selector = record.u16()?;
        match selector {
            0 | 3 => {
                if owed != 0 {
                    return Err(ResourceError::Malformed(
                        "a subpath ends before its knots do".into(),
                    ));
                }
                owed = record.u16()? as usize;
                if owed > *knots_left {
                    return Err(ResourceError::LimitExceeded {
                        what: "custom shape knots",
                        value: owed as u64,
                        max: *knots_left as u64,
                    });
                }
                *knots_left -= owed;
                subpaths.push(SubpathResource {
                    closed: selector == 0,
                    knots: Vec::with_capacity(owed.min(body.remaining() / RECORD)),
                });
            }
            1 | 2 | 4 | 5 => {
                let Some(sub) = subpaths.last_mut().filter(|_| owed > 0) else {
                    return Err(ResourceError::Malformed(
                        "a knot outside any subpath".into(),
                    ));
                };
                let before = point(&mut record)?;
                let anchor = point(&mut record)?;
                let after = point(&mut record)?;
                sub.knots.push(KnotResource {
                    before,
                    anchor,
                    after,
                });
                owed -= 1;
            }
            // Fill rule, clipboard and initial-fill records carry no geometry.
            6..=8 => {}
            other => {
                return Err(ResourceError::Malformed(format!(
                    "unknown path record {other}"
                )))
            }
        }
    }
    if owed != 0 {
        return Err(ResourceError::Malformed(
            "the shape ends before its knots do".into(),
        ));
    }
    subpaths.retain(|s| !s.knots.is_empty());
    if subpaths.is_empty() {
        return Err(ResourceError::Malformed("the shape has no path".into()));
    }
    Ok(ShapeResource { name, id, subpaths })
}
