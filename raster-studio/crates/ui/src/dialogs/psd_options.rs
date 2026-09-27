//! W18-G: File ▸ Save PSD/PSB… — Photopea's Save PSD/PSB options.
//!
//! Photopea's dialog: a Format choice (PSD or PSB), then under "Minify the
//! file" the switches that make the written file smaller, in its order.
//! Three of its four are here — Blank preview image (the merged composite
//! written as blank paper) and ZIP for pixel data (every channel
//! ZIP-compressed instead of RLE), which `psd::WriteOptions` carries, and
//! Put the file into ZIP (the file written inside a `.zip` archive,
//! `psd::write::zip_container`). Remove Smart Object pixels is not offered:
//! this build shows a smart object read from a `.psd` by its layer pixels,
//! so a file without them would reopen here with blank smart objects.
//!
//! The dialog hands back a [`PsdSaveOptions`] and nothing else, the way the
//! Trim dialog hands back a spec: the shell writes the file. The last
//! options confirmed are remembered per UI thread ([`remember_options`]), so
//! the next Save PSD/PSB opens on them and the save route reads them.
//!
//! Hosting: File ▸ Save PSD/PSB… asks for the dialog ([`request_open`]); the
//! right-click menu's per-frame host draws it ([`draw_requested`]) and, on
//! Save, marks the options confirmed and asks for the row again, whose
//! route then writes the file ([`take_confirmed`]).

use egui::Context;

use crate::dialogs::chrome::{
    action_row, modal, DialogButton, DialogKeys, DialogOutcome, DialogWidth,
};
use crate::dialogs::controls::{checkbox_row, combo};
use crate::strings::tr;

/// What Save PSD/PSB writes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub struct PsdSaveOptions {
    /// Photopea's Format ▸ PSB: Photoshop's large document format (a `.psb`,
    /// version 2) instead of a `.psd`.
    pub psb: bool,
    /// "Blank preview image": the merged composite is blank paper.
    pub blank_preview: bool,
    /// "ZIP for pixel data": channels ZIP-compressed instead of RLE.
    pub zip_pixel_data: bool,
    /// "Put the file into ZIP": the `.psd` / `.psb` written inside a `.zip`
    /// archive.
    pub put_into_zip: bool,
}

impl PsdSaveOptions {
    /// The layered file's extension, without a dot.
    pub fn document_extension(self) -> &'static str {
        if self.psb {
            "psb"
        } else {
            "psd"
        }
    }

    /// The extension of the file written, without a dot: the archive's when
    /// the file is put into a ZIP.
    pub fn extension(self) -> &'static str {
        if self.put_into_zip {
            "zip"
        } else {
            self.document_extension()
        }
    }
}

thread_local! {
    static REMEMBERED: std::cell::Cell<PsdSaveOptions> =
        const { std::cell::Cell::new(PsdSaveOptions {
            psb: false,
            blank_preview: false,
            zip_pixel_data: false,
            put_into_zip: false,
        }) };
    /// The row asked for the dialog; the next frame opens it.
    static WANTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// The dialog was confirmed; the row's next run writes the file.
    static CONFIRMED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// File ▸ Save PSD/PSB… asks for its options: the next frame opens the
/// dialog on the remembered ones.
pub fn request_open() {
    WANTED.with(|w| w.set(true));
}

/// Whether the dialog was just confirmed (and forget it): the row's route
/// writes the file when it was, and asks for the dialog when not.
pub fn take_confirmed() -> bool {
    CONFIRMED.with(|c| c.replace(false))
}

/// The egui memory slot the open dialog lives in.
fn slot() -> egui::Id {
    egui::Id::new("raster-psd-options-dialog")
}

/// Whether the dialog is open.
pub fn is_open(ctx: &Context) -> bool {
    ctx.data(|d| d.get_temp::<Option<PsdOptionsDialog>>(slot()))
        .flatten()
        .is_some()
}

/// The open dialog, if any (a test reads its drawn switches).
pub fn open_dialog(ctx: &Context) -> Option<PsdOptionsDialog> {
    ctx.data(|d| d.get_temp::<Option<PsdOptionsDialog>>(slot()))
        .flatten()
}

/// Once a frame: open the dialog when the row asked for it, draw it while
/// it is open; on Save the options are remembered and marked confirmed and
/// the row is asked for again, which writes the file. Cancel closes it.
pub fn draw_requested(w: &mut crate::Workspace, ctx: &Context) {
    if WANTED.with(|wanted| wanted.replace(false)) {
        let dialog = PsdOptionsDialog::new(remembered_options());
        ctx.data_mut(|d| d.insert_temp(slot(), Some(dialog)));
    }
    let Some(mut dialog) = open_dialog(ctx) else {
        return;
    };
    let keep = match dialog.show(ctx) {
        DialogOutcome::Open => Some(dialog),
        DialogOutcome::Cancelled => None,
        DialogOutcome::Confirmed(_) => {
            CONFIRMED.with(|c| c.set(true));
            w.emit(crate::Intent::Action(crate::menu::MenuAction::SavePsdPsb));
            ctx.request_repaint();
            None
        }
    };
    ctx.data_mut(|d| d.insert_temp(slot(), keep));
}

/// The options the last confirmed Save PSD/PSB dialog chose (Photopea's
/// defaults, every switch off, before any).
pub fn remembered_options() -> PsdSaveOptions {
    REMEMBERED.with(std::cell::Cell::get)
}

/// Remember `options` for the next Save PSD/PSB (the dialog's confirmation).
pub fn remember_options(options: PsdSaveOptions) {
    REMEMBERED.with(|slot| slot.set(options));
}

/// File ▸ Save PSD/PSB….
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PsdOptionsDialog {
    options: PsdSaveOptions,
    /// The ids the last frame drew the Blank preview, ZIP for pixel data and
    /// Put the file into ZIP checkboxes with, so a test can press the real
    /// rows.
    drawn: [Option<egui::Id>; 3],
}

impl PsdOptionsDialog {
    /// Open on `options` (the remembered ones, as the shell opens it).
    pub fn new(options: PsdSaveOptions) -> Self {
        Self {
            options,
            drawn: [None; 3],
        }
    }

    /// The ids the last frame drew the Blank preview image, ZIP for pixel
    /// data and Put the file into ZIP checkboxes with.
    pub fn drawn_switches(&self) -> [Option<egui::Id>; 3] {
        self.drawn
    }

    /// The options as they stand.
    pub fn options(&self) -> PsdSaveOptions {
        self.options
    }

    /// Set the options directly — tests and presets.
    pub fn set_options(&mut self, options: PsdSaveOptions) {
        self.options = options;
    }

    pub fn title(&self) -> &'static str {
        tr("ui.w18g.psd.title")
    }

    /// Every choice is a valid file, so the confirmation is always live.
    pub fn confirm(&self) -> Option<PsdSaveOptions> {
        Some(self.options)
    }

    /// Escape and Enter, without drawing — Escape wins over Enter.
    pub fn resolve(&self, keys: DialogKeys) -> DialogOutcome<PsdSaveOptions> {
        if keys.cancel {
            return DialogOutcome::Cancelled;
        }
        if keys.confirm {
            if let Some(options) = self.confirm() {
                return DialogOutcome::Confirmed(options);
            }
        }
        DialogOutcome::Open
    }

    /// Draw one frame and fold the keyboard and the action row into one
    /// outcome. A confirmation is remembered for the next save.
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<PsdSaveOptions> {
        let mut outcome = self.resolve(DialogKeys::read(ctx));
        let drawn = modal(
            ctx,
            "psd-options",
            self.title(),
            None,
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
        if let DialogOutcome::Confirmed(options) = outcome {
            remember_options(options);
        }
        outcome
    }

    fn body(&mut self, ui: &mut egui::Ui) -> Option<DialogButton> {
        design::inspector_field(ui, tr("ui.w18g.psd.format"), |ui| {
            combo(
                ui,
                "psd-options-format",
                &mut self.options.psb,
                &[false, true],
                |psb| if psb { "PSB" } else { "PSD" }.to_string(),
                |_| None,
            )
        });
        design::section_header(ui, tr("ui.w18g.psd.minify"));
        let blank = checkbox_row(
            ui,
            tr("ui.w18g.psd.blank.preview"),
            &mut self.options.blank_preview,
        );
        let zip = checkbox_row(ui, tr("ui.w18g.psd.zip"), &mut self.options.zip_pixel_data);
        let archive = checkbox_row(
            ui,
            tr("ui.w18g.psd.into.zip"),
            &mut self.options.put_into_zip,
        );
        self.drawn = [Some(blank.id), Some(zip.id), Some(archive.id)];
        action_row(ui, tr("ui.w18g.psd.save"), None, &[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_opens_on_photopeas_defaults_and_confirms_what_was_set() {
        let dialog = PsdOptionsDialog::default();
        assert_eq!(dialog.options(), PsdSaveOptions::default());
        assert_eq!(dialog.options().extension(), "psd");
        let mut dialog = PsdOptionsDialog::new(PsdSaveOptions::default());
        let chosen = PsdSaveOptions {
            psb: true,
            blank_preview: true,
            zip_pixel_data: true,
            put_into_zip: false,
        };
        dialog.set_options(chosen);
        assert_eq!(chosen.extension(), "psb");
        let zipped = PsdSaveOptions {
            put_into_zip: true,
            ..chosen
        };
        assert_eq!(
            (zipped.extension(), zipped.document_extension()),
            ("zip", "psb")
        );
        assert_eq!(
            dialog.resolve(DialogKeys::CONFIRM),
            DialogOutcome::Confirmed(chosen)
        );
        assert_eq!(
            dialog.resolve(DialogKeys {
                confirm: true,
                cancel: true,
            }),
            DialogOutcome::Cancelled
        );
        assert!(dialog.resolve(DialogKeys::NONE).is_open());
    }

    /// The drawn switches are wired: pressing the real Blank preview, ZIP
    /// for pixel data and Put the file into ZIP rows turns the three options
    /// on, and Enter confirms them and remembers them for the save route.
    #[test]
    fn the_drawn_switches_set_the_options_and_enter_remembers_them() {
        use crate::dialogs::chrome::test_support::Harness;
        remember_options(PsdSaveOptions::default());
        let harness = Harness::new();
        let dialog = std::cell::RefCell::new(PsdOptionsDialog::new(remembered_options()));
        let draw = |ctx: &Context| {
            let _ = dialog.borrow_mut().show(ctx);
        };
        harness.frame(Vec::new(), draw);
        for row in 0..3 {
            let id = dialog.borrow().drawn_switches()[row].expect("the row is drawn");
            harness.click_widget(id, draw);
        }
        let options = dialog.borrow().options();
        assert!(
            options.blank_preview && options.zip_pixel_data && options.put_into_zip,
            "{options:?}"
        );
        assert!(!options.psb, "the format is untouched");
        let mut last = DialogOutcome::Open;
        harness.frame(Harness::key_events(egui::Key::Enter), |ctx| {
            last = dialog.borrow_mut().show(ctx);
        });
        assert_eq!(last, DialogOutcome::Confirmed(options));
        assert_eq!(remembered_options(), options);
        remember_options(PsdSaveOptions::default());
    }

    #[test]
    fn it_draws_in_both_appearances() {
        crate::dialogs::chrome::test_support::frame_both_themes(|ctx| {
            let mut dialog = PsdOptionsDialog::default();
            assert!(dialog.show(ctx).is_open());
        });
    }
}
