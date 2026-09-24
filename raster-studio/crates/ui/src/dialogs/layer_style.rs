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
//! W9-H: the page also carries **Blend If** — per channel (gray, red,
//! green, blue) a This Layer and an Underlying Layer slider, each with a
//! black and a white end whose handles split (Alt-drag) into a fade. The
//! ranges live in the effect block (`LayerEffects::extras.blend_if`), so
//! they ride the same one command.
//!
//! # W9-H: contours, repeated effects and the Styles page
//!
//! Drop and inner shadow, both glows and bevel and emboss draw a Contour
//! row: Photoshop's preset shapes, and Custom, which edits its own curve in
//! the W5 Curves widget. Drop shadow, inner shadow, stroke, colour overlay
//! and gradient overlay can repeat: the page's `+` and `-` buttons add a
//! copy of the instance being edited or remove it, and the instance row
//! picks which one the page edits. The Styles page, first in the list, is
//! the style-preset grid: a click puts that preset's whole effect block in
//! place of the one being edited.
//!
//! # About the preview
//!
//! The preview here is deliberately a labelled *approximate* schematic. The
//! real rendering is the compositor's (`compositor::effects::render`, called
//! from its layer composite), and the canvas shows that result. All ten
//! effects render there. W7-B: the Pattern Overlay page picks one of the
//! patterns Edit > Define Pattern made (handed in with
//! [`LayerStyleDialog::with_patterns`]), shows it as a swatch, and puts the
//! pattern's own pixels into the effect, so the compositor, the saved file
//! and undo all carry them. Drawing the effects again inside a dialog would
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
use layer_model::effects::{BlendIfRange, BlendIfSource, Contour, ContourPreset, ShadowInstance};
use layer_model::{
    BevelEffect, BlendMode, ColorOverlayEffect, FillStyle, GlowEffect, GradientOverlayEffect,
    LayerEffects, LayerId, PatternOverlayEffect, PatternTile, Rgba, SatinEffect, ShadowEffect,
    StrokeEffect, StrokePosition,
};

use super::action::DialogAction;
use super::adjustment_dialog::curve_widget::{self, CurveEditor};
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
        // W9-H: switching an effect off takes its extra instances with it,
        // so an unticked row draws nothing at all.
        if !on {
            let x = &mut effects.extras;
            match self {
                Self::DropShadow => x.drop_shadows.clear(),
                Self::InnerShadow => x.inner_shadows.clear(),
                Self::Stroke => x.strokes.clear(),
                Self::ColorOverlay => x.color_overlays.clear(),
                Self::GradientOverlay => x.gradient_overlays.clear(),
                _ => {}
            }
        }
    }

    /// W9-H: whether a style may carry more than one of this effect
    /// (Photoshop's `+` button).
    pub const fn repeatable(self) -> bool {
        matches!(
            self,
            Self::DropShadow
                | Self::InnerShadow
                | Self::Stroke
                | Self::ColorOverlay
                | Self::GradientOverlay
        )
    }

    /// W9-H: whether this effect is shaped by a contour.
    pub const fn has_contour(self) -> bool {
        matches!(
            self,
            Self::DropShadow
                | Self::InnerShadow
                | Self::OuterGlow
                | Self::InnerGlow
                | Self::BevelEmboss
        )
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
    /// W9-H: the style-preset grid.
    Styles,
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
    /// W7-B: the defined patterns the Pattern Overlay page offers, in the
    /// preset store's order.
    patterns: Vec<PatternTile>,
    /// W9-H: which instance of the selected effect the page edits; `0` is
    /// the primary slot, `n` the `n`-th extra instance.
    instance: usize,
    /// W9-H: the channel the Blend If sliders show.
    blend_if_channel: BlendIfSource,
    /// W9-H: the Blend If handle a drag holds: slider (0 this layer,
    /// 1 underlying) and handle (0..4: black lo, black hi, white lo, white hi).
    blend_if_drag: Option<(usize, usize)>,
    /// W9-H: the W5 Curves widget's state, for a Custom contour.
    contour_editor: CurveEditor,
    /// W9-H: the style presets the Styles page offers, `(name, block)`.
    styles: Vec<(String, LayerEffects)>,
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
            patterns: Vec::new(),
            instance: 0,
            blend_if_channel: BlendIfSource::Gray,
            blend_if_drag: None,
            contour_editor: CurveEditor::default(),
            styles: Vec::new(),
        }
    }

    /// W9-H: offer these style presets on the Styles page.
    pub fn with_styles(mut self, styles: Vec<(String, LayerEffects)>) -> Self {
        self.styles = styles;
        self
    }

    /// W9-H: the style presets the Styles page offers.
    pub fn styles(&self) -> &[(String, LayerEffects)] {
        &self.styles
    }

    /// W9-H: show the Styles page.
    pub fn show_styles(&mut self) {
        self.page = StylePage::Styles;
    }

    /// W9-H: put the `index`-th offered style in place of the effect block
    /// being edited. `false`, changing nothing, when there is no such style.
    pub fn apply_style(&mut self, index: usize) -> bool {
        match self.styles.get(index) {
            Some((_, effects)) => {
                self.effects = effects.clone();
                self.instance = 0;
                true
            }
            None => false,
        }
    }

    /// W9-H: the instance of the selected effect the page edits.
    pub fn instance(&self) -> usize {
        self.instance
    }

    /// W9-H: how many instances of `kind` the style carries (`0` when off).
    pub fn instance_count(&self, kind: EffectKind) -> usize {
        if !self.is_enabled(kind) {
            return 0;
        }
        let x = &self.effects.extras;
        1 + match kind {
            EffectKind::DropShadow => x.drop_shadows.len(),
            EffectKind::InnerShadow => x.inner_shadows.len(),
            EffectKind::Stroke => x.strokes.len(),
            EffectKind::ColorOverlay => x.color_overlays.len(),
            EffectKind::GradientOverlay => x.gradient_overlays.len(),
            _ => 0,
        }
    }

    /// W9-H: edit instance `index` of the selected effect (clamped).
    pub fn select_instance(&mut self, index: usize) {
        let n = self.instance_count(self.selected).max(1);
        self.instance = index.min(n - 1);
    }

    /// W9-H: add a copy of the instance being edited of `kind` (Photoshop's
    /// `+`), and edit the copy. `false` when `kind` cannot repeat or is off.
    pub fn add_instance(&mut self, kind: EffectKind) -> bool {
        if !kind.repeatable() || !self.is_enabled(kind) {
            return false;
        }
        let i = if self.selected == kind {
            self.instance
        } else {
            0
        };
        let fx = &mut self.effects;
        match kind {
            EffectKind::DropShadow | EffectKind::InnerShadow => {
                let Some((effect, contour)) = shadow_instance_mut(fx, kind, i) else {
                    return false;
                };
                let copy = ShadowInstance {
                    effect: effect.clone(),
                    contour: contour.clone(),
                };
                if kind == EffectKind::DropShadow {
                    fx.extras.drop_shadows.push(copy);
                } else {
                    fx.extras.inner_shadows.push(copy);
                }
            }
            EffectKind::Stroke => {
                let Some(copy) = nth(&fx.stroke, &fx.extras.strokes, i).cloned() else {
                    return false;
                };
                fx.extras.strokes.push(copy);
            }
            EffectKind::ColorOverlay => {
                let Some(copy) = nth(&fx.color_overlay, &fx.extras.color_overlays, i).cloned()
                else {
                    return false;
                };
                fx.extras.color_overlays.push(copy);
            }
            EffectKind::GradientOverlay => {
                let Some(copy) =
                    nth(&fx.gradient_overlay, &fx.extras.gradient_overlays, i).cloned()
                else {
                    return false;
                };
                fx.extras.gradient_overlays.push(copy);
            }
            _ => return false,
        }
        self.selected = kind;
        self.page = StylePage::Effect(kind);
        self.instance = self.instance_count(kind) - 1;
        true
    }

    /// W9-H: remove the instance being edited of `kind` (Photoshop's `-`).
    /// Removing the primary promotes the next instance into its place;
    /// removing the only one switches the effect off.
    pub fn remove_instance(&mut self, kind: EffectKind) -> bool {
        if !kind.repeatable() || !self.is_enabled(kind) {
            return false;
        }
        let i = if self.selected == kind {
            self.instance
        } else {
            0
        };
        let fx = &mut self.effects;
        let x = &mut fx.extras;
        macro_rules! remove {
            ($primary:expr, $list:expr) => {
                if i > 0 {
                    if i - 1 < $list.len() {
                        $list.remove(i - 1);
                    }
                } else if $list.is_empty() {
                    $primary = None;
                } else {
                    $primary = Some($list.remove(0));
                }
            };
        }
        match kind {
            EffectKind::DropShadow | EffectKind::InnerShadow => {
                let (primary, list, contour) = if kind == EffectKind::DropShadow {
                    (
                        &mut fx.drop_shadow,
                        &mut x.drop_shadows,
                        &mut x.contours.drop_shadow,
                    )
                } else {
                    (
                        &mut fx.inner_shadow,
                        &mut x.inner_shadows,
                        &mut x.contours.inner_shadow,
                    )
                };
                if i > 0 {
                    if i - 1 < list.len() {
                        list.remove(i - 1);
                    }
                } else if list.is_empty() {
                    *primary = None;
                    *contour = Contour::default();
                } else {
                    let next = list.remove(0);
                    *primary = Some(next.effect);
                    *contour = next.contour;
                }
            }
            EffectKind::Stroke => remove!(fx.stroke, x.strokes),
            EffectKind::ColorOverlay => remove!(fx.color_overlay, x.color_overlays),
            EffectKind::GradientOverlay => remove!(fx.gradient_overlay, x.gradient_overlays),
            _ => return false,
        }
        let n = self.instance_count(kind);
        self.instance = self.instance.min(n.saturating_sub(1));
        true
    }

    /// W9-H: the contour of the instance being edited of `kind`.
    pub fn contour(&self, kind: EffectKind) -> Option<&Contour> {
        let fx = &self.effects;
        let c = &fx.extras.contours;
        let i = if self.selected == kind {
            self.instance
        } else {
            0
        };
        match kind {
            EffectKind::DropShadow | EffectKind::InnerShadow => {
                let (primary, list, contour) = if kind == EffectKind::DropShadow {
                    (&fx.drop_shadow, &fx.extras.drop_shadows, &c.drop_shadow)
                } else {
                    (&fx.inner_shadow, &fx.extras.inner_shadows, &c.inner_shadow)
                };
                primary.as_ref()?;
                if i == 0 {
                    Some(contour)
                } else {
                    list.get(i - 1).map(|s| &s.contour)
                }
            }
            EffectKind::OuterGlow => fx.outer_glow.as_ref().map(|_| &c.outer_glow),
            EffectKind::InnerGlow => fx.inner_glow.as_ref().map(|_| &c.inner_glow),
            EffectKind::BevelEmboss => fx.bevel_emboss.as_ref().map(|_| &c.bevel),
            _ => None,
        }
    }

    /// W9-H: set the contour preset of the instance being edited of `kind`.
    /// `false`, changing nothing, when the effect is off or has no contour.
    pub fn set_contour_preset(&mut self, kind: EffectKind, preset: ContourPreset) -> bool {
        let i = if self.selected == kind {
            self.instance
        } else {
            0
        };
        match contour_mut(&mut self.effects, kind, i) {
            Some(contour) => {
                set_preset(contour, preset);
                true
            }
            None => false,
        }
    }

    /// W9-H: the Blend If channel the sliders show.
    pub fn blend_if_channel(&self) -> BlendIfSource {
        self.blend_if_channel
    }

    /// W9-H: show another channel's Blend If sliders.
    pub fn set_blend_if_channel(&mut self, channel: BlendIfSource) {
        self.blend_if_channel = channel;
        self.blend_if_drag = None;
    }

    /// W7-B: offer these defined patterns on the Pattern Overlay page.
    pub fn with_patterns(mut self, patterns: Vec<PatternTile>) -> Self {
        self.patterns = patterns;
        self
    }

    /// W7-B: the patterns the Pattern Overlay page offers.
    pub fn patterns(&self) -> &[PatternTile] {
        &self.patterns
    }

    /// W7-B: put the `index`-th offered pattern into the Pattern Overlay.
    ///
    /// Returns `false`, changing nothing, when the overlay is off or there is
    /// no such pattern, the same contract [`Self::set_effect_color`] has.
    pub fn set_overlay_pattern(&mut self, index: usize) -> bool {
        match (
            self.effects.pattern_overlay.as_mut(),
            self.patterns.get(index),
        ) {
            (Some(overlay), Some(tile)) => {
                overlay.pattern.tile = Some(tile.clone());
                true
            }
            _ => false,
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
        if self.selected != kind {
            self.instance = 0;
        }
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
        let i = if self.selected == kind {
            self.instance
        } else {
            0
        };
        let fx = &self.effects;
        match kind {
            EffectKind::DropShadow => shadow_instance(fx, kind, i).map(|s| s.color),
            EffectKind::InnerShadow => shadow_instance(fx, kind, i).map(|s| s.color),
            EffectKind::OuterGlow => self.effects.outer_glow.as_ref().and_then(solid_fill),
            EffectKind::InnerGlow => self.effects.inner_glow.as_ref().and_then(solid_fill),
            EffectKind::Satin => self.effects.satin.as_ref().map(|s| s.color),
            EffectKind::ColorOverlay => {
                nth(&fx.color_overlay, &fx.extras.color_overlays, i).map(|o| o.color)
            }
            EffectKind::Stroke => match nth(&fx.stroke, &fx.extras.strokes, i).map(|s| &s.fill) {
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
        let i = if self.selected == kind {
            self.instance
        } else {
            0
        };
        match kind {
            EffectKind::DropShadow | EffectKind::InnerShadow => set_field(
                shadow_instance_mut(&mut self.effects, kind, i).map(|(s, _)| s),
                rgba,
            ),
            EffectKind::OuterGlow => set_solid_fill(self.effects.outer_glow.as_mut(), rgba),
            EffectKind::InnerGlow => set_solid_fill(self.effects.inner_glow.as_mut(), rgba),
            EffectKind::Satin => match self.effects.satin.as_mut() {
                Some(satin) => {
                    satin.color = rgba;
                    true
                }
                None => false,
            },
            EffectKind::ColorOverlay => match nth_mut(
                &mut self.effects.color_overlay,
                &mut self.effects.extras.color_overlays,
                i,
            ) {
                Some(overlay) => {
                    overlay.color = rgba;
                    true
                }
                None => false,
            },
            EffectKind::Stroke => match nth_mut(
                &mut self.effects.stroke,
                &mut self.effects.extras.strokes,
                i,
            ) {
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
        let i = if self.selected == kind {
            self.instance
        } else {
            0
        };
        match kind {
            EffectKind::GradientOverlay => nth(
                &self.effects.gradient_overlay,
                &self.effects.extras.gradient_overlays,
                i,
            )
            .map(|o| &o.gradient),
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
        let i = if self.selected == kind {
            self.instance
        } else {
            0
        };
        match kind {
            EffectKind::GradientOverlay => match nth_mut(
                &mut self.effects.gradient_overlay,
                &mut self.effects.extras.gradient_overlays,
                i,
            ) {
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
        // W9-H: the style-preset grid first, as Photoshop lists it.
        let styles_row = design::list_row(
            ui,
            crate::strings::tr("ui.layer_style.styles"),
            self.page == StylePage::Styles,
        );
        tag(ui, styles_row.rect.min, styles_row_id());
        if styles_row.clicked() {
            self.show_styles();
        }
        // The layer's own blending fields come next, as Photopea lists them:
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
            StylePage::Styles => {
                self.styles_page(ui);
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
        if kind.repeatable() {
            self.instance_row(ui, kind);
        }
        let light = self.global_light_angle;
        let i = self.instance;
        // Set by whichever swatch was clicked; drained after the borrow of
        // `self.effects` ends, which is what lets the picker be opened from
        // inside a `&mut` on the effect it edits.
        let mut open_picker = false;
        let mut open_gradient = false;
        match kind {
            EffectKind::DropShadow | EffectKind::InnerShadow => {
                if let Some((shadow, contour)) = shadow_instance_mut(&mut self.effects, kind, i) {
                    open_picker = shadow_params(ui, shadow, light, ids::effect_color(kind));
                    contour_row(ui, contour, &mut self.contour_editor);
                }
            }
            EffectKind::OuterGlow | EffectKind::InnerGlow => {
                let LayerEffects {
                    outer_glow,
                    inner_glow,
                    extras,
                    ..
                } = &mut self.effects;
                let (glow, contour) = if kind == EffectKind::OuterGlow {
                    (outer_glow.as_mut(), &mut extras.contours.outer_glow)
                } else {
                    (inner_glow.as_mut(), &mut extras.contours.inner_glow)
                };
                if let Some(glow) = glow {
                    open_picker = glow_params(ui, glow, ids::effect_color(kind));
                    contour_row(ui, contour, &mut self.contour_editor);
                }
            }
            EffectKind::BevelEmboss => {
                let LayerEffects {
                    bevel_emboss,
                    extras,
                    ..
                } = &mut self.effects;
                if let Some(bevel) = bevel_emboss.as_mut() {
                    bevel_params(ui, bevel, light);
                    contour_row(ui, &mut extras.contours.bevel, &mut self.contour_editor);
                }
            }
            EffectKind::Satin => {
                if let Some(satin) = self.effects.satin.as_mut() {
                    open_picker = satin_params(ui, satin, ids::effect_color(kind));
                }
            }
            EffectKind::ColorOverlay => {
                if let Some(overlay) = nth_mut(
                    &mut self.effects.color_overlay,
                    &mut self.effects.extras.color_overlays,
                    i,
                ) {
                    design::slider_row(ui, "Opacity", &mut overlay.opacity, 0.0..=1.0);
                    design::inspector_field(ui, "Color", |ui| {
                        open_picker |=
                            swatch(ui, ids::effect_color(kind), overlay.color, sizes::swatch())
                                .clicked();
                    });
                }
            }
            EffectKind::GradientOverlay => {
                if let Some(overlay) = nth_mut(
                    &mut self.effects.gradient_overlay,
                    &mut self.effects.extras.gradient_overlays,
                    i,
                ) {
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
                let patterns = &self.patterns;
                if let Some(overlay) = self.effects.pattern_overlay.as_mut() {
                    pattern_overlay_params(ui, overlay, patterns);
                }
            }
            EffectKind::Stroke => {
                if let Some(stroke) = nth_mut(
                    &mut self.effects.stroke,
                    &mut self.effects.extras.strokes,
                    i,
                ) {
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
        self.blend_if_params(ui);
    }

    /// W9-H: the Blend If section: a channel and the This Layer and
    /// Underlying Layer sliders for it.
    fn blend_if_params(&mut self, ui: &mut egui::Ui) {
        ui.add_space(Space::Small.pt());
        design::section_header(ui, crate::strings::tr("ui.layer_style.blend.if"));
        design::inspector_field(
            ui,
            crate::strings::tr("ui.adjustment.curve.channel"),
            |ui| {
                let mut channel = self.blend_if_channel;
                let before = ui.cursor().min;
                if combo(
                    ui,
                    blend_if_channel_id(),
                    &mut channel,
                    &BlendIfSource::ALL,
                    blend_if_label,
                    |_| None,
                ) {
                    self.set_blend_if_channel(channel);
                }
                tag(ui, before, blend_if_channel_id());
            },
        );
        let source = self.blend_if_channel;
        for (slider, label) in [
            (
                0usize,
                crate::strings::tr("ui.layer_style.blend.if.this.layer"),
            ),
            (
                1usize,
                crate::strings::tr("ui.layer_style.blend.if.underlying"),
            ),
        ] {
            design::inspector_field(ui, label, |ui| {
                let channel = self.effects.extras.blend_if.channel_mut(source);
                let range = if slider == 0 {
                    &mut channel.this_layer
                } else {
                    &mut channel.underlying
                };
                let mut drag = self
                    .blend_if_drag
                    .filter(|(s, _)| *s == slider)
                    .map(|(_, h)| h);
                blend_if_slider(ui, blend_if_slider_id(slider), range, source, &mut drag);
                match drag {
                    Some(h) => self.blend_if_drag = Some((slider, h)),
                    None if self.blend_if_drag.is_some_and(|(s, _)| s == slider) => {
                        self.blend_if_drag = None;
                    }
                    None => {}
                }
            });
        }
    }

    /// W9-H: the instance row of a repeatable effect: which instance the page
    /// edits, and the `+` / `-` buttons.
    fn instance_row(&mut self, ui: &mut egui::Ui, kind: EffectKind) {
        let n = self.instance_count(kind);
        if n == 0 {
            return;
        }
        let mut add = false;
        let mut remove = false;
        ui.horizontal(|ui| {
            let mut index = self.instance.min(n - 1);
            let options: Vec<usize> = (0..n).collect();
            let before = ui.cursor().min;
            if combo(
                ui,
                instance_combo_id(),
                &mut index,
                &options,
                |i| format!("{}", i + 1),
                |_| None,
            ) {
                self.select_instance(index);
            }
            tag(ui, before, instance_combo_id());
            add = icon_control(
                ui,
                instance_add_id(),
                "plus",
                crate::strings::tr("ui.layer_style.instance.add"),
            )
            .clicked();
            remove = icon_control(
                ui,
                instance_remove_id(),
                "minus",
                crate::strings::tr("ui.layer_style.instance.remove"),
            )
            .clicked();
        });
        if add {
            self.add_instance(kind);
        }
        if remove {
            self.remove_instance(kind);
        }
    }

    /// W9-H: the Styles page: every style preset as a clickable tile.
    fn styles_page(&mut self, ui: &mut egui::Ui) {
        design::section_header(ui, crate::strings::tr("ui.layer_style.styles"));
        if self.styles.is_empty() {
            caption(ui, crate::strings::tr("ui.layer_style.styles.empty"));
        }
        let mut chosen = None;
        let tile = sizes::swatch_square() * 2.0;
        ui.horizontal_wrapped(|ui| {
            for (index, (name, effects)) in self.styles.iter().enumerate() {
                let (rect, _) = ui.allocate_exact_size(tile, Sense::hover());
                let response = ui
                    .interact(rect, style_tile_id(index), Sense::click())
                    .on_hover_text(name.as_str());
                paint_style_tile(ui, rect, effects, response.hovered());
                if response.clicked() {
                    chosen = Some(index);
                }
            }
        });
        if let Some(index) = chosen {
            self.apply_style(index);
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
            // The layer opacity scales the silhouette the way it scales the
            // layer: a 40% layer previews at 40%.
            let base = base.gamma_multiply(self.blending.opacity);
            ui.painter()
                .rect_filled(shape, rounding(shape_radius), base);

            if on {
                // W7-B: the chosen pattern tiled over the silhouette.
                if let Some(tile) = self
                    .effects
                    .pattern_overlay
                    .as_ref()
                    .and_then(|o| o.pattern.tile.as_ref())
                {
                    paint_pattern_cells(ui, shape, tile);
                }
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

/// W7-B: the stable id of the Pattern Overlay page's pattern swatch.
pub fn pattern_swatch_id() -> egui::Id {
    egui::Id::new("layer-style-pattern-swatch")
}

/// W7-B: the Pattern Overlay page: mode, opacity, the pattern and its
/// placement.
fn pattern_overlay_params(
    ui: &mut egui::Ui,
    overlay: &mut PatternOverlayEffect,
    patterns: &[PatternTile],
) {
    design::inspector_field(ui, "Mode", |ui| {
        combo(
            ui,
            "ls-pattern-mode",
            &mut overlay.blend_mode,
            &BlendMode::ALL,
            |m| m.label().to_string(),
            |_| None,
        );
    });
    design::slider_row(ui, "Opacity", &mut overlay.opacity, 0.0..=1.0);
    // The chosen pattern is found among the offered ones by its pixels; a
    // pattern that is not offered (a file from another machine) still shows,
    // by name, as the current choice.
    let current = overlay.pattern.tile.as_ref();
    let mut pick = current
        .and_then(|t| {
            patterns
                .iter()
                .position(|p| p.content_hash() == t.content_hash())
        })
        .unwrap_or(usize::MAX);
    let current_name = current.map_or_else(|| "None".to_string(), |t| t.name().to_string());
    let options: Vec<usize> = (0..patterns.len()).collect();
    design::inspector_field(ui, "Pattern", |ui| {
        if patterns.is_empty() {
            caption(
                ui,
                crate::strings::tr("ui.fill_stroke.no.patterns.are.defined.yet"),
            );
        } else if combo(
            ui,
            "ls-pattern-choice",
            &mut pick,
            &options,
            |i| {
                patterns
                    .get(i)
                    .map_or_else(|| current_name.clone(), |p| p.name().to_string())
            },
            |_| None,
        ) {
            if let Some(tile) = patterns.get(pick) {
                overlay.pattern.tile = Some(tile.clone());
            }
        }
    });
    if let Some(tile) = overlay.pattern.tile.as_ref() {
        design::inspector_field(ui, "Preview", |ui| {
            let side = sizes::swatch().y * 3.0;
            let (_, rect) = ui.allocate_space(vec2(side, side));
            let _ = ui.interact(rect, pattern_swatch_id(), Sense::hover());
            if ui.is_rect_visible(rect) {
                paint_pattern_cells(ui, rect, tile);
            }
        });
    }
    design::slider_row(ui, "Scale", &mut overlay.pattern.scale, 0.1..=10.0);
    design::slider_row(ui, "Angle", &mut overlay.pattern.angle_deg, 0.0..=360.0);
    design::inspector_field(ui, "Offset", |ui| {
        for axis in 0..2 {
            let mut v = f64::from(overlay.pattern.offset_px[axis]);
            if numeric(ui, &mut v, -10_000.0..=10_000.0, 0, "px").changed() {
                overlay.pattern.offset_px[axis] = v as f32;
            }
        }
    });
    checkbox_row(
        ui,
        crate::strings::tr("ui.layer_style.link.with.layer"),
        &mut overlay.pattern.link_with_layer,
    );
    if overlay.pattern.tile.is_none() {
        caption(ui, crate::strings::tr("ui.layer_style.no.pattern"));
    }
}

/// W7-B: a pattern drawn into `rect` as a grid of flat cells: about two
/// repeats of the tile across, each cell the tile's pixel at that spot. A
/// preview, not the composite: the canvas is where the real result shows.
fn paint_pattern_cells(ui: &egui::Ui, rect: Rect, tile: &PatternTile) {
    const CELLS: u32 = 12;
    let step_x = tile.width().div_ceil(CELLS / 2).max(1);
    let step_y = tile.height().div_ceil(CELLS / 2).max(1);
    let (cw, ch) = (rect.width() / CELLS as f32, rect.height() / CELLS as f32);
    let mut mesh = egui::Mesh::default();
    for cy in 0..CELLS {
        for cx in 0..CELLS {
            let px = tile.pixel(i64::from(cx * step_x), i64::from(cy * step_y));
            let color = super::controls::color_of([
                f32::from(px[0]) / 255.0,
                f32::from(px[1]) / 255.0,
                f32::from(px[2]) / 255.0,
                f32::from(px[3]) / 255.0,
            ]);
            let min = rect.min + vec2(cx as f32 * cw, cy as f32 * ch);
            mesh.add_colored_rect(Rect::from_min_size(min, vec2(cw, ch)), color);
        }
    }
    ui.painter().add(egui::Shape::mesh(mesh));
}

/// W9-H: the stable id of the Styles row in the effect list.
pub fn styles_row_id() -> egui::Id {
    egui::Id::new("layer-style-styles-row")
}

/// W9-H: the stable id of the `index`-th tile on the Styles page.
pub fn style_tile_id(index: usize) -> egui::Id {
    egui::Id::new(("layer-style-style-tile", index))
}

/// W9-H: the stable id of the Contour combo.
pub fn contour_combo_id() -> egui::Id {
    egui::Id::new("layer-style-contour")
}

/// W9-H: the stable id of the instance picker of a repeatable effect.
pub fn instance_combo_id() -> egui::Id {
    egui::Id::new("layer-style-instance")
}

/// W9-H: the stable id of the `+` button of a repeatable effect.
pub fn instance_add_id() -> egui::Id {
    egui::Id::new("layer-style-instance-add")
}

/// W9-H: the stable id of the `-` button of a repeatable effect.
pub fn instance_remove_id() -> egui::Id {
    egui::Id::new("layer-style-instance-remove")
}

/// W9-H: the stable id of the Blend If channel combo.
pub fn blend_if_channel_id() -> egui::Id {
    egui::Id::new("layer-style-blend-if-channel")
}

/// W9-H: the stable id of a Blend If slider: `0` This Layer, `1`
/// Underlying Layer.
pub fn blend_if_slider_id(slider: usize) -> egui::Id {
    egui::Id::new(("layer-style-blend-if", slider))
}

/// W9-H: the `index`-th instance of a repeatable effect: the primary slot,
/// then the extras. `None` when the effect is off or there is no such one.
fn nth<'a, T>(primary: &'a Option<T>, extra: &'a [T], index: usize) -> Option<&'a T> {
    let first = primary.as_ref()?;
    if index == 0 {
        Some(first)
    } else {
        extra.get(index - 1)
    }
}

/// W9-H: [`nth`], mutably.
fn nth_mut<'a, T>(
    primary: &'a mut Option<T>,
    extra: &'a mut [T],
    index: usize,
) -> Option<&'a mut T> {
    if index == 0 {
        primary.as_mut()
    } else if primary.is_some() {
        extra.get_mut(index - 1)
    } else {
        None
    }
}

/// W9-H: the `index`-th drop or inner shadow.
fn shadow_instance(fx: &LayerEffects, kind: EffectKind, index: usize) -> Option<&ShadowEffect> {
    let (primary, list) = match kind {
        EffectKind::DropShadow => (&fx.drop_shadow, &fx.extras.drop_shadows),
        EffectKind::InnerShadow => (&fx.inner_shadow, &fx.extras.inner_shadows),
        _ => return None,
    };
    let first = primary.as_ref()?;
    if index == 0 {
        Some(first)
    } else {
        list.get(index - 1).map(|s| &s.effect)
    }
}

/// W9-H: the `index`-th drop or inner shadow and the contour it draws
/// through, mutably.
fn shadow_instance_mut(
    fx: &mut LayerEffects,
    kind: EffectKind,
    index: usize,
) -> Option<(&mut ShadowEffect, &mut Contour)> {
    let LayerEffects {
        drop_shadow,
        inner_shadow,
        extras,
        ..
    } = fx;
    let layer_model::effects::StyleExtras {
        drop_shadows,
        inner_shadows,
        contours,
        ..
    } = extras;
    let (primary, list, contour) = match kind {
        EffectKind::DropShadow => (drop_shadow, drop_shadows, &mut contours.drop_shadow),
        EffectKind::InnerShadow => (inner_shadow, inner_shadows, &mut contours.inner_shadow),
        _ => return None,
    };
    if index == 0 {
        primary.as_mut().map(|s| (s, contour))
    } else if primary.is_some() {
        list.get_mut(index - 1)
            .map(|s| (&mut s.effect, &mut s.contour))
    } else {
        None
    }
}

/// W9-H: the contour of the `index`-th instance of `kind`, mutably.
fn contour_mut(fx: &mut LayerEffects, kind: EffectKind, index: usize) -> Option<&mut Contour> {
    match kind {
        EffectKind::DropShadow | EffectKind::InnerShadow => {
            shadow_instance_mut(fx, kind, index).map(|(_, c)| c)
        }
        EffectKind::OuterGlow => fx
            .outer_glow
            .as_ref()
            .map(|_| &mut fx.extras.contours.outer_glow),
        EffectKind::InnerGlow => fx
            .inner_glow
            .as_ref()
            .map(|_| &mut fx.extras.contours.inner_glow),
        EffectKind::BevelEmboss => fx
            .bevel_emboss
            .as_ref()
            .map(|_| &mut fx.extras.contours.bevel),
        _ => None,
    }
}

/// W9-H: switch a contour to `preset`; a Custom contour with no curve yet
/// starts as the identity, so it can be edited from there.
fn set_preset(contour: &mut Contour, preset: ContourPreset) {
    contour.preset = preset;
    if preset == ContourPreset::Custom && contour.points.len() < 2 {
        contour.points = curve_widget::normalized(&[]);
    }
}

/// W9-H: the name a contour preset is listed under.
fn contour_label(preset: ContourPreset) -> String {
    match preset {
        ContourPreset::Linear => crate::strings::tr("ui.layer_style.contour.linear"),
        ContourPreset::Cone => crate::strings::tr("ui.layer_style.contour.cone"),
        ContourPreset::Gaussian => crate::strings::tr("ui.layer_style.contour.gaussian"),
        ContourPreset::Ring => crate::strings::tr("ui.layer_style.contour.ring"),
        ContourPreset::RoundedSteps => crate::strings::tr("ui.layer_style.contour.rounded.steps"),
        ContourPreset::Custom => crate::strings::tr("ui.layer_style.contour.custom"),
    }
    .to_string()
}

/// W9-H: the Contour row, and for a Custom contour the W5 Curves widget
/// editing its curve.
fn contour_row(ui: &mut egui::Ui, contour: &mut Contour, editor: &mut CurveEditor) {
    design::inspector_field(ui, crate::strings::tr("ui.layer_style.contour"), |ui| {
        let mut preset = contour.preset;
        let before = ui.cursor().min;
        if combo(
            ui,
            contour_combo_id(),
            &mut preset,
            &ContourPreset::ALL,
            contour_label,
            |_| None,
        ) {
            set_preset(contour, preset);
        }
        tag(ui, before, contour_combo_id());
    });
    if contour.preset == ContourPreset::Custom {
        if contour.points.len() < 2 {
            contour.points = curve_widget::normalized(&[]);
        }
        // The widget edits four curves; a contour has one, so the other three
        // are scratch and the channel is held on the composite.
        let (mut r, mut g, mut b) = (Vec::new(), Vec::new(), Vec::new());
        editor.set_channel(0);
        curve_widget::show(
            ui,
            editor,
            [&mut contour.points, &mut r, &mut g, &mut b],
            None,
        );
        if editor.channel() != 0 {
            editor.set_channel(0);
        }
    }
}

/// W9-H: the name a Blend If channel is listed under.
fn blend_if_label(source: BlendIfSource) -> String {
    match source {
        BlendIfSource::Gray => crate::strings::tr("ui.layer_style.blend.if.gray"),
        BlendIfSource::Red => crate::strings::tr("ui.adjustment.red"),
        BlendIfSource::Green => crate::strings::tr("ui.adjustment.green"),
        BlendIfSource::Blue => crate::strings::tr("ui.adjustment.blue"),
    }
    .to_string()
}

/// W9-H: a Blend If slider — the channel's ramp with the black end's two
/// handles and the white end's two beneath it. A drag moves the handle
/// nearest the press; a joined pair moves together unless Alt is held,
/// which splits it (the black pair's upper handle, the white pair's lower
/// one), as in Photoshop. The four handles never cross.
///
/// Returns `true` when a handle moved.
fn blend_if_slider(
    ui: &mut egui::Ui,
    id: egui::Id,
    range: &mut BlendIfRange,
    source: BlendIfSource,
    drag: &mut Option<usize>,
) -> bool {
    let t = current_tokens(ui);
    let bar_height = sizes::gradient_bar_height();
    let width = ui.available_width().max(sizes::combo_min_width());
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, bar_height * 2.0), Sense::hover());
    let response = ui.interact(rect, id, Sense::click_and_drag());
    let bar = Rect::from_min_size(rect.min, egui::vec2(rect.width(), bar_height));
    let x_of = |v: f32| bar.left() + bar.width() * v.clamp(0.0, 1.0);

    // The ramp, black to the channel's full value.
    let (r_on, g_on, b_on) = match source {
        BlendIfSource::Gray => (true, true, true),
        BlendIfSource::Red => (true, false, false),
        BlendIfSource::Green => (false, true, false),
        BlendIfSource::Blue => (false, false, true),
    };
    let mut mesh = egui::Mesh::default();
    let steps = 16u32;
    for s in 0..=steps {
        let v = s as f32 / steps as f32;
        let level = (v * 255.0).round() as u8;
        let pick = |on: bool| if on { level } else { 0 };
        let color = egui::Color32::from_rgba_unmultiplied(pick(r_on), pick(g_on), pick(b_on), 255);
        let x = x_of(v);
        mesh.colored_vertex(egui::pos2(x, bar.top()), color);
        mesh.colored_vertex(egui::pos2(x, bar.bottom()), color);
        if s > 0 {
            let k = 2 * s;
            mesh.add_triangle(k - 2, k - 1, k);
            mesh.add_triangle(k - 1, k, k + 1);
        }
    }
    ui.painter().add(egui::Shape::mesh(mesh));

    // The handles.
    let values = [
        range.black[0],
        range.black[1],
        range.white[0],
        range.white[1],
    ];
    let ink = color32(t.palette.color(ColorRole::TextPrimary));
    let half = bar_height * 0.5;
    for (index, v) in values.iter().enumerate() {
        let x = x_of(*v);
        let tip = egui::pos2(x, bar.bottom());
        let held = *drag == Some(index);
        let size = if held { half * 1.25 } else { half };
        ui.painter().add(egui::Shape::convex_polygon(
            vec![
                tip,
                egui::pos2(x + size * 0.6, tip.y + size),
                egui::pos2(x - size * 0.6, tip.y + size),
            ],
            ink,
            egui::Stroke::NONE,
        ));
    }

    let Some(pos) = response.interact_pointer_pos() else {
        if !response.dragged() {
            *drag = None;
        }
        return false;
    };
    let v = ((pos.x - bar.left()) / bar.width().max(f32::EPSILON)).clamp(0.0, 1.0);
    if response.drag_started() || (drag.is_none() && response.is_pointer_button_down_on()) {
        let nearest = values
            .iter()
            .enumerate()
            .min_by(|a, b| (a.1 - v).abs().total_cmp(&(b.1 - v).abs()))
            .map(|(i, _)| i);
        *drag = nearest;
    }
    let Some(handle) = *drag else {
        return false;
    };
    let split = ui.input(|i| i.modifiers.alt);
    let before = *range;
    move_blend_if_handle(range, handle, v, split);
    if response.drag_stopped() || !response.is_pointer_button_down_on() {
        *drag = None;
    }
    *range != before
}

/// W9-H: move handle `handle` of `range` to `v`, keeping the four handles
/// ordered. A joined pair moves as one unless `split`, which moves only the
/// handle that opens the fade.
pub fn move_blend_if_handle(range: &mut BlendIfRange, handle: usize, v: f32, split: bool) {
    let mut h = [
        range.black[0],
        range.black[1],
        range.white[0],
        range.white[1],
    ];
    let pair = if handle < 2 { 0 } else { 2 };
    let joined = (h[pair] - h[pair + 1]).abs() <= f32::EPSILON;
    if joined && !split {
        let lo = if pair == 0 { 0.0 } else { h[1] };
        let hi = if pair == 0 { h[2] } else { 1.0 };
        let v = v.clamp(lo, hi);
        h[pair] = v;
        h[pair + 1] = v;
    } else {
        // Splitting a joined pair opens the fade inward: the black pair's
        // upper handle, the white pair's lower one.
        let handle = if joined {
            if pair == 0 {
                1
            } else {
                2
            }
        } else {
            handle
        };
        let lo = if handle == 0 { 0.0 } else { h[handle - 1] };
        let hi = if handle == 3 { 1.0 } else { h[handle + 1] };
        h[handle] = v.clamp(lo, hi);
    }
    range.black = [h[0], h[1]];
    range.white = [h[2], h[3]];
}

/// W9-H: a square icon control with a stable id: the `+` and `-` of a
/// repeatable effect.
fn icon_control(ui: &mut egui::Ui, id: egui::Id, icon: &str, tooltip: &str) -> egui::Response {
    let t = current_tokens(ui);
    let (rect, _) = ui.allocate_exact_size(sizes::swatch_square(), Sense::hover());
    let response = ui.interact(rect, id, Sense::click()).on_hover_text(tooltip);
    let role = if response.hovered() {
        ColorRole::TextPrimary
    } else {
        ColorRole::TextSecondary
    };
    crate::icons::ui_icon(icon).paint(
        ui.painter(),
        rect.shrink(Space::Small.pt()),
        color32(t.palette.color(role)),
        t.borders.hairline * 1.5,
    );
    response
}

/// W9-H: a Styles-page tile: a schematic of the style — its drop shadow,
/// its fill (the colour overlay's colour when it has one) and its stroke.
fn paint_style_tile(ui: &egui::Ui, rect: Rect, effects: &LayerEffects, hovered: bool) {
    let t = current_tokens(ui);
    let radius = Radius::Small.resolve(&t.radii, rect.height());
    let ground = if hovered {
        ColorRole::SurfaceElevated
    } else {
        ColorRole::SurfaceSunken
    };
    ui.painter()
        .rect_filled(rect, rounding(radius), color32(t.palette.color(ground)));
    let shape = Rect::from_center_size(rect.center(), rect.size() * 0.5);
    if let Some(shadow) = &effects.drop_shadow {
        let (dx, dy) = shadow_offset(
            shadow.angle_deg,
            shadow.distance_px.min(shape.width() * 0.25),
        );
        ui.painter().rect_filled(
            shape.translate(egui::vec2(dx, dy)),
            rounding(radius),
            with_alpha(shadow.color, shadow.opacity * 0.6),
        );
    }
    let fill = effects
        .color_overlay
        .as_ref()
        .map(|o| with_alpha(o.color, o.opacity))
        .unwrap_or_else(|| color32(t.palette.color(ColorRole::TextSecondary)));
    ui.painter().rect_filled(shape, rounding(radius), fill);
    if let Some(stroke) = &effects.stroke {
        if let FillStyle::Solid(color) = stroke.fill {
            ui.painter().rect_stroke(
                shape,
                rounding(radius),
                egui::Stroke {
                    width: t.borders.hairline * 2.0,
                    color: with_alpha(color, stroke.opacity),
                },
            );
        }
    }
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

    // ---- W7-B: the Pattern Overlay page picks a defined pattern ----------

    fn tile(name: &str, rgba: [u8; 4], other: [u8; 4]) -> PatternTile {
        PatternTile::new(name, 2, 1, [rgba, other].concat()).unwrap()
    }

    fn with_two_patterns() -> LayerStyleDialog {
        let mut dialog = dialog().with_patterns(vec![
            tile("Checker", [200, 30, 40, 255], [20, 180, 60, 255]),
            tile("Stripes", [10, 20, 230, 255], [240, 220, 10, 255]),
        ]);
        dialog.set_enabled(EffectKind::PatternOverlay, true);
        dialog.select(EffectKind::PatternOverlay);
        dialog
    }

    /// Every mesh vertex egui painted in one quiet frame: where, and in what
    /// colour.
    fn painted_vertices(
        h: &Harness,
        draw: impl FnOnce(&egui::Context),
    ) -> Vec<(egui::Pos2, egui::Color32)> {
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
        let mut out = Vec::new();
        for clipped in &output.shapes {
            if let egui::Shape::Mesh(mesh) = &clipped.shape {
                out.extend(mesh.vertices.iter().map(|v| (v.pos, v.color)));
            }
        }
        out
    }

    #[test]
    fn choosing_a_pattern_puts_its_pixels_into_the_one_command() {
        let mut dialog = with_two_patterns();
        assert!(dialog.set_overlay_pattern(1));
        assert!(!dialog.set_overlay_pattern(7), "no such pattern");
        match dialog.confirm() {
            Some(DialogAction::Command(command)) => match *command {
                Command::SetLayerProperties { patch, .. } => {
                    let effects = patch.effects.unwrap();
                    let chosen = effects.pattern_overlay.unwrap().pattern.tile.unwrap();
                    assert_eq!(chosen.name(), "Stripes");
                    assert_eq!(chosen.rgba8(), dialog.patterns()[1].rgba8());
                }
                other => panic!("expected SetLayerProperties, got {other:?}"),
            },
            other => panic!("expected a command, got {other:?}"),
        }
        // An overlay that is off takes no pattern.
        let mut off = dialog.clone();
        off.set_enabled(EffectKind::PatternOverlay, false);
        assert!(!off.set_overlay_pattern(0));
    }

    #[test]
    fn picking_a_pattern_from_the_drawn_combo_lands_in_the_overlay() {
        // The real route: the page draws a combo reading "None", a click
        // opens it, and a click on a name chooses that pattern.
        let h = Harness::new();
        let mut dialog = with_two_patterns();
        let mut closed = None;
        for _ in 0..Harness::STABLE_FRAMES + 2 {
            closed = painted_text_rect(&h, "None", |ctx| {
                dialog.show(ctx, None);
            });
        }
        let at = closed.expect("the pattern combo was drawn").center();
        h.frame(Harness::click_events(at), |ctx| {
            dialog.show(ctx, None);
        });
        let mut item = None;
        for _ in 0..3 {
            item = painted_text_rect(&h, "Stripes", |ctx| {
                dialog.show(ctx, None);
            });
        }
        let at = item
            .expect("the open combo lists the defined patterns")
            .center();
        h.frame(Harness::click_events(at), |ctx| {
            dialog.show(ctx, None);
        });
        let chosen = dialog
            .effects()
            .pattern_overlay
            .as_ref()
            .and_then(|o| o.pattern.tile.as_ref())
            .expect("the click chose a pattern");
        assert_eq!(chosen.name(), "Stripes");
    }

    #[test]
    fn the_chosen_pattern_is_previewed_in_its_own_colours() {
        let h = Harness::new();
        let mut dialog = with_two_patterns();
        // Before a pattern is chosen there is no swatch to draw.
        h.frame(Vec::new(), |ctx| {
            dialog.show(ctx, None);
        });
        assert!(!h.was_drawn(pattern_swatch_id()));
        assert!(dialog.set_overlay_pattern(0));
        // Enough frames for the modal's fade-in to finish: until it does,
        // every colour it paints is scaled by the fade.
        let mut vertices = Vec::new();
        for _ in 0..40 {
            vertices = painted_vertices(&h, |ctx| {
                dialog.show(ctx, None);
            });
        }
        let swatch = h
            .ctx
            .read_response(pattern_swatch_id())
            .expect("the swatch is drawn")
            .rect;
        // Only what was painted inside the swatch's own rectangle counts: the
        // schematic preview beside it paints the pattern too.
        let colors: Vec<egui::Color32> = vertices
            .iter()
            .filter(|(pos, _)| swatch.contains(*pos))
            .map(|(_, c)| *c)
            .collect();
        for want in [
            super::super::controls::color_of([200.0 / 255.0, 30.0 / 255.0, 40.0 / 255.0, 1.0]),
            super::super::controls::color_of([20.0 / 255.0, 180.0 / 255.0, 60.0 / 255.0, 1.0]),
        ] {
            assert!(
                colors.contains(&want),
                "the preview paints the pattern's {want:?}"
            );
        }
    }

    #[test]
    fn with_no_defined_patterns_the_page_says_so_and_still_draws() {
        let mut dialog = dialog();
        dialog.set_enabled(EffectKind::PatternOverlay, true);
        dialog.select(EffectKind::PatternOverlay);
        frame_both_themes(|ctx| {
            assert!(dialog.show(ctx, None).is_open());
        });
        assert!(!dialog.set_overlay_pattern(0));
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

    // ---- W9-H: contours, repeated effects, Blend If, the Styles page -----

    /// The effect block the confirmed command carries.
    fn confirmed_effects(dialog: &LayerStyleDialog) -> LayerEffects {
        match dialog.confirm() {
            Some(DialogAction::Command(command)) => match *command {
                Command::SetLayerProperties { patch, .. } => *patch.effects.expect("effects"),
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        }
    }

    /// A press at `from`, a drag through to `to` and a release, over four
    /// frames so egui sees a drag rather than a click.
    fn drag(h: &Harness, from: egui::Pos2, to: egui::Pos2, mut draw: impl FnMut(&egui::Context)) {
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        h.frame(
            vec![egui::Event::PointerMoved(from), button(from, true)],
            &mut draw,
        );
        h.frame(
            vec![egui::Event::PointerMoved(from + (to - from) * 0.5)],
            &mut draw,
        );
        h.frame(vec![egui::Event::PointerMoved(to)], &mut draw);
        h.frame(vec![button(to, false)], &mut draw);
    }

    #[test]
    fn the_contour_row_is_drawn_exactly_on_the_effects_that_have_one() {
        for kind in EffectKind::ALL {
            let h = Harness::new();
            let mut dialog = dialog();
            dialog.set_enabled(kind, true);
            dialog.select(kind);
            h.frame(Vec::new(), |ctx| {
                dialog.show(ctx, None);
            });
            assert_eq!(
                h.was_drawn(contour_combo_id()),
                kind.has_contour(),
                "{kind:?}"
            );
            assert_eq!(dialog.contour(kind).is_some(), kind.has_contour());
        }
    }

    #[test]
    fn a_chosen_contour_rides_the_command_and_custom_opens_the_curve_widget() {
        let mut dialog = dialog();
        dialog.set_enabled(EffectKind::OuterGlow, true);
        dialog.select(EffectKind::OuterGlow);
        assert!(dialog.set_contour_preset(EffectKind::OuterGlow, ContourPreset::Cone));
        assert_eq!(
            confirmed_effects(&dialog).extras.contours.outer_glow.preset,
            ContourPreset::Cone
        );
        // An effect that is off takes no contour.
        assert!(!dialog.set_contour_preset(EffectKind::InnerGlow, ContourPreset::Ring));

        // Custom draws the W5 Curves graph, over a curve seeded as the
        // identity, and a click on the graph adds a knot to the contour.
        assert!(dialog.set_contour_preset(EffectKind::OuterGlow, ContourPreset::Custom));
        assert_eq!(
            dialog.contour(EffectKind::OuterGlow).unwrap().points.len(),
            2
        );
        let h = Harness::new();
        let graph = curve_widget::curve_graph_id();
        let at = h.settle(graph, |ctx| {
            dialog.show(ctx, None);
        });
        let target = egui::pos2(
            at.left() + at.width() * 0.3,
            at.bottom() - at.height() * 0.8,
        );
        h.frame(Harness::click_events(target), |ctx| {
            dialog.show(ctx, None);
        });
        let contour = confirmed_effects(&dialog).extras.contours.outer_glow;
        assert_eq!(contour.preset, ContourPreset::Custom);
        assert_eq!(contour.points.len(), 3, "{:?}", contour.points);
        assert!(
            (contour.eval(0.3) - 0.8).abs() < 0.05,
            "the knot bends the contour: {:?}",
            contour.points
        );
    }

    #[test]
    fn plus_adds_an_instance_and_minus_removes_it_through_the_drawn_buttons() {
        let h = Harness::new();
        let mut dialog = dialog();
        dialog.set_enabled(EffectKind::DropShadow, true);
        dialog.select(EffectKind::DropShadow);
        h.click_widget(instance_add_id(), |ctx| {
            dialog.show(ctx, None);
        });
        assert_eq!(dialog.instance_count(EffectKind::DropShadow), 2);
        assert_eq!(dialog.instance(), 1, "the new copy is the one edited");
        // The page now edits the copy: a colour lands on it, not the first.
        assert!(dialog.set_effect_color(EffectKind::DropShadow, [0.0, 0.0, 1.0, 1.0]));
        let fx = confirmed_effects(&dialog);
        assert_eq!(fx.drop_shadows().len(), 2, "both ride the command");
        assert_eq!(fx.extras.drop_shadows[0].effect.color, [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(fx.drop_shadow.as_ref().unwrap().color, [0.0, 0.0, 0.0, 1.0]);

        h.click_widget(instance_remove_id(), |ctx| {
            dialog.show(ctx, None);
        });
        assert_eq!(dialog.instance_count(EffectKind::DropShadow), 1);
        assert_eq!(dialog.instance(), 0);
        // And removing the only one switches the effect off.
        h.click_widget(instance_remove_id(), |ctx| {
            dialog.show(ctx, None);
        });
        assert!(!dialog.is_enabled(EffectKind::DropShadow));
        // An effect that cannot repeat draws no buttons.
        let h = Harness::new();
        let mut satin = super::tests::dialog();
        satin.set_enabled(EffectKind::Satin, true);
        satin.select(EffectKind::Satin);
        h.frame(Vec::new(), |ctx| {
            satin.show(ctx, None);
        });
        assert!(!h.was_drawn(instance_add_id()));
        assert!(!satin.add_instance(EffectKind::Satin));
    }

    #[test]
    fn removing_the_first_instance_promotes_the_next_and_unticking_drops_all() {
        let mut dialog = dialog();
        dialog.set_enabled(EffectKind::Stroke, true);
        dialog.select(EffectKind::Stroke);
        assert!(dialog.add_instance(EffectKind::Stroke));
        assert!(dialog.set_effect_color(EffectKind::Stroke, [1.0, 0.0, 0.0, 1.0]));
        dialog.select_instance(0);
        assert!(dialog.remove_instance(EffectKind::Stroke));
        let fx = dialog.effects();
        assert_eq!(
            fx.stroke.as_ref().map(|s| s.fill.clone()),
            Some(FillStyle::Solid([1.0, 0.0, 0.0, 1.0])),
            "the red copy took the first slot"
        );
        assert!(fx.extras.strokes.is_empty());
        assert!(dialog.add_instance(EffectKind::Stroke));
        dialog.set_enabled(EffectKind::Stroke, false);
        assert!(dialog.effects().strokes().is_empty());
    }

    #[test]
    fn dragging_the_underlying_black_handle_sets_blend_if_and_rides_the_command() {
        let h = Harness::new();
        let mut dialog = dialog();
        dialog.show_blending();
        let slider = blend_if_slider_id(1);
        let rect = h.settle(slider, |ctx| {
            dialog.show(ctx, None);
        });
        let y = rect.top() + sizes::gradient_bar_height() * 0.5;
        drag(
            &h,
            egui::pos2(rect.left() + 1.0, y),
            egui::pos2(rect.center().x, y),
            |ctx| {
                dialog.show(ctx, None);
            },
        );
        let fx = confirmed_effects(&dialog);
        let under = fx.extras.blend_if.gray.underlying;
        assert!(
            (under.black[0] - 0.5).abs() < 0.05 && under.black[0] == under.black[1],
            "the joined black pair moved to the middle: {under:?}"
        );
        assert!(
            fx.extras.blend_if.gray.this_layer.is_full(),
            "only that slider"
        );
        // The channel combo is drawn, and another channel keeps its own ranges.
        assert!(h.was_drawn(blend_if_channel_id()));
        dialog.set_blend_if_channel(BlendIfSource::Red);
        assert!(dialog.effects().extras.blend_if.red.underlying.is_full());
    }

    #[test]
    fn an_alt_drag_splits_a_joined_pair_and_handles_never_cross() {
        let mut r = BlendIfRange::default();
        move_blend_if_handle(&mut r, 0, 0.4, false);
        assert_eq!(r.black, [0.4, 0.4], "a joined pair moves together");
        move_blend_if_handle(&mut r, 0, 0.6, true);
        assert_eq!(r.black, [0.4, 0.6], "Alt opens the fade");
        move_blend_if_handle(&mut r, 3, 0.1, false);
        assert_eq!(r.white, [0.6, 0.6], "the white pair cannot pass the black");
        let mut split = BlendIfRange::default();
        move_blend_if_handle(&mut split, 3, 0.7, true);
        assert_eq!(split.white, [0.7, 1.0], "Alt opens the white fade inward");
        move_blend_if_handle(&mut r, 1, 0.9, false);
        assert_eq!(r.black[1], 0.6, "nor the black pass the white");
    }

    #[test]
    fn the_styles_page_applies_the_clicked_preset() {
        let blue = LayerEffects {
            color_overlay: Some(ColorOverlayEffect {
                color: [0.0, 0.0, 1.0, 1.0],
                ..Default::default()
            }),
            ..Default::default()
        };
        let shadowed = LayerEffects {
            drop_shadow: Some(ShadowEffect::default()),
            stroke: Some(StrokeEffect::default()),
            ..Default::default()
        };
        let mut dialog = dialog().with_styles(vec![
            ("Blue".to_string(), blue),
            ("Shadowed".to_string(), shadowed.clone()),
        ]);
        let h = Harness::new();
        // In through the list row, as a user gets there.
        h.click_widget(styles_row_id(), |ctx| {
            dialog.show(ctx, None);
        });
        assert_eq!(dialog.page(), StylePage::Styles);
        h.click_widget(style_tile_id(1), |ctx| {
            dialog.show(ctx, None);
        });
        assert_eq!(confirmed_effects(&dialog), shadowed);
        assert!(!dialog.apply_style(7), "no such preset");
        // The page draws in both appearances with nothing to offer too.
        frame_both_themes(|ctx| {
            let mut empty = super::tests::dialog();
            empty.show_styles();
            assert!(empty.show(ctx, None).is_open());
        });
    }
}
