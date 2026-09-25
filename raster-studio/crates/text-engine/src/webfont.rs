//! W16-L: WOFF and WOFF2 web fonts.
//!
//! `fontdb` (under `cosmic-text`) reads TrueType / OpenType (`sfnt`) data
//! only, so a `.woff` / `.woff2` is unwrapped to the sfnt it carries before
//! it reaches a font database: [`FontLibrary::load_bytes`] and
//! [`register_session_font`] both pass their bytes through
//! [`sfnt_from_webfont`]. The unwrapping is `wuff` 0.2.9 (MIT, pure Rust):
//! WOFF 1 tables inflate through `flate2`, WOFF 2 through
//! `brotli-decompressor`, with the `glyf` / `loca` / `hmtx` transforms
//! reversed. `wuff` bounds its own output (a hard cap on the decompressed
//! size and on the compression ratio), and the header's declared sfnt size
//! is checked here against [`MAX_SFNT_BYTES`] before it is called.
//!
//! [`FontLibrary::load_bytes`]: crate::FontLibrary::load_bytes
//! [`register_session_font`]: crate::register_session_font

/// The largest unwrapped font accepted: 64 MiB (the largest CJK fonts are
/// a few tens of megabytes).
pub const MAX_SFNT_BYTES: u32 = 64 << 20;

/// Whether `bytes` start like a WOFF (`wOFF`) or WOFF2 (`wOF2`) font.
#[must_use]
pub fn is_webfont(bytes: &[u8]) -> bool {
    bytes.starts_with(b"wOFF") || bytes.starts_with(b"wOF2")
}

/// The sfnt a WOFF / WOFF2 font carries.
///
/// # Errors
///
/// A sentence naming what is wrong: not a web font, a declared size past
/// [`MAX_SFNT_BYTES`], or damaged data.
pub fn unwrap_webfont(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if !is_webfont(bytes) {
        return Err("this is not a WOFF or WOFF2 font".into());
    }
    let declared = bytes
        .get(16..20)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or("the web font's header is cut short")?;
    if declared > MAX_SFNT_BYTES {
        return Err(format!(
            "the web font declares {declared} bytes of font data; at most {MAX_SFNT_BYTES} are read"
        ));
    }
    let result = if bytes.starts_with(b"wOFF") {
        wuff::decompress_woff1(bytes)
    } else {
        wuff::decompress_woff2(bytes)
    };
    result.map_err(|e| format!("the web font is damaged ({e:?})"))
}

/// `bytes` unchanged when they are not a web font; the sfnt inside when
/// they are; nothing (so no face loads) when a web font cannot be
/// unwrapped.
#[must_use]
pub fn sfnt_from_webfont(bytes: Vec<u8>) -> Vec<u8> {
    if is_webfont(&bytes) {
        unwrap_webfont(&bytes).unwrap_or_default()
    } else {
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FontLibrary;

    fn sfnt() -> Vec<u8> {
        dejavu::sans::regular().to_vec()
    }

    fn be16(b: &[u8], at: usize) -> usize {
        usize::from(u16::from_be_bytes([b[at], b[at + 1]]))
    }

    fn be32(b: &[u8], at: usize) -> u32 {
        u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
    }

    /// The sfnt's tables: (tag, offset, length), in directory order.
    fn tables(font: &[u8]) -> Vec<([u8; 4], usize, usize)> {
        (0..be16(font, 4))
            .map(|i| {
                let at = 12 + i * 16;
                let mut tag = [0u8; 4];
                tag.copy_from_slice(&font[at..at + 4]);
                (
                    tag,
                    be32(font, at + 8) as usize,
                    be32(font, at + 12) as usize,
                )
            })
            .collect()
    }

    /// A WOFF 1 file with every table stored uncompressed (which WOFF 1
    /// allows: `compLength == origLength`).
    fn woff1(font: &[u8]) -> Vec<u8> {
        let t = tables(font);
        let header = 44 + t.len() * 20;
        let mut out = vec![0u8; header];
        out[..4].copy_from_slice(b"wOFF");
        out[4..8].copy_from_slice(&font[..4]);
        out[12..14].copy_from_slice(&(t.len() as u16).to_be_bytes());
        out[16..20].copy_from_slice(&(font.len() as u32).to_be_bytes());
        for (i, (tag, off, len)) in t.iter().enumerate() {
            while !out.len().is_multiple_of(4) {
                out.push(0);
            }
            let at = 44 + i * 20;
            let data_at = out.len() as u32;
            out[at..at + 4].copy_from_slice(tag);
            out[at + 4..at + 8].copy_from_slice(&data_at.to_be_bytes());
            out[at + 8..at + 12].copy_from_slice(&(*len as u32).to_be_bytes());
            out[at + 12..at + 16].copy_from_slice(&(*len as u32).to_be_bytes());
            let dir = 12 + i * 16;
            out[at + 16..at + 20].copy_from_slice(&font[dir + 4..dir + 8]);
            out.extend_from_slice(&font[*off..off + len]);
        }
        let total = out.len() as u32;
        out[8..12].copy_from_slice(&total.to_be_bytes());
        out
    }

    fn base128(mut v: u32) -> Vec<u8> {
        let mut bytes = vec![(v & 0x7F) as u8];
        v >>= 7;
        while v > 0 {
            bytes.push(((v & 0x7F) as u8) | 0x80);
            v >>= 7;
        }
        bytes.reverse();
        bytes
    }

    /// A brotli stream of uncompressed meta-blocks (RFC 7932 section 9.2:
    /// `ISLAST` 0, `MNIBBLES`, `MLEN - 1`, `ISUNCOMPRESSED` 1, then the
    /// bytes), closed by an empty last meta-block.
    fn brotli_stored(data: &[u8]) -> Vec<u8> {
        struct Bits {
            out: Vec<u8>,
            acc: u64,
            n: u32,
        }
        impl Bits {
            fn put(&mut self, v: u64, bits: u32) {
                self.acc |= v << self.n;
                self.n += bits;
                while self.n >= 8 {
                    self.out.push(self.acc as u8);
                    self.acc >>= 8;
                    self.n -= 8;
                }
            }
            fn flush(&mut self) {
                if self.n > 0 {
                    self.out.push(self.acc as u8);
                    self.acc = 0;
                    self.n = 0;
                }
            }
        }
        let mut b = Bits {
            out: Vec::new(),
            acc: 0,
            n: 0,
        };
        // WBITS 16: the single bit 0.
        b.put(0, 1);
        for chunk in data.chunks(65536) {
            b.put(0, 1); // ISLAST
            b.put(0, 2); // MNIBBLES = 4
            b.put((chunk.len() - 1) as u64, 16);
            b.put(1, 1); // ISUNCOMPRESSED
            b.flush();
            b.out.extend_from_slice(chunk);
        }
        b.put(1, 1); // ISLAST
        b.put(1, 1); // ISLASTEMPTY
        b.flush();
        b.out
    }

    /// A WOFF 2 file with every table under the null transform (`glyf` and
    /// `loca` at transform version 3), brotli-"compressed" with stored
    /// meta-blocks.
    fn woff2(font: &[u8]) -> Vec<u8> {
        const KNOWN: [&[u8; 4]; 7] = [
            b"cmap", b"head", b"hhea", b"hmtx", b"maxp", b"name", b"OS/2",
        ];
        let t = tables(font);
        let mut dir = Vec::new();
        let mut stream = Vec::new();
        for (tag, off, len) in &t {
            let version: u8 = if tag == b"glyf" || tag == b"loca" {
                3
            } else {
                0
            };
            match KNOWN.iter().position(|k| *k == tag) {
                Some(i) => {
                    // Known-tag indices from the WOFF2 table: cmap 0, head 1,
                    // hhea 2, hmtx 3, maxp 4, name 5, OS/2 6.
                    dir.push((version << 6) | i as u8);
                }
                None => {
                    dir.push((version << 6) | 63);
                    dir.extend_from_slice(tag);
                }
            }
            dir.extend(base128(*len as u32));
            stream.extend_from_slice(&font[*off..off + len]);
        }
        let compressed = brotli_stored(&stream);
        let mut out = vec![0u8; 48];
        out[..4].copy_from_slice(b"wOF2");
        out[4..8].copy_from_slice(&font[..4]);
        out[12..14].copy_from_slice(&(t.len() as u16).to_be_bytes());
        out[16..20].copy_from_slice(&(font.len() as u32).to_be_bytes());
        out[20..24].copy_from_slice(&(compressed.len() as u32).to_be_bytes());
        out.extend(dir);
        out.extend(compressed);
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
        let total = out.len() as u32;
        out[8..12].copy_from_slice(&total.to_be_bytes());
        out
    }

    #[test]
    fn woff_and_woff2_fonts_load_their_faces() {
        let font = sfnt();
        for (label, file) in [("woff", woff1(&font)), ("woff2", woff2(&font))] {
            assert!(is_webfont(&file), "{label}");
            let unwrapped = unwrap_webfont(&file).unwrap_or_else(|e| panic!("{label}: {e}"));
            // The same tables come back out.
            for (tag, off, len) in tables(&font) {
                let got = tables(&unwrapped)
                    .into_iter()
                    .find(|t| t.0 == tag)
                    .unwrap_or_else(|| panic!("{label}: {tag:?} is missing"));
                assert_eq!(
                    &unwrapped[got.1..got.1 + got.2],
                    &font[off..off + len],
                    "{label}"
                );
            }
            let mut library = FontLibrary::empty();
            assert_eq!(library.load_bytes(file.clone()).len(), 1, "{label}");
            assert!(
                library.family_names().iter().any(|f| f == "DejaVu Sans"),
                "{label}: {:?}",
                library.family_names()
            );
        }
    }

    #[test]
    fn damaged_or_oversized_web_fonts_load_nothing_and_never_panic() {
        let font = sfnt();
        for file in [woff1(&font), woff2(&font)] {
            let step = (file.len() / 300).max(1);
            let mut cut = 0;
            while cut < file.len() {
                let _ = sfnt_from_webfont(file[..cut].to_vec());
                cut += step * 7;
            }
            let mut i = 0;
            while i < file.len().min(4096) {
                let mut bad = file.clone();
                bad[i] ^= 0xFF;
                let _ = sfnt_from_webfont(bad);
                i += 3;
            }
        }
        let mut huge = woff1(&font);
        huge[16..20].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(unwrap_webfont(&huge).unwrap_err().contains("at most"));
        let mut library = FontLibrary::empty();
        assert!(library.load_bytes(huge).is_empty());
        // Plain TrueType passes through untouched.
        assert_eq!(sfnt_from_webfont(font.clone()), font);
    }
}
