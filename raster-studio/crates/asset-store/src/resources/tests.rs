//! W9-N: every resource format, from fixtures built here byte by byte, and
//! the untrusted-input promise — a truncated or corrupted file is an error or
//! a smaller library, never a panic.

use psd::bytes::Sink;
use psd::pattern::{encode_block, PsdPattern};
use psd::{Descriptor, Value};

use super::*;

// ------------------------------------------------------------- fixtures

fn aco_v1(colors: &[(u16, [u16; 4])]) -> Sink {
    let mut s = Sink::new();
    s.u16(1);
    s.u16(colors.len() as u16);
    for (space, c) in colors {
        s.u16(*space);
        for v in c {
            s.u16(*v);
        }
    }
    s
}

fn aco_v1_v2(colors: &[(u16, [u16; 4], &str)]) -> Vec<u8> {
    let plain: Vec<_> = colors.iter().map(|(s, c, _)| (*s, *c)).collect();
    let mut s = aco_v1(&plain);
    s.u16(2);
    s.u16(colors.len() as u16);
    for (space, c, name) in colors {
        s.u16(*space);
        for v in c {
            s.u16(*v);
        }
        s.unicode_string(name);
    }
    s.into_inner()
}

fn ase(entries: &[(&str, &[u8; 4], &[f32])]) -> Vec<u8> {
    let mut s = Sink::new();
    s.tag(b"ASEF");
    s.u16(1);
    s.u16(0);
    s.u32(entries.len() as u32 + 2);
    // A group around everything, as Illustrator writes one.
    let mut group = Sink::new();
    group.u16(3);
    for u in "Grp".encode_utf16() {
        group.u16(u);
    }
    group.u16(0);
    s.u16(0xC001);
    s.u32(group.len() as u32);
    s.bytes(group.as_slice());
    for (name, model, values) in entries {
        let mut e = Sink::new();
        let units: Vec<u16> = name.encode_utf16().collect();
        e.u16(units.len() as u16 + 1);
        for u in units {
            e.u16(u);
        }
        e.u16(0);
        e.tag(model);
        for v in *values {
            e.u32(v.to_bits());
        }
        e.u16(2); // normal swatch
        s.u16(0x0001);
        s.u32(e.len() as u32);
        s.bytes(e.as_slice());
    }
    s.u16(0xC002);
    s.u32(0);
    s.into_inner()
}

/// A minimal but well-formed RGB display profile with a v2 `desc` tag.
fn icc_profile(description: &str) -> Vec<u8> {
    let mut desc = Vec::new();
    desc.extend_from_slice(b"desc");
    desc.extend_from_slice(&[0; 4]);
    desc.extend_from_slice(&(description.len() as u32 + 1).to_be_bytes());
    desc.extend_from_slice(description.as_bytes());
    desc.push(0);
    let tag_offset = 128 + 4 + 12;
    let total = tag_offset + desc.len();
    let mut p = vec![0u8; 128];
    p[0..4].copy_from_slice(&(total as u32).to_be_bytes());
    p[8] = 2; // version 2
    p[12..16].copy_from_slice(b"mntr");
    p[16..20].copy_from_slice(b"RGB ");
    p[20..24].copy_from_slice(b"XYZ ");
    p[36..40].copy_from_slice(b"acsp");
    p.extend_from_slice(&1u32.to_be_bytes());
    p.extend_from_slice(b"desc");
    p.extend_from_slice(&(tag_offset as u32).to_be_bytes());
    p.extend_from_slice(&(desc.len() as u32).to_be_bytes());
    p.extend_from_slice(&desc);
    p
}

fn psd_pattern(name: &str, w: u32, h: u32, seed: u8) -> PsdPattern {
    let rgba8 = (0..w * h)
        .flat_map(|i| {
            let v = (i as u8).wrapping_mul(37).wrapping_add(seed);
            [v, 255 - v, seed, if i % 3 == 0 { 128 } else { 255 }]
        })
        .collect();
    PsdPattern {
        name: name.to_string(),
        id: format!("id-{name}"),
        width: w,
        height: h,
        rgba8,
    }
}

/// A Photoshop `.pat`. `prefixed` keeps the `Patt` block's per-pattern
/// length and padding; otherwise they are stripped.
fn photoshop_pat(patterns: &[PsdPattern], prefixed: bool) -> Vec<u8> {
    let block = encode_block(patterns);
    let mut out = Vec::new();
    out.extend_from_slice(b"8BPT");
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&(patterns.len() as u32).to_be_bytes());
    if prefixed {
        out.extend_from_slice(&block);
    } else {
        let mut at = 0;
        while at + 4 <= block.len() {
            let len = u32::from_be_bytes(block[at..at + 4].try_into().unwrap()) as usize;
            out.extend_from_slice(&block[at + 4..at + 4 + len]);
            at = (at + 4 + len).next_multiple_of(4);
        }
    }
    out
}

fn gimp_pat(name: &str, w: u32, h: u32, depth: u32, pixels: &[u8]) -> Vec<u8> {
    let header = 24 + name.len() as u32 + 1;
    let mut out = Vec::new();
    for v in [header, 1, w, h, depth] {
        out.extend_from_slice(&v.to_be_bytes());
    }
    out.extend_from_slice(b"GPAT");
    out.extend_from_slice(name.as_bytes());
    out.push(0);
    out.extend_from_slice(pixels);
    out
}

fn obj(class: &str, items: Vec<(&str, Value)>) -> Value {
    let mut d = Descriptor::new(class);
    for (k, v) in items {
        d.push(k, v).unwrap();
    }
    Value::Descriptor(d)
}

fn rgb_color(r: f64, g: f64, b: f64) -> Value {
    obj(
        "RGBC",
        vec![
            ("Rd  ", Value::Double(r)),
            ("Grn ", Value::Double(g)),
            ("Bl  ", Value::Double(b)),
        ],
    )
}

fn color_stop(color: Value, kind: &str, location: i32, midpoint: i32) -> Value {
    obj(
        "Clrt",
        vec![
            ("Clr ", color),
            (
                "Type",
                Value::Enumerated {
                    type_id: "Clry".into(),
                    value: kind.into(),
                },
            ),
            ("Lctn", Value::Integer(location)),
            ("Mdpn", Value::Integer(midpoint)),
        ],
    )
}

fn opacity_stop(percent: f64, location: i32) -> Value {
    obj(
        "TrnS",
        vec![
            (
                "Opct",
                Value::UnitFloat {
                    unit: *b"#Prc",
                    value: percent,
                },
            ),
            ("Lctn", Value::Integer(location)),
            ("Mdpn", Value::Integer(50)),
        ],
    )
}

fn gradient(name: &str, form: &str, colors: Vec<Value>, opacities: Vec<Value>) -> Value {
    let inner = obj(
        "Grdn",
        vec![
            ("Nm  ", Value::Text(format!("{name}\0"))),
            (
                "GrdF",
                Value::Enumerated {
                    type_id: "GrdF".into(),
                    value: form.into(),
                },
            ),
            ("Intr", Value::Double(2048.0)),
            ("Clrs", Value::List(colors)),
            ("Trns", Value::List(opacities)),
        ],
    );
    obj("Grdn", vec![("Grad", inner)])
}

fn grd(gradients: Vec<Value>) -> Vec<u8> {
    let mut root = Descriptor::new("null");
    root.push("GrdL", Value::List(gradients)).unwrap();
    let mut s = Sink::new();
    s.tag(b"8BGR");
    s.u16(5);
    s.u32(16);
    root.write(&mut s).unwrap();
    s.into_inner()
}

/// A fixed-point path coordinate.
fn fx(v: f64) -> i32 {
    (v * f64::from(1u32 << 24)).round() as i32
}

/// One 26-byte path record: `selector`, what `fill` writes, zero padding.
fn record(sink: &mut Sink, selector: u16, fill: &dyn Fn(&mut Sink)) {
    let start = sink.len();
    sink.u16(selector);
    fill(sink);
    let used = sink.len() - start;
    sink.zeros(26 - used);
}

/// A `.csh` holding one closed square (0,0)-(2,1) per name, plus one shape
/// whose records are garbage.
fn csh(names: &[&str], with_broken: bool) -> Vec<u8> {
    let mut s = Sink::new();
    s.tag(b"cush");
    s.u32(2);
    s.u32(names.len() as u32 + u32::from(with_broken));
    let square = [[0.0, 0.0], [2.0, 0.0], [2.0, 1.0], [0.0, 1.0]];
    for (i, name) in names
        .iter()
        .copied()
        .chain(with_broken.then_some("Broken"))
        .enumerate()
    {
        s.unicode_string(name);
        s.align_to(4);
        s.u32(1);
        let body = s.begin_len();
        s.pascal_string(&format!("shape-{i}"), 1);
        for v in [0u32, 0, 1, 2] {
            s.u32(v);
        }
        record(&mut s, 6, &|_| {});
        if name == "Broken" {
            // A knot with no subpath before it.
            record(&mut s, 1, &|_| {});
        } else {
            record(&mut s, 0, &|k| k.u16(4));
            for [x, y] in square {
                record(&mut s, 2, &|k| {
                    for _ in 0..3 {
                        k.i32(fx(y));
                        k.i32(fx(x));
                    }
                });
            }
        }
        s.end_len(body);
    }
    s.into_inner()
}

/// Every truncation, and a spread of single-byte corruptions, of `bytes`:
/// parsing must return (Ok or Err) every time.
fn survives_damage(kind: ResourceKind, bytes: &[u8]) {
    for cut in 0..bytes.len() {
        let _ = parse(kind, &bytes[..cut]);
    }
    let step = (bytes.len() / 97).max(1);
    for at in (0..bytes.len()).step_by(step) {
        for flip in [0xFFu8, 0x80, 0x01] {
            let mut damaged = bytes.to_vec();
            damaged[at] ^= flip;
            let _ = parse(kind, &damaged);
        }
    }
}

fn close(a: [f32; 4], b: [f32; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.02)
}

// ---------------------------------------------------------------- swatches

#[test]
fn aco_version_1_reads_every_supported_space_and_names_them_by_number() {
    let bytes = aco_v1(&[
        (0, [65535, 0, 0, 0]),         // RGB red
        (1, [21845, 65535, 65535, 0]), // HSB 120°: green
        (2, [0, 65535, 65535, 65535]), // CMYK: full cyan, no other ink
        (8, [10000, 0, 0, 0]),         // Grey: full ink = black
        (7, [10000, 0, 0, 0]),         // Lab L=100: white
    ])
    .into_inner();
    let Resource::Swatches(loaded) = parse(ResourceKind::SwatchesAco, &bytes).unwrap() else {
        panic!("not swatches")
    };
    let rgba: Vec<_> = loaded.items.iter().map(|s| s.rgba).collect();
    assert!(close(rgba[0], [1.0, 0.0, 0.0, 1.0]), "{:?}", rgba[0]);
    assert!(close(rgba[1], [0.0, 1.0, 0.0, 1.0]), "{:?}", rgba[1]);
    assert!(close(rgba[2], [0.0, 1.0, 1.0, 1.0]), "{:?}", rgba[2]);
    assert!(close(rgba[3], [0.0, 0.0, 0.0, 1.0]), "{:?}", rgba[3]);
    assert!(close(rgba[4], [1.0, 1.0, 1.0, 1.0]), "{:?}", rgba[4]);
    assert_eq!(loaded.items[0].name, "Swatch 1");
    assert!(loaded.refused.is_empty());
}

#[test]
fn aco_version_2_names_win_and_a_colour_book_is_refused_by_name() {
    let bytes = aco_v1_v2(&[
        (0, [0, 0, 65535, 0], "Deep Blue"),
        (3, [1, 2, 3, 4], "Pantone thing"),
    ]);
    let Resource::Swatches(loaded) = parse(ResourceKind::SwatchesAco, &bytes).unwrap() else {
        panic!("not swatches")
    };
    assert_eq!(loaded.items.len(), 1);
    assert_eq!(loaded.items[0].name, "Deep Blue");
    assert!(close(loaded.items[0].rgba, [0.0, 0.0, 1.0, 1.0]));
    assert_eq!(loaded.refused.len(), 1);
    assert!(
        loaded.refused[0].contains("Pantone thing"),
        "{:?}",
        loaded.refused
    );
}

#[test]
fn ase_reads_rgb_cmyk_lab_and_grey_entries_and_skips_groups() {
    let bytes = ase(&[
        ("Orange", b"RGB ", &[1.0, 0.5, 0.0]),
        ("Magenta ink", b"CMYK", &[0.0, 1.0, 0.0, 0.0]),
        ("Mid grey", b"Gray", &[0.5]),
        ("Lab black", b"LAB ", &[0.0, 0.0, 0.0]),
    ]);
    let Resource::Swatches(loaded) = parse(ResourceKind::SwatchesAse, &bytes).unwrap() else {
        panic!("not swatches")
    };
    let names: Vec<_> = loaded.items.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["Orange", "Magenta ink", "Mid grey", "Lab black"]);
    assert!(close(loaded.items[0].rgba, [1.0, 0.5, 0.0, 1.0]));
    assert!(close(loaded.items[1].rgba, [1.0, 0.0, 1.0, 1.0]));
    assert!(close(loaded.items[2].rgba, [0.5, 0.5, 0.5, 1.0]));
    assert!(close(loaded.items[3].rgba, [0.0, 0.0, 0.0, 1.0]));
}

#[test]
fn swatch_files_that_lie_error_instead_of_panicking() {
    // A count far past what the file holds: refused, not reserved.
    let mut lying = Sink::new();
    lying.u16(1);
    lying.u16(u16::MAX);
    assert!(parse(ResourceKind::SwatchesAco, lying.as_slice()).is_err());
    assert!(
        parse(ResourceKind::SwatchesAco, &[0, 9]).is_err(),
        "unknown version"
    );
    assert!(parse(ResourceKind::SwatchesAse, b"ASEX\0\x01\0\0").is_err());
    let mut many = Sink::new();
    many.tag(b"ASEF");
    many.u16(1);
    many.u16(0);
    many.u32(u32::MAX);
    assert!(matches!(
        parse(ResourceKind::SwatchesAse, many.as_slice()),
        Err(ResourceError::LimitExceeded { .. })
    ));
    survives_damage(
        ResourceKind::SwatchesAco,
        &aco_v1_v2(&[(0, [1, 2, 3, 0], "A"), (2, [4, 5, 6, 7], "B")]),
    );
    survives_damage(
        ResourceKind::SwatchesAse,
        &ase(&[
            ("A", b"RGB ", &[0.1, 0.2, 0.3]),
            ("B", b"CMYK", &[0.1, 0.2, 0.3, 0.4]),
        ]),
    );
}

// ---------------------------------------------------------------- patterns

#[test]
fn photoshop_patterns_read_in_both_spellings() {
    let patterns = [
        psd_pattern("Bricks", 3, 2, 9),
        psd_pattern("Dots", 2, 4, 200),
    ];
    for prefixed in [false, true] {
        let bytes = photoshop_pat(&patterns, prefixed);
        let Resource::Patterns(loaded) = parse(ResourceKind::Patterns, &bytes).unwrap() else {
            panic!("not patterns")
        };
        assert!(loaded.refused.is_empty(), "{:?}", loaded.refused);
        assert_eq!(loaded.items.len(), 2, "prefixed={prefixed}");
        for (got, want) in loaded.items.iter().zip(&patterns) {
            assert_eq!(got.name, want.name);
            assert_eq!((got.width, got.height), (want.width, want.height));
            assert_eq!(
                got.rgba8, want.rgba8,
                "{} pixels, prefixed={prefixed}",
                want.name
            );
        }
    }
}

/// Length-prefixed patterns padded to a four-byte *file* offset (the
/// 10-byte header puts that two bytes away from the block's own alignment).
#[test]
fn length_prefixed_patterns_padded_to_the_file_offset_read() {
    let patterns = [psd_pattern("A", 3, 1, 1), psd_pattern("B", 1, 2, 2)];
    let stripped = photoshop_pat(&patterns, false);
    let mut bodies = Vec::new();
    let mut cur = psd::bytes::Cursor::new(&stripped[10..]);
    for _ in 0..2 {
        let start = cur.pos();
        cur.skip(12).unwrap();
        let _ = cur.unicode_string(64).unwrap();
        let _ = cur.pascal_string(1).unwrap();
        cur.skip(4).unwrap();
        let list = cur.u32().unwrap() as usize;
        cur.skip(list).unwrap();
        bodies.push(stripped[10 + start..10 + cur.pos()].to_vec());
    }
    let mut out = stripped[..10].to_vec();
    for body in &bodies {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(body);
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
    }
    let Resource::Patterns(loaded) = parse(ResourceKind::Patterns, &out).unwrap() else {
        panic!("not patterns")
    };
    let names: Vec<_> = loaded.items.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["A", "B"], "{:?}", loaded.refused);
}

#[test]
fn a_gimp_pattern_reads_at_every_depth() {
    let grey = gimp_pat("Grey", 2, 1, 1, &[10, 20]);
    let Resource::Patterns(loaded) = parse(ResourceKind::Patterns, &grey).unwrap() else {
        panic!("not patterns")
    };
    assert_eq!(loaded.items[0].name, "Grey");
    assert_eq!(loaded.items[0].rgba8, [10, 10, 10, 255, 20, 20, 20, 255]);
    let rgba = gimp_pat("Rgba", 1, 1, 4, &[1, 2, 3, 4]);
    let Resource::Patterns(loaded) = parse(ResourceKind::Patterns, &rgba).unwrap() else {
        panic!("not patterns")
    };
    assert_eq!(loaded.items[0].rgba8, [1, 2, 3, 4]);
}

#[test]
fn pattern_files_that_lie_error_instead_of_panicking() {
    assert!(matches!(
        parse(ResourceKind::Patterns, b"not a pattern file at all, no"),
        Err(ResourceError::BadSignature { .. })
    ));
    // A GIMP header claiming a vast canvas is refused before allocating.
    let huge = gimp_pat("Huge", 1 << 20, 1 << 20, 4, &[]);
    assert!(matches!(
        parse(ResourceKind::Patterns, &huge),
        Err(ResourceError::LimitExceeded { .. })
    ));
    // A count of patterns the file does not hold.
    let mut bytes = photoshop_pat(&[psd_pattern("One", 2, 2, 1)], false);
    bytes[6..10].copy_from_slice(&3u32.to_be_bytes());
    let Resource::Patterns(loaded) = parse(ResourceKind::Patterns, &bytes).unwrap() else {
        panic!("not patterns")
    };
    assert_eq!(loaded.items.len(), 1, "the good one still loads");
    assert!(!loaded.refused.is_empty(), "the missing ones are named");
    survives_damage(
        ResourceKind::Patterns,
        &photoshop_pat(
            &[psd_pattern("A", 3, 3, 5), psd_pattern("B", 2, 2, 7)],
            false,
        ),
    );
    survives_damage(
        ResourceKind::Patterns,
        &photoshop_pat(&[psd_pattern("A", 3, 3, 5)], true),
    );
    survives_damage(ResourceKind::Patterns, &gimp_pat("G", 2, 2, 3, &[7; 12]));
}

// --------------------------------------------------------------- gradients

#[test]
fn a_version_5_gradient_file_reads_stops_opacity_and_smoothness() {
    let bytes = grd(vec![
        gradient(
            "Red to clear",
            "CstS",
            vec![
                color_stop(rgb_color(255.0, 0.0, 0.0), "UsrS", 0, 50),
                color_stop(rgb_color(0.0, 0.0, 255.0), "UsrS", 4096, 25),
            ],
            vec![opacity_stop(100.0, 0), opacity_stop(0.0, 4096)],
        ),
        gradient(
            "Foreground to background",
            "CstS",
            vec![
                color_stop(rgb_color(1.0, 2.0, 3.0), "FrgC", 0, 50),
                color_stop(rgb_color(1.0, 2.0, 3.0), "BckC", 4096, 50),
            ],
            vec![],
        ),
        gradient("Noise", "ClNs", vec![], vec![]),
    ]);
    let Resource::Gradients(loaded) = parse(ResourceKind::Gradients, &bytes).unwrap() else {
        panic!("not gradients")
    };
    assert_eq!(loaded.items.len(), 2);
    let g = &loaded.items[0];
    assert_eq!(g.name, "Red to clear");
    assert!((g.smoothness - 0.5).abs() < 1e-6);
    assert_eq!(g.stops.len(), 2);
    assert_eq!(g.stops[0].rgb, [1.0, 0.0, 0.0]);
    assert_eq!(g.stops[1].rgb, [0.0, 0.0, 1.0]);
    assert_eq!((g.stops[1].position, g.stops[1].midpoint), (1.0, 0.25));
    assert_eq!(g.opacity_stops[1].opacity, 0.0);
    let fb = &loaded.items[1];
    assert_eq!(fb.stops[0].rgb, [0.0, 0.0, 0.0], "foreground is black");
    assert_eq!(fb.stops[1].rgb, [1.0, 1.0, 1.0], "background is white");
    assert_eq!(loaded.refused.len(), 1);
    assert!(loaded.refused[0].contains("Noise"), "{:?}", loaded.refused);
}

#[test]
fn gradient_files_that_lie_error_instead_of_panicking() {
    let mut v3 = b"8BGR".to_vec();
    v3.extend_from_slice(&3u16.to_be_bytes());
    assert!(matches!(
        parse(ResourceKind::Gradients, &v3),
        Err(ResourceError::Unsupported { .. })
    ));
    assert!(parse(ResourceKind::Gradients, b"8BGX").is_err());
    let only_noise = grd(vec![gradient("N", "ClNs", vec![], vec![])]);
    assert!(
        parse(ResourceKind::Gradients, &only_noise).is_err(),
        "nothing usable"
    );
    survives_damage(
        ResourceKind::Gradients,
        &grd(vec![gradient(
            "G",
            "CstS",
            vec![color_stop(rgb_color(1.0, 2.0, 3.0), "UsrS", 0, 50)],
            vec![opacity_stop(50.0, 10)],
        )]),
    );
}

// ------------------------------------------------------------ custom shapes

#[test]
fn custom_shapes_read_their_paths_and_a_broken_one_is_refused_by_name() {
    let bytes = csh(&["Box", "Other box"], true);
    let Resource::Shapes(loaded) = parse(ResourceKind::Shapes, &bytes).unwrap() else {
        panic!("not shapes")
    };
    assert_eq!(loaded.items.len(), 2);
    let shape = &loaded.items[0];
    assert_eq!(shape.name, "Box");
    assert_eq!(shape.id, "shape-0");
    assert_eq!(shape.subpaths.len(), 1);
    assert!(shape.subpaths[0].closed);
    assert_eq!(shape.subpaths[0].knots[2].anchor, [2.0, 1.0]);
    // Normalised to the unit square: x spans 0..2 and y 0..1 in the file.
    assert_eq!(
        shape.unit_svg_path(),
        "M0 0 C0 0 1 0 1 0 C1 0 1 1 1 1 C1 1 0 1 0 1 C0 1 0 0 0 0 Z"
    );
    assert_eq!(loaded.refused.len(), 1);
    assert!(
        loaded.refused[0].starts_with("Broken"),
        "{:?}",
        loaded.refused
    );
}

#[test]
fn custom_shape_files_that_lie_error_instead_of_panicking() {
    assert!(parse(ResourceKind::Shapes, b"cush\0\0\0\x03").is_err());
    let mut many = b"cush".to_vec();
    many.extend_from_slice(&2u32.to_be_bytes());
    many.extend_from_slice(&u32::MAX.to_be_bytes());
    assert!(matches!(
        parse(ResourceKind::Shapes, &many),
        Err(ResourceError::LimitExceeded { .. })
    ));
    survives_damage(ResourceKind::Shapes, &csh(&["A", "B"], true));
}

// ------------------------------------------------------------ ICC profiles

#[test]
fn an_icc_profile_is_checked_and_described() {
    let bytes = icc_profile("Test RGB");
    let Resource::Icc(icc) = parse(ResourceKind::IccProfile, &bytes).unwrap() else {
        panic!("not a profile")
    };
    assert!(icc.is_rgb());
    assert_eq!(&icc.device_class, b"mntr");
    assert_eq!(icc.description.as_deref(), Some("Test RGB"));
    assert_eq!(icc.bytes, bytes);
    assert_eq!(
        ResourceKind::from_extension("ICM"),
        Some(ResourceKind::IccProfile)
    );
}

#[test]
fn icc_files_that_lie_error_instead_of_panicking() {
    let good = icc_profile("X");
    let mut no_sig = good.clone();
    no_sig[36] = b'x';
    assert!(parse(ResourceKind::IccProfile, &no_sig).is_err());
    let mut too_long = good.clone();
    too_long[0..4].copy_from_slice(&(good.len() as u32 + 1).to_be_bytes());
    assert!(parse(ResourceKind::IccProfile, &too_long).is_err());
    let mut tag_past_end = good.clone();
    tag_past_end[136..140].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(parse(ResourceKind::IccProfile, &tag_past_end).is_err());
    let mut tags = good.clone();
    tags[128..132].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(parse(ResourceKind::IccProfile, &tags).is_err());
    survives_damage(ResourceKind::IccProfile, &good);
}

// -------------------------------------------------------------- dispatch

#[test]
fn actions_are_refused_with_the_documented_reason() {
    let err = parse(ResourceKind::Actions, b"anything").unwrap_err();
    assert!(err.to_string().contains(".atn"), "{err}");
    assert_eq!(
        ResourceKind::from_extension("atn"),
        Some(ResourceKind::Actions)
    );
    assert_eq!(ResourceKind::from_extension("png"), None);
    for ext in ResourceKind::EXTENSIONS {
        assert!(ResourceKind::from_extension(ext).is_some(), "{ext}");
    }
}

#[test]
fn load_refuses_an_oversized_file_before_reading_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("big.aco");
    let file = std::fs::File::create(&path).unwrap();
    file.set_len(MAX_RESOURCE_BYTES + 1).unwrap();
    assert!(matches!(
        load(&path),
        Err(ResourceError::LimitExceeded { .. })
    ));
    let ok = dir.path().join("ok.ACO");
    std::fs::write(&ok, aco_v1(&[(0, [0, 65535, 0, 0])]).into_inner()).unwrap();
    assert!(matches!(load(&ok), Ok(Resource::Swatches(_))));
}
