//! Edit ▸ Fill… and Edit ▸ Stroke… — the two dialogs that paint over the
//! selection.
//!
//! They share one shape: a *what* (contents or geometry), a *how* (blend mode
//! and opacity) and a *where* (preserve transparency). The specs travel to the
//! application, whose selection machinery already knows the pixels — the
//! dialogs only decide what to ask for.

use egui::{vec2, Context};

use layer_model::BlendMode;

use super::action::DialogAction;
use super::chrome::{
    action_row, caption, hairline, modal, resolve, Dialog, DialogButton, DialogKeys, DialogOutcome,
    DialogWidth,
};
use super::color_edit::ColorEdit;
use super::controls::{checkbox_row, combo, numeric};
use super::ids;

/// What a Fill paints with.
#[derive(Clone, PartialEq, Debug, Default)]
pub enum FillContents {
    /// The foreground colour well.
    #[default]
    Foreground,
    /// The background colour well.
    Background,
    /// A colour picked here.
    Color([f32; 4]),
    /// A named pattern from the asset store. An unknown name refuses at apply
    /// time with a status the user can act on.
    Pattern(String),
    /// 50% grey, the neutral Photoshop offers.
    Gray50,
}

/// The kinds of [`FillContents`], for the combo box's stable labels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FillContentsKind {
    Foreground,
    Background,
    Color,
    Pattern,
    Gray50,
}

impl FillContentsKind {
    /// Every kind, in menu order.
    pub const ALL: [FillContentsKind; 5] = [
        Self::Foreground,
        Self::Background,
        Self::Color,
        Self::Pattern,
        Self::Gray50,
    ];

    /// The label the combo shows.
    pub fn label(self) -> &'static str {
        match self {
            Self::Foreground => "Foreground",
            Self::Background => "Background",
            Self::Color => "Colour",
            Self::Pattern => "Pattern",
            Self::Gray50 => "50% Grey",
        }
    }
}

impl FillContents {
    /// Which kind this value is, for the combo box.
    pub fn kind(&self) -> FillContentsKind {
        match self {
            Self::Foreground => FillContentsKind::Foreground,
            Self::Background => FillContentsKind::Background,
            Self::Color(_) => FillContentsKind::Color,
            Self::Pattern(_) => FillContentsKind::Pattern,
            Self::Gray50 => FillContentsKind::Gray50,
        }
    }
}

/// What the Fill dialog confirms with.
#[derive(Clone, PartialEq, Debug)]
pub struct FillSpec {
    pub contents: FillContents,
    pub blend: BlendMode,
    /// 0.0..=1.0, as a fraction of full strength.
    pub opacity: f32,
    /// Paint only where the layer already has alpha.
    pub preserve_transparency: bool,
}

impl Default for FillSpec {
    /// Photopea opens at the foreground colour, Normal, fully opaque.
    fn default() -> Self {
        Self {
            contents: FillContents::Foreground,
            blend: BlendMode::Normal,
            opacity: 1.0,
            preserve_transparency: false,
        }
    }
}

impl FillSpec {
    /// A fill is always committable; an empty selection is the engine's
    /// refusal, not the dialog's.
    pub fn is_valid(&self) -> bool {
        self.opacity.is_finite() && (0.0..=1.0).contains(&self.opacity)
    }
}

/// Where the stroke band sits relative to the selection's edge.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum StrokeLocation {
    /// Whole band inside the selection.
    #[default]
    Inside,
    /// Half in, half out — the straddling band.
    Center,
    /// Whole band outside the selection.
    Outside,
}

impl StrokeLocation {
    pub const ALL: [StrokeLocation; 3] = [
        StrokeLocation::Inside,
        StrokeLocation::Center,
        StrokeLocation::Outside,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Inside => "Inside",
            Self::Center => "Center",
            Self::Outside => "Outside",
        }
    }
}

/// What the Stroke dialog confirms with.
#[derive(Clone, PartialEq, Debug)]
pub struct StrokeSpec {
    /// Band width in pixels, 1..=250 like Photoshop's cap.
    pub width: u32,
    pub location: StrokeLocation,
    pub blend: BlendMode,
    /// 0.0..=1.0, as a fraction of full strength.
    pub opacity: f32,
    /// Paint only where the layer already has alpha.
    pub preserve_transparency: bool,
}

impl Default for StrokeSpec {
    /// Photopea opens at 3 px inside; so does this.
    fn default() -> Self {
        Self {
            width: 3,
            location: StrokeLocation::Inside,
            blend: BlendMode::Normal,
            opacity: 1.0,
            preserve_transparency: false,
        }
    }
}

impl StrokeSpec {
    pub fn is_valid(&self) -> bool {
        (1..=250).contains(&self.width)
            && self.opacity.is_finite()
            && (0.0..=1.0).contains(&self.opacity)
    }
}

/// Edit ▸ Fill…
pub struct FillDialog {
    spec: FillSpec,
    /// The colour picker behind the contents swatch.
    picker: ColorEdit<()>,
    /// The colour used while contents is [`FillContents::Color`].
    color: [f32; 4],
    /// The pattern name used while contents is [`FillContents::Pattern`].
    pattern: String,
    /// Names the asset store offers, shown in the pattern combo.
    pattern_names: Vec<String>,
}

impl FillDialog {
    pub fn new(spec: FillSpec, pattern_names: Vec<String>) -> Self {
        let color = match &spec.contents {
            FillContents::Color(c) => *c,
            _ => [0.0, 0.0, 0.0, 1.0],
        };
        let pattern = match &spec.contents {
            FillContents::Pattern(name) => name.clone(),
            _ => pattern_names.first().cloned().unwrap_or_default(),
        };
        Self {
            spec,
            picker: ColorEdit::new(),
            color,
            pattern,
            pattern_names,
        }
    }

    /// The dialog's current state as a spec: which kind is selected decides
    /// where the colour/pattern payload comes from.
    pub fn spec(&self) -> FillSpec {
        let mut spec = self.spec.clone();
        spec.contents = match self.spec.contents.kind() {
            FillContentsKind::Color => FillContents::Color(self.color),
            FillContentsKind::Pattern => FillContents::Pattern(self.pattern.clone()),
            FillContentsKind::Foreground => FillContents::Foreground,
            FillContentsKind::Background => FillContents::Background,
            FillContentsKind::Gray50 => FillContents::Gray50,
        };
        spec
    }

    /// The currently chosen colour, for the swatch that opens the picker.
    pub fn color(&self) -> [f32; 4] {
        self.color
    }

    /// The colour picker confirmed; only meaningful while contents is Colour.
    pub fn set_color(&mut self, rgba: [f32; 4]) {
        self.color = rgba;
        if self.spec.contents.kind() == FillContentsKind::Color {
            self.spec.contents = FillContents::Color(rgba);
        }
    }
}

/// Manual: [`ColorEdit`] holds a picker whose type chain is not `Debug`, and
/// the host's dialog enum derives `Debug` for its test messages.
impl std::fmt::Debug for FillDialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FillDialog")
            .field("spec", &self.spec)
            .field("color", &self.color)
            .field("pattern", &self.pattern)
            .finish_non_exhaustive()
    }
}

impl Dialog for FillDialog {
    fn title(&self) -> &'static str {
        "Fill"
    }

    fn confirm_label(&self) -> &'static str {
        "Fill"
    }

    fn confirm(&self) -> Option<DialogAction> {
        self.spec
            .is_valid()
            .then_some(DialogAction::Fill(Box::new(self.spec())))
    }

    fn blocked_reason(&self) -> Option<String> {
        if !self.spec.is_valid() {
            return Some("Opacity must be between 0% and 100%".to_string());
        }
        if self.spec.contents.kind() == FillContentsKind::Pattern && self.pattern_names.is_empty() {
            return Some(
                crate::strings::tr("ui.fill_stroke.no.patterns.are.defined.yet").to_string(),
            );
        }
        None
    }
}

/// Edit ▸ Stroke…
pub struct StrokeDialog {
    spec: StrokeSpec,
}

impl StrokeDialog {
    pub fn new(spec: StrokeSpec) -> Self {
        Self { spec }
    }

    pub fn spec(&self) -> StrokeSpec {
        self.spec.clone()
    }
}

impl std::fmt::Debug for StrokeDialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StrokeDialog")
            .field("spec", &self.spec)
            .finish()
    }
}

impl Dialog for StrokeDialog {
    fn title(&self) -> &'static str {
        "Stroke"
    }

    fn confirm_label(&self) -> &'static str {
        "Stroke"
    }

    fn confirm(&self) -> Option<DialogAction> {
        self.spec
            .is_valid()
            .then_some(DialogAction::Stroke(Box::new(self.spec())))
    }

    fn blocked_reason(&self) -> Option<String> {
        if !(1..=250).contains(&self.spec.width) {
            return Some(
                crate::strings::tr("ui.fill_stroke.width.must.be.between.1.and").to_string(),
            );
        }
        if !self.spec.is_valid() {
            return Some("Opacity must be between 0% and 100%".to_string());
        }
        None
    }
}

impl FillDialog {
    pub fn show(
        &mut self,
        ctx: &Context,
        sampler: Option<&dyn super::color_picker::ScreenSampler>,
    ) -> DialogOutcome<DialogAction> {
        let nested = self.picker.is_open();
        let keys = if nested {
            DialogKeys::NONE
        } else {
            DialogKeys::read(ctx)
        };
        let mut outcome = resolve(self, keys);
        let drawn = modal(
            ctx,
            "fill",
            self.title(),
            Some(crate::strings::tr(
                "ui.fill_stroke.fills.the.active.selection.with.the",
            )),
            DialogWidth::Narrow,
            |ui| self.body(ui),
        );
        if let Some(((), rgba)) = self.picker.show(ctx, "fill-contents", sampler) {
            self.set_color(rgba);
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
        caption(
            ui,
            crate::strings::tr("ui.fill_stroke.fills.the.active.selection.with.the"),
        );
        hairline(ui);

        design::section_header(ui, "Contents");
        design::inspector_field(ui, "Use", |ui| {
            let mut kind = self.spec.contents.kind();
            if combo(
                ui,
                ids::fill_contents(),
                &mut kind,
                &FillContentsKind::ALL,
                |k| k.label().to_string(),
                |_| None,
            ) {
                self.spec.contents = match kind {
                    FillContentsKind::Foreground => FillContents::Foreground,
                    FillContentsKind::Background => FillContents::Background,
                    FillContentsKind::Color => FillContents::Color(self.color),
                    FillContentsKind::Pattern => FillContents::Pattern(self.pattern.clone()),
                    FillContentsKind::Gray50 => FillContents::Gray50,
                };
            }
        });
        if self.spec.contents.kind() == FillContentsKind::Color {
            design::inspector_field(ui, "Colour", |ui| {
                let tokens = design::current_tokens(ui);
                let swatch = super::controls::swatch(
                    ui,
                    ids::fill_color(),
                    self.color,
                    vec2(tokens.metrics.control_height, tokens.metrics.control_height),
                );
                if swatch.clicked() {
                    self.picker.open((), self.color);
                }
            });
        }
        if self.spec.contents.kind() == FillContentsKind::Pattern {
            design::inspector_field(ui, "Pattern", |ui| {
                if !self.pattern_names.is_empty() {
                    let names: Vec<&str> = self.pattern_names.iter().map(|s| s.as_str()).collect();
                    let mut name: &str = self.pattern.as_str();
                    if combo(
                        ui,
                        ids::fill_pattern(),
                        &mut name,
                        &names,
                        |n| n.to_string(),
                        |_| None,
                    ) {
                        self.pattern = name.to_string();
                    }
                }
            });
        }

        self.paint_options(ui);
        action_row(
            ui,
            self.confirm_label(),
            self.blocked_reason().as_deref(),
            &[],
        )
    }

    fn paint_options(&mut self, ui: &mut egui::Ui) {
        design::section_header(ui, "Painting");
        design::inspector_field(ui, "Mode", |ui| {
            let mut blend = self.spec.blend;
            if combo(
                ui,
                ids::fill_blend(),
                &mut blend,
                &BlendMode::ALL,
                |m| m.label().to_string(),
                |_| None,
            ) {
                self.spec.blend = blend;
            }
        });
        design::inspector_field(ui, "Opacity", |ui| {
            let mut percent = f64::from(self.spec.opacity) * 100.0;
            if numeric(ui, &mut percent, 0.0..=100.0, 0, "%").changed() {
                self.spec.opacity = (percent / 100.0).clamp(0.0, 1.0) as f32;
            }
        });
        checkbox_row(
            ui,
            crate::strings::tr("ui.fill_stroke.preserve.transparency"),
            &mut self.spec.preserve_transparency,
        );
    }
}

impl StrokeDialog {
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<DialogAction> {
        let keys = DialogKeys::read(ctx);
        let mut outcome = resolve(self, keys);
        let drawn = modal(
            ctx,
            "stroke",
            self.title(),
            Some(crate::strings::tr(
                "ui.fill_stroke.paints.a.band.along.the.active",
            )),
            DialogWidth::Narrow,
            |ui| self.body(ui),
        );
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
        caption(
            ui,
            crate::strings::tr("ui.fill_stroke.paints.a.band.along.the.active"),
        );
        hairline(ui);

        design::section_header(ui, "Stroke");
        design::inspector_field(ui, "Width", |ui| {
            let mut width = f64::from(self.spec.width);
            if numeric(ui, &mut width, 1.0..=250.0, 0, "px").changed() {
                self.spec.width = (width.round().clamp(1.0, 250.0)) as u32;
            }
        });
        design::inspector_field(ui, "Location", |ui| {
            let mut location = self.spec.location;
            if combo(
                ui,
                ids::stroke_location(),
                &mut location,
                &StrokeLocation::ALL,
                |l| l.label().to_string(),
                |_| None,
            ) {
                self.spec.location = location;
            }
        });

        design::section_header(ui, "Painting");
        design::inspector_field(ui, "Mode", |ui| {
            let mut blend = self.spec.blend;
            if combo(
                ui,
                ids::stroke_blend(),
                &mut blend,
                &BlendMode::ALL,
                |m| m.label().to_string(),
                |_| None,
            ) {
                self.spec.blend = blend;
            }
        });
        design::inspector_field(ui, "Opacity", |ui| {
            let mut percent = f64::from(self.spec.opacity) * 100.0;
            if numeric(ui, &mut percent, 0.0..=100.0, 0, "%").changed() {
                self.spec.opacity = (percent / 100.0).clamp(0.0, 1.0) as f32;
            }
        });
        checkbox_row(
            ui,
            crate::strings::tr("ui.fill_stroke.preserve.transparency"),
            &mut self.spec.preserve_transparency,
        );

        action_row(
            ui,
            self.confirm_label(),
            self.blocked_reason().as_deref(),
            &[],
        )
    }
}
