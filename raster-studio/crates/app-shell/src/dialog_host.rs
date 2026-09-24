//! The dialog host: the one place a modal [`ui::dialogs`] surface is drawn.
//!
//! [`crate::chrome::Chrome`] owns one [`DialogHost`]. The ten finished dialogs
//! in `ui::dialogs` each keep their own state, validate it, and return a
//! [`ui::dialogs::DialogOutcome`] — everything except a surface to be drawn in.
//! This module is that surface: at most one dialog open at a time, drawn after
//! the docks, opened from a [`ui::menu::MenuAction`], closed by Escape or by
//! the action row, and folded back into the frame's [`ChromeOutput`].
//!
//! # The rules
//!
//! * **The dialogs own their keyboard.** Escape cancels and Enter confirms
//!   through [`ui::dialogs::resolve`] inside each `show`; the shell stops
//!   feeding the keymap while [`DialogHost::is_open`] so nothing acts beside
//!   them, and the canvas stops receiving pointer samples so a click that
//!   meant "dismiss this modal" can never start a stroke.
//! * **Confirmed values ride existing channels where they exist.** A
//!   [`ui::dialogs::DialogAction::Command`] is a document edit like any other
//!   and joins [`ChromeOutput::commands`]; a colour lands in
//!   [`ChromeOutput::set_foreground`]; a brush lands in
//!   [`ChromeOutput::set_brush`]. The rest — creating a document, resampling
//!   one, export, running a filter, replacing the preferences — are parked in
//!   [`ChromeOutput::dialog`] and consumed by the menu-item wiring that opens
//!   their dialog (each PRODUCTION-TODO P0 task owns its variant).
//! * **One at a time.** Opening a dialog replaces whatever was open, the way
//!   Photopea's modals do; a dialog is view state on the chrome, never
//!   document state, so closing one loses nothing but the edits the user
//!   chose to lose.

use std::cell::RefCell;

use crate::chrome::ChromeOutput;
use tools::ToolId;
use ui::dialogs::{
    AdjustmentDialog, AdjustmentInvocation, ArbitraryRotationDialog, BrushEditorDialog,
    CanvasSizeDialog, ColorPickerDialog, DialogAction, DialogOutcome, ExportAsDialog, FilterDialog,
    GradientEditorDialog, ImageSizeDialog, LayerStyleDialog, NewDocumentDialog, PreferencesDialog,
    ScreenSampler,
};
use ui::menu::AdjustmentId;

/// W5-B: the `.cube` file Color Lookup's "Load" button asks for. A test sets
/// [`PICKED_CUBE_FOR_TEST`] to stand in for the person at the file dialog, so
/// the dialog's real Load route can be driven headless.
fn pick_cube_file() -> Option<std::path::PathBuf> {
    #[cfg(test)]
    if let Some(path) = PICKED_CUBE_FOR_TEST.with(|p| p.borrow_mut().take()) {
        return Some(path);
    }
    rfd::FileDialog::new()
        .add_filter("3D LUT", &["cube", "CUBE"])
        .set_title("Load Color Lookup")
        .pick_file()
}

#[cfg(test)]
thread_local! {
    /// The file [`pick_cube_file`] answers once, in place of the dialog.
    pub(crate) static PICKED_CUBE_FOR_TEST: RefCell<Option<std::path::PathBuf>> =
        const { RefCell::new(None) };
}

thread_local! {
    /// The parameters the Adjustments dialog confirmed, waiting for the
    /// [`ui::menu::MenuAction::ApplyAdjustment`] pick that rides
    /// [`ChromeOutput::menu`] out of the same frame.
    ///
    /// The menu action carries only the adjustment's *id* — it is the menu's
    /// vocabulary, and a menu row has no parameters — while the bake itself
    /// needs `&mut Editor`, which only [`crate::menu_bridge::perform`] holds.
    /// The confirmed parameters are therefore parked here by
    /// [`DialogHost::ui`] and taken by `perform` when it reaches the arm, on
    /// the same thread in the same frame. Thread-local rather than global so
    /// two windows' worth of tests cannot hand each other their settings, and
    /// keyed by id so a stale entry can never be applied as another
    /// adjustment. `perform` with nothing parked runs the adjustment at its
    /// starting parameters, which is what a keyboard chord with no dialog
    /// asked for.
    static CONFIRMED_ADJUSTMENT: RefCell<Option<AdjustmentInvocation>> = const { RefCell::new(None) };
    /// The options the Trim dialog confirmed, waiting for the
    /// [`ui::menu::MenuAction::Trim`] pick that rides out of the same frame
    /// — the same shape as the parked adjustment, for the same reason.
    static CONFIRMED_TRIM: RefCell<Option<ui::dialogs::TrimSpec>> = const { RefCell::new(None) };
    /// W7-D: the palette/count/dither the Indexed Color dialog confirmed,
    /// waiting for the `SetColorMode(Indexed)` pick of the same frame.
    static CONFIRMED_INDEXED: RefCell<Option<ui::dialogs::IndexedSpec>> =
        const { RefCell::new(None) };
    /// The parameters the Refine Edge dialog confirmed, waiting for the
    /// [`ui::menu::MenuAction::RefineEdge`] pick.
    static CONFIRMED_REFINE_EDGE: RefCell<Option<ui::dialogs::refine_mask::RefineMaskSpec>> =
        const { RefCell::new(None) };
    /// The name the Duplicate Layer dialog confirmed, waiting for the
    /// [`ui::menu::MenuAction::DuplicateLayer`] pick that rides out of the
    /// same frame.
    static CONFIRMED_DUPLICATE_NAME: RefCell<Option<String>> = const { RefCell::new(None) };
    /// W9-I: a Duplicate Layer confirmed into ANOTHER open document, as
    /// (source layer, target document, name), waiting for the chrome, which
    /// holds the editor, to make the copy (`Chrome::copy_layers_across`).
    static CONFIRMED_DUPLICATE_INTO: RefCell<Option<(layer_model::LayerId, crate::doc::DocumentId, String)>> =
        const { RefCell::new(None) };
    /// W3-H: the spec the Color Range dialog confirmed, waiting for the
    /// [`ui::menu::MenuAction::ColorRange`] pick.
    static CONFIRMED_COLOR_RANGE: RefCell<Option<ui::dialogs::ColorRangeSpec>> =
        const { RefCell::new(None) };
    /// W3-H: the amount a Select > Modify dialog confirmed, waiting for the
    /// [`ui::menu::MenuAction::Modify`] pick of the same operation.
    static CONFIRMED_MODIFY: RefCell<Option<ui::dialogs::ModifySpec>> = const { RefCell::new(None) };
    /// W3-H: the name the Save Selection dialog confirmed.
    static CONFIRMED_SAVE_SELECTION: RefCell<Option<ui::dialogs::SaveSelectionSpec>> =
        const { RefCell::new(None) };
    /// W3-H: the entry and operation the Load Selection dialog confirmed.
    static CONFIRMED_LOAD_SELECTION: RefCell<Option<ui::dialogs::LoadSelectionSpec>> =
        const { RefCell::new(None) };
    /// W7-H: the warp the Liquify dialog confirmed, waiting for the
    /// [`ui::menu::MenuAction::Liquify`] pick.
    static CONFIRMED_LIQUIFY: RefCell<Option<ui::dialogs::LiquifySpec>> =
        const { RefCell::new(None) };
    /// W7-H: the deformation the Puppet Warp dialog confirmed, waiting for
    /// the [`ui::menu::MenuAction::PuppetWarp`] pick.
    static CONFIRMED_PUPPET_WARP: RefCell<Option<ui::dialogs::PuppetWarpSpec>> =
        const { RefCell::new(None) };
    /// W9-O: the blur a Blur Gallery dialog confirmed, waiting for the
    /// [`ui::menu::MenuAction::BlurGallery`] pick of the same kind.
    static CONFIRMED_BLUR_GALLERY: RefCell<Option<ui::dialogs::BlurGallerySpec>> =
        const { RefCell::new(None) };
}

/// W9-O: the blur a Blur Gallery dialog confirmed, if one did since the last
/// take. Consumed on read, so a confirmation is applied exactly once.
pub(crate) fn take_confirmed_blur_gallery() -> Option<ui::dialogs::BlurGallerySpec> {
    CONFIRMED_BLUR_GALLERY.with(|slot| slot.borrow_mut().take())
}

/// W7-H: the warp a Liquify dialog confirmed, if one did since the last take.
/// Consumed on read, so a confirmation is applied exactly once.
pub(crate) fn take_confirmed_liquify() -> Option<ui::dialogs::LiquifySpec> {
    CONFIRMED_LIQUIFY.with(|slot| slot.borrow_mut().take())
}

/// W7-H: the deformation a Puppet Warp dialog confirmed, if one did since
/// the last take. Consumed on read.
pub(crate) fn take_confirmed_puppet_warp() -> Option<ui::dialogs::PuppetWarpSpec> {
    CONFIRMED_PUPPET_WARP.with(|slot| slot.borrow_mut().take())
}

/// The Color Range spec a dialog confirmed, if one did since the last take.
/// Consumed on read; `perform` with nothing parked selects around the
/// foreground at the dialog's opening fuzziness.
pub(crate) fn take_confirmed_color_range() -> Option<ui::dialogs::ColorRangeSpec> {
    CONFIRMED_COLOR_RANGE.with(|slot| slot.borrow_mut().take())
}

/// The Modify amount a dialog confirmed for `op`, if one did since the last
/// take. A parked spec for *another* operation is left in place, the way a
/// parked adjustment for another id is.
pub(crate) fn take_confirmed_modify(
    op: ui::menu::ModifySelection,
) -> Option<ui::dialogs::ModifySpec> {
    CONFIRMED_MODIFY.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.as_ref().is_some_and(|parked| parked.op == op) {
            slot.take()
        } else {
            None
        }
    })
}

/// The name a Save Selection dialog confirmed, if one did since the last take.
pub(crate) fn take_confirmed_save_selection() -> Option<ui::dialogs::SaveSelectionSpec> {
    CONFIRMED_SAVE_SELECTION.with(|slot| slot.borrow_mut().take())
}

/// The choice a Load Selection dialog confirmed, if one did since the last
/// take.
pub(crate) fn take_confirmed_load_selection() -> Option<ui::dialogs::LoadSelectionSpec> {
    CONFIRMED_LOAD_SELECTION.with(|slot| slot.borrow_mut().take())
}

/// Where the licence lives, as the About window says it.
pub const LICENCE_POINTER: &str =
    "Proprietary. The licence terms are in LICENSES/ beside the source.";
/// Where the third-party notices live, relative to the source tree.
pub const THIRD_PARTY_NOTICES: &str = "LICENSES/THIRD_PARTY_NOTICES.md";

fn stage_confirmed_indexed(spec: ui::dialogs::IndexedSpec) {
    CONFIRMED_INDEXED.with(|slot| *slot.borrow_mut() = Some(spec));
}

/// W7-D: the spec an Indexed Color dialog confirmed, if one did since the
/// last take. Consumed on read; the `SetColorMode(Indexed)` arm with nothing
/// parked converts at the dialog's defaults.
pub(crate) fn take_confirmed_indexed() -> Option<ui::dialogs::IndexedSpec> {
    CONFIRMED_INDEXED.with(|slot| slot.borrow_mut().take())
}

fn stage_confirmed_trim(spec: ui::dialogs::TrimSpec) {
    CONFIRMED_TRIM.with(|slot| *slot.borrow_mut() = Some(spec));
}

/// The options a Trim dialog confirmed, if one did since the last take.
/// Consumed on read, so a confirmation is applied exactly once; `perform`
/// with nothing parked trims at the dialog's defaults, which is what a chord
/// with no dialog asked for.
pub(crate) fn take_confirmed_trim() -> Option<ui::dialogs::TrimSpec> {
    CONFIRMED_TRIM.with(|slot| slot.borrow_mut().take())
}

fn stage_confirmed_duplicate_name(name: String) {
    CONFIRMED_DUPLICATE_NAME.with(|slot| *slot.borrow_mut() = Some(name));
}

/// The name a Duplicate Layer dialog confirmed, if one did since the last
/// take. Consumed on read; `perform` with nothing parked names the copy
/// "<name> copy", which is what a chord with no dialog asked for.
pub(crate) fn take_confirmed_duplicate_name() -> Option<String> {
    CONFIRMED_DUPLICATE_NAME.with(|slot| slot.borrow_mut().take())
}

/// W9-I: the cross-document copy a Duplicate Layer dialog confirmed, if one
/// did since the last take, as (source layer, target document, name).
/// Consumed on read.
pub(crate) fn take_confirmed_duplicate_into(
) -> Option<(layer_model::LayerId, crate::doc::DocumentId, String)> {
    CONFIRMED_DUPLICATE_INTO.with(|slot| slot.borrow_mut().take())
}

fn stage_confirmed_refine_edge(spec: ui::dialogs::refine_mask::RefineMaskSpec) {
    CONFIRMED_REFINE_EDGE.with(|slot| *slot.borrow_mut() = Some(spec));
}

/// The parameters a Refine Edge dialog confirmed, if one did since the last
/// take. Consumed on read.
pub(crate) fn take_confirmed_refine_edge() -> Option<ui::dialogs::refine_mask::RefineMaskSpec> {
    CONFIRMED_REFINE_EDGE.with(|slot| slot.borrow_mut().take())
}

/// Park the parameters an Adjustments dialog confirmed for the `perform` arm
/// that applies them.
fn stage_confirmed_adjustment(invocation: AdjustmentInvocation) {
    CONFIRMED_ADJUSTMENT.with(|slot| *slot.borrow_mut() = Some(invocation));
}

/// The parameters a dialog confirmed for `id`, if one did since the last take.
///
/// Consumed on read, so a confirmation is applied exactly once. A parked
/// invocation for *another* id is left in place: it belongs to a pick that
/// has not been performed yet, and answering `None` here means the caller
/// runs `id` at its own starting parameters rather than somebody else's.
pub(crate) fn take_confirmed_adjustment(id: AdjustmentId) -> Option<layer_model::AdjustmentKind> {
    CONFIRMED_ADJUSTMENT.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.as_ref().is_some_and(|parked| parked.id == id) {
            slot.take().map(|parked| parked.kind)
        } else {
            None
        }
    })
}

/// A dialog that is open, holding its live state.
///
/// Each variant is one `ui::dialogs` dialog mid-edit. The constructors that
/// need document context take it at open time, from the [`crate::Editor`] the
/// chrome is drawing — a dialog is a view over the document, not a second copy
/// of it.
#[derive(Debug)]
pub enum ActiveDialog {
    NewDocument(Box<NewDocumentDialog>),
    ImageSize(Box<ImageSizeDialog>),
    CanvasSize(Box<CanvasSizeDialog>),
    ExportAs(Box<ExportAsDialog>),
    LayerStyle(Box<LayerStyleDialog>),
    ColorPicker(Box<ColorPickerDialog>),
    GradientEditor(Box<GradientEditorDialog>),
    BrushEditor(Box<BrushEditorDialog>),
    Preferences(Box<PreferencesDialog>),
    Filter(Box<FilterDialog>),
    /// Image ▸ Rotation ▸ Arbitrary….
    Rotation(Box<ArbitraryRotationDialog>),
    /// Filter ▸ Filter Gallery.
    FilterGallery(Box<ui::dialogs::FilterGalleryDialog>),
    /// Edit ▸ Fill…
    Fill(Box<ui::dialogs::FillDialog>),
    /// Edit ▸ Stroke…
    Stroke(Box<ui::dialogs::StrokeDialog>),
    /// Layer ▸ Refine Mask… — the edge-refinement dialog (card 060).
    RefineMask(Box<ui::dialogs::refine_mask::RefineMaskDialog>),
    /// Layer ▸ Remove Color Fringe… — the edge colour cleanup dialog
    /// (card 062).
    Defringe(Box<ui::dialogs::defringe::DefringeDialog>),
    /// Image ▸ Adjustments ▸ <adjustment>… — one dialog for all fifteen.
    Adjustment(Box<AdjustmentDialog>),
    /// Image ▸ Trim… — the basis and the sides (W2-F). Confirms to a
    /// [`ui::dialogs::TrimSpec`] parked for the `Trim` menu arm, the way an
    /// adjustment's parameters are.
    Trim(Box<ui::dialogs::TrimDialog>),
    /// Image ▸ Mode ▸ Indexed Color… (W7-D): palette source, colour count and
    /// dither. Its confirmed spec is parked for the `SetColorMode(Indexed)`
    /// arm, which quantises the document as one undo step.
    IndexedColor(Box<ui::dialogs::IndexedColorDialog>),
    /// Help ▸ About Raster Studio — a window with one button (W2-F).
    About(Box<ui::dialogs::AboutDialog>),
    /// View ▸ New Guide… (W2-F). Confirms to a `SetGuides` command.
    NewGuide(Box<ui::dialogs::NewGuideDialog>),
    /// Layer ▸ Rename Layer… (W2-F). Confirms to a `SetLayerProperties`.
    RenameLayer(Box<ui::dialogs::RenameLayerDialog>),
    /// Layer ▸ Duplicate Layer… (W2-F): the copy's name. Confirms to a name
    /// parked for the `DuplicateLayer` menu arm, which makes the copy.
    DuplicateLayer(Box<ui::dialogs::DuplicateLayerDialog>),
    /// Select ▸ Refine Edge… (W2-F): the Refine Mask dialog over the
    /// selection's coverage instead of a layer mask's. Its confirmed spec is
    /// parked for the `RefineEdge` menu arm, which writes the selection.
    RefineEdge(Box<ui::dialogs::refine_mask::RefineMaskDialog>),
    /// Select > Color Range... (W3-H): colour, fuzziness, invert and a
    /// selection preview. Its confirmed spec is parked for the `ColorRange`
    /// menu arm, which selects at full resolution.
    ColorRange(Box<ui::dialogs::ColorRangeDialog>),
    /// Select > Modify > <op>... (W3-H): the amount. Parked for the `Modify`
    /// arm of the same operation.
    SelectionModify(Box<ui::dialogs::SelectionModifyDialog>),
    /// Select > Save Selection... (W3-H): the name. Parked for the
    /// `SaveSelection` arm.
    SaveSelection(Box<ui::dialogs::SaveSelectionDialog>),
    /// Select > Load Selection... (W3-H): which saved selection, and how it
    /// meets the live one. Parked for the `LoadSelection` arm.
    LoadSelection(Box<ui::dialogs::LoadSelectionDialog>),
    /// Filter > Liquify... (W7-H): brush warping over a preview. Its
    /// confirmed field is parked for the `Liquify` arm.
    Liquify(Box<ui::dialogs::LiquifyDialog>),
    /// Edit > Puppet Warp (W7-H): pins on a mesh over the layer's ink. Its
    /// confirmed deformation is parked for the `PuppetWarp` arm.
    PuppetWarp(Box<ui::dialogs::PuppetWarpDialog>),
    /// W9-B: Layer ▸ New Fill Layer ▸ Solid Color / Gradient / Pattern, and
    /// re-editing a live fill layer. Confirms to a `CreateLayer` transaction
    /// or a `SetLayerKind` — a plain command, one undo step.
    FillLayer(Box<ui::dialogs::FillLayerDialog>),
    /// Filter > Blur Gallery > <kind>... (W9-O): handles over a preview. Its
    /// confirmed blur is parked for the `BlurGallery` arm.
    BlurGallery(Box<ui::dialogs::BlurGalleryDialog>),
    /// W9-K: Layer > Text > Warp Text... over the active text layer. Confirms
    /// to a `SetLayerKind` carrying the whole new warp - one undo step.
    WarpText(Box<ui::dialogs::WarpTextDialog>),
}

impl ActiveDialog {
    /// Draw one frame and fold the keyboard and the action row into one
    /// outcome.
    ///
    /// `sampler` is the screen eyedropper the shell supplies; while it is
    /// `None` the dialogs that offer one draw it disabled with a reason rather
    /// than pretending.
    fn show(
        &mut self,
        ctx: &egui::Context,
        sampler: Option<&dyn ScreenSampler>,
    ) -> DialogOutcome<DialogAction> {
        match self {
            Self::NewDocument(dialog) => dialog.show(ctx, sampler),
            Self::ImageSize(dialog) => dialog.show(ctx),
            Self::CanvasSize(dialog) => dialog.show(ctx, sampler),
            Self::ExportAs(dialog) => dialog.show(ctx),
            Self::LayerStyle(dialog) => dialog.show(ctx, sampler),
            Self::ColorPicker(dialog) => dialog.show(ctx, sampler),
            Self::GradientEditor(dialog) => dialog.show(ctx, sampler),
            Self::BrushEditor(dialog) => dialog.show(ctx),
            Self::Preferences(dialog) => dialog.show(ctx),
            Self::Filter(dialog) => dialog.show(ctx, sampler),
            Self::Rotation(dialog) => dialog.show(ctx),
            Self::FilterGallery(dialog) => dialog.show(ctx),
            Self::Fill(dialog) => dialog.show(ctx, sampler),
            Self::Stroke(dialog) => dialog.show(ctx),
            Self::RefineMask(dialog) => dialog.show(ctx),
            Self::Defringe(dialog) => dialog.show(ctx),
            Self::Adjustment(dialog) => dialog.show(ctx, sampler),
            Self::NewGuide(dialog) => dialog.show(ctx),
            Self::RenameLayer(dialog) => dialog.show(ctx),
            Self::WarpText(dialog) => dialog.show(ctx),
            Self::RefineEdge(dialog) => dialog.show(ctx),
            Self::FillLayer(dialog) => dialog.show(ctx, sampler),
            // Neither confirms to a `DialogAction`: `DialogHost::ui` drives
            // them before it reaches this generic show (see `is_special` and
            // the debug assertion there), and the host tests
            // `trim_confirms_to_a_parked_spec_and_the_trim_pick` and
            // `about_opens_from_the_menu_and_escape_closes_it` drive both
            // through `ui` to prove the arm below is never the one that runs.
            Self::Trim(_)
            | Self::IndexedColor(_)
            | Self::About(_)
            | Self::DuplicateLayer(_)
            | Self::ColorRange(_)
            | Self::SelectionModify(_)
            | Self::SaveSelection(_)
            | Self::LoadSelection(_)
            | Self::Liquify(_)
            | Self::BlurGallery(_)
            | Self::PuppetWarp(_) => DialogOutcome::Open,
        }
    }

    /// Whether [`DialogHost::ui`] drives this dialog itself rather than
    /// through [`ActiveDialog::show`]: the ones whose confirmation is not a
    /// [`DialogAction`].
    fn is_special(&self) -> bool {
        matches!(
            self,
            Self::Adjustment(_)
                | Self::Trim(_)
                | Self::IndexedColor(_)
                | Self::About(_)
                | Self::RefineEdge(_)
                | Self::DuplicateLayer(_)
                | Self::ColorRange(_)
                | Self::SelectionModify(_)
                | Self::SaveSelection(_)
                | Self::LoadSelection(_)
                | Self::Liquify(_)
                | Self::BlurGallery(_)
                | Self::PuppetWarp(_)
        )
    }
}

/// The chrome's dialog state: which modal is open, if any.
#[derive(Default)]
pub struct DialogHost {
    active: Option<ActiveDialog>,
    /// Whether the active Export As dialog has been given a real composite
    /// proxy. Opening the dialog needs only `&Editor` (placeholder proxy);
    /// the first refresh after that composites once and stops paying for it.
    preview_seeded: bool,
    /// Which colour well the open picker edits, when one does.
    color_target: Option<ui::panels::color::ColorWell>,
    /// Which tool the open gradient editor edits, when one does.
    gradient_target: Option<ToolId>,
}

impl DialogHost {
    /// Whether the active dialog is waiting for a chord (the Preferences
    /// dialog's keymap section), for the status bar.
    pub fn is_recording(&self) -> bool {
        matches!(
            self.active.as_ref(),
            Some(ActiveDialog::Preferences(dialog)) if dialog.capturing().is_some()
        )
    }

    /// Whether a modal is open this frame.
    pub fn is_open(&self) -> bool {
        self.active.is_some()
    }

    /// Open `dialog`, replacing any dialog already open.
    ///
    /// "At most one at a time" is the whole modal contract; replacing rather
    /// than refusing keeps that true without a second question the user did
    /// not ask.
    pub fn open(&mut self, dialog: ActiveDialog) {
        self.active = Some(dialog);
        self.preview_seeded = false;
    }

    /// Close whatever is open, keeping nothing.
    pub fn close(&mut self) {
        self.active = None;
        // W7-E: a smart-filter re-edit is armed only while its dialog is open.
        crate::menu_bridge::disarm_smart_filter_edit();
    }

    /// Open the dialog a [`ui::menu::MenuAction`] names, if this host has one
    /// wired for it.
    ///
    /// Returns whether a dialog opened. `false` leaves the intent for
    /// [`crate::menu_bridge::pick`], which is how actions this build performs
    /// without a dialog keep working: every P0 task moves its action from
    /// [`crate::menu_bridge::perform`] into this match and takes over the
    /// confirmed value, so no row is ever routed twice.
    pub fn open_for_menu_action(
        &mut self,
        action: &ui::menu::MenuAction,
        editor: &crate::Editor,
    ) -> bool {
        self.open_for_menu_action_at(action, editor, None)
    }

    /// [`Self::open_for_menu_action`], with the saved-selection row a
    /// Channels click named (W3-X). For `LoadSelection`, `load` opens the
    /// dialog on that row; a direct (Ctrl+click) request parks that row as
    /// a confirmed New load and returns `false`, so the bridge performs it
    /// without asking. Every other action ignores `load`.
    pub fn open_for_menu_action_at(
        &mut self,
        action: &ui::menu::MenuAction,
        editor: &crate::Editor,
        load: Option<ui::SelectionLoadRequest>,
    ) -> bool {
        match action {
            // Edit ▸ Keyboard Shortcuts… is the Preferences dialog opened on
            // its Keymap page (W3-G). Answered here, where a menu click, the
            // context menu and the chord (posted as the same intent) all
            // arrive, so no road can land on the General page instead.
            ui::menu::MenuAction::KeyboardShortcuts => {
                let mut prefs = editor.ui_preferences();
                prefs.page = ui::dialogs::PrefsSection::Keymap;
                self.open_preferences(prefs);
                true
            }
            // File ▸ New… asks for size and background before anything is
            // created; the confirmed spec comes back as
            // [`DialogAction::NewDocument`] and the shell builds the document
            // from it.
            ui::menu::MenuAction::NewDocument => {
                self.open(ActiveDialog::NewDocument(
                    Box::<NewDocumentDialog>::default(),
                ));
                true
            }
            // File ▸ Export As… — the per-format rows all open the one dialog,
            // seeded with the format the row names; the dialog's list is where
            // the choice can still be changed.
            ui::menu::MenuAction::Export(format) => match export_as_dialog(editor, *format) {
                Some(dialog) => {
                    self.open(dialog);
                    true
                }
                None => false,
            },
            // Image ▸ Canvas Size… re-frames the document without resampling.
            ui::menu::MenuAction::CanvasSize => match canvas_size_dialog(editor) {
                Some(dialog) => {
                    self.open(dialog);
                    true
                }
                None => false,
            },
            // Image ▸ Image Size… resamples the whole document as one
            // undoable step; the dialog asks for the target size.
            ui::menu::MenuAction::ImageSize => match image_size_dialog(editor) {
                Some(dialog) => {
                    self.open(dialog);
                    true
                }
                None => false,
            },
            // Card 060: Layer ▸ Refine Mask… — the dialog
            // is seeded with the active layer's RAW pixels and its mask's
            // pose-aware baseline (see refine_mask_dialog).
            ui::menu::MenuAction::RefineMask => match refine_mask_dialog(editor) {
                Some(dialog) => {
                    self.open(dialog);
                    true
                }
                None => false,
            },
            // Card 062: Layer ▸ Remove Color Fringe… — the dialog is seeded
            // with the active layer's RAW pixels and its mask's pose-aware
            // coverage (see defringe_dialog).
            ui::menu::MenuAction::RemoveColorFringe => match defringe_dialog(editor) {
                Some(dialog) => {
                    self.open(dialog);
                    true
                }
                None => false,
            },
            // W7-H: Filter > Liquify... and Edit > Puppet Warp open over the
            // active layer's pixels; with nothing to warp they fall through
            // to the bridge, whose message names the reason.
            ui::menu::MenuAction::Liquify => match crate::menu_bridge::warp_source(editor) {
                Some(source) => {
                    self.open(ActiveDialog::Liquify(Box::new(
                        ui::dialogs::LiquifyDialog::new(&source),
                    )));
                    true
                }
                None => false,
            },
            // W9-O: Filter > Blur Gallery > <kind>... opens over the active
            // layer's pixels, like Liquify.
            ui::menu::MenuAction::BlurGallery(kind) => {
                match crate::menu_bridge::warp_source(editor) {
                    Some(source) => {
                        self.open(ActiveDialog::BlurGallery(Box::new(
                            ui::dialogs::BlurGalleryDialog::new(*kind, &source),
                        )));
                        true
                    }
                    None => false,
                }
            }
            ui::menu::MenuAction::PuppetWarp => match crate::menu_bridge::warp_source(editor) {
                Some(source) => {
                    let dialog = ui::dialogs::PuppetWarpDialog::new(&source);
                    if dialog.mesh().is_none() {
                        return false;
                    }
                    self.open(ActiveDialog::PuppetWarp(Box::new(dialog)));
                    true
                }
                None => false,
            },
            ui::menu::MenuAction::FilterGallery => match filter_gallery_dialog(editor) {
                Some(dialog) => {
                    self.open(dialog);
                    true
                }
                None => false,
            },
            // Filter ▸ <filter>… opens the real parameter dialog. A filter
            // with no schema (none today — the catalogue is checked against
            // the menu in both directions) falls through to the bridge.
            ui::menu::MenuAction::Filter(id) => match filter_dialog_for(editor, *id) {
                Some(dialog) => {
                    self.open(dialog);
                    true
                }
                None => false,
            },
            // Image ▸ Adjustments ▸ <adjustment>… opens the parameter dialog
            // over the active pixel layer, for all fifteen — the ten that used
            // to be greyed for want of exactly this, and the five that used
            // to bake their defaults on the click. With no pixel layer to
            // preview it falls through to the bridge, whose message names the
            // reason.
            ui::menu::MenuAction::ApplyAdjustment(id) => match adjustment_dialog(editor, *id) {
                Some(dialog) => {
                    self.open(dialog);
                    true
                }
                None => false,
            },
            // W9-B: Layer ▸ New Fill Layer ▸ … asks for the colour, the
            // gradient or the pattern before the live fill layer exists.
            ui::menu::MenuAction::NewFillLayer(kind) => {
                match new_fill_layer_dialog(editor, *kind) {
                    Some(dialog) => {
                        self.open(dialog);
                        true
                    }
                    None => false,
                }
            }
            // W9-B: on a fill layer, Edit Adjustment… (and the Properties
            // page's "Edit fill") reopens its own dialog on its own source.
            ui::menu::MenuAction::EditAdjustmentLayer if active_fill_layer(editor).is_some() => {
                match edit_fill_layer_dialog(editor) {
                    Some(dialog) => {
                        self.open(dialog);
                        true
                    }
                    None => false,
                }
            }
            // W4-E round 2: Layer ▸ Edit Adjustment… (and the Properties
            // panel's "Open editor…", the same intent) on a Color Lookup
            // layer reopens its dialog — the built-in looks and the .cube
            // loader — on the layer's own table; confirming edits the layer
            // in place. Every other adjustment layer falls through to the
            // bridge, which reveals the Properties panel as before. (A fill
            // layer never reaches this arm: the W9-B arm above takes it.)
            ui::menu::MenuAction::EditAdjustmentLayer => match adjustment_layer_dialog(editor) {
                Some(dialog) => {
                    self.open(dialog);
                    true
                }
                None => false,
            },
            // Image ▸ Rotation ▸ Arbitrary… asks for the angle.
            ui::menu::MenuAction::RotateCanvas(ui::menu::CanvasRotation::Arbitrary) => {
                self.open(ActiveDialog::Rotation(
                    Box::<ArbitraryRotationDialog>::default(),
                ));
                true
            }
            // Edit ▸ Fill… asks for contents, blend and opacity. With no
            // document (or no pixel layer) it falls through to the bridge,
            // whose error message names the reason.
            ui::menu::MenuAction::FillDialog => match fill_dialog(editor) {
                Some(dialog) => {
                    self.open(dialog);
                    true
                }
                None => false,
            },
            // Edit ▸ Stroke… asks for width and location.
            ui::menu::MenuAction::StrokeDialog => match stroke_dialog(editor) {
                Some(dialog) => {
                    self.open(dialog);
                    true
                }
                None => false,
            },
            // Image ▸ Reveal All grows the frame; there is no dialog to ask
            // anything, so it performs directly.
            ui::menu::MenuAction::RevealAll => false,
            // Layer ▸ Layer Style ▸ … opens the real dialog instead of
            // toggling the effect at its defaults. The dialog lists every
            // effect; the row clicked is just the way in, the same way
            // Photopea's Blending Options… is.
            ui::menu::MenuAction::LayerStyle(_) => match layer_style_dialog(editor, false) {
                Some(dialog) => {
                    self.open(dialog);
                    true
                }
                None => false,
            },
            // Blending Options… is Photopea's other way into the same
            // dialog, opened on its Blending page (mode, opacity, fill). It
            // used to reveal the Properties panel, then to open the dialog
            // on the Drop Shadow page.
            ui::menu::MenuAction::BlendingOptions => match layer_style_dialog(editor, true) {
                Some(dialog) => {
                    self.open(dialog);
                    true
                }
                None => false,
            },
            // W9-H: Apply Style Preset opens the same dialog on its Styles
            // page, a grid of every style preset (defined or imported from
            // an `.asl`); the one clicked replaces the layer's style when
            // the dialog is confirmed. With no preset it falls through to
            // the bridge, whose message says none is defined.
            ui::menu::MenuAction::ApplyStylePreset if !editor.presets().styles().is_empty() => {
                match layer_style_dialog(editor, false) {
                    Some(ActiveDialog::LayerStyle(mut dialog)) => {
                        dialog.show_styles();
                        self.open(ActiveDialog::LayerStyle(dialog));
                        true
                    }
                    _ => false,
                }
            }
            // Image ▸ Trim… asks for the basis and the sides (W2-F); the
            // confirmed options are parked for the `Trim` arm.
            ui::menu::MenuAction::Trim => {
                if editor.active().is_none() {
                    return false;
                }
                self.open(ActiveDialog::Trim(Box::<ui::dialogs::TrimDialog>::default()));
                true
            }
            // W7-D: Image ▸ Mode ▸ Indexed Color asks for the palette first;
            // the confirmed spec is parked for the `SetColorMode` arm. A
            // document already indexed asks nothing (the arm refuses loudly).
            ui::menu::MenuAction::SetColorMode(ui::menu::ColorMode::Indexed) => {
                let Some(doc) = editor.active() else {
                    return false;
                };
                if doc.document.meta.color_mode == ui::menu::ColorMode::Indexed as u8 {
                    return false;
                }
                self.open(ActiveDialog::IndexedColor(Box::<
                    ui::dialogs::IndexedColorDialog,
                >::default()));
                true
            }
            // Help ▸ About: the executable's stamp and the licence pointers.
            ui::menu::MenuAction::About => {
                self.open(ActiveDialog::About(Box::new(
                    ui::dialogs::AboutDialog::new(
                        crate::version::about_line(),
                        LICENCE_POINTER,
                        THIRD_PARTY_NOTICES,
                    ),
                )));
                true
            }
            // View ▸ New Guide… over the document's current set.
            ui::menu::MenuAction::NewGuide => match new_guide_dialog(editor) {
                Some(dialog) => {
                    self.open(dialog);
                    true
                }
                None => false,
            },
            // W9-K: Layer > Text > Warp Text... over the active text layer.
            ui::menu::MenuAction::WarpText(ui::menu::WarpTextItem::Dialog) => {
                match warp_text_dialog(editor) {
                    Some(dialog) => {
                        self.open(dialog);
                        true
                    }
                    None => false,
                }
            }
            // Layer ▸ Rename Layer… over the active layer's name.
            ui::menu::MenuAction::RenameLayer => match rename_layer_dialog(editor) {
                Some(dialog) => {
                    self.open(dialog);
                    true
                }
                None => false,
            },
            // Layer ▸ Duplicate Layer… asks the copy's name first (W2-F): the
            // row's ellipsis promised a question, and it used to copy at once.
            ui::menu::MenuAction::DuplicateLayer => match duplicate_layer_dialog(editor) {
                Some(dialog) => {
                    self.open(dialog);
                    true
                }
                None => false,
            },
            // W3-H: Select > Color Range... over the active pixel layer. With
            // no pixel layer it falls through to the bridge, whose message
            // names the reason.
            ui::menu::MenuAction::ColorRange => match color_range_dialog(editor) {
                Some(dialog) => {
                    self.open(dialog);
                    true
                }
                None => false,
            },
            // W3-H: Select > Modify > <op>... asks the amount -- only over a
            // live selection, the same gate the menu row has.
            ui::menu::MenuAction::Modify(op) => {
                if !has_live_selection(editor) {
                    return false;
                }
                self.open(ActiveDialog::SelectionModify(Box::new(
                    ui::dialogs::SelectionModifyDialog::new(*op),
                )));
                true
            }
            // W3-H: Select > Save Selection... asks the name.
            ui::menu::MenuAction::SaveSelection => {
                let Some(open) = editor.active() else {
                    return false;
                };
                if !has_live_selection(editor) {
                    return false;
                }
                self.open(ActiveDialog::SaveSelection(Box::new(
                    ui::dialogs::SaveSelectionDialog::new(saved_selection_names(&open.document)),
                )));
                true
            }
            // W3-H: Select > Load Selection... lists the saved selections by
            // name.
            ui::menu::MenuAction::LoadSelection => {
                let Some(open) = editor.active() else {
                    return false;
                };
                let names = saved_selection_names(&open.document);
                if names.is_empty() {
                    return false;
                }
                // W3-X: Ctrl+click on a Channels row loads that row as the
                // new selection, as Photopea's Ctrl+click on a channel
                // thumbnail does: parked exactly as a confirmed dialog would
                // park it, for the bridge's `load_selection` to perform. A
                // row that no longer exists falls through to the dialog.
                if let Some(request) = load.filter(|r| r.direct) {
                    if let Some(name) = names.get(request.index) {
                        let spec = ui::dialogs::LoadSelectionSpec {
                            index: request.index,
                            name: name.clone(),
                            op: ui::dialogs::LoadOperation::New,
                            invert: false,
                        };
                        CONFIRMED_LOAD_SELECTION.with(|slot| *slot.borrow_mut() = Some(spec));
                        return false;
                    }
                }
                let live = has_live_selection(editor);
                let dialog = match load {
                    Some(request) => {
                        ui::dialogs::LoadSelectionDialog::new_at(names, live, request.index)
                    }
                    None => ui::dialogs::LoadSelectionDialog::new(names, live),
                };
                self.open(ActiveDialog::LoadSelection(Box::new(dialog)));
                true
            }
            // Select ▸ Refine Edge… over the selection's coverage.
            ui::menu::MenuAction::RefineEdge => {
                match crate::layer_ops::refine_edge_dialog(editor) {
                    Some(dialog) => {
                        self.open(ActiveDialog::RefineEdge(Box::new(dialog)));
                        true
                    }
                    None => false,
                }
            }
            _ => false,
        }
    }

    /// Open the gradient editor over one tool's ramp.
    ///
    /// The tool is remembered: the confirmed ramp is written back to that
    /// tool's options-bar swatch and to the editor's stroke ramp, even if the
    /// user switches tools while the dialog is up.
    pub fn open_gradient_editor(&mut self, tool: ToolId, gradient: layer_model::Gradient) {
        self.gradient_target = Some(tool);
        self.open(ActiveDialog::GradientEditor(Box::new(
            GradientEditorDialog::new(gradient),
        )));
    }

    /// Open the brush editor over one tool's brush.
    pub fn open_brush_editor(&mut self, brush: tools::BrushSettings) {
        self.open(ActiveDialog::BrushEditor(Box::new(BrushEditorDialog::new(
            brush,
        ))));
    }

    /// Open the Preferences dialog over the application's current settings.
    pub fn open_preferences(&mut self, prefs: ui::dialogs::UiPreferences) {
        self.open(ActiveDialog::Preferences(Box::new(PreferencesDialog::new(
            prefs,
        ))));
    }

    /// Open the colour picker for one of the colour wells.
    ///
    /// The target is remembered: the picker's confirmed colour lands in the
    /// well that opened it, not always the foreground.
    pub fn open_color_picker(
        &mut self,
        editor: &crate::Editor,
        target: ui::panels::color::ColorWell,
    ) {
        let current = match target {
            ui::panels::color::ColorWell::Foreground => editor.foreground(),
            ui::panels::color::ColorWell::Background => editor.background(),
        };
        self.color_target = Some(target);
        self.open(ActiveDialog::ColorPicker(Box::new(ColorPickerDialog::new(
            ui::dialogs::ColorValue::new(current),
        ))));
    }

    /// The open dialog's state, for tests that drive it directly.
    #[cfg(test)]
    pub(crate) fn active_for_test(&mut self) -> &mut ActiveDialog {
        self.active
            .as_mut()
            .expect("no dialog is open for a test to drive")
    }

    /// The open Fill dialog, for tests that drive its contents.
    #[cfg(test)]
    pub(crate) fn active_fill_dialog_for_test(&mut self) -> &mut ui::dialogs::FillDialog {
        match self.active_for_test() {
            ActiveDialog::Fill(dialog) => dialog,
            other => panic!("the active dialog is {other:?}, not the fill dialog"),
        }
    }

    /// Card 060: the opened Refine Mask dialog, for host-path tests — the
    /// place where the content-source wiring (raw pixels vs masked) broke in
    /// review round 1.
    #[cfg(test)]
    pub(crate) fn active_refine_mask_dialog_for_test(
        &mut self,
    ) -> &mut ui::dialogs::refine_mask::RefineMaskDialog {
        match self.active_for_test() {
            ActiveDialog::RefineMask(dialog) => dialog,
            other => panic!("the active dialog is {other:?}, not the refine-mask dialog"),
        }
    }

    /// W7-H: the open Liquify dialog, for host-path tests.
    #[cfg(test)]
    pub(crate) fn active_liquify_dialog_for_test(&mut self) -> &mut ui::dialogs::LiquifyDialog {
        match self.active_for_test() {
            ActiveDialog::Liquify(dialog) => dialog,
            other => panic!("the active dialog is {other:?}, not the Liquify dialog"),
        }
    }

    /// W9-O: the open Blur Gallery dialog, for host-path tests.
    #[cfg(test)]
    pub(crate) fn active_blur_gallery_dialog_for_test(
        &mut self,
    ) -> &mut ui::dialogs::BlurGalleryDialog {
        match self.active_for_test() {
            ActiveDialog::BlurGallery(dialog) => dialog,
            other => panic!("the active dialog is {other:?}, not the Blur Gallery dialog"),
        }
    }

    /// W7-H: the open Puppet Warp dialog, for host-path tests.
    #[cfg(test)]
    pub(crate) fn active_puppet_warp_dialog_for_test(
        &mut self,
    ) -> &mut ui::dialogs::PuppetWarpDialog {
        match self.active_for_test() {
            ActiveDialog::PuppetWarp(dialog) => dialog,
            other => panic!("the active dialog is {other:?}, not the Puppet Warp dialog"),
        }
    }

    /// Card 062: the opened Remove Color Fringe dialog, for host-path tests.
    #[cfg(test)]
    pub(crate) fn active_defringe_dialog_for_test(
        &mut self,
    ) -> &mut ui::dialogs::defringe::DefringeDialog {
        match self.active_for_test() {
            ActiveDialog::Defringe(dialog) => dialog,
            other => panic!("the active dialog is {other:?}, not the defringe dialog"),
        }
    }

    /// The open Export As dialog, for tests that read the row it was seeded
    /// with.
    #[cfg(test)]
    pub(crate) fn active_export_dialog_for_test(&mut self) -> &mut ExportAsDialog {
        match self.active_for_test() {
            ActiveDialog::ExportAs(dialog) => dialog,
            other => panic!("the active dialog is {other:?}, not the export dialog"),
        }
    }

    /// The open Stroke dialog, for tests that drive its geometry.
    #[cfg(test)]
    pub(crate) fn active_stroke_dialog_for_test(&mut self) -> &mut ui::dialogs::StrokeDialog {
        match self.active_for_test() {
            ActiveDialog::Stroke(dialog) => dialog,
            other => panic!("the active dialog is {other:?}, not the stroke dialog"),
        }
    }

    /// The open Preferences dialog, for tests that drive its sections.
    #[cfg(test)]
    pub(crate) fn active_preferences_for_test(&mut self) -> &mut ui::dialogs::PreferencesDialog {
        match self.active_for_test() {
            ActiveDialog::Preferences(dialog) => dialog,
            other => panic!("the active dialog is {other:?}, not the preferences dialog"),
        }
    }

    /// The open gradient editor, for tests that drive its stops.
    #[cfg(test)]
    pub(crate) fn active_gradient_editor_for_test(
        &mut self,
    ) -> &mut ui::dialogs::GradientEditorDialog {
        match self.active_for_test() {
            ActiveDialog::GradientEditor(dialog) => dialog,
            other => panic!("the active dialog is {other:?}, not the gradient editor"),
        }
    }

    /// The open brush editor, for tests that drive its settings.
    #[cfg(test)]
    pub(crate) fn active_brush_editor_for_test(&mut self) -> &mut ui::dialogs::BrushEditorDialog {
        match self.active_for_test() {
            ActiveDialog::BrushEditor(dialog) => dialog,
            other => panic!("the active dialog is {other:?}, not the brush editor"),
        }
    }

    /// The open Adjustments dialog, for tests that drive its parameters.
    #[cfg(test)]
    pub(crate) fn active_adjustment_dialog_for_test(&mut self) -> &mut AdjustmentDialog {
        match self.active_for_test() {
            ActiveDialog::Adjustment(dialog) => dialog,
            other => panic!("the active dialog is {other:?}, not the adjustment dialog"),
        }
    }

    /// W7-D: the open Indexed Color dialog, for tests that drive its options.
    #[cfg(test)]
    pub(crate) fn active_indexed_dialog_for_test(
        &mut self,
    ) -> &mut ui::dialogs::IndexedColorDialog {
        match self.active.as_mut() {
            Some(ActiveDialog::IndexedColor(dialog)) => dialog,
            other => panic!("expected the Indexed Color dialog, got {other:?}"),
        }
    }

    /// The open Trim dialog, for tests that drive its options.
    #[cfg(test)]
    pub(crate) fn active_trim_dialog_for_test(&mut self) -> &mut ui::dialogs::TrimDialog {
        match self.active_for_test() {
            ActiveDialog::Trim(dialog) => dialog,
            other => panic!("the active dialog is {other:?}, not the trim dialog"),
        }
    }

    /// The open About window, for tests that read what it shows.
    #[cfg(test)]
    pub(crate) fn active_about_dialog_for_test(&mut self) -> &mut ui::dialogs::AboutDialog {
        match self.active_for_test() {
            ActiveDialog::About(dialog) => dialog,
            other => panic!("the active dialog is {other:?}, not the about window"),
        }
    }

    /// The open New Guide dialog, for tests that drive its fields.
    #[cfg(test)]
    pub(crate) fn active_new_guide_dialog_for_test(&mut self) -> &mut ui::dialogs::NewGuideDialog {
        match self.active_for_test() {
            ActiveDialog::NewGuide(dialog) => dialog,
            other => panic!("the active dialog is {other:?}, not the new-guide dialog"),
        }
    }

    /// The open Rename Layer dialog, for tests that type a name.
    #[cfg(test)]
    pub(crate) fn active_rename_dialog_for_test(&mut self) -> &mut ui::dialogs::RenameLayerDialog {
        match self.active_for_test() {
            ActiveDialog::RenameLayer(dialog) => dialog,
            other => panic!("the active dialog is {other:?}, not the rename dialog"),
        }
    }

    /// W9-K: the open Warp Text dialog, for tests that move its fields.
    #[cfg(test)]
    pub(crate) fn active_warp_text_dialog_for_test(&mut self) -> &mut ui::dialogs::WarpTextDialog {
        match self.active_for_test() {
            ActiveDialog::WarpText(dialog) => dialog,
            other => panic!("the active dialog is {other:?}, not Warp Text"),
        }
    }

    /// The open Duplicate Layer dialog, for tests that type a name.
    #[cfg(test)]
    pub(crate) fn active_duplicate_dialog_for_test(
        &mut self,
    ) -> &mut ui::dialogs::DuplicateLayerDialog {
        match self.active_for_test() {
            ActiveDialog::DuplicateLayer(dialog) => dialog,
            other => panic!("the active dialog is {other:?}, not the duplicate dialog"),
        }
    }

    /// The open Refine Edge dialog, for tests that drive its parameters.
    #[cfg(test)]
    pub(crate) fn active_refine_edge_dialog_for_test(
        &mut self,
    ) -> &mut ui::dialogs::refine_mask::RefineMaskDialog {
        match self.active_for_test() {
            ActiveDialog::RefineEdge(dialog) => dialog,
            other => panic!("the active dialog is {other:?}, not the refine-edge dialog"),
        }
    }

    /// Whether the open dialog is the Layer Style dialog — Blending Options…
    /// and every Layer Style row open it.
    #[cfg(test)]
    pub(crate) fn layer_style_is_open_for_test(&self) -> bool {
        matches!(self.active, Some(ActiveDialog::LayerStyle(_)))
    }

    /// The open Layer Style dialog, for tests that drive its pages.
    #[cfg(test)]
    pub(crate) fn active_layer_style_dialog_for_test(&mut self) -> &mut LayerStyleDialog {
        match self.active_for_test() {
            ActiveDialog::LayerStyle(dialog) => dialog,
            other => panic!("the active dialog is {other:?}, not the layer style dialog"),
        }
    }

    /// Swap the Export As dialog's placeholder preview for a real composite.
    ///
    /// Opening the dialog happens from a harvest that holds only `&Editor`, so
    /// it starts with the placeholder; the first refresh after that — a frame
    /// that has the editor — composites once. The dialog's own encode counter
    /// shows the swap: one re-encode, then a steady frame encodes nothing.
    pub fn refresh_preview(&mut self, editor: &crate::Editor) {
        if self.preview_seeded {
            return;
        }
        let Some(ActiveDialog::ExportAs(dialog)) = self.active.as_mut() else {
            return;
        };
        let Some(open) = editor.active() else {
            return;
        };
        match open.export_preview(ui::dialogs::export_as::MAX_PROXY_SIDE) {
            Ok(proxy) => {
                dialog.set_proxy(proxy);
                self.preview_seeded = true;
            }
            Err(e) => tracing::warn!("export preview composite failed: {e}"),
        }
    }

    /// Draw the open dialog, if any, and fold its outcome into the frame.
    ///
    /// Takes no editor: a dialog holds its own state, captured when it was
    /// opened, and the confirmed value is folded into `out` for the shell to
    /// apply — the same one-way road every other control's edit travels. The
    /// one exception is the eyedropper's screen sampler, which reads the live
    /// composite and therefore arrives per frame.
    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        sampler: Option<&dyn ScreenSampler>,
        out: &mut ChromeOutput,
    ) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        // The Adjustments dialog is applied through the menu's own route, not
        // through its `DialogAction`: the confirmed parameters are parked for
        // `menu_bridge::perform`, and the pick that reaches that arm rides
        // `out.menu` like the click used to. So the confirmed value travels
        // the road Image ▸ Adjustments already had, with the dialog's numbers
        // in place of the defaults, and lands as the same single undo step.
        if let ActiveDialog::Adjustment(dialog) = active {
            match dialog.show(ctx, sampler) {
                // W4-E: Color Lookup's "Load .cube file" asks the host for a
                // file; the dialog parses it and shows why when it cannot.
                DialogOutcome::Open => {
                    if dialog.take_lut_file_request() {
                        if let Some(path) = pick_cube_file() {
                            let name = path
                                .file_stem()
                                .map(|s| s.to_string_lossy().into_owned())
                                .unwrap_or_default();
                            // W5-B: size-checked from the metadata before a
                            // byte is read — this is the interaction thread.
                            match adjustments::extended::read_cube_file(&path) {
                                Ok(text) => {
                                    let _ = dialog.load_cube_text(&name, &text);
                                }
                                Err(e) => dialog.set_lut_error(e.to_string()),
                            }
                        }
                    }
                }
                DialogOutcome::Cancelled => self.active = None,
                // Opened on an adjustment layer: the confirmed parameters
                // replace that layer's, as one kind edit (one undo step).
                DialogOutcome::Confirmed(_) if dialog.edit_layer().is_some() => {
                    if let Some(layer) = dialog.edit_layer() {
                        out.layer_kind.push(crate::chrome::KindEdit {
                            layer,
                            kind: Box::new(layer_model::LayerKind::Adjustment(
                                layer_model::AdjustmentLayer {
                                    kind: dialog.kind().clone(),
                                },
                            )),
                            gesture: None,
                        });
                    }
                    self.active = None;
                }
                DialogOutcome::Confirmed(_) => {
                    let invocation = dialog.invocation();
                    let id = invocation.id;
                    stage_confirmed_adjustment(invocation);
                    out.menu.push(ui::menu::MenuAction::ApplyAdjustment(id));
                    self.active = None;
                }
            }
            return;
        }
        // Trim takes the same road as the adjustments: the options are
        // parked and the `Trim` pick rides `out.menu` to the arm that crops.
        if let ActiveDialog::Trim(dialog) = active {
            match dialog.show(ctx) {
                DialogOutcome::Open => {}
                DialogOutcome::Cancelled => self.active = None,
                DialogOutcome::Confirmed(spec) => {
                    stage_confirmed_trim(spec);
                    out.menu.push(ui::menu::MenuAction::Trim);
                    self.active = None;
                }
            }
            return;
        }
        // W7-D: Indexed Color takes Trim's road — the spec is parked and the
        // `SetColorMode(Indexed)` pick carries it to the converting arm.
        if let ActiveDialog::IndexedColor(dialog) = active {
            match dialog.show(ctx) {
                DialogOutcome::Open => {}
                DialogOutcome::Cancelled => self.active = None,
                DialogOutcome::Confirmed(spec) => {
                    stage_confirmed_indexed(spec);
                    out.menu.push(ui::menu::MenuAction::SetColorMode(
                        ui::menu::ColorMode::Indexed,
                    ));
                    self.active = None;
                }
            }
            return;
        }
        // Refine Edge reuses the Refine Mask dialog, so its confirmation is
        // a `DialogAction::RefineMask` — which the shell would bake into a
        // layer MASK. Intercepted here: the spec is parked and the
        // `RefineEdge` pick carries it to the arm that writes the selection.
        if let ActiveDialog::RefineEdge(dialog) = active {
            match dialog.show(ctx) {
                DialogOutcome::Open => {}
                DialogOutcome::Cancelled => self.active = None,
                DialogOutcome::Confirmed(action) => {
                    if let DialogAction::RefineMask(spec) = action {
                        stage_confirmed_refine_edge(*spec);
                        out.menu.push(ui::menu::MenuAction::RefineEdge);
                    }
                    self.active = None;
                }
            }
            return;
        }
        // Duplicate Layer takes Trim's road: the name is parked and the
        // `DuplicateLayer` pick rides `out.menu` to the arm that copies.
        if let ActiveDialog::DuplicateLayer(dialog) = active {
            match dialog.show(ctx) {
                DialogOutcome::Open => {}
                DialogOutcome::Cancelled => self.active = None,
                // W9-I: a Destination naming another open document parks
                // the copy for the chrome instead; the menu arm copies
                // within the active document only.
                DialogOutcome::Confirmed(ui::dialogs::duplicate_layer::DuplicateLayerSpec {
                    name,
                    destination: Some(key),
                }) => {
                    let into = (dialog.source(), crate::doc::DocumentId(key), name);
                    CONFIRMED_DUPLICATE_INTO.with(|slot| *slot.borrow_mut() = Some(into));
                    self.active = None;
                }
                DialogOutcome::Confirmed(spec) => {
                    stage_confirmed_duplicate_name(spec.name);
                    out.menu.push(ui::menu::MenuAction::DuplicateLayer);
                    self.active = None;
                }
            }
            return;
        }
        // W3-H: the four Select-menu questions take Trim's road -- the
        // confirmed value is parked and the pick rides `out.menu` to the arm
        // that edits the selection as one undoable step.
        if let ActiveDialog::ColorRange(dialog) = active {
            match dialog.show(ctx, sampler) {
                DialogOutcome::Open => {}
                DialogOutcome::Cancelled => self.active = None,
                DialogOutcome::Confirmed(spec) => {
                    CONFIRMED_COLOR_RANGE.with(|slot| *slot.borrow_mut() = Some(spec));
                    out.menu.push(ui::menu::MenuAction::ColorRange);
                    self.active = None;
                }
            }
            return;
        }
        if let ActiveDialog::SelectionModify(dialog) = active {
            match dialog.show(ctx) {
                DialogOutcome::Open => {}
                DialogOutcome::Cancelled => self.active = None,
                DialogOutcome::Confirmed(spec) => {
                    let op = spec.op;
                    CONFIRMED_MODIFY.with(|slot| *slot.borrow_mut() = Some(spec));
                    out.menu.push(ui::menu::MenuAction::Modify(op));
                    self.active = None;
                }
            }
            return;
        }
        if let ActiveDialog::SaveSelection(dialog) = active {
            match dialog.show(ctx) {
                DialogOutcome::Open => {}
                DialogOutcome::Cancelled => self.active = None,
                DialogOutcome::Confirmed(spec) => {
                    CONFIRMED_SAVE_SELECTION.with(|slot| *slot.borrow_mut() = Some(spec));
                    out.menu.push(ui::menu::MenuAction::SaveSelection);
                    self.active = None;
                }
            }
            return;
        }
        if let ActiveDialog::LoadSelection(dialog) = active {
            match dialog.show(ctx) {
                DialogOutcome::Open => {}
                DialogOutcome::Cancelled => self.active = None,
                DialogOutcome::Confirmed(spec) => {
                    CONFIRMED_LOAD_SELECTION.with(|slot| *slot.borrow_mut() = Some(spec));
                    out.menu.push(ui::menu::MenuAction::LoadSelection);
                    self.active = None;
                }
            }
            return;
        }
        // W7-H: Liquify and Puppet Warp take Trim's road -- the confirmed
        // warp is parked and the pick rides `out.menu` to the arm that warps
        // the full-resolution layer as one undoable step.
        if let ActiveDialog::Liquify(dialog) = active {
            match dialog.show(ctx) {
                DialogOutcome::Open => {}
                DialogOutcome::Cancelled => self.active = None,
                DialogOutcome::Confirmed(spec) => {
                    CONFIRMED_LIQUIFY.with(|slot| *slot.borrow_mut() = Some(spec));
                    out.menu.push(ui::menu::MenuAction::Liquify);
                    self.active = None;
                }
            }
            return;
        }
        // W9-O: the Blur Gallery takes the same road, naming its kind.
        if let ActiveDialog::BlurGallery(dialog) = active {
            match dialog.show(ctx) {
                DialogOutcome::Open => {}
                DialogOutcome::Cancelled => self.active = None,
                DialogOutcome::Confirmed(spec) => {
                    let kind = spec.kind();
                    CONFIRMED_BLUR_GALLERY.with(|slot| *slot.borrow_mut() = Some(spec));
                    out.menu.push(ui::menu::MenuAction::BlurGallery(kind));
                    self.active = None;
                }
            }
            return;
        }
        if let ActiveDialog::PuppetWarp(dialog) = active {
            match dialog.show(ctx) {
                DialogOutcome::Open => {}
                DialogOutcome::Cancelled => self.active = None,
                DialogOutcome::Confirmed(spec) => {
                    CONFIRMED_PUPPET_WARP.with(|slot| *slot.borrow_mut() = Some(spec));
                    out.menu.push(ui::menu::MenuAction::PuppetWarp);
                    self.active = None;
                }
            }
            return;
        }
        // About asks nothing: dismissed is closed.
        if let ActiveDialog::About(dialog) = active {
            if dialog.show(ctx) {
                self.active = None;
            }
            return;
        }
        debug_assert!(
            !active.is_special(),
            "a special dialog reached the generic show"
        );
        match active.show(ctx, sampler) {
            DialogOutcome::Open => {}
            DialogOutcome::Cancelled => {
                self.active = None;
                self.color_target = None;
                // W7-E: a cancelled re-edit must not arm a later filter run.
                crate::menu_bridge::disarm_smart_filter_edit();
            }
            DialogOutcome::Confirmed(action) => {
                self.active = None;
                match action {
                    // The picker's colour lands in the well that opened it.
                    DialogAction::SetColor(color) => match self.color_target.take() {
                        Some(ui::panels::color::ColorWell::Background) => {
                            out.set_background = Some(color.rgba)
                        }
                        _ => out.set_foreground = Some(color.rgba),
                    },
                    // The gradient lands on the tool that opened the editor.
                    DialogAction::SetGradient(gradient) => {
                        if let Some(tool) = self.gradient_target.take() {
                            out.set_tool_gradient = Some((tool, *gradient));
                        }
                    }
                    other => fold(other, out),
                }
            }
        }
    }
}

/// Fold a confirmed [`DialogAction`] into the frame's output.
///
/// Existing channels first, so a confirmed edit travels exactly the road every
/// other edit travels; what has no channel yet is parked in
/// [`ChromeOutput::dialog`] for the menu-item wiring that opens its dialog.
fn fold(action: DialogAction, out: &mut ChromeOutput) {
    match action {
        DialogAction::Command(command) => out.commands.push(*command),
        // The name is for the preset store the brush-library task adds
        // (P0.16); the settings themselves are the active brush either way.
        DialogAction::SetBrush { settings, .. } => out.set_brush = Some(*settings),
        // The dialog owns Preferences now; the shell maps the ui schema onto
        // the app's and applies it.
        DialogAction::SetPreferences(prefs) => out.set_ui_preferences = Some(prefs),
        // W7-I: Fill and Stroke travel as `out.dialog`, which the shell's
        // `apply_chrome` applies (`menu_bridge::fill_selection_with` /
        // `stroke_selection_with`). They used to be parked in two fields no
        // code read, so a confirmed Fill or Stroke changed nothing.
        other => out.dialog = Some(other),
    }
}

/// The screen sampler the colour picker's eyedropper reads through: one
/// document pixel under the pointer, composited on demand.
///
/// The canvas is drawn across the whole window — the panels are an overlay —
/// so a logical window point maps straight through the camera's viewport
/// (physical pixels, `ppp` applied first) to document coordinates, and the
/// sample is a 1×1 composite at that pixel. Per-click cost, not per-frame.
pub struct CanvasSampler<'a> {
    doc: &'a crate::doc::OpenDocument,
    surface_px: egui::Vec2,
    ppp: f32,
}

impl<'a> CanvasSampler<'a> {
    pub fn new(doc: &'a crate::doc::OpenDocument, surface_pt: egui::Vec2, ppp: f32) -> Self {
        Self {
            doc,
            surface_px: surface_pt * ppp,
            ppp,
        }
    }
}

impl ScreenSampler for CanvasSampler<'_> {
    fn sample(&self, screen_pos: [f32; 2]) -> Option<[f32; 4]> {
        // The dialogs pass egui's logical pointer position; the camera works
        // in physical pixels.
        let px = egui::vec2(screen_pos[0] * self.ppp, screen_pos[1] * self.ppp);
        if !px.x.is_finite() || !px.y.is_finite() {
            return None;
        }
        let surface_px = glam::Vec2::new(self.surface_px.x, self.surface_px.y);
        let px_glam = glam::Vec2::new(px.x, px.y);
        // Off the window is off the canvas.
        if px_glam.cmplt(glam::Vec2::ZERO).any() || px_glam.cmpgt(surface_px).any() {
            return None;
        }
        // The canvas area the document camera draws into, not the window.
        let viewport = crate::tool_input::canvas_viewport(&self.doc.camera);
        let mirror = crate::tool_input::canvas_camera_of(&self.doc.camera);
        let doc_pt = mirror.doc_of_screen_pt(&viewport, px_glam);
        let (w, h) = (
            self.doc.document.width() as i64,
            self.doc.document.height() as i64,
        );
        let x = doc_pt.x.floor() as i64;
        let y = doc_pt.y.floor() as i64;
        if x < 0 || y < 0 || x >= w || y >= h {
            return None;
        }
        // The free compositor, not the cached one: the sampler holds a shared
        // borrow of the document, and a 1×1 composite is a single read.
        let canvas = compositor::composite_region(
            &self.doc.document,
            &self.doc.tiles,
            raster::PixelRect::new(x, y, 1, 1),
            0,
            compositor::CompositeOptions::default(),
        )
        .ok()?;
        let rgba = canvas.to_rgba8(&self.doc.document.meta.color_space);
        Some([
            f32::from(rgba[0]) / 255.0,
            f32::from(rgba[1]) / 255.0,
            f32::from(rgba[2]) / 255.0,
            f32::from(rgba[3]) / 255.0,
        ])
    }
}

/// Whether the active document has a live selection -- the gate Select >
/// Modify and Save Selection share with their menu rows (`Selection::None`
/// has no bounds, so it does not count).
fn has_live_selection(editor: &crate::Editor) -> bool {
    editor
        .active()
        .is_some_and(|open| open.document.selection.bounds().is_some())
}

/// The names of the document's saved selections, oldest first -- what the
/// Load dialog lists and the Save dialog checks a new name against.
pub(crate) fn saved_selection_names(doc: &editor_core::Document) -> Vec<String> {
    doc.saved_selections
        .iter()
        .map(|(name, _)| name.clone())
        .collect()
}

/// A [`ui::dialogs::ColorRangeDialog`] over the active pixel layer, starting
/// from the foreground colour. `None` without a pixel layer; the dialog keeps
/// only a bounded preview copy of the pixels.
fn color_range_dialog(editor: &crate::Editor) -> Option<ActiveDialog> {
    let open = editor.active()?;
    let id = open.document.active_layer()?;
    let layer = open.document.layers.get(id)?;
    if !matches!(
        layer.kind,
        layer_model::LayerKind::Raster(_) | layer_model::LayerKind::Generator(_)
    ) {
        return None;
    }
    let rgba = crate::menu_bridge::pixels::read_layer(open, id);
    Some(ActiveDialog::ColorRange(Box::new(
        ui::dialogs::ColorRangeDialog::new(
            crate::menu_bridge::rgba8_of(editor.foreground()),
            &rgba,
            open.document.width(),
            open.document.height(),
        ),
    )))
}

/// The open W3-H Select-menu dialogs, for tests that drive them.
#[cfg(test)]
impl DialogHost {
    pub(crate) fn active_color_range_dialog_for_test(
        &mut self,
    ) -> &mut ui::dialogs::ColorRangeDialog {
        match self.active_for_test() {
            ActiveDialog::ColorRange(dialog) => dialog,
            other => panic!("the active dialog is {other:?}, not the color range dialog"),
        }
    }

    pub(crate) fn active_selection_modify_dialog_for_test(
        &mut self,
    ) -> &mut ui::dialogs::SelectionModifyDialog {
        match self.active_for_test() {
            ActiveDialog::SelectionModify(dialog) => dialog,
            other => panic!("the active dialog is {other:?}, not a Modify dialog"),
        }
    }

    pub(crate) fn active_save_selection_dialog_for_test(
        &mut self,
    ) -> &mut ui::dialogs::SaveSelectionDialog {
        match self.active_for_test() {
            ActiveDialog::SaveSelection(dialog) => dialog,
            other => panic!("the active dialog is {other:?}, not Save Selection"),
        }
    }

    pub(crate) fn active_load_selection_dialog_for_test(
        &mut self,
    ) -> &mut ui::dialogs::LoadSelectionDialog {
        match self.active_for_test() {
            ActiveDialog::LoadSelection(dialog) => dialog,
            other => panic!("the active dialog is {other:?}, not Load Selection"),
        }
    }
}

/// A [`LayerStyleDialog`] over the active layer's effects.
fn layer_style_dialog(editor: &crate::Editor, blending: bool) -> Option<ActiveDialog> {
    let open = editor.active()?;
    let id = open.document.active_layer()?;
    let layer = open.document.layers.get(id)?;
    let mut dialog = LayerStyleDialog::new(id, layer.name.clone(), layer.effects.clone())
        .with_blending(layer.blend_mode, layer.opacity, layer.fill_opacity)
        .with_patterns(crate::doc::pattern_tiles(editor.presets()))
        // W9-H: the style presets (defined ones and `.asl` imports) as the
        // Styles page's grid.
        .with_styles(crate::menu_bridge::asl_import::style_presets(
            editor.presets(),
        ));
    if blending {
        dialog.show_blending();
    }
    Some(ActiveDialog::LayerStyle(Box::new(dialog)))
}

/// A [`ui::dialogs::NewGuideDialog`] over the active document's guide set
/// and canvas size.
fn new_guide_dialog(editor: &crate::Editor) -> Option<ActiveDialog> {
    let open = editor.active()?;
    Some(ActiveDialog::NewGuide(Box::new(
        ui::dialogs::NewGuideDialog::new(
            open.document.guides.clone(),
            (open.document.width(), open.document.height()),
        ),
    )))
}

/// W9-K: a [`ui::dialogs::WarpTextDialog`] over the active text layer's
/// payload. `None` when the active layer is not an unlocked text layer (the
/// menu gates the same way; the bridge names the reason).
fn warp_text_dialog(editor: &crate::Editor) -> Option<ActiveDialog> {
    let open = editor.active()?;
    let id = open.document.active_layer()?;
    let layer = open.document.layers.get(id)?;
    match &layer.kind {
        layer_model::LayerKind::Text(text) if !layer.locked.all => Some(ActiveDialog::WarpText(
            Box::new(ui::dialogs::WarpTextDialog::new(id, text.clone())),
        )),
        _ => None,
    }
}

/// A [`ui::dialogs::RenameLayerDialog`] over the active layer's name. `None`
/// when there is no layer, or when the layer's blanket lock refuses a rename
/// (the menu gates the same way; this is the second line of defence).
fn rename_layer_dialog(editor: &crate::Editor) -> Option<ActiveDialog> {
    let open = editor.active()?;
    let id = open.document.active_layer()?;
    let layer = open.document.layers.get(id)?;
    if layer.locked.all {
        return None;
    }
    Some(ActiveDialog::RenameLayer(Box::new(
        ui::dialogs::RenameLayerDialog::new(id, layer.name.clone()),
    )))
}

/// A [`ui::dialogs::DuplicateLayerDialog`] over the active layer, opening at
/// "<name> copy". `None` when there is no layer to copy.
fn duplicate_layer_dialog(editor: &crate::Editor) -> Option<ActiveDialog> {
    let open = editor.active()?;
    let id = open.document.active_layer()?;
    let layer = open.document.layers.get(id)?;
    // W9-I: Destination ▸ Document lists every open document, by identity.
    let destinations = editor
        .documents()
        .iter()
        .map(|d| ui::dialogs::duplicate_layer::DuplicateDestination {
            key: d.id().0,
            title: d.title().to_string(),
        })
        .collect();
    Some(ActiveDialog::DuplicateLayer(Box::new(
        ui::dialogs::DuplicateLayerDialog::new(id, &layer.name)
            .with_destinations(open.id().0, destinations),
    )))
}

/// An [`ImageSizeDialog`] over the active document's size.
///
/// `editor_core::DocumentMeta` records no print resolution, so the dialog
/// starts from the 72 ppi its presets assume; a confirmed spec that only
/// changes the ppi resamples nothing and is a no-op the shell reports.
fn image_size_dialog(editor: &crate::Editor) -> Option<ActiveDialog> {
    let open = editor.active()?;
    // W3-G: the fields open in the Units preference.
    Some(ActiveDialog::ImageSize(Box::new(
        ImageSizeDialog::new(open.document.width(), open.document.height(), 72.0)
            .with_unit(editor.display_unit()),
    )))
}

/// W9-B: the active layer and its fill, when it is a live fill layer.
fn active_fill_layer(
    editor: &crate::Editor,
) -> Option<(layer_model::LayerId, layer_model::FillLayer)> {
    let doc = editor.active()?;
    let id = doc.document.active_layer()?;
    match &doc.document.layers.get(id)?.kind {
        layer_model::LayerKind::Fill(fill) => Some((id, fill.clone())),
        _ => None,
    }
}

/// W9-B: the dialog for a NEW fill layer of `kind`, seeded the way Photopea
/// seeds it: the foreground colour; a foreground-to-background ramp; the
/// latest defined pattern. `None` with no document, and for a Pattern fill
/// with no pattern defined — the bridge then refuses loudly with the reason.
fn new_fill_layer_dialog(
    editor: &crate::Editor,
    kind: ui::menu::FillLayerKind,
) -> Option<ActiveDialog> {
    editor.active()?;
    let patterns = crate::doc::pattern_tiles(editor.presets());
    let clamp = |c: [f32; 4]| c.map(|v| v.clamp(0.0, 1.0));
    let source = match kind {
        ui::menu::FillLayerKind::SolidColor => layer_model::FillSource::Solid {
            color: clamp(editor.foreground()),
        },
        ui::menu::FillLayerKind::Gradient => {
            layer_model::FillSource::Gradient(layer_model::GradientFill {
                gradient: layer_model::Gradient {
                    stops: vec![
                        layer_model::GradientStop {
                            position: 0.0,
                            color: clamp(editor.foreground()),
                            midpoint: 0.5,
                        },
                        layer_model::GradientStop {
                            position: 1.0,
                            color: clamp(editor.background()),
                            midpoint: 0.5,
                        },
                    ],
                    ..layer_model::Gradient::default()
                },
                ..layer_model::GradientFill::default()
            })
        }
        ui::menu::FillLayerKind::Pattern => {
            let latest = patterns.last()?.clone();
            layer_model::FillSource::Pattern(layer_model::PatternFill {
                tile: Some(latest),
                ..layer_model::PatternFill::default()
            })
        }
    };
    Some(ActiveDialog::FillLayer(Box::new(
        ui::dialogs::FillLayerDialog::new_layer(source, patterns),
    )))
}

/// W9-B: the dialog re-editing the active fill layer's source.
fn edit_fill_layer_dialog(editor: &crate::Editor) -> Option<ActiveDialog> {
    let (id, fill) = active_fill_layer(editor)?;
    Some(ActiveDialog::FillLayer(Box::new(
        ui::dialogs::FillLayerDialog::edit_layer(
            id,
            fill,
            crate::doc::pattern_tiles(editor.presets()),
        ),
    )))
}

/// A [`FilterDialog`] over the active layer's pixels for one filter's schema.
/// A [`ui::dialogs::FillDialog`] seeded from the editor's wells. The pattern
/// list is the asset store's, which is empty until the preset-store task adds
/// it — the dialog refuses the Pattern kind while that list is empty.
fn fill_dialog(editor: &crate::Editor) -> Option<ActiveDialog> {
    pixel_layer_available(editor)?;
    Some(ActiveDialog::Fill(Box::new(ui::dialogs::FillDialog::new(
        ui::dialogs::FillSpec {
            contents: ui::dialogs::FillContents::Foreground,
            ..Default::default()
        },
        editor.presets().pattern_names(),
    ))))
}

/// A [`ui::dialogs::StrokeDialog`] at Photopea's opening defaults.
fn stroke_dialog(editor: &crate::Editor) -> Option<ActiveDialog> {
    pixel_layer_available(editor)?;
    Some(ActiveDialog::Stroke(Box::new(
        ui::dialogs::StrokeDialog::new(ui::dialogs::StrokeSpec::default()),
    )))
}

/// Whether the fill/stroke engines would find something to paint on. The
/// availability check mirrors [`crate::menu_bridge::pixel_layer`] so the
/// dialog opens exactly when the bridge could perform.
fn pixel_layer_available(editor: &crate::Editor) -> Option<()> {
    let doc = editor.active()?;
    let id = doc.document.active_layer()?;
    let layer = doc.document.layers.get(id)?;
    matches!(
        &layer.kind,
        layer_model::LayerKind::Raster(_) | layer_model::LayerKind::Generator(_)
    )
    .then_some(())
}

fn filter_dialog_for(editor: &crate::Editor, id: ui::menu::FilterId) -> Option<ActiveDialog> {
    let spec = ui::dialogs::filter_by_id(id)?;
    // W7-E: a Layers-panel double-click on a smart filter re-opens its dialog
    // at the parameters it stored (and arms the confirm to replace it).
    let stored = crate::menu_bridge::arm_smart_filter_edit(editor, id);
    let source = crate::menu_bridge::filter_dialog_source(editor, id)?;
    let mut dialog = FilterDialog::new(spec, source);
    for (key, value) in stored {
        dialog.set_param(&key, value);
    }
    Some(ActiveDialog::Filter(Box::new(dialog)))
}

/// An [`AdjustmentDialog`] over the active pixel layer, previewing in the
/// document's colour space.
///
/// Gated the way [`crate::menu_bridge::perform`]'s adjustment arm is — the
/// active layer must own pixels — so the dialog never opens over a layer its
/// confirmation could not edit. The preview source is the same buffer the
/// filter dialogs preview against; the dialog bounds it itself.
fn adjustment_dialog(editor: &crate::Editor, id: AdjustmentId) -> Option<ActiveDialog> {
    // Desaturate and Equalize ask nothing (W4-E): the pick falls through to
    // the bridge, which applies them on the click.
    if !id.has_dialog() {
        return None;
    }
    pixel_layer_available(editor)?;
    let source = crate::menu_bridge::filter_source(editor)?;
    let space = editor.active()?.document.meta.color_space.clone();
    let mut dialog = AdjustmentDialog::new(id, source, space);
    // W8-B: a Lab document's Levels/Curves list and run on L, a and b.
    dialog.set_lab_channels(
        editor.active()?.document.meta.color_mode == editor_core::color_mode::mode::LAB,
    );
    // W7-G: Match Color's Source list — every open document and layer.
    if id == AdjustmentId::MatchColor {
        dialog.set_match_sources(match_color_sources(editor));
    }
    Some(ActiveDialog::Adjustment(Box::new(dialog)))
}

/// Largest side a Match Color source is measured at. Its statistics are
/// taken from a box-downsampled copy, so opening the dialog over many large
/// layers stays quick; the target is measured at full size when applied.
const MATCH_SOURCE_SIDE: u32 = 1024;

/// W7-G: what Match Color can take its colours from — every open document's
/// merged image, then every pixel layer of every document except the layer
/// being changed — each with its CIELAB statistics.
fn match_color_sources(editor: &crate::Editor) -> Vec<ui::dialogs::adjustment_dialog::MatchSource> {
    use ui::dialogs::adjustment_dialog::{preview_proxy, MatchSource};
    let stats_of = |w: u32, h: u32, rgba: &[u8]| {
        let buffer = filters::FilterBuffer::from_rgba8(w, h, rgba).ok()?;
        adjustments::LabStats::measure(preview_proxy(&buffer, MATCH_SOURCE_SIDE).pixels(), None)
    };
    let active_doc = editor.active().map(|d| d.id());
    let active_layer = editor.active().and_then(|d| d.document.active_layer());
    let mut out = Vec::new();
    for open in editor.documents() {
        let (w, h) = (open.document.width(), open.document.height());
        if w == 0 || h == 0 {
            continue;
        }
        let space = open.document.meta.color_space.clone();
        if let Ok(canvas) = compositor::composite_region(
            &open.document,
            &open.tiles,
            open.canvas_rect(),
            0,
            compositor::CompositeOptions::default(),
        ) {
            if let Some(stats) = stats_of(w, h, &canvas.to_rgba8(&space)) {
                out.push(MatchSource {
                    label: format!(
                        "{} ({})",
                        open.title(),
                        ui::strings::tr("ui.adjustment.match.merged")
                    ),
                    stats,
                });
            }
        }
    }
    for open in editor.documents() {
        let (w, h) = (open.document.width(), open.document.height());
        if w == 0 || h == 0 {
            continue;
        }
        for id in open.document.layers.iter_depth_first() {
            if Some(open.id()) == active_doc && Some(id) == active_layer {
                continue;
            }
            let Some(layer) = open.document.layers.get(id) else {
                continue;
            };
            if !matches!(
                layer.kind,
                layer_model::LayerKind::Raster(_) | layer_model::LayerKind::Generator(_)
            ) {
                continue;
            }
            let rgba = crate::menu_bridge::pixels::read_layer(open, id);
            if let Some(stats) = stats_of(w, h, &rgba) {
                out.push(MatchSource {
                    label: format!("{} / {}", open.title(), layer.name),
                    stats,
                });
            }
        }
    }
    out
}

/// W4-E round 2: the dialog for the active Color Lookup layer's own table,
/// previewing over the composite with that layer hidden (what the layer
/// adjusts). `None` when the active layer is not a Color Lookup adjustment
/// layer.
fn adjustment_layer_dialog(editor: &crate::Editor) -> Option<ActiveDialog> {
    let open = editor.active()?;
    let layer = open.document.active_layer()?;
    let layer_model::LayerKind::Adjustment(adjustment) = &open.document.layers.get(layer)?.kind
    else {
        return None;
    };
    // Only Color Lookup for now: its table cannot be chosen anywhere else.
    // The other dialog-only kinds keep the Properties road — Curves' dialog
    // resamples a curve to five points, which would lose a layer's own.
    if !matches!(
        adjustment.kind,
        layer_model::AdjustmentKind::ColorLookup { .. }
    ) {
        return None;
    }
    let mut beneath = open.document.clone();
    beneath.layers.get_mut(layer)?.visible = false;
    let (w, h) = (open.document.width(), open.document.height());
    let canvas = compositor::composite_region(
        &beneath,
        &open.tiles,
        open.canvas_rect(),
        0,
        compositor::CompositeOptions::default(),
    )
    .ok()?;
    let space = open.document.meta.color_space.clone();
    let source = filters::FilterBuffer::from_rgba8(w, h, &canvas.to_rgba8(&space)).ok()?;
    let dialog = AdjustmentDialog::for_layer(layer, adjustment.kind.clone(), source, space)?;
    Some(ActiveDialog::Adjustment(Box::new(dialog)))
}

/// Card 060: a [`RefineMaskDialog`] over the active layer's RAW pixels
/// (card 059 review lesson: `layer_pixels` already carries the enabled mask
/// in its alpha — the preview would double-apply the baseline and hide the
/// expand/outer-feather effect) and its mask's pose-aware baseline coverage.
/// `None` when there is no layer or no mask to refine (the menu gates the
/// same way; this is the second line of defence).
///
/// RECORDED (row 060 / T043 remainder, the apply_mask mixed-space family):
/// `read_layer` lays the store 1:1 onto canvas indices, so on a TRANSFORMED
/// layer the preview's content and the pose-aware coverage only align at an
/// identity layer transform — the confirmation itself writes through the
/// pose-aware writer either way.
fn refine_mask_dialog(editor: &crate::Editor) -> Option<ActiveDialog> {
    let open = editor.active()?;
    let layer = open.document.active_layer()?;
    // The second line of defence behind the menu's gate: no mask, no dialog.
    open.document.layers.get(layer)?.mask.as_ref()?;
    let (w, h) = (open.document.width(), open.document.height());
    let command = {
        let doc = editor.active()?;
        crate::menu_bridge::pixels::read_layer(doc, layer)
    };
    let baseline = {
        let doc = editor.active()?;
        crate::menu_bridge::read_mask_coverage(doc, layer, w, h)
    };
    Some(ActiveDialog::RefineMask(Box::new(
        ui::dialogs::refine_mask::RefineMaskDialog::new(command, baseline, w, h),
    )))
}

/// Card 062: a [`DefringeDialog`] over the active layer's RAW pixels and
/// its mask's pose-aware canvas-space coverage. `None` when there is no
/// layer or no mask (the menu gates the same way; this is the second line
/// of defence). The mixed-space caveat of [`refine_mask_dialog`] applies
/// identically: on a TRANSFORMED layer the preview aligns only at an
/// identity layer transform; the confirmation writes through the pose-aware
/// pixel writer either way.
fn defringe_dialog(editor: &crate::Editor) -> Option<ActiveDialog> {
    let open = editor.active()?;
    let layer = open.document.active_layer()?;
    open.document.layers.get(layer)?.mask.as_ref()?;
    let (w, h) = (open.document.width(), open.document.height());
    let command = {
        let doc = editor.active()?;
        crate::menu_bridge::pixels::read_layer(doc, layer)
    };
    let coverage = {
        let doc = editor.active()?;
        crate::menu_bridge::read_mask_coverage(doc, layer, w, h)
    };
    Some(ActiveDialog::Defringe(Box::new(
        ui::dialogs::defringe::DefringeDialog::new(command, coverage, w, h),
    )))
}

/// The [`FilterGalleryDialog`] over the active layer's pixels.
fn filter_gallery_dialog(editor: &crate::Editor) -> Option<ActiveDialog> {
    // W7-E: the Gallery always adds a filter; it never finishes an earlier
    // smart-filter re-edit, so whatever was armed is dropped here.
    crate::menu_bridge::disarm_smart_filter_edit();
    let source = crate::menu_bridge::filter_source(editor)?;
    Some(ActiveDialog::FilterGallery(Box::new(
        ui::dialogs::FilterGalleryDialog::new(source),
    )))
}

/// A [`CanvasSizeDialog`] over the active document's size.
fn canvas_size_dialog(editor: &crate::Editor) -> Option<ActiveDialog> {
    let open = editor.active()?;
    let mut dialog = CanvasSizeDialog::new(open.document.width(), open.document.height(), 72.0);
    // W3-G: the fields open in the Units preference.
    dialog.set_unit(editor.display_unit());
    Some(ActiveDialog::CanvasSize(Box::new(dialog)))
}

/// An [`ExportAsDialog`] over the active document: its size, its title as the
/// base file name, a placeholder proxy for the live preview, and its one row
/// set to `format` — the format the menu row the user came in through names.
/// A JPEG row keeps the dialog's default quality rather than inventing one.
///
/// The real preview is a downscaled *composite*, which needs `&mut` to run the
/// compositor — [`crate::chrome::Chrome::ui`] swaps it in on the frame after
/// the dialog opens, through [`DialogHost::refresh_preview`].
fn export_as_dialog(editor: &crate::Editor, format: raster::ExportFormat) -> Option<ActiveDialog> {
    let open = editor.active()?;
    let (w, h) = (open.document.width(), open.document.height());
    let name = open.title().to_string();
    let proxy = ui::dialogs::PreviewSource::placeholder(
        ui::dialogs::export_as::MAX_PROXY_SIDE.min(w.max(1)),
        ui::dialogs::export_as::MAX_PROXY_SIDE.min(h.max(1)),
    );
    let mut dialog = ui::dialogs::ExportAsDialog::new(w, h, name, proxy);
    dialog.set_format(format);
    // W7-D: so the dialog can say how a Lab / CMYK / Indexed document goes out.
    dialog.set_color_mode(open.document.meta.color_mode);
    // W9-J: the Animated option is offered only when the document has `_a_`
    // frame layers, and its caption names how many.
    dialog.set_animation_frames(crate::import::animation_frame_layers(&open.document).len());
    Some(ActiveDialog::ExportAs(Box::new(dialog)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialogs::ScriptedDialogs;
    use crate::editor::Editor;
    use crate::prefs::{AppPaths, Preferences};
    use crate::recent::RecentFiles;
    use std::path::PathBuf;

    fn editor(dir: &std::path::Path) -> Editor {
        Editor::with_state(
            AppPaths::rooted(dir),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        )
    }

    fn png(dir: &std::path::Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(
            &path,
            raster::encode(raster::ExportFormat::Png, 8, 8, &[9u8; 8 * 8 * 4]).unwrap(),
        )
        .unwrap();
        path
    }

    /// How much history the active document has. Opening a dialog, replacing
    /// one and closing one must never move it: a dialog is view state.
    fn history_len(ed: &Editor) -> usize {
        ed.active().unwrap().history.journal().count()
    }

    #[test]
    fn a_layer_style_menu_action_opens_the_dialog_instead_of_toggling() {
        let dir = tempfile::tempdir().unwrap();
        let p = png(dir.path(), "a.png");
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&p).unwrap();
        let history = history_len(&ed);

        let mut host = DialogHost::default();
        assert!(
            host.open_for_menu_action(&ui::menu::MenuAction::FilterGallery, &ed),
            "the gallery opened its dialog"
        );
        host.close();
        assert!(
            !host.open_for_menu_action(&ui::menu::MenuAction::FileInfo, &ed),
            "an action with no dialog yet is left to the bridge"
        );
        assert!(host.open_for_menu_action(&ui::menu::MenuAction::NewDocument, &ed));
        assert!(host.is_open(), "File ▸ New opened its dialog");
        host.close();
        assert!(
            host.open_for_menu_action(
                &ui::menu::MenuAction::Export(raster::ExportFormat::Png),
                &ed
            ),
            "File ▸ Export As opened its dialog"
        );
        host.close();
        assert!(host.open_for_menu_action(
            &ui::menu::MenuAction::LayerStyle(ui::menu::EffectSlot::DropShadow),
            &ed
        ));
        assert!(host.is_open(), "the dialog opened");

        // Image ▸ Image Size and Canvas Size are hosted now too.
        assert!(host.open_for_menu_action(&ui::menu::MenuAction::ImageSize, &ed));
        assert!(host.is_open());
        assert!(host.open_for_menu_action(&ui::menu::MenuAction::CanvasSize, &ed));
        assert!(host.is_open());

        // Opening again replaces rather than stacks.
        assert!(host.open_for_menu_action(
            &ui::menu::MenuAction::LayerStyle(ui::menu::EffectSlot::Stroke),
            &ed
        ));
        host.close();
        assert!(!host.is_open());
        assert_eq!(
            history,
            history_len(&ed),
            "opening, replacing and closing dialogs never touched the document"
        );
    }

    #[test]
    fn the_export_row_seeds_the_dialog_with_the_format_it_names() {
        // Six rows, one dialog: the row is the way in, and the dialog opens on
        // that row's format rather than always on PNG. A JPEG row keeps the
        // dialog's own default quality.
        let dir = tempfile::tempdir().unwrap();
        let p = png(dir.path(), "a.png");
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&p).unwrap();
        let mut host = DialogHost::default();
        for format in raster::ExportFormat::ALL.iter().copied() {
            assert!(
                host.open_for_menu_action(&ui::menu::MenuAction::Export(format), &ed),
                "{format:?} opened the dialog"
            );
            let seeded = host.active_export_dialog_for_test().format();
            match format {
                raster::ExportFormat::Jpeg(_) => {
                    assert!(
                        matches!(seeded, raster::ExportFormat::Jpeg(_)),
                        "the JPEG row opened on {seeded:?}"
                    );
                }
                other => assert_eq!(seeded, other, "the row opened on another format"),
            }
            host.close();
        }
    }

    fn raw_input(events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1400.0, 900.0),
            )),
            events,
            ..Default::default()
        }
    }

    fn key(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        }
    }

    /// One document with two pixel layers, the topmost active — the fixture
    /// the menu-bridge gates use.
    fn two_layers(dir: &std::path::Path) -> Editor {
        let p = png(dir, "two.png");
        let mut ed = editor(&dir.join("config"));
        ed.open_path(&p).unwrap();
        let second = layer_model::Layer::raster("Second");
        let id = second.id;
        ed.apply_command(editor_core::Command::create_layer(second));
        ed.set_active_layer(id);
        ed
    }

    #[test]
    fn levels_opens_its_dialog_from_the_menu_action_and_every_adjustment_does() {
        // The finding: ten of the fifteen Image ▸ Adjustments rows were
        // greyed because no dialog existed, and the other five ran at their
        // defaults. Every one opens a dialog now, and opening moves no
        // history.
        let dir = tempfile::tempdir().unwrap();
        let ed = two_layers(dir.path());
        let history = history_len(&ed);
        let mut host = DialogHost::default();
        assert!(host.open_for_menu_action(
            &ui::menu::MenuAction::ApplyAdjustment(AdjustmentId::Levels),
            &ed
        ));
        assert!(host.is_open(), "Levels did not open a dialog");
        assert_eq!(
            host.active_adjustment_dialog_for_test().id(),
            AdjustmentId::Levels
        );
        assert!(
            host.active_adjustment_dialog_for_test()
                .histogram()
                .is_some(),
            "Levels opened without its histogram"
        );
        for id in AdjustmentId::ALL {
            host.close();
            if !id.has_dialog() {
                // Desaturate and Equalize apply on the click, as in Photopea.
                assert!(
                    !host.open_for_menu_action(&ui::menu::MenuAction::ApplyAdjustment(*id), &ed),
                    "{id:?} opened a dialog"
                );
                assert!(!host.is_open());
                continue;
            }
            assert!(
                host.open_for_menu_action(&ui::menu::MenuAction::ApplyAdjustment(*id), &ed),
                "{id:?} opened no dialog"
            );
            assert_eq!(host.active_adjustment_dialog_for_test().id(), *id);
            // The preview source is the layer, bounded: an 8x8 probe stays 8x8.
            assert_eq!(
                host.active_adjustment_dialog_for_test()
                    .source()
                    .dimensions(),
                (8, 8)
            );
        }
        assert_eq!(
            history,
            history_len(&ed),
            "opening dialogs touched the document"
        );
    }

    #[test]
    fn a_confirmed_adjustment_travels_as_the_menu_pick_with_its_parameters_parked() {
        // The confirmation channel this host owns: Enter on a moved dialog
        // puts exactly one `ApplyAdjustment` pick in `out.menu` — the road
        // the click used to take — and parks the parameters for the arm that
        // performs it. Nothing rides `out.commands` or `out.dialog`.
        let dir = tempfile::tempdir().unwrap();
        let ed = two_layers(dir.path());
        let mut host = DialogHost::default();
        assert!(host.open_for_menu_action(
            &ui::menu::MenuAction::ApplyAdjustment(AdjustmentId::BrightnessContrast),
            &ed
        ));
        let moved = layer_model::AdjustmentKind::BrightnessContrast {
            brightness: 0.5,
            contrast: 0.0,
        };
        assert!(host
            .active_adjustment_dialog_for_test()
            .set_kind(moved.clone()));
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let mut out = ChromeOutput::default();
        // A settle frame so the dialog exists before the key arrives, then Enter.
        let _ = ctx.run(raw_input(Vec::new()), |ctx| host.ui(ctx, None, &mut out));
        assert!(host.is_open() && out.is_empty());
        let _ = ctx.run(raw_input(vec![key(egui::Key::Enter)]), |ctx| {
            host.ui(ctx, None, &mut out)
        });
        assert!(!host.is_open(), "Enter did not close the dialog");
        assert_eq!(
            out.menu,
            vec![ui::menu::MenuAction::ApplyAdjustment(
                AdjustmentId::BrightnessContrast
            )]
        );
        assert!(out.commands.is_empty(), "the confirmation leaked a command");
        assert!(
            out.dialog.is_none(),
            "the confirmation leaked a dialog action"
        );
        // Another id cannot take the parked parameters; the right one takes
        // them exactly once.
        assert_eq!(take_confirmed_adjustment(AdjustmentId::Levels), None);
        assert_eq!(
            take_confirmed_adjustment(AdjustmentId::BrightnessContrast),
            Some(moved)
        );
        assert_eq!(
            take_confirmed_adjustment(AdjustmentId::BrightnessContrast),
            None,
            "a confirmation applied twice"
        );
    }

    #[test]
    fn cancelling_an_adjustment_dialog_parks_and_emits_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let ed = two_layers(dir.path());
        let mut host = DialogHost::default();
        assert!(host.open_for_menu_action(
            &ui::menu::MenuAction::ApplyAdjustment(AdjustmentId::Threshold),
            &ed
        ));
        host.active_adjustment_dialog_for_test()
            .set_kind(layer_model::AdjustmentKind::Threshold { level: 0.2 });
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let mut out = ChromeOutput::default();
        let _ = ctx.run(raw_input(Vec::new()), |ctx| host.ui(ctx, None, &mut out));
        let _ = ctx.run(raw_input(vec![key(egui::Key::Escape)]), |ctx| {
            host.ui(ctx, None, &mut out)
        });
        assert!(!host.is_open(), "Escape did not close the dialog");
        assert!(out.is_empty(), "cancelling produced {out:?}");
        assert_eq!(take_confirmed_adjustment(AdjustmentId::Threshold), None);
    }

    #[test]
    fn an_identity_adjustment_does_not_confirm_on_enter() {
        // Ten adjustments open at their identity. Enter on one leaves it open
        // with its reason showing rather than parking a no-op.
        let dir = tempfile::tempdir().unwrap();
        let ed = two_layers(dir.path());
        let mut host = DialogHost::default();
        assert!(host.open_for_menu_action(
            &ui::menu::MenuAction::ApplyAdjustment(AdjustmentId::Curves),
            &ed
        ));
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let mut out = ChromeOutput::default();
        let _ = ctx.run(raw_input(Vec::new()), |ctx| host.ui(ctx, None, &mut out));
        let _ = ctx.run(raw_input(vec![key(egui::Key::Enter)]), |ctx| {
            host.ui(ctx, None, &mut out)
        });
        assert!(host.is_open(), "an untouched Curves confirmed");
        assert!(out.is_empty());
        assert_eq!(take_confirmed_adjustment(AdjustmentId::Curves), None);
    }

    #[test]
    fn without_a_document_no_dialog_opens_and_the_intent_falls_through() {
        let dir = tempfile::tempdir().unwrap();
        let ed = editor(&dir.path().join("config"));
        let mut host = DialogHost::default();
        assert!(
            !host.open_for_menu_action(
                &ui::menu::MenuAction::LayerStyle(ui::menu::EffectSlot::DropShadow),
                &ed
            ),
            "there is no active layer to style"
        );
        assert!(!host.is_open());
        assert!(
            !host.open_for_menu_action(
                &ui::menu::MenuAction::ApplyAdjustment(AdjustmentId::Levels),
                &ed
            ),
            "there is no pixel layer to adjust"
        );
        assert!(!host.is_open());
    }

    // ---- W2-F: the dialogs the audit found missing ----------------------

    #[test]
    fn trim_confirms_to_a_parked_spec_and_the_trim_pick() {
        // The Trim dialog's confirmation is not a `DialogAction`: `ui` drives
        // it before the generic show, parks the options and sends the `Trim`
        // pick down the menu road, exactly as an adjustment travels.
        let dir = tempfile::tempdir().unwrap();
        let ed = two_layers(dir.path());
        let mut host = DialogHost::default();
        assert!(host.open_for_menu_action(&ui::menu::MenuAction::Trim, &ed));
        assert!(host.is_open(), "Trim… opened its dialog");
        let spec = ui::dialogs::TrimSpec {
            basis: ui::dialogs::TrimBasis::TopLeftColor,
            left: false,
            ..Default::default()
        };
        host.active_trim_dialog_for_test().set_spec(spec);
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let mut out = ChromeOutput::default();
        let _ = ctx.run(raw_input(Vec::new()), |ctx| host.ui(ctx, None, &mut out));
        assert!(
            host.is_open() && out.is_empty(),
            "a settle frame decides nothing"
        );
        let _ = ctx.run(raw_input(vec![key(egui::Key::Enter)]), |ctx| {
            host.ui(ctx, None, &mut out)
        });
        assert!(!host.is_open(), "Enter closed the dialog");
        assert_eq!(out.menu, vec![ui::menu::MenuAction::Trim]);
        assert!(out.commands.is_empty() && out.dialog.is_none());
        assert_eq!(take_confirmed_trim(), Some(spec));
        assert_eq!(take_confirmed_trim(), None, "a confirmation is taken once");
        // Cancelling parks nothing.
        assert!(host.open_for_menu_action(&ui::menu::MenuAction::Trim, &ed));
        let mut out = ChromeOutput::default();
        let _ = ctx.run(raw_input(Vec::new()), |ctx| host.ui(ctx, None, &mut out));
        let _ = ctx.run(raw_input(vec![key(egui::Key::Escape)]), |ctx| {
            host.ui(ctx, None, &mut out)
        });
        assert!(!host.is_open() && out.is_empty());
        assert_eq!(take_confirmed_trim(), None);
        // And without a document there is nothing to trim: no dialog.
        let empty = editor(&dir.path().join("config2"));
        assert!(!host.open_for_menu_action(&ui::menu::MenuAction::Trim, &empty));
    }

    #[test]
    fn duplicate_layer_confirms_to_a_parked_name_and_the_duplicate_pick() {
        // Same road as Trim: the name is not a `DialogAction`, so `ui`
        // drives the dialog itself, parks the name and sends the
        // `DuplicateLayer` pick down the menu road.
        let dir = tempfile::tempdir().unwrap();
        let ed = two_layers(dir.path());
        let mut host = DialogHost::default();
        assert_eq!(take_confirmed_duplicate_name(), None);
        assert!(host.open_for_menu_action(&ui::menu::MenuAction::DuplicateLayer, &ed));
        assert!(host.is_open(), "Duplicate Layer… opened its dialog");
        assert_eq!(
            host.active_duplicate_dialog_for_test().name(),
            "Second copy",
            "the field opens at Photoshop's suggestion"
        );
        host.active_duplicate_dialog_for_test().set_name("Twin");
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let mut out = ChromeOutput::default();
        let _ = ctx.run(raw_input(Vec::new()), |ctx| host.ui(ctx, None, &mut out));
        assert!(
            host.is_open() && out.is_empty(),
            "a settle frame decides nothing"
        );
        let _ = ctx.run(raw_input(vec![key(egui::Key::Enter)]), |ctx| {
            host.ui(ctx, None, &mut out)
        });
        assert!(!host.is_open(), "Enter closed the dialog");
        assert_eq!(out.menu, vec![ui::menu::MenuAction::DuplicateLayer]);
        assert!(out.commands.is_empty() && out.dialog.is_none());
        assert_eq!(take_confirmed_duplicate_name(), Some("Twin".to_string()));
        assert_eq!(
            take_confirmed_duplicate_name(),
            None,
            "a confirmation is taken once"
        );
        // Cancelling parks nothing.
        assert!(host.open_for_menu_action(&ui::menu::MenuAction::DuplicateLayer, &ed));
        let mut out = ChromeOutput::default();
        let _ = ctx.run(raw_input(Vec::new()), |ctx| host.ui(ctx, None, &mut out));
        let _ = ctx.run(raw_input(vec![key(egui::Key::Escape)]), |ctx| {
            host.ui(ctx, None, &mut out)
        });
        assert!(!host.is_open() && out.is_empty());
        assert_eq!(take_confirmed_duplicate_name(), None);
        // And without a document there is nothing to copy: no dialog.
        let empty = editor(&dir.path().join("config2"));
        assert!(!host.open_for_menu_action(&ui::menu::MenuAction::DuplicateLayer, &empty));
    }

    #[test]
    fn about_opens_from_the_menu_and_escape_closes_it() {
        let dir = tempfile::tempdir().unwrap();
        let ed = editor(&dir.path().join("config"));
        let mut host = DialogHost::default();
        // About needs no document at all.
        assert!(host.open_for_menu_action(&ui::menu::MenuAction::About, &ed));
        assert!(host.is_open());
        assert_eq!(
            host.active_about_dialog_for_test().version_line(),
            crate::version::about_line()
        );
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let mut out = ChromeOutput::default();
        let _ = ctx.run(raw_input(Vec::new()), |ctx| host.ui(ctx, None, &mut out));
        assert!(host.is_open(), "the window stays until dismissed");
        let _ = ctx.run(raw_input(vec![key(egui::Key::Escape)]), |ctx| {
            host.ui(ctx, None, &mut out)
        });
        assert!(!host.is_open(), "Escape closed it");
        assert!(
            out.is_empty(),
            "a window that asks nothing produces nothing"
        );
    }

    #[test]
    fn the_w2f_rows_open_their_dialogs_only_over_what_they_need() {
        let dir = tempfile::tempdir().unwrap();
        let ed = two_layers(dir.path());
        let history = history_len(&ed);
        let mut host = DialogHost::default();
        // Rename and New Guide open over a layer / a document.
        assert!(host.open_for_menu_action(&ui::menu::MenuAction::RenameLayer, &ed));
        assert_eq!(host.active_rename_dialog_for_test().name(), "Second");
        assert!(host.open_for_menu_action(&ui::menu::MenuAction::NewGuide, &ed));
        assert!(host.is_open());
        // Blending Options is the Layer Style dialog.
        assert!(host.open_for_menu_action(&ui::menu::MenuAction::BlendingOptions, &ed));
        assert!(host.layer_style_is_open_for_test());
        // Refine Edge needs a selection: none here, so it falls through.
        assert!(!host.open_for_menu_action(&ui::menu::MenuAction::RefineEdge, &ed));
        host.close();
        assert_eq!(
            history,
            history_len(&ed),
            "opening dialogs touched the document"
        );
        // Without a document none of them opens.
        let empty = editor(&dir.path().join("config2"));
        for action in [
            ui::menu::MenuAction::RenameLayer,
            ui::menu::MenuAction::NewGuide,
            ui::menu::MenuAction::BlendingOptions,
            ui::menu::MenuAction::RefineEdge,
            ui::menu::MenuAction::Trim,
        ] {
            assert!(
                !host.open_for_menu_action(&action, &empty),
                "{action:?} opened over nothing"
            );
        }
    }

    // ---- W2-X: Blending Options is a page, and confirms as one step -------

    #[test]
    fn blending_options_opens_on_the_blending_page_and_confirms_one_undoable_step() {
        use ui::dialogs::layer_style::StylePage;
        // The defect this pins: Blending Options… opened the Layer Style
        // dialog on its Drop Shadow page, and the dialog had no page with
        // the layer's blend mode, opacity or fill at all.
        let dir = tempfile::tempdir().unwrap();
        let mut ed = two_layers(dir.path());
        let history = history_len(&ed);
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        let opacity = |ed: &Editor| {
            ed.active()
                .unwrap()
                .document
                .layers
                .get(layer)
                .unwrap()
                .opacity
        };
        assert_eq!(opacity(&ed), 1.0);

        let mut host = DialogHost::default();
        assert!(host.open_for_menu_action(&ui::menu::MenuAction::BlendingOptions, &ed));
        assert_eq!(
            host.active_layer_style_dialog_for_test().page(),
            StylePage::Blending,
            "Blending Options… opens on the Blending page"
        );
        // Control: a Layer Style effect row opens on an effect page, so the
        // page above is the row's doing and not the dialog's default.
        assert!(host.open_for_menu_action(
            &ui::menu::MenuAction::LayerStyle(ui::menu::EffectSlot::DropShadow),
            &ed
        ));
        assert!(matches!(
            host.active_layer_style_dialog_for_test().page(),
            StylePage::Effect(_)
        ));
        // Back on the Blending page, seeded from the layer's own fields.
        assert!(host.open_for_menu_action(&ui::menu::MenuAction::BlendingOptions, &ed));
        {
            let dialog = host.active_layer_style_dialog_for_test();
            assert_eq!(dialog.opacity(), 1.0);
            assert_eq!(dialog.fill_opacity(), 1.0);
            assert_eq!(dialog.blend_mode(), layer_model::BlendMode::Normal);
            dialog.set_opacity(0.4);
        }

        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let mut out = ChromeOutput::default();
        // A settle frame draws the page: its three controls are on screen.
        let _ = ctx.run(raw_input(Vec::new()), |ctx| host.ui(ctx, None, &mut out));
        assert!(host.is_open() && out.is_empty());
        for id in [
            ui::dialogs::ids::blending_mode(),
            ui::dialogs::ids::blending_opacity(),
            ui::dialogs::ids::blending_fill(),
        ] {
            assert!(ctx.read_response(id).is_some(), "{id:?} was not drawn");
        }
        // Enter confirms: exactly one command rides out, nothing else.
        let _ = ctx.run(raw_input(vec![key(egui::Key::Enter)]), |ctx| {
            host.ui(ctx, None, &mut out)
        });
        assert!(!host.is_open(), "Enter did not close the dialog");
        assert_eq!(out.commands.len(), 1, "one command: {:?}", out.commands);
        assert!(out.menu.is_empty() && out.dialog.is_none());
        for command in out.commands {
            ed.apply_command(command);
        }
        assert_eq!(
            history_len(&ed),
            history + 1,
            "the confirmed opacity was not exactly one history entry"
        );
        assert_eq!(opacity(&ed), 0.4);
        let depth = ed.active().unwrap().history_depth();
        assert!(ed.active_mut().unwrap().undo().unwrap());
        assert_eq!(
            opacity(&ed),
            1.0,
            "undo did not restore the layer's opacity"
        );
        assert_eq!(
            ed.active().unwrap().history_depth(),
            depth - 1,
            "one undo took the whole confirmation back"
        );
    }

    // ---- W3-H: Select-menu dialogs ------------------------------------------

    /// An 8x8 document whose left half is red and right half blue, the image
    /// layer active.
    fn halves(dir: &std::path::Path) -> Editor {
        let mut bytes = Vec::with_capacity(8 * 8 * 4);
        for _y in 0..8 {
            for x in 0..8 {
                bytes.extend_from_slice(if x < 4 {
                    &[255, 0, 0, 255]
                } else {
                    &[0, 0, 255, 255]
                });
            }
        }
        let path = dir.join("halves.png");
        std::fs::write(
            &path,
            raster::encode(raster::ExportFormat::Png, 8, 8, &bytes).unwrap(),
        )
        .unwrap();
        let mut ed = editor(&dir.join("config"));
        ed.open_path(&path).unwrap();
        ed
    }

    /// Run a settle frame and then an Enter frame over the host.
    fn settle_and_confirm(host: &mut DialogHost) -> ChromeOutput {
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let mut out = ChromeOutput::default();
        let _ = ctx.run(raw_input(Vec::new()), |ctx| host.ui(ctx, None, &mut out));
        assert!(
            host.is_open() && out.is_empty(),
            "a settle frame decides nothing"
        );
        let _ = ctx.run(raw_input(vec![key(egui::Key::Enter)]), |ctx| {
            host.ui(ctx, None, &mut out)
        });
        assert!(!host.is_open(), "Enter closed the dialog");
        out
    }

    /// A settle frame and an Enter frame over a dialog whose primary is
    /// blocked: it stays open and emits nothing.
    fn settle_and_confirm_blocked(host: &mut DialogHost) {
        let ctx = egui::Context::default();
        let mut out = ChromeOutput::default();
        let _ = ctx.run(raw_input(Vec::new()), |ctx| host.ui(ctx, None, &mut out));
        let _ = ctx.run(raw_input(vec![key(egui::Key::Enter)]), |ctx| {
            host.ui(ctx, None, &mut out)
        });
        assert!(
            host.is_open() && out.is_empty(),
            "a blocked Enter decides nothing"
        );
    }

    #[test]
    fn color_range_opens_a_previewing_dialog_and_its_confirmation_selects_as_one_step() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = halves(dir.path());
        let mut host = DialogHost::default();
        assert!(host.open_for_menu_action(&ui::menu::MenuAction::ColorRange, &ed));
        let before = history_len(&ed);
        {
            let dialog = host.active_color_range_dialog_for_test();
            assert_eq!(dialog.preview_size(), (8, 8));
            // A click on the preview's right half samples blue.
            assert!(dialog.sample_preview(6, 3));
            dialog.set_fuzziness(0);
            let preview = dialog.preview_coverage().unwrap();
            assert_eq!(preview[3], 0, "red is not in a blue range");
            assert_eq!(preview[4], 255, "blue is");
        }
        assert_eq!(history_len(&ed), before, "previewing is not an edit");
        let out = settle_and_confirm(&mut host);
        assert_eq!(out.menu, vec![ui::menu::MenuAction::ColorRange]);
        assert!(out.commands.is_empty() && out.dialog.is_none());
        let status = crate::menu_bridge::perform(ui::menu::MenuAction::ColorRange, &mut ed)
            .expect("the confirmed range selects");
        assert!(status.contains("#0000FF"), "{status}");
        let sel = &ed.active().unwrap().document.selection;
        for y in 0..8 {
            for x in 0..8 {
                let want = if x < 4 { 0.0 } else { 1.0 };
                assert_eq!(sel.coverage_at(glam::IVec2::new(x, y)), want, "({x}, {y})");
            }
        }
        assert_eq!(history_len(&ed), before + 1, "one undoable step");
        assert_eq!(take_confirmed_color_range(), None, "taken once");
        // Cancelling parks nothing and selects nothing.
        assert!(host.open_for_menu_action(&ui::menu::MenuAction::ColorRange, &ed));
        let ctx = egui::Context::default();
        let mut out = ChromeOutput::default();
        let _ = ctx.run(raw_input(vec![key(egui::Key::Escape)]), |ctx| {
            host.ui(ctx, None, &mut out)
        });
        assert!(!host.is_open() && out.is_empty());
        assert_eq!(take_confirmed_color_range(), None);
    }

    #[test]
    fn each_modify_row_asks_its_amount_and_the_confirmed_amount_is_the_one_applied() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = two_layers(dir.path());
        let mut host = DialogHost::default();
        // Without a live selection no Modify dialog opens.
        for op in ui::menu::ModifySelection::ALL {
            assert!(!host.open_for_menu_action(&ui::menu::MenuAction::Modify(*op), &ed));
        }
        ed.apply_command(editor_core::Command::SetSelection {
            selection: editor_core::Selection::Rect {
                min: glam::IVec2::new(3, 3),
                max: glam::IVec2::new(5, 5),
            },
        });
        let op = ui::menu::ModifySelection::Expand;
        assert!(host.open_for_menu_action(&ui::menu::MenuAction::Modify(op), &ed));
        host.active_selection_modify_dialog_for_test()
            .set_amount(2.0);
        let before = history_len(&ed);
        let out = settle_and_confirm(&mut host);
        assert_eq!(out.menu, vec![ui::menu::MenuAction::Modify(op)]);
        let status = crate::menu_bridge::perform(ui::menu::MenuAction::Modify(op), &mut ed)
            .expect("the confirmed expand applies");
        assert!(status.contains("by 2 px"), "{status}");
        assert_eq!(
            ed.active().unwrap().document.selection.bounds(),
            Some((glam::IVec2::new(1, 1), glam::IVec2::new(7, 7))),
            "expanded by the confirmed 2 px, not the default 4"
        );
        assert_eq!(history_len(&ed), before + 1);
        // A parked amount belongs to its own operation only.
        assert!(host.open_for_menu_action(
            &ui::menu::MenuAction::Modify(ui::menu::ModifySelection::Feather),
            &ed
        ));
        host.active_selection_modify_dialog_for_test()
            .set_amount(1.5);
        let _ = settle_and_confirm(&mut host);
        assert_eq!(
            take_confirmed_modify(ui::menu::ModifySelection::Border),
            None
        );
        assert_eq!(
            take_confirmed_modify(ui::menu::ModifySelection::Feather).map(|s| s.amount),
            Some(1.5)
        );
    }

    #[test]
    fn save_names_the_selection_and_load_lists_it_by_name_with_an_operation() {
        use ui::dialogs::LoadOperation;
        let dir = tempfile::tempdir().unwrap();
        let mut ed = two_layers(dir.path());
        let mut host = DialogHost::default();
        let left = editor_core::Selection::Rect {
            min: glam::IVec2::new(0, 0),
            max: glam::IVec2::new(4, 8),
        };
        let right = editor_core::Selection::Rect {
            min: glam::IVec2::new(4, 0),
            max: glam::IVec2::new(8, 8),
        };
        // Nothing saved yet: Load opens no dialog.
        assert!(!host.open_for_menu_action(&ui::menu::MenuAction::LoadSelection, &ed));

        ed.apply_command(editor_core::Command::SetSelection {
            selection: left.clone(),
        });
        assert!(host.open_for_menu_action(&ui::menu::MenuAction::SaveSelection, &ed));
        assert_eq!(
            host.active_save_selection_dialog_for_test().name(),
            "Alpha 1"
        );
        host.active_save_selection_dialog_for_test()
            .set_name("Left half");
        let out = settle_and_confirm(&mut host);
        assert_eq!(out.menu, vec![ui::menu::MenuAction::SaveSelection]);
        crate::menu_bridge::perform(ui::menu::MenuAction::SaveSelection, &mut ed).unwrap();

        ed.apply_command(editor_core::Command::SetSelection {
            selection: right.clone(),
        });
        assert!(host.open_for_menu_action(&ui::menu::MenuAction::SaveSelection, &ed));
        assert_eq!(
            host.active_save_selection_dialog_for_test().name(),
            "Alpha 1",
            "the first free Alpha name"
        );
        host.active_save_selection_dialog_for_test()
            .set_name("Left half");
        settle_and_confirm_blocked(&mut host);
        host.active_save_selection_dialog_for_test()
            .set_name("Right half");
        let _ = settle_and_confirm(&mut host);
        crate::menu_bridge::perform(ui::menu::MenuAction::SaveSelection, &mut ed).unwrap();
        assert_eq!(
            saved_selection_names(&ed.active().unwrap().document),
            vec!["Left half".to_string(), "Right half".to_string()]
        );

        // Load "Left half" as a new selection.
        ed.apply_command(editor_core::Command::SetSelection {
            selection: editor_core::Selection::None,
        });
        assert!(host.open_for_menu_action(&ui::menu::MenuAction::LoadSelection, &ed));
        {
            let dialog = host.active_load_selection_dialog_for_test();
            assert_eq!(dialog.names(), &["Left half", "Right half"]);
            dialog.set_operation(LoadOperation::Add);
            assert!(dialog.confirm().is_none(), "no live selection: only New");
            dialog.set_operation(LoadOperation::New);
            dialog.select(0);
        }
        let _ = settle_and_confirm(&mut host);
        let before = history_len(&ed);
        let status =
            crate::menu_bridge::perform(ui::menu::MenuAction::LoadSelection, &mut ed).unwrap();
        assert!(status.contains("Left half"), "{status}");
        assert_eq!(ed.active().unwrap().document.selection, left);
        assert_eq!(history_len(&ed), before + 1);

        // Add "Right half": the union covers the canvas.
        assert!(host.open_for_menu_action(&ui::menu::MenuAction::LoadSelection, &ed));
        {
            let dialog = host.active_load_selection_dialog_for_test();
            dialog.select(1);
            dialog.set_operation(LoadOperation::Add);
        }
        let _ = settle_and_confirm(&mut host);
        crate::menu_bridge::perform(ui::menu::MenuAction::LoadSelection, &mut ed).unwrap();
        let sel = &ed.active().unwrap().document.selection;
        assert_eq!(
            sel.bounds(),
            Some((glam::IVec2::new(0, 0), glam::IVec2::new(8, 8)))
        );
        assert_eq!(sel.coverage_at(glam::IVec2::new(1, 1)), 1.0);
        assert_eq!(sel.coverage_at(glam::IVec2::new(6, 6)), 1.0);

        // Subtract the inverse of "Left half": what is left is the left half.
        assert!(host.open_for_menu_action(&ui::menu::MenuAction::LoadSelection, &ed));
        {
            let dialog = host.active_load_selection_dialog_for_test();
            dialog.select(0);
            dialog.set_operation(LoadOperation::Subtract);
            dialog.set_invert(true);
        }
        let _ = settle_and_confirm(&mut host);
        crate::menu_bridge::perform(ui::menu::MenuAction::LoadSelection, &mut ed).unwrap();
        let sel = &ed.active().unwrap().document.selection;
        assert_eq!(sel.coverage_at(glam::IVec2::new(1, 1)), 1.0);
        assert_eq!(sel.coverage_at(glam::IVec2::new(6, 6)), 0.0);
    }
}

/// W9-B: the live fill layer, driven the way a user drives it — the menu row
/// opens its dialog, the dialog's own keyboard confirms it, and the confirmed
/// command is applied the way the shell applies every chrome command.
#[cfg(test)]
mod w9b_fill_layer_tests {
    use super::*;
    use crate::dialogs::ScriptedDialogs;
    use crate::editor::Editor;
    use crate::prefs::{AppPaths, Preferences};
    use crate::recent::RecentFiles;

    fn opened(dir: &std::path::Path) -> Editor {
        std::fs::create_dir_all(dir).unwrap();
        let png = dir.join("base.png");
        std::fs::write(
            &png,
            raster::encode(raster::ExportFormat::Png, 8, 8, &[9u8; 8 * 8 * 4]).unwrap(),
        )
        .unwrap();
        let mut ed = Editor::with_state(
            AppPaths::rooted(dir.join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        );
        ed.open_path(&png).unwrap();
        ed
    }

    fn raw_input(events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1400.0, 900.0),
            )),
            events,
            ..Default::default()
        }
    }

    /// Two settle frames, then Enter: what a user's OK does.
    fn confirm_with_enter(host: &mut DialogHost) -> ChromeOutput {
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let mut out = ChromeOutput::default();
        for _ in 0..2 {
            let _ = ctx.run(raw_input(Vec::new()), |ctx| host.ui(ctx, None, &mut out));
        }
        assert!(host.is_open() && out.is_empty(), "the dialog waits for OK");
        let enter = egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        };
        let _ = ctx.run(raw_input(vec![enter]), |ctx| host.ui(ctx, None, &mut out));
        assert!(!host.is_open(), "Enter did not close the dialog");
        out
    }

    fn fill_dialog(host: &mut DialogHost) -> &mut ui::dialogs::FillLayerDialog {
        match host.active_for_test() {
            ActiveDialog::FillLayer(dialog) => dialog,
            other => panic!("the active dialog is {other:?}, not a fill layer dialog"),
        }
    }

    /// The one fill layer in the active document.
    fn the_fill_layer(ed: &Editor) -> (layer_model::LayerId, layer_model::FillLayer) {
        let doc = &ed.active().unwrap().document;
        doc.layers
            .iter_depth_first()
            .into_iter()
            .find_map(|id| match &doc.layers.get(id)?.kind {
                layer_model::LayerKind::Fill(f) => Some((id, f.clone())),
                _ => None,
            })
            .expect("a fill layer exists")
    }

    fn composite(ed: &mut Editor) -> Vec<u8> {
        let open = ed.active_mut().unwrap();
        let rect = open.canvas_rect();
        open.composite(rect).unwrap()
    }

    fn every_pixel_is(rgba: &[u8], want: [u8; 4]) -> bool {
        rgba.as_chunks::<4>().0.iter().all(|p| *p == want)
    }

    #[test]
    fn new_solid_color_opens_its_dialog_and_ok_adds_a_live_fill_that_covers_the_canvas() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let mut host = DialogHost::default();
        assert!(
            host.open_for_menu_action(
                &ui::menu::MenuAction::NewFillLayer(ui::menu::FillLayerKind::SolidColor),
                &ed
            ),
            "Solid Color opens its dialog instead of baking"
        );
        fill_dialog(&mut host).set_color([1.0, 0.0, 0.0, 1.0]);
        let out = confirm_with_enter(&mut host);
        assert_eq!(out.commands.len(), 1, "OK is one command");
        let depth = ed.active().unwrap().history.undo_depth();
        for command in out.commands {
            ed.apply_command(command);
        }
        assert_eq!(ed.active().unwrap().history.undo_depth(), depth + 1);

        let (id, fill) = the_fill_layer(&ed);
        assert_eq!(
            fill.source,
            layer_model::FillSource::Solid {
                color: [1.0, 0.0, 0.0, 1.0]
            }
        );
        assert!(
            ed.active()
                .unwrap()
                .document
                .layer_tiles(id)
                .is_none_or(|m| m.is_empty()),
            "a live fill stores no pixels"
        );
        assert!(
            every_pixel_is(&composite(&mut ed), [255, 0, 0, 255]),
            "the fill composites its colour everywhere"
        );
    }

    #[test]
    fn re_editing_the_colour_changes_the_composite_in_one_undo_step() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        ed.set_foreground([1.0, 0.0, 0.0, 1.0]);
        ed.new_solid_fill_layer().unwrap();
        let (id, _) = the_fill_layer(&ed);
        ed.set_active_layer(id);
        assert!(every_pixel_is(&composite(&mut ed), [255, 0, 0, 255]));
        let depth = ed.active().unwrap().history.undo_depth();

        // Layer > Edit Adjustment (the Properties page's "Edit fill") on a
        // fill layer reopens its dialog on its own colour.
        let mut host = DialogHost::default();
        assert!(host.open_for_menu_action(&ui::menu::MenuAction::EditAdjustmentLayer, &ed));
        let dialog = fill_dialog(&mut host);
        assert!(dialog.is_edit() && dialog.layer() == id);
        dialog.set_color([0.0, 0.0, 1.0, 1.0]);
        let out = confirm_with_enter(&mut host);
        for command in out.commands {
            ed.apply_command(command);
        }

        assert_eq!(the_fill_layer(&ed).0, id, "the same layer, edited in place");
        assert!(
            every_pixel_is(&composite(&mut ed), [0, 0, 255, 255]),
            "the new colour composites"
        );
        assert_eq!(
            ed.active().unwrap().history.undo_depth(),
            depth + 1,
            "the edit is one undo step"
        );
        ed.active_mut().unwrap().undo().unwrap();
        assert!(
            every_pixel_is(&composite(&mut ed), [255, 0, 0, 255]),
            "one undo brings the old colour back"
        );
    }

    #[test]
    fn a_fill_layer_survives_save_and_reopen_still_live() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        ed.set_foreground([0.0, 1.0, 0.0, 1.0]);
        ed.set_background([0.0, 0.0, 1.0, 1.0]);
        ed.new_gradient_fill_layer().unwrap();
        let (_, before) = the_fill_layer(&ed);
        let pixels = composite(&mut ed);

        let project = dir.path().join("fill.rstudio");
        ed.active_mut().unwrap().save_to(&project, "test").unwrap();
        let mut reopened = opened(&dir.path().join("second"));
        reopened.open_path(&project).unwrap();
        let (_, after) = the_fill_layer(&reopened);
        assert_eq!(
            after, before,
            "the fill layer reopened live, parameters intact"
        );
        assert_eq!(composite(&mut reopened), pixels, "and composites the same");
    }

    #[test]
    fn rasterize_turns_a_fill_layer_into_the_same_pixels() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        ed.set_foreground([0.0, 1.0, 0.0, 1.0]);
        ed.new_solid_fill_layer().unwrap();
        let (id, _) = the_fill_layer(&ed);
        ed.set_active_layer(id);
        let before = composite(&mut ed);
        ed.rasterize_layer().unwrap();
        let doc = &ed.active().unwrap().document;
        assert!(
            doc.layers.iter_depth_first().into_iter().all(|l| !matches!(
                doc.layers.get(l).unwrap().kind,
                layer_model::LayerKind::Fill(_)
            )),
            "the fill layer became pixels"
        );
        assert_eq!(composite(&mut ed), before, "and draws what it drew");
    }

    #[test]
    fn gradient_and_pattern_rows_open_their_dialogs_too() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let mut host = DialogHost::default();
        assert!(host.open_for_menu_action(
            &ui::menu::MenuAction::NewFillLayer(ui::menu::FillLayerKind::Gradient),
            &ed
        ));
        assert_eq!(
            fill_dialog(&mut host).kind(),
            ui::menu::FillLayerKind::Gradient
        );
        fill_dialog(&mut host).gradient_mut().unwrap().angle_deg = 0.0;
        let out = confirm_with_enter(&mut host);
        for command in out.commands {
            ed.apply_command(command);
        }
        assert!(matches!(
            the_fill_layer(&ed).1.source,
            layer_model::FillSource::Gradient(_)
        ));

        // No pattern defined: the row falls through to the bridge, which
        // refuses loudly with the reason, rather than a dialog that cannot OK.
        assert!(!host.open_for_menu_action(
            &ui::menu::MenuAction::NewFillLayer(ui::menu::FillLayerKind::Pattern),
            &ed
        ));
        ed.presets_mut()
            .define_pattern(asset_store::presets::PatternPreset {
                name: "Dots".into(),
                width: 2,
                height: 2,
                rgba8: vec![255; 16],
            });
        assert!(host.open_for_menu_action(
            &ui::menu::MenuAction::NewFillLayer(ui::menu::FillLayerKind::Pattern),
            &ed
        ));
        assert_eq!(
            fill_dialog(&mut host).kind(),
            ui::menu::FillLayerKind::Pattern
        );
    }
}
