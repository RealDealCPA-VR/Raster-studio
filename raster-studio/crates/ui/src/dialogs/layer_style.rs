//! The layer style editor.
//!
//! Ten effects, each optional, each with its own parameter block. The list on
//! the left is the *enable* control and the panel on the right edits whichever
//! one is selected; the whole [`LayerEffects`] block is replaced in one
//! [`Command::SetLayerProperties`], which is what makes an entire style change
//! a single undo step.
//!
//! # The Blending Options page
//!
//! Above the effect list sits one more page, the one Layer ▸ Layer Style ▸
//! Blending Options… opens on: the layer's blend mode, opacity and fill
//! opacity, which are layer fields rather than effects. They ride the same
//! [`LayerPatch`] as the effect block, so a changed opacity and a new drop
//! shadow confirmed together are still one undo step. Photopea's page also
//! carries knockout and per-channel toggles; this engine has no such layer
//! fields, so the page does not draw controls it could not honour.
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
    BevelEffect, BlendMode, ColorOverlayEffect, FillStyle, GlowEffect, GradientOverlayEffect,
    LayerEffects, LayerId, PatternOverlayEffect, Rgba, SatinEffect, ShadowEffect, StrokeEffect,
    StrokePosition,
};

use super::action::DialogAction;
use super::chrome::{
    action_row, caption, hairline, modal, Dialog, DialogButton, DialogKeys, DialogOutcome,
    DialogWidth,
};
use super::color_edit::ColorEdit;
use super::color_picker::ScreenSampler;
use super::controls::{checkbox_row, combo, numeric, swatch};
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
    pub fn label(self) -> &'static str {
        match self {
            Self::DropShadow => crate::strings::tr("ui.layer_style.drop.shadow"),
            Self::InnerShadow => crate::strings::tr("ui.layer_style.inner.shadow"),
            Self::OuterGlow => crate::strings::tr("ui.layer_style.outer.glow"),
            Self::InnerGlow => crate::strings::tr("ui.layer_style.inner.glow"),
            Self::BevelEmboss => crate::strings::tr("ui.layer_style.bevel.emboss"),
            Self::Satin => "Satin",
            Self::ColorOverlay => crate::strings::tr("ui.layer_style.color.overlay"),
            Self::GradientOverlay => crate::strings::tr("ui.layer_style.gradient.overlay"),
            Self::PatternOverlay => crate::strings::tr("ui.layer_style.pattern.overlay"),
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

/// Which page the parameter panel shows: the layer's own blending fields, or
/// one effect's parameters.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum StylePage {
    /// Blend mode, opacity and fill opacity — the page Layer ▸ Layer Style ▸
    /// Blending Options… opens on.
    Blending,
    /// One effect's parameter block.
    Effect(EffectKind),
}

/// The layer's blending fields as the dialog edits them, in one place so the
/// "did anything change" comparison is a single equality.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Blending {
    mode: BlendMode,
    opacity: f32,
    fill_opacity: f32,
}

impl Default for Blending {
    fn default() -> Self {
        Self {
            mode: BlendMode::Normal,
            opacity: 1.0,
            fill_opacity: 1.0,
        }
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
    /// The layer's blend mode, opacity and fill as edited, and as they were
    /// when the dialog opened — only a changed field rides the patch.
    blending: Blending,
    original_blending: Blending,
    /// The page the parameter panel shows.
    page: StylePage,
    /// The effect the panel showed last (or shows now, when `page` is an
    /// effect page). Kept apart from `page` so switching to Blending Options
    /// and back lands on the effect the user was editing.
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
            blending: Blending::default(),
            original_blending: Blending::default(),
            page: StylePage::Effect(EffectKind::DropShadow),
            selected: EffectKind::DropShadow,
            global_light_angle,
            color_edit: ColorEdit::new(),
            gradient_edit: None,
        }
    }

    /// Seed the Blending Options page with the layer's current blend mode,
    /// opacity and fill opacity. These become the "unchanged" baseline: a
    /// confirm carries a blending field only when it moved from here.
    pub fn with_blending(mut self, mode: BlendMode, opacity: f32, fill_opacity: f32) -> Self {
        let blending = Blending {
            mode,
            opacity: clamp_unit(opacity),
            fill_opacity: clamp_unit(fill_opacity),
        };
        self.blending = blending;
        self.original_blending = blending;
        self
    }

    /// The layer being styled.
    pub fn layer(&self) -> LayerId {
        self.layer
    }

    /// The page the parameter panel shows.
    pub fn page(&self) -> StylePage {
        self.page
    }

    /// Show the Blending Options page.
    pub fn show_blending(&mut self) {
        self.page = StylePage::Blending;
    }

    /// The blend mode the Blending Options page holds.
    pub fn blend_mode(&self) -> BlendMode {
        self.blending.mode
    }

    /// Set the layer's blend mode.
    pub fn set_blend_mode(&mut self, mode: BlendMode) {
        self.blending.mode = mode;
    }

    /// The layer opacity the Blending Options page holds, `0..=1`.
    pub fn opacity(&self) -> f32 {
        self.blending.opacity
    }

    /// Set the layer opacity. Clamped to `0..=1`, the range
    /// [`LayerPatch::validate`] accepts, so the dialog can never confirm a
    /// value the command would refuse.
    pub fn set_opacity(&mut self, opacity: f32) {
        self.blending.opacity = clamp_unit(opacity);
    }

    /// The fill opacity the Blending Options page holds, `0..=1`.
    pub fn fill_opacity(&self) -> f32 {
        self.blending.fill_opacity
    }

    /// Set the layer's fill opacity. Clamped like [`Self::set_opacity`].
    pub fn set_fill_opacity(&mut self, fill_opacity: f32) {
        self.blending.fill_opacity = clamp_unit(fill_opacity);
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

    /// Show a different effect's parameters (leaving the Blending Options
    /// page, if that is what was showing).
    pub fn select(&mut self, kind: EffectKind) {
        self.selected = kind;
        self.page = StylePage::Effect(kind);
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

    /// Whether anything has changed since the dialog opened — an effect or
    /// one of the blending fields.
    pub fn is_modified(&self) -> bool {
        self.effects != self.original || self.blending != self.original_blending
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
            Some(crate::strings::tr(
                "ui.layer_style.effects.apply.to.the.whole.layer",
            )),
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
            &[crate::strings::tr("ui.layer_style.clear.all")],
        )
    }

    fn effect_list(&mut self, ui: &mut egui::Ui) {
        // The layer's own blending fields come first, as Photopea lists them:
        // they are not an effect, so the row has no enable checkbox.
        if design::list_row(
            ui,
            crate::strings::tr("ui.layer_style.blending.options"),
            self.page == StylePage::Blending,
        )
        .clicked()
        {
            self.show_blending();
        }
        ui.add_space(Space::XSmall.pt());
        design::section_header(ui, "Effects");
        let mut master = self.effects.enabled;
        if checkbox_row(
            ui,
            crate::strings::tr("ui.layer_style.styles.enabled"),
            &mut master,
        )
        .changed()
        {
            self.effects.enabled = master;
        }
        ui.add_space(Space::XSmall.pt());
        for kind in EffectKind::ALL {
            ui.horizontal(|ui| {
                let mut on = self.is_enabled(kind);
                if checkbox_row(ui, "", &mut on).changed() {
                    self.set_enabled(kind, on);
                }
                if design::list_row(ui, kind.label(), self.page == StylePage::Effect(kind))
                    .clicked()
                {
                    self.select(kind);
                }
            });
        }
        ui.add_space(Space::Small.pt());
        let mut angle = self.global_light_angle;
        if design::slider_row(
            ui,
            crate::strings::tr("ui.layer_style.global.light"),
            &mut angle,
            0.0..=360.0,
        )
        .changed()
        {
            self.set_global_light_angle(angle);
        }
    }

    fn parameters(&mut self, ui: &mut egui::Ui) {
        let kind = match self.page {
            StylePage::Blending => {
                self.blending_params(ui);
                return;
            }
            StylePage::Effect(kind) => kind,
        };
        design::section_header(ui, kind.label());
        if !self.is_enabled(kind) {
            caption(
                ui,
                crate::strings::tr("ui.layer_style.this.effect.is.off.tick.it"),
            );
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
                    checkbox_row(
                        ui,
                        crate::strings::tr("ui.layer_style.align.with.layer"),
                        &mut overlay.align_with_layer,
                    );
                    checkbox_row(ui, "Dither", &mut overlay.dither);
                    design::inspector_field(ui, "Gradient", |ui| {
                        open_gradient |= gradient_swatch(
                            ui,
                            ids::effect_gradient(kind),
                            &overlay.gradient,
                            sizes::swatch(),
                        )
                        .on_hover_text(crate::strings::tr("ui.layer_style.edit.this.ramp"))
                        .clicked();
                    });
                    caption(
                        ui,
                        crate::strings::tr("ui.layer_style.click.the.ramp.to.edit.its"),
                    );
                }
            }
            EffectKind::PatternOverlay => {
                if let Some(overlay) = self.effects.pattern_overlay.as_mut() {
                    design::slider_row(ui, "Opacity", &mut overlay.opacity, 0.0..=1.0);
                    design::slider_row(ui, "Scale", &mut overlay.pattern.scale, 0.1..=10.0);
                    design::slider_row(ui, "Angle", &mut overlay.pattern.angle_deg, 0.0..=360.0);
                    checkbox_row(
                        ui,
                        crate::strings::tr("ui.layer_style.link.with.layer"),
                        &mut overlay.pattern.link_with_layer,
                    );
                    if overlay.pattern.asset.is_none() {
                        caption(ui, crate::strings::tr("ui.layer_style.no.pattern"));
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

    /// The Blending Options page: blend mode, opacity and fill opacity.
    ///
    /// Each control is registered under its stable [`ids`] id so a test can
    /// see the page was drawn — the mode combo and the two numeric fields
    /// take an egui-allocated id of their own, which nothing outside this
    /// frame can look up.
    fn blending_params(&mut self, ui: &mut egui::Ui) {
        design::section_header(ui, crate::strings::tr("ui.layer_style.blending.options"));
        design::inspector_field(
            ui,
            crate::strings::tr("ui.layer_style.blending.mode"),
            |ui| {
                let mut mode = self.blending.mode;
                let before = ui.cursor().min;
                if combo(
                    ui,
                    ids::blending_mode(),
                    &mut mode,
                    &BlendMode::ALL,
                    |m| m.label().to_string(),
                    |_| None,
                ) {
                    self.set_blend_mode(mode);
                }
                tag(ui, before, ids::blending_mode());
            },
        );
        design::inspector_field(
            ui,
            crate::strings::tr("ui.layer_style.blending.opacity"),
            |ui| {
                let mut percent = f64::from(self.blending.opacity) * 100.0;
                let response = numeric(ui, &mut percent, 0.0..=100.0, 0, "%");
                if response.changed() {
                    self.set_opacity((percent / 100.0) as f32);
                }
                tag(ui, response.rect.min, ids::blending_opacity());
            },
        );
        design::inspector_field(
            ui,
            crate::strings::tr("ui.layer_style.blending.fill"),
            |ui| {
                let mut percent = f64::from(self.blending.fill_opacity) * 100.0;
                let response = numeric(ui, &mut percent, 0.0..=100.0, 0, "%");
                if response.changed() {
                    self.set_fill_opacity((percent / 100.0) as f32);
                }
                tag(ui, response.rect.min, ids::blending_fill());
            },
        );
        caption(ui, crate::strings::tr("ui.layer_style.blending.caption"));
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
            // The layer opacity scales the silhouette the way it scales the
            // layer: a 40% layer previews at 40%.
            let base = base.gamma_multiply(self.blending.opacity);
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
            crate::strings::tr("ui.layer_style.approximate.the.composited.result.is.what"),
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
    if checkbox_row(
        ui,
        crate::strings::tr("ui.layer_style.use.global.light"),
        &mut use_global,
    )
    .changed()
    {
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
    checkbox_row(
        ui,
        crate::strings::tr("ui.layer_style.layer.knocks.out.drop.shadow"),
        &mut shadow.knockout,
    );
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
    if checkbox_row(
        ui,
        crate::strings::tr("ui.layer_style.use.global.light"),
        &mut use_global,
    )
    .changed()
    {
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

/// Register `id` as a zero-sized marker at `at`, so the control drawn there
/// can be found from a test through [`egui::Context::read_response`]. A
/// zero-sized hover-only widget contains no pointer position, so it takes no
/// click and no hover from the control it marks.
fn tag(ui: &mut egui::Ui, at: egui::Pos2, id: egui::Id) {
    let _ = ui.interact(
        Rect::from_min_size(at, egui::Vec2::ZERO),
        id,
        Sense::hover(),
    );
}

/// `value` held to the `0..=1` the layer commands accept; a NaN becomes fully
/// opaque rather than a value [`LayerPatch::validate`] would refuse.
fn clamp_unit(value: f32) -> f32 {
    if value.is_nan() {
        1.0
    } else {
        value.clamp(0.0, 1.0)
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
        crate::strings::tr("ui.layer_style.layer.style")
    }

    fn confirm_label(&self) -> &'static str {
        crate::strings::tr("ui.layer_style.apply.style")
    }

    /// One command for the whole dialog: the effect block always, and each
    /// blending field only when it moved — so an untouched opacity is not
    /// re-written (and a style-only change stays a style-only patch).
    fn confirm(&self) -> Option<DialogAction> {
        let now = self.blending;
        let was = self.original_blending;
        let moved = |now: f32, was: f32| (now != was).then_some(now);
        Some(DialogAction::Command(Box::new(
            Command::SetLayerProperties {
                layer_id: self.layer,
                patch: LayerPatch {
                    effects: Some(Box::new(self.effects.clone())),
                    blend_mode: (now.mode != was.mode).then_some(now.mode),
                    opacity: moved(now.opacity, was.opacity),
                    fill_opacity: moved(now.fill_opacity, was.fill_opacity),
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

    // ---- W2-X: the Blending Options page ---------------------------------

    /// The blending controls, as drawn on the last frame.
    fn blending_drawn(h: &Harness) -> [bool; 3] {
        [
            h.was_drawn(crate::dialogs::ids::blending_mode()),
            h.was_drawn(crate::dialogs::ids::blending_opacity()),
            h.was_drawn(crate::dialogs::ids::blending_fill()),
        ]
    }

    #[test]
    fn the_dialog_opens_on_an_effect_page_and_show_blending_moves_it() {
        let mut dialog = dialog();
        assert_eq!(dialog.page(), StylePage::Effect(EffectKind::DropShadow));
        dialog.show_blending();
        assert_eq!(dialog.page(), StylePage::Blending);
        // Choosing an effect leaves the page; the effect it lands on is the
        // one asked for.
        dialog.select(EffectKind::Stroke);
        assert_eq!(dialog.page(), StylePage::Effect(EffectKind::Stroke));
        assert_eq!(dialog.selected(), EffectKind::Stroke);
    }

    #[test]
    fn the_blending_page_draws_mode_opacity_and_fill_and_the_effect_pages_do_not() {
        // The defect this pins: Blending Options… opened this dialog on the
        // Drop Shadow page, and there was no page with the layer's blend
        // mode, opacity or fill anywhere in it.
        let h = Harness::new();
        let mut on_blending = dialog();
        on_blending.show_blending();
        h.frame(Vec::new(), |ctx| {
            assert!(on_blending.show(ctx, None).is_open());
        });
        assert_eq!(
            blending_drawn(&h),
            [true, true, true],
            "[mode, opacity, fill] on the Blending Options page"
        );
        // And not on any effect page — a control that is drawn where its
        // page is not is the same defect the other way round.
        for kind in EffectKind::ALL {
            let h = Harness::new();
            let mut dialog = dialog();
            dialog.set_enabled(kind, true);
            dialog.select(kind);
            h.frame(Vec::new(), |ctx| {
                dialog.show(ctx, None);
            });
            assert_eq!(blending_drawn(&h), [false; 3], "{kind:?}");
        }
    }

    #[test]
    fn the_blending_page_draws_in_both_appearances() {
        frame_both_themes(|ctx| {
            let mut dialog = dialog().with_blending(BlendMode::Multiply, 0.4, 0.7);
            dialog.show_blending();
            assert!(dialog.show(ctx, None).is_open());
        });
    }

    #[test]
    fn with_blending_seeds_the_page_from_the_layer_and_counts_as_unchanged() {
        let dialog = dialog().with_blending(BlendMode::Screen, 0.25, 0.5);
        assert_eq!(dialog.blend_mode(), BlendMode::Screen);
        assert_eq!(dialog.opacity(), 0.25);
        assert_eq!(dialog.fill_opacity(), 0.5);
        assert!(
            !dialog.is_modified(),
            "the layer's own values are not a change"
        );
        // Nothing moved, so nothing but the effects rides the patch.
        match dialog.confirm() {
            Some(DialogAction::Command(command)) => match *command {
                Command::SetLayerProperties { patch, .. } => {
                    assert!(patch.effects.is_some());
                    assert!(patch.blend_mode.is_none());
                    assert!(patch.opacity.is_none());
                    assert!(patch.fill_opacity.is_none());
                }
                other => panic!("expected SetLayerProperties, got {other:?}"),
            },
            other => panic!("expected a command, got {other:?}"),
        }
    }

    #[test]
    fn a_moved_blending_field_rides_the_one_command_with_the_effects() {
        let mut dialog = dialog().with_blending(BlendMode::Normal, 1.0, 1.0);
        dialog.set_enabled(EffectKind::Stroke, true);
        dialog.set_opacity(0.4);
        dialog.set_blend_mode(BlendMode::Multiply);
        assert!(dialog.is_modified());
        match dialog.confirm() {
            Some(DialogAction::Command(command)) => match *command {
                Command::SetLayerProperties { layer_id, patch } => {
                    assert_eq!(layer_id, dialog.layer());
                    // One patch, one undo step: the stroke and the opacity
                    // and the mode arrive together.
                    assert!(patch.effects.as_ref().unwrap().stroke.is_some());
                    assert_eq!(patch.opacity, Some(0.4));
                    assert_eq!(patch.blend_mode, Some(BlendMode::Multiply));
                    // Fill was not touched, so it is not re-written.
                    assert!(patch.fill_opacity.is_none());
                    assert!(patch.name.is_none());
                }
                other => panic!("expected SetLayerProperties, got {other:?}"),
            },
            other => panic!("expected a command, got {other:?}"),
        }
    }

    #[test]
    fn opacity_and_fill_are_held_to_the_range_the_command_accepts() {
        let mut dialog = dialog();
        dialog.set_opacity(1.7);
        assert_eq!(dialog.opacity(), 1.0);
        dialog.set_opacity(-0.2);
        assert_eq!(dialog.opacity(), 0.0);
        dialog.set_fill_opacity(f32::NAN);
        assert_eq!(dialog.fill_opacity(), 1.0, "a NaN is not sent to validate");
        dialog.set_fill_opacity(0.3);
        assert_eq!(dialog.fill_opacity(), 0.3);
        let seeded = LayerStyleDialog::new(LayerId::new(), "Headline", LayerEffects::default())
            .with_blending(BlendMode::Normal, 2.0, -1.0);
        assert_eq!((seeded.opacity(), seeded.fill_opacity()), (1.0, 0.0));
    }

    #[test]
    fn clicking_the_blending_row_in_the_list_opens_the_page() {
        // The row is the way in from inside the dialog: the parameter panel
        // shows the page after the click, and the effect it left is the one
        // a later effect click returns to.
        let h = Harness::new();
        let mut dialog = dialog();
        dialog.select(EffectKind::Satin);
        // Settle, then find the row by the text it draws: the list row is a
        // design widget with no id of its own, so it is located by painting
        // the frame and reading the label's rectangle back.
        let label = crate::strings::tr("ui.layer_style.blending.options");
        let mut row = None;
        for _ in 0..Harness::STABLE_FRAMES + 2 {
            row = painted_text_rect(&h, label, |ctx| {
                dialog.show(ctx, None);
            });
        }
        let at = row.expect("the Blending Options row was drawn").center();
        h.frame(Harness::click_events(at), |ctx| {
            dialog.show(ctx, None);
        });
        assert_eq!(
            dialog.page(),
            StylePage::Blending,
            "the click did not open the page"
        );
        assert_eq!(
            dialog.selected(),
            EffectKind::Satin,
            "the effect it left is remembered"
        );
    }

    /// Run one quiet frame and return the rectangle of the first text shape
    /// reading exactly `text`, read off what egui painted.
    fn painted_text_rect(
        h: &Harness,
        text: &str,
        draw: impl FnOnce(&egui::Context),
    ) -> Option<Rect> {
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Harness::SCREEN)),
            ..Default::default()
        };
        let mut draw = Some(draw);
        let output = h.ctx.run(input, |ctx| {
            if let Some(draw) = draw.take() {
                draw(ctx);
            }
        });
        output
            .shapes
            .iter()
            .find_map(|clipped| match &clipped.shape {
                egui::Shape::Text(t) if t.galley.text() == text => {
                    Some(Rect::from_min_size(t.pos, t.galley.size()))
                }
                _ => None,
            })
    }
}
