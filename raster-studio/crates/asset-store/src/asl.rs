//! W9-H: Photoshop style libraries (`.asl`) — the container, read.
//!
//! An `.asl` file is a list of layer styles, each a pair of action
//! descriptors, after an optional block of the patterns the styles use:
//!
//! ```text
//! u16  version            = 2
//! [4]  signature          = "8BSL"
//! u16  pattern version    = 3
//! u32  pattern section length, then that many bytes
//! u32  style count
//! per style:
//!   u32  length of the rest of this style (padded to 4)
//!   u32  descriptor version = 16, then the "null" descriptor naming the
//!        style: `Nm  ` (TEXT, its name) and `Idnt` (TEXT, its id)
//!   u32  descriptor version = 16, then the "Styl" descriptor whose `Lefx`
//!        item holds the effects
//! ```
//!
//! This crate knows the framing and the name descriptor, both fixed shapes;
//! it hands each style's effect descriptor back **as bytes**
//! ([`AslStyle::style_descriptor`], starting at its version word). Decoding
//! that into layer effects is the job of the crate that already reads action
//! descriptors (`psd`), which this store deliberately does not depend on —
//! the same contract the style presets' JSON keeps.
//!
//! Every length is read from an untrusted file, so each is checked against
//! the bytes actually left before anything is sliced or allocated, and the
//! style count is capped.

use std::fmt;

/// The most styles one library may declare. Photoshop's own libraries hold
/// dozens; the cap only stops a forged count from driving a huge loop.
pub const MAX_ASL_STYLES: u32 = 100_000;

/// One style read from a library.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AslStyle {
    /// The style's name (`Nm  `), as the Styles grid shows it.
    pub name: String,
    /// The style's id (`Idnt`), empty when the file has none.
    pub id: String,
    /// The style's second descriptor, from its version word on: a `Styl`
    /// descriptor whose `Lefx` item is the effects block.
    pub style_descriptor: Vec<u8>,
}

/// A library read from bytes.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct AslLibrary {
    pub styles: Vec<AslStyle>,
    /// The raw pattern section (the patterns the styles' fills reference),
    /// kept for a reader that decodes them; empty when the file has none.
    pub patterns: Vec<u8>,
}

/// Why a library could not be read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AslError {
    /// The file ended inside `what`.
    Truncated { what: &'static str },
    /// The first bytes are not a style library.
    NotAStyleLibrary,
    /// A version this reader does not know.
    UnsupportedVersion { what: &'static str, version: u32 },
    /// More styles than [`MAX_ASL_STYLES`].
    TooManyStyles(u32),
    /// A style's name descriptor holds something other than text items.
    UnreadableName { style: usize },
}

impl fmt::Display for AslError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated { what } => write!(f, "the style library ends inside {what}"),
            Self::NotAStyleLibrary => write!(f, "this is not a Photoshop style library (.asl)"),
            Self::UnsupportedVersion { what, version } => {
                write!(f, "unsupported {what} version {version}")
            }
            Self::TooManyStyles(n) => write!(f, "the library declares {n} styles"),
            Self::UnreadableName { style } => {
                write!(f, "style {} has an unreadable name", style + 1)
            }
        }
    }
}

impl std::error::Error for AslError {}

/// A bounds-checked big-endian reader.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize, what: &'static str) -> Result<&'a [u8], AslError> {
        let end = self
            .at
            .checked_add(n)
            .filter(|end| *end <= self.bytes.len())
            .ok_or(AslError::Truncated { what })?;
        let out = &self.bytes[self.at..end];
        self.at = end;
        Ok(out)
    }

    fn u16(&mut self, what: &'static str) -> Result<u16, AslError> {
        let b = self.take(2, what)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    fn u32(&mut self, what: &'static str) -> Result<u32, AslError> {
        let b = self.take(4, what)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn rest(&self) -> usize {
        self.bytes.len() - self.at
    }

    /// A length-prefixed UTF-16BE string, trailing NULs dropped.
    fn unicode(&mut self, what: &'static str) -> Result<String, AslError> {
        let n = self.u32(what)? as usize;
        let raw = self.take(n.checked_mul(2).ok_or(AslError::Truncated { what })?, what)?;
        let units: Vec<u16> = raw
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_be_bytes(*c))
            .collect();
        Ok(String::from_utf16_lossy(&units)
            .trim_end_matches('\0')
            .to_string())
    }

    /// A descriptor key or class id: a length, or `0` then four bytes.
    fn key(&mut self, what: &'static str) -> Result<String, AslError> {
        let n = self.u32(what)? as usize;
        let n = if n == 0 { 4 } else { n };
        Ok(String::from_utf8_lossy(self.take(n, what)?).into_owned())
    }
}

/// Read a style library.
pub fn parse_asl(bytes: &[u8]) -> Result<AslLibrary, AslError> {
    let mut r = Reader { bytes, at: 0 };
    let version = r
        .u16("the header")
        .map_err(|_| AslError::NotAStyleLibrary)?;
    let signature = r
        .take(4, "the header")
        .map_err(|_| AslError::NotAStyleLibrary)?;
    if signature != b"8BSL" {
        return Err(AslError::NotAStyleLibrary);
    }
    if version != 2 {
        return Err(AslError::UnsupportedVersion {
            what: "library",
            version: u32::from(version),
        });
    }
    let pattern_version = r.u16("the pattern header")?;
    if pattern_version != 3 {
        return Err(AslError::UnsupportedVersion {
            what: "pattern section",
            version: u32::from(pattern_version),
        });
    }
    let pattern_len = r.u32("the pattern header")? as usize;
    let patterns = r.take(pattern_len, "the pattern section")?.to_vec();
    let count = r.u32("the style count")?;
    if count > MAX_ASL_STYLES {
        return Err(AslError::TooManyStyles(count));
    }
    let mut styles = Vec::with_capacity((count as usize).min(r.rest() / 4));
    for index in 0..count as usize {
        let len = r.u32("a style's length")? as usize;
        let body = r.take(len, "a style")?;
        styles.push(read_style(body, index)?);
    }
    Ok(AslLibrary { styles, patterns })
}

/// One style's body: the name descriptor, then the effect descriptor.
fn read_style(body: &[u8], index: usize) -> Result<AslStyle, AslError> {
    let mut r = Reader { bytes: body, at: 0 };
    let version = r.u32("a style's name")?;
    if version != 16 {
        return Err(AslError::UnsupportedVersion {
            what: "descriptor",
            version,
        });
    }
    let _display = r.unicode("a style's name")?;
    let _class = r.key("a style's name")?;
    let items = r.u32("a style's name")?;
    let (mut name, mut id) = (String::new(), String::new());
    for _ in 0..items {
        let key = r.key("a style's name")?;
        let kind = r.take(4, "a style's name")?;
        if kind != b"TEXT" {
            return Err(AslError::UnreadableName { style: index });
        }
        let text = r.unicode("a style's name")?;
        match key.as_str() {
            "Nm  " => name = text,
            "Idnt" => id = text,
            _ => {}
        }
    }
    let rest = &body[r.at..];
    // The effect descriptor must at least carry its version word.
    let mut check = Reader { bytes: rest, at: 0 };
    let version = check.u32("a style's effects")?;
    if version != 16 {
        return Err(AslError::UnsupportedVersion {
            what: "descriptor",
            version,
        });
    }
    if name.is_empty() {
        name = format!("Style {}", index + 1);
    }
    Ok(AslStyle {
        name,
        id,
        style_descriptor: rest.to_vec(),
    })
}

/// W9-H: build a style library from `(name, id, style descriptor)` triples
/// — the writer half of [`parse_asl`], used to make fixtures and to export.
/// `style_descriptor` starts at its version word, as [`AslStyle`] holds it.
pub fn write_asl(styles: &[(&str, &str, &[u8])]) -> Vec<u8> {
    fn unicode(out: &mut Vec<u8>, s: &str) {
        let units: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
        out.extend_from_slice(&(units.len() as u32).to_be_bytes());
        for u in units {
            out.extend_from_slice(&u.to_be_bytes());
        }
    }
    fn key(out: &mut Vec<u8>, k: &str) {
        if k.len() == 4 {
            out.extend_from_slice(&0u32.to_be_bytes());
        } else {
            out.extend_from_slice(&(k.len() as u32).to_be_bytes());
        }
        out.extend_from_slice(k.as_bytes());
    }
    let mut out = Vec::new();
    out.extend_from_slice(&2u16.to_be_bytes());
    out.extend_from_slice(b"8BSL");
    out.extend_from_slice(&3u16.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes());
    out.extend_from_slice(&(styles.len() as u32).to_be_bytes());
    for (name, id, descriptor) in styles {
        let mut body = Vec::new();
        body.extend_from_slice(&16u32.to_be_bytes());
        unicode(&mut body, "");
        key(&mut body, "null");
        body.extend_from_slice(&2u32.to_be_bytes());
        key(&mut body, "Nm  ");
        body.extend_from_slice(b"TEXT");
        unicode(&mut body, name);
        key(&mut body, "Idnt");
        body.extend_from_slice(b"TEXT");
        unicode(&mut body, id);
        body.extend_from_slice(descriptor);
        while body.len() % 4 != 0 {
            body.push(0);
        }
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(&body);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal effect descriptor: version, empty name, class `Styl`, no
    /// items.
    fn empty_styl() -> Vec<u8> {
        let mut d = Vec::new();
        d.extend_from_slice(&16u32.to_be_bytes());
        d.extend_from_slice(&1u32.to_be_bytes());
        d.extend_from_slice(&0u16.to_be_bytes());
        d.extend_from_slice(&0u32.to_be_bytes());
        d.extend_from_slice(b"Styl");
        d.extend_from_slice(&0u32.to_be_bytes());
        d
    }

    #[test]
    fn a_written_library_reads_back_with_its_names_and_descriptors() {
        let d = empty_styl();
        let bytes = write_asl(&[("Chrome", "id-1", &d), ("Neon Glow", "", &d)]);
        let lib = parse_asl(&bytes).unwrap();
        assert_eq!(lib.styles.len(), 2);
        assert_eq!(lib.styles[0].name, "Chrome");
        assert_eq!(lib.styles[0].id, "id-1");
        assert_eq!(lib.styles[1].name, "Neon Glow");
        assert!(lib.styles[0].style_descriptor.starts_with(&d));
    }

    #[test]
    fn malformed_libraries_are_refused_with_a_reason() {
        assert_eq!(parse_asl(b""), Err(AslError::NotAStyleLibrary));
        assert_eq!(
            parse_asl(b"\x00\x02PNG!\x00\x03"),
            Err(AslError::NotAStyleLibrary)
        );
        let good = write_asl(&[("A", "", &empty_styl())]);
        // Cut anywhere past the header: a truncation, never a panic.
        for cut in 6..good.len() - 1 {
            let err = parse_asl(&good[..cut]).unwrap_err();
            assert!(
                matches!(err, AslError::Truncated { .. }),
                "cut at {cut}: {err:?}"
            );
        }
        // A forged style count.
        let mut forged = good.clone();
        forged[12..16].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(parse_asl(&forged), Err(AslError::TooManyStyles(u32::MAX)));
        // A forged style length runs off the end.
        let mut long = good.clone();
        long[16..20].copy_from_slice(&0x7FFF_FFFFu32.to_be_bytes());
        assert!(matches!(
            parse_asl(&long),
            Err(AslError::Truncated { what: "a style" })
        ));
        // The wrong library version.
        let mut v1 = good.clone();
        v1[1] = 1;
        assert!(matches!(
            parse_asl(&v1),
            Err(AslError::UnsupportedVersion { .. })
        ));
        assert!(!format!("{}", AslError::NotAStyleLibrary).is_empty());
    }
}
