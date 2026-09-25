//! W10-F: the containers this crate decodes (and, for some, encodes) with its
//! own code or a pure-Rust crate other than `image`.
//!
//! A child module of [`crate::codec`] (declared there with `#[path]`), so the
//! codec facade stays the one place the application talks to a decoder. Every
//! entry point here takes the file's bytes, already read through a bounded
//! reader by [`read_bounded`], and an [`ImportLimits`] that is checked against
//! the *declared* dimensions before any pixel buffer exists.
//!
//! | Format | Read | Write | Module |
//! | --- | --- | --- | --- |
//! | PPM / PGM / PBM (ASCII and binary) | yes, 1-16 bit | binary P6 / P5 / P4 | [`pnm`] |
//! | DDS | uncompressed RGB(A)/luminance, BC1, BC2, BC3 | uncompressed BGRA, BC3 | [`dds`] |
//! | GIMP XCF | 8-bit RGB/RGBA/grey: the layer tree ([`xcf::read`], which app-shell opens layered) and the flattened composite ([`xcf::decode`]) | no | [`xcf`] |
//! | JPEG XL | yes (`jxl-oxide`) | W11-H: lossless 8-bit RGBA only, at least 2x2 (`zune-jpegxl`, in `codec::encode_into`); 16-bit samples are refused | [`jxl`] |
//! | AVIF | W15-A: yes, **in the decode worker process only** (`rusty_av1d`; see [`heif`]); refused by name where no worker is installed | yes (`image` over `ravif`) | [`heif`], [`avif`] |
//! | HEIC / HEIF | W15-A: yes, **in the decode worker process only** (`heic-rs`; see [`heif`]); refused by name where no worker is installed | no | [`heif`] |
//! | PSB | through the `psd` crate, as a layered document | through the `psd` crate (W11-H: version 2, 8-byte lengths) | - |
//! | OpenEXR, Radiance HDR (W11-H) | yes: File > Open makes a 32-bit document (`app-shell`); the surface returned here is clipped to 16-bit sRGB | EXR, 32-bit float | [`float`] |
//! | Apple ICNS (W11-H) | PNG, ARGB and 24-bit RLE entries, largest | no | [`icns`] |
//! | IFF ILBM / PBM (W11-H) | 1-8 planes (EHB, HAM6/8), 24, 32; ByteRun1 | no | [`iff`] |
//! | Krita KRA (W11-H) | the merged image only | no | [`kra`] |
//! | PDF, PDF-compatible AI (W13-D) | every page, rendered by `hayro` (the flat decode answers page 1; `app-shell` opens one artboard per page) | no (the print path's writer is `crate::pdf`) | [`pdf`] |
//! | WMF, EMF (W13-D) | the common GDI records, drawn through `resvg` | no | [`metafile`] |
//! | EPS (W15-E) | the PostScript artwork, run by the bounded interpreter in [`postscript`]; the embedded TIFF / WMF / EPSI preview when that cannot draw it, saying why | no | [`vector_docs`], [`postscript`] |
//! | Paint.NET PDN, Sketch, Adobe XD, Figma FIG (W13-D) | the embedded preview only, with a sentence saying so; a bare `fig-kiwi` canvas is refused by name | no | [`vector_docs`] |
//! | DNG (W13-C) | CFA or linear raw, uncompressed or lossless JPEG: developed to 16-bit sRGB | no | [`raw`] |
//! | CR2, CR3, NEF, ARW, RAF, ORF, RW2 (W13-C) | **refused by name**: no permissive reader (see [`raw`]) | no | [`raw`] |
//!
//! W15-A: AVIF and HEIC are read, but never in the calling process. The
//! decoders (`rusty_av1d` 1.2.0, a BSD-2-Clause rav1d fork, and `heic-rs`
//! 0.1.1, MIT OR Apache-2.0) both panic on some damaged files (an
//! `unwrap()` on a missing reference frame header at `rusty_av1d`'s
//! `src/decode.rs:4993`; a slice index at `heic-rs`'s
//! `src/hevc/decode/recon.rs:70`), and the release profile is
//! `panic = "abort"`. So [`decode`] and [`probe`] hand an AVIF / HEIC to the
//! decoder installed with [`heif::install_isolated_decoder`] - in the
//! application, `app-shell`'s decode worker, a child process running
//! [`heif::decode_in_this_process`] - and with none installed refuse it by
//! name ([`heif::no_worker_refusal`], [`heic_refusal`]). A HEIC is found by
//! content (there is no `.heic` [`ImportFormat`]: see
//! [`heif::HeifKind::import_format`]) and travels as [`ImportFormat::Avif`],
//! the HEIF-family format; the decode tells the two apart by brand again.

use std::io::{BufRead, Read, Seek};

use super::{read_head, CodecError, DecodedSurface, ImageInfo, ImportFormat, ImportLimits};

pub mod avif;
pub mod dds;
pub mod float;
/// W15-A: AVIF and HEIC reading, run in the decode worker process.
pub mod heif;
pub mod icns;
pub mod iff;
pub mod jxl;
pub mod kra;
/// W13-D: WMF / EMF.
pub mod metafile;
/// W13-D: PDF and PDF-compatible `.ai`.
pub mod pdf;
/// W15-E: the bounded PostScript interpreter that draws EPS artwork.
pub mod postscript;
/// W13-D: EPS, PDN, Sketch, XD and FIG previews.
pub mod vector_docs;
// W13-C: lossless JPEG, the compression DNG raw data uses.
mod ljpeg;
// W13-L: MP4 (AV1) video export, and the refusal a video file gets on open.
pub mod mp4;
pub mod pnm;
// W13-C: camera RAW: DNG developed, vendor RAWs refused by name.
pub mod raw;
pub mod xcf;

/// How many leading bytes the sniff looks at.
/// 64 bytes hold an `ftyp` box with up to eleven compatible brands, which
/// is where a `mif1` file says whether it is AVIF or HEIC.
const SNIFF_BYTES: usize = 64;

/// The formats this module owns, identified by content.
pub fn sniff(head: &[u8]) -> Option<ImportFormat> {
    if pnm::looks_like_pnm(head) {
        Some(ImportFormat::Pnm)
    } else if dds::looks_like_dds(head) {
        Some(ImportFormat::Dds)
    } else if xcf::looks_like_xcf(head) {
        Some(ImportFormat::Xcf)
    } else if jxl::looks_like_jxl(head) {
        Some(ImportFormat::Jxl)
    } else if avif::looks_like_avif(head) {
        Some(ImportFormat::Avif)
    } else if float::looks_like_exr(head) {
        Some(ImportFormat::Exr)
    } else if float::looks_like_hdr(head) {
        Some(ImportFormat::Hdr)
    } else if icns::looks_like_icns(head) {
        Some(ImportFormat::Icns)
    } else if iff::looks_like_iff(head) {
        Some(ImportFormat::Iff)
    } else if kra::looks_like_kra(head) {
        Some(ImportFormat::Kra)
    } else if pdf::looks_like_pdf(head) {
        Some(ImportFormat::Pdf)
    } else if vector_docs::looks_like_eps(head) {
        Some(ImportFormat::Eps)
    } else if vector_docs::looks_like_pdn(head) {
        Some(ImportFormat::Pdn)
    } else if vector_docs::looks_like_xd(head) {
        Some(ImportFormat::Xd)
    } else if vector_docs::looks_like_fig_kiwi(head) {
        Some(ImportFormat::Fig)
    } else if metafile::looks_like_emf(head) {
        Some(ImportFormat::Emf)
    } else if metafile::looks_like_wmf(head) {
        Some(ImportFormat::Wmf)
    } else {
        None
    }
}

/// `true` when `head` is an ISOBMFF `ftyp` box naming a HEIF/HEIC brand (and
/// not an AVIF one).
pub fn looks_like_heic(head: &[u8]) -> bool {
    if head.len() < 12 || &head[4..8] != b"ftyp" {
        return false;
    }
    matches!(
        &head[8..12],
        b"heic" | b"heix" | b"heim" | b"heis" | b"hevc" | b"hevx" | b"mif1" | b"msf1"
    ) && !avif::looks_like_avif(head)
}

/// The refusal a HEIC file gets where no decode worker is installed, naming
/// why rather than "unknown format" (W15-A: see [`heif`]).
pub fn heic_refusal() -> CodecError {
    heif::no_worker_refusal(heif::HeifKind::Heic)
}

/// Which of this module's formats `source` holds, by content; the stream is
/// left where it was found. W15-A: a HEIC answers [`ImportFormat::Avif`],
/// the HEIF family both travel as (see [`heif`]).
pub(super) fn sniff_source<R: BufRead + Seek>(
    source: &mut R,
) -> Result<Option<ImportFormat>, CodecError> {
    let start = source.stream_position()?;
    let mut head = [0u8; SNIFF_BYTES];
    let filled = read_head(source, &mut head)?;
    source.seek(std::io::SeekFrom::Start(start))?;
    let head = &head[..filled];
    // W13-C: a camera RAW is told apart from an ordinary TIFF by its IFDs,
    // which lie past the first 64 bytes: read a larger prefix for those.
    if raw::might_be_raw(head) {
        let mut prefix = Vec::new();
        source
            .by_ref()
            .take(raw::SNIFF_BYTES as u64)
            .read_to_end(&mut prefix)?;
        source.seek(std::io::SeekFrom::Start(start))?;
        if let Some(kind) = raw::identify(&prefix) {
            return Ok(Some(kind.format()));
        }
    }
    if looks_like_heic(head) {
        return Ok(Some(ImportFormat::Avif));
    }
    if mp4::looks_like_video(head) {
        return Err(mp4::video_refusal());
    }
    Ok(sniff(head))
}

/// Read the rest of `source`, refusing a file larger than any decode `limits`
/// allows could come from.
///
/// The bound is four times the decode allocation ceiling: an ASCII PPM spends
/// up to four characters per sample, which is the most any of these formats
/// spends per decoded byte. It bounds what the *read* may hold and is checked
/// as the read grows rather than trusted from a length field.
pub(super) fn read_bounded<R: Read>(
    source: R,
    limits: ImportLimits,
) -> Result<Vec<u8>, CodecError> {
    let cap = limits.max_alloc_bytes.saturating_mul(4);
    let mut data = Vec::new();
    source.take(cap.saturating_add(1)).read_to_end(&mut data)?;
    if data.len() as u64 > cap {
        return Err(CodecError::LimitExceeded(format!(
            "the file is larger than {cap} bytes"
        )));
    }
    Ok(data)
}

/// Header facts for one of this module's formats.
pub(super) fn probe<R: Read>(
    format: ImportFormat,
    source: R,
    limits: ImportLimits,
) -> Result<ImageInfo, CodecError> {
    let bytes = read_bounded(source, limits)?;
    match format {
        // W15-A: AVIF and HEIC, through the decode worker.
        ImportFormat::Avif => heif::probe_isolated(&bytes, limits),
        ImportFormat::Pnm => pnm::probe(&bytes, limits),
        ImportFormat::Dds => dds::probe(&bytes, limits),
        ImportFormat::Xcf => xcf::probe(&bytes, limits),
        ImportFormat::Jxl => jxl::probe(&bytes, limits),
        ImportFormat::Exr | ImportFormat::Hdr => float::probe(format, &bytes, limits),
        ImportFormat::Icns => icns::probe(&bytes, limits),
        ImportFormat::Iff => iff::probe(&bytes, limits),
        ImportFormat::Kra => kra::probe(&bytes, limits),
        ImportFormat::Pdf => pdf::probe(&bytes, limits),
        ImportFormat::Wmf | ImportFormat::Emf => metafile::probe(format, &bytes, limits),
        ImportFormat::Eps
        | ImportFormat::Pdn
        | ImportFormat::Sketch
        | ImportFormat::Xd
        | ImportFormat::Fig => vector_docs::probe(format, &bytes, limits),
        ImportFormat::Dng => raw::probe(&bytes, limits),
        ImportFormat::CameraRaw => Err(raw::refusal(&bytes)),
        other => Err(not_ours(other)),
    }
}

/// Decode one of this module's formats.
pub(super) fn decode<R: Read>(
    format: ImportFormat,
    source: R,
    limits: ImportLimits,
) -> Result<DecodedSurface, CodecError> {
    let bytes = read_bounded(source, limits)?;
    match format {
        // W15-A: AVIF and HEIC, through the decode worker.
        ImportFormat::Avif => heif::decode_isolated(&bytes, limits),
        ImportFormat::Pnm => pnm::decode(&bytes, limits),
        ImportFormat::Dds => dds::decode(&bytes, limits),
        ImportFormat::Xcf => xcf::decode(&bytes, limits),
        ImportFormat::Jxl => jxl::decode(&bytes, limits),
        ImportFormat::Exr | ImportFormat::Hdr => float::decode(format, &bytes, limits),
        ImportFormat::Icns => icns::decode(&bytes, limits),
        ImportFormat::Iff => iff::decode(&bytes, limits),
        ImportFormat::Kra => kra::decode(&bytes, limits),
        ImportFormat::Pdf => pdf::decode(&bytes, limits),
        ImportFormat::Wmf | ImportFormat::Emf => metafile::decode(format, &bytes, limits),
        ImportFormat::Eps
        | ImportFormat::Pdn
        | ImportFormat::Sketch
        | ImportFormat::Xd
        | ImportFormat::Fig => vector_docs::decode(format, &bytes, limits),
        ImportFormat::Dng => raw::decode(&bytes, limits),
        ImportFormat::CameraRaw => Err(raw::refusal(&bytes)),
        other => Err(not_ours(other)),
    }
}

fn not_ours(format: ImportFormat) -> CodecError {
    CodecError::Unsupported(format!("{} is not read by this module", format.name()))
}

/// A malformed-file error in this module's vocabulary.
pub(crate) fn malformed(format: &str, what: impl std::fmt::Display) -> CodecError {
    CodecError::Unsupported(format!("malformed {format} file: {what}"))
}

/// Check the dimensions, and the RGBA buffer the decode will allocate, before
/// it does. `bytes_per_pixel` is the storage size (4 for RGBA8, 8 for RGBA16);
/// `extra` is any further live allocation (a source plane, a layer).
pub(crate) fn check_decode(
    limits: ImportLimits,
    width: u32,
    height: u32,
    bytes_per_pixel: u64,
    extra: u64,
) -> Result<(), CodecError> {
    limits.check_dimensions(width, height)?;
    let out = u64::from(width)
        .saturating_mul(u64::from(height))
        .saturating_mul(bytes_per_pixel);
    limits.check_alloc(out.saturating_add(extra))
}

/// A straight-alpha sRGB RGBA8 surface.
pub(crate) fn rgba8_surface(
    width: u32,
    height: u32,
    rgba: Vec<u8>,
    source_format: ImportFormat,
) -> DecodedSurface {
    DecodedSurface {
        width,
        height,
        pixels: super::SurfacePixels::Rgba8(rgba),
        color_space: color::ColorSpace::Srgb,
        icc_profile: None,
        source_format,
    }
}

/// Header facts for a decode with no ICC profile.
pub(crate) fn info(width: u32, height: u32, format: ImportFormat, sixteen: bool) -> ImageInfo {
    ImageInfo {
        width,
        height,
        format,
        pixel_format: if sixteen {
            super::PixelFormat::Rgba16
        } else {
            super::PixelFormat::Rgba8
        },
        icc_profile: None,
    }
}

#[cfg(test)]
#[path = "w11h_tests.rs"]
mod w11h_tests;

#[cfg(test)]
mod tests {
    use crate::codec::{
        decode_surface_bytes, decode_surface_bytes_as, decode_surface_path, encode, probe_bytes,
        CodecError, ExportFormat, ImportFormat, ImportLimits,
    };

    fn solid(w: u32, h: u32) -> Vec<u8> {
        (0..w * h)
            .flat_map(|i| [(i * 9) as u8, 40, 200, 255])
            .collect()
    }

    /// The public decode entry points reach every W10-F reader by content,
    /// with no hint: this is the road File > Open takes.
    #[test]
    fn the_codec_facade_sniffs_and_decodes_every_new_format() {
        let (w, h) = (5u32, 3u32);
        let px = solid(w, h);
        for (format, import) in [
            (ExportFormat::Ppm, ImportFormat::Pnm),
            (ExportFormat::Pgm, ImportFormat::Pnm),
            (ExportFormat::Pbm, ImportFormat::Pnm),
            (ExportFormat::Dds, ImportFormat::Dds),
            (ExportFormat::DdsBc3, ImportFormat::Dds),
        ] {
            let bytes = encode(format, w, h, &px).unwrap();
            let s = decode_surface_bytes(&bytes, ImportLimits::default())
                .unwrap_or_else(|e| panic!("{format:?}: {e}"));
            assert_eq!((s.width, s.height, s.source_format), (w, h, import));
            assert_eq!(
                probe_bytes(&bytes, ImportLimits::default()).unwrap().format,
                import
            );
        }
        let xcf = super::xcf::tests::write_xcf(
            11,
            4,
            2,
            false,
            2,
            &[super::xcf::tests::TestLayer::rgba(
                "a",
                0,
                0,
                4,
                2,
                [9, 8, 7, 255],
            )],
        );
        let s = decode_surface_bytes(&xcf, ImportLimits::default()).unwrap();
        assert_eq!(s.source_format, ImportFormat::Xcf);
        let jxl = include_bytes!("testdata/ramp_6x4_rgba_lossless.jxl");
        let s = decode_surface_bytes(jxl, ImportLimits::default()).unwrap();
        assert_eq!(
            (s.width, s.height, s.source_format),
            (6, 4, ImportFormat::Jxl)
        );
    }

    #[test]
    fn a_path_opens_by_content_and_extensions_map() {
        let dir = std::env::temp_dir().join(format!("w10f-formats-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let px = solid(3, 2);
        // A DDS saved under a `.png` name still opens as the DDS it is.
        let lying = dir.join("really_a_dds.png");
        std::fs::write(&lying, encode(ExportFormat::Dds, 3, 2, &px).unwrap()).unwrap();
        let s = decode_surface_path(&lying, ImportLimits::default()).unwrap();
        assert_eq!(s.source_format, ImportFormat::Dds);
        // A PNG saved under `.dds` opens as the PNG it is: content wins.
        let png_named_dds = dir.join("really_a_png.dds");
        std::fs::write(
            &png_named_dds,
            encode(ExportFormat::Png, 3, 2, &px).unwrap(),
        )
        .unwrap();
        let s = decode_surface_path(&png_named_dds, ImportLimits::default()).unwrap();
        assert_eq!(s.source_format, ImportFormat::Png);
        // Garbage under `.dds` is reported by the DDS reader, by name.
        let err = decode_surface_bytes_as(b"garbage!", ImportLimits::default(), ImportFormat::Dds)
            .unwrap_err();
        assert!(err.to_string().contains("DDS"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
        for (ext, format) in [
            ("ppm", ImportFormat::Pnm),
            ("PGM", ImportFormat::Pnm),
            ("pbm", ImportFormat::Pnm),
            ("dds", ImportFormat::Dds),
            ("xcf", ImportFormat::Xcf),
            ("jxl", ImportFormat::Jxl),
            ("avif", ImportFormat::Avif),
            ("psb", ImportFormat::Psd),
        ] {
            assert_eq!(ImportFormat::from_extension(ext), Some(format), "{ext}");
        }
        assert_eq!(ImportFormat::from_extension("heic"), None);
    }

    #[test]
    fn heic_is_refused_by_name_not_as_an_unknown_format() {
        let mut heic = vec![0, 0, 0, 24];
        heic.extend_from_slice(b"ftypheic\0\0\0\0mif1heic");
        heic.extend_from_slice(&[0; 64]);
        let err = decode_surface_bytes(&heic, ImportLimits::default()).unwrap_err();
        assert!(matches!(err, CodecError::Unsupported(_)));
        assert!(err.to_string().contains("HEIC"), "{err}");
        // W15-A: with no decode worker installed (this test process has
        // none) the refusal says why, and its advice names formats that open
        // everywhere.
        let advice = err.to_string();
        assert!(advice.contains("decode worker"), "{advice}");
        assert!(advice.contains("JPEG or PNG"), "{advice}");
        assert!(!advice.contains("AVIF"), "{advice}");
        let err = probe_bytes(&heic, ImportLimits::default()).unwrap_err();
        assert!(err.to_string().contains("HEIC"), "{err}");
    }
}
