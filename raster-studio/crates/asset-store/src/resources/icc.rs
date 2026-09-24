//! ICC colour profiles (`.icc`, `.icm`).
//!
//! Checked, not interpreted: the 128-byte header must declare the file's own
//! size (or less), carry the `acsp` signature, and its tag table must fit
//! inside the profile. The data colour space and device class are read from
//! the header and the description from the `desc` tag — a v2
//! `textDescriptionType` (`desc`, ASCII) or a v4 `multiLocalizedUnicodeType`
//! (`mluc`, the first record's UTF-16). No colour transform is built from it;
//! assigning a profile re-tags a document, which the export path writes back
//! into files that carry one.

use psd::bytes::Cursor;

use super::{check_count, IccResource, ResourceError};

/// Largest profile accepted: 16 MiB, the codec's own ceiling for an embedded
/// profile.
pub const MAX_ICC_BYTES: usize = 16 << 20;

/// Most tags a profile's table may declare.
const MAX_TAGS: usize = 1_024;

/// Longest description kept, in characters.
const MAX_DESCRIPTION: usize = 1_024;

/// Parse and check an ICC profile.
pub fn parse(bytes: &[u8]) -> Result<IccResource, ResourceError> {
    if bytes.len() < 132 {
        return Err(ResourceError::BadSignature {
            what: "ICC profile",
        });
    }
    if bytes.len() > MAX_ICC_BYTES {
        return Err(ResourceError::LimitExceeded {
            what: "ICC profile size",
            value: bytes.len() as u64,
            max: MAX_ICC_BYTES as u64,
        });
    }
    if &bytes[36..40] != b"acsp" {
        return Err(ResourceError::BadSignature {
            what: "ICC profile",
        });
    }
    let mut header = Cursor::new(bytes);
    let declared = header.u32()? as usize;
    if declared < 132 || declared > bytes.len() {
        return Err(ResourceError::Malformed(format!(
            "the profile declares {declared} bytes but the file has {}",
            bytes.len()
        )));
    }
    let profile = &bytes[..declared];
    let sig = |at: usize| -> [u8; 4] {
        [
            profile[at],
            profile[at + 1],
            profile[at + 2],
            profile[at + 3],
        ]
    };
    let device_class = sig(12);
    let data_space = sig(16);

    let mut table = Cursor::new(&profile[128..]);
    let count = table.u32()? as usize;
    check_count("ICC tag count", count, MAX_TAGS)?;
    let mut description = None;
    for _ in 0..count {
        let tag = table.tag()?;
        let offset = table.u32()? as usize;
        let size = table.u32()? as usize;
        let end = offset
            .checked_add(size)
            .filter(|end| *end <= profile.len())
            .ok_or_else(|| {
                ResourceError::Malformed(format!(
                    "tag {} runs past the end of the profile",
                    String::from_utf8_lossy(&tag)
                ))
            })?;
        if &tag == b"desc" && description.is_none() {
            description = read_description(&profile[offset..end]);
        }
    }
    Ok(IccResource {
        bytes: profile.to_vec(),
        data_space,
        device_class,
        description,
    })
}

/// A `desc` tag's text, when it is one of the two types that carry text.
fn read_description(tag: &[u8]) -> Option<String> {
    let mut cur = Cursor::new(tag);
    let kind = cur.tag().ok()?;
    cur.skip(4).ok()?; // reserved
    let text = match &kind {
        b"desc" => {
            let len = cur.u32().ok()? as usize;
            let raw = cur.take(len.min(cur.remaining())).ok()?;
            let raw = raw.split(|b| *b == 0).next().unwrap_or(raw);
            String::from_utf8_lossy(raw).into_owned()
        }
        b"mluc" => {
            let records = cur.u32().ok()?;
            let record_size = cur.u32().ok()? as usize;
            if records == 0 || record_size < 12 {
                return None;
            }
            cur.skip(4).ok()?; // language + country
            let len = cur.u32().ok()? as usize;
            let offset = cur.u32().ok()? as usize;
            let raw = tag.get(offset..offset.checked_add(len)?)?;
            let units: Vec<u16> = raw
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_be_bytes(*c))
                .collect();
            String::from_utf16_lossy(&units)
        }
        _ => return None,
    };
    let text: String = text
        .trim_matches(char::from(0))
        .trim()
        .chars()
        .take(MAX_DESCRIPTION)
        .collect();
    (!text.is_empty()).then_some(text)
}
