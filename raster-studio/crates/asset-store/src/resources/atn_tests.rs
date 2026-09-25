//! W13-E: `.atn` from bytes laid down here by hand (not by [`write`]), the
//! round trip, the step interpreter, and damage that must not panic.

use psd::bytes::Sink;
use psd::{Descriptor, RefItem, Value};

use super::*;
use crate::resources::{parse as parse_resource, Resource, ResourceKind};

fn unicode(s: &mut Sink, text: &str) {
    let units: Vec<u16> = text.encode_utf16().collect();
    s.u32(units.len() as u32 + 1);
    for u in units {
        s.u16(u);
    }
    s.u16(0);
}

fn ascii_field(s: &mut Sink, text: &str) {
    s.u32(text.len() as u32);
    s.bytes(text.as_bytes());
}

/// A descriptor key: `0` then four bytes for a four-character code.
fn key(s: &mut Sink, k: &str) {
    if k.len() == 4 {
        s.u32(0);
    } else {
        s.u32(k.len() as u32);
    }
    s.bytes(k.as_bytes());
}

fn step_header(s: &mut Sink, enabled: bool, event_long: Option<&str>, event_text: Option<&str>) {
    s.u8(0); // expanded
    s.u8(u8::from(enabled));
    s.u8(0); // with dialog
    s.u8(0); // dialog options
    if let Some(code) = event_long {
        s.bytes(b"long");
        s.bytes(code.as_bytes());
    }
    if let Some(id) = event_text {
        s.bytes(b"TEXT");
        ascii_field(s, id);
    }
}

/// "Web Prep": one action of four steps, as Photoshop lays them down —
/// Make (layer), Gaussian Blur 3.5 px written with a *string* event id,
/// Plastic Wrap (no equivalent here), and an unchecked Invert.
fn hand_built() -> Vec<u8> {
    let mut s = Sink::new();
    s.u32(16);
    unicode(&mut s, "Web Prep");
    s.u8(1);
    s.u32(1); // actions
    s.u16(2); // F2
    s.u8(1); // shift
    s.u8(0);
    s.u16(3); // colour
    unicode(&mut s, "Soften");
    s.u8(0);
    s.u32(4); // steps

    // 1. Make: null = reference to class Lyr.
    step_header(&mut s, true, Some("Mk  "), None);
    ascii_field(&mut s, "Make");
    s.i32(-1);
    s.u32(16);
    unicode(&mut s, "");
    key(&mut s, "null");
    s.u32(1);
    key(&mut s, "null");
    s.bytes(b"obj ");
    s.u32(1);
    s.bytes(b"Clss");
    unicode(&mut s, "");
    key(&mut s, "Lyr ");

    // 2. Gaussian Blur, radius 3.5 px, event as a string id.
    step_header(&mut s, true, None, Some("gaussianBlur"));
    ascii_field(&mut s, "Gaussian Blur");
    s.i32(-1);
    s.u32(16);
    unicode(&mut s, "");
    key(&mut s, "null");
    s.u32(1);
    key(&mut s, "Rds ");
    s.bytes(b"UntF");
    s.bytes(b"#Pxl");
    s.f64(3.5);

    // 3. Plastic Wrap: no descriptor.
    step_header(&mut s, true, Some("PlsW"), None);
    ascii_field(&mut s, "Plastic Wrap");
    s.i32(0);

    // 4. Invert, unchecked.
    step_header(&mut s, false, Some("Invr"), None);
    ascii_field(&mut s, "Invert");
    s.i32(0);
    s.into_inner()
}

#[test]
fn a_hand_built_atn_parses_into_its_set_actions_and_steps() {
    let set = parse(&hand_built()).unwrap();
    assert_eq!(set.name, "Web Prep");
    assert!(set.expanded);
    assert_eq!(set.actions.len(), 1);
    let action = &set.actions[0];
    assert_eq!(action.name, "Soften");
    assert_eq!(
        (action.function_key, action.shift, action.color),
        (2, true, 3)
    );
    let names: Vec<_> = action.steps.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["Make", "Gaussian Blur", "Plastic Wrap", "Invert"]);
    assert!(action.steps[1].event == "gaussianBlur" && !action.steps[1].char_id);
    assert!(!action.steps[3].enabled);

    let ops: Vec<_> = action.steps.iter().map(interpret).collect();
    assert_eq!(ops[0], Ok(StepOp::MakeLayer));
    assert_eq!(ops[1], Ok(StepOp::GaussianBlur { radius: 3.5 }));
    let why = ops[2].clone().unwrap_err();
    assert!(
        why.contains("Plastic Wrap") && why.contains("PlsW"),
        "{why}"
    );
    assert_eq!(ops[3], Ok(StepOp::Invert));

    // And through the resource dispatch File > Open uses.
    match parse_resource(ResourceKind::Actions, &hand_built()).unwrap() {
        Resource::Actions(s) => assert_eq!(s, set),
        other => panic!("{other:?}"),
    }
}

#[test]
fn write_then_parse_is_the_identity_and_rewrites_the_same_bytes() {
    let hand = hand_built();
    let set = parse(&hand).unwrap();
    let written = write(&set).unwrap();
    assert_eq!(
        written, hand,
        "the writer lays the file down as Photoshop does"
    );
    assert_eq!(parse(&written).unwrap(), set);
}

fn every_op() -> Vec<StepOp> {
    vec![
        StepOp::MakeLayer,
        StepOp::SelectAll,
        StepOp::Deselect,
        StepOp::InverseSelection,
        StepOp::SelectRect {
            left: 2.0,
            top: 3.0,
            right: 10.0,
            bottom: 12.0,
        },
        StepOp::Fill {
            with: FillWith::Foreground,
            opacity: 1.0,
        },
        StepOp::Fill {
            with: FillWith::Background,
            opacity: 0.5,
        },
        StepOp::Fill {
            with: FillWith::Rgb([1.0, 0.0, 0.2]),
            opacity: 1.0,
        },
        StepOp::ImageSize {
            width: Some(Length::Pixels(640.0)),
            height: None,
        },
        StepOp::ImageSize {
            width: Some(Length::Percent(50.0)),
            height: Some(Length::Percent(25.0)),
        },
        StepOp::CanvasSize {
            width: Some(Length::Pixels(20.0)),
            height: Some(Length::Pixels(30.0)),
            relative: true,
            horizontal: 0,
            vertical: 2,
        },
        StepOp::Invert,
        StepOp::Desaturate,
        StepOp::Equalize,
        StepOp::BrightnessContrast {
            brightness: 40.0,
            contrast: -20.0,
        },
        StepOp::GaussianBlur { radius: 2.0 },
        StepOp::UnsharpMask {
            amount: 120.0,
            radius: 1.5,
            threshold: 3.0,
        },
        StepOp::Median { radius: 2.0 },
        StepOp::RotateCanvas { degrees: 90.0 },
        StepOp::FlipCanvas { horizontal: false },
        StepOp::RotateLayer { degrees: 180.0 },
        StepOp::FlipLayer { horizontal: true },
        StepOp::Save,
    ]
}

#[test]
fn every_operation_exports_as_a_step_that_reads_back_as_itself() {
    let set = AtnSet {
        name: "Everything".into(),
        expanded: false,
        actions: vec![AtnAction {
            name: "All ops".into(),
            steps: every_op().iter().map(StepOp::to_step).collect(),
            ..AtnAction::default()
        }],
    };
    let back = parse(&write(&set).unwrap()).unwrap();
    assert_eq!(back, set);
    for (op, step) in every_op().iter().zip(&back.actions[0].steps) {
        let read = interpret(step).unwrap();
        match (op, &read) {
            (
                StepOp::Fill {
                    with: FillWith::Rgb(a),
                    ..
                },
                StepOp::Fill {
                    with: FillWith::Rgb(b),
                    ..
                },
            ) => {
                assert!(
                    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-6),
                    "{a:?} {b:?}"
                );
            }
            _ => assert_eq!(&read, op),
        }
    }
}

#[test]
fn a_step_keeps_its_descriptor_through_the_actions_file_json() {
    let step = StepOp::GaussianBlur { radius: 7.25 }.to_step();
    let json = serde_json::to_string(&step).unwrap();
    let back: AtnStep = serde_json::from_str(&json).unwrap();
    assert_eq!(back, step);
}

#[test]
fn unmapped_and_off_target_steps_say_why() {
    let other_make = AtnStep::new(
        "Mk  ",
        "Make",
        Some({
            let mut d = Descriptor::new("null");
            d.push(
                "null",
                Value::Reference(vec![RefItem::Class {
                    name: String::new(),
                    class_id: "Chnl".into(),
                }]),
            )
            .unwrap();
            d
        }),
    );
    assert!(interpret(&other_make)
        .unwrap_err()
        .contains("other than a layer"));
    let unknown = AtnStep::new("PlsW", "Plastic Wrap", None);
    assert!(interpret(&unknown).unwrap_err().contains("no equivalent"));
}

#[test]
fn other_versions_and_damaged_files_are_errors_not_panics() {
    let mut v12 = hand_built();
    v12[3] = 12;
    assert!(matches!(
        parse(&v12),
        Err(ResourceError::Unsupported {
            what: "actions file version",
            ..
        })
    ));
    let mut trailing = hand_built();
    trailing.push(0);
    assert!(parse(&trailing).is_err());
    // A step count far past the limit is refused before anything is reserved.
    let mut s = Sink::new();
    s.u32(16);
    unicode(&mut s, "x");
    s.u8(0);
    s.u32(u32::MAX);
    assert!(matches!(
        parse(&s.into_inner()),
        Err(ResourceError::LimitExceeded { .. })
    ));
    let good = hand_built();
    for cut in 0..good.len() {
        assert!(parse(&good[..cut]).is_err(), "a cut at {cut} parsed");
    }
    for at in 0..good.len() {
        for flip in [0xFFu8, 0x80, 0x01] {
            let mut damaged = good.clone();
            damaged[at] ^= flip;
            let _ = parse(&damaged);
        }
    }
}
