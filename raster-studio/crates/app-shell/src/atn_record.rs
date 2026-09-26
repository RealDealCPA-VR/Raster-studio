//! W16-H: a recorded edit as the Photoshop steps that do the same, for
//! Export of an action set as `.atn`.
//!
//! A child module of `actions_library` (declared there with `#[path]`).
//!
//! The recorder captures [`Command`]s — what the edit did, not the dialog
//! that asked for it — so a step is written wherever the command itself
//! carries the parameters Photoshop needs: layers made, deleted, renamed,
//! re-blended, shown or hidden; selections set to all, none or a rectangle;
//! free transforms; Image Size; Canvas Size and Crop (as a crop rectangle);
//! mode and bit-depth changes; and, by the undo label the menu gives them,
//! the parameterless menu commands (Invert, Desaturate, Equalize, flips,
//! 180° turns, merges, Flatten, Group, Duplicate, Paste, Layer via Copy /
//! Cut). An adjustment layer made or edited carries its settings, so it is
//! written as Photoshop's `make` / `set` of an `adjustmentLayer`
//! ([`adjustment_step`]). A pixel edit whose settings the command does not
//! keep — a brush stroke, a filter or adjustment applied through its dialog,
//! whose tiles are recorded but not its sliders — has no step, nor has a new
//! text, shape, fill or smart-object layer (its content is not a Photoshop
//! step this writer spells), and the export counts each as left out.

use asset_store::resources::atn::{
    CurvesEntry, Length, LevelsEntry, ModeTarget, Pivot, StepOp, ToneChannel, TransformTarget,
};
use editor_core::Command;
use layer_model::{AdjustmentKind, LayerKind};

/// W16-H: an adjustment layer's settings as the adjustment step that holds
/// them, in Photoshop's units; `None` for an adjustment Photoshop's
/// adjustment-layer steps here do not carry (Selective Color, Color Lookup
/// and the destructive-only ones).
pub(crate) fn adjustment_step(kind: &AdjustmentKind) -> Option<StepOp> {
    let level = |v: f32| f64::from(v) * 255.0;
    let pct = |v: f32| f64::from(v) * 100.0;
    let levels_entry = |channel, m: &[f32; 5]| LevelsEntry {
        channel,
        input: [level(m[0]), level(m[1])],
        gamma: f64::from(m[2]),
        output: [level(m[3]), level(m[4])],
    };
    let curve_entry = |channel, points: &[[f32; 2]]| CurvesEntry {
        channel,
        points: points.iter().map(|p| [level(p[0]), level(p[1])]).collect(),
    };
    Some(match kind {
        AdjustmentKind::Levels {
            black,
            white,
            gamma,
        } => StepOp::Levels(vec![levels_entry(
            ToneChannel::Composite,
            &[*black, *white, *gamma, 0.0, 1.0],
        )]),
        AdjustmentKind::LevelsFull {
            composite,
            red,
            green,
            blue,
        } => {
            const IDENTITY: [f32; 5] = [0.0, 1.0, 1.0, 0.0, 1.0];
            let mut entries = vec![levels_entry(ToneChannel::Composite, composite)];
            for (channel, m) in [
                (ToneChannel::Red, red),
                (ToneChannel::Green, green),
                (ToneChannel::Blue, blue),
            ] {
                if *m != IDENTITY {
                    entries.push(levels_entry(channel, m));
                }
            }
            StepOp::Levels(entries)
        }
        AdjustmentKind::Curves { points } => {
            StepOp::Curves(vec![curve_entry(ToneChannel::Composite, points)])
        }
        AdjustmentKind::CurvesFull {
            composite,
            red,
            green,
            blue,
        } => {
            let identity: &[[f32; 2]] = &[[0.0, 0.0], [1.0, 1.0]];
            let mut entries = vec![curve_entry(ToneChannel::Composite, composite)];
            for (channel, points) in [
                (ToneChannel::Red, red),
                (ToneChannel::Green, green),
                (ToneChannel::Blue, blue),
            ] {
                if points.as_slice() != identity {
                    entries.push(curve_entry(channel, points));
                }
            }
            StepOp::Curves(entries)
        }
        AdjustmentKind::Exposure { stops } => StepOp::Exposure {
            exposure: f64::from(*stops),
            offset: 0.0,
            gamma: 1.0,
        },
        AdjustmentKind::ExposureFull {
            stops,
            offset,
            gamma,
        } => StepOp::Exposure {
            exposure: f64::from(*stops),
            offset: f64::from(*offset),
            gamma: f64::from(*gamma),
        },
        AdjustmentKind::HueSaturation {
            hue,
            saturation,
            lightness,
        }
        | AdjustmentKind::HueSaturationFull {
            hue,
            saturation,
            lightness,
            colorize: None,
        } => StepOp::HueSaturation {
            hue: f64::from(*hue),
            saturation: pct(*saturation),
            lightness: pct(*lightness),
            colorize: false,
        },
        AdjustmentKind::HueSaturationFull {
            colorize: Some([hue, saturation, lightness]),
            ..
        } => StepOp::HueSaturation {
            hue: f64::from(*hue),
            saturation: pct(*saturation),
            lightness: pct(*lightness),
            colorize: true,
        },
        AdjustmentKind::ColorBalance {
            shadows,
            midtones,
            highlights,
        } => StepOp::ColorBalance {
            shadows: shadows.map(pct),
            midtones: midtones.map(pct),
            highlights: highlights.map(pct),
            preserve_luminosity: false,
        },
        AdjustmentKind::ColorBalanceFull {
            shadows,
            midtones,
            highlights,
            preserve_luminosity,
        } => StepOp::ColorBalance {
            shadows: shadows.map(pct),
            midtones: midtones.map(pct),
            highlights: highlights.map(pct),
            preserve_luminosity: *preserve_luminosity,
        },
        // Photoshop's ranges, as `atn_play` reads them back.
        AdjustmentKind::BrightnessContrast {
            brightness,
            contrast,
        } => StepOp::BrightnessContrast {
            brightness: f64::from(*brightness) * 150.0,
            contrast: pct(*contrast),
        },
        AdjustmentKind::Vibrance {
            vibrance,
            saturation,
        } => StepOp::Vibrance {
            vibrance: pct(*vibrance),
            saturation: pct(*saturation),
        },
        AdjustmentKind::BlackAndWhite { weights, tint } => StepOp::BlackAndWhite {
            weights: weights.map(pct),
            // The tint as the fully bright colour of its hue and saturation,
            // which `atn_play` reads back as that hue and saturation.
            tint: tint.map(|[hue, saturation]| hsv_to_rgb(f64::from(hue), f64::from(saturation))),
        },
        AdjustmentKind::PhotoFilter {
            color_srgb,
            density,
            preserve_luminosity,
        } => StepOp::PhotoFilter {
            color: color_srgb.map(level),
            density: pct(*density),
            preserve_luminosity: *preserve_luminosity,
        },
        AdjustmentKind::ChannelMixer { rows, monochrome } => StepOp::ChannelMixer {
            red: rows[0].map(pct),
            green: rows[1].map(pct),
            blue: rows[2].map(pct),
            monochrome: *monochrome,
        },
        AdjustmentKind::Invert => StepOp::Invert,
        AdjustmentKind::Posterize { levels } => StepOp::Posterize { levels: *levels },
        AdjustmentKind::Threshold { level: l } => StepOp::Threshold { level: level(*l) },
        AdjustmentKind::GradientMap { stops, reverse } => StepOp::GradientMap {
            stops: stops
                .iter()
                .map(|(at, rgb)| (f64::from(*at), rgb.map(level)))
                .collect(),
            reverse: *reverse,
        },
        _ => return None,
    })
}

/// `[r, g, b]` `0..=255` of a hue (degrees) and saturation `0..=1` at full
/// value.
fn hsv_to_rgb(hue: f64, saturation: f64) -> [f64; 3] {
    let h = hue.rem_euclid(360.0) / 60.0;
    let c = saturation.clamp(0.0, 1.0);
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = 1.0 - c;
    [(r + m) * 255.0, (g + m) * 255.0, (b + m) * 255.0]
}

/// A recorded `TransformLayer` delta as Photoshop's Free Transform about
/// the document origin: the translation, then rotate · skew · scale.
pub(crate) fn transform_of(matrix: [f32; 6]) -> Option<StepOp> {
    let [a, b, c, d, e, f] = matrix.map(f64::from);
    let sx = a.hypot(b);
    let det = a * d - b * c;
    if !(sx.is_finite() && det.is_finite()) || sx < 1e-9 || det.abs() < 1e-12 {
        return None;
    }
    let angle = b.atan2(a).to_degrees();
    let sy = det / sx;
    let shear = (a * c + b * d) / sx / sy;
    Some(StepOp::Transform {
        target: TransformTarget::Layer,
        pivot: Pivot::Point([0.0, 0.0]),
        offset: [e, f],
        scale: [sx * 100.0, sy * 100.0],
        angle,
        skew: [shear.atan().to_degrees(), 0.0],
    })
}

fn mode_of(code: u8) -> Option<ModeTarget> {
    // `DocumentMeta::color_mode` is the `ui::menu::ColorMode` order.
    Some(match code {
        0 => ModeTarget::Rgb,
        1 => ModeTarget::Grayscale,
        2 => ModeTarget::Lab,
        3 => ModeTarget::Cmyk,
        4 => ModeTarget::Indexed,
        5 => ModeTarget::Bitmap,
        6 => ModeTarget::Duotone,
        _ => return None,
    })
}

fn is_identity_linear(m: &[f32; 6]) -> bool {
    (m[0] - 1.0).abs() < 1e-6 && m[1].abs() < 1e-6 && m[2].abs() < 1e-6 && (m[3] - 1.0).abs() < 1e-6
}

/// The parameterless menu commands, by the undo label their menu arm gives
/// the transaction.
fn by_label(label: &str) -> Option<StepOp> {
    Some(match label {
        "Apply Invert" => StepOp::Invert,
        "Apply Desaturate" => StepOp::Desaturate,
        "Apply Equalize" => StepOp::Equalize,
        "Rotate 180°" => StepOp::RotateCanvas { degrees: 180.0 },
        "Flip Canvas Horizontal" => StepOp::FlipCanvas { horizontal: true },
        "Flip Canvas Vertical" => StepOp::FlipCanvas { horizontal: false },
        "Rotate Layer 180°" => StepOp::RotateLayer { degrees: 180.0 },
        "Rotate Layer 90° CW" => StepOp::RotateLayer { degrees: 90.0 },
        "Rotate Layer 90° CCW" => StepOp::RotateLayer { degrees: 270.0 },
        "Flip Layer Horizontal" => StepOp::FlipLayer { horizontal: true },
        "Flip Layer Vertical" => StepOp::FlipLayer { horizontal: false },
        "Merge Down" | "Merge Layers" => StepOp::MergeDown,
        "Merge Visible" => StepOp::MergeVisible,
        "Flatten Image" => StepOp::Flatten,
        "Group Layers" => StepOp::GroupLayers,
        "Paste" => StepOp::Paste,
        "Layer via Copy" => StepOp::LayerVia { cut: false },
        "Layer via Cut" => StepOp::LayerVia { cut: true },
        l if l.starts_with("Duplicate ") => StepOp::DuplicateLayer { name: None },
        _ => return None,
    })
}

/// The steps that do what `command` did, or `None` when Photoshop has no
/// step for it or the command does not carry its parameters.
pub(crate) fn steps_for(command: &Command) -> Option<Vec<StepOp>> {
    let one = |op: StepOp| Some(vec![op]);
    match command {
        Command::CreateLayer { layer } => one(match &layer.kind {
            LayerKind::Group(_) => StepOp::MakeGroup,
            LayerKind::Raster(_) => StepOp::MakeLayer,
            LayerKind::Adjustment(a) => {
                StepOp::MakeAdjustmentLayer(Box::new(adjustment_step(&a.kind)?))
            }
            // A text, shape, fill, smart-object or generator layer is not a
            // new empty layer: writing Make Layer for it would replay as one.
            _ => return None,
        }),
        Command::SetLayerKind { kind, .. } => match kind.as_ref() {
            LayerKind::Adjustment(a) => one(StepOp::SetAdjustmentLayer(Box::new(adjustment_step(
                &a.kind,
            )?))),
            _ => None,
        },
        Command::DeleteLayer { .. } => one(StepOp::DeleteLayer),
        Command::SetLayerProperties { patch, .. } => {
            let mut bare = patch.clone();
            let (name, opacity, blend, visible) = (
                bare.name.take(),
                bare.opacity.take(),
                bare.blend_mode.take(),
                bare.visible.take(),
            );
            if bare != editor_core::LayerPatch::default() {
                return None;
            }
            let mut out = Vec::new();
            if name.is_some() || opacity.is_some() || blend.is_some() {
                out.push(StepOp::SetLayer {
                    name,
                    opacity: opacity.map(|o| f64::from(o) * 100.0),
                    blend,
                });
            }
            if let Some(visible) = visible {
                out.push(StepOp::SetVisibility { visible });
            }
            (!out.is_empty()).then_some(out)
        }
        Command::SetSelection { selection } => match selection {
            editor_core::Selection::None => one(StepOp::Deselect),
            editor_core::Selection::Rect { min, max } => one(StepOp::SelectRect {
                left: f64::from(min.x),
                top: f64::from(min.y),
                right: f64::from(max.x),
                bottom: f64::from(max.y),
            }),
            editor_core::Selection::Mask(_) => None,
        },
        Command::SetMetaColorMode { to, .. } => one(StepOp::ConvertMode {
            mode: mode_of(*to)?,
        }),
        Command::SetMetaBitDepth { to, .. } => one(StepOp::BitDepth { bits: *to }),
        Command::TransformLayer { matrix, .. } => one(transform_of(*matrix)?),
        Command::ResampleImage { size, .. } => one(StepOp::ImageSize {
            width: Some(Length::Pixels(f64::from(size.x))),
            height: Some(Length::Pixels(f64::from(size.y))),
        }),
        Command::SetCanvasSize { size } => one(StepOp::Crop {
            rect: Some([0.0, 0.0, f64::from(size.x), f64::from(size.y)]),
        }),
        Command::MoveLayer { .. } => None,
        Command::Transaction { label, commands } => transaction(label, commands),
        _ => None,
    }
}

fn transaction(label: &str, commands: &[Command]) -> Option<Vec<StepOp>> {
    if let Some(op) = by_label(label) {
        return Some(vec![op]);
    }
    // Image Size: the resample carries the new size.
    if let Some(Command::ResampleImage { size, .. }) = commands
        .iter()
        .find(|c| matches!(c, Command::ResampleImage { .. }))
    {
        return steps_for(&Command::ResampleImage {
            size: *size,
            changes: Vec::new(),
        });
    }
    // Mode / depth conversions: the meta change names the target.
    if let Some(c) = commands.iter().find(|c| {
        matches!(
            c,
            Command::SetMetaColorMode { .. } | Command::SetMetaBitDepth { .. }
        )
    }) {
        return steps_for(c);
    }
    // Canvas Size, Crop, Reveal All: the new size and the one translation
    // every root layer took is a crop rectangle in the old canvas. A resize
    // that rewrote the pixels instead ("Resize Canvas", "Rotate 90°") keeps
    // no offset or direction, so it has no step.
    if let Some(Command::SetCanvasSize { size }) = commands
        .iter()
        .find(|c| matches!(c, Command::SetCanvasSize { .. }))
    {
        if commands
            .iter()
            .any(|c| matches!(c, Command::PaintTiles { .. }))
            && !commands
                .iter()
                .any(|c| matches!(c, Command::TransformLayer { .. }))
        {
            return None;
        }
        let mut shift = None;
        for c in commands {
            if let Command::TransformLayer { matrix, .. } = c {
                if !is_identity_linear(matrix) {
                    return None;
                }
                let t = (f64::from(matrix[4]), f64::from(matrix[5]));
                match shift {
                    None => shift = Some(t),
                    Some(s) if s == t => {}
                    Some(_) => return None,
                }
            }
        }
        let (dx, dy) = shift.unwrap_or((0.0, 0.0));
        let (w, h) = (f64::from(size.x), f64::from(size.y));
        return Some(vec![StepOp::Crop {
            rect: Some([-dx, -dy, w - dx, h - dy]),
        }]);
    }
    // Anything else: a transaction whose every member has a step.
    let mut out = Vec::new();
    for c in commands {
        out.extend(steps_for(c)?);
    }
    (!out.is_empty()).then_some(out)
}
