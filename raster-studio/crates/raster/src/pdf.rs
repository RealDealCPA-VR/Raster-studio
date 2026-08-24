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
    let rgb = composite_onto_white(rgba, width as usize, height as usize);
    let mut compressed = Vec::new();
    {
        use std::io::Write;
        let mut e = ZlibEncoder::new(&mut compressed, Compression::default());
        e.write_all(&rgb).expect("in-memory deflate cannot fail");
        e.finish().expect("in-memory deflate cannot fail");
    }

    let mut out: Vec<u8> = Vec::new();
    let mut offsets = vec![0u64; 6];
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
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width} {height}] \
             /Contents 4 0 R /Resources << /XObject << /Im0 5 0 R >> >> >>"
        ),
    );
    let content = format!("q\n{width} 0 0 {height} 0 0 cm\n/Im0 Do\nQ\n");
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

    let xref_off = out.len() as u64;
    out.extend_from_slice(b"xref\n0 6\n");
    out.extend_from_slice(b"0000000000 65535 f \n");
    for off in offsets.iter().skip(1) {
        out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_off}\n%%EOF\n").as_bytes(),
    );
    out
}

/// Composite RGBA onto an opaque white background, returning `width*height*3`
/// bytes of `DeviceRGB`.
fn composite_onto_white(rgba: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(w * h * 3);
    for px in rgba.chunks_exact(4).take(w * h) {
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

    fn find(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }
    fn find_off(hay: &[u8], needle: &[u8]) -> usize {
        hay.windows(needle.len())
            .position(|w| w == needle)
            .unwrap_or_else(|| panic!("needle not found"))
    }
}
