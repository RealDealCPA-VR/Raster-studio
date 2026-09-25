//! W16-H: playing the `.atn` steps `atn_more` reads. Each goes through the
//! route the user's own menu click or dialog OK takes, so a played step is
//! the same undo step as doing it by hand.
//!
//! A child module of `atn_play` (declared there with `#[path]`).
//!
//! | Step | Route |
//! |---|---|
//! | Levels, Curves, Hue/Saturation, Color Balance, Black & White, Vibrance, Exposure, Threshold, Posterize, Gradient Map, Photo Filter, Channel Mixer | [`crate::menu_bridge::run_adjustment_kind`] (Image ▸ Adjustments' OK) |
//! | Crop to a rectangle / to the selection | [`Editor::resize_canvas`] / [`Editor::crop_to_selection`] |
//! | Trim | [`crate::layer_ops::trim_with`] (the Trim dialog's OK) |
//! | Mode, bit depth, delete, merge down / visible, flatten, new group, group, show / hide, arrange, copy / copy merged / paste / cut, layer via copy / cut | [`crate::menu_bridge::perform`] with the menu row's `MenuAction` |
//! | Transform of a layer | one `TransformLayer` in a transaction (Free Transform's commit shape) |
//! | Transform of the selection | `SetSelection` of `selection::transform_selection` |
//! | Duplicate | [`crate::layer_ops::duplicate_layer`] (the Duplicate Layer dialog's OK) |
//! | Layer name / opacity / blend | `SetLayerProperties` (the Layers panel's edit) |
//! | Select a layer | [`Editor::set_layer_selection`] |
//! | Color Range, Feather / Expand / Contract / Border / Smooth | `SetSelection` built as the dialogs' OK builds it |
//! | Stroke | [`crate::menu_bridge::stroke_selection_with`] (the Stroke dialog's OK) |
//! | Add Noise, Motion Blur, High Pass, Smart Sharpen | [`crate::menu_bridge::run_filter_invocation`] (the Filter dialog's OK) |
//! | Save As / Export | [`Editor::dispatch`] of `Action::Export` (the export dialog) |

use asset_store::resources::atn::{
    LayerRef, ModeTarget, Pivot, StepOp, StrokeAt, ToneChannel, TransformTarget, TrimBasis,
};
use ui::menu::{Arrange, ChannelDepth, ColorMode, FilterId, MenuAction, ModifySelection};

use super::super::{Action, Editor};
use super::filter;

fn adjust(
    editor: &mut Editor,
    adjustment: Result<adjustments::Adjustment, adjustments::AdjustmentError>,
    label: &str,
) -> Result<String, String> {
    let adjustment = adjustment.map_err(|e| e.to_string())?;
    crate::menu_bridge::run_adjustment_kind(editor, &adjustment, label)
}

fn unit(v: f64) -> f32 {
    (v / 255.0) as f32
}

fn pct(v: f64) -> f32 {
    (v / 100.0) as f32
}

/// Hue (degrees) and saturation (`0..=1`) of an `[r, g, b]` `0..=255`.
fn hue_saturation(rgb: [f64; 3]) -> (f32, f32) {
    let max = rgb[0].max(rgb[1]).max(rgb[2]);
    let min = rgb[0].min(rgb[1]).min(rgb[2]);
    let d = max - min;
    if d <= 0.0 {
        return (0.0, 0.0);
    }
    let h = if max == rgb[0] {
        60.0 * ((rgb[1] - rgb[2]) / d).rem_euclid(6.0)
    } else if max == rgb[1] {
        60.0 * ((rgb[2] - rgb[0]) / d + 2.0)
    } else {
        60.0 * ((rgb[0] - rgb[1]) / d + 4.0)
    };
    (h as f32, (d / max) as f32)
}

/// The linear part of a Photoshop Free Transform: rotate · skew · scale.
pub(crate) fn transform_linear(scale: [f64; 2], angle: f64, skew: [f64; 2]) -> glam::Mat2 {
    let (s, c) = (angle.to_radians() as f32).sin_cos();
    let rotate = glam::Mat2::from_cols(glam::vec2(c, s), glam::vec2(-s, c));
    let (th, tv) = (
        (skew[0].to_radians() as f32).tan(),
        (skew[1].to_radians() as f32).tan(),
    );
    let shear = glam::Mat2::from_cols(glam::vec2(1.0, tv), glam::vec2(th, 1.0));
    let stretch = glam::Mat2::from_diagonal(glam::vec2(pct(scale[0]), pct(scale[1])));
    rotate * shear * stretch
}

fn pivot_in(pivot: Pivot, bounds: (f32, f32, f32, f32)) -> glam::Vec2 {
    match pivot {
        Pivot::Point([x, y]) => glam::vec2(x as f32, y as f32),
        Pivot::Bounds([fx, fy]) => glam::vec2(
            bounds.0 + (bounds.2 - bounds.0) * fx as f32,
            bounds.1 + (bounds.3 - bounds.1) * fy as f32,
        ),
    }
}

fn canvas_rect(editor: &Editor) -> Result<selection::Rect, String> {
    let doc = editor.active().ok_or("No document is open")?;
    Ok(selection::Rect::from_xywh(
        0,
        0,
        doc.document.width(),
        doc.document.height(),
    ))
}

fn set_selection(editor: &mut Editor, next: editor_core::Selection) -> Result<String, String> {
    editor.apply_command(editor_core::Command::SetSelection { selection: next });
    Ok(String::new())
}

impl Editor {
    /// Perform one W16-H step; `None` for the W13-E steps `atn_play`
    /// performs itself.
    pub(super) fn perform_w16_op(&mut self, op: &StepOp) -> Option<Result<String, String>> {
        use adjustments as a;
        use ui::dialogs::ParamValue as P;
        let menu = |editor: &mut Editor, action| crate::menu_bridge::perform(action, editor);
        Some(match op {
            StepOp::Levels(entries) => {
                let levels = (|| {
                    let mut levels = a::Levels::IDENTITY;
                    for e in entries {
                        let ch = a::LevelsChannel::new(
                            unit(e.input[0]),
                            unit(e.input[1]),
                            e.gamma as f32,
                        )?
                        .with_output(unit(e.output[0]), unit(e.output[1]))?;
                        match e.channel {
                            ToneChannel::Composite => levels.composite = ch,
                            ToneChannel::Red => levels.red = ch,
                            ToneChannel::Green => levels.green = ch,
                            ToneChannel::Blue => levels.blue = ch,
                        }
                    }
                    Ok(a::Adjustment::Levels(levels))
                })();
                adjust(self, levels, "Apply Levels")
            }
            StepOp::Curves(entries) => {
                let curves = (|| {
                    let mut curves = a::Curves::identity();
                    for e in entries {
                        let points: Vec<[f32; 2]> =
                            e.points.iter().map(|p| [unit(p[0]), unit(p[1])]).collect();
                        let curve = a::Curve::new(&points)?;
                        match e.channel {
                            ToneChannel::Composite => curves.composite = curve,
                            ToneChannel::Red => curves.red = curve,
                            ToneChannel::Green => curves.green = curve,
                            ToneChannel::Blue => curves.blue = curve,
                        }
                    }
                    Ok(a::Adjustment::Curves(curves))
                })();
                adjust(self, curves, "Apply Curves")
            }
            StepOp::HueSaturation {
                hue,
                saturation,
                lightness,
                colorize,
            } => {
                let hs = if *colorize {
                    a::Colorize::new(*hue as f32, pct(*saturation), pct(*lightness))
                        .map(a::HueSaturation::colorized)
                } else {
                    a::HueSaturation::new(*hue as f32, pct(*saturation), pct(*lightness))
                };
                adjust(
                    self,
                    hs.map(a::Adjustment::HueSaturation),
                    "Apply Hue/Saturation",
                )
            }
            StepOp::ColorBalance {
                shadows,
                midtones,
                highlights,
                preserve_luminosity,
            } => {
                let cb =
                    a::ColorBalance::new(shadows.map(pct), midtones.map(pct), highlights.map(pct))
                        .map(|cb| cb.with_preserve_luminosity(*preserve_luminosity));
                adjust(
                    self,
                    cb.map(a::Adjustment::ColorBalance),
                    "Apply Color Balance",
                )
            }
            StepOp::BlackAndWhite { weights, tint } => {
                let bw = (|| {
                    let tint = match tint {
                        Some(rgb) => {
                            let (h, s) = hue_saturation(*rgb);
                            Some(a::BwTint::new(h, s)?)
                        }
                        None => None,
                    };
                    Ok(a::Adjustment::BlackAndWhite(
                        a::BlackAndWhite::new(weights.map(pct))?.with_tint(tint),
                    ))
                })();
                adjust(self, bw, "Apply Black & White")
            }
            StepOp::Vibrance {
                vibrance,
                saturation,
            } => adjust(
                self,
                a::Vibrance::new(pct(*vibrance), pct(*saturation)).map(a::Adjustment::Vibrance),
                "Apply Vibrance",
            ),
            StepOp::Exposure {
                exposure,
                offset,
                gamma,
            } => adjust(
                self,
                a::ExposureParams::new(*exposure as f32, *offset as f32, *gamma as f32)
                    .map(a::Adjustment::Exposure),
                "Apply Exposure",
            ),
            StepOp::Threshold { level } => adjust(
                self,
                a::Threshold::new(unit(*level)).map(a::Adjustment::Threshold),
                "Apply Threshold",
            ),
            StepOp::Posterize { levels } => adjust(
                self,
                a::Posterize::new(*levels).map(a::Adjustment::Posterize),
                "Apply Posterize",
            ),
            StepOp::GradientMap { stops, reverse } => {
                let map = (|| {
                    let stops = stops
                        .iter()
                        .map(|(at, rgb)| {
                            let at = if *reverse { 1.0 - at } else { *at };
                            a::GradientStop::new(at as f32, rgb.map(unit))
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok(a::Adjustment::GradientMap(a::GradientMap::new(&stops)?))
                })();
                adjust(self, map, "Apply Gradient Map")
            }
            StepOp::PhotoFilter {
                color,
                density,
                preserve_luminosity,
            } => adjust(
                self,
                a::PhotoFilter::new(color.map(unit), pct(*density)).map(|f| {
                    a::Adjustment::PhotoFilter(f.with_preserve_luminosity(*preserve_luminosity))
                }),
                "Apply Photo Filter",
            ),
            StepOp::ChannelMixer {
                red,
                green,
                blue,
                monochrome,
            } => adjust(
                self,
                a::ChannelMixer::new([red.map(pct), green.map(pct), blue.map(pct)])
                    .map(|m| a::Adjustment::ChannelMixer(m.monochrome(*monochrome))),
                "Apply Channel Mixer",
            ),
            StepOp::Crop { rect: None } => self.crop_to_selection(),
            StepOp::Crop {
                rect: Some([l, t, r, b]),
            } => {
                let px = |v: f64| v.round().clamp(-1e9, 1e9) as i64;
                let (l, t, r, b) = (px(*l), px(*t), px(*r), px(*b));
                if r <= l || b <= t {
                    return Some(Err("the crop rectangle is empty".to_string()));
                }
                let (Ok(w), Ok(h), Ok(x), Ok(y)) = (
                    u32::try_from(r - l),
                    u32::try_from(b - t),
                    i32::try_from(l),
                    i32::try_from(t),
                ) else {
                    return Some(Err("the crop rectangle is out of range".to_string()));
                };
                self.resize_canvas(w, h, glam::IVec2::new(x, y))
            }
            StepOp::Trim {
                basis,
                top,
                left,
                bottom,
                right,
            } => crate::layer_ops::trim_with(
                self,
                ui::dialogs::TrimSpec {
                    basis: match basis {
                        TrimBasis::Transparent => ui::dialogs::TrimBasis::Transparent,
                        TrimBasis::TopLeft => ui::dialogs::TrimBasis::TopLeftColor,
                        TrimBasis::BottomRight => ui::dialogs::TrimBasis::BottomRightColor,
                    },
                    top: *top,
                    bottom: *bottom,
                    left: *left,
                    right: *right,
                },
            ),
            StepOp::ConvertMode { mode } => menu(
                self,
                MenuAction::SetColorMode(match mode {
                    ModeTarget::Rgb => ColorMode::Rgb,
                    ModeTarget::Grayscale => ColorMode::Grayscale,
                    ModeTarget::Lab => ColorMode::Lab,
                    ModeTarget::Cmyk => ColorMode::Cmyk,
                    ModeTarget::Indexed => ColorMode::Indexed,
                    ModeTarget::Bitmap => ColorMode::Bitmap,
                    ModeTarget::Duotone => ColorMode::Duotone,
                }),
            ),
            StepOp::BitDepth { bits } => match bits {
                8 => menu(self, MenuAction::SetBitDepth(ChannelDepth::Eight)),
                16 => menu(self, MenuAction::SetBitDepth(ChannelDepth::Sixteen)),
                32 => menu(self, MenuAction::SetBitDepth(ChannelDepth::ThirtyTwo)),
                other => Err(format!("{other} bits per channel is not a depth here")),
            },
            StepOp::Transform {
                target,
                pivot,
                offset,
                scale,
                angle,
                skew,
            } => self.play_transform(*target, *pivot, *offset, *scale, *angle, *skew),
            StepOp::ArrangeLayer(to) => match to {
                LayerRef::Forward => menu(self, MenuAction::ArrangeLayer(Arrange::BringForward)),
                LayerRef::Backward => menu(self, MenuAction::ArrangeLayer(Arrange::SendBackward)),
                LayerRef::Front => menu(self, MenuAction::ArrangeLayer(Arrange::BringToFront)),
                LayerRef::Back => menu(self, MenuAction::ArrangeLayer(Arrange::SendToBack)),
                LayerRef::Index(i) => self.move_layer_to(*i),
                LayerRef::Name(_) => Err("a layer cannot be moved to a name".to_string()),
            },
            StepOp::DuplicateLayer { name } => {
                crate::layer_ops::duplicate_layer(self, name.clone())
            }
            StepOp::DeleteLayer => menu(self, MenuAction::DeleteLayer),
            StepOp::MergeDown => menu(self, MenuAction::MergeDown),
            StepOp::MergeVisible => menu(self, MenuAction::MergeVisible),
            StepOp::Flatten => menu(self, MenuAction::FlattenImage),
            StepOp::MakeGroup => menu(self, MenuAction::NewGroup),
            StepOp::GroupLayers => menu(self, MenuAction::GroupLayers),
            StepOp::SetLayer {
                name,
                opacity,
                blend,
            } => {
                let Some(layer_id) = self.active().and_then(|d| d.document.active_layer()) else {
                    return Some(Err("No layer is active".to_string()));
                };
                let patch = editor_core::LayerPatch {
                    name: name.clone(),
                    opacity: opacity.map(|o| pct(o).clamp(0.0, 1.0)),
                    blend_mode: *blend,
                    ..Default::default()
                };
                self.apply_command(editor_core::Command::SetLayerProperties { layer_id, patch });
                Ok(String::new())
            }
            StepOp::SetVisibility { visible } => menu(
                self,
                if *visible {
                    MenuAction::ShowLayers
                } else {
                    MenuAction::HideLayers
                },
            ),
            StepOp::SelectLayer(which) => self.select_layer_ref(which),
            StepOp::ColorRange {
                color,
                fuzziness,
                invert,
            } => self.play_color_range(*color, *fuzziness, *invert),
            StepOp::Feather { radius } => self.play_modify(ModifySelection::Feather, *radius),
            StepOp::Expand { by } => self.play_modify(ModifySelection::Expand, *by),
            StepOp::Contract { by } => self.play_modify(ModifySelection::Contract, *by),
            StepOp::Border { width } => self.play_modify(ModifySelection::Border, *width),
            StepOp::Smooth { radius } => self.play_modify(ModifySelection::Smooth, *radius),
            StepOp::Stroke {
                width,
                location,
                opacity,
                color,
                blend,
            } => {
                let spec = ui::dialogs::StrokeSpec {
                    width: width.round().clamp(1.0, 250.0) as u32,
                    location: match location {
                        StrokeAt::Inside => ui::dialogs::StrokeLocation::Inside,
                        StrokeAt::Center => ui::dialogs::StrokeLocation::Center,
                        StrokeAt::Outside => ui::dialogs::StrokeLocation::Outside,
                    },
                    blend: *blend,
                    opacity: pct(*opacity).clamp(0.0, 1.0),
                    preserve_transparency: false,
                };
                // The step's own colour, as Photoshop's Stroke carries it;
                // the foreground the user chose comes back afterwards.
                let before = self.foreground();
                if let Some(rgb) = color {
                    let [r, g, b] = rgb.map(|v| unit(v).clamp(0.0, 1.0));
                    self.set_foreground([r, g, b, 1.0]);
                }
                let out = crate::menu_bridge::stroke_selection_with(self, &spec);
                self.set_foreground(before);
                out
            }
            StepOp::Copy => menu(self, MenuAction::Copy),
            StepOp::CopyMerged => menu(self, MenuAction::CopyMerged),
            StepOp::Paste => menu(self, MenuAction::Paste),
            StepOp::Cut => menu(self, MenuAction::Cut),
            StepOp::LayerVia { cut } => menu(
                self,
                if *cut {
                    MenuAction::LayerViaCut
                } else {
                    MenuAction::LayerViaCopy
                },
            ),
            StepOp::AddNoise {
                amount,
                gaussian,
                monochromatic,
            } => filter(
                self,
                FilterId::AddNoise,
                &[
                    ("amount", P::Float(pct(*amount))),
                    ("distribution", P::Choice(usize::from(*gaussian))),
                    ("monochromatic", P::Bool(*monochromatic)),
                ],
            ),
            StepOp::MotionBlur { angle, distance } => filter(
                self,
                FilterId::MotionBlur,
                &[
                    ("angle", P::Float(*angle as f32)),
                    ("distance", P::Float(*distance as f32)),
                ],
            ),
            StepOp::HighPass { radius } => filter(
                self,
                FilterId::HighPass,
                &[("radius", P::Float(*radius as f32))],
            ),
            StepOp::SmartSharpen {
                amount,
                radius,
                noise_reduction,
            } => filter(
                self,
                FilterId::SmartSharpen,
                &[
                    ("amount", P::Float(pct(*amount))),
                    ("radius", P::Float(*radius as f32)),
                    ("noise_floor", P::Float(pct(*noise_reduction))),
                ],
            ),
            StepOp::Export => self
                .dispatch(Action::Export)
                .map(|_| String::new())
                .map_err(|e| e.to_string()),
            _ => return None,
        })
    }

    fn play_transform(
        &mut self,
        target: TransformTarget,
        pivot: Pivot,
        offset: [f64; 2],
        scale: [f64; 2],
        angle: f64,
        skew: [f64; 2],
    ) -> Result<String, String> {
        let linear = transform_linear(scale, angle, skew);
        if !linear.determinant().is_finite() || linear.determinant().abs() < 1e-6 {
            return Err("the transform flattens everything to a line".to_string());
        }
        let shift = glam::vec2(offset[0] as f32, offset[1] as f32);
        let delta_about = |at: glam::Vec2| {
            glam::Affine2::from_translation(at + shift)
                * glam::Affine2::from_mat2(linear)
                * glam::Affine2::from_translation(-at)
        };
        match target {
            TransformTarget::Layer => {
                let command = {
                    let doc = self.active().ok_or("No document is open")?;
                    let id = doc.document.active_layer().ok_or("No layer is active")?;
                    let layer = doc
                        .document
                        .layers
                        .get(id)
                        .ok_or("The active layer is not in the tree")?;
                    if layer.locked.blocks_transform() {
                        return Err("The layer's position is locked".to_string());
                    }
                    let (w, h) = (doc.document.width() as f32, doc.document.height() as f32);
                    let bounds =
                        crate::tool_input::tight_document_bounds(&doc.document, &doc.tiles, id)
                            .filter(|r| r.width > 0 && r.height > 0)
                            .map_or((0.0, 0.0, w, h), |r| {
                                (
                                    r.x as f32,
                                    r.y as f32,
                                    (r.x + r.width as i64) as f32,
                                    (r.y + r.height as i64) as f32,
                                )
                            });
                    let delta = delta_about(pivot_in(pivot, bounds));
                    // Conjugated onto the layer's own transform through its
                    // parent chain, as Free Transform's commit does.
                    let total =
                        crate::interaction_geometry::document_transform_of(&doc.document, id, 0)
                            .map_err(|e| e.to_string())?;
                    let parent = total * layer.transform.inverse();
                    editor_core::Command::Transaction {
                        label: "Transform".to_string(),
                        commands: vec![editor_core::Command::TransformLayer {
                            layer_id: id,
                            matrix: (parent.inverse() * delta * parent).to_cols_array(),
                        }],
                    }
                };
                self.apply_command(command);
                Ok(String::new())
            }
            TransformTarget::Selection => {
                let canvas = canvas_rect(self)?;
                let doc = self.active().ok_or("No document is open")?;
                let current = doc.document.selection.clone();
                let Some((min, max)) = current.bounds() else {
                    return Err("There is no selection to transform".to_string());
                };
                let bounds = (min.x as f32, min.y as f32, max.x as f32, max.y as f32);
                let next = selection::transform_selection(
                    &current,
                    canvas,
                    delta_about(pivot_in(pivot, bounds)),
                    selection::ResampleFilter::Bilinear,
                )
                .map_err(|e| e.to_string())?;
                set_selection(self, next)
            }
        }
    }

    /// Photoshop's layer index: 1 is the bottom of the root stack.
    fn move_layer_to(&mut self, index: u32) -> Result<String, String> {
        let doc = self.active().ok_or("No document is open")?;
        let id = doc.document.active_layer().ok_or("No layer is active")?;
        let root = doc.document.layers.root();
        if !root.contains(&id) {
            return Err("only a top-level layer moves to an index".to_string());
        }
        let n = root.len();
        let from_bottom = (index.max(1) as usize).min(n);
        self.apply_command(editor_core::Command::MoveLayer {
            layer_id: id,
            parent: None,
            index: n - from_bottom,
        });
        Ok(String::new())
    }

    fn select_layer_ref(&mut self, which: &LayerRef) -> Result<String, String> {
        let doc = self.active().ok_or("No document is open")?;
        let order = doc.document.layers.iter_depth_first();
        let active = doc.document.active_layer();
        let at = active.and_then(|a| order.iter().position(|id| *id == a));
        let found = match which {
            LayerRef::Name(name) => order.iter().copied().find(|id| {
                doc.document
                    .layers
                    .get(*id)
                    .is_some_and(|l| &l.name == name)
            }),
            LayerRef::Index(i) => order
                .len()
                .checked_sub(*i as usize)
                .and_then(|k| order.get(k).copied()),
            LayerRef::Forward => at
                .and_then(|k| k.checked_sub(1))
                .and_then(|k| order.get(k).copied()),
            LayerRef::Backward => at.and_then(|k| order.get(k + 1).copied()),
            LayerRef::Front => order.first().copied(),
            LayerRef::Back => order.last().copied(),
        };
        let id = found.ok_or_else(|| format!("there is no layer {which:?}"))?;
        self.set_layer_selection(vec![id], Some(id));
        Ok(String::new())
    }

    fn play_color_range(
        &mut self,
        color: [f64; 3],
        fuzziness: f64,
        invert: bool,
    ) -> Result<String, String> {
        let [r, g, b] = color.map(|v| v.round().clamp(0.0, 255.0) as u8);
        let spec = ui::dialogs::ColorRangeSpec {
            color: [r, g, b, 255],
            fuzziness: fuzziness
                .round()
                .clamp(0.0, f64::from(ui::dialogs::color_range::MAX_FUZZINESS))
                as u32,
            invert,
        };
        let mask = {
            let doc = self.active().ok_or("No document is open")?;
            let layer = doc.document.active_layer().ok_or("No layer is active")?;
            let rgba = crate::menu_bridge::pixels::read_layer(doc, layer);
            spec.mask(&rgba, doc.document.width(), doc.document.height())?
        };
        set_selection(self, editor_core::Selection::Mask(mask))
    }

    fn play_modify(&mut self, op: ModifySelection, amount: f64) -> Result<String, String> {
        let spec = ui::dialogs::ModifySpec {
            op,
            amount: amount as f32,
        };
        if !spec.is_valid() {
            return Err(format!("{amount} px is outside what {op:?} accepts"));
        }
        let canvas = canvas_rect(self)?;
        let current = self
            .active()
            .ok_or("No document is open")?
            .document
            .selection
            .clone();
        let mask = selection::to_mask(&current, canvas).map_err(|e| e.to_string())?;
        let px = spec.whole_px();
        let next = match op {
            ModifySelection::Border => selection::border(&mask, px),
            ModifySelection::Smooth => selection::smooth(&mask, px),
            ModifySelection::Expand => selection::expand(&mask, px),
            ModifySelection::Contract => selection::contract(&mask, px),
            ModifySelection::Feather => selection::feather(&mask, spec.amount),
        }
        .map_err(|e| e.to_string())?;
        set_selection(self, editor_core::Selection::Mask(next))
    }
}
