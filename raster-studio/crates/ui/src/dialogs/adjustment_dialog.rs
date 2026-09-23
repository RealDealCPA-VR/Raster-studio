//! Image ▸ Adjustments: one dialog for every [`AdjustmentId`].
//!
//! Before this module the Image ▸ Adjustments submenu was two lists: ten items
//! greyed with "the shell hosts no dialog to change it in", and five that
//! baked their fixed starting parameters into the pixels the moment they were
//! clicked — Posterize at four levels, Threshold at the middle, no question
//! asked. This is the dialog that was missing, built in the shape of
//! [`super::filter_dialog::FilterDialog`]: the parameter editor, a live
//! preview over a bounded proxy of the layer, a Preview toggle, Reset, and a
//! confirm path that goes through [`super::chrome::resolve`] like every other
//! dialog's.
//!
//! # The editor
//!
//! For the adjustments the Properties panel edits in place — Brightness/
//! Contrast, Levels, Exposure, Vibrance, Hue/Saturation, Posterize, Threshold —
//! the rows here are the same [`design::slider_row`] rows over the same
//! ranges, so a value dragged in the panel and a value dragged here mean the
//! same thing. The panel's own editor is a private function of
//! `crate::view::docks` bound to a live `Workspace`, so it cannot be *called*
//! from a dialog; the widgets it is made of are what is shared. The eight the
//! panel hands off to "Open editor…" — Curves, Color Balance, Black & White,
//! Photo Filter, Channel Mixer, Gradient Map, Selective Color and Invert — get
//! their editors here.
//!
//! # The preview
//!
//! The preview runs the *real* [`adjustments::PreparedAdjustment`] over a
//! box-downsampled copy of the layer no larger than [`MAX_PREVIEW_SIDE`] on a
//! side, so a slider drag on an 8K canvas costs the same as on a thumbnail.
//! Levels also shows the luma histogram of that proxy, which is what the
//! black and white points are read against.
//!
//! # What confirming produces
//!
//! [`AdjustmentDialog::invocation`] is the answer: the adjustment and its
//! parameters. The dialog host in the shell reads it and bakes the adjustment
//! into the active layer's pixels through the same route the menu item used
//! to take at its defaults. [`Dialog::confirm`] itself answers with the
//! non-destructive spelling of the same parameters — a
//! [`Command::CreateLayer`] holding them as an adjustment layer — because that
//! is the one form a [`DialogAction`] can carry without a host, and a host
//! with no bake path would still get a real, undoable edit out of it. Either
//! way an all-identity setting is refused with a reason rather than confirmed
//! into a no-op.

use adjustments::{
    Adjustment, BuiltinLut, Curve, EncodedRgb, ImageStats, Lut3d, PreparedAdjustment, ReplaceColor,
    ShadowsHighlights, HISTOGRAM_BINS,
};
use color::ColorSpace;
use design::tokens::palette::ColorRole;
use design::tokens::{grid, Radius, Space};
use design::{color32, current_tokens, egui_theme::rounding};
use editor_core::Command;
use egui::{pos2, Context, Rect, TextureHandle};
use filters::FilterBuffer;
use layer_model::{AdjustmentKind, AdjustmentLayer, Layer, LayerId, LayerKind};

use super::action::DialogAction;
use super::chrome::{
    action_row, caption, hairline, modal, Dialog, DialogButton, DialogKeys, DialogOutcome,
    DialogWidth,
};
use super::color_edit::ColorEdit;
use super::color_picker::ScreenSampler;
use super::controls::{checkbox_row, combo};
use super::{ids, sizes};
use crate::menu::AdjustmentId;
use crate::panels::properties::adjustment_id_of;
use crate::strings::tr;

/// Largest side of the proxy the live preview adjusts. The same bound the
/// filter dialogs use, so the two previews cost the same.
pub const MAX_PREVIEW_SIDE: u32 = super::filter_dialog::MAX_PREVIEW_SIDE;

/// The fixed input positions the Curves editor exposes a slider for.
///
/// A full curve editor is a canvas; a dialog gets the five points Photoshop's
/// own curve presets are written at, each with its output value editable. Five
/// knots on `y = x` are still recognised as the identity by
/// [`adjustments::Curve`], so an untouched Curves dialog is refused like any
/// other untouched adjustment.
pub const CURVE_INPUTS: [f32; 5] = [0.0, 0.25, 0.5, 0.75, 1.0];

/// Which colour the nested picker edits.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ColorTarget {
    /// Photo Filter's filter colour.
    PhotoFilter,
    /// One stop of the Gradient Map ramp, by index.
    GradientStop(usize),
    /// Replace Color's sampled colour.
    ReplaceColor,
}

/// The Replace Color dialog's selection preview: the coverage of the sampled
/// colour over the preview source, white replaced and black kept.
pub fn replace_color_mask_id() -> egui::Id {
    egui::Id::new(("raster-dialogs", "adjustment-replace-mask"))
}

/// The Color Lookup dialog's "Load .cube file" button.
pub fn lut_load_button_id() -> egui::Id {
    egui::Id::new(("raster-dialogs", "adjustment-lut-load"))
}

/// The Color Lookup table choices the dialog lists: `0` is "none", `1..`
/// index [`BuiltinLut::ALL`].
const LUT_CHOICES: [usize; 6] = [0, 1, 2, 3, 4, 5];

/// The Color Lookup choice a loaded `.cube` file (or a layer's stored table
/// that is none of the built-in looks) stands at. It is not in
/// [`LUT_CHOICES`], so every listed row, "None" included, differs from it and
/// can be picked.
const LUT_LOADED: usize = usize::MAX;

/// What the dialog commits: which adjustment, with which parameters.
#[derive(Clone, PartialEq, Debug)]
pub struct AdjustmentInvocation {
    pub id: AdjustmentId,
    pub kind: AdjustmentKind,
}

impl AdjustmentInvocation {
    /// Whether the parameters belong to the adjustment and would change a
    /// pixel. An identity setting is not something to apply.
    pub fn is_valid(&self) -> bool {
        adjustment_id_of(&self.kind) == Some(self.id) && !self.is_identity()
    }

    /// Whether these parameters provably change nothing.
    ///
    /// Equalize is an analysis of the image, like the auto commands: it has
    /// no settings and is never an identity *setting*, whatever it would do
    /// to one particular image.
    pub fn is_identity(&self) -> bool {
        let adjustment = self.adjustment();
        !adjustment.needs_stats() && PreparedAdjustment::new(&adjustment).is_identity()
    }

    /// The adjustment, ready to prepare and apply.
    pub fn adjustment(&self) -> Adjustment {
        Adjustment::from(&self.kind)
    }

    /// The history label for applying this.
    pub fn label(&self) -> String {
        format!("{} {}", tr("ui.adjustment.confirm"), self.id.label())
    }

    /// The same parameters as a new adjustment layer, with `layer_id` as its
    /// id. The caller names the id so that asking twice builds the same
    /// command — [`Dialog::confirm`] is documented as pure.
    pub fn layer_command(&self, layer_id: LayerId) -> Command {
        let mut layer = Layer::with_kind(
            self.id.label(),
            LayerKind::Adjustment(AdjustmentLayer {
                kind: self.kind.clone(),
            }),
        );
        layer.id = layer_id;
        Command::create_layer(layer)
    }
}

/// The one adjustment dialog.
pub struct AdjustmentDialog {
    id: AdjustmentId,
    kind: AdjustmentKind,
    /// The id the confirm-time layer command carries, fixed when the dialog
    /// opens so confirming is pure.
    layer_id: LayerId,
    space: ColorSpace,
    source: FilterBuffer,
    /// The luma histogram of `source`, computed once. Only Levels draws it,
    /// so only Levels pays for it.
    histogram: Option<[u32; HISTOGRAM_BINS]>,
    preview_enabled: bool,
    texture: Option<TextureHandle>,
    cached_for: Option<AdjustmentKind>,
    color_edit: ColorEdit<ColorTarget>,
    /// Color Balance: which tone range the three sliders edit.
    balance_tone: usize,
    /// Channel Mixer: which output channel the row edits.
    mixer_output: usize,
    /// Selective Color: which colour range the four sliders edit.
    selective_range: usize,
    /// How many layer pixels one proxy pixel stands for, so a radius in
    /// layer pixels previews at the right size.
    proxy_scale: f32,
    /// Equalize: the preview source's histogram, measured once.
    stats: Option<ImageStats>,
    /// Replace Color: the selection preview and the parameters it shows.
    mask_texture: Option<TextureHandle>,
    mask_cached_for: Option<AdjustmentKind>,
    /// Color Lookup: which listed table is chosen (see [`LUT_CHOICES`]), or
    /// [`LUT_LOADED`] for a loaded file.
    lut_choice: usize,
    /// Color Lookup: the "Load .cube file" button was pressed and the host
    /// has not answered yet.
    lut_file_requested: bool,
    /// Color Lookup: why the last file did not load.
    lut_error: Option<String>,
    /// W4-E round 2: the adjustment layer this dialog edits in place, when it
    /// was opened by Layer > Edit Adjustment (or the Properties panel's
    /// "Open editor") rather than by Image > Adjustments. Confirming then
    /// rewrites that layer's parameters instead of baking pixels.
    edit_layer: Option<LayerId>,
}

impl std::fmt::Debug for AdjustmentDialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdjustmentDialog")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("preview_enabled", &self.preview_enabled)
            .finish_non_exhaustive()
    }
}

impl AdjustmentDialog {
    /// Open `id`'s dialog over `source`, the layer's pixels in `space`.
    ///
    /// `source` is downsampled to [`MAX_PREVIEW_SIDE`] here, so the caller
    /// hands over the layer as it is and the dialog decides what it can
    /// afford to adjust per frame.
    pub fn new(id: AdjustmentId, source: FilterBuffer, space: ColorSpace) -> Self {
        let full_width = source.dimensions().0.max(1);
        let source = preview_proxy(&source, MAX_PREVIEW_SIDE);
        let proxy_scale = full_width as f32 / source.dimensions().0.max(1) as f32;
        let histogram = (id == AdjustmentId::Levels).then(|| {
            *ImageStats::from_premultiplied_rgba(source.pixels(), &space)
                .luma
                .bins()
        });
        let stats = (id == AdjustmentId::Equalize)
            .then(|| ImageStats::from_premultiplied_rgba(source.pixels(), &space));
        let mut kind = editable_kind(id, id.identity_kind());
        // Replace Color opens sampling the middle of the layer, the way
        // Photopea's opens on the colour under its eyedropper.
        if let AdjustmentKind::ReplaceColor { color, .. } = &mut kind {
            let (w, h) = source.dimensions();
            if let Some(sampled) = sample_encoded(&source, &space, w / 2, h / 2) {
                *color = sampled;
            }
        }
        Self {
            id,
            kind,
            layer_id: LayerId::new(),
            space,
            source,
            histogram,
            preview_enabled: true,
            texture: None,
            cached_for: None,
            color_edit: ColorEdit::new(),
            balance_tone: 1,
            mixer_output: 0,
            selective_range: 0,
            proxy_scale,
            stats,
            mask_texture: None,
            mask_cached_for: None,
            lut_choice: 0,
            lut_file_requested: false,
            lut_error: None,
            edit_layer: None,
        }
    }

    /// Open the dialog on an existing adjustment layer's parameters, so
    /// confirming edits `layer` in place. `source` is what the layer adjusts
    /// (the composite beneath it). `None` when `kind` has no dialog.
    ///
    /// For Color Lookup the listed choice follows the table's name, so a
    /// layer holding a built-in look reopens with that look selected.
    pub fn for_layer(
        layer: LayerId,
        kind: AdjustmentKind,
        source: FilterBuffer,
        space: ColorSpace,
    ) -> Option<Self> {
        let id = adjustment_id_of(&kind)?;
        if !id.has_dialog() {
            return None;
        }
        let mut dialog = Self::new(id, source, space);
        if let AdjustmentKind::ColorLookup { name, .. } = &kind {
            dialog.lut_choice = match BuiltinLut::ALL.iter().position(|b| b.name() == name) {
                Some(i) => i + 1,
                None if name.is_empty() => 0,
                None => LUT_LOADED,
            };
        }
        dialog.set_kind(kind);
        dialog.edit_layer = Some(layer);
        Some(dialog)
    }

    /// The adjustment layer a confirmation edits in place, if the dialog
    /// was opened on one (see [`Self::for_layer`]).
    pub fn edit_layer(&self) -> Option<LayerId> {
        self.edit_layer
    }

    /// Color Lookup: pick entry `choice` of the listed tables — `0` is the
    /// identity, `1..` index [`BuiltinLut::ALL`]. The dialog's "Lookup table"
    /// combo calls this (from `params`) when a row is picked. Returns whether
    /// the choice was known.
    pub fn choose_lut(&mut self, choice: usize) -> bool {
        if !matches!(self.kind, AdjustmentKind::ColorLookup { .. })
            || choice > BuiltinLut::ALL.len()
        {
            return false;
        }
        if choice == self.lut_choice {
            return true;
        }
        self.lut_choice = choice;
        self.lut_error = None;
        let next = match choice.checked_sub(1).and_then(|i| BuiltinLut::ALL.get(i)) {
            Some(builtin) => lut_kind(&builtin.lut()),
            None => AdjustmentId::ColorLookup.identity_kind(),
        };
        self.set_kind(next)
    }

    /// Open `id`'s dialog over a generated proxy, for a caller with no pixels.
    pub fn with_placeholder(id: AdjustmentId) -> Self {
        Self::new(
            id,
            super::filter_dialog::placeholder_buffer(96, 96),
            ColorSpace::default(),
        )
    }

    /// Which adjustment this dialog edits.
    pub fn id(&self) -> AdjustmentId {
        self.id
    }

    /// The parameters as edited.
    pub fn kind(&self) -> &AdjustmentKind {
        &self.kind
    }

    /// Replace the parameters. Returns `false`, changing nothing, when `kind`
    /// is another adjustment's — a Threshold dialog does not become a
    /// Posterize dialog by being handed a level count.
    pub fn set_kind(&mut self, kind: AdjustmentKind) -> bool {
        if adjustment_id_of(&kind) != Some(self.id) {
            return false;
        }
        let kind = editable_kind(self.id, kind);
        if kind != self.kind {
            self.kind = kind;
            self.cached_for = None;
        }
        true
    }

    /// Put every parameter back to the adjustment's starting setting.
    pub fn reset(&mut self) {
        self.lut_choice = 0;
        self.lut_error = None;
        let identity = editable_kind(self.id, self.id.identity_kind());
        if identity != self.kind {
            self.kind = identity;
            self.cached_for = None;
        }
    }

    /// Whether the live preview is on.
    pub fn preview_enabled(&self) -> bool {
        self.preview_enabled
    }

    /// Turn the live preview on or off. Off means the adjustment is not run
    /// at all per frame.
    pub fn set_preview_enabled(&mut self, on: bool) {
        self.preview_enabled = on;
        if !on {
            self.texture = None;
            self.cached_for = None;
        }
    }

    /// The proxy the preview is computed over.
    pub fn source(&self) -> &FilterBuffer {
        &self.source
    }

    /// The luma histogram of the preview source, when this dialog shows one.
    pub fn histogram(&self) -> Option<&[u32; HISTOGRAM_BINS]> {
        self.histogram.as_ref()
    }

    /// Run the adjustment over the proxy and return the result.
    ///
    /// Equalize is resolved against the proxy's own histogram, and
    /// Shadows/Highlights runs its neighbourhood form with the radius scaled
    /// down to the proxy, so both preview what applying them would do.
    pub fn preview_buffer(&self) -> FilterBuffer {
        let mut out = self.source.clone();
        let adjustment = Adjustment::from(&self.kind);
        if let Adjustment::ShadowsHighlights(sh) = &adjustment {
            let scale = self.proxy_scale.max(1.0);
            let [sa, st, sr] = sh.shadows();
            let [ha, ht, hr] = sh.highlights();
            let scaled =
                ShadowsHighlights::new([sa, st, sr / scale], [ha, ht, hr / scale]).unwrap_or(*sh);
            let (w, h) = out.dimensions();
            let _ = scaled.apply_premultiplied_rgba_spatial(
                out.pixels_mut(),
                w as usize,
                h as usize,
                &self.space,
            );
            return out;
        }
        let prepared = match &self.stats {
            Some(stats) => PreparedAdjustment::with_stats(&adjustment, stats),
            None => PreparedAdjustment::new(&adjustment),
        };
        prepared.apply_premultiplied_rgba(out.pixels_mut(), &self.space);
        out
    }

    /// Replace Color: the coverage of the sampled colour over the preview
    /// source, one byte per pixel (255 fully replaced, 0 kept). Empty for any
    /// other adjustment.
    pub fn replace_color_coverage(&self) -> Vec<u8> {
        let Adjustment::ReplaceColor(rc) = Adjustment::from(&self.kind) else {
            return Vec::new();
        };
        self.source
            .pixels()
            .iter()
            .map(|px| {
                if px[3] <= color::UNPREMULTIPLY_ALPHA_EPSILON {
                    return 0;
                }
                let s = color::unpremultiply(*px);
                let enc = EncodedRgb(color::from_linear(&self.space, [s[0], s[1], s[2]]));
                (rc.coverage(enc) * 255.0).round() as u8
            })
            .collect()
    }

    /// Replace Color: sample the preview source at `(x, y)` (proxy pixels)
    /// as the colour to replace. Returns whether a colour was taken.
    pub fn sample_preview(&mut self, x: u32, y: u32) -> bool {
        let Some(sampled) = sample_encoded(&self.source, &self.space, x, y) else {
            return false;
        };
        let mut next = self.kind.clone();
        let AdjustmentKind::ReplaceColor { color, .. } = &mut next else {
            return false;
        };
        *color = sampled;
        self.set_kind(next)
    }

    /// Color Lookup: whether the "Load .cube file" button was pressed since
    /// the last call. The host answers with [`Self::load_cube_text`].
    pub fn take_lut_file_request(&mut self) -> bool {
        std::mem::take(&mut self.lut_file_requested)
    }

    /// Color Lookup: use the `.cube` file `text`, named `name` unless it has
    /// a `TITLE`. A file that does not parse leaves the table as it was and
    /// is reported in the dialog; the error is returned too.
    pub fn load_cube_text(&mut self, name: &str, text: &str) -> Result<(), String> {
        match Lut3d::parse_cube(name, text) {
            Ok(lut) => {
                self.lut_error = None;
                self.lut_choice = LUT_LOADED;
                self.set_kind(lut_kind(&lut));
                Ok(())
            }
            Err(e) => {
                let message = e.to_string();
                self.lut_error = Some(message.clone());
                Err(message)
            }
        }
    }

    /// Color Lookup: report a file that could not even be read.
    pub fn set_lut_error(&mut self, message: impl Into<String>) {
        self.lut_error = Some(message.into());
    }

    /// The invocation the dialog would commit.
    pub fn invocation(&self) -> AdjustmentInvocation {
        AdjustmentInvocation {
            id: self.id,
            kind: self.kind.clone(),
        }
    }

    /// The nested colour picker, when a swatch has been clicked.
    pub fn color_edit(&self) -> &ColorEdit<ColorTarget> {
        &self.color_edit
    }

    /// Mutable access to it, for a test that drives the swatch path.
    pub fn color_edit_mut(&mut self) -> &mut ColorEdit<ColorTarget> {
        &mut self.color_edit
    }

    /// Draw the dialog for one frame.
    ///
    /// `sampler` reaches the nested colour picker's eyedropper; `None` draws
    /// that button disabled with its reason.
    pub fn show(
        &mut self,
        ctx: &Context,
        sampler: Option<&dyn ScreenSampler>,
    ) -> DialogOutcome<DialogAction> {
        let nested = self.color_edit.is_open();
        let keys = if nested {
            DialogKeys::NONE
        } else {
            DialogKeys::read(ctx)
        };
        let mut outcome = super::chrome::resolve(self, keys);
        self.refresh_preview(ctx);
        let subtitle = tr("ui.adjustment.subtitle");
        let drawn = modal(
            ctx,
            ("adjustment", self.id),
            self.id.label(),
            Some(subtitle),
            DialogWidth::Standard,
            |ui| self.body(ui),
        );
        if let Some((target, rgba)) = self.color_edit.show(ctx, "adjustment-color", sampler) {
            self.write_color(target, rgba);
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
                DialogButton::Extra(_) => {
                    self.reset();
                    DialogOutcome::Open
                }
            };
        }
        outcome
    }

    /// Write a colour the nested picker confirmed into the parameter that
    /// opened it.
    fn write_color(&mut self, target: ColorTarget, rgba: [f32; 4]) {
        let rgb = [rgba[0], rgba[1], rgba[2]];
        let mut next = self.kind.clone();
        match (&mut next, target) {
            (AdjustmentKind::PhotoFilter { color_srgb, .. }, ColorTarget::PhotoFilter) => {
                *color_srgb = rgb;
            }
            (AdjustmentKind::GradientMap { stops, .. }, ColorTarget::GradientStop(index)) => {
                if let Some(stop) = stops.get_mut(index) {
                    stop.1 = rgb;
                }
            }
            (AdjustmentKind::ReplaceColor { color, .. }, ColorTarget::ReplaceColor) => {
                *color = rgb.map(|c| c.clamp(0.0, 1.0));
            }
            _ => return,
        }
        self.set_kind(next);
    }

    fn refresh_preview(&mut self, ctx: &Context) {
        if !self.preview_enabled {
            return;
        }
        if self.cached_for.as_ref() == Some(&self.kind) && self.texture.is_some() {
            return;
        }
        let adjusted = self.preview_buffer();
        let (width, height) = adjusted.dimensions();
        if width == 0 || height == 0 {
            self.texture = None;
            return;
        }
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [width as usize, height as usize],
            &adjusted.to_rgba8(),
        );
        self.texture =
            Some(ctx.load_texture("adjustment-preview", image, egui::TextureOptions::LINEAR));
        self.cached_for = Some(self.kind.clone());
    }

    fn body(&mut self, ui: &mut egui::Ui) -> Option<DialogButton> {
        let mut preview = self.preview_enabled;
        if checkbox_row(ui, tr("ui.adjustment.preview"), &mut preview).changed() {
            self.set_preview_enabled(preview);
        }
        match (&self.texture, self.preview_enabled) {
            (Some(texture), true) => {
                let size = texture.size_vec2();
                let scale = (sizes::filter_preview_width() / size.x.max(1.0)).min(2.0);
                let response = ui.image((texture.id(), size * scale));
                // Replace Color samples its colour from a click on the
                // preview, as Color Range does.
                let sense = if self.id == AdjustmentId::ReplaceColor {
                    egui::Sense::click()
                } else {
                    egui::Sense::hover()
                };
                let clicked = ui.interact(response.rect, ids::adjustment_preview(), sense);
                if clicked.clicked() {
                    if let Some(pos) = clicked.interact_pointer_pos() {
                        let local = (pos - response.rect.min) / scale.max(f32::EPSILON);
                        if local.x >= 0.0 && local.y >= 0.0 {
                            self.sample_preview(local.x as u32, local.y as u32);
                        }
                    }
                }
            }
            (_, true) => {
                caption(ui, tr("ui.adjustment.nothing.to.preview"));
            }
            (_, false) => {
                let (w, h) = self.source.dimensions();
                let width = sizes::filter_preview_width();
                let (rect, _) = ui.allocate_exact_size(
                    egui::vec2(width, width * h as f32 / w.max(1) as f32),
                    egui::Sense::hover(),
                );
                let radius = {
                    let t = current_tokens(ui);
                    Radius::Small.resolve(&t.radii, rect.height())
                };
                super::controls::checkerboard(ui, rect, radius);
                let _ = ui.interact(rect, ids::adjustment_preview(), egui::Sense::hover());
                caption(ui, tr("ui.adjustment.preview.off"));
            }
        }

        hairline(ui);
        if self.histogram.is_some() {
            self.histogram_row(ui);
        }
        if self.id == AdjustmentId::ReplaceColor {
            self.replace_mask_row(ui);
        }
        self.params(ui);
        ui.add_space(Space::Small.pt());
        action_row(
            ui,
            self.confirm_label(),
            self.blocked_reason().as_deref(),
            &[tr("ui.adjustment.reset")],
        )
    }

    /// Replace Color's selection preview, rebuilt when the parameters move.
    fn replace_mask_row(&mut self, ui: &mut egui::Ui) {
        if self.mask_cached_for.as_ref() != Some(&self.kind) || self.mask_texture.is_none() {
            let (w, h) = self.source.dimensions();
            let coverage = self.replace_color_coverage();
            if w == 0 || h == 0 || coverage.len() != (w * h) as usize {
                self.mask_texture = None;
            } else {
                let image = egui::ColorImage::from_gray([w as usize, h as usize], &coverage);
                self.mask_texture = Some(ui.ctx().load_texture(
                    "adjustment-replace-mask",
                    image,
                    egui::TextureOptions::NEAREST,
                ));
                self.mask_cached_for = Some(self.kind.clone());
            }
        }
        if let Some(texture) = &self.mask_texture {
            let size = texture.size_vec2();
            let scale = (sizes::filter_preview_width() / size.x.max(1.0)).min(2.0);
            let response = ui.image((texture.id(), size * scale));
            let _ = ui.interact(response.rect, replace_color_mask_id(), egui::Sense::hover());
            caption(ui, tr("ui.adjustment.replace.selection"));
        }
    }

    /// The luma histogram, with the black and white points marked on it.
    fn histogram_row(&self, ui: &mut egui::Ui) {
        let Some(bins) = self.histogram.as_ref() else {
            return;
        };
        let width = sizes::filter_preview_width();
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(width, histogram_height()), egui::Sense::hover());
        let _ = ui.interact(rect, ids::adjustment_histogram(), egui::Sense::hover());
        let t = current_tokens(ui);
        let radius = Radius::Small.resolve(&t.radii, rect.height());
        let painter = ui.painter();
        painter.rect_filled(
            rect,
            rounding(radius),
            color32(t.palette.color(ColorRole::SurfaceSunken)),
        );
        let peak = bins.iter().copied().max().unwrap_or(0).max(1) as f32;
        let bar = rect.width() / HISTOGRAM_BINS as f32;
        let fill = color32(t.palette.color(ColorRole::TextSecondary));
        let flat = Radius::None.resolve(&t.radii, bar);
        for (index, count) in bins.iter().enumerate() {
            if *count == 0 {
                continue;
            }
            let height = rect.height() * (*count as f32 / peak);
            let x0 = rect.left() + index as f32 * bar;
            painter.rect_filled(
                Rect::from_min_max(
                    pos2(x0, rect.bottom() - height),
                    pos2(x0 + bar, rect.bottom()),
                ),
                rounding(flat),
                fill,
            );
        }
        if let AdjustmentKind::Levels { black, white, .. } = &self.kind {
            let marker =
                egui::Stroke::new(t.borders.thick, color32(t.palette.color(ColorRole::Accent)));
            for point in [black, white] {
                let x = rect.left() + rect.width() * point.clamp(0.0, 1.0);
                painter.vline(x, rect.y_range(), marker);
            }
        }
        painter.rect_stroke(
            rect,
            rounding(radius),
            egui::Stroke::new(
                t.borders.hairline,
                color32(t.palette.color(ColorRole::ControlStroke)),
            ),
        );
        caption(ui, tr("ui.adjustment.histogram"));
    }

    /// The parameter editor. One arm per stored shape; the rows for the seven
    /// the Properties panel edits are the panel's rows.
    fn params(&mut self, ui: &mut egui::Ui) {
        use AdjustmentKind as K;
        let mut next = self.kind.clone();
        let mut changed = false;
        let mut open_picker: Option<(ColorTarget, [f32; 3])> = None;
        let mut promote: Option<AdjustmentKind> = None;
        let mut lut_pick: Option<usize> = None;

        match &mut next {
            K::BrightnessContrast {
                brightness,
                contrast,
            } => {
                let (mut b, mut c) = (*brightness * 100.0, *contrast * 100.0);
                changed |=
                    design::slider_row(ui, tr("ui.adjustment.brightness"), &mut b, -100.0..=100.0)
                        .changed();
                changed |=
                    design::slider_row(ui, tr("ui.adjustment.contrast"), &mut c, -100.0..=100.0)
                        .changed();
                *brightness = b / 100.0;
                *contrast = c / 100.0;
            }
            K::Levels {
                black,
                white,
                gamma,
            } => {
                changed |=
                    design::slider_row(ui, tr("ui.adjustment.black"), black, 0.0..=1.0).changed();
                changed |=
                    design::slider_row(ui, tr("ui.adjustment.white"), white, 0.0..=1.0).changed();
                changed |=
                    design::slider_row(ui, tr("ui.adjustment.gamma"), gamma, 0.1..=10.0).changed();
            }
            K::Curves { points } => {
                for (index, point) in points.iter_mut().enumerate() {
                    let label = curve_point_label(index);
                    changed |= design::slider_row(ui, label, &mut point[1], 0.0..=1.0).changed();
                }
            }
            K::Exposure { stops } => {
                changed |=
                    design::slider_row(ui, tr("ui.adjustment.exposure"), stops, -10.0..=10.0)
                        .changed();
            }
            K::Vibrance {
                vibrance,
                saturation,
            } => {
                changed |=
                    design::slider_row(ui, tr("ui.adjustment.vibrance"), vibrance, -1.0..=1.0)
                        .changed();
                changed |=
                    design::slider_row(ui, tr("ui.adjustment.saturation"), saturation, -1.0..=1.0)
                        .changed();
            }
            K::HueSaturation {
                hue,
                saturation,
                lightness,
            } => {
                changed |=
                    design::slider_row(ui, tr("ui.adjustment.hue"), hue, -180.0..=180.0).changed();
                changed |=
                    design::slider_row(ui, tr("ui.adjustment.saturation"), saturation, -1.0..=1.0)
                        .changed();
                changed |=
                    design::slider_row(ui, tr("ui.adjustment.lightness"), lightness, -1.0..=1.0)
                        .changed();
            }
            K::ColorBalance {
                shadows,
                midtones,
                highlights,
            } => {
                design::inspector_field(ui, tr("ui.adjustment.tone"), |ui| {
                    combo(
                        ui,
                        ("adjustment", "tone"),
                        &mut self.balance_tone,
                        &[0, 1, 2],
                        tone_label,
                        |_| None,
                    );
                });
                let range: &mut [f32; 3] = match self.balance_tone {
                    0 => &mut *shadows,
                    2 => &mut *highlights,
                    _ => &mut *midtones,
                };
                changed |= balance_sliders(ui, range);
                let mut preserve = false;
                if checkbox_row(ui, tr("ui.adjustment.preserve.luminosity"), &mut preserve)
                    .changed()
                {
                    // The switch lives on the wide stored spelling; the three
                    // ranges carry over as they are.
                    promote = Some(K::ColorBalanceFull {
                        shadows: *shadows,
                        midtones: *midtones,
                        highlights: *highlights,
                        preserve_luminosity: preserve,
                    });
                }
            }
            K::ColorBalanceFull {
                shadows,
                midtones,
                highlights,
                preserve_luminosity,
            } => {
                design::inspector_field(ui, tr("ui.adjustment.tone"), |ui| {
                    combo(
                        ui,
                        ("adjustment", "tone"),
                        &mut self.balance_tone,
                        &[0, 1, 2],
                        tone_label,
                        |_| None,
                    );
                });
                let range: &mut [f32; 3] = match self.balance_tone {
                    0 => &mut *shadows,
                    2 => &mut *highlights,
                    _ => &mut *midtones,
                };
                changed |= balance_sliders(ui, range);
                changed |= checkbox_row(
                    ui,
                    tr("ui.adjustment.preserve.luminosity"),
                    preserve_luminosity,
                )
                .changed();
            }
            K::BlackAndWhite { weights, tint } => {
                const KEYS: [&str; 6] = [
                    "ui.adjustment.reds",
                    "ui.adjustment.yellows",
                    "ui.adjustment.greens",
                    "ui.adjustment.cyans",
                    "ui.adjustment.blues",
                    "ui.adjustment.magentas",
                ];
                for (weight, key) in weights.iter_mut().zip(KEYS) {
                    let mut percent = *weight * 100.0;
                    changed |=
                        design::slider_row(ui, tr(key), &mut percent, -300.0..=300.0).changed();
                    *weight = percent / 100.0;
                }
                let mut tinted = tint.is_some();
                if checkbox_row(ui, tr("ui.adjustment.tint"), &mut tinted).changed() {
                    *tint = tinted.then_some([40.0, 0.2]);
                    changed = true;
                }
                if let Some([hue, saturation]) = tint {
                    changed |=
                        design::slider_row(ui, tr("ui.adjustment.tint.hue"), hue, 0.0..=360.0)
                            .changed();
                    changed |= design::slider_row(
                        ui,
                        tr("ui.adjustment.tint.saturation"),
                        saturation,
                        0.0..=1.0,
                    )
                    .changed();
                }
            }
            K::PhotoFilter {
                color_srgb,
                density,
                preserve_luminosity,
            } => {
                let rgba = [color_srgb[0], color_srgb[1], color_srgb[2], 1.0];
                design::inspector_field(ui, tr("ui.adjustment.color"), |ui| {
                    if super::controls::swatch(
                        ui,
                        ids::adjustment_color(ColorTarget::PhotoFilter),
                        rgba,
                        sizes::swatch(),
                    )
                    .clicked()
                    {
                        open_picker = Some((ColorTarget::PhotoFilter, *color_srgb));
                    }
                });
                let mut percent = *density * 100.0;
                changed |=
                    design::slider_row(ui, tr("ui.adjustment.density"), &mut percent, 0.0..=100.0)
                        .changed();
                *density = percent / 100.0;
                changed |= checkbox_row(
                    ui,
                    tr("ui.adjustment.preserve.luminosity"),
                    preserve_luminosity,
                )
                .changed();
            }
            K::ChannelMixer { rows, monochrome } => {
                design::inspector_field(ui, tr("ui.adjustment.output.channel"), |ui| {
                    combo(
                        ui,
                        ("adjustment", "output-channel"),
                        &mut self.mixer_output,
                        &[0, 1, 2],
                        channel_label,
                        |_| None,
                    );
                });
                let row = &mut rows[self.mixer_output.min(2)];
                const KEYS: [&str; 3] = [
                    "ui.adjustment.red",
                    "ui.adjustment.green",
                    "ui.adjustment.blue",
                ];
                for (weight, key) in row[..3].iter_mut().zip(KEYS) {
                    let mut percent = *weight * 100.0;
                    changed |=
                        design::slider_row(ui, tr(key), &mut percent, -200.0..=200.0).changed();
                    *weight = percent / 100.0;
                }
                let mut constant = row[3] * 100.0;
                changed |= design::slider_row(
                    ui,
                    tr("ui.adjustment.constant"),
                    &mut constant,
                    -100.0..=100.0,
                )
                .changed();
                row[3] = constant / 100.0;
                changed |= checkbox_row(ui, tr("ui.adjustment.monochrome"), monochrome).changed();
            }
            K::Invert => {
                caption(ui, tr("ui.adjustment.no.settings"));
            }
            K::Posterize { levels } => {
                let mut v = *levels as f32;
                changed |= design::slider_row(ui, tr("ui.adjustment.levels"), &mut v, 2.0..=256.0)
                    .changed();
                *levels = v.round().clamp(2.0, 256.0) as u32;
            }
            K::Threshold { level } => {
                changed |=
                    design::slider_row(ui, tr("ui.adjustment.level"), level, 0.0..=1.0).changed();
            }
            K::GradientMap { stops, reverse } => {
                for (index, (position, color)) in stops.iter().enumerate() {
                    let label = format!("{} {}", tr("ui.adjustment.stop"), index + 1);
                    let rgba = [color[0], color[1], color[2], 1.0];
                    design::inspector_field(ui, &label, |ui| {
                        if super::controls::swatch(
                            ui,
                            ids::adjustment_color(ColorTarget::GradientStop(index)),
                            rgba,
                            sizes::swatch(),
                        )
                        .clicked()
                        {
                            open_picker = Some((ColorTarget::GradientStop(index), *color));
                        }
                        caption(ui, format!("{:.0}%", position * 100.0));
                    });
                }
                changed |= checkbox_row(ui, tr("ui.adjustment.reverse"), reverse).changed();
            }
            K::SelectiveColor { ranges, relative } => {
                design::inspector_field(ui, tr("ui.adjustment.colors"), |ui| {
                    combo(
                        ui,
                        ("adjustment", "selective-range"),
                        &mut self.selective_range,
                        &[0, 1, 2, 3, 4, 5, 6, 7, 8],
                        selective_range_label,
                        |_| None,
                    );
                });
                let range = &mut ranges[self.selective_range.min(8)];
                const KEYS: [&str; 4] = [
                    "ui.adjustment.cyan",
                    "ui.adjustment.magenta",
                    "ui.adjustment.yellow",
                    "ui.adjustment.black.ink",
                ];
                for (delta, key) in range.iter_mut().zip(KEYS) {
                    let mut percent = *delta * 100.0;
                    changed |=
                        design::slider_row(ui, tr(key), &mut percent, -100.0..=100.0).changed();
                    *delta = percent / 100.0;
                }
                changed |= checkbox_row(ui, tr("ui.adjustment.relative"), relative).changed();
            }
            // The wide stored spellings the menu never starts from. A document
            // cannot hand one to this dialog either: it always opens at
            // `identity_kind`. Drawn as their narrow form's rows would be
            // misleading, so they say what they are.
            K::LevelsFull { .. }
            | K::CurvesFull { .. }
            | K::ExposureFull { .. }
            | K::HueSaturationFull { .. }
            | K::Auto { .. }
            | K::Desaturate
            | K::Equalize => {
                caption(ui, tr("ui.adjustment.no.settings"));
            }
            K::ShadowsHighlights {
                shadows,
                highlights,
            } => {
                for (heading, band) in [
                    ("ui.adjustment.shadows", shadows),
                    ("ui.adjustment.highlights", highlights),
                ] {
                    caption(ui, tr(heading));
                    let (mut amount, mut tone) = (band[0] * 100.0, band[1] * 100.0);
                    changed |= design::slider_row(
                        ui,
                        tr("ui.adjustment.amount"),
                        &mut amount,
                        0.0..=100.0,
                    )
                    .changed();
                    changed |= design::slider_row(
                        ui,
                        tr("ui.adjustment.tonal.width"),
                        &mut tone,
                        0.0..=100.0,
                    )
                    .changed();
                    changed |= design::slider_row(
                        ui,
                        tr("ui.adjustment.radius"),
                        &mut band[2],
                        0.0..=adjustments::MAX_SHADOWS_HIGHLIGHTS_RADIUS,
                    )
                    .changed();
                    band[0] = amount / 100.0;
                    band[1] = tone / 100.0;
                }
            }
            K::ReplaceColor {
                color,
                fuzziness,
                hue,
                saturation,
                lightness,
            } => {
                let rgba = [color[0], color[1], color[2], 1.0];
                design::inspector_field(ui, tr("ui.adjustment.sampled.color"), |ui| {
                    if super::controls::swatch(
                        ui,
                        ids::adjustment_color(ColorTarget::ReplaceColor),
                        rgba,
                        sizes::swatch(),
                    )
                    .clicked()
                    {
                        open_picker = Some((ColorTarget::ReplaceColor, *color));
                    }
                });
                caption(ui, tr("ui.adjustment.replace.click"));
                let mut fuzz = *fuzziness * 255.0;
                changed |=
                    design::slider_row(ui, tr("ui.adjustment.fuzziness"), &mut fuzz, 0.0..=200.0)
                        .changed();
                *fuzziness = (fuzz / 255.0).clamp(0.0, ReplaceColor::MAX_FUZZINESS);
                changed |=
                    design::slider_row(ui, tr("ui.adjustment.hue"), hue, -180.0..=180.0).changed();
                changed |=
                    design::slider_row(ui, tr("ui.adjustment.saturation"), saturation, -1.0..=1.0)
                        .changed();
                changed |=
                    design::slider_row(ui, tr("ui.adjustment.lightness"), lightness, -1.0..=1.0)
                        .changed();
            }
            K::ColorLookup { name, .. } => {
                let mut choice = self.lut_choice;
                design::inspector_field(ui, tr("ui.adjustment.lut"), |ui| {
                    combo(
                        ui,
                        ("adjustment", "lut"),
                        &mut choice,
                        &LUT_CHOICES,
                        lut_choice_label,
                        |_| None,
                    );
                });
                if choice != self.lut_choice {
                    lut_pick = Some(choice);
                }
                let load = design::secondary_button(ui, tr("ui.adjustment.lut.load"));
                // A stable id over the button, so the click can be found and
                // driven by a test; it takes the press the button would.
                let named = ui.interact(load.rect, lut_load_button_id(), egui::Sense::click());
                if load.clicked() || named.clicked() {
                    self.lut_file_requested = true;
                }
                if !name.is_empty() {
                    caption(ui, format!("{} {}", tr("ui.adjustment.lut.using"), name));
                }
                if let Some(error) = &self.lut_error {
                    caption(ui, format!("{} {}", tr("ui.adjustment.lut.error"), error));
                }
            }
        }

        if let Some((target, rgb)) = open_picker {
            self.color_edit.open(target, [rgb[0], rgb[1], rgb[2], 1.0]);
        }
        if let Some(choice) = lut_pick {
            // The combo's pick goes through the one path tests and hosts use.
            self.choose_lut(choice);
            return;
        }
        if let Some(wide) = promote {
            next = wide;
            changed = true;
        }
        if changed {
            self.set_kind(next);
        }
    }
}

/// Height of the Levels histogram well: a whole number of grid units, the way
/// every extent in [`super::sizes`] is. It lives here rather than there only
/// because it is drawn by exactly one dialog.
fn histogram_height() -> f32 {
    grid(20.0)
}

/// Three sliders over one tone range of Color Balance. Returns whether any
/// moved.
fn balance_sliders(ui: &mut egui::Ui, range: &mut [f32; 3]) -> bool {
    const KEYS: [&str; 3] = [
        "ui.adjustment.cyan.red",
        "ui.adjustment.magenta.green",
        "ui.adjustment.yellow.blue",
    ];
    let mut changed = false;
    for (amount, key) in range.iter_mut().zip(KEYS) {
        let mut percent = *amount * 100.0;
        changed |= design::slider_row(ui, tr(key), &mut percent, -100.0..=100.0).changed();
        *amount = percent / 100.0;
    }
    changed
}

/// The stored form of a lookup table.
fn lut_kind(lut: &Lut3d) -> AdjustmentKind {
    AdjustmentKind::ColorLookup {
        name: lut.name().to_string(),
        size: lut.size() as u32,
        table: lut.table().to_vec(),
    }
}

/// The encoded colour of `source` at `(x, y)`, or `None` off the buffer or
/// on a fully transparent pixel.
fn sample_encoded(source: &FilterBuffer, space: &ColorSpace, x: u32, y: u32) -> Option<[f32; 3]> {
    let (w, h) = source.dimensions();
    if x >= w || y >= h {
        return None;
    }
    let px = source.get(x, y);
    if px[3] <= color::UNPREMULTIPLY_ALPHA_EPSILON {
        return None;
    }
    let s = color::unpremultiply(px);
    Some(color::from_linear(space, [s[0], s[1], s[2]]).map(|c| c.clamp(0.0, 1.0)))
}

fn lut_choice_label(index: usize) -> String {
    tr(match index {
        1 => "ui.adjustment.lut.invert",
        2 => "ui.adjustment.lut.warm",
        3 => "ui.adjustment.lut.cool",
        4 => "ui.adjustment.lut.sepia",
        5 => "ui.adjustment.lut.high.contrast",
        LUT_LOADED => "ui.adjustment.lut.file",
        _ => "ui.adjustment.lut.none",
    })
    .to_string()
}

fn tone_label(index: usize) -> String {
    tr(match index {
        0 => "ui.adjustment.shadows",
        2 => "ui.adjustment.highlights",
        _ => "ui.adjustment.midtones",
    })
    .to_string()
}

fn channel_label(index: usize) -> String {
    tr(match index {
        0 => "ui.adjustment.red",
        1 => "ui.adjustment.green",
        _ => "ui.adjustment.blue",
    })
    .to_string()
}

fn selective_range_label(index: usize) -> String {
    const KEYS: [&str; 9] = [
        "ui.adjustment.reds",
        "ui.adjustment.yellows",
        "ui.adjustment.greens",
        "ui.adjustment.cyans",
        "ui.adjustment.blues",
        "ui.adjustment.magentas",
        "ui.adjustment.whites",
        "ui.adjustment.neutrals",
        "ui.adjustment.blacks",
    ];
    tr(KEYS[index.min(8)]).to_string()
}

fn curve_point_label(index: usize) -> &'static str {
    tr(match index {
        0 => "ui.adjustment.curve.0",
        1 => "ui.adjustment.curve.1",
        2 => "ui.adjustment.curve.2",
        3 => "ui.adjustment.curve.3",
        _ => "ui.adjustment.curve.4",
    })
}

/// The shape of `kind` this dialog edits.
///
/// Curves is the one whose stored form is open-ended — any number of points
/// anywhere — while the editor here is five sliders at [`CURVE_INPUTS`]. A
/// curve handed in is resampled at those inputs, so what the sliders show is
/// what the curve does there; the identity comes back as five knots on
/// `y = x`, which [`adjustments::Curve`] still recognises as the identity.
fn editable_kind(id: AdjustmentId, kind: AdjustmentKind) -> AdjustmentKind {
    match (id, kind) {
        (AdjustmentId::Curves, AdjustmentKind::Curves { points }) => {
            let curve = Curve::new(&points).unwrap_or_else(|_| Curve::identity());
            AdjustmentKind::Curves {
                points: CURVE_INPUTS
                    .iter()
                    .map(|x| [*x, curve.eval(*x).clamp(0.0, 1.0)])
                    .collect(),
            }
        }
        (_, kind) => kind,
    }
}

/// A box-downsampled copy of `source` no larger than `max_side` on a side.
///
/// Integer box averaging over the premultiplied linear pixels, so the proxy
/// keeps the layer's mean colour and coverage exactly. A buffer already inside
/// the bound is returned as it is.
pub fn preview_proxy(source: &FilterBuffer, max_side: u32) -> FilterBuffer {
    let (w, h) = source.dimensions();
    let longest = w.max(h);
    if w == 0 || h == 0 || longest <= max_side.max(1) {
        return source.clone();
    }
    let step = longest.div_ceil(max_side.max(1));
    let dw = (w / step).max(1);
    let dh = (h / step).max(1);
    let mut out = FilterBuffer::transparent(dw, dh).expect("a non-empty proxy");
    for y in 0..dh {
        for x in 0..dw {
            let mut sum = [0.0f32; 4];
            let mut n = 0.0f32;
            for sy in (y * step)..((y + 1) * step).min(h) {
                for sx in (x * step)..((x + 1) * step).min(w) {
                    let px = source.get(sx, sy);
                    for (acc, v) in sum.iter_mut().zip(px) {
                        *acc += v;
                    }
                    n += 1.0;
                }
            }
            if n > 0.0 {
                out.set(x, y, sum.map(|v| v / n));
            }
        }
    }
    out
}

impl Dialog for AdjustmentDialog {
    fn title(&self) -> &'static str {
        self.id.label()
    }

    fn confirm_label(&self) -> &'static str {
        tr("ui.adjustment.confirm")
    }

    fn confirm(&self) -> Option<DialogAction> {
        let invocation = self.invocation();
        // An adjustment layer may be set back to its identity: the layer
        // stays and simply stops changing pixels, as Photopea's does.
        (self.edit_layer.is_some() || invocation.is_valid())
            .then(|| DialogAction::Command(Box::new(invocation.layer_command(self.layer_id))))
    }

    fn blocked_reason(&self) -> Option<String> {
        (self.edit_layer.is_none() && !self.invocation().is_valid())
            .then(|| tr("ui.adjustment.blocked.identity").to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialogs::chrome::test_support::{frame_both_themes, Harness};

    fn sliders_moved(id: AdjustmentId) -> AdjustmentKind {
        crate::dialogs::tests_support::adjusted_kind(id)
    }

    #[test]
    fn every_adjustment_opens_at_its_identity_and_refuses_to_apply_it() {
        for id in AdjustmentId::ALL {
            let dialog = AdjustmentDialog::with_placeholder(*id);
            assert_eq!(dialog.id(), *id);
            let starts_identity = dialog.invocation().is_identity();
            // The five that are never the identity confirm straight away; the
            // ten that start there are blocked, with a reason.
            assert_eq!(
                dialog.confirm().is_some(),
                !starts_identity,
                "{id:?}: confirm() disagrees with the identity of its start"
            );
            assert_eq!(dialog.blocked_reason().is_some(), starts_identity, "{id:?}");
            // An analysis (Equalize) is never an identity *setting*; every
            // other adjustment's start is judged by `adjustments` itself.
            let start = Adjustment::from(&id.identity_kind());
            assert_eq!(
                !start.needs_stats() && PreparedAdjustment::new(&start).is_identity(),
                starts_identity,
                "{id:?}: the dialog and `adjustments` disagree about the start"
            );
        }
    }

    #[test]
    fn moving_a_control_changes_the_preview_and_unblocks_apply() {
        for id in AdjustmentId::ALL {
            let mut dialog = AdjustmentDialog::with_placeholder(*id);
            let untouched = dialog.source().to_rgba8();
            assert!(
                dialog.set_kind(sliders_moved(*id)),
                "{id:?} rejected its own kind"
            );
            let after = dialog.preview_buffer().to_rgba8();
            assert_ne!(
                untouched, after,
                "{id:?}: the moved control changed no preview pixel"
            );
            let action = dialog
                .confirm()
                .unwrap_or_else(|| panic!("{id:?} stayed blocked"));
            assert!(action.is_valid());
            assert!(dialog.blocked_reason().is_none());
        }
    }

    #[test]
    fn a_kind_of_another_adjustment_is_refused() {
        let mut dialog = AdjustmentDialog::with_placeholder(AdjustmentId::Threshold);
        assert!(!dialog.set_kind(AdjustmentKind::Posterize { levels: 8 }));
        assert_eq!(dialog.kind(), &AdjustmentKind::Threshold { level: 0.5 });
    }

    #[test]
    fn reset_returns_to_the_starting_parameters() {
        let mut dialog = AdjustmentDialog::with_placeholder(AdjustmentId::BrightnessContrast);
        dialog.set_kind(AdjustmentKind::BrightnessContrast {
            brightness: 0.5,
            contrast: -0.2,
        });
        dialog.reset();
        assert_eq!(
            dialog.kind(),
            &AdjustmentId::BrightnessContrast.identity_kind()
        );
    }

    #[test]
    fn threshold_at_a_fifth_differs_from_four_fifths() {
        let mut low = AdjustmentDialog::with_placeholder(AdjustmentId::Threshold);
        low.set_kind(AdjustmentKind::Threshold { level: 0.2 });
        let mut high = AdjustmentDialog::with_placeholder(AdjustmentId::Threshold);
        high.set_kind(AdjustmentKind::Threshold { level: 0.8 });
        assert_ne!(
            low.preview_buffer().to_rgba8(),
            high.preview_buffer().to_rgba8()
        );
    }

    #[test]
    fn only_levels_carries_a_histogram_and_it_counts_every_opaque_pixel() {
        let levels = AdjustmentDialog::with_placeholder(AdjustmentId::Levels);
        let bins = levels.histogram().expect("Levels has a histogram");
        let (w, h) = levels.source().dimensions();
        assert_eq!(
            bins.iter().map(|b| u64::from(*b)).sum::<u64>(),
            u64::from(w * h)
        );
        for id in AdjustmentId::ALL
            .iter()
            .filter(|id| **id != AdjustmentId::Levels)
        {
            assert!(
                AdjustmentDialog::with_placeholder(*id)
                    .histogram()
                    .is_none(),
                "{id:?} grew a histogram"
            );
        }
    }

    #[test]
    fn the_proxy_is_bounded_and_keeps_the_mean() {
        let mut big = FilterBuffer::transparent(400, 100).unwrap();
        for y in 0..100 {
            for x in 0..400 {
                let v = if x < 200 { 1.0 } else { 0.0 };
                big.set(x, y, [v, v, v, 1.0]);
            }
        }
        let proxy = preview_proxy(&big, MAX_PREVIEW_SIDE);
        let (w, h) = proxy.dimensions();
        assert!(w <= MAX_PREVIEW_SIDE && h <= MAX_PREVIEW_SIDE, "{w}x{h}");
        assert!(w > h, "the aspect was lost: {w}x{h}");
        let mean: f32 = proxy.pixels().iter().map(|p| p[0]).sum::<f32>() / (w * h) as f32;
        assert!((mean - 0.5).abs() < 0.02, "mean {mean}");
        // Inside the bound nothing is touched.
        let small = FilterBuffer::transparent(64, 64).unwrap();
        assert_eq!(preview_proxy(&small, MAX_PREVIEW_SIDE), small);
    }

    #[test]
    fn a_curve_is_resampled_at_the_five_inputs_and_the_identity_stays_one() {
        let dialog = AdjustmentDialog::with_placeholder(AdjustmentId::Curves);
        let AdjustmentKind::Curves { points } = dialog.kind() else {
            panic!("not curves");
        };
        assert_eq!(points.len(), CURVE_INPUTS.len());
        assert!(points.iter().all(|p| p[0] == p[1]), "{points:?}");
        assert!(dialog.invocation().is_identity());
    }

    #[test]
    fn the_nested_picker_writes_back_to_the_parameter_that_opened_it() {
        let mut dialog = AdjustmentDialog::with_placeholder(AdjustmentId::PhotoFilter);
        dialog.write_color(ColorTarget::PhotoFilter, [0.1, 0.2, 0.3, 1.0]);
        assert!(matches!(
            dialog.kind(),
            AdjustmentKind::PhotoFilter { color_srgb, .. } if *color_srgb == [0.1, 0.2, 0.3]
        ));
        let mut ramp = AdjustmentDialog::with_placeholder(AdjustmentId::GradientMap);
        ramp.write_color(ColorTarget::GradientStop(1), [0.9, 0.1, 0.1, 1.0]);
        assert!(matches!(
            ramp.kind(),
            AdjustmentKind::GradientMap { stops, .. } if stops[1].1 == [0.9, 0.1, 0.1]
        ));
        // A target the kind has no slot for changes nothing.
        let before = ramp.kind().clone();
        ramp.write_color(ColorTarget::PhotoFilter, [0.0; 4]);
        assert_eq!(ramp.kind(), &before);
    }

    #[test]
    fn every_adjustment_dialog_draws_in_both_themes_without_panicking() {
        for id in AdjustmentId::ALL {
            let mut dialog = AdjustmentDialog::with_placeholder(*id);
            frame_both_themes(|ctx| {
                assert!(dialog.show(ctx, None).is_open(), "{id:?} closed itself");
            });
            dialog.set_preview_enabled(false);
            frame_both_themes(|ctx| {
                assert!(dialog.show(ctx, None).is_open());
            });
        }
    }

    #[test]
    fn replace_color_draws_its_selection_preview_and_samples_from_a_click() {
        let harness = Harness::new();
        let mut dialog = AdjustmentDialog::with_placeholder(AdjustmentId::ReplaceColor);
        let mask = harness.settle(replace_color_mask_id(), |ctx| {
            let _ = dialog.show(ctx, None);
        });
        assert!(mask.width() > 0.0 && mask.height() > 0.0);
        let preview = harness.settle(ids::adjustment_preview(), |ctx| {
            let _ = dialog.show(ctx, None);
        });
        assert!(
            mask.top() >= preview.bottom(),
            "the selection preview {mask:?} is not below the image {preview:?}"
        );
        // The coverage is a real selection: the sampled pixel is fully in it
        // and something on the placeholder is out of it.
        let coverage = dialog.replace_color_coverage();
        assert_eq!(coverage.len(), {
            let (w, h) = dialog.source().dimensions();
            (w * h) as usize
        });
        assert!(
            coverage.contains(&255) && coverage.contains(&0),
            "{coverage:?}"
        );
        // A click on the preview's corner samples that pixel's colour.
        let before = dialog.kind().clone();
        let corner = preview.min + egui::vec2(1.0, 1.0);
        harness.frame(Harness::click_events(corner), |ctx| {
            let _ = dialog.show(ctx, None);
        });
        assert_ne!(dialog.kind(), &before, "the click sampled nothing");
        // No other adjustment draws the selection preview.
        let harness = Harness::new();
        let mut other = AdjustmentDialog::with_placeholder(AdjustmentId::HueSaturation);
        harness.settle(ids::adjustment_preview(), |ctx| {
            let _ = other.show(ctx, None);
        });
        assert!(!harness.was_drawn(replace_color_mask_id()));
    }

    #[test]
    fn color_lookup_asks_the_host_for_a_file_and_reports_a_bad_one() {
        let harness = Harness::new();
        let mut dialog = AdjustmentDialog::with_placeholder(AdjustmentId::ColorLookup);
        assert!(!dialog.take_lut_file_request());
        harness.click_widget(lut_load_button_id(), |ctx| {
            let _ = dialog.show(ctx, None);
        });
        assert!(
            dialog.take_lut_file_request(),
            "the click asked for no file"
        );
        assert!(!dialog.take_lut_file_request(), "the request is taken once");
        // A file that is not a cube leaves the table alone and says why.
        let before = dialog.kind().clone();
        assert!(dialog.load_cube_text("junk", "not a lut").is_err());
        assert_eq!(dialog.kind(), &before);
        assert!(dialog.invocation().is_identity());
        // A real one takes, previews and unblocks Apply.
        let cube = "LUT_3D_SIZE 2
1 1 1
0 1 1
1 0 1
0 0 1
1 1 0
0 1 0
1 0 0
0 0 0
";
        dialog.load_cube_text("invert", cube).unwrap();
        assert!(dialog.confirm().is_some());
        assert_ne!(
            dialog.preview_buffer().to_rgba8(),
            dialog.source().to_rgba8()
        );
    }

    /// The "Lookup table" combo is the only route to the built-in looks.
    /// Click the drawn combo, then the drawn row, and read the table the
    /// dialog now holds; then load a file and pick "None" through the same
    /// combo to return to identity.
    #[test]
    fn the_lookup_table_combo_picks_a_built_in_look_and_returns_to_none_after_a_file() {
        use crate::dialogs::chrome::test_support::Harness;
        let h = Harness::new();
        let mut dialog = AdjustmentDialog::with_placeholder(AdjustmentId::ColorLookup);
        let text_rect = |h: &Harness, dialog: &mut AdjustmentDialog, text: &str| {
            let mut found = None;
            for _ in 0..Harness::STABLE_FRAMES {
                let input = egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, Harness::SCREEN)),
                    ..Default::default()
                };
                let output = h.ctx.run(input, |ctx| {
                    let _ = dialog.show(ctx, None);
                });
                found = output
                    .shapes
                    .iter()
                    .find_map(|clipped| match &clipped.shape {
                        egui::Shape::Text(t) if t.galley.text() == text => {
                            Some(egui::Rect::from_min_size(t.pos, t.galley.size()))
                        }
                        _ => None,
                    });
            }
            found
        };
        let pick = |h: &Harness, dialog: &mut AdjustmentDialog, current: &str, row: &str| {
            let combo = text_rect(h, dialog, current)
                .unwrap_or_else(|| panic!("the combo does not show {current:?}"));
            h.frame(Harness::click_events(combo.center()), |ctx| {
                let _ = dialog.show(ctx, None);
            });
            let row_rect = text_rect(h, dialog, row)
                .unwrap_or_else(|| panic!("the open list draws no {row:?} row"));
            h.frame(Harness::click_events(row_rect.center()), |ctx| {
                let _ = dialog.show(ctx, None);
            });
        };
        let none = tr("ui.adjustment.lut.none").to_string();
        let sepia = tr("ui.adjustment.lut.sepia").to_string();
        assert!(dialog.invocation().is_identity());
        pick(&h, &mut dialog, &none, &sepia);
        assert_eq!(
            dialog.kind(),
            &lut_kind(&BuiltinLut::Sepia.lut()),
            "clicking the Sepia row did not choose the Sepia table"
        );
        assert!(dialog.confirm().is_some());
        // A loaded file shows as such, and "None" can then be picked.
        let cube = "LUT_3D_SIZE 2
1 1 1
0 1 1
1 0 1
0 0 1
1 1 0
0 1 0
1 0 0
0 0 0
";
        dialog.load_cube_text("invert", cube).unwrap();
        assert!(!dialog.invocation().is_identity());
        let loaded = tr("ui.adjustment.lut.file").to_string();
        pick(&h, &mut dialog, &loaded, &none);
        assert!(
            dialog.invocation().is_identity(),
            "picking None after a loaded file left the table in place"
        );
    }

    #[test]
    fn a_dialog_opened_on_a_color_lookup_layer_reopens_its_look_and_may_return_to_identity() {
        let layer = LayerId::new();
        let source = super::super::filter_dialog::placeholder_buffer(32, 32);
        let sepia = lut_kind(&BuiltinLut::Sepia.lut());
        let mut dialog = AdjustmentDialog::for_layer(
            layer,
            sepia.clone(),
            source.clone(),
            ColorSpace::default(),
        )
        .expect("Color Lookup has a dialog");
        assert_eq!(dialog.edit_layer(), Some(layer));
        assert_eq!(dialog.kind(), &sepia);
        // The listed choice follows the stored name: Sepia is the fourth.
        assert_eq!(dialog.lut_choice, 4);
        // Choosing another look swaps the table; choosing none is allowed on
        // a layer (it stops changing pixels) though Image > Adjustments
        // refuses to bake an identity.
        assert!(dialog.choose_lut(1));
        assert_eq!(dialog.kind(), &lut_kind(&BuiltinLut::Invert.lut()));
        assert!(dialog.choose_lut(0));
        assert!(dialog.invocation().is_identity());
        assert!(dialog.confirm().is_some());
        assert!(dialog.blocked_reason().is_none());
        assert!(!dialog.choose_lut(BuiltinLut::ALL.len() + 1));
        // A kind with no dialog opens none: Desaturate asks nothing.
        assert!(AdjustmentDialog::for_layer(
            layer,
            AdjustmentKind::Desaturate,
            source,
            ColorSpace::default()
        )
        .is_none());
        // Image > Adjustments' dialog has no layer and still refuses identity.
        let menu = AdjustmentDialog::with_placeholder(AdjustmentId::ColorLookup);
        assert_eq!(menu.edit_layer(), None);
        assert!(menu.confirm().is_none());
    }

    #[test]
    fn equalize_previews_against_the_layer_histogram_and_confirms() {
        let dialog = AdjustmentDialog::with_placeholder(AdjustmentId::Equalize);
        assert!(!dialog.invocation().is_identity());
        assert!(dialog.confirm().is_some());
        assert_ne!(
            dialog.preview_buffer().to_rgba8(),
            dialog.source().to_rgba8()
        );
    }

    #[test]
    fn the_drawn_levels_dialog_shows_its_histogram_and_the_others_do_not() {
        let harness = Harness::new();
        let mut levels = AdjustmentDialog::with_placeholder(AdjustmentId::Levels);
        let rect = harness.settle(ids::adjustment_histogram(), |ctx| {
            let _ = levels.show(ctx, None);
        });
        assert!(rect.width() > 0.0 && rect.height() > 0.0);
        assert!(harness.was_drawn(ids::adjustment_preview()));

        let harness = Harness::new();
        let mut bc = AdjustmentDialog::with_placeholder(AdjustmentId::BrightnessContrast);
        harness.settle(ids::adjustment_preview(), |ctx| {
            let _ = bc.show(ctx, None);
        });
        assert!(!harness.was_drawn(ids::adjustment_histogram()));
    }

    #[test]
    fn enter_applies_a_moved_adjustment_and_escape_backs_out() {
        let harness = Harness::new();
        let mut dialog = AdjustmentDialog::with_placeholder(AdjustmentId::Threshold);
        harness.settle(ids::adjustment_preview(), |ctx| {
            let _ = dialog.show(ctx, None);
        });
        let mut outcome = DialogOutcome::Open;
        harness.frame(Harness::key_events(egui::Key::Enter), |ctx| {
            outcome = dialog.show(ctx, None);
        });
        match std::mem::replace(&mut outcome, DialogOutcome::Open) {
            DialogOutcome::Confirmed(DialogAction::Command(command)) => match *command {
                Command::CreateLayer { layer } => assert!(matches!(
                    layer.kind,
                    LayerKind::Adjustment(AdjustmentLayer {
                        kind: AdjustmentKind::Threshold { level }
                    }) if level == 0.5
                )),
                other => panic!("confirm produced {other:?}"),
            },
            other => panic!("Enter produced {other:?}"),
        }
        let mut dialog = AdjustmentDialog::with_placeholder(AdjustmentId::Threshold);
        harness.frame(Harness::key_events(egui::Key::Escape), |ctx| {
            outcome = dialog.show(ctx, None);
        });
        assert_eq!(outcome, DialogOutcome::Cancelled);

        // An untouched Levels is blocked: Enter leaves it open.
        let mut levels = AdjustmentDialog::with_placeholder(AdjustmentId::Levels);
        harness.frame(Harness::key_events(egui::Key::Enter), |ctx| {
            outcome = levels.show(ctx, None);
        });
        assert!(
            outcome.is_open(),
            "an identity Levels confirmed to {outcome:?}"
        );
    }
}
