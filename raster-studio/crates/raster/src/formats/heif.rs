//! W15-A: AVIF and HEIC reading, and the seam that keeps it out of the
//! editor's own process.
//!
//! # Why a worker process
//!
//! Both decoders this module drives can panic on a damaged file:
//! `rusty_av1d` 1.2.0 (the AV1 decoder, a rav1d fork) reaches an
//! `unwrap()` on a missing reference frame header (`src/decode.rs:4993`),
//! and `heic-rs` 0.1.1 (the HEVC decoder) indexes past a slice in
//! `src/hevc/decode/recon.rs:70` on 3 of 4000 bit-flipped or truncated
//! files W13-C tried. The release profile is `panic = "abort"`, so either
//! would close the editor with every open document. So the codec facade
//! never calls these decoders itself: [`decode_isolated`] hands the bytes
//! to the decoder installed with [`install_isolated_decoder`], which in
//! the application is `app-shell`'s `decode_worker` - it re-executes the
//! editor's own binary with `--decode-worker <kind>`, and that child calls
//! [`decode_in_this_process`]. A panic (or a hang) then ends the child,
//! and the editor reports "the decoder crashed on this file". With no
//! decoder installed (a tool, a test, a library user) an AVIF or HEIC is
//! refused by name ([`no_worker_refusal`]), as before this wave.
//!
//! # What is read
//!
//! - **AVIF**: the primary item, a single `av01` item or a `grid` of them,
//!   decoded by `rusty_av1d` (8-bit, and 10/12-bit to a 16-bit surface);
//!   the `auxl` alpha item; 4:0:0, 4:2:0, 4:2:2 and 4:4:4. The container is
//!   read with `heic-rs`'s ISOBMFF parser (`iinf` `iloc` `iref` `iprp`
//!   `idat` are the same boxes in both formats) and the colour conversion
//!   is `heic-rs`'s too, so the two formats share one YCbCr-to-RGB path.
//!   YCbCr uses the `colr` `nclx` matrix and range, or, without one, the
//!   AV1 sequence header's.
//! - **HEIC / HEIF**: whatever `heic-rs` decodes (HEVC Main / Main 10 /
//!   Main Still Picture intra, grids, alpha), at 8 bits or, for a deeper
//!   file, 16.
//! - **Both**: `irot` / `imir` / `clap` are applied in association order,
//!   as ISO/IEC 23008-12 requires. That is a HEIF file's orientation: the
//!   Exif `Orientation` tag it may also carry is informative there, and a
//!   reader that applied it on top of `irot` would rotate twice, so it is
//!   not applied. An embedded ICC profile (`colr` `prof`) is kept and
//!   becomes the surface's space; without one, `nclx` primaries 12 (Display
//!   P3) open as Display P3 and everything else as sRGB.
//!
//! # What is not
//!
//! Premultiplied alpha (`prem`) is not divided out; HDR transfer functions
//! (PQ, HLG) and BT.2020 primaries are not converted (their samples open as
//! sRGB); image sequences (`avis`, `msf1`) open their primary still item
//! only when they have one; `iovl` overlays are refused by name.

use std::sync::RwLock;

use heic_rs::context::Context;
use heic_rs::ftyp::FileType;
use heic_rs::hevc::{ChromaFormat, Frame};
use heic_rs::props::colr::{MatrixCoefficients, Nclx, Range};
use heic_rs::{Brand, DecodeOptions, Image, PixelLayout};

use super::{check_decode, malformed};
use crate::codec::{
    icc_profile_space, CodecError, DecodedSurface, ImageInfo, ImportFormat, ImportLimits,
    PixelFormat, SurfacePixels,
};
use color::ColorSpace;

/// Which HEIF-family format a file is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HeifKind {
    /// AV1 in HEIF.
    Avif,
    /// HEVC in HEIF (`.heic`, `.heif`).
    Heic,
}

impl HeifKind {
    /// The kind `bytes` holds, by its `ftyp` brands.
    pub fn of(bytes: &[u8]) -> Option<HeifKind> {
        let head = &bytes[..bytes.len().min(super::SNIFF_BYTES)];
        if super::avif::looks_like_avif(head) {
            Some(HeifKind::Avif)
        } else if super::looks_like_heic(head) {
            Some(HeifKind::Heic)
        } else {
            None
        }
    }

    /// The format's name in messages.
    pub fn name(self) -> &'static str {
        match self {
            HeifKind::Avif => "AVIF",
            HeifKind::Heic => "HEIC",
        }
    }

    /// The word the decode worker takes after `--decode-worker`.
    pub fn worker_arg(self) -> &'static str {
        match self {
            HeifKind::Avif => "avif",
            HeifKind::Heic => "heic",
        }
    }

    /// The inverse of [`HeifKind::worker_arg`].
    pub fn from_worker_arg(arg: &str) -> Option<HeifKind> {
        match arg {
            "avif" => Some(HeifKind::Avif),
            "heic" => Some(HeifKind::Heic),
            _ => None,
        }
    }

    /// The [`ImportFormat`] a surface of this kind reports.
    ///
    /// `ImportFormat` has no HEIC variant (it lives in `codec.rs`, which
    /// this wave did not change), so a HEIC reports [`ImportFormat::Avif`],
    /// the other HEIF-family member; nothing outside this crate reads a
    /// surface's `source_format`, and every message names the kind itself.
    pub fn import_format(self) -> ImportFormat {
        ImportFormat::Avif
    }
}

/// A decoder that runs AVIF / HEIC decoding somewhere a panic cannot reach
/// the caller (the application's decode worker process).
pub type IsolatedDecoder = fn(HeifKind, &[u8], ImportLimits) -> Result<DecodedSurface, CodecError>;

static ISOLATED: RwLock<Option<IsolatedDecoder>> = RwLock::new(None);

/// Route every AVIF / HEIC decode the codec facade makes through `decoder`.
/// The application installs its worker-process client here at start-up.
pub fn install_isolated_decoder(decoder: IsolatedDecoder) {
    let mut slot = ISOLATED.write().unwrap_or_else(|e| e.into_inner());
    *slot = Some(decoder);
}

/// Whether [`install_isolated_decoder`] has been called in this process.
pub fn isolated_decoder_installed() -> bool {
    ISOLATED.read().unwrap_or_else(|e| e.into_inner()).is_some()
}

/// The refusal an AVIF / HEIC gets in a process with no isolated decoder.
pub fn no_worker_refusal(kind: HeifKind) -> CodecError {
    let (decoder, advice) = match kind {
        HeifKind::Avif => (
            "its AV1 decoder (rusty_av1d, a rav1d fork) can abort the process on a damaged file",
            "AVIF export works (File > Export As > AVIF)",
        ),
        HeifKind::Heic => (
            "its HEVC decoder (heic-rs) can abort the process on a damaged file",
            "convert it to JPEG or PNG first",
        ),
    };
    CodecError::Unsupported(format!(
        "opening {} needs the decode worker process, because {decoder}; this process has \
         not started one (the Raster Studio application does); {advice}",
        kind.name()
    ))
}

/// Decode `bytes` (an AVIF or HEIC) through the installed isolated decoder.
pub(super) fn decode_isolated(
    bytes: &[u8],
    limits: ImportLimits,
) -> Result<DecodedSurface, CodecError> {
    let kind = HeifKind::of(bytes)
        .ok_or_else(|| malformed("AVIF", "the file does not start with an AVIF or HEIC ftyp"))?;
    let decoder = *ISOLATED.read().unwrap_or_else(|e| e.into_inner());
    let Some(decoder) = decoder else {
        return Err(no_worker_refusal(kind));
    };
    let surface = decoder(kind, bytes, limits)?;
    // Whatever produced it, the caller's limits hold for what comes back.
    check_worker_header(
        limits,
        surface.width,
        surface.height,
        surface.format() == PixelFormat::Rgba16,
    )?;
    if surface.pixels.pixel_count() as u64 != u64::from(surface.width) * u64::from(surface.height) {
        return Err(malformed(kind.name(), "the decoder returned a short image"));
    }
    Ok(surface)
}

/// Header facts, through the same isolated decode (there is no separate
/// in-process probe: parsing the container is the decoder's code too).
pub(super) fn probe_isolated(bytes: &[u8], limits: ImportLimits) -> Result<ImageInfo, CodecError> {
    let surface = decode_isolated(bytes, limits)?;
    Ok(ImageInfo {
        width: surface.width,
        height: surface.height,
        format: surface.source_format,
        pixel_format: surface.format(),
        icc_profile: surface.icc_profile,
    })
}

/// The check a worker client makes on the header an answer declares,
/// before it reads (or allocates) a single pixel.
pub fn check_worker_header(
    limits: ImportLimits,
    width: u32,
    height: u32,
    sixteen: bool,
) -> Result<(), CodecError> {
    check_decode(limits, width, height, if sixteen { 8 } else { 4 }, 0)
}

/// Decode an AVIF or HEIC **in this process**.
///
/// Only the decode worker calls this: the decoders underneath can panic on
/// a damaged file (see the module docs), and a panic here, under the
/// release profile's `panic = "abort"`, ends the process.
pub fn decode_in_this_process(
    kind: HeifKind,
    bytes: &[u8],
    limits: ImportLimits,
) -> Result<DecodedSurface, CodecError> {
    match kind {
        HeifKind::Avif => decode_avif(bytes, limits),
        HeifKind::Heic => decode_heic(bytes, limits),
    }
}

fn heif_error(kind: HeifKind) -> impl Fn(heic_rs::Error) -> CodecError {
    move |e| match e {
        heic_rs::Error::PixelLimit { pixels, max_pixels } => CodecError::LimitExceeded(format!(
            "the {} image has {pixels} pixels, limit is {max_pixels}",
            kind.name()
        )),
        other => malformed(kind.name(), other),
    }
}

/// The largest allocation a decode of a `width` x `height` image makes on
/// top of the output: three 16-bit planes and a 16-bit alpha plane.
fn planes_bytes(width: u32, height: u32) -> u64 {
    u64::from(width)
        .saturating_mul(u64::from(height))
        .saturating_mul(2 * 4)
}

fn decode_heic(bytes: &[u8], limits: ImportLimits) -> Result<DecodedSurface, CodecError> {
    let err = heif_error(HeifKind::Heic);
    let info = heic_rs::probe(bytes).map_err(&err)?;
    for (w, h) in [
        (info.coded_width, info.coded_height),
        (info.width, info.height),
    ] {
        check_decode(limits, w, h, 8, planes_bytes(w, h))?;
    }
    let ctx = Context::open(bytes).map_err(&err)?;
    let props = ctx.props(ctx.meta.primary).map_err(&err)?;
    let sixteen = info.bit_depth > 8;
    let options = DecodeOptions::default()
        .with_layout(if sixteen {
            PixelLayout::Rgba16
        } else {
            PixelLayout::Rgba8
        })
        .with_max_pixels(Some(limits.max_pixels))
        .with_transforms(true)
        .with_alpha(true);
    let image = heic_rs::decode(bytes, &options).map_err(&err)?;
    let primaries = props.nclx.map(|n| n.primaries);
    surface_from(image, props.icc, primaries, HeifKind::Heic, limits)
}

fn decode_avif(bytes: &[u8], limits: ImportLimits) -> Result<DecodedSurface, CodecError> {
    let err = heif_error(HeifKind::Avif);
    let meta = heic_rs::meta::parse(bytes).map_err(&err)?;
    // `heic-rs`'s `ftyp` reader refuses AVIF brands (it has no AV1
    // decoder); the rest of its container code does not look at them.
    let ctx = Context {
        file: bytes,
        ftyp: FileType {
            major: Brand::Avif,
            effective: Brand::Avif,
            minor_version: 0,
            compatible_count: 0,
        },
        meta,
    };
    let id = ctx.meta.primary;
    let item = ctx.meta.primary_item().map_err(&err)?;
    if &item.item_type == b"iovl" {
        return Err(CodecError::Unsupported(
            "this AVIF's primary image is an `iovl` overlay, which is not read; only single \
             and `grid` images are"
                .into(),
        ));
    }
    let props = ctx.props(id).map_err(&err)?;
    let ispe = props
        .ispe
        .ok_or_else(|| malformed("AVIF", "the primary item has no `ispe` (image size)"))?;
    let (cw, ch) = (ispe.width, ispe.height);
    check_decode(limits, cw, ch, 8, planes_bytes(cw, ch))?;
    let (tw, th) = heic_rs::transform::transformed_size(cw, ch, &props.transforms).map_err(&err)?;
    check_decode(limits, tw, th, 8, planes_bytes(tw, th))?;

    let (frame, cicp) = decode_av1_item(&ctx, id, limits)?;
    let alpha = match ctx.alpha_item(id).map_err(&err)? {
        Some(aux) => Some(decode_av1_item(&ctx, aux, limits)?.0),
        None => None,
    };
    let nclx = props.nclx.unwrap_or(cicp);
    let layout = if frame.bit_depth > 8 {
        PixelLayout::Rgba16
    } else {
        PixelLayout::Rgba8
    };
    let image = heic_rs::color::convert(
        &frame,
        alpha.as_ref(),
        nclx,
        layout,
        limits.max_pixels,
        None,
    )
    .map_err(&err)?;
    let image = heic_rs::transform::apply_all(image, &props.transforms).map_err(&err)?;
    surface_from(
        image,
        props.icc,
        Some(nclx.primaries),
        HeifKind::Avif,
        limits,
    )
}

/// One AVIF item's pixels: a single `av01` item, or a `grid` of them
/// composed. The colour description is the AV1 sequence header's (the
/// first tile's, for a grid).
fn decode_av1_item(
    ctx: &Context<'_>,
    id: u32,
    limits: ImportLimits,
) -> Result<(Frame, Nclx), CodecError> {
    let err = heif_error(HeifKind::Avif);
    match ctx.grid(id).map_err(&err)? {
        Some((grid, tiles)) => {
            check_decode(
                limits,
                grid.output_width,
                grid.output_height,
                8,
                planes_bytes(grid.output_width, grid.output_height),
            )?;
            let mut frames = Vec::with_capacity(tiles.len());
            let mut cicp = None;
            for tile in tiles {
                let (frame, c) = decode_av1_coded(ctx, tile, limits)?;
                cicp.get_or_insert(c);
                frames.push(frame);
            }
            let composed =
                heic_rs::grid::compose(&grid, &frames, limits.max_pixels).map_err(&err)?;
            let cicp = cicp.ok_or_else(|| malformed("AVIF", "the grid has no tiles"))?;
            Ok((composed, cicp))
        }
        None => decode_av1_coded(ctx, id, limits),
    }
}

/// Decode one `av01` item's OBUs to a planar frame.
fn decode_av1_coded(
    ctx: &Context<'_>,
    id: u32,
    limits: ImportLimits,
) -> Result<(Frame, Nclx), CodecError> {
    use rusty_av1d::{Decoder, PlanarImageComponent, Rav1dError, Settings};
    let err = heif_error(HeifKind::Avif);
    let item = ctx
        .meta
        .item(id)
        .ok_or_else(|| malformed("AVIF", format!("item {id} is missing")))?;
    if &item.item_type != b"av01" {
        return Err(malformed(
            "AVIF",
            format!(
                "item {id} is `{}`, not an AV1 image",
                String::from_utf8_lossy(&item.item_type)
            ),
        ));
    }
    let data = ctx.item_data(id).map_err(&err)?;
    let av1 = |e: Rav1dError| malformed("AVIF", format!("AV1 decoding failed: {e}"));
    let mut settings = Settings::new();
    // One thread, one frame: a still image, decoded on this thread.
    settings.set_n_threads(1);
    settings.set_max_frame_delay(1);
    settings.set_frame_size_limit(u32::try_from(limits.max_pixels).unwrap_or(u32::MAX));
    let mut decoder = Decoder::with_settings(&settings).map_err(av1)?;
    match decoder.send_data(data.to_vec().into_boxed_slice(), None, None, None) {
        Ok(()) | Err(Rav1dError::TryAgain) => {}
        Err(e) => return Err(av1(e)),
    }
    // A still picture needs a handful of rounds at most; the bound only
    // stops a stream that never produces one from spinning forever.
    let mut picture = None;
    for _ in 0..64 {
        match decoder.get_picture() {
            Ok(p) => {
                picture = Some(p);
                break;
            }
            Err(Rav1dError::TryAgain) => match decoder.send_pending_data() {
                Ok(()) | Err(Rav1dError::TryAgain) => {}
                Err(e) => return Err(av1(e)),
            },
            Err(e) => return Err(av1(e)),
        }
    }
    let picture = picture.ok_or_else(|| malformed("AVIF", "the AV1 data holds no picture"))?;
    let (width, height) = (picture.width(), picture.height());
    check_decode(limits, width, height, 8, planes_bytes(width, height))?;
    // `bit_depth()` is dav1d's `bpc`, the bits per component (8, 10 or 12;
    // its doc comment says 8 or 16, but the value is the component depth);
    // anything past 8 is stored in 16-bit words.
    let bit_depth = u8::try_from(picture.bit_depth())
        .ok()
        .filter(|b| matches!(b, 8 | 10 | 12))
        .ok_or_else(|| malformed("AVIF", "unsupported AV1 bit depth"))?;
    let wide = bit_depth > 8;
    let chroma = match picture.pixel_layout() {
        rusty_av1d::PixelLayout::I400 => ChromaFormat::Monochrome,
        rusty_av1d::PixelLayout::I420 => ChromaFormat::Yuv420,
        rusty_av1d::PixelLayout::I422 => ChromaFormat::Yuv422,
        rusty_av1d::PixelLayout::I444 => ChromaFormat::Yuv444,
    };
    let plane = |component: PlanarImageComponent, w: u32, h: u32| -> Result<Vec<u16>, CodecError> {
        let stride = picture.stride(component) as usize;
        let bytes = picture.plane(component);
        let row_bytes = w as usize * if wide { 2 } else { 1 };
        let mut out = Vec::with_capacity(w as usize * h as usize);
        for row in 0..h as usize {
            let line = bytes
                .get(row * stride..row * stride + row_bytes)
                .ok_or_else(|| malformed("AVIF", "a decoded plane is shorter than its size"))?;
            if wide {
                out.extend(
                    line.as_chunks::<2>()
                        .0
                        .iter()
                        .map(|b| u16::from_ne_bytes(*b)),
                );
            } else {
                out.extend(line.iter().map(|&b| u16::from(b)));
            }
        }
        Ok(out)
    };
    let y = plane(PlanarImageComponent::Y, width, height)?;
    let (cw, ch) = if chroma == ChromaFormat::Monochrome {
        (0, 0)
    } else {
        chroma.chroma_size(width, height)
    };
    let (cb, cr) = if chroma == ChromaFormat::Monochrome {
        (Vec::new(), Vec::new())
    } else {
        (
            plane(PlanarImageComponent::U, cw, ch)?,
            plane(PlanarImageComponent::V, cw, ch)?,
        )
    };
    let matrix_code = picture.matrix_coefficients() as u16;
    let cicp = Nclx {
        primaries: picture.color_primaries() as u16,
        transfer: picture.transfer_characteristic() as u16,
        matrix: MatrixCoefficients::from_code(matrix_code),
        matrix_code,
        range: match picture.color_range() {
            rusty_av1d::pixel::YUVRange::Full => Range::Full,
            rusty_av1d::pixel::YUVRange::Limited => Range::Limited,
        },
    };
    let frame = Frame {
        width,
        height,
        bit_depth,
        chroma,
        y,
        cb,
        cr,
        y_stride: width,
        c_stride: cw,
    };
    frame.validate().map_err(&err)?;
    Ok((frame, cicp))
}

/// The surface for a decoded `image`, with its colour space.
fn surface_from(
    image: Image,
    icc: Option<&[u8]>,
    primaries: Option<u16>,
    kind: HeifKind,
    limits: ImportLimits,
) -> Result<DecodedSurface, CodecError> {
    let Image {
        data,
        width,
        height,
        layout,
    } = image;
    let sixteen = match layout {
        PixelLayout::Rgba8 => false,
        PixelLayout::Rgba16 => true,
        other => {
            return Err(malformed(
                kind.name(),
                format!("unexpected layout {other:?}"),
            ));
        }
    };
    let expected = u64::from(width) * u64::from(height) * if sixteen { 8 } else { 4 };
    if data.len() as u64 != expected {
        return Err(malformed(kind.name(), "the decoder returned a short image"));
    }
    let pixels = if sixteen {
        SurfacePixels::Rgba16(
            data.as_chunks::<2>()
                .0
                .iter()
                .map(|b| u16::from_ne_bytes(*b))
                .collect(),
        )
    } else {
        SurfacePixels::Rgba8(data)
    };
    let icc_profile = limits.take_icc(icc.map(<[u8]>::to_vec));
    let color_space = match (&icc_profile, primaries) {
        (Some(profile), _) => icc_profile_space(profile),
        (None, Some(12)) => ColorSpace::DisplayP3,
        _ => ColorSpace::Srgb,
    };
    Ok(DecodedSurface {
        width,
        height,
        pixels,
        color_space,
        icc_profile,
        source_format: kind.import_format(),
    })
}

#[cfg(test)]
#[path = "heif_tests.rs"]
pub(crate) mod tests;
