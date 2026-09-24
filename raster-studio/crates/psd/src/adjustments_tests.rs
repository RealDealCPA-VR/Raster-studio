//! W11-A: every adjustment kind round-trips through its `.psd` payload, and
//! malformed payloads are refused rather than panicking.

use super::*;
use crate::model::{PsdFile, PsdLayer, Rect};

fn q(n: i32) -> f32 {
    n as f32 / 255.0
}

fn p(n: i32) -> f32 {
    n as f32 / 100.0
}

fn opts() -> ReadOptions {
    ReadOptions::default()
}

fn round_trip(kind: &AdjustmentKind) -> Decoded {
    let adjustment = encode(kind).unwrap_or_else(|e| panic!("{kind:?} did not encode: {e}"));
    decode(&adjustment, &opts()).unwrap_or_else(|e| panic!("{kind:?} did not decode: {e}"))
}

/// One of every kind a `.psd` can carry, with settings on the file's own
/// quantisation grid (so write -> read is exact).
fn every_kind() -> Vec<AdjustmentKind> {
    let lut_size = 2u32;
    let table: Vec<[f32; 3]> = (0..8)
        .map(|i| {
            [
                (i & 1) as f32,
                ((i >> 1) & 1) as f32 * 0.5,
                ((i >> 2) & 1) as f32 * 0.25,
            ]
        })
        .collect();
    vec![
        AdjustmentKind::Invert,
        AdjustmentKind::Levels {
            black: q(12),
            white: q(240),
            gamma: p(135),
        },
        AdjustmentKind::LevelsFull {
            composite: [q(5), q(250), p(90), q(10), q(245)],
            red: [q(20), q(255), p(100), q(0), q(255)],
            green: [0.0, 1.0, 1.0, 0.0, 1.0],
            blue: [q(0), q(200), p(120), q(0), q(255)],
        },
        AdjustmentKind::Curves {
            points: vec![[0.0, q(10)], [q(128), q(150)], [1.0, q(250)]],
        },
        AdjustmentKind::CurvesFull {
            composite: vec![[0.0, 0.0], [q(64), q(80)], [1.0, 1.0]],
            red: vec![[0.0, q(20)], [1.0, 1.0]],
            green: vec![[0.0, 0.0], [1.0, 1.0]],
            blue: vec![[0.0, 0.0], [q(100), q(90)], [1.0, q(230)]],
        },
        AdjustmentKind::BrightnessContrast {
            brightness: q(40),
            contrast: p(-30),
        },
        AdjustmentKind::HueSaturation {
            hue: 45.0,
            saturation: p(-20),
            lightness: p(15),
        },
        AdjustmentKind::HueSaturationFull {
            hue: -30.0,
            saturation: p(10),
            lightness: p(-5),
            colorize: Some([200.0, p(60), p(-10)]),
        },
        AdjustmentKind::ColorBalance {
            shadows: [p(10), p(-20), p(30)],
            midtones: [p(-40), p(50), p(0)],
            highlights: [p(5), p(6), p(-7)],
        },
        AdjustmentKind::ColorBalanceFull {
            shadows: [p(1), p(2), p(3)],
            midtones: [p(-4), p(-5), p(-6)],
            highlights: [p(70), p(-80), p(90)],
            preserve_luminosity: true,
        },
        AdjustmentKind::BlackAndWhite {
            weights: [p(40), p(60), p(40), p(60), p(20), p(80)],
            tint: None,
        },
        AdjustmentKind::PhotoFilter {
            color_srgb: [1.0, 0.0, 0.0],
            density: p(25),
            preserve_luminosity: true,
        },
        AdjustmentKind::ChannelMixer {
            rows: [
                [p(100), p(0), p(0), p(0)],
                [p(20), p(70), p(10), p(-5)],
                [p(-50), p(0), p(150), p(20)],
            ],
            monochrome: false,
        },
        AdjustmentKind::Posterize { levels: 6 },
        AdjustmentKind::Threshold { level: q(128) },
        AdjustmentKind::GradientMap {
            stops: vec![
                (0.0, [0.0, 0.0, 0.0]),
                (0.5, [1.0, 0.0, 0.0]),
                (1.0, [1.0, 1.0, 1.0]),
            ],
            reverse: true,
        },
        AdjustmentKind::SelectiveColor {
            ranges: [
                [p(10), p(-10), p(20), p(0)],
                [0.0; 4],
                [p(1), p(2), p(3), p(4)],
                [0.0; 4],
                [p(-100), p(100), p(0), p(50)],
                [0.0; 4],
                [0.0; 4],
                [p(5), p(5), p(5), p(5)],
                [0.0; 4],
            ],
            relative: false,
        },
        AdjustmentKind::Exposure { stops: 1.25 },
        AdjustmentKind::ExposureFull {
            stops: -0.5,
            offset: 0.03125,
            gamma: 1.5,
        },
        AdjustmentKind::Vibrance {
            vibrance: p(35),
            saturation: p(-15),
        },
        AdjustmentKind::ColorLookup {
            name: "Warm".into(),
            size: lut_size,
            table,
        },
    ]
}

#[test]
fn every_kind_round_trips_with_equal_parameters() {
    for kind in every_kind() {
        let back = round_trip(&kind);
        assert_eq!(back.kind, kind, "write -> read changed the parameters");
        assert!(back.unmapped.is_empty(), "{kind:?}: {:?}", back.unmapped);
    }
}

#[test]
fn every_kind_round_trips_through_a_whole_file() {
    let mut file = PsdFile::new(crate::header::PsdHeader::rgba8(2, 2));
    let kinds = every_kind();
    for (i, kind) in kinds.iter().enumerate() {
        let mut layer = PsdLayer::raster(format!("adj {i}"), Rect::default());
        layer.pixel_data_irrelevant = true;
        layer.adjustment = Some(encode(kind).unwrap());
        file.layers.push(layer);
    }
    let back = crate::read(&crate::write(&file).unwrap()).unwrap();
    assert_eq!(back.layers.len(), kinds.len());
    for (layer, kind) in back.layers.iter().zip(&kinds) {
        let adjustment = layer.adjustment.as_ref().expect("the adjustment survives");
        assert_eq!(&decode(adjustment, &opts()).unwrap().kind, kind);
    }
}

#[test]
fn a_tinted_black_and_white_keeps_its_tint_hue_and_saturation() {
    let kind = AdjustmentKind::BlackAndWhite {
        weights: [p(-50), p(300), p(0), p(100), p(-200), p(10)],
        tint: Some([30.0, 0.4]),
    };
    let AdjustmentKind::BlackAndWhite { weights, tint } = round_trip(&kind).kind else {
        panic!("not black and white")
    };
    assert_eq!(weights, [p(-50), p(300), p(0), p(100), p(-200), p(10)]);
    let [h, s] = tint.expect("the tint survives");
    assert!((h - 30.0).abs() < 1e-3 && (s - 0.4).abs() < 1e-3, "{h} {s}");
}

#[test]
fn off_grid_settings_land_within_one_step() {
    let kind = AdjustmentKind::Levels {
        black: 0.1,
        white: 0.9,
        gamma: 1.234,
    };
    let AdjustmentKind::Levels {
        black,
        white,
        gamma,
    } = round_trip(&kind).kind
    else {
        panic!("not levels")
    };
    assert!((black - 0.1).abs() <= 0.5 / 255.0 + 1e-6);
    assert!((white - 0.9).abs() <= 0.5 / 255.0 + 1e-6);
    assert!((gamma - 1.234).abs() <= 0.005 + 1e-6);
}

#[test]
fn kinds_without_a_psd_adjustment_layer_are_refused_by_name() {
    for kind in [
        AdjustmentKind::Desaturate,
        AdjustmentKind::Equalize,
        AdjustmentKind::Auto {
            mode: layer_model::AutoAdjustment::Tone,
            clip: 0.01,
        },
    ] {
        let err = encode(&kind).unwrap_err();
        assert!(err.reason.contains("no adjustment layer"), "{err}");
    }
}

#[test]
fn per_range_hue_saturation_settings_are_reported_not_dropped_silently() {
    let mut data = encode(&AdjustmentKind::HueSaturation {
        hue: 0.0,
        saturation: 0.0,
        lightness: 0.0,
    })
    .unwrap()
    .data;
    // The reds range's saturation setting: header 16 bytes, bounds 8, hue 2.
    data[16 + 8 + 2..16 + 8 + 4].copy_from_slice(&40i16.to_be_bytes());
    let decoded = decode(
        &Adjustment {
            key: *b"hue2",
            data,
        },
        &opts(),
    )
    .unwrap();
    assert_eq!(
        decoded.unmapped,
        vec!["per-colour-range hue/saturation settings".to_string()]
    );
}

/// A tiny deterministic generator, so the fuzz below is reproducible.
fn lcg(seed: &mut u64) -> u8 {
    *seed = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    (*seed >> 33) as u8
}

#[test]
fn malformed_payloads_are_errors_never_panics() {
    let mut seed = 0x5eed_u64;
    for kind in every_kind() {
        let good = encode(&kind).unwrap();
        // Every truncation of a valid payload.
        for cut in 0..good.data.len() {
            let adjustment = Adjustment {
                key: good.key,
                data: good.data[..cut].to_vec(),
            };
            let _ = decode(&adjustment, &opts());
        }
        // Byte flips across the valid payload.
        for _ in 0..200 {
            let mut data = good.data.clone();
            if data.is_empty() {
                break;
            }
            for _ in 0..3 {
                let at = usize::from(lcg(&mut seed)) * 7 % data.len();
                data[at] = lcg(&mut seed);
            }
            let _ = decode(
                &Adjustment {
                    key: good.key,
                    data,
                },
                &opts(),
            );
        }
        // Pure noise under the same key.
        for len in [1usize, 7, 64, 300] {
            let data: Vec<u8> = (0..len).map(|_| lcg(&mut seed)).collect();
            let _ = decode(
                &Adjustment {
                    key: good.key,
                    data,
                },
                &opts(),
            );
        }
    }
}

#[test]
fn named_malformations_come_back_as_errors_with_their_reason() {
    let cases: [(&[u8; 4], Vec<u8>, &str); 8] = [
        (b"levl", vec![0, 9], "levels version 9"),
        (b"levl", vec![0, 2, 0, 0], "RGB needs 4"),
        (b"curv", vec![0, 0, 0, 0, 0, 0, 0, 0], "curves version 0"),
        (b"curv", vec![0, 0, 1, 0, 0, 0, 1, 0, 40], "40 points"),
        (
            b"hue2",
            vec![0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0x7f, 0],
            "hue is",
        ),
        (b"post", vec![0, 1, 0, 0], "posterize levels is 1"),
        (b"phfl", vec![0, 3, 0, 0], "XYZ"),
        (b"SoCo", vec![], "not an adjustment"),
    ];
    for (key, data, want) in cases {
        let err = decode(&Adjustment { key: *key, data }, &opts()).unwrap_err();
        assert!(err.to_string().contains(want), "{err} lacks {want:?}");
    }
    // A gradient map whose stop count outruns its payload is refused before
    // anything is reserved for it.
    let mut s = Sink::new();
    s.u16(1);
    s.u16(0);
    s.unicode_string("x");
    s.u16(u16::MAX);
    let err = decode(
        &Adjustment {
            key: *b"grdm",
            data: s.into_inner(),
        },
        &opts(),
    )
    .unwrap_err();
    assert!(err.reason.contains("65535 colour stops"), "{err}");
}

#[test]
fn a_cube_lut_is_parsed_bounded() {
    assert!(parse_cube(b"LUT_3D_SIZE 2\n0 0 0\n").is_err());
    assert!(parse_cube(b"LUT_3D_SIZE 99999\n").is_err());
    assert!(parse_cube(b"0 0 0\nLUT_3D_SIZE 2\n").is_err());
    let mut ok = String::from("# c\nTITLE \"t\"\nLUT_3D_SIZE 2\n");
    for _ in 0..8 {
        ok.push_str("0.5 0.25 1\n");
    }
    let (size, table) = parse_cube(ok.as_bytes()).unwrap();
    assert_eq!((size, table.len(), table[0]), (2, 8, [0.5, 0.25, 1.0]));
}

/// Review round 2: a setting outside what the layout stores is refused by
/// name, never clamped into range and saved as a different value.
#[test]
fn settings_a_psd_cannot_spell_are_refused_not_clamped() {
    let cases: Vec<(AdjustmentKind, &str)> = vec![
        (
            AdjustmentKind::BrightnessContrast {
                brightness: 0.8,
                contrast: 0.0,
            },
            "brightness 0.8",
        ),
        (
            AdjustmentKind::BlackAndWhite {
                weights: [-2.5, 0.6, 0.4, 0.6, 0.2, 0.8],
                tint: None,
            },
            "the Rd weight -2.5",
        ),
        (
            AdjustmentKind::Posterize { levels: 256 },
            "posterize levels 256",
        ),
        (
            AdjustmentKind::Curves {
                points: vec![[0.0, 0.0], [0.001, 0.5], [0.003, 0.6], [1.0, 1.0]],
            },
            "collide",
        ),
        (
            AdjustmentKind::Levels {
                black: 0.0,
                white: 1.0,
                gamma: 10.0,
            },
            "levels gamma 10",
        ),
        (
            AdjustmentKind::Threshold { level: 0.0 },
            "threshold level 0",
        ),
        (
            AdjustmentKind::Vibrance {
                vibrance: f32::NAN,
                saturation: 0.0,
            },
            "not a finite number",
        ),
        (
            AdjustmentKind::ExposureFull {
                stops: 0.0,
                offset: 2.0,
                gamma: 1.0,
            },
            "offset 2",
        ),
        (
            AdjustmentKind::ColorLookup {
                name: "Broken".into(),
                size: 2,
                table: vec![
                    [0.0, 0.0, 0.0],
                    [1.0, 0.0, 0.0],
                    [0.0, 1.0, 0.0],
                    [1.0, 1.0, 0.0],
                    [0.0, 0.0, 1.0],
                    [1.0, f32::NAN, 1.0],
                    [0.0, 1.0, 1.0],
                    [1.0, 1.0, f32::INFINITY],
                ],
            },
            "LUT entry 5 component NaN is not a finite number",
        ),
    ];
    for (kind, reason) in cases {
        match encode(&kind) {
            Ok(adjustment) => panic!(
                "{kind:?} encoded (and would reopen as {:?}) instead of being refused",
                decode(&adjustment, &opts()).map(|d| d.kind)
            ),
            Err(e) => assert!(e.reason.contains(reason), "{kind:?}: {e}"),
        }
    }
}

type Make = fn(f32) -> AdjustmentKind;
type Read = fn(&AdjustmentKind) -> f32;

/// Scalar settings swept by the test below: how to build the kind, how to
/// read the setting back, and the storage step.
fn scalar_probes() -> Vec<(&'static str, Make, Read, f32)> {
    vec![
        (
            "brightness",
            |v| AdjustmentKind::BrightnessContrast {
                brightness: v,
                contrast: 0.0,
            },
            |k| match k {
                AdjustmentKind::BrightnessContrast { brightness, .. } => *brightness,
                _ => f32::NAN,
            },
            1.0 / 255.0,
        ),
        (
            "contrast",
            |v| AdjustmentKind::BrightnessContrast {
                brightness: 0.0,
                contrast: v,
            },
            |k| match k {
                AdjustmentKind::BrightnessContrast { contrast, .. } => *contrast,
                _ => f32::NAN,
            },
            0.01,
        ),
        (
            "b&w weight",
            |v| AdjustmentKind::BlackAndWhite {
                weights: [v, 0.6, 0.4, 0.6, 0.2, 0.8],
                tint: None,
            },
            |k| match k {
                AdjustmentKind::BlackAndWhite { weights, .. } => weights[0],
                _ => f32::NAN,
            },
            0.01,
        ),
        (
            "mixer",
            |v| AdjustmentKind::ChannelMixer {
                rows: [
                    [v, 0.0, 0.0, 0.0],
                    [0.0, 1.0, 0.0, 0.0],
                    [0.0, 0.0, 1.0, 0.0],
                ],
                monochrome: false,
            },
            |k| match k {
                AdjustmentKind::ChannelMixer { rows, .. } => rows[0][0],
                _ => f32::NAN,
            },
            0.01,
        ),
        (
            "levels gamma",
            |v| AdjustmentKind::Levels {
                black: 0.0,
                white: 1.0,
                gamma: v,
            },
            |k| match k {
                AdjustmentKind::Levels { gamma, .. } => *gamma,
                _ => f32::NAN,
            },
            0.01,
        ),
        (
            "threshold",
            |v| AdjustmentKind::Threshold { level: v },
            |k| match k {
                AdjustmentKind::Threshold { level } => *level,
                _ => f32::NAN,
            },
            1.0 / 255.0,
        ),
        (
            "vibrance",
            |v| AdjustmentKind::Vibrance {
                vibrance: v,
                saturation: 0.0,
            },
            |k| match k {
                AdjustmentKind::Vibrance { vibrance, .. } => *vibrance,
                _ => f32::NAN,
            },
            0.01,
        ),
        (
            "colour balance",
            |v| AdjustmentKind::ColorBalance {
                shadows: [v, 0.0, 0.0],
                midtones: [0.0; 3],
                highlights: [0.0; 3],
            },
            |k| match k {
                AdjustmentKind::ColorBalance { shadows, .. }
                | AdjustmentKind::ColorBalanceFull { shadows, .. } => shadows[0],
                _ => f32::NAN,
            },
            0.01,
        ),
        (
            "hue/saturation saturation",
            |v| AdjustmentKind::HueSaturation {
                hue: 0.0,
                saturation: v,
                lightness: 0.0,
            },
            |k| match k {
                AdjustmentKind::HueSaturation { saturation, .. } => *saturation,
                _ => f32::NAN,
            },
            0.01,
        ),
        (
            "exposure stops",
            |v| AdjustmentKind::Exposure { stops: v },
            |k| match k {
                AdjustmentKind::Exposure { stops } => *stops,
                _ => f32::NAN,
            },
            0.0,
        ),
    ]
}

/// Whatever `encode` accepts decodes again, and lands within half a storage
/// step of what was written: a sweep across and past every range.
#[test]
fn whatever_encode_accepts_decodes_again_within_half_a_step() {
    let values = [
        -400.0f32, -3.0, -2.5, -2.0, -1.5, -1.0, -0.8, -0.5, -0.1, 0.0, 0.001, 0.3, 0.5, 0.588,
        0.6, 0.8, 1.0, 1.5, 2.0, 2.5, 3.0, 10.0, 30.0,
    ];
    for (name, make, read, step) in scalar_probes() {
        let mut accepted = 0;
        for v in values {
            let Ok(adjustment) = encode(&make(v)) else {
                continue;
            };
            accepted += 1;
            let back = decode(&adjustment, &opts())
                .unwrap_or_else(|e| panic!("{name} {v}: encoded but does not decode: {e}"));
            let got = read(&back.kind);
            assert!(
                (got - v).abs() <= step / 2.0 + 1e-5,
                "{name}: wrote {v}, reads back {got}"
            );
        }
        // Not vacuous: the in-range values were written and checked.
        assert!(accepted >= 3, "{name}: only {accepted} values encoded");
    }
    for levels in 0..=300u32 {
        if let Ok(adjustment) = encode(&AdjustmentKind::Posterize { levels }) {
            assert_eq!(
                decode(&adjustment, &opts()).unwrap().kind,
                AdjustmentKind::Posterize { levels },
            );
        }
    }
    // Curves whose points often sit closer together than 1/255.
    let mut seed = 0xc0ffee_u64;
    for _ in 0..500 {
        let n = 2 + usize::from(lcg(&mut seed)) % 6;
        let mut x = 0.0f32;
        let mut points = Vec::new();
        for _ in 0..n {
            points.push([x.min(1.0), f32::from(lcg(&mut seed)) / 255.0]);
            x += f32::from(lcg(&mut seed) % 8) / 1024.0 + 1e-4;
        }
        let Ok(adjustment) = encode(&AdjustmentKind::Curves {
            points: points.clone(),
        }) else {
            continue;
        };
        let back = decode(&adjustment, &opts())
            .unwrap_or_else(|e| panic!("{points:?}: encoded but does not decode: {e}"));
        let AdjustmentKind::Curves { points: got } = back.kind else {
            panic!("not curves")
        };
        assert_eq!(got.len(), points.len());
        for (a, b) in got.iter().zip(&points) {
            assert!((a[0] - b[0]).abs() <= 0.5 / 255.0 + 1e-6, "{points:?}");
            assert!((a[1] - b[1]).abs() <= 0.5 / 255.0 + 1e-6, "{points:?}");
        }
    }
}

/// Review round 2: a version-4 `curv` header is a curve count (the curves
/// following in channel order), not a channel bitmap. Read as a bitmap, a
/// count of 2 would put the one curve on the red channel alone.
#[test]
fn a_version_four_curves_header_is_a_count_of_curves_in_channel_order() {
    let mut data = vec![0u8];
    data.extend_from_slice(&4u16.to_be_bytes());
    data.extend_from_slice(&2u32.to_be_bytes());
    for points in [[(10i16, 0i16), (245, 255)], [(20, 0), (255, 255)]] {
        data.extend_from_slice(&2u16.to_be_bytes());
        for (output, input) in points {
            data.extend_from_slice(&output.to_be_bytes());
            data.extend_from_slice(&input.to_be_bytes());
        }
    }
    let decoded = decode(
        &Adjustment {
            key: *b"curv",
            data,
        },
        &opts(),
    )
    .unwrap();
    assert_eq!(
        decoded.kind,
        AdjustmentKind::CurvesFull {
            composite: vec![[0.0, q(10)], [1.0, q(245)]],
            red: vec![[0.0, q(20)], [1.0, 1.0]],
            green: vec![[0.0, 0.0], [1.0, 1.0]],
            blue: vec![[0.0, 0.0], [1.0, 1.0]],
        }
    );
    // A count the payload cannot hold is refused before anything is read.
    let mut huge = vec![0u8];
    huge.extend_from_slice(&4u16.to_be_bytes());
    huge.extend_from_slice(&u32::MAX.to_be_bytes());
    let err = decode(
        &Adjustment {
            key: *b"curv",
            data: huge,
        },
        &opts(),
    )
    .unwrap_err();
    assert!(err.reason.contains("declares"), "{err}");
}
