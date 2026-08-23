//! The layer style editor.
//!
//! Ten effects, each optional, each with its own parameter block. The list on
//! the left is the *enable* control and the panel on the right edits whichever
//! one is selected; the whole [`LayerEffects`] block is replaced in one
//! [`Command::SetLayerProperties`], which is what makes an entire style change
//! a single undo step.
//!
//! # About the preview
//!
//! The preview here is deliberately labelled *approximate*. There is no layer
//! effect renderer in the compositor yet, and drawing one inside a dialog would
//! be a second implementation that silently disagrees with the first. What is
//! **not** approximate is the geometry: [`shadow_offset`] is the shared
//! angle-and-distance arithmetic every one of these effects needs, and it is
//! tested here rather than guessed at in three places.

use design::{
    color32, current_tokens, egui_theme::rounding, tokens::palette::ColorRole, tokens::Radius,
    tokens::Space,
};
use editor_core::{Command, LayerPatch};
use egui::{vec2, Context, Rect, Sense};
use layer_model::{
    BevelEffect, ColorOverlayEffect, FillStyle, GlowEffect, GradientOverlayEffect, LayerEffects,
    LayerId, PatternOverlayEffect, Rgba, SatinEffect, ShadowEffect, StrokeEffect, StrokePosition,
};

use super::action::DialogAction;
use super::chrome::{
    action_row, caption, hairline, modal, Dialog, DialogButton, DialogKeys, DialogOutcome,
    DialogWidth,
};
use super::color_edit::ColorEdit;
use super::color_picker::ScreenSampler;
use super::controls::{checkbox_row, combo, swatch};
use super::gradient_editor::{gradient_swatch, GradientEditorDialog};
use super::{ids, sizes};

/// One entry in the effect list.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum EffectKind {
    DropShadow,
    InnerShadow,
    OuterGlow,
    InnerGlow,
    BevelEmboss,
    Satin,
    ColorOverlay,
    GradientOverlay,
    PatternOverlay,
    Stroke,
}

impl EffectKind {
    /// Every effect, in the order the list shows them.
    pub const ALL: [EffectKind; 10] = [
        Self::BevelEmboss,
        Self::Stroke,
        Self::InnerShadow,
        Self::InnerGlow,
        Self::Satin,
        Self::ColorOverlay,
        Self::GradientOverlay,
        Self::PatternOverlay,
        Self::OuterGlow,
        Self::DropShadow,
    ];

    /// List label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::DropShadow => "Drop Shadow",
            Self::InnerShadow => "Inner Shadow",
            Self::OuterGlow => "Outer Glow",
            Self::InnerGlow => "Inner Glow",
            Self::BevelEmboss => "Bevel & Emboss",
            Self::Satin => "Satin",
            Self::ColorOverlay => "Color Overlay",
            Self::GradientOverlay => "Gradient Overlay",
            Self::PatternOverlay => "Pattern Overlay",
            Self::Stroke => "Stroke",
        }
    }

    /// Whether this effect is switched on in `effects`.
    pub fn is_enabled(self, effects: &LayerEffects) -> bool {
        match self {
            Self::DropShadow => effects.drop_shadow.is_some(),
            Self::InnerShadow => effects.inner_shadow.is_some(),
            Self::OuterGlow => effects.outer_glow.is_some(),
            Self::InnerGlow => effects.inner_glow.is_some(),
            Self::BevelEmboss => effects.bevel_emboss.is_some(),
            Self::Satin => effects.satin.is_some(),
            Self::ColorOverlay => effects.color_overlay.is_some(),
            Self::GradientOverlay => effects.gradient_overlay.is_some(),
            Self::PatternOverlay => effects.pattern_overlay.is_some(),
            Self::Stroke => effects.stroke.is_some(),
        }
    }

    /// Switch this effect on (installing defaults) or off (dropping its block).
    ///
    /// Enabling an effect that is already on keeps the parameters the user has
    /// set — a checkbox must not reset what it re-checks.
    pub fn set_enabled(self, effects: &mut LayerEffects, on: bool) {
        macro_rules! toggle {
            ($field:ident, $default:expr) => {
                if on {
                    if effects.$field.is_none() {
                        effects.$field = Some($default);
                    }
                } else {
                    effects.$field = None;
                }
            };
        }
        match self {
            Self::DropShadow => toggle!(drop_shadow, ShadowEffect::default()),
            Self::InnerShadow => toggle!(inner_shadow, ShadowEffect::default()),
            Self::OuterGlow => toggle!(outer_glow, GlowEffect::default()),
            Self::InnerGlow => toggle!(inner_glow, GlowEffect::default()),
            Self::BevelEmboss => toggle!(bevel_emboss, BevelEffect::default()),
            Self::Satin => toggle!(satin, SatinEffect::default()),
            Self::ColorOverlay => toggle!(color_overlay, ColorOverlayEffect::default()),
            Self::GradientOverlay => toggle!(gradient_overlay, GradientOverlayEffect::default()),
            Self::PatternOverlay => toggle!(pattern_overlay, PatternOverlayEffect::default()),
            Self::Stroke => toggle!(stroke, StrokeEffect::default()),
        }
    }

    /// Whether this effect has an angle that can follow the global light.
    pub const fn uses_light(self) -> bool {
        matches!(
            self,
            Self::DropShadow | Self::InnerShadow | Self::BevelEmboss
        )
    }

    /// Whether this *kind* of effect has a single colour the user picks.
    ///
    /// Bevel & Emboss draws from the layer's own tones, and the two ramp
    /// overlays carry a whole gradient or pattern rather than one colour, so
    /// those three have no swatch at all — as opposed to a swatch that does
    /// nothing, which is what this list exists to prevent.
    ///
    /// A property of the kind, not of one effect's current state: a glow or a
    /// stroke filled with a gradient rather than a solid also draws no swatch,
    /// which is why [`LayerStyleDialog::effect_color`] — which reads the actual
    /// fill — is what the dialog consults before opening the picker.
    pub const fn has_color(self) -> bool {
        matches!(
            self,
            Self::DropShadow
                | Self::InnerShadow
                | Self::OuterGlow
                | Self::InnerGlow
                | Self::Satin
                | Self::ColorOverlay
                | Self::Stroke
        )
    }
}

/// Where a shadow lands, given the light's angle and the shadow's distance.
///
/// `angle_deg` is the direction the light comes *from*, measured
/// counter-clockwise from the positive x axis, which is the convention every
/// one of these effects stores. Screen y grows downward, so a light from above
/// (90 degrees) casts a shadow *down* the screen.
///
/// Shared rather than re-derived per effect: three effects need this number and
/// three sign conventions is three bugs.
pub fn shadow_offset(angle_deg: f32, distance_px: f32) -> (f32, f32) {
    let radians = angle_deg.to_radians();
    (-distance_px * radians.cos(), distance_px * radians.sin())
}

/// The layer style editor.
#[derive(Clone, Debug)]
pub struct LayerStyleDialog {
    layer: LayerId,
    layer_name: String,
    effects: LayerEffects,
    original: LayerEffects,
    selected: EffectKind,
    global_light_angle: f32,
    /// The nested colour picker, when a swatch has been clicked.
    color_edit: ColorEdit<EffectKind>,
    /// The nested gradient editor, when the Gradient Overlay's ramp has been
    /// clicked. The overlay is the only effect here with a gradient, so it
    /// needs no target beside it — unlike the colour picker, which five
    /// different effects share.
    gradient_edit: Option<GradientEditorDialog>,
}

impl LayerStyleDialog {
    /// Open on `layer`'s current effect block.
    pub fn new(layer: LayerId, layer_name: impl Into<String>, effects: LayerEffects) -> Self {
        let global_light_angle = effects
            .drop_shadow
            .as_ref()
            .filter(|s| s.use_global_light)
            .map_or(120.0, |s| s.angle_deg);
        Self {
            layer,
            layer_name: layer_name.into(),
            effects: effects.clone(),
            original: effects,
            selected: EffectKind::DropShadow,
            global_light_angle,
            color_edit: ColorEdit::new(),
            gradient_edit: None,
        }
    }

    /// The layer being styled.
    pub fn layer(&self) -> LayerId {
        self.layer
    }

    /// The effect block as edited.
    pub fn effects(&self) -> &LayerEffects {
        &self.effects
    }

    /// Mutable access to the effect block.
    pub fn effects_mut(&mut self) -> &mut LayerEffects {
        &mut self.effects
    }

    /// The effect the parameter panel is showing.
    pub fn selected(&self) -> EffectKind {
        self.selected
    }

    /// Show a different effect's parameters.
    pub fn select(&mut self, kind: EffectKind) {
        self.selected = kind;
    }

    /// Whether `kind` is switched on.
    pub fn is_enabled(&self, kind: EffectKind) -> bool {
        kind.is_enabled(&self.effects)
    }

    /// Switch an effect on or off.
    pub fn set_enabled(&mut self, kind: EffectKind, on: bool) {
        kind.set_enabled(&mut self.effects, on);
        if on && kind.uses_light() {
            self.apply_global_light();
        }
    }

    /// The shared light angle, in degrees.
    pub fn global_light_angle(&self) -> f32 {
        self.global_light_angle
    }

    /// Move the global light. Every effect with `use_global_light` follows it;
    /// the ones that opted out keep their own angle.
    pub fn set_global_light_angle(&mut self, degrees: f32) {
        self.global_light_angle = degrees.rem_euclid(360.0);
        self.apply_global_light();
    }

    fn apply_global_light(&mut self) {
        let angle = self.global_light_angle;
        for shadow in [
            self.effects.drop_shadow.as_mut(),
            self.effects.inner_shadow.as_mut(),
        ]
        .into_iter()
        .flatten()
        {
            if shadow.use_global_light {
                shadow.angle_deg = angle;
            }
        }
        if let Some(bevel) = self.effects.bevel_emboss.as_mut() {
            if bevel.use_global_light {
                bevel.angle_deg = angle;
            }
        }
    }

    /// Whether anything has changed since the dialog opened.
    pub fn is_modified(&self) -> bool {
        self.effects != self.original
    }

    /// Drop every effect.
    pub fn clear_all(&mut self) {
        for kind in EffectKind::ALL {
            kind.set_enabled(&mut self.effects, false);
        }
    }

    /// Where the drop shadow lands, if there is one.
    pub fn drop_shadow_offset(&self) -> Option<(f32, f32)> {
        self.effects
            .drop_shadow
            .as_ref()
            .map(|s| shadow_offset(s.angle_deg, s.distance_px))
    }

    /// The colour of `kind`, when it is on and has one.
    pub fn effect_color(&self, kind: EffectKind) -> Option<[f32; 4]> {
        match kind {
            EffectKind::DropShadow => self.effects.drop_shadow.as_ref().map(|s| s.color),
            EffectKind::InnerShadow => self.effects.inner_shadow.as_ref().map(|s| s.color),
            EffectKind::OuterGlow => self.effects.outer_glow.as_ref().and_then(solid_fill),
            EffectKind::InnerGlow => self.effects.inner_glow.as_ref().and_then(solid_fill),
            EffectKind::Satin => self.effects.satin.as_ref().map(|s| s.color),
            EffectKind::ColorOverlay => self.effects.color_overlay.as_ref().map(|o| o.color),
            EffectKind::Stroke => match self.effects.stroke.as_ref().map(|s| &s.fill) {
                Some(FillStyle::Solid(color)) => Some(*color),
                _ => None,
            },
            EffectKind::BevelEmboss | EffectKind::GradientOverlay | EffectKind::PatternOverlay => {
                None
            }
        }
    }

    /// Set the colour of `kind`.
    ///
    /// Returns `false` — changing nothing — when the effect is off or has no
    /// single colour to set, so a caller cannot quietly write into an effect
    /// that has no swatch.
    pub fn set_effect_color(&mut self, kind: EffectKind, rgba: [f32; 4]) -> bool {
        let rgba = [
            rgba[0].clamp(0.0, 1.0),
            rgba[1].clamp(0.0, 1.0),
            rgba[2].clamp(0.0, 1.0),
            rgba[3].clamp(0.0, 1.0),
        ];
        match kind {
            EffectKind::DropShadow => set_field(self.effects.drop_shadow.as_mut(), rgba),
            EffectKind::InnerShadow => set_field(self.effects.inner_shadow.as_mut(), rgba),
            EffectKind::OuterGlow => set_solid_fill(self.effects.outer_glow.as_mut(), rgba),
            EffectKind::InnerGlow => set_solid_fill(self.effects.inner_glow.as_mut(), rgba),
            EffectKind::Satin => match self.effects.satin.as_mut() {
                Some(satin) => {
                    satin.color = rgba;
                    true
                }
                None => false,
            },
            EffectKind::ColorOverlay => match self.effects.color_overlay.as_mut() {
                Some(overlay) => {
                    overlay.color = rgba;
                    true
                }
                None => false,
            },
            EffectKind::Stroke => match self.effects.stroke.as_mut() {
                Some(stroke) => match &mut stroke.fill {
                    FillStyle::Solid(color) => {
                        *color = rgba;
                        true
                    }
                    _ => false,
                },
                None => false,
            },
            EffectKind::BevelEmboss | EffectKind::GradientOverlay | EffectKind::PatternOverlay => {
                false
            }
        }
    }

    /// The gradient of `kind`, when it is on and has one.
    ///
    /// Only the Gradient Overlay does today. It is written as a match rather
    /// than a field read so a second gradient-bearing effect — a gradient
    /// stroke fill — has an obvious place to join.
    pub fn effect_gradient(&self, kind: EffectKind) -> Option<&layer_model::Gradient> {
        match kind {
            EffectKind::GradientOverlay => {
                self.effects.gradient_overlay.as_ref().map(|o| &o.gradient)
            }
            _ => None,
        }
    }

    /// Replace the gradient of `kind`.
    ///
    /// Returns `false` — changing nothing — when the effect is off or has no
    /// ramp, the same contract [`LayerStyleDialog::set_effect_color`] has.
    pub fn set_effect_gradient(
        &mut self,
        kind: EffectKind,
        gradient: layer_model::Gradient,
    ) -> bool {
        match kind {
            EffectKind::GradientOverlay => match self.effects.gradient_overlay.as_mut() {
                Some(overlay) => {
                    overlay.gradient = gradient;
                    true
                }
                None => false,
            },
            _ => false,
        }
    }

    /// Open the gradient editor on `kind`'s ramp.
    ///
    /// A no-op when the effect has no ramp, so the only way the editor can be
    /// up is over an effect it can actually write back to.
    pub fn open_gradient_editor(&mut self, kind: EffectKind) -> bool {
        match self.effect_gradient(kind).cloned() {
            Some(gradient) => {
                self.gradient_edit = Some(GradientEditorDialog::new(gradient));
                true
            }
            None => false,
        }
    }

    /// The nested gradient editor, when the ramp has been clicked.
    pub fn gradient_edit(&self) -> Option<&GradientEditorDialog> {
        self.gradient_edit.as_ref()
    }

    /// Mutable access to it, so a caller — or a test — can drive the editor
    /// without synthesising pointer input.
    pub fn gradient_edit_mut(&mut self) -> Option<&mut GradientEditorDialog> {
        self.gradient_edit.as_mut()
    }

    /// The nested colour picker, when a swatch has been clicked.
    pub fn color_edit(&self) -> &ColorEdit<EffectKind> {
        &self.color_edit
    }

    /// Mutable access to it, so a caller (or a test) can drive the picker
    /// without synthesising pointer input.
    pub fn color_edit_mut(&mut self) -> &mut ColorEdit<EffectKind> {
        &mut self.color_edit
    }

    /// Draw the dialog for one frame.
    ///
    /// `sampler` is passed straight through to the nested colour picker's
    /// eyedropper; `None` draws that button disabled with its reason.
    pub fn show(
        &mut self,
        ctx: &Context,
        sampler: Option<&dyn ScreenSampler>,
    ) -> DialogOutcome<DialogAction> {
        // While the picker is up it owns the keyboard: Escape closes it, not
        // the dialog underneath, and Enter must not commit a style the user is
        // still choosing a colour for.
        let nested = self.color_edit.is_open() || self.gradient_edit.is_some();
        let keys = if nested {
            DialogKeys::NONE
        } else {
            DialogKeys::read(ctx)
        };
        let mut outcome = super::chrome::resolve(self, keys);
        let drawn = modal(
            ctx,
            "layer-style",
            self.title(),
            Some("Effects apply to the whole layer and undo as one step."),
            DialogWidth::Split,
            |ui| self.body(ui),
        );
        if let Some((kind, rgba)) = self.color_edit.show(ctx, "layer-style-color", sampler) {
            self.set_effect_color(kind, rgba);
        }
        if let Some(editor) = self.gradient_edit.as_mut() {
            match editor.show_nested(ctx, "layer-style-gradient", sampler) {
                DialogOutcome::Confirmed(DialogAction::SetGradient(gradient)) => {
                    self.gradient_edit = None;
                    self.set_effect_gradient(EffectKind::GradientOverlay, *gradient);
                }
                DialogOutcome::Confirmed(_) => {
                    // The gradient editor's only action is `SetGradient`.
                    self.gradient_edit = None;
                }
                DialogOutcome::Cancelled => self.gradient_edit = None,
                DialogOutcome::Open => {}
            }
        }
        if nested {
            // The action row under an open picker is not what the user is
            // aiming at.
            return DialogOutcome::Open;
        }
        if let Some(Some(button)) = drawn {
            outcome = match button {
                DialogButton::Cancel => DialogOutcome::Cancelled,
                DialogButton::Confirm => self
                    .confirm()
                    .map_or(DialogOutcome::Open, DialogOutcome::Confirmed),
                DialogButton::Extra(_) => {
                    self.clear_all();
                    DialogOutcome::Open
                }
            };
        }
        outcome
    }

    fn body(&mut self, ui: &mut egui::Ui) -> Option<DialogButton> {
        caption(ui, format!("Layer: {}", self.layer_name));
        ui.add_space(Space::Small.pt());
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(sizes::sidebar_width());
                self.effect_list(ui);
            });
            ui.add_space(Space::Large.pt());
            ui.vertical(|ui| {
                ui.set_width(sizes::params_column_width());
                self.parameters(ui);
            });
            ui.add_space(Space::Large.pt());
            ui.vertical(|ui| {
                design::section_header(ui, "Preview");
                self.preview(ui);
            });
        });
        hairline(ui);
        ui.add_space(Space::Small.pt());
        action_row(
            ui,
            self.confirm_label(),
            self.blocked_reason().as_deref(),
            &["Clear All"],
        )
    }

    fn effect_list(&mut self, ui: &mut egui::Ui) {
        design::section_header(ui, "Effects");
        let mut master = self.effects.enabled;
        if checkbox_row(ui, "Styles enabled", &mut master).changed() {
            self.effects.enabled = master;
        }
        ui.add_space(Space::XSmall.pt());
        for kind in EffectKind::ALL {
            ui.horizontal(|ui| {
                let mut on = self.is_enabled(kind);
                if checkbox_row(ui, "", &mut on).changed() {
                    self.set_enabled(kind, on);
                }
                if design::list_row(ui, kind.label(), self.selected == kind).clicked() {
                    self.select(kind);
                }
            });
        }
        ui.add_space(Space::Small.pt());
        let mut angle = self.global_light_angle;
        if design::slider_row(ui, "Global light", &mut angle, 0.0..=360.0).changed() {
            self.set_global_light_angle(angle);
        }
    }

    fn parameters(&mut self, ui: &mut egui::Ui) {
        let kind = self.selected;
        design::section_header(ui, kind.label());
        if !self.is_enabled(kind) {
            caption(ui, "This effect is off. Tick it in the list to edit it.");
            return;
        }
        let light = self.global_light_angle;
        // Set by whichever swatch was clicked; drained after the borrow of
        // `self.effects` ends, which is what lets the picker be opened from
        // inside a `&mut` on the effect it edits.
        let mut open_picker = false;
        let mut open_gradient = false;
        match kind {
            EffectKind::DropShadow | EffectKind::InnerShadow => {
                let shadow = if kind == EffectKind::DropShadow {
                    self.effects.drop_shadow.as_mut()
                } else {
                    self.effects.inner_shadow.as_mut()
                };
                if let Some(shadow) = shadow {
                    open_picker = shadow_params(ui, shadow, light, ids::effect_color(kind));
                }
            }
            EffectKind::OuterGlow | EffectKind::InnerGlow => {
                let glow = if kind == EffectKind::OuterGlow {
                    self.effects.outer_glow.as_mut()
                } else {
                    self.effects.inner_glow.as_mut()
                };
                if let Some(glow) = glow {
                    open_picker = glow_params(ui, glow, ids::effect_color(kind));
                }
            }
            EffectKind::BevelEmboss => {
                if let Some(bevel) = self.effects.bevel_emboss.as_mut() {
                    bevel_params(ui, bevel, light);
                }
            }
            EffectKind::Satin => {
                if let Some(satin) = self.effects.satin.as_mut() {
                    open_picker = satin_params(ui, satin, ids::effect_color(kind));
                }
            }
            EffectKind::ColorOverlay => {
                if let Some(overlay) = self.effects.color_overlay.as_mut() {
                    design::slider_row(ui, "Opacity", &mut overlay.opacity, 0.0..=1.0);
                    design::inspector_field(ui, "Color", |ui| {
                        open_picker |=
                            swatch(ui, ids::effect_color(kind), overlay.color, sizes::swatch())
                                .clicked();
                    });
                }
            }
            EffectKind::GradientOverlay => {
                if let Some(overlay) = self.effects.gradient_overlay.as_mut() {
                    design::slider_row(ui, "Opacity", &mut overlay.opacity, 0.0..=1.0);
                    design::slider_row(ui, "Angle", &mut overlay.angle_deg, 0.0..=360.0);
                    design::slider_row(ui, "Scale", &mut overlay.scale, 0.1..=10.0);
                    checkbox_row(ui, "Reverse", &mut overlay.reverse);
                    checkbox_row(ui, "Align with layer", &mut overlay.align_with_layer);
                    checkbox_row(ui, "Dither", &mut overlay.dither);
                    design::inspector_field(ui, "Gradient", |ui| {
                        open_gradient |= gradient_swatch(
                            ui,
                            ids::effect_gradient(kind),
                            &overlay.gradient,
                            sizes::swatch(),
                        )
                        .on_hover_text("Edit this ramp")
                        .clicked();
                    });
                    caption(ui, "Click the ramp to edit its stops.");
                }
            }
            EffectKind::PatternOverlay => {
                if let Some(overlay) = self.effects.pattern_overlay.as_mut() {
                    design::slider_row(ui, "Opacity", &mut overlay.opacity, 0.0..=1.0);
                    design::slider_row(ui, "Scale", &mut overlay.pattern.scale, 0.1..=10.0);
                    design::slider_row(ui, "Angle", &mut overlay.pattern.angle_deg, 0.0..=360.0);
                    checkbox_row(ui, "Link with layer", &mut overlay.pattern.link_with_layer);
                    if overlay.pattern.asset.is_none() {
                        caption(ui, "No pattern chosen — the overlay paints nothing.");
                    }
                }
            }
            EffectKind::Stroke => {
                if let Some(stroke) = self.effects.stroke.as_mut() {
                    design::slider_row(ui, "Size", &mut stroke.size_px, 0.0..=250.0);
                    design::slider_row(ui, "Opacity", &mut stroke.opacity, 0.0..=1.0);
                    design::inspector_field(ui, "Position", |ui| {
                        combo(
                            ui,
                            "ls-stroke-position",
                            &mut stroke.position,
                            &[
                                StrokePosition::Outside,
                                StrokePosition::Inside,
                                StrokePosition::Center,
                            ],
                            |p| {
                                match p {
                                    StrokePosition::Outside => "Outside",
                                    StrokePosition::Inside => "Inside",
                                    StrokePosition::Center => "Center",
                                }
                                .to_string()
                            },
                            |_| None,
                        );
                    });
                    if let FillStyle::Solid(color) = &mut stroke.fill {
                        design::inspector_field(ui, "Color", |ui| {
                            open_picker |=
                                swatch(ui, ids::effect_color(kind), *color, sizes::swatch())
                                    .clicked();
                        });
                    }
                    checkbox_row(ui, "Overprint", &mut stroke.overprint);
                }
            }
        }
        if open_picker {
            if let Some(color) = self.effect_color(kind) {
                self.color_edit.open(kind, color);
            }
        }
        if open_gradient {
            self.open_gradient_editor(kind);
        }
    }

    /// A schematic of the style: the layer silhouette with the effects that
    /// have a screen-space geometry drawn around it.
    fn preview(&mut self, ui: &mut egui::Ui) {
        let t = current_tokens(ui);
        let size = sizes::style_preview();
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        if ui.is_rect_visible(rect) {
            let radius = Radius::Medium.resolve(&t.radii, size.y);
            ui.painter().rect_filled(
                rect,
                rounding(radius),
                color32(t.palette.color(ColorRole::SurfaceSunken)),
            );
            let shape = Rect::from_center_size(rect.center(), size * 0.45);
            let shape_radius = Radius::Small.resolve(&t.radii, shape.height());
            let on = self.effects.enabled;

            if on {
                if let Some(shadow) = &self.effects.drop_shadow {
                    let (dx, dy) = shadow_offset(shadow.angle_deg, shadow.distance_px);
                    let offset = shape.translate(vec2(dx, dy));
                    ui.painter().rect_filled(
                        offset.expand(shadow.size_px * 0.25),
                        rounding(shape_radius),
                        with_alpha(shadow.color, shadow.opacity * 0.6),
                    );
                }
                if let Some(glow) = &self.effects.outer_glow {
                    if let FillStyle::Solid(color) = glow.fill {
                        ui.painter().rect_stroke(
                            shape.expand(glow.size_px * 0.3),
                            rounding(shape_radius),
                            egui::Stroke::new(
                                (glow.size_px * 0.3).max(1.0),
                                with_alpha(color, glow.opacity * 0.5),
                            ),
                        );
                    }
                }
            }

            let base = if on {
                self.effects
                    .color_overlay
                    .as_ref()
                    .map(|o| with_alpha(o.color, o.opacity))
                    .unwrap_or_else(|| color32(t.palette.color(ColorRole::TextPrimary)))
            } else {
                color32(t.palette.color(ColorRole::TextPrimary))
            };
            ui.painter()
                .rect_filled(shape, rounding(shape_radius), base);

            if on {
                if let Some(stroke) = &self.effects.stroke {
                    if let FillStyle::Solid(color) = stroke.fill {
                        ui.painter().rect_stroke(
                            shape,
                            rounding(shape_radius),
                            egui::Stroke::new(
                                stroke.size_px.max(0.5),
                                with_alpha(color, stroke.opacity),
                            ),
                        );
                    }
                }
            }
        }
        caption(
            ui,
            "Approximate. The composited result is what the canvas shows.",
        );
    }
}

/// Returns `true` when the colour swatch was clicked.
fn shadow_params(
    ui: &mut egui::Ui,
    shadow: &mut ShadowEffect,
    global_light: f32,
    swatch_id: egui::Id,
) -> bool {
    let mut clicked = false;
    design::slider_row(ui, "Opacity", &mut shadow.opacity, 0.0..=1.0);
    design::inspector_field(ui, "Color", |ui| {
        clicked = swatch(ui, swatch_id, shadow.color, sizes::swatch()).clicked();
    });
    let mut use_global = shadow.use_global_light;
    if checkbox_row(ui, "Use global light", &mut use_global).changed() {
        shadow.use_global_light = use_global;
        if use_global {
            shadow.angle_deg = global_light;
        }
    }
    ui.add_enabled_ui(!shadow.use_global_light, |ui| {
        design::slider_row(ui, "Angle", &mut shadow.angle_deg, 0.0..=360.0);
    });
    design::slider_row(ui, "Distance", &mut shadow.distance_px, 0.0..=250.0);
    design::slider_row(ui, "Spread", &mut shadow.spread, 0.0..=1.0);
    design::slider_row(ui, "Size", &mut shadow.size_px, 0.0..=250.0);
    design::slider_row(ui, "Noise", &mut shadow.noise, 0.0..=1.0);
    checkbox_row(ui, "Layer knocks out drop shadow", &mut shadow.knockout);
    let (dx, dy) = shadow_offset(shadow.angle_deg, shadow.distance_px);
    caption(ui, format!("Offset {dx:.1}, {dy:.1} px"));
    clicked
}

/// Returns `true` when the colour swatch was clicked.
fn glow_params(ui: &mut egui::Ui, glow: &mut GlowEffect, swatch_id: egui::Id) -> bool {
    let mut clicked = false;
    design::slider_row(ui, "Opacity", &mut glow.opacity, 0.0..=1.0);
    if let FillStyle::Solid(color) = &mut glow.fill {
        design::inspector_field(ui, "Color", |ui| {
            clicked = swatch(ui, swatch_id, *color, sizes::swatch()).clicked();
        });
    }
    design::slider_row(ui, "Spread", &mut glow.spread, 0.0..=1.0);
    design::slider_row(ui, "Size", &mut glow.size_px, 0.0..=250.0);
    design::slider_row(ui, "Range", &mut glow.range, 0.0..=1.0);
    design::slider_row(ui, "Jitter", &mut glow.jitter, 0.0..=1.0);
    design::slider_row(ui, "Noise", &mut glow.noise, 0.0..=1.0);
    clicked
}

fn bevel_params(ui: &mut egui::Ui, bevel: &mut BevelEffect, global_light: f32) {
    design::slider_row(ui, "Depth", &mut bevel.depth, 0.0..=10.0);
    design::slider_row(ui, "Size", &mut bevel.size_px, 0.0..=250.0);
    design::slider_row(ui, "Soften", &mut bevel.soften_px, 0.0..=16.0);
    let mut use_global = bevel.use_global_light;
    if checkbox_row(ui, "Use global light", &mut use_global).changed() {
        bevel.use_global_light = use_global;
        if use_global {
            bevel.angle_deg = global_light;
        }
    }
    ui.add_enabled_ui(!bevel.use_global_light, |ui| {
        design::slider_row(ui, "Angle", &mut bevel.angle_deg, 0.0..=360.0);
    });
    design::slider_row(ui, "Altitude", &mut bevel.altitude_deg, 0.0..=90.0);
    design::slider_row(ui, "Highlight", &mut bevel.highlight_opacity, 0.0..=1.0);
    design::slider_row(ui, "Shadow", &mut bevel.shadow_opacity, 0.0..=1.0);
}

/// Returns `true` when the colour swatch was clicked.
fn satin_params(ui: &mut egui::Ui, satin: &mut SatinEffect, swatch_id: egui::Id) -> bool {
    let mut clicked = false;
    design::slider_row(ui, "Opacity", &mut satin.opacity, 0.0..=1.0);
    design::slider_row(ui, "Angle", &mut satin.angle_deg, 0.0..=360.0);
    design::slider_row(ui, "Distance", &mut satin.distance_px, 0.0..=250.0);
    design::slider_row(ui, "Size", &mut satin.size_px, 0.0..=250.0);
    checkbox_row(ui, "Invert", &mut satin.invert);
    design::inspector_field(ui, "Color", |ui| {
        clicked = swatch(ui, swatch_id, satin.color, sizes::swatch()).clicked();
    });
    clicked
}

/// A glow's colour, when its fill is a solid one.
fn solid_fill(glow: &GlowEffect) -> Option<Rgba> {
    match glow.fill {
        FillStyle::Solid(color) => Some(color),
        _ => None,
    }
}

fn set_field(shadow: Option<&mut ShadowEffect>, rgba: Rgba) -> bool {
    match shadow {
        Some(shadow) => {
            shadow.color = rgba;
            true
        }
        None => false,
    }
}

fn set_solid_fill(glow: Option<&mut GlowEffect>, rgba: Rgba) -> bool {
    match glow.map(|g| &mut g.fill) {
        Some(FillStyle::Solid(color)) => {
            *color = rgba;
            true
        }
        _ => false,
    }
}

fn with_alpha(color: Rgba, opacity: f32) -> egui::Color32 {
    super::controls::color_of([
        color[0],
        color[1],
        color[2],
        (color[3] * opacity).clamp(0.0, 1.0),
    ])
}

impl Dialog for LayerStyleDialog {
    fn title(&self) -> &'static str {
        "Layer Style"
    }

    fn confirm_label(&self) -> &'static str {
        "Apply Style"
    }

    fn confirm(&self) -> Option<DialogAction> {
        Some(DialogAction::Command(Box::new(
            Command::SetLayerProperties {
                layer_id: self.layer,
                patch: LayerPatch {
                    effects: Some(Box::new(self.effects.clone())),
                    ..LayerPatch::default()
                },
            },
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialogs::chrome::test_support::{frame_both_themes, Harness};
    use crate::dialogs::color_picker::ColorValue;
    use layer_model::LayerId;

    fn dialog() -> LayerStyleDialog {
        LayerStyleDialog::new(LayerId::new(), "Headline", LayerEffects::default())
    }

    #[test]
    fn every_effect_can_be_switched_on_and_off_independently() {
        let mut dialog = dialog();
        for kind in EffectKind::ALL {
            assert!(!dialog.is_enabled(kind), "{kind:?} started on");
            dialog.set_enabled(kind, true);
            assert!(dialog.is_enabled(kind), "{kind:?} did not switch on");
            for other in EffectKind::ALL {
                if other != kind {
                    assert!(!dialog.is_enabled(other), "{kind:?} also enabled {other:?}");
                }
            }
            dialog.set_enabled(kind, false);
            assert!(!dialog.is_enabled(kind), "{kind:?} did not switch off");
        }
    }

    #[test]
    fn re_enabling_an_effect_keeps_the_parameters_that_were_set() {
        let mut dialog = dialog();
        dialog.set_enabled(EffectKind::DropShadow, true);
        dialog.effects_mut().drop_shadow.as_mut().unwrap().size_px = 77.0;
        // Ticking an already-ticked box must not reset it.
        dialog.set_enabled(EffectKind::DropShadow, true);
        assert_eq!(dialog.effects().drop_shadow.as_ref().unwrap().size_px, 77.0);
    }

    #[test]
    fn switching_an_effect_off_drops_its_block_entirely() {
        let mut dialog = dialog();
        dialog.set_enabled(EffectKind::Stroke, true);
        dialog.set_enabled(EffectKind::Stroke, false);
        assert!(dialog.effects().stroke.is_none());
    }

    #[test]
    fn clear_all_removes_every_effect() {
        let mut dialog = dialog();
        for kind in EffectKind::ALL {
            dialog.set_enabled(kind, true);
        }
        dialog.clear_all();
        for kind in EffectKind::ALL {
            assert!(!dialog.is_enabled(kind), "{kind:?} survived Clear All");
        }
    }

    #[test]
    fn the_shadow_offset_follows_the_light_the_way_the_screen_does() {
        // Light from above casts the shadow downward (screen y grows down).
        let (dx, dy) = shadow_offset(90.0, 10.0);
        assert!(dx.abs() < 1e-5, "dx was {dx}");
        assert!((dy - 10.0).abs() < 1e-5, "dy was {dy}");
        // Light from the right casts it left.
        let (dx, dy) = shadow_offset(0.0, 10.0);
        assert!((dx + 10.0).abs() < 1e-5, "dx was {dx}");
        assert!(dy.abs() < 1e-5, "dy was {dy}");
        // Light from the left casts it right.
        let (dx, _) = shadow_offset(180.0, 10.0);
        assert!((dx - 10.0).abs() < 1e-4, "dx was {dx}");
        // Zero distance never moves it, whatever the angle.
        for angle in [0.0, 45.0, 137.0, 359.0] {
            assert_eq!(shadow_offset(angle, 0.0), (-0.0, 0.0));
        }
    }

    #[test]
    fn the_offset_magnitude_is_the_distance_at_every_angle() {
        for angle in (0..360).step_by(7) {
            let (dx, dy) = shadow_offset(angle as f32, 25.0);
            let length = (dx * dx + dy * dy).sqrt();
            assert!((length - 25.0).abs() < 1e-3, "angle {angle} gave {length}");
        }
    }

    #[test]
    fn the_global_light_moves_only_the_effects_that_follow_it() {
        let mut dialog = dialog();
        dialog.set_enabled(EffectKind::DropShadow, true);
        dialog.set_enabled(EffectKind::InnerShadow, true);
        dialog.set_enabled(EffectKind::BevelEmboss, true);
        dialog
            .effects_mut()
            .inner_shadow
            .as_mut()
            .unwrap()
            .use_global_light = false;
        dialog
            .effects_mut()
            .inner_shadow
            .as_mut()
            .unwrap()
            .angle_deg = 15.0;

        dialog.set_global_light_angle(200.0);
        assert_eq!(
            dialog.effects().drop_shadow.as_ref().unwrap().angle_deg,
            200.0
        );
        assert_eq!(
            dialog.effects().bevel_emboss.as_ref().unwrap().angle_deg,
            200.0
        );
        assert_eq!(
            dialog.effects().inner_shadow.as_ref().unwrap().angle_deg,
            15.0,
            "an effect that opted out of the global light was moved anyway"
        );
    }

    #[test]
    fn the_global_light_wraps_rather_than_clamping() {
        let mut dialog = dialog();
        dialog.set_global_light_angle(-90.0);
        assert_eq!(dialog.global_light_angle(), 270.0);
        dialog.set_global_light_angle(450.0);
        assert_eq!(dialog.global_light_angle(), 90.0);
    }

    #[test]
    fn enabling_a_light_driven_effect_adopts_the_current_light() {
        let mut dialog = dialog();
        dialog.set_global_light_angle(33.0);
        dialog.set_enabled(EffectKind::DropShadow, true);
        assert_eq!(
            dialog.effects().drop_shadow.as_ref().unwrap().angle_deg,
            33.0
        );
    }

    #[test]
    fn only_the_light_driven_effects_claim_to_use_the_light() {
        for kind in EffectKind::ALL {
            let expected = matches!(
                kind,
                EffectKind::DropShadow | EffectKind::InnerShadow | EffectKind::BevelEmboss
            );
            assert_eq!(kind.uses_light(), expected, "{kind:?}");
        }
    }

    #[test]
    fn the_dialog_reports_whether_anything_changed() {
        let mut dialog = dialog();
        assert!(!dialog.is_modified());
        dialog.set_enabled(EffectKind::Satin, true);
        assert!(dialog.is_modified());
        dialog.set_enabled(EffectKind::Satin, false);
        assert!(!dialog.is_modified());
    }

    #[test]
    fn confirm_produces_one_command_carrying_the_whole_style() {
        let mut dialog = dialog();
        dialog.set_enabled(EffectKind::DropShadow, true);
        dialog.set_enabled(EffectKind::Stroke, true);
        match dialog.confirm() {
            Some(DialogAction::Command(command)) => match *command {
                Command::SetLayerProperties { layer_id, patch } => {
                    assert_eq!(layer_id, dialog.layer());
                    let effects = patch.effects.expect("the style rides in the patch");
                    assert!(effects.drop_shadow.is_some());
                    assert!(effects.stroke.is_some());
                    // Nothing else on the layer is touched.
                    assert!(patch.name.is_none());
                    assert!(patch.opacity.is_none());
                    assert!(patch.transform.is_none());
                }
                other => panic!("expected SetLayerProperties, got {other:?}"),
            },
            other => panic!("expected a command, got {other:?}"),
        }
    }

    #[test]
    fn cancel_produces_nothing() {
        let dialog = dialog();
        assert_eq!(
            super::super::chrome::resolve(&dialog, DialogKeys::CANCEL),
            DialogOutcome::Cancelled
        );
    }

    #[test]
    fn the_drop_shadow_offset_is_exposed_only_when_there_is_one() {
        let mut dialog = dialog();
        assert!(dialog.drop_shadow_offset().is_none());
        dialog.set_enabled(EffectKind::DropShadow, true);
        dialog.set_global_light_angle(90.0);
        dialog
            .effects_mut()
            .drop_shadow
            .as_mut()
            .unwrap()
            .distance_px = 8.0;
        let (dx, dy) = dialog.drop_shadow_offset().unwrap();
        assert!(dx.abs() < 1e-5 && (dy - 8.0).abs() < 1e-5, "{dx}, {dy}");
    }

    #[test]
    fn every_effects_panel_draws_in_both_appearances() {
        for kind in EffectKind::ALL {
            frame_both_themes(|ctx| {
                let mut dialog = dialog();
                dialog.set_enabled(kind, true);
                dialog.select(kind);
                assert!(dialog.show(ctx, None).is_open());
            });
            // And with the effect off, so the "this effect is off" path draws.
            frame_both_themes(|ctx| {
                let mut dialog = dialog();
                dialog.select(kind);
                assert!(dialog.show(ctx, None).is_open());
            });
        }
    }

    /// The colour of every effect that has one, read back off the effect block
    /// rather than off the dialog's own accessor, so the accessor cannot agree
    /// with itself.
    fn stored_color(dialog: &LayerStyleDialog, kind: EffectKind) -> Option<Rgba> {
        let effects = dialog.effects();
        match kind {
            EffectKind::DropShadow => effects.drop_shadow.as_ref().map(|s| s.color),
            EffectKind::InnerShadow => effects.inner_shadow.as_ref().map(|s| s.color),
            EffectKind::OuterGlow => effects.outer_glow.as_ref().and_then(super::solid_fill),
            EffectKind::InnerGlow => effects.inner_glow.as_ref().and_then(super::solid_fill),
            EffectKind::Satin => effects.satin.as_ref().map(|s| s.color),
            EffectKind::ColorOverlay => effects.color_overlay.as_ref().map(|o| o.color),
            EffectKind::Stroke => match effects.stroke.as_ref().map(|s| &s.fill) {
                Some(FillStyle::Solid(color)) => Some(*color),
                _ => None,
            },
            _ => None,
        }
    }

    #[test]
    fn clicking_an_effects_swatch_opens_the_picker_and_the_chosen_colour_lands() {
        // The defect this pins: five `swatch(..)` calls whose Response was
        // dropped. Every layer effect's colour was frozen at its default,
        // because no code path in the crate could change one — while the
        // dialog's own preview faithfully drew a colour the user could not
        // pick. This drives the real drawn rectangle.
        for kind in EffectKind::ALL.into_iter().filter(|k| k.has_color()) {
            let h = Harness::new();
            let mut dialog = dialog();
            dialog.set_enabled(kind, true);
            dialog.select(kind);
            let before = stored_color(&dialog, kind).unwrap_or_else(|| panic!("{kind:?}"));

            h.click_widget(crate::dialogs::ids::effect_color(kind), |ctx| {
                dialog.show(ctx, None);
            });
            assert_eq!(
                dialog.color_edit().target(),
                Some(kind),
                "{kind:?}: the swatch did not open the picker"
            );

            let chosen = ColorValue::new([0.1, 0.8, 0.3, 0.75]);
            dialog
                .color_edit_mut()
                .picker_mut()
                .expect("the picker is up")
                .set_color(chosen);
            // Enter confirms the nested picker, not the dialog behind it.
            h.frame(Harness::key_events(egui::Key::Enter), |ctx| {
                assert!(
                    dialog.show(ctx, None).is_open(),
                    "{kind:?}: Enter closed the dialog under the picker"
                );
            });

            let after = stored_color(&dialog, kind).unwrap_or_else(|| panic!("{kind:?}"));
            assert_ne!(after, before, "{kind:?}: the colour did not change");
            assert_eq!(
                ColorValue::new(after).to_bytes(),
                chosen.to_bytes(),
                "{kind:?}: a different colour landed"
            );
            assert!(!dialog.color_edit().is_open(), "{kind:?}: picker stayed up");
        }
    }

    #[test]
    fn the_effects_without_a_swatch_do_not_draw_one() {
        // The other half of the rule: a control that cannot work must not be
        // drawn at all. Bevel, and the gradient and pattern overlays, carry no
        // single colour, so there is no swatch to click.
        for kind in EffectKind::ALL.into_iter().filter(|k| !k.has_color()) {
            let h = Harness::new();
            let mut dialog = dialog();
            dialog.set_enabled(kind, true);
            dialog.select(kind);
            h.frame(Vec::new(), |ctx| {
                dialog.show(ctx, None);
            });
            assert!(
                !h.was_drawn(crate::dialogs::ids::effect_color(kind)),
                "{kind:?} drew a colour swatch it cannot use"
            );
            assert!(dialog.effect_color(kind).is_none(), "{kind:?}");
            assert!(
                !dialog.set_effect_color(kind, [1.0, 0.0, 0.0, 1.0]),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn setting_a_colour_on_an_effect_that_is_off_changes_nothing() {
        let mut dialog = dialog();
        for kind in EffectKind::ALL {
            assert!(
                !dialog.set_effect_color(kind, [1.0, 0.0, 0.0, 1.0]),
                "{kind:?} accepted a colour while switched off"
            );
        }
        assert!(!dialog.is_modified());
    }

    /// The overlay's ramp as stored, for comparing before and after.
    fn overlay_gradient(dialog: &LayerStyleDialog) -> layer_model::Gradient {
        dialog
            .effect_gradient(EffectKind::GradientOverlay)
            .expect("the overlay is on")
            .clone()
    }

    #[test]
    fn the_gradient_overlays_ramp_opens_an_editor_that_writes_back_to_it() {
        // The defect this pins: the panel said "Edit the ramp itself in the
        // Gradient Editor", and no such path existed — the gradient editor
        // emits `SetGradient` with no target, so nothing could aim it at a
        // layer effect. The overlay's defining parameter was uneditable from
        // anywhere in the crate while the dialog told the user otherwise.
        let h = Harness::new();
        let mut dialog = dialog();
        dialog.set_enabled(EffectKind::GradientOverlay, true);
        dialog.select(EffectKind::GradientOverlay);
        let before = overlay_gradient(&dialog);

        h.click_widget(
            crate::dialogs::ids::effect_gradient(EffectKind::GradientOverlay),
            |ctx| {
                dialog.show(ctx, None);
            },
        );
        assert!(
            dialog.gradient_edit().is_some(),
            "the ramp opened no editor"
        );

        // Load a preset in the nested editor, then confirm it with Enter.
        dialog
            .gradient_edit_mut()
            .expect("the editor is up")
            .apply_preset(3);
        let chosen = dialog.gradient_edit().unwrap().gradient().clone();
        h.frame(Harness::key_events(egui::Key::Enter), |ctx| {
            assert!(
                dialog.show(ctx, None).is_open(),
                "Enter closed the dialog under the gradient editor"
            );
        });

        assert!(dialog.gradient_edit().is_none(), "the editor stayed up");
        let after = overlay_gradient(&dialog);
        assert_ne!(after, before, "the ramp did not change");
        assert_eq!(after, chosen, "a different ramp landed");

        // And it leaves in the one command the whole dialog produces.
        match dialog.confirm() {
            Some(DialogAction::Command(command)) => match *command {
                Command::SetLayerProperties { patch, .. } => {
                    let effects = patch.effects.clone().expect("effects in the patch");
                    assert_eq!(
                        effects.gradient_overlay.expect("the overlay").gradient,
                        chosen
                    );
                }
                other => panic!("expected a layer-properties command, got {other:?}"),
            },
            other => panic!("expected a command, got {other:?}"),
        }
    }

    #[test]
    fn cancelling_the_gradient_editor_leaves_the_overlay_alone() {
        let h = Harness::new();
        let mut dialog = dialog();
        dialog.set_enabled(EffectKind::GradientOverlay, true);
        dialog.select(EffectKind::GradientOverlay);
        let before = overlay_gradient(&dialog);

        h.click_widget(
            crate::dialogs::ids::effect_gradient(EffectKind::GradientOverlay),
            |ctx| {
                dialog.show(ctx, None);
            },
        );
        dialog
            .gradient_edit_mut()
            .expect("the editor is up")
            .apply_preset(3);
        h.frame(Harness::key_events(egui::Key::Escape), |ctx| {
            assert!(
                dialog.show(ctx, None).is_open(),
                "Escape closed the dialog under the gradient editor"
            );
        });
        assert!(dialog.gradient_edit().is_none());
        assert_eq!(overlay_gradient(&dialog), before);
    }

    #[test]
    fn only_an_effect_with_a_ramp_draws_one_or_can_be_given_one() {
        for kind in EffectKind::ALL {
            let h = Harness::new();
            let mut dialog = dialog();
            dialog.set_enabled(kind, true);
            dialog.select(kind);
            h.frame(Vec::new(), |ctx| {
                dialog.show(ctx, None);
            });
            let has_ramp = kind == EffectKind::GradientOverlay;
            assert_eq!(
                h.was_drawn(crate::dialogs::ids::effect_gradient(kind)),
                has_ramp,
                "{kind:?} drew the wrong thing for a gradient"
            );
            assert_eq!(dialog.effect_gradient(kind).is_some(), has_ramp, "{kind:?}");
            assert_eq!(
                dialog.set_effect_gradient(kind, layer_model::Gradient::default()),
                has_ramp,
                "{kind:?}"
            );
            assert_eq!(dialog.open_gradient_editor(kind), has_ramp, "{kind:?}");
        }
    }

    #[test]
    fn an_overlay_that_is_off_takes_no_gradient() {
        let mut dialog = dialog();
        assert!(dialog
            .effect_gradient(EffectKind::GradientOverlay)
            .is_none());
        assert!(!dialog.set_effect_gradient(
            EffectKind::GradientOverlay,
            layer_model::Gradient::default()
        ));
        assert!(!dialog.open_gradient_editor(EffectKind::GradientOverlay));
        assert!(dialog.gradient_edit().is_none());
        assert!(!dialog.is_modified());
    }

    #[test]
    fn cancelling_the_nested_picker_leaves_the_colour_alone() {
        let h = Harness::new();
        let mut dialog = dialog();
        dialog.set_enabled(EffectKind::DropShadow, true);
        dialog.select(EffectKind::DropShadow);
        let before = stored_color(&dialog, EffectKind::DropShadow).unwrap();

        h.click_widget(
            crate::dialogs::ids::effect_color(EffectKind::DropShadow),
            |ctx| {
                dialog.show(ctx, None);
            },
        );
        dialog
            .color_edit_mut()
            .picker_mut()
            .unwrap()
            .set_color(ColorValue::new([1.0, 0.0, 0.0, 1.0]));
        h.frame(Harness::key_events(egui::Key::Escape), |ctx| {
            assert!(
                dialog.show(ctx, None).is_open(),
                "Escape closed the dialog under the picker"
            );
        });
        assert!(!dialog.color_edit().is_open());
        assert_eq!(
            stored_color(&dialog, EffectKind::DropShadow).unwrap(),
            before
        );
    }
}
