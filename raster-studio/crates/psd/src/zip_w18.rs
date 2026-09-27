//! W18-G: Photopea's Save PSD/PSB "Put the file into ZIP" — one file inside
//! a ZIP archive.
//!
//! [`wrap`] writes a single-entry archive: a local file header, the file
//! DEFLATE-compressed (method 8; stored, method 0, when DEFLATE would not
//! make it smaller), one central directory record and the end record, the
//! name flagged UTF-8 (bit 11). ZIP64 is not written, so a file or archive
//! of 4 GiB or more is refused. [`unwrap_single`] reads such an archive
//! back — the first entry's name and bytes, CRC-checked — and is what the
//! tests (and a reader of this build's own archives) use.

use std::io::{Read, Write};

const LOCAL: u32 = 0x0403_4b50;
const CENTRAL: u32 = 0x0201_4b50;
const END: u32 = 0x0605_4b50;
/// 1980-01-01, the earliest date a ZIP header can say.
const DOS_DATE: u16 = (1 << 5) | 1;
const UTF8_NAME: u16 = 1 << 11;

fn crc32(data: &[u8]) -> u32 {
    let mut crc = flate2::Crc::new();
    crc.update(data);
    crc.sum()
}

/// `data` as the one file, called `name`, of a ZIP archive.
pub fn wrap(name: &str, data: &[u8]) -> Result<Vec<u8>, String> {
    let limit = u32::MAX as usize;
    if data.len() >= limit || name.len() > u16::MAX as usize {
        return Err(format!(
            "{name} is {} bytes: a ZIP archive without ZIP64 holds less than 4 GiB",
            data.len()
        ));
    }
    let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(data).map_err(|e| e.to_string())?;
    let deflated = enc.finish().map_err(|e| e.to_string())?;
    let (method, body): (u16, &[u8]) = if deflated.len() < data.len() {
        (8, &deflated)
    } else {
        (0, data)
    };
    let crc = crc32(data);
    let mut out = Vec::with_capacity(body.len() + 2 * name.len() + 128);
    let put16 = |out: &mut Vec<u8>, v: u16| out.extend_from_slice(&v.to_le_bytes());
    let put32 = |out: &mut Vec<u8>, v: u32| out.extend_from_slice(&v.to_le_bytes());
    // Local file header.
    put32(&mut out, LOCAL);
    put16(&mut out, 20);
    put16(&mut out, UTF8_NAME);
    put16(&mut out, method);
    put16(&mut out, 0);
    put16(&mut out, DOS_DATE);
    put32(&mut out, crc);
    put32(&mut out, body.len() as u32);
    put32(&mut out, data.len() as u32);
    put16(&mut out, name.len() as u16);
    put16(&mut out, 0);
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(body);
    let central_at = out.len();
    // Central directory.
    put32(&mut out, CENTRAL);
    put16(&mut out, 20);
    put16(&mut out, 20);
    put16(&mut out, UTF8_NAME);
    put16(&mut out, method);
    put16(&mut out, 0);
    put16(&mut out, DOS_DATE);
    put32(&mut out, crc);
    put32(&mut out, body.len() as u32);
    put32(&mut out, data.len() as u32);
    put16(&mut out, name.len() as u16);
    put16(&mut out, 0);
    put16(&mut out, 0);
    put16(&mut out, 0);
    put16(&mut out, 0);
    put32(&mut out, 0);
    put32(&mut out, 0);
    out.extend_from_slice(name.as_bytes());
    let central_len = out.len() - central_at;
    if out.len() >= limit {
        return Err("the ZIP archive would reach 4 GiB, which needs ZIP64".to_string());
    }
    // End of central directory.
    put32(&mut out, END);
    put16(&mut out, 0);
    put16(&mut out, 0);
    put16(&mut out, 1);
    put16(&mut out, 1);
    put32(&mut out, central_len as u32);
    put32(&mut out, central_at as u32);
    put16(&mut out, 0);
    Ok(out)
}

/// The first file of a ZIP archive [`wrap`] wrote: its name and bytes,
/// inflated and CRC-checked. `max_bytes` bounds the inflated size.
pub fn unwrap_single(zip: &[u8], max_bytes: usize) -> Result<(String, Vec<u8>), String> {
    let at = |i: usize, n: usize| {
        zip.get(i..i + n)
            .ok_or_else(|| "the ZIP archive is cut short".to_string())
    };
    let u16_at = |i: usize| at(i, 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u32_at = |i: usize| at(i, 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
    if u32_at(0)? != LOCAL {
        return Err("not a ZIP archive".to_string());
    }
    let method = u16_at(8)?;
    let crc = u32_at(14)?;
    let packed = u32_at(18)? as usize;
    let size = u32_at(22)? as usize;
    let name_len = u16_at(26)? as usize;
    let extra_len = u16_at(28)? as usize;
    if size > max_bytes {
        return Err(format!(
            "the archived file is {size} bytes, over the {max_bytes} allowed"
        ));
    }
    let name = String::from_utf8_lossy(at(30, name_len)?).into_owned();
    let body = at(30 + name_len + extra_len, packed)?;
    let data = match method {
        0 => body.to_vec(),
        8 => {
            let mut out = Vec::with_capacity(size);
            flate2::read::DeflateDecoder::new(body)
                .take(size as u64 + 1)
                .read_to_end(&mut out)
                .map_err(|e| format!("the archived file does not inflate: {e}"))?;
            out
        }
        other => return Err(format!("ZIP method {other} is not read here")),
    };
    if data.len() != size || crc32(&data) != crc {
        return Err("the archived file fails its size or CRC check".to_string());
    }
    Ok((name, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wrapped_file_reads_back_by_name_and_is_smaller_when_it_compresses() {
        let data: Vec<u8> = (0..20_000u32).map(|i| (i % 7) as u8).collect();
        let zip = wrap("art.psd", &data).unwrap();
        assert!(zip.starts_with(b"PK\x03\x04"));
        assert!(zip.len() < data.len() / 4, "DEFLATE made it smaller");
        assert_eq!(
            unwrap_single(&zip, usize::MAX).unwrap(),
            ("art.psd".to_string(), data)
        );
        // The end record points at the one central record.
        let end = zip.len() - 22;
        assert_eq!(&zip[end..end + 4], b"PK\x05\x06");
        let central = u32::from_le_bytes(zip[end + 16..end + 20].try_into().unwrap()) as usize;
        assert_eq!(&zip[central..central + 4], b"PK\x01\x02");
    }

    #[test]
    fn incompressible_bytes_are_stored_and_a_corrupt_body_is_refused() {
        let data: Vec<u8> = (0..64u32)
            .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
            .collect();
        let mut zip = wrap("x.bin", &data).unwrap();
        assert_eq!(u16::from_le_bytes([zip[8], zip[9]]), 0, "stored");
        assert_eq!(unwrap_single(&zip, 1 << 20).unwrap().1, data);
        let body = 30 + "x.bin".len();
        zip[body] ^= 0xFF;
        assert!(unwrap_single(&zip, 1 << 20).is_err(), "the CRC catches it");
        assert!(unwrap_single(b"not a zip", 10).is_err());
    }
}
