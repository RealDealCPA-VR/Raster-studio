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
//! Resources are preserved verbatim. This crate interprets exactly one of them
//! — 1005, `ResolutionInfo` — because a document with no resolution resource
//! opens in Photoshop at 1 dpi.

use crate::bytes::{Cursor, Sink};
use crate::error::PsdResult;
use crate::limits::{check_limit, ReadOptions};
use crate::model::ImageResource;

/// `8BIM`, the signature every resource block starts with.
pub const RESOURCE_SIGNATURE: [u8; 4] = *b"8BIM";

/// Resource id 1005: pixels per inch for both axes, plus display units.
pub const ID_RESOLUTION_INFO: u16 = 1005;

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
}
