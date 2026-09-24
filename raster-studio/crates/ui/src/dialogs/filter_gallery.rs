//! Filter ▸ Filter Gallery — the catalogue of filters, and Photoshop's four
//! gallery sets as a stackable effect list (W10-D).
//!
//! The first tab is the whole filter catalogue: one row per [`FilterSpec`],
//! each with a thumbnail that is the real filter at its schema defaults on a
//! downscaled copy of the layer. Confirming a catalogue row runs the same
//! invocation the menu item would have run at its defaults.
//!
//! The other four tabs are Photoshop's gallery sets — Artistic, Brush
//! Strokes, Sketch and Texture ([`filters::gallery_sets`]). Clicking one of
//! their effects puts it on the *effect list*: into the selected list entry
//! when there is one (Photoshop's "the gallery edits the current effect"),
//! as the first entry otherwise. The list can take more entries (New), lose
//! them (Delete), be reordered (Up / Down) and have entries hidden (the
//! checkbox); the selected entry's parameters are the sliders. The preview
//! is the whole list applied, bottom to top, to a crop of the layer at 100%
//! — the same pixels the confirmed stack produces there, since the effects
//! are measured in pixels.
//!
//! Confirming with a non-empty list yields a [`FilterGallerySpec`], which the
//! shell parks for the `MenuAction::FilterGallery` arm; that arm applies the
//! whole list to the full-resolution layer as ONE undo step. Picking a
//! catalogue row clears the list, so what the gallery commits is always
//! either one catalogue filter or the effect list, never a mix.

use egui::Context;
use std::collections::HashMap;

use super::action::DialogAction;
use super::chrome::{
    action_row, modal, Dialog, DialogButton, DialogKeys, DialogOutcome, DialogWidth,
};
use super::sizes;
use crate::menu::FilterId;
use crate::strings::tr;
use filters::gallery_sets::{GalleryEffect, GalleryLayer, GallerySet, GalleryStack};
use filters::FilterBuffer;

/// The largest side of the 100% crop the effect list is previewed on.
pub const GALLERY_PREVIEW_SIDE: u32 = 256;

/// A confirmed effect list: what to apply, and the document size it was
/// built over.
#[derive(Clone, PartialEq, Debug)]
pub struct FilterGallerySpec {
    pub image_size: (u32, u32),
    pub stack: GalleryStack,
}

impl FilterGallerySpec {
    /// Whether applying the list changes nothing (no visible entry).
    pub fn is_identity(&self) -> bool {
        self.stack.is_identity()
    }

    /// The list applied to `src` (document-sized).
    pub fn apply(&self, src: &FilterBuffer) -> FilterBuffer {
        self.stack.apply(src)
    }
}

/// What a confirmed gallery commits.
#[derive(Clone, PartialEq, Debug)]
pub enum GalleryOutcome {
    /// One catalogue filter at its defaults — the menu item's invocation.
    Filter(DialogAction),
    /// The effect list, applied by the shell as one undo step.
    Stack(FilterGallerySpec),
}

/// Which folder the gallery shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GalleryTab {
    /// Every catalogue filter.
    Catalogue,
    /// One of Photoshop's gallery sets.
    Set(GallerySet),
}

/// Filter ▸ Filter Gallery.
pub struct FilterGalleryDialog {
    /// The layer's pixels, downscaled once to thumbnail size: every
    /// thumbnail is this proxy through a different filter.
    proxy: FilterBuffer,
    /// A crop from the middle of the layer at 100%, for the list preview.
    crop: FilterBuffer,
    image_size: (u32, u32),
    /// Index into [`super::filter_dialog::FILTERS`], defaulted to the first
    /// entry so the dialog can always confirm — a gallery that opens with
    /// nothing chosen cannot be committed, and the registry contract requires
    /// every dialog to be committable in its default state.
    selected: usize,
    tab: GalleryTab,
    stack: GalleryStack,
    /// The list entry the sliders edit.
    current: usize,
    thumbnails: HashMap<FilterId, egui::TextureHandle>,
    effect_thumbnails: HashMap<GalleryEffect, egui::TextureHandle>,
    preview: Option<egui::TextureHandle>,
    preview_dirty: bool,
    /// Where each effect tile was drawn last frame (screen points).
    effect_rects: Vec<(GalleryEffect, egui::Rect)>,
}

impl std::fmt::Debug for FilterGalleryDialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilterGalleryDialog")
            .field("selected", &self.selected)
            .field("tab", &self.tab)
            .field("stack", &self.stack)
            .field("thumbnails", &self.thumbnails.len())
            .finish_non_exhaustive()
    }
}

/// The central `side x side` (or smaller) crop of `src`, at 100%.
fn centre_crop(src: &FilterBuffer, side: u32) -> FilterBuffer {
    let (w, h) = src.dimensions();
    let (cw, ch) = (w.min(side), h.min(side));
    if (cw, ch) == (w, h) {
        return src.clone();
    }
    let (x0, y0) = ((w - cw) / 2, (h - ch) / 2);
    let mut px = Vec::with_capacity((cw * ch) as usize);
    for y in 0..ch {
        for x in 0..cw {
            px.push(src.get(x0 + x, y0 + y));
        }
    }
    FilterBuffer::from_pixels(cw, ch, px).unwrap_or_else(|_| src.clone())
}

impl FilterGalleryDialog {
    /// Open over the active layer's pixels.
    pub fn new(source: FilterBuffer) -> Self {
        Self {
            proxy: downscale_to_fit(&source, 48),
            crop: centre_crop(&source, GALLERY_PREVIEW_SIDE),
            image_size: source.dimensions(),
            selected: 0,
            tab: GalleryTab::Catalogue,
            stack: GalleryStack::default(),
            current: 0,
            thumbnails: HashMap::new(),
            effect_thumbnails: HashMap::new(),
            preview: None,
            preview_dirty: true,
            effect_rects: Vec::new(),
        }
    }

    /// The catalogue the gallery lists, in menu order.
    pub fn entries(&self) -> &'static [super::filter_dialog::FilterSpec] {
        super::filter_dialog::FILTERS
    }

    /// The filter a confirm would run right now while the list is empty.
    pub fn selected_filter(&self) -> &'static super::filter_dialog::FilterSpec {
        &super::filter_dialog::FILTERS[self.selected]
    }

    /// The folder on show.
    pub fn tab(&self) -> GalleryTab {
        self.tab
    }

    /// Show another folder.
    pub fn set_tab(&mut self, tab: GalleryTab) {
        self.tab = tab;
    }

    /// The effect list.
    pub fn stack(&self) -> &GalleryStack {
        &self.stack
    }

    /// The list entry the sliders edit, if the list is not empty.
    pub fn current(&self) -> Option<usize> {
        (!self.stack.layers.is_empty()).then_some(self.current)
    }

    /// Choose catalogue row `index`; the effect list is cleared, since the
    /// gallery commits one or the other.
    pub fn pick_catalogue(&mut self, index: usize) {
        if index < super::filter_dialog::FILTERS.len() {
            self.selected = index;
            self.stack.layers.clear();
            self.current = 0;
            self.preview_dirty = true;
        }
    }

    /// Click on a set effect: it replaces the selected list entry's effect
    /// (at its defaults), or becomes the first entry of an empty list.
    pub fn pick_effect(&mut self, effect: GalleryEffect) {
        match self.stack.layers.get_mut(self.current) {
            Some(layer) if layer.effect != effect => *layer = GalleryLayer::new(effect),
            Some(_) => {}
            None => {
                self.stack.layers.push(GalleryLayer::new(effect));
                self.current = self.stack.layers.len() - 1;
            }
        }
        self.preview_dirty = true;
    }

    /// New effect layer: a copy of the selected entry on top of it (or the
    /// folder's first effect on an empty list).
    pub fn new_layer(&mut self) {
        let layer = match self.stack.layers.get(self.current) {
            Some(layer) => layer.clone(),
            None => {
                let set = match self.tab {
                    GalleryTab::Set(set) => set,
                    GalleryTab::Catalogue => GallerySet::Artistic,
                };
                match set.effects().next() {
                    Some(effect) => GalleryLayer::new(effect),
                    None => return,
                }
            }
        };
        let at = if self.stack.layers.is_empty() {
            0
        } else {
            self.current + 1
        };
        self.stack.layers.insert(at, layer);
        self.current = at;
        self.preview_dirty = true;
    }

    /// Remove the selected entry.
    pub fn delete_layer(&mut self) {
        if self.current < self.stack.layers.len() {
            self.stack.layers.remove(self.current);
            self.current = self.current.min(self.stack.layers.len().saturating_sub(1));
            self.preview_dirty = true;
        }
    }

    /// Move the selected entry one step up (`true`, later in the list, so
    /// applied after) or down.
    pub fn move_layer(&mut self, up: bool) {
        let n = self.stack.layers.len();
        let to = if up {
            self.current + 1
        } else {
            self.current.wrapping_sub(1)
        };
        if self.current < n && to < n {
            self.stack.layers.swap(self.current, to);
            self.current = to;
            self.preview_dirty = true;
        }
    }

    /// Select list entry `index` for the sliders.
    pub fn select_layer(&mut self, index: usize) {
        if index < self.stack.layers.len() {
            self.current = index;
        }
    }

    /// Set parameter `param` of the selected entry.
    pub fn set_value(&mut self, param: usize, value: f32) {
        if let Some(layer) = self.stack.layers.get_mut(self.current) {
            if let Some(slot) = layer.values.get_mut(param) {
                *slot = value;
                self.preview_dirty = true;
            }
        }
    }

    /// Show or hide list entry `index`.
    pub fn set_visible(&mut self, index: usize, visible: bool) {
        if let Some(layer) = self.stack.layers.get_mut(index) {
            layer.visible = visible;
            self.preview_dirty = true;
        }
    }

    /// Where each effect's thumbnail tile was drawn last frame, clipped to
    /// the visible part of the folder's scroll area.
    pub fn effect_rect(&self, effect: GalleryEffect) -> Option<egui::Rect> {
        self.effect_rects
            .iter()
            .find(|(e, _)| *e == effect)
            .map(|(_, r)| *r)
    }

    /// The preview: the list over the 100% crop, or the selected catalogue
    /// filter at its defaults while the list is empty.
    pub fn preview_buffer(&self) -> FilterBuffer {
        if self.stack.layers.is_empty() {
            let spec = self.selected_filter();
            (spec.apply)(
                &self.crop,
                &super::filter_dialog::FilterParams::defaults(spec.params),
            )
        } else {
            self.stack.apply(&self.crop)
        }
    }

    /// What confirming commits right now.
    pub fn outcome(&self) -> Option<GalleryOutcome> {
        if self.stack.layers.is_empty() {
            return self.confirm().map(GalleryOutcome::Filter);
        }
        (!self.stack.is_identity()).then(|| {
            GalleryOutcome::Stack(FilterGallerySpec {
                image_size: self.image_size,
                stack: self.stack.clone(),
            })
        })
    }

    /// Fold the keyboard into an outcome.
    pub fn resolve_keys(&self, keys: DialogKeys) -> DialogOutcome<GalleryOutcome> {
        if keys.cancel {
            return DialogOutcome::Cancelled;
        }
        if keys.confirm {
            if let Some(outcome) = self.outcome() {
                return DialogOutcome::Confirmed(outcome);
            }
        }
        DialogOutcome::Open
    }

    /// Draw one frame and fold the keyboard and the action row into one
    /// outcome.
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<GalleryOutcome> {
        let mut outcome = self.resolve_keys(DialogKeys::read(ctx));
        let title = self.title();
        let drawn = modal(
            ctx,
            "filter-gallery",
            title,
            Some(tr("ui.filter_gallery.pick.a.filter")),
            DialogWidth::Broad,
            |ui| self.body(ctx, ui),
        );
        if let Some(Some(button)) = drawn {
            outcome = match button {
                DialogButton::Cancel => DialogOutcome::Cancelled,
                DialogButton::Confirm => self
                    .outcome()
                    .map_or(DialogOutcome::Open, DialogOutcome::Confirmed),
                DialogButton::Extra(_) => DialogOutcome::Open,
            };
        }
        outcome
    }

    fn body(&mut self, ctx: &Context, ui: &mut egui::Ui) -> Option<DialogButton> {
        // Folder tabs.
        ui.horizontal(|ui| {
            let catalogue = self.tab == GalleryTab::Catalogue;
            if ui
                .selectable_label(catalogue, tr("ui.filter_gallery.filter.gallery"))
                .clicked()
            {
                self.tab = GalleryTab::Catalogue;
            }
            for set in GallerySet::ALL {
                if ui
                    .selectable_label(self.tab == GalleryTab::Set(set), set.name())
                    .clicked()
                {
                    self.tab = GalleryTab::Set(set);
                }
            }
        });
        ui.add_space(design::tokens::Space::Small.pt());
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(sizes::sidebar_width());
                egui::ScrollArea::vertical()
                    .id_salt("filter-gallery-folder")
                    .max_height(sizes::list_max_height())
                    // The row beside it starts one line tall; without a
                    // floor the folder would open clipped to that line.
                    .min_scrolled_height(sizes::list_max_height())
                    .show(ui, |ui| self.folder(ctx, ui));
            });
            ui.vertical(|ui| {
                ui.set_width(sizes::params_column_width());
                self.preview_widget(ctx, ui);
                self.params(ui);
                ui.add_space(design::tokens::Space::Small.pt());
                self.list(ui);
            });
        });
        ui.add_space(design::tokens::Space::Small.pt());
        let summary = if self.stack.layers.is_empty() {
            let count = super::filter_dialog::FILTERS.len();
            let summary = self.selected_filter().summary;
            format!("{count} filters — {summary}")
        } else {
            let n = self.stack.layers.iter().filter(|l| l.visible).count();
            format!("{n} / {} effects", self.stack.layers.len())
        };
        ui.label(egui::RichText::new(summary));
        let blocked = self.blocked_reason();
        action_row(ui, tr("ui.adjustment.confirm"), blocked.as_deref(), &[])
    }

    fn folder(&mut self, ctx: &Context, ui: &mut egui::Ui) {
        match self.tab {
            GalleryTab::Catalogue => {
                let selected = self.selected;
                let empty = self.stack.layers.is_empty();
                for (index, spec) in super::filter_dialog::FILTERS.iter().enumerate() {
                    let texture = self.thumbnail_for(ctx, spec);
                    let clicked = ui
                        .horizontal(|ui| {
                            let image = ui.add(
                                egui::Image::new(&texture)
                                    .fit_to_exact_size(sizes::style_preview() * 0.5)
                                    .sense(egui::Sense::click()),
                            );
                            let row = design::list_row(ui, spec.name(), empty && selected == index);
                            image.clicked() || row.clicked()
                        })
                        .inner;
                    if clicked {
                        self.pick_catalogue(index);
                    }
                }
            }
            GalleryTab::Set(set) => {
                self.effect_rects.clear();
                let current = self.stack.layers.get(self.current).map(|l| l.effect);
                for effect in set.effects() {
                    let texture = self.effect_thumbnail_for(ctx, effect);
                    let response = ui.horizontal(|ui| {
                        let image = ui.add(
                            egui::Image::new(&texture)
                                .fit_to_exact_size(sizes::style_preview() * 0.5)
                                .sense(egui::Sense::click()),
                        );
                        let row = design::list_row(ui, effect.name(), current == Some(effect));
                        let drawn = image.rect.intersect(ui.clip_rect());
                        (image.clicked() || row.clicked(), drawn)
                    });
                    let (clicked, rect) = response.inner;
                    self.effect_rects.push((effect, rect));
                    if clicked {
                        self.pick_effect(effect);
                    }
                }
            }
        }
    }

    fn preview_widget(&mut self, ctx: &Context, ui: &mut egui::Ui) {
        if self.preview.is_none() || self.preview_dirty {
            let buffer = self.preview_buffer();
            let (w, h) = buffer.dimensions();
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [w as usize, h as usize],
                &buffer.to_rgba8(),
            );
            self.preview = Some(ctx.load_texture(
                "filter-gallery-preview",
                image,
                egui::TextureOptions::NEAREST,
            ));
            self.preview_dirty = false;
        }
        if let Some(texture) = &self.preview {
            let side = sizes::filter_preview_width();
            let (w, h) = self.crop.dimensions();
            let k = side / w.max(h).max(1) as f32;
            ui.add(
                egui::Image::new(texture)
                    .fit_to_exact_size(egui::Vec2::new(w as f32 * k, h as f32 * k)),
            );
        }
    }

    fn params(&mut self, ui: &mut egui::Ui) {
        let Some(layer) = self.stack.layers.get(self.current).cloned() else {
            return;
        };
        ui.label(egui::RichText::new(layer.effect.name()).strong());
        let mut changed = None;
        egui::Grid::new("filter-gallery-params")
            .num_columns(2)
            .show(ui, |ui| {
                for (i, param) in layer.effect.params().iter().enumerate() {
                    let mut value = layer.values.get(i).copied().unwrap_or(param.default);
                    ui.label(param.label);
                    if ui
                        .add(egui::Slider::new(&mut value, param.min..=param.max).step_by(1.0))
                        .changed()
                    {
                        changed = Some((i, value));
                    }
                    ui.end_row();
                }
            });
        if let Some((i, value)) = changed {
            self.set_value(i, value);
        }
    }

    fn list(&mut self, ui: &mut egui::Ui) {
        // Top of the list is the last-applied effect, as in Photoshop.
        let mut select = None;
        let mut visibility = None;
        for index in (0..self.stack.layers.len()).rev() {
            let layer = &self.stack.layers[index];
            ui.horizontal(|ui| {
                let mut visible = layer.visible;
                if ui.checkbox(&mut visible, "").changed() {
                    visibility = Some((index, visible));
                }
                if design::list_row(ui, layer.effect.name(), index == self.current).clicked() {
                    select = Some(index);
                }
            });
        }
        if let Some(index) = select {
            self.select_layer(index);
        }
        if let Some((index, visible)) = visibility {
            self.set_visible(index, visible);
        }
        ui.horizontal(|ui| {
            if ui.button("New").clicked() {
                self.new_layer();
            }
            let any = !self.stack.layers.is_empty();
            if ui.add_enabled(any, egui::Button::new("Delete")).clicked() {
                self.delete_layer();
            }
            if ui.add_enabled(any, egui::Button::new("Up")).clicked() {
                self.move_layer(true);
            }
            if ui.add_enabled(any, egui::Button::new("Down")).clicked() {
                self.move_layer(false);
            }
        });
    }

    /// One row's thumbnail: the real filter at its schema defaults, applied
    /// to the downscaled source, rendered once and cached.
    fn thumbnail_for(
        &mut self,
        ctx: &Context,
        spec: &'static super::filter_dialog::FilterSpec,
    ) -> egui::TextureHandle {
        if let Some(texture) = self.thumbnails.get(&spec.id) {
            return texture.clone();
        }
        let filtered = (spec.apply)(
            &self.proxy,
            &super::filter_dialog::FilterParams::defaults(spec.params),
        );
        let texture = upload(ctx, format!("filter-gallery-{:?}", spec.id), &filtered);
        self.thumbnails.insert(spec.id, texture.clone());
        texture
    }

    /// A set effect's thumbnail at its defaults, rendered once and cached.
    fn effect_thumbnail_for(
        &mut self,
        ctx: &Context,
        effect: GalleryEffect,
    ) -> egui::TextureHandle {
        if let Some(texture) = self.effect_thumbnails.get(&effect) {
            return texture.clone();
        }
        let filtered = effect.apply(&self.proxy, &effect.defaults());
        let texture = upload(ctx, format!("filter-gallery-set-{effect:?}"), &filtered);
        self.effect_thumbnails.insert(effect, texture.clone());
        texture
    }
}

fn upload(ctx: &Context, name: String, buffer: &FilterBuffer) -> egui::TextureHandle {
    let (w, h) = buffer.dimensions();
    let image =
        egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &buffer.to_rgba8());
    ctx.load_texture(name, image, egui::TextureOptions::NEAREST)
}

/// A box-averaged copy of `source`, the longer side clamped to `max_edge`.
fn downscale_to_fit(source: &FilterBuffer, max_edge: u32) -> FilterBuffer {
    let (w, h) = source.dimensions();
    if w == 0 || h == 0 {
        return source.clone();
    }
    let scale = (max_edge as f32 / w.max(h) as f32).min(1.0);
    let (dw, dh) = (
        ((w as f32 * scale).round() as u32).max(1),
        ((h as f32 * scale).round() as u32).max(1),
    );
    if (dw, dh) == (w, h) {
        return source.clone();
    }
    let rgba = source.to_rgba8();
    let mut out = vec![0u8; (dw * dh * 4) as usize];
    let (fw, fh) = (f64::from(w) / f64::from(dw), f64::from(h) / f64::from(dh));
    for dy in 0..dh {
        let y0 = (f64::from(dy) * fh).floor() as u32;
        let y1 = ((f64::from(dy + 1) * fh).ceil() as u32).clamp(y0 + 1, h);
        for dx in 0..dw {
            let x0 = (f64::from(dx) * fw).floor() as u32;
            let x1 = ((f64::from(dx + 1) * fw).ceil() as u32).clamp(x0 + 1, w);
            let mut acc = [0u64; 4];
            for y in y0..y1 {
                for x in x0..x1 {
                    let s = ((y * w + x) * 4) as usize;
                    for c in 0..4 {
                        acc[c] += u64::from(rgba[s + c]);
                    }
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as u64;
            let d = ((dy * dw + dx) * 4) as usize;
            for c in 0..4 {
                out[d + c] = (acc[c] / n) as u8;
            }
        }
    }
    FilterBuffer::from_rgba8(dw, dh, &out).expect("the downscaled buffer matches its size")
}

impl Dialog for FilterGalleryDialog {
    fn title(&self) -> &'static str {
        tr("ui.filter_gallery.filter.gallery")
    }

    fn confirm_label(&self) -> &'static str {
        tr("ui.adjustment.confirm")
    }

    /// The catalogue road: the selected filter at its defaults. With a
    /// non-empty effect list the gallery commits through [`Self::outcome`]
    /// instead (a stack is not a [`DialogAction`]), so this answers `None`.
    fn confirm(&self) -> Option<DialogAction> {
        if !self.stack.layers.is_empty() {
            return None;
        }
        let spec = self.selected_filter();
        Some(DialogAction::RunFilter(Box::new(
            super::filter_dialog::FilterInvocation {
                filter: spec,
                params: super::filter_dialog::FilterParams::defaults(spec.params),
            },
        )))
    }

    fn blocked_reason(&self) -> Option<String> {
        (!self.stack.layers.is_empty() && self.stack.is_identity())
            .then(|| tr("ui.adjustment.blocked.identity").to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::super::chrome::resolve;
    use super::*;
    use crate::dialogs::filter_dialog::{filter_by_id, FilterId};

    fn source() -> FilterBuffer {
        filters::FilterBuffer::from_rgba8(
            32,
            32,
            &(0..32 * 32)
                .flat_map(|i| {
                    [
                        (i % 255) as u8,
                        (i % 131) as u8,
                        ((i * 7) % 255) as u8,
                        255u8,
                    ]
                })
                .collect::<Vec<u8>>(),
        )
        .unwrap()
    }

    #[test]
    fn the_gallery_lists_exactly_the_menu_catalogue() {
        let gallery = FilterGalleryDialog::new(source());
        assert_eq!(
            gallery.entries().len(),
            FilterId::ALL.len(),
            "the gallery and the menu disagree about the catalogue"
        );
    }

    #[test]
    fn confirming_runs_the_selected_filter_at_its_defaults() {
        let mut gallery = FilterGalleryDialog::new(source());
        // Default selection is the first entry; move to Gaussian Blur.
        let index = gallery
            .entries()
            .iter()
            .position(|s| s.id == FilterId::GaussianBlur)
            .unwrap();
        gallery.pick_catalogue(index);
        let action = gallery.confirm().expect("a selection always confirms");
        match action {
            DialogAction::RunFilter(invocation) => {
                let invocation = *invocation;
                assert_eq!(invocation.filter.id, FilterId::GaussianBlur);
                // The same invocation the menu item produces at defaults.
                let via_menu = {
                    let spec = filter_by_id(FilterId::GaussianBlur).unwrap();
                    super::super::filter_dialog::FilterInvocation {
                        filter: spec,
                        params: super::super::filter_dialog::FilterParams::defaults(spec.params),
                    }
                };
                assert_eq!(invocation, via_menu, "the gallery is a second engine");
            }
            other => panic!("the gallery confirmed to {other:?}"),
        }
        assert!(matches!(
            gallery.resolve_keys(DialogKeys::CONFIRM),
            DialogOutcome::Confirmed(GalleryOutcome::Filter(DialogAction::RunFilter(_)))
        ));
    }

    #[test]
    fn resolve_confirms_on_enter_and_cancels_on_escape() {
        let dialog = FilterGalleryDialog::new(source());
        assert!(matches!(
            resolve(&dialog, DialogKeys::CONFIRM),
            DialogOutcome::Confirmed(_)
        ));
        assert!(matches!(
            resolve(&dialog, DialogKeys::CANCEL),
            DialogOutcome::Cancelled
        ));
    }

    #[test]
    fn it_draws_in_both_appearances() {
        super::super::chrome::test_support::frame_both_themes(|ctx| {
            let mut gallery = FilterGalleryDialog::new(source());
            assert!(gallery.show(ctx).is_open());
            gallery.set_tab(GalleryTab::Set(GallerySet::Sketch));
            gallery.pick_effect(GalleryEffect::Chrome);
            assert!(gallery.show(ctx).is_open());
        });
    }

    #[test]
    fn the_effect_list_stacks_edits_and_confirms_as_one_spec() {
        let src = source();
        let mut gallery = FilterGalleryDialog::new(src.clone());
        gallery.set_tab(GalleryTab::Set(GallerySet::Artistic));
        gallery.pick_effect(GalleryEffect::Cutout);
        // Clicking another effect edits the current entry, like Photoshop.
        gallery.pick_effect(GalleryEffect::Fresco);
        assert_eq!(gallery.stack().layers.len(), 1);
        assert_eq!(gallery.stack().layers[0].effect, GalleryEffect::Fresco);
        // New effect layer, then change it.
        gallery.new_layer();
        gallery.pick_effect(GalleryEffect::Grain);
        gallery.set_value(0, 90.0);
        assert_eq!(gallery.current(), Some(1));
        let stack = gallery.stack().clone();
        assert_eq!(stack.layers.len(), 2);
        assert_eq!(stack.layers[1].values[0], 90.0);
        // The preview is the whole list over the crop.
        assert_eq!(gallery.preview_buffer(), stack.apply(&src));
        match gallery.resolve_keys(DialogKeys::CONFIRM) {
            DialogOutcome::Confirmed(GalleryOutcome::Stack(spec)) => {
                assert_eq!(spec.image_size, (32, 32));
                assert_eq!(spec.stack, stack);
                assert_eq!(spec.apply(&src), stack.apply(&src));
            }
            other => panic!("Enter did not confirm the list: {other:?}"),
        }
        // Reorder and hide.
        gallery.move_layer(false);
        assert_eq!(gallery.stack().layers[0].effect, GalleryEffect::Grain);
        gallery.set_visible(0, false);
        gallery.set_visible(1, false);
        assert!(gallery.resolve_keys(DialogKeys::CONFIRM).is_open());
        assert!(gallery.blocked_reason().is_some());
        // A catalogue pick clears the list.
        gallery.pick_catalogue(0);
        assert!(gallery.stack().layers.is_empty());
        gallery.pick_effect(GalleryEffect::Stamp);
        gallery.delete_layer();
        assert!(gallery.stack().layers.is_empty());
    }

    #[test]
    fn the_preview_crop_is_at_full_resolution() {
        let big = FilterBuffer::filled(600, 300, [0.2, 0.4, 0.6, 1.0]).unwrap();
        let gallery = FilterGalleryDialog::new(big);
        assert_eq!(
            gallery.crop.dimensions(),
            (GALLERY_PREVIEW_SIDE, GALLERY_PREVIEW_SIDE)
        );
    }

    #[test]
    fn clicking_a_drawn_effect_tile_puts_it_on_the_list() {
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let mut gallery = FilterGalleryDialog::new(source());
        gallery.set_tab(GalleryTab::Set(GallerySet::Texture));
        let run = |gallery: &mut FilterGalleryDialog, events: Vec<egui::Event>| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::Vec2::new(1600.0, 1000.0),
                )),
                events,
                ..Default::default()
            };
            let mut out = DialogOutcome::Open;
            let _ = ctx.run(input, |ctx| out = gallery.show(ctx));
            out
        };
        for _ in 0..4 {
            run(&mut gallery, Vec::new());
        }
        let rect = gallery
            .effect_rect(GalleryEffect::MosaicTiles)
            .expect("the Texture folder drew Mosaic Tiles");
        let at = rect.center();
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        run(
            &mut gallery,
            vec![egui::Event::PointerMoved(at), button(true)],
        );
        run(&mut gallery, vec![button(false)]);
        assert_eq!(
            gallery.stack().layers.first().map(|l| l.effect),
            Some(GalleryEffect::MosaicTiles),
            "the click reached the tile"
        );
    }
}
