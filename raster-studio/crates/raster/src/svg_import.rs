//! W9-N: File > Open rasterises an SVG.
//!
//! A child module of [`crate::codec`] (declared there with `#[path]`), so the
//! codec facade stays the one place the application talks to a third-party
//! decoder. The SVG is parsed by `usvg` and drawn by `resvg` (both pure Rust,
//! Apache-2.0 OR MIT) at the document's own size — its `width`/`height`, or
//! its `viewBox` when those are absent, rounded up to whole pixels — onto a
//! transparent canvas, then handed on as straight-alpha sRGB RGBA8 like any
//! other flat import.
//!
//! # Untrusted input
//!
//! * The file is read whole but bounded: [`MAX_SVG_BYTES`] is checked before
//!   the read can grow past it.
//! * A gzip-compressed SVG (`.svgz`, a head of `1f 8b`) is inflated here,
//!   through a reader capped at [`MAX_SVG_BYTES`] of *output*, and the text
//!   handed to `usvg::Tree::from_str`. `usvg::Tree::from_data` is never
//!   called: it inflates with an unbounded `read_to_end`, so a 100 KB file
//!   could expand to gigabytes before any check ran.
//! * An SVG embedded as a `data:` image is parsed by `usvg` itself (through
//!   `from_data`), so a gzip-compressed embedded SVG is refused outright (not
//!   drawn) for the same reason.
//! * The rendered size goes through the same [`ImportLimits`] dimension and
//!   allocation checks every raster decode does, **before** the pixmap is
//!   allocated, so `<svg width="100000000">` fails fast.
//! * `<image href="…">` is resolved for `data:` URLs only. A plain path or
//!   URL is refused (the image is simply not drawn): an SVG someone sent must
//!   not be able to read another file off this disk into the picture.
//! * A panic inside the parser or renderer is caught and reported as a
//!   decode error rather than taking the application down.

use std::io::{BufRead, Read, Seek};
use std::sync::{Arc, OnceLock};

use color::ColorSpace;
use resvg::{tiny_skia, usvg};

use super::{read_head, CodecError, DecodedSurface, ImageInfo, ImportFormat, ImportLimits};
use super::{PixelFormat, SurfacePixels};

/// Largest SVG file the importer reads: 64 MiB. Real SVGs are kilobytes to a
/// few megabytes; one with large embedded rasters can be tens.
pub const MAX_SVG_BYTES: u64 = 64 << 20;

/// The two bytes every gzip stream starts with.
const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// How much of the head the content sniff looks at.
const SNIFF_BYTES: usize = 4096;

/// `true` when `head` (the first bytes of a file) is an SVG document: after
/// an optional UTF-8 byte-order mark and whitespace it opens with markup, and
/// an `<svg` element starts within the sniffed window.
pub fn looks_like_svg(head: &[u8]) -> bool {
    let head = head.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(head);
    let Some(first) = head.iter().position(|b| !b.is_ascii_whitespace()) else {
        return false;
    };
    head[first] == b'<' && head.windows(4).any(|w| w == b"<svg")
}

/// Whether `source` is an SVG, by content — or by the caller's hint. The
/// stream is left where it was found.
pub(super) fn is_svg_source<R: BufRead + Seek>(
    source: &mut R,
    hint: Option<ImportFormat>,
) -> Result<bool, CodecError> {
    if hint == Some(ImportFormat::Svg) {
        return Ok(true);
    }
    let start = source.stream_position()?;
    let mut head = vec![0u8; SNIFF_BYTES];
    let filled = read_head(source, &mut head)?;
    source.seek(std::io::SeekFrom::Start(start))?;
    Ok(looks_like_svg(&head[..filled]))
}

/// Read the rest of `source`, refusing past [`MAX_SVG_BYTES`].
fn read_bounded<R: Read>(source: R) -> Result<Vec<u8>, CodecError> {
    let mut data = Vec::new();
    source.take(MAX_SVG_BYTES + 1).read_to_end(&mut data)?;
    if data.len() as u64 > MAX_SVG_BYTES {
        return Err(CodecError::LimitExceeded(format!(
            "the SVG is larger than {MAX_SVG_BYTES} bytes"
        )));
    }
    Ok(data)
}

/// The system fonts, loaded once per process: `<text>` in an SVG draws with
/// what this machine has installed, as a browser would.
fn fonts() -> Arc<usvg::fontdb::Database> {
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    FONTS
        .get_or_init(|| {
            let mut db = usvg::fontdb::Database::new();
            db.load_system_fonts();
            Arc::new(db)
        })
        .clone()
}

/// Inflate a gzip-compressed SVG, refusing once the *output* passes
/// [`MAX_SVG_BYTES`] — the cap is on what is produced, so a small file that
/// expands a thousandfold stops at the cap instead of exhausting memory.
fn inflate_bounded(data: &[u8]) -> Result<Vec<u8>, CodecError> {
    let mut inflated = Vec::new();
    flate2::read::GzDecoder::new(data)
        .take(MAX_SVG_BYTES + 1)
        .read_to_end(&mut inflated)
        .map_err(|e| CodecError::Unsupported(format!("not a readable compressed SVG: {e}")))?;
    if inflated.len() as u64 > MAX_SVG_BYTES {
        return Err(CodecError::LimitExceeded(format!(
            "the compressed SVG expands past {MAX_SVG_BYTES} bytes"
        )));
    }
    Ok(inflated)
}

/// Parse options: system fonts, and `data:` images only — and of those, no
/// gzip-compressed embedded SVG (`usvg` would inflate it unbounded).
fn options() -> usvg::Options<'static> {
    let data_resolver = usvg::ImageHrefResolver::default_data_resolver();
    usvg::Options {
        fontdb: fonts(),
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(move |mime, data, opts| {
                if data.starts_with(&GZIP_MAGIC) {
                    return None;
                }
                data_resolver(mime, data, opts)
            }),
            resolve_string: Box::new(|_, _| None),
        },
        ..usvg::Options::default()
    }
}

/// Run `f`, turning a panic in the third-party parser or renderer into an
/// error.
fn guarded<T>(what: &str, f: impl FnOnce() -> Result<T, CodecError>) -> Result<T, CodecError> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| {
        Err(CodecError::Unsupported(format!(
            "the SVG {what} failed on this file"
        )))
    })
}

fn parse(data: &[u8]) -> Result<usvg::Tree, CodecError> {
    let inflated;
    let data = if data.starts_with(&GZIP_MAGIC) {
        inflated = inflate_bounded(data)?;
        &inflated[..]
    } else {
        data
    };
    let text = std::str::from_utf8(data)
        .map_err(|_| CodecError::Unsupported("not a readable SVG: not UTF-8 text".into()))?;
    guarded("parser", || {
        // `from_str`, never `from_data`: see the module docs on gzip.
        usvg::Tree::from_str(text, &options())
            .map_err(|e| CodecError::Unsupported(format!("not a readable SVG: {e}")))
    })
}

/// The whole-pixel size an SVG renders at: its own size, rounded up.
fn pixel_size(tree: &usvg::Tree, limits: ImportLimits) -> Result<(u32, u32), CodecError> {
    let size = tree.size();
    let side = |v: f32| -> u32 {
        if v.is_finite() && v > 0.0 {
            v.ceil().min(u32::MAX as f32) as u32
        } else {
            0
        }
    };
    let (width, height) = (side(size.width()), side(size.height()));
    limits.check_dimensions(width, height)?;
    // The pixmap (premultiplied) and the straight-alpha copy are both live.
    limits.check_alloc(u64::from(width) * u64::from(height) * 8)?;
    Ok((width, height))
}

/// Probe an SVG: its rendered size, without rendering.
pub(super) fn probe_svg<R: Read>(source: R, limits: ImportLimits) -> Result<ImageInfo, CodecError> {
    let tree = parse(&read_bounded(source)?)?;
    let (width, height) = pixel_size(&tree, limits)?;
    Ok(ImageInfo {
        width,
        height,
        format: ImportFormat::Svg,
        pixel_format: PixelFormat::Rgba8,
        icc_profile: None,
    })
}

/// Rasterise an SVG at its own size into straight-alpha sRGB RGBA8.
pub(super) fn decode_svg<R: Read>(
    source: R,
    limits: ImportLimits,
) -> Result<DecodedSurface, CodecError> {
    let data = read_bounded(source)?;
    rasterize(&data, limits)
}

/// [`decode_svg`] over bytes already in memory.
pub fn rasterize(data: &[u8], limits: ImportLimits) -> Result<DecodedSurface, CodecError> {
    let tree = parse(data)?;
    let (width, height) = pixel_size(&tree, limits)?;
    let rgba8 = guarded("renderer", || {
        let mut pixmap = tiny_skia::Pixmap::new(width, height).ok_or_else(|| {
            CodecError::LimitExceeded(format!("cannot allocate a {width}x{height} canvas"))
        })?;
        resvg::render(
            &tree,
            tiny_skia::Transform::identity(),
            &mut pixmap.as_mut(),
        );
        let mut rgba8 = Vec::with_capacity(pixmap.pixels().len() * 4);
        for px in pixmap.pixels() {
            let c = px.demultiply();
            rgba8.extend_from_slice(&[c.red(), c.green(), c.blue(), c.alpha()]);
        }
        Ok(rgba8)
    })?;
    Ok(DecodedSurface {
        width,
        height,
        pixels: SurfacePixels::Rgba8(rgba8),
        color_space: ColorSpace::Srgb,
        icc_profile: None,
        source_format: ImportFormat::Svg,
    })
}

/// W16-I: File > Open reads an SVG (and the EPS / PDF display lists) as
/// layers: groups, shape layers, text layers and raster images.
#[path = "svg_layers_w16.rs"]
pub mod layers;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::{decode_bytes, decode_surface_bytes, probe_bytes};

    const RED_SQUARE: &str = r##"<?xml version="1.0"?>
<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10" viewBox="0 0 20 10">
  <rect x="0" y="0" width="10" height="10" fill="#ff0000"/>
</svg>"##;

    fn px(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * width + x) * 4) as usize;
        [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
    }

    #[test]
    fn an_svg_decodes_through_the_flat_codec_at_its_own_size() {
        // `decode_bytes` is what File > Open's import worker calls; it has
        // no extension to go on, so the SVG is recognised by content.
        let image = decode_bytes(RED_SQUARE.as_bytes()).expect("an SVG decodes");
        assert_eq!((image.width, image.height), (20, 10));
        assert_eq!(px(&image.rgba8, 20, 2, 5), [255, 0, 0, 255], "the rect");
        assert_eq!(px(&image.rgba8, 20, 15, 5)[3], 0, "outside is transparent");
        let surface = decode_surface_bytes(RED_SQUARE.as_bytes(), ImportLimits::default()).unwrap();
        assert_eq!(surface.source_format, ImportFormat::Svg);
        assert_eq!(surface.color_space, ColorSpace::Srgb);
        let info = probe_bytes(RED_SQUARE.as_bytes(), ImportLimits::default()).unwrap();
        assert_eq!(
            (info.width, info.height, info.format),
            (20, 10, ImportFormat::Svg)
        );
    }

    #[test]
    fn a_view_box_only_svg_takes_the_view_box_size() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 7.5 3"><rect width="7.5" height="3"/></svg>"#;
        let image = decode_bytes(svg.as_bytes()).unwrap();
        assert_eq!((image.width, image.height), (8, 3), "rounded up");
    }

    #[test]
    fn the_extension_names_svg() {
        assert_eq!(ImportFormat::from_extension("SVG"), Some(ImportFormat::Svg));
        assert!(ImportFormat::ALL.contains(&ImportFormat::Svg));
        assert!(ImportFormat::Svg.is_decodable_here());
    }

    #[test]
    fn malformed_and_hostile_svgs_error_and_never_panic() {
        let limits = ImportLimits::default();
        let cases: &[(&str, &[u8])] = &[
            ("unclosed", b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"4\" height=\"4\">"),
            ("not xml", b"<svg <<<< >>>>"),
            ("zero size", br#"<svg xmlns="http://www.w3.org/2000/svg" width="0" height="0"/>"#),
            ("negative size", br#"<svg xmlns="http://www.w3.org/2000/svg" width="-5" height="5"/>"#),
            (
                "huge size",
                br#"<svg xmlns="http://www.w3.org/2000/svg" width="100000000" height="100000000"/>"#,
            ),
            ("empty", b""),
            ("bom only", b"\xEF\xBB\xBF"),
        ];
        for (name, bytes) in cases {
            let decoded = std::panic::catch_unwind(|| {
                super::super::decode_surface_bytes_as(bytes, limits, ImportFormat::Svg)
            });
            let decoded = decoded.unwrap_or_else(|_| panic!("{name}: panicked"));
            assert!(decoded.is_err(), "{name}: decoded {decoded:?}");
        }
        // Truncations of a good file at every byte: an error or a picture,
        // never a panic.
        for cut in 0..RED_SQUARE.len() {
            let _ = super::super::decode_surface_bytes_as(
                &RED_SQUARE.as_bytes()[..cut],
                limits,
                ImportFormat::Svg,
            );
        }
    }

    #[test]
    fn an_image_href_to_a_local_file_is_not_read() {
        let png_path =
            std::env::temp_dir().join(format!("w9n-svg-secret-{}.png", std::process::id()));
        let red = [255u8, 0, 0, 255].repeat(4);
        std::fs::write(
            &png_path,
            crate::codec::encode(crate::codec::ExportFormat::Png, 2, 2, &red).unwrap(),
        )
        .unwrap();
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="2" height="2"><image width="2" height="2" xlink:href="{}"/></svg>"#,
            png_path.display().to_string().replace('\\', "/")
        );
        let image = decode_bytes(svg.as_bytes()).unwrap();
        let _ = std::fs::remove_file(&png_path);
        assert!(
            image.rgba8.chunks(4).all(|p| p[3] == 0),
            "the local file was drawn into the picture"
        );
    }

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        enc.write_all(bytes).unwrap();
        enc.finish().unwrap()
    }

    #[test]
    fn a_small_svgz_still_opens() {
        let image = super::super::decode_surface_bytes_as(
            &gzip(RED_SQUARE.as_bytes()),
            ImportLimits::default(),
            ImportFormat::Svg,
        )
        .expect("a gzip-compressed SVG decodes");
        assert_eq!((image.width, image.height), (20, 10));
    }

    #[test]
    fn a_gzip_bomb_stops_at_the_output_cap() {
        // A valid SVG whose comment pads the inflated text past the cap:
        // compressed it is about a thousandth of that.
        let mut text = Vec::with_capacity(MAX_SVG_BYTES as usize + 256);
        text.extend_from_slice(
            br#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"><!--"#,
        );
        text.resize(MAX_SVG_BYTES as usize + 16, b' ');
        text.extend_from_slice(b"--></svg>");
        let bomb = gzip(&text);
        drop(text);
        assert!(
            (bomb.len() as u64) < MAX_SVG_BYTES / 100,
            "the fixture is a real bomb: {} bytes compressed",
            bomb.len()
        );
        let decoded = super::super::decode_surface_bytes_as(
            &bomb,
            ImportLimits::default(),
            ImportFormat::Svg,
        );
        assert!(
            matches!(decoded, Err(CodecError::LimitExceeded(_))),
            "the bomb was inflated past the cap: {:?}",
            decoded.map(|s| (s.width, s.height))
        );
        // The on-disk route drag-and-drop, recent files and the command line
        // take (`OpenDocument::open_image`): a `.svg` named file.
        let path = std::env::temp_dir().join(format!("w9n-svg-bomb-{}.svg", std::process::id()));
        std::fs::write(&path, &bomb).unwrap();
        let by_path = crate::codec::decode_path(&path).map(|i| (i.width, i.height));
        let surface = crate::codec::decode_surface_path(&path, ImportLimits::default())
            .map(|s| (s.width, s.height));
        let _ = std::fs::remove_file(&path);
        assert!(
            by_path.is_err(),
            "decode_path inflated the bomb: {by_path:?}"
        );
        assert!(
            matches!(surface, Err(CodecError::LimitExceeded(_))),
            "decode_surface_path inflated the bomb: {surface:?}"
        );
    }

    fn base64(bytes: &[u8]) -> String {
        const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let n = chunk
                .iter()
                .enumerate()
                .fold(0u32, |n, (i, b)| n | (u32::from(*b) << (16 - 8 * i)));
            for i in 0..4 {
                if i <= chunk.len() {
                    out.push(ABC[((n >> (18 - 6 * i)) & 63) as usize] as char);
                } else {
                    out.push('=');
                }
            }
        }
        out
    }

    #[test]
    fn an_embedded_gzip_svg_is_not_drawn() {
        let inner = r##"<svg xmlns="http://www.w3.org/2000/svg" width="2" height="2"><rect width="2" height="2" fill="#ff0000"/></svg>"##;
        let outer = |payload: &[u8]| {
            format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="2" height="2"><image width="2" height="2" xlink:href="data:image/svg+xml;base64,{}"/></svg>"#,
                base64(payload)
            )
        };
        // Control: the same SVG embedded uncompressed is drawn.
        let plain = decode_bytes(outer(inner.as_bytes()).as_bytes()).unwrap();
        assert_eq!(px(&plain.rgba8, 2, 1, 1), [255, 0, 0, 255], "control");
        // Compressed, usvg would inflate it unbounded: it is refused.
        let packed = decode_bytes(outer(&gzip(inner.as_bytes())).as_bytes()).unwrap();
        assert!(
            packed.rgba8.chunks(4).all(|p| p[3] == 0),
            "an embedded gzip SVG was inflated and drawn"
        );
    }

    #[test]
    fn the_sniff_wants_markup_and_an_svg_element() {
        assert!(looks_like_svg(b"  \n<svg>"));
        assert!(looks_like_svg(b"\xEF\xBB\xBF<?xml version=\"1.0\"?><svg/>"));
        assert!(!looks_like_svg(b"<html><body></body></html>"));
        assert!(!looks_like_svg(b"\x89PNG<svg"));
        assert!(!looks_like_svg(b""));
    }
}
