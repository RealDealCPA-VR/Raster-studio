//! The image-resources section: thumbnails, resolution, guides, ICC profiles.
//!
//! ```text
//! '8BIM'  u16 id  pascal-name (field padded to even)  u32 size  data (padded to even)
//! ```
//!
//! The padding is the part that bites: both the name field *and* the data are
//! padded to an even length, and the padding byte is **not** counted in the
//! declared size. A reader that trusts the size alone drifts one byte out of
//! step on the first odd-sized resource and reads the rest of the section as
//! noise.
//!
//! Resources are preserved verbatim. This crate interprets 1005,
//! `ResolutionInfo` — because a document with no resolution resource opens in
//! Photoshop at 1 dpi — and 1039, the ICC profile.
//!
//! W11-C: it also reads and builds the document-level resources an editor
//! maps onto its own model: 1032 grid and guides ([`guides`]), 2000–2997
//! saved paths and 1025 the work path ([`saved_paths`]), 1050 slices
//! ([`slices`]), and 1006 / 1045 the alpha channel names that name the
//! merged image's extra channels ([`alpha_channels`] /
//! [`set_alpha_channels`]). Every parser here is bounded: counts are checked
//! against the bytes present (and a fixed ceiling) before anything is
//! reserved, and a malformed resource yields what parsed before the damage,
//! never a panic.

use crate::bytes::{Cursor, Sink};
use crate::descriptor::{Descriptor, Value};
use crate::error::{PsdError, PsdResult};
use crate::header::Depth;
use crate::limits::{check_limit, ReadOptions};
use crate::model::{ImageResource, PsdFile, Rect};
use crate::shape::VectorPath;

/// `8BIM`, the signature every resource block starts with.
pub const RESOURCE_SIGNATURE: [u8; 4] = *b"8BIM";

/// Resource id 1005: pixels per inch for both axes, plus display units.
pub const ID_RESOLUTION_INFO: u16 = 1005;

/// Resource id 1039: the embedded ICC profile (an `icc` block, the raw bytes
/// of the profile itself).
pub const ID_ICC_PROFILE: u16 = 1039;

/// Parse the whole section from a cursor bounded to it.
pub fn read_resources(
    cur: &mut Cursor<'_>,
    opts: &ReadOptions,
    warnings: &mut Vec<String>,
) -> PsdResult<Vec<ImageResource>> {
    let mut out = Vec::new();
    while cur.remaining() >= 8 {
        let at = cur.offset();
        let sig = cur.tag()?;
        if sig != RESOURCE_SIGNATURE {
            warnings.push(format!(
                "image resource at offset {at} has signature {:?}; \
                 stopped reading resources there",
                crate::error::tag_name(sig)
            ));
            cur.skip_rest();
            break;
        }
        let id = cur.u16()?;
        let name = cur.pascal_string(2)?;
        let size = cur.u32()? as usize;
        check_limit(
            "image resource size",
            size as u64,
            opts.max_resource_bytes as u64,
        )?;
        let data = cur.take(size)?.to_vec();
        if size % 2 == 1 {
            // The pad byte is outside the declared size and may be missing at
            // the very end of the section.
            let _ = cur.skip(1);
        }
        out.push(ImageResource { id, name, data });
    }
    Ok(out)
}

pub fn write_resources(resources: &[ImageResource], sink: &mut Sink) {
    for r in resources {
        sink.tag(&RESOURCE_SIGNATURE);
        sink.u16(r.id);
        sink.pascal_string(&r.name, 2);
        sink.u32(r.data.len() as u32);
        sink.bytes(&r.data);
        if r.data.len() % 2 == 1 {
            sink.u8(0);
        }
    }
}

/// A `ResolutionInfo` resource at `dpi` in both axes.
///
/// ```text
/// u32 h-res (16.16 fixed)  u16 h-res-unit  u16 width-unit
/// u32 v-res (16.16 fixed)  u16 v-res-unit  u16 height-unit
/// ```
///
/// Units 1 and 2 are "pixels per inch" and "inches", which is what Photoshop
/// writes for a screen-resolution document.
pub fn resolution_info(dpi: f64) -> ImageResource {
    let fixed = (dpi * 65536.0).round().clamp(0.0, f64::from(u32::MAX)) as u32;
    let mut s = Sink::new();
    s.u32(fixed);
    s.u16(1);
    s.u16(2);
    s.u32(fixed);
    s.u16(1);
    s.u16(2);
    ImageResource {
        id: ID_RESOLUTION_INFO,
        name: String::new(),
        data: s.into_inner(),
    }
}

/// Read the horizontal dpi out of a 1005 resource.
pub fn resolution_dpi(r: &ImageResource) -> Option<f64> {
    if r.id != ID_RESOLUTION_INFO || r.data.len() < 4 {
        return None;
    }
    let fixed = u32::from_be_bytes([r.data[0], r.data[1], r.data[2], r.data[3]]);
    Some(f64::from(fixed) / 65536.0)
}

/// The embedded ICC profile — resource 1039 — as the raw profile bytes.
///
/// Photoshop writes one profile per file; if several claim the id, the first
/// wins, matching how a reader elsewhere treats single-instance resources.
/// `None` when the file carries no profile (or an empty one, which carries no
/// information either).
pub fn icc_profile(resources: &[ImageResource]) -> Option<&[u8]> {
    resources
        .iter()
        .find(|r| r.id == ID_ICC_PROFILE)
        .map(|r| r.data.as_slice())
        .filter(|d| !d.is_empty())
}

// ------------------------------------------------------------------ W11-C
//
// The document-level resources an editor maps onto its own model.

/// Resource id 1006: the alpha channel names as Pascal strings, one per
/// extra channel of the merged image that is not transparency.
pub const ID_ALPHA_NAMES: u16 = 1006;

/// Resource id 1025: the work path (unsaved), the same records as a saved path.
pub const ID_WORK_PATH: u16 = 1025;

/// Resource id 1032: grid and guides.
pub const ID_GRID_GUIDES: u16 = 1032;

/// Resource id 1045: the alpha channel names as Unicode strings. Preferred
/// over 1006 when both are present, because 1006 truncates at 255 bytes.
pub const ID_UNICODE_ALPHA_NAMES: u16 = 1045;

/// Resource id 1050: slices.
pub const ID_SLICES: u16 = 1050;

/// The first and last resource ids Photoshop gives saved paths.
pub const ID_SAVED_PATH_FIRST: u16 = 2000;
pub const ID_SAVED_PATH_LAST: u16 = 2997;

/// Most guides one 1032 resource may yield; the rest are ignored.
pub const MAX_GUIDES: usize = 65_536;

/// Most slices one 1050 resource may yield; the rest are ignored.
pub const MAX_SLICES: usize = 65_536;

/// Most alpha channels: the format's 56-channel ceiling less RGB and
/// transparency.
pub const MAX_ALPHA_CHANNELS: usize = 52;

/// Longest slice or channel string read, in UTF-16 code units.
const MAX_STRING_UNITS: usize = 4_096;

/// A guide's position is stored in 1/32 document pixels.
const GUIDE_UNITS_PER_PIXEL: f64 = 32.0;

/// One ruler guide.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PsdGuide {
    /// `true` for a horizontal guide (its position is a y); `false` for a
    /// vertical one (its position is an x).
    pub horizontal: bool,
    /// Document pixels, at the format's 1/32 px precision.
    pub position: f64,
}

/// The guides in a 1032 resource, in file order. Empty when there is none or
/// it is malformed; a truncated list yields the guides before the cut.
///
/// ```text
/// u32 version (1)  u32 grid-h  u32 grid-v  u32 count
/// count x { i32 position (1/32 px)  u8 direction (0 vertical, 1 horizontal) }
/// ```
pub fn guides(resources: &[ImageResource]) -> Vec<PsdGuide> {
    let Some(r) = resources.iter().find(|r| r.id == ID_GRID_GUIDES) else {
        return Vec::new();
    };
    let mut cur = Cursor::new(&r.data);
    let (Ok(_version), Ok(_gh), Ok(_gv), Ok(count)) = (cur.u32(), cur.u32(), cur.u32(), cur.u32())
    else {
        return Vec::new();
    };
    // The count is a claim; five bytes per guide is the truth.
    let count = (count as usize).min(cur.remaining() / 5).min(MAX_GUIDES);
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let (Ok(at), Ok(dir)) = (cur.i32(), cur.u8()) else {
            break;
        };
        out.push(PsdGuide {
            horizontal: dir == 1,
            position: f64::from(at) / GUIDE_UNITS_PER_PIXEL,
        });
    }
    out
}

/// A 1032 resource holding `list` (Photoshop's default quarter-inch grid).
pub fn guides_resource(list: &[PsdGuide]) -> ImageResource {
    let list = &list[..list.len().min(MAX_GUIDES)];
    let mut s = Sink::new();
    s.u32(1);
    s.u32(576);
    s.u32(576);
    s.u32(list.len() as u32);
    for g in list {
        let at = (g.position * GUIDE_UNITS_PER_PIXEL)
            .round()
            .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
        s.i32(at);
        s.u8(u8::from(g.horizontal));
    }
    ImageResource {
        id: ID_GRID_GUIDES,
        name: String::new(),
        data: s.into_inner(),
    }
}

/// One saved path (2000–2997) or the work path (1025).
#[derive(Debug, Clone, PartialEq)]
pub struct SavedPath {
    /// The resource id: 1025 for the work path.
    pub id: u16,
    /// The path's name: the resource's own name ("Work Path" for 1025, whose
    /// resource name Photoshop leaves empty).
    pub name: String,
    /// The path, in document pixels.
    pub path: VectorPath,
}

impl SavedPath {
    /// `true` for the (unsaved) work path.
    pub fn is_work_path(&self) -> bool {
        self.id == ID_WORK_PATH
    }
}

/// Every saved path, in resource order, then the work path if present.
///
/// A path resource is the same 26-byte records as a `vmsk` block without its
/// eight-byte version-and-flags prefix; [`VectorPath::decode`] reads it once
/// that prefix is supplied, with the same knot ceiling.
pub fn saved_paths(resources: &[ImageResource], width: u32, height: u32) -> Vec<SavedPath> {
    let decode = |r: &ImageResource| {
        let mut data = Vec::with_capacity(8 + r.data.len());
        data.extend_from_slice(&[0, 0, 0, 3, 0, 0, 0, 0]);
        data.extend_from_slice(&r.data);
        VectorPath::decode(&data, width, height)
    };
    let saved = resources
        .iter()
        .filter(|r| (ID_SAVED_PATH_FIRST..=ID_SAVED_PATH_LAST).contains(&r.id));
    let work = resources.iter().filter(|r| r.id == ID_WORK_PATH).take(1);
    saved
        .chain(work)
        .filter_map(|r| {
            let path = decode(r)?;
            let name = if r.id == ID_WORK_PATH && r.name.is_empty() {
                "Work Path".to_string()
            } else {
                r.name.clone()
            };
            Some(SavedPath {
                id: r.id,
                name,
                path,
            })
        })
        .collect()
}

/// A saved-path (or, with [`ID_WORK_PATH`], work-path) resource.
pub fn saved_path_resource(
    id: u16,
    name: &str,
    path: &VectorPath,
    width: u32,
    height: u32,
) -> ImageResource {
    let encoded = path.encode(width, height);
    ImageResource {
        id,
        name: name.to_string(),
        // Drop the `vmsk` version and flags: a path resource starts at its
        // first record.
        data: encoded.get(8..).unwrap_or_default().to_vec(),
    }
}

/// One slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PsdSlice {
    pub id: u32,
    pub name: String,
    /// Document pixels.
    pub bounds: Rect,
    /// 0 auto-generated, 1 layer-based, 2 user-drawn.
    pub origin: u32,
    pub url: String,
    pub alt: String,
}

/// The user and layer slices in a 1050 resource (versions 6, 7 and 8).
/// Photoshop's auto-generated fill slices (origin 0) are skipped: they are
/// the remainder of the canvas, not something the user drew. A truncated
/// list yields the slices before the cut.
pub fn slices(resources: &[ImageResource], opts: &ReadOptions) -> Vec<PsdSlice> {
    let Some(r) = resources.iter().find(|r| r.id == ID_SLICES) else {
        return Vec::new();
    };
    let mut cur = Cursor::new(&r.data);
    let mut out = match cur.u32() {
        Ok(6) => slices_v6(&mut cur),
        Ok(7 | 8) => slices_descriptor(&mut cur, opts),
        _ => Vec::new(),
    };
    out.retain(|s| s.origin != 0);
    out
}

fn slices_v6(cur: &mut Cursor<'_>) -> Vec<PsdSlice> {
    let mut out = Vec::new();
    let head = (|| -> PsdResult<u32> {
        cur.skip(16)?; // bounding rectangle
        cur.unicode_string(MAX_STRING_UNITS)?; // group name
        cur.u32()
    })();
    let Ok(count) = head else {
        return out;
    };
    // The smallest slice record is well over 40 bytes.
    let count = (count as usize).min(cur.remaining() / 40).min(MAX_SLICES);
    for _ in 0..count {
        let one = (|| -> PsdResult<PsdSlice> {
            let id = cur.u32()?;
            let _group = cur.u32()?;
            let origin = cur.u32()?;
            if origin == 1 {
                cur.u32()?; // associated layer id
            }
            let name = cur.unicode_string(MAX_STRING_UNITS)?;
            let _kind = cur.u32()?;
            let left = cur.i32()?;
            let top = cur.i32()?;
            let right = cur.i32()?;
            let bottom = cur.i32()?;
            let url = cur.unicode_string(MAX_STRING_UNITS)?;
            let _target = cur.unicode_string(MAX_STRING_UNITS)?;
            let _message = cur.unicode_string(MAX_STRING_UNITS)?;
            let alt = cur.unicode_string(MAX_STRING_UNITS)?;
            let _html = cur.u8()?;
            let _text = cur.unicode_string(MAX_STRING_UNITS)?;
            let _h = cur.u32()?;
            let _v = cur.u32()?;
            cur.skip(4)?; // ARGB
            Ok(PsdSlice {
                id,
                name,
                bounds: Rect::new(left, top, right, bottom),
                origin,
                url,
                alt,
            })
        })();
        match one {
            Ok(s) => out.push(s),
            Err(_) => break,
        }
    }
    out
}

fn slices_descriptor(cur: &mut Cursor<'_>, opts: &ReadOptions) -> Vec<PsdSlice> {
    let Ok(16) = cur.u32() else {
        return Vec::new();
    };
    let Ok(root) = Descriptor::read(cur, opts) else {
        return Vec::new();
    };
    let Some(Value::List(items)) = root.get("slices") else {
        return Vec::new();
    };
    items
        .iter()
        .take(MAX_SLICES)
        .filter_map(|v| {
            let Value::Descriptor(d) = v else {
                return None;
            };
            let b = d.descriptor("bounds")?;
            let n = |k: &str| b.number(k).map(|v| v.round() as i32);
            let origin = match d.get("origin") {
                Some(Value::Enumerated { value, .. }) if value == "autoGenerated" => 0,
                Some(Value::Enumerated { value, .. })
                    if value == "layerGenerated" || value == "layer" =>
                {
                    1
                }
                _ => 2,
            };
            Some(PsdSlice {
                id: d.number("sliceID").map_or(0, |v| v as u32),
                name: d.text("Nm  ").unwrap_or_default().to_string(),
                bounds: Rect::new(n("Left")?, n("Top ")?, n("Rght")?, n("Btom")?),
                origin,
                url: d.text("url").unwrap_or_default().to_string(),
                alt: d.text("altTag").unwrap_or_default().to_string(),
            })
        })
        .collect()
}

/// A version-6 1050 resource holding `list` as user slices, grouped under
/// `group` with the canvas as the bounding rectangle.
pub fn slices_resource(list: &[PsdSlice], group: &str, width: u32, height: u32) -> ImageResource {
    let list = &list[..list.len().min(MAX_SLICES)];
    let mut s = Sink::new();
    s.u32(6);
    s.i32(0);
    s.i32(0);
    s.i32(height as i32);
    s.i32(width as i32);
    s.unicode_string(group);
    s.u32(list.len() as u32);
    for sl in list {
        s.u32(sl.id);
        s.u32(0); // group id
                  // Written as user slices: no associated layer id follows.
        s.u32(2);
        s.unicode_string(&sl.name);
        s.u32(1); // type: image
        s.i32(sl.bounds.left);
        s.i32(sl.bounds.top);
        s.i32(sl.bounds.right);
        s.i32(sl.bounds.bottom);
        s.unicode_string(&sl.url);
        s.unicode_string(""); // target
        s.unicode_string(""); // message
        s.unicode_string(&sl.alt);
        s.u8(0); // cell text is not HTML
        s.unicode_string(""); // cell text
        s.u32(0); // horizontal alignment
        s.u32(0); // vertical alignment
        s.bytes(&[0, 0, 0, 0]); // ARGB
    }
    ImageResource {
        id: ID_SLICES,
        name: String::new(),
        data: s.into_inner(),
    }
}

/// The alpha channel names, from 1045 when present, else 1006.
pub fn alpha_names(resources: &[ImageResource]) -> Vec<String> {
    if let Some(r) = resources.iter().find(|r| r.id == ID_UNICODE_ALPHA_NAMES) {
        let mut cur = Cursor::new(&r.data);
        let mut out = Vec::new();
        while cur.remaining() >= 4 && out.len() < MAX_ALPHA_CHANNELS {
            match cur.unicode_string(MAX_STRING_UNITS) {
                Ok(name) => out.push(name),
                Err(_) => break,
            }
        }
        return out;
    }
    let Some(r) = resources.iter().find(|r| r.id == ID_ALPHA_NAMES) else {
        return Vec::new();
    };
    let mut cur = Cursor::new(&r.data);
    let mut out = Vec::new();
    while cur.remaining() >= 1 && out.len() < MAX_ALPHA_CHANNELS {
        match cur.pascal_string(1) {
            Ok(name) => out.push(name),
            Err(_) => break,
        }
    }
    out
}

/// One alpha channel of the merged image, at 8 bits per sample.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlphaChannel {
    pub name: String,
    /// `width * height` samples, row-major; 255 is selected.
    pub coverage: Vec<u8>,
}

/// The merged image's named alpha channels.
///
/// The names (1045 / 1006) say how many there are: they are the LAST that
/// many channels past the colour channels, because a transparency channel,
/// when the document has one, comes first and is not named. A deeper sample
/// is reduced to 8 bits. A channel whose plane is the wrong length is
/// skipped rather than trusted.
pub fn alpha_channels(file: &PsdFile) -> Vec<AlphaChannel> {
    let Some(merged) = &file.merged else {
        return Vec::new();
    };
    let names = alpha_names(&file.resources);
    let colour = usize::from(file.header.color_mode.color_channels());
    let extra = merged.channels.len().saturating_sub(colour);
    let named = names.len().min(extra);
    if named == 0 {
        return Vec::new();
    }
    let first = merged.channels.len() - named;
    let pixels = file.header.canvas_pixels() as usize;
    let bps = file.header.depth.bytes_per_sample();
    names
        .into_iter()
        .zip(&merged.channels[first..])
        .filter_map(|(name, plane)| {
            if plane.len() != pixels.checked_mul(bps)? {
                return None;
            }
            let coverage = match file.header.depth {
                Depth::Eight => plane.clone(),
                Depth::Sixteen => plane.as_chunks::<2>().0.iter().map(|c| c[0]).collect(),
                Depth::ThirtyTwo => plane
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|c| {
                        let v = f32::from_be_bytes(*c);
                        (v.clamp(0.0, 1.0) * 255.0).round() as u8
                    })
                    .collect(),
            };
            Some(AlphaChannel { name, coverage })
        })
        .collect()
}

/// How many alpha channels [`set_alpha_channels`] can still add to `file`: the
/// format's 56-channel ceiling less the channels the header already counts,
/// and never more than [`MAX_ALPHA_CHANNELS`]. A caller with more writes the
/// first this many and names the rest, rather than failing the whole save.
pub fn alpha_channel_room(file: &PsdFile) -> usize {
    56usize
        .saturating_sub(usize::from(file.header.channels))
        .min(MAX_ALPHA_CHANNELS)
}

/// Append `channels` to the merged image as named alpha channels, widening
/// each 8-bit sample to the file's depth, raising the header's channel count
/// and replacing the 1006 / 1045 name resources.
///
/// Refused when the file has no merged image to extend, when a channel's
/// coverage is not `width * height` samples, or when the total would pass the
/// format's 56-channel ceiling.
pub fn set_alpha_channels(file: &mut PsdFile, channels: &[AlphaChannel]) -> PsdResult<()> {
    if channels.is_empty() {
        return Ok(());
    }
    let pixels = file.header.canvas_pixels() as usize;
    let total = usize::from(file.header.channels) + channels.len();
    if total > 56 || channels.len() > MAX_ALPHA_CHANNELS {
        return Err(PsdError::InvalidDocument(format!(
            "{} alpha channels would give the document {total} channels; a .psd holds 56",
            channels.len()
        )));
    }
    let depth = file.header.depth;
    let Some(merged) = file.merged.as_mut() else {
        return Err(PsdError::InvalidDocument(
            "alpha channels need a merged image to extend".to_string(),
        ));
    };
    for ch in channels {
        if ch.coverage.len() != pixels {
            return Err(PsdError::ChannelSizeMismatch {
                what: "alpha channel",
                expected: pixels,
                actual: ch.coverage.len(),
            });
        }
    }
    for ch in channels {
        let plane = match depth {
            Depth::Eight => ch.coverage.clone(),
            Depth::Sixteen => ch
                .coverage
                .iter()
                .flat_map(|v| (u16::from(*v) * 257).to_be_bytes())
                .collect(),
            Depth::ThirtyTwo => ch
                .coverage
                .iter()
                .flat_map(|v| (f32::from(*v) / 255.0).to_be_bytes())
                .collect(),
        };
        merged.channels.push(plane);
    }
    file.header.channels = total as u16;
    file.resources
        .retain(|r| r.id != ID_ALPHA_NAMES && r.id != ID_UNICODE_ALPHA_NAMES);
    let mut pascal = Sink::new();
    let mut unicode = Sink::new();
    for ch in channels {
        pascal.pascal_string(&ch.name, 1);
        unicode.unicode_string(&ch.name);
    }
    file.resources.push(ImageResource {
        id: ID_ALPHA_NAMES,
        name: String::new(),
        data: pascal.into_inner(),
    });
    file.resources.push(ImageResource {
        id: ID_UNICODE_ALPHA_NAMES,
        name: String::new(),
        data: unicode.into_inner(),
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::PsdError;

    fn round_trip(resources: &[ImageResource]) -> Vec<ImageResource> {
        let mut sink = Sink::new();
        write_resources(resources, &mut sink);
        let buf = sink.into_inner();
        assert_eq!(buf.len() % 2, 0, "the section must end on an even boundary");
        let mut warnings = Vec::new();
        let got = read_resources(
            &mut Cursor::new(&buf),
            &ReadOptions::default(),
            &mut warnings,
        )
        .unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        got
    }

    #[test]
    fn odd_sized_data_and_odd_length_names_stay_in_step() {
        let resources = vec![
            ImageResource {
                id: 1005,
                name: String::new(),
                data: vec![1, 2, 3], // odd
            },
            ImageResource {
                id: 1006,
                name: "abc".into(), // 1 + 3 = 4, already even
                data: vec![9],      // odd
            },
            ImageResource {
                id: 1007,
                name: "ab".into(), // 1 + 2 = 3, needs a pad byte
                data: vec![4, 5],  // even
            },
            ImageResource {
                id: 1008,
                name: "z".into(),
                data: Vec::new(),
            },
        ];
        assert_eq!(round_trip(&resources), resources);
    }

    #[test]
    fn an_empty_section_reads_as_no_resources() {
        assert_eq!(round_trip(&[]), Vec::new());
    }

    #[test]
    fn resolution_info_round_trips_through_sixteen_sixteen_fixed_point() {
        let r = resolution_info(72.0);
        assert_eq!(r.id, ID_RESOLUTION_INFO);
        assert_eq!(r.data.len(), 16);
        assert_eq!(resolution_dpi(&r), Some(72.0));
        assert_eq!(resolution_dpi(&resolution_info(300.0)), Some(300.0));
        // A resource that is not 1005 is not misread as a resolution.
        let other = ImageResource {
            id: 1006,
            name: String::new(),
            data: r.data.clone(),
        };
        assert_eq!(resolution_dpi(&other), None);
    }

    #[test]
    fn an_absurd_declared_size_is_refused_before_it_allocates() {
        let mut s = Sink::new();
        s.tag(&RESOURCE_SIGNATURE);
        s.u16(1005);
        s.pascal_string("", 2);
        s.u32(u32::MAX); // four gigabytes, in a sixteen byte file
        let buf = s.into_inner();
        let mut warnings = Vec::new();
        let err = read_resources(
            &mut Cursor::new(&buf),
            &ReadOptions::default(),
            &mut warnings,
        )
        .unwrap_err();
        assert!(matches!(err, PsdError::LimitExceeded { .. }), "{err}");
    }

    #[test]
    fn a_size_within_the_limit_but_past_the_section_is_a_truncation() {
        let mut s = Sink::new();
        s.tag(&RESOURCE_SIGNATURE);
        s.u16(1005);
        s.pascal_string("", 2);
        s.u32(1000);
        let buf = s.into_inner();
        let mut warnings = Vec::new();
        assert!(matches!(
            read_resources(
                &mut Cursor::new(&buf),
                &ReadOptions::default(),
                &mut warnings
            )
            .unwrap_err(),
            PsdError::Truncated { .. }
        ));
    }

    #[test]
    fn a_bad_signature_stops_the_scan_with_a_warning_rather_than_a_failure() {
        let mut s = Sink::new();
        write_resources(
            &[ImageResource {
                id: 1005,
                name: String::new(),
                data: vec![1, 2],
            }],
            &mut s,
        );
        s.tag(b"junk");
        s.zeros(16);
        let buf = s.into_inner();
        let mut warnings = Vec::new();
        let got = read_resources(
            &mut Cursor::new(&buf),
            &ReadOptions::default(),
            &mut warnings,
        )
        .unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("junk"), "{warnings:?}");
    }

    #[test]
    fn truncating_the_section_anywhere_never_panics() {
        let mut sink = Sink::new();
        write_resources(
            &[
                ImageResource {
                    id: 1005,
                    name: "n".into(),
                    data: vec![1, 2, 3],
                },
                ImageResource {
                    id: 1039,
                    name: String::new(),
                    data: vec![7; 33],
                },
            ],
            &mut sink,
        );
        let buf = sink.into_inner();
        for cut in 0..buf.len() {
            let mut warnings = Vec::new();
            let _ = read_resources(
                &mut Cursor::new(&buf[..cut]),
                &ReadOptions::default(),
                &mut warnings,
            );
        }
    }

    // ------------------------------------------------------------- W11-C

    use crate::shape::{Knot, SubPath};

    fn triangle() -> VectorPath {
        let k = |x: f64, y: f64| Knot {
            before: [x, y],
            anchor: [x, y],
            after: [x, y],
            linked: false,
        };
        VectorPath {
            subpaths: vec![SubPath {
                closed: true,
                operation: 1,
                knots: vec![k(4.0, 4.0), k(60.0, 8.0), k(20.0, 40.0)],
            }],
            ..VectorPath::default()
        }
    }

    fn close(a: &VectorPath, b: &VectorPath) -> bool {
        a.subpaths.len() == b.subpaths.len()
            && a.subpaths.iter().zip(&b.subpaths).all(|(x, y)| {
                x.closed == y.closed
                    && x.knots.len() == y.knots.len()
                    && x.knots.iter().zip(&y.knots).all(|(p, q)| {
                        (p.anchor[0] - q.anchor[0]).abs() < 1e-3
                            && (p.anchor[1] - q.anchor[1]).abs() < 1e-3
                    })
            })
    }

    #[test]
    fn guides_round_trip_through_the_1032_resource_at_a_32nd_of_a_pixel() {
        let list = vec![
            PsdGuide {
                horizontal: false,
                position: 12.0,
            },
            PsdGuide {
                horizontal: true,
                position: 30.5,
            },
            PsdGuide {
                horizontal: true,
                position: 0.03125,
            },
        ];
        let got = round_trip(&[guides_resource(&list)]);
        assert_eq!(guides(&got), list);
        assert!(guides(&[]).is_empty());
    }

    #[test]
    fn a_guide_count_past_the_bytes_present_yields_only_the_guides_there() {
        let mut r = guides_resource(&[PsdGuide {
            horizontal: true,
            position: 3.0,
        }]);
        // Claim four billion guides over five bytes of them.
        r.data[12..16].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(guides(&[r.clone()]).len(), 1);
        for cut in 0..r.data.len() {
            let mut t = r.clone();
            t.data.truncate(cut);
            assert!(guides(&[t]).len() <= 1);
        }
    }

    #[test]
    fn saved_paths_and_the_work_path_round_trip_by_id_and_name() {
        let saved = saved_path_resource(2000, "Outline", &triangle(), 64, 48);
        let work = saved_path_resource(ID_WORK_PATH, "", &triangle(), 64, 48);
        // A path resource carries records only, never the vmsk prefix.
        assert_eq!(saved.data.len() % 26, 0);
        let got = saved_paths(&round_trip(&[work, saved]), 64, 48);
        assert_eq!(got.len(), 2);
        assert_eq!((got[0].id, got[0].name.as_str()), (2000, "Outline"));
        assert!(close(&got[0].path, &triangle()), "{:?}", got[0].path);
        assert!(got[1].is_work_path());
        assert_eq!(got[1].name, "Work Path");
        assert!(close(&got[1].path, &triangle()));
    }

    #[test]
    fn slices_round_trip_through_a_version_6_resource() {
        let list = vec![
            PsdSlice {
                id: 1,
                name: "hero".into(),
                bounds: Rect::new(0, 0, 32, 16),
                origin: 2,
                url: "https://example.test/".into(),
                alt: "Hero".into(),
            },
            PsdSlice {
                id: 2,
                name: "footer".into(),
                bounds: Rect::new(4, 20, 60, 44),
                origin: 2,
                url: String::new(),
                alt: String::new(),
            },
        ];
        let got = round_trip(&[slices_resource(&list, "doc", 64, 48)]);
        assert_eq!(slices(&got, &ReadOptions::default()), list);
    }

    #[test]
    fn a_truncated_or_lying_slice_resource_never_panics() {
        let one = PsdSlice {
            id: 1,
            name: "s".into(),
            bounds: Rect::new(0, 0, 8, 8),
            origin: 2,
            url: String::new(),
            alt: String::new(),
        };
        let r = slices_resource(&[one.clone(), one], "g", 8, 8);
        for cut in 0..r.data.len() {
            let mut t = r.clone();
            t.data.truncate(cut);
            assert!(slices(&[t], &ReadOptions::default()).len() <= 2);
        }
    }

    #[test]
    fn alpha_channels_ride_the_merged_image_under_their_names_through_a_file() {
        use crate::header::PsdHeader;
        use crate::model::MergedImage;
        let (w, h) = (4u32, 2u32);
        let mut file = PsdFile::new(PsdHeader::rgba8(w, h));
        file.merged = Some(MergedImage::from_rgba8(w, h, &[9u8; 32]).unwrap());
        let a = AlphaChannel {
            name: "Alpha 1".into(),
            coverage: vec![0, 255, 128, 0, 0, 0, 255, 255],
        };
        let b = AlphaChannel {
            name: "Kanal é".into(),
            coverage: vec![7; 8],
        };
        set_alpha_channels(&mut file, &[a.clone(), b.clone()]).unwrap();
        assert_eq!(file.header.channels, 6);
        let back = crate::read(&crate::write(&file).unwrap()).unwrap();
        assert_eq!(alpha_channels(&back), vec![a.clone(), b]);
        // Transparency is untouched: channel 3 is still the composite alpha.
        assert_eq!(back.merged.as_ref().unwrap().channels[3], vec![9u8; 8]);

        // A 16-bit file widens and narrows the samples exactly.
        let mut deep = PsdFile::new(PsdHeader {
            depth: Depth::Sixteen,
            ..PsdHeader::rgba8(w, h)
        });
        deep.merged = Some(MergedImage {
            channels: vec![vec![0u8; 16]; 4],
        });
        set_alpha_channels(&mut deep, std::slice::from_ref(&a)).unwrap();
        let back = crate::read(&crate::write(&deep).unwrap()).unwrap();
        assert_eq!(alpha_channels(&back), vec![a]);
    }

    #[test]
    fn a_wrong_sized_alpha_channel_is_refused_and_extra_channels_without_names_are_not_alphas() {
        use crate::header::PsdHeader;
        use crate::model::MergedImage;
        let mut file = PsdFile::new(PsdHeader::rgba8(2, 2));
        file.merged = Some(MergedImage::from_rgba8(2, 2, &[0u8; 16]).unwrap());
        let bad = AlphaChannel {
            name: "x".into(),
            coverage: vec![0; 3],
        };
        assert!(set_alpha_channels(&mut file, &[bad]).is_err());
        assert_eq!(file.header.channels, 4, "a refusal changes nothing");
        // Channel 3 is transparency, not a named alpha channel.
        assert!(alpha_channels(&file).is_empty());
    }

    /// W11-C round 3: the room left for alpha channels is the 56-channel
    /// ceiling less the header's channels, capped at MAX_ALPHA_CHANNELS, and
    /// exactly that many are accepted.
    #[test]
    fn alpha_channel_room_is_what_set_alpha_channels_accepts() {
        use crate::header::PsdHeader;
        use crate::model::MergedImage;
        let mut file = PsdFile::new(PsdHeader::rgba8(2, 2));
        file.merged = Some(MergedImage::from_rgba8(2, 2, &[0u8; 16]).unwrap());
        assert_eq!(alpha_channel_room(&file), 52);
        let one = |i: usize| AlphaChannel {
            name: format!("a{i}"),
            coverage: vec![0; 4],
        };
        let fit: Vec<AlphaChannel> = (0..52).map(one).collect();
        let mut over = fit.clone();
        over.push(one(52));
        assert!(set_alpha_channels(&mut file.clone(), &over).is_err());
        set_alpha_channels(&mut file, &fit).unwrap();
        assert_eq!(file.header.channels, 56);
        assert_eq!(alpha_channel_room(&file), 0);
    }

    /// W11-C round 3: a version-7 1050 resource (a descriptor) yields its
    /// user and layer slices, reading Photoshop's `layerGenerated` origin as
    /// a layer slice and skipping `autoGenerated` ones.
    #[test]
    fn a_descriptor_1050_resource_yields_its_user_and_layer_slices() {
        let slice = |id: i32, name: &str, origin: &str, top: i32| {
            let mut b = Descriptor::new("Rct1");
            b.push("Top ", Value::Integer(top)).unwrap();
            b.push("Left", Value::Integer(0)).unwrap();
            b.push("Btom", Value::Integer(top + 10)).unwrap();
            b.push("Rght", Value::Integer(20)).unwrap();
            let mut d = Descriptor::new("slice");
            d.push("sliceID", Value::Integer(id)).unwrap();
            d.push("Nm  ", name.into()).unwrap();
            d.push(
                "origin",
                Value::Enumerated {
                    type_id: "ESliceOrigin".into(),
                    value: origin.into(),
                },
            )
            .unwrap();
            d.push("bounds", Value::Descriptor(b)).unwrap();
            d.push("url", "https://example.com".into()).unwrap();
            d.push("altTag", "alt".into()).unwrap();
            Value::Descriptor(d)
        };
        let mut root = Descriptor::new("null");
        root.push(
            "slices",
            Value::List(vec![
                slice(1, "auto", "autoGenerated", 0),
                slice(2, "drawn", "userGenerated", 10),
                slice(3, "from layer", "layerGenerated", 20),
            ]),
        )
        .unwrap();
        let mut sink = Sink::new();
        sink.u32(7);
        sink.u32(16);
        root.write(&mut sink).unwrap();
        let res = ImageResource {
            id: ID_SLICES,
            name: String::new(),
            data: sink.into_inner(),
        };
        let got = slices(&[res], &ReadOptions::default());
        let brief: Vec<(u32, &str, u32, Rect, &str, &str)> = got
            .iter()
            .map(|s| {
                (
                    s.id,
                    s.name.as_str(),
                    s.origin,
                    s.bounds,
                    s.url.as_str(),
                    s.alt.as_str(),
                )
            })
            .collect();
        assert_eq!(
            brief,
            vec![
                (
                    2,
                    "drawn",
                    2,
                    Rect::new(0, 10, 20, 20),
                    "https://example.com",
                    "alt"
                ),
                (
                    3,
                    "from layer",
                    1,
                    Rect::new(0, 20, 20, 30),
                    "https://example.com",
                    "alt"
                ),
            ]
        );
    }
}
