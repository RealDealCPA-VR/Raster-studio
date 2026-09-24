//! A minimal, single-page PDF encoder for printing.
//!
//! This is the S1.8 Print path's file half: it turns a composited RGBA raster
//! into a valid **single-page** PDF, with the artwork embedded as a
//! FlateDecode-compressed `DeviceRGB` image on one page whose media box is the
//! raster's own size (a print service then scales/centres it onto paper). It is
//! pure — no I/O, no OS printing API — so it is fully testable in a headless
//! build, which is exactly what the print flow needs before a dialog ever
//! spools to the OS.
//!
//! The output is intentionally the smallest correct PDF that renders the
//! image: catalog, page tree, one page, one content stream that `cm`s the
//! image across the page, and one image XObject. Alpha is composited onto
//! white paper (print is opaque), and the image bytes are deflate-compressed
//! with a `FlateDecode` filter. The encoder writes a correct cross-reference
//! table and `startxref`, so the file opens without repair in any conformant
//! reader.

use flate2::write::ZlibEncoder;
use flate2::Compression;

/// Encode a single-page PDF containing `rgba` (row-major, `width*height*4`)
/// composited onto white, with the page media box equal to the raster size.
pub fn encode_pdf(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let page = PdfPage {
        width_pt: f64::from(width),
        height_pt: f64::from(height),
    };
    encode_pdf_on_page(width, height, rgba, page, None)
}

/// W10-E: a page size for [`encode_pdf_on_page`], in PDF points (1/72 in).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PdfPage {
    pub width_pt: f64,
    pub height_pt: f64,
}

impl PdfPage {
    /// ISO A4 portrait, 210 x 297 mm.
    pub const A4: PdfPage = PdfPage {
        width_pt: 595.276,
        height_pt: 841.89,
    };
    /// US Letter portrait, 8.5 x 11 in.
    pub const LETTER: PdfPage = PdfPage {
        width_pt: 612.0,
        height_pt: 792.0,
    };

    /// The page the image fills exactly at `ppi` pixels per inch.
    pub fn at_ppi(width: u32, height: u32, ppi: f64) -> PdfPage {
        let ppi = if ppi.is_finite() && ppi > 0.0 {
            ppi
        } else {
            72.0
        };
        PdfPage {
            width_pt: f64::from(width) * 72.0 / ppi,
            height_pt: f64::from(height) * 72.0 / ppi,
        }
    }

    /// Where a `width` x `height` image lands: scaled to fit the page with
    /// its aspect kept, centred — `(x, y, w, h)` in points.
    pub fn placement(self, width: u32, height: u32) -> (f64, f64, f64, f64) {
        let (iw, ih) = (f64::from(width.max(1)), f64::from(height.max(1)));
        let s = (self.width_pt / iw).min(self.height_pt / ih);
        let (w, h) = (iw * s, ih * s);
        ((self.width_pt - w) / 2.0, (self.height_pt - h) / 2.0, w, h)
    }
}

/// W10-E: the document information dictionary (`/Info`) a PDF export
/// carries — File Info's fields, as PDF text strings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PdfInfo {
    pub title: String,
    pub author: String,
    pub subject: String,
    pub keywords: String,
}

impl PdfInfo {
    fn dictionary(&self) -> Option<String> {
        let mut out = String::new();
        for (key, value) in [
            ("Title", &self.title),
            ("Author", &self.author),
            ("Subject", &self.subject),
            ("Keywords", &self.keywords),
        ] {
            if !value.is_empty() {
                out.push_str(&format!(" /{key} {}", pdf_text(value)));
            }
        }
        (!out.is_empty()).then(|| format!("<<{out} /Producer (Raster Studio) >>"))
    }
}

/// A PDF text string: a literal `( )` string for ASCII, escaped, and a
/// UTF-16BE hex string with a byte-order mark otherwise (PDF 1.4, 3.8.1).
fn pdf_text(s: &str) -> String {
    if s.is_ascii() {
        let mut out = String::from("(");
        for c in s.chars() {
            match c {
                '(' | ')' | '\\' => {
                    out.push('\\');
                    out.push(c);
                }
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                c => out.push(c),
            }
        }
        out.push(')');
        return out;
    }
    let mut out = String::from("<FEFF");
    for unit in s.encode_utf16() {
        out.push_str(&format!("{unit:04X}"));
    }
    out.push('>');
    out
}

/// A number as a PDF content stream writes it: integers bare, the rest to
/// three decimals.
fn num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v:.3}")
    }
}

/// W10-E: File > Export > PDF. [`encode_pdf`] on a chosen `page`: the image
/// scaled to fit it with its aspect kept and centred, and `info` (when given
/// and not blank) written as the document information dictionary. Still a
/// raster PDF: one image XObject, no vector content.
pub fn encode_pdf_on_page(
    width: u32,
    height: u32,
    rgba: &[u8],
    page: PdfPage,
    info: Option<&PdfInfo>,
) -> Vec<u8> {
    let rgb = composite_onto_white(rgba, width as usize, height as usize);
    let mut compressed = Vec::new();
    {
        use std::io::Write;
        let mut e = ZlibEncoder::new(&mut compressed, Compression::default());
        e.write_all(&rgb).expect("in-memory deflate cannot fail");
        e.finish().expect("in-memory deflate cannot fail");
    }
    let info = info.and_then(PdfInfo::dictionary);
    let objects: usize = if info.is_some() { 7 } else { 6 };

    let mut out: Vec<u8> = Vec::new();
    let mut offsets = vec![0u64; objects];
    fn obj(out: &mut Vec<u8>, offsets: &mut [u64], n: u64, body: String) {
        offsets[n as usize] = out.len() as u64;
        out.extend_from_slice(format!("{n} 0 obj\n{body}\nendobj\n").as_bytes());
    }

    out.extend_from_slice(b"%PDF-1.4\n% raster-studio print job\n");

    obj(
        &mut out,
        &mut offsets,
        1,
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
    );
    obj(
        &mut out,
        &mut offsets,
        2,
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
    );
    obj(
        &mut out,
        &mut offsets,
        3,
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] \
             /Contents 4 0 R /Resources << /XObject << /Im0 5 0 R >> >> >>",
            num(page.width_pt),
            num(page.height_pt)
        ),
    );
    let (x, y, w, h) = page.placement(width, height);
    let content = format!(
        "q\n{} 0 0 {} {} {} cm\n/Im0 Do\nQ\n",
        num(w),
        num(h),
        num(x),
        num(y)
    );
    obj(
        &mut out,
        &mut offsets,
        4,
        format!(
            "<< /Length {} >>\nstream\n{}endstream",
            content.len(),
            content
        ),
    );
    let image_head = format!(
        "5 0 obj\n<< /Type /XObject /Subtype /Image /Width {width} /Height {height} \
         /ColorSpace /DeviceRGB /BitsPerComponent 8 \
         /Filter /FlateDecode /Length {} >>\nstream\n",
        compressed.len()
    );
    offsets[5] = out.len() as u64;
    out.extend_from_slice(image_head.as_bytes());
    out.extend_from_slice(&compressed);
    out.extend_from_slice(b"\nendstream\nendobj\n");
    if let Some(dictionary) = &info {
        obj(&mut out, &mut offsets, 6, dictionary.clone());
    }

    let xref_off = out.len() as u64;
    out.extend_from_slice(format!("xref\n0 {objects}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for off in offsets.iter().skip(1) {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    let info_ref = if info.is_some() { " /Info 6 0 R" } else { "" };
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {objects} /Root 1 0 R{info_ref} >>\nstartxref\n{xref_off}\n%%EOF\n"
        )
        .as_bytes(),
    );
    out
}

/// Composite RGBA onto an opaque white background, returning `width*height*3`
/// bytes of `DeviceRGB`.
fn composite_onto_white(rgba: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(w * h * 3);
    for px in rgba.as_chunks::<4>().0.iter().take(w * h) {
        let a = px[3] as f32 / 255.0;
        let blend = |c: u8| (c as f32 * a + 255.0 * (1.0 - a)).round().clamp(0.0, 255.0) as u8;
        rgb.push(blend(px[0]));
        rgb.push(blend(px[1]));
        rgb.push(blend(px[2]));
    }
    rgb
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pdf_has_a_header_trailer_and_startxref() {
        let rgba = vec![255u8; 4 * 4 * 4];
        let pdf = encode_pdf(4, 4, &rgba);
        assert!(pdf.starts_with(b"%PDF-1.4"), "PDF header");
        assert!(find(&pdf, b"FlateDecode"), "image is deflate-filtered");
        assert!(pdf.ends_with(b"%%EOF\n"), "trailer");
        let start = find_off(&pdf, b"startxref") + "startxref".len();
        let num_at = start
            + pdf[start..]
                .iter()
                .take_while(|&&c| c.is_ascii_whitespace())
                .count();
        let num_end = num_at
            + pdf[num_at..]
                .iter()
                .take_while(|&&c| c.is_ascii_digit())
                .count();
        let xref: u64 = std::str::from_utf8(&pdf[num_at..num_end])
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(
            &pdf[xref as usize..xref as usize + 4],
            b"xref",
            "xref offset"
        );
    }

    #[test]
    fn every_object_offset_in_the_xref_is_in_bounds() {
        let rgba = vec![128u8; 40 * 30 * 4];
        let pdf = encode_pdf(40, 30, &rgba);
        let needle = b"\nxref\n0 6\n";
        let xref_at = find_off(&pdf, needle) + needle.len();
        let mut offs = Vec::new();
        for _ in 0..6 {
            let line = &pdf[xref_at + offs.len() * 20..];
            let off: u16 = std::str::from_utf8(&line[..10]).unwrap().parse().unwrap();
            offs.push(off as usize);
        }
        assert_eq!(offs[0], 0, "object 0 is the free head");
        for n in offs[1..].iter().enumerate() {
            let (idx, &off) = n;
            let objn = idx + 1;
            assert!(off < pdf.len(), "object {objn} offset in bounds");
            let head = std::str::from_utf8(&pdf[off..off + 20]).unwrap();
            assert!(
                head.starts_with(&format!("{objn} 0 obj")),
                "xref offset {off} lands on object {objn}: {head:?}"
            );
        }
    }

    #[test]
    fn rgba_is_composited_onto_white_without_alpha_in_the_image() {
        let rgba = [0u8, 0, 0, 0, 255, 0, 0, 255];
        let rgb = composite_onto_white(&rgba, 1, 2);
        assert_eq!(&rgb[0..3], &[255, 255, 255], "transparent -> white");
        assert_eq!(&rgb[3..6], &[255, 0, 0], "opaque red stays red");
    }

    /// W10-E: a structural parse of what [`encode_pdf_on_page`] writes —
    /// every xref row lands on its object, the trailer names the catalog and
    /// the info dictionary, the media box is the chosen page, the content
    /// stream places the image where [`PdfPage::placement`] says, and the
    /// image stream inflates to exactly the RGB samples.
    #[test]
    fn a_pdf_on_an_a4_page_with_info_parses_back_to_its_page_image_and_info() {
        use std::io::Read as _;
        let (w, h) = (30u32, 20u32);
        let rgba: Vec<u8> = (0..w * h).flat_map(|i| [i as u8, 50, 90, 255]).collect();
        let info = PdfInfo {
            title: "Harbour (draft)".into(),
            author: "Ana".into(),
            subject: String::new(),
            keywords: "sea, boats".into(),
        };
        let pdf = encode_pdf_on_page(w, h, &rgba, PdfPage::A4, Some(&info));
        let text = String::from_utf8_lossy(&pdf).into_owned();
        // The xref: 7 rows, each landing on its object.
        let xref_at = find_off(&pdf, b"\nxref\n0 7\n") + b"\nxref\n0 7\n".len();
        for n in 1..7usize {
            let row = &pdf[xref_at + n * 20..xref_at + n * 20 + 10];
            let off: usize = std::str::from_utf8(row).unwrap().parse().unwrap();
            assert!(
                pdf[off..].starts_with(format!("{n} 0 obj").as_bytes()),
                "xref row {n} lands on its object"
            );
        }
        assert!(text.contains("/Size 7 /Root 1 0 R /Info 6 0 R"), "{text}");
        assert!(text.contains("/Title (Harbour \\(draft\\))"), "{text}");
        assert!(text.contains("/Author (Ana)"));
        assert!(!text.contains("/Subject"), "a blank field is left out");
        assert!(text.contains("/MediaBox [0 0 595.276 841.890]"), "{text}");
        let (x, y, pw, ph) = PdfPage::A4.placement(w, h);
        assert!((pw / ph - 1.5).abs() < 1e-9, "the aspect is kept");
        assert!((pw - 595.276).abs() < 1e-9, "a wide image fills the width");
        assert!(x.abs() < 1e-9 && (y - (841.89 - ph) / 2.0).abs() < 1e-9);
        assert!(text.contains(&format!(
            "{} 0 0 {} {} {} cm",
            num(pw),
            num(ph),
            num(x),
            num(y)
        )));
        // The image stream inflates to the RGB samples.
        let head = find_off(&pdf, b"/Filter /FlateDecode /Length ");
        let len_at = head + b"/Filter /FlateDecode /Length ".len();
        let len_end = len_at + pdf[len_at..].iter().position(|&c| c == b' ').unwrap();
        let len: usize = std::str::from_utf8(&pdf[len_at..len_end])
            .unwrap()
            .parse()
            .unwrap();
        let data_at = find_off(&pdf[head..], b"stream\n") + head + b"stream\n".len();
        let mut rgb = Vec::new();
        flate2::read::ZlibDecoder::new(&pdf[data_at..data_at + len])
            .read_to_end(&mut rgb)
            .unwrap();
        assert_eq!(rgb.len(), (w * h * 3) as usize);
        assert_eq!(&rgb[..3], &[0, 50, 90]);
        // Non-ASCII text goes out as UTF-16BE.
        assert_eq!(pdf_text("é"), "<FEFF00E9>");
    }

    #[test]
    fn the_raster_sized_pdf_is_unchanged_by_the_page_variant() {
        let rgba = vec![200u8; 3 * 2 * 4];
        let pdf = String::from_utf8_lossy(&encode_pdf(3, 2, &rgba)).into_owned();
        assert!(pdf.contains("/MediaBox [0 0 3 2]"));
        assert!(pdf.contains("q\n3 0 0 2 0 0 cm\n"));
        assert!(pdf.contains("/Size 6 /Root 1 0 R >>"));
        let at = PdfPage::at_ppi(300, 150, 300.0);
        assert_eq!((at.width_pt, at.height_pt), (72.0, 36.0));
    }

    fn find(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }
    fn find_off(hay: &[u8], needle: &[u8]) -> usize {
        hay.windows(needle.len())
            .position(|w| w == needle)
            .unwrap_or_else(|| panic!("needle not found"))
    }
}
