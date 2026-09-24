//! The brush editor.
//!
//! The stroke preview is not a drawing of a brush — it runs the **real**
//! [`tools::DabEmitter`] along a synthetic gesture and rasterises the dabs it
//! produces with the same coverage function the brush tool paints with. A
//! preview drawn any other way is a second implementation that quietly stops
//! agreeing with the first; this one cannot, because there is only one.

use design::{
    color32, current_tokens, egui_theme::rounding, tokens::palette::ColorRole, tokens::Radius,
    tokens::Space,
};
use egui::{Context, TextureHandle};
use glam::Vec2;
use tools::{BrushSettings, Dab, DabEmitter};

use super::action::DialogAction;
use super::chrome::{
    action_row, caption, hairline, modal, warning, Dialog, DialogButton, DialogKeys, DialogOutcome,
    DialogWidth,
};
use super::controls::checkbox_row;
use super::sizes;

/// Resolution of the stroke preview **texture**, in pixels.
///
/// This is a raster size, not a layout size: it is how many samples the dab
/// engine renders into. The rectangle that texture is drawn in is a point-space
/// extent and belongs to [`sizes::brush_stroke_preview`] like every other
/// dialog dimension — `the_preview_texture_matches_the_rectangle_it_fills`
/// keeps the two in step so the preview is never resampled.
pub const PREVIEW_SIZE: (u32, u32) = (280, 96);

/// The brush editor dialog.
pub struct BrushEditorDialog {
    settings: BrushSettings,
    name: String,
    texture: Option<TextureHandle>,
    /// The settings the cached texture was rendered from.
    cached_for: Option<BrushSettings>,
}

impl Default for BrushEditorDialog {
    fn default() -> Self {
        Self::new(BrushSettings::default())
    }
}

impl std::fmt::Debug for BrushEditorDialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrushEditorDialog")
            .field("settings", &self.settings)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl BrushEditorDialog {
    /// Open on `settings`.
    pub fn new(settings: BrushSettings) -> Self {
        Self {
            settings,
            name: crate::strings::tr("ui.brush_editor.custom.brush").to_string(),
            texture: None,
            cached_for: None,
        }
    }

    /// The settings as edited.
    pub fn settings(&self) -> &BrushSettings {
        &self.settings
    }

    /// Mutable access, invalidating the preview.
    pub fn settings_mut(&mut self) -> &mut BrushSettings {
        self.cached_for = None;
        &mut self.settings
    }

    /// The preset's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Rename the preset.
    pub fn set_name(&mut self, name: impl Into<String>) {
        self.name = name.into();
    }

    /// The settings, clamped by the brush engine's own validator, or the error
    /// that makes them unusable.
    pub fn validated(&self) -> Result<BrushSettings, tools::ToolError> {
        self.settings.validated()
    }

    /// Coverage of the live stroke preview, row-major, `width * height` values
    /// in `0..=1`.
    ///
    /// The gesture is a fixed S-curve with a pressure ramp from zero to full,
    /// so the pressure mappings are visible in the preview the moment they are
    /// switched on.
    pub fn preview_coverage(&self, width: u32, height: u32) -> Vec<f32> {
        let mut coverage = vec![0.0f32; (width as usize) * (height as usize)];
        let Ok(settings) = self.validated() else {
            return coverage;
        };
        let Some(dabs) = preview_dabs(&settings, width, height) else {
            return coverage;
        };
        for dab in &dabs {
            let (lo, hi) = dab.bounds();
            for y in lo.y.max(0)..hi.y.min(height as i32) {
                for x in lo.x.max(0)..hi.x.min(width as i32) {
                    let c = dab.coverage_pixel(x, y) * dab.flow;
                    if c <= 0.0 {
                        continue;
                    }
                    let slot = &mut coverage[(y as usize) * (width as usize) + x as usize];
                    *slot += c * (1.0 - *slot);
                }
            }
        }
        let cap = settings.opacity.clamp(0.0, 1.0);
        for value in &mut coverage {
            *value = (*value).min(1.0) * cap;
        }
        coverage
    }

    /// How many dabs the preview gesture produces. Exposed because "spacing
    /// halves, dab count roughly doubles" is the property worth asserting.
    pub fn preview_dab_count(&self) -> usize {
        self.validated()
            .ok()
            .and_then(|s| preview_dabs(&s, PREVIEW_SIZE.0, PREVIEW_SIZE.1))
            .map_or(0, |dabs| dabs.len())
    }

    /// Draw the dialog for one frame.
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<DialogAction> {
        let keys = DialogKeys::read(ctx);
        let mut outcome = super::chrome::resolve(self, keys);
        self.refresh_preview(ctx);
        let drawn = modal(
            ctx,
            "brush-editor",
            self.title(),
            Some(crate::strings::tr(
                "ui.brush_editor.the.preview.runs.the.real.brush",
            )),
            DialogWidth::Standard,
            |ui| self.body(ui),
        );
        if let Some(Some(button)) = drawn {
            outcome = match button {
                DialogButton::Cancel => DialogOutcome::Cancelled,
                DialogButton::Confirm => self
                    .confirm()
                    .map_or(DialogOutcome::Open, DialogOutcome::Confirmed),
                DialogButton::Extra(_) => {
                    *self.settings_mut() = BrushSettings::default();
                    DialogOutcome::Open
                }
            };
        }
        outcome
    }

    fn refresh_preview(&mut self, ctx: &Context) {
        if self.cached_for.as_ref() == Some(&self.settings) && self.texture.is_some() {
            return;
        }
        let (width, height) = PREVIEW_SIZE;
        let coverage = self.preview_coverage(width, height);
        let ink = color32(
            design::current_theme(ctx)
                .palette()
                .text(design::tokens::TextRole::Primary),
        );
        let pixels: Vec<egui::Color32> = coverage
            .iter()
            .map(|c| {
                egui::Color32::from_rgba_unmultiplied(
                    ink.r(),
                    ink.g(),
                    ink.b(),
                    (c.clamp(0.0, 1.0) * 255.0).round() as u8,
                )
            })
            .collect();
        let image = egui::ColorImage {
            size: [width as usize, height as usize],
            pixels,
        };
        self.texture = Some(ctx.load_texture("brush-preview", image, egui::TextureOptions::LINEAR));
        self.cached_for = Some(self.settings);
    }

    fn body(&mut self, ui: &mut egui::Ui) -> Option<DialogButton> {
        self.preview(ui);
        hairline(ui);

        design::inspector_field(ui, "Name", |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.name).desired_width(sizes::text_field_wide()),
            );
        });

        design::section_header(ui, "Shape");
        let mut edited = self.settings;
        let mut changed = design::slider_row(ui, "Size", &mut edited.size, 1.0..=1000.0).changed();
        changed |= design::slider_row(ui, "Hardness", &mut edited.hardness, 0.0..=1.0).changed();
        changed |= design::slider_row(ui, "Roundness", &mut edited.roundness, 0.01..=1.0).changed();
        let mut degrees = edited.angle.to_degrees();
        if design::slider_row(ui, "Angle", &mut degrees, -180.0..=180.0).changed() {
            edited.angle = degrees.to_radians();
            changed = true;
        }
        changed |= design::slider_row(ui, "Spacing", &mut edited.spacing, 0.01..=10.0).changed();

        design::section_header(ui, "Paint");
        changed |= design::slider_row(ui, "Opacity", &mut edited.opacity, 0.0..=1.0).changed();
        changed |= design::slider_row(ui, "Flow", &mut edited.flow, 0.0..=1.0).changed();
        changed |= design::slider_row(ui, "Smoothing", &mut edited.smoothing, 0.0..=0.99).changed();
        if changed {
            *self.settings_mut() = edited;
        }
        let mut aliased = self.settings.aliased;
        if checkbox_row(
            ui,
            crate::strings::tr("ui.brush_editor.aliased.pencil"),
            &mut aliased,
        )
        .changed()
        {
            self.settings_mut().aliased = aliased;
        }

        design::section_header(ui, "Pressure");
        let mut size_pressure = self.settings.size_pressure;
        if checkbox_row(
            ui,
            crate::strings::tr("ui.brush_editor.pressure.controls.size"),
            &mut size_pressure,
        )
        .changed()
        {
            self.settings_mut().size_pressure = size_pressure;
        }
        let mut flow_pressure = self.settings.flow_pressure;
        if checkbox_row(
            ui,
            crate::strings::tr("ui.brush_editor.pressure.controls.flow"),
            &mut flow_pressure,
        )
        .changed()
        {
            self.settings_mut().flow_pressure = flow_pressure;
        }
        ui.add_enabled_ui(size_pressure, |ui| {
            let mut value = self.settings.min_size_ratio;
            if design::slider_row(
                ui,
                crate::strings::tr("ui.brush_editor.min.size"),
                &mut value,
                0.0..=1.0,
            )
            .changed()
            {
                self.settings_mut().min_size_ratio = value;
            }
        });
        if !size_pressure {
            caption(
                ui,
                crate::strings::tr("ui.brush_editor.minimum.size.only.applies.when.pressure"),
            );
        }

        self.dynamics_sections(ui);

        if let Some(reason) = self.blocked_reason() {
            ui.add_space(Space::Small.pt());
            warning(ui, reason);
        }
        ui.add_space(Space::Small.pt());
        action_row(
            ui,
            self.confirm_label(),
            self.blocked_reason().as_deref(),
            &["Reset"],
        )
    }

    /// W9-E: Photopea's Brush panel sections past the tip — Tip (which
    /// shape is stamped), Shape Dynamics, Scattering, Color Dynamics and
    /// Transfer. Each is a collapsing section, closed until opened, so the
    /// dialog stays the height it was for a brush that uses none of them.
    fn dynamics_sections(&mut self, ui: &mut egui::Ui) {
        use crate::strings::tr;
        let tip_caption = match self.settings.tip {
            tools::brush::BrushTip::Round => tr("ui.brush_editor.tip.round").to_string(),
            tools::brush::BrushTip::Sampled(id) => match tools::brush::sampled_tip(id) {
                Some(t) => format!(
                    "{} {} x {}",
                    tr("ui.brush_editor.tip.sampled"),
                    t.width(),
                    t.height()
                ),
                None => tr("ui.brush_editor.tip.missing").to_string(),
            },
        };
        design::section_header(ui, "Tip");
        caption(ui, tip_caption);

        let mut d = self.settings.dynamics;
        let mut changed = false;
        let section = |ui: &mut egui::Ui,
                       id: &str,
                       title: &str,
                       body: &mut dyn FnMut(&mut egui::Ui) -> bool| {
            egui::CollapsingHeader::new(title)
                .id_salt(("brush-editor-section", id))
                .default_open(false)
                .show(ui, |ui| body(ui))
                .body_returned
                .unwrap_or(false)
        };
        changed |= section(
            ui,
            "shape",
            tr("ui.brush_editor.shape.dynamics"),
            &mut |ui| {
                let mut c = design::slider_row(
                    ui,
                    tr("ui.brush_editor.size.jitter"),
                    &mut d.size_jitter,
                    0.0..=1.0,
                )
                .changed();
                c |= design::slider_row(
                    ui,
                    tr("ui.brush_editor.min.diameter"),
                    &mut d.min_diameter,
                    0.0..=1.0,
                )
                .changed();
                c |= design::slider_row(
                    ui,
                    tr("ui.brush_editor.angle.jitter"),
                    &mut d.angle_jitter,
                    0.0..=1.0,
                )
                .changed();
                c |= design::slider_row(
                    ui,
                    tr("ui.brush_editor.roundness.jitter"),
                    &mut d.roundness_jitter,
                    0.0..=1.0,
                )
                .changed();
                c |= design::slider_row(
                    ui,
                    tr("ui.brush_editor.min.roundness"),
                    &mut d.min_roundness,
                    0.0..=1.0,
                )
                .changed();
                c
            },
        );
        changed |= section(ui, "scatter", "Scattering", &mut |ui| {
            let mut c = design::slider_row(ui, "Scatter", &mut d.scatter, 0.0..=10.0).changed();
            c |= checkbox_row(
                ui,
                tr("ui.brush_editor.both.axes"),
                &mut d.scatter_both_axes,
            )
            .changed();
            let mut count = d.count as f32;
            if design::slider_row(ui, "Count", &mut count, 1.0..=16.0).changed() {
                d.count = count.round().clamp(1.0, 16.0) as u32;
                c = true;
            }
            c |= design::slider_row(
                ui,
                tr("ui.brush_editor.count.jitter"),
                &mut d.count_jitter,
                0.0..=1.0,
            )
            .changed();
            c
        });
        changed |= section(
            ui,
            "colour",
            tr("ui.brush_editor.color.dynamics"),
            &mut |ui| {
                let mut c = design::slider_row(
                    ui,
                    tr("ui.brush_editor.fg.bg.jitter"),
                    &mut d.fg_bg_jitter,
                    0.0..=1.0,
                )
                .changed();
                c |= design::slider_row(
                    ui,
                    tr("ui.brush_editor.hue.jitter"),
                    &mut d.hue_jitter,
                    0.0..=1.0,
                )
                .changed();
                c |= design::slider_row(
                    ui,
                    tr("ui.brush_editor.saturation.jitter"),
                    &mut d.saturation_jitter,
                    0.0..=1.0,
                )
                .changed();
                c |= design::slider_row(
                    ui,
                    tr("ui.brush_editor.brightness.jitter"),
                    &mut d.brightness_jitter,
                    0.0..=1.0,
                )
                .changed();
                caption(ui, tr("ui.brush_editor.colour.varies.per.stroke"));
                c
            },
        );
        changed |= section(ui, "transfer", "Transfer", &mut |ui| {
            let mut c = design::slider_row(
                ui,
                tr("ui.brush_editor.opacity.jitter"),
                &mut d.opacity_jitter,
                0.0..=1.0,
            )
            .changed();
            c |= design::slider_row(
                ui,
                tr("ui.brush_editor.flow.jitter"),
                &mut d.flow_jitter,
                0.0..=1.0,
            )
            .changed();
            c
        });
        if changed {
            self.settings_mut().dynamics = d;
        }
    }

    fn preview(&mut self, ui: &mut egui::Ui) {
        let t = current_tokens(ui);
        let size = sizes::brush_stroke_preview();
        let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
        if ui.is_rect_visible(rect) {
            let radius = Radius::Medium.resolve(&t.radii, size.y);
            ui.painter().rect_filled(
                rect,
                rounding(radius),
                color32(t.palette.color(ColorRole::SurfaceSunken)),
            );
            if let Some(texture) = &self.texture {
                ui.painter().image(
                    texture.id(),
                    rect,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    super::controls::UNTINTED,
                );
            }
        }
        caption(
            ui,
            format!(
                "{} dabs  ·  step {:.1} px",
                self.preview_dab_count(),
                self.validated().map(|s| s.step()).unwrap_or(0.0)
            ),
        );
    }
}

/// The dabs the preview gesture produces for `settings`.
///
/// A shallow S-curve across the preview with pressure ramping 0 -> 1 -> 0, run
/// through the real emitter. `None` when the emitter refuses the settings.
pub fn preview_dabs(settings: &BrushSettings, width: u32, height: u32) -> Option<Vec<Dab>> {
    const SAMPLES: usize = 96;
    let w = width as f32;
    let h = height as f32;
    let margin = (settings.size * 0.5).min(w * 0.2) + 4.0;
    let point = |t: f32| {
        let x = margin + (w - 2.0 * margin).max(1.0) * t;
        let y = h * 0.5 - (h * 0.3) * (t * std::f32::consts::TAU).sin();
        Vec2::new(x, y)
    };
    let pressure = |t: f32| (t * std::f32::consts::PI).sin().clamp(0.0, 1.0);

    let mut emitter = DabEmitter::begin(*settings, point(0.0), pressure(0.0)).ok()?;
    for step in 1..SAMPLES {
        let t = step as f32 / (SAMPLES - 1) as f32;
        emitter.extend(point(t), pressure(t)).ok()?;
    }
    emitter.finish(point(1.0), pressure(1.0)).ok()?;
    Some(emitter.dabs().to_vec())
}

impl Dialog for BrushEditorDialog {
    fn title(&self) -> &'static str {
        crate::strings::tr("ui.brush_editor.brush.editor")
    }

    fn confirm_label(&self) -> &'static str {
        crate::strings::tr("ui.brush_editor.save.brush")
    }

    fn confirm(&self) -> Option<DialogAction> {
        if self.name.trim().is_empty() {
            return None;
        }
        self.validated()
            .ok()
            .map(|settings| DialogAction::SetBrush {
                name: self.name.trim().to_string(),
                settings: Box::new(settings),
            })
    }

    fn blocked_reason(&self) -> Option<String> {
        if self.name.trim().is_empty() {
            return Some(crate::strings::tr("ui.brush_editor.give.the.brush.a.name").to_string());
        }
        self.validated().err().map(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialogs::chrome::test_support::frame_both_themes;

    #[test]
    fn the_preview_texture_matches_the_rectangle_it_fills() {
        // Two different kinds of number that happen to be equal: the texture is
        // in pixels, the rectangle is in points. Keeping them equal is what
        // stops the preview being resampled — a resampled stroke preview lies
        // about hardness, which is the parameter it exists to show.
        let on_screen = sizes::brush_stroke_preview();
        assert_eq!(on_screen.x, PREVIEW_SIZE.0 as f32);
        assert_eq!(on_screen.y, PREVIEW_SIZE.1 as f32);
    }

    #[test]
    fn the_default_brush_is_savable() {
        let dialog = BrushEditorDialog::default();
        assert!(dialog.confirm().is_some());
        assert!(dialog.blocked_reason().is_none());
    }

    #[test]
    fn the_preview_uses_the_real_engine_and_paints_something() {
        let dialog = BrushEditorDialog::default();
        let coverage = dialog.preview_coverage(PREVIEW_SIZE.0, PREVIEW_SIZE.1);
        assert_eq!(coverage.len(), (PREVIEW_SIZE.0 * PREVIEW_SIZE.1) as usize);
        let painted = coverage.iter().filter(|c| **c > 0.01).count();
        assert!(painted > 100, "only {painted} pixels were painted");
        assert!(coverage.iter().all(|c| (0.0..=1.0).contains(c)));
    }

    #[test]
    fn halving_the_spacing_roughly_doubles_the_dab_count() {
        let mut dialog = BrushEditorDialog::default();
        dialog.settings_mut().spacing = 0.4;
        let coarse = dialog.preview_dab_count();
        dialog.settings_mut().spacing = 0.2;
        let fine = dialog.preview_dab_count();
        assert!(
            fine as f32 > coarse as f32 * 1.7,
            "{coarse} dabs at 0.4 spacing, {fine} at 0.2"
        );
    }

    #[test]
    fn opacity_caps_the_whole_stroke() {
        let mut dialog = BrushEditorDialog::default();
        dialog.settings_mut().opacity = 1.0;
        let full = dialog.preview_coverage(PREVIEW_SIZE.0, PREVIEW_SIZE.1);
        dialog.settings_mut().opacity = 0.5;
        let half = dialog.preview_coverage(PREVIEW_SIZE.0, PREVIEW_SIZE.1);
        let peak_full = full.iter().cloned().fold(0.0f32, f32::max);
        let peak_half = half.iter().cloned().fold(0.0f32, f32::max);
        assert!(peak_full > 0.9, "peak was {peak_full}");
        assert!(
            (peak_half - peak_full * 0.5).abs() < 0.02,
            "{peak_half} is not half of {peak_full}"
        );
    }

    #[test]
    fn pressure_on_size_makes_the_stroke_taper() {
        // The gesture ramps pressure 0 -> 1 -> 0, so with size pressure on the
        // stroke is thin at both ends. Count painted pixels in the first
        // eighth against the middle.
        let mut dialog = BrushEditorDialog::default();
        dialog.settings_mut().size_pressure = true;
        dialog.settings_mut().min_size_ratio = 0.05;
        let (w, h) = PREVIEW_SIZE;
        let coverage = dialog.preview_coverage(w, h);
        let column_ink = |x0: u32, x1: u32| -> f32 {
            let mut sum = 0.0;
            for y in 0..h {
                for x in x0..x1 {
                    sum += coverage[(y * w + x) as usize];
                }
            }
            sum
        };
        let start = column_ink(0, w / 8);
        let middle = column_ink(w * 3 / 8, w / 2);
        assert!(
            middle > start * 1.5,
            "start {start} vs middle {middle}: the taper is missing"
        );
    }

    #[test]
    fn turning_size_pressure_off_removes_the_taper() {
        let mut dialog = BrushEditorDialog::default();
        dialog.settings_mut().size_pressure = false;
        let with_pressure = {
            let mut other = BrushEditorDialog::default();
            other.settings_mut().size_pressure = true;
            other.settings_mut().min_size_ratio = 0.05;
            other.preview_coverage(PREVIEW_SIZE.0, PREVIEW_SIZE.1)
        };
        let flat = dialog.preview_coverage(PREVIEW_SIZE.0, PREVIEW_SIZE.1);
        let ink = |c: &[f32]| c.iter().sum::<f32>();
        assert!(
            ink(&flat) > ink(&with_pressure),
            "a constant-width stroke should lay down more ink than a tapered one"
        );
    }

    #[test]
    fn an_impossible_brush_blocks_the_save_and_says_why() {
        let mut dialog = BrushEditorDialog::default();
        dialog.settings_mut().size = 0.0;
        assert!(dialog.confirm().is_none());
        assert!(dialog.blocked_reason().is_some());
        // And it still previews without panicking.
        assert!(dialog
            .preview_coverage(PREVIEW_SIZE.0, PREVIEW_SIZE.1)
            .iter()
            .all(|c| *c == 0.0));
    }

    #[test]
    fn a_nameless_brush_blocks_the_save() {
        let mut dialog = BrushEditorDialog::default();
        dialog.set_name("   ");
        assert!(dialog.confirm().is_none());
        assert!(dialog.blocked_reason().unwrap().contains("name"));
    }

    #[test]
    fn saving_hands_back_the_clamped_settings_not_the_raw_ones() {
        let mut dialog = BrushEditorDialog::default();
        dialog.settings_mut().spacing = 500.0;
        match dialog.confirm() {
            Some(DialogAction::SetBrush { settings, .. }) => {
                assert_eq!(settings.spacing, 10.0, "spacing was not clamped");
            }
            other => panic!("expected brush settings, got {other:?}"),
        }
    }

    #[test]
    fn the_typed_name_rides_out_with_the_settings() {
        // The defect this pins: the Name field gated the save — `confirm()`
        // returned `None` and the button said why while it was blank — and
        // then the name never left the dialog, because the action had nowhere
        // to put it. A control that demands input and discards it is a bug.
        let mut dialog = BrushEditorDialog::default();
        dialog.set_name("  Round Soft 40  ");
        match dialog.confirm() {
            Some(DialogAction::SetBrush { name, .. }) => {
                assert_eq!(name, "Round Soft 40", "the name was not carried out");
            }
            other => panic!("expected a named brush, got {other:?}"),
        }
        // And the label the status bar shows names it too.
        assert!(dialog.confirm().unwrap().label().contains("Round Soft 40"));
    }

    #[test]
    fn a_nameless_brush_is_not_a_valid_action_even_if_one_is_built_by_hand() {
        // The gate lives in the action as well as in the dialog, so a caller
        // that assembles one cannot smuggle a blank name past it.
        let action = DialogAction::SetBrush {
            name: "   ".to_string(),
            settings: Box::default(),
        };
        assert!(!action.is_valid());
    }

    #[test]
    fn confirm_produces_settings_and_cancel_produces_nothing() {
        let dialog = BrushEditorDialog::default();
        assert!(dialog.confirm().unwrap().is_valid());
        assert_eq!(
            super::super::chrome::resolve(&dialog, DialogKeys::CANCEL),
            DialogOutcome::Cancelled
        );
    }

    #[test]
    fn it_draws_in_both_appearances() {
        frame_both_themes(|ctx| {
            let mut dialog = BrushEditorDialog::default();
            assert!(dialog.show(ctx).is_open());
            let mut pencil = BrushEditorDialog::new(BrushSettings::pencil(3.0));
            assert!(pencil.show(ctx).is_open());
        });
    }

    /// W9-E: every text one settled frame of the dialog draws, with its rect.
    fn drawn(
        ctx: &Context,
        dialog: &mut BrushEditorDialog,
        events: Vec<egui::Event>,
    ) -> Vec<(String, egui::Rect)> {
        let input = |events: Vec<egui::Event>| egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1600.0, 1600.0),
            )),
            events,
            ..Default::default()
        };
        let out = ctx.run(input(events), |ctx| {
            let _ = dialog.show(ctx);
        });
        out.shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Text(t) => Some((
                    t.galley.text().to_string(),
                    t.galley.rect.translate(t.pos.to_vec2()),
                )),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_dialog_draws_the_tip_and_dynamics_sections_and_they_open() {
        let ctx = Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let mut dialog = BrushEditorDialog::default();
        let mut texts = Vec::new();
        for _ in 0..3 {
            texts = drawn(&ctx, &mut dialog, Vec::new());
        }
        let has = |texts: &[(String, egui::Rect)], s: &str| texts.iter().any(|(t, _)| t == s);
        for header in [
            "Tip",
            "Round (computed)",
            "Shape Dynamics",
            "Scattering",
            "Color Dynamics",
            "Transfer",
        ] {
            assert!(has(&texts, header), "{header:?} not drawn: {texts:?}");
        }
        assert!(!has(&texts, "Size jitter"), "sections start closed");
        // Click the Shape Dynamics header: its sliders appear.
        let at = texts
            .iter()
            .find(|(t, _)| t == "Shape Dynamics")
            .map(|(_, r)| r.center())
            .unwrap();
        let click = vec![
            egui::Event::PointerMoved(at),
            egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            },
            egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::default(),
            },
        ];
        drawn(&ctx, &mut dialog, click);
        for _ in 0..3 {
            texts = drawn(&ctx, &mut dialog, Vec::new());
        }
        for label in [
            "Size jitter",
            "Min diameter",
            "Angle jitter",
            "Roundness jitter",
        ] {
            assert!(has(&texts, label), "{label:?} not drawn once opened");
        }
    }

    #[test]
    fn dynamics_change_the_preview_and_ride_out_with_the_saved_brush() {
        let mut dialog = BrushEditorDialog::default();
        dialog.settings_mut().size = 12.0;
        dialog.settings_mut().size_pressure = false;
        let plain = dialog.preview_coverage(PREVIEW_SIZE.0, PREVIEW_SIZE.1);
        let plain_count = dialog.preview_dab_count();
        dialog.settings_mut().dynamics.scatter = 2.0;
        dialog.settings_mut().dynamics.count = 2;
        dialog.settings_mut().dynamics.size_jitter = 0.5;
        let scattered = dialog.preview_coverage(PREVIEW_SIZE.0, PREVIEW_SIZE.1);
        assert_ne!(plain, scattered, "the preview ignored the dynamics");
        assert_eq!(dialog.preview_dab_count(), plain_count * 2);
        match dialog.confirm() {
            Some(DialogAction::SetBrush { settings, .. }) => {
                assert_eq!(settings.dynamics.scatter, 2.0);
                assert_eq!(settings.dynamics.count, 2);
                assert_eq!(settings.dynamics.size_jitter, 0.5);
            }
            other => panic!("expected brush settings, got {other:?}"),
        }
    }

    #[test]
    fn a_sampled_tip_is_named_in_the_tip_section() {
        let id = tools::brush::TipId([0xE7; 32]);
        tools::brush::register_sampled_tip(
            id,
            tools::brush::SampledTip::new(3, 2, vec![255; 6]).unwrap(),
        );
        let ctx = Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let mut dialog = BrushEditorDialog::new(BrushSettings {
            tip: tools::brush::BrushTip::Sampled(id),
            ..Default::default()
        });
        let mut texts = Vec::new();
        for _ in 0..3 {
            texts = drawn(&ctx, &mut dialog, Vec::new());
        }
        assert!(texts.iter().any(|(t, _)| t == "Sampled 3 x 2"), "{texts:?}");
    }
}
