//! W16-L: a small ZIP reader for the zipped containers (`.cdr` X4+, a
//! zipped `.pxd` package): the end record, the central directory, one local
//! header per entry read; stored or deflated entries. ZIP64 and encrypted
//! entries are refused by name.
//!
//! The central directory is walked once, at most once per declared entry
//! and never past the file; a deflated entry inflates through a reader
//! capped at the caller's `cap`, whatever size the directory declares.

use std::io::Read;

use super::super::malformed;
use crate::codec::CodecError;

const LOCAL: &[u8] = b"PK\x03\x04";
const CENTRAL: &[u8] = b"PK\x01\x02";
const END: &[u8] = b"PK\x05\x06";

/// One central-directory record.
#[derive(Debug, Clone)]
pub struct Entry {
    /// The entry's name, as stored (lossy UTF-8).
    pub name: String,
    method: usize,
    flags: usize,
    packed: usize,
    local: usize,
}

/// `true` when `head` starts like a ZIP archive.
pub fn looks_like_zip(head: &[u8]) -> bool {
    head.starts_with(LOCAL)
}

fn le16(b: &[u8], at: usize, name: &str) -> Result<usize, CodecError> {
    b.get(at..at.saturating_add(2))
        .filter(|s| s.len() == 2)
        .map(|s| usize::from(u16::from_le_bytes([s[0], s[1]])))
        .ok_or_else(|| malformed(name, "the archive ends inside a record"))
}

fn le32(b: &[u8], at: usize, name: &str) -> Result<usize, CodecError> {
    b.get(at..at.saturating_add(4))
        .filter(|s| s.len() == 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]) as usize)
        .ok_or_else(|| malformed(name, "the archive ends inside a record"))
}

/// Every entry in the central directory of `zip`. `name` is the format
/// named in errors.
pub fn entries(zip: &[u8], name: &str) -> Result<Vec<Entry>, CodecError> {
    if zip.len() < 22 {
        return Err(malformed(name, "too short to be a ZIP archive"));
    }
    let floor = zip.len().saturating_sub(22 + 0xFFFF);
    let end = (floor..=zip.len() - 22)
        .rev()
        .find(|&i| &zip[i..i + 4] == END)
        .ok_or_else(|| malformed(name, "no ZIP end-of-directory record"))?;
    let count = le16(zip, end + 10, name)?;
    let dir_at = le32(zip, end + 16, name)?;
    if count == 0xFFFF || dir_at == 0xFFFF_FFFF {
        return Err(CodecError::Unsupported(format!(
            "ZIP64 {name} archives are not supported"
        )));
    }
    let mut out = Vec::new();
    let mut at = dir_at;
    for _ in 0..count {
        if zip.get(at..at.saturating_add(4)) != Some(CENTRAL) {
            return Err(malformed(name, "a central directory record is damaged"));
        }
        let flags = le16(zip, at + 8, name)?;
        let method = le16(zip, at + 10, name)?;
        let packed = le32(zip, at + 20, name)?;
        let name_len = le16(zip, at + 28, name)?;
        let extra_len = le16(zip, at + 30, name)?;
        let comment_len = le16(zip, at + 32, name)?;
        let local = le32(zip, at + 42, name)?;
        let entry_name = zip
            .get(at + 46..at + 46 + name_len)
            .ok_or_else(|| malformed(name, "an entry name runs past the file"))?;
        out.push(Entry {
            name: String::from_utf8_lossy(entry_name).into_owned(),
            method,
            flags,
            packed,
            local,
        });
        at += 46 + name_len + extra_len + comment_len;
    }
    Ok(out)
}

/// The bytes of `entry`, inflated, at most `cap` of them.
pub fn read(zip: &[u8], entry: &Entry, cap: u64, name: &str) -> Result<Vec<u8>, CodecError> {
    if entry.flags & 1 != 0 {
        return Err(CodecError::Unsupported(format!(
            "encrypted {name} archives are not supported"
        )));
    }
    let local = entry.local;
    if zip.get(local..local.saturating_add(4)) != Some(LOCAL) {
        return Err(malformed(name, "a local header is damaged"));
    }
    let data_at = local + 30 + le16(zip, local + 26, name)? + le16(zip, local + 28, name)?;
    let data = zip
        .get(data_at..data_at.saturating_add(entry.packed))
        .filter(|d| d.len() == entry.packed)
        .ok_or_else(|| malformed(name, format!("{} runs past the file", entry.name)))?;
    match entry.method {
        0 => {
            if data.len() as u64 > cap {
                return Err(CodecError::LimitExceeded(format!(
                    "{} is larger than {cap} bytes",
                    entry.name
                )));
            }
            Ok(data.to_vec())
        }
        8 => {
            let mut out = Vec::new();
            flate2::read::DeflateDecoder::new(data)
                .take(cap.saturating_add(1))
                .read_to_end(&mut out)
                .map_err(|e| malformed(name, format!("{} does not inflate: {e}", entry.name)))?;
            if out.len() as u64 > cap {
                return Err(CodecError::LimitExceeded(format!(
                    "{} inflates past {cap} bytes",
                    entry.name
                )));
            }
            Ok(out)
        }
        other => Err(CodecError::Unsupported(format!(
            "ZIP compression method {other} in a {name} archive is not supported"
        ))),
    }
}

/// The first entry whose name matches one of `wanted` (case-insensitively,
/// in the order `wanted` lists them), read.
pub fn first_of(
    zip: &[u8],
    wanted: &[&str],
    cap: u64,
    name: &str,
) -> Result<Option<(String, Vec<u8>)>, CodecError> {
    let all = entries(zip, name)?;
    for w in wanted {
        if let Some(e) = all.iter().find(|e| e.name.eq_ignore_ascii_case(w)) {
            return Ok(Some((e.name.clone(), read(zip, e, cap, name)?)));
        }
    }
    Ok(None)
}
