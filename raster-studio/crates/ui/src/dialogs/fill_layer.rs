//! W9-B: Layer ▸ New Fill Layer ▸ Solid Color… / Gradient… / Pattern…, and
//! re-editing a live fill layer's contents.
//!
//! Photopea's three fill-layer dialogs. Confirming a new fill creates a
//! [`LayerKind::Fill`] layer — a live source the compositor evaluates, never
//! baked pixels — as one [`Command::Transaction`]; confirming over an existing
//! fill layer replaces its source with one [`Command::SetLayerKind`]. Either
//! way the edit is one undo step and travels the road every other edit does.
//!
//! * **Solid Color** is the colour picker itself, as in Photopea: its OK
//!   creates (or recolours) the layer.
//! * **Gradient** is the ramp (click it for the nested gradient editor), the
//!   style, angle and scale, and the reverse / dither switches.
//! * **Pattern** picks one of the defined patterns and its scale and angle;
//!   with none defined it says so and will not confirm.

use editor_core::Command;
use egui::Context;
use layer_model::{
    FillLayer, FillSource, GradientFill, GradientStyle, Layer, LayerId, LayerKind, PatternFill,
    PatternTile, Rgba,
};

use super::action::DialogAction;
use super::chrome::{
    action_row, caption, modal, resolve, Dialog, DialogButton, DialogKeys, DialogOutcome,
    DialogWidth,
};
use super::color_picker::{ColorPickerDialog, ColorValue, ScreenSampler};
use super::controls::{checkbox_row, combo};
use super::gradient_editor::{gradient_swatch, GradientEditorDialog};
use super::sizes;
use crate::menu::FillLayerKind;

/// Whether the dialog makes a new layer or edits an existing one.
#[derive(Clone, Debug, PartialEq)]
enum Target {
    /// A new fill layer, with the id it will be created under (fixed when
    /// the dialog opens, so `confirm` is a pure function of the state).
    New(LayerId),
    /// An existing fill layer.
    Edit { layer: LayerId },
}

/// The Solid Color / Gradient Fill / Pattern Fill dialog.
#[derive(Clone, Debug)]
pub struct FillLayerDialog {
    target: Target,
    fill: FillLayer,
    /// Solid Color's surface: the picker is the dialog.
    picker: ColorPickerDialog,
    /// Gradient's nested ramp editor, while it is open.
    gradient_edit: Option<GradientEditorDialog>,
    /// The patterns the Pattern kind offers.
    patterns: Vec<PatternTile>,
}

/// Stable ids, so a headless test can find the drawn controls.
pub mod ids {
    /// The gradient ramp swatch that opens the nested editor.
    pub fn gradient_ramp() -> egui::Id {
        egui::Id::new("raster-fill-layer-gradient-ramp")
    }
}

impl FillLayerDialog {
    /// A dialog creating a new fill layer of `source`'s kind, starting from
    /// `source` (the foreground colour, the foreground-to-background ramp,
    /// the latest pattern — whatever the host seeds it with).
    pub fn new_layer(source: FillSource, patterns: Vec<PatternTile>) -> Self {
        Self::with_target(
            Target::New(LayerId::new()),
            FillLayer::new(source),
            patterns,
        )
    }

    /// A dialog re-editing `layer`, whose current fill is `fill`.
    pub fn edit_layer(layer: LayerId, fill: FillLayer, patterns: Vec<PatternTile>) -> Self {
        Self::with_target(Target::Edit { layer }, fill, patterns)
    }

    fn with_target(target: Target, fill: FillLayer, patterns: Vec<PatternTile>) -> Self {
        let start = match &fill.source {
            FillSource::Solid { color } => *color,
            _ => [0.0, 0.0, 0.0, 1.0],
        };
        Self {
            target,
            fill,
            picker: ColorPickerDialog::new(ColorValue::new(start)),
            gradient_edit: None,
            patterns,
        }
    }

    /// The fill-layer kind the dialog edits.
    pub fn kind(&self) -> FillLayerKind {
        match self.fill.source {
            FillSource::Solid { .. } => FillLayerKind::SolidColor,
            FillSource::Gradient(_) => FillLayerKind::Gradient,
            FillSource::Pattern(_) => FillLayerKind::Pattern,
        }
    }

    /// The layer a confirmation creates or edits.
    pub fn layer(&self) -> LayerId {
        match &self.target {
            Target::New(id) => *id,
            Target::Edit { layer } => *layer,
        }
    }

    /// Whether the dialog edits an existing layer rather than creating one.
    pub fn is_edit(&self) -> bool {
        matches!(self.target, Target::Edit { .. })
    }

    /// The fill as the dialog would confirm it.
    pub fn fill(&self) -> FillLayer {
        let mut fill = self.fill.clone();
        if let FillSource::Solid { color } = &mut fill.source {
            *color = self.picker.color().rgba;
        }
        fill
    }

    /// Set the Solid Color's colour.
    pub fn set_color(&mut self, color: Rgba) {
        self.picker.set_color(ColorValue::new(color));
    }

    /// The gradient parameters, for the Gradient kind.
    pub fn gradient_mut(&mut self) -> Option<&mut GradientFill> {
        match &mut self.fill.source {
            FillSource::Gradient(g) => Some(g),
            _ => None,
        }
    }

    /// The pattern parameters, for the Pattern kind.
    pub fn pattern_mut(&mut self) -> Option<&mut PatternFill> {
        match &mut self.fill.source {
            FillSource::Pattern(p) => Some(p),
            _ => None,
        }
    }

    /// The nested gradient editor, while it is open.
    pub fn gradient_edit_mut(&mut self) -> Option<&mut GradientEditorDialog> {
        self.gradient_edit.as_mut()
    }

    /// Open the nested gradient editor over the current ramp.
    pub fn open_gradient_editor(&mut self) -> bool {
        let Some(g) = self.gradient_mut() else {
            return false;
        };
        let ramp = g.gradient.clone();
        self.gradient_edit = Some(GradientEditorDialog::new(ramp));
        true
    }

    /// Draw one frame and fold the keyboard and the action row into one
    /// outcome.
    pub fn show(
        &mut self,
        ctx: &Context,
        sampler: Option<&dyn ScreenSampler>,
    ) -> DialogOutcome<DialogAction> {
        if matches!(self.fill.source, FillSource::Solid { .. }) {
            // The picker is the whole dialog: its OK is this dialog's OK.
            return match self.picker.show(ctx, sampler) {
                DialogOutcome::Confirmed(DialogAction::SetColor(color)) => {
                    self.picker.set_color(color);
                    self.confirm()
                        .map_or(DialogOutcome::Open, DialogOutcome::Confirmed)
                }
                DialogOutcome::Confirmed(_) | DialogOutcome::Open => DialogOutcome::Open,
                DialogOutcome::Cancelled => DialogOutcome::Cancelled,
            };
        }
        let nested = self.gradient_edit.is_some();
        let keys = if nested {
            DialogKeys::NONE
        } else {
            DialogKeys::read(ctx)
        };
        let mut outcome = resolve(self, keys);
        let drawn = modal(
            ctx,
            "fill-layer",
            self.title(),
            None,
            DialogWidth::Standard,
            |ui| self.body(ui),
        );
        if let Some(editor) = self.gradient_edit.as_mut() {
            match editor.show_nested(ctx, "fill-layer-gradient", sampler) {
                DialogOutcome::Confirmed(DialogAction::SetGradient(gradient)) => {
                    self.gradient_edit = None;
                    if let Some(g) = self.gradient_mut() {
                        g.gradient = *gradient;
                    }
                }
                DialogOutcome::Confirmed(_) | DialogOutcome::Cancelled => {
                    self.gradient_edit = None;
                }
                DialogOutcome::Open => {}
            }
        }
        if nested {
            return DialogOutcome::Open;
        }
        if let Some(Some(button)) = drawn {
            outcome = match button {
                DialogButton::Cancel => DialogOutcome::Cancelled,
                DialogButton::Confirm => self
                    .confirm()
                    .map_or(DialogOutcome::Open, DialogOutcome::Confirmed),
                DialogButton::Extra(_) => DialogOutcome::Open,
            };
        }
        outcome
    }

    fn body(&mut self, ui: &mut egui::Ui) -> Option<DialogButton> {
        let mut open_ramp = false;
        let patterns = self.patterns.clone();
        match &mut self.fill.source {
            FillSource::Solid { .. } => {}
            FillSource::Gradient(g) => {
                design::inspector_field(ui, "Gradient", |ui| {
                    open_ramp =
                        gradient_swatch(ui, ids::gradient_ramp(), &g.gradient, sizes::swatch())
                            .clicked();
                });
                design::inspector_field(ui, "Style", |ui| {
                    combo(
                        ui,
                        "fill-layer-gradient-style",
                        &mut g.style,
                        &[
                            GradientStyle::Linear,
                            GradientStyle::Radial,
                            GradientStyle::Angle,
                            GradientStyle::Reflected,
                            GradientStyle::Diamond,
                        ],
                        |s| style_label(s).to_string(),
                        |_| None,
                    );
                });
                design::slider_row(ui, "Angle", &mut g.angle_deg, -180.0..=180.0);
                design::slider_row(ui, "Scale", &mut g.scale, 0.1..=1.5);
                checkbox_row(ui, "Reverse", &mut g.reverse);
                checkbox_row(ui, "Dither", &mut g.dither);
            }
            FillSource::Pattern(p) => {
                let current = p.tile.as_ref();
                let mut pick = current
                    .and_then(|t| {
                        patterns
                            .iter()
                            .position(|q| q.content_hash() == t.content_hash())
                    })
                    .unwrap_or(usize::MAX);
                let current_name = current.map_or_else(String::new, |t| t.name().to_string());
                let options: Vec<usize> = (0..patterns.len()).collect();
                design::inspector_field(ui, "Pattern", |ui| {
                    if patterns.is_empty() {
                        caption(
                            ui,
                            crate::strings::tr("ui.fill_stroke.no.patterns.are.defined.yet"),
                        );
                    } else if combo(
                        ui,
                        "fill-layer-pattern",
                        &mut pick,
                        &options,
                        |i| {
                            patterns
                                .get(i)
                                .map_or_else(|| current_name.clone(), |t| t.name().to_string())
                        },
                        |_| None,
                    ) {
                        if let Some(tile) = patterns.get(pick) {
                            p.tile = Some(tile.clone());
                        }
                    }
                });
                design::slider_row(ui, "Scale", &mut p.scale, 0.01..=10.0);
                design::slider_row(ui, "Angle", &mut p.angle_deg, -180.0..=180.0);
                checkbox_row(ui, "Link", &mut p.link_with_layer);
            }
        }
        if open_ramp {
            self.open_gradient_editor();
        }
        action_row(
            ui,
            self.confirm_label(),
            self.blocked_reason().as_deref(),
            &[],
        )
    }
}

const fn style_label(style: GradientStyle) -> &'static str {
    match style {
        GradientStyle::Linear => "Linear",
        GradientStyle::Radial => "Radial",
        GradientStyle::Angle => "Angle",
        GradientStyle::Reflected => "Reflected",
        GradientStyle::Diamond => "Diamond",
    }
}

impl Dialog for FillLayerDialog {
    fn title(&self) -> &'static str {
        // The menu row's own words, without its trailing ellipsis.
        self.kind()
            .label()
            .trim_end_matches(|c: char| !c.is_ascii_alphanumeric())
    }

    fn confirm_label(&self) -> &'static str {
        if self.is_edit() {
            "Apply"
        } else {
            "Create"
        }
    }

    fn confirm(&self) -> Option<DialogAction> {
        let fill = self.fill();
        if let FillSource::Pattern(p) = &fill.source {
            p.tile.as_ref()?;
        }
        let command = match &self.target {
            Target::New(id) => {
                let mut layer = Layer::with_kind(fill.source.kind_name(), LayerKind::Fill(fill));
                layer.id = *id;
                Command::Transaction {
                    label: layer.name.clone(),
                    commands: vec![Command::create_layer(layer)],
                }
            }
            Target::Edit { layer } => Command::SetLayerKind {
                layer_id: *layer,
                kind: Box::new(LayerKind::Fill(fill)),
            },
        };
        Some(DialogAction::Command(Box::new(command)))
    }

    fn blocked_reason(&self) -> Option<String> {
        match &self.fill.source {
            FillSource::Pattern(p) if p.tile.is_none() => {
                Some(crate::strings::tr("ui.fill_stroke.no.patterns.are.defined.yet").to_string())
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_layer_of(action: Option<DialogAction>) -> Layer {
        match action {
            Some(DialogAction::Command(command)) => match *command {
                Command::Transaction { commands, .. } => match commands.as_slice() {
                    [Command::CreateLayer { layer }] => (**layer).clone(),
                    other => panic!("confirmed to {other:?}"),
                },
                other => panic!("confirmed to {other:?}"),
            },
            other => panic!("confirmed to {other:?}"),
        }
    }

    #[test]
    fn a_new_solid_color_confirms_to_one_live_fill_layer_in_the_chosen_colour() {
        let mut dialog = FillLayerDialog::new_layer(FillSource::default(), Vec::new());
        assert_eq!(dialog.title(), "Solid Color");
        dialog.set_color([1.0, 0.0, 0.0, 1.0]);
        let layer = new_layer_of(dialog.confirm());
        assert_eq!(layer.id, dialog.layer(), "confirm is pure: the id is fixed");
        match layer.kind {
            LayerKind::Fill(FillLayer {
                source: FillSource::Solid { color },
            }) => assert_eq!(ColorValue::new(color).to_bytes(), [255, 0, 0, 255]),
            other => panic!("created {other:?}"),
        }
    }

    #[test]
    fn editing_a_fill_layer_confirms_to_one_kind_edit() {
        let id = LayerId::new();
        let mut dialog = FillLayerDialog::edit_layer(
            id,
            FillLayer::new(FillSource::Gradient(GradientFill::default())),
            Vec::new(),
        );
        assert!(dialog.is_edit());
        dialog.gradient_mut().unwrap().angle_deg = 12.0;
        match dialog.confirm() {
            Some(DialogAction::Command(command)) => match *command {
                Command::SetLayerKind { layer_id, kind } => {
                    assert_eq!(layer_id, id);
                    match *kind {
                        LayerKind::Fill(FillLayer {
                            source: FillSource::Gradient(g),
                        }) => assert_eq!(g.angle_deg, 12.0),
                        other => panic!("edited to {other:?}"),
                    }
                }
                other => panic!("confirmed to {other:?}"),
            },
            other => panic!("confirmed to {other:?}"),
        }
    }

    #[test]
    fn a_pattern_fill_with_no_pattern_is_blocked_with_a_reason() {
        let dialog =
            FillLayerDialog::new_layer(FillSource::Pattern(PatternFill::default()), Vec::new());
        assert_eq!(dialog.confirm(), None);
        assert!(dialog.blocked_reason().is_some());
        let tile = PatternTile::new("Dots", 1, 1, vec![1, 2, 3, 255]).unwrap();
        let dialog = FillLayerDialog::new_layer(
            FillSource::Pattern(PatternFill {
                tile: Some(tile.clone()),
                ..PatternFill::default()
            }),
            vec![tile],
        );
        assert!(dialog.confirm().is_some());
        assert_eq!(dialog.blocked_reason(), None);
    }

    #[test]
    fn enter_confirms_and_escape_cancels() {
        let dialog =
            FillLayerDialog::new_layer(FillSource::Gradient(GradientFill::default()), Vec::new());
        assert!(matches!(
            resolve(&dialog, DialogKeys::CONFIRM),
            DialogOutcome::Confirmed(DialogAction::Command(_))
        ));
        assert_eq!(
            resolve(&dialog, DialogKeys::CANCEL),
            DialogOutcome::Cancelled
        );
    }

    #[test]
    fn every_kind_draws_in_both_appearances() {
        let tile = PatternTile::new("Dots", 1, 1, vec![1, 2, 3, 255]).unwrap();
        for source in [
            FillSource::default(),
            FillSource::Gradient(GradientFill::default()),
            FillSource::Pattern(PatternFill {
                tile: Some(tile.clone()),
                ..PatternFill::default()
            }),
        ] {
            super::super::chrome::test_support::frame_both_themes(|ctx| {
                let mut dialog = FillLayerDialog::new_layer(source.clone(), vec![tile.clone()]);
                assert!(dialog.show(ctx, None).is_open());
            });
        }
    }
}
