//! Hex encoding for the 32-byte content hashes that name blobs on disk.
//!
//! Blob filenames are the only place an on-disk name is derived from data, so
//! the decoder is strict: exactly 64 lowercase-or-uppercase hex digits, nothing
//! else. Anything a package hands us that is not that shape never becomes a
//! path component.

const HEX: [u8; 16] = *b"0123456789abcdef";

/// Lowercase hex of a 32-byte hash. Always 64 characters.
pub(crate) fn to_hex(bytes: &[u8; 32]) -> String {
    let mut out = [0u8; 64];
    for (i, b) in bytes.iter().enumerate() {
        out[i * 2] = HEX[(b >> 4) as usize];
        out[i * 2 + 1] = HEX[(b & 0x0f) as usize];
    }
    String::from_utf8(out.to_vec()).expect("hex digits are ASCII")
}

/// Parse exactly 64 hex digits. `None` for any other length or character —
/// including the separators (`/`, `\`, `.`) that would matter if the value
/// were ever pasted into a path.
pub(crate) fn from_hex(s: &str) -> Option<[u8; 32]> {
    let b = s.as_bytes();
    if b.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, chunk) in b.chunks_exact(2).enumerate() {
        let hi = digit(chunk[0])?;
        let lo = digit(chunk[1])?;
        out[i] = (hi << 4) | lo;
    }
    Some(out)
}

fn digit(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        let mut bytes = [0u8; 32];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(7).wrapping_add(3);
        }
        let hex = to_hex(&bytes);
        assert_eq!(hex.len(), 64);
        assert_eq!(from_hex(&hex), Some(bytes));
    }

    #[test]
    fn a_name_that_is_not_64_hex_digits_is_refused() {
        // Every one of these would be a path component if the decoder were
        // lenient about length or alphabet.
        for bad in [
            "",
            "00",
            &"0".repeat(63),
            &"0".repeat(65),
            &format!("{}..", "0".repeat(62)),
            &format!("{}//", "0".repeat(62)),
            &format!("{}\\\\", "0".repeat(62)),
            &"g".repeat(64),
        ] {
            assert_eq!(from_hex(bad), None, "accepted {bad:?}");
        }
    }

    #[test]
    fn uppercase_is_accepted_and_normalizes_to_the_same_bytes() {
        let lower = "ab".repeat(32);
        let upper = "AB".repeat(32);
        assert_eq!(from_hex(&lower), from_hex(&upper));
        // ...but we only ever *write* lowercase, so a name we produced round
        // trips to itself.
        assert_eq!(to_hex(&from_hex(&upper).unwrap()), lower);
    }
}
