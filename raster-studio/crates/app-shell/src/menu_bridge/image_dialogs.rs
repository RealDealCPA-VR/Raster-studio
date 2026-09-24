//! W10-H: the dialogs of Image ▸ Mode ▸ Bitmap… / Duotone… and Image ▸
//! Apply Image… / Calculations…, as ONE host dialog kind.
//!
//! Each confirms to a spec, not a `DialogAction`, so it takes the Indexed
//! Color road: the confirmed spec is parked here and the menu pick rides the
//! frame's output to the `perform` arm, which takes it
//! (`take_confirmed_*`). Keeping the four behind one [`ImageDialog`] keeps
//! the shared dialog host's edits to one variant.

use std::cell::RefCell;

use color::bitmap::BitmapMethod;
use color::duotone::DuotoneSpec;
use editor_core::color_mode::mode;
use ui::dialogs::{
    ApplyImageDialog, ApplyImageSpec, BitmapDialog, CalculationsDialog, CalculationsSpec,
    DialogOutcome, DuotoneDialog,
};
use ui::menu::{ColorMode, MenuAction};

use crate::chrome::ChromeOutput;
use crate::editor::Editor;

thread_local! {
    static CONFIRMED_BITMAP: RefCell<Option<BitmapMethod>> = const { RefCell::new(None) };
    static CONFIRMED_DUOTONE: RefCell<Option<DuotoneSpec>> = const { RefCell::new(None) };
    static CONFIRMED_APPLY_IMAGE: RefCell<Option<ApplyImageSpec>> = const { RefCell::new(None) };
    static CONFIRMED_CALCULATIONS: RefCell<Option<CalculationsSpec>> =
        const { RefCell::new(None) };
}

/// The Bitmap method a dialog confirmed, if one did since the last take.
pub(crate) fn take_confirmed_bitmap() -> Option<BitmapMethod> {
    CONFIRMED_BITMAP.with(|slot| slot.borrow_mut().take())
}

/// The Duotone inks a dialog confirmed, if one did since the last take.
pub(crate) fn take_confirmed_duotone() -> Option<DuotoneSpec> {
    CONFIRMED_DUOTONE.with(|slot| slot.borrow_mut().take())
}

/// The Apply Image a dialog confirmed, if one did since the last take.
pub(crate) fn take_confirmed_apply_image() -> Option<ApplyImageSpec> {
    CONFIRMED_APPLY_IMAGE.with(|slot| slot.borrow_mut().take())
}

/// The Calculations a dialog confirmed, if one did since the last take.
pub(crate) fn take_confirmed_calculations() -> Option<CalculationsSpec> {
    CONFIRMED_CALCULATIONS.with(|slot| slot.borrow_mut().take())
}

/// One of the four dialogs.
#[derive(Debug)]
pub enum ImageDialog {
    Bitmap(BitmapDialog),
    Duotone(DuotoneDialog),
    ApplyImage(ApplyImageDialog),
    Calculations(CalculationsDialog),
}

/// Whether `action` opens one of these dialogs.
pub(crate) fn owns(action: MenuAction) -> bool {
    matches!(
        action,
        MenuAction::SetColorMode(ColorMode::Bitmap | ColorMode::Duotone)
            | MenuAction::ApplyImage
            | MenuAction::Calculations
    )
}

/// The dialog `action` opens over the editor's state, or `None` when it
/// cannot (no document, or a Bitmap / Duotone pick on a document that is
/// not Grayscale — the arm then refuses and says why).
pub(crate) fn open(action: MenuAction, editor: &Editor) -> Option<ImageDialog> {
    let doc = editor.active()?;
    let from = doc.document.meta.color_mode;
    match action {
        MenuAction::SetColorMode(ColorMode::Bitmap) => {
            (from == mode::GRAYSCALE).then(|| ImageDialog::Bitmap(BitmapDialog::default()))
        }
        MenuAction::SetColorMode(ColorMode::Duotone) => {
            (from == mode::GRAYSCALE).then(|| ImageDialog::Duotone(DuotoneDialog::default()))
        }
        MenuAction::ApplyImage => {
            doc.document.active_layer()?;
            Some(ImageDialog::ApplyImage(ApplyImageDialog::new(
                super::apply_image::source_documents(editor),
            )))
        }
        MenuAction::Calculations => Some(ImageDialog::Calculations(CalculationsDialog::new(
            super::apply_image::source_documents(editor),
        ))),
        _ => None,
    }
}

impl ImageDialog {
    /// Draw one frame. A confirmation parks the spec and pushes the menu
    /// pick into `out`. Returns `true` when the dialog closed.
    pub(crate) fn drive(&mut self, ctx: &egui::Context, out: &mut ChromeOutput) -> bool {
        fn settle<T>(
            outcome: DialogOutcome<T>,
            park: impl FnOnce(T),
            action: MenuAction,
            out: &mut ChromeOutput,
        ) -> bool {
            match outcome {
                DialogOutcome::Open => false,
                DialogOutcome::Cancelled => true,
                DialogOutcome::Confirmed(spec) => {
                    park(spec);
                    out.menu.push(action);
                    true
                }
            }
        }
        match self {
            ImageDialog::Bitmap(d) => settle(
                d.show(ctx),
                |s| CONFIRMED_BITMAP.with(|slot| *slot.borrow_mut() = Some(s)),
                MenuAction::SetColorMode(ColorMode::Bitmap),
                out,
            ),
            ImageDialog::Duotone(d) => settle(
                d.show(ctx),
                |s| CONFIRMED_DUOTONE.with(|slot| *slot.borrow_mut() = Some(s)),
                MenuAction::SetColorMode(ColorMode::Duotone),
                out,
            ),
            ImageDialog::ApplyImage(d) => settle(
                d.show(ctx),
                |s| CONFIRMED_APPLY_IMAGE.with(|slot| *slot.borrow_mut() = Some(s)),
                MenuAction::ApplyImage,
                out,
            ),
            ImageDialog::Calculations(d) => settle(
                d.show(ctx),
                |s| CONFIRMED_CALCULATIONS.with(|slot| *slot.borrow_mut() = Some(s)),
                MenuAction::Calculations,
                out,
            ),
        }
    }
}
