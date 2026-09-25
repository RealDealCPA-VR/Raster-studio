//! W13-D: PDF, and Adobe Illustrator `.ai` files saved PDF-compatible, read
//! by rendering their pages with `hayro`.
//!
//! `hayro` 0.4 is a pure-Rust PDF rasteriser (Apache-2.0, the crate itself
//! `#![forbid(unsafe_code)]`) with its own CPU rasteriser: it interprets the
//! page's content stream (paths, fills, strokes, clipping, shadings, images,
//! text in embedded fonts and in the standard 14 through its bundled Foxit
//! fonts) and paints it **on white paper**, as a PDF viewer shows a page, so
//! a page opens opaque. What it does not do, by its own documentation:
//! encrypted / password-protected files (refused here by name), blending
//! and isolation, knockout groups, colour-key masking; and JPEG 2000 images,
//! whose decoder is an optional OpenJPEG (C) feature this build leaves off.
//! A page that uses those opens, drawn as far as `hayro` draws it.
//!
//! Why 0.4 and not a newer release: 0.5 turns on `flate2`'s `zlib-rs`
//! backend, which Cargo's feature unification would swap in for every
//! `flate2` user in the workspace, and 0.6+ needs rustc 1.92, past the
//! workspace MSRV.
//!
//! # Size
//!
//! A page renders at [`PIXELS_PER_POINT`] pixels per PDF point, so a Letter
//! page (612 x 792 pt) opens 612 x 792 px, as an SVG opens at its own size.
//! The size is checked against the caller's [`ImportLimits`] before a pixel
//! buffer exists.
//!
//! # Pages
//!
//! [`decode`] (the flat codec's road) answers the **first** page.
//! [`render_pages`] renders up to [`MAX_PAGES`] of them, which is what
//! `app-shell` opens as one artboard per page.
//!
//! # Untrusted input
//!
//! The file is parsed by `hayro-syntax`, which reads objects lazily and
//! answers `None` for what it cannot parse. A panic inside the third-party
//! parser or renderer is caught and reported as an error in builds that
//! unwind (tests, debug); the release profile aborts on panic, so there the
//! guard is not a guarantee. The render itself has no time bound: a
//! pathological content stream is as slow as `hayro` makes it.

use std::sync::Arc;

use hayro::{InterpreterSettings, Pdf, RenderSettings};
use hayro_syntax::LoadPdfError;

use super::{check_decode, info, malformed, rgba8_surface};
use crate::codec::{CodecError, DecodedSurface, ImageInfo, ImportFormat, ImportLimits};

const NAME: &str = "PDF";

/// Pixels per PDF point (1/72 inch): a page opens at its own size in points.
pub const PIXELS_PER_POINT: f32 = 1.0;

/// The most pages [`render_pages`] renders; a longer file opens its first
/// `MAX_PAGES` and says so (see [`PdfPages::total`]).
pub const MAX_PAGES: usize = 100;

/// `true` when `head` holds the `%PDF-` marker. The specification lets it
/// sit anywhere in the first 1024 bytes; this looks at what it is given.
pub fn looks_like_pdf(head: &[u8]) -> bool {
    head.windows(5).any(|w| w == b"%PDF-")
}

/// Run `f`, turning a panic in the third-party parser or renderer into an
/// error (only where the build unwinds, see the module docs).
fn guarded<T>(f: impl FnOnce() -> Result<T, CodecError>) -> Result<T, CodecError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .unwrap_or_else(|_| Err(malformed(NAME, "the PDF renderer failed on this file")))
}

fn load(bytes: &[u8]) -> Result<Pdf, CodecError> {
    if !looks_like_pdf(&bytes[..bytes.len().min(1024)]) {
        return Err(malformed(NAME, "no %PDF- header"));
    }
    let data: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new(bytes.to_vec());
    Pdf::new(data).map_err(|e| match e {
        LoadPdfError::Decryption(_) => CodecError::Unsupported(
            "this PDF is encrypted (password-protected); this build cannot open it".into(),
        ),
        LoadPdfError::Invalid => {
            malformed(NAME, "the cross-reference table or trailer is unreadable")
        }
    })
}

/// The pixel size page `page`'s render has, checked against `limits`.
fn pixel_size(pdf: &Pdf, page: usize, limits: ImportLimits) -> Result<(u16, u16), CodecError> {
    let pages = pdf.pages();
    let page = pages
        .get(page)
        .ok_or_else(|| malformed(NAME, format!("there is no page {}", page + 1)))?;
    let (w, h) = page.render_dimensions();
    let side = |v: f32| -> Result<u32, CodecError> {
        let v = (v * PIXELS_PER_POINT).floor();
        if !v.is_finite() || v < 1.0 {
            return Err(malformed(NAME, "a page has no area"));
        }
        Ok(v.min(u32::MAX as f32) as u32)
    };
    let (width, height) = (side(w)?, side(h)?);
    // The renderer's canvas is `u16` a side.
    if width > u32::from(u16::MAX) || height > u32::from(u16::MAX) {
        return Err(CodecError::LimitExceeded(format!(
            "a {width}x{height} page is larger than the PDF renderer draws (65535 a side)"
        )));
    }
    // The premultiplied canvas, its paint buffers and the straight copy.
    check_decode(
        limits,
        width,
        height,
        4,
        u64::from(width) * u64::from(height) * 8,
    )?;
    Ok((width as u16, height as u16))
}

fn render(pdf: &Pdf, index: usize, limits: ImportLimits) -> Result<DecodedSurface, CodecError> {
    let (width, height) = pixel_size(pdf, index, limits)?;
    let pages = pdf.pages();
    let page = &pages[index];
    let pixmap = guarded(|| {
        Ok(hayro::render(
            page,
            &InterpreterSettings::default(),
            &RenderSettings {
                x_scale: PIXELS_PER_POINT,
                y_scale: PIXELS_PER_POINT,
                width: Some(width),
                height: Some(height),
            },
        ))
    })?;
    // Premultiplied RGBA8 out of the renderer, straight alpha in.
    let mut rgba = pixmap.take_u8();
    for px in rgba.as_chunks_mut::<4>().0 {
        let a = u32::from(px[3]);
        if a != 0 && a != 255 {
            for c in &mut px[..3] {
                *c = ((u32::from(*c) * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    Ok(rgba8_surface(
        u32::from(width),
        u32::from(height),
        rgba,
        ImportFormat::Pdf,
    ))
}

/// How many pages the file has.
pub fn page_count(bytes: &[u8]) -> Result<usize, CodecError> {
    guarded(|| Ok(load(bytes)?.pages().len()))
}

/// Page `index` (from 0), rendered.
pub fn render_page(
    bytes: &[u8],
    index: usize,
    limits: ImportLimits,
) -> Result<DecodedSurface, CodecError> {
    guarded(|| render(&load(bytes)?, index, limits))
}

/// The pages [`render_pages`] rendered, and how many the file has.
#[derive(Debug)]
pub struct PdfPages {
    /// The first `min(total, MAX_PAGES)` pages, in order.
    pub pages: Vec<DecodedSurface>,
    /// Every page the file has; more than `pages.len()` when it was cut at
    /// [`MAX_PAGES`].
    pub total: usize,
}

/// Every page up to [`MAX_PAGES`], rendered. Each page is checked against
/// `limits` on its own, and all of them together against its allocation
/// ceiling, since they are held at once.
pub fn render_pages(bytes: &[u8], limits: ImportLimits) -> Result<PdfPages, CodecError> {
    guarded(|| {
        let pdf = load(bytes)?;
        let total = pdf.pages().len();
        if total == 0 {
            return Err(malformed(NAME, "the document has no pages"));
        }
        let mut held = 0u64;
        let mut pages = Vec::new();
        for index in 0..total.min(MAX_PAGES) {
            let (w, h) = pixel_size(&pdf, index, limits)?;
            held = held.saturating_add(u64::from(w) * u64::from(h) * 4);
            limits.check_alloc(held)?;
            pages.push(render(&pdf, index, limits)?);
        }
        Ok(PdfPages { pages, total })
    })
}

/// Header facts: the first page's size.
pub(super) fn probe(bytes: &[u8], limits: ImportLimits) -> Result<ImageInfo, CodecError> {
    guarded(|| {
        let pdf = load(bytes)?;
        if pdf.pages().is_empty() {
            return Err(malformed(NAME, "the document has no pages"));
        }
        let (w, h) = pixel_size(&pdf, 0, limits)?;
        Ok(info(u32::from(w), u32::from(h), ImportFormat::Pdf, false))
    })
}

/// The flat codec's decode: the first page.
pub(super) fn decode(bytes: &[u8], limits: ImportLimits) -> Result<DecodedSurface, CodecError> {
    guarded(|| {
        let pdf = load(bytes)?;
        if pdf.pages().is_empty() {
            return Err(malformed(NAME, "the document has no pages"));
        }
        render(&pdf, 0, limits)
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::codec::{decode_surface_bytes, probe_bytes, SurfacePixels};

    /// A well-formed PDF with one page per `(width, height, content)`, the
    /// content stream written as given. The cross-reference table carries
    /// the real offsets.
    pub(crate) fn build_pdf(pages: &[(u32, u32, &str)]) -> Vec<u8> {
        let mut objects: Vec<String> = Vec::new();
        let n = pages.len();
        // 1: catalog, 2: page tree, then (page, content) pairs.
        objects.push("<< /Type /Catalog /Pages 2 0 R >>".into());
        let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 3 + i * 2)).collect();
        objects.push(format!(
            "<< /Type /Pages /Kids [{}] /Count {n} >>",
            kids.join(" ")
        ));
        for (i, (w, h, content)) in pages.iter().enumerate() {
            objects.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Contents {} 0 R >>",
                4 + i * 2
            ));
            objects.push(format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len() + 1
            ));
        }
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for o in offsets {
            out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    fn px(s: &DecodedSurface, x: u32, y: u32) -> [u8; 4] {
        let SurfacePixels::Rgba8(p) = &s.pixels else {
            panic!("8-bit expected")
        };
        let i = ((y * s.width + x) * 4) as usize;
        [p[i], p[i + 1], p[i + 2], p[i + 3]]
    }

    #[test]
    fn a_page_of_paths_renders_fills_and_strokes_at_its_own_size() {
        // A red filled square in the lower-left quarter (PDF y runs up), a
        // blue stroke across the top, the rest of the page unpainted paper.
        let pdf = build_pdf(&[(
            40,
            30,
            "1 0 0 rg 0 0 20 15 re f\n0 0 1 RG 4 w 0 26 m 40 26 l S",
        )]);
        let s = decode_surface_bytes(&pdf, ImportLimits::default()).unwrap();
        assert_eq!(
            (s.width, s.height, s.source_format),
            (40, 30, ImportFormat::Pdf)
        );
        // Lower-left in PDF space is lower-left on screen: row 25 of 30.
        assert_eq!(px(&s, 5, 25), [255, 0, 0, 255], "the red fill");
        let stroke = px(&s, 20, 4);
        assert!(
            stroke[2] > 200 && stroke[0] < 40 && stroke[3] > 200,
            "{stroke:?}"
        );
        assert_eq!(
            px(&s, 35, 20),
            [255, 255, 255, 255],
            "unpainted paper is white"
        );
        let info = probe_bytes(&pdf, ImportLimits::default()).unwrap();
        assert_eq!(
            (info.width, info.height, info.format),
            (40, 30, ImportFormat::Pdf)
        );
    }

    #[test]
    fn every_page_renders_and_the_count_is_the_files() {
        let pdf = build_pdf(&[
            (10, 10, "0 1 0 rg 0 0 10 10 re f"),
            (20, 8, "0 0 1 rg 0 0 20 8 re f"),
            (6, 6, "1 1 0 rg 0 0 6 6 re f"),
        ]);
        assert_eq!(page_count(&pdf).unwrap(), 3);
        let all = render_pages(&pdf, ImportLimits::default()).unwrap();
        assert_eq!(all.total, 3);
        let sizes: Vec<_> = all.pages.iter().map(|p| (p.width, p.height)).collect();
        assert_eq!(sizes, vec![(10, 10), (20, 8), (6, 6)]);
        assert_eq!(px(&all.pages[0], 5, 5), [0, 255, 0, 255]);
        assert_eq!(px(&all.pages[1], 5, 5), [0, 0, 255, 255]);
        assert_eq!(px(&all.pages[2], 3, 3), [255, 255, 0, 255]);
        let second = render_page(&pdf, 1, ImportLimits::default()).unwrap();
        assert_eq!((second.width, second.height), (20, 8));
        assert!(render_page(&pdf, 3, ImportLimits::default()).is_err());
    }

    #[test]
    fn an_image_pdf_from_the_print_encoder_opens() {
        // The app's own PDF writer: an image XObject, FlateDecode, on white.
        let rgba: Vec<u8> = (0..4 * 3).flat_map(|_| [10u8, 200, 30, 255]).collect();
        let pdf = crate::pdf::encode_pdf(4, 3, &rgba);
        let s = decode_surface_bytes(&pdf, ImportLimits::default()).unwrap();
        assert_eq!((s.width, s.height), (4, 3));
        let c = px(&s, 2, 1);
        assert!(
            c[0].abs_diff(10) < 8 && c[1].abs_diff(200) < 8 && c[2].abs_diff(30) < 8,
            "{c:?}"
        );
    }

    #[test]
    fn malformed_and_oversized_pdfs_error_and_never_panic() {
        let limits = ImportLimits::default();
        for bad in [
            b"%PDF-1.4\n".to_vec(),
            b"%PDF-1.7\n1 0 obj << /Type /Catalog >> endobj\ntrailer << >>\n%%EOF".to_vec(),
            b"not a pdf at all".to_vec(),
            {
                let mut v = build_pdf(&[(10, 10, "0 0 10 10 re f")]);
                v.truncate(v.len() / 2);
                v
            },
        ] {
            // Whatever the file is, the answer is a value, not a crash; a
            // truncated file hayro repairs may still render.
            let _ = decode_surface_bytes(&bad, limits);
            let _ = render_pages(&bad, limits);
        }
        assert!(decode(b"not a pdf at all", limits).is_err());
        assert!(render_pages(b"%PDF-1.4\n", limits).is_err());
        // A page too large for the limits is refused before it is drawn.
        let huge = build_pdf(&[(60000, 60000, "0 0 1 1 re f")]);
        let err = decode_surface_bytes(&huge, limits).unwrap_err();
        assert!(matches!(err, CodecError::LimitExceeded(_)), "{err}");
        // Every prefix of a real file is survived.
        let pdf = build_pdf(&[(8, 8, "1 0 0 rg 0 0 8 8 re f")]);
        for cut in (0..pdf.len()).step_by(7) {
            let _ = decode(&pdf[..cut], limits);
        }
    }
}
