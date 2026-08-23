//! Fixed-width hex encoding for 32-byte content hashes.
//!
//! [`encode32`] performs exactly one allocation (a 64-byte `String` reserved up
//! front); the previous implementation allocated a `String` per byte via
//! `format!` and then concatenated them. [`encode32_into`] performs none at
//! all, which is what the hot path (building a blob's on-disk path) uses.

const LUT: &[u8; 16] = b"0123456789abcdef";

/// Encode 32 bytes as 64 lowercase hex digits into a caller-owned buffer.
///
/// Allocation-free: the caller supplies the (typically stack) buffer. Every
/// byte written is an ASCII hex digit, so the result is always valid UTF-8.
pub(crate) fn encode32_into(bytes: &[u8; 32], out: &mut [u8; 64]) {
    for (i, &b) in bytes.iter().enumerate() {
        out[i * 2] = LUT[usize::from(b >> 4)];
        out[i * 2 + 1] = LUT[usize::from(b & 0x0f)];
    }
}

/// Encode 32 bytes as 64 lowercase hex characters using a single allocation.
pub(crate) fn encode32(bytes: &[u8; 32]) -> String {
    let mut buf = [0u8; 64];
    encode32_into(bytes, &mut buf);
    // Every byte came from `LUT`, which is ASCII.
    let out = String::from_utf8(buf.to_vec()).expect("hex digits are ASCII");
    debug_assert_eq!(out.len(), 64);
    out
}

/// Decode exactly 64 hex characters (either case) into 32 bytes.
///
/// Returns `None` for any other length or any non-hex byte. Callers use this on
/// file names read back from disk, so it must never panic and never accept a
/// partially valid string.
pub(crate) fn decode32(s: &str) -> Option<[u8; 32]> {
    let src = s.as_bytes();
    if src.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (dst, pair) in out.iter_mut().zip(src.chunks_exact(2)) {
        *dst = (nibble(pair[0])? << 4) | nibble(pair[1])?;
    }
    Some(out)
}

fn nibble(c: u8) -> Option<u8> {
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
    fn encodes_low_and_high_nibbles() {
        let mut bytes = [0u8; 32];
        bytes[0] = 0x0f;
        bytes[1] = 0xf0;
        bytes[31] = 0xab;
        let hex = encode32(&bytes);
        assert_eq!(hex.len(), 64);
        assert!(hex.starts_with("0ff0"), "got {hex}");
        assert!(hex.ends_with("ab"), "got {hex}");
    }

    #[test]
    fn encode_into_fills_the_whole_buffer_with_ascii_hex() {
        let mut bytes = [0u8; 32];
        bytes[0] = 0x0f;
        bytes[1] = 0xf0;
        bytes[31] = 0xab;
        let mut buf = [0u8; 64];
        encode32_into(&bytes, &mut buf);
        assert!(
            buf.iter().all(|c| c.is_ascii_hexdigit()),
            "every byte is a hex digit, so the buffer is valid UTF-8"
        );
        assert_eq!(std::str::from_utf8(&buf).unwrap(), encode32(&bytes));
        assert_eq!(&buf[0..4], b"0ff0");
        assert_eq!(&buf[62..64], b"ab");
    }

    #[test]
    fn round_trips() {
        let mut bytes = [0u8; 32];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(7).wrapping_add(3);
        }
        assert_eq!(decode32(&encode32(&bytes)), Some(bytes));
    }

    #[test]
    fn decode_accepts_uppercase() {
        let bytes = [0xabu8; 32];
        let upper = encode32(&bytes).to_uppercase();
        assert_eq!(decode32(&upper), Some(bytes));
    }

    #[test]
    fn decode_rejects_bad_input() {
        assert_eq!(decode32(""), None);
        assert_eq!(decode32(&"0".repeat(63)), None);
        assert_eq!(decode32(&"0".repeat(65)), None);
        let mut bad = "0".repeat(63);
        bad.push('z');
        assert_eq!(decode32(&bad), None);
        // Non-ASCII must be rejected on a byte boundary check, not panic.
        assert_eq!(decode32(&"é".repeat(32)), None);
    }
}
