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

use crate::chrome::ChromeOutput;
use tools::ToolId;
use ui::dialogs::{
    ArbitraryRotationDialog, BrushEditorDialog, CanvasSizeDialog, ColorPickerDialog, DialogAction,
    DialogOutcome, ExportAsDialog, FilterDialog, GradientEditorDialog, ImageSizeDialog,
    LayerStyleDialog, NewDocumentDialog, PreferencesDialog, ScreenSampler,
};

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
            // File ▸ Export As… — the per-format rows all open the one dialog;
            // the format a row names is just the row the user came in through,
            // and the dialog's list is where the choices live.
            ui::menu::MenuAction::Export(_) => match export_as_dialog(editor) {
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
            // Filter ▸ Filter Gallery opens the browser over the catalogue.
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
/// base file name, and a placeholder proxy for the live preview.
///
/// The real preview is a downscaled *composite*, which needs `&mut` to run the
/// compositor — [`crate::chrome::Chrome::ui`] swaps it in on the frame after
/// the dialog opens, through [`DialogHost::refresh_preview`].
fn export_as_dialog(editor: &crate::Editor) -> Option<ActiveDialog> {
    let open = editor.active()?;
    let (w, h) = (open.document.width(), open.document.height());
    let name = open.title().to_string();
    let proxy = ui::dialogs::PreviewSource::placeholder(
        ui::dialogs::export_as::MAX_PROXY_SIDE.min(w.max(1)),
        ui::dialogs::export_as::MAX_PROXY_SIDE.min(h.max(1)),
    );
    Some(ActiveDialog::ExportAs(Box::new(
        ui::dialogs::ExportAsDialog::new(w, h, name, proxy),
    )))
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
    }
}
