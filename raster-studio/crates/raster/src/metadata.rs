//! W10-E: document metadata written into exported files — XMP (File Info's
//! title, author, description, keywords and copyright) and EXIF (carried over
//! from the file a document was opened from).
//!
//! # Why a post-pass over encoded bytes
//!
//! `image` 0.25's encoders write pixels and, at most, an ICC profile; none of
//! them takes an XMP packet or an EXIF block. So metadata is spliced into the
//! finished file here, by container:
//!
//! | Container | XMP                                   | EXIF                     |
//! |-----------|---------------------------------------|--------------------------|
//! | PNG       | an `iTXt` chunk, keyword `XML:com.adobe.xmp`, before `IDAT` | read only (`eXIf`) |
//! | JPEG      | an `APP1` segment, `http://ns.adobe.com/xap/1.0/\0` | an `APP1` `Exif\0\0` segment |
//! | TIFF      | tag 700 (`XMLPacket`) in the first IFD | read only                |
//!
//! Every splice is *structural*: the PNG chunk carries its own CRC, the JPEG
//! segment its own length, and the TIFF IFD is rewritten whole (the new one
//! appended, the header re-pointed), so the file stays one any decoder —
//! this crate's included — reads unchanged. A packet that does not fit its
//! container (a JPEG segment tops out at 65 533 payload bytes) is an error,
//! never a truncation.
//!
//! # The packet
//!
//! [`XmpFields::to_packet`] writes the five Dublin Core properties Photopea's
//! File Info edits (`dc:title`, `dc:creator`, `dc:description`, `dc:subject`,
//! `dc:rights`) in the RDF/XML shape Adobe applications write, text escaped.
//! [`XmpFields::from_packet`] reads those five back — enough to round-trip
//! what this editor writes and to read the same properties from a packet
//! another application wrote; it is not a general RDF parser.

use crate::codec::ExportFormat;

/// The XMP properties File Info edits.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct XmpFields {
    /// `dc:title`.
    #[serde(default)]
    pub title: String,
    /// `dc:creator` (one entry).
    #[serde(default)]
    pub author: String,
    /// `dc:description`.
    #[serde(default)]
    pub description: String,
    /// `dc:subject`, one keyword per entry.
    #[serde(default)]
    pub keywords: Vec<String>,
    /// `dc:rights`.
    #[serde(default)]
    pub copyright: String,
}

/// Why metadata could not be written into a file.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum MetadataError {
    #[error("the file is not a {0} this writer understands")]
    Malformed(&'static str),
    #[error("{0} cannot carry XMP metadata")]
    Unsupported(&'static str),
    #[error("the {what} is {len} bytes; a JPEG segment holds at most {max}")]
    TooLarge {
        what: &'static str,
        len: usize,
        max: usize,
    },
}

/// The namespace header that opens a JPEG XMP `APP1` segment.
pub const JPEG_XMP_HEADER: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
/// The header that opens a JPEG EXIF `APP1` segment.
pub const JPEG_EXIF_HEADER: &[u8] = b"Exif\0\0";
/// The `iTXt` keyword a PNG XMP packet is stored under.
pub const PNG_XMP_KEYWORD: &[u8] = b"XML:com.adobe.xmp";
/// TIFF tag 700, `XMLPacket`.
pub const TIFF_XMP_TAG: u16 = 700;
/// Largest payload one JPEG marker segment carries (its length field is 16
/// bits and counts itself).
const JPEG_SEGMENT_MAX: usize = 65_533;

impl XmpFields {
    /// Whether every field is blank — nothing worth writing.
    pub fn is_empty(&self) -> bool {
        self.title.trim().is_empty()
            && self.author.trim().is_empty()
            && self.description.trim().is_empty()
            && self.keywords.iter().all(|k| k.trim().is_empty())
            && self.copyright.trim().is_empty()
    }

    /// Split a comma- or semicolon-separated keyword line, trimming each.
    pub fn parse_keywords(line: &str) -> Vec<String> {
        line.split([',', ';'])
            .map(str::trim)
            .filter(|k| !k.is_empty())
            .map(str::to_string)
            .collect()
    }

    /// The keywords as one comma-separated line.
    pub fn keyword_line(&self) -> String {
        self.keywords.join(", ")
    }

    /// The RDF/XML packet, wrapped in `<?xpacket?>` processing instructions.
    pub fn to_packet(&self) -> String {
        let mut body = String::new();
        let alt = |name: &str, value: &str, out: &mut String| {
            if value.trim().is_empty() {
                return;
            }
            out.push_str(&format!(
                "   <dc:{name}><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></dc:{name}>\n",
                escape_xml(value)
            ));
        };
        alt("title", &self.title, &mut body);
        if !self.author.trim().is_empty() {
            body.push_str(&format!(
                "   <dc:creator><rdf:Seq><rdf:li>{}</rdf:li></rdf:Seq></dc:creator>\n",
                escape_xml(&self.author)
            ));
        }
        alt("description", &self.description, &mut body);
        let keywords: Vec<&String> = self
            .keywords
            .iter()
            .filter(|k| !k.trim().is_empty())
            .collect();
        if !keywords.is_empty() {
            body.push_str("   <dc:subject><rdf:Bag>");
            for k in keywords {
                body.push_str(&format!("<rdf:li>{}</rdf:li>", escape_xml(k)));
            }
            body.push_str("</rdf:Bag></dc:subject>\n");
        }
        alt("rights", &self.copyright, &mut body);
        format!(
            "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n\
             <x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n \
             <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n  \
             <rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\n\
             {body}  </rdf:Description>\n </rdf:RDF>\n</x:xmpmeta>\n<?xpacket end=\"w\"?>"
        )
    }

    /// Read the five File Info properties out of an XMP packet. Properties
    /// the packet does not carry stay blank.
    pub fn from_packet(packet: &str) -> Self {
        let first = |name: &str| {
            element_items(packet, name)
                .into_iter()
                .next()
                .unwrap_or_default()
        };
        XmpFields {
            title: first("dc:title"),
            author: first("dc:creator"),
            description: first("dc:description"),
            keywords: element_items(packet, "dc:subject"),
            copyright: first("dc:rights"),
        }
    }
}

/// Escape the five XML specials.
fn escape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// Undo [`escape_xml`], plus numeric character references.
fn unescape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        let Some(end) = tail.find(';') else {
            out.push_str(tail);
            return out;
        };
        let entity = &tail[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            e if e.starts_with("#x") || e.starts_with("#X") => {
                u32::from_str_radix(&e[2..], 16).ok().and_then(char::from_u32)
            }
            e if e.starts_with('#') => e[1..].parse::<u32>().ok().and_then(char::from_u32),
            _ => None,
        };
        match decoded {
            Some(c) => out.push(c),
            None => out.push_str(&tail[..=end]),
        }
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    out
}

/// The text of every `rdf:li` inside the first `<name>…</name>` element, or
/// the element's own text when it holds no list.
fn element_items(packet: &str, name: &str) -> Vec<String> {
    let open = format!("<{name}");
    let close = format!("</{name}>");
    let Some(start) = packet.find(&open) else {
        return Vec::new();
    };
    let after_open = &packet[start + open.len()..];
    let Some(gt) = after_open.find('>') else {
        return Vec::new();
    };
    if after_open[..gt].ends_with('/') {
        return Vec::new();
    }
    let inner_start = start + open.len() + gt + 1;
    let Some(end) = packet[inner_start..].find(&close) else {
        return Vec::new();
    };
    let inner = &packet[inner_start..inner_start + end];
    let mut items = Vec::new();
    let mut rest = inner;
    while let Some(li) = rest.find("<rdf:li") {
        let tail = &rest[li..];
        let Some(gt) = tail.find('>') else { break };
        let body = &tail[gt + 1..];
        let Some(end) = body.find("</rdf:li>") else {
            break;
        };
        items.push(unescape_xml(&body[..end]));
        rest = &body[end + "</rdf:li>".len()..];
    }
    if items.is_empty() && !inner.contains('<') && !inner.trim().is_empty() {
        items.push(unescape_xml(inner.trim()));
    }
    items
}

// ---------------------------------------------------------------------------
// Dispatch by container
// ---------------------------------------------------------------------------

/// Whether [`embed_xmp`] can write into files of `format`.
pub fn carries_xmp(format: ExportFormat) -> bool {
    matches!(
        format,
        ExportFormat::Png | ExportFormat::Jpeg(_) | ExportFormat::Tiff
    )
}

/// Splice `packet` into an encoded file of `format`, replacing any XMP the
/// file already carries.
pub fn embed_xmp(format: ExportFormat, bytes: &[u8], packet: &str) -> Result<Vec<u8>, MetadataError> {
    match format {
        ExportFormat::Png => png_set_xmp(bytes, packet),
        ExportFormat::Jpeg(_) => jpeg_set_app1(bytes, JPEG_XMP_HEADER, packet.as_bytes(), "XMP packet"),
        ExportFormat::Tiff => tiff_set_xmp(bytes, packet),
        _ => Err(MetadataError::Unsupported(format.extension_upper())),
    }
}

/// Splice an EXIF block (the TIFF-structured payload that follows
/// `Exif\0\0`) into a JPEG, replacing any it already carries.
pub fn embed_jpeg_exif(bytes: &[u8], exif: &[u8]) -> Result<Vec<u8>, MetadataError> {
    jpeg_set_app1(bytes, JPEG_EXIF_HEADER, exif, "EXIF block")
}

/// The XMP packet an encoded PNG, JPEG or TIFF carries, sniffed by content.
pub fn read_xmp(bytes: &[u8]) -> Option<String> {
    if bytes.starts_with(PNG_SIGNATURE) {
        png_read_xmp(bytes)
    } else if bytes.starts_with(&[0xFF, 0xD8]) {
        jpeg_find_app1(bytes, JPEG_XMP_HEADER)
            .and_then(|payload| String::from_utf8(payload.to_vec()).ok())
    } else if bytes.starts_with(b"II*\0") || bytes.starts_with(b"MM\0*") {
        tiff_read_xmp(bytes)
    } else {
        None
    }
}

/// The EXIF block (without its `Exif\0\0` header) a JPEG's `APP1` or a PNG's
/// `eXIf` chunk carries.
pub fn read_exif(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.starts_with(&[0xFF, 0xD8]) {
        jpeg_find_app1(bytes, JPEG_EXIF_HEADER).map(<[u8]>::to_vec)
    } else if bytes.starts_with(PNG_SIGNATURE) {
        png_chunks(bytes)?
            .into_iter()
            .find(|c| &c.kind == b"eXIf")
            .map(|c| bytes[c.data.clone()].to_vec())
    } else {
        None
    }
}

trait ExtensionUpper {
    fn extension_upper(self) -> &'static str;
}

impl ExtensionUpper for ExportFormat {
    fn extension_upper(self) -> &'static str {
        match self {
            ExportFormat::Png => "PNG",
            ExportFormat::Jpeg(_) => "JPEG",
            ExportFormat::WebP => "WebP",
            ExportFormat::Tiff => "TIFF",
            ExportFormat::Gif => "GIF",
            ExportFormat::Bmp => "BMP",
            ExportFormat::Tga => "TGA",
            ExportFormat::Ico => "ICO",
            ExportFormat::Svg => "SVG",
        }
    }
}

// ---------------------------------------------------------------------------
// PNG
// ---------------------------------------------------------------------------

const PNG_SIGNATURE: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

struct PngChunk {
    kind: [u8; 4],
    /// The whole chunk: length, type, data and CRC.
    whole: std::ops::Range<usize>,
    data: std::ops::Range<usize>,
}

fn png_chunks(bytes: &[u8]) -> Option<Vec<PngChunk>> {
    if !bytes.starts_with(PNG_SIGNATURE) {
        return None;
    }
    let mut at = PNG_SIGNATURE.len();
    let mut out = Vec::new();
    while at + 12 <= bytes.len() {
        let len = u32::from_be_bytes(bytes[at..at + 4].try_into().ok()?) as usize;
        let kind: [u8; 4] = bytes[at + 4..at + 8].try_into().ok()?;
        let data_end = (at + 8).checked_add(len)?;
        let end = data_end.checked_add(4)?;
        if end > bytes.len() {
            return None;
        }
        out.push(PngChunk {
            kind,
            whole: at..end,
            data: at + 8..data_end,
        });
        at = end;
        if &kind == b"IEND" {
            break;
        }
    }
    Some(out)
}

fn is_xmp_itxt(bytes: &[u8], chunk: &PngChunk) -> bool {
    &chunk.kind == b"iTXt" && {
        let data = &bytes[chunk.data.clone()];
        data.starts_with(PNG_XMP_KEYWORD) && data.get(PNG_XMP_KEYWORD.len()) == Some(&0)
    }
}

fn png_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 12);
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc_input = Vec::with_capacity(data.len() + 4);
    crc_input.extend_from_slice(kind);
    crc_input.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
    out
}

fn png_set_xmp(bytes: &[u8], packet: &str) -> Result<Vec<u8>, MetadataError> {
    let chunks = png_chunks(bytes).ok_or(MetadataError::Malformed("PNG"))?;
    if chunks.first().map(|c| &c.kind) != Some(b"IHDR") {
        return Err(MetadataError::Malformed("PNG"));
    }
    // iTXt: keyword, NUL, compression flag 0, method 0, empty language tag
    // and translated keyword (each NUL-terminated), then the UTF-8 text.
    let mut data = PNG_XMP_KEYWORD.to_vec();
    data.extend_from_slice(&[0, 0, 0, 0, 0]);
    data.extend_from_slice(packet.as_bytes());
    let itxt = png_chunk(b"iTXt", &data);
    let mut out = Vec::with_capacity(bytes.len() + itxt.len());
    out.extend_from_slice(PNG_SIGNATURE);
    for (i, chunk) in chunks.iter().enumerate() {
        if is_xmp_itxt(bytes, chunk) {
            continue;
        }
        out.extend_from_slice(&bytes[chunk.whole.clone()]);
        if i == 0 {
            out.extend_from_slice(&itxt);
        }
    }
    Ok(out)
}

fn png_read_xmp(bytes: &[u8]) -> Option<String> {
    let chunks = png_chunks(bytes)?;
    let chunk = chunks.iter().find(|c| is_xmp_itxt(bytes, c))?;
    let data = &bytes[chunk.data.clone()];
    let mut rest = &data[PNG_XMP_KEYWORD.len() + 1..];
    let compressed = *rest.first()? != 0;
    rest = rest.get(2..)?;
    // Language tag, then translated keyword: both NUL-terminated.
    for _ in 0..2 {
        let nul = rest.iter().position(|&b| b == 0)?;
        rest = &rest[nul + 1..];
    }
    if compressed {
        use std::io::Read as _;
        let mut text = String::new();
        flate2::read::ZlibDecoder::new(rest)
            .read_to_string(&mut text)
            .ok()?;
        return Some(text);
    }
    String::from_utf8(rest.to_vec()).ok()
}

/// CRC-32 (ISO 3309, the polynomial PNG uses).
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

// ---------------------------------------------------------------------------
// JPEG
// ---------------------------------------------------------------------------

/// The marker segments before the entropy-coded data, as
/// `(marker, whole segment range, payload range)`, and where the rest (from
/// SOS) starts.
#[allow(clippy::type_complexity)]
fn jpeg_segments(
    bytes: &[u8],
) -> Option<(Vec<(u8, std::ops::Range<usize>, std::ops::Range<usize>)>, usize)> {
    if !bytes.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut at = 2;
    let mut out = Vec::new();
    loop {
        if at + 4 > bytes.len() || bytes[at] != 0xFF {
            return None;
        }
        let marker = bytes[at + 1];
        if marker == 0xDA || marker == 0xD9 {
            return Some((out, at));
        }
        let len = u16::from_be_bytes([bytes[at + 2], bytes[at + 3]]) as usize;
        if len < 2 || at + 2 + len > bytes.len() {
            return None;
        }
        out.push((marker, at..at + 2 + len, at + 4..at + 2 + len));
        at += 2 + len;
    }
}

fn jpeg_find_app1<'a>(bytes: &'a [u8], header: &[u8]) -> Option<&'a [u8]> {
    let (segments, _) = jpeg_segments(bytes)?;
    segments.into_iter().find_map(|(marker, _, payload)| {
        let p = &bytes[payload];
        (marker == 0xE1 && p.starts_with(header)).then(|| &p[header.len()..])
    })
}

fn jpeg_set_app1(
    bytes: &[u8],
    header: &[u8],
    body: &[u8],
    what: &'static str,
) -> Result<Vec<u8>, MetadataError> {
    let (segments, rest) = jpeg_segments(bytes).ok_or(MetadataError::Malformed("JPEG"))?;
    let payload_len = header.len() + body.len();
    if payload_len > JPEG_SEGMENT_MAX {
        return Err(MetadataError::TooLarge {
            what,
            len: body.len(),
            max: JPEG_SEGMENT_MAX - header.len(),
        });
    }
    let mut segment = vec![0xFF, 0xE1];
    segment.extend_from_slice(&((payload_len + 2) as u16).to_be_bytes());
    segment.extend_from_slice(header);
    segment.extend_from_slice(body);
    let mut out = Vec::with_capacity(bytes.len() + segment.len());
    out.extend_from_slice(&[0xFF, 0xD8]);
    // After a leading JFIF APP0 (which must come first), before the rest.
    let mut placed = false;
    for (marker, whole, payload) in &segments {
        let p = &bytes[payload.clone()];
        if *marker == 0xE1 && p.starts_with(header) {
            continue;
        }
        if !placed && *marker != 0xE0 {
            out.extend_from_slice(&segment);
            placed = true;
        }
        out.extend_from_slice(&bytes[whole.clone()]);
    }
    if !placed {
        out.extend_from_slice(&segment);
    }
    out.extend_from_slice(&bytes[rest..]);
    Ok(out)
}

// ---------------------------------------------------------------------------
// TIFF
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Endian(bool);

impl Endian {
    fn u16(self, b: &[u8]) -> u16 {
        let a = [b[0], b[1]];
        if self.0 {
            u16::from_le_bytes(a)
        } else {
            u16::from_be_bytes(a)
        }
    }
    fn u32(self, b: &[u8]) -> u32 {
        let a = [b[0], b[1], b[2], b[3]];
        if self.0 {
            u32::from_le_bytes(a)
        } else {
            u32::from_be_bytes(a)
        }
    }
    fn put16(self, v: u16) -> [u8; 2] {
        if self.0 {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        }
    }
    fn put32(self, v: u32) -> [u8; 4] {
        if self.0 {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        }
    }
}

/// The first IFD: its entries (12 raw bytes each) and the next-IFD offset.
fn tiff_ifd0(bytes: &[u8]) -> Option<(Endian, Vec<[u8; 12]>, u32)> {
    let endian = match bytes.get(..4)? {
        b"II*\0" => Endian(true),
        b"MM\0*" => Endian(false),
        _ => return None,
    };
    let ifd = endian.u32(bytes.get(4..8)?) as usize;
    let count = endian.u16(bytes.get(ifd..ifd + 2)?) as usize;
    let mut entries = Vec::with_capacity(count + 1);
    for i in 0..count {
        let at = ifd + 2 + i * 12;
        entries.push(bytes.get(at..at + 12)?.try_into().ok()?);
    }
    let next_at = ifd + 2 + count * 12;
    let next = endian.u32(bytes.get(next_at..next_at + 4)?);
    Some((endian, entries, next))
}

fn tiff_set_xmp(bytes: &[u8], packet: &str) -> Result<Vec<u8>, MetadataError> {
    let (endian, mut entries, next) =
        tiff_ifd0(bytes).ok_or(MetadataError::Malformed("TIFF"))?;
    entries.retain(|e| endian.u16(&e[0..2]) != TIFF_XMP_TAG);
    let mut out = bytes.to_vec();
    if out.len() % 2 == 1 {
        out.push(0);
    }
    // The packet first, word-aligned, then the rewritten IFD pointing at it.
    let data_at = out.len();
    out.extend_from_slice(packet.as_bytes());
    if out.len() % 2 == 1 {
        out.push(0);
    }
    let too_far = |n: usize| u32::try_from(n).map_err(|_| MetadataError::Malformed("TIFF"));
    let mut entry = [0u8; 12];
    entry[0..2].copy_from_slice(&endian.put16(TIFF_XMP_TAG));
    // Type 1 (BYTE), as the TIFF/XMP specification writes XMLPacket.
    entry[2..4].copy_from_slice(&endian.put16(1));
    entry[4..8].copy_from_slice(&endian.put32(too_far(packet.len())?));
    entry[8..12].copy_from_slice(&endian.put32(too_far(data_at)?));
    entries.push(entry);
    entries.sort_by_key(|e| endian.u16(&e[0..2]));
    let ifd_at = out.len();
    out.extend_from_slice(&endian.put16(entries.len() as u16));
    for e in &entries {
        out.extend_from_slice(e);
    }
    out.extend_from_slice(&endian.put32(next));
    out[4..8].copy_from_slice(&endian.put32(too_far(ifd_at)?));
    Ok(out)
}

fn tiff_read_xmp(bytes: &[u8]) -> Option<String> {
    let (endian, entries, _) = tiff_ifd0(bytes)?;
    let entry = entries
        .iter()
        .find(|e| endian.u16(&e[0..2]) == TIFF_XMP_TAG)?;
    let len = endian.u32(&entry[4..8]) as usize;
    let data = if len <= 4 {
        &entry[8..8 + len]
    } else {
        let at = endian.u32(&entry[8..12]) as usize;
        bytes.get(at..at.checked_add(len)?)?
    };
    String::from_utf8(data.to_vec()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields() -> XmpFields {
        XmpFields {
            title: "Harbour at dusk & <night>".into(),
            author: "A. Painter".into(),
            description: "Boats, \"quoted\"".into(),
            keywords: vec!["sea".into(), "boats".into()],
            copyright: "(c) 2026".into(),
        }
    }

    fn pixels(w: u32, h: u32) -> Vec<u8> {
        (0..w * h)
            .flat_map(|i| [(i * 7) as u8, (i * 3) as u8, 200, 255])
            .collect()
    }

    #[test]
    fn a_packet_round_trips_every_field_through_its_own_parser() {
        let f = fields();
        assert_eq!(XmpFields::from_packet(&f.to_packet()), f);
        assert!(XmpFields::default().is_empty());
        assert!(!f.is_empty());
    }

    #[test]
    fn the_xmp_title_round_trips_through_a_png_that_still_decodes() {
        let png = crate::encode(ExportFormat::Png, 5, 4, &pixels(5, 4)).unwrap();
        let tagged = embed_xmp(ExportFormat::Png, &png, &fields().to_packet()).unwrap();
        let packet = read_xmp(&tagged).expect("the iTXt chunk is there");
        assert_eq!(XmpFields::from_packet(&packet).title, "Harbour at dusk & <night>");
        // A second embed replaces rather than stacks.
        let again = embed_xmp(ExportFormat::Png, &tagged, &XmpFields {
            title: "Second".into(),
            ..XmpFields::default()
        }
        .to_packet())
        .unwrap();
        let chunks = png_chunks(&again).unwrap();
        assert_eq!(chunks.iter().filter(|c| is_xmp_itxt(&again, c)).count(), 1);
        assert_eq!(
            XmpFields::from_packet(&read_xmp(&again).unwrap()).title,
            "Second"
        );
        let decoded = crate::decode_bytes(&tagged).expect("the PNG still decodes");
        assert_eq!((decoded.width, decoded.height), (5, 4));
        assert_eq!(decoded.rgba8, pixels(5, 4));
    }

    #[test]
    fn jpeg_carries_xmp_and_exif_and_still_decodes() {
        let jpeg = crate::encode(ExportFormat::Jpeg(90), 8, 8, &pixels(8, 8)).unwrap();
        let exif = b"II*\0\x08\0\0\0\0\0\0\0\0\0".to_vec();
        let with_exif = embed_jpeg_exif(&jpeg, &exif).unwrap();
        let tagged = embed_xmp(ExportFormat::Jpeg(90), &with_exif, &fields().to_packet()).unwrap();
        assert_eq!(read_exif(&tagged).as_deref(), Some(&exif[..]));
        assert_eq!(XmpFields::from_packet(&read_xmp(&tagged).unwrap()), fields());
        let decoded = crate::decode_bytes(&tagged).expect("the JPEG still decodes");
        assert_eq!((decoded.width, decoded.height), (8, 8));
        // Replacing, not stacking.
        let again = embed_jpeg_exif(&tagged, &exif).unwrap();
        let (segments, _) = jpeg_segments(&again).unwrap();
        let exif_segments = segments
            .iter()
            .filter(|(m, _, p)| *m == 0xE1 && again[p.clone()].starts_with(JPEG_EXIF_HEADER))
            .count();
        assert_eq!(exif_segments, 1);
    }

    #[test]
    fn an_oversized_jpeg_packet_is_refused_not_truncated() {
        let jpeg = crate::encode(ExportFormat::Jpeg(90), 4, 4, &pixels(4, 4)).unwrap();
        let big = "x".repeat(70_000);
        assert!(matches!(
            embed_xmp(ExportFormat::Jpeg(90), &jpeg, &big),
            Err(MetadataError::TooLarge { .. })
        ));
    }

    #[test]
    fn tiff_carries_xmp_in_tag_700_and_still_decodes() {
        let tiff = crate::encode(ExportFormat::Tiff, 6, 3, &pixels(6, 3)).unwrap();
        let tagged = embed_xmp(ExportFormat::Tiff, &tiff, &fields().to_packet()).unwrap();
        assert_eq!(XmpFields::from_packet(&read_xmp(&tagged).unwrap()), fields());
        let decoded = crate::decode_bytes(&tagged).expect("the TIFF still decodes");
        assert_eq!(decoded.rgba8, pixels(6, 3));
    }

    #[test]
    fn formats_without_a_metadata_slot_say_so() {
        assert!(!carries_xmp(ExportFormat::Gif));
        assert_eq!(
            embed_xmp(ExportFormat::Gif, b"GIF89a", "x"),
            Err(MetadataError::Unsupported("GIF"))
        );
    }

    #[test]
    fn the_crc_matches_the_png_reference_value() {
        // The CRC of the bare "IEND" chunk type, as every PNG ends with.
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
    }
}
