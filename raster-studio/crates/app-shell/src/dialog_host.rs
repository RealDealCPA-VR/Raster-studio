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
        }
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
        match action {
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
            ui::menu::MenuAction::LayerStyle(_) => match layer_style_dialog(editor) {
                Some(dialog) => {
                    self.open(dialog);
                    true
                }
                None => false,
            },
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
                DialogOutcome::Open => {}
                DialogOutcome::Cancelled => self.active = None,
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
        match active.show(ctx, sampler) {
            DialogOutcome::Open => {}
            DialogOutcome::Cancelled => {
                self.active = None;
                self.color_target = None;
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
        DialogAction::Fill(spec) => out.fill_spec = Some(spec),
        DialogAction::Stroke(spec) => out.stroke_spec = Some(spec),
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
        let viewport = crate::tool_input::canvas_viewport(surface_px);
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

/// A [`LayerStyleDialog`] over the active layer's effects.
fn layer_style_dialog(editor: &crate::Editor) -> Option<ActiveDialog> {
    let open = editor.active()?;
    let id = open.document.active_layer()?;
    let layer = open.document.layers.get(id)?;
    Some(ActiveDialog::LayerStyle(Box::new(LayerStyleDialog::new(
        id,
        layer.name.clone(),
        layer.effects.clone(),
    ))))
}

/// An [`ImageSizeDialog`] over the active document's size.
///
/// `editor_core::DocumentMeta` records no print resolution, so the dialog
/// starts from the 72 ppi its presets assume; a confirmed spec that only
/// changes the ppi resamples nothing and is a no-op the shell reports.
fn image_size_dialog(editor: &crate::Editor) -> Option<ActiveDialog> {
    let open = editor.active()?;
    Some(ActiveDialog::ImageSize(Box::new(ImageSizeDialog::new(
        open.document.width(),
        open.document.height(),
        72.0,
    ))))
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
    let source = crate::menu_bridge::filter_source(editor)?;
    Some(ActiveDialog::Filter(Box::new(FilterDialog::new(
        spec, source,
    ))))
}

/// An [`AdjustmentDialog`] over the active pixel layer, previewing in the
/// document's colour space.
///
/// Gated the way [`crate::menu_bridge::perform`]'s adjustment arm is — the
/// active layer must own pixels — so the dialog never opens over a layer its
/// confirmation could not edit. The preview source is the same buffer the
/// filter dialogs preview against; the dialog bounds it itself.
fn adjustment_dialog(editor: &crate::Editor, id: AdjustmentId) -> Option<ActiveDialog> {
    pixel_layer_available(editor)?;
    let source = crate::menu_bridge::filter_source(editor)?;
    let space = editor.active()?.document.meta.color_space.clone();
    Some(ActiveDialog::Adjustment(Box::new(AdjustmentDialog::new(
        id, source, space,
    ))))
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
    let source = crate::menu_bridge::filter_source(editor)?;
    Some(ActiveDialog::FilterGallery(Box::new(
        ui::dialogs::FilterGalleryDialog::new(source),
    )))
}

/// A [`CanvasSizeDialog`] over the active document's size.
fn canvas_size_dialog(editor: &crate::Editor) -> Option<ActiveDialog> {
    let open = editor.active()?;
    Some(ActiveDialog::CanvasSize(Box::new(CanvasSizeDialog::new(
        open.document.width(),
        open.document.height(),
        72.0,
    ))))
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
}
