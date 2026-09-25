//! W16-H: the events a recorded action commonly holds, read from
//! descriptors laid down the way Photoshop records them, and every
//! operation written back and read again.

use psd::{Descriptor, RefItem, Value};

use super::*;

fn d(class: &str, items: Vec<(&str, Value)>) -> Descriptor {
    let mut out = Descriptor::new(class);
    for (k, v) in items {
        out.push(k, v).unwrap();
    }
    out
}

fn target_layer() -> Value {
    Value::Reference(vec![RefItem::Enumerated {
        name: String::new(),
        class_id: "Lyr ".into(),
        type_id: "Ordn".into(),
        value: "Trgt".into(),
    }])
}

fn en(t: &str, v: &str) -> Value {
    Value::Enumerated {
        type_id: t.into(),
        value: v.into(),
    }
}

/// Levels as Photoshop CC records it: a preset kind and one composite
/// `LvlA` entry with input 20..235, gamma 1.2 and output 0..255.
pub(crate) fn photoshop_levels() -> AtnStep {
    let lvla = d(
        "LvlA",
        vec![
            (
                "Chnl",
                Value::Reference(vec![RefItem::Enumerated {
                    name: String::new(),
                    class_id: "Chnl".into(),
                    type_id: "Chnl".into(),
                    value: "Cmps".into(),
                }]),
            ),
            (
                "Inpt",
                Value::List(vec![Value::Integer(20), Value::Integer(235)]),
            ),
            ("Gmm ", Value::Double(1.2)),
        ],
    );
    AtnStep::new(
        "Lvls",
        "Levels",
        Some(d(
            "null",
            vec![
                ("presetKind", en("presetKindType", "presetKindCustom")),
                ("Adjs", Value::List(vec![Value::Descriptor(lvla)])),
            ],
        )),
    )
}

/// Duplicate as recorded: the target layer, a name, a version.
pub(crate) fn photoshop_duplicate() -> AtnStep {
    AtnStep::new(
        "Dplc",
        "Duplicate",
        Some(d(
            "null",
            vec![
                ("null", target_layer()),
                ("Nm  ", Value::Text("Copy of it".into())),
                ("Vrsn", Value::Integer(5)),
            ],
        )),
    )
}

#[test]
fn photoshop_recorded_levels_duplicate_transform_and_friends_read_with_their_parameters() {
    assert_eq!(
        interpret(&photoshop_levels()),
        Ok(StepOp::Levels(vec![LevelsEntry {
            channel: ToneChannel::Composite,
            input: [20.0, 235.0],
            gamma: 1.2,
            output: [0.0, 255.0],
        }]))
    );
    assert_eq!(
        interpret(&photoshop_duplicate()),
        Ok(StepOp::DuplicateLayer {
            name: Some("Copy of it".into())
        })
    );
    // Free Transform of the layer: 50% x 25%, 30° about the centre.
    let trnf = AtnStep::new(
        "Trnf",
        "Transform",
        Some(d(
            "null",
            vec![
                ("null", target_layer()),
                ("FTcs", en("QCSt", "Qcsa")),
                (
                    "Ofst",
                    Value::Descriptor(d(
                        "Ofst",
                        vec![
                            (
                                "Hrzn",
                                Value::UnitFloat {
                                    unit: *b"#Pxl",
                                    value: 4.0,
                                },
                            ),
                            (
                                "Vrtc",
                                Value::UnitFloat {
                                    unit: *b"#Pxl",
                                    value: -2.0,
                                },
                            ),
                        ],
                    )),
                ),
                (
                    "Wdth",
                    Value::UnitFloat {
                        unit: *b"#Prc",
                        value: 50.0,
                    },
                ),
                (
                    "Hght",
                    Value::UnitFloat {
                        unit: *b"#Prc",
                        value: 25.0,
                    },
                ),
                (
                    "Angl",
                    Value::UnitFloat {
                        unit: *b"#Ang",
                        value: 30.0,
                    },
                ),
            ],
        )),
    );
    assert_eq!(
        interpret(&trnf),
        Ok(StepOp::Transform {
            target: TransformTarget::Layer,
            pivot: Pivot::Bounds([0.5, 0.5]),
            offset: [4.0, -2.0],
            scale: [50.0, 25.0],
            angle: 30.0,
            skew: [0.0, 0.0],
        })
    );
    // Parameterless events under either spelling.
    for (event, op) in [
        ("Mrg2", StepOp::MergeDown),
        ("MrgV", StepOp::MergeVisible),
        ("FltI", StepOp::Flatten),
        ("copy", StepOp::Copy),
        ("past", StepOp::Paste),
        ("mergeVisible", StepOp::MergeVisible),
        ("flattenImage", StepOp::Flatten),
    ] {
        let step = AtnStep::new(event, event, None);
        assert_eq!(interpret(&step), Ok(op), "{event}");
    }
    // A string-id event whose id is four characters is still a string id.
    let mut trim = AtnStep::new("trim", "Trim", None);
    trim.char_id = false;
    assert!(matches!(interpret(&trim), Ok(StepOp::Trim { .. })));
    // Save with a format is Save As, which goes to the export dialog;
    // Save alone stays Save.
    let save_as = AtnStep::new(
        "save",
        "Save",
        Some(d(
            "null",
            vec![("As  ", Value::Descriptor(d("JPEG", vec![])))],
        )),
    );
    assert_eq!(interpret(&save_as), Ok(StepOp::Export));
    assert_eq!(
        interpret(&AtnStep::new("save", "Save", None)),
        Ok(StepOp::Save)
    );
    // Setting layer properties.
    let set = AtnStep::new(
        "setd",
        "Set",
        Some(d(
            "null",
            vec![
                ("null", target_layer()),
                (
                    "T   ",
                    Value::Descriptor(d(
                        "Lyr ",
                        vec![
                            ("Nm  ", Value::Text("Shadow".into())),
                            (
                                "Opct",
                                Value::UnitFloat {
                                    unit: *b"#Prc",
                                    value: 40.0,
                                },
                            ),
                            ("Md  ", en("BlnM", "Mltp")),
                        ],
                    )),
                ),
            ],
        )),
    );
    assert_eq!(
        interpret(&set),
        Ok(StepOp::SetLayer {
            name: Some("Shadow".into()),
            opacity: Some(40.0),
            blend: Some(layer_model::BlendMode::Multiply),
        })
    );
}

#[test]
fn steps_this_application_cannot_follow_say_why() {
    let per_range = AtnStep::new(
        "HStr",
        "Hue/Saturation",
        Some(d(
            "null",
            vec![(
                "Adjs",
                Value::List(vec![Value::Descriptor(d(
                    "Hst2",
                    vec![("LclR", Value::Integer(1)), ("H   ", Value::Integer(10))],
                ))]),
            )],
        )),
    );
    assert!(interpret(&per_range).unwrap_err().contains("colour range"));
    let preset_levels = AtnStep::new("Lvls", "Levels", None);
    assert!(interpret(&preset_levels).unwrap_err().contains("preset"));
    let preset_range = AtnStep::new(
        "ClrR",
        "Color Range",
        Some(d("null", vec![("Clrs", en("Clrs", "Rds "))])),
    );
    assert!(interpret(&preset_range).unwrap_err().contains("preset"));
}

fn every_w16_op() -> Vec<StepOp> {
    use layer_model::BlendMode;
    vec![
        StepOp::Levels(vec![
            LevelsEntry {
                channel: ToneChannel::Composite,
                input: [10.0, 240.0],
                gamma: 1.25,
                output: [5.0, 250.0],
            },
            LevelsEntry {
                channel: ToneChannel::Blue,
                input: [0.0, 200.0],
                gamma: 0.8,
                output: [0.0, 255.0],
            },
        ]),
        StepOp::Curves(vec![CurvesEntry {
            channel: ToneChannel::Red,
            points: vec![[0.0, 0.0], [128.0, 150.0], [255.0, 255.0]],
        }]),
        StepOp::HueSaturation {
            hue: 30.0,
            saturation: -20.0,
            lightness: 5.0,
            colorize: false,
        },
        StepOp::HueSaturation {
            hue: 200.0,
            saturation: 40.0,
            lightness: 0.0,
            colorize: true,
        },
        StepOp::BrightnessContrast {
            brightness: 10.0,
            contrast: 5.0,
        },
        StepOp::ColorBalance {
            shadows: [10.0, 0.0, -10.0],
            midtones: [0.0, 20.0, 0.0],
            highlights: [-5.0, 0.0, 5.0],
            preserve_luminosity: true,
        },
        StepOp::BlackAndWhite {
            weights: [40.0, 60.0, 40.0, 60.0, 20.0, 80.0],
            tint: Some([225.0, 211.0, 179.0]),
        },
        StepOp::Vibrance {
            vibrance: 30.0,
            saturation: -10.0,
        },
        StepOp::Exposure {
            exposure: 0.5,
            offset: -0.01,
            gamma: 1.1,
        },
        StepOp::Invert,
        StepOp::Desaturate,
        StepOp::Threshold { level: 100.0 },
        StepOp::Posterize { levels: 6 },
        StepOp::GradientMap {
            stops: vec![
                (0.0, [0.0, 0.0, 64.0]),
                (0.25, [128.0, 0.0, 0.0]),
                (1.0, [255.0, 255.0, 200.0]),
            ],
            reverse: true,
        },
        StepOp::PhotoFilter {
            color: [236.0, 138.0, 0.0],
            density: 25.0,
            preserve_luminosity: true,
        },
        StepOp::ChannelMixer {
            red: [100.0, 0.0, 0.0, 0.0],
            green: [10.0, 80.0, 10.0, 0.0],
            blue: [0.0, 0.0, 100.0, 5.0],
            monochrome: false,
        },
        StepOp::ChannelMixer {
            red: [40.0, 40.0, 20.0, 0.0],
            green: [40.0, 40.0, 20.0, 0.0],
            blue: [40.0, 40.0, 20.0, 0.0],
            monochrome: true,
        },
        StepOp::ImageSize {
            width: Some(Length::Pixels(300.0)),
            height: Some(Length::Pixels(200.0)),
        },
        StepOp::CanvasSize {
            width: Some(Length::Percent(120.0)),
            height: None,
            relative: false,
            horizontal: 1,
            vertical: 1,
        },
        StepOp::Crop {
            rect: Some([2.0, 3.0, 12.0, 14.0]),
        },
        StepOp::Crop { rect: None },
        StepOp::RotateCanvas { degrees: 90.0 },
        StepOp::FlipCanvas { horizontal: true },
        StepOp::Trim {
            basis: TrimBasis::TopLeft,
            top: true,
            left: false,
            bottom: true,
            right: false,
        },
        StepOp::ConvertMode {
            mode: ModeTarget::Grayscale,
        },
        StepOp::ConvertMode {
            mode: ModeTarget::Lab,
        },
        StepOp::BitDepth { bits: 16 },
        StepOp::Transform {
            target: TransformTarget::Layer,
            pivot: Pivot::Bounds([0.5, 0.5]),
            offset: [3.0, -4.0],
            scale: [50.0, 150.0],
            angle: 30.0,
            skew: [10.0, 0.0],
        },
        StepOp::Transform {
            target: TransformTarget::Selection,
            pivot: Pivot::Point([0.0, 0.0]),
            offset: [1.0, 1.0],
            scale: [100.0, 100.0],
            angle: 0.0,
            skew: [0.0, 0.0],
        },
        StepOp::Transform {
            target: TransformTarget::Layer,
            pivot: Pivot::Bounds([1.0, 0.0]),
            offset: [0.0, 0.0],
            scale: [-100.0, 100.0],
            angle: 0.0,
            skew: [0.0, 0.0],
        },
        StepOp::RotateLayer { degrees: 90.0 },
        StepOp::FlipLayer { horizontal: false },
        StepOp::ArrangeLayer(LayerRef::Forward),
        StepOp::ArrangeLayer(LayerRef::Back),
        StepOp::ArrangeLayer(LayerRef::Index(2)),
        StepOp::DuplicateLayer { name: None },
        StepOp::DuplicateLayer {
            name: Some("Twin".into()),
        },
        StepOp::DeleteLayer,
        StepOp::MergeDown,
        StepOp::MergeVisible,
        StepOp::Flatten,
        StepOp::MakeLayer,
        StepOp::MakeGroup,
        StepOp::GroupLayers,
        StepOp::SetLayer {
            name: Some("Glow".into()),
            opacity: Some(60.0),
            blend: Some(BlendMode::Screen),
        },
        StepOp::SetLayer {
            name: None,
            opacity: None,
            blend: Some(BlendMode::LinearDodge),
        },
        StepOp::SetVisibility { visible: false },
        StepOp::SetVisibility { visible: true },
        StepOp::SelectLayer(LayerRef::Name("Background".into())),
        StepOp::SelectLayer(LayerRef::Index(1)),
        StepOp::SelectLayer(LayerRef::Backward),
        StepOp::SelectAll,
        StepOp::Deselect,
        StepOp::InverseSelection,
        StepOp::SelectRect {
            left: 1.0,
            top: 1.0,
            right: 5.0,
            bottom: 5.0,
        },
        StepOp::ColorRange {
            color: [200.0, 30.0, 40.0],
            fuzziness: 40.0,
            invert: true,
        },
        StepOp::Feather { radius: 2.5 },
        StepOp::Expand { by: 3.0 },
        StepOp::Contract { by: 2.0 },
        StepOp::Border { width: 4.0 },
        StepOp::Smooth { radius: 2.0 },
        StepOp::Fill {
            with: FillWith::Foreground,
            opacity: 1.0,
        },
        StepOp::Stroke {
            width: 3.0,
            location: StrokeAt::Outside,
            opacity: 80.0,
            color: Some([255.0, 0.0, 0.0]),
            blend: BlendMode::Normal,
        },
        StepOp::Stroke {
            width: 1.0,
            location: StrokeAt::Center,
            opacity: 100.0,
            color: None,
            blend: BlendMode::Multiply,
        },
        StepOp::Copy,
        StepOp::CopyMerged,
        StepOp::Paste,
        StepOp::Cut,
        StepOp::LayerVia { cut: false },
        StepOp::LayerVia { cut: true },
        StepOp::GaussianBlur { radius: 1.5 },
        StepOp::UnsharpMask {
            amount: 80.0,
            radius: 1.0,
            threshold: 2.0,
        },
        StepOp::AddNoise {
            amount: 12.5,
            gaussian: true,
            monochromatic: true,
        },
        StepOp::MotionBlur {
            angle: 45.0,
            distance: 12.0,
        },
        StepOp::HighPass { radius: 4.0 },
        StepOp::Median { radius: 2.0 },
        StepOp::SmartSharpen {
            amount: 150.0,
            radius: 1.2,
            noise_reduction: 10.0,
        },
        StepOp::Save,
        StepOp::Export,
    ]
}

/// Close enough for the one lossy pair (Color Range's colour goes through
/// Lab, as Photoshop records it).
fn same(a: &StepOp, b: &StepOp) -> bool {
    match (a, b) {
        (
            StepOp::ColorRange {
                color: x,
                fuzziness: f,
                invert: i,
            },
            StepOp::ColorRange {
                color: y,
                fuzziness: g,
                invert: j,
            },
        ) => f == g && i == j && x.iter().zip(y).all(|(p, q)| (p - q).abs() < 0.01),
        _ => a == b,
    }
}

#[test]
fn every_w16_operation_exports_and_parses_back_as_itself() {
    let ops = every_w16_op();
    let set = AtnSet {
        name: "Wave 16".into(),
        expanded: true,
        actions: vec![AtnAction {
            name: "All of it".into(),
            steps: ops.iter().map(StepOp::to_step).collect(),
            ..AtnAction::default()
        }],
    };
    let back = parse(&write(&set).unwrap()).unwrap();
    assert_eq!(back, set, "the bytes carry every step unchanged");
    assert_eq!(back.actions[0].steps.len(), ops.len());
    for (op, step) in ops.iter().zip(&back.actions[0].steps) {
        let read = interpret(step).unwrap_or_else(|e| panic!("{op:?}: {e}"));
        assert!(same(&read, op), "{op:?} read back as {read:?}");
    }
}

#[test]
fn lab_and_rgb_are_inverse() {
    for rgb in [[0.0, 0.0, 0.0], [255.0, 255.0, 255.0], [200.0, 30.0, 40.0]] {
        let back = lab_to_rgb(rgb_to_lab(rgb));
        assert!(
            rgb.iter().zip(back).all(|(a, b)| (a - b).abs() < 0.01),
            "{rgb:?} -> {back:?}"
        );
    }
}
