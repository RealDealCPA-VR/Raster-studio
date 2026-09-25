//! W15-A: the in-process AVIF / HEIC decoders the decode worker runs, and
//! the fixtures the worker's own tests (in `studio-desktop`) open.
//!
//! These tests call [`decode_in_this_process`] on well-formed files only:
//! a damaged file is the worker's business, and the tests that feed it
//! bit-flipped copies spawn the real worker process.
//!
//! HEIC content comes from `heic-rs`'s own synthetic HEVC bitstream builder
//! (its `bench` feature, a dev-dependency only), the one HEVC encoder in
//! reach: it writes DC-predicted pictures, so every HEIC here is a flat
//! mid-grey (luma and chroma `1 << (depth - 1)`), wrapped in a HEIF
//! container this file writes by hand. AVIF content is real: `image`'s
//! 8-bit encoder and `ravif`'s 10-bit one, plus an independent libaom file.

use super::*;
use crate::codec::{decode_surface_bytes, ImportLimits};

/// The libaom (through ffmpeg) 4:2:0 file the W10-F tests used.
const LIBAOM_420: &[u8] = include_bytes!("testdata/quadrants_16x12_libaom_420.avif");
/// Committed fixtures (written by [`write_w15a_fixtures`]), which the
/// decode worker's tests in `studio-desktop` open through the real worker.
pub const FIXTURE_AVIF_10BIT: &[u8] =
    include_bytes!("testdata/w15a_quadrants_16x12_10bit_alpha.avif");
pub const FIXTURE_HEIC: &[u8] = include_bytes!("testdata/w15a_grey_64x128_irot90_alpha.heic");

// ---------------------------------------------------------------------------
// A minimal HEIF writer (test only).
// ---------------------------------------------------------------------------

fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    out
}

fn full(kind: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
    let mut b = vec![version];
    b.extend_from_slice(&flags.to_be_bytes()[1..]);
    b.extend_from_slice(body);
    bx(kind, &b)
}

/// One item of a hand-written HEIF file.
pub struct Item {
    pub id: u16,
    pub kind: [u8; 4],
    pub data: Vec<u8>,
    /// Property boxes, associated in this order (transforms are applied in
    /// association order).
    pub props: Vec<Vec<u8>>,
}

pub fn ispe(w: u32, h: u32) -> Vec<u8> {
    let mut b = w.to_be_bytes().to_vec();
    b.extend_from_slice(&h.to_be_bytes());
    full(b"ispe", 0, 0, &b)
}

pub fn irot(angle: u8) -> Vec<u8> {
    bx(b"irot", &[angle])
}

pub fn imir(axis: u8) -> Vec<u8> {
    bx(b"imir", &[axis])
}

pub fn nclx(primaries: u16, transfer: u16, matrix: u16, full_range: bool) -> Vec<u8> {
    let mut b = b"nclx".to_vec();
    for v in [primaries, transfer, matrix] {
        b.extend_from_slice(&v.to_be_bytes());
    }
    b.push(if full_range { 0x80 } else { 0 });
    bx(b"colr", &b)
}

pub fn icc(profile: &[u8]) -> Vec<u8> {
    let mut b = b"prof".to_vec();
    b.extend_from_slice(profile);
    bx(b"colr", &b)
}

pub fn auxc_alpha() -> Vec<u8> {
    full(
        b"auxC",
        0,
        0,
        b"urn:mpeg:mpegB:cicp:systems:auxiliary:alpha\0",
    )
}

/// A HEIF file: `ftyp` (`brands[0]` is the major brand), `meta` and one
/// `mdat` holding every item's data. `refs` are `(type, from, to)`.
pub fn heif_file(
    brands: &[&[u8; 4]],
    primary: u16,
    items: &[Item],
    refs: &[(&[u8; 4], u16, Vec<u16>)],
) -> Vec<u8> {
    let mut ftyp_body = brands[0].to_vec();
    ftyp_body.extend_from_slice(&[0; 4]);
    for b in brands {
        ftyp_body.extend_from_slice(*b);
    }
    let ftyp = bx(b"ftyp", &ftyp_body);

    let mut hdlr = vec![0; 4];
    hdlr.extend_from_slice(b"pict");
    hdlr.extend_from_slice(&[0; 12]);
    hdlr.push(0);
    let hdlr = full(b"hdlr", 0, 0, &hdlr);
    let pitm = full(b"pitm", 0, 0, &primary.to_be_bytes());
    let mut iinf = (items.len() as u16).to_be_bytes().to_vec();
    for item in items {
        let mut infe = item.id.to_be_bytes().to_vec();
        infe.extend_from_slice(&[0, 0]);
        infe.extend_from_slice(&item.kind);
        infe.push(0);
        iinf.extend_from_slice(&full(b"infe", 2, 0, &infe));
    }
    let iinf = full(b"iinf", 0, 0, &iinf);
    let mut iref = Vec::new();
    for (kind, from, to) in refs {
        let mut b = from.to_be_bytes().to_vec();
        b.extend_from_slice(&(to.len() as u16).to_be_bytes());
        for t in to {
            b.extend_from_slice(&t.to_be_bytes());
        }
        iref.extend_from_slice(&bx(kind, &b));
    }
    let iref = full(b"iref", 0, 0, &iref);
    // One property box per association (no sharing): simple and valid.
    let mut ipco = Vec::new();
    let mut ipma = (items.len() as u32).to_be_bytes().to_vec();
    let mut index = 0u8;
    for item in items {
        ipma.extend_from_slice(&item.id.to_be_bytes());
        ipma.push(item.props.len() as u8);
        for p in &item.props {
            ipco.extend_from_slice(p);
            index += 1;
            let essential = !(p[4..8] == *b"ispe" || p[4..8] == *b"colr");
            ipma.push(if essential { 0x80 | index } else { index });
        }
    }
    let iprp = bx(
        b"iprp",
        &[bx(b"ipco", &ipco), full(b"ipma", 0, 0, &ipma)].concat(),
    );
    // `iloc` version 0: 4-byte offsets and lengths, no base offset. Its size
    // does not depend on the offsets, so lay it out with zeros first.
    let iloc = |mdat_start: u32| {
        let mut b = vec![0x44, 0x00];
        b.extend_from_slice(&(items.len() as u16).to_be_bytes());
        let mut at = mdat_start;
        for item in items {
            b.extend_from_slice(&item.id.to_be_bytes());
            b.extend_from_slice(&[0, 0, 0, 1]);
            b.extend_from_slice(&at.to_be_bytes());
            b.extend_from_slice(&(item.data.len() as u32).to_be_bytes());
            at += item.data.len() as u32;
        }
        full(b"iloc", 0, 0, &b)
    };
    let meta_of = |iloc: Vec<u8>| {
        let mut body = [hdlr.clone(), pitm.clone(), iloc, iinf.clone()].concat();
        if !refs.is_empty() {
            body.extend_from_slice(&iref);
        }
        body.extend_from_slice(&iprp);
        full(b"meta", 0, 0, &body)
    };
    let meta_len = meta_of(iloc(0)).len();
    let mdat_start = (ftyp.len() + meta_len + 8) as u32;
    let meta = meta_of(iloc(mdat_start));
    let data: Vec<u8> = items.iter().flat_map(|i| i.data.clone()).collect();
    [ftyp, meta, bx(b"mdat", &data)].concat()
}

/// An `hvcC` for the Main Still Picture parameter sets `sets` (VPS, SPS,
/// PPS NAL units, headers included).
fn hvcc(sets: &[Vec<u8>], chroma_idc: u8, bit_depth: u8) -> Vec<u8> {
    let mut b = vec![1, 0x03];
    b.extend_from_slice(&0x1000_0000u32.to_be_bytes());
    b.extend_from_slice(&[0x90, 0, 0, 0, 0, 0]);
    b.push(30);
    b.extend_from_slice(&0xf000u16.to_be_bytes());
    b.push(0xfc);
    b.push(0xfc | chroma_idc);
    b.push(0xf8 | (bit_depth - 8));
    b.push(0xf8 | (bit_depth - 8));
    b.extend_from_slice(&[0, 0]);
    b.push(0x0f); // one temporal layer, nested, 4-byte NAL lengths
    b.push(sets.len() as u8);
    for (set, nal_type) in sets.iter().zip([32u8, 33, 34]) {
        b.push(0x80 | nal_type);
        b.extend_from_slice(&1u16.to_be_bytes());
        b.extend_from_slice(&(set.len() as u16).to_be_bytes());
        b.extend_from_slice(set);
    }
    bx(b"hvcC", &b)
}

/// A flat mid-grey HEVC still (`heic-rs`'s synthetic builder) as an item's
/// data and its `hvcC`.
fn grey_hevc(w: u32, h: u32, chroma_idc: u32, bit_depth: u32) -> (Vec<u8>, Vec<u8>) {
    let (sets, slice) = heic_rs::hevc::synth::picture_fmt(w, h, chroma_idc, bit_depth);
    let mut data = (slice.len() as u32).to_be_bytes().to_vec();
    data.extend_from_slice(&slice);
    (data, hvcc(&sets, chroma_idc as u8, bit_depth as u8))
}

/// A HEIC of a `w` x `h` grey picture with `extra` properties after its
/// `ispe`, `hvcC` and a full-range BT.601 `nclx`; with `alpha`, a grey
/// monochrome alpha item too.
pub fn grey_heic(w: u32, h: u32, bit_depth: u32, extra: Vec<Vec<u8>>, alpha: bool) -> Vec<u8> {
    let (data, config) = grey_hevc(w, h, 1, bit_depth);
    let mut props = vec![ispe(w, h), config, nclx(1, 13, 6, true)];
    props.extend(extra);
    let mut items = vec![Item {
        id: 1,
        kind: *b"hvc1",
        data,
        props,
    }];
    let mut refs = Vec::new();
    if alpha {
        let (data, config) = grey_hevc(w, h, 0, bit_depth);
        items.push(Item {
            id: 2,
            kind: *b"hvc1",
            data,
            props: vec![ispe(w, h), config, auxc_alpha()],
        });
        refs.push((b"auxl", 2, vec![1]));
    }
    heif_file(&[b"heic", b"mif1", b"heic"], 1, &items, &refs)
}

/// The primary (and alpha) item data and `av1C` of an AVIF, re-wrapped with
/// `extra` properties after `ispe` and `av1C` (and a `w`x`h` `ispe`).
pub fn rewrap_avif(avif: &[u8], extra: Vec<Vec<u8>>) -> Vec<u8> {
    let meta = heic_rs::meta::parse(avif).unwrap();
    let ctx = Context {
        file: avif,
        ftyp: FileType {
            major: Brand::Avif,
            effective: Brand::Avif,
            minor_version: 0,
            compatible_count: 0,
        },
        meta,
    };
    let id = ctx.meta.primary;
    let p = ctx.props(id).unwrap();
    let ispe_box = ispe(p.ispe.unwrap().width, p.ispe.unwrap().height);
    let av1c = |item: u32| -> Vec<u8> {
        let assoc = ctx.meta.props.associations(item);
        assoc
            .iter()
            .filter_map(|a| ctx.meta.props.resolve(*a))
            .find(|b| &b.boxtype == b"av1C")
            .map(|b| bx(b"av1C", b.payload))
            .unwrap()
    };
    let mut props = vec![ispe_box.clone(), av1c(id)];
    props.extend(extra);
    let mut items = vec![Item {
        id: 1,
        kind: *b"av01",
        data: ctx.item_data(id).unwrap().into_owned(),
        props,
    }];
    let mut refs = Vec::new();
    if let Some(aux) = ctx.alpha_item(id).unwrap() {
        items.push(Item {
            id: 2,
            kind: *b"av01",
            data: ctx.item_data(aux).unwrap().into_owned(),
            props: vec![ispe_box, av1c(aux), auxc_alpha()],
        });
        refs.push((b"auxl", 2, vec![1]));
    }
    heif_file(&[b"avif", b"mif1", b"miaf"], 1, &items, &refs)
}

// ---------------------------------------------------------------------------
// Pictures.
// ---------------------------------------------------------------------------

const RED: [u8; 4] = [220, 30, 40, 255];
const GREEN: [u8; 4] = [30, 200, 60, 255];
const BLUE: [u8; 4] = [40, 50, 210, 255];
const CLEAR: [u8; 4] = [255, 255, 255, 0];

/// A `w` x `h` picture of four flat quadrants: red, green / blue, clear.
fn quadrants(w: u32, h: u32) -> Vec<u8> {
    let mut px = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let c = match (x < w / 2, y < h / 2) {
                (true, true) => RED,
                (false, true) => GREEN,
                (true, false) => BLUE,
                (false, false) => CLEAR,
            };
            px.extend_from_slice(&c);
        }
    }
    px
}

fn ravif_10bit(w: u32, h: u32) -> Vec<u8> {
    let px: Vec<ravif::RGBA8> = quadrants(w, h)
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&[r, g, b, a]| ravif::RGBA8::new(r, g, b, a))
        .collect();
    ravif::Encoder::new()
        .with_quality(100.0)
        .with_alpha_quality(100.0)
        .with_speed(10)
        .with_bit_depth(ravif::BitDepth::Ten)
        .encode_rgba(ravif::Img::new(&px[..], w as usize, h as usize))
        .unwrap()
        .avif_file
}

/// RGBA8 of pixel (`x`, `y`) of `s`, 16-bit samples taken to 8.
pub fn px8(s: &DecodedSurface, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * s.width + x) * 4) as usize;
    match &s.pixels {
        SurfacePixels::Rgba8(v) => [v[i], v[i + 1], v[i + 2], v[i + 3]],
        SurfacePixels::Rgba16(v) => [0, 1, 2, 3].map(|c| (v[i + c] as u32 * 255 / 65535) as u8),
    }
}

pub fn close(got: [u8; 4], want: [u8; 4], tolerance: i32) -> bool {
    got.iter()
        .zip(want)
        .all(|(g, w)| (i32::from(*g) - i32::from(w)).abs() <= tolerance)
}

/// The quadrant centres of a `w` x `h` quadrants picture hold its colours
/// (the clear quadrant: alpha 0 only; its colour is not meaningful).
fn assert_quadrants(s: &DecodedSurface, w: u32, h: u32, tolerance: i32) {
    assert_eq!((s.width, s.height), (w, h));
    let (qx, qy) = (w / 4, h / 4);
    for (x, y, want) in [
        (qx, qy, RED),
        (w - 1 - qx, qy, GREEN),
        (qx, h - 1 - qy, BLUE),
    ] {
        let got = px8(s, x, y);
        assert!(
            close(got, want, tolerance),
            "({x},{y}): {got:?} vs {want:?}"
        );
    }
    assert_eq!(px8(s, w - 1 - qx, h - 1 - qy)[3], 0, "the clear quadrant");
}

// ---------------------------------------------------------------------------
// AVIF.
// ---------------------------------------------------------------------------

#[test]
fn an_eight_bit_avif_from_our_encoder_decodes_to_its_size_colours_and_alpha() {
    let (w, h) = (32u32, 24u32);
    let file = crate::codec::formats::avif::encode(w, h, &quadrants(w, h), 100).unwrap();
    assert_eq!(HeifKind::of(&file), Some(HeifKind::Avif));
    let s = decode_in_this_process(HeifKind::Avif, &file, ImportLimits::default()).unwrap();
    assert_eq!(s.format(), PixelFormat::Rgba8);
    assert_eq!(s.color_space, ColorSpace::Srgb);
    assert_eq!(s.source_format, ImportFormat::Avif);
    assert_quadrants(&s, w, h, 6);
}

#[test]
fn a_ten_bit_avif_decodes_to_a_sixteen_bit_surface() {
    let (w, h) = (16u32, 12u32);
    let file = ravif_10bit(w, h);
    let s = decode_in_this_process(HeifKind::Avif, &file, ImportLimits::default()).unwrap();
    assert_eq!(s.format(), PixelFormat::Rgba16);
    assert_quadrants(&s, w, h, 6);
    // Sixteen-bit samples, not eight-bit ones widened: a 10-bit code lands
    // between multiples of 257 somewhere in the picture.
    let SurfacePixels::Rgba16(v) = &s.pixels else {
        unreachable!()
    };
    assert!(v.iter().any(|&x| x % 257 != 0), "10-bit precision is kept");
}

#[test]
fn an_independent_libaom_avif_decodes() {
    let s = decode_in_this_process(HeifKind::Avif, LIBAOM_420, ImportLimits::default()).unwrap();
    assert_eq!((s.width, s.height), (16, 12));
    // Four distinct flat quadrants, each opaque.
    let corners = [(3, 2), (12, 2), (3, 9), (12, 9)].map(|(x, y)| px8(&s, x, y));
    for c in corners {
        assert_eq!(c[3], 255);
    }
    for i in 0..4 {
        for j in i + 1..4 {
            assert!(!close(corners[i], corners[j], 40), "{corners:?}");
        }
    }
}

#[test]
fn irot_and_imir_orient_an_avif_in_association_order() {
    let (w, h) = (16u32, 12u32);
    let base = ravif_10bit(w, h);
    let plain = decode_in_this_process(HeifKind::Avif, &base, ImportLimits::default()).unwrap();
    // irot 1: 90 degrees anticlockwise. Output (x, y) comes from input
    // (w - 1 - y, x): the top-right (green) quadrant lands top-left.
    let rotated = rewrap_avif(&base, vec![irot(1)]);
    let s = decode_in_this_process(HeifKind::Avif, &rotated, ImportLimits::default()).unwrap();
    assert_eq!((s.width, s.height), (h, w));
    for (x, y) in [(2, 2), (9, 13), (2, 13), (9, 2)] {
        assert_eq!(px8(&s, x, y), px8(&plain, w - 1 - y, x), "({x},{y})");
    }
    // imir 0 (a vertical axis): left and right exchange.
    let mirrored = rewrap_avif(&base, vec![imir(0)]);
    let s = decode_in_this_process(HeifKind::Avif, &mirrored, ImportLimits::default()).unwrap();
    assert_eq!((s.width, s.height), (w, h));
    assert_eq!(px8(&s, 3, 2), px8(&plain, w - 1 - 3, 2));
    // Mirror then rotate differs from rotate then mirror: order is data.
    let a = rewrap_avif(&base, vec![imir(0), irot(1)]);
    let b = rewrap_avif(&base, vec![irot(1), imir(0)]);
    let a = decode_in_this_process(HeifKind::Avif, &a, ImportLimits::default()).unwrap();
    let b = decode_in_this_process(HeifKind::Avif, &b, ImportLimits::default()).unwrap();
    assert_ne!(px8(&a, 2, 2), px8(&b, 2, 2));
}

#[test]
fn an_embedded_icc_profile_and_p3_nclx_become_the_surface_space() {
    let base = ravif_10bit(16, 12);
    let profile = b"not a real profile, only bytes to carry".to_vec();
    let file = rewrap_avif(&base, vec![icc(&profile)]);
    let s = decode_in_this_process(HeifKind::Avif, &file, ImportLimits::default()).unwrap();
    assert_eq!(s.icc_profile.as_deref(), Some(&profile[..]));
    assert!(matches!(&s.color_space, ColorSpace::IccProfile { profile: p, .. } if *p == profile));
    let file = rewrap_avif(&base, vec![nclx(12, 13, 6, true)]);
    let s = decode_in_this_process(HeifKind::Avif, &file, ImportLimits::default()).unwrap();
    assert_eq!(s.color_space, ColorSpace::DisplayP3);
    assert_eq!(s.icc_profile, None);
}

#[test]
fn a_grid_avif_composes_its_tiles() {
    let (w, h) = (16u32, 12u32);
    let tile = ravif_10bit(w, h);
    let meta = heic_rs::meta::parse(&tile).unwrap();
    let data = meta.item_data(&tile, meta.primary).unwrap().into_owned();
    // A 1 x 2 grid of the same tile: 32 x 12.
    let mut grid = vec![0, 0, 0, 1];
    grid.extend_from_slice(&((2 * w) as u16).to_be_bytes());
    grid.extend_from_slice(&(h as u16).to_be_bytes());
    let tile_item = |id| Item {
        id,
        kind: *b"av01",
        data: data.clone(),
        props: vec![ispe(w, h)],
    };
    let file = heif_file(
        &[b"avif", b"mif1"],
        1,
        &[
            Item {
                id: 1,
                kind: *b"grid",
                data: grid,
                props: vec![ispe(2 * w, h)],
            },
            tile_item(2),
            tile_item(3),
        ],
        &[(b"dimg", 1, vec![2, 3])],
    );
    let s = decode_in_this_process(HeifKind::Avif, &file, ImportLimits::default()).unwrap();
    assert_eq!((s.width, s.height), (2 * w, h));
    assert_eq!(px8(&s, 3, 2), px8(&s, w + 3, 2));
    assert!(close(px8(&s, 3, 2), RED, 6));
}

#[test]
fn a_declared_size_past_the_limits_is_refused_before_decoding() {
    let base = ravif_10bit(16, 12);
    let meta = heic_rs::meta::parse(&base).unwrap();
    let data = meta.item_data(&base, meta.primary).unwrap().into_owned();
    let file = heif_file(
        &[b"avif", b"mif1"],
        1,
        &[Item {
            id: 1,
            kind: *b"av01",
            data,
            props: vec![ispe(70_000, 70_000)],
        }],
        &[],
    );
    let err = decode_in_this_process(HeifKind::Avif, &file, ImportLimits::default()).unwrap_err();
    assert!(matches!(err, CodecError::LimitExceeded(_)), "{err}");
    let heic = grey_heic(64, 64, 8, Vec::new(), false);
    let tight = ImportLimits {
        max_pixels: 64 * 64 - 1,
        ..ImportLimits::default()
    };
    let err = decode_in_this_process(HeifKind::Heic, &heic, tight).unwrap_err();
    assert!(matches!(err, CodecError::LimitExceeded(_)), "{err}");
}

// ---------------------------------------------------------------------------
// HEIC.
// ---------------------------------------------------------------------------

#[test]
fn a_heic_decodes_to_its_size_grey_and_alpha() {
    let file = grey_heic(64, 128, 8, Vec::new(), true);
    assert_eq!(HeifKind::of(&file), Some(HeifKind::Heic));
    let s = decode_in_this_process(HeifKind::Heic, &file, ImportLimits::default()).unwrap();
    assert_eq!((s.width, s.height), (64, 128));
    assert_eq!(s.format(), PixelFormat::Rgba8);
    // Luma and chroma 128, full-range BT.601: RGB 128; the alpha plane's
    // 128 is alpha 128.
    for (x, y) in [(0, 0), (63, 127), (30, 70)] {
        assert!(
            close(px8(&s, x, y), [128, 128, 128, 128], 1),
            "{:?}",
            px8(&s, x, y)
        );
    }
}

#[test]
fn a_ten_bit_heic_decodes_to_sixteen_bits_and_irot_turns_it() {
    let file = grey_heic(64, 128, 10, vec![irot(1)], false);
    let s = decode_in_this_process(HeifKind::Heic, &file, ImportLimits::default()).unwrap();
    assert_eq!(
        (s.width, s.height),
        (128, 64),
        "irot 1 turns 64x128 on its side"
    );
    assert_eq!(s.format(), PixelFormat::Rgba16);
    let SurfacePixels::Rgba16(v) = &s.pixels else {
        unreachable!()
    };
    // 512 of 1023, full range: 32 800 of 65 535.
    assert!((i32::from(v[0]) - 32_800).abs() <= 128, "{}", v[0]);
    assert_eq!(v[3], 65_535, "opaque");
}

#[test]
fn a_heic_icc_profile_is_kept() {
    let profile = b"heic profile bytes".to_vec();
    let file = grey_heic(64, 64, 8, vec![icc(&profile)], false);
    let s = decode_in_this_process(HeifKind::Heic, &file, ImportLimits::default()).unwrap();
    assert_eq!(s.icc_profile.as_deref(), Some(&profile[..]));
}

// ---------------------------------------------------------------------------
// The committed fixtures and the facade with no worker.
// ---------------------------------------------------------------------------

#[test]
fn the_committed_fixtures_decode_to_their_documented_size_and_colours() {
    let s = decode_in_this_process(HeifKind::Avif, FIXTURE_AVIF_10BIT, ImportLimits::default())
        .unwrap();
    assert_eq!(s.format(), PixelFormat::Rgba16);
    assert_quadrants(&s, 16, 12, 6);
    let s = decode_in_this_process(HeifKind::Heic, FIXTURE_HEIC, ImportLimits::default()).unwrap();
    assert_eq!((s.width, s.height), (128, 64));
    assert!(close(px8(&s, 5, 5), [128, 128, 128, 128], 1));
    // The HEIC fixture is exactly what the builder writes (it is
    // deterministic); the AVIF one came from `ravif` and is only checked by
    // what it decodes to.
    assert_eq!(
        FIXTURE_HEIC,
        &grey_heic(64, 128, 8, vec![irot(1)], true)[..]
    );
}

/// With no isolated decoder installed (no test in this crate installs one),
/// the codec facade refuses AVIF and HEIC by name rather than decoding them
/// in this process.
#[test]
fn without_a_worker_the_facade_refuses_rather_than_decoding_in_process() {
    assert!(!isolated_decoder_installed());
    for (file, name) in [(FIXTURE_AVIF_10BIT, "AVIF"), (FIXTURE_HEIC, "HEIC")] {
        let err = decode_surface_bytes(file, ImportLimits::default()).unwrap_err();
        let text = err.to_string();
        assert!(
            text.contains(name) && text.contains("decode worker"),
            "{text}"
        );
    }
}

/// Regenerate the committed fixtures:
/// `cargo test -p raster write_w15a_fixtures -- --ignored`.
#[test]
#[ignore = "writes the committed fixtures"]
fn write_w15a_fixtures() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/formats/testdata");
    std::fs::write(
        dir.join("w15a_quadrants_16x12_10bit_alpha.avif"),
        ravif_10bit(16, 12),
    )
    .unwrap();
    std::fs::write(
        dir.join("w15a_grey_64x128_irot90_alpha.heic"),
        grey_heic(64, 128, 8, vec![irot(1)], true),
    )
    .unwrap();
}
