//! Filter ▸ Filter Gallery — every filter in one scrolling sheet.
//!
//! One row per [`FilterSpec`], each with a thumbnail that is the real filter
//! applied at its schema defaults to a downscaled copy of the layer the
//! gallery was opened on. Selecting a row and confirming runs the same
//! invocation the menu item would have run at its defaults — the gallery is a
//! browser over the same catalogue, not a second filter engine.

use egui::Context;
use std::collections::HashMap;

use super::action::DialogAction;
use super::chrome::{
    action_row, modal, resolve, Dialog, DialogButton, DialogKeys, DialogOutcome, DialogWidth,
};
use super::sizes;
use crate::menu::FilterId;
use filters::FilterBuffer;

/// Filter ▸ Filter Gallery.
pub struct FilterGalleryDialog {
    /// The layer's pixels, downscaled once to thumbnail size: every
    /// thumbnail is this proxy through a different filter.
    proxy: FilterBuffer,
    /// Index into [`super::filter_dialog::FILTERS`], defaulted to the first
    /// entry so the dialog can always confirm — a gallery that opens with
    /// nothing chosen cannot be committed, and the registry contract requires
    /// every dialog to be committable in its default state.
    selected: usize,
    thumbnails: HashMap<FilterId, egui::TextureHandle>,
}

impl std::fmt::Debug for FilterGalleryDialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilterGalleryDialog")
            .field("selected", &self.selected)
            .field("thumbnails", &self.thumbnails.len())
            .finish()
    }
}

impl FilterGalleryDialog {
    /// Open over the active layer's pixels.
    pub fn new(source: FilterBuffer) -> Self {
        Self {
            proxy: downscale_to_fit(&source, 48),
            selected: 0,
            thumbnails: HashMap::new(),
        }
    }

    /// The catalogue the gallery lists, in menu order.
    pub fn entries(&self) -> &'static [super::filter_dialog::FilterSpec] {
        super::filter_dialog::FILTERS
    }

    /// The filter a confirm would run right now.
    pub fn selected_filter(&self) -> &'static super::filter_dialog::FilterSpec {
        &super::filter_dialog::FILTERS[self.selected]
    }

    /// Draw one frame and fold the keyboard and the action row into one
    /// outcome.
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<DialogAction> {
        let keys = DialogKeys::read(ctx);
        let mut outcome = resolve(self, keys);
        let confirm_label = self.confirm_label();
        let blocked = self.blocked_reason();
        let count = super::filter_dialog::FILTERS.len();
        let selected = self.selected;
        let summary = self.selected_filter().summary;
        let drawn = modal(
            ctx,
            "filter-gallery",
            self.title(),
            Some(crate::strings::tr("ui.filter_gallery.pick.a.filter")),
            DialogWidth::Wide,
            |ui| {
                egui::ScrollArea::vertical()
                    .max_height(sizes::list_max_height())
                    .show(ui, |ui| {
                        for (index, spec) in super::filter_dialog::FILTERS.iter().enumerate() {
                            let texture = self.thumbnail_for(ctx, spec);
                            let clicked = ui
                                .horizontal(|ui| {
                                    let image = ui.add(
                                        egui::Image::new(&texture)
                                            .fit_to_exact_size(sizes::style_preview()),
                                    );
                                    let row = design::list_row(ui, spec.name(), selected == index);
                                    image.clicked() || row.clicked()
                                })
                                .inner;
                            if clicked {
                                self.selected = index;
                            }
                        }
                    });
                ui.add_space(design::tokens::Space::Small.pt());
                ui.label(egui::RichText::new(format!("{count} filters — {summary}")));
                action_row(ui, confirm_label, blocked.as_deref(), &[])
            },
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
        let (w, h) = filtered.dimensions();
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [w as usize, h as usize],
            &filtered.to_rgba8(),
        );
        let texture = ctx.load_texture(
            format!("filter-gallery-{:?}", spec.id),
            image,
            egui::TextureOptions::NEAREST,
        );
        self.thumbnails.insert(spec.id, texture.clone());
        texture
    }
}

/// A box-averaged copy of `source`, the longer side clamped to `max_edge`.
fn downscale_to_fit(source: &FilterBuffer, max_edge: u32) -> FilterBuffer {
    let (w, h) = source.dimensions();
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
        crate::strings::tr("ui.filter_gallery.filter.gallery")
    }

    fn confirm_label(&self) -> &'static str {
        "Apply"
    }

    fn confirm(&self) -> Option<DialogAction> {
        let spec = self.selected_filter();
        Some(DialogAction::RunFilter(Box::new(
            super::filter_dialog::FilterInvocation {
                filter: spec,
                params: super::filter_dialog::FilterParams::defaults(spec.params),
            },
        )))
    }

    fn blocked_reason(&self) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
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
        gallery.selected = index;
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
        });
    }
}
