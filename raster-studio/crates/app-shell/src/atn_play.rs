//! W13-E: playing an imported `.atn` step — each [`StepOp`] runs through
//! the route the user's own click or dialog OK takes, so a played step is
//! the same undo step, on the same layer, as doing it by hand.
//!
//! A child module of `actions_library` (declared there with `#[path]`).
//!
//! | Step | Route |
//! |---|---|
//! | Make layer, Save | [`Editor::dispatch`] (`Action::NewLayer`, `Action::Save`) |
//! | Select all / none / inverse, Invert, Desaturate, Equalize, 180° and flips | [`crate::menu_bridge::perform`] with the menu row's `MenuAction` |
//! | Rectangle selection | `Command::SetSelection` through [`Editor::apply_command`] |
//! | Fill | [`crate::menu_bridge::fill_selection_with`] (the Fill dialog's OK) |
//! | Image Size, Canvas Size | the dialog's spec through `resample_command` / `canvas_size_command` (the shell's OK route) |
//! | Brightness/Contrast | [`crate::menu_bridge::run_adjustment_kind`] |
//! | Gaussian Blur, Unsharp Mask, Median | [`crate::menu_bridge::run_filter_invocation`] (the Filter dialog's OK) |
//! | Rotate canvas 90° | [`Editor::rotate_canvas_90`]; any other angle `rotate_canvas_arbitrary` |

use asset_store::resources::atn::{FillWith, StepOp};
use ui::menu::{AdjustmentId, CanvasRotation, FilterId, MenuAction, TransformOp};

use super::{Action, Editor};

// W16-H: the steps `atn_more` reads, each through its menu or dialog route.
#[path = "atn_play_more.rs"]
mod more;
#[cfg(test)]
pub(super) use more::transform_linear;

fn right_angle(degrees: f64) -> Option<i32> {
    let turns = degrees.rem_euclid(360.0);
    [90, 180, 270]
        .into_iter()
        .find(|a| (turns - f64::from(*a)).abs() < 1e-9)
}

fn filter(
    editor: &mut Editor,
    id: FilterId,
    params: &[(&str, ui::dialogs::ParamValue)],
) -> Result<String, String> {
    let spec = ui::dialogs::filter_by_id(id).ok_or("that filter is not in this build")?;
    let mut values = ui::dialogs::FilterParams::defaults(spec.params);
    for (key, value) in params {
        if !values.set(key, *value) {
            return Err(format!("{} has no parameter {key}", spec.name()));
        }
    }
    crate::menu_bridge::run_filter_invocation(
        editor,
        &ui::dialogs::FilterInvocation {
            filter: spec,
            params: values,
        },
    )
}

impl Editor {
    /// Perform one interpreted `.atn` step on the active document.
    pub(super) fn perform_step_op(&mut self, op: &StepOp) -> Result<String, String> {
        use ui::dialogs::ParamValue as P;
        let menu = |editor: &mut Editor, action| crate::menu_bridge::perform(action, editor);
        let dispatch = |editor: &mut Editor, action| {
            editor
                .dispatch(action)
                .map(|_| String::new())
                .map_err(|e| e.to_string())
        };
        if self.active().is_none() {
            return Err("No document is open".to_string());
        }
        if let Some(result) = self.perform_w16_op(op) {
            return result;
        }
        match *op {
            StepOp::MakeLayer => dispatch(self, Action::NewLayer),
            StepOp::Save => dispatch(self, Action::Save),
            StepOp::SelectAll => menu(self, MenuAction::SelectAll),
            StepOp::Deselect => menu(self, MenuAction::Deselect),
            StepOp::InverseSelection => menu(self, MenuAction::InverseSelection),
            StepOp::SelectRect {
                left,
                top,
                right,
                bottom,
            } => {
                let px = |v: f64| v.round().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
                let (min, max) = (
                    glam::IVec2::new(px(left.min(right)), px(top.min(bottom))),
                    glam::IVec2::new(px(left.max(right)), px(top.max(bottom))),
                );
                if min.x == max.x || min.y == max.y {
                    return Err("the rectangle is empty".to_string());
                }
                self.apply_command(editor_core::Command::SetSelection {
                    selection: editor_core::Selection::Rect { min, max },
                });
                Ok(String::new())
            }
            StepOp::Fill { with, opacity } => {
                let contents = match with {
                    FillWith::Foreground => ui::dialogs::FillContents::Foreground,
                    FillWith::Background => ui::dialogs::FillContents::Background,
                    FillWith::Rgb([r, g, b]) => ui::dialogs::FillContents::Color([r, g, b, 1.0]),
                };
                crate::menu_bridge::fill_selection_with(
                    self,
                    &ui::dialogs::FillSpec {
                        contents,
                        opacity,
                        ..Default::default()
                    },
                )
            }
            StepOp::ImageSize { width, height } => {
                let doc = self.active_mut().ok_or("No document is open")?;
                let (w, h) = (doc.document.width(), doc.document.height());
                let (nw, nh) = match (width, height) {
                    (Some(a), Some(b)) => (a.resolve(w), b.resolve(h)),
                    (Some(a), None) => {
                        let nw = a.resolve(w);
                        (
                            nw,
                            ((f64::from(h) * f64::from(nw) / f64::from(w)).round() as u32).max(1),
                        )
                    }
                    (None, Some(b)) => {
                        let nh = b.resolve(h);
                        (
                            ((f64::from(w) * f64::from(nh) / f64::from(h)).round() as u32).max(1),
                            nh,
                        )
                    }
                    (None, None) => return Err("no size is given".to_string()),
                };
                let spec = ui::dialogs::ImageSizeSpec {
                    width: nw,
                    height: nh,
                    resolution_ppi: 72.0,
                    resample: Some(raster::ResampleFilter::Lanczos3),
                };
                if !spec.is_valid() {
                    return Err(format!("{nw} x {nh} is not a size this editor accepts"));
                }
                let command = doc.resample_command(&spec).map_err(|e| e.to_string())?;
                self.apply_command(command);
                Ok(String::new())
            }
            StepOp::CanvasSize {
                width,
                height,
                relative,
                horizontal,
                vertical,
            } => {
                let doc = self.active_mut().ok_or("No document is open")?;
                let (w, h) = (doc.document.width(), doc.document.height());
                let side = |l: Option<asset_store::resources::atn::Length>, of: u32| match l {
                    None => of,
                    Some(l) if relative => {
                        let add = match l {
                            asset_store::resources::atn::Length::Pixels(p) => p,
                            asset_store::resources::atn::Length::Percent(p) => {
                                f64::from(of) * p / 100.0
                            }
                        };
                        (f64::from(of) + add).round().max(1.0) as u32
                    }
                    Some(l) => l.resolve(of),
                };
                let (nw, nh) = (side(width, w), side(height, h));
                let anchor =
                    ui::dialogs::Anchor::at(vertical.min(2), horizontal.min(2)).unwrap_or_default();
                let spec = ui::dialogs::CanvasSizeSpec {
                    width: nw,
                    height: nh,
                    offset: anchor.offset((w, h), (nw, nh)),
                    anchor,
                    background: ui::dialogs::BackgroundContents::Transparent,
                };
                if !spec.is_valid() {
                    return Err(format!("{nw} x {nh} is not a size this editor accepts"));
                }
                let command = doc.canvas_size_command(&spec).map_err(|e| e.to_string())?;
                self.apply_command(command);
                Ok(String::new())
            }
            StepOp::Invert => menu(self, MenuAction::ApplyAdjustment(AdjustmentId::Invert)),
            StepOp::Desaturate => menu(self, MenuAction::ApplyAdjustment(AdjustmentId::Desaturate)),
            StepOp::Equalize => menu(self, MenuAction::ApplyAdjustment(AdjustmentId::Equalize)),
            StepOp::BrightnessContrast {
                brightness,
                contrast,
            } => {
                let unit = |v: f64, full: f64| ((v / full) as f32).clamp(-1.0, 1.0);
                let bc = adjustments::BrightnessContrast::new(
                    unit(brightness, 150.0),
                    unit(contrast, 100.0),
                )
                .map_err(|e| e.to_string())?;
                crate::menu_bridge::run_adjustment_kind(
                    self,
                    &adjustments::Adjustment::BrightnessContrast(bc),
                    "Brightness/Contrast",
                )
            }
            StepOp::GaussianBlur { radius } => filter(
                self,
                FilterId::GaussianBlur,
                &[("radius", P::Float(radius as f32))],
            ),
            StepOp::UnsharpMask {
                amount,
                radius,
                threshold,
            } => filter(
                self,
                FilterId::UnsharpMask,
                &[
                    ("amount", P::Float((amount / 100.0) as f32)),
                    ("radius", P::Float(radius as f32)),
                    ("threshold", P::Float((threshold / 255.0) as f32)),
                ],
            ),
            StepOp::Median { radius } => filter(
                self,
                FilterId::Median,
                &[("radius", P::Int(radius.round() as i32))],
            ),
            StepOp::RotateCanvas { degrees } => match right_angle(degrees) {
                Some(90) => self.rotate_canvas_90(true),
                Some(270) => self.rotate_canvas_90(false),
                Some(_) => menu(self, MenuAction::RotateCanvas(CanvasRotation::Deg180)),
                None => {
                    let doc = self.active_mut().ok_or("No document is open")?;
                    let command = doc
                        .rotate_canvas_arbitrary(degrees)
                        .map_err(|e| e.to_string())?;
                    self.apply_command(command);
                    Ok(String::new())
                }
            },
            StepOp::FlipCanvas { horizontal } => menu(
                self,
                MenuAction::RotateCanvas(if horizontal {
                    CanvasRotation::FlipHorizontal
                } else {
                    CanvasRotation::FlipVertical
                }),
            ),
            StepOp::RotateLayer { degrees } => {
                let op = match right_angle(degrees) {
                    Some(90) => TransformOp::Rotate90Cw,
                    Some(270) => TransformOp::Rotate90Ccw,
                    Some(_) => TransformOp::Rotate180,
                    None => {
                        return Err(format!(
                            "a layer rotation of {degrees}° is not a right angle; only 90°, 180° and 270° play"
                        ))
                    }
                };
                menu(self, MenuAction::Transform(op))
            }
            StepOp::FlipLayer { horizontal } => menu(
                self,
                MenuAction::Transform(if horizontal {
                    TransformOp::FlipHorizontal
                } else {
                    TransformOp::FlipVertical
                }),
            ),
            _ => Err(format!("{op:?} has no route in this application")),
        }
    }
}
