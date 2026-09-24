//! W11-H: JPEG XL export (`zune-jpegxl`, lossless) read back by `jxl-oxide`,
//! and the facade/extension routes of every W11-H reader.

use crate::codec::{
    decode_surface_bytes, encode, encode_with, CodecError, EncodeOptions, EncodedPixels,
    ExportFormat, ImportFormat, ImportLimits, SurfacePixels,
};

fn noise8(w: u32, h: u32) -> Vec<u8> {
    let mut s = 0x2545_F491u32;
    (0..w * h * 4)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            (s >> 11) as u8
        })
        .collect()
}

#[test]
fn jpeg_xl_export_is_lossless_at_8_bits_and_reads_back() {
    // 300 wide: more than one 256-pixel group.
    for (w, h) in [(2u32, 2u32), (5, 3), (300, 7)] {
        let px = noise8(w, h);
        let bytes = encode(ExportFormat::Jxl, w, h, &px).unwrap();
        let s = decode_surface_bytes(&bytes, ImportLimits::default())
            .unwrap_or_else(|e| panic!("{w}x{h}: {e}"));
        assert_eq!(
            (s.width, s.height, s.source_format),
            (w, h, ImportFormat::Jxl)
        );
        assert_eq!(
            s.pixels,
            SurfacePixels::Rgba8(px),
            "{w}x{h} is not lossless"
        );
    }
}

/// `zune-jpegxl` 0.5's 16-bit RGBA stream reads back with every alpha at
/// 65535 (measured: the first version of this test compared the samples
/// and failed on alpha alone), so JPEG XL export is 8-bit and says so.
#[test]
fn jpeg_xl_refuses_16_bit_samples() {
    assert!(!ExportFormat::Jxl.supports_16_bit());
    let deep = vec![1000u16; 4 * 3 * 4];
    let err = encode_with(
        ExportFormat::Jxl,
        4,
        3,
        EncodedPixels::Rgba16(&deep),
        &EncodeOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(err, CodecError::InvalidParameter(_)), "{err}");
}

#[test]
fn jpeg_xl_refuses_a_one_pixel_edge_by_name_and_is_offered_as_writable() {
    let err = encode(ExportFormat::Jxl, 1, 4, &[0; 16]).unwrap_err();
    assert!(matches!(err, CodecError::InvalidParameter(_)), "{err}");
    assert!(err.to_string().contains("2x2"), "{err}");
    assert!(ExportFormat::writable().contains(&ExportFormat::Jxl));
    assert!(ExportFormat::writable().contains(&ExportFormat::Exr));
    assert!(ExportFormat::Jxl.reads_back() && ExportFormat::Exr.reads_back());
    assert_eq!(
        (ExportFormat::Jxl.extension(), ExportFormat::Exr.extension()),
        ("jxl", "exr")
    );
}

#[test]
fn every_new_reader_is_reached_by_extension_and_by_content() {
    for (ext, format) in [
        ("exr", ImportFormat::Exr),
        ("HDR", ImportFormat::Hdr),
        ("icns", ImportFormat::Icns),
        ("iff", ImportFormat::Iff),
        ("ilbm", ImportFormat::Iff),
        ("lbm", ImportFormat::Iff),
        ("kra", ImportFormat::Kra),
    ] {
        assert_eq!(ImportFormat::from_extension(ext), Some(format), "{ext}");
        assert!(format.is_decodable_here(), "{ext}");
        assert!(ImportFormat::ALL.contains(&format), "{ext}");
    }
    // An EXR saved under a `.png` name opens as the EXR it is, through the
    // path road File > Open takes.
    let dir = std::env::temp_dir().join(format!("w11h-formats-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let lying = dir.join("really_an_exr.png");
    std::fs::write(
        &lying,
        encode(ExportFormat::Exr, 3, 2, &noise8(3, 2)).unwrap(),
    )
    .unwrap();
    let s = crate::codec::decode_surface_path(&lying, ImportLimits::default()).unwrap();
    assert_eq!(s.source_format, ImportFormat::Exr);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A smooth 8-bit RGBA image (lossy codecs are judged on content like this,
/// not on noise), with alpha varying down the rows.
fn smooth8(w: u32, h: u32) -> Vec<u8> {
    (0..h)
        .flat_map(|y| {
            (0..w).flat_map(move |x| {
                [
                    (x * 255 / w.max(2).saturating_sub(1).max(1)) as u8,
                    (y * 255 / h.max(2).saturating_sub(1).max(1)) as u8,
                    ((x + y) * 2) as u8,
                    (255 - y * 2) as u8,
                ]
            })
        })
        .collect()
}

/// W11-H: lossy WebP export (`tiny-webp`) reads back through the ordinary
/// WebP decoder: the colour close, the alpha exact, and the quality knob
/// trading size for fidelity.
#[test]
fn lossy_webp_export_reads_back_close_with_exact_alpha() {
    let (w, h) = (64u32, 48u32);
    let px = smooth8(w, h);
    let psnr = |back: &[u8]| {
        let mut se = 0f64;
        let mut n = 0f64;
        for (a, b) in px.as_chunks::<4>().0.iter().zip(back.as_chunks::<4>().0) {
            for c in 0..3 {
                let d = f64::from(a[c]) - f64::from(b[c]);
                se += d * d;
                n += 1.0;
            }
        }
        10.0 * (255.0f64 * 255.0 / (se / n).max(1e-9)).log10()
    };
    let mut sizes = Vec::new();
    for q in [10u8, 90] {
        let bytes = encode(ExportFormat::WebPLossy(q), w, h, &px).unwrap();
        assert_eq!(&bytes[..4], b"RIFF");
        assert!(
            bytes.windows(4).any(|c| c == b"VP8 "),
            "a lossy VP8 stream, not VP8L"
        );
        let s = decode_surface_bytes(&bytes, ImportLimits::default()).unwrap();
        assert_eq!(
            (s.width, s.height, s.source_format),
            (w, h, ImportFormat::WebP)
        );
        let back = s.into_decoded_image().rgba8;
        for (i, (a, b)) in px.iter().zip(&back).enumerate().skip(3).step_by(4) {
            assert_eq!(a, b, "alpha of pixel {} is exact", i / 4);
        }
        let db = psnr(&back);
        assert!(db > if q == 90 { 34.0 } else { 24.0 }, "q{q}: {db:.1} dB");
        sizes.push(bytes.len());
    }
    assert!(sizes[0] < sizes[1], "quality trades size: {sizes:?}");
    // Lossy output is not lossless output.
    let lossless = encode(ExportFormat::WebP, w, h, &px).unwrap();
    assert!(!lossless.windows(4).any(|c| c == b"VP8 "));
}

#[test]
fn lossy_webp_refuses_what_it_cannot_write_without_panicking() {
    let px = smooth8(4, 4);
    for q in [0u8, 101] {
        let err = encode(ExportFormat::WebPLossy(q), 4, 4, &px).unwrap_err();
        assert!(matches!(err, CodecError::InvalidParameter(_)), "{err}");
    }
    let deep = vec![1000u16; 4 * 4 * 4];
    let err = encode_with(
        ExportFormat::WebPLossy(80),
        4,
        4,
        EncodedPixels::Rgba16(&deep),
        &EncodeOptions::default(),
    )
    .unwrap_err();
    assert!(matches!(err, CodecError::InvalidParameter(_)), "{err}");
    // VP8 carries 14-bit dimensions: a wider image is an error, not a panic.
    let wide = vec![0u8; 16_384 * 4];
    assert!(encode(ExportFormat::WebPLossy(80), 16_384, 1, &wide).is_err());
    // One pixel and odd sizes are fine.
    for (w, h) in [(1u32, 1u32), (3, 5), (17, 9)] {
        let bytes = encode(ExportFormat::WebPLossy(80), w, h, &smooth8(w, h)).unwrap();
        let s = decode_surface_bytes(&bytes, ImportLimits::default()).unwrap();
        assert_eq!((s.width, s.height), (w, h));
    }
    assert_eq!(ExportFormat::WebPLossy(80).extension(), "webp");
    assert!(ExportFormat::WebPLossy(80).reads_back());
    assert!(!ExportFormat::WebPLossy(80).supports_16_bit());
}
