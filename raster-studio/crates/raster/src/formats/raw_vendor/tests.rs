//! W18-D: the vendor RAW decoders against synthetic files built from the
//! same public descriptions ([`super::super::vendor_fixture`]): exact CFA
//! values, developed colours through the codec facade (the road File > Open
//! takes), named refusals, damage, limits.

use super::super::fixture::srgb16;
use super::super::vendor_fixture::{self as fx, Cr2, Shot, Store};
use super::{canon, fuji, nikon, sony, Want};
use crate::codec::{
    decode_surface_bytes, probe_bytes, CodecError, ImportFormat, ImportLimits, SurfacePixels,
};

/// Four colours whose per-channel means are equal (so grey world returns
/// them unchanged), one per quadrant.
const QUADS: [[f64; 3]; 4] = [
    [0.40, 0.20, 0.10],
    [0.10, 0.40, 0.20],
    [0.20, 0.10, 0.40],
    [0.30, 0.30, 0.30],
];

/// The quadrants, with the top and bottom three rows blown out (a clipped
/// highlight, equal in both halves so the quadrant means stay equal).
fn scene(w: u32, h: u32) -> impl Fn(u32, u32) -> [f64; 3] {
    move |x, y| {
        if y < 3 || y + 3 >= h {
            [4.0; 3]
        } else {
            QUADS[usize::from(x >= w / 2) + 2 * usize::from(y >= h / 2)]
        }
    }
}

fn develop(bytes: &[u8]) -> (u32, u32, Vec<u16>) {
    let s = decode_surface_bytes(bytes, ImportLimits::default())
        .unwrap_or_else(|e| panic!("decode failed: {e}"));
    assert_eq!(s.source_format, ImportFormat::CameraRaw);
    let SurfacePixels::Rgba16(px) = s.pixels else {
        panic!("a camera RAW develops to 16 bits");
    };
    (s.width, s.height, px)
}

fn pixel(px: &[u16], w: u32, x: u32, y: u32) -> [u16; 4] {
    let i = ((y * w + x) * 4) as usize;
    [px[i], px[i + 1], px[i + 2], px[i + 3]]
}

/// The developed image is `w x h`, each quadrant centre is its scene colour
/// within 1% of full scale (sRGB-encoded), and the blown band is white.
fn check(bytes: &[u8], w: u32, h: u32, what: &str) {
    let (ow, oh, px) = develop(bytes);
    assert_eq!((ow, oh), (w, h), "{what}: size");
    for (q, (x, y)) in [
        (w / 4, h / 4),
        (3 * w / 4, h / 4),
        (w / 4, 3 * h / 4),
        (3 * w / 4, 3 * h / 4),
    ]
    .into_iter()
    .enumerate()
    {
        let got = pixel(&px, w, x, y);
        for c in 0..3 {
            let want = srgb16(QUADS[q][c]);
            assert!(
                (i32::from(got[c]) - i32::from(want)).abs() <= 655,
                "{what}: quadrant {q} channel {c}: {} vs {want} ({got:?})",
                got[c]
            );
        }
        assert_eq!(got[3], u16::MAX);
    }
    let band = pixel(&px, w, w / 2, 0);
    assert!(
        band[..3].iter().all(|&c| c >= 65_000),
        "{what}: a blown highlight develops white, got {band:?}"
    );
}

fn refused(bytes: &[u8], words: &[&str]) {
    let err = decode_surface_bytes(bytes, ImportLimits::default()).unwrap_err();
    assert!(matches!(err, CodecError::Unsupported(_)), "{err}");
    let text = err.to_string();
    for w in words {
        assert!(text.contains(w), "{w:?} not in: {text}");
    }
    assert!(text.contains("DNG"), "{text}");
    let text = probe_bytes(bytes, ImportLimits::default())
        .unwrap_err()
        .to_string();
    assert!(text.contains(words[0]), "{text}");
}

fn lcg(seed: &mut u64) -> u64 {
    *seed = seed
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    *seed >> 33
}

// ------------------------------------------------------ lossless JPEG ----

/// The lossless-JPEG decoder CR2 relies on returns exactly what the
/// reference encoder in the fixture coded: every predictor, precisions 8 to
/// 16, one to four components.
#[test]
fn lossless_jpeg_round_trips_every_predictor_precision_and_component_count() {
    let mut seed = 7;
    for precision in [8u32, 12, 14, 16] {
        for predictor in 1u8..=7 {
            for comps in 1u32..=4 {
                let (w, h) = (9u32, 7u32);
                let n = (w * h * comps) as usize;
                let mask = ((1u64 << precision) - 1) as u16;
                // A ramp with noise, so every predictor sees real residuals.
                let samples: Vec<u16> = (0..n)
                    .map(|i| ((i as u64 * 37 + lcg(&mut seed) % 64) as u16) & mask)
                    .collect();
                let jpeg = fx::ljpeg(&samples, w, h, comps, precision, predictor);
                let back = super::super::ljpeg::decode(&jpeg, n)
                    .unwrap_or_else(|e| panic!("P{precision} predictor {predictor} x{comps}: {e}"));
                assert_eq!(back, samples, "P{precision} predictor {predictor} x{comps}");
            }
        }
    }
}

// ---------------------------------------------------------------- CR2 ----

/// A CR2 decodes to exactly the CFA it was built from in every slice
/// layout (several slices, one, none), with two or four components, 12 or
/// 14 bits and any predictor; its black comes from the masked border per
/// 2x2 cell, its active area and as-shot balance from the maker note; it
/// develops to the scene's colours with the blown band white.
#[test]
fn cr2_decodes_its_slices_to_the_exact_cfa_and_develops() {
    let base = Cr2::default();
    let mut variants = vec![base.clone()];
    variants.push(Cr2 {
        comps: 4,
        slices: Some([1, 36, 36]),
        ..base.clone()
    });
    variants.push(Cr2 {
        slices: None,
        predictor: 6,
        ..base.clone()
    });
    // A body whose file starts one row off: the phase is found from the data.
    variants.push(Cr2 {
        row_shift: true,
        ..base.clone()
    });
    let mut twelve = base.clone();
    twelve.precision = 12;
    twelve.shot.white = 4095;
    twelve.shot.black = [256, 258, 255, 260];
    variants.push(twelve);
    for spec in &variants {
        let what = format!("{spec:?}");
        let s = &spec.shot;
        let [top, left, bottom, right] = s.active;
        let (aw, ah) = (right - left, bottom - top);
        let file = fx::cr2(spec, scene(aw, ah));
        let m = canon::read(&file, ImportLimits::default(), Want::Pixels)
            .unwrap_or_else(|e| panic!("{what}: {e}"));
        let shift = u32::from(spec.row_shift);
        let cfa = |x: u32, y: u32| fx::rggb(x, y + shift);
        assert_eq!(m.samples, fx::mosaic(s, cfa, scene(aw, ah)), "{what}");
        let codes: Vec<u8> = (0..4).map(|k| cfa(k % 2, k / 2)).collect();
        assert_eq!(m.pattern.codes, codes, "{what}: the CFA phase");
        assert_eq!(
            m.active,
            (top as usize, left as usize, bottom as usize, right as usize)
        );
        for c in 0..4 {
            assert!(
                (m.black[c] - f64::from(s.black[c])).abs() < 1e-9,
                "{what}: black {:?}",
                m.black
            );
        }
        let wb = m.wb.expect("the as-shot balance is read");
        assert!((wb[0] - 2.0).abs() < 0.01 && (wb[2] - 1.0 / 0.7).abs() < 0.01);
        check(&file, aw, ah, &what);
        let info = probe_bytes(&file, ImportLimits::default()).unwrap();
        assert_eq!(
            (info.width, info.height, info.format),
            (aw, ah, ImportFormat::CameraRaw)
        );
        assert_eq!(info.pixel_format, crate::format::PixelFormat::Rgba16);
    }
    // Orientation 6 stands the image up.
    let mut turned = base.clone();
    turned.shot.orientation = 6;
    let (w, h, _) = develop(&fx::cr2(&turned, scene(64, 40)));
    assert_eq!((w, h), (40, 64));
    // With no maker note: the whole sensor, black 0, grey-world balance.
    let mut bare = base;
    bare.maker_note = false;
    bare.shot.active = [0, 0, 44, 72];
    bare.shot.black = [0; 4];
    check(&fx::cr2(&bare, scene(72, 44)), 72, 44, "no maker note");
}

// ---------------------------------------------------------------- NEF ----

/// An uncompressed NEF, as 16-bit words or packed 12/14-bit rows, decodes
/// to its exact CFA, takes black and the as-shot balance from the maker
/// note, and develops; Nikon's Huffman compression is refused by name.
#[test]
fn nef_uncompressed_decodes_exactly_and_huffman_nef_is_refused_by_name() {
    for (bits, store) in [
        (14u32, Store::Words),
        (12, Store::Packed),
        (14, Store::Packed),
        (10, Store::Packed),
    ] {
        let what = format!("NEF {bits}-bit {store:?}");
        let white = ((1u32 << bits) - 1) as u16;
        let mut shot = Shot::new(48, 32, 0, white);
        shot.black = [white / 16, white / 16 + 2, white / 16 + 1, white / 16 + 3];
        let file = fx::nef(&shot, bits, store, scene(48, 32));
        let m = nikon::read(&file, ImportLimits::default(), Want::Pixels)
            .unwrap_or_else(|e| panic!("{what}: {e}"));
        assert_eq!(
            m.samples,
            fx::mosaic(&shot, fx::rggb, scene(48, 32)),
            "{what}"
        );
        assert_eq!(m.black, shot.black.map(f64::from), "{what}");
        let wb = m.wb.expect("WB_RBLevels is read");
        assert!((wb[0] - 2.0).abs() < 1e-3 && (wb[2] - 1.0 / 0.7).abs() < 1e-3);
        check(&file, 48, 32, &what);
    }
    let shot = Shot::new(48, 32, 600, 16383);
    let mut file = fx::nef(&shot, 14, Store::Words, scene(48, 32));
    // Compression = 1 (SHORT, big-endian) in the raw IFD becomes 34713.
    let needle = [0x01, 0x03, 0x00, 0x03, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01];
    let at = file
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("the raw IFD's Compression entry");
    file[at + 8..at + 10].copy_from_slice(&34713u16.to_be_bytes());
    refused(&file, &["Nikon NEF", "Huffman"]);
}

// ---------------------------------------------------------------- ARW ----

/// One hand-built ARW 2 block pair decodes to the values its fields say:
/// the maximum and minimum at their positions, every other pixel `min +
/// (delta << shift)` (shift 2 here: `max - min` is 500), even columns from
/// the first block and odd ones from the second.
#[test]
fn an_arw2_block_decodes_to_the_values_its_bit_fields_hold() {
    let (max, min, imax, imin) = (1500u128, 1000u128, 3u128, 9u128);
    let deltas: [u128; 14] = [0, 1, 2, 3, 10, 20, 40, 80, 100, 120, 124, 125, 126, 127];
    let mut v = max | (min << 11) | (imax << 22) | (imin << 26);
    for (k, &d) in deltas.iter().enumerate() {
        v |= d << (30 + 7 * k);
    }
    let mut row = v.to_le_bytes().to_vec();
    // Second block: max == min == 700, every delta 0.
    let flat: u128 = 700 | (700 << 11) | (1 << 26);
    row.extend(flat.to_le_bytes());
    let identity: Vec<u32> = (0..4096).collect();
    let out = sony::arw2(&row, 32, 1, &identity).unwrap();
    let mut want = [0u32; 16];
    let mut k = 0;
    for (i, slot) in want.iter_mut().enumerate() {
        *slot = if i == 3 {
            1500
        } else if i == 9 {
            1000
        } else {
            let d = deltas[k] as u32;
            k += 1;
            (1000 + (d << 2)).min(2047)
        };
    }
    for i in 0..16 {
        assert_eq!(u32::from(out[2 * i]), want[i] * 2, "even column {i}");
        assert_eq!(out[2 * i + 1], 1400, "odd column {i}");
    }
    // The tone curve: identity below the first knot, then steps of 2, 4, 8, 16.
    let curve = sony::tone_curve(Some(&fx::SONY_CURVE.map(u32::from)));
    assert_eq!(curve, fx::sony_curve(fx::SONY_CURVE));
    assert_eq!(curve[2000], 2000);
    assert_eq!(curve[2001] - curve[2000], 2);
    assert_eq!(curve[4095] - curve[4094], 16);
}

/// ARW 2 and uncompressed ARWs decode to the exact CFA the encoder says a
/// decoder reconstructs, and develop (black 512 by default, grey world);
/// Sony's lossless compression and ARW 1 are refused by name.
#[test]
fn arw_decodes_curve_compressed_and_uncompressed_and_refuses_the_rest() {
    let curve = fx::sony_curve(fx::SONY_CURVE);
    let top = curve[2047 * 2] as u16;
    for (compressed, shot) in [
        (true, Shot::new(64, 32, 512, top)),
        (false, Shot::new(64, 32, 512, 16383)),
    ] {
        let what = format!("ARW compressed={compressed}");
        let (file, expect) = fx::arw(&shot, compressed, scene(64, 32));
        let m = sony::read(&file, ImportLimits::default(), Want::Pixels)
            .unwrap_or_else(|e| panic!("{what}: {e}"));
        assert_eq!(m.samples, expect, "{what}");
        assert!(m.wb.is_none(), "Sony's balance is encrypted: grey world");
        check(&file, 64, 32, &what);
    }
    let (good, _) = fx::arw(&Shot::new(64, 32, 512, top), true, scene(64, 32));
    // Compression 32767 (SHORT, little-endian) becomes 7.
    let needle = [0x03, 0x01, 0x03, 0x00, 0x01, 0x00, 0x00, 0x00, 0xFF, 0x7F];
    let at = good
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("the raw IFD's Compression entry");
    let mut lossless = good.clone();
    lossless[at + 8..at + 10].copy_from_slice(&7u16.to_le_bytes());
    refused(&lossless, &["Sony ARW", "lossless"]);
    // Less than a byte per pixel: ARW 1. StripByteCounts is halved.
    let counts = [0x17, 0x01, 0x04, 0x00, 0x01, 0x00, 0x00, 0x00];
    let at = good
        .windows(counts.len())
        .position(|w| w == counts)
        .expect("StripByteCounts");
    let mut arw1 = good;
    arw1[at + 8..at + 12].copy_from_slice(&(64u32 * 32 / 2).to_le_bytes());
    refused(&arw1, &["Sony ARW", "ARW 1"]);
}

// ---------------------------------------------------------------- RAF ----

/// A RAF with a `FujiIFD`, Bayer or X-Trans, either byte order, decodes to
/// its exact CFA inside its crop and develops; compressed data is refused
/// by name.
#[test]
fn raf_decodes_bayer_and_xtrans_and_refuses_compressed_data() {
    for (xtrans, le) in [(false, true), (true, true), (true, false), (false, false)] {
        let what = format!("RAF xtrans={xtrans} le={le}");
        let mut shot = Shot::new(60, 48, 1024, 16383);
        shot.active = [6, 6, 42, 54];
        let (aw, ah) = (48, 36);
        let file = fx::raf(&shot, xtrans, le, scene(aw, ah));
        let m = fuji::read(&file, ImportLimits::default(), Want::Pixels)
            .unwrap_or_else(|e| panic!("{what}: {e}"));
        let cfa = |x: u32, y: u32| {
            if xtrans {
                fx::XTRANS[((y % 6) * 6 + x % 6) as usize]
            } else {
                fx::rggb(x, y)
            }
        };
        assert_eq!(m.samples, fx::mosaic(&shot, cfa, scene(aw, ah)), "{what}");
        assert_eq!(m.active, (6, 6, 42, 54), "{what}");
        assert_eq!(m.pattern.rows, if xtrans { 6 } else { 2 });
        check(&file, aw, ah, &what);
    }
    // Halve StripByteCounts (0xF008, LONG, little-endian): compressed.
    let shot = Shot::new(60, 48, 1024, 16383);
    let good = fx::raf(&shot, true, true, scene(60, 48));
    let needle = [0x08, 0xF0, 0x04, 0x00, 0x01, 0x00, 0x00, 0x00];
    let at = good
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("0xF008");
    let mut compressed = good;
    compressed[at + 8..at + 12].copy_from_slice(&(60u32 * 48).to_le_bytes());
    refused(&compressed, &["Fujifilm RAF", "compressed"]);
}

// ----------------------------------------------------------- ORF, RW2 ----

/// An uncompressed ORF decodes and develops; a compressed one (less than
/// two bytes per pixel) is refused by name.
#[test]
fn orf_uncompressed_develops_and_compressed_orf_is_refused() {
    let shot = Shot::new(40, 32, 64, 4095);
    let file = fx::orf(&shot, scene(40, 32));
    check(&file, 40, 32, "ORF");
    let mut short = file.clone();
    // StripByteCounts (279, LONG, little-endian) to one byte per pixel.
    let needle = [0x17, 0x01, 0x04, 0x00, 0x01, 0x00, 0x00, 0x00];
    let at = short
        .windows(needle.len())
        .position(|w| w == needle)
        .expect("StripByteCounts");
    short[at + 8..at + 12].copy_from_slice(&(40u32 * 32).to_le_bytes());
    refused(&short, &["Olympus ORF", "compression"]);
}

/// An uncompressed RW2 decodes in every CFA phase, inside its sensor
/// borders, with its black and balance tags; Panasonic's packed RAW is
/// refused by name.
#[test]
fn rw2_uncompressed_develops_in_every_phase_and_packed_rw2_is_refused() {
    for pattern in 1u16..=4 {
        let mut shot = Shot::new(52, 40, 128, 4095);
        shot.active = [4, 2, 36, 50];
        let file = fx::rw2(&shot, pattern, scene(48, 32));
        check(&file, 48, 32, &format!("RW2 pattern {pattern}"));
    }
    let shot = Shot::new(52, 40, 128, 4095);
    let file = fx::rw2(&shot, 1, scene(52, 40));
    // Cut the data short: a packed file.
    let packed = file[..file.len() - 52 * 40].to_vec();
    refused(&packed, &["Panasonic RW2", "34316"]);
}

// ------------------------------------------------------------ PEF, SRW ----

/// A generic TIFF-shaped RAW (the Pentax PEF / Samsung SRW road): IFD0 is
/// the CFA, 16-bit little-endian words, green-blue / red-green declared by
/// TIFF/EP `CFAPattern`, the DNG `BlackLevel` / `WhiteLevel` written in it.
fn generic_raw(make: &'static str, shot: &Shot, compression: u16) -> Vec<u8> {
    let gbrg = |x: u32, y: u32| [1u8, 2, 0, 1][((y % 2) * 2 + x % 2) as usize];
    let samples = fx::mosaic(shot, gbrg, scene(shot.width, shot.height));
    let mut t = fx::T::new(true, b"II*\0\0\0\0\0");
    let data: Vec<u8> = samples.iter().flat_map(|&s| s.to_le_bytes()).collect();
    let data_at = t.blob(&data);
    let ifd0 = t.ifd(vec![
        (256, fx::V::L(vec![shot.width])),
        (257, fx::V::L(vec![shot.height])),
        (258, fx::V::S(vec![16])),
        (259, fx::V::S(vec![compression])),
        (262, fx::V::S(vec![32803])),
        (271, fx::V::A(make)),
        (273, fx::V::L(vec![data_at])),
        (277, fx::V::S(vec![1])),
        (279, fx::V::L(vec![data.len() as u32])),
        (33421, fx::V::S(vec![2, 2])),
        (33422, fx::V::B(vec![1, 2, 0, 1])),
        (50714, fx::V::L(vec![u32::from(shot.black[0])])),
        (50717, fx::V::L(vec![u32::from(shot.white)])),
    ]);
    t.put32(4, ifd0);
    t.buf
}

/// An uncompressed PEF and SRW decode to their exact CFA with the declared
/// pattern and levels, and develop; a vendor-compressed one is refused by
/// name.
#[test]
fn pef_and_srw_uncompressed_decode_exactly_and_compressed_ones_are_refused() {
    let gbrg = |x: u32, y: u32| [1u8, 2, 0, 1][((y % 2) * 2 + x % 2) as usize];
    for (make, name) in [
        ("PENTAX Corporation", "Pentax PEF"),
        ("SAMSUNG", "Samsung SRW"),
    ] {
        let shot = Shot::new(40, 32, 256, 4000);
        let file = generic_raw(make, &shot, 1);
        let m = super::read(&file, ImportLimits::default(), Want::Pixels)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(m.name, name);
        assert_eq!(m.samples, fx::mosaic(&shot, gbrg, scene(40, 32)), "{name}");
        assert_eq!(m.pattern.codes, vec![1, 2, 0, 1], "{name}");
        assert_eq!((m.black, m.white), ([256.0; 4], Some(4000.0)), "{name}");
        check(&file, 40, 32, name);
        refused(
            &generic_raw(make, &shot, 65535),
            &[name, "compression 65535"],
        );
    }
}

// ------------------------------------------------------------ refusals ----

/// CR3 is still refused by name (CRX is not implemented).
#[test]
fn cr3_is_refused_by_name() {
    let mut cr3 = vec![0, 0, 0, 24];
    cr3.extend_from_slice(b"ftypcrx \0\0\0\x01crx isom");
    cr3.extend([0u8; 64]);
    refused(&cr3, &["Canon CR3", "CRX"]);
}

// -------------------------------------------------------------- damage ----

fn every_synthetic_file() -> Vec<(&'static str, Vec<u8>)> {
    let mut cr2 = Cr2::default();
    cr2.shot = {
        let mut s = cr2.shot.clone();
        s.width = 48;
        s.height = 20;
        s.active = [2, 8, 20, 48];
        s
    };
    cr2.slices = Some([1, 24, 24]);
    let curve = fx::sony_curve(fx::SONY_CURVE);
    let top = curve[2047 * 2] as u16;
    vec![
        ("CR2", fx::cr2(&cr2, scene(40, 18))),
        (
            "NEF",
            fx::nef(
                &Shot::new(24, 12, 100, 4095),
                12,
                Store::Packed,
                scene(24, 12),
            ),
        ),
        (
            "ARW2",
            fx::arw(&Shot::new(32, 12, 512, top), true, scene(32, 12)).0,
        ),
        (
            "ARW",
            fx::arw(&Shot::new(24, 12, 512, 16383), false, scene(24, 12)).0,
        ),
        (
            "RAF",
            fx::raf(&Shot::new(24, 12, 512, 16383), true, true, scene(24, 12)),
        ),
        ("ORF", fx::orf(&Shot::new(24, 12, 64, 4095), scene(24, 12))),
        (
            "RW2",
            fx::rw2(&Shot::new(24, 12, 64, 4095), 2, scene(24, 12)),
        ),
        (
            "PEF",
            generic_raw("PENTAX", &Shot::new(24, 12, 64, 4000), 1),
        ),
    ]
}

/// Truncated and bit-flipped vendor RAWs are errors or images, never
/// panics, on both entry points and through the facade.
#[test]
fn damaged_vendor_raws_error_and_never_panic() {
    let mut seed = 0xD18;
    for (name, good) in every_synthetic_file() {
        // The undamaged file decodes (the damage below starts from a file
        // that works).
        develop(&good);
        let mut cases: Vec<Vec<u8>> = (0..good.len())
            .step_by(5)
            .map(|n| good[..n].to_vec())
            .collect();
        for _ in 0..500 {
            let mut bad = good.clone();
            for _ in 0..1 + lcg(&mut seed) % 4 {
                let at = (lcg(&mut seed) as usize) % bad.len();
                bad[at] ^= 1 << (lcg(&mut seed) % 8);
            }
            cases.push(bad);
        }
        for (i, bytes) in cases.iter().enumerate() {
            let outcome = std::panic::catch_unwind(|| {
                let _ = super::decode(bytes, ImportLimits::default());
                let _ = super::probe(bytes, ImportLimits::default());
                let _ = decode_surface_bytes(bytes, ImportLimits::default());
            });
            assert!(outcome.is_ok(), "{name} case {i} panicked");
        }
    }
}

/// Declared sizes are checked against the limits before any buffer.
#[test]
fn a_vendor_raw_too_large_for_the_limits_is_refused_before_allocating() {
    let tight = ImportLimits {
        max_alloc_bytes: 2048,
        ..ImportLimits::default()
    };
    for (name, file) in every_synthetic_file() {
        let err = decode_surface_bytes(&file, tight).unwrap_err();
        assert!(matches!(err, CodecError::LimitExceeded(_)), "{name}: {err}");
    }
}

// --------------------------------------------------------- real files ----

/// Not a gate: decodes every camera file in `RASTER_RAW_SAMPLES` (a
/// directory) and writes each developed image as a PNG next to it, for a
/// person to compare with the camera's own preview.
#[test]
#[ignore]
fn develop_real_camera_files_from_a_directory() {
    let Ok(dir) = std::env::var("RASTER_RAW_SAMPLES") else {
        return;
    };
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "png") {
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        let started = std::time::Instant::now();
        match decode_surface_bytes(&bytes, ImportLimits::default()) {
            Ok(s) => {
                let SurfacePixels::Rgba16(px) = &s.pixels else {
                    continue;
                };
                let rgba8: Vec<u8> = px.iter().map(|&v| (v >> 8) as u8).collect();
                let mean = [0, 1, 2].map(|c| {
                    rgba8
                        .iter()
                        .skip(c)
                        .step_by(4)
                        .map(|&v| u64::from(v))
                        .sum::<u64>()
                        / u64::from(s.width * s.height)
                });
                println!(
                    "{}: {}x{} {:?} in {:?}, mean RGB {mean:?}",
                    path.display(),
                    s.width,
                    s.height,
                    s.source_format,
                    started.elapsed()
                );
                let png = crate::codec::encode(
                    crate::codec::ExportFormat::Png,
                    s.width,
                    s.height,
                    &rgba8,
                )
                .unwrap();
                std::fs::write(path.with_extension("developed.png"), png).unwrap();
            }
            Err(e) => println!("{}: {e}", path.display()),
        }
    }
}
