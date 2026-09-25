//! W13X-7: File > Open of a PDF (or a PDF-compatible `.ai`) with two or more
//! pages asks, as Photopea's PDF import does, **which pages** (a thumbnail
//! per page, each one a toggle, with Select All / Select None), at **what
//! resolution** (dots per inch; 72 is one pixel per point) and **how they
//! open**: as artboards in one document, or each as a document of its own.
//!
//! The dialog hands back a [`PdfImportSpec`] and nothing else; the pages are
//! rendered by the application (`app-shell`'s open route) once it is
//! confirmed. Like Trim, there is no [`super::chrome::Dialog`] impl: the
//! shell parks the confirmed spec for the open route. `show` folds Escape,
//! Enter and the action row into one [`DialogOutcome`], so the keyboard
//! contract is every other dialog's.

use design::tokens::grid;
use egui::{Context, TextureHandle};
use raster::codec::formats::pdf::{DEFAULT_DPI, MAX_DPI, MIN_DPI};

use super::chrome::{
    action_row, caption, modal, warning, DialogButton, DialogKeys, DialogOutcome, DialogWidth,
};
use super::controls::{checkbox_row, combo, integer};
use crate::strings::tr;

/// How the chosen pages open.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum PdfOpenMode {
    /// One document, one artboard per page (Photopea's default).
    #[default]
    Artboards,
    /// One document per page.
    SeparateDocuments,
}

impl PdfOpenMode {
    pub const ALL: [PdfOpenMode; 2] = [PdfOpenMode::Artboards, PdfOpenMode::SeparateDocuments];

    pub fn label(self) -> &'static str {
        match self {
            PdfOpenMode::Artboards => tr("ui.pdf_import.mode.artboards"),
            PdfOpenMode::SeparateDocuments => tr("ui.pdf_import.mode.separate"),
        }
    }
}

/// The confirmed import: pages (from 0, in file order), resolution, mode.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PdfImportSpec {
    pub pages: Vec<usize>,
    pub dpi: u32,
    pub mode: PdfOpenMode,
}

/// One page as the dialog lists it.
#[derive(Clone, PartialEq, Debug)]
pub struct PdfPageThumb {
    /// The page's size in points.
    pub width_pt: f32,
    pub height_pt: f32,
    /// The thumbnail, RGBA8 straight alpha.
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// The side of the square each thumbnail is fitted into, in points.
fn thumb_box() -> f32 {
    grid(24.0)
}

/// File > Open's PDF import question.
pub struct PdfImportDialog {
    file_name: String,
    /// Every page the file has; more than `pages.len()` when only the first
    /// ones are listed.
    total: usize,
    pages: Vec<PdfPageThumb>,
    selected: Vec<bool>,
    dpi: u32,
    mode: PdfOpenMode,
    textures: Vec<Option<TextureHandle>>,
}

impl std::fmt::Debug for PdfImportDialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PdfImportDialog")
            .field("file_name", &self.file_name)
            .field("total", &self.total)
            .field("selected", &self.selected)
            .field("dpi", &self.dpi)
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

impl PdfImportDialog {
    /// Every listed page chosen, at [`DEFAULT_DPI`], as artboards.
    pub fn new(file_name: impl Into<String>, total: usize, pages: Vec<PdfPageThumb>) -> Self {
        let n = pages.len();
        Self {
            file_name: file_name.into(),
            total: total.max(n),
            pages,
            selected: vec![true; n],
            dpi: DEFAULT_DPI,
            mode: PdfOpenMode::default(),
            textures: vec![None; n],
        }
    }

    /// The id of page `index`'s tile: clicking it toggles the page.
    pub fn page_id(index: usize) -> egui::Id {
        egui::Id::new(("dialogs", "pdf-import-page", index))
    }

    pub fn title(&self) -> &'static str {
        tr("ui.pdf_import.title")
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    pub fn is_selected(&self, index: usize) -> bool {
        self.selected.get(index).copied().unwrap_or(false)
    }

    /// Choose or drop page `index` (ignored past the list).
    pub fn select(&mut self, index: usize, on: bool) {
        if let Some(s) = self.selected.get_mut(index) {
            *s = on;
        }
    }

    pub fn select_all(&mut self, on: bool) {
        self.selected.iter_mut().for_each(|s| *s = on);
    }

    pub fn dpi(&self) -> u32 {
        self.dpi
    }

    /// Set the resolution — tests and presets. Not clamped: an out-of-range
    /// value blocks the primary action with a reason.
    pub fn set_dpi(&mut self, dpi: u32) {
        self.dpi = dpi;
    }

    pub fn mode(&self) -> PdfOpenMode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: PdfOpenMode) {
        self.mode = mode;
    }

    /// How many thumbnails have been uploaded as textures (drawn at least
    /// once).
    pub fn thumbnails_loaded(&self) -> usize {
        self.textures.iter().filter(|t| t.is_some()).count()
    }

    /// Page `index` in pixels at the chosen resolution.
    pub fn pixel_size(&self, index: usize) -> Option<(u32, u32)> {
        let page = self.pages.get(index)?;
        let scale = self.dpi as f32 / 72.0;
        Some((
            (page.width_pt * scale).floor() as u32,
            (page.height_pt * scale).floor() as u32,
        ))
    }

    fn chosen(&self) -> Vec<usize> {
        (0..self.selected.len())
            .filter(|i| self.selected[*i])
            .collect()
    }

    /// Why the primary action is unavailable, or `None` when it is live.
    pub fn blocked_reason(&self) -> Option<String> {
        if self.chosen().is_empty() {
            return Some(tr("ui.pdf_import.none").to_string());
        }
        if !(MIN_DPI..=MAX_DPI).contains(&self.dpi) {
            return Some(
                tr("ui.pdf_import.dpi_range")
                    .replace("{min}", &MIN_DPI.to_string())
                    .replace("{max}", &MAX_DPI.to_string()),
            );
        }
        None
    }

    pub fn confirm(&self) -> Option<PdfImportSpec> {
        self.blocked_reason().is_none().then(|| PdfImportSpec {
            pages: self.chosen(),
            dpi: self.dpi,
            mode: self.mode,
        })
    }

    /// Escape and Enter, without drawing.
    pub fn resolve(&self, keys: DialogKeys) -> DialogOutcome<PdfImportSpec> {
        if keys.cancel {
            return DialogOutcome::Cancelled;
        }
        match self.confirm() {
            Some(spec) if keys.confirm => DialogOutcome::Confirmed(spec),
            _ => DialogOutcome::Open,
        }
    }

    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<PdfImportSpec> {
        let keys = DialogKeys::read(ctx);
        self.upload(ctx);
        let title = self.title();
        let drawn = modal(
            ctx,
            "pdf-import",
            title,
            Some(tr("ui.pdf_import.subtitle")),
            DialogWidth::Wide,
            |ui| self.body(ui),
        );
        let mut out = self.resolve(keys);
        match drawn.flatten() {
            Some(DialogButton::Cancel) => out = DialogOutcome::Cancelled,
            Some(DialogButton::Confirm) => {
                if let Some(spec) = self.confirm() {
                    out = DialogOutcome::Confirmed(spec);
                }
            }
            Some(DialogButton::Extra(0)) => self.select_all(true),
            Some(DialogButton::Extra(_)) => self.select_all(false),
            None => {}
        }
        out
    }

    /// Upload each thumbnail once.
    fn upload(&mut self, ctx: &Context) {
        for (i, page) in self.pages.iter().enumerate() {
            if self.textures[i].is_some() || page.width == 0 || page.height == 0 {
                continue;
            }
            if page.rgba.len() != page.width as usize * page.height as usize * 4 {
                continue;
            }
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [page.width as usize, page.height as usize],
                &page.rgba,
            );
            self.textures[i] = Some(ctx.load_texture(
                format!("pdf-import-page-{i}"),
                image,
                egui::TextureOptions::LINEAR,
            ));
        }
    }

    fn body(&mut self, ui: &mut egui::Ui) -> Option<DialogButton> {
        caption(
            ui,
            tr("ui.pdf_import.file")
                .replace("{name}", &self.file_name)
                .replace("{n}", &self.total.to_string()),
        );
        if self.total > self.pages.len() {
            caption(
                ui,
                tr("ui.pdf_import.listed")
                    .replace("{shown}", &self.pages.len().to_string())
                    .replace("{n}", &self.total.to_string()),
            );
        }
        design::section_header(ui, tr("ui.pdf_import.pages"));
        let side = thumb_box();
        egui::ScrollArea::vertical()
            .max_height(super::sizes::list_max_height())
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for i in 0..self.pages.len() {
                        ui.vertical(|ui| {
                            ui.set_width(side);
                            self.tile(ui, i, side);
                        });
                    }
                });
            });
        design::section_header(ui, tr("ui.pdf_import.resolution"));
        let mut dpi = i64::from(self.dpi);
        ui.horizontal(|ui| {
            integer(ui, &mut dpi, i64::from(MIN_DPI)..=i64::from(MAX_DPI));
            ui.label(tr("ui.pdf_import.dpi"));
        });
        self.dpi = dpi.clamp(i64::from(MIN_DPI), i64::from(MAX_DPI)) as u32;
        if let Some(first) = self.chosen().first().copied() {
            if let Some((w, h)) = self.pixel_size(first) {
                caption(
                    ui,
                    tr("ui.pdf_import.size")
                        .replace("{page}", &(first + 1).to_string())
                        .replace("{w}", &w.to_string())
                        .replace("{h}", &h.to_string()),
                );
            }
        }
        design::section_header(ui, tr("ui.pdf_import.open_as"));
        combo(
            ui,
            egui::Id::new(("dialogs", "pdf-import-mode")),
            &mut self.mode,
            &PdfOpenMode::ALL,
            |m| m.label().to_string(),
            |_| None,
        );
        let blocked = self.blocked_reason();
        if let Some(reason) = &blocked {
            warning(ui, reason.clone());
        }
        action_row(
            ui,
            tr("ui.pdf_import.ok"),
            blocked.as_deref(),
            &[tr("ui.pdf_import.all"), tr("ui.pdf_import.none_button")],
        )
    }

    /// Page `i`: its thumbnail (a toggle) and its checkbox.
    fn tile(&mut self, ui: &mut egui::Ui, i: usize, side: f32) {
        let rect = ui
            .allocate_exact_size(egui::vec2(side, side), egui::Sense::hover())
            .0;
        let response = ui.interact(rect, Self::page_id(i), egui::Sense::click());
        if let Some(texture) = &self.textures[i] {
            let size = texture.size_vec2();
            let scale = side / size.x.max(size.y).max(1.0);
            let fitted = egui::Rect::from_center_size(rect.center(), size * scale);
            egui::Image::new((texture.id(), fitted.size())).paint_at(ui, fitted);
        }
        if response.clicked() {
            self.selected[i] = !self.selected[i];
        }
        let mut on = self.selected[i];
        let label = tr("ui.pdf_import.page").replace("{n}", &(i + 1).to_string());
        if checkbox_row(ui, &label, &mut on).changed() {
            self.selected[i] = on;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thumbs(n: usize) -> Vec<PdfPageThumb> {
        (0..n)
            .map(|i| PdfPageThumb {
                width_pt: 100.0 + i as f32,
                height_pt: 50.0,
                width: 8,
                height: 4,
                rgba: vec![200; 8 * 4 * 4],
            })
            .collect()
    }

    #[test]
    fn it_opens_on_every_page_at_72_dpi_as_artboards() {
        let dialog = PdfImportDialog::new("a.pdf", 3, thumbs(3));
        assert_eq!(
            dialog.confirm(),
            Some(PdfImportSpec {
                pages: vec![0, 1, 2],
                dpi: 72,
                mode: PdfOpenMode::Artboards,
            })
        );
        assert_eq!(dialog.pixel_size(1), Some((101, 50)));
    }

    #[test]
    fn nothing_chosen_or_a_bad_resolution_blocks_with_a_reason() {
        let mut dialog = PdfImportDialog::new("a.pdf", 3, thumbs(3));
        dialog.select_all(false);
        assert_eq!(dialog.confirm(), None);
        assert_eq!(
            dialog.blocked_reason().as_deref(),
            Some("Choose at least one page to open")
        );
        assert!(dialog.resolve(DialogKeys::CONFIRM).is_open());
        dialog.select(2, true);
        dialog.set_dpi(5000);
        assert_eq!(
            dialog.blocked_reason().as_deref(),
            Some("The resolution must be 18 to 1200 dpi")
        );
        dialog.set_dpi(144);
        dialog.set_mode(PdfOpenMode::SeparateDocuments);
        assert_eq!(
            dialog.resolve(DialogKeys::CONFIRM),
            DialogOutcome::Confirmed(PdfImportSpec {
                pages: vec![2],
                dpi: 144,
                mode: PdfOpenMode::SeparateDocuments,
            })
        );
        assert_eq!(dialog.pixel_size(2), Some((204, 100)));
        assert_eq!(dialog.resolve(DialogKeys::CANCEL), DialogOutcome::Cancelled);
    }

    #[test]
    fn it_draws_a_thumbnail_per_page_in_both_appearances() {
        super::super::chrome::test_support::frame_both_themes(|ctx| {
            let mut dialog = PdfImportDialog::new("a.pdf", 120, thumbs(3));
            assert!(dialog.show(ctx).is_open());
            assert_eq!(dialog.thumbnails_loaded(), 3);
        });
    }

    #[test]
    fn clicking_a_page_thumbnail_drops_it_and_clicking_again_takes_it_back() {
        let harness = super::super::chrome::test_support::Harness::new();
        let mut dialog = PdfImportDialog::new("a.pdf", 3, thumbs(3));
        harness.click_widget(PdfImportDialog::page_id(1), |ctx| {
            let _ = dialog.show(ctx);
        });
        assert!(dialog.is_selected(0) && !dialog.is_selected(1) && dialog.is_selected(2));
        harness.click_widget(PdfImportDialog::page_id(1), |ctx| {
            let _ = dialog.show(ctx);
        });
        assert!(dialog.is_selected(1));
    }
}
