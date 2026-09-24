//! The application's menu bar *is* the `ui` crate's menu bar.
//!
//! # Why this module exists
//!
//! There were two menus in this workspace: a small one built here from
//! [`crate::action::Action`], and the full nine-menu Photopea-shaped one in
//! `ui::menu` that nothing instantiated. Two menus is two vocabularies, two
//! enablement rules and two places for an item to rot. This module deletes one
//! of them: [`draw`] renders `ui::menu::menu_bar` and nothing else, and
//! `the_menu_bar_the_shell_draws_is_the_ui_crates` pins that so the two cannot
//! silently diverge again.
//!
//! # The contract, and where this build falls short of it
//!
//! `ui::menu` resolves every item to [`ui::Resolution`]: `Enabled(intent)` or
//! `Disabled(reason)`. That is a promise about the *menu model*, not about this
//! application: `ui` describes a finished editor, and the shell can perform a
//! subset of it so far. So there is a second gate here, and it is explicit
//! rather than silent — [`pick`] turns an intent into something the shell can
//! do, and an item it cannot answer is drawn **disabled** carrying
//! [`NOT_WIRED`]. A menu item that does nothing is still a bug; an item that is
//! greyed out and says why is not.
//!
//! `every_ui_menu_item_is_either_performable_or_disabled_with_a_reason` walks
//! all nine menus in two document states, counts the three outcomes and pins
//! the number of unwired items this build has, by name.
//!
//! # The bridge is the whole workspace's, not only the menu's
//!
//! [`pick`] answers *any* [`ui::Intent`], not only the ones a menu item
//! produces, because the same vocabulary comes back out of
//! [`ui::Workspace::drain_intents`] when the docked panels, the tool palette and
//! the options bar are drawn. One translation table, so a control in a panel and
//! the menu item that does the same thing cannot disagree.
//!
//! # A menu click takes the same road a panel click does
//!
//! [`draw`] resolves each row to the [`Intent`] the shared model gives it and,
//! on a click, hands that intent to the chrome (`Chrome::menu_click`), which
//! routes it exactly as it routes an intent drained from the workspace: the
//! dialog host is asked first, then [`pick`] and [`record`]. It used to call
//! [`record`] here directly, which is how Image Size, Canvas Size, Arbitrary
//! rotation, Layer Style, the Filter Gallery and every Filter row skipped the
//! dialogs the chrome hosts for them and landed in [`perform`], which has no
//! arm for a question only a dialog can answer.
//! `every_enabled_menu_item_really_does_something` drives every enabled row
//! through that click handler and fails on one that reaches nobody.

use std::path::PathBuf;

use editor_core::Command;
use layer_model::LayerId;
use tools::ToolId;
use ui::menu::{Entry, Menu, MenuAction};
use ui::{Intent, MenuContext, Resolution, Workspace};

use crate::action::Action;
use crate::chrome::ChromeOutput;
use crate::editor::Editor;
use crate::prefs::{Preferences, ThemeChoice};

// W7-D: Image > Mode conversions (RGB, Grayscale, Lab, CMYK, Indexed).
mod color_mode;

// W10-H: Image > Apply Image and Calculations, and the Bitmap / Duotone /
// Apply Image / Calculations dialogs as one host dialog kind.
pub(crate) mod apply_image;
pub mod image_dialogs;

// W7-I: Content-Aware Fill and Content-Aware Scale, run on a worker.
pub(crate) mod content_aware_job;

// W10-K: Select > Subject (saliency seeds + GrabCut, no model), run on a worker.
pub(crate) mod subject_job;

// W9-H: Photoshop style libraries (.asl) into the style presets, and the
// style presets as the Layer Style dialog's Styles grid lists them.
pub mod asl_import;

// W10-J: New Guides from Shape, Alt+Ctrl+T, Shift+[ / ] and the number keys.
pub(crate) mod view_keys;
// W10-J: File > New's Artboard option.
pub(crate) mod artboard_doc;

// W10-B: the Channels panel's alpha rows — a saved selection edited as a
// channel.
pub(crate) mod alpha_channel;
// W10-B: the Glyphs panel's pick, into the live typing session or the
// committed text.
pub(crate) mod glyph_insert;
#[cfg(test)]
mod w10b_panel_tests;

// W10-I: Hide / Show Layers, Matting, and the Smart Object rows.
pub(crate) mod layer_extras;

// W10-A: Edit > Define Custom Shape, and Layer > Link Layers' link groups.
pub(crate) mod custom_shape;
pub(crate) mod link_groups;

/// Shown on an item the shared menu model allows but this build cannot perform.
///
/// Kept as the *fallback* only. Every item this build genuinely cannot do now
/// carries a sentence naming the specific thing that is missing — see
/// [`unavailable_reason`] — because "this build cannot do that yet" tells a
/// user nothing they can act on and tells a reviewer nothing about what is
/// left. `no_unavailable_item_falls_back_to_the_generic_reason` pins that the
/// fallback is unreachable from any menu.
pub const NOT_WIRED: &str = "This build cannot do that yet";

/// What the shell should do about a menu click.
#[derive(Debug, Clone, PartialEq)]
pub enum Pick {
    /// A named application action, routed through [`Editor::dispatch`].
    Action(Action),
    /// A document edit, routed through history.
    Command(Command),
    /// A menu operation this build performs against the *live* document, in
    /// [`perform`].
    ///
    /// # Why this is not a [`Pick::Command`]
    ///
    /// Three quarters of the menu bar cannot be answered with a value built
    /// from `&Editor` alone:
    ///
    /// * A filter and an adjustment produce **new pixels**, and pixels reach a
    ///   document in two halves — the bytes go into the
    ///   [`compositor::MemoryTileSource`] (which needs `&mut`) and the
    ///   *references* to them arrive as [`Command::PaintTiles`]. A `Pick` built
    ///   during enablement has no `&mut` and no business hashing a megabyte.
    /// * The selection is a **field** of [`editor_core::Document`], not a
    ///   command — `editor_core` says so — so Select ▸ Inverse has no `Command`
    ///   to be.
    /// * `resolve` runs for every one of the 256 items every frame the menu is
    ///   open. Building a Gaussian Blur's result forty times a second to decide
    ///   whether its row should be grey is not a design, it is a hang.
    ///
    /// So the *decision* is cheap and eager (this variant is one enum tag) and
    /// the *work* is expensive and lazy: [`perform`] runs once, on the click,
    /// with `&mut Editor`.
    Menu(MenuAction),
    /// An edit to a layer's kind payload — an adjustment's parameters, a text
    /// layer's run.
    ///
    /// A document edit like [`Pick::Command`], and it becomes an
    /// [`editor_core::Command::SetLayerKind`] before it reaches the document.
    /// It travels as its own variant because a *drag* produces one of these per
    /// frame and they must collapse into a single undo step; only
    /// [`crate::chrome::Chrome`] knows whether the pointer is still down, so it
    /// stamps the gesture on and [`crate::Editor::apply_kind_edit`] does the
    /// folding.
    Kind {
        layer: LayerId,
        kind: Box<layer_model::LayerKind>,
    },
    /// The Actions panel's transport. Performed on the editor immediately:
    /// start/stop toggle the recording, replay re-runs the last capture.
    StartRecording,
    StopRecording,
    ReplayRecording,
    /// Open one of the recent files.
    OpenRecent(PathBuf),
    /// A settings change — the Window ▸ Appearance items.
    Preferences(Box<Preferences>),
    /// An intent whose whole effect is on the workspace itself: which panels
    /// are open, where they are docked, the view overlays, channel isolation,
    /// tool options. [`ui::Workspace::absorb`] performs these, and
    /// [`crate::chrome::Chrome`] owns the workspace, so it applies them itself.
    ///
    /// **Every intent routed here must be idempotent under
    /// [`ui::Workspace::absorb`].** A control in a drawn panel applies its own
    /// effect and then emits, so absorbing what was drained applies it again;
    /// only an absolute set (`open`, `side`, `to`, `on`, `visible`, a value)
    /// survives that. The `ui` crate states the rule on [`ui::Intent`] and
    /// enforces it in `every_workspace_intent_is_idempotent_under_absorb`; this
    /// list is the other half of the contract, so adding a *relative* intent
    /// here is the mistake to refuse.
    Workspace(Box<Intent>),
    /// Make a tool active.
    Tool(ToolId),
    /// Move the selection in the layers panel.
    SelectLayer(LayerId),
    /// Photopea's multi-selection: the whole set, in click order, plus the
    /// active layer the click landed on.
    SelectLayers(Vec<LayerId>, Option<LayerId>),
    /// Aim edits at the active layer's content or mask coverage (card 007).
    EditTarget(crate::edit_target::EditTargetKind),
    /// Card 026: double-clicking a text row enters that layer for editing.
    EnterTextLayer(LayerId),
    /// Activate a tool AND a named choice it wears — the transform menu's
    /// Scale/Rotate/Skew/Distort/Perspective (`mode`) and Transform Selection
    /// (`target`), as one pick.
    ToolChoice(tools::ToolId, &'static str, usize),
    /// Stand on this many applied commands — [`Editor::jump_history`]'s
    /// absolute depth, converted here from the panel's relative step count.
    History(usize),
    /// The active document's zoom, as a scale factor.
    Zoom(f32),
    /// The active document's camera centre, in image pixels.
    ViewCenter((f32, f32)),
    Foreground([f32; 4]),
    Background([f32; 4]),
    /// Open the colour picker dialog for one of the colour wells — the
    /// swatches' double-click. The chrome opens the dialog and remembers the
    /// target, so the confirmed colour lands in the right well.
    OpenColorPicker(ui::panels::color::ColorWell),
    /// Open the gradient editor dialog for the effective tool's ramp.
    OpenGradientEditor,
    /// Open the brush editor dialog over the effective tool's brush.
    OpenBrushEditor,
    /// W4-G: confirm the live tool's held gesture (the Ruler's Straighten
    /// Layer button); the shell treats it exactly as Enter.
    ConfirmTool,
    /// W10-B: the Glyphs panel's pick for a text layer; the shell inserts it
    /// ([`glyph_insert::insert_glyph`]).
    InsertGlyph(LayerId, String),
}

/// The nine menus, exactly as the `ui` crate publishes them.
///
/// A thin wrapper on purpose: the test that pins "the shell draws `ui`'s menu"
/// needs one function to point at, and [`draw`] must have no other source of
/// entries.
pub fn menus(editor: &Editor) -> Vec<Menu> {
    ui::menu::menu_bar(editor.recent().entries().len())
}

/// The state every item is resolved against this frame.
///
/// The dock, the view overlays and the ruler unit come from the live
/// [`Workspace`] rather than from a default, which is what makes Window ▸
/// Workspace and the View menu's checkmarks describe the window the user is
/// looking at.
pub fn context(editor: &mut Editor, workspace: &Workspace) -> MenuContext {
    // W7-E: every frame builds this, so the smart-filter runner is in place
    // before the canvas composites a smart object.
    install_smart_filter_runner();
    // W10-K: and so a finished Select Subject lands on the next frame.
    subject_job::poll(editor);
    // W10-E: and a running Batch / Convert Formats reports its progress.
    crate::automate::poll(editor);
    let recent_files = editor
        .recent()
        .entries()
        .iter()
        .map(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.display().to_string())
        })
        .collect();

    let mut context = match editor.active() {
        Some(open) => workspace.menu_context(&open.document, &open.history),
        None => MenuContext {
            dock: workspace.dock.clone(),
            view: workspace.view_flags,
            clipboard: workspace.clipboard,
            ..MenuContext::default()
        },
    };
    context.recent_files = recent_files;
    // The clipboard is the *editor's*, not the workspace's. `ui::Workspace`
    // carries a `ClipboardState`, but nothing ever wrote it, so Paste and Paste
    // Into were greyed out no matter how many times Copy had been used.
    context.clipboard = ui::ClipboardState {
        // Card 052: the two sources are reported separately. The internal
        // store enables every paste; the OS image clipboard (a screenshot,
        // another application's payload) enables plain Paste only — it has
        // no in-document origin yet, so Paste Into waits for card 053. The
        // OS probe reads the image and is throttled inside the editor: the
        // menu context is built every frame, and a full clipboard read per
        // frame would be absurd.
        pixels: editor.clipboard().is_some(),
        external_pixels: editor.os_clipboard_has_image(),
        layers: false,
    };
    context.open_documents = editor.documents().len();
    // W4-H: Edit > Purge > Histories drops every open document's history.
    context.any_history = editor
        .documents()
        .iter()
        .any(|d| d.history.can_undo() || d.history.can_redo());
    context.theme = editor.preferences().theme.resolve(design::Theme::Dark);
    // W9-G: Layer ▸ Vector Mask ▸ Current Path reads the Paths panel, which
    // is the workspace's; `perform` only holds the editor, so the path the
    // menu was enabled for is parked here, frame by frame.
    let current = editor.active().and_then(|open| {
        workspace
            .paths
            .selected_path(&open.document)
            .or_else(|| workspace.paths.work_path.clone())
            .map(|p| compositor::vector_mask::svg_of(&p))
    });
    context.has_current_path = current.is_some();
    set_current_vector_path(current);
    // The multi-selection lives on the DOCUMENT (`Editor::set_layer_selection`
    // puts it there); the workspace's Layers panel echoes it a frame later.
    // Layer ▸ Distribute needs the count the document holds, not the echo.
    if let Some(open) = editor.active() {
        context.selected_layers = context
            .selected_layers
            .max(open.document.layer_selection().len());
        // The view rotation is the DOCUMENT camera's — the camera the shell
        // renders from and `perform`'s ResetViewRotation arm uprights — not
        // the workspace canvas camera's, which this shell never turns.
        //
        // The Rotate View tool's drag turns this camera (`write_camera_back`
        // writes the rotation back), so the item enables as soon as the view
        // is turned and uprights it.
        context.view_rotated = open.camera.is_rotated();
    }
    // W10-G: Edit > Fade names the step it would fade, or greys out.
    context.fade_step = crate::fade::fadeable(editor);
    context
}

/// What the shell should do about `intent`, or `None` when this build has no
/// answer for it.
///
/// Every arm is an explicit decision, and the one that returns `None` says why:
/// a [`MenuAction`] with no [`Action`] and no workspace effect is a menu item
/// this build does not implement.
///
/// **A `None` here is never dropped on the floor.** The menu bar draws such an
/// item disabled carrying [`NOT_WIRED`]; a panel control that raises one has
/// its intent reported through [`crate::chrome::ChromeOutput::unrouted`] and
/// named in the status bar by [`unrouted_message`]. Silence is what let
/// [`Intent::EditLayerKind`] — every adjustment slider in the Properties panel
/// — go unanswered for a whole wave.
pub fn pick(intent: &Intent, editor: &Editor) -> Option<Pick> {
    match intent {
        // W10-A: the Layers panel's link button makes a group of its own.
        Intent::Document(command) => Some(Pick::Command(
            link_groups::panel_link(command, editor).unwrap_or_else(|| command.clone()),
        )),
        Intent::Action(action) => shell_action(*action, editor),
        Intent::SetTheme(theme) => {
            let mut prefs = editor.preferences().clone();
            prefs.theme = match theme {
                design::Theme::Light => ThemeChoice::Light,
                design::Theme::Dark => ThemeChoice::Dark,
            };
            Some(Pick::Preferences(Box::new(prefs)))
        }
        Intent::SelectTool(tool) => Some(Pick::Tool(*tool)),
        // A layer row click names the layer it wants. Select ▸ Deselect Layers
        // names *no* layer, and `ChromeOutput::select_layer` cannot carry the
        // absence of one — which is why this arm used to answer `None` and the
        // item sat greyed out with the generic refusal. Clearing the cursor is
        // a document edit, so it goes down the same road every other one does.
        Intent::SelectLayers { active: None, .. } => Some(Pick::Menu(MenuAction::DeselectLayers)),
        Intent::SelectLayers { layers, active } => {
            Some(Pick::SelectLayers(layers.clone(), *active))
        }
        // The Properties panel's Layer/Mask control (card 007). The shell's
        // per-document state is the authority; the panel keeps its own display
        // echo. Not a workspace intent: the target is shell state, so it does
        // not ride `Pick::Workspace` and its absorb-idempotency rule.
        Intent::SetEditTarget { mask } => Some(Pick::EditTarget(
            crate::edit_target::EditTargetKind::from_focus(*mask),
        )),
        Intent::EnterTextLayer { layer } => Some(Pick::EnterTextLayer(*layer)),
        Intent::InsertGlyph { layer, text } => Some(Pick::InsertGlyph(*layer, text.clone())),
        Intent::HistoryJump(jump) => {
            // The panel counts *steps* from where the document stands; the
            // editor walks to an absolute depth. Converting here keeps the one
            // place that knows both.
            let here = editor.active()?.history_depth();
            Some(Pick::History(
                here.saturating_sub(jump.undo).saturating_add(jump.redo),
            ))
        }
        Intent::SetZoom(zoom) => Some(Pick::Zoom(*zoom)),
        Intent::SetViewCenter(center) => Some(Pick::ViewCenter(*center)),
        Intent::SetForeground(rgba) => Some(Pick::Foreground(*rgba)),
        Intent::SetBackground(rgba) => Some(Pick::Background(*rgba)),
        Intent::OpenColorPicker(target) => Some(Pick::OpenColorPicker(*target)),
        Intent::OpenGradientEditor => Some(Pick::OpenGradientEditor),
        Intent::OpenBrushEditor => Some(Pick::OpenBrushEditor),
        Intent::ConfirmTool => Some(Pick::ConfirmTool),
        // Everything whose whole effect is on the workspace's own state. Listed
        // rather than caught by a wildcard: a new intent variant must be an
        // explicit decision here, which is what the wildcard used to hide.
        Intent::SetPanelOpen { .. }
        | Intent::DockPanel { .. }
        | Intent::ReorderPanel { .. }
        | Intent::ApplyLayout(_)
        | Intent::SetViewFlag { .. }
        | Intent::SetRulerUnit(_)
        | Intent::SetChannelVisible { .. }
        | Intent::SelectChannel(_)
        | Intent::SetToolOption { .. }
        | Intent::SetToolGradient { .. }
        | Intent::ResetToolOptions(_)
        | Intent::SetGroupExpanded { .. } => Some(Pick::Workspace(Box::new(intent.clone()))),
        // The Properties panel's sliders and the Text panel's fields. Routed
        // through [`editor_core::Command::SetLayerKind`], one undo step per
        // drag rather than one per frame.
        Intent::EditLayerKind { layer, kind } => Some(Pick::Kind {
            layer: *layer,
            kind: kind.clone(),
        }),
        // The Actions panel's transport: the recording lives on the editor,
        // so the shell harvests these into `ChromeOutput::actions`.
        Intent::StartRecording => Some(Pick::StartRecording),
        Intent::StopRecording => Some(Pick::StopRecording),
        Intent::ReplayRecording => Some(Pick::ReplayRecording),
    }
}

/// What the status bar should say about an intent no [`Pick`] answers.
///
/// The point is that it says *something*. A control whose intent falls through
/// used to disappear without a trace, which is precisely how an inert
/// Properties panel survived review: nothing on screen ever admitted that a
/// click had gone nowhere.
pub fn unrouted_message(intent: &Intent) -> String {
    match intent {
        Intent::Action(action) => format!(
            "{}: {}",
            action.label(),
            unavailable_reason(*action).unwrap_or(NOT_WIRED)
        ),
        other => format!("{NOT_WIRED} ({other:?})"),
    }
}

/// The View-menu items whose whole implementation is [`Workspace`]'s own.
///
/// [`ui::Workspace::absorb_action`] performs all three against the canvas
/// camera, and did so for a whole release while they sat greyed out beside
/// Zoom In and Fit on Screen — because the bridge routed *no* [`Intent::Action`]
/// to the workspace, so the only actions that worked were the ones the shell
/// happened to reimplement as an [`Action`].
///
/// Every one is an absolute placement of the camera (fill this rectangle, frame
/// this selection, this many pixels per inch), so all three satisfy the
/// idempotence [`Pick::Workspace`] requires.
///
/// # Reset View Rotation is deliberately NOT one of them
///
/// It used to be routed here, and was permanently greyed out: `ui::menu` gates
/// it on [`MenuContext::view_rotated`], the workspace camera's rotation is
/// never written by this shell, and the camera the shell actually renders from
/// — [`render::Camera`] on `OpenDocument` — is where the view rotation now
/// lives. So the item is performed against the *document* camera by
/// [`perform`] (`doc.camera.reset_rotation()`), its enablement is read off the
/// same camera by [`context`], and the read-back-to-the-document dance the
/// three zoom items need does not apply: there is nothing on the workspace
/// side to read back.
pub fn is_workspace_camera_action(action: MenuAction) -> bool {
    use ui::menu::ZoomCommand as Z;
    matches!(
        action,
        MenuAction::Zoom(Z::FillScreen)
            | MenuAction::Zoom(Z::ToSelection)
            | MenuAction::Zoom(Z::PrintSize)
    )
}

/// The [`Action`] a named menu action maps onto, if this build has one.
fn shell_action(action: MenuAction, editor: &Editor) -> Option<Pick> {
    use ui::menu::TransformOp as T;
    use ui::menu::ZoomCommand as Z;
    // The five interactive modes are the transform tool wearing its mode
    // choice: the pick carries the index so the shell sets both the tool and
    // the option in one click. The order is TransformMode::ALL's, which the
    // registry's choice spec and the options bar both speak.
    if let Some((key, index)) = match action {
        MenuAction::Transform(T::Scale) => Some(("mode", 0)),
        MenuAction::Transform(T::Rotate) => Some(("mode", 1)),
        MenuAction::Transform(T::Skew) => Some(("mode", 2)),
        MenuAction::Transform(T::Distort) => Some(("mode", 3)),
        MenuAction::Transform(T::Perspective) => Some(("mode", 4)),
        // Warp is the gizmo's mesh mode (P2.3).
        MenuAction::Transform(T::Warp) => Some(("mode", 5)),
        // Transform Selection is the gizmo wearing its Selection target: the
        // drag resamples the selection mask and commits as one undoable step.
        MenuAction::TransformSelection => Some(("target", 1)),
        // W10-J: Edit > Content-Aware Scale > With Handles is the gizmo in
        // its Content-Aware mode.
        MenuAction::ContentAwareScaleFree => {
            Some(("mode", tools::transform::TransformMode::CONTENT_AWARE_INDEX))
        }
        _ => None,
    } {
        return Some(Pick::ToolChoice(tools::ToolId::FreeTransform, key, index));
    }
    if is_workspace_camera_action(action) {
        return Some(Pick::Workspace(Box::new(Intent::Action(action))));
    }
    // W10-J: View > Snap To > All / None set the workspace's five Snap To
    // flags at once (`ui::Workspace::absorb`), an absolute set.
    if matches!(action, MenuAction::SnapToAll | MenuAction::SnapToNone) {
        return Some(Pick::Workspace(Box::new(Intent::Action(action))));
    }
    // `Edit Adjustments…` and the Properties panel's "Open editor…" are the
    // same request: reveal the Properties panel, which is where an adjustment
    // layer's parameters are edited. Routing it here (rather than through
    // [`perform`], which has no dock to reveal) keeps the menu item and the
    // panel button agreeing about what the click means. The set is absolute, so
    // opening an already-open panel is harmless.
    if action == MenuAction::EditAdjustmentLayer {
        return Some(Pick::Workspace(Box::new(Intent::SetPanelOpen {
            panel: ui::PanelId::Properties,
            open: true,
        })));
    }
    // File Info… opens a document-metadata window hosted by the chrome. It
    // keeps its `unavailable_reason` (which names what the metadata editor does
    // not yet hold) so the unrouted-message path still has a specific reason,
    // but it is genuinely performable: routed to [`perform`] here so the click
    // flips the editor's flag and the chrome draws the window.
    if action == MenuAction::FileInfo {
        return Some(Pick::Menu(MenuAction::FileInfo));
    }
    // Print… renders the composite to a print-ready PDF through [`perform`]
    // (the file half is the tested raster::pdf encoder).
    if action == MenuAction::Print {
        return Some(Pick::Menu(MenuAction::Print));
    }
    let mapped = match action {
        MenuAction::NewDocument => Action::NewDocument,
        MenuAction::Open => Action::Open,
        MenuAction::OpenRecent(i) => {
            return editor
                .recent()
                .entries()
                .get(i)
                .cloned()
                .map(Pick::OpenRecent)
        }
        MenuAction::Save => Action::Save,
        MenuAction::SaveAs => Action::SaveAs,
        MenuAction::CloseDocument => Action::CloseDocument,
        MenuAction::CloseOthers => Action::CloseOthers,
        MenuAction::Quit => Action::Quit,
        // `ui` names a format per item; the shell's export dialog is where the
        // format is finally chosen, so every one routes to the same action.
        MenuAction::Export(_) => Action::Export,
        MenuAction::Undo => Action::Undo,
        MenuAction::Redo => Action::Redo,
        // Edit ▸ Step Backward / Step Forward are Photoshop's names for the
        // same two application actions; the rows exist so the menu paints
        // them, and they perform exactly what Undo and Redo perform.
        MenuAction::StepBackward => Action::Undo,
        MenuAction::StepForward => Action::Redo,
        MenuAction::Preferences => Action::ShowPreferences,
        // Edit > Keyboard Shortcuts... is never performed through this arm:
        // `Chrome::route` asks the dialog host first, and
        // `DialogHost::open_for_menu_action` answers this intent by opening
        // Preferences on its Keymap page (the click, the context menu and the
        // chord all arrive as that one intent). What this pick decides is the
        // row's enablement in `resolve_intent`: the shortcut editor lives in
        // the preferences window, so the row is live whenever Preferences is.
        MenuAction::KeyboardShortcuts => Action::ShowPreferences,
        MenuAction::DuplicateLayer => Action::DuplicateLayer,
        MenuAction::Zoom(Z::In) => Action::ZoomIn,
        MenuAction::Zoom(Z::Out) => Action::ZoomOut,
        MenuAction::Zoom(Z::FitOnScreen) => Action::ZoomFit,
        MenuAction::Zoom(Z::ActualPixels) => Action::ZoomActualPixels,
        // View ▸ 200% is an absolute zoom, the channel the Navigator's own
        // zoom field already rides.
        MenuAction::Zoom(Z::Double) => return Some(Pick::Zoom(2.0)),
        // Everything else is either performed against the live document by
        // `perform` or is honestly out of this build's reach; the one table in
        // `unavailable_reason` decides which, and says why when it is the
        // latter.
        other => {
            return unavailable_reason(other)
                .is_none()
                .then_some(Pick::Menu(other))
        }
    };
    Some(Pick::Action(mapped))
}

// ---------------------------------------------------------------------------
// What this build cannot do, and why
// ---------------------------------------------------------------------------

/// The sentence to show on a menu item this build cannot perform, or `None`
/// when [`perform`] performs it.
///
/// **This function is the whole gate.** An item it answers `None` for is
/// enabled, so either the chrome's dialog host opens a dialog for it or
/// [`perform`] has a real arm for it —
/// `every_enabled_menu_item_really_does_something` drives every one of them
/// through the menu bar's click handler against a live document and fails on
/// one that does neither, and `no_enabled_menu_item_resolves_to_a_no_op`
/// fails on a performed arm that changes nothing.
///
/// Every reason names the *specific* missing piece. "This build cannot do that
/// yet" is not a reason; it is the absence of one, and 126 items wore it.
pub fn unavailable_reason(action: MenuAction) -> Option<&'static str> {
    Some(match action {
        // ---- File ----------------------------------------------------------
        // Place Embedded/Linked route to `editor.place_from_dialog` (P2.4);
        // the transform gizmo the old reason awaited landed in P2.1.
        // File Info is deliberately retained — see its own note below.
        MenuAction::FileInfo => {
            // Not a disabled item: `pick` routes File Info to [`perform`]
            // before this table is ever consulted. The text stays so the
            // unrouted-message path still has a specific sentence to show
            // (what the metadata window holds today, and what it does not).
            // W10-E: File Info edits the XMP fields, kept beside the
            // document for the session rather than in it.
            "File Info edits the document's XMP description (title, author, \
             description, keywords, copyright) for this session; a .rstudio \
             save does not keep it"
        }

        // ---- Edit ----------------------------------------------------------
        // Free Transform and five of the six interactive transforms route to
        // the canvas gizmo now (P2.1): the item activates the transform tool
        // and the drag ends as one undoable command. The five fixed
        // rotations and flips never needed it.
        // Warp routes too (P2.3): the mesh gizmo and the mesh deformer were
        // already in tools::transform — WarpMesh, the mesh handles, and the
        // commit-time resample that bends the interior.

        // ---- Image ---------------------------------------------------------
        // Every Image ▸ Adjustments row opens its parameter dialog now
        // (`DialogHost::open_for_menu_action`, `ui::dialogs::AdjustmentDialog`),
        // including the ten whose starting parameters are the identity — a
        // dialog is exactly what they were waiting for. The confirmed
        // parameters arrive at [`perform`]'s `ApplyAdjustment` arm through
        // `dialog_host::take_confirmed_adjustment`; the only refusal left is
        // the menu's own "no document / no pixel layer" gate.
        // Everything that changes the canvas *rectangle* is hosted now:
        // `ImageSize`, `CanvasSize` and `RotateCanvas(Arbitrary)` open real
        // dialogs whose confirmed specs land as one undoable step each (right-
        // angle rotations take the exact fixed path), and Reveal All performs
        // directly in [`perform`] — it asks nothing. SetColorMode has no
        // reason here either: P2.9's depth-aware command carries it.

        // ---- Layer ---------------------------------------------------------
        // Select ▸ All Layers performs now — the document keeps a real
        // multi-selection set (see `perform`), so it has no reason here.
        // ---- Select --------------------------------------------------------
        // SelectSubject has no reason here either: W10-K put the item back
        // and `perform` runs it on the job worker (`subject_job`).
        // Transform Selection routes to the gizmo wearing its Selection
        // target now (P2.2): the drag resamples the selection mask and
        // commits as one undoable SetSelection step. It has no reason here.
        // Select ▸ Reselect/Save/Load Selection are wired (the store lives on
        // the document, see `Document::stored_selection` / `saved_selections`)
        // and Save/Load ask their name / entry in a dialog (W3-H), so they
        // have no entry here. Color Range and the five Modify rows open their
        // dialogs too (W3-H).

        // ---- Filter --------------------------------------------------------
        // Every filter opens its parameter dialog now, including the two whose
        // schema defaults are the identity (Custom's convolution kernel,
        // Offset's zero displacement) — a dialog is exactly what they were
        // waiting for. The gallery is hosted too, for the same reason.

        // ---- Help ----------------------------------------------------------
        // Nothing: all four are wired.
        _ => return None,
    })
}

/// Fold a pick into the frame's output.
pub fn record(pick: Pick, out: &mut ChromeOutput) {
    match pick {
        Pick::Action(action) => out.actions.push(action),
        Pick::StartRecording => out
            .actions_transport
            .push(crate::chrome::ActionsTransport::StartRecording),
        Pick::StopRecording => out
            .actions_transport
            .push(crate::chrome::ActionsTransport::StopRecording),
        Pick::ReplayRecording => out
            .actions_transport
            .push(crate::chrome::ActionsTransport::ReplayRecording),
        Pick::Command(command) => out.commands.push(command),
        Pick::Menu(action) => out.menu.push(action),
        // `gesture` is filled in by `Chrome::harvest`, which is the only place
        // that knows whether the pointer is still down.
        Pick::Kind { layer, kind } => out.layer_kind.push(crate::chrome::KindEdit {
            layer,
            kind,
            gesture: None,
        }),
        Pick::OpenRecent(path) => out.open_recent = Some(path),
        Pick::Preferences(prefs) => out.preferences = Some(*prefs),
        Pick::Workspace(intent) => out.workspace.push(*intent),
        Pick::Tool(tool) => out.select_tool = Some(tool),
        Pick::ToolChoice(tool, key, index) => {
            out.select_tool = Some(tool);
            out.tool_choice = Some((tool, key.to_string(), index));
        }
        Pick::SelectLayer(layer) => out.select_layer = Some(layer),
        Pick::SelectLayers(layers, active) => out.select_layers = Some((layers, active)),
        Pick::EditTarget(kind) => out.edit_target = Some(kind),
        Pick::EnterTextLayer(layer) => out.enter_text_layer = Some(layer),
        Pick::History(depth) => out.history_jump = Some(depth),
        Pick::Zoom(zoom) => out.set_zoom = Some(zoom),
        Pick::ViewCenter(center) => out.set_view_center = Some(center),
        Pick::Foreground(rgba) => out.set_foreground = Some(rgba),
        Pick::Background(rgba) => out.set_background = Some(rgba),
        Pick::OpenColorPicker(target) => out.color_picker = Some(target),
        Pick::OpenGradientEditor => out.gradient_editor = true,
        Pick::OpenBrushEditor => out.brush_editor = true,
        Pick::ConfirmTool => out.confirm_tool = true,
        Pick::InsertGlyph(layer, text) => out.insert_glyphs.push((layer, text)),
    }
}

/// How one item resolves *for this build*: either something to do, or a
/// sentence saying why it is off.
///
/// Exposed so the enablement rule is testable without a window; [`draw`] is
/// the only caller that paints it.
pub fn resolve(action: MenuAction, context: &MenuContext, editor: &Editor) -> Result<Pick, String> {
    let intent = resolve_intent(action, context, editor)?;
    pick(&intent, editor).ok_or_else(|| refusal(action))
}

/// The [`Intent`] an enabled row hands the chrome when it is clicked, or the
/// sentence saying why the row is off.
///
/// This is what [`draw`] paints and what a click sends: the *intent*, not the
/// [`Pick`], because the chrome routes an intent — the dialog host is asked
/// first, and only an intent no dialog answers is picked and recorded. A row
/// whose intent [`pick`] cannot answer is off, so a click can never hand the
/// chrome something it has no road for.
pub fn resolve_intent(
    action: MenuAction,
    context: &MenuContext,
    editor: &Editor,
) -> Result<Intent, String> {
    // W3-A: a View toggle this build cannot honour is greyed with its reason
    // rather than refused only after the click.
    if let MenuAction::ToggleView(flag) = action {
        if !context.view.get(flag) {
            if let Some(reason) = crate::chrome::view_flag_refusal(flag) {
                return Err(reason.to_string());
            }
        }
    }
    match action.resolve(context) {
        Resolution::Disabled(reason) => Err(reason.to_string()),
        Resolution::Enabled(intent) => {
            if pick(&intent, editor).is_some() {
                Ok(intent)
            } else {
                Err(refusal(action))
            }
        }
    }
}

/// The specific sentence for a row this build cannot perform, when there is
/// one. [`NOT_WIRED`] is the last resort and no menu item reaches it — see
/// `no_menu_item_falls_back_to_the_generic_refusal`.
fn refusal(action: MenuAction) -> String {
    unavailable_reason(action).unwrap_or(NOT_WIRED).to_string()
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

/// Draw the menu bar and hand the chrome the intent of whatever row was
/// clicked.
///
/// `on_click` is `Chrome::menu_click`: the click is *routed*, not recorded,
/// so a row whose dialog the chrome hosts opens that dialog, and only a row
/// with no dialog is picked and recorded — the road every panel control's
/// intent already travels. `context` is built by the caller because it needs
/// the chrome's live workspace (dock, view flags) and the editor's clipboard
/// probe, and the chrome is what holds both.
pub fn draw(
    ctx: &egui::Context,
    editor: &Editor,
    context: &MenuContext,
    on_click: &mut dyn FnMut(Intent),
) {
    let menus = menus(editor);
    egui::TopBottomPanel::top("raster-menu-bar")
        // The header band: the menu bar and the options bar under it share
        // the header shade, the columns beneath them the panel shade.
        .frame(crate::chrome::panel_frame(
            ctx,
            design::SurfaceRole::Header,
            design::Space::Hair,
        ))
        .show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                for menu in &menus {
                    ui.menu_button(menu.title, |ui| {
                        entries(ui, &menu.entries, context, editor, on_click);
                    });
                }
            });
        });
}

fn entries(
    ui: &mut egui::Ui,
    entries: &[Entry],
    context: &MenuContext,
    editor: &Editor,
    on_click: &mut dyn FnMut(Intent),
) {
    for entry in entries {
        match entry {
            Entry::Item(action) => item(ui, *action, context, editor, on_click),
            Entry::Separator => {
                ui.separator();
            }
            Entry::Submenu {
                label,
                entries: children,
            } => {
                // A submenu whose every child is off is itself off, and says so
                // rather than opening onto a list of dead rows.
                let live = children
                    .iter()
                    .flat_map(Entry::actions)
                    .any(|a| resolve_intent(a, context, editor).is_ok());
                if live {
                    ui.menu_button(*label, |ui| {
                        self::entries(ui, children, context, editor, on_click);
                    });
                } else {
                    ui.add_enabled(false, egui::Button::new(*label))
                        .on_disabled_hover_text("Nothing in this submenu is available right now");
                }
            }
        }
    }
}

fn item(
    ui: &mut egui::Ui,
    action: MenuAction,
    context: &MenuContext,
    editor: &Editor,
    on_click: &mut dyn FnMut(Intent),
) {
    let outcome = resolve_intent(action, context, editor);
    // A checkable row reserves the gutter with spaces and the tick is *drawn*
    // into it below. It used to be a "✓" in the label, and U+2713 is not in the
    // font egui loads, so every checked row showed a tofu box.
    let checked = action.checked(context);
    let check = if checked.is_some() { "     " } else { "" };
    let label = format!("{check}{}", action.label_in(context));

    let mut button = egui::Button::new(label);
    if let Some(chord) = action.shortcut() {
        button = button.shortcut_text(chord.to_string());
    }
    let enabled = outcome.is_ok();
    let response = ui.add_enabled(enabled, button);
    if checked == Some(true) {
        let side = response
            .rect
            .height()
            .min(design::current_tokens(ui).metrics.min_hit_target);
        let gutter = egui::Rect::from_center_size(
            egui::pos2(response.rect.left() + side * 0.5, response.rect.center().y),
            egui::Vec2::splat(side),
        );
        let role = if enabled {
            design::TextRole::Primary
        } else {
            design::TextRole::Disabled
        };
        ui::icons::paint_ui_icon(ui, gutter, "check", role);
    }
    match outcome {
        Ok(intent) => {
            if response.clicked() {
                on_click(intent);
                ui.close_menu();
            }
        }
        Err(reason) => {
            response.on_disabled_hover_text(reason);
        }
    }
}

// ---------------------------------------------------------------------------
// Performing a menu operation against the live document
// ---------------------------------------------------------------------------

/// One pixel pipeline, shared by the Filter menu, Image ▸ Adjustments and
/// Select ▸ Grow/Similar/Color Range.
///
/// A layer's stored tiles are read into a canvas-sized RGBA8 buffer, that
/// buffer becomes a [`filters::FilterBuffer`] — linear, premultiplied, the form
/// both `filters` and `adjustments` are defined on — the operation runs, and
/// the result goes back as tiles referenced by one undoable
/// [`Command::PaintTiles`].
pub(crate) mod pixels {
    use std::collections::HashSet;

    use editor_core::pixels::{PixelTarget, TileEdit};
    use editor_core::{Command, Selection};
    use glam::IVec2;
    use layer_model::LayerId;
    use raster::{TileCoord, TileGrid, TILE_SIZE};

    use crate::doc::OpenDocument;

    /// Card 053: a selection's coverage as mask tiles - the focused
    /// selection-to-coverage helper card 057's four mask-creation actions
    /// will reuse and extend.
    ///
    /// Each entry is one full `TILE_SIZE` coverage tile (one byte per pixel:
    /// 255 selected, 0 not). Tiles the selection never reaches are ABSENT
    /// from the list, which is the compositor's missing-tile convention:
    /// an absent mask tile is fully hidden. A tile entirely inside the
    /// selection is present and all-255 (an absent tile could not express
    /// "visible"), and a tile the selection boundary crosses carries the
    /// per-pixel coverage - fractional lasso/wand samples map through
    /// unchanged.
    ///
    /// `Selection::None` yields nothing: with no selection every pixel is
    /// selected, and "visible everywhere" needs no mask at all - the caller
    /// pastes unmasked instead.
    pub fn selection_coverage_tiles(
        selection: &Selection,
        canvas_w: u32,
        canvas_h: u32,
    ) -> Vec<(TileCoord, Vec<u8>)> {
        selection_coverage_tiles_with(selection, canvas_w, canvas_h, false)
    }

    /// The same coverage, inverted: 255 becomes 0 and the other way round.
    /// Card 057's Hide Selection rides this — and the same absent-tile
    /// rule does the last bit of work for free: a tile FULLY inside the
    /// selection inverts to all-zero, which is exactly what an ABSENT tile
    /// means, so it is omitted rather than stored.
    pub fn selection_coverage_tiles_with(
        selection: &Selection,
        canvas_w: u32,
        canvas_h: u32,
        invert: bool,
    ) -> Vec<(TileCoord, Vec<u8>)> {
        let ts = TILE_SIZE as i32;
        // The pixel-level test one pixel answers, specialized per shape.
        let raw: Box<dyn Fn(IVec2) -> u8> = match selection {
            Selection::None => return Vec::new(),
            Selection::Rect { min, max } => {
                let (min, max) = (*min, *max);
                Box::new(move |p| {
                    (p.x >= min.x && p.x < max.x && p.y >= min.y && p.y < max.y) as u8 * 255
                })
            }
            Selection::Mask(m) => {
                let m = m.clone();
                Box::new(move |p| m.coverage_at(p))
            }
        };
        let coverage_at: Box<dyn Fn(IVec2) -> u8> = if invert {
            Box::new(move |p| 255 - raw(p))
        } else {
            raw
        };
        // Tiles that can hold selected pixels, clipped to the canvas: the
        // pasted layer is canvas-sized, so tiles past its edge are pointless.
        // The loop is bounded by the SELECTION's bounds — an 8K canvas with
        // a small lasso must not walk 4 billion pixels.
        let (tiles_x, tiles_y) = (canvas_w.div_ceil(TILE_SIZE), canvas_h.div_ceil(TILE_SIZE));
        let mut out = Vec::new();
        let x_range = match selection.bounds() {
            Some((min, max)) => (min.x.div_euclid(ts)..=((max.x - 1).max(0)).div_euclid(ts))
                .map(|t| t.clamp(0, tiles_x as i32 - 1))
                .collect::<Vec<i32>>(),
            None => (0..tiles_x as i32).collect(),
        };
        let y_range = match selection.bounds() {
            Some((min, max)) => (min.y.div_euclid(ts)..=((max.y - 1).max(0)).div_euclid(ts))
                .map(|t| t.clamp(0, tiles_y as i32 - 1))
                .collect::<Vec<i32>>(),
            None => (0..tiles_y as i32).collect(),
        };
        for ty in y_range {
            for tx in x_range.clone() {
                let tile_origin = IVec2::new(tx * ts, ty * ts);
                let mut coverage = vec![0u8; (TILE_SIZE * TILE_SIZE) as usize];
                let mut any = false;
                for py in 0..ts {
                    for px in 0..ts {
                        let p = tile_origin + IVec2::new(px, py);
                        if p.x < 0 || p.y < 0 || p.x >= canvas_w as i32 || p.y >= canvas_h as i32 {
                            continue;
                        }
                        let c = coverage_at(p);
                        coverage[(py * ts + px) as usize] = c;
                        any |= c != 0;
                    }
                }
                if !any {
                    continue;
                }
                out.push((TileCoord::new(tx, ty, 0), coverage));
            }
        }
        out
    }

    /// Which of the four mask-creation ops a coverage computation serves.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum MaskCoverageMode {
        RevealAll,
        HideAll,
        RevealSelection,
        HideSelection,
    }

    /// Card 057: mask-creation coverage for a layer, in the LAYER's local
    /// tile space. A new mask attaches LINKED with an identity extra
    /// transform, so the compositor resolves its coverage through the layer
    /// transform — a document-space selection must be pre-imaged through
    /// that transform's inverse before it can confine anything, or the
    /// revealed area lands displaced by exactly the layer's transform.
    ///
    /// The walk covers the union of the layer's own content tiles and the
    /// canvas rectangle's pre-image, because:
    /// - Reveal All must reveal every pixel the layer could ever show (its
    ///   content tiles — an oversized placed layer extends past the canvas),
    ///   and absent tiles mean hidden, so "revealed" has to be STORED;
    /// - Hide Selection must REVEAL the layer's off-canvas content too
    ///   (inverted coverage outside the canvas is 255), which the content
    ///   tiles also carry.
    ///
    /// Per pixel: map the layer-local point through `layer_to_doc` and test
    /// the selection there. A document point past the canvas edge is outside
    /// every selection (selections live in the canvas), so Hide Selection
    /// reveals it and Reveal Selection conceals it.
    ///
    /// All-zero tiles are omitted (absent = hidden — the same statement for
    /// less memory), which is what makes Hide Selection's fully-covered
    /// interior tiles free.
    pub fn mask_creation_coverage(
        selection: &Selection,
        layer_to_doc: glam::Affine2,
        layer_content_tiles: &[TileCoord],
        canvas_w: u32,
        canvas_h: u32,
        mode: MaskCoverageMode,
    ) -> Result<Vec<(TileCoord, Vec<u8>)>, String> {
        if matches!(mode, MaskCoverageMode::HideAll) {
            // Absent tiles mean hidden: the empty mask IS the requested
            // state, stated by the convention rather than by megabytes of
            // zeros.
            return Ok(Vec::new());
        }
        if matches!(
            mode,
            MaskCoverageMode::RevealSelection | MaskCoverageMode::HideSelection
        ) && matches!(selection, Selection::None)
        {
            // "Hide nothing" / an all-pixels "reveal" is the OTHER op's job
            // (Reveal All); the caller refuses a missing selection for these
            // two.
            return Ok(Vec::new());
        }
        let ts = TILE_SIZE as i32;
        // The layer-space points one canvas corner maps to: the pre-image of
        // the canvas rectangle bounds the tiles the canvas can possibly show.
        let corners = [
            glam::Vec2::ZERO,
            glam::Vec2::new(canvas_w as f32, 0.0),
            glam::Vec2::new(0.0, canvas_h as f32),
            glam::Vec2::new(canvas_w as f32, canvas_h as f32),
        ];
        let to_layer = layer_to_doc.inverse();
        let mut min_tx = i32::MAX;
        let mut min_ty = i32::MAX;
        let mut max_tx = i32::MIN;
        let mut max_ty = i32::MIN;
        let mut grow = |p: glam::Vec2| {
            let tx = (p.x as i32).div_euclid(ts);
            let ty = (p.y as i32).div_euclid(ts);
            min_tx = min_tx.min(tx);
            min_ty = min_ty.min(ty);
            max_tx = max_tx.max(tx);
            max_ty = max_ty.max(ty);
        };
        for c in corners {
            grow(to_layer.transform_point2(c));
        }
        for coord in layer_content_tiles {
            grow(glam::Vec2::new(
                (coord.x * ts) as f32,
                (coord.y * ts) as f32,
            ));
            grow(glam::Vec2::new(
                ((coord.x + 1) * ts) as f32,
                ((coord.y + 1) * ts) as f32,
            ));
        }
        let in_canvas = |p: glam::Vec2| {
            p.x >= 0.0 && p.y >= 0.0 && p.x < canvas_w as f32 && p.y < canvas_h as f32
        };
        // The pre-image of a strongly minified layer explodes (a 1% scale on
        // a 300px canvas spans ~100 tiles per axis ... times 300). Cap the
        // walk and refuse: storing hundreds of thousands of all-255 tiles in
        // one undoable command is not a mask creation, it is a memory event.
        // The bound is generous - a 16384px canvas is 64x64 = 4096 tiles.
        const MAX_COVERAGE_TILES_PER_AXIS: i32 = 256;
        if (max_tx - min_tx + 1) > MAX_COVERAGE_TILES_PER_AXIS
            || (max_ty - min_ty + 1) > MAX_COVERAGE_TILES_PER_AXIS
        {
            // Say what happened instead of storing gigabytes in one undoable
            // command — or worse, silently hiding a Reveal All.
            return Err(
                "The layer's transform spreads the mask over too large an area".to_string(),
            );
        }
        let mut out = Vec::new();
        for ty in min_ty..=max_ty {
            for tx in min_tx..=max_tx {
                let mut coverage = vec![0u8; (TILE_SIZE * TILE_SIZE) as usize];
                let mut any = false;
                for py in 0..ts {
                    for px in 0..ts {
                        let local = glam::Vec2::new(
                            (tx * ts + px) as f32 + 0.5,
                            (ty * ts + py) as f32 + 0.5,
                        );
                        let doc = layer_to_doc.transform_point2(local);
                        let selected = if in_canvas(doc) {
                            // `Selection::coverage_at` is already a 0..1 f32.
                            selection.coverage_at(glam::IVec2::new(doc.x as i32, doc.y as i32))
                        } else {
                            // A document point past the canvas edge is
                            // outside every selection (selections live in
                            // the canvas): Hide Selection reveals it, Reveal
                            // Selection conceals it.
                            0.0
                        };
                        let c = match mode {
                            MaskCoverageMode::RevealAll => 1.0,
                            MaskCoverageMode::HideAll => unreachable!("returned above"),
                            MaskCoverageMode::RevealSelection => selected,
                            MaskCoverageMode::HideSelection => 1.0 - selected,
                        };
                        let v = (c * 255.0).round() as u8;
                        coverage[(py * ts + px) as usize] = v;
                        any |= v != 0;
                    }
                }
                if !any {
                    continue;
                }
                out.push((TileCoord::new(tx, ty, 0), coverage));
            }
        }
        Ok(out)
    }

    /// A layer's pixels, flattened over the canvas rectangle.
    ///
    /// Tiles outside the canvas are dropped and absent tiles read as
    /// transparent black, which is exactly what
    /// [`raster::TileGrid::to_rgba8`] promises for the same data. An RGBA16
    /// tile (a 16-bit document) is rounded to RGBA8 on the way out.
    pub fn read_layer(doc: &OpenDocument, layer: LayerId) -> Vec<u8> {
        let w = doc.document.width() as usize;
        let h = doc.document.height() as usize;
        let mut out = vec![0u8; w * h * 4];
        let Some(map) = doc.document.layer_tiles(layer) else {
            return out;
        };
        let ts = TILE_SIZE as usize;
        let need = ts * ts * 4;
        for (coord, hash) in map.iter() {
            if coord.level != 0 {
                continue;
            }
            let Some(stored) = compositor::TileSource::tile(&doc.tiles, hash) else {
                continue;
            };
            // W4-F: a 16-bit tile is read rounded to 8 bits. Every caller
            // works in RGBA8 and writes back through `write_layer`, and the
            // apply boundary (`doc_depth.rs`) widens that output again,
            // keeping the exact 16-bit value of every pixel left unchanged.
            let bytes = raster::rgba8_view(stored);
            if bytes.len() != need {
                continue;
            }
            let ox = coord.x as i64 * ts as i64;
            let oy = coord.y as i64 * ts as i64;
            for row in 0..ts {
                let y = oy + row as i64;
                if y < 0 || y >= h as i64 {
                    continue;
                }
                let x0 = ox.max(0);
                let x1 = (ox + ts as i64).min(w as i64);
                if x1 <= x0 {
                    continue;
                }
                let n = (x1 - x0) as usize * 4;
                let s = (row * ts + (x0 - ox) as usize) * 4;
                let d = (y as usize * w + x0 as usize) * 4;
                out[d..d + n].copy_from_slice(&bytes[s..s + n]);
            }
        }
        out
    }

    /// The command that makes `rgba` the layer's pixels, storing the bytes it
    /// needs in the document's tile source on the way.
    ///
    /// Tiles the layer used to reference and the new image does not cover are
    /// *cleared* rather than left behind, so a rewrite cannot leave a stale
    /// tile hanging off the edge of the canvas.
    pub fn write_layer(
        doc: &mut OpenDocument,
        layer: LayerId,
        rgba: &[u8],
        label: &str,
    ) -> Result<Command, String> {
        let (w, h) = (doc.document.width(), doc.document.height());
        let grid = TileGrid::from_rgba8(w, h, rgba).map_err(|e| e.to_string())?;
        let previous: Vec<TileCoord> = doc
            .document
            .layer_tiles(layer)
            .map(|m| m.iter().map(|(c, _)| c).collect())
            .unwrap_or_default();
        let mut covered = HashSet::new();
        let mut edits = Vec::new();
        for (coord, tile) in grid.iter() {
            covered.insert(coord);
            let hash = doc.tiles.insert_bytes(tile.data().to_vec());
            edits.push(TileEdit::set(coord, hash));
        }
        for coord in previous {
            if !covered.contains(&coord) {
                edits.push(TileEdit::clear(coord));
            }
        }
        let paint = Command::paint_tiles(PixelTarget::Layer(layer), edits)
            .map_err(|e: editor_core::CommandError| e.to_string())?;
        Ok(Command::Transaction {
            label: label.to_string(),
            commands: vec![paint],
        })
    }

    /// Card 058: replace a layer's mask coverage with `coverage` (CANVAS-space
    /// w×h grayscale bytes), resampled into the mask's LOCAL store space
    /// through the mask pose. Emits ONLY the tiles that actually change —
    /// an all-zero result tile is CLEARED (an absent tile means hidden), an
    /// unchanged tile is skipped, and store tiles outside the canvas
    /// pre-image are left untouched (a fill only claims the canvas). One
    /// labelled transaction = one undo step; the layer's pixel tiles are
    /// never touched — a coverage edit is a one-channel delta, never the
    /// four-channel ColorPatch a content edit would be.
    pub fn write_mask_coverage(
        doc: &mut OpenDocument,
        layer: LayerId,
        coverage: &[u8],
        label: &str,
    ) -> Result<Command, String> {
        let (w, h) = (doc.document.width(), doc.document.height());
        if coverage.len() != (w as usize) * (h as usize) {
            return Err("Coverage buffer does not match the canvas".to_string());
        }
        // Resolving the mask id doubles as the existence check.
        doc.document
            .layers
            .get(layer)
            .and_then(|l| l.mask_id())
            .ok_or_else(|| "The layer has no mask".to_string())?;
        let ts = raster::TILE_SIZE as usize;
        let existing: std::collections::HashMap<raster::TileCoord, raster::TileHash> = doc
            .document
            .mask_tiles(layer)
            .map(|m| m.iter().collect())
            .unwrap_or_default();
        let pose_inverse = crate::menu_bridge::mask_pose(doc, layer).inverse();
        // The store tiles to (re)write: the pre-image of the CANVAS rectangle
        // through the pose inverse — the mask-local area the canvas
        // actually shows. (Corner-grow + cap, the same walk shape card 057's
        // coverage helper uses.)
        let mut min_tx = i32::MAX;
        let mut min_ty = i32::MAX;
        let mut max_tx = i32::MIN;
        let mut max_ty = i32::MIN;
        for corner in [
            glam::Vec2::ZERO,
            glam::Vec2::new(w as f32, 0.0),
            glam::Vec2::new(0.0, h as f32),
            glam::Vec2::new(w as f32, h as f32),
        ] {
            let (mx, my) = crate::menu_bridge::canvas_to_mask(pose_inverse, corner);
            let (mx2, my2) =
                crate::menu_bridge::canvas_to_mask(pose_inverse, corner + glam::Vec2::splat(1.0));
            for (x, y) in [(mx, my), (mx2, my2)] {
                let (tx, ty) = (x.div_euclid(ts as i32), y.div_euclid(ts as i32));
                min_tx = min_tx.min(tx);
                min_ty = min_ty.min(ty);
                max_tx = max_tx.max(tx);
                max_ty = max_ty.max(ty);
            }
        }
        // A strongly minified pose explodes the pre-image; refuse rather
        // than emit a memory event (card 057's bound).
        const MAX_COVERAGE_TILES_PER_AXIS: i32 = 256;
        if (max_tx - min_tx + 1) > MAX_COVERAGE_TILES_PER_AXIS
            || (max_ty - min_ty + 1) > MAX_COVERAGE_TILES_PER_AXIS
        {
            return Err(
                "The mask's transform spreads the canvas over too large an area".to_string(),
            );
        }
        let mut edits = Vec::new();
        for ty in min_ty..=max_ty {
            for tx in min_tx..=max_tx {
                let coord = raster::TileCoord::new(tx, ty, 0);
                let old_bytes: Vec<u8> = existing
                    .get(&coord)
                    .and_then(|hash| compositor::TileSource::tile(&doc.tiles, *hash))
                    .map(|b| b.to_vec())
                    .unwrap_or_else(|| vec![0u8; ts * ts]);
                let mut bytes = old_bytes.clone();
                let mut in_canvas = false;
                for py in 0..ts {
                    for px in 0..ts {
                        // This store pixel's document position; pixels that
                        // land off-canvas keep the coverage they already
                        // have — the fill only claims the canvas.
                        let (cx, cy) = crate::menu_bridge::mask_to_canvas(
                            pose_inverse.inverse(),
                            glam::Vec2::new(
                                (tx * ts as i32 + px as i32) as f32,
                                (ty * ts as i32 + py as i32) as f32,
                            ),
                        );
                        if cx < 0 || cy < 0 || cx >= w as i64 || cy >= h as i64 {
                            continue;
                        }
                        bytes[py * ts + px] = coverage[cy as usize * w as usize + cx as usize];
                        in_canvas = true;
                    }
                }
                if !in_canvas {
                    continue;
                }
                if bytes == old_bytes {
                    continue;
                }
                if bytes.iter().all(|&v| v == 0) {
                    // All-zero IS the absent-tile meaning: an existing tile
                    // whose coverage vanished must be CLEARED, not left
                    // behind silently revealing stale coverage.
                    if existing.contains_key(&coord) {
                        edits.push(TileEdit::clear(coord));
                    }
                    continue;
                }
                let hash = doc.tiles.insert_bytes(bytes);
                edits.push(TileEdit::set(coord, hash));
            }
        }
        if edits.is_empty() {
            return Err("The mask edit would change nothing".to_string());
        }
        // `PixelTarget::Mask` names the LAYER; resolve_pixel_key maps it to
        // the layer's mask (existence checked at apply time).
        let paint = Command::paint_tiles(PixelTarget::Mask(layer), edits)
            .map_err(|e: editor_core::CommandError| e.to_string())?;
        Ok(Command::Transaction {
            label: label.to_string(),
            commands: vec![paint],
        })
    }

    /// Fold `after` back towards `before` wherever the selection does not
    /// fully cover the pixel.
    ///
    /// This is what makes every operation in this module honour the marquee.
    /// [`Selection::None`] covers everything ([`Selection::coverage_at`] answers
    /// 1.0), so the no-selection case is the whole layer and needs no branch of
    /// its own — but it does get a fast path, because walking two million
    /// pixels to multiply each by one is a waste.
    ///
    /// W7-C: generic over the sample depth, so a 16-bit layer (`u16`) is
    /// blended at 16 bits; the `u8` arithmetic is the 8-bit one unchanged.
    pub fn mask_by_selection<S: raster::depth::DepthSample>(
        before: &[S],
        after: &mut [S],
        sel: &Selection,
        w: u32,
        h: u32,
    ) {
        if sel.is_none() {
            return;
        }
        for y in 0..h {
            for x in 0..w {
                let c = sel.coverage_at(IVec2::new(x as i32, y as i32));
                if c >= 1.0 {
                    continue;
                }
                let i = (y as usize * w as usize + x as usize) * 4;
                for k in 0..4 {
                    let mixed = before[i + k].to_f32() * (1.0 - c) + after[i + k].to_f32() * c;
                    after[i + k] = S::from_f32_rounded(mixed);
                }
            }
        }
    }
}

/// Perform a menu operation that needs the live document.
///
/// The other half of [`Pick::Menu`]: the bridge decides *whether* an item is
/// usable during enablement, and this decides what it does — once, on the
/// click, with `&mut Editor`.
///
/// `Ok` carries the sentence the status bar shows; `Err` carries the reason it
/// did not happen. Both are shown: an operation that quietly did nothing is the
/// defect this whole module exists to stop.
///
/// # The parameters are the schema's defaults
///
/// `ui::dialogs` generates a parameter dialog for every filter, and the shell
/// hosts no dialog surface to draw one in — so a filter here runs at
/// [`ui::dialogs::FilterParams::defaults`], and the status line says so in
/// those words. That is a real shortfall against the ellipsis in the menu
/// label, and it is stated at the point of use rather than hidden: the
/// alternative was leaving all forty-one filters greyed out, which is what this
/// wave exists to end.
pub fn perform(action: MenuAction, editor: &mut Editor) -> Result<String, String> {
    use ui::menu::{CanvasRotation as CR, MaskOp, TransformOp as T};
    install_smart_filter_runner();

    /// The Help destinations (P3.14): open the URL in the user's browser and
    /// report it. The open rides the injected [`UrlLauncher`] seam — the
    /// shipped `BrowserUrls` opens the platform browser, tests inject a
    /// recorder so the digest gate can reach these arms without opening tabs
    /// (C5). The URL is reported either way, which is what the gate's
    /// message-length check wants.
    fn open_help_url(
        editor: &mut Editor,
        url: &str,
        fallback_prefix: &str,
    ) -> Result<String, String> {
        if editor.url_launcher_mut().open_url(url) {
            Ok(format!("Opened {url}"))
        } else {
            Ok(format!("{fallback_prefix} {url}"))
        }
    }

    let outcome = match action {
        // ---- File ----------------------------------------------------------
        // W10-E: a confirmed File Info dialog parks its XMP fields for this
        // arm; with nothing parked (the chord) the facts window toggles.
        MenuAction::FileInfo => match crate::file_extras::take_confirmed_file_info() {
            Some(fields) => crate::file_extras::set_file_info(editor, fields),
            None => {
                editor.toggle_file_info();
                Ok("File Info…".to_string())
            }
        },
        // W10-E: Batch / Convert Formats, Export Color Lookup / PDF,
        // Variables, Vectorize Bitmap.
        action if crate::file_extras::performs(action) => {
            crate::file_extras::perform(action, editor)
        }
        MenuAction::ExportLayers => editor.export_layers(),
        // W4-H: one file per committed Slice-tool region.
        MenuAction::ExportSlices => crate::slices_export::export_slices(editor),
        // W10-A: the Slice Options dialog's parked answer.
        MenuAction::SliceOptions => crate::slices_export::perform_slice_options(editor),
        // W8-C: one file per artboard.
        MenuAction::ExportArtboards => crate::artboard_export::export_artboards(editor),
        MenuAction::PlaceEmbedded => editor.place_from_dialog(false),
        MenuAction::PlaceLinked => editor.place_from_dialog(true),
        MenuAction::Print => editor.print_pdf(),
        MenuAction::Rasterize(ui::menu::RasterizeTarget::Text)
        | MenuAction::Rasterize(ui::menu::RasterizeTarget::Shape)
        | MenuAction::Rasterize(ui::menu::RasterizeTarget::LayerStyle)
        | MenuAction::Rasterize(ui::menu::RasterizeTarget::Layer)
        | MenuAction::Rasterize(ui::menu::RasterizeTarget::SmartObject) => editor.rasterize_layer(),
        // W9-F: Layer > Combine Shapes.
        MenuAction::CombineShapes(op) => combine_shapes(editor, op),
        // W9-K: Layer > Text.
        MenuAction::WarpText(item) => warp_text(editor, item),
        MenuAction::ConvertTextToShape => convert_text_to_shape(editor),
        MenuAction::DefinePattern => editor.define_pattern_from_selection(),
        MenuAction::CopyLayerStyle => editor.copy_layer_style(),
        MenuAction::PasteLayerStyle => editor.paste_layer_style(),
        MenuAction::DefineStylePreset => editor.define_style_preset(),
        MenuAction::ApplyStylePreset => editor.apply_latest_style_preset(),
        MenuAction::DefineBrush => editor.define_brush_preset(),
        // W10-A.
        MenuAction::DefineCustomShape => custom_shape::define_custom_shape(editor),
        MenuAction::Rasterize(ui::menu::RasterizeTarget::AllLayers) => editor.flatten_all_layers(),
        MenuAction::NewFillLayer(ui::menu::FillLayerKind::SolidColor) => {
            editor.new_solid_fill_layer()
        }
        MenuAction::NewFillLayer(ui::menu::FillLayerKind::Pattern) => {
            editor.new_pattern_fill_layer()
        }
        MenuAction::NewFillLayer(ui::menu::FillLayerKind::Gradient) => {
            editor.new_gradient_fill_layer()
        }
        MenuAction::ConvertToSmartObject => editor.convert_to_smart_object(),
        MenuAction::DuplicateDocument => editor.duplicate_document(),
        MenuAction::CloseAll => editor.close_all_documents(),
        MenuAction::EditSmartObjectContents => editor.edit_smart_object_contents(),
        MenuAction::ReplaceContents => editor.replace_from_dialog(),
        MenuAction::CommitSmartObjectContents => editor.commit_smart_object_contents(),
        // W10-I: the rest of the Smart Object submenu, Matting, Hide / Show
        // Layers (`layer_extras`).
        MenuAction::SmartObject(op) => layer_extras::smart_object(editor, op),
        MenuAction::Matting(op) => layer_extras::matting(editor, op),
        MenuAction::SmartFilter(op) => layer_extras::smart_filter(editor, op),
        MenuAction::HideLayers => layer_extras::set_layers_visible(editor, false),
        MenuAction::ShowLayers => layer_extras::set_layers_visible(editor, true),
        // W10-A: each click makes an independent link group.
        MenuAction::LinkLayers => link_groups::link_layers(editor),
        // ---- Filter --------------------------------------------------------
        MenuAction::ConvertForSmartFilters => convert_for_smart_filters(editor),
        // W10-D: a Displace the external-map dialog confirmed runs that map;
        // with nothing parked the row runs at its defaults like any filter.
        MenuAction::Filter(ui::menu::FilterId::Displace) => {
            match crate::dialog_host::take_confirmed_displace_map() {
                Some(spec) => displace_map_with(editor, &spec),
                None => run_filter(editor, ui::menu::FilterId::Displace),
            }
        }
        MenuAction::Filter(id) => run_filter(editor, id),

        // ---- Image ▸ Adjustments -------------------------------------------
        // The parameters are the dialog's when one was just confirmed (the
        // pick and the parameters leave `DialogHost::ui` in the same frame);
        // otherwise the adjustment's starting parameters, which is what a
        // click that opened no dialog — no pixel layer to preview — or a
        // chord asked for.
        MenuAction::ApplyAdjustment(id) => {
            let kind = crate::dialog_host::take_confirmed_adjustment(id)
                .unwrap_or_else(|| id.identity_kind());
            let label = format!("Apply {}", id.label());
            // W8-B: Levels and Curves on a Lab document run on L, a and b.
            color_mode::run_lab_tone(editor, &kind, &label).unwrap_or_else(|| {
                run_adjustment_kind(editor, &adjustments::Adjustment::from(&kind), &label)
            })
        }
        MenuAction::AutoTone => run_auto(editor, adjustments::AutoKind::Tone, "Auto Tone"),
        MenuAction::AutoContrast => {
            run_auto(editor, adjustments::AutoKind::Contrast, "Auto Contrast")
        }
        MenuAction::AutoColor => run_auto(editor, adjustments::AutoKind::Color, "Auto Color"),

        // ---- Image ▸ Image Rotation ----------------------------------------
        // Only the three that keep the canvas rectangle. The 90° pair and
        // Arbitrary change the document's size, and there is no command that
        // carries a resize, so they could not be undone; see
        // `unavailable_reason`.
        MenuAction::RotateCanvas(CR::Deg180) => {
            remap_all_layers(editor, "Rotate 180°", |x, y, w, h| (w - 1 - x, h - 1 - y))
        }
        MenuAction::RotateCanvas(CR::FlipHorizontal) => {
            remap_all_layers(editor, "Flip Canvas Horizontal", |x, y, w, _| {
                (w - 1 - x, y)
            })
        }
        MenuAction::RotateCanvas(CR::FlipVertical) => {
            remap_all_layers(editor, "Flip Canvas Vertical", |x, y, _, h| (x, h - 1 - y))
        }
        // Free Transform and its five modes route to the gizmo tool. W5-C:
        // the session no longer waits for a canvas press: the request is
        // parked with the tool it was invoked from, and the pointer begins
        // it over the selection bounds (or the layer's ink) at its next call
        // (`ToolPointer::begin_pending_session`), so the handles are up
        // before any click. The options bar's mode choice names the shape of
        // the drag, Enter commits one undoable step and hands the palette
        // back to the previous tool, Escape cancels. The mode itself is set
        // by the workspace option, which the Transform items also arrive as
        // a pick for (see resolve).
        MenuAction::FreeTransform => {
            crate::tool_input::request_free_transform(editor.tool());
            editor.set_tool(tools::ToolId::FreeTransform);
            Ok("Free Transform: drag a handle, Enter to commit, Escape to cancel".to_string())
        }
        MenuAction::Transform(T::Scale)
        | MenuAction::Transform(T::Rotate)
        | MenuAction::Transform(T::Skew)
        | MenuAction::Transform(T::Distort)
        | MenuAction::Transform(T::Perspective) => {
            // The menu bar never reaches here: `resolve` turns these items
            // into `Pick::ToolChoice`, which `Shell::apply_chrome` performs
            // (mode, tool and the session request). This arm serves a caller
            // that performs the action directly.
            crate::tool_input::request_free_transform(editor.tool());
            editor.set_tool(tools::ToolId::FreeTransform);
            Ok("Transform: drag a handle, Enter to commit, Escape to cancel".to_string())
        }
        MenuAction::CropToSelection => editor.crop_to_selection(),
        // W2-F: the options are the dialog's when one was just confirmed
        // (parked by `DialogHost::ui`, taken here in the same frame);
        // otherwise Photopea's defaults — transparent pixels, every side —
        // which is what a click that opened no dialog asked for.
        MenuAction::Trim => crate::layer_ops::trim_with(
            editor,
            crate::dialog_host::take_confirmed_trim().unwrap_or_default(),
        ),
        MenuAction::RotateCanvas(CR::Deg90Cw) => editor.rotate_canvas_90(true),
        MenuAction::RotateCanvas(CR::Deg90Ccw) => editor.rotate_canvas_90(false),
        MenuAction::RevealAll => {
            let command = {
                let doc = editor
                    .active_mut()
                    .ok_or_else(|| "No document is open".to_string())?;
                doc.reveal_all_command().map_err(|e| e.to_string())?
            };
            // A canvas that already contains every layer answers with an
            // empty transaction; recording that as an undo step would make
            // Ctrl+Z feel broken ("nothing happened, but I can undo it").
            if matches!(&command, Command::Transaction { commands, .. } if commands.is_empty()) {
                return Ok("Every layer already fits the canvas".to_string());
            }
            editor.apply_command(command);
            Ok("Revealed all layer content".to_string())
        }

        // ---- Edit ▸ Transform (the fixed ones) -----------------------------
        MenuAction::Transform(T::Rotate180) => {
            remap_active_layer(editor, "Rotate Layer 180°", |x, y, w, h| {
                (w - 1 - x, h - 1 - y)
            })
        }
        MenuAction::Transform(T::FlipHorizontal) => {
            remap_active_layer(editor, "Flip Layer Horizontal", |x, y, w, _| (w - 1 - x, y))
        }
        MenuAction::Transform(T::FlipVertical) => {
            remap_active_layer(editor, "Flip Layer Vertical", |x, y, _, h| (x, h - 1 - y))
        }
        // A 90° turn of a *layer* keeps the canvas, so unlike Image ▸ Image
        // Rotation it needs no resize: the layer is rotated about the canvas
        // centre and whatever leaves the canvas is cropped, exactly as the
        // pixel pipeline crops everything else.
        MenuAction::Transform(T::Rotate90Cw) => {
            remap_active_layer(editor, "Rotate Layer 90° CW", |x, y, w, h| {
                let (cx, cy) = ((w - 1) as f32 * 0.5, (h - 1) as f32 * 0.5);
                let (dx, dy) = (x as f32 - cx, y as f32 - cy);
                ((cx + dy).round() as i64, (cy - dx).round() as i64)
            })
        }
        MenuAction::Transform(T::Rotate90Ccw) => {
            remap_active_layer(editor, "Rotate Layer 90° CCW", |x, y, w, h| {
                let (cx, cy) = ((w - 1) as f32 * 0.5, (h - 1) as f32 * 0.5);
                let (dx, dy) = (x as f32 - cx, y as f32 - cy);
                ((cx - dy).round() as i64, (cy + dx).round() as i64)
            })
        }

        // ---- Edit ----------------------------------------------------------
        // W10-A: with the Slice Select tool and a picked slice, the key
        // deletes the slice rather than clearing pixels.
        MenuAction::ClearPixels => crate::slices_export::delete_picked_slice(editor)
            .unwrap_or_else(|| clear_selection(editor)),
        MenuAction::FillDialog => fill_selection(editor),
        MenuAction::StrokeDialog => stroke_selection(editor),
        MenuAction::ContentAwareScale(step) => content_aware_scale_layer(editor, step),
        MenuAction::Copy => copy(editor, false),
        MenuAction::CopyMerged => copy(editor, true),
        MenuAction::Cut => cut(editor),
        MenuAction::Paste => paste(editor, PasteMode::Plain),
        MenuAction::PasteInto => paste(editor, PasteMode::Into),
        // W4-H: Edit > Paste Special and Edit > Purge.
        MenuAction::PasteInPlace => paste(editor, PasteMode::InPlace),
        MenuAction::PasteOutside => paste(editor, PasteMode::Outside),
        MenuAction::Purge(target) => editor.purge(target),

        // ---- Select --------------------------------------------------------
        MenuAction::SelectAll => set_selection(editor, |_, w, h| {
            Ok(editor_core::Selection::Rect {
                min: glam::IVec2::ZERO,
                max: glam::IVec2::new(w as i32, h as i32),
            })
        })
        .map(|_| "Everything is selected".to_string()),
        MenuAction::Deselect => set_selection(editor, |_, _, _| Ok(editor_core::Selection::None))
            .map(|_| "Deselected".to_string()),
        MenuAction::InverseSelection => set_selection(editor, |sel, w, h| {
            selection::invert_selection(sel, canvas_rect(w, h)).map_err(|e| e.to_string())
        })
        .map(|_| "Selection inverted".to_string()),
        MenuAction::Modify(op) => modify_selection(editor, op),
        MenuAction::GrowSelection | MenuAction::SimilarSelection => {
            grow_or_similar(editor, action == MenuAction::GrowSelection)
        }
        MenuAction::ColorRange => color_range(editor),
        // W10-K: Select > Subject runs on the job worker (`subject_job`) and
        // lands as one undoable SetSelection step.
        MenuAction::SelectSubject => subject_job::start(editor),
        // Select ▸ Save / Load / Reselect — the store lives on the document
        // (`Document::stored_selection` / `saved_selections`); the selection
        // changes themselves ride history since card 056.
        MenuAction::SaveSelection => save_selection(editor),
        MenuAction::LoadSelection => load_selection(editor),
        // W9-A: Photopea's Select Pixels (a Ctrl+click on a layer or mask
        // thumbnail, or the layer-row menu) — one undoable SetSelection.
        MenuAction::SelectLayerPixels { layer, mask, op } => {
            select_layer_pixels(editor, layer, mask, op)
        }
        MenuAction::Reselect => reselect(editor),
        MenuAction::ToggleQuickMask => editor.toggle_quick_mask(),
        // W10-B: the Channels panel's alpha rows.
        MenuAction::EditAlphaChannel(index) => alpha_channel::open_alpha_channel(editor, index),
        MenuAction::CloseAlphaChannel => alpha_channel::close_alpha_channel(editor),
        // W7-D: all five modes convert, each as one undo step
        // (`color_mode.rs`). Indexed carries the Indexed Color dialog's
        // parked spec; a pick with nothing parked converts at its defaults.
        MenuAction::SetColorMode(mode) => {
            let indexed = if mode == ui::menu::ColorMode::Indexed {
                crate::dialog_host::take_confirmed_indexed()
            } else {
                None
            };
            // W10-H: Bitmap and Duotone carry their dialogs' parked answers
            // (the defaults when nothing is parked).
            let options = color_mode::ModeOptions {
                bitmap: (mode == ui::menu::ColorMode::Bitmap)
                    .then(image_dialogs::take_confirmed_bitmap)
                    .flatten(),
                duotone: (mode == ui::menu::ColorMode::Duotone)
                    .then(image_dialogs::take_confirmed_duotone)
                    .flatten(),
            };
            color_mode::set_color_mode_with(editor, mode, indexed, options)
        }
        // W10-H: Image > Apply Image / Calculations, with the dialog's
        // parked spec, or the dialog's opening spec when nothing is parked.
        MenuAction::ApplyImage => {
            let spec = match image_dialogs::take_confirmed_apply_image() {
                Some(spec) => spec,
                None => ui::dialogs::ApplyImageDialog::new(apply_image::source_documents(editor))
                    .confirm()
                    .ok_or("No document is open")?,
            };
            apply_image::apply_image(editor, spec)
        }
        MenuAction::Calculations => {
            let spec = match image_dialogs::take_confirmed_calculations() {
                Some(spec) => spec,
                None => ui::dialogs::CalculationsDialog::new(apply_image::source_documents(editor))
                    .confirm()
                    .ok_or("No document is open")?,
            };
            apply_image::calculations(editor, spec)
        }
        // W4-F: Image > Mode > 8/16 Bits/Channel. Every raster tile is
        // widened (8 -> 16, lossless) or rounded (16 -> 8) in ONE undoable
        // Transaction that also carries the depth (`doc_depth.rs`). The row
        // for the current depth is greyed with `conversion_reason`; a caller
        // that bypasses enablement gets the same sentence from the builder.
        // 16 -> 8 dithers, as Photoshop does with its default "Use Dither"
        // colour setting: the offset is under half an 8-bit step, so a pixel
        // that was already an exact 8-bit code comes back unchanged, and a
        // smooth 16-bit gradient does not band.
        MenuAction::SetBitDepth(depth) => {
            // W10-H: 32 Bits/Channel at either end (`crate::depth32`).
            if let Some(done) = crate::depth32::perform_set_bit_depth(editor, depth) {
                return done;
            }
            let command = editor
                .active_mut()
                .ok_or("No document is open")?
                .depth_conversion(depth.bits(), true)?;
            editor.apply_command(command);
            match editor.active().map(|d| d.document.meta.bit_depth) {
                Some(bits) if bits == depth.bits() => Ok(format!("Converted to {}", depth.label())),
                _ => Err(format!("Could not convert to {}", depth.label())),
            }
        }
        // W2-F: Select ▸ Refine Edge… confirmed. The parameters are the
        // dialog's (parked by `DialogHost::ui`); a click that opened no
        // dialog — no selection to refine — is answered with the reason.
        MenuAction::RefineEdge => match crate::dialog_host::take_confirmed_refine_edge() {
            Some(spec) => crate::layer_ops::refine_edge_with(editor, &spec),
            None => Err(dialog_refused(action, editor)),
        },
        // W7-H: Filter > Liquify... and Edit > Puppet Warp confirmed. The
        // warp is the dialog's (parked by `DialogHost::ui`); a click that
        // opened no dialog is answered with the reason.
        MenuAction::Liquify => match crate::dialog_host::take_confirmed_liquify() {
            Some(spec) => liquify_with(editor, &spec),
            None => Err(dialog_refused(action, editor)),
        },
        MenuAction::PuppetWarp => match crate::dialog_host::take_confirmed_puppet_warp() {
            Some(spec) => puppet_warp_with(editor, &spec),
            None => Err(dialog_refused(action, editor)),
        },
        // W10-G: Preset Manager, Fade, Auto-Align / Auto-Blend, Perspective
        // Warp: each applies its parked dialog answer (`crate::edit_gaps`).
        MenuAction::PresetManager
        | MenuAction::Fade
        | MenuAction::AutoAlignLayers
        | MenuAction::AutoBlendLayers
        | MenuAction::PerspectiveWarp => crate::edit_gaps::perform(action, editor),
        // W9-O: Filter > Blur Gallery > <kind>... confirmed; the blur is the
        // dialog's (parked by `DialogHost::ui`).
        MenuAction::BlurGallery(kind) => match crate::dialog_host::take_confirmed_blur_gallery() {
            Some(spec) if spec.kind() == kind => blur_gallery_with(editor, &spec),
            _ => Err(dialog_refused(action, editor)),
        },
        // W10-D: the Filter Gallery's effect list and Filter > Vanishing
        // Point confirmed (parked by `DialogHost::ui`); each lands as one undo
        // step.
        MenuAction::FilterGallery => match crate::dialog_host::take_confirmed_filter_gallery() {
            Some(spec) => filter_gallery_with(editor, &spec),
            None => Err(dialog_refused(action, editor)),
        },
        MenuAction::VanishingPoint => match crate::dialog_host::take_confirmed_vanishing_point() {
            Some(spec) => vanishing_point_with(editor, &spec),
            None => Err(dialog_refused(action, editor)),
        },

        // ---- Layer ---------------------------------------------------------
        MenuAction::LayerViaCopy => layer_via(editor, false),
        MenuAction::LayerViaCut => layer_via(editor, true),
        // W2-F: the layer_ops module. Duplicate Layer… arrives here only from
        // its name dialog (the host opens it for every click with a layer);
        // the parked name is the one typed, `None` names the copy itself.
        MenuAction::DuplicateLayer => crate::layer_ops::duplicate_layer(
            editor,
            crate::dialog_host::take_confirmed_duplicate_name(),
        ),
        MenuAction::AlignLayers(edge) => crate::layer_ops::align(editor, edge),
        MenuAction::DistributeLayers(axis) => crate::layer_ops::distribute(editor, axis),
        MenuAction::StampVisible => crate::layer_ops::stamp_visible(editor),
        MenuAction::SaveAsPsd => crate::layer_ops::save_as_psd(editor),
        MenuAction::GroupLayers => group_layers(editor),
        MenuAction::UngroupLayers => ungroup_layers(editor),
        MenuAction::MergeDown => merge(editor, MergeScope::Down),
        MenuAction::MergeVisible => merge(editor, MergeScope::Visible),
        MenuAction::FlattenImage => merge(editor, MergeScope::All),
        MenuAction::Mask(MaskOp::Toggle) => toggle_mask(editor, false),
        MenuAction::Mask(MaskOp::ToggleLink) => toggle_mask(editor, true),
        MenuAction::Mask(MaskOp::Apply) => apply_mask(editor),
        // Card 057: the four creation ops attach mask + coverage atomically.
        MenuAction::Mask(
            op @ (MaskOp::RevealAll
            | MaskOp::HideAll
            | MaskOp::RevealSelection
            | MaskOp::HideSelection),
        ) => create_mask(editor, op),
        // Card 058: inverting coverage is a one-channel pixel edit on the
        // mask's tile map — undoable, layer pixels untouched.
        MenuAction::Mask(MaskOp::Invert) => invert_mask(editor),
        // W9-G: Layer ▸ Vector Mask — one undoable mask patch each.
        MenuAction::VectorMask(op) => vector_mask_op(editor, op),
        // ---- Rows a dialog answers ---------------------------------------
        // Image Size, Canvas Size, Arbitrary rotation, Layer Style, the
        // Filter Gallery, Refine Mask and Remove Color Fringe are questions,
        // and the chrome's dialog host asks them
        // (`DialogHost::open_for_menu_action`); the confirmed value arrives
        // as a `DialogAction` one or more frames later. A click reaches here
        // only when the host had nothing to open a dialog *over* — no
        // document, no layer, no mask, no pixels — so the answer is the
        // reason, on the status line, rather than a silent nothing or the
        // "no implementation" shrug below.
        MenuAction::ImageSize
        | MenuAction::CanvasSize
        | MenuAction::RotateCanvas(CR::Arbitrary)
        | MenuAction::LayerStyle(_)
        | MenuAction::BlendingOptions
        | MenuAction::RefineMask
        | MenuAction::RemoveColorFringe
        | MenuAction::RenameLayer
        | MenuAction::NewGuide
        | MenuAction::NewGuideLayout => Err(dialog_refused(action, editor)),
        // W10-J: the View guide rows and the chords with no menu row.
        MenuAction::NewGuidesFromShape => view_keys::guides_from_shape(editor),
        MenuAction::DuplicateFreeTransform => view_keys::duplicate_free_transform(editor),
        MenuAction::BrushHardness(harder) => view_keys::brush_hardness(editor, harder),
        MenuAction::ToolOpacity(digit) => view_keys::tool_opacity(editor, digit),
        MenuAction::SelectAllLayers => {
            let doc = editor
                .active_mut()
                .ok_or_else(|| "No document is open".to_string())?;
            let all = doc.document.layers.iter_depth_first();
            if all.is_empty() {
                return Err("The document has no layers".to_string());
            }
            doc.document
                .set_layer_selection(all.clone())
                .map_err(|e| e.to_string())?;
            // A cursor that named a layer keeps it; an empty cursor takes the
            // top of the depth-first walk, which is what a fresh click on the
            // top row would name.
            if doc.document.active_layer().is_none() {
                let _ = doc.document.set_active_layer(all.first().copied());
            }
            Ok(format!("Selected {} layers", all.len()))
        }
        MenuAction::DeselectLayers => match editor.active_mut() {
            None => Err("No document is open".to_string()),
            Some(doc) if doc.document.active_layer().is_none() => {
                Err("No layer is selected".to_string())
            }
            Some(doc) => doc
                .document
                .set_active_layer(None)
                .map(|()| "No layer is selected now".to_string())
                .map_err(|e| e.to_string()),
        },

        // ---- View ----------------------------------------------------------
        // The view rotation lives on the DOCUMENT camera — the one the shell
        // renders from — so it is uprighted there. `context` enables the item
        // from that same camera, so a click can only arrive on a turned view;
        // the refusal below is for a caller that bypasses enablement.
        MenuAction::ResetViewRotation => match editor.active_mut() {
            None => Err("No document is open".to_string()),
            Some(doc) if !doc.camera.is_rotated() => Err("The view is already upright".to_string()),
            Some(doc) => {
                doc.camera.reset_rotation();
                Ok("View rotation reset".to_string())
            }
        },

        // ---- Help ----------------------------------------------------------
        MenuAction::Help => open_help_url(
            editor,
            "https://github.com/RealDealCPA-VR/Raster-studio/wiki",
            "Help lives at",
        ),
        MenuAction::ExportDiagnostics => editor.export_diagnostics(),
        MenuAction::ReleaseNotes => open_help_url(
            editor,
            "https://github.com/RealDealCPA-VR/Raster-studio/releases",
            "Release notes live at",
        ),
        MenuAction::ReportIssue => open_help_url(
            editor,
            "https://github.com/RealDealCPA-VR/Raster-studio/issues/new",
            "File issues at",
        ),
        // The version is the executable's stamp (package version + short
        // commit, set by `studio-desktop` before launch), not this library's
        // own `CARGO_PKG_VERSION` — see `crate::version`.
        MenuAction::About => Ok(format!(
            "{} — a layered raster editor",
            crate::version::about_line()
        )),

        // Anything else must have been refused during enablement. Reaching here
        // means `unavailable_reason`, the dialog host and this match disagree,
        // which `every_enabled_menu_item_really_does_something` is there to
        // catch: it drives every enabled row through the menu bar's click
        // handler and fails on one that lands here.
        other => Err(format!(
            "{}: this build has no implementation for it",
            other.label()
        )),
    };

    match &outcome {
        Ok(message) => editor.set_status(message.clone()),
        Err(reason) => editor.set_status(reason.clone()),
    }
    outcome
}

fn canvas_rect(w: u32, h: u32) -> selection::Rect {
    selection::Rect::from_xywh(0, 0, w, h)
}

/// Why a dialog-hosted row's dialog did not open: the thing it had nothing
/// to open over, named.
///
/// The menu's own gate (`ui::menu`) and `DialogHost::open_for_menu_action`
/// agree in every state a user can reach, so this is the second line of
/// defence — but it has to say something specific, because the status line is
/// the only place a click that opened no dialog is visible at all.
fn dialog_refused(action: MenuAction, editor: &Editor) -> String {
    let Some(doc) = editor.active() else {
        return "No document is open".to_string();
    };
    let layer = doc.document.active_layer();
    let reason = match action {
        MenuAction::LayerStyle(_) | MenuAction::BlendingOptions if layer.is_none() => {
            "Select a layer first"
        }
        MenuAction::RenameLayer => match layer {
            None => "Select a layer first",
            Some(id) => match doc.document.layers.get(id) {
                Some(l) if l.locked.all => "The layer is locked",
                _ => "",
            },
        },
        MenuAction::RefineEdge if doc.document.selection.bounds().is_none() => {
            "There is no selection"
        }
        MenuAction::RefineMask | MenuAction::RemoveColorFringe => match layer {
            None => "Select a layer first",
            Some(id) => match doc.document.layers.get(id).and_then(|l| l.mask.as_ref()) {
                None => "The active layer has no mask",
                Some(_) => "",
            },
        },
        // W7-H: nothing to warp, or (Puppet Warp) no ink to pin.
        MenuAction::Liquify | MenuAction::PuppetWarp => {
            return match pixel_layer(editor) {
                Ok(_) if action == MenuAction::PuppetWarp => {
                    "The active layer has no ink to pin".to_string()
                }
                Ok(_) => format!("{}: its dialog could not open", action.label()),
                Err(reason) => reason,
            }
        }
        MenuAction::FilterGallery | MenuAction::BlurGallery(_) | MenuAction::VanishingPoint => {
            return match pixel_layer(editor) {
                Ok(_) => format!("{}: its dialog could not open", action.label()),
                Err(reason) => reason,
            }
        }
        _ => "",
    };
    if reason.is_empty() {
        format!("{}: its dialog could not open", action.label())
    } else {
        reason.to_string()
    }
}

/// A colour well's value as a stored 8-bit pixel.
///
/// No transfer function is applied, because the application does not put one
/// here: [`crate::editor::color_hex`] turns the same `[f32; 4]` into `#RRGGBB`
/// by multiplying by 255, so these components are already the display-referred
/// codes the tile store holds.
pub(crate) fn rgba8_of(rgba: [f32; 4]) -> [u8; 4] {
    let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    [c(rgba[0]), c(rgba[1]), c(rgba[2]), c(rgba[3])]
}

/// The active document's canvas size, or the reason there is none.
fn canvas_of(editor: &Editor) -> Result<(u32, u32), String> {
    let doc = editor.active().ok_or("No document is open")?;
    Ok((doc.document.width(), doc.document.height()))
}

/// The layer a pixel operation acts on: the active one, and it must own pixels.
fn pixel_layer(editor: &Editor) -> Result<LayerId, String> {
    let doc = editor.active().ok_or("No document is open")?;
    let id = doc.document.active_layer().ok_or("Select a layer first")?;
    let layer = doc
        .document
        .layers
        .get(id)
        .ok_or("The active layer is not in the document")?;
    match &layer.kind {
        layer_model::LayerKind::Raster(_) | layer_model::LayerKind::Generator(_) => Ok(id),
        other => Err(format!(
            "This works on a pixel layer; the active layer is a {}",
            editor_core::layer_class_name(other)
        )),
    }
}

/// Read the active pixel layer, run `op` over a linear premultiplied buffer,
/// mask the result by the selection and apply it as one undoable step.
pub(crate) fn edit_active_pixels(
    editor: &mut Editor,
    label: &str,
    op: impl FnOnce(&mut filters::FilterBuffer, &color::ColorSpace) -> Result<(), String>,
) -> Result<(), String> {
    let layer = pixel_layer(editor)?;
    let (w, h) = canvas_of(editor)?;
    if w == 0 || h == 0 {
        return Err("The canvas has no pixels".to_string());
    }
    let (deep, selection, space) = {
        let doc = editor.active().ok_or("No document is open")?;
        (
            doc.is_sixteen_bit(),
            doc.document.selection.clone(),
            doc.document.meta.color_space.clone(),
        )
    };
    let resized = |buffer: &filters::FilterBuffer| {
        format!(
            "{label} changed the image from {w}x{h} to {:?}, and this build \
             cannot resize a layer",
            buffer.dimensions()
        )
    };
    // W10-H: a 32-bit document's layer is read, filtered and written as f32.
    if editor
        .active()
        .is_some_and(|d| d.document.meta.bit_depth == 32)
    {
        return crate::depth32::edit_active_pixels_f32(editor, layer, (w, h), label, op);
    }
    // W7-C: a 16-bit document's layer is read, filtered and written at 16
    // bits (the buffer itself is f32); only an 8-bit one takes the RGBA8 road.
    let command = if deep {
        let before = editor
            .active()
            .ok_or("No document is open")?
            .layer_rgba16(layer);
        let mut buffer =
            filters::FilterBuffer::from_rgba16(w, h, &before).map_err(|e| e.to_string())?;
        op(&mut buffer, &space)?;
        if buffer.dimensions() != (w, h) {
            return Err(resized(&buffer));
        }
        let mut after = buffer.to_rgba16();
        pixels::mask_by_selection(&before, &mut after, &selection, w, h);
        if after == before {
            return Err(format!("{label} changed nothing"));
        }
        let doc = editor.active_mut().ok_or("No document is open")?;
        // W10-G: Edit > Fade can fade this step.
        crate::fade::remember(doc.id(), layer, label, &before, &after);
        doc.layer_rgba16_command(layer, &after, label)?
    } else {
        let before = pixels::read_layer(editor.active().ok_or("No document is open")?, layer);
        let mut buffer =
            filters::FilterBuffer::from_rgba8(w, h, &before).map_err(|e| e.to_string())?;
        op(&mut buffer, &space)?;
        if buffer.dimensions() != (w, h) {
            return Err(resized(&buffer));
        }
        let mut after = buffer.to_rgba8();
        pixels::mask_by_selection(&before, &mut after, &selection, w, h);
        if after == before {
            return Err(format!("{label} changed nothing"));
        }
        let doc = editor.active_mut().ok_or("No document is open")?;
        // W10-G: Edit > Fade can fade this step.
        crate::fade::remember(doc.id(), layer, label, &before, &after);
        pixels::write_layer(doc, layer, &after, label)?
    };
    editor.apply_command(command);
    Ok(())
}

fn run_filter(editor: &mut Editor, id: ui::menu::FilterId) -> Result<String, String> {
    // W7-E: no dialog opened for this run, so no smart-filter re-edit is
    // armed for it — it appends like any fresh filter.
    SMART_FILTER_EDIT.with(|armed| armed.set(None));
    let spec = ui::dialogs::filter_by_id(id)
        .ok_or("ui::dialogs has no parameter schema for this filter")?;
    let params = ui::dialogs::FilterParams::defaults(spec.params);
    let invocation = ui::dialogs::FilterInvocation {
        filter: spec,
        params,
    };
    run_filter_invocation(editor, &invocation)
}

/// The active layer's pixels as a [`filters::FilterBuffer`] — the source the
/// Filter dialog previews against. Read-only: opening the dialog never
/// touches the document.
pub(crate) fn filter_source(editor: &Editor) -> Option<filters::FilterBuffer> {
    let doc = editor.active()?;
    let layer = doc.document.active_layer()?;
    let (w, h) = (doc.document.width(), doc.document.height());
    if w == 0 || h == 0 {
        return None;
    }
    let before = pixels::read_layer(doc, layer);
    let buffer = filters::FilterBuffer::from_rgba8(w, h, &before).ok()?;
    // W7-E: a smart object's filter applies to what its stack produced below
    // it — the whole stack for a new filter, the entries under it for a
    // re-edit — so that is what the dialog previews over.
    Some(smart_filter_input(editor, layer, buffer))
}

/// W7-H: the pixels Liquify and Puppet Warp open over — the active layer's,
/// and only when it is a pixel layer (the menu gates the same way; this is
/// the second line of defence).
pub(crate) fn warp_source(editor: &Editor) -> Option<filters::FilterBuffer> {
    pixel_layer(editor).ok()?;
    filter_source(editor)
}

/// W7-H: apply the Liquify dialog's confirmed warp to the active layer at
/// full resolution, folded by the selection, as one undoable step.
pub(crate) fn liquify_with(
    editor: &mut Editor,
    spec: &ui::dialogs::LiquifySpec,
) -> Result<String, String> {
    let size = canvas_of(editor)?;
    if spec.field.image_size() != size {
        return Err("The document changed size since Liquify opened; open it again".to_string());
    }
    if spec.is_identity() {
        return Err("Liquify moved nothing: paint a stroke on its canvas first".to_string());
    }
    edit_active_pixels(editor, "Liquify", |buffer, _| {
        *buffer = spec.apply(buffer);
        Ok(())
    })?;
    Ok("Liquify applied".to_string())
}

/// W9-O: apply a Blur Gallery dialog's confirmed blur to the active layer at
/// full resolution, folded by the selection, as one undoable step.
pub(crate) fn blur_gallery_with(
    editor: &mut Editor,
    spec: &ui::dialogs::BlurGallerySpec,
) -> Result<String, String> {
    let name = action_name(MenuAction::BlurGallery(spec.kind()));
    let size = canvas_of(editor)?;
    if spec.image_size != size {
        return Err(format!(
            "The document changed size since {name} opened; open it again"
        ));
    }
    if spec.is_identity() {
        return Err(format!("{name} would change nothing: raise its blur first"));
    }
    edit_active_pixels(editor, &name, |buffer, _| {
        *buffer = spec.apply(buffer);
        Ok(())
    })?;
    Ok(format!("{name} applied"))
}

/// W10-D: apply the Filter Gallery's confirmed effect list to the active
/// layer at full resolution, folded by the selection, as one undoable step.
pub(crate) fn filter_gallery_with(
    editor: &mut Editor,
    spec: &ui::dialogs::FilterGallerySpec,
) -> Result<String, String> {
    let name = action_name(MenuAction::FilterGallery);
    let size = canvas_of(editor)?;
    if spec.image_size != size {
        return Err(format!(
            "The document changed size since {name} opened; open it again"
        ));
    }
    if spec.is_identity() {
        return Err(format!("{name} would change nothing: show an effect first"));
    }
    edit_active_pixels(editor, &name, |buffer, _| {
        *buffer = spec.apply(buffer);
        Ok(())
    })?;
    Ok(format!("{name} applied"))
}

/// W10-D: apply a Displace the external-map dialog confirmed to the active
/// layer at full resolution, folded by the selection, as one undoable step.
pub(crate) fn displace_map_with(
    editor: &mut Editor,
    spec: &ui::dialogs::DisplaceMapSpec,
) -> Result<String, String> {
    let name = action_name(MenuAction::Filter(ui::menu::FilterId::Displace));
    let size = canvas_of(editor)?;
    if spec.image_size != size {
        return Err(format!(
            "The document changed size since {name} opened; open it again"
        ));
    }
    if spec.is_identity() {
        return Err(format!("{name} would move nothing: raise a scale first"));
    }
    edit_active_pixels(editor, &name, |buffer, _| {
        *buffer = spec.apply(buffer);
        Ok(())
    })?;
    Ok(format!("{name} applied"))
}

/// W10-D: apply a confirmed Vanishing Point session to the active layer at
/// full resolution, folded by the selection, as one undoable step.
pub(crate) fn vanishing_point_with(
    editor: &mut Editor,
    spec: &ui::dialogs::VanishingPointSpec,
) -> Result<String, String> {
    let name = action_name(MenuAction::VanishingPoint);
    let size = canvas_of(editor)?;
    if spec.image_size != size {
        return Err(format!(
            "The document changed size since {name} opened; open it again"
        ));
    }
    if spec.is_identity() {
        return Err(format!("{name} would change nothing: clone or paste first"));
    }
    edit_active_pixels(editor, &name, |buffer, _| {
        *buffer = spec.apply(buffer);
        Ok(())
    })?;
    Ok(format!("{name} applied"))
}

/// A menu row's label without its trailing ellipsis.
fn action_name(action: MenuAction) -> String {
    action.label().trim_end_matches('…').to_string()
}

/// W7-H: apply the Puppet Warp dialog's confirmed deformation to the active
/// layer at full resolution, folded by the selection, as one undoable step.
pub(crate) fn puppet_warp_with(
    editor: &mut Editor,
    spec: &ui::dialogs::PuppetWarpSpec,
) -> Result<String, String> {
    let size = canvas_of(editor)?;
    if spec.mesh.image_size() != size {
        return Err(
            "The document changed size since Puppet Warp opened; open it again".to_string(),
        );
    }
    if spec.is_identity() {
        return Err("Puppet Warp moved nothing: drag a pin first".to_string());
    }
    edit_active_pixels(editor, "Puppet Warp", |buffer, _| {
        *buffer = spec.apply(buffer);
        Ok(())
    })?;
    Ok("Puppet Warp applied".to_string())
}

/// Run one filter invocation — the dialog's confirmed answer — against the
/// active layer's pixels as one undoable step.
pub(crate) fn run_filter_invocation(
    editor: &mut Editor,
    invocation: &ui::dialogs::FilterInvocation,
) -> Result<String, String> {
    let spec = invocation.filter;
    let label = spec.name();
    // W7-E: over a smart object the filter joins its smart-filter stack; the
    // source pixels are never rewritten. Only the ACTIVE layer is ever the
    // target: an armed re-edit replaces its entry only when it names this very
    // object (checked in `apply_smart_filter`), so a stale arm can never pull
    // a filter meant for another layer onto a smart object.
    if let Some(layer) = active_smart_object(editor) {
        return apply_smart_filter(editor, layer, invocation);
    }
    edit_active_pixels(editor, label, |buffer, _| {
        let filtered = invocation.run(buffer);
        *buffer = filtered;
        Ok(())
    })?;
    Ok(format!("{label} applied"))
}

// ---------------------------------------------------------------------------
// W7-E: smart filters
// ---------------------------------------------------------------------------

thread_local! {
    /// The smart-filter entry the open Filter dialog re-edits, armed by
    /// [`arm_smart_filter_edit`] when the dialog opened from a Layers-panel
    /// double-click; `None` for a dialog that adds a new filter.
    static SMART_FILTER_EDIT: std::cell::Cell<Option<(LayerId, usize, ui::menu::FilterId)>> =
        const { std::cell::Cell::new(None) };
}

/// Install the Filter dialogs' own table into the compositor as its
/// smart-filter runner, so every composite — canvas, export, thumbnail —
/// runs a stored smart filter exactly as the dialog that made it did.
/// Idempotent; called from every entry point that can composite a document.
pub(crate) fn install_smart_filter_runner() {
    compositor::smart::install_runner(run_smart_filter);
}

/// The [`compositor::smart::SmartFilterRunner`] the application installs: the
/// stored key names a [`ui::menu::FilterId`] (its variant name), the stored
/// parameters are written over the schema's defaults (clamped by the schema,
/// as the dialog's own are), and the filter's `apply` does the rest.
pub(crate) fn run_smart_filter(
    filter: &layer_model::SmartFilter,
    src: &filters::FilterBuffer,
) -> Option<filters::FilterBuffer> {
    let id = filter_id_of_key(&filter.filter)?;
    let spec = ui::dialogs::filter_by_id(id)?;
    let mut params = ui::dialogs::FilterParams::defaults(spec.params);
    for (key, value) in &filter.params {
        params.set(key, dialog_param(*value));
    }
    Some((spec.apply)(src, &params))
}

/// The stable key a smart filter stores for `id`.
fn filter_key(id: ui::menu::FilterId) -> String {
    format!("{id:?}")
}

fn filter_id_of_key(key: &str) -> Option<ui::menu::FilterId> {
    ui::menu::FilterId::ALL
        .iter()
        .copied()
        .find(|id| filter_key(*id) == key)
}

fn dialog_param(value: layer_model::SmartParam) -> ui::dialogs::ParamValue {
    use layer_model::SmartParam as S;
    use ui::dialogs::ParamValue as P;
    match value {
        S::Float(v) => P::Float(v),
        S::Int(v) => P::Int(v),
        S::Bool(v) => P::Bool(v),
        S::Choice(v) => P::Choice(v as usize),
        S::Color(c) => P::Color(c),
    }
}

fn smart_param(value: ui::dialogs::ParamValue) -> layer_model::SmartParam {
    use layer_model::SmartParam as S;
    use ui::dialogs::ParamValue as P;
    match value {
        P::Float(v) => S::Float(v),
        P::Int(v) => S::Int(v),
        P::Bool(v) => S::Bool(v),
        P::Choice(v) => S::Choice(u32::try_from(v).unwrap_or(u32::MAX)),
        P::Color(c) => S::Color(c),
    }
}

/// The smart filter a confirmed dialog describes: its filter's key and every
/// parameter the schema names.
fn smart_filter_of(invocation: &ui::dialogs::FilterInvocation) -> layer_model::SmartFilter {
    let params = invocation
        .filter
        .params
        .iter()
        .filter_map(|o| {
            invocation
                .params
                .get(o.key)
                .map(|v| (o.key.to_string(), smart_param(v)))
        })
        .collect();
    layer_model::SmartFilter::new(filter_key(invocation.filter.id), params)
}

/// Forget any armed smart-filter re-edit. The dialog host calls this when a
/// dialog is cancelled or closed and before any dialog opens, so the arm
/// lives exactly as long as the re-edit dialog it was made for.
pub(crate) fn disarm_smart_filter_edit() {
    SMART_FILTER_EDIT.with(|armed| armed.set(None));
}

/// The smart object an armed re-edit of filter `id` targets, while it is
/// still a smart object in the active document.
fn armed_smart_filter_layer(editor: &Editor, id: ui::menu::FilterId) -> Option<LayerId> {
    let (layer, _, armed) = SMART_FILTER_EDIT.with(|a| a.get())?;
    (armed == id && smart_filters_of(editor, layer).is_some()).then_some(layer)
}

/// The pixels a Filter dialog for `id` previews over: the re-edited smart
/// object's (its source run through the entries below the edited one) when
/// [`arm_smart_filter_edit`] armed a re-edit, else [`filter_source`]'s. The
/// armed object need not be the active layer: the Layers panel selects it in
/// the same frame the dialog is routed, against the pre-selection editor.
pub(crate) fn filter_dialog_source(
    editor: &Editor,
    id: ui::menu::FilterId,
) -> Option<filters::FilterBuffer> {
    let Some(layer) = armed_smart_filter_layer(editor, id) else {
        return filter_source(editor);
    };
    let doc = editor.active()?;
    let (w, h) = (doc.document.width(), doc.document.height());
    if w == 0 || h == 0 {
        return None;
    }
    let before = pixels::read_layer(doc, layer);
    let buffer = filters::FilterBuffer::from_rgba8(w, h, &before).ok()?;
    Some(smart_filter_input(editor, layer, buffer))
}

/// The active layer, when it is a smart object.
fn active_smart_object(editor: &Editor) -> Option<LayerId> {
    let doc = editor.active()?;
    let id = doc.document.active_layer()?;
    matches!(
        doc.document.layers.get(id)?.kind,
        layer_model::LayerKind::SmartObject(_)
    )
    .then_some(id)
}

/// `layer`'s smart-filter stack, when it is a smart object.
fn smart_filters_of(editor: &Editor, layer: LayerId) -> Option<Vec<layer_model::SmartFilter>> {
    match &editor.active()?.document.layers.get(layer)?.kind {
        layer_model::LayerKind::SmartObject(so) => Some(so.filters.clone()),
        _ => None,
    }
}

/// Replace `layer`'s smart-filter stack as ONE undoable step: a
/// [`Command::SetLayerKind`] carrying the same smart object with the new
/// stack. The source tiles are not touched.
pub(crate) fn set_smart_filters(
    editor: &mut Editor,
    layer: LayerId,
    filters: Vec<layer_model::SmartFilter>,
) -> Result<(), String> {
    install_smart_filter_runner();
    let doc = editor.active().ok_or("No document is open")?;
    let current = doc
        .document
        .layers
        .get(layer)
        .ok_or("The layer is not in the document")?;
    if current.locked.all {
        return Err("The layer is locked".to_string());
    }
    let layer_model::LayerKind::SmartObject(so) = &current.kind else {
        return Err("Smart filters live on smart objects; this layer is not one".to_string());
    };
    let mut so = so.clone();
    so.filters = filters;
    let wanted = so.filters.clone();
    editor.apply_command(Command::SetLayerKind {
        layer_id: layer,
        kind: Box::new(layer_model::LayerKind::SmartObject(so)),
    });
    if smart_filters_of(editor, layer).as_ref() == Some(&wanted) {
        Ok(())
    } else {
        Err("The smart-filter change was refused".to_string())
    }
}

/// A confirmed Filter dialog over a smart object: append the filter to its
/// stack, or — when the dialog was opened to re-edit an entry — replace that
/// entry's parameters, keeping its eye, opacity and blend mode.
fn apply_smart_filter(
    editor: &mut Editor,
    layer: LayerId,
    invocation: &ui::dialogs::FilterInvocation,
) -> Result<String, String> {
    let label = invocation.filter.name();
    let mut stack = smart_filters_of(editor, layer).unwrap_or_default();
    let fresh = smart_filter_of(invocation);
    let armed = SMART_FILTER_EDIT.with(|armed| armed.take());
    let message = match armed {
        Some((l, index, id))
            if l == layer
                && id == invocation.filter.id
                && stack.get(index).is_some_and(|f| f.filter == fresh.filter) =>
        {
            stack[index].params = fresh.params;
            format!("{label} smart filter updated")
        }
        _ => {
            stack.push(fresh);
            format!("{label} added as a smart filter")
        }
    };
    set_smart_filters(editor, layer, stack)?;
    Ok(message)
}

/// Called as a Filter dialog for `id` opens: when the Layers panel asked to
/// re-edit that very filter of a smart object (active or not), arm the re-edit and
/// answer the stored parameters for the dialog to start from. Every other
/// opening disarms, so a plain Filter-menu dialog always appends.
pub(crate) fn arm_smart_filter_edit(
    editor: &Editor,
    id: ui::menu::FilterId,
) -> Vec<(String, ui::dialogs::ParamValue)> {
    let request = compositor::smart::take_edit_request();
    let armed = request.and_then(|r| {
        let stack = smart_filters_of(editor, r.layer)?;
        let entry = stack.get(r.index)?;
        // The object need not be active yet: the panel's double-click selects
        // it in the same frame this dialog is routed.
        (entry.filter == filter_key(id)).then(|| (r, entry.params.clone()))
    });
    match armed {
        Some((r, params)) => {
            SMART_FILTER_EDIT.with(|a| a.set(Some((r.layer, r.index, id))));
            params
                .into_iter()
                .map(|(k, v)| (k, dialog_param(v)))
                .collect()
        }
        None => {
            SMART_FILTER_EDIT.with(|a| a.set(None));
            Vec::new()
        }
    }
}

/// What a new (or re-edited) smart filter on the active layer applies to:
/// `buffer` — the source — run through the stack entries below it. A layer
/// that is not a smart object, or has no filters, answers `buffer` itself.
fn smart_filter_input(
    editor: &Editor,
    layer: LayerId,
    buffer: filters::FilterBuffer,
) -> filters::FilterBuffer {
    install_smart_filter_runner();
    let Some(stack) = smart_filters_of(editor, layer) else {
        return buffer;
    };
    let below = match SMART_FILTER_EDIT.with(|a| a.get()) {
        Some((l, index, _)) if l == layer => index.min(stack.len()),
        _ => stack.len(),
    };
    let (w, h) = buffer.dimensions();
    let (Some(runner), false) = (compositor::smart::runner(), stack[..below].is_empty()) else {
        return buffer;
    };
    let rect = raster::PixelRect::new(0, 0, w, h);
    let Ok(canvas) = compositor::Canvas::from_pixels(rect, buffer.pixels().to_vec()) else {
        return buffer;
    };
    let out = compositor::smart::apply_stack(&canvas, &stack[..below], runner);
    filters::FilterBuffer::from_pixels(w, h, out.pixels().to_vec()).unwrap_or(buffer)
}

/// Filter ▸ Convert for Smart Filters: the active layer becomes a smart
/// object (the same conversion as Layer ▸ Smart Object ▸ Convert to Smart
/// Object), and every filter applied to it from then on joins its
/// smart-filter stack.
fn convert_for_smart_filters(editor: &mut Editor) -> Result<String, String> {
    install_smart_filter_runner();
    if active_smart_object(editor).is_some() {
        return Err(ui::menu::SMART_FILTERS_ALREADY.to_string());
    }
    let (parent, index) = {
        let doc = editor.active().ok_or("No document is open")?;
        let layers = &doc.document.layers;
        let source = doc.document.active_layer().ok_or("Select a layer first")?;
        (
            layers.parent_of(source),
            layers
                .index_in_parent(source)
                .ok_or("The layer is not in the tree")?,
        )
    };
    editor.convert_to_smart_object()?;
    // The conversion puts the new smart object where the layer stood and
    // removes the layer; make the object active, so the next filter lands in
    // its stack rather than finding no layer.
    let converted = editor.active().and_then(|doc| {
        let layers = &doc.document.layers;
        let siblings = match parent {
            Some(p) => layers.get(p)?.children().to_vec(),
            None => layers.root().to_vec(),
        };
        siblings.get(index).copied()
    });
    if let Some(id) = converted {
        editor.set_active_layer(id);
    }
    Ok("Converted for Smart Filters: filters applied to this layer stay editable".to_string())
}

fn run_adjustment(
    editor: &mut Editor,
    adjustment: &adjustments::Adjustment,
    label: &str,
) -> Result<String, String> {
    let prepared = adjustments::PreparedAdjustment::new(adjustment);
    if prepared.is_identity() {
        return Err(format!(
            "{label} is at its identity setting, so applying it would change \
             nothing; open it from Image > Adjustments and move a control in \
             its dialog first"
        ));
    }
    edit_active_pixels(editor, label, |buffer, space| {
        prepared.apply_premultiplied_rgba(buffer.pixels_mut(), space);
        Ok(())
    })?;
    Ok(format!("{label} applied"))
}

/// Image ▸ Adjustments for any adjustment (W4-E): the two that are not a
/// plain per-pixel function take their own road here — Equalize reads the
/// histogram of the pixels it is about to change, and Shadows/Highlights reads
/// each pixel's neighbourhood over its radius — and every other one is
/// [`run_adjustment`]. Each lands as one undoable step.
pub(crate) fn run_adjustment_kind(
    editor: &mut Editor,
    adjustment: &adjustments::Adjustment,
    label: &str,
) -> Result<String, String> {
    match adjustment {
        adjustments::Adjustment::Equalize => run_equalize(editor, label),
        adjustments::Adjustment::ShadowsHighlights(sh) => {
            if sh.is_identity() {
                return Err(format!(
                    "{label} is at its identity setting, so applying it would change \
                     nothing; move a control in its dialog first"
                ));
            }
            let sh = *sh;
            edit_active_pixels(editor, label, |buffer, space| {
                let (w, h) = buffer.dimensions();
                sh.apply_premultiplied_rgba_spatial(
                    buffer.pixels_mut(),
                    w as usize,
                    h as usize,
                    space,
                )
                .map_err(|e| e.to_string())
            })?;
            Ok(format!("{label} applied"))
        }
        // W7-G: HDR Toning reads each pixel's neighbourhood over its Edge
        // Glow radius (the base/detail split in `adjustments::hdr`).
        adjustments::Adjustment::HdrToning(hdr) => {
            if hdr.is_identity() {
                return Err(format!(
                    "{label} is at its identity setting, so applying it would change \
                     nothing; move a control in its dialog first"
                ));
            }
            let hdr = *hdr;
            edit_active_pixels(editor, label, |buffer, space| {
                let (w, h) = buffer.dimensions();
                hdr.apply_premultiplied_rgba_spatial(
                    buffer.pixels_mut(),
                    w as usize,
                    h as usize,
                    space,
                )
                .map_err(|e| e.to_string())
            })?;
            Ok(format!("{label} applied"))
        }
        adjustments::Adjustment::MatchColor(mc) => run_match_color(editor, *mc, label),
        other => run_adjustment(editor, other, label),
    }
}

/// W7-G: Match Color over the active layer. The target statistics are
/// measured here, from the full-resolution pixels the selection covers (all
/// of them with no selection), replacing the dialog's proxy estimate — so the
/// transfer lands the layer's own means on the source's, as Photopea's
/// "use selection in target" does.
fn run_match_color(
    editor: &mut Editor,
    mc: adjustments::MatchColor,
    label: &str,
) -> Result<String, String> {
    if mc.is_identity() {
        return Err(format!(
            "{label} is at its identity setting, so applying it would change \
             nothing; pick a source or move a control in its dialog first"
        ));
    }
    let selection = editor
        .active()
        .map(|doc| doc.document.selection.clone())
        .ok_or("No document is open")?;
    edit_active_pixels(editor, label, |buffer, space| {
        let (w, _) = buffer.dimensions();
        let covered = |i: usize| {
            let at = glam::IVec2::new((i as u32 % w) as i32, (i as u32 / w) as i32);
            selection.coverage_at(at)
        };
        let coverage: Option<&dyn Fn(usize) -> f32> =
            (!selection.is_none()).then_some(&covered as &dyn Fn(usize) -> f32);
        let target = adjustments::LabStats::measure(buffer.pixels(), coverage)
            .ok_or_else(|| format!("{label} found no pixels to match"))?;
        let prepared = adjustments::PreparedAdjustment::new(&adjustments::Adjustment::MatchColor(
            mc.with_target(target),
        ));
        prepared.apply_premultiplied_rgba(buffer.pixels_mut(), space);
        Ok(())
    })?;
    Ok(format!("{label} applied"))
}

/// Equalize over the active layer: the histogram is taken from the pixels
/// the selection covers (all of them with no selection), so equalising a
/// selection spreads *its* tones, as Photopea's does.
fn run_equalize(editor: &mut Editor, label: &str) -> Result<String, String> {
    let selection = editor
        .active()
        .map(|doc| doc.document.selection.clone())
        .ok_or("No document is open")?;
    edit_active_pixels(editor, label, |buffer, space| {
        let (w, _) = buffer.dimensions();
        let mut stats = adjustments::ImageStats::new();
        for (i, px) in buffer.pixels().iter().enumerate() {
            if px[3] <= color::UNPREMULTIPLY_ALPHA_EPSILON {
                continue;
            }
            if !selection.is_none() {
                let at = glam::IVec2::new((i as u32 % w) as i32, (i as u32 / w) as i32);
                if selection.coverage_at(at) <= 0.0 {
                    continue;
                }
            }
            let s = color::unpremultiply(*px);
            stats.add(adjustments::EncodedRgb::new(color::from_linear(
                space,
                [s[0], s[1], s[2]],
            )));
        }
        let prepared =
            adjustments::PreparedAdjustment::with_stats(&adjustments::Adjustment::Equalize, &stats);
        if prepared.is_identity() {
            return Err(format!("{label} found nothing to equalize"));
        }
        prepared.apply_premultiplied_rgba(buffer.pixels_mut(), space);
        Ok(())
    })?;
    Ok(format!("{label} applied"))
}

fn run_auto(
    editor: &mut Editor,
    kind: adjustments::AutoKind,
    label: &str,
) -> Result<String, String> {
    let mode =
        adjustments::AutoMode::new(kind, adjustments::DEFAULT_CLIP).map_err(|e| e.to_string())?;
    edit_active_pixels(editor, label, |buffer, space| {
        let stats = adjustments::ImageStats::from_premultiplied_rgba(buffer.pixels(), space);
        let prepared = adjustments::PreparedAdjustment::with_stats(
            &adjustments::Adjustment::Auto(mode),
            &stats,
        );
        if prepared.is_identity() {
            return Err(format!("{label} found nothing to correct"));
        }
        prepared.apply_premultiplied_rgba(buffer.pixels_mut(), space);
        Ok(())
    })?;
    Ok(format!("{label} applied"))
}

/// Rewrite one layer's pixels through a coordinate map — the flips and the
/// fixed rotations.
fn remap_active_layer(
    editor: &mut Editor,
    label: &str,
    map: impl Fn(i64, i64, i64, i64) -> (i64, i64),
) -> Result<String, String> {
    let layer = pixel_layer(editor)?;
    let (w, h) = canvas_of(editor)?;
    let command = {
        let doc = editor.active_mut().ok_or("No document is open")?;
        // W10-H: a 32-bit layer moves its f32 samples, HDR included.
        if doc.document.meta.bit_depth == 32 {
            let before = crate::depth32::layer_rgbaf32(doc, layer);
            let after = remap(&before, w, h, &map);
            if after == before {
                return Err(format!("{label} changed nothing"));
            }
            crate::depth32::layer_rgbaf32_command(doc, layer, &after, label)?
        } else if doc.is_sixteen_bit() {
            let before = doc.layer_rgba16(layer);
            let after = remap(&before, w, h, &map);
            if after == before {
                return Err(format!("{label} changed nothing"));
            }
            crate::fade::remember(doc.id(), layer, label, &before, &after);
            doc.layer_rgba16_command(layer, &after, label)?
        } else {
            let before = pixels::read_layer(doc, layer);
            let after = remap(&before, w, h, &map);
            if after == before {
                return Err(format!("{label} changed nothing"));
            }
            // W10-G: Edit > Fade can fade this step.
            crate::fade::remember(doc.id(), layer, label, &before, &after);
            pixels::write_layer(doc, layer, &after, label)?
        }
    };
    editor.apply_command(command);
    Ok(format!("{label} applied"))
}

/// The same map over every pixel layer in the document, as one undoable step.
fn remap_all_layers(
    editor: &mut Editor,
    label: &str,
    map: impl Fn(i64, i64, i64, i64) -> (i64, i64),
) -> Result<String, String> {
    let (w, h) = canvas_of(editor)?;
    let command = {
        let doc = editor.active_mut().ok_or("No document is open")?;
        let ids: Vec<LayerId> = doc
            .document
            .layers
            .iter_depth_first()
            .into_iter()
            .filter(|id| doc.document.layer_tiles(*id).is_some())
            .collect();
        if ids.is_empty() {
            return Err(format!("{label}: no layer in this document has pixels"));
        }
        let mut commands = Vec::new();
        for id in ids {
            // W10-H: a 32-bit layer moves its f32 samples, HDR included.
            if doc.document.meta.bit_depth == 32 {
                let before = crate::depth32::layer_rgbaf32(doc, id);
                let after = remap(&before, w, h, &map);
                if after != before {
                    commands.push(crate::depth32::layer_rgbaf32_command(
                        doc, id, &after, label,
                    )?);
                }
                continue;
            }
            // W7-C: at the document's own depth, as `remap_active_layer`.
            if doc.is_sixteen_bit() {
                let before = doc.layer_rgba16(id);
                let after = remap(&before, w, h, &map);
                if after != before {
                    commands.push(doc.layer_rgba16_command(id, &after, label)?);
                }
                continue;
            }
            let before = pixels::read_layer(doc, id);
            let after = remap(&before, w, h, &map);
            if after == before {
                continue;
            }
            commands.push(pixels::write_layer(doc, id, &after, label)?);
        }
        if commands.is_empty() {
            return Err(format!("{label} changed nothing"));
        }
        Command::Transaction {
            label: label.to_string(),
            commands,
        }
    };
    editor.apply_command(command);
    Ok(format!("{label} applied"))
}

/// `dst[map(x, y)] = src[x, y]`, with anything landing off the canvas dropped.
fn remap<S: raster::depth::DepthSample>(
    src: &[S],
    w: u32,
    h: u32,
    map: &impl Fn(i64, i64, i64, i64) -> (i64, i64),
) -> Vec<S> {
    let mut out = vec![S::default(); src.len()];
    let (wi, hi) = (w as i64, h as i64);
    for y in 0..hi {
        for x in 0..wi {
            let (nx, ny) = map(x, y, wi, hi);
            if nx < 0 || ny < 0 || nx >= wi || ny >= hi {
                continue;
            }
            let s = ((y * wi + x) * 4) as usize;
            let d = ((ny * wi + nx) * 4) as usize;
            out[d..d + 4].copy_from_slice(&src[s..s + 4]);
        }
    }
    out
}

fn clear_selection(editor: &mut Editor) -> Result<String, String> {
    let layer = pixel_layer(editor)?;
    let (w, h) = canvas_of(editor)?;
    let command = {
        let doc = editor.active_mut().ok_or("No document is open")?;
        let selection = doc.document.selection.clone();
        // W10-H: a partly selected 32-bit pixel keeps its f32 remainder.
        if doc.document.meta.bit_depth == 32 {
            let before = crate::depth32::layer_rgbaf32(doc, layer);
            let mut after = vec![0f32; before.len()];
            pixels::mask_by_selection(&before, &mut after, &selection, w, h);
            if after == before {
                return Err("There is nothing to clear here".to_string());
            }
            crate::depth32::layer_rgbaf32_command(doc, layer, &after, "Clear")?
        } else if doc.is_sixteen_bit() {
            let before = doc.layer_rgba16(layer);
            let mut after = vec![0u16; before.len()];
            pixels::mask_by_selection(&before, &mut after, &selection, w, h);
            if after == before {
                return Err("There is nothing to clear here".to_string());
            }
            doc.layer_rgba16_command(layer, &after, "Clear")?
        } else {
            let before = pixels::read_layer(doc, layer);
            let mut after = vec![0u8; before.len()];
            pixels::mask_by_selection(&before, &mut after, &selection, w, h);
            if after == before {
                return Err("There is nothing to clear here".to_string());
            }
            pixels::write_layer(doc, layer, &after, "Clear")?
        }
    };
    editor.apply_command(command);
    Ok("Cleared".to_string())
}

fn fill_selection(editor: &mut Editor) -> Result<String, String> {
    fill_selection_with(
        editor,
        &ui::dialogs::FillSpec {
            contents: ui::dialogs::FillContents::Foreground,
            ..Default::default()
        },
    )
}

/// Fill the selection with the Fill dialog's confirmed contents.
///
/// The paint is source-over with the chosen [`layer_model::BlendMode`] at the
/// chosen opacity, in sRGB space like the rest of this build's per-pixel
/// edits. "Preserve transparency" scales the paint's own alpha by the
/// destination's coverage, so a fill can never paint where the layer was
/// empty.
pub(crate) fn fill_selection_with(
    editor: &mut Editor,
    spec: &ui::dialogs::FillSpec,
) -> Result<String, String> {
    let rgba = match &spec.contents {
        ui::dialogs::FillContents::Foreground => editor.foreground(),
        ui::dialogs::FillContents::Background => editor.background(),
        ui::dialogs::FillContents::Color(c) => *c,
        ui::dialogs::FillContents::Pattern(name) => {
            let preset = editor
                .presets()
                .pattern(name)
                .ok_or_else(|| format!("No pattern named “{name}” is defined yet"))?
                .clone();
            // Tile the pattern across the canvas; the selection mask still
            // scopes the paint below.
            return fill_selection_painting(editor, spec, &move |x, y| {
                let [r, g, b, a] = preset.pixel(x, y);
                [
                    f32::from(r) / 255.0,
                    f32::from(g) / 255.0,
                    f32::from(b) / 255.0,
                    f32::from(a) / 255.0,
                ]
            });
        }
        ui::dialogs::FillContents::Gray50 => [0.5, 0.5, 0.5, 1.0],
        // W7-I: synthesised from the rest of the layer, not a colour.
        ui::dialogs::FillContents::ContentAware => {
            return content_aware_fill_selection(editor, spec);
        }
    };
    let hex = crate::editor::color_hex(rgba);
    // The wells and the dialog's Colour payload are normalized floats.
    let src = [rgba[0], rgba[1], rgba[2]];
    let src_a = rgba[3].clamp(0.0, 1.0) * spec.opacity.clamp(0.0, 1.0);
    fill_selection_painting(editor, spec, &|_, _| [src[0], src[1], src[2], src_a])?;
    Ok(format!(
        "Filled with {hex} at {}% opacity, {} mode",
        (spec.opacity * 100.0).round() as u32,
        spec.blend.label()
    ))
}

/// W7-I: Edit ▸ Fill ▸ Contents: Content-Aware. The selection's pixels (every
/// pixel it covers at all) are synthesised from the rest of the active layer
/// by PatchMatch ([`filters::content_aware_fill`], bounded to the selection's
/// bounding box plus a context margin, fixed seed) ON A WORKER
/// ([`content_aware_job`]), then painted through the same
/// [`fill_selection_painting`] the colour fills use — so the dialog's blend
/// mode, opacity and Preserve Transparency apply, the selection's soft edge
/// feathers the result, and the whole fill is ONE undo step.
fn content_aware_fill_selection(
    editor: &mut Editor,
    spec: &ui::dialogs::FillSpec,
) -> Result<String, String> {
    content_aware_job::start(
        editor,
        content_aware_job::Kind::Fill(Box::new(spec.clone())),
    )
}

/// W7-I: Edit ▸ Content-Aware Scale ▸ one step. The active layer is seam
/// carved ([`filters::content_aware_scale`], gradient-magnitude energy, so
/// high-contrast content is carved last) ON A WORKER ([`content_aware_job`])
/// to the step's fraction of the canvas along one axis and placed centred on
/// the canvas; what a widening pushes past the canvas edge is cropped, and a
/// narrowing leaves transparent bands. With a selection active only the
/// selected part of the result lands (the same selection fold every filter
/// takes). No protect-skin option.
fn content_aware_scale_layer(
    editor: &mut Editor,
    step: ui::menu::ContentAwareScaleStep,
) -> Result<String, String> {
    content_aware_job::start(editor, content_aware_job::Kind::Scale(step))
}

/// The shared fill painter: `source` answers the paint colour (normalized
/// RGBA) for each canvas pixel, so the solid colours and the tiled pattern go
/// through the same source-over-with-blend loop and the same selection mask.
/// Card 058: fill the MASK coverage when the edit target is the mask.
///
/// The paint's value is the source colour's LUMINANCE (the same mapping a
/// brush stroke uses — white reveals, black conceals), applied at the
/// dialog's opacity inside the selection. Non-Normal blend modes are
/// refused: they are defined over RGBA compositing, not a scalar field, and
/// pretending otherwise would invent semantics. `preserve_transparency` is
/// ignored — a mask has no alpha to preserve — and the layer's pixel
/// tiles are never touched.
fn fill_mask_coverage(
    editor: &mut Editor,
    layer: layer_model::LayerId,
    spec: &ui::dialogs::FillSpec,
    source: &dyn Fn(i64, i64) -> [f32; 4],
    w: u32,
    h: u32,
) -> Result<String, String> {
    if spec.blend != layer_model::BlendMode::Normal {
        return Err(
            "Blend modes other than Normal are not meaningful on a coverage mask".to_string(),
        );
    }
    // The dialog's opacity: on the solid route `fill_selection_with` bakes
    // it into the source's alpha (the content fill does the same), on the
    // pattern route the pattern's own alpha passes through. Either way the
    // mask route must NOT multiply again — a dialog opacity of 50% would
    // otherwise apply 25%.
    let command = {
        let doc = editor.active_mut().ok_or("No document is open")?;
        let before = read_mask_coverage(doc, layer, w, h);
        let selection = doc.document.selection.clone();
        let mut after = before.clone();
        for py in 0..i64::from(h) {
            for px in 0..i64::from(w) {
                let paint = source(px, py);
                // The paint's VALUE is its luminance (the same mapping a
                // brush stroke uses) and its AMOUNT is the paint alpha —
                // coverage blends toward the VALUE by the AMOUNT, exactly
                // like `CoveragePatch::blend` on the brush path: black
                // hides, white reveals, grey lands in between.
                let value = tools::patch::mask_coverage_of(paint);
                let amount = paint[3].clamp(0.0, 1.0);
                let i = py as usize * w as usize + px as usize;
                let c = selection.coverage_at(glam::IVec2::new(px as i32, py as i32));
                let old = f32::from(before[i]) / 255.0;
                let blended = old * (1.0 - amount) + value * amount;
                after[i] = ((old * (1.0 - c) + blended * c) * 255.0)
                    .round()
                    .clamp(0.0, 255.0) as u8;
            }
        }
        pixels::write_mask_coverage(doc, layer, &after, "Fill Mask")?
    };
    editor.apply_command(command);
    Ok(format!(
        "Filled the mask with {} at {}% opacity",
        match &spec.contents {
            ui::dialogs::FillContents::Pattern(_) => "the pattern",
            _ => "the foreground colour",
        },
        (spec.opacity * 100.0).round() as i64
    ))
}

/// The Fill composite over a whole layer at its own depth (W7-C): `S` is
/// `u8` in an 8-bit document, with exactly the arithmetic the fill always
/// used, and `u16` in a 16-bit one.
fn fill_pixels<S: raster::depth::DepthSample>(
    before: &[S],
    selection: &editor_core::Selection,
    spec: &ui::dialogs::FillSpec,
    source: &dyn Fn(i64, i64) -> [f32; 4],
    w: u32,
    h: u32,
) -> Vec<S> {
    let mut after = before.to_vec();
    for py in 0..i64::from(h) {
        for px in 0..i64::from(w) {
            let i = (py as usize * w as usize + px as usize) * 4;
            let paint = source(px, py);
            let src = [paint[0], paint[1], paint[2]];
            let src_a = paint[3].clamp(0.0, 1.0);
            let dst_a = before[i + 3].to_unit();
            if spec.preserve_transparency && dst_a <= 0.0 {
                continue;
            }
            let paint_a = if spec.preserve_transparency {
                src_a * dst_a
            } else {
                src_a
            };
            let base = [
                before[i].to_unit(),
                before[i + 1].to_unit(),
                before[i + 2].to_unit(),
            ];
            let blended = spec.blend.blend_rgb(base, src);
            let out_a = paint_a + dst_a * (1.0 - paint_a);
            let out_rgb = if out_a <= 0.0 {
                [0.0; 3]
            } else {
                [
                    (blended[0] * paint_a + base[0] * dst_a * (1.0 - paint_a)) / out_a,
                    (blended[1] * paint_a + base[1] * dst_a * (1.0 - paint_a)) / out_a,
                    (blended[2] * paint_a + base[2] * dst_a * (1.0 - paint_a)) / out_a,
                ]
            };
            after[i] = S::from_unit(out_rgb[0]);
            after[i + 1] = S::from_unit(out_rgb[1]);
            after[i + 2] = S::from_unit(out_rgb[2]);
            after[i + 3] = S::from_unit(out_a);
        }
    }
    pixels::mask_by_selection(before, &mut after, selection, w, h);
    after
}

pub(crate) fn fill_selection_painting(
    editor: &mut Editor,
    spec: &ui::dialogs::FillSpec,
    source: &dyn Fn(i64, i64) -> [f32; 4],
) -> Result<String, String> {
    let layer = pixel_layer(editor)?;
    let (w, h) = canvas_of(editor)?;
    // Card 058: when the edit target is the layer's mask, a Fill paints
    // COVERAGE — one grayscale channel, never the four-channel ColorPatch
    // a content fill would be (that mismatch is the card's stated hazard).
    let mask_fill = editor.edit_target_is_mask()
        && editor
            .active()
            .and_then(|d| {
                d.document
                    .active_layer()
                    .and_then(|id| d.document.layers.get(id).and_then(|l| l.mask_id()))
            })
            .is_some();
    if mask_fill {
        return fill_mask_coverage(editor, layer, spec, source, w, h);
    }
    let command = {
        let doc = editor.active_mut().ok_or("No document is open")?;
        let selection = doc.document.selection.clone();
        // W10-H: a 32-bit layer is filled at f32; unfilled pixels keep HDR.
        if doc.document.meta.bit_depth == 32 {
            let before = crate::depth32::layer_rgbaf32(doc, layer);
            let after = fill_pixels(&before, &selection, spec, source, w, h);
            if after == before {
                return Err("The fill would change nothing".to_string());
            }
            crate::depth32::layer_rgbaf32_command(doc, layer, &after, "Fill")?
        } else if doc.is_sixteen_bit() {
            let before = doc.layer_rgba16(layer);
            let after = fill_pixels(&before, &selection, spec, source, w, h);
            if after == before {
                return Err("The fill would change nothing".to_string());
            }
            crate::fade::remember(doc.id(), layer, "Fill", &before, &after);
            doc.layer_rgba16_command(layer, &after, "Fill")?
        } else {
            let before = pixels::read_layer(doc, layer);
            let after = fill_pixels(&before, &selection, spec, source, w, h);
            if after == before {
                return Err("The fill would change nothing".to_string());
            }
            // W10-G: Edit > Fade can fade this step.
            crate::fade::remember(doc.id(), layer, "Fill", &before, &after);
            pixels::write_layer(doc, layer, &after, "Fill")?
        }
    };
    editor.apply_command(command);
    Ok(format!(
        "Filled with {} at {}% opacity, {} mode",
        if matches!(spec.contents, ui::dialogs::FillContents::Pattern(_)) {
            "the pattern"
        } else {
            "the chosen colour"
        },
        (spec.opacity * 100.0).round() as u32,
        spec.blend.label()
    ))
}

/// The width of the Edit ▸ Stroke band, in pixels, when the dialog is not
/// hosted. Named as one decision in one place, exactly like [`MODIFY_RADIUS`].
const STROKE_WIDTH: u32 = 1;

/// Edit ▸ Stroke…: paint a `STROKE_WIDTH`-pixel band of the foreground colour
/// along the active selection's border, through the same compile-time-masked
/// read-modify-write each fill uses. Honest about running at its default width
/// because the shell hosts no stroke dialog.
fn stroke_selection(editor: &mut Editor) -> Result<String, String> {
    stroke_selection_with(
        editor,
        &ui::dialogs::StrokeSpec {
            width: STROKE_WIDTH,
            ..Default::default()
        },
    )
}

/// Stroke the selection's border with the Stroke dialog's confirmed spec.
///
/// The band comes from the selection's own morphology: *inside* is the mask
/// minus its erosion, *outside* the dilation minus the mask, and *centre* the
/// straddling band [`selection::border`] already computes. Painting is the
/// same source-over-with-blend the fill uses.
pub(crate) fn stroke_selection_with(
    editor: &mut Editor,
    spec: &ui::dialogs::StrokeSpec,
) -> Result<String, String> {
    let layer = pixel_layer(editor)?;
    let (w, h) = canvas_of(editor)?;
    let rgba = editor.foreground();
    let hex = crate::editor::color_hex(rgba);
    let command = {
        let doc = editor.active_mut().ok_or("No document is open")?;
        let selection = doc.document.selection.clone();
        let rect = canvas_rect(w, h);
        let mask = selection::to_mask(&selection, rect).map_err(|e| e.to_string())?;
        if mask.is_empty() {
            return Err("There is no selection to stroke".to_string());
        }
        let band = match spec.location {
            ui::dialogs::StrokeLocation::Inside => selection::combine(
                &mask,
                &selection::contract(&mask, spec.width).map_err(|e| e.to_string())?,
                selection::BooleanOp::Subtract,
            )
            .map_err(|e| e.to_string())?,
            ui::dialogs::StrokeLocation::Outside => selection::combine(
                &selection::expand(&mask, spec.width).map_err(|e| e.to_string())?,
                &mask,
                selection::BooleanOp::Subtract,
            )
            .map_err(|e| e.to_string())?,
            ui::dialogs::StrokeLocation::Center => {
                selection::border(&mask, spec.width).map_err(|e| e.to_string())?
            }
        };
        let before = pixels::read_layer(doc, layer);
        let mut after = before.clone();
        // The wells are normalized floats.
        let src = [rgba[0], rgba[1], rgba[2]];
        let src_a = rgba[3].clamp(0.0, 1.0) * spec.opacity.clamp(0.0, 1.0);
        if let Some((lo, hi)) = band.bounds() {
            for py in lo.y.max(0)..hi.y.min(h as i32) {
                for px in lo.x.max(0)..hi.x.min(w as i32) {
                    let coverage = band.coverage_at(glam::IVec2::new(px, py));
                    if coverage == 0 {
                        continue;
                    }
                    let i = (py as usize * w as usize + px as usize) * 4;
                    let dst_a = f32::from(before[i + 3]) / 255.0;
                    if spec.preserve_transparency && dst_a <= 0.0 {
                        continue;
                    }
                    // The band's own anti-aliased coverage joins the paint's
                    // alpha, so a soft edge strokes softly.
                    let paint_a = src_a
                        * (f32::from(coverage) / 255.0)
                        * if spec.preserve_transparency {
                            dst_a
                        } else {
                            1.0
                        };
                    let base = [
                        f32::from(before[i]) / 255.0,
                        f32::from(before[i + 1]) / 255.0,
                        f32::from(before[i + 2]) / 255.0,
                    ];
                    let blended = spec.blend.blend_rgb(base, src);
                    let out_a = paint_a + dst_a * (1.0 - paint_a);
                    let out_rgb = if out_a <= 0.0 {
                        [0.0; 3]
                    } else {
                        [
                            (blended[0] * paint_a + base[0] * dst_a * (1.0 - paint_a)) / out_a,
                            (blended[1] * paint_a + base[1] * dst_a * (1.0 - paint_a)) / out_a,
                            (blended[2] * paint_a + base[2] * dst_a * (1.0 - paint_a)) / out_a,
                        ]
                    };
                    after[i] = (out_rgb[0] * 255.0).round().clamp(0.0, 255.0) as u8;
                    after[i + 1] = (out_rgb[1] * 255.0).round().clamp(0.0, 255.0) as u8;
                    after[i + 2] = (out_rgb[2] * 255.0).round().clamp(0.0, 255.0) as u8;
                    after[i + 3] = (out_a * 255.0).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        if after == before {
            return Err("The stroke would change nothing".to_string());
        }
        // W10-G: Edit > Fade can fade this step.
        crate::fade::remember(doc.id(), layer, "Stroke", &before, &after);
        pixels::write_layer(doc, layer, &after, "Stroke")?
    };
    editor.apply_command(command);
    Ok(format!(
        "Stroked {} px {} with {hex} at {}% opacity, {} mode",
        spec.width,
        spec.location.label(),
        (spec.opacity * 100.0).round() as u32,
        spec.blend.label()
    ))
}

/// Replace the active document's selection.
///
/// Card 056: every Select-menu selection change rides HISTORY as
/// `Command::SetSelection` (inverse = the previous selection), the same route
/// a marquee drag takes since that card — so Select ▸ Inverse and a marquee
/// drag are both undoable, one gesture per step. A change that does not
/// alter the selection is refused rather than recorded as a no-op step.
fn set_selection(
    editor: &mut Editor,
    next: impl FnOnce(&editor_core::Selection, u32, u32) -> Result<editor_core::Selection, String>,
) -> Result<(), String> {
    let (w, h) = canvas_of(editor)?;
    let current = editor
        .active()
        .ok_or("No document is open")?
        .document
        .selection
        .clone();
    let value = next(&current, w, h)?;
    if value == current {
        return Err("The selection is already that".to_string());
    }
    editor.apply_command(Command::SetSelection { selection: value });
    Ok(())
}

/// Select ▸ Save Selection…: append the live selection to the document's
/// named list under the name the dialog confirmed (W3-H), and make it the
/// stored selection Reselect brings back.
///
/// With no name parked — a caller that opened no dialog — the entry takes the
/// first free "Alpha N", the name the dialog would have opened at. A name the
/// list already holds is refused rather than shadowed: Load lists by name.
fn save_selection(editor: &mut Editor) -> Result<String, String> {
    let parked = crate::dialog_host::take_confirmed_save_selection();
    let doc = editor
        .active_mut()
        .ok_or_else(|| "No document is open".to_string())?;
    let selection = doc.document.selection.clone();
    if selection.is_none() {
        return Err("There is no selection to save".to_string());
    }
    let names = crate::dialog_host::saved_selection_names(&doc.document);
    let name = match parked {
        Some(spec) => spec.name,
        None => ui::dialogs::selection_name::next_alpha_name(&names),
    };
    if names.contains(&name) {
        return Err(format!("A saved selection is already called \"{name}\""));
    }
    doc.document
        .saved_selections
        .push((name.clone(), selection.clone()));
    doc.document.stored_selection = Some(selection);
    doc.document.mark_dirty();
    Ok(format!("Saved the selection as \"{name}\""))
}

/// Select ▸ Load Selection…: bring a saved selection back by name, combined
/// with the live one the way the dialog confirmed (W3-H) — new, add,
/// subtract or intersect, optionally inverted — as one undoable
/// `SetSelection` step.
///
/// With nothing parked (no dialog), the most recent entry replaces the live
/// selection, which is what the row did before it asked.
fn load_selection(editor: &mut Editor) -> Result<String, String> {
    use ui::dialogs::LoadOperation as Op;
    let parked = crate::dialog_host::take_confirmed_load_selection();
    let (w, h) = canvas_of(editor)?;
    let doc = editor
        .active_mut()
        .ok_or_else(|| "No document is open".to_string())?;
    let (name, saved, op, invert) = match parked {
        Some(spec) => {
            // The dialog captured the list when it opened; if the entry at
            // that index is no longer the one it named, refuse rather than
            // load whatever moved into the slot.
            let Some((name, saved)) = doc.document.saved_selections.get(spec.index) else {
                return Err(format!("\"{}\" is no longer saved", spec.name));
            };
            if *name != spec.name {
                return Err(format!("\"{}\" is no longer saved", spec.name));
            }
            (name.clone(), saved.clone(), spec.op, spec.invert)
        }
        None => {
            let (name, saved) = doc
                .document
                .saved_selections
                .last()
                .cloned()
                .or_else(|| {
                    doc.document
                        .stored_selection
                        .clone()
                        .map(|s| ("the stored selection".to_string(), s))
                })
                .ok_or_else(|| "No selection has been saved".to_string())?;
            (name, saved, Op::New, false)
        }
    };
    let canvas = canvas_rect(w, h);
    let incoming = if invert {
        selection::invert_selection(&saved, canvas).map_err(|e| e.to_string())?
    } else {
        saved
    };
    let live = doc.document.selection.clone();
    let has_live = live.bounds().is_some();
    let next = match op {
        Op::New => incoming,
        // "Everything" is what a Selection::None materialises as, so a
        // combine against no live selection would answer against the whole
        // canvas. Photopea offers only New there; the dialog greys the
        // others, and a bypassing caller is refused with the same reason.
        _ if !has_live => {
            return Err("There is no live selection to combine with".to_string());
        }
        Op::Add | Op::Subtract | Op::Intersect => {
            let boolean = match op {
                Op::Add => selection::BooleanOp::Add,
                Op::Subtract => selection::BooleanOp::Subtract,
                _ => selection::BooleanOp::Intersect,
            };
            selection::combine_selection(canvas, &live, &incoming, boolean)
                .map_err(|e| e.to_string())?
        }
    };
    if next == live {
        return Err("The saved selection is already active".to_string());
    }
    doc.document.stored_selection = None;
    // Card 056: a user-facing selection edit rides history like every other.
    editor.apply_command(Command::SetSelection { selection: next });
    Ok(match op {
        Op::New => format!("Loaded \"{name}\""),
        Op::Add => format!("Added \"{name}\" to the selection"),
        Op::Subtract => format!("Subtracted \"{name}\" from the selection"),
        Op::Intersect => format!("Intersected the selection with \"{name}\""),
    })
}

/// W9-A: Photopea's "Select Pixels" — a Ctrl+click on a layer thumbnail (or
/// the layer-row menu) makes a selection from that layer's transparency; on a
/// mask thumbnail, from the mask's coverage. `op` combines it with the live
/// selection through the selection crate's boolean ops (Add with nothing live
/// is a plain New, as in Photopea). Lands as one undoable
/// `Command::SetSelection`. `layer: None` means the active layer.
///
/// The layer's alpha is the REAL compositor's answer over a staged copy of
/// the document in which everything but the layer, its descendants and its
/// ancestors is hidden, and the layer and its ancestors are neutralised
/// (full opacity, no mask, no effects, no clipping) — so a text, shape,
/// smart-object or transformed layer selects exactly the ink it draws, in
/// document space, and a layer's opacity or mask does not thin the result
/// (Photoshop loads the layer's transparency, not its contribution).
fn select_layer_pixels(
    editor: &mut Editor,
    layer: Option<LayerId>,
    mask: bool,
    op: ui::dialogs::LoadOperation,
) -> Result<String, String> {
    use ui::dialogs::LoadOperation as Op;
    let (w, h) = canvas_of(editor)?;
    let canvas = canvas_rect(w, h);
    let doc = editor.active().ok_or("No document is open")?;
    let id = match layer {
        Some(id) => id,
        None => doc.document.active_layer().ok_or("Select a layer first")?,
    };
    let target = doc
        .document
        .layers
        .get(id)
        .ok_or("That layer is no longer in the document")?;
    let name = target.name.clone();
    let incoming = if mask {
        let m = target
            .mask
            .as_ref()
            .ok_or_else(|| format!("\"{name}\" has no mask"))?;
        layer_mask_selection(doc, id, m, canvas)?
    } else {
        layer_alpha_selection(doc, id)?
    };
    let live = doc.document.selection.clone();
    let has_live = live.bounds().is_some();
    let replace = op == Op::New || (op == Op::Add && !has_live);
    let next = if replace {
        if incoming.bounds().is_none() {
            return Err(format!(
                "No pixels are selected: the {} of \"{name}\" is empty",
                if mask { "mask" } else { "layer" }
            ));
        }
        incoming
    } else if !has_live {
        return Err("There is no selection to combine with".to_string());
    } else {
        let boolean = match op {
            Op::Subtract => selection::BooleanOp::Subtract,
            Op::Intersect => selection::BooleanOp::Intersect,
            _ => selection::BooleanOp::Add,
        };
        let combined = selection::combine_selection(canvas, &live, &incoming, boolean)
            .map_err(|e| e.to_string())?;
        // Nothing left selected is "no selection", as Deselect leaves it —
        // an empty mask would read as a selection that covers nothing.
        if combined.bounds().is_none() {
            editor_core::Selection::None
        } else {
            combined
        }
    };
    if next == live {
        return Err("The selection is already that".to_string());
    }
    editor.apply_command(Command::SetSelection { selection: next });
    let what = if mask {
        format!("the mask of \"{name}\"")
    } else {
        format!("\"{name}\"")
    };
    Ok(match op {
        Op::New => format!("Selected the pixels of {what}"),
        Op::Add => format!("Added the pixels of {what} to the selection"),
        Op::Subtract => format!("Subtracted the pixels of {what} from the selection"),
        Op::Intersect => format!("Intersected the selection with the pixels of {what}"),
    })
}

/// W9-A: `layer`'s transparency as a document-space selection — see
/// [`select_layer_pixels`] for why the staged copy is shaped as it is.
fn layer_alpha_selection(
    doc: &crate::doc::OpenDocument,
    layer: LayerId,
) -> Result<editor_core::Selection, String> {
    let mut staged = doc.document.clone();
    let mut ancestors = Vec::new();
    let mut parent = staged.layers.parent_of(layer);
    while let Some(p) = parent {
        ancestors.push(p);
        parent = staged.layers.parent_of(p);
    }
    let within = |tree: &layer_model::LayerTree, mut at: LayerId| loop {
        if at == layer {
            return true;
        }
        match tree.parent_of(at) {
            Some(p) => at = p,
            None => return false,
        }
    };
    let hidden: Vec<LayerId> = staged
        .layers
        .iter_depth_first()
        .into_iter()
        .filter(|&other| !within(&staged.layers, other) && !ancestors.contains(&other))
        .collect();
    for other in hidden {
        if let Some(l) = staged.layers.get_mut(other) {
            l.visible = false;
        }
    }
    for id in std::iter::once(layer).chain(ancestors.iter().copied()) {
        if let Some(l) = staged.layers.get_mut(id) {
            l.visible = true;
            l.opacity = 1.0;
            l.fill_opacity = 1.0;
            l.blend_mode = layer_model::BlendMode::Normal;
            l.mask = None;
            l.effects = layer_model::LayerEffects::default();
            l.clipping = layer_model::ClippingMode::None;
        }
    }
    let rect = doc.canvas_rect();
    let composite = compositor::composite_region(
        &staged,
        &doc.tiles,
        rect,
        0,
        compositor::CompositeOptions::default(),
    )
    .map_err(|e| e.to_string())?;
    let alpha: Vec<u8> = composite
        .pixels()
        .iter()
        .map(|px| (px[3].clamp(0.0, 1.0) * 255.0).round() as u8)
        .collect();
    let mask = selection::channel_to_selection(glam::IVec2::ZERO, rect.width, rect.height, &alpha)
        .map_err(|e| e.to_string())?;
    Ok(editor_core::Selection::Mask(mask))
}

/// W9-A: a layer mask's coverage as a document-space selection: the stored
/// tiles reassembled (`selection::mask_tiles_to_selection`), carried through
/// the mask's document pose (the layer's full placement composed with the
/// mask's own transform — the compositor's pose), clipped to the canvas, and
/// inverted over the canvas when the mask is.
fn layer_mask_selection(
    doc: &crate::doc::OpenDocument,
    layer: LayerId,
    mask: &layer_model::LayerMask,
    canvas: selection::Rect,
) -> Result<editor_core::Selection, String> {
    use compositor::TileSource;
    let tiles: Vec<selection::MaskTile> = doc
        .document
        .pixels
        .tiles(editor_core::PixelKey::Mask(mask.id))
        .map(|map| {
            map.iter()
                .filter(|(coord, _)| coord.level == 0)
                .filter_map(|(coord, hash)| {
                    doc.tiles.tile(hash).map(|bytes| selection::MaskTile {
                        coord,
                        coverage: bytes.to_vec(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let raw = selection::mask_tiles_to_selection(&tiles).map_err(|e| e.to_string())?;
    let pose = crate::interaction_geometry::document_transform_of(&doc.document, layer, 0)
        .map_err(|e| format!("{e:?}"))?
        * *mask.transform;
    let placed = if pose == glam::Affine2::IDENTITY {
        raw
    } else {
        selection::transform(&raw, pose, selection::ResampleFilter::Bilinear)
            .map_err(|e| e.to_string())?
    };
    let clip = selection::rectangle(canvas).map_err(|e| e.to_string())?;
    let placed = selection::combine(&placed, &clip, selection::BooleanOp::Intersect)
        .map_err(|e| e.to_string())?;
    let selection = editor_core::Selection::Mask(placed);
    if mask.inverted {
        selection::invert_selection(&selection, canvas).map_err(|e| e.to_string())
    } else {
        Ok(selection)
    }
}

/// Select ▸ Reselect: bring back the most recently saved selection and clear
/// the store (the “Ctrl+Shift+D” shortcut).
fn reselect(editor: &mut Editor) -> Result<String, String> {
    let doc = editor
        .active_mut()
        .ok_or_else(|| "No document is open".to_string())?;
    let saved = doc
        .document
        .stored_selection
        .clone()
        .or_else(|| doc.document.saved_selections.last().map(|(_, s)| s.clone()))
        .ok_or_else(|| "There is no selection to restore".to_string())?;
    if doc.document.selection == saved {
        return Err("The saved selection is already active".to_string());
    }
    doc.document.stored_selection = None;
    // Card 056: history-backed, like every Select-menu edit.
    editor.apply_command(Command::SetSelection { selection: saved });
    Ok("Reselected".to_string())
}

/// The radius a Select ▸ Modify item uses when no dialog asked, in pixels —
/// the amount every Modify dialog opens at
/// (`ui::dialogs::selection_modify::DEFAULT_MODIFY_PX`).
pub const MODIFY_RADIUS: u32 = 4;

/// Select ▸ Modify ▸ <op>…: the morphology at the amount the dialog confirmed
/// (W3-H), as one undoable `SetSelection` step. With nothing parked for `op`
/// (no dialog) it runs at [`MODIFY_RADIUS`].
fn modify_selection(editor: &mut Editor, op: ui::menu::ModifySelection) -> Result<String, String> {
    use ui::menu::ModifySelection as M;
    let spec = crate::dialog_host::take_confirmed_modify(op).unwrap_or(ui::dialogs::ModifySpec {
        op,
        amount: MODIFY_RADIUS as f32,
    });
    if !spec.is_valid() {
        return Err(format!(
            "{} px is outside what {} accepts",
            spec.amount,
            op.label().trim_end_matches('…')
        ));
    }
    let px = spec.whole_px();
    set_selection(editor, |sel, w, h| {
        let rect = canvas_rect(w, h);
        let mask = selection::to_mask(sel, rect).map_err(|e| e.to_string())?;
        let next = match op {
            M::Border => selection::border(&mask, px),
            M::Smooth => selection::smooth(&mask, px),
            M::Expand => selection::expand(&mask, px),
            M::Contract => selection::contract(&mask, px),
            M::Feather => selection::feather(&mask, spec.amount),
        }
        .map_err(|e| e.to_string())?;
        Ok(editor_core::Selection::Mask(next))
    })?;
    let amount = if op == M::Feather {
        format!("{}", spec.amount)
    } else {
        px.to_string()
    };
    Ok(format!(
        "{} by {amount} px",
        op.label().trim_end_matches('…')
    ))
}

/// The colour distance Grow, Similar and Color Range work to.
pub const DEFAULT_TOLERANCE: f32 = 32.0 / 255.0;

fn grow_or_similar(editor: &mut Editor, contiguous: bool) -> Result<String, String> {
    let layer = pixel_layer(editor)?;
    let (w, h) = canvas_of(editor)?;
    let rgba = {
        let doc = editor.active().ok_or("No document is open")?;
        pixels::read_layer(doc, layer)
    };
    let image = selection::ImageBuffer::from_rgba8(glam::IVec2::ZERO, w, h, rgba)
        .map_err(|e| e.to_string())?;
    set_selection(editor, |sel, w, h| {
        let mask = selection::to_mask(sel, canvas_rect(w, h)).map_err(|e| e.to_string())?;
        let metric = selection::ColorMetric::default();
        let next = if contiguous {
            selection::grow(&image.view(), &mask, DEFAULT_TOLERANCE, metric, false)
        } else {
            selection::similar(&image.view(), &mask, DEFAULT_TOLERANCE, metric)
        }
        .map_err(|e| e.to_string())?;
        Ok(editor_core::Selection::Mask(next))
    })?;
    Ok(if contiguous {
        "Grown into neighbouring pixels of a similar colour".to_string()
    } else {
        "Extended to every pixel of a similar colour".to_string()
    })
}

/// Select ▸ Color Range…: select by the spec the dialog confirmed (W3-H) —
/// colour, fuzziness, invert — over the active pixel layer at full
/// resolution, as one undoable `SetSelection` step. The coverage comes from
/// [`ui::dialogs::ColorRangeSpec::mask`], the function the dialog's preview
/// runs, so the preview and the result cannot disagree. With nothing parked
/// (no dialog) it selects around the foreground at the opening fuzziness.
fn color_range(editor: &mut Editor) -> Result<String, String> {
    let layer = pixel_layer(editor)?;
    let (w, h) = canvas_of(editor)?;
    let spec = crate::dialog_host::take_confirmed_color_range()
        .unwrap_or_else(|| ui::dialogs::ColorRangeSpec::new(rgba8_of(editor.foreground())));
    let rgba = {
        let doc = editor.active().ok_or("No document is open")?;
        pixels::read_layer(doc, layer)
    };
    let mask = spec.mask(&rgba, w, h)?;
    set_selection(editor, |_, _, _| Ok(editor_core::Selection::Mask(mask)))?;
    let hex = format!(
        "#{:02X}{:02X}{:02X}",
        spec.color[0], spec.color[1], spec.color[2]
    );
    Ok(format!(
        "Selected {} {hex} at fuzziness {}",
        if spec.invert {
            "everything but"
        } else {
            "everything near"
        },
        spec.fuzziness
    ))
}

/// Edit ▸ Copy and Edit ▸ Copy Merged.
///
/// The lifted rectangle is the selection's bounding box, with anything the
/// selection does not cover made transparent — so copying a lasso brings back
/// the lasso's shape and not its bounding box.
fn copy(editor: &mut Editor, merged: bool) -> Result<String, String> {
    let (w, h) = canvas_of(editor)?;
    let full = if merged {
        // The real compositor, so Copy Merged is what the canvas shows.
        let doc = editor.active_mut().ok_or("No document is open")?;
        let rect = doc.canvas_rect();
        doc.composite(rect).map_err(|e| e.to_string())?
    } else {
        let layer = pixel_layer(editor)?;
        let doc = editor.active().ok_or("No document is open")?;
        pixels::read_layer(doc, layer)
    };
    let selection = editor
        .active()
        .ok_or("No document is open")?
        .document
        .selection
        .clone();
    let mut shaped = full;
    let empty = vec![0u8; (w as usize) * (h as usize) * 4];
    pixels::mask_by_selection(&empty, &mut shaped, &selection, w, h);

    let (min, max) = selection
        .bounds()
        .unwrap_or((glam::IVec2::ZERO, glam::IVec2::new(w as i32, h as i32)));
    let x0 = min.x.clamp(0, w as i32) as u32;
    let y0 = min.y.clamp(0, h as i32) as u32;
    let x1 = max.x.clamp(0, w as i32) as u32;
    let y1 = max.y.clamp(0, h as i32) as u32;
    if x1 <= x0 || y1 <= y0 {
        return Err("There is nothing inside the selection to copy".to_string());
    }
    let (cw, ch) = (x1 - x0, y1 - y0);
    let mut rgba = vec![0u8; (cw as usize) * (ch as usize) * 4];
    for row in 0..ch {
        let s = (((y0 + row) as usize) * w as usize + x0 as usize) * 4;
        let d = (row as usize) * cw as usize * 4;
        let n = cw as usize * 4;
        rgba[d..d + n].copy_from_slice(&shaped[s..s + n]);
    }
    editor.set_clipboard(crate::editor::Clipboard {
        width: cw,
        height: ch,
        rgba8: rgba.clone(),
        origin: (x0, y0),
    });
    // Card 052: the same pixels cross the process boundary — the OS image
    // clipboard carries the copy so another application can take it. A
    // refusal there (busy, image-less platform) does NOT fail the copy: the
    // internal store above still holds everything, and the status says what
    // did not cross.
    let mut os_note = String::new();
    match editor
        .image_clipboard_mut()
        .set_image(&crate::clipboard::ClipboardImage {
            width: cw,
            height: ch,
            rgba: rgba.clone(),
        }) {
        Ok(()) => editor.remember_os_copy(&crate::clipboard::ClipboardImage {
            width: cw,
            height: ch,
            rgba,
        }),
        Err(e) => os_note = format!(" (the OS clipboard refused: {e})"),
    }
    Ok(format!(
        "Copied {cw}×{ch} pixels{}{}",
        if merged {
            " from every visible layer"
        } else {
            ""
        },
        os_note
    ))
}

fn cut(editor: &mut Editor) -> Result<String, String> {
    let copied = copy(editor, false)?;
    let cleared = clear_selection(editor)?;
    Ok(format!("{copied}, {}", cleared.to_lowercase()))
}

/// Edit ▸ Paste and Edit ▸ Paste Into.
///
/// Card 052 freshness policy (plain Paste only): the OS image clipboard is
/// read FIRST. An image that is NOT the editor's own last copy — a screenshot,
/// another application's payload — wins and pastes through the full-source
/// placement builder (centered; see [`Editor::paste_external_image`]); the
/// editor's OWN copy is recognized by fingerprint and pastes through the
/// internal store below (same pixels, at-origin semantics). A clipboard that
/// holds no image, or cannot be read at all (busy, image-less platform), falls
/// back to the internal store — the card's "internal Copy → Paste still works
/// when the OS clipboard is temporarily unavailable". A TEXT payload is never
/// fetched or converted: no URL becomes an image.
///
/// The internal path lands on a **new layer** at the canvas origin, which is
/// one undoable step and cannot destroy what was already there. Paste Into
/// masks it by the current selection, which is the only thing that
/// distinguishes the two; Paste Into keeps the internal path (card 053 owns
/// its mask semantics).
///
/// W4-H, Edit > Paste Special: **Paste in Place** puts the internal copy back
/// at the canvas position it was copied from ([`crate::editor::Clipboard::
/// origin`]) instead of the origin, and **Paste Outside** is Paste Into with
/// the mask inverted — the pasted pixels show everywhere *except* the
/// selection. Both read only the internal store: an OS-clipboard image has no
/// copied position and no in-document origin.
fn paste(editor: &mut Editor, mode: PasteMode) -> Result<String, String> {
    let into = matches!(mode, PasteMode::Into | PasteMode::Outside);
    if mode == PasteMode::Plain {
        let external = editor.image_clipboard_mut().get_image();
        match external {
            Ok(Some(image)) if !editor.os_copy_is_ours(&image) => {
                return editor.paste_external_image(image);
            }
            Ok(Some(_)) | Ok(None) | Err(_) => {}
        }
    }
    let clip = editor
        .clipboard()
        .cloned()
        .ok_or("The clipboard is empty")?;
    let (w, h) = canvas_of(editor)?;
    let label = mode.label();
    // Card 053: the selection's coverage is read before anything mutates.
    let selection = editor
        .active()
        .ok_or("No document is open")?
        .document
        .selection
        .clone();
    if into && selection.is_empty() {
        return Err(format!("{label}: the selection holds no pixels"));
    }
    // Paste Outside masks by everything the selection does not cover.
    let selection = if mode == PasteMode::Outside {
        selection::invert_selection(&selection, canvas_rect(w, h)).map_err(|e| e.to_string())?
    } else {
        selection
    };
    let (ox, oy) = if mode == PasteMode::InPlace {
        clip.origin
    } else {
        (0, 0)
    };
    // The layer is created up front so its exact id is available after the
    // transaction applies (the paste selects what it created).
    let layer = layer_model::Layer::raster(label);
    let new_id = layer.id;
    let command = {
        let doc = editor.active_mut().ok_or("No document is open")?;
        // The FULL image is stored - nothing is multiplied away here. What
        // confines it to the selection is a retained raster mask derived from
        // the selection, so disabling the mask (Layer > Layer Mask > Toggle)
        // reveals every original pixel again.
        let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
        let rows = clip.height.min(h.saturating_sub(oy));
        let cols = clip.width.min(w.saturating_sub(ox));
        if rows == 0 || cols == 0 {
            return Err(format!("{label}: the clipboard does not fit the canvas"));
        }
        for row in 0..rows {
            let s = (row as usize) * clip.width as usize * 4;
            let d = ((oy + row) as usize * w as usize + ox as usize) * 4;
            let n = cols as usize * 4;
            rgba[d..d + n].copy_from_slice(&clip.rgba8[s..s + n]);
        }
        let mut commands = vec![Command::create_layer(layer)];
        let grid = raster::TileGrid::from_rgba8(w, h, &rgba).map_err(|e| e.to_string())?;
        let mut edits = Vec::new();
        for (coord, tile) in grid.iter() {
            let hash = doc.tiles.insert_bytes(tile.data().to_vec());
            edits.push(editor_core::pixels::TileEdit::set(coord, hash));
        }
        commands.push(
            Command::paint_tiles(editor_core::pixels::PixelTarget::Layer(new_id), edits)
                .map_err(|e| e.to_string())?,
        );
        if into {
            // The mask rides in the SAME transaction: one undo removes the
            // layer, its pixels and its mask together. The layer is born at
            // the canvas origin with an identity transform, so the coverage
            // (computed in document space) aligns exactly; `linked: true`
            // keeps that alignment as the layer moves. Fractional samples
            // from a lasso/wand selection map through unchanged.
            let coverage = pixels::selection_coverage_tiles(&selection, w, h);
            if coverage.is_empty() {
                // The selection has bounds but never intersects the canvas
                // (or the origin-pinned clip): a raster mask with no tiles is
                // hidden EVERYWHERE, so this paste would land invisible and
                // "succeed".
                return Err(format!("{label}: the selection hides all of it"));
            }
            let mut mask_edits = Vec::new();
            for (coord, coverage_bytes) in coverage {
                let hash = doc.tiles.insert_bytes(coverage_bytes);
                mask_edits.push(editor_core::pixels::TileEdit::set(coord, hash));
            }
            commands.push(Command::SetLayerProperties {
                layer_id: new_id,
                patch: editor_core::LayerPatch {
                    mask: editor_core::Patch::Set(layer_model::LayerMask::new(
                        layer_model::MaskId::new(),
                    )),
                    ..Default::default()
                },
            });
            commands.push(
                Command::paint_tiles(editor_core::pixels::PixelTarget::Mask(new_id), mask_edits)
                    .map_err(|e| e.to_string())?,
            );
        }
        Command::Transaction {
            label: label.to_string(),
            commands,
        }
    };
    editor.apply_command(command);
    // The pasted layer becomes the selection: the next gesture aims at what
    // the user just pasted (the same rule card 048 gave placements). The
    // created id is exact — if paste ever changes insertion position, an
    // indirect root().first() would silently select the wrong layer.
    editor.set_layer_selection(vec![new_id], Some(new_id));
    Ok(match mode {
        PasteMode::Plain => "Pasted onto a new layer".to_string(),
        PasteMode::Into => "Pasted into the selection on a new layer".to_string(),
        PasteMode::Outside => "Pasted outside the selection on a new layer".to_string(),
        PasteMode::InPlace => format!("Pasted in place at {ox}, {oy} onto a new layer"),
    })
}

/// Which Edit-menu paste [`paste`] performs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PasteMode {
    /// Edit > Paste: the OS clipboard first, then the internal store at the
    /// canvas origin.
    Plain,
    /// Edit > Paste Special > Paste Into: masked by the selection.
    Into,
    /// Edit > Paste Special > Paste Outside: masked by the inverse of the
    /// selection.
    Outside,
    /// Edit > Paste Special > Paste in Place: at the copied position.
    InPlace,
}

impl PasteMode {
    fn label(self) -> &'static str {
        match self {
            PasteMode::Plain => "Paste",
            PasteMode::Into => "Paste Into",
            PasteMode::Outside => "Paste Outside",
            PasteMode::InPlace => "Paste in Place",
        }
    }
}

/// Layer via Copy / Layer via Cut.
fn layer_via(editor: &mut Editor, cut: bool) -> Result<String, String> {
    let source = pixel_layer(editor)?;
    let (w, h) = canvas_of(editor)?;
    let label = if cut {
        "Layer via Cut"
    } else {
        "Layer via Copy"
    };
    let command = {
        let doc = editor.active_mut().ok_or("No document is open")?;
        let before = pixels::read_layer(doc, source);
        let selection = doc.document.selection.clone();
        // The lifted pixels: the layer masked down to the selection. Blending
        // *towards transparent black* outside the selection is exactly what
        // "copy what is selected" means, and it is the same call that clears
        // the hole below, run the other way round.
        let mut lifted = before.clone();
        let empty = vec![0u8; before.len()];
        pixels::mask_by_selection(&empty, &mut lifted, &selection, w, h);
        if lifted.iter().skip(3).step_by(4).all(|a| *a == 0) {
            return Err(format!("{label}: the selection holds no pixels"));
        }
        let layer = layer_model::Layer::raster(label);
        let new_id = layer.id;
        let mut commands = vec![Command::create_layer(layer)];
        // The new layer's pixels have to be addressable, so it is created
        // first and painted second, inside one transaction — the shape
        // `crate::import` uses for the same reason.
        let grid = raster::TileGrid::from_rgba8(w, h, &lifted).map_err(|e| e.to_string())?;
        let mut edits = Vec::new();
        for (coord, tile) in grid.iter() {
            let hash = doc.tiles.insert_bytes(tile.data().to_vec());
            edits.push(editor_core::pixels::TileEdit::set(coord, hash));
        }
        commands.push(
            Command::paint_tiles(editor_core::pixels::PixelTarget::Layer(new_id), edits)
                .map_err(|e| e.to_string())?,
        );
        if cut {
            let mut remaining = vec![0u8; before.len()];
            pixels::mask_by_selection(&before, &mut remaining, &selection, w, h);
            commands.push(pixels::write_layer(doc, source, &remaining, label)?);
        }
        Command::Transaction {
            label: label.to_string(),
            commands,
        }
    };
    editor.apply_command(command);
    Ok(format!("{label} created"))
}

fn group_layers(editor: &mut Editor) -> Result<String, String> {
    let command = {
        let doc = editor.active().ok_or("No document is open")?;
        let id = doc.document.active_layer().ok_or("Select a layer first")?;
        let parent = doc.document.layers.parent_of(id);
        let index = doc.document.layers.index_in_parent(id).unwrap_or(0);
        let group = layer_model::Layer::group("Group");
        let gid = group.id;
        Command::Transaction {
            label: "Group Layers".to_string(),
            commands: vec![
                Command::create_layer(group),
                Command::MoveLayer {
                    layer_id: gid,
                    parent,
                    index,
                },
                Command::MoveLayer {
                    layer_id: id,
                    parent: Some(gid),
                    index: 0,
                },
            ],
        }
    };
    editor.apply_command(command);
    Ok("Grouped".to_string())
}

fn ungroup_layers(editor: &mut Editor) -> Result<String, String> {
    let command = {
        let doc = editor.active().ok_or("No document is open")?;
        let id = doc.document.active_layer().ok_or("Select a layer first")?;
        let layer = doc
            .document
            .layers
            .get(id)
            .ok_or("The active layer is not in the document")?;
        let layer_model::LayerKind::Group(group) = &layer.kind else {
            return Err("Only a group can be ungrouped".to_string());
        };
        let children = group.children.clone();
        if children.is_empty() {
            return Err("The group is empty".to_string());
        }
        let parent = doc.document.layers.parent_of(id);
        let index = doc.document.layers.index_in_parent(id).unwrap_or(0);
        let mut commands: Vec<Command> = children
            .iter()
            .enumerate()
            .map(|(i, child)| Command::MoveLayer {
                layer_id: *child,
                parent,
                index: index + i,
            })
            .collect();
        commands.push(Command::DeleteLayer { layer_id: id });
        Command::Transaction {
            label: "Ungroup Layers".to_string(),
            commands,
        }
    };
    editor.apply_command(command);
    Ok("Ungrouped".to_string())
}

/// W9-K: the active layer, when it is an unlocked text layer: its id and a
/// copy of its payload.
fn active_text_layer(editor: &Editor) -> Result<(LayerId, layer_model::TextLayer), String> {
    let doc = editor.active().ok_or("No document is open")?;
    let id = doc.document.active_layer().ok_or("Select a layer first")?;
    let layer = doc
        .document
        .layers
        .get(id)
        .ok_or("The active layer is not in the document")?;
    if layer.locked.all {
        return Err("The layer is locked".to_string());
    }
    match &layer.kind {
        layer_model::LayerKind::Text(text) => Ok((id, text.clone())),
        _ => Err("The active layer is not a text layer".to_string()),
    }
}

/// W9-K: Layer > Text > Warp Style > <row>: the active text layer's live
/// warp takes the row's style (None clears it), as one undoable
/// [`Command::SetLayerKind`]. The text stays editable; the compositor bends
/// its glyph outlines on every render. Warp Text... is a question the dialog
/// host asks (`DialogHost::open_for_menu_action`); a click reaches here only
/// when there was no unlocked text layer to open it over, so the answer is
/// that reason.
fn warp_text(editor: &mut Editor, item: ui::menu::WarpTextItem) -> Result<String, String> {
    let (id, mut text) = active_text_layer(editor)?;
    if item == ui::menu::WarpTextItem::Dialog {
        return Err("Warp Text needs the dialog; open it from Layer > Text".to_string());
    }
    let before = text.warp;
    text.warp = item.apply(before);
    if text.warp == before {
        return Ok(format!("Warp Text: {} (no change)", item.label()));
    }
    editor.apply_command(Command::SetLayerKind {
        layer_id: id,
        kind: Box::new(layer_model::LayerKind::Text(text)),
    });
    Ok(format!("Warp Text: {}", item.label()))
}

/// W9-K: Layer > Text > Convert to Shape: the active text layer's glyph
/// outlines (through its warp or path) become a shape layer filled with the
/// text's colour, in the text layer's place - same parent, index, name,
/// transform, opacity, blending and effects - as one undoable transaction.
fn convert_text_to_shape(editor: &mut Editor) -> Result<String, String> {
    let (id, text) = active_text_layer(editor)?;
    let run = text_engine::TextRun::from(&text);
    let svg = text_engine::with_shared_library(|library| {
        let shaped = text_engine::shape(library, &run);
        text_engine::outline_svg(library, &shaped)
    });
    if svg.is_empty() {
        return Err("The text has no glyph outlines to convert".to_string());
    }
    let command = {
        let doc = editor.active().ok_or("No document is open")?;
        let source = doc
            .document
            .layers
            .get(id)
            .ok_or("The active layer is not in the document")?;
        let space = &doc.document.meta.color_space;
        let fill = text.style.fill;
        let encoded = color::from_linear(space, [fill[0], fill[1], fill[2]]);
        let mut shape = layer_model::Layer::with_kind(
            source.name.clone(),
            layer_model::LayerKind::Shape(layer_model::ShapeLayer {
                path_svg: svg,
                fill: Some([encoded[0], encoded[1], encoded[2], fill[3]]),
                ..layer_model::ShapeLayer::default()
            }),
        );
        shape.transform = source.transform;
        shape.opacity = source.opacity;
        shape.fill_opacity = source.fill_opacity;
        shape.blend_mode = source.blend_mode;
        shape.visible = source.visible;
        shape.effects = source.effects.clone();
        let new_id = shape.id;
        let parent = doc.document.layers.parent_of(id);
        let index = doc.document.layers.index_in_parent(id).unwrap_or(0);
        Command::Transaction {
            label: "Convert to Shape".to_string(),
            commands: vec![
                Command::create_layer(shape),
                Command::MoveLayer {
                    layer_id: new_id,
                    parent,
                    index,
                },
                Command::DeleteLayer { layer_id: id },
            ],
        }
    };
    editor.apply_command(command);
    Ok("Converted the text to a shape".to_string())
}

/// W9-F: Layer > Combine Shapes > Unite / Subtract Front / Intersect /
/// Exclude. The selected shape layers' paths are taken into document space
/// through each layer's own transform chain, folded bottom to top with the
/// vector boolean op (`tools::path_select::combine_shape_layers`), and
/// written back into the bottom-most layer (in its own space, keeping its
/// paint); the others are deleted. One transaction, so one Ctrl+Z restores
/// every layer.
fn combine_shapes(editor: &mut Editor, op: ui::menu::ShapeCombine) -> Result<String, String> {
    use ui::menu::ShapeCombine as Sc;
    let (command, base, count) = {
        let doc = editor.active().ok_or("No document is open")?;
        let mut picked = doc.document.layer_selection();
        if let Some(active) = doc.document.active_layer() {
            if !picked.contains(&active) {
                picked.push(active);
            }
        }
        // Depth-first is top-most first; the fold runs bottom first.
        let bottom_up: Vec<LayerId> = doc
            .document
            .layers
            .iter_depth_first()
            .into_iter()
            .rev()
            .filter(|id| picked.contains(id))
            .collect();
        if bottom_up.len() < 2 {
            return Err("Select two or more shape layers to combine".to_string());
        }
        let mut stack = Vec::with_capacity(bottom_up.len());
        for id in &bottom_up {
            let layer = doc
                .document
                .layers
                .get(*id)
                .ok_or("A selected layer is not in the document")?;
            let layer_model::LayerKind::Shape(shape) = &layer.kind else {
                return Err(format!("\"{}\" is not a shape layer", layer.name));
            };
            if layer.locked.all {
                return Err(format!("\"{}\" is locked", layer.name));
            }
            let to_doc = crate::interaction_geometry::document_transform_of(&doc.document, *id, 0)
                .map_err(|_| format!("\"{}\" cannot be placed in the document", layer.name))?;
            stack.push((shape, to_doc));
        }
        let combine = match op {
            Sc::Unite => tools::path_select::PathCombine::Unite,
            Sc::SubtractFront => tools::path_select::PathCombine::Subtract,
            Sc::Intersect => tools::path_select::PathCombine::Intersect,
            Sc::Exclude => tools::path_select::PathCombine::Exclude,
        };
        let svg = tools::path_select::combine_shape_layers(&stack, combine).map_err(|e| {
            format!(
                "{}: nothing of the shapes could be combined ({e})",
                op.label()
            )
        })?;
        let mut shape = stack[0].0.clone();
        shape.path_svg = svg;
        // A boolean result is a non-zero region whatever the inputs used.
        shape.fill_rule = layer_model::ShapeFillRule::NonZero;
        let base = bottom_up[0];
        let mut commands = vec![Command::SetLayerKind {
            layer_id: base,
            kind: Box::new(layer_model::LayerKind::Shape(shape)),
        }];
        commands.extend(
            bottom_up[1..]
                .iter()
                .map(|id| Command::DeleteLayer { layer_id: *id }),
        );
        (
            Command::Transaction {
                label: op.label().to_string(),
                commands,
            },
            base,
            bottom_up.len(),
        )
    };
    editor.apply_command(command);
    editor.set_layer_selection(vec![base], Some(base));
    Ok(format!("{}: {count} shape layers combined", op.label()))
}

enum MergeScope {
    /// The active layer and the one directly beneath it.
    Down,
    /// Every visible layer.
    Visible,
    /// Every layer, visible or not.
    All,
}

/// Merge Down / Merge Visible / Flatten Image.
///
/// All three are the same operation with a different set of layers: composite
/// that set through the real [`compositor`], put the result in one new raster
/// layer, and delete the originals — as one transaction, so one Ctrl+Z takes
/// the whole thing back.
fn merge(editor: &mut Editor, scope: MergeScope) -> Result<String, String> {
    let (label, doomed, home, merged) = {
        let doc = editor.active().ok_or("No document is open")?;
        let order = doc.document.layers.iter_depth_first();
        // `home` is where the merged layer has to end up. Only Merge Down has
        // one: it replaces two layers in the middle of a stack, so landing the
        // result on top of the document would silently restack the drawing.
        // Flatten and Merge Visible legitimately produce the root's only, or
        // topmost, layer.
        let (label, doomed, home) = match scope {
            MergeScope::All => ("Flatten Image", order.clone(), None),
            MergeScope::Visible => (
                "Merge Visible",
                order
                    .iter()
                    .copied()
                    .filter(|id| doc.document.layers.get(*id).is_some_and(|l| l.visible))
                    .collect(),
                None,
            ),
            MergeScope::Down => {
                let active = doc.document.active_layer().ok_or("Select a layer first")?;
                let parent = doc.document.layers.parent_of(active);
                let siblings = doc
                    .document
                    .layers
                    .siblings_of(active)
                    .ok_or("The active layer has no siblings")?;
                let at = doc
                    .document
                    .layers
                    .index_in_parent(active)
                    .ok_or("The active layer is not in the tree")?;
                let below = *siblings
                    .get(at + 1)
                    .ok_or("There is no layer below to merge into")?;
                // Two layers leave that sibling list and one arrives, so
                // the destination index is `at` clamped to the list it
                // lands in. `at + 1` exists, so `at` never exceeds it.
                let landing = at.min(siblings.len().saturating_sub(2));
                ("Merge Down", vec![active, below], Some((parent, landing)))
            }
        };
        if doomed.len() < 2 {
            return Err(format!("{label} needs at least two layers"));
        }
        // Composite a *copy* of the document with everything but the merge set
        // hidden, so the result is exactly what those layers draw and nothing
        // else. The real compositor, not a second implementation of blending.
        let mut staged = doc.document.clone();
        for id in staged.layers.iter_depth_first() {
            if !doomed.contains(&id) {
                if let Some(layer) = staged.layers.get_mut(id) {
                    layer.visible = false;
                }
            }
        }
        let rect = doc.canvas_rect();
        let canvas = compositor::composite_region(
            &staged,
            &doc.tiles,
            rect,
            0,
            compositor::CompositeOptions::default(),
        )
        .map_err(|e| e.to_string())?;
        (
            label,
            doomed,
            home,
            canvas.to_rgba8(&doc.document.meta.color_space),
        )
    };

    let (w, h) = canvas_of(editor)?;
    let command = {
        let doc = editor.active_mut().ok_or("No document is open")?;
        let layer = layer_model::Layer::raster(match scope {
            MergeScope::All => "Background",
            _ => "Merged",
        });
        let new_id = layer.id;
        let mut commands = vec![Command::create_layer(layer)];
        let grid = raster::TileGrid::from_rgba8(w, h, &merged).map_err(|e| e.to_string())?;
        let mut edits = Vec::new();
        for (coord, tile) in grid.iter() {
            let hash = doc.tiles.insert_bytes(tile.data().to_vec());
            edits.push(editor_core::pixels::TileEdit::set(coord, hash));
        }
        commands.push(
            Command::paint_tiles(editor_core::pixels::PixelTarget::Layer(new_id), edits)
                .map_err(|e| e.to_string())?,
        );
        // Deepest first, so deleting a group does not strand a child that is
        // also on the list.
        for id in doomed.iter().rev() {
            if doc.document.layers.contains(*id) {
                commands.push(Command::DeleteLayer { layer_id: *id });
            }
        }
        if let Some((parent, index)) = home {
            commands.push(Command::MoveLayer {
                layer_id: new_id,
                parent,
                index,
            });
        }
        Command::Transaction {
            label: label.to_string(),
            commands,
        }
    };
    editor.apply_command(command);
    Ok(format!("{label} applied"))
}

thread_local! {
    /// W9-G: the current path (SVG path data, document pixels) as of the
    /// last [`context`] — see the note there.
    static CURRENT_VECTOR_PATH: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

/// W9-G: `true` when `mask` is only a vector mask — vector kind, a path, and
/// no pixel coverage stored under its id (the compositor then skips its
/// pixel half; see `compositor::composite::Ctx::active_mask`).
pub(crate) fn is_vector_only(doc: &editor_core::Document, mask: &layer_model::LayerMask) -> bool {
    mask.kind == layer_model::MaskKind::Vector
        && mask.vector.is_some()
        && doc
            .pixels
            .tiles(editor_core::PixelKey::Mask(mask.id))
            .is_none_or(|t| t.is_empty())
}

/// W9-G: park the path Layer ▸ Vector Mask ▸ Current Path will use.
pub(crate) fn set_current_vector_path(path: Option<String>) {
    CURRENT_VECTOR_PATH.with(|c| *c.borrow_mut() = path);
}

/// W9-K: the Paths panel's current path (selected, else the Work Path) the
/// last menu context parked, in document space - what a Type click may flow
/// text along. The pointer route never sees the workspace, so it reads this.
pub(crate) fn current_vector_path() -> Option<String> {
    CURRENT_VECTOR_PATH.with(|c| c.borrow().clone())
}

/// W9-G: Layer ▸ Vector Mask ▸ Reveal All / Hide All / Current Path /
/// Delete / Disable-Enable. Each is one `SetLayerProperties` mask patch, so
/// each is one undo step.
///
/// A vector mask lives on [`layer_model::LayerMask::vector`]: added beside an
/// existing pixel mask, or as a vector-only mask
/// ([`layer_model::LayerMask::vector_only`]) on a layer without one. Current
/// Path maps the document-space path into the mask's layer space, so it
/// lands where it was drawn whatever the layer's transform.
fn vector_mask_op(editor: &mut Editor, op: ui::menu::VectorMaskOp) -> Result<String, String> {
    use layer_model::{LayerMask, MaskId, VectorMask};
    use ui::menu::VectorMaskOp as V;
    let (command, message) = {
        let doc = editor.active().ok_or("No document is open")?;
        let id = doc.document.active_layer().ok_or("Select a layer first")?;
        let layer = doc
            .document
            .layers
            .get(id)
            .ok_or("The active layer is not in the document")?;
        let existing = layer.mask.clone();
        let has_vector = existing.as_ref().is_some_and(|m| m.vector.is_some());
        let with_vector = |v: VectorMask| match existing.clone() {
            Some(mut m) => {
                m.vector = Some(Box::new(v));
                m
            }
            None => LayerMask::vector_only(MaskId::new(), v),
        };
        let (patch, message) = match op {
            V::RevealAll | V::HideAll | V::CurrentPath if has_vector => {
                return Err("The layer already has a vector mask".into())
            }
            V::RevealAll => (
                editor_core::Patch::Set(with_vector(VectorMask::reveal_all())),
                "Vector mask added (reveal all)",
            ),
            V::HideAll => (
                editor_core::Patch::Set(with_vector(VectorMask::hide_all())),
                "Vector mask added (hide all)",
            ),
            V::CurrentPath => {
                let path = CURRENT_VECTOR_PATH
                    .with(|c| c.borrow().clone())
                    .ok_or("There is no path; draw one or select it in the Paths panel")?;
                // Document pixels → the mask's layer space.
                let mask_pose = existing
                    .as_ref()
                    .map_or(glam::Affine2::IDENTITY, |m| *m.transform);
                let to_layer = (layer.transform * mask_pose).inverse();
                let local = compositor::vector_mask::svg_transformed(&path, to_layer)
                    .ok_or("The layer's transform cannot be inverted")?;
                (
                    editor_core::Patch::Set(with_vector(VectorMask::new(local))),
                    "Vector mask added from the current path",
                )
            }
            V::Delete | V::Toggle if !has_vector => {
                return Err("The layer has no vector mask".into())
            }
            V::Delete => {
                let mut m = existing.clone().expect("has_vector implies a mask");
                // A vector-only mask goes entirely; a pixel mask it rode on
                // stays.
                if is_vector_only(&doc.document, &m) {
                    (editor_core::Patch::Clear, "Vector mask deleted")
                } else {
                    m.vector = None;
                    (editor_core::Patch::Set(m), "Vector mask deleted")
                }
            }
            V::Toggle => {
                let mut m = existing.clone().expect("has_vector implies a mask");
                let v = m.vector.as_mut().expect("has_vector");
                v.enabled = !v.enabled;
                let message = if v.enabled {
                    "Vector mask enabled"
                } else {
                    "Vector mask disabled"
                };
                (editor_core::Patch::Set(m), message)
            }
        };
        (
            Command::SetLayerProperties {
                layer_id: id,
                patch: editor_core::LayerPatch {
                    mask: patch,
                    ..Default::default()
                },
            },
            message,
        )
    };
    editor.apply_command(command);
    Ok(message.to_string())
}

fn toggle_mask(editor: &mut Editor, link: bool) -> Result<String, String> {
    let (command, message) = {
        let doc = editor.active().ok_or("No document is open")?;
        let id = doc.document.active_layer().ok_or("Select a layer first")?;
        let layer = doc
            .document
            .layers
            .get(id)
            .ok_or("The active layer is not in the document")?;
        let mut mask = layer.mask.clone().ok_or("The layer has no mask")?;
        let message = if link {
            mask.linked = !mask.linked;
            if mask.linked {
                "Mask linked to the layer"
            } else {
                "Mask unlinked from the layer"
            }
        } else {
            mask.enabled = !mask.enabled;
            if mask.enabled {
                "Mask enabled"
            } else {
                "Mask disabled"
            }
        };
        (
            Command::SetLayerProperties {
                layer_id: id,
                patch: editor_core::LayerPatch {
                    mask: editor_core::Patch::Set(mask),
                    ..Default::default()
                },
            },
            message,
        )
    };
    editor.apply_command(command);
    Ok(message.to_string())
}

/// Card 057: the four mask-creation ops (Layer ▸ Layer Mask ▸ …) as REAL
/// coverage — the E09 lesson is that a bare `LayerMask::new` has NO tiles and
/// the compositor's convention reads an absent tile as HIDDEN, so "Reveal All"
/// without coverage hid the layer completely. Each op now attaches the mask
/// and paints its coverage in ONE undoable transaction.
///
/// The rules:
/// - **Reveal All** writes all-255 coverage over the whole canvas — it MUST
///   be stored, because "absent" means hidden and there is no absent-tile
///   way to say "revealed".
/// - **Hide All** attaches the mask with no tiles at all: absent means
///   hidden, which is exactly the requested state, stated by the convention
///   rather than by megabytes of zeros.
/// - **Reveal / Hide Selection** derive per-pixel coverage from the current
///   selection through the card 053 helper (fractional lasso/wand samples
///   survive; Hide Selection inverts, and a tile fully inside the selection
///   inverts to all-zero — omitted, because absent already hides it).
/// - An already-masked layer is REFUSED (delete the mask first): the menu
///   gates this too, and a silent replace would destroy coverage the user
///   can see.
/// - The selection ops refuse without a selection (`Selection::None` means
///   EVERY pixel, so "reveal none-of-it" and "hide all-of-it" are the other
///   two ops' jobs).
///
/// Card 058: invert the active layer's mask coverage (255 − v per stored
/// pixel).
///
/// One labelled transaction — one undo step — and the layer's pixel tiles
/// are never touched. The inversion runs over the mask's OWN store space, so
/// it is exact for any pose (a moved unlinked mask inverts where its
/// coverage lives, not where the canvas grid happens to be) and covers the
/// tiles the compositor samples: canvas-grid tiles that are absent read 0
/// (hidden) and become STORED all-255 (revealed), while existing tiles —/// including any outside the canvas pre-image — flip in place. The
/// all-zero-tile convention composes with itself: inverting twice returns
/// the exact original store.
fn invert_mask(editor: &mut Editor) -> Result<String, String> {
    use editor_core::pixels::{PixelTarget, TileEdit};
    let layer = pixel_layer(editor)?;
    let (w, h) = canvas_of(editor)?;
    let command = {
        let doc = editor.active_mut().ok_or("No document is open")?;
        doc.document
            .layers
            .get(layer)
            .and_then(|l| l.mask_id())
            .ok_or_else(|| "The layer has no mask".to_string())?;
        let ts = raster::TILE_SIZE as usize;
        let existing: std::collections::HashMap<raster::TileCoord, raster::TileHash> = doc
            .document
            .mask_tiles(layer)
            .map(|m| m.iter().collect())
            .unwrap_or_default();
        let tiles_x = w.div_ceil(ts as u32) as i32;
        let tiles_y = h.div_ceil(ts as u32) as i32;
        let mut edits = Vec::new();
        for ty in 0..tiles_y {
            for tx in 0..tiles_x {
                let coord = raster::TileCoord::new(tx, ty, 0);
                match existing.get(&coord) {
                    Some(hash) => {
                        let bytes = compositor::TileSource::tile(&doc.tiles, *hash)
                            .ok_or_else(|| "The mask's coverage is missing".to_string())?;
                        let inverted: Vec<u8> = bytes.iter().map(|&v| 255 - v).collect();
                        if inverted == bytes {
                            continue;
                        }
                        if inverted.iter().all(|&v| v == 0) {
                            // An all-zero result IS the absent-tile meaning:
                            // CLEAR the tile, don't store stale zeros (which
                            // would still read hidden but bloat the store
                            // and break the round-trip's convention).
                            edits.push(TileEdit::clear(coord));
                        } else {
                            let hash = doc.tiles.insert_bytes(inverted);
                            edits.push(TileEdit::set(coord, hash));
                        }
                    }
                    // Absent = all-hidden: the inversion reveals it, and an
                    // all-255 tile must be STORED, not left absent (the
                    // convention would keep reading it as hidden).
                    None => {
                        let hash = doc.tiles.insert_bytes(vec![255u8; ts * ts]);
                        edits.push(TileEdit::set(coord, hash));
                    }
                }
            }
        }
        // Store tiles outside the canvas grid flip in place too: they are
        // part of the mask's coverage field.
        for (coord, hash) in &existing {
            if coord.x >= 0 && coord.y >= 0 && coord.x < tiles_x && coord.y < tiles_y {
                continue;
            }
            if let Some(bytes) = compositor::TileSource::tile(&doc.tiles, *hash) {
                let inverted: Vec<u8> = bytes.iter().map(|&v| 255 - v).collect();
                if inverted != bytes {
                    if inverted.iter().all(|&v| v == 0) {
                        edits.push(TileEdit::clear(*coord));
                    } else {
                        let hash = doc.tiles.insert_bytes(inverted);
                        edits.push(TileEdit::set(*coord, hash));
                    }
                }
            }
        }
        // Defensively unreachable for u8 (255 − v = v has no integer
        // solution, and absent tiles always emit a set), but a ≥0-tile
        // canvas must never emit an empty delta.
        if edits.is_empty() {
            return Err("Nothing to invert".to_string());
        }
        // `PixelTarget::Mask` names the LAYER; resolve_pixel_key maps it to
        // the layer's mask (existence checked at apply time).
        let paint = Command::paint_tiles(PixelTarget::Mask(layer), edits)
            .map_err(|e: editor_core::CommandError| e.to_string())?;
        Ok::<Command, String>(Command::Transaction {
            label: "Invert Mask".to_string(),
            commands: vec![paint],
        })
    }?;
    editor.apply_command(command);
    Ok("Inverted the layer mask".to_string())
}

fn create_mask(editor: &mut Editor, op: ui::menu::MaskOp) -> Result<String, String> {
    let (w, h) = canvas_of(editor)?;
    let selection = editor
        .active()
        .ok_or("No document is open")?
        .document
        .selection
        .clone();
    let needs_selection = matches!(
        op,
        ui::menu::MaskOp::RevealSelection | ui::menu::MaskOp::HideSelection
    );
    if needs_selection
        && (matches!(selection, editor_core::Selection::None) || selection.is_empty())
    {
        return Err("Select an area first".to_string());
    }
    // Card 057 requirement: the coverage lives in the LAYER's local space
    // (the new mask is linked, so the compositor resolves it through the
    // layer transform) — the document-space selection is pre-imaged through
    // the layer transform's inverse, and the walk spans the layer's own
    // content tiles plus the canvas pre-image.
    let (layer_to_doc, content_tiles) = {
        let doc = editor.active().ok_or("No document is open")?;
        let id = doc.document.active_layer().ok_or("Select a layer first")?;
        let layer_to_doc = crate::interaction_geometry::document_transform_of(&doc.document, id, 0)
            .unwrap_or(glam::Affine2::IDENTITY);
        let content_tiles: Vec<raster::TileCoord> = doc
            .document
            .layer_tiles(id)
            .map(|m| m.iter().map(|(c, _)| c).collect())
            .unwrap_or_default();
        (layer_to_doc, content_tiles)
    };
    // W9-G (review round 3): a pixel mask added beside a vector-only mask
    // shares that mask's pose (`linked` + `transform`), so the vector half
    // does not jump or silently re-link. The coverage is then built in the
    // MASK's space: pre-imaged through layer ∘ mask transform, and the
    // layer's content tiles carried into mask space through the inverse.
    let vector_pose: Option<(bool, glam::Affine2)> = editor.active().and_then(|doc| {
        let id = doc.document.active_layer()?;
        let m = doc.document.layers.get(id)?.mask.as_ref()?;
        is_vector_only(&doc.document, m).then_some((m.linked, *m.transform))
    });
    let (layer_to_doc, content_tiles) = match vector_pose {
        Some((_, mask_t))
            if mask_t != glam::Affine2::IDENTITY && mask_t.matrix2.determinant() != 0.0 =>
        {
            let to_mask = mask_t.inverse();
            let ts = raster::TILE_SIZE as f32;
            let mut mapped = Vec::with_capacity(content_tiles.len() * 2);
            for c in &content_tiles {
                let (x0, y0) = (c.x as f32 * ts, c.y as f32 * ts);
                let pts = [
                    to_mask.transform_point2(glam::Vec2::new(x0, y0)),
                    to_mask.transform_point2(glam::Vec2::new(x0 + ts, y0)),
                    to_mask.transform_point2(glam::Vec2::new(x0, y0 + ts)),
                    to_mask.transform_point2(glam::Vec2::new(x0 + ts, y0 + ts)),
                ];
                for p in pts {
                    mapped.push(raster::TileCoord::new(
                        (p.x / ts).floor() as i32,
                        (p.y / ts).floor() as i32,
                        c.level,
                    ));
                }
            }
            (layer_to_doc * mask_t, mapped)
        }
        _ => (layer_to_doc, content_tiles),
    };
    let mode = match op {
        ui::menu::MaskOp::RevealAll => pixels::MaskCoverageMode::RevealAll,
        ui::menu::MaskOp::HideAll => pixels::MaskCoverageMode::HideAll,
        ui::menu::MaskOp::RevealSelection => pixels::MaskCoverageMode::RevealSelection,
        ui::menu::MaskOp::HideSelection => pixels::MaskCoverageMode::HideSelection,
        _ => return Err("Not a mask-creation op".to_string()),
    };
    let coverage: Vec<(raster::TileCoord, Vec<u8>)> =
        pixels::mask_creation_coverage(&selection, layer_to_doc, &content_tiles, w, h, mode)?;
    let label = match op {
        ui::menu::MaskOp::RevealAll => "Reveal All",
        ui::menu::MaskOp::HideAll => "Hide All",
        ui::menu::MaskOp::RevealSelection => "Reveal Selection",
        ui::menu::MaskOp::HideSelection => "Hide Selection",
        _ => unreachable!("filtered above"),
    };
    let command = {
        let doc = editor.active_mut().ok_or("No document is open")?;
        let id = doc.document.active_layer().ok_or("Select a layer first")?;
        let existing = doc.document.layers.get(id).and_then(|l| l.mask.clone());
        // W9-G: a vector-only mask has no pixel half yet; the new pixel mask
        // joins it rather than being refused (or replacing it).
        let keep_vector = existing
            .as_ref()
            .filter(|m| is_vector_only(&doc.document, m))
            .and_then(|m| m.vector.clone());
        if existing.is_some() && keep_vector.is_none() {
            return Err("The layer already has a mask — delete it first".to_string());
        }
        let mut new_mask = layer_model::LayerMask::new(layer_model::MaskId::new());
        new_mask.vector = keep_vector;
        if let (Some(old), Some(_)) = (existing.as_ref(), new_mask.vector.as_ref()) {
            // The vector half keeps its pose (see `vector_pose` above).
            new_mask.linked = old.linked;
            new_mask.transform = old.transform.clone();
        }
        let mut commands = vec![Command::SetLayerProperties {
            layer_id: id,
            patch: editor_core::LayerPatch {
                mask: editor_core::Patch::Set(new_mask),
                ..Default::default()
            },
        }];
        if !coverage.is_empty() {
            let mut edits = Vec::new();
            for (coord, coverage_bytes) in coverage {
                let hash = doc.tiles.insert_bytes(coverage_bytes);
                edits.push(editor_core::pixels::TileEdit::set(coord, hash));
            }
            commands.push(
                Command::paint_tiles(editor_core::pixels::PixelTarget::Mask(id), edits)
                    .map_err(|e| e.to_string())?,
            );
        }
        Command::Transaction {
            label: label.to_string(),
            commands,
        }
    };
    editor.apply_command(command);
    // W5-D: a new mask is the paint target, as in Photopea — otherwise the
    // first brush stroke after "add mask" lands on the image pixels.
    let attached = editor.active().is_some_and(|doc| {
        doc.document
            .active_layer()
            .and_then(|id| doc.document.layers.get(id))
            .is_some_and(|l| l.mask.is_some())
    });
    if attached {
        editor.set_edit_target_kind(crate::edit_target::EditTargetKind::Mask);
    }
    Ok(format!("{label} mask attached"))
}

/// Bake a layer's mask into its alpha and remove the mask.
fn apply_mask(editor: &mut Editor) -> Result<String, String> {
    let (w, h) = canvas_of(editor)?;
    let command = {
        let doc = editor.active_mut().ok_or("No document is open")?;
        let id = doc.document.active_layer().ok_or("Select a layer first")?;
        // W9-G: a vector-only mask has no pixel coverage to bake — reading
        // its absent tiles as "hidden" would erase the layer.
        let has_mask = doc
            .document
            .layers
            .get(id)
            .and_then(|l| l.mask.as_ref())
            .is_some_and(|m| !is_vector_only(&doc.document, m));
        if !has_mask {
            return Err("The layer has no mask".to_string());
        }
        let before = pixels::read_layer(doc, id);
        let mut coverage = read_mask_coverage(doc, id, w, h);
        // Card 059 (review round 2): Apply bakes what the SCREEN shows, and
        // the compositor applies the mask's INVERTED flag on top of the
        // store — an inverted mask must flatten its complement, or the
        // result contradicts the view. (Density/feather remain recorded
        // limitations: the store's coverage is what the pipeline bakes.)
        //
        // RECORDED LIMITATION (review round 3, ledger row 059 / T043
        // remainder): on a TRANSFORMED layer the bake mixes spaces —
        // read_layer/write_layer lay tiles 1:1 onto canvas indices while the
        // coverage read is pose-aware — so the exact bake for store
        // pixel s is store(s) · cov(M⁻¹·s) (cov sampled through the
        // mask's own extra-transform inverse), and this code computes
        // store(s) · cov((T·M)⁻¹·s). Equal only at an identity layer
        // transform; a moved layer's Apply can blank a stripe at its edge.
        // Fixing it needs a store-space coverage read — recorded as work,
        // not silently shipped as correct.
        if doc
            .document
            .layers
            .get(id)
            .and_then(|l| l.mask.as_ref())
            .is_some_and(|m| m.inverted)
        {
            coverage.iter_mut().for_each(|v| *v = 255 - *v);
        }
        let mut after = before.clone();
        for (i, c) in coverage.iter().enumerate() {
            let a = i * 4 + 3;
            after[a] = ((after[a] as u32 * *c as u32) / 255) as u8;
        }
        // W9-G: Apply bakes the PIXEL mask only. A vector mask riding on it
        // stays, as a vector-only mask with the same pose (a fresh id, so it
        // carries no coverage tiles).
        let remaining = doc
            .document
            .layers
            .get(id)
            .and_then(|l| l.mask.as_ref())
            .and_then(|m| {
                let v = m.vector.clone()?;
                let mut left = layer_model::LayerMask::vector_only(layer_model::MaskId::new(), *v);
                left.linked = m.linked;
                left.transform = m.transform.clone();
                Some(left)
            });
        let mut commands = vec![pixels::write_layer(doc, id, &after, "Apply Mask")?];
        commands.push(Command::SetLayerProperties {
            layer_id: id,
            patch: editor_core::LayerPatch {
                mask: match remaining {
                    Some(m) => editor_core::Patch::Set(m),
                    None => editor_core::Patch::Clear,
                },
                ..Default::default()
            },
        });
        Command::Transaction {
            label: "Apply Mask".to_string(),
            commands,
        }
    };
    editor.apply_command(command);
    Ok("Mask applied".to_string())
}

/// Card 060: bake the Refine Mask dialog's parameters into the ACTIVE
/// layer's mask coverage — one labelled transaction (one undo step), the
/// layer's pixels never touched. The parameters run in the mask coverage's
/// own pixels through [`selection::refine_mask`], the same pipeline the
/// dialog's preview uses, so what was previewed is what lands.
pub(crate) fn refine_mask_with(
    editor: &mut Editor,
    spec: &ui::dialogs::refine_mask::RefineMaskSpec,
) -> Result<String, String> {
    let layer = pixel_layer(editor)?;
    let (w, h) = canvas_of(editor)?;
    let command = {
        let doc = editor.active_mut().ok_or("No document is open")?;
        doc.document
            .layers
            .get(layer)
            .and_then(|l| l.mask_id())
            .ok_or_else(|| "The layer has no mask".to_string())?;
        let baseline = read_mask_coverage(doc, layer, w, h);
        let mask = editor_core::SelectionMask::new(glam::IVec2::ZERO, w, h, baseline)
            .map_err(|e| e.to_string())?;
        let refined = selection::refine_mask(&mask, &spec.params()).map_err(|e| e.to_string())?;
        // Canvas-align the refined field (its rect may have grown).
        let mut coverage = vec![0u8; (w * h) as usize];
        for y in 0..h {
            for x in 0..w {
                coverage[(y * w + x) as usize] =
                    refined.coverage_at(glam::IVec2::new(x as i32, y as i32));
            }
        }
        pixels::write_mask_coverage(doc, layer, &coverage, "Refine Mask")?
    };
    editor.apply_command(command);
    Ok(format!(
        "Refined the mask: feather {} px, shift {} px, smooth {} px, contrast {:.0}%",
        spec.feather_px,
        spec.shift_px,
        spec.smooth_px,
        spec.contrast * 100.0
    ))
}

/// Card 062: run the Remove Color Fringe dialog's cleanup over the ACTIVE
/// layer's pixels — a SEPARATE edit from [`refine_mask_with`]: it recolours
/// the boundary band toward the nearby interior ink and never touches the
/// mask coverage. One labelled transaction (one undo step); the inverse
/// restores the exact original RGB, which is how "disabling" the cleanup
/// works — the operation is explicit and reversible, not a mode. Applies to
/// the layer's whole masked boundary (not folded by the selection: the
/// fringe lives where the MASK says the edge is). The mixed-space caveat of
/// the card-060 family applies on transformed layers (raw-store read,
/// canvas-aligned write).
pub(crate) fn defringe_with(
    editor: &mut Editor,
    spec: &ui::dialogs::defringe::DefringeSpec,
) -> Result<String, String> {
    let layer = pixel_layer(editor)?;
    let (w, h) = canvas_of(editor)?;
    let command = {
        let doc = editor.active_mut().ok_or("No document is open")?;
        doc.document
            .layers
            .get(layer)
            .and_then(|l| l.mask.as_ref())
            .ok_or_else(|| "The layer has no mask".to_string())?;
        let before = pixels::read_layer(doc, layer);
        let coverage = read_mask_coverage(doc, layer, w, h);
        let mut after = before.clone();
        filters::defringe::defringe(&mut after, &coverage, w, h, spec.params())
            .map_err(|e| e.to_string())?;
        if after == before {
            return Err("No boundary fringe matched the cleanup parameters".to_string());
        }
        pixels::write_layer(doc, layer, &after, "Remove Color Fringe")?
    };
    editor.apply_command(command);
    Ok(format!(
        "Removed color fringe: radius {} px, strength {:.0}%",
        spec.radius_px,
        spec.strength * 100.0
    ))
}

/// Card 058: the mask's document pose — the layer transform composed
/// with the mask's own extra transform (card 043), the SAME pose the
/// compositor samples the coverage through (`composite.rs`'s mask
/// sampling). Identity for a linked mask on an untransformed layer.
pub(crate) fn mask_pose(doc: &crate::doc::OpenDocument, layer: LayerId) -> glam::Affine2 {
    crate::interaction_geometry::document_transform_of(&doc.document, layer, 0)
        .unwrap_or(glam::Affine2::IDENTITY)
        * doc
            .document
            .layers
            .get(layer)
            .and_then(|l| l.mask.as_ref())
            .map(|m| *m.transform)
            .unwrap_or(glam::Affine2::IDENTITY)
}

/// A pixel-center-consistent mapping pair for the mask store:
/// store pixel `m` covers the document area around `pose * (m + 0.5)`,
/// so canvas pixel `p`'s store pixel is
/// `floor(pose.inverse() * (p + 0.5) − 0.5)` and the canvas pixel a
/// store pixel writes is `floor(pose * (m + 0.5) − 0.5)`. At an
/// identity pose both reduce to the plain integer coordinates.
fn mask_to_canvas(pose: glam::Affine2, m: glam::Vec2) -> (i64, i64) {
    let d = pose.transform_point2(m + glam::Vec2::splat(0.5)) - glam::Vec2::splat(0.5);
    (d.x.floor() as i64, d.y.floor() as i64)
}
pub(crate) fn canvas_to_mask(pose_inverse: glam::Affine2, p: glam::Vec2) -> (i32, i32) {
    let m = pose_inverse.transform_point2(p + glam::Vec2::splat(0.5)) - glam::Vec2::splat(0.5);
    (m.x.floor() as i32, m.y.floor() as i32)
}

/// One coverage byte per canvas pixel, read out of a layer's mask tiles
/// through the mask pose.
///
/// An absent mask tile is *hidden* — the table in [`editor_core::pixels`] says
/// so — which is why the buffer starts at zero rather than at 255.
pub(crate) fn read_mask_coverage(
    doc: &crate::doc::OpenDocument,
    layer: LayerId,
    w: u32,
    h: u32,
) -> Vec<u8> {
    let (w, h) = (w as usize, h as usize);
    let mut out = vec![0u8; w * h];
    let Some(map) = doc.document.mask_tiles(layer) else {
        return out;
    };
    let ts = raster::TILE_SIZE as i32;
    // Card 058: canvas pixels read through the mask pose — the SAME
    // mapping the compositor samples the coverage through — so a read
    // (and the fill/invert built on it) agrees with what the user sees on an
    // unlinked, independently transformed mask. Nearest-store-pixel sampling;
    // at an identity pose this is the exact 1:1 copy it always was.
    let pose_inverse = mask_pose(doc, layer).inverse();
    for y in 0..h {
        for x in 0..w {
            let (mx, my) = crate::menu_bridge::canvas_to_mask(
                pose_inverse,
                glam::Vec2::new(x as f32, y as f32),
            );
            let coord = raster::TileCoord::new(mx.div_euclid(ts), my.div_euclid(ts), 0);
            let Some(hash) = map.get(coord) else {
                continue; // absent tile = hidden
            };
            let Some(bytes) = compositor::TileSource::tile(&doc.tiles, hash) else {
                continue;
            };
            if bytes.len() < (ts * ts) as usize {
                continue;
            }
            let px = my.rem_euclid(ts) as usize * ts as usize + mx.rem_euclid(ts) as usize;
            out[y * w + x] = bytes[px];
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialogs::{CloseChoice, FileDialogs, RecordingUrls, ScriptedDialogs, UrlLauncher};
    use crate::prefs::AppPaths;
    use crate::recent::RecentFiles;

    /// A launcher the test can read after the editor used it: the recorded
    /// URLs live behind an `Rc`, so the test keeps its own handle while the
    /// editor owns the box.
    #[derive(Clone, Default)]
    struct SharedRecorder(std::rc::Rc<std::cell::RefCell<Vec<String>>>);

    impl UrlLauncher for SharedRecorder {
        fn open_url(&mut self, url: &str) -> bool {
            self.0.borrow_mut().push(url.to_string());
            true
        }
    }

    /// C5: the Help menu's browser launches ride the injected seam, so a
    /// test can assert exactly what the shipped `BrowserUrls` would be asked
    /// to open — and nothing ever opens a browser tab on a CI runner.
    #[test]
    fn the_help_menu_opens_the_recorded_urls_through_the_injected_seam() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(&dir.path().join("config"));
        let recorder = SharedRecorder::default();
        ed.set_url_launcher(Box::new(recorder.clone()));

        let status = perform(MenuAction::Help, &mut ed).unwrap();
        assert_eq!(
            status,
            "Opened https://github.com/RealDealCPA-VR/Raster-studio/wiki"
        );
        let status = perform(MenuAction::ReleaseNotes, &mut ed).unwrap();
        assert_eq!(
            status,
            "Opened https://github.com/RealDealCPA-VR/Raster-studio/releases"
        );
        let status = perform(MenuAction::ReportIssue, &mut ed).unwrap();
        assert_eq!(
            status,
            "Opened https://github.com/RealDealCPA-VR/Raster-studio/issues/new"
        );

        assert_eq!(
            recorder.0.borrow().as_slice(),
            [
                "https://github.com/RealDealCPA-VR/Raster-studio/wiki",
                "https://github.com/RealDealCPA-VR/Raster-studio/releases",
                "https://github.com/RealDealCPA-VR/Raster-studio/issues/new",
            ]
        );
        // The shipped type still compiles to the real browser call.
        let _ = crate::dialogs::BrowserUrls;
        let _ = RecordingUrls::default();
    }

    fn editor(dir: &std::path::Path) -> Editor {
        with_recent(dir, RecentFiles::new())
    }

    /// W9-F: Layer > Combine Shapes > Unite over two overlapping rectangle
    /// layers — the second moved by its layer transform — leaves one shape
    /// layer whose path covers their union, in the bottom layer's own space;
    /// one Undo brings both layers back. The menu resolves it only with two
    /// or more layers selected.
    #[test]
    fn combine_shapes_unite_merges_two_overlapping_rectangles_into_their_union() {
        use layer_model::{Layer, LayerKind, ShapeLayer};
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor_with_a_document(dir.path());
        let bottom = Layer::with_kind(
            "A",
            LayerKind::Shape(ShapeLayer::from_svg("M10 10 L30 10 L30 30 L10 30 Z")),
        );
        let mut top = Layer::with_kind(
            "B",
            LayerKind::Shape(ShapeLayer::from_svg("M0 0 L20 0 L20 20 L0 20 Z")),
        );
        // B's path sits at 20..40 in the document.
        top.transform = glam::Affine2::from_translation(glam::vec2(20.0, 20.0));
        let (a, b) = (bottom.id, top.id);
        ed.apply_command(Command::create_layer(bottom));
        ed.apply_command(Command::create_layer(top));
        ed.set_layer_selection(vec![a], Some(a));
        let one = context(&mut ed, &Workspace::new());
        assert!(matches!(
            MenuAction::CombineShapes(ui::menu::ShapeCombine::Unite).resolve(&one),
            ui::menu::Resolution::Disabled(_)
        ));
        ed.set_layer_selection(vec![a, b], Some(b));
        let two = context(&mut ed, &Workspace::new());
        assert!(
            !matches!(
                MenuAction::CombineShapes(ui::menu::ShapeCombine::Unite).resolve(&two),
                ui::menu::Resolution::Disabled(_)
            ),
            "two shape layers selected: the item is live"
        );
        assert!(ui::menu::menu_bar(0)
            .iter()
            .any(|m| m.title == "Layer"
                && format!("{:?}", m.entries).contains("CombineShapes(Unite)")));

        let status = perform(
            MenuAction::CombineShapes(ui::menu::ShapeCombine::Unite),
            &mut ed,
        )
        .expect("combined");
        assert!(status.contains("2 shape layers"), "{status}");
        let doc = &ed.active().unwrap().document;
        assert!(!doc.layers.contains(b), "the upper shape was merged away");
        let LayerKind::Shape(merged) = &doc.layers.get(a).unwrap().kind else {
            panic!("the bottom layer is no longer a shape");
        };
        let path = vector::parse_svg(&merged.path_svg).unwrap();
        let area = vector::fill(&path, &vector::FillOptions::default())
            .unwrap()
            .area();
        // 20x20 + 20x20 - the 10x10 overlap.
        assert_eq!(area, 700.0);
        for p in [(12.0, 12.0), (38.0, 38.0), (25.0, 25.0)] {
            assert!(
                vector::contains(&path, vector::point(p.0, p.1), vector::FillRule::NonZero),
                "{p:?} is not covered"
            );
        }
        assert!(!vector::contains(
            &path,
            vector::point(38.0, 12.0),
            vector::FillRule::NonZero
        ));

        ed.dispatch(Action::Undo).expect("undo");
        let doc = &ed.active().unwrap().document;
        assert!(
            doc.layers.contains(a) && doc.layers.contains(b),
            "undo restores both"
        );
    }

    /// Card 007: the Properties Layer/Mask control's intent lands as the
    /// shell's edit-target pick, and applying it stores the validated target.
    #[test]
    fn the_properties_mask_focus_becomes_the_shell_edit_target() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("canvas.png");
        std::fs::write(
            &png,
            raster::encode(raster::ExportFormat::Png, 32, 32, &[255u8; 32 * 32 * 4]).unwrap(),
        )
        .unwrap();
        let mut ed = editor(dir.path());
        ed.open_path(&png).unwrap();

        // The intent the control raises (docks.rs), through the real bridge:
        // routed, recorded into the frame's output, applied by the shell.
        let intent = ui::Intent::SetEditTarget { mask: true };
        let pick = crate::menu_bridge::pick(&intent, &ed).expect("the intent routes");
        let mut out = crate::chrome::ChromeOutput::default();
        crate::menu_bridge::record(pick, &mut out);
        assert_eq!(
            out.edit_target,
            Some(crate::edit_target::EditTargetKind::Mask),
            "the intent became an edit-target pick"
        );

        // The shell's half (`shell.rs::apply_chrome`).
        if let Some(kind) = out.edit_target {
            ed.set_edit_target_kind(kind);
        }
        // The opened layer has no mask yet: the snapshot falls back to
        // content, the documented reconciliation.
        let target = ed.edit_target().expect("a target for the active layer");
        assert_eq!(target.kind, crate::edit_target::EditTargetKind::Content);
    }

    fn with_recent(dir: &std::path::Path, recent: RecentFiles) -> Editor {
        Editor::with_state(
            AppPaths::rooted(dir.join("config")),
            Preferences::default(),
            recent,
            Box::new(ScriptedDialogs::new()),
        )
    }

    #[test]
    fn the_menu_bar_the_shell_draws_is_the_ui_crates() {
        // `draw` builds its entries from `menus`, and `menus` is
        // `ui::menu::menu_bar` and nothing else. If somebody grows a second
        // menu here, this stops being true.
        let dir = tempfile::tempdir().unwrap();
        let ed = editor(dir.path());
        let mine = menus(&ed);
        let theirs = ui::menu::menu_bar(ed.recent().entries().len());
        let titles: Vec<&str> = mine.iter().map(|m| m.title).collect();
        assert_eq!(titles, theirs.iter().map(|m| m.title).collect::<Vec<_>>());
        assert_eq!(
            titles,
            vec!["File", "Edit", "Image", "Layer", "Select", "Filter", "View", "Window", "Help"]
        );
        for (a, b) in mine.iter().zip(&theirs) {
            assert_eq!(a.actions(), b.actions(), "{} diverged", a.title);
        }
    }

    /// Every string one drawn frame put on screen.
    ///
    /// `FullOutput::shapes` is pre-tessellation, so a text shape still carries
    /// its galley and its galley still knows its own text. That is what lets a
    /// headless test read what the window says.
    fn painted_text(ctx: &egui::Context, output: &egui::FullOutput) -> Vec<String> {
        let _ = ctx;
        output
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Text(text) => Some(text.galley.text().to_string()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_real_app_surface_draws_the_ui_crates_nine_menus() {
        // Not "the bridge would return them" but "the window says them": one
        // frame of the actual `Chrome::ui`, read back off the paint list.
        let dir = tempfile::tempdir().unwrap();
        let ed = editor(dir.path());
        let ctx = egui::Context::default();
        crate::chrome::install_theme(&ctx, design::Theme::Dark);
        let mut chrome = crate::chrome::Chrome::new();

        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1400.0, 900.0),
            )),
            ..Default::default()
        };
        let mut painted = Vec::new();
        let mut ed = ed;
        for _ in 0..2 {
            let output = ctx.run(input.clone(), |ctx| {
                chrome.ui(ctx, &mut ed);
            });
            painted = painted_text(&ctx, &output);
        }

        for title in ui::menu::menu_bar(0).iter().map(|m| m.title) {
            assert!(
                painted.iter().any(|t| t == title),
                "the window never drew the {title} menu; it drew {painted:?}"
            );
        }
        // "Tools" was a title of the old, parallel menu bar. Its absence is
        // what says the duplicate is gone rather than merely unused.
        assert!(
            !painted.iter().any(|t| t == "Tools"),
            "a second menu bar is still being drawn"
        );
    }

    /// How every item in every menu resolved, in one document state.
    #[derive(Default)]
    struct Tally {
        /// Items the shell can perform right now.
        performable: Vec<MenuAction>,
        /// Items the *shared model* turned off, with the reason it gave.
        disabled: Vec<(MenuAction, String)>,
        /// Items the model allows and this build has no answer for.
        unwired: Vec<MenuAction>,
    }

    impl Tally {
        fn total(&self) -> usize {
            self.performable.len() + self.disabled.len() + self.unwired.len()
        }

        /// The unwired items, one per line, for a failure message that names
        /// what is dead rather than only counting it.
        fn unwired_list(&self) -> String {
            self.unwired
                .iter()
                .map(|a| format!("  {a:?}"))
                .collect::<Vec<_>>()
                .join("\n")
        }
    }

    /// Walk all nine menus and sort every item into the three outcomes.
    ///
    /// "Unwired" is measured against [`unavailable_reason`], not against the
    /// text of the refusal. Giving every dead item a nicely worded sentence
    /// would otherwise move it out of the `unwired` bucket and into `disabled`
    /// — the ratchet would fall to zero and nothing would have been wired at
    /// all. The bucket an item lands in is decided by *who* refused it: the
    /// shared model (disabled) or this shell (unwired).
    fn tally(ed: &mut Editor, ws: &Workspace) -> Tally {
        let context = context(ed, ws);
        let mut tally = Tally::default();
        for menu in menus(ed) {
            for action in menu.actions() {
                match resolve(action, &context, ed) {
                    Ok(_) => tally.performable.push(action),
                    Err(reason)
                        if reason == NOT_WIRED
                            || unavailable_reason(action) == Some(reason.as_str()) =>
                    {
                        tally.unwired.push(action)
                    }
                    Err(reason) => tally.disabled.push((action, reason)),
                }
            }
        }
        tally
    }

    /// A document open, one layer, nothing else special: the state a user is in
    /// for almost the whole session, and therefore the state the menu contract
    /// has to be measured in.
    fn editor_with_a_document(dir: &std::path::Path) -> Editor {
        let mut ed = editor(dir);
        ed.dispatch(Action::NewDocument).expect("a new document");
        ed
    }

    // The ratchet, and the honest measurement of where this build stands.
    //
    // All nine menus carry 256 items. With one document open, 79 of them are
    // performable, 51 are legitimately disabled by the shared model, and 126
    // still answer `NOT_WIRED` — every Filter, every Adjustment, Image Size,
    // Canvas Size, every Transform and Select All, none of which has an
    // `editor_core` command behind it yet. Before the shell hosted
    // `ui::Workspace` the split was 41 / 51 / 164: the thirty-six items that
    // moved are all four workspace presets, all thirteen panels, all thirteen
    // view overlays and the ruler units, which had nowhere to act.
    //
    // The two that moved most recently are Fill Screen and Print Size, and
    // their siblings Zoom to Selection and Reset View Rotation moved with them.
    // Both of those are counted under `disabled` here, for the same kind of
    // reason: Zoom to Selection is disabled because *this* state has nothing
    // selected, and a selection enables it; Reset View Rotation is disabled
    // because *this* state's view is upright, and turning the document camera
    // enables it (`reset_view_rotation_is_enabled_on_a_turned_view_and_uprights_
    // the_document_camera`). All four were implemented in
    // `ui::Workspace::absorb_action` and unreachable, because the bridge routed
    // no `Intent::Action` to the workspace at all.
    //
    // The floors may only rise and the caps may only fall. A new menu item
    // nobody wired pushes the cap over and the failure lists it by name.
    const MAX_UNWIRED_WITH_A_DOCUMENT: usize = 45;
    const MIN_PERFORMABLE_WITH_A_DOCUMENT: usize = 159;
    const MAX_UNWIRED_WITH_NOTHING_OPEN: usize = 0;
    const MIN_PERFORMABLE_WITH_NOTHING_OPEN: usize = 30;

    #[test]
    fn every_ui_menu_item_is_either_performable_or_disabled_with_a_reason() {
        // The contract this module's doc names, measured rather than asserted:
        // every item in every menu lands in exactly one of three buckets, a
        // disabled item always says why, and the size of the dead bucket is
        // pinned so it can only shrink.
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new();

        for (label, mut ed, min_performable, max_unwired) in [
            (
                "with a document open",
                editor_with_a_document(dir.path()),
                MIN_PERFORMABLE_WITH_A_DOCUMENT,
                MAX_UNWIRED_WITH_A_DOCUMENT,
            ),
            (
                "with nothing open",
                editor(dir.path()),
                MIN_PERFORMABLE_WITH_NOTHING_OPEN,
                MAX_UNWIRED_WITH_NOTHING_OPEN,
            ),
        ] {
            let t = tally(&mut ed, &ws);
            assert!(t.total() > 200, "{label}: only {} items walked", t.total());
            for (action, reason) in &t.disabled {
                assert!(!reason.is_empty(), "{label}: {action:?} greys out silently");
            }
            assert!(
                t.performable.len() >= min_performable,
                "{label}: only {} of {} items are performable, down from {min_performable}. \
                 Something the bridge used to route stopped resolving.",
                t.performable.len(),
                t.total()
            );
            assert!(
                t.unwired.len() <= max_unwired,
                "{label}: {} of {} items answer “{NOT_WIRED}”, up from {max_unwired}. \
                 The dead ones are:\n{}",
                t.unwired.len(),
                t.total(),
                t.unwired_list()
            );
        }
    }

    #[test]
    fn the_window_and_view_menus_are_wired_through_the_workspace() {
        // The named half of the count above. Every one of these used to answer
        // `NOT_WIRED`, because the bridge's `pick` ended in `_ => None` and the
        // shell had no `ui::Workspace` for them to act on. They are the items a
        // reviewer measured as dead: all four workspace presets, all thirteen
        // panels, and every view overlay.
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor_with_a_document(dir.path());
        let ws = Workspace::new();
        let context = context(&mut ed, &ws);

        for layout in ui::LayoutId::ALL {
            match resolve(MenuAction::ApplyLayout(*layout), &context, &ed) {
                Ok(Pick::Workspace(intent)) => {
                    assert_eq!(*intent, Intent::ApplyLayout(*layout))
                }
                other => panic!("{layout:?} resolved to {other:?}"),
            }
        }
        for panel in ui::PanelId::ALL {
            match resolve(MenuAction::TogglePanel(*panel), &context, &ed) {
                Ok(Pick::Workspace(intent)) => assert_eq!(
                    *intent,
                    Intent::SetPanelOpen {
                        panel: *panel,
                        open: !context.dock.is_open(*panel),
                    }
                ),
                other => panic!("{panel:?} resolved to {other:?}"),
            }
        }
        for flag in ui::ViewFlag::ALL {
            let outcome = resolve(MenuAction::ToggleView(*flag), &context, &ed);
            // W3-A: the toggles this build cannot honour are greyed with
            // their reason instead of routed.
            if let Some(reason) = crate::chrome::view_flag_refusal(*flag) {
                assert!(
                    matches!(&outcome, Err(r) if r == reason),
                    "{flag:?} resolved to {outcome:?}"
                );
                continue;
            }
            assert!(
                matches!(outcome, Ok(Pick::Workspace(_))),
                "{flag:?} resolved to {outcome:?}"
            );
        }
    }

    #[test]
    fn absorbing_what_the_window_menu_picks_really_moves_the_dock() {
        // A pick that nothing performs is the defect this whole file exists to
        // stop, so the round trip is the assertion: resolve the menu item,
        // absorb what it produced, and read the dock back.
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor_with_a_document(dir.path());
        let mut ws = Workspace::new();
        // Actions is the one panel the default (Essentials) layout leaves
        // closed — W2-D opened Channels and Paths with Layers — so it is the
        // one whose opening proves something.
        let panel = ui::PanelId::Actions;
        assert!(!ws.dock.is_open(panel), "not open yet");

        let context = context(&mut ed, &ws);
        let Ok(Pick::Workspace(intent)) = resolve(MenuAction::TogglePanel(panel), &context, &ed)
        else {
            panic!("Window ▸ Actions is not wired");
        };
        assert!(ws.absorb(&intent), "absorbing it changed nothing");
        assert!(ws.dock.is_open(panel));

        // ...and the menu now shows the checkmark, because the context is read
        // off the same workspace rather than off a fresh default.
        let after = self::context(&mut ed, &ws);
        assert_eq!(MenuAction::TogglePanel(panel).checked(&after), Some(true));
    }

    #[test]
    fn the_file_menu_routes_the_actions_this_build_has() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        let context = context(&mut ed, &Workspace::new());
        assert_eq!(
            resolve(MenuAction::NewDocument, &context, &ed),
            Ok(Pick::Action(Action::NewDocument))
        );
        assert_eq!(
            resolve(MenuAction::Open, &context, &ed),
            Ok(Pick::Action(Action::Open))
        );
        assert_eq!(
            resolve(MenuAction::Preferences, &context, &ed),
            Ok(Pick::Action(Action::ShowPreferences))
        );
        // Nothing is open, so Save is off — with the shared model's reason,
        // not one invented here.
        assert_eq!(
            resolve(MenuAction::Save, &context, &ed),
            Err("No document is open".to_string())
        );
    }

    #[test]
    fn an_item_this_build_cannot_perform_is_disabled_rather_than_dead() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        ed.dispatch(Action::NewDocument).expect("a new document");
        let context = context(&mut ed, &Workspace::new());
        // Place routes and performs now (P2.4): the item resolves to a real
        // command — the dialog asks where the file lives (C7 removed the
        // stale reason that read as a live product limitation).
        assert!(
            resolve(MenuAction::PlaceEmbedded, &context, &ed).is_ok(),
            "Place Embedded… should be performable now"
        );
        // File Info… now opens the metadata window, so it is genuinely
        // performable rather than refused.
        assert!(
            resolve(MenuAction::FileInfo, &context, &ed).is_ok(),
            "File Info… should be performable now"
        );
        // ...and Print… is a real command too (it renders the composite to a
        // print-ready PDF), not a disabled orphan.
        assert!(
            resolve(MenuAction::Print, &context, &ed).is_ok(),
            "Print… should be performable now"
        );
        // ...and one it *can* do resolves to a real command rather than a name.
        match resolve(MenuAction::NewLayer, &context, &ed) {
            Ok(Pick::Command(Command::CreateLayer { .. })) => {}
            other => panic!("New Layer resolved to {other:?}"),
        }
    }

    #[test]
    fn the_three_view_items_the_workspace_performs_are_routed_to_it() {
        // Fill Screen, Zoom to Selection and Print Size are implemented by
        // `ui::Workspace::absorb_action` and were all greyed out with
        // `NOT_WIRED`, sitting beside four zoom items that
        // worked. The cause was structural: `shell_action` mapped a
        // `MenuAction` to a shell `Action` or to nothing, so an action whose
        // whole implementation lives in the workspace had no way through.
        use ui::menu::ZoomCommand as Z;
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor_with_a_document(dir.path());
        // Zoom to Selection is (correctly) disabled with nothing selected, so
        // the state this is measured in has a selection.
        ed.active_mut().unwrap().document.selection = editor_core::Selection::Rect {
            min: glam::IVec2::new(2, 2),
            max: glam::IVec2::new(20, 20),
        };
        // ...and Reset View Rotation is gated on a rotated view — the DOCUMENT
        // camera's rotation, the one the shell renders from. The workspace
        // canvas camera is deliberately left upright here: it is not the
        // camera the item reads or resets, and turning it must not enable
        // anything.
        let ws = Workspace::new();
        ed.active_mut().unwrap().camera.set_rotation(0.7);
        let context = context(&mut ed, &ws);

        for action in [
            MenuAction::Zoom(Z::FillScreen),
            MenuAction::Zoom(Z::ToSelection),
            MenuAction::Zoom(Z::PrintSize),
        ] {
            match resolve(action, &context, &ed) {
                Ok(Pick::Workspace(intent)) => assert_eq!(*intent, Intent::Action(action)),
                other => panic!(
                    "{action:?} resolved to {other:?}; it must reach the workspace that \
                     already implements it"
                ),
            }
        }
        // Reset View Rotation is performed against the document camera, not
        // routed to the workspace.
        match resolve(MenuAction::ResetViewRotation, &context, &ed) {
            Ok(Pick::Menu(MenuAction::ResetViewRotation)) => {}
            other => panic!(
                "ResetViewRotation resolved to {other:?}; it must reach `perform`, which \
                 uprights the document camera"
            ),
        }
    }

    /// View ▸ Reset View Rotation, end to end on the camera the shell renders
    /// from: greyed out with the reason while the view is upright, enabled the
    /// moment the document camera is turned, performed through `perform` so
    /// the camera is upright again, and greyed out once more afterwards.
    #[test]
    fn reset_view_rotation_is_enabled_on_a_turned_view_and_uprights_the_document_camera() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor_with_a_document(dir.path());
        let ws = Workspace::new();

        let upright = context(&mut ed, &ws);
        assert_eq!(
            resolve(MenuAction::ResetViewRotation, &upright, &ed),
            Err("The view is already upright".to_string()),
            "an upright view must grey the item out with the reason"
        );
        // Bypassing enablement is refused with the same sentence.
        assert_eq!(
            perform(MenuAction::ResetViewRotation, &mut ed),
            Err("The view is already upright".to_string())
        );

        ed.active_mut().unwrap().camera.set_rotation(0.7);
        let turned = context(&mut ed, &ws);
        assert!(
            turned.view_rotated,
            "the context did not read the document camera"
        );
        let pick = resolve(MenuAction::ResetViewRotation, &turned, &ed)
            .expect("a turned view enables Reset View Rotation");
        assert_eq!(pick, Pick::Menu(MenuAction::ResetViewRotation));

        assert_eq!(
            perform(MenuAction::ResetViewRotation, &mut ed),
            Ok("View rotation reset".to_string())
        );
        assert_eq!(
            ed.active().unwrap().camera.rotation,
            0.0,
            "Reset View Rotation left the document camera turned"
        );
        assert_eq!(ed.status(), Some("View rotation reset"));

        let after = context(&mut ed, &ws);
        assert!(!after.view_rotated);
        assert!(
            resolve(MenuAction::ResetViewRotation, &after, &ed).is_err(),
            "the item stayed enabled on an upright view"
        );
    }

    #[test]
    fn an_adjustments_parameters_reach_a_real_document_edit() {
        // `Intent::EditLayerKind` is the only channel an adjustment layer's
        // parameters or a text layer's content can change through, and `pick`
        // used to answer it with `None`. Every slider in the Properties panel
        // was therefore inert, and an adjustment created at identity — Curves,
        // Levels — could never be made to do anything at all.
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor_with_a_document(dir.path());
        let layer = layer_model::Layer::with_kind(
            "Posterize",
            layer_model::LayerKind::Adjustment(layer_model::AdjustmentLayer {
                kind: layer_model::AdjustmentKind::Posterize { levels: 8 },
            }),
        );
        let id = layer.id;
        ed.apply_command(Command::create_layer(layer));

        let next = layer_model::LayerKind::Adjustment(layer_model::AdjustmentLayer {
            kind: layer_model::AdjustmentKind::Posterize { levels: 3 },
        });
        match pick(
            &Intent::EditLayerKind {
                layer: id,
                kind: Box::new(next.clone()),
            },
            &ed,
        ) {
            Some(Pick::Kind { layer, kind }) => {
                assert_eq!(layer, id);
                assert_eq!(*kind, next);
            }
            other => panic!("an adjustment's parameters resolved to {other:?}"),
        }
    }

    #[test]
    fn an_intent_with_no_answer_says_what_it_was() {
        // The reporting half of the contract this module's docs claim. A
        // dropped intent used to be indistinguishable from a performed one.
        let message = unrouted_message(&Intent::Action(MenuAction::FileInfo));
        assert!(
            message.contains(unavailable_reason(MenuAction::FileInfo).unwrap()),
            "{message}"
        );
        assert!(
            message.contains(&MenuAction::FileInfo.label()),
            "the message must name the item: {message}"
        );
        // An intent the bridge answers has no message to give, so the fallback
        // is what an *unknown* one gets — and it is still not silence.
        let message = unrouted_message(&Intent::SetZoom(2.0));
        assert!(message.contains(NOT_WIRED), "{message}");
    }

    #[test]
    fn the_recent_submenu_labels_and_opens_real_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("seaside.png");
        let mut recent = RecentFiles::new();
        recent.record(path.clone());
        let mut ed = with_recent(dir.path(), recent);
        let context = context(&mut ed, &Workspace::new());
        assert_eq!(MenuAction::OpenRecent(0).label_in(&context), "seaside.png");
        assert_eq!(
            resolve(MenuAction::OpenRecent(0), &context, &ed),
            Ok(Pick::OpenRecent(path))
        );
        assert_eq!(
            resolve(MenuAction::OpenRecent(1), &context, &ed),
            Err("This slot has no recent file".to_string())
        );
    }

    // -----------------------------------------------------------------------
    // The wired menus
    // -----------------------------------------------------------------------

    /// A small document with real, deliberately non-uniform pixels.
    ///
    /// Non-uniform on purpose: a flat image is a fixed point of half the filter
    /// catalogue, so "the blur changed the pixels" would prove nothing on one.
    fn probe_png(dir: &std::path::Path, w: u32, h: u32) -> std::path::PathBuf {
        let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                rgba[i] = (x * 5 % 251) as u8;
                rgba[i + 1] = (y * 7 % 241) as u8;
                rgba[i + 2] = ((x * 13 + y * 3) % 239) as u8;
                rgba[i + 3] = 255;
            }
        }
        let bytes = raster::encode(raster::ExportFormat::Png, w, h, &rgba).unwrap();
        let path = dir.join(format!("probe-{w}x{h}.png"));
        std::fs::write(&path, bytes).unwrap();
        path
    }

    /// An open document with pixels, two layers, and a selection: the state in
    /// which most of the menu bar is live.
    fn opened(dir: &std::path::Path) -> Editor {
        let mut ed = editor(dir);
        // Card 052: the image clipboard is the deterministic fake — default
        // menu tests must not touch the real OS clipboard (parallel tests
        // would race on it, and headless machines have none). The host-bound
        // paste test re-arms the real clipboard explicitly.
        ed.set_image_clipboard(Box::new(crate::clipboard::FakeClipboard::new()));
        ed.open_path(&probe_png(dir, 48, 32))
            .expect("the probe opens");
        ed
    }

    /// Two layers, both holding pixels, the topmost active.
    ///
    /// Both halves matter. An *empty* second layer would make Clear, the
    /// filters and the adjustments honestly no-ops on it, and the topmost is
    /// active so Merge Down has a layer below it to merge into — which is the
    /// state a user is in whenever they have stacked anything at all.
    fn with_two_layers(dir: &std::path::Path) -> Editor {
        let mut ed = opened(dir);
        let source = ed.active().unwrap().document.active_layer().unwrap();
        let extra = layer_model::Layer::raster("Second");
        let id = extra.id;
        ed.apply_command(Command::create_layer(extra));

        let mut rgba = pixels::read_layer(ed.active().unwrap(), source);
        for (i, byte) in rgba.iter_mut().enumerate() {
            if i % 4 != 3 {
                *byte = byte.wrapping_add(37);
            }
        }
        let paint = {
            let doc = ed.active_mut().unwrap();
            pixels::write_layer(doc, id, &rgba, "Second").unwrap()
        };
        ed.apply_command(paint);

        let top = ed.active().unwrap().document.layers.root()[0];
        ed.set_active_layer(top);
        ed
    }

    fn select_rect(ed: &mut Editor, min: (i32, i32), max: (i32, i32)) {
        ed.active_mut().unwrap().document.selection = editor_core::Selection::Rect {
            min: glam::IVec2::new(min.0, min.1),
            max: glam::IVec2::new(max.0, max.1),
        };
    }

    /// Everything about the active document a menu operation could change.
    ///
    /// The layers' *tile hashes* are in it, so a change to one pixel changes
    /// the digest — which is what makes "this item is not a no-op" a real
    /// assertion rather than a claim about the history depth.
    fn digest(ed: &Editor) -> String {
        // The clipboard is part of it: Copy's whole effect is on the clipboard,
        // so leaving it out would make Copy read as a no-op and this oracle
        // would have to grow an exception instead of an answer.
        let clip = ed.clipboard().map(|c| {
            (
                c.width,
                c.height,
                c.rgba8.iter().map(|b| *b as u64).sum::<u64>(),
            )
        });
        let Some(d) = ed.active() else {
            return format!("no document {clip:?}");
        };
        let mut s = format!("{clip:?} ");
        s += &format!(
            "{} {:?} {:?} sel={:?} tool={:?} {}x{} stored={} saved={} guides={:?}",
            d.history_depth(),
            d.document.selection,
            d.document.active_layer(),
            d.document.layer_selection(),
            ed.effective_tool(),
            d.document.width(),
            d.document.height(),
            d.document.stored_selection.is_some(),
            d.document.saved_selections.len(),
            d.document.guides,
        );
        for id in d.document.layers.iter_depth_first() {
            let layer = d.document.layers.get(id).expect("a listed layer exists");
            // W2-F: the name, the lock flags and the transform are in it too,
            // so Rename, the Lock rows and Align/Distribute (which move a
            // layer without touching a tile) read as the edits they are.
            s.push_str(&format!(
                "|{id:?} {:?} v{} {:?} {:?} {:?} {:?}",
                layer.name, layer.visible, layer.locked, layer.transform, layer.mask, layer.kind
            ));
            if let Some(map) = d.document.layer_tiles(id) {
                let mut tiles: Vec<_> = map.iter().collect();
                tiles.sort_by_key(|(c, _)| (c.level, c.y, c.x));
                s.push_str(&format!("{tiles:?}"));
            }
        }
        s
    }

    /// Drive one menu item the way the shell does and report whether the
    /// document is different afterwards.
    fn invoke(ed: &mut Editor, action: MenuAction) -> Result<bool, String> {
        let before = digest(ed);
        let context = context(ed, &Workspace::new());
        match resolve(action, &context, ed)? {
            Pick::Menu(a) => perform(a, ed)?,
            Pick::Command(c) => {
                ed.apply_command(c);
                String::new()
            }
            other => {
                return Err(format!(
                    "{action:?} resolved to {other:?}, not a document edit"
                ))
            }
        };
        Ok(digest(ed) != before)
    }

    #[test]
    fn a_filter_menu_item_really_filters_the_active_layers_pixels() {
        // The whole Filter menu answered `NOT_WIRED`: `crates/filters` was
        // complete, tested, and unreachable from every user gesture there is.
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let before = digest(&ed);
        let depth = ed.active().unwrap().history_depth();

        assert!(
            invoke(
                &mut ed,
                MenuAction::Filter(ui::menu::FilterId::GaussianBlur)
            )
            .unwrap(),
            "Gaussian Blur left every pixel alone"
        );
        assert_eq!(
            ed.active().unwrap().history_depth(),
            depth + 1,
            "a filter must be exactly one undoable step"
        );
        // ...and one Ctrl+Z puts every pixel back.
        ed.dispatch(Action::Undo).expect("undo");
        assert_eq!(digest(&ed), before, "undoing the filter did not restore it");
    }

    #[test]
    fn a_filter_honours_the_selection_as_a_mask() {
        // The claim the Filter menu makes by being in the same window as the
        // marquee: pixels outside the selection are not touched.
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        let before = pixels::read_layer(ed.active().unwrap(), layer);
        select_rect(&mut ed, (0, 0), (16, 32));

        assert!(invoke(&mut ed, MenuAction::Filter(ui::menu::FilterId::Mosaic)).unwrap());
        let after = pixels::read_layer(ed.active().unwrap(), layer);

        let w = 48usize;
        let inside = (0..32usize)
            .flat_map(|y| (0..16usize).map(move |x| y * w + x))
            .any(|i| after[i * 4..i * 4 + 4] != before[i * 4..i * 4 + 4]);
        let outside_changed = (0..32usize)
            .flat_map(|y| (20..48usize).map(move |x| y * w + x))
            .any(|i| after[i * 4..i * 4 + 4] != before[i * 4..i * 4 + 4]);
        assert!(inside, "the filter did not reach the selected pixels");
        assert!(
            !outside_changed,
            "the filter escaped the selection and changed pixels outside it"
        );
    }

    #[test]
    fn every_filter_in_the_menu_is_reachable_and_changes_the_document() {
        // Not "the crate has a function" — the menu item, resolved and
        // performed, over the real document.
        let dir = tempfile::tempdir().unwrap();
        let mut dead = Vec::new();
        for id in ui::menu::FilterId::ALL {
            // A filter the shell refuses on purpose is not in the menu as a
            // live item; it is greyed out with its reason, and
            // `no_menu_item_falls_back_to_the_generic_refusal` covers that.
            if unavailable_reason(MenuAction::Filter(*id)).is_some() {
                continue;
            }
            // Custom and Offset are the identity at their schema defaults, so
            // a defaults run changes nothing by design. They are hosted now —
            // their rows open the parameter dialog, and
            // `a_confirmed_filter_dialog_runs_at_radius_zero_and_eight`
            // drives the dialog path end to end.
            if matches!(id, ui::menu::FilterId::Custom | ui::menu::FilterId::Offset) {
                continue;
            }
            // W10-C: Camera Raw and Lens Correction are the identity at their
            // defaults too (every slider at zero), as in Photoshop.
            // `w10c_filters_open_from_the_menu_and_land_as_one_step_or_a_smart_filter`
            // drives their dialogs with a moved slider end to end.
            if matches!(
                id,
                ui::menu::FilterId::CameraRaw | ui::menu::FilterId::LensCorrection
            ) {
                continue;
            }
            let mut ed = opened(dir.path());
            match invoke(&mut ed, MenuAction::Filter(*id)) {
                Ok(true) => {}
                Ok(false) => dead.push(format!("{}: changed nothing", id.label())),
                Err(reason) => dead.push(format!("{}: {reason}", id.label())),
            }
        }
        assert!(
            dead.is_empty(),
            "live Filter items that do nothing:\n{dead:#?}"
        );
    }

    #[test]
    fn the_select_menu_moves_the_documents_selection() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());

        assert!(invoke(&mut ed, MenuAction::SelectAll).unwrap());
        assert_eq!(
            ed.active().unwrap().document.selection,
            editor_core::Selection::Rect {
                min: glam::IVec2::ZERO,
                max: glam::IVec2::new(48, 32),
            }
        );

        // Inverse really is the complement, measured on the document.
        select_rect(&mut ed, (0, 0), (24, 32));
        assert!(invoke(&mut ed, MenuAction::InverseSelection).unwrap());
        let (min, max) = ed
            .active()
            .unwrap()
            .document
            .selection
            .bounds()
            .expect("the complement of the left half is the right half");
        assert_eq!(((min.x, min.y), (max.x, max.y)), ((24, 0), (48, 32)));

        assert!(invoke(&mut ed, MenuAction::Deselect).unwrap());
        assert!(ed.active().unwrap().document.selection.is_none());

        // ...and inverting the *whole* canvas selects nothing, which is a
        // different thing from no selection at all — see
        // `editor_core::Selection`'s table.
        assert!(invoke(&mut ed, MenuAction::SelectAll).unwrap());
        assert!(invoke(&mut ed, MenuAction::InverseSelection).unwrap());
        let nothing = &ed.active().unwrap().document.selection;
        assert!(nothing.is_empty(), "{nothing:?}");
        assert!(!nothing.is_none());
    }

    #[test]
    fn select_modify_reshapes_the_selection_by_the_radius_it_names() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        select_rect(&mut ed, (10, 10), (20, 20));

        let message = {
            let context = context(&mut ed, &Workspace::new());
            let Ok(Pick::Menu(a)) = resolve(
                MenuAction::Modify(ui::menu::ModifySelection::Expand),
                &context,
                &ed,
            ) else {
                panic!("Select ▸ Modify ▸ Expand is not wired");
            };
            perform(a, &mut ed).expect("expand")
        };
        assert!(message.contains(&MODIFY_RADIUS.to_string()), "{message}");

        let (min, max) = ed.active().unwrap().document.selection.bounds().unwrap();
        assert_eq!(
            (min.x, min.y),
            (10 - MODIFY_RADIUS as i32, 10 - MODIFY_RADIUS as i32),
            "expand must grow the selection by its radius"
        );
        assert_eq!(
            (max.x, max.y),
            (20 + MODIFY_RADIUS as i32, 20 + MODIFY_RADIUS as i32)
        );

        // ...and Contract by the same radius takes it back.
        let context = context(&mut ed, &Workspace::new());
        let Ok(Pick::Menu(a)) = resolve(
            MenuAction::Modify(ui::menu::ModifySelection::Contract),
            &context,
            &ed,
        ) else {
            panic!("Select ▸ Modify ▸ Contract is not wired");
        };
        perform(a, &mut ed).expect("contract");
        let (min, max) = ed.active().unwrap().document.selection.bounds().unwrap();
        assert_eq!(((min.x, min.y), (max.x, max.y)), ((10, 10), (20, 20)));
    }

    #[test]
    fn edit_clear_and_fill_reach_the_pixels_inside_the_selection() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        select_rect(&mut ed, (0, 0), (8, 8));

        assert!(invoke(&mut ed, MenuAction::ClearPixels).unwrap());
        let cleared = pixels::read_layer(ed.active().unwrap(), layer);
        assert_eq!(cleared[3], 0, "the top-left pixel is not cleared");
        let outside = ((8 * 48) + 40) * 4;
        assert_ne!(cleared[outside + 3], 0, "Clear escaped the selection");

        ed.set_foreground([1.0, 0.0, 0.0, 1.0]);
        assert!(invoke(&mut ed, MenuAction::FillDialog).unwrap());
        let filled = pixels::read_layer(ed.active().unwrap(), layer);
        assert_eq!(&filled[0..4], &[255, 0, 0, 255], "Fill used another colour");
        assert_eq!(
            &filled[outside..outside + 4],
            &cleared[outside..outside + 4],
            "Fill escaped the selection"
        );
    }

    #[test]
    fn copy_and_paste_move_pixels_through_the_editors_clipboard() {
        // Five Edit-menu items in one round trip. Paste was greyed out with
        // "The clipboard is empty" *forever*, because `ui::ClipboardState` was
        // never written by anything — the flag described a store that did not
        // exist.
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        let original = pixels::read_layer(ed.active().unwrap(), layer);
        select_rect(&mut ed, (4, 4), (12, 10));

        // Paste is off until something has been copied, and it says so.
        let context = context(&mut ed, &Workspace::new());
        assert_eq!(
            resolve(MenuAction::Paste, &context, &ed),
            Err("The clipboard is empty".to_string())
        );

        assert!(invoke(&mut ed, MenuAction::Copy).unwrap());
        let clip = ed.clipboard().expect("Copy filled the clipboard").clone();
        assert_eq!((clip.width, clip.height), (8, 6), "Copy took the wrong box");
        let first = ((4 * 48) + 4) * 4;
        assert_eq!(&clip.rgba8[0..4], &original[first..first + 4]);

        // ...and now Paste is live, and lands on a layer of its own.
        let layers_before = ed.active().unwrap().document.layers.len();
        assert!(invoke(&mut ed, MenuAction::Paste).unwrap());
        let doc = ed.active().unwrap();
        assert_eq!(doc.document.layers.len(), layers_before + 1);
        let pasted = doc.document.layers.root()[0];
        assert_eq!(
            &pixels::read_layer(doc, pasted)[0..4],
            &original[first..first + 4],
            "the pasted layer does not hold the copied pixels"
        );

        // Cut copies and then clears, as one visible outcome.
        let mut ed = opened(dir.path());
        select_rect(&mut ed, (0, 0), (8, 8));
        assert!(invoke(&mut ed, MenuAction::Cut).unwrap());
        assert!(ed.clipboard().is_some(), "Cut did not copy");
        let after = pixels::read_layer(ed.active().unwrap(), layer);
        assert_eq!(after[3], 0, "Cut did not clear the selected pixels");
    }

    /// Card 052: Copy also crosses the process boundary, and the OS
    /// clipboard's image is authoritative on paste — a payload ANOTHER
    /// application put there wins over the editor's stale internal copy.
    #[test]
    fn an_external_clipboard_image_wins_over_the_stale_internal_copy() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        select_rect(&mut ed, (4, 4), (12, 10));
        assert!(invoke(&mut ed, MenuAction::Copy).unwrap());

        // Another application replaces the OS clipboard's image entirely.
        let foreign =
            crate::clipboard::ClipboardImage::validate(2, 1, vec![9, 8, 7, 255, 6, 5, 4, 255])
                .unwrap();
        let mut os = crate::clipboard::FakeClipboard::new();
        os.seed(foreign);
        ed.set_image_clipboard(Box::new(os));

        assert!(invoke(&mut ed, MenuAction::Paste).unwrap());
        let open = ed.active().unwrap();
        let pasted = open.document.active_layer().unwrap();
        // The external payload routes through full-source placement: a smart
        // object over an embedded asset, not the at-origin raster the
        // internal route makes.
        assert!(
            matches!(
                open.document.layers.get(pasted).unwrap().kind,
                layer_model::LayerKind::SmartObject(_)
            ),
            "an external image pastes as a placed smart object"
        );
        use compositor::TileSource;
        let hash = open
            .document
            .layer_tiles(pasted)
            .unwrap()
            .get(raster::TileCoord::new(0, 0, 0))
            .expect("the placed source's first tile");
        let bytes = open.tiles.tile(hash).unwrap();
        assert_eq!(
            &bytes[0..4],
            &[9, 8, 7, 255],
            "the external payload's pixels won, not the stale copy"
        );
        assert_eq!(
            ed.status(),
            Some("Pasted 2×1 from the clipboard"),
            "the status names the external paste"
        );
    }

    /// Card 052 ownership policy: the editor's OWN copy is recognized by
    /// fingerprint and keeps the internal at-origin route — Copy → Paste
    /// behaves exactly as before card 052.
    #[test]
    fn the_editors_own_copy_still_pastes_through_the_internal_store() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        let original = pixels::read_layer(ed.active().unwrap(), layer);
        select_rect(&mut ed, (4, 4), (12, 10));
        assert!(invoke(&mut ed, MenuAction::Copy).unwrap());
        assert!(invoke(&mut ed, MenuAction::Paste).unwrap());
        let doc = ed.active().unwrap();
        let pasted = doc.document.layers.root()[0];
        assert!(
            matches!(
                doc.document.layers.get(pasted).unwrap().kind,
                layer_model::LayerKind::Raster(_)
            ),
            "our own copy keeps the internal at-origin route"
        );
        let clip = ed.clipboard().expect("the internal store still holds it");
        assert_eq!(&pixels::read_layer(doc, pasted)[0..4], &clip.rgba8[0..4]);
        assert_eq!(
            &clip.rgba8[0..4],
            &original[((4 * 48) + 4) * 4..((4 * 48) + 4) * 4 + 4],
            "the copied pixels are what pasted"
        );
    }

    /// Card 052: the OS clipboard being unreadable (busy, image-less
    /// platform) degrades to the internal store — Copy → Paste keeps working.
    #[test]
    fn paste_falls_back_to_the_internal_store_when_the_os_clipboard_fails() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        ed.set_image_clipboard(Box::new(
            crate::clipboard::FakeClipboard::new()
                .with_failure(crate::clipboard::ClipboardError::Busy),
        ));
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        let original = pixels::read_layer(ed.active().unwrap(), layer);
        select_rect(&mut ed, (4, 4), (12, 10));
        let message = perform(MenuAction::Copy, &mut ed).unwrap();
        assert!(
            message.contains("refused"),
            "the copy says the OS clipboard would not take it: {message}"
        );
        // The copy itself succeeded regardless.
        assert!(ed.clipboard().is_some(), "the internal store filled anyway");
        assert!(invoke(&mut ed, MenuAction::Paste).unwrap());
        let doc = ed.active().unwrap();
        let pasted = doc.document.layers.root()[0];
        assert_eq!(
            &pixels::read_layer(doc, pasted)[0..4],
            &original[((4 * 48) + 4) * 4..((4 * 48) + 4) * 4 + 4],
            "the internal fallback pasted the copied pixels"
        );
    }

    /// Card 052: menu enablement distinguishes the two sources. With ONLY a
    /// foreign OS image (no internal copy), plain Paste is enabled and Paste
    /// Into is not — an external payload has no in-document origin to mask
    /// into yet (card 053).
    #[test]
    fn only_the_os_image_enables_paste_but_not_paste_into() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        // No internal copy at all; the OS clipboard holds a foreign image.
        let foreign = crate::clipboard::ClipboardImage::validate(3, 2, vec![1; 24]).unwrap();
        let mut os = crate::clipboard::FakeClipboard::new();
        os.seed(foreign);
        ed.set_image_clipboard(Box::new(os));
        let context = context(&mut ed, &Workspace::new());
        assert!(
            resolve(MenuAction::Paste, &context, &ed).is_ok(),
            "a foreign OS image enables plain Paste"
        );
        assert_eq!(
            resolve(MenuAction::PasteInto, &context, &ed),
            Err("The clipboard is empty".to_string()),
            "Paste Into waits for card 053: the external payload cannot honor the mask"
        );
    }

    /// Card 052: Copy Merged writes the OS clipboard too — the composited
    /// pixels, not one layer's — and remembers the fingerprint, so the
    /// editor's own merged copy still pastes through the internal route.
    #[test]
    fn copy_merged_writes_the_os_clipboard_and_still_pastes_internally() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        select_rect(&mut ed, (4, 4), (12, 10));
        assert!(invoke(&mut ed, MenuAction::CopyMerged).unwrap());
        // The OS clipboard now holds the merged crop: paste finds OUR
        // fingerprint and takes the internal route.
        assert!(invoke(&mut ed, MenuAction::Paste).unwrap());
        let doc = ed.active().unwrap();
        let pasted = doc.document.layers.root()[0];
        assert!(
            matches!(
                doc.document.layers.get(pasted).unwrap().kind,
                layer_model::LayerKind::Raster(_)
            ),
            "our own merged copy keeps the internal at-origin route"
        );
    }

    /// Card 052: the keyboard reaches the same policy — Action::Paste (the
    /// Ctrl+V binding, card 052's keymap defaults) routes the foreign OS
    /// payload through placement exactly like the menu item.
    #[test]
    fn the_paste_action_routes_the_foreign_os_payload() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let foreign = crate::clipboard::ClipboardImage::validate(
            2,
            2,
            vec![7, 7, 7, 255, 8, 8, 8, 255, 9, 9, 9, 255, 10, 10, 10, 255],
        )
        .unwrap();
        let mut os = crate::clipboard::FakeClipboard::new();
        os.seed(foreign);
        ed.set_image_clipboard(Box::new(os));
        ed.dispatch(Action::Paste).unwrap();
        let open = ed.active().unwrap();
        let pasted = open.document.active_layer().unwrap();
        assert!(
            matches!(
                open.document.layers.get(pasted).unwrap().kind,
                layer_model::LayerKind::SmartObject(_)
            ),
            "Ctrl+V pasted the foreign payload as a placed smart object"
        );
        use compositor::TileSource;
        let hash = open
            .document
            .layer_tiles(pasted)
            .unwrap()
            .get(raster::TileCoord::new(0, 0, 0))
            .expect("the placed source's first tile");
        let bytes = open.tiles.tile(hash).unwrap();
        assert_eq!(
            &bytes[0..4],
            &[7, 7, 7, 255],
            "the keyboard route used the OS payload"
        );
    }

    // ------------------------------------------------ W4-H: File / Edit gaps

    /// Resolve `action` through the live menu context and perform it — the
    /// road a menu click takes — returning the status sentence.
    fn through_the_menu(ed: &mut Editor, action: MenuAction) -> Result<String, String> {
        let ctx = context(ed, &Workspace::new());
        match resolve(action, &ctx, ed)? {
            Pick::Menu(a) => perform(a, ed),
            other => Err(format!("{action:?} resolved to {other:?}")),
        }
    }

    fn menu_offers(ed: &Editor, action: MenuAction) -> bool {
        menus(ed).into_iter().any(|m| m.actions().contains(&action))
    }

    #[test]
    fn paste_in_place_puts_the_copy_back_where_it_was_copied() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        assert!(menu_offers(&ed, MenuAction::PasteInPlace));
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        let original = pixels::read_layer(ed.active().unwrap(), layer);
        // Nothing copied yet: the row is off, and says why.
        let ctx = context(&mut ed, &Workspace::new());
        assert_eq!(
            resolve(MenuAction::PasteInPlace, &ctx, &ed).unwrap_err(),
            "The clipboard is empty"
        );
        select_rect(&mut ed, (10, 6), (18, 12));
        assert!(invoke(&mut ed, MenuAction::Copy).unwrap());
        assert_eq!(ed.clipboard().unwrap().origin, (10, 6));
        let depth = ed.active().unwrap().history_depth();
        let said = through_the_menu(&mut ed, MenuAction::PasteInPlace).unwrap();
        assert!(said.contains("in place at 10, 6"), "{said}");
        assert_eq!(ed.active().unwrap().history_depth(), depth + 1);
        let doc = ed.active().unwrap();
        let pasted = doc.document.active_layer().unwrap();
        assert_ne!(pasted, layer, "the paste made a new layer");
        let stored = pixels::read_layer(doc, pasted);
        let at =
            |buf: &[u8], x: usize, y: usize| buf[(y * 48 + x) * 4..(y * 48 + x) * 4 + 4].to_vec();
        // Every copied pixel is back at its own canvas position...
        for (x, y) in [(10, 6), (17, 11), (13, 9)] {
            assert_eq!(at(&stored, x, y), at(&original, x, y), "({x},{y})");
        }
        // ...and nothing landed at the origin, where plain Paste would put it.
        assert_eq!(
            at(&stored, 0, 0)[3],
            0,
            "Paste in Place pasted at the origin"
        );
        assert_eq!(at(&stored, 18, 12)[3], 0, "the paste spilled past the copy");
    }

    #[test]
    fn paste_outside_masks_by_the_inverse_of_the_selection() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        assert!(menu_offers(&ed, MenuAction::PasteOutside));
        select_rect(&mut ed, (0, 0), (48, 32));
        assert!(invoke(&mut ed, MenuAction::Copy).unwrap());
        // Without a selection the row is off.
        ed.active_mut().unwrap().document.selection = editor_core::Selection::None;
        let ctx = context(&mut ed, &Workspace::new());
        assert_eq!(
            resolve(MenuAction::PasteOutside, &ctx, &ed).unwrap_err(),
            "Paste Outside needs a selection"
        );
        select_rect(&mut ed, (8, 8), (16, 16));
        let depth = ed.active().unwrap().history_depth();
        let status = through_the_menu(&mut ed, MenuAction::PasteOutside).unwrap();
        assert_eq!(status, "Pasted outside the selection on a new layer");
        assert_eq!(
            ed.active().unwrap().history_depth(),
            depth + 1,
            "one undo step"
        );
        let doc = ed.active().unwrap();
        let pasted = doc.document.active_layer().unwrap();
        let mask = read_mask_coverage(doc, pasted, 48, 32);
        // Hidden inside the selection, shown everywhere else: Paste Into
        // turned inside out.
        assert_eq!(mask[10 * 48 + 10], 0, "the selection is not hidden");
        assert_eq!(mask[2 * 48 + 2], 255, "outside the selection is hidden");
        assert_eq!(mask[20 * 48 + 30], 255, "outside the selection is hidden");
    }

    #[test]
    fn purging_the_clipboard_empties_it_and_turns_paste_special_off() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let purge = MenuAction::Purge(ui::menu::PurgeTarget::Clipboard);
        assert!(menu_offers(&ed, purge));
        let ctx = context(&mut ed, &Workspace::new());
        assert_eq!(
            resolve(purge, &ctx, &ed).unwrap_err(),
            "The clipboard is empty"
        );
        select_rect(&mut ed, (0, 0), (8, 8));
        assert!(invoke(&mut ed, MenuAction::Copy).unwrap());
        let depth = ed.active().unwrap().history_depth();
        // The clipboard purge needs no confirmation: it goes at once.
        let said = through_the_menu(&mut ed, purge).unwrap();
        assert!(said.contains("Purged the clipboard"), "{said}");
        assert!(ed.clipboard().is_none());
        assert_eq!(
            ed.active().unwrap().history_depth(),
            depth,
            "history untouched"
        );
        let ctx = context(&mut ed, &Workspace::new());
        assert!(resolve(MenuAction::PasteInPlace, &ctx, &ed).is_err());
        assert!(resolve(purge, &ctx, &ed).is_err());
    }

    #[test]
    fn purging_histories_asks_first_then_drops_every_step() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = with_two_layers(dir.path());
        let purge = MenuAction::Purge(ui::menu::PurgeTarget::Histories);
        assert!(menu_offers(&ed, purge));
        let steps = ed.active().unwrap().history_depth();
        assert!(steps > 0, "the fixture has history");
        assert!(ed.active_mut().unwrap().undo().unwrap());
        let rect = ed.active().unwrap().canvas_rect();
        let before = ed.active_mut().unwrap().composite(rect).unwrap();
        let layers = ed.active().unwrap().document.layers.len();
        // The first choice only asks.
        let asked = through_the_menu(&mut ed, purge).unwrap();
        assert!(asked.contains("cannot be undone"), "{asked}");
        assert!(
            asked.contains(&format!("{steps} undo/redo step(s)")),
            "{asked}"
        );
        assert_eq!(ed.status(), Some(asked.as_str()));
        let doc = ed.active().unwrap();
        assert!(
            doc.history.can_undo() && doc.history.can_redo(),
            "asking purged"
        );
        // A different purge does not confirm this one.
        let other = through_the_menu(&mut ed, MenuAction::Purge(ui::menu::PurgeTarget::All));
        assert!(other.unwrap().contains("cannot be undone"));
        assert!(ed.active().unwrap().history.can_undo());
        // Choosing Histories again (re-arming after the All question) asks,
        // and the next Histories confirms.
        through_the_menu(&mut ed, purge).unwrap();
        let done = through_the_menu(&mut ed, purge).unwrap();
        assert_eq!(done, format!("Purged {steps} history step(s)"));
        let doc = ed.active().unwrap();
        assert!(!doc.history.can_undo() && !doc.history.can_redo());
        assert_eq!(
            ed.active_mut().unwrap().composite(rect).unwrap(),
            before,
            "purging history changed the pixels"
        );
        assert_eq!(ed.active().unwrap().document.layers.len(), layers);
        // Nothing left to purge: the row is off, and says why.
        let ctx = context(&mut ed, &Workspace::new());
        assert_eq!(
            resolve(purge, &ctx, &ed).unwrap_err(),
            "There is no history to purge"
        );
    }

    /// Purge Histories drops every open document's history, so a background
    /// document's history is enough for the row to be on.
    #[test]
    fn purging_histories_is_offered_for_a_background_documents_history() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = with_two_layers(dir.path());
        let background = ed.active().unwrap().id();
        let steps = ed.active().unwrap().history_depth();
        assert!(steps > 0, "the fixture has history");
        ed.open_path(&probe_png(dir.path(), 20, 20)).unwrap();
        let active = ed.active().unwrap();
        assert_ne!(active.id(), background, "the second document is active");
        assert!(!active.history.can_undo() && !active.history.can_redo());
        let purge = MenuAction::Purge(ui::menu::PurgeTarget::Histories);
        let asked = through_the_menu(&mut ed, purge).unwrap();
        assert!(
            asked.contains(&format!("{steps} undo/redo step(s)")),
            "{asked}"
        );
        let done = through_the_menu(&mut ed, purge).unwrap();
        assert_eq!(done, format!("Purged {steps} history step(s)"));
        assert!(ed.documents().iter().all(|d| !d.history.can_undo()));
    }

    /// An edit between the question and the confirming choice changes what
    /// the purge would drop, so the second choice asks again, naming the new
    /// count, instead of dropping a count the question did not name.
    #[test]
    fn an_edit_between_the_two_purge_choices_asks_again() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = with_two_layers(dir.path());
        let steps = ed.active().unwrap().history_depth();
        let purge = MenuAction::Purge(ui::menu::PurgeTarget::Histories);
        let asked = through_the_menu(&mut ed, purge).unwrap();
        assert!(asked.contains(&format!("{steps} undo/redo step(s)")));
        ed.apply_command(Command::create_layer(layer_model::Layer::raster("Third")));
        let again = through_the_menu(&mut ed, purge).unwrap();
        assert!(
            again.contains(&format!("{} undo/redo step(s)", steps + 1)),
            "the edit did not re-ask: {again}"
        );
        assert!(ed.active().unwrap().history.can_undo(), "it purged unasked");
        let done = through_the_menu(&mut ed, purge).unwrap();
        assert_eq!(done, format!("Purged {} history step(s)", steps + 1));
    }

    #[test]
    fn export_slices_writes_one_file_per_slice_with_the_export_as_format() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("slices");
        let mut ed = Editor::with_state(
            AppPaths::rooted(dir.path().join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(crate::dialogs::ScriptedDialogs::new().exporting_folder(&out)),
        );
        ed.set_image_clipboard(Box::new(crate::clipboard::FakeClipboard::new()));
        ed.open_path(&probe_png(dir.path(), 48, 32)).unwrap();
        assert!(menu_offers(&ed, MenuAction::ExportSlices));
        // No slices yet: a loud refusal, and no folder asked for or written.
        let refused = through_the_menu(&mut ed, MenuAction::ExportSlices).unwrap_err();
        assert!(refused.contains("no slices"), "{refused}");
        assert!(!out.exists());
        // The Slice tool's commit hands its regions over (the same call the
        // tool pointer makes on Enter).
        crate::slices_export::remember_committed(
            &mut ed,
            &[
                tools::Slice {
                    rect: raster::PixelRect::new(0, 0, 16, 8),
                    name: String::new(),
                },
                tools::Slice {
                    rect: raster::PixelRect::new(20, 10, 12, 20),
                    name: String::new(),
                },
            ],
        );
        ui::dialogs::export_as::forget_last_confirmed_entry();
        let said = through_the_menu(&mut ed, MenuAction::ExportSlices).unwrap();
        assert!(said.contains("Exported 2 slice(s) as PNG"), "{said}");
        let mut names: Vec<String> = std::fs::read_dir(&out)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, vec!["probe-48x32_01.png", "probe-48x32_02.png"]);
        let second = raster::decode_path(&out.join("probe-48x32_02.png")).unwrap();
        assert_eq!((second.width, second.height), (12, 20));
        // The slice's first pixel is canvas (20,10) of the probe.
        assert_eq!(
            &second.rgba8[0..4],
            // probe_png: (x*5 % 251, y*7 % 241, (x*13 + y*3) % 239, 255).
            &[100, 70, 51, 255]
        );
        ui::dialogs::export_as::forget_last_confirmed_entry();
    }

    #[test]
    fn export_slices_follows_the_last_exported_export_as_settings() {
        use ui::dialogs::Dialog as _;
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("slices");
        let mut ed = Editor::with_state(
            AppPaths::rooted(dir.path().join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(crate::dialogs::ScriptedDialogs::new().exporting_folder(&out)),
        );
        ed.set_image_clipboard(Box::new(crate::clipboard::FakeClipboard::new()));
        ed.open_path(&probe_png(dir.path(), 48, 32)).unwrap();
        crate::slices_export::remember_committed(
            &mut ed,
            &[tools::Slice {
                rect: raster::PixelRect::new(4, 4, 20, 10),
                name: String::new(),
            }],
        );
        // The user exports an Export As at WebP, half size (the shell
        // remembers the job once the folder picker answers; pinned by
        // `shell::tests::an_export_as_job_is_remembered_for_slices_only_once_a_folder_is_chosen`).
        let mut dialog = ui::dialogs::ExportAsDialog::new(
            48,
            32,
            "probe",
            ui::dialogs::PreviewSource::placeholder(8, 8),
        );
        dialog.set_format(raster::ExportFormat::WebP);
        dialog.set_scale(0.5);
        let Some(ui::dialogs::DialogAction::Export(job)) = dialog.confirm() else {
            panic!("a valid job");
        };
        ui::dialogs::export_as::remember_exported_job(&job);
        let said = through_the_menu(&mut ed, MenuAction::ExportSlices).unwrap();
        assert!(said.contains("as WEBP"), "{said}");
        let file = out.join("probe-48x32_01.webp");
        let decoded = raster::decode_path(&file).unwrap();
        assert_eq!(
            (decoded.width, decoded.height),
            (10, 5),
            "scale not applied"
        );
        ui::dialogs::export_as::forget_last_confirmed_entry();
    }

    /// Card 053: Paste Into keeps the FULL image on the new layer and
    /// confines it with a retained selection-derived mask - the destructive
    /// alpha multiplication is gone. Disabling the mask reveals every
    /// original pixel; one undo removes layer, pixels and mask together.
    #[test]
    fn paste_into_keeps_the_full_image_under_a_selection_mask() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        let original = pixels::read_layer(ed.active().unwrap(), layer);
        select_rect(&mut ed, (4, 4), (12, 10));
        assert!(invoke(&mut ed, MenuAction::Copy).unwrap());
        // The backdrop changes AFTER the copy: the pasted crop (the old ink)
        // is now distinguishable from what is below it.
        {
            let doc = ed.active_mut().unwrap();
            let (w, h) = (doc.document.width(), doc.document.height());
            let red = vec![255u8; (w as usize) * (h as usize) * 4];
            let paint = pixels::write_layer(doc, layer, &red, "red").unwrap();
            ed.apply_command(paint);
        }
        let depth_before = ed.active().unwrap().history_depth();
        let status = through_the_menu(&mut ed, MenuAction::PasteInto).unwrap();
        assert_eq!(status, "Pasted into the selection on a new layer");

        let doc = ed.active().unwrap();
        let pasted = doc.document.layers.root()[0];
        let stored = pixels::read_layer(doc, pasted);
        // The stored pixels are the full crop: NOTHING was multiplied away.
        assert_eq!(
            &stored[0..4],
            &original[((4 * 48) + 4) * 4..((4 * 48) + 4) * 4 + 4],
            "the layer stores the full copied image"
        );
        // The composite shows the pasted ink only INSIDE the selection.
        let rect = ed.active().unwrap().canvas_rect();
        let composite = ed.active_mut().unwrap().composite(rect).unwrap();
        let outside = ((20 * 48) + 20) * 4;
        assert_eq!(
            &composite[outside..outside + 4],
            &[255, 255, 255, 255],
            "outside the selection the red backdrop shows"
        );
        // One undo removes the layer and its mask together.
        assert_eq!(
            ed.active().unwrap().history_depth(),
            depth_before + 1,
            "the whole paste-into is one undoable step"
        );
        assert!(ed.active_mut().unwrap().undo().unwrap());
        {
            let doc = ed.active().unwrap();
            assert_eq!(
                doc.document.layers.root().len(),
                1,
                "the pasted layer is gone"
            );
            assert!(
                doc.document.layers.get(layer).unwrap().mask.is_none(),
                "undo restored the backdrop untouched"
            );
        }
        // Back to the pasted state: disabling the mask reveals the FULL
        // original image. Probe a pixel inside the pasted crop (it sits at
        // the canvas origin) but outside the selection — masked it was
        // hidden, unmasked the original ink shows.
        assert!(ed.active_mut().unwrap().redo().unwrap());
        assert!(invoke(&mut ed, MenuAction::Mask(ui::menu::MaskOp::Toggle)).unwrap());
        let rect = ed.active().unwrap().canvas_rect();
        let composite = ed.active_mut().unwrap().composite(rect).unwrap();
        let crop_only = ((2 * 48) + 2) * 4;
        // The crop came from (4,4), so canvas (2,2) holds original (6,6).
        let source = ((6 * 48) + 6) * 4;
        assert_eq!(
            &composite[crop_only..crop_only + 4],
            &original[source..source + 4],
            "with the mask disabled the full original image shows"
        );
    }

    /// Card 053: fractional selection coverage maps through the mask - a
    /// half-covered pixel composites as a half blend, not a cutoff.
    #[test]
    fn paste_into_maps_fractional_coverage_through_the_mask() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        let original = pixels::read_layer(ed.active().unwrap(), layer);
        // A mask selection with HALF coverage over an 8x8 box.
        let (w, h) = (8u32, 8u32);
        let coverage = vec![128u8; (w * h) as usize];
        let mask = editor_core::SelectionMask::new(glam::IVec2::new(4, 4), w, h, coverage).unwrap();
        ed.active_mut().unwrap().document.selection = editor_core::Selection::Mask(mask);
        assert!(invoke(&mut ed, MenuAction::Copy).unwrap());
        {
            let doc = ed.active_mut().unwrap();
            let (cw, ch) = (doc.document.width(), doc.document.height());
            let red = vec![255u8; (cw as usize) * (ch as usize) * 4];
            let paint = pixels::write_layer(doc, layer, &red, "red").unwrap();
            ed.apply_command(paint);
        }
        assert!(invoke(&mut ed, MenuAction::PasteInto).unwrap());
        let rect = ed.active().unwrap().canvas_rect();
        let composite = ed.active_mut().unwrap().composite(rect).unwrap();
        // The copied ink at (5,5), composited at half coverage over the
        // white backdrop. The compositing runs in the working (linear)
        // space, so the exact byte is not the sRGB-space formula — what the
        // card demands is that the coverage BLENDS: strictly between the
        // backdrop and the full ink (a cutoff would be pure ink, full
        // coverage would hide the backdrop entirely).
        let probe = ((5 * 48) + 5) * 4;
        // The clip was copied from the selection at (4,4), so canvas (5,5)
        // holds original (9,9) — the pixel that must be blending.
        let ink = &original[((9 * 48) + 9) * 4..((9 * 48) + 9) * 4 + 4];
        for c in 0..3 {
            assert!(
                composite[probe + c] > ink[c] && composite[probe + c] < 255,
                "channel {c}: {} must blend between ink {} and backdrop 255",
                composite[probe + c],
                ink[c]
            );
        }
    }

    /// Card 056: the Select menu rides history too - Select All is one
    /// undoable step, and undo puts the previous selection back.
    #[test]
    fn select_all_is_one_undoable_step() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let depth_before = ed.active().unwrap().history_depth();
        assert!(invoke(&mut ed, MenuAction::SelectAll).unwrap());
        assert!(
            ed.active().unwrap().document.selection.bounds().is_some(),
            "Select All selects the canvas"
        );
        assert_eq!(
            ed.active().unwrap().history_depth(),
            depth_before + 1,
            "one undoable step"
        );
        assert!(ed.active_mut().unwrap().undo().unwrap());
        assert_eq!(
            ed.active().unwrap().document.selection,
            editor_core::Selection::None,
            "undo restores no selection"
        );
    }

    // ---- W9-G: Layer > Vector Mask ---------------------------------------

    /// W9-G: the whole road — the Paths panel's Work Path enables Current
    /// Path in the menu context, the click attaches a live vector mask, and
    /// the application's own compositor hides the layer outside the path.
    /// Disable/Enable and Delete are one undo step each.
    #[test]
    fn a_vector_mask_from_the_current_path_hides_the_layer_outside_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        let mut ws = Workspace::new();
        ws.paths.work_path = Some(vector::parse_svg("M0 0 L48 0 L0 32 Z").unwrap());
        ws.paths.work_selected = true;
        let row = |op| MenuAction::VectorMask(op);
        let click = |ed: &mut Editor, ws: &Workspace, action: MenuAction| {
            let ctx = context(ed, ws);
            match resolve(action, &ctx, ed).expect("the row is enabled") {
                Pick::Menu(a) => perform(a, ed).expect("performed"),
                other => panic!("{other:?}"),
            };
        };
        let alpha_at = |ed: &mut Editor, x: usize, y: usize| {
            let rgba = ed
                .active_mut()
                .unwrap()
                .composite(raster::PixelRect::new(0, 0, 48, 32))
                .unwrap();
            rgba[(y * 48 + x) * 4 + 3]
        };
        assert_eq!(alpha_at(&mut ed, 46, 30), 255, "unmasked to begin with");

        use ui::menu::VectorMaskOp as V;
        assert!(context(&mut ed, &ws).has_current_path);
        click(&mut ed, &ws, row(V::CurrentPath));
        let mask = ed
            .active()
            .unwrap()
            .document
            .layers
            .get(layer)
            .unwrap()
            .mask
            .clone();
        let v = mask.and_then(|m| m.vector).expect("a live vector mask");
        assert!(v.path_svg.contains('M'), "kept as a path: {}", v.path_svg);
        assert_eq!(alpha_at(&mut ed, 2, 2), 255, "inside the triangle");
        assert_eq!(alpha_at(&mut ed, 46, 30), 0, "outside the triangle");

        let depth = ed.active().unwrap().history_depth();
        click(&mut ed, &ws, row(V::Toggle));
        assert_eq!(alpha_at(&mut ed, 46, 30), 255, "disabled");
        assert_eq!(ed.active().unwrap().history_depth(), depth + 1);
        assert!(ed.active_mut().unwrap().undo().unwrap());
        assert_eq!(alpha_at(&mut ed, 46, 30), 0, "undo re-enables");

        // A vector-only mask is not a pixel mask: Apply Mask has no coverage
        // to bake (reading none would erase the layer), and adding a pixel
        // mask keeps the vector one beside it.
        use ui::menu::MaskOp;
        let ctx = context(&mut ed, &ws);
        assert!(resolve(MenuAction::Mask(MaskOp::Apply), &ctx, &ed).is_err());
        click(&mut ed, &ws, MenuAction::Mask(MaskOp::RevealAll));
        let both = ed.active().unwrap().document.layers.get(layer).unwrap();
        let both = both.mask.clone().unwrap();
        assert_eq!(both.kind, layer_model::MaskKind::Raster, "a pixel mask now");
        assert!(both.vector.is_some(), "the vector mask survived");
        assert_eq!(alpha_at(&mut ed, 2, 2), 255);
        assert_eq!(alpha_at(&mut ed, 46, 30), 0, "the vector still hides");
        assert!(ed.active_mut().unwrap().undo().unwrap());

        click(&mut ed, &ws, row(V::Delete));
        assert!(ed
            .active()
            .unwrap()
            .document
            .layers
            .get(layer)
            .unwrap()
            .mask
            .is_none());
        assert_eq!(alpha_at(&mut ed, 46, 30), 255, "deleted");

        // Without a path the row is off, and says why.
        let empty = Workspace::new();
        let ctx = context(&mut ed, &empty);
        assert!(!ctx.has_current_path);
        assert!(resolve(row(V::CurrentPath), &ctx, &ed).is_err());
    }

    /// W9-G (review round 2): Layer > Layer Mask > Apply on a layer with
    /// BOTH masks bakes the pixel mask and keeps the vector mask live.
    #[test]
    fn apply_mask_with_both_masks_keeps_the_vector_mask() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        let mut ws = Workspace::new();
        ws.paths.work_path = Some(vector::parse_svg("M0 0 L48 0 L0 32 Z").unwrap());
        ws.paths.work_selected = true;
        let click = |ed: &mut Editor, ws: &Workspace, action: MenuAction| {
            let ctx = context(ed, ws);
            match resolve(action, &ctx, ed).expect("the row is enabled") {
                Pick::Menu(a) => perform(a, ed).expect("performed"),
                other => panic!("{other:?}"),
            };
        };
        let alpha_at = |ed: &mut Editor, x: usize, y: usize| {
            let rgba = ed
                .active_mut()
                .unwrap()
                .composite(raster::PixelRect::new(0, 0, 48, 32))
                .unwrap();
            rgba[(y * 48 + x) * 4 + 3]
        };
        use ui::menu::{MaskOp, VectorMaskOp as V};
        click(&mut ed, &ws, MenuAction::VectorMask(V::CurrentPath));
        click(&mut ed, &ws, MenuAction::Mask(MaskOp::RevealAll));
        let path = |ed: &Editor| {
            ed.active()
                .unwrap()
                .document
                .layers
                .get(layer)
                .unwrap()
                .mask
                .clone()
                .and_then(|m| m.vector)
                .map(|v| v.path_svg)
        };
        let before = path(&ed).expect("both masks before Apply");
        click(&mut ed, &ws, MenuAction::Mask(MaskOp::Apply));
        assert_eq!(
            path(&ed).as_deref(),
            Some(before.as_str()),
            "the vector mask survives Apply"
        );
        let doc = &ed.active().unwrap().document;
        let mask = doc.layers.get(layer).unwrap().mask.clone().unwrap();
        assert!(is_vector_only(doc, &mask), "only the vector mask is left");
        assert_eq!(alpha_at(&mut ed, 2, 2), 255, "inside the triangle");
        assert_eq!(alpha_at(&mut ed, 46, 30), 0, "the vector still hides");
        // One undo step brings both masks back.
        assert!(ed.active_mut().unwrap().undo().unwrap());
        let doc = &ed.active().unwrap().document;
        let mask = doc.layers.get(layer).unwrap().mask.clone().unwrap();
        assert_eq!(mask.kind, layer_model::MaskKind::Raster);
        assert!(mask.vector.is_some());
    }

    /// W9-G (review round 3): adding a pixel mask (Layer Mask > Reveal All)
    /// to a layer whose vector-only mask was unlinked and then left behind by
    /// a layer move keeps the vector mask's pose and its Linked=false — it
    /// neither jumps with the layer nor silently re-links — and the new
    /// pixel mask's coverage is laid in that same pose, so it reveals the
    /// whole canvas.
    #[test]
    fn a_pixel_mask_added_beside_an_unlinked_vector_mask_keeps_its_pose() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        let mut ws = Workspace::new();
        ws.paths.work_path = Some(vector::parse_svg("M0 0 L24 0 L24 32 L0 32 Z").unwrap());
        ws.paths.work_selected = true;
        let click = |ed: &mut Editor, ws: &Workspace, action: MenuAction| {
            let ctx = context(ed, ws);
            match resolve(action, &ctx, ed).expect("the row is enabled") {
                Pick::Menu(a) => perform(a, ed).expect("performed"),
                other => panic!("{other:?}"),
            };
        };
        let alpha_at = |ed: &mut Editor, x: usize, y: usize| {
            let rgba = ed
                .active_mut()
                .unwrap()
                .composite(raster::PixelRect::new(0, 0, 48, 32))
                .unwrap();
            rgba[(y * 48 + x) * 4 + 3]
        };
        let mask_of = |ed: &Editor| {
            ed.active()
                .unwrap()
                .document
                .layers
                .get(layer)
                .unwrap()
                .mask
                .clone()
                .unwrap()
        };
        use ui::menu::{MaskOp, VectorMaskOp as V};
        click(&mut ed, &ws, MenuAction::VectorMask(V::CurrentPath));
        // Properties > Linked unchecked, then the layer moves 10px right.
        let mut m = mask_of(&ed);
        m.linked = false;
        ed.apply_command(Command::SetLayerProperties {
            layer_id: layer,
            patch: editor_core::LayerPatch {
                mask: editor_core::Patch::Set(m),
                ..Default::default()
            },
        });
        let shift = glam::Affine2::from_translation(glam::Vec2::new(10.0, 0.0));
        ed.apply_command(Command::TransformLayer {
            layer_id: layer,
            matrix: shift.to_cols_array(),
        });
        let before = mask_of(&ed);
        assert!(!before.linked);
        assert_ne!(*before.transform, glam::Affine2::IDENTITY, "the mask held");
        assert_eq!(alpha_at(&mut ed, 15, 16), 255, "inside the vector mask");
        assert_eq!(alpha_at(&mut ed, 30, 16), 0, "the vector mask stayed put");

        click(&mut ed, &ws, MenuAction::Mask(MaskOp::RevealAll));
        let after = mask_of(&ed);
        assert_eq!(
            after.kind,
            layer_model::MaskKind::Raster,
            "a pixel mask now"
        );
        assert!(after.vector.is_some(), "the vector mask survived");
        assert!(!after.linked, "still unlinked");
        assert_eq!(*after.transform, *before.transform, "the pose is kept");
        assert_eq!(alpha_at(&mut ed, 15, 16), 255, "inside the vector mask");
        assert_eq!(alpha_at(&mut ed, 30, 16), 0, "the vector mask did not jump");

        // With the vector half disabled, the pixel mask alone reveals the
        // whole (moved) layer on the canvas: its coverage sits in the pose.
        click(&mut ed, &ws, MenuAction::VectorMask(V::Toggle));
        assert_eq!(alpha_at(&mut ed, 12, 16), 255, "pixel mask reveals");
        assert_eq!(
            alpha_at(&mut ed, 45, 16),
            255,
            "pixel mask reveals the far edge"
        );
        assert_eq!(
            alpha_at(&mut ed, 5, 16),
            0,
            "left of the moved layer: no content"
        );

        // Reveal Selection samples the selection per pixel, so a coverage
        // laid in the wrong pose would show a shifted band: undo the toggle
        // and the Reveal All, select document x 30..46, and add the mask.
        assert!(ed.active_mut().unwrap().undo().unwrap());
        assert!(ed.active_mut().unwrap().undo().unwrap());
        assert!(is_vector_only(
            &ed.active().unwrap().document,
            &mask_of(&ed)
        ));
        select_rect(&mut ed, (30, 0), (46, 32));
        click(&mut ed, &ws, MenuAction::Mask(MaskOp::RevealSelection));
        assert!(!mask_of(&ed).linked, "still unlinked");
        click(&mut ed, &ws, MenuAction::VectorMask(V::Toggle));
        assert_eq!(alpha_at(&mut ed, 40, 16), 255, "the selected band shows");
        assert_eq!(
            alpha_at(&mut ed, 25, 16),
            0,
            "left of the selection is hidden"
        );
    }

    // ---- Card 057: the four mask-creation ops ----------------------------

    /// Card 057: the four ops produce REAL coverage — a known two-colour
    /// image gives four distinct, exactly-pinned results through the real
    /// menu actions, one undoable step each.
    #[test]
    fn the_four_mask_creation_ops_produce_four_distinct_real_coverages() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let layer = ed.active().unwrap().document.active_layer().unwrap();

        // Reveal All: the whole canvas revealed with stored coverage.
        assert!(invoke(&mut ed, MenuAction::Mask(ui::menu::MaskOp::RevealAll)).unwrap());
        {
            let doc = ed.active().unwrap();
            assert!(doc.document.layers.get(layer).unwrap().mask.is_some());
            let coverage = read_mask_coverage(doc, layer, 48, 32);
            assert!(
                coverage.iter().all(|&c| c == 255),
                "Reveal All stores all-255 coverage (absent tiles mean hidden, so it MUST be stored)"
            );
        }
        ed.active_mut().unwrap().undo().unwrap();
        assert!(
            ed.active()
                .unwrap()
                .document
                .layers
                .get(layer)
                .unwrap()
                .mask
                .is_none(),
            "undo removes the mask the op attached"
        );

        // Hide All: the mask with no tiles at all — absent means hidden,
        // which IS the requested state.
        assert!(invoke(&mut ed, MenuAction::Mask(ui::menu::MaskOp::HideAll)).unwrap());
        assert_eq!(
            read_mask_coverage(ed.active().unwrap(), layer, 48, 32),
            vec![0u8; 48 * 32],
            "Hide All hides everything"
        );
        let rect = ed.active().unwrap().canvas_rect();
        let composite = ed.active_mut().unwrap().composite(rect).unwrap();
        assert!(
            composite.iter().skip(3).step_by(4).all(|&a| a == 0),
            "the layer is fully hidden"
        );
        ed.active_mut().unwrap().undo().unwrap();

        // Reveal Selection: only the selected area shows, fractional edges
        // retained by the shared helper.
        select_rect(&mut ed, (0, 0), (24, 32));
        assert!(invoke(&mut ed, MenuAction::Mask(ui::menu::MaskOp::RevealSelection)).unwrap());
        let rect = ed.active().unwrap().canvas_rect();
        let composite = ed.active_mut().unwrap().composite(rect).unwrap();
        assert_ne!(composite[3], 0, "inside the selection shows");
        assert_eq!(
            composite[(40 * 4) + 3],
            0,
            "outside the selection is hidden"
        );
        ed.active_mut().unwrap().undo().unwrap();

        // Hide Selection: the mirror image.
        assert!(invoke(&mut ed, MenuAction::Mask(ui::menu::MaskOp::HideSelection)).unwrap());
        let rect = ed.active().unwrap().canvas_rect();
        let composite = ed.active_mut().unwrap().composite(rect).unwrap();
        assert_eq!(composite[3], 0, "the selection is hidden");
        assert_ne!(composite[(40 * 4) + 3], 0, "outside the selection shows");
        // One undo removes the mask and its coverage together.
        assert!(ed.active_mut().unwrap().undo().unwrap());
        assert!(
            ed.active()
                .unwrap()
                .document
                .layers
                .get(layer)
                .unwrap()
                .mask
                .is_none(),
            "undo removes the whole op"
        );
    }

    /// Card 057: the already-masked rule and the selection rules.
    #[test]
    fn mask_creation_refuses_an_existing_mask_and_a_missing_selection() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());

        // Reveal Selection with NO selection refuses — the menu gate says
        // it first; the app-side rule (create_mask) is the defensive second
        // gate, driven directly here.
        let err = invoke(&mut ed, MenuAction::Mask(ui::menu::MaskOp::RevealSelection)).unwrap_err();
        assert!(
            err.contains("no selection") || err.contains("Select an area"),
            "{err:?}"
        );
        let err = create_mask(&mut ed, ui::menu::MaskOp::RevealSelection).unwrap_err();
        assert!(err.contains("Select an area"), "{err:?}");

        // An existing mask refuses creation (delete first — a silent replace
        // would destroy coverage).
        assert!(invoke(&mut ed, MenuAction::Mask(ui::menu::MaskOp::RevealAll)).unwrap());
        let err = invoke(&mut ed, MenuAction::Mask(ui::menu::MaskOp::HideAll)).unwrap_err();
        assert!(err.contains("already has a mask"), "{err:?}");
        // The refused op changed nothing.
        assert!(
            ed.active()
                .unwrap()
                .document
                .layers
                .get(ed.active().unwrap().document.active_layer().unwrap())
                .unwrap()
                .mask
                .is_some(),
            "the original mask survives the refusal"
        );
    }

    /// Card 057: the ops survive a MULTI-TILE canvas, where the absent-tile
    /// convention bites: Hide Selection must REVEAL outside the selection's
    /// bounding tiles (the round-1 review's single-tile blind spot), and
    /// Reveal All must store 255 over every tile the layer touches.
    #[test]
    fn mask_creation_on_a_multi_tile_canvas_respects_the_missing_tile_convention() {
        let dir = tempfile::tempdir().unwrap();
        // 300x64: two tiles wide, one tall. The selection sits in tile (0,0)
        // only, so tiles (1,0) — and the far half of tile (0,0) — are
        // outside it.
        let mut ed = {
            let png = dir.path().join("wide.png");
            let mut rgba = vec![0u8; (300 * 64 * 4) as usize];
            for px in rgba.as_chunks_mut::<4>().0 {
                px.copy_from_slice(&[120, 40, 200, 255]);
            }
            std::fs::write(
                &png,
                raster::encode(raster::ExportFormat::Png, 300, 64, &rgba).unwrap(),
            )
            .unwrap();
            let mut ed = opened(dir.path());
            ed.open_path(&png).expect("the wide canvas opens");
            ed
        };
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        select_rect(&mut ed, (10, 10), (40, 40));

        // Hide Selection: INSIDE hidden, and — the convention — the far
        // tile REVEALED (stored 255), not absent.
        assert!(invoke(&mut ed, MenuAction::Mask(ui::menu::MaskOp::HideSelection)).unwrap());
        {
            let doc = ed.active().unwrap();
            let coverage = read_mask_coverage(doc, layer, 300, 64);
            assert_eq!(
                coverage[(20 * 300 + 20) as usize],
                0,
                "inside the selection hides"
            );
            assert_eq!(
                coverage[(30 * 300 + 200) as usize],
                255,
                "the far tile is STORED revealed (an absent tile would hide it)"
            );
        }
        ed.active_mut().unwrap().undo().unwrap();

        // Reveal All: every tile the layer touches carries stored 255 —
        // including tile (1,0), which a bounds-clipped walk would skip.
        assert!(invoke(&mut ed, MenuAction::Mask(ui::menu::MaskOp::RevealAll)).unwrap());
        {
            let doc = ed.active().unwrap();
            let coverage = read_mask_coverage(doc, layer, 300, 64);
            assert!(
                coverage.iter().all(|&c| c == 255),
                "Reveal All stores 255 over the whole multi-tile extent"
            );
        }
    }

    /// Card 057: fractional selection coverage is retained through the ops
    /// themselves — a half-covered selection reveals at half strength.
    #[test]
    fn reveal_selection_retains_fractional_coverage() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        // A mask selection with HALF coverage over an 8x8 box at the origin.
        let coverage = vec![128u8; 8 * 8];
        let mask = editor_core::SelectionMask::new(glam::IVec2::ZERO, 8, 8, coverage).unwrap();
        ed.active_mut().unwrap().document.selection = editor_core::Selection::Mask(mask);

        assert!(invoke(&mut ed, MenuAction::Mask(ui::menu::MaskOp::RevealSelection)).unwrap());
        let stored = read_mask_coverage(ed.active().unwrap(), layer_of(&ed), 48, 32);
        assert_eq!(
            stored[0], 128,
            "the fractional sample survives the op verbatim"
        );
        // The composite blends: strictly between hidden and fully shown.
        let rect = ed.active().unwrap().canvas_rect();
        let composite = ed.active_mut().unwrap().composite(rect).unwrap();
        assert!(
            composite[3] > 0 && composite[3] < 255,
            "the half coverage blends (alpha {})",
            composite[3]
        );

        fn layer_of(ed: &Editor) -> layer_model::LayerId {
            ed.active().unwrap().document.active_layer().unwrap()
        }
    }

    /// Card 057: the coverage lives in the LAYER's local space, so a
    /// document-space selection survives a TRANSFORMED layer: move the layer
    /// first, then Reveal Selection — the document-space area the user drew
    /// is what shows.
    #[test]
    fn reveal_selection_on_a_moved_layer_lands_on_the_document_space_selection() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let layer = ed.active().unwrap().document.active_layer().unwrap();

        // Move the layer +12px right: its content now sits at document
        // x in [12, 60] — still inside the 48px canvas for probing, but the
        // document-space selection no longer lines up with layer-local
        // coordinates.
        editor_core::Command::TransformLayer {
            layer_id: layer,
            matrix: glam::Affine2::from_translation(glam::Vec2::new(12.0, 0.0)).to_cols_array(),
        }
        .apply(&mut ed.active_mut().unwrap().document)
        .unwrap();

        // Select a document-space rect over the moved layer's content.
        select_rect(&mut ed, (20, 8), (40, 24));
        assert!(invoke(&mut ed, MenuAction::Mask(ui::menu::MaskOp::RevealSelection)).unwrap());

        // The DOCUMENT-space selection area shows; the unselected part of the
        // moved layer (document x=44, still on the layer) is hidden.
        let rect = ed.active().unwrap().canvas_rect();
        let composite = ed.active_mut().unwrap().composite(rect).unwrap();
        let inside = ((16 * 48) + 24) * 4;
        let outside = ((16 * 48) + 44) * 4;
        assert_ne!(
            composite[inside + 3],
            0,
            "the document-space selection shows"
        );
        assert_eq!(
            composite[outside + 3], 0,
            "the rest of the moved layer is hidden (the coverage was not painted in document space)"
        );
    }

    /// Card 058: with the edit target on the mask, a Fill paints COVERAGE
    /// (one channel, selection-constrained, undoable) — the layer's pixels
    /// are never touched, and a non-Normal blend is refused rather than
    /// inventing semantics for a scalar field.
    #[test]
    fn fill_targets_the_mask_when_the_edit_target_is_the_mask() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        // Start from Hide All: coverage absent (hidden) everywhere, so white
        // reveals exactly where the fill lands.
        assert!(invoke(&mut ed, MenuAction::Mask(ui::menu::MaskOp::HideAll)).unwrap());
        // Aim at the mask — the wells swap to the mask pair (white fg).
        ed.set_edit_target_kind(crate::edit_target::EditTargetKind::Mask);
        select_rect(&mut ed, (4, 4), (12, 12));

        use compositor::TileSource as _;
        let layer_pixels = |ed: &Editor| {
            let doc = ed.active().unwrap();
            doc.document
                .layer_tiles(layer)
                .map(|m| {
                    m.iter()
                        .map(|(_, h)| doc.tiles.tile(h).map(|b| b.to_vec()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        let layer_before = layer_pixels(&ed);

        assert!(invoke(&mut ed, MenuAction::FillDialog).unwrap());
        {
            let doc = ed.active().unwrap();
            let coverage = read_mask_coverage(doc, layer, 48, 32);
            assert_eq!(
                coverage[6 * 48 + 6],
                255,
                "inside the selection the fill reveals"
            );
            assert_eq!(
                coverage[20 * 48 + 30],
                0,
                "outside the selection nothing was stored"
            );
            assert_eq!(
                layer_pixels(&ed),
                layer_before,
                "a mask fill changed no layer pixels"
            );
        }
        // One undo removes the fill.
        ed.active_mut().unwrap().undo().unwrap();
        {
            let doc = ed.active().unwrap();
            let coverage = read_mask_coverage(doc, layer, 48, 32);
            assert_eq!(coverage[6 * 48 + 6], 0, "undo restores the hidden mask");
        }

        // A non-Normal blend mode is refused on a coverage mask.
        let spec = ui::dialogs::FillSpec {
            contents: ui::dialogs::FillContents::Foreground,
            blend: layer_model::BlendMode::Multiply,
            ..Default::default()
        };
        let err = fill_selection_with(&mut ed, &spec).unwrap_err();
        assert!(
            err.contains("not meaningful on a coverage mask"),
            "the refusal names the limitation: {err}"
        );
    }

    /// Card 058: the Layer \u25b8 Layer Mask \u25b8 Invert op flips the
    /// coverage (255 \u2212 v), is one undo step, and never touches the
    /// layer's pixels.
    #[test]
    fn the_mask_invert_op_flips_coverage_and_is_undoable() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        select_rect(&mut ed, (4, 4), (12, 12));
        assert!(invoke(&mut ed, MenuAction::Mask(ui::menu::MaskOp::RevealSelection)).unwrap());
        {
            let doc = ed.active().unwrap();
            let coverage = read_mask_coverage(doc, layer, 48, 32);
            assert_eq!(coverage[6 * 48 + 6], 255);
            assert_eq!(coverage[20 * 48 + 30], 0);
        }

        assert!(invoke(&mut ed, MenuAction::Mask(ui::menu::MaskOp::Invert)).unwrap());
        {
            let doc = ed.active().unwrap();
            let coverage = read_mask_coverage(doc, layer, 48, 32);
            assert_eq!(coverage[6 * 48 + 6], 0, "the revealed area conceals");
            assert_eq!(
                coverage[20 * 48 + 30],
                255,
                "the absent (hidden) area reveals — and is now STORED, not absent"
            );
        }
        ed.active_mut().unwrap().undo().unwrap();
        {
            let doc = ed.active().unwrap();
            let coverage = read_mask_coverage(doc, layer, 48, 32);
            assert_eq!(
                coverage[6 * 48 + 6],
                255,
                "undo restores the selection reveal"
            );
            assert_eq!(coverage[20 * 48 + 30], 0);
        }
    }

    /// Card 058 (review round 1): the invert round trip on a MULTI-TILE
    /// canvas — Reveal All stores 255 over every tile, Invert must CLEAR the
    /// all-zero result tiles (an absent tile IS the all-hidden meaning), and
    /// a second Invert restores the stored 255s. The round-1 critical: the
    /// writer's clear path never ran, so Invert silently no-op'd.
    #[test]
    fn invert_round_trips_over_a_multi_tile_canvas() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = {
            let png = dir.path().join("wide.png");
            let mut rgba = vec![0u8; (300 * 64 * 4) as usize];
            for px in rgba.as_chunks_mut::<4>().0 {
                px.copy_from_slice(&[120, 40, 200, 255]);
            }
            std::fs::write(
                &png,
                raster::encode(raster::ExportFormat::Png, 300, 64, &rgba).unwrap(),
            )
            .unwrap();
            let mut ed = opened(dir.path());
            ed.open_path(&png).expect("the wide canvas opens");
            ed
        };
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        assert!(invoke(&mut ed, MenuAction::Mask(ui::menu::MaskOp::RevealAll)).unwrap());

        assert!(invoke(&mut ed, MenuAction::Mask(ui::menu::MaskOp::Invert)).unwrap());
        {
            let doc = ed.active().unwrap();
            let coverage = read_mask_coverage(doc, layer, 300, 64);
            assert!(
                coverage.iter().all(|&c| c == 0),
                "the inversion hid everything"
            );
            assert_eq!(
                doc.document.mask_tiles(layer).map(|m| m.len()),
                None,
                "all-zero result tiles are CLEARED — the store empties entirely (None = no tiles, the absent-tile meaning, not stale 255s)"
            );
        }
        // ...and the second inversion restores the stored reveal.
        assert!(invoke(&mut ed, MenuAction::Mask(ui::menu::MaskOp::Invert)).unwrap());
        {
            let doc = ed.active().unwrap();
            let coverage = read_mask_coverage(doc, layer, 300, 64);
            assert!(
                coverage.iter().all(|&c| c == 255),
                "the second inversion reveals again"
            );
        }
    }

    /// Card 058 (review round 1): BLACK hides — the fill's blend must go
    /// toward the paint's LUMINANCE, not toward white. A black fill on a
    /// revealed mask conceals; a 50%-opacity white fill lands strictly
    /// between; Gray50 lands near half.
    #[test]
    fn mask_fills_blend_toward_the_paint_value_at_the_paint_strength() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        assert!(invoke(&mut ed, MenuAction::Mask(ui::menu::MaskOp::RevealAll)).unwrap());
        ed.set_edit_target_kind(crate::edit_target::EditTargetKind::Mask);
        select_rect(&mut ed, (0, 0), (48, 32)); // the whole canvas

        // Black fg (the wells show the mask pair's black as bg; pick it).
        ed.set_foreground([0.0, 0.0, 0.0, 1.0]);
        assert!(invoke(&mut ed, MenuAction::FillDialog).unwrap());
        {
            let doc = ed.active().unwrap();
            let coverage = read_mask_coverage(doc, layer, 48, 32);
            assert_eq!(coverage[16 * 48 + 24], 0, "black hides");
        }
        ed.active_mut().unwrap().undo().unwrap();

        // 50%-opacity black on the revealed mask: coverage blends toward
        // the value (0) by the amount (0.5) — half strength, not a
        // no-op (50% WHITE on revealed would legitimately change nothing,
        // because blending toward 1.0 at any amount keeps 1.0).
        let spec = ui::dialogs::FillSpec {
            contents: ui::dialogs::FillContents::Foreground,
            blend: layer_model::BlendMode::Normal,
            opacity: 0.5,
            preserve_transparency: false,
        };
        ed.set_foreground([0.0, 0.0, 0.0, 1.0]);
        assert!(fill_selection_with(&mut ed, &spec).is_ok());
        {
            let doc = ed.active().unwrap();
            let coverage = read_mask_coverage(doc, layer, 48, 32);
            let v = coverage[16 * 48 + 24];
            assert!(
                (120..=136).contains(&v),
                "50% black on revealed lands at half coverage (got {v})"
            );
        }
        ed.active_mut().unwrap().undo().unwrap();

        // Gray50: mid-luminance (the brush's own value mapping).
        let spec = ui::dialogs::FillSpec {
            contents: ui::dialogs::FillContents::Gray50,
            blend: layer_model::BlendMode::Normal,
            opacity: 1.0,
            preserve_transparency: false,
        };
        assert!(fill_selection_with(&mut ed, &spec).is_ok());
        {
            let doc = ed.active().unwrap();
            let coverage = read_mask_coverage(doc, layer, 48, 32);
            let v = coverage[16 * 48 + 24];
            assert!(
                (100..=200).contains(&v),
                "a half-grey fill lands mid-coverage (got {v})"
            );
        }
    }

    /// Card 058 (review round 1): a fill on a TRANSFORMED mask lands where
    /// the mask displays — the canvas-space coverage is resampled through
    /// the mask pose, so a +8px unlinked mask reveals the pointer's whole
    /// canvas area, not a displaced strip.
    #[test]
    fn mask_fill_maps_through_the_mask_pose() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        assert!(invoke(&mut ed, MenuAction::Mask(ui::menu::MaskOp::HideAll)).unwrap());
        // Give the mask its own +8px transform (unlinked, moved).
        let mut mask = layer_model::LayerMask::new(layer_model::MaskId::new());
        mask.transform = Box::new(glam::Affine2::from_translation(glam::Vec2::new(8.0, 0.0)));
        ed.apply_command(editor_core::Command::SetLayerProperties {
            layer_id: layer,
            patch: editor_core::LayerPatch {
                mask: editor_core::Patch::Set(mask),
                ..Default::default()
            },
        });
        ed.set_edit_target_kind(crate::edit_target::EditTargetKind::Mask);
        select_rect(&mut ed, (0, 0), (48, 32));

        assert!(invoke(&mut ed, MenuAction::FillDialog).unwrap());
        {
            use compositor::TileSource as _;
            let doc = ed.active().unwrap();
            // Probe the STORE at the pose-mapped points: canvas (24,16) is
            // store (16,16) — revealed; the displaced store point (24,16)
            // (= canvas 32) must ALSO be revealed, because the pose maps the
            // whole canvas rect through.
            let map = doc.document.mask_tiles(layer).unwrap();
            let hash = map.get(raster::TileCoord::new(0, 0, 0)).unwrap();
            let bytes = doc.tiles.tile(hash).unwrap();
            assert_eq!(bytes[16 * 256 + 16], 255, "the mapped point reveals");
            assert_eq!(
                bytes[16 * 256 + 33],
                255,
                "the pose-shifted point reveals too"
            );
        }
        // What the user sees: every canvas pixel the fill covered shows.
        {
            let doc = ed.active().unwrap();
            let coverage = read_mask_coverage(doc, layer, 48, 32);
            assert_eq!(
                coverage[16 * 48 + 4],
                255,
                "the left edge reveals — not displaced"
            );
        }
    }

    /// Card 059 (review round 2): the pose-aware READ direction is
    /// discriminated — canvas (16,16) over a +8px unlinked mask reads store
    /// (8,16), the INVERSE mapping the compositor samples with, never the
    /// forward mapping's (24,16).
    #[test]
    fn reading_mask_coverage_maps_through_the_pose_inverse() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        assert!(invoke(&mut ed, MenuAction::Mask(ui::menu::MaskOp::HideAll)).unwrap());
        // Replace the mask with one carrying a +8px own transform.
        let mut mask = layer_model::LayerMask::new(layer_model::MaskId::new());
        mask.transform = Box::new(glam::Affine2::from_translation(glam::Vec2::new(8.0, 0.0)));
        ed.apply_command(editor_core::Command::SetLayerProperties {
            layer_id: layer,
            patch: editor_core::LayerPatch {
                mask: editor_core::Patch::Set(mask),
                ..Default::default()
            },
        });
        // Paint coverage at STORE (8,16) only (a 1px hole in an empty store).
        let doc = ed.active_mut().unwrap();
        let ts = raster::TILE_SIZE as usize;
        let mut tile = vec![0u8; ts * ts];
        tile[16 * ts + 8] = 255;
        let hash = doc.tiles.insert_bytes(tile);
        let delta = editor_core::pixels::TileDelta::new(std::iter::once(
            editor_core::pixels::TileEdit::set(raster::TileCoord::new(0, 0, 0), hash),
        ))
        .unwrap();
        editor_core::Command::PaintTiles {
            target: editor_core::pixels::PixelTarget::Mask(layer),
            delta,
        }
        .apply(&mut doc.document)
        .unwrap();

        let coverage = {
            let doc = ed.active().unwrap();
            read_mask_coverage(doc, layer, 48, 32)
        };
        // canvas (16,16) → store (8,16) through the INVERSE: revealed.
        assert_eq!(
            coverage[16 * 48 + 16],
            255,
            "the read maps canvas → store through the pose INVERSE (the compositor's direction)"
        );
        // canvas (24,16) → store (16,16): the forward mapping's answer —
        // must stay hidden.
        assert_eq!(
            coverage[16 * 48 + 24],
            0,
            "the forward mapping would light this pixel up — wrong direction"
        );
    }

    /// Card 060: the Refine Mask service bakes the dialog's parameters as
    /// ONE undoable transaction, leaves the layer's pixels untouched, and
    /// undo restores the exact baseline coverage. The baked field is exactly
    /// `selection::refine_mask` over the baseline, canvas-aligned — the
    /// same pipeline the preview uses.
    #[test]
    fn refining_a_mask_is_one_undoable_step_that_keeps_the_pixels() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        // A real half-plane mask (a synthetic edge, the card's controlled
        // case): reveal all, then hide the LEFT half via Hide Selection.
        select_rect(&mut ed, (0, 0), (24, 32));
        assert!(invoke(&mut ed, MenuAction::Mask(ui::menu::MaskOp::RevealSelection)).unwrap());
        assert!(invoke(&mut ed, MenuAction::Mask(ui::menu::MaskOp::Invert)).unwrap());
        let baseline = {
            let doc = ed.active().unwrap();
            read_mask_coverage(doc, layer, 48, 32)
        };
        assert_eq!(baseline[8 * 48 + 4], 0, "the left half is hidden");
        assert_eq!(baseline[8 * 48 + 40], 255, "the right half is revealed");
        let depth_before = ed.active().unwrap().history.undo_depth();
        let pixels_before = {
            use compositor::TileSource as _;
            let doc = ed.active().unwrap();
            doc.document
                .layer_tiles(layer)
                .map(|m| {
                    m.iter()
                        .map(|(_, hash)| doc.tiles.tile(hash).map(|b| b.to_vec()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };

        let spec = ui::dialogs::refine_mask::RefineMaskSpec {
            feather_px: 4.0,
            shift_px: 4, // expand: the edge moves left by 4
            ..Default::default()
        };
        let message = refine_mask_with(&mut ed, &spec).unwrap();
        assert!(message.contains("Refined the mask"), "{message}");

        // ONE undo step.
        assert_eq!(
            ed.active().unwrap().history.undo_depth(),
            depth_before + 1,
            "one confirmation is one history entry"
        );
        // The layer's pixels are untouched.
        {
            use compositor::TileSource as _;
            let doc = ed.active().unwrap();
            let pixels_after = doc
                .document
                .layer_tiles(layer)
                .map(|m| {
                    m.iter()
                        .map(|(_, hash)| doc.tiles.tile(hash).map(|b| b.to_vec()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            assert_eq!(pixels_after, pixels_before, "the source RGB never moved");
        }
        // The baked field is refine_mask over the baseline, canvas-aligned:
        // the +4px expand moved the edge left; the feather softened it.
        {
            let doc = ed.active().unwrap();
            let baked = read_mask_coverage(doc, layer, 48, 32);
            let baseline_mask =
                editor_core::SelectionMask::new(glam::IVec2::ZERO, 48, 32, baseline.clone())
                    .unwrap();
            let expected_full = selection::refine_mask(&baseline_mask, &spec.params()).unwrap();
            let expected = |x: usize, y: usize| {
                expected_full.coverage_at(glam::IVec2::new(x as i32, y as i32))
            };
            assert_eq!(
                baked[8 * 48 + 8],
                expected(8, 8),
                "the baked field is the pipeline's"
            );
            // The expand moved the old edge (x=24) left: x=20 was hidden, now
            // it is inside the expanded+feathered ramp's solid side... verify
            // geometrically: the baked value at the OLD edge region is above
            // the baseline's.
            assert!(
                baked[8 * 48 + 20] > baseline[8 * 48 + 20],
                "the +4px shift moved the edge left ({} > {})",
                baked[8 * 48 + 20],
                baseline[8 * 48 + 20]
            );
        }
        // Undo restores the EXACT baseline coverage.
        ed.active_mut().unwrap().undo().unwrap();
        {
            let doc = ed.active().unwrap();
            let restored = read_mask_coverage(doc, layer, 48, 32);
            assert_eq!(
                restored, baseline,
                "undo restores the baseline byte-for-byte"
            );
        }
        // Redo replays it.
        ed.active_mut().unwrap().redo().unwrap();
        {
            let doc = ed.active().unwrap();
            let again = read_mask_coverage(doc, layer, 48, 32);
            assert_ne!(
                again[8 * 48 + 20],
                baseline[8 * 48 + 20],
                "redo replays the refine"
            );
        }
    }

    /// Card 060: the dialog host REFUSES to open over a layer without a mask,
    /// and canceling an opened dialog writes nothing at all.
    #[test]
    fn the_refine_dialog_opens_only_over_a_mask_and_cancel_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        let depth = ed.active().unwrap().history.undo_depth();

        // No mask: the host refuses (the menu gates the same way).
        let mut host = crate::dialog_host::DialogHost::default();
        assert!(
            !host.open_for_menu_action(&MenuAction::RefineMask, &ed),
            "a maskless layer has nothing to refine"
        );

        // With a mask: the dialog opens over the layer's pixels + baseline.
        assert!(invoke(&mut ed, MenuAction::Mask(ui::menu::MaskOp::RevealAll)).unwrap());
        let mut host = crate::dialog_host::DialogHost::default();
        assert!(host.open_for_menu_action(&MenuAction::RefineMask, &ed));
        // Cancel = drop the host: nothing was written, nothing to undo.
        drop(host);
        assert_eq!(
            ed.active().unwrap().history.undo_depth(),
            depth + 1, // only the Reveal All from the setup
            "opening and canceling the dialog wrote no history"
        );
        let coverage = {
            let doc = ed.active().unwrap();
            read_mask_coverage(doc, layer, 48, 32)
        };
        assert!(
            coverage.iter().all(|&c| c == 255),
            "the baseline coverage survived the canceled dialog"
        );
    }

    /// Card 062 fixture: an opaque grey left half with a GREEN fringe
    /// column just inside the edge (x=23 of 48), the right half transparent,
    /// under a Reveal-All mask. The controlled colored-background fringe the
    /// card's check names.
    fn fringed_cutout(ed: &mut Editor) -> LayerId {
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        let (w, h) = (48u32, 32u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 0..h as usize {
            for x in 0..w as usize {
                let i = (y * w as usize + x) * 4;
                if x < 23 {
                    rgba[i] = 40;
                    rgba[i + 1] = 40;
                    rgba[i + 2] = 40;
                    rgba[i + 3] = 255;
                } else if x == 23 {
                    rgba[i] = 20;
                    rgba[i + 1] = 230;
                    rgba[i + 2] = 30;
                    rgba[i + 3] = 255;
                }
            }
        }
        let paint = {
            let doc = ed.active_mut().unwrap();
            pixels::write_layer(doc, layer, &rgba, "Cutout").unwrap()
        };
        ed.apply_command(paint);
        // The mask FOLLOWS the cutout edge (reveal the left 24 columns) —
        // a Reveal-All mask would have no boundary for the cleanup to find.
        select_rect(ed, (0, 0), (24, 32));
        assert!(invoke(ed, MenuAction::Mask(ui::menu::MaskOp::RevealSelection)).unwrap());
        layer
    }

    // ---- W7-H: Filter > Liquify... and Edit > Puppet Warp -----------------

    fn w7h_frame(
        host: &mut crate::dialog_host::DialogHost,
        ctx: &egui::Context,
        keys: &[egui::Key],
    ) -> crate::chrome::ChromeOutput {
        let events = keys
            .iter()
            .map(|key| egui::Event::Key {
                key: *key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
            })
            .collect();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1400.0, 900.0),
            )),
            events,
            ..Default::default()
        };
        let mut out = crate::chrome::ChromeOutput::default();
        let _ = ctx.run(input, |ctx| host.ui(ctx, None, &mut out));
        out
    }

    /// Paint `rgba` (48x32) onto the active layer and return it.
    fn w7h_paint(ed: &mut Editor, rgba: &[u8]) -> LayerId {
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        let paint = {
            let doc = ed.active_mut().unwrap();
            pixels::write_layer(doc, layer, rgba, "Fixture").unwrap()
        };
        ed.apply_command(paint);
        layer
    }

    /// The darkness-weighted mean x of row `y` of a 48-wide RGBA buffer.
    fn w7h_dark_centroid(rgba: &[u8], y: usize) -> f32 {
        let (mut sum, mut total) = (0.0f32, 0.0f32);
        for x in 0..48 {
            let dark = 255.0 - rgba[(y * 48 + x) * 4] as f32;
            sum += dark * x as f32;
            total += dark;
        }
        sum / total
    }

    /// The alpha-weighted mean y of column `x` of a 48x32 RGBA buffer.
    fn w7h_alpha_centroid(rgba: &[u8], x: usize) -> f32 {
        let (mut sum, mut total) = (0.0f32, 0.0f32);
        for y in 0..32 {
            let a = rgba[(y * 48 + x) * 4 + 3] as f32;
            sum += a * y as f32;
            total += a;
        }
        sum / total
    }

    /// W9-O: Filter > Blur Gallery > Iris Blur... is a live row that opens
    /// its dialog over the active layer; Escape writes nothing; Enter parks
    /// the blur, the pick rides `out.menu` to the `BlurGallery` arm, and it
    /// lands as ONE history entry that leaves the focus sharp and blurs the
    /// corners; undo restores the exact bytes.
    #[test]
    fn w9o_blur_gallery_opens_from_the_menu_and_lands_as_one_undo_step() {
        use filters::blur_gallery::{BlurGallery, BlurGalleryKind, IrisBlur};
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let mut rgba = vec![255u8; 48 * 32 * 4];
        for y in 0..32 {
            for x in 0..48 {
                if (x + y) % 2 == 1 {
                    let i = (y * 48 + x) * 4;
                    rgba[i..i + 3].copy_from_slice(&[0, 0, 0]);
                }
            }
        }
        let layer = w7h_paint(&mut ed, &rgba);
        let before = pixels::read_layer(ed.active().unwrap(), layer);
        let depth = ed.active().unwrap().history.undo_depth();
        let action = MenuAction::BlurGallery(BlurGalleryKind::Iris);
        let live = context(&mut ed, &Workspace::new());
        match resolve(action, &live, &ed) {
            Ok(Pick::Menu(a)) if a == action => {}
            other => panic!("Filter > Blur Gallery > Iris Blur is not a live row: {other:?}"),
        }
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let mut host = crate::dialog_host::DialogHost::default();
        let iris = BlurGallery::Iris(IrisBlur {
            center: [24.0, 16.0],
            radii: [12.0, 12.0],
            rotation_deg: 0.0,
            focus: 0.5,
            blur: 4.0,
        });

        // Cancel changes nothing.
        assert!(host.open_for_menu_action(&action, &ed));
        host.active_blur_gallery_dialog_for_test()
            .set_gallery(iris.clone());
        let _ = w7h_frame(&mut host, &ctx, &[]);
        let out = w7h_frame(&mut host, &ctx, &[egui::Key::Escape]);
        assert!(!host.is_open() && out.menu.is_empty());
        assert!(crate::dialog_host::take_confirmed_blur_gallery().is_none());
        assert!(perform(action, &mut ed).is_err());
        assert_eq!(ed.active().unwrap().history.undo_depth(), depth);
        assert_eq!(pixels::read_layer(ed.active().unwrap(), layer), before);

        // Confirm lands one step.
        assert!(host.open_for_menu_action(&action, &ed));
        host.active_blur_gallery_dialog_for_test()
            .set_gallery(iris.clone());
        let _ = w7h_frame(&mut host, &ctx, &[]);
        let out = w7h_frame(&mut host, &ctx, &[egui::Key::Enter]);
        assert!(!host.is_open(), "Enter closed the dialog");
        assert_eq!(out.menu, vec![action]);
        let message = perform(action, &mut ed).unwrap();
        assert!(message.contains("Iris Blur"), "{message}");
        assert_eq!(
            ed.active().unwrap().history.undo_depth(),
            depth + 1,
            "one confirmation is one history entry"
        );
        let after = pixels::read_layer(ed.active().unwrap(), layer);
        // The focus (within 6 px of the centre) is the source's exactly.
        for y in 12..20 {
            for x in 20..28 {
                let i = (y * 48 + x) * 4;
                assert_eq!(after[i..i + 4], before[i..i + 4], "({x},{y}) stayed sharp");
            }
        }
        // The corner's checkerboard is blurred toward grey.
        let spread = |buf: &[u8]| {
            let mut vals = Vec::new();
            for y in 0..6 {
                for x in 0..6 {
                    vals.push(buf[(y * 48 + x) * 4] as f32);
                }
            }
            let max = vals.iter().cloned().fold(f32::MIN, f32::max);
            let min = vals.iter().cloned().fold(f32::MAX, f32::min);
            max - min
        };
        assert!(spread(&before) > 200.0);
        assert!(
            spread(&after) < 40.0,
            "the corner blurred: {} -> {}",
            spread(&before),
            spread(&after)
        );
        // A second perform has nothing parked: no second entry.
        assert!(perform(action, &mut ed).is_err());
        assert_eq!(ed.active().unwrap().history.undo_depth(), depth + 1);
        ed.active_mut().unwrap().undo().unwrap();
        assert_eq!(pixels::read_layer(ed.active().unwrap(), layer), before);
    }

    /// W7-H: Filter > Liquify... is a live row that opens its dialog; Escape
    /// writes nothing; a stroke confirmed with Enter rides the parked-spec
    /// road to the `Liquify` arm and lands as ONE history entry that moves
    /// the ink in the stroke's direction, and undo restores the exact bytes.
    #[test]
    fn w7h_liquify_opens_from_the_menu_and_lands_as_one_undo_step() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let mut rgba = vec![255u8; 48 * 32 * 4];
        for y in 0..32 {
            for x in 20..22 {
                let i = (y * 48 + x) * 4;
                rgba[i..i + 3].copy_from_slice(&[0, 0, 0]);
            }
        }
        let layer = w7h_paint(&mut ed, &rgba);
        let before = pixels::read_layer(ed.active().unwrap(), layer);
        let depth = ed.active().unwrap().history.undo_depth();
        let live = context(&mut ed, &Workspace::new());
        match resolve(MenuAction::Liquify, &live, &ed) {
            Ok(Pick::Menu(MenuAction::Liquify)) => {}
            other => panic!("Filter > Liquify... is not a live row: {other:?}"),
        }
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let mut host = crate::dialog_host::DialogHost::default();
        let brush = filters::liquify::LiquifyBrush {
            size: 24.0,
            pressure: 1.0,
            density: 0.5,
        };

        // Cancel changes nothing.
        assert!(host.open_for_menu_action(&MenuAction::Liquify, &ed));
        host.active_liquify_dialog_for_test().set_brush(brush);
        host.active_liquify_dialog_for_test()
            .stroke([21.0, 16.0], [30.0, 16.0]);
        let _ = w7h_frame(&mut host, &ctx, &[]);
        let out = w7h_frame(&mut host, &ctx, &[egui::Key::Escape]);
        assert!(!host.is_open() && out.menu.is_empty());
        assert!(crate::dialog_host::take_confirmed_liquify().is_none());
        assert!(perform(MenuAction::Liquify, &mut ed).is_err());
        assert_eq!(ed.active().unwrap().history.undo_depth(), depth);
        assert_eq!(pixels::read_layer(ed.active().unwrap(), layer), before);

        // Confirm lands one step.
        assert!(host.open_for_menu_action(&MenuAction::Liquify, &ed));
        host.active_liquify_dialog_for_test().set_brush(brush);
        host.active_liquify_dialog_for_test()
            .stroke([21.0, 16.0], [30.0, 16.0]);
        let _ = w7h_frame(&mut host, &ctx, &[]);
        let out = w7h_frame(&mut host, &ctx, &[egui::Key::Enter]);
        assert!(!host.is_open(), "Enter closed the dialog");
        assert_eq!(out.menu, vec![MenuAction::Liquify]);
        let message = perform(MenuAction::Liquify, &mut ed).unwrap();
        assert!(message.contains("Liquify"), "{message}");
        assert_eq!(
            ed.active().unwrap().history.undo_depth(),
            depth + 1,
            "one confirmation is one history entry"
        );
        let after = pixels::read_layer(ed.active().unwrap(), layer);
        let (c0, c1) = (
            w7h_dark_centroid(&before, 16),
            w7h_dark_centroid(&after, 16),
        );
        assert!(c1 > c0 + 2.0, "the bar moved with the stroke: {c0} -> {c1}");
        assert_eq!(
            after[..48 * 4],
            before[..48 * 4],
            "a row outside the brush is untouched"
        );
        // A second perform has nothing parked: no second entry.
        assert!(perform(MenuAction::Liquify, &mut ed).is_err());
        assert_eq!(ed.active().unwrap().history.undo_depth(), depth + 1);
        ed.active_mut().unwrap().undo().unwrap();
        assert_eq!(pixels::read_layer(ed.active().unwrap(), layer), before);
    }

    /// W10-D: Filter > Filter Gallery with a stacked effect list, opened from
    /// its menu row through the real host and confirmed with Enter, rides the
    /// parked-spec road to the `FilterGallery` arm and lands as ONE history
    /// entry holding exactly the stack's pixels; undo restores the bytes.
    #[test]
    fn w10d_the_gallery_effect_list_lands_as_one_undo_step() {
        use filters::gallery_sets::{GalleryEffect, GallerySet};
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        let before = pixels::read_layer(ed.active().unwrap(), layer);
        let depth = ed.active().unwrap().history.undo_depth();
        let live = context(&mut ed, &Workspace::new());
        assert!(matches!(
            resolve(MenuAction::FilterGallery, &live, &ed),
            Ok(Pick::Menu(MenuAction::FilterGallery))
        ));
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let mut host = crate::dialog_host::DialogHost::default();
        assert!(host.open_for_menu_action(&MenuAction::FilterGallery, &ed));
        let gallery = host.active_filter_gallery_for_test();
        gallery.set_tab(ui::dialogs::GalleryTab::Set(GallerySet::Artistic));
        gallery.pick_effect(GalleryEffect::Cutout);
        gallery.new_layer();
        gallery.pick_effect(GalleryEffect::Craquelure);
        let stack = gallery.stack().clone();
        assert_eq!(stack.layers.len(), 2);
        let _ = w7h_frame(&mut host, &ctx, &[]);
        let out = w7h_frame(&mut host, &ctx, &[egui::Key::Enter]);
        assert!(!host.is_open(), "Enter closed the gallery");
        assert_eq!(out.menu, vec![MenuAction::FilterGallery]);
        let message = perform(MenuAction::FilterGallery, &mut ed).unwrap();
        assert!(message.contains("Filter Gallery"), "{message}");
        assert_eq!(
            ed.active().unwrap().history.undo_depth(),
            depth + 1,
            "the whole list is one history entry"
        );
        let after = pixels::read_layer(ed.active().unwrap(), layer);
        let expected = stack
            .apply(&filters::FilterBuffer::from_rgba8(48, 32, &before).unwrap())
            .to_rgba8();
        assert_eq!(after, expected, "the layer holds exactly the stacked list");
        assert!(perform(MenuAction::FilterGallery, &mut ed).is_err());
        ed.active_mut().unwrap().undo().unwrap();
        assert_eq!(pixels::read_layer(ed.active().unwrap(), layer), before);
    }

    /// W10-D: Filter > Distort > Displace over a pixel layer opens the
    /// external-map dialog; its map comes from another open document or from
    /// a file (Load...), and Enter lands ONE history entry that shifts the
    /// pixels by the map: 255 red is +scale in x, 0 red is -scale.
    #[test]
    fn w10d_displace_reads_an_open_document_or_a_file_as_its_map() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let map_png = |name: &str, red: u8| {
            let rgba: Vec<u8> = (0..48 * 32).flat_map(|_| [red, 128, 128, 255]).collect();
            let path = dir.path().join(name);
            std::fs::write(
                &path,
                raster::encode(raster::ExportFormat::Png, 48, 32, &rgba).unwrap(),
            )
            .unwrap();
            path
        };
        // A second open document is the map; the probe stays the target.
        ed.open_path(&map_png("right.png", 255)).unwrap();
        ed.activate(0).unwrap();
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        let before = pixels::read_layer(ed.active().unwrap(), layer);
        let depth = ed.active().unwrap().history.undo_depth();
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let mut host = crate::dialog_host::DialogHost::default();
        let displace = MenuAction::Filter(ui::menu::FilterId::Displace);
        assert!(host.open_for_menu_action(&displace, &ed));
        let dialog = host.active_displace_map_for_test();
        assert_eq!(dialog.maps().len(), 2, "both open documents are offered");
        assert_eq!(dialog.maps()[dialog.chosen()].name, "right.png");
        dialog.set_scale(4.0, 0.0);
        let _ = w7h_frame(&mut host, &ctx, &[]);
        let out = w7h_frame(&mut host, &ctx, &[egui::Key::Enter]);
        assert_eq!(out.menu, vec![displace]);
        perform(displace, &mut ed).unwrap();
        assert_eq!(ed.active().unwrap().history.undo_depth(), depth + 1);
        let after = pixels::read_layer(ed.active().unwrap(), layer);
        let px =
            |buf: &[u8], x: usize, y: usize| buf[(y * 48 + x) * 4..(y * 48 + x) * 4 + 4].to_vec();
        assert_eq!(px(&after, 10, 5), px(&before, 14, 5), "shifted +4 px in x");
        ed.active_mut().unwrap().undo().unwrap();
        assert_eq!(pixels::read_layer(ed.active().unwrap(), layer), before);

        // A file loaded with Load... joins the list, is chosen, and drives the
        // shift the other way.
        assert!(host.open_for_menu_action(&displace, &ed));
        crate::dialog_host::PICKED_DISPLACE_MAP_FOR_TEST
            .with(|p| *p.borrow_mut() = Some(map_png("left.png", 0)));
        host.active_displace_map_for_test().request_map_file();
        let _ = w7h_frame(&mut host, &ctx, &[]);
        let dialog = host.active_displace_map_for_test();
        assert_eq!(dialog.maps().len(), 3, "the file joined the list");
        assert_eq!(dialog.maps()[dialog.chosen()].name, "left.png");
        dialog.set_scale(4.0, 0.0);
        let out = w7h_frame(&mut host, &ctx, &[egui::Key::Enter]);
        assert_eq!(out.menu, vec![displace]);
        perform(displace, &mut ed).unwrap();
        let after = pixels::read_layer(ed.active().unwrap(), layer);
        assert_eq!(px(&after, 14, 5), px(&before, 10, 5), "shifted -4 px in x");
        // With nothing parked the row still runs at its defaults.
        assert!(perform(displace, &mut ed).is_ok());
    }

    /// W10-D: Filter > Vanishing Point opens over the layer with the
    /// clipboard to paste; a paste into a perspective plane confirmed with
    /// Enter lands ONE history entry, and the pasted rectangle's corners sit
    /// where the plane's homography puts them.
    #[test]
    fn w10d_vanishing_point_pastes_through_the_planes_homography_as_one_undo_step() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        ed.set_clipboard(crate::editor::Clipboard {
            width: 8,
            height: 8,
            rgba8: [255u8, 0, 0, 255].repeat(64),
            origin: (0, 0),
        });
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        let before = pixels::read_layer(ed.active().unwrap(), layer);
        let depth = ed.active().unwrap().history.undo_depth();
        let live = context(&mut ed, &Workspace::new());
        assert!(matches!(
            resolve(MenuAction::VanishingPoint, &live, &ed),
            Ok(Pick::Menu(MenuAction::VanishingPoint))
        ));
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let mut host = crate::dialog_host::DialogHost::default();
        assert!(host.open_for_menu_action(&MenuAction::VanishingPoint, &ed));
        let vp = host.active_vanishing_point_for_test();
        assert!(vp.has_paste(), "the clipboard is what Paste lays down");
        for (i, p) in [[16.0, 4.0], [32.0, 4.0], [46.0, 30.0], [2.0, 30.0]]
            .into_iter()
            .enumerate()
        {
            vp.set_corner(i, p);
        }
        vp.set_mode(ui::dialogs::VanishingMode::Paste);
        let plane = vp.plane();
        assert!(vp.paste_at(plane.to_image([0.5, 0.5]).unwrap()));
        let filters::vanishing_point::VanishingOp::Paste { rect, .. } = vp.ops()[0].clone() else {
            panic!("not a paste");
        };
        let _ = w7h_frame(&mut host, &ctx, &[]);
        let out = w7h_frame(&mut host, &ctx, &[egui::Key::Enter]);
        assert_eq!(out.menu, vec![MenuAction::VanishingPoint]);
        let message = perform(MenuAction::VanishingPoint, &mut ed).unwrap();
        assert!(message.contains("Vanishing Point"), "{message}");
        assert_eq!(ed.active().unwrap().history.undo_depth(), depth + 1);
        let after = pixels::read_layer(ed.active().unwrap(), layer);
        let red = |x: f32, y: f32| {
            let i = (y as usize * 48 + x as usize) * 4;
            after[i..i + 4].to_vec()
        };
        let inset = 0.03;
        for uv in [
            [rect[0] + inset, rect[1] + inset],
            [rect[2] - inset, rect[1] + inset],
            [rect[2] - inset, rect[3] - inset],
            [rect[0] + inset, rect[3] - inset],
        ] {
            let p = plane.to_image(uv).unwrap();
            assert_eq!(
                red(p[0], p[1]),
                vec![255, 0, 0, 255],
                "corner {uv:?} at {p:?}"
            );
        }
        // Off the plane: untouched.
        assert_eq!(after[..4], before[..4]);
        assert!(perform(MenuAction::VanishingPoint, &mut ed).is_err());
        ed.active_mut().unwrap().undo().unwrap();
        assert_eq!(pixels::read_layer(ed.active().unwrap(), layer), before);
    }

    /// W7-H: Edit > Puppet Warp is a live row that opens over the layer's
    /// ink; confirming with the pins unmoved changes nothing and writes no
    /// history; dragging one pin and pressing Enter lands ONE history entry
    /// that moves the ink near that pin more than the ink by the held pin.
    #[test]
    fn w7h_puppet_warp_opens_from_the_menu_and_lands_as_one_undo_step() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let mut rgba = vec![0u8; 48 * 32 * 4];
        for y in 12..20 {
            for x in 6..42 {
                let i = (y * 48 + x) * 4;
                rgba[i..i + 4].copy_from_slice(&[200, 80, 40, 255]);
            }
        }
        let layer = w7h_paint(&mut ed, &rgba);
        let before = pixels::read_layer(ed.active().unwrap(), layer);
        let depth = ed.active().unwrap().history.undo_depth();
        let live = context(&mut ed, &Workspace::new());
        match resolve(MenuAction::PuppetWarp, &live, &ed) {
            Ok(Pick::Menu(MenuAction::PuppetWarp)) => {}
            other => panic!("Edit > Puppet Warp is not a live row: {other:?}"),
        }
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let mut host = crate::dialog_host::DialogHost::default();

        // Pins placed but not moved: the identity, refused with no history.
        assert!(host.open_for_menu_action(&MenuAction::PuppetWarp, &ed));
        let dialog = host.active_puppet_warp_dialog_for_test();
        assert!(dialog.add_pin([8.0, 16.0]).is_some());
        assert!(dialog.add_pin([40.0, 16.0]).is_some());
        let _ = w7h_frame(&mut host, &ctx, &[]);
        let out = w7h_frame(&mut host, &ctx, &[egui::Key::Enter]);
        assert_eq!(out.menu, vec![MenuAction::PuppetWarp]);
        assert!(perform(MenuAction::PuppetWarp, &mut ed).is_err());
        assert_eq!(ed.active().unwrap().history.undo_depth(), depth);
        assert_eq!(pixels::read_layer(ed.active().unwrap(), layer), before);

        // Drag the right pin down; Enter commits one step.
        assert!(host.open_for_menu_action(&MenuAction::PuppetWarp, &ed));
        let dialog = host.active_puppet_warp_dialog_for_test();
        dialog.add_pin([8.0, 16.0]).unwrap();
        let right = dialog.add_pin([40.0, 16.0]).unwrap();
        let at = dialog.pins()[right].at;
        dialog.move_pin(right, [at[0], at[1] + 8.0]);
        let _ = w7h_frame(&mut host, &ctx, &[]);
        let out = w7h_frame(&mut host, &ctx, &[egui::Key::Enter]);
        assert!(!host.is_open());
        assert_eq!(out.menu, vec![MenuAction::PuppetWarp]);
        perform(MenuAction::PuppetWarp, &mut ed).unwrap();
        assert_eq!(
            ed.active().unwrap().history.undo_depth(),
            depth + 1,
            "one commit is one history entry"
        );
        let after = pixels::read_layer(ed.active().unwrap(), layer);
        let near = w7h_alpha_centroid(&after, 38) - w7h_alpha_centroid(&before, 38);
        let far = w7h_alpha_centroid(&after, 9) - w7h_alpha_centroid(&before, 9);
        assert!(
            near > 3.0 && near > far.abs() * 2.0,
            "near shift {near}, far shift {far}"
        );
        ed.active_mut().unwrap().undo().unwrap();
        assert_eq!(pixels::read_layer(ed.active().unwrap(), layer), before);

        // A layer with no ink opens no dialog, and the row says why.
        let empty = vec![0u8; 48 * 32 * 4];
        w7h_paint(&mut ed, &empty);
        assert!(!host.open_for_menu_action(&MenuAction::PuppetWarp, &ed));
        assert_eq!(
            perform(MenuAction::PuppetWarp, &mut ed).unwrap_err(),
            "The active layer has no ink to pin"
        );
    }

    /// Card 062: the fringe cleanup reduces the known colored-background
    /// fringe, keeps the interior's exact bytes and the coverage untouched,
    /// and is one undoable step whose undo restores the original RGB.
    #[test]
    fn defringe_reduces_the_boundary_fringe_in_one_undoable_step() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let layer = fringed_cutout(&mut ed);
        let depth_before = ed.active().unwrap().history.undo_depth();
        let coverage_before = {
            let doc = ed.active().unwrap();
            read_mask_coverage(doc, layer, 48, 32)
        };

        let spec = ui::dialogs::defringe::DefringeSpec {
            radius_px: 3,
            strength: 1.0,
            ..Default::default()
        };
        let message = defringe_with(&mut ed, &spec).unwrap();
        assert!(message.contains("Removed color fringe"), "{message}");

        // ONE undo step.
        assert_eq!(
            ed.active().unwrap().history.undo_depth(),
            depth_before + 1,
            "one confirmation is one history entry"
        );
        let after = pixels::read_layer(ed.active().unwrap(), layer);
        let at = |x: usize, y: usize| (y * 48 + x) * 4;
        // The fringe pixel is pulled to the interior ink exactly (full
        // strength; its whole interior window is the flat grey).
        assert_eq!(after[at(23, 4)], 40);
        assert_eq!(after[at(23, 4) + 1], 40, "the green fringe is gone");
        assert_eq!(after[at(23, 4) + 3], 255, "alpha untouched");
        // Interior away from the boundary keeps its exact bytes.
        assert_eq!(
            after[at(10, 4)..at(10, 4) + 4],
            [40, 40, 40, 255],
            "the interior does not move"
        );
        // The mask coverage is untouched — a colour cleanup never regrades.
        let coverage_after = {
            let doc = ed.active().unwrap();
            read_mask_coverage(doc, layer, 48, 32)
        };
        assert_eq!(coverage_after, coverage_before);

        // Undo restores the exact original RGB.
        ed.active_mut().unwrap().undo().unwrap();
        let restored = pixels::read_layer(ed.active().unwrap(), layer);
        assert_eq!(restored[at(23, 4) + 1], 230, "undo brings the fringe back");
        assert_eq!(restored[at(10, 4)..at(10, 4) + 4], [40, 40, 40, 255]);
    }

    /// Card 062 gates: a maskless layer refuses (the fringe lives where the
    /// MASK says the edge is), and a no-op parameter set refuses without
    /// touching history.
    #[test]
    fn defringe_refuses_a_maskless_layer_and_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let layer = fringed_cutout(&mut ed);
        // Strip the mask.
        assert!(invoke(&mut ed, MenuAction::Mask(ui::menu::MaskOp::Delete)).unwrap());
        let depth = ed.active().unwrap().history.undo_depth();
        let spec = ui::dialogs::defringe::DefringeSpec {
            radius_px: 3,
            strength: 1.0,
            ..Default::default()
        };
        assert_eq!(
            defringe_with(&mut ed, &spec).unwrap_err(),
            "The layer has no mask"
        );
        // Re-attach the mask (Reveal All) and ask for the identity: refused.
        assert!(invoke(&mut ed, MenuAction::Mask(ui::menu::MaskOp::RevealAll)).unwrap());
        // Restore the cutout pixels (mask delete/attach left them alone, but
        // Reveal All may have re-created coverage only).
        let identity = ui::dialogs::defringe::DefringeSpec::default();
        assert!(defringe_with(&mut ed, &identity).is_err());
        assert_eq!(
            ed.active().unwrap().history.undo_depth(),
            depth + 1, // the Reveal All only
            "a refused cleanup writes no history"
        );
        let _ = layer;
    }

    /// Card 062 (host path): the dialog opens over the RAW layer pixels —
    /// seeded so the preview's cleaned content differs from the raw input at
    /// the fringe column while the coverage stays the baseline.
    #[test]
    fn the_defringe_dialog_opens_over_the_raw_content_and_confirms_one_step() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let _layer = fringed_cutout(&mut ed);
        // (Maskless refusal is covered by defringe_refuses_a_maskless_layer_and_a_no_op
        // and the ui gate test; the fixture's mask is required here.)
        // The dialog opens over the raw content: the fringe column's green
        // is visible in the dialog's own content seam.
        let mut host = crate::dialog_host::DialogHost::default();
        assert!(host.open_for_menu_action(&MenuAction::RemoveColorFringe, &ed));
        let raw = host
            .active_defringe_dialog_for_test()
            .content_pixel_for_test(23, 4);
        assert_eq!(
            raw,
            [20, 230, 30, 255],
            "the raw fringe pixel, not a masked blend"
        );
        // A non-identity spec confirms to the Defringe action (the shell's
        // DialogAction::Defringe arm routes it to defringe_with, whose
        // end-to-end behaviour the first test pins).
        host.active_defringe_dialog_for_test().set_spec_for_test(
            ui::dialogs::defringe::DefringeSpec {
                radius_px: 3,
                strength: 1.0,
                ..Default::default()
            },
        );
        use ui::dialogs::Dialog as _;
        match host.active_defringe_dialog_for_test().confirm() {
            Some(ui::dialogs::DialogAction::Defringe(spec)) => {
                assert_eq!(spec.radius_px, 3);
                assert_eq!(spec.strength, 1.0);
            }
            other => panic!("the confirmation carries the spec: {other:?}"),
        }
        drop(host);
    }

    /// Card 060 (review round 2): the HOST-fed path — the dialog's content
    /// must be the layer's RAW pixels, not the masked composite. A positive
    /// expand REVEALS ink where the baseline was fully hidden; under the
    /// round-1 defect (masked `layer_pixels` as content) that area showed the
    /// backdrop instead.
    #[test]
    fn the_refine_dialog_previews_expansion_over_the_raw_content() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        // Baseline: everything hidden on the LEFT half (a controlled edge).
        select_rect(&mut ed, (0, 0), (24, 32));
        assert!(invoke(&mut ed, MenuAction::Mask(ui::menu::MaskOp::RevealSelection)).unwrap());
        assert!(invoke(&mut ed, MenuAction::Mask(ui::menu::MaskOp::Invert)).unwrap());

        // Open the dialog the way the shell does.
        let mut host = crate::dialog_host::DialogHost::default();
        assert!(host.open_for_menu_action(&MenuAction::RefineMask, &ed));
        let dialog = host.active_refine_mask_dialog_for_test();
        let probe = dialog.content_pixel_for_test(22, 8);
        let ink = probe[0];
        assert_eq!(probe[3], 255, "the fixture's layer is opaque at the probe");

        // Expand +4: the refined coverage covers part of the concealed half,
        // so the preview must show INK there (over the black backdrop).
        dialog.set_spec_for_test(ui::dialogs::refine_mask::RefineMaskSpec {
            shift_px: 4,
            background: ui::dialogs::refine_mask::PreviewBackground::Black,
            ..Default::default()
        });
        let (rgba, pw, _ph) = dialog.preview_sized().unwrap();
        // The preview's own scale: canvas x=10 maps to preview x=10 (48 < 256).
        assert_eq!(pw, 48);
        let coverage = dialog.refined_coverage().unwrap();
        // The baseline hid x<24; the +4px expand moved the edge LEFT to ~20,
        // so canvas x=20..24 gained coverage.
        let c = f32::from(coverage[8 * 48 + 22]) / 255.0;
        assert!(c > 0.0, "the expand revealed part of the concealed half");
        // Black backdrop: the preview pixel is the raw ink scaled by c.
        let expected = (f32::from(ink) * c).round() as u8;
        let got = rgba[(8 * pw as usize + 22) * 4];
        assert_eq!(
            got, expected,
            "the preview shows the RAW content pixel, not the backdrop"
        );
        assert!(
            got > 0,
            "under the round-1 defect this pixel was the black backdrop"
        );
    }

    /// Card 052's host-bound check, the repeatable form of "a screenshot can"
    /// be pasted": seed the OS clipboard from any other application (or take
    /// a screenshot), then run
    /// `cargo test -p app-shell --lib paste_takes_a_foreign -- --ignored --nocapture`.
    /// The paste must route the FOREIGN payload through full-source placement.
    #[test]
    #[ignore = "host-bound: reads whatever image the OS clipboard currently holds"]
    fn paste_takes_a_foreign_os_clipboard_image_through_real_placement() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        // Real OS clipboard by construction; a document to paste into.
        ed.open_path(&probe_png(dir.path(), 48, 32))
            .expect("the probe opens");
        assert!(invoke(&mut ed, MenuAction::Paste).unwrap());
        let open = ed.active().unwrap();
        let pasted = open.document.active_layer().unwrap();
        assert!(
            matches!(
                open.document.layers.get(pasted).unwrap().kind,
                layer_model::LayerKind::SmartObject(_)
            ),
            "the foreign image pasted as a placed smart object"
        );
        println!("status: {:?}", ed.status());
    }

    #[test]
    fn the_fixed_transforms_flip_the_active_layer_and_are_undoable() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        let before = pixels::read_layer(ed.active().unwrap(), layer);

        assert!(invoke(
            &mut ed,
            MenuAction::Transform(ui::menu::TransformOp::FlipHorizontal)
        )
        .unwrap());
        let after = pixels::read_layer(ed.active().unwrap(), layer);
        let w = 48usize;
        for y in 0..32usize {
            for x in 0..w {
                let s = (y * w + x) * 4;
                let d = (y * w + (w - 1 - x)) * 4;
                assert_eq!(
                    &after[d..d + 4],
                    &before[s..s + 4],
                    "the flip did not mirror ({x}, {y})"
                );
            }
        }
        ed.dispatch(Action::Undo).expect("undo");
        assert_eq!(pixels::read_layer(ed.active().unwrap(), layer), before);
    }

    #[test]
    fn flatten_replaces_every_layer_with_one_that_holds_the_composite() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = with_two_layers(dir.path());
        assert_eq!(ed.active().unwrap().document.layers.len(), 2);
        let before = digest(&ed);

        assert!(invoke(&mut ed, MenuAction::FlattenImage).unwrap());
        let doc = ed.active().unwrap();
        assert_eq!(
            doc.document.layers.len(),
            1,
            "flatten left more than one layer"
        );
        let id = doc.document.layers.root()[0];
        assert!(
            doc.document.layer_tiles(id).is_some(),
            "the flattened layer has no pixels"
        );
        // One transaction, so one undo brings both layers back.
        ed.dispatch(Action::Undo).expect("undo");
        assert_eq!(digest(&ed), before, "flatten is not one undoable step");
    }

    #[test]
    fn grouping_and_ungrouping_move_the_layer_through_the_tree() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = with_two_layers(dir.path());
        let active = ed.active().unwrap().document.active_layer().unwrap();

        assert!(invoke(&mut ed, MenuAction::GroupLayers).unwrap());
        let doc = &ed.active().unwrap().document;
        let parent = doc
            .layers
            .parent_of(active)
            .expect("the layer has a parent now");
        assert!(doc.layers.get(parent).unwrap().is_group());

        // Ungroup acts on the group, so point the cursor at it first — which is
        // exactly what a user does by clicking the group's row.
        ed.set_active_layer(parent);
        assert!(invoke(&mut ed, MenuAction::UngroupLayers).unwrap());
        let doc = &ed.active().unwrap().document;
        assert!(!doc.layers.contains(parent), "the group survived");
        assert!(doc.layers.contains(active), "ungroup lost the child");
        assert_eq!(doc.layers.parent_of(active), None);
    }

    #[test]
    fn an_adjustment_that_is_not_the_identity_is_applied_to_the_pixels() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        let before = pixels::read_layer(ed.active().unwrap(), layer);

        assert!(invoke(
            &mut ed,
            MenuAction::ApplyAdjustment(ui::menu::AdjustmentId::Invert)
        )
        .unwrap());
        let after = pixels::read_layer(ed.active().unwrap(), layer);
        assert_ne!(after, before);
        // Inverting twice is the identity, which is a property rather than a
        // number and therefore the same on every libm.
        assert!(invoke(
            &mut ed,
            MenuAction::ApplyAdjustment(ui::menu::AdjustmentId::Invert)
        )
        .unwrap());
        let twice = pixels::read_layer(ed.active().unwrap(), layer);
        let worst = twice
            .iter()
            .zip(&before)
            .map(|(a, b)| (*a as i32 - *b as i32).abs())
            .max()
            .unwrap();
        assert!(worst <= 2, "invert twice moved a channel by {worst}");
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

    /// The whole composite of the active document, straight-alpha RGBA8.
    fn composite(ed: &mut Editor) -> Vec<u8> {
        let doc = ed.active_mut().unwrap();
        let rect = doc.canvas_rect();
        doc.composite(rect).unwrap()
    }

    /// Open `id`'s dialog from the menu bar exactly as a click does, then run
    /// the frames a user would: one to settle, one with `key`, through the
    /// real `Chrome::ui`. Returns that frame's output; the caller performs
    /// `out.menu` the way the shell does.
    fn drive_adjustment_dialog(
        ed: &mut Editor,
        id: ui::menu::AdjustmentId,
        edit: impl FnOnce(&mut ui::dialogs::AdjustmentDialog),
        key_pressed: egui::Key,
    ) -> ChromeOutput {
        let mut chrome = crate::chrome::Chrome::new();
        let menu_ctx = context(ed, chrome.workspace());
        let intent =
            resolve_intent(MenuAction::ApplyAdjustment(id), &menu_ctx, ed).expect("enabled");
        let mut out = ChromeOutput::default();
        chrome.menu_click(intent, ed, &mut out);
        assert!(chrome.dialog_open(), "{id:?} opened no dialog");
        assert!(out.is_empty(), "opening {id:?} produced {out:?}");
        edit(
            chrome
                .dialogs_for_test()
                .active_adjustment_dialog_for_test(),
        );
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let _ = ctx.run(raw_input(Vec::new()), |ctx| {
            let _ = chrome.ui(ctx, ed);
        });
        assert!(
            chrome.dialog_open(),
            "the dialog closed on a frame with no key"
        );
        let mut out = ChromeOutput::default();
        let _ = ctx.run(raw_input(vec![key(key_pressed)]), |ctx| {
            out = chrome.ui(ctx, ed);
        });
        assert!(
            !chrome.dialog_open(),
            "{key_pressed:?} did not close the dialog"
        );
        out
    }

    #[test]
    fn every_adjustment_row_is_enabled_with_a_document_and_needs_one() {
        // The finding: `unavailable_reason` greyed ten of the fifteen. With a
        // pixel layer under the pointer every row resolves; with no document
        // the menu's own gate is the one refusal left.
        let dir = tempfile::tempdir().unwrap();
        let mut ed = with_two_layers(dir.path());
        let menu_ctx = context(&mut ed, &Workspace::new());
        for id in ui::menu::AdjustmentId::ALL {
            let action = MenuAction::ApplyAdjustment(*id);
            assert_eq!(unavailable_reason(action), None, "{id:?} is still greyed");
            assert!(
                resolve_intent(action, &menu_ctx, &ed).is_ok(),
                "{id:?} does not resolve with a pixel layer active"
            );
        }
        let mut empty = editor(dir.path());
        let menu_ctx = context(&mut empty, &Workspace::new());
        for id in ui::menu::AdjustmentId::ALL {
            let action = MenuAction::ApplyAdjustment(*id);
            assert!(
                resolve_intent(action, &menu_ctx, &empty).is_err(),
                "{id:?} resolved with no document open"
            );
        }
    }

    #[test]
    fn every_adjustment_row_opens_its_dialog_from_the_menu_bar_over_two_layers() {
        let dir = tempfile::tempdir().unwrap();
        // Desaturate and Equalize ask nothing (W4-E); their click is covered
        // by `desaturate_applies_on_the_click_...` and `equalize_applies_...`.
        for id in ui::menu::AdjustmentId::ALL
            .iter()
            .filter(|id| id.has_dialog())
        {
            let mut ed = with_two_layers(dir.path());
            let before = digest(&ed);
            let mut chrome = crate::chrome::Chrome::new();
            let menu_ctx = context(&mut ed, chrome.workspace());
            let intent = resolve_intent(MenuAction::ApplyAdjustment(*id), &menu_ctx, &ed)
                .expect("enabled above");
            let mut out = ChromeOutput::default();
            chrome.menu_click(intent, &ed, &mut out);
            assert!(
                chrome.dialog_open(),
                "{id:?} opened no dialog from the menu bar"
            );
            assert!(
                out.is_empty(),
                "{id:?} did something besides opening: {out:?}"
            );
            // And the opened dialog draws, in both themes, without panicking.
            for theme in design::Theme::ALL {
                let ctx = egui::Context::default();
                design::apply_theme(&ctx, *theme);
                let _ = ctx.run(raw_input(Vec::new()), |ctx| {
                    let _ = chrome.ui(ctx, &mut ed);
                });
            }
            assert!(chrome.dialog_open(), "{id:?} closed itself while drawing");
            assert_eq!(digest(&ed), before, "{id:?}: opening touched the document");
        }
    }

    #[test]
    fn a_confirmed_brightness_contrast_dialog_bakes_one_undoable_step() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = with_two_layers(dir.path());
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        let before_pixels = pixels::read_layer(ed.active().unwrap(), layer);
        let before_composite = composite(&mut ed);
        let depth = ed.active().unwrap().history_depth();

        let out = drive_adjustment_dialog(
            &mut ed,
            ui::menu::AdjustmentId::BrightnessContrast,
            |dialog| {
                assert!(
                    dialog.set_kind(layer_model::AdjustmentKind::BrightnessContrast {
                        brightness: 0.5,
                        contrast: 0.0,
                    })
                );
            },
            egui::Key::Enter,
        );
        assert_eq!(
            out.menu,
            vec![MenuAction::ApplyAdjustment(
                ui::menu::AdjustmentId::BrightnessContrast
            )],
            "the confirmation did not ride the menu channel: {out:?}"
        );
        assert!(out.commands.is_empty() && out.dialog.is_none());
        // The shell's loop: every menu pick is performed.
        for action in out.menu {
            perform(action, &mut ed).expect("the bake applies");
        }
        assert_ne!(
            pixels::read_layer(ed.active().unwrap(), layer),
            before_pixels,
            "brightness +50 changed no layer pixel"
        );
        assert_ne!(
            composite(&mut ed),
            before_composite,
            "brightness +50 changed no composite pixel"
        );
        assert_eq!(
            ed.active().unwrap().history_depth(),
            depth + 1,
            "the bake is not exactly one history entry"
        );
        // Nothing is left parked for a later click to pick up.
        assert_eq!(
            crate::dialog_host::take_confirmed_adjustment(
                ui::menu::AdjustmentId::BrightnessContrast
            ),
            None
        );
    }

    #[test]
    fn cancelling_an_adjustment_dialog_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = with_two_layers(dir.path());
        let before = digest(&ed);
        let depth = ed.active().unwrap().history_depth();
        let out = drive_adjustment_dialog(
            &mut ed,
            ui::menu::AdjustmentId::BrightnessContrast,
            |dialog| {
                assert!(
                    dialog.set_kind(layer_model::AdjustmentKind::BrightnessContrast {
                        brightness: 0.5,
                        contrast: 0.0,
                    })
                );
            },
            egui::Key::Escape,
        );
        assert!(out.menu.is_empty(), "cancel performed something: {out:?}");
        assert!(out.commands.is_empty() && out.dialog.is_none());
        assert_eq!(digest(&ed), before, "cancel changed the document");
        assert_eq!(ed.active().unwrap().history_depth(), depth);
        // A later Image ▸ Adjustments pick finds nothing parked either: it
        // runs at the starting parameters, which for Brightness/Contrast is
        // the identity and is refused loudly.
        let reason = perform(
            MenuAction::ApplyAdjustment(ui::menu::AdjustmentId::BrightnessContrast),
            &mut ed,
        )
        .unwrap_err();
        assert!(reason.contains("identity"), "{reason}");
        assert_eq!(digest(&ed), before);
    }

    #[test]
    fn threshold_confirmed_at_a_fifth_differs_from_four_fifths() {
        let dir = tempfile::tempdir().unwrap();
        let bake = |level: f32| -> (Vec<u8>, usize) {
            let mut ed = with_two_layers(dir.path());
            let layer = ed.active().unwrap().document.active_layer().unwrap();
            let depth = ed.active().unwrap().history_depth();
            let out = drive_adjustment_dialog(
                &mut ed,
                ui::menu::AdjustmentId::Threshold,
                |dialog| {
                    assert!(dialog.set_kind(layer_model::AdjustmentKind::Threshold { level }));
                },
                egui::Key::Enter,
            );
            for action in out.menu {
                perform(action, &mut ed).expect("the bake applies");
            }
            (
                pixels::read_layer(ed.active().unwrap(), layer),
                ed.active().unwrap().history_depth() - depth,
            )
        };
        let (low, low_steps) = bake(0.2);
        let (high, high_steps) = bake(0.8);
        assert_ne!(low, high, "Threshold at 0.2 and 0.8 baked identical pixels");
        assert_eq!((low_steps, high_steps), (1, 1));
        // And the two are real thresholds: every opaque pixel is black or white.
        for (name, image) in [("0.2", &low), ("0.8", &high)] {
            for px in image.as_chunks::<4>().0.iter().filter(|px| px[3] > 0) {
                assert!(
                    (px[0] == 0 && px[1] == 0 && px[2] == 0)
                        || (px[0] == 255 && px[1] == 255 && px[2] == 255),
                    "Threshold {name} left {px:?}"
                );
            }
        }
    }

    #[test]
    fn an_identity_adjustment_reaching_perform_is_refused_loudly() {
        // A pick with no dialog behind it — no parameters parked — runs the
        // adjustment at its start. For the ten that start at the identity that
        // is a refusal naming the dialog, never a silent no-op; for the five
        // that never are, it applies.
        let dir = tempfile::tempdir().unwrap();
        let mut ed = with_two_layers(dir.path());
        let reason = perform(
            MenuAction::ApplyAdjustment(ui::menu::AdjustmentId::Curves),
            &mut ed,
        )
        .unwrap_err();
        assert!(reason.contains("Image > Adjustments"), "{reason}");
        assert!(invoke(
            &mut ed,
            MenuAction::ApplyAdjustment(ui::menu::AdjustmentId::Posterize)
        )
        .unwrap());
    }

    // ---- W4-E: Desaturate, Equalize, Shadows/Highlights, Replace Color,
    // Color Lookup, each through the menu bar the way a user reaches it. ----

    /// The probe document with its active layer repainted as `paint(x, y)`,
    /// so each adjustment below has pixels whose answer is known.
    fn painted(dir: &std::path::Path, paint: impl Fn(u32, u32) -> [u8; 4]) -> Editor {
        let mut ed = opened(dir);
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        let (w, h) = canvas_of(&ed).unwrap();
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                rgba.extend_from_slice(&paint(x, y));
            }
        }
        let seed = {
            let doc = ed.active_mut().unwrap();
            pixels::write_layer(doc, layer, &rgba, "Seed").unwrap()
        };
        ed.apply_command(seed);
        ed
    }

    /// Click `id` in Image ▸ Adjustments on the menu bar and perform what the
    /// click produced, as the shell does. For the two with no dialog the
    /// click itself is the pick; for the rest the dialog is driven with
    /// `edit` and confirmed with Enter.
    fn apply_from_menu_bar(
        ed: &mut Editor,
        id: ui::menu::AdjustmentId,
        edit: impl FnOnce(&mut ui::dialogs::AdjustmentDialog),
    ) -> Result<(), String> {
        let out = if id.has_dialog() {
            drive_adjustment_dialog(ed, id, edit, egui::Key::Enter)
        } else {
            let mut chrome = crate::chrome::Chrome::new();
            let menu_ctx = context(ed, chrome.workspace());
            let intent =
                resolve_intent(MenuAction::ApplyAdjustment(id), &menu_ctx, ed).expect("enabled");
            let mut out = ChromeOutput::default();
            chrome.menu_click(intent, ed, &mut out);
            assert!(!chrome.dialog_open(), "{id:?} opened a dialog");
            out
        };
        assert_eq!(
            out.menu,
            vec![MenuAction::ApplyAdjustment(id)],
            "{id:?}: the click did not ride the menu channel: {out:?}"
        );
        for action in out.menu {
            perform(action, ed)?;
        }
        Ok(())
    }

    fn active_pixels(ed: &Editor) -> Vec<u8> {
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        pixels::read_layer(ed.active().unwrap(), layer)
    }

    #[test]
    fn desaturate_applies_on_the_click_as_one_step_and_leaves_r_g_b_equal() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = painted(dir.path(), |x, y| {
            [(x * 5) as u8, (200 - y * 3) as u8, ((x + y) * 2) as u8, 255]
        });
        let depth = ed.active().unwrap().history_depth();
        apply_from_menu_bar(&mut ed, ui::menu::AdjustmentId::Desaturate, |_| {}).unwrap();
        assert_eq!(ed.active().unwrap().history_depth(), depth + 1);
        for px in active_pixels(&ed).as_chunks::<4>().0 {
            assert!(
                px[0].abs_diff(px[1]) <= 1 && px[1].abs_diff(px[2]) <= 1,
                "desaturate left {px:?}"
            );
        }
        // Shift+Ctrl+U is the chord it wears.
        assert_eq!(
            MenuAction::ApplyAdjustment(ui::menu::AdjustmentId::Desaturate).shortcut(),
            Some(ui::shortcut::Shortcut::ctrl_shift('u'))
        );
    }

    #[test]
    fn equalize_applies_on_the_click_and_spreads_a_dark_layer_to_white() {
        let dir = tempfile::tempdir().unwrap();
        // Every value crowded into the darkest quarter.
        let mut ed = painted(dir.path(), |x, y| {
            let v = ((x + y * 48) % 60) as u8;
            [v, v, v, 255]
        });
        let depth = ed.active().unwrap().history_depth();
        apply_from_menu_bar(&mut ed, ui::menu::AdjustmentId::Equalize, |_| {}).unwrap();
        assert_eq!(ed.active().unwrap().history_depth(), depth + 1);
        let after = active_pixels(&ed);
        let max = after.as_chunks::<4>().0.iter().map(|p| p[0]).max().unwrap();
        assert!(max >= 250, "equalize left the brightest value at {max}");
        let bright = after
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[0] >= 128)
            .count();
        let share = bright as f32 / (after.len() / 4) as f32;
        assert!(
            (share - 0.5).abs() < 0.1,
            "{share} of the pixels are above mid-grey"
        );
    }

    #[test]
    fn shadows_highlights_confirmed_from_its_dialog_lifts_only_the_dark_half() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = painted(dir.path(), |x, _| {
            if x < 24 {
                [20, 20, 20, 255]
            } else {
                [230, 230, 230, 255]
            }
        });
        let before = active_pixels(&ed);
        let depth = ed.active().unwrap().history_depth();
        apply_from_menu_bar(&mut ed, ui::menu::AdjustmentId::ShadowsHighlights, |d| {
            // A 9 px radius: the masks are read from the neighbourhood.
            assert!(d.set_kind(layer_model::AdjustmentKind::ShadowsHighlights {
                shadows: [0.8, 0.5, 9.0],
                highlights: [0.0, 0.5, 9.0],
            }));
        })
        .unwrap();
        assert_eq!(ed.active().unwrap().history_depth(), depth + 1);
        let after = active_pixels(&ed);
        let (w, _) = canvas_of(&ed).unwrap();
        let lift =
            |x: u32| i32::from(after[(x * 4) as usize]) - i32::from(before[(x * 4) as usize]);
        for (i, (a, b)) in after
            .as_chunks::<4>()
            .0
            .iter()
            .zip(before.as_chunks::<4>().0)
            .enumerate()
        {
            let x = i as u32 % w;
            if x < 12 {
                assert!(
                    a[0] > b[0] + 10,
                    "a dark pixel was not lifted: {b:?} -> {a:?}"
                );
            } else if x >= 36 {
                assert_eq!(a, b, "a bright pixel moved");
            }
        }
        // The radius is honoured: a dark pixel beside the bright half reads
        // a brighter neighbourhood and is lifted less than one deep in the
        // dark half. A per-pixel Shadows/Highlights lifts both the same.
        assert!(
            lift(0) > lift(23),
            "deep {} vs edge {}: the radius was ignored",
            lift(0),
            lift(23)
        );
    }

    /// W7-G: log2 luminance of an 8-bit sRGB pixel.
    fn log_luma8(px: &[u8]) -> f32 {
        let lin = color::to_linear(
            &color::ColorSpace::Srgb,
            [0, 1, 2].map(|c| f32::from(px[c]) / 255.0),
        );
        (color::linear_srgb_luminance(lin).max(0.0) + 1.0 / 4096.0).log2()
    }

    /// W7-G: CIELAB means of an 8-bit sRGB buffer.
    fn lab_means8(rgba: &[u8]) -> [f32; 3] {
        let mut sum = [0.0f64; 3];
        let mut n = 0.0f64;
        for px in rgba.as_chunks::<4>().0 {
            let lin = color::to_linear(
                &color::ColorSpace::Srgb,
                [0, 1, 2].map(|c| f32::from(px[c]) / 255.0),
            );
            let lab = color::linear_srgb_to_lab(lin);
            for k in 0..3 {
                sum[k] += f64::from(lab[k]);
            }
            n += 1.0;
        }
        sum.map(|v| (v / n) as f32)
    }

    #[test]
    fn hdr_toning_confirmed_from_its_dialog_raises_local_contrast_as_one_step() {
        let dir = tempfile::tempdir().unwrap();
        // A dark and a light field with the same fine checker on both.
        let mut ed = painted(dir.path(), |x, y| {
            let base: i32 = if x < 24 { 60 } else { 170 };
            let v = (base + if (x / 2 + y / 2) % 2 == 0 { 8 } else { -8 }) as u8;
            [v, v, v, 255]
        });
        let before = active_pixels(&ed);
        let depth = ed.active().unwrap().history_depth();
        apply_from_menu_bar(&mut ed, ui::menu::AdjustmentId::HdrToning, |d| {
            assert!(d.set_kind(layer_model::AdjustmentKind::HdrToning {
                radius: 8.0,
                strength: 2.0,
                gamma: 1.0,
                exposure: 0.0,
                detail: 1.0,
                vibrance: 0.0,
                saturation: 0.0,
            }));
        })
        .unwrap();
        assert_eq!(ed.active().unwrap().history_depth(), depth + 1);
        let after = active_pixels(&ed);
        let (w, _) = canvas_of(&ed).unwrap();
        // The checker's amplitude in stops, far from the step on both sides.
        let amplitude = |rgba: &[u8], x0: u32| {
            let a = log_luma8(&rgba[(x0 * 4) as usize..]);
            let b = log_luma8(&rgba[((x0 + 2) * 4) as usize..]);
            (a - b).abs()
        };
        for x0 in [4, 40] {
            let (b, a) = (amplitude(&before, x0), amplitude(&after, x0));
            assert!(a > b * 1.25, "x {x0}: checker {b} -> {a} stops (w {w})");
        }
        // Undo is the one step back.
        assert!(ed.active_mut().unwrap().undo().unwrap());
        assert_eq!(active_pixels(&ed), before);
    }

    #[test]
    fn match_color_confirmed_from_its_dialog_moves_the_layer_means_to_the_source_layer() {
        let dir = tempfile::tempdir().unwrap();
        // The target: warm, low contrast.
        let mut ed = painted(dir.path(), |x, y| {
            [150 + (x % 16) as u8, 110 + (y % 8) as u8, 70, 255]
        });
        let target = ed.active().unwrap().document.active_layer().unwrap();
        // The source: a cool, contrasty layer beside it.
        let source_layer = layer_model::Layer::raster("Cool source");
        let source_id = source_layer.id;
        ed.apply_command(Command::create_layer(source_layer));
        let (w, h) = canvas_of(&ed).unwrap();
        let mut cool = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                cool.extend_from_slice(&[40 + (x * 3) as u8, 90 + (y * 2) as u8, 200, 255]);
            }
        }
        let seed = {
            let doc = ed.active_mut().unwrap();
            pixels::write_layer(doc, source_id, &cool, "Seed source").unwrap()
        };
        ed.apply_command(seed);
        ed.set_active_layer(target);
        let before = active_pixels(&ed);
        let depth = ed.active().unwrap().history_depth();

        apply_from_menu_bar(&mut ed, ui::menu::AdjustmentId::MatchColor, |d| {
            // The host offered the other layer; with None picked the dialog
            // is the identity and refuses to apply.
            assert!(d.invocation().is_identity());
            let pick = d
                .match_sources()
                .iter()
                .position(|s| s.label.contains("Cool source"))
                .unwrap_or_else(|| panic!("no source offered: {:?}", d.match_sources()));
            assert!(d.choose_match_source(pick + 1));
            assert!(!d.invocation().is_identity());
        })
        .unwrap();
        assert_eq!(ed.active().unwrap().history_depth(), depth + 1);
        let after = active_pixels(&ed);
        let (want, was, got) = (lab_means8(&cool), lab_means8(&before), lab_means8(&after));
        for k in 0..3 {
            assert!(
                (got[k] - want[k]).abs() < 2.0,
                "channel {k}: {was:?} -> {got:?}, source {want:?}"
            );
            assert!(
                (was[k] - want[k]).abs() > 5.0 || k == 0,
                "fixture too close"
            );
        }
        // The source layer is untouched.
        let source_now = pixels::read_layer(ed.active().unwrap(), source_id);
        assert_eq!(source_now, cool);
    }

    #[test]
    fn replace_color_confirmed_from_its_dialog_shifts_only_the_sampled_colour() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = painted(dir.path(), |x, _| {
            if x < 24 {
                [220, 30, 30, 255]
            } else {
                [30, 40, 220, 255]
            }
        });
        let before = active_pixels(&ed);
        let depth = ed.active().unwrap().history_depth();
        apply_from_menu_bar(&mut ed, ui::menu::AdjustmentId::ReplaceColor, |d| {
            // Sample the red half from the preview, as a click there would.
            assert!(d.sample_preview(2, 2), "the preview could not be sampled");
            let layer_model::AdjustmentKind::ReplaceColor {
                color, fuzziness, ..
            } = d.kind().clone()
            else {
                panic!("not a Replace Color kind");
            };
            assert!(color[0] > 0.8 && color[2] < 0.2, "sampled {color:?}");
            assert!(d.set_kind(layer_model::AdjustmentKind::ReplaceColor {
                color,
                fuzziness,
                hue: 120.0,
                saturation: 0.0,
                lightness: 0.0,
            }));
        })
        .unwrap();
        assert_eq!(ed.active().unwrap().history_depth(), depth + 1);
        let after = active_pixels(&ed);
        let (w, _) = canvas_of(&ed).unwrap();
        for (i, (a, b)) in after
            .as_chunks::<4>()
            .0
            .iter()
            .zip(before.as_chunks::<4>().0)
            .enumerate()
        {
            if (i as u32 % w) < 24 {
                assert!(a[1] > a[0], "the red half did not turn green: {a:?}");
            } else {
                assert_eq!(a, b, "the blue half moved");
            }
        }
    }

    #[test]
    fn color_lookup_loads_a_cube_file_in_its_dialog_and_bakes_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = painted(dir.path(), |x, y| [(x * 5) as u8, (y * 7) as u8, 90, 255]);
        let before = active_pixels(&ed);
        let depth = ed.active().unwrap().history_depth();
        // An inverting 2-point cube, as a .cube file on disk would read.
        let mut cube = String::from("TITLE \"Invert\"\nLUT_3D_SIZE 2\n");
        for b in 0..2 {
            for g in 0..2 {
                for r in 0..2 {
                    cube.push_str(&format!("{} {} {}\n", 1 - r, 1 - g, 1 - b));
                }
            }
        }
        apply_from_menu_bar(&mut ed, ui::menu::AdjustmentId::ColorLookup, |d| {
            // Untouched it is the identity cube and cannot confirm.
            assert!(d.invocation().is_identity());
            d.load_cube_text("invert", &cube).expect("the cube parses");
            assert!(!d.invocation().is_identity());
        })
        .unwrap();
        assert_eq!(ed.active().unwrap().history_depth(), depth + 1);
        for (a, b) in active_pixels(&ed)
            .as_chunks::<4>()
            .0
            .iter()
            .zip(before.as_chunks::<4>().0)
        {
            for c in 0..3 {
                assert!(
                    (i32::from(a[c]) - (255 - i32::from(b[c]))).abs() <= 2,
                    "{b:?} inverted to {a:?}"
                );
            }
        }
        // And Color Lookup is also an adjustment layer, as in Photopea.
        assert!(ui::menu::AdjustmentId::ColorLookup.is_layer());
        let menu_ctx = context(&mut ed, &Workspace::new());
        assert!(resolve_intent(
            MenuAction::NewAdjustmentLayer(ui::menu::AdjustmentId::ColorLookup),
            &menu_ctx,
            &ed
        )
        .is_ok());
    }

    /// Click Layer ▸ New Adjustment Layer ▸ `id` on the menu bar, apply what
    /// it produced, and select the new layer the way a Layers-panel click
    /// does. Returns the new layer.
    fn new_adjustment_layer(ed: &mut Editor, id: ui::menu::AdjustmentId) -> layer_model::LayerId {
        let ids_before = ed.active().unwrap().document.layers.iter_depth_first();
        let mut chrome = crate::chrome::Chrome::new();
        let menu_ctx = context(ed, chrome.workspace());
        let intent =
            resolve_intent(MenuAction::NewAdjustmentLayer(id), &menu_ctx, ed).expect("enabled");
        let mut out = ChromeOutput::default();
        chrome.menu_click(intent, ed, &mut out);
        assert!(!chrome.dialog_open(), "a new layer asks nothing");
        apply_output(ed, out).unwrap();
        let layer = ed
            .active()
            .unwrap()
            .document
            .layers
            .iter_depth_first()
            .into_iter()
            .find(|l| !ids_before.contains(l))
            .expect("no layer was created");
        assert!(matches!(
            &ed.active().unwrap().document.layers.get(layer).unwrap().kind,
            layer_model::LayerKind::Adjustment(a)
                if ui::panels::properties::adjustment_id_of(&a.kind) == Some(id)
        ));
        ed.set_active_layer(layer);
        layer
    }

    /// Open Layer ▸ Edit Adjustment… (the intent the Properties panel's
    /// "Open editor…" posts too) through the chrome's own route, let `edit`
    /// drive the dialog it opened, press Enter, and apply the kind edit the
    /// confirmation produced, as the shell does.
    fn edit_adjustment_layer_via_dialog(
        ed: &mut Editor,
        edit: impl FnOnce(&mut ui::dialogs::AdjustmentDialog),
    ) {
        let mut chrome = crate::chrome::Chrome::new();
        let menu_ctx = context(ed, chrome.workspace());
        let intent =
            resolve_intent(MenuAction::EditAdjustmentLayer, &menu_ctx, ed).expect("enabled");
        let mut out = ChromeOutput::default();
        chrome.menu_click(intent, ed, &mut out);
        assert!(
            chrome.dialog_open(),
            "Edit Adjustment opened no dialog on a Color Lookup layer: {out:?}"
        );
        assert!(out.is_empty(), "opening produced {out:?}");
        edit(
            chrome
                .dialogs_for_test()
                .active_adjustment_dialog_for_test(),
        );
        let out = press_in_dialog(&mut chrome, egui::Key::Enter);
        assert!(!chrome.dialog_open(), "Enter did not close the dialog");
        assert!(
            out.menu.is_empty() && out.commands.is_empty(),
            "an adjustment layer's edit baked pixels instead: {out:?}"
        );
        assert_eq!(out.layer_kind.len(), 1, "{out:?}");
        for edit in out.layer_kind {
            ed.apply_kind_edit(edit);
        }
    }

    #[test]
    fn a_color_lookup_layer_takes_a_built_in_look_and_a_cube_file_through_edit_adjustment() {
        // W4-E round 2: the layer is created holding the identity cube, and
        // the only road to its table is the dialog Edit Adjustment reopens.
        let dir = tempfile::tempdir().unwrap();
        let mut ed = painted(dir.path(), |x, y| [(x * 5) as u8, (y * 7) as u8, 90, 255]);
        let base = composite(&mut ed);
        let layer = new_adjustment_layer(&mut ed, ui::menu::AdjustmentId::ColorLookup);
        assert_eq!(composite(&mut ed), base, "the identity cube changed pixels");

        // A built-in look: Invert, the first listed.
        let depth = ed.active().unwrap().history_depth();
        edit_adjustment_layer_via_dialog(&mut ed, |d| {
            assert_eq!(d.edit_layer(), Some(layer));
            assert!(d.choose_lut(1), "the Invert look is not listed");
        });
        assert_eq!(ed.active().unwrap().history_depth(), depth + 1);
        let layer_model::LayerKind::Adjustment(a) = &ed
            .active()
            .unwrap()
            .document
            .layers
            .get(layer)
            .unwrap()
            .kind
        else {
            panic!("the layer is no longer an adjustment layer");
        };
        let layer_model::AdjustmentKind::ColorLookup { name, .. } = &a.kind else {
            panic!("the layer is no longer a Color Lookup: {:?}", a.kind);
        };
        assert_eq!(name, "Invert");
        let inverted = composite(&mut ed);
        for (a, b) in inverted
            .as_chunks::<4>()
            .0
            .iter()
            .zip(base.as_chunks::<4>().0)
        {
            for c in 0..3 {
                assert!(
                    (i32::from(a[c]) - (255 - i32::from(b[c]))).abs() <= 3,
                    "{b:?} looked up to {a:?}"
                );
            }
        }
        // Reopened, the dialog shows that look, and a loaded .cube file
        // (a red/blue swap) replaces it.
        let mut cube = String::from("TITLE \"Swap\"\nLUT_3D_SIZE 2\n");
        for b in 0..2 {
            for g in 0..2 {
                for r in 0..2 {
                    cube.push_str(&format!("{b} {g} {r}\n"));
                }
            }
        }
        let depth = ed.active().unwrap().history_depth();
        edit_adjustment_layer_via_dialog(&mut ed, |d| {
            assert!(!d.choose_lut(99));
            d.load_cube_text("swap", &cube).expect("the cube parses");
        });
        assert_eq!(ed.active().unwrap().history_depth(), depth + 1);
        let swapped = composite(&mut ed);
        for (a, b) in swapped
            .as_chunks::<4>()
            .0
            .iter()
            .zip(base.as_chunks::<4>().0)
        {
            assert!(
                (i32::from(a[0]) - i32::from(b[2])).abs() <= 3
                    && (i32::from(a[2]) - i32::from(b[0])).abs() <= 3,
                "{b:?} swapped to {a:?}"
            );
        }
        // Undo walks back to the look, then to the identity.
        ed.active_mut().unwrap().undo().unwrap();
        assert_eq!(composite(&mut ed), inverted);
    }

    #[test]
    fn the_properties_panels_open_editor_on_a_color_lookup_layer_opens_its_dialog() {
        // The button is the real one, drawn by the real Properties panel in
        // the real chrome and pressed with a pointer; what it opens is the
        // Color Lookup dialog aimed at the layer, and its Enter edits the
        // layer as one undo step.
        let dir = tempfile::tempdir().unwrap();
        let mut ed = painted(dir.path(), |x, y| [(x * 5) as u8, (y * 7) as u8, 90, 255]);
        let base = composite(&mut ed);
        let layer = new_adjustment_layer(&mut ed, ui::menu::AdjustmentId::ColorLookup);
        let ctx = egui::Context::default();
        crate::chrome::install_theme(&ctx, design::Theme::Dark);
        let mut chrome = crate::chrome::Chrome::new();
        chrome.workspace_for_test().emit(ui::Intent::SetPanelOpen {
            panel: ui::PanelId::Properties,
            open: true,
        });
        for _ in 0..5 {
            let _ = ctx.run(raw_input(Vec::new()), |ctx| {
                let _ = chrome.ui(ctx, &mut ed);
            });
        }
        let pos = ctx
            .read_response(ui::view::ids::adjustment_editor())
            .expect("the Properties panel drew no \"Open editor\" for the Color Lookup layer")
            .rect
            .center();
        let events = vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            },
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::default(),
            },
        ];
        let _ = ctx.run(raw_input(events), |ctx| {
            let _ = chrome.ui(ctx, &mut ed);
        });
        let _ = ctx.run(raw_input(Vec::new()), |ctx| {
            let _ = chrome.ui(ctx, &mut ed);
        });
        assert!(chrome.dialog_open(), "Open editor opened no dialog");
        let dialog = chrome
            .dialogs_for_test()
            .active_adjustment_dialog_for_test();
        assert_eq!(dialog.id(), ui::menu::AdjustmentId::ColorLookup);
        assert_eq!(dialog.edit_layer(), Some(layer));
        assert!(dialog.choose_lut(1), "Invert is not listed");
        let depth = ed.active().unwrap().history_depth();
        let out = press_in_dialog(&mut chrome, egui::Key::Enter);
        assert!(
            out.menu.is_empty(),
            "the layer's edit baked pixels: {out:?}"
        );
        for edit in out.layer_kind {
            ed.apply_kind_edit(edit);
        }
        assert_eq!(ed.active().unwrap().history_depth(), depth + 1);
        let after = composite(&mut ed);
        let (a, b) = (&after[..4], &base[..4]);
        assert!(
            (i32::from(a[2]) - (255 - i32::from(b[2]))).abs() <= 3,
            "{b:?} looked up to {a:?}"
        );
    }

    #[test]
    fn edit_adjustment_on_a_dock_edited_layer_still_reveals_the_properties_panel() {
        // The dock-edited kinds keep their road: no dialog, the panel opens.
        let dir = tempfile::tempdir().unwrap();
        let mut ed = painted(dir.path(), |_, _| [90, 90, 90, 255]);
        new_adjustment_layer(&mut ed, ui::menu::AdjustmentId::BrightnessContrast);
        assert!(!crate::dialog_host::DialogHost::default()
            .open_for_menu_action(&MenuAction::EditAdjustmentLayer, &ed));
        let menu_ctx = context(&mut ed, &Workspace::new());
        assert!(matches!(
            resolve(MenuAction::EditAdjustmentLayer, &menu_ctx, &ed),
            Ok(Pick::Workspace(_))
        ));
    }

    #[test]
    fn equalize_inside_a_selection_reads_only_the_selected_pixels() {
        // The left half is dark, the right half bright. Equalising with the
        // left half selected must spread the *left half's* tones over the
        // whole range; read over the whole layer, the dark half would own
        // only the lower half of it.
        let dir = tempfile::tempdir().unwrap();
        let mut ed = painted(dir.path(), |x, y| {
            if x < 24 {
                let v = ((x + y * 24) % 40) as u8;
                [v, v, v, 255]
            } else {
                let v = 200 + ((x + y) % 56) as u8;
                [v, v, v, 255]
            }
        });
        let (w, h) = canvas_of(&ed).unwrap();
        ed.active_mut().unwrap().document.selection = editor_core::Selection::Rect {
            min: glam::IVec2::ZERO,
            max: glam::IVec2::new(24, h as i32),
        };
        let before = active_pixels(&ed);
        let depth = ed.active().unwrap().history_depth();
        apply_from_menu_bar(&mut ed, ui::menu::AdjustmentId::Equalize, |_| {}).unwrap();
        assert_eq!(ed.active().unwrap().history_depth(), depth + 1);
        let after = active_pixels(&ed);
        let mut left_max = 0u8;
        for (i, (a, b)) in after
            .as_chunks::<4>()
            .0
            .iter()
            .zip(before.as_chunks::<4>().0)
            .enumerate()
        {
            if (i as u32 % w) < 24 {
                left_max = left_max.max(a[0]);
            } else {
                assert_eq!(a, b, "a pixel outside the selection moved");
            }
        }
        assert!(
            left_max >= 240,
            "the selected dark half only reached {left_max}: the histogram was not the selection's"
        );
    }

    #[test]
    fn shadows_highlights_at_its_identity_says_so_in_one_clean_sentence() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = painted(dir.path(), |_, _| [90, 90, 90, 255]);
        // Both amounts at zero: nothing to lift or darken.
        let identity =
            adjustments::Adjustment::from(&layer_model::AdjustmentKind::ShadowsHighlights {
                shadows: [0.0, 0.5, 9.0],
                highlights: [0.0, 0.5, 9.0],
            });
        let message = run_adjustment_kind(&mut ed, &identity, "Shadows/Highlights").unwrap_err();
        assert!(
            !message.contains("  "),
            "the message has a run of spaces: {message:?}"
        );
        assert!(message.contains("would change nothing"), "{message:?}");
    }

    #[test]
    fn no_enabled_menu_item_resolves_to_a_no_op() {
        // The bar this whole wave is measured against: an item that is *not*
        // greyed out has to change the document when it is clicked. Every item
        // is driven exactly the way the shell drives it — resolve, then either
        // `perform` or `apply_command` — against a fresh document each time, so
        // one item cannot leave another with nothing to do.
        let dir = tempfile::tempdir().unwrap();
        let mut checked = 0usize;
        let mut template = with_two_layers(dir.path());
        let context = context(&mut template, &Workspace::new());
        let candidates: Vec<MenuAction> = menus(&template)
            .into_iter()
            .flat_map(|m| m.actions())
            .filter(|a| {
                matches!(
                    resolve(*a, &context, &template),
                    Ok(Pick::Menu(_)) | Ok(Pick::Command(_))
                )
            })
            .collect();
        drop(template);

        // The four Help items are the only live ones whose effect is a
        // *message* rather than a document edit, so they are held to that
        // instead — not skipped. An item here that fell silent would fail.
        const INFORMATIONAL: &[MenuAction] = &[
            MenuAction::Help,
            MenuAction::ReleaseNotes,
            MenuAction::ReportIssue,
            MenuAction::About,
        ];

        // Window-opening actions change UI state (a window becomes visible),
        // not the document digest, so they are held to flipping their editor
        // flag rather than to touching a pixel.
        const WINDOWS: &[MenuAction] = &[MenuAction::FileInfo];

        let mut dead = Vec::new();
        for action in candidates {
            let mut ed = with_two_layers(dir.path());
            // C5/C6: the Help arms open a browser through the editor's
            // launcher — record instead of opening tabs on the CI runner.
            ed.set_url_launcher(Box::new(SharedRecorder::default()));
            // File ▸ Export Layers… writes files or refuses worriedly when no
            // destination is chosen; either outcome is loud, never a silent
            // no-op, so it does not have to change the document digest.
            if action == MenuAction::ExportLayers
                || action == MenuAction::Print
                // Export Diagnostics writes a file (or refuses when the dialog
                // is declined) — loud either way, and the digest cannot move.
                || action == MenuAction::ExportDiagnostics
                // Place Embedded/Linked route now (C7 removed the stale
                // reasons): with no file queued the scripted dialog cancels,
                // and a cancelled place refuses loudly — its point.
                || action == MenuAction::PlaceEmbedded
                || action == MenuAction::PlaceLinked
                // Card 069: with no file queued the scripted dialog cancels,
                // and a cancelled replace refuses loudly — its point.
                || action == MenuAction::ReplaceContents
                // SetColorMode refuses when the document already wears the
                // requested mode — the correct loud answer to a no-op
                // request; the other modes genuinely rewrite the tiles.
                || matches!(action, MenuAction::SetColorMode(_))
                || action == MenuAction::CommitSmartObjectContents
                || action == MenuAction::CloseAll
                || action == MenuAction::Trim
                || action == MenuAction::CropToSelection
                || action == MenuAction::StrokeDialog
                // Define Brush Preset stores a *brush*, not pixels — the
                // digest cannot move; `defining_a_brush_preset_offers_it_
                // again_after_a_restart` pins the persistence instead.
                // Card 067's style controls: Copy captures a session field
                // (the digest cannot move), Define stores a preset (the
                // same store as Define Brush), and Paste/Apply refuse
                // LOUDLY when nothing was copied/defined — the correct
                // answer with no style in flight. The dedicated test
                // `layer_styles_are_reusable_across_layers_and_restarts`
                // pins the real sequences.
                || action == MenuAction::CopyLayerStyle
                || action == MenuAction::PasteLayerStyle
                || action == MenuAction::DefineStylePreset
                || action == MenuAction::ApplyStylePreset
                || action == MenuAction::DefineBrush
                // New ▸ Fill Layer ▸ Pattern needs a user-defined pattern;
                // `a_new_pattern_fill_layer_tiles_the_latest_pattern` drives
                // the define-then-fill sequence this fixture has no time for.
                || action == MenuAction::NewFillLayer(ui::menu::FillLayerKind::Pattern)
                // Reveal All is a no-op when every layer already fits — its
                // answer is the status line, and `reveal_all_on_a_contained_
                // canvas_reveals_nothing` pins the grow case.
                || action == MenuAction::RevealAll
                // W2-F: Save as PSD cancels at the scripted picker (loud);
                // Align refuses a layer that already meets the edge (loud),
                // and in this fixture both layers fill the canvas.
                || action == MenuAction::SaveAsPsd
                || matches!(action, MenuAction::AlignLayers(_))
                // W4-H: Export Slices refuses loudly with no slices drawn;
                // Purge Histories/All only ASK on the first choice (history
                // is not in the digest, and dropping it is the point) —
                // `purging_histories_asks_first_then_drops_every_step` pins
                // the real sequence.
                || action == MenuAction::ExportSlices
                // W10-A: and Slice Options with no slice picked (it says to
                // pick one); `slices_export::tests::the_slice_options_row_*`
                // pins the dialog route.
                || action == MenuAction::SliceOptions
                // W8-C: and Export Artboards with no artboard drawn.
                || action == MenuAction::ExportArtboards
                // W10-I: Matting refuses loudly on these opaque layers (no
                // edge pixel for a matte to be in);
                // `layer_extras::tests::remove_black_and_white_matte_take_the_matte_back_out`
                // pins the real un-matting.
                || matches!(action, MenuAction::Matting(_))
                // W10-E: Batch / Convert / Variables refuse loudly with no
                // dialog confirmation; the two exports cancel at the
                // scripted folder picker (loud).
                || crate::file_extras::is_loud_without_a_dialog(action)
                || matches!(action, MenuAction::Purge(_))
            {
                match perform(action, &mut ed) {
                    Ok(_) | Err(_) => checked += 1,
                }
                continue;
            }
            if WINDOWS.contains(&action) {
                if let Err(reason) = perform(action, &mut ed) {
                    dead.push(format!("{action:?}: refused with {reason:?}"));
                }
                if !ed.file_info_open() {
                    dead.push(format!("{action:?}: did not open the window"));
                }
                checked += 1;
                continue;
            }
            if INFORMATIONAL.contains(&action) {
                // C6: perform exactly ONCE — the old code called it a second
                // time to build the expected status, which double-executed
                // any informational action with a side effect (and turned an
                // environment-dependent message into the hard CI failure
                // 2d2fde9 chased). Capture the first call's message instead.
                let message = match perform(action, &mut ed) {
                    Ok(message) if message.len() > 20 => message,
                    Ok(message) => {
                        dead.push(format!("{action:?}: said only {message:?}"));
                        continue;
                    }
                    Err(reason) => {
                        dead.push(format!("{action:?}: refused with {reason:?}"));
                        continue;
                    }
                };
                assert_eq!(
                    ed.status().map(str::to_string),
                    Some(message),
                    "{action:?} did not reach the status bar"
                );
                checked += 1;
                continue;
            }
            // The Layer Style rows are hosted by the chrome's dialog host:
            // the click opens the real dialog and the confirmed style arrives
            // as the command it emits, one or more frames later. Driven here
            // the way [`crate::chrome::Chrome::harvest`] drives it — the host
            // must answer, and answering must leave the document alone.
            if crate::dialog_host::DialogHost::default().open_for_menu_action(&action, &ed) {
                checked += 1;
                continue;
            }
            match invoke(&mut ed, action) {
                Ok(true) => checked += 1,
                Ok(false) => dead.push(format!("{action:?}: changed nothing")),
                Err(reason) => dead.push(format!("{action:?}: refused with {reason:?}")),
            }
        }
        assert!(
            dead.is_empty(),
            "{} enabled menu items do nothing when clicked:\n{dead:#?}",
            dead.len()
        );
        assert!(
            checked > 60,
            "only {checked} items were exercised; the walk stopped finding them"
        );
    }

    #[test]
    fn every_enabled_menu_item_really_does_something() {
        // The gate `unavailable_reason`'s doc names, driven the way a user
        // drives it. Every row the menu bar draws *enabled* with a document
        // open is sent through `Chrome::menu_click` — the handler `draw` calls
        // on a click, nothing else — and has to either open a dialog or reach
        // a pick the shell accepts. Landing in `perform`'s "no implementation"
        // arm is the failure this exists to catch: it is exactly what Image
        // Size, Canvas Size, Arbitrary rotation, Layer Style and the Filter
        // Gallery did while the menu bar recorded picks straight into the
        // output and the dialog host was consulted only for workspace intents.
        let dir = tempfile::tempdir().unwrap();
        let mut template = with_two_layers(dir.path());
        let menu_ctx = context(&mut template, &Workspace::new());
        let enabled: Vec<MenuAction> = menus(&template)
            .into_iter()
            .flat_map(|m| m.actions())
            .filter(|a| resolve_intent(*a, &menu_ctx, &template).is_ok())
            .collect();
        drop(template);
        assert!(
            enabled.len() > 100,
            "only {} rows are enabled; the walk stopped finding them",
            enabled.len()
        );

        // Rows `perform` legitimately *refuses* in this fixture, and says so
        // on the status line — a loud refusal is a real answer, a silent
        // "no implementation" is not. Each one is refused for a state this
        // fixture is in, not for a missing arm: the scripted file dialogs
        // cancel (Place, Replace Contents, Export Layers, Print, Export
        // Diagnostics, and the unsaved-changes prompt behind Close All),
        // nothing has been copied or defined (Paste/Apply a style, a Pattern
        // fill layer), the layer has no style to define as a preset, the
        // content already fills the canvas (Trim), the document already wears
        // one of the colour modes, and there is no smart object to commit.
        const LOUD_REFUSALS: &[MenuAction] = &[
            MenuAction::CloseAll,
            MenuAction::Trim,
            MenuAction::DefineStylePreset,
            MenuAction::PlaceEmbedded,
            MenuAction::PlaceLinked,
            MenuAction::ReplaceContents,
            MenuAction::ExportLayers,
            MenuAction::Print,
            MenuAction::ExportDiagnostics,
            MenuAction::PasteLayerStyle,
            MenuAction::ApplyStylePreset,
            MenuAction::NewFillLayer(ui::menu::FillLayerKind::Pattern),
            MenuAction::CommitSmartObjectContents,
            MenuAction::SetColorMode(ui::menu::ColorMode::Rgb),
            MenuAction::SetColorMode(ui::menu::ColorMode::Grayscale),
            MenuAction::SetColorMode(ui::menu::ColorMode::Lab),
            MenuAction::SetColorMode(ui::menu::ColorMode::Cmyk),
            MenuAction::SetColorMode(ui::menu::ColorMode::Indexed),
            // W2-F: the scripted picker cancels the PSD save; both fixture
            // layers already fill the canvas, so every Align edge is met.
            MenuAction::SaveAsPsd,
            MenuAction::AlignLayers(ui::menu::AlignEdge::Left),
            MenuAction::AlignLayers(ui::menu::AlignEdge::HorizontalCenter),
            MenuAction::AlignLayers(ui::menu::AlignEdge::Right),
            MenuAction::AlignLayers(ui::menu::AlignEdge::Top),
            MenuAction::AlignLayers(ui::menu::AlignEdge::VerticalCenter),
            MenuAction::AlignLayers(ui::menu::AlignEdge::Bottom),
            // W4-H: no slices have been drawn in this fixture.
            MenuAction::ExportSlices,
            // W10-A: so none is picked for Slice Options either.
            MenuAction::SliceOptions,
            // W8-C: nor any artboard.
            MenuAction::ExportArtboards,
            // W10-I: both fixture layers are opaque, so there is no edge
            // pixel for a matte to be in.
            MenuAction::Matting(ui::menu::MattingOp::RemoveBlackMatte),
            MenuAction::Matting(ui::menu::MattingOp::RemoveWhiteMatte),
        ];

        let mut broken = Vec::new();
        let mut opened = Vec::new();
        let mut routed = 0usize;
        for action in enabled {
            let mut ed = with_two_layers(dir.path());
            // The Help rows open a browser through the editor's launcher —
            // record instead of opening tabs on the CI runner.
            ed.set_url_launcher(Box::new(SharedRecorder::default()));
            let mut chrome = crate::chrome::Chrome::new();
            let menu_ctx = context(&mut ed, chrome.workspace());
            let intent = resolve_intent(action, &menu_ctx, &ed).expect("enabled above");
            let mut out = ChromeOutput::default();
            chrome.menu_click(intent, &ed, &mut out);
            if chrome.dialog_open() {
                opened.push(action);
                continue;
            }
            if !out.unrouted.is_empty() {
                broken.push(format!("{action:?}: the chrome had no road for it"));
                continue;
            }
            if out.is_empty() {
                broken.push(format!("{action:?}: the click produced nothing"));
                continue;
            }
            // A pick reached the output. `Pick::Menu` is the one kind the
            // shell hands back to this module, so it is the one kind that can
            // still fall through; the others are the shell's own channels.
            for named in std::mem::take(&mut out.menu) {
                match perform(named, &mut ed) {
                    Ok(_) => {}
                    Err(reason) if reason.contains("no implementation") => {
                        broken.push(format!("{action:?}: {reason}"));
                    }
                    Err(_) if LOUD_REFUSALS.contains(&action) => {}
                    Err(reason) => broken.push(format!("{action:?}: refused with {reason:?}")),
                }
            }
            routed += 1;
        }
        assert!(
            broken.is_empty(),
            "{} enabled menu rows reach nobody when clicked:\n{broken:#?}",
            broken.len()
        );
        // The rows whose whole point is a question: with a document open every
        // one of them has to open its dialog from the menu bar, not run at its
        // defaults and not refuse. Every Image ▸ Adjustments row is one of
        // them now — the ten that used to be greyed and the five that used to
        // bake their defaults on the click.
        let mut asked = vec![
            MenuAction::ImageSize,
            MenuAction::CanvasSize,
            MenuAction::RotateCanvas(ui::menu::CanvasRotation::Arbitrary),
            MenuAction::LayerStyle(ui::menu::EffectSlot::DropShadow),
            MenuAction::FilterGallery,
            MenuAction::Filter(ui::menu::FilterId::GaussianBlur),
            MenuAction::Filter(ui::menu::FilterId::Custom),
            MenuAction::Filter(ui::menu::FilterId::Offset),
            MenuAction::Export(raster::ExportFormat::Png),
            MenuAction::FillDialog,
            MenuAction::StrokeDialog,
            MenuAction::NewDocument,
            // W2-F: the rows the audit found asking nothing, or asking in
            // the wrong place (Blending Options revealed a panel).
            MenuAction::Trim,
            MenuAction::RenameLayer,
            MenuAction::NewGuide,
            MenuAction::About,
            MenuAction::BlendingOptions,
            MenuAction::DuplicateLayer,
            // W3-H: Color Range asks its fuzziness and colour. The other
            // Select-menu questions (Modify, Save/Load Selection) are gated on
            // a selection this fixture does not have; their menu-bar route is
            // pinned by `the_select_menu_questions_open_from_the_menu_bar_...`.
            MenuAction::ColorRange,
        ];
        // Every adjustment that asks something; Desaturate and Equalize
        // (W4-E) apply on the click, so they are performed by the walk.
        asked.extend(
            ui::menu::AdjustmentId::ALL
                .iter()
                .copied()
                .filter(|id| id.has_dialog())
                .map(MenuAction::ApplyAdjustment),
        );
        // W3-E: the Photopea-parity filter rows. Each opens the generated
        // FilterDialog (live preview) from the menu bar, the parameterless
        // one-click ones included.
        {
            use ui::menu::FilterId as F;
            asked.extend(
                [
                    F::Average,
                    F::Blur,
                    F::BlurMore,
                    F::SmartBlur,
                    F::Sharpen,
                    F::SharpenMore,
                    F::SharpenEdges,
                    F::Displace,
                    F::Facet,
                    F::Fragment,
                    F::Mezzotint,
                    F::Extrude,
                    F::Tiles,
                    F::TraceContour,
                ]
                .map(MenuAction::Filter),
            );
            // W10-C: Camera Raw, Lens Correction, Lighting Effects, HSB/HSL.
            asked.extend(
                [
                    F::CameraRaw,
                    F::LensCorrection,
                    F::LightingEffects,
                    F::HsbHsl,
                ]
                .map(MenuAction::Filter),
            );
        }
        // W7-E: Convert for Smart Filters is live over a pixel layer, and
        // routes to `perform`.
        assert_eq!(
            resolve_intent(
                MenuAction::ConvertForSmartFilters,
                &menu_ctx,
                &with_two_layers(dir.path())
            ),
            Ok(Intent::Action(MenuAction::ConvertForSmartFilters))
        );
        for asked in asked {
            assert!(
                opened.contains(&asked),
                "{asked:?} did not open its dialog from the menu bar; the rows that did: {opened:?}"
            );
        }
        assert!(
            routed > 60,
            "only {routed} rows were performed; the walk stopped finding them"
        );
    }

    /// W3-H, the real route: each Select-menu question is clicked in the
    /// menu bar (`Chrome::menu_click`, what `draw` calls), must open its
    /// dialog instead of running at a default, and Enter through the whole
    /// chrome's frame must hand back the pick whose `perform` edits the
    /// document -- the selection for Color Range / Modify / Load, the named
    /// store for Save.
    #[test]
    fn the_select_menu_questions_open_from_the_menu_bar_and_their_confirmation_edits_the_document()
    {
        use ui::menu::ModifySelection as M;
        let dir = tempfile::tempdir().unwrap();
        let rows = [
            MenuAction::ColorRange,
            MenuAction::Modify(M::Border),
            MenuAction::Modify(M::Smooth),
            MenuAction::Modify(M::Expand),
            MenuAction::Modify(M::Contract),
            MenuAction::Modify(M::Feather),
            MenuAction::SaveSelection,
            MenuAction::LoadSelection,
        ];
        let raw = |events: Vec<egui::Event>| egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1400.0, 900.0),
            )),
            events,
            ..Default::default()
        };
        for action in rows {
            let mut ed = with_two_layers(dir.path());
            select_rect(&mut ed, (4, 4), (20, 20));
            ed.active_mut().unwrap().document.saved_selections.push((
                "Alpha 1".to_string(),
                editor_core::Selection::Rect {
                    min: glam::IVec2::new(0, 0),
                    max: glam::IVec2::new(3, 3),
                },
            ));
            let before_selection = ed.active().unwrap().document.selection.clone();

            let mut chrome = crate::chrome::Chrome::new();
            let ctx = egui::Context::default();
            design::apply_theme(&ctx, design::Theme::Dark);
            // One frame so the workspace mirrors the document (the saved
            // selections count the Load row is gated on) as it does live.
            let _ = ctx.run(raw(Vec::new()), |ctx| {
                let _ = chrome.ui(ctx, &mut ed);
            });
            let menu_ctx = context(&mut ed, chrome.workspace());
            let intent = resolve_intent(action, &menu_ctx, &ed)
                .unwrap_or_else(|reason| panic!("{action:?} is greyed: {reason}"));
            let mut out = ChromeOutput::default();
            chrome.menu_click(intent, &ed, &mut out);
            assert!(
                chrome.dialog_open(),
                "{action:?} did not open its dialog from the menu bar"
            );
            assert!(out.menu.is_empty(), "{action:?} ran at its defaults");

            let _ = ctx.run(raw(Vec::new()), |ctx| {
                let _ = chrome.ui(ctx, &mut ed);
            });
            let mut out = ChromeOutput::default();
            let enter = egui::Event::Key {
                key: egui::Key::Enter,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
                physical_key: None,
            };
            let _ = ctx.run(raw(vec![enter]), |ctx| {
                out = chrome.ui(ctx, &mut ed);
            });
            assert!(!chrome.dialog_open(), "{action:?}: Enter did not confirm");
            assert_eq!(
                out.menu,
                vec![action],
                "{action:?}: the pick did not travel"
            );
            for named in std::mem::take(&mut out.menu) {
                perform(named, &mut ed)
                    .unwrap_or_else(|reason| panic!("{action:?} refused: {reason}"));
            }
            let doc = &ed.active().unwrap().document;
            if action == MenuAction::SaveSelection {
                assert_eq!(
                    doc.saved_selections
                        .iter()
                        .map(|(n, _)| n.as_str())
                        .collect::<Vec<_>>(),
                    vec!["Alpha 1", "Alpha 2"],
                    "Save did not store under the dialog's default name"
                );
            } else {
                assert_ne!(
                    doc.selection, before_selection,
                    "{action:?} left the selection as it was"
                );
            }
        }
    }

    #[test]
    fn a_three_pixel_inside_stroke_differs_from_an_outside_one() {
        // The Validate for the stroke dialog: same selection, same width, and
        // the location choice alone moves the band — inside strokes the
        // selection's own edge pixels, outside paints beside them.
        let paint = |location: ui::dialogs::StrokeLocation| -> (Vec<u8>, Vec<u8>, usize) {
            let dir = tempfile::tempdir().unwrap();
            let mut ed = opened(dir.path());
            let layer = ed.active().unwrap().document.active_layer().unwrap();
            let before = pixels::read_layer(ed.active().unwrap(), layer);
            select_rect(&mut ed, (8, 8), (24, 24));
            let spec = ui::dialogs::StrokeSpec {
                width: 3,
                location,
                ..Default::default()
            };
            stroke_selection_with(&mut ed, &spec).unwrap();
            let after = pixels::read_layer(ed.active().unwrap(), layer);
            let steps = ed.active().unwrap().history.undo_depth();
            (before, after, steps)
        };
        let (before, inside, inside_steps) = paint(ui::dialogs::StrokeLocation::Inside);
        let (_, outside, outside_steps) = paint(ui::dialogs::StrokeLocation::Outside);
        assert_ne!(
            inside, outside,
            "3 px inside and 3 px outside stroked identical pixels"
        );
        assert_eq!(
            inside_steps, 1,
            "the inside stroke is exactly one undo step"
        );
        assert_eq!(
            outside_steps, 1,
            "the outside stroke is exactly one undo step"
        );
        // And the geometric claim itself: the inside band repaints the
        // selection's first pixel, the outside band leaves it alone. (The
        // probe is a noisy image, so "untouched" means equal to `before`, not
        // equal to white.)
        let w = 48usize;
        let idx = |x: usize, y: usize| (y * w + x) * 4;
        assert_ne!(
            &inside[idx(8, 8)..idx(8, 8) + 4],
            &before[idx(8, 8)..idx(8, 8) + 4]
        );
        assert_eq!(
            &outside[idx(8, 8)..idx(8, 8) + 4],
            &before[idx(8, 8)..idx(8, 8) + 4]
        );
    }

    #[test]
    fn fill_honours_opacity_preserve_transparency_and_blend() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        select_rect(&mut ed, (0, 0), (16, 16));

        // Half-strength black over transparent lands at half alpha — fully
        // deterministic, unlike blending against the probe's noise.
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        let before = pixels::read_layer(ed.active().unwrap(), layer);
        assert!(invoke(&mut ed, MenuAction::ClearPixels).unwrap());
        let spec = ui::dialogs::FillSpec {
            contents: ui::dialogs::FillContents::Color([0.0, 0.0, 0.0, 1.0]),
            opacity: 0.5,
            ..Default::default()
        };
        fill_selection_with(&mut ed, &spec).unwrap();
        let after = pixels::read_layer(ed.active().unwrap(), layer);
        assert_eq!(
            &after[0..4],
            &[0, 0, 0, 128],
            "50% black over transparent is black at half alpha, was {:?}",
            &before[0..4]
        );
        assert_eq!(
            ed.active().unwrap().history.undo_depth(),
            2,
            "clear plus fill is two undo steps — the fill alone is one"
        );

        // Preserve transparency keeps an untouched (transparent) region
        // transparent; the same fill without it paints there.
        let mut ed = opened(dir.path());
        let spec = ui::dialogs::FillSpec {
            contents: ui::dialogs::FillContents::Color([1.0, 0.0, 0.0, 1.0]),
            preserve_transparency: true,
            ..Default::default()
        };
        // A new layer starts transparent; the probe may hold pixels, so paint
        // on a fresh transparent layer instead.
        ed.dispatch(crate::action::Action::NewLayer).unwrap();
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        select_rect(&mut ed, (0, 0), (8, 8));
        // On a fully transparent selection there is nothing to preserve, so
        // the engine refuses rather than silently painting.
        assert_eq!(
            fill_selection_with(&mut ed, &spec).unwrap_err(),
            "The fill would change nothing"
        );
        let after = pixels::read_layer(ed.active().unwrap(), layer);
        assert_eq!(after[3], 0, "preserve transparency painted nothing");
        let spec = ui::dialogs::FillSpec {
            contents: ui::dialogs::FillContents::Color([1.0, 0.0, 0.0, 1.0]),
            ..Default::default()
        };
        fill_selection_with(&mut ed, &spec).unwrap();
        let after = pixels::read_layer(ed.active().unwrap(), layer);
        assert_eq!(after[3], 255, "without it the fill paints");

        // Multiply: black ink over the red fill stays black; white ink is a
        // no-op, which is the mode's whole point.
        let spec = ui::dialogs::FillSpec {
            contents: ui::dialogs::FillContents::Color([0.0, 0.0, 0.0, 1.0]),
            blend: layer_model::BlendMode::Multiply,
            ..Default::default()
        };
        fill_selection_with(&mut ed, &spec).unwrap();
        let after = pixels::read_layer(ed.active().unwrap(), layer);
        assert_eq!(&after[0..3], &[0, 0, 0], "multiply by black is black");
    }

    #[test]
    fn the_fill_and_stroke_menu_items_open_their_dialogs() {
        let dir = tempfile::tempdir().unwrap();
        let ed = opened(dir.path());
        let mut host = crate::dialog_host::DialogHost::default();
        assert!(host.open_for_menu_action(&MenuAction::FillDialog, &ed));
        assert!(host.is_open(), "Fill opened the dialog");
        host.close();
        assert!(host.open_for_menu_action(&MenuAction::StrokeDialog, &ed));
        assert!(host.is_open(), "Stroke opened the dialog");
        // A stroke dialog opened at Photopea's defaults: 3 px inside.
        assert_eq!(host.active_stroke_dialog_for_test().spec().width, 3);
        assert_eq!(
            host.active_stroke_dialog_for_test().spec().location,
            ui::dialogs::StrokeLocation::Inside
        );
    }

    /// W7-I: paint the active layer of `ed` from `f(x, y)` (straight RGBA8).
    fn paint_active(ed: &mut Editor, f: impl Fn(usize, usize) -> [u8; 4]) -> LayerId {
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        let (w, h) = canvas_of(ed).unwrap();
        let mut rgba = vec![0u8; w as usize * h as usize * 4];
        for y in 0..h as usize {
            for x in 0..w as usize {
                let i = (y * w as usize + x) * 4;
                rgba[i..i + 4].copy_from_slice(&f(x, y));
            }
        }
        let paint = {
            let doc = ed.active_mut().unwrap();
            pixels::write_layer(doc, layer, &rgba, "Probe").unwrap()
        };
        ed.apply_command(paint);
        layer
    }

    /// W7-I: Fill ▸ Contents: Content-Aware (the spec the Fill dialog confirms
    /// and the shell hands to `fill_selection_with`) rebuilds vertical stripes
    /// inside the selection from the rest of the layer, in ONE undo step, and
    /// leaves everything outside the selection alone.
    #[test]
    fn a_content_aware_fill_rebuilds_stripes_in_one_undo_step() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let stripe = |x: usize| if x % 8 < 4 { 0u8 } else { 255u8 };
        // The stripes, with a red blotch where the selection will go.
        let layer = paint_active(&mut ed, |x, y| {
            if (18..30).contains(&x) && (10..22).contains(&y) {
                [255, 0, 0, 255]
            } else {
                let v = stripe(x);
                [v, v, v, 255]
            }
        });
        select_rect(&mut ed, (18, 10), (30, 22));
        let before = pixels::read_layer(ed.active().unwrap(), layer);
        let steps = ed.active().unwrap().history.undo_depth();
        let spec = ui::dialogs::FillSpec {
            contents: ui::dialogs::FillContents::ContentAware,
            ..Default::default()
        };
        fill_selection_with(&mut ed, &spec).unwrap();
        assert_eq!(
            ed.active().unwrap().history.undo_depth(),
            steps + 1,
            "a content-aware fill is exactly one undo step"
        );
        let after = pixels::read_layer(ed.active().unwrap(), layer);
        let w = 48usize;
        let (mut good, mut total) = (0, 0);
        for y in 10..22usize {
            for x in 18..30usize {
                let i = (y * w + x) * 4;
                total += 1;
                let want = stripe(x);
                if after[i..i + 3].iter().all(|&c| c.abs_diff(want) < 48) {
                    good += 1;
                }
            }
        }
        assert!(
            good * 100 >= total * 90,
            "only {good}/{total} filled pixels follow the stripes"
        );
        for y in 0..32usize {
            for x in 0..w {
                if (18..30).contains(&x) && (10..22).contains(&y) {
                    continue;
                }
                let i = (y * w + x) * 4;
                assert_eq!(after[i..i + 4], before[i..i + 4], "({x},{y}) moved");
            }
        }
        // And one undo puts the blotch back.
        ed.dispatch(crate::action::Action::Undo).unwrap();
        assert_eq!(pixels::read_layer(ed.active().unwrap(), layer), before);
    }

    /// W7-I: with no selection there is nothing to synthesise, and the fill
    /// says so rather than painting anything.
    #[test]
    fn a_content_aware_fill_without_a_selection_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        ed.active_mut().unwrap().document.selection = editor_core::Selection::None;
        let spec = ui::dialogs::FillSpec {
            contents: ui::dialogs::FillContents::ContentAware,
            ..Default::default()
        };
        assert_eq!(
            fill_selection_with(&mut ed, &spec).unwrap_err(),
            "Content-Aware fill needs a selection to fill"
        );
    }

    /// W7-I: Edit ▸ Content-Aware Scale is a real menu row, and picking
    /// "Width to 80%" through the menu resolver keeps a high-contrast
    /// object's width where a plain 80% resample would shrink it.
    #[test]
    fn the_content_aware_scale_row_keeps_an_objects_width() {
        let step = ui::menu::ContentAwareScaleStep::Width80;
        let edit = ui::menu::menu_bar(0)
            .into_iter()
            .find(|m| m.title == "Edit")
            .expect("an Edit menu");
        let row = edit.entries.iter().find_map(|e| match e {
            Entry::Submenu { label, entries } if *label == "Content-Aware Scale" => {
                Some(entries.iter().flat_map(Entry::actions).collect::<Vec<_>>())
            }
            _ => None,
        });
        assert!(
            row.is_some_and(|actions| actions.contains(&MenuAction::ContentAwareScale(step))),
            "Edit has no Content-Aware Scale row for {step:?}"
        );

        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        ed.active_mut().unwrap().document.selection = editor_core::Selection::None;
        // A black 12-px square near the left edge on a light ground with a
        // faint grain: carving columns without an energy map would eat it.
        let layer = paint_active(&mut ed, |x, y| {
            if (3..15).contains(&x) && (10..22).contains(&y) {
                [0, 0, 0, 255]
            } else {
                let v = 230 + ((x * 7 + y * 13) % 5) as u8;
                [v, v, v, 255]
            }
        });
        assert!(invoke(&mut ed, MenuAction::ContentAwareScale(step)).unwrap());
        let after = pixels::read_layer(ed.active().unwrap(), layer);
        let w = 48usize;
        let dark = (0..w)
            .filter(|&x| {
                let i = (16 * w + x) * 4;
                after[i + 3] > 0 && after[i] < 128
            })
            .count();
        // A plain resample to 80% leaves 12 * 0.8 = 9.6 dark columns.
        assert_eq!(dark, 12, "the square is {dark} columns wide after carving");
        // The canvas is 48 wide; the carved layer is 38 wide and centred, so
        // the outer bands are transparent now.
        assert_eq!(after[3], 0, "the left band is transparent");
        assert_eq!(
            after[(16 * w + 47) * 4 + 3],
            0,
            "the right band is transparent"
        );
    }

    #[test]
    fn defining_a_pattern_offers_it_again_after_a_restart() {
        // The Validate for the preset store: define through the menu item,
        // persist, reopen the editor over the same config directory, and the
        // pattern is offered again — to the Fill dialog and to the engine.
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        std::fs::create_dir_all(&config).unwrap();
        let mut ed = opened(&config);
        select_rect(&mut ed, (4, 4), (6, 6));
        invoke(&mut ed, MenuAction::DefinePattern).unwrap();
        assert_eq!(ed.presets().pattern_names(), vec!["Pattern 1"]);
        ed.persist().unwrap();
        drop(ed);

        let mut reopened = opened(&config);
        assert_eq!(
            reopened.presets().pattern_names(),
            vec!["Pattern 1"],
            "the defined pattern survived the restart"
        );

        // The Fill dialog offers it; the engine paints it tiled.
        let mut host = crate::dialog_host::DialogHost::default();
        assert!(host.open_for_menu_action(&MenuAction::FillDialog, &reopened));
        use ui::dialogs::Dialog as _;
        let confirmed = match host.active_fill_dialog_for_test().confirm() {
            Some(ui::dialogs::DialogAction::Fill(spec)) => spec,
            other => panic!("confirm produced {other:?}"),
        };
        assert!(
            matches!(confirmed.contents, ui::dialogs::FillContents::Foreground),
            "the dialog opens on the foreground, not a pattern"
        );

        let spec = ui::dialogs::FillSpec {
            contents: ui::dialogs::FillContents::Pattern("Pattern 1".to_string()),
            ..Default::default()
        };
        select_rect(&mut reopened, (0, 0), (4, 4));
        fill_selection_with(&mut reopened, &spec).unwrap();
        let layer = reopened.active().unwrap().document.active_layer().unwrap();
        let after = pixels::read_layer(reopened.active().unwrap(), layer);
        // The pattern is the 2x2 snapshot of the probe at (4,4), tiled: the
        // fill at (0,0) equals the snapshot's (0,0), i.e. the probe at (4,4).
        let before_probe = pixels::read_layer(reopened.active().unwrap(), layer);
        let _ = before_probe;
        assert_eq!(
            after[0..4],
            after[(2 * 48 + 2) * 4..(2 * 48 + 2) * 4 + 4],
            "the pattern tiles"
        );
    }

    #[test]
    fn defining_a_brush_preset_offers_it_again_after_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        std::fs::create_dir_all(&config).unwrap();
        let mut ed = opened(&config);
        ed.set_brush(tools::BrushSettings {
            size: 33.0,
            ..Default::default()
        });
        invoke(&mut ed, MenuAction::DefineBrush).unwrap();
        ed.persist().unwrap();
        drop(ed);

        let reopened = opened(&config);
        let brushes = reopened.presets().brushes();
        assert_eq!(brushes.len(), 1, "the preset survived the restart");
        let settings: tools::BrushSettings = serde_json::from_str(&brushes[0].1).unwrap();
        // W9-E: Define Brush Preset makes a brush FROM PIXELS (the probe
        // layer), not a copy of the settings: a sampled tip whose pixels are
        // stored with the presets and come back after the restart.
        let tools::brush::BrushTip::Sampled(id) = settings.tip else {
            panic!("the defined brush is not sampled: {settings:?}");
        };
        let stored = reopened
            .presets()
            .tip(asset_store::BlobHash(id.0))
            .expect("the tip's pixels survived the restart");
        assert_eq!(settings.size, stored.width.max(stored.height) as f32);
        assert!(
            tools::brush::sampled_tip(id).is_some(),
            "the reopened editor registered the stored tip"
        );
    }

    #[test]
    fn a_new_pattern_fill_layer_tiles_the_latest_pattern() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let layer_before = ed.active().unwrap().document.active_layer().unwrap();
        select_rect(&mut ed, (2, 2), (4, 4));
        invoke(&mut ed, MenuAction::DefinePattern).unwrap();
        invoke(
            &mut ed,
            MenuAction::NewFillLayer(ui::menu::FillLayerKind::Pattern),
        )
        .unwrap();

        // The fill layer is a NEW layer (the old one still holds its pixels);
        // find it by elimination and read that.
        let doc = ed.active().unwrap();
        let layer = (doc.document.layers.root())
            .iter()
            .copied()
            .find(|id| *id != layer_before)
            .expect("the fill layer was added");
        let after = pixels::read_layer(doc, layer);
        // The new layer is tiled with the 2x2 pattern everywhere: pixel (0,0)
        // repeats at (2,0) and (0,2).
        assert_eq!(after[0..4], after[(2 * 48) * 4..(2 * 48) * 4 + 4]);
        assert_eq!(after[0..4], after[(2 * 48 + 2) * 4..(2 * 48 + 2) * 4 + 4]);
        assert_eq!(after[3], 255, "the pattern fill layer holds pixels");
        assert_eq!(doc.history.undo_depth(), 1, "one undoable step");
    }

    #[test]
    fn canvas_size_is_enabled_and_hosted_now() {
        assert_eq!(unavailable_reason(MenuAction::CanvasSize), None);
        let dir = tempfile::tempdir().unwrap();
        let ed = with_two_layers(dir.path());
        let mut host = crate::dialog_host::DialogHost::default();
        assert!(host.open_for_menu_action(&MenuAction::CanvasSize, &ed));
    }

    #[test]
    fn arbitrary_rotation_is_enabled_and_hosted_now() {
        assert_eq!(
            unavailable_reason(MenuAction::RotateCanvas(
                ui::menu::CanvasRotation::Arbitrary
            )),
            None
        );
        let dir = tempfile::tempdir().unwrap();
        let ed = with_two_layers(dir.path());
        let mut host = crate::dialog_host::DialogHost::default();
        assert!(host.open_for_menu_action(
            &MenuAction::RotateCanvas(ui::menu::CanvasRotation::Arbitrary),
            &ed
        ));
    }

    #[test]
    fn filter_parameter_dialogs_are_enabled_and_hosted() {
        // The two identity-at-defaults filters were the reason this dialog
        // surface was missing; both are hosted now.
        assert_eq!(
            unavailable_reason(MenuAction::Filter(ui::menu::FilterId::Custom)),
            None
        );
        assert_eq!(
            unavailable_reason(MenuAction::Filter(ui::menu::FilterId::Offset)),
            None
        );
        let dir = tempfile::tempdir().unwrap();
        let ed = with_two_layers(dir.path());
        let mut host = crate::dialog_host::DialogHost::default();
        assert!(
            host.open_for_menu_action(&MenuAction::Filter(ui::menu::FilterId::GaussianBlur), &ed,),
            "the filter opened its dialog"
        );
    }

    #[test]
    fn image_size_is_enabled_and_hosted_now() {
        // The dialog host opens the real dialog and the confirmed spec lands
        // as one undoable `ResampleImage` — the reason this row wore is gone.
        assert_eq!(unavailable_reason(MenuAction::ImageSize), None);
        let dir = tempfile::tempdir().unwrap();
        let ed = with_two_layers(dir.path());
        let mut host = crate::dialog_host::DialogHost::default();
        assert!(host.open_for_menu_action(&MenuAction::ImageSize, &ed));
    }

    #[test]
    fn no_menu_item_falls_back_to_the_generic_refusal() {
        // `NOT_WIRED` is the fallback, and nothing may reach it: every item
        // this build cannot perform names the specific piece that is missing.
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new();
        for ed in [editor(dir.path()), with_two_layers(dir.path())] {
            let mut ed = ed;
            let context = context(&mut ed, &ws);
            for menu in menus(&ed) {
                for action in menu.actions() {
                    if let Err(reason) = resolve(action, &context, &ed) {
                        assert_ne!(
                            reason, NOT_WIRED,
                            "{action:?} still wears the generic refusal"
                        );
                        // A refusal from the shared model ("No document is
                        // open") is short because the whole state is the
                        // reason. A refusal from *this shell* has to name the
                        // missing piece, and that takes more than four words.
                        if unavailable_reason(action) == Some(reason.as_str()) {
                            assert!(
                                reason.len() > 30,
                                "{action:?}'s reason is too thin to act on: {reason}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn every_reason_this_shell_gives_names_something_specific() {
        // A sentence that says "not supported" is the generic refusal with
        // extra words. Each of these has to name the crate, the type or the
        // surface that is missing, which is what makes the gap actionable.
        for action in MenuAction::all() {
            let Some(reason) = unavailable_reason(action) else {
                continue;
            };
            assert!(
                reason.len() > 30,
                "{action:?}: {reason:?} is not a reason, it is a shrug"
            );
            assert_ne!(reason, NOT_WIRED);
        }
    }

    #[test]
    fn switching_appearance_writes_the_preference_rather_than_a_dead_action() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        let context = context(&mut ed, &Workspace::new());
        let other = match context.theme {
            design::Theme::Dark => design::Theme::Light,
            design::Theme::Light => design::Theme::Dark,
        };
        match resolve(MenuAction::SetTheme(other), &context, &ed) {
            Ok(Pick::Preferences(prefs)) => assert_eq!(
                prefs.theme,
                match other {
                    design::Theme::Light => ThemeChoice::Light,
                    design::Theme::Dark => ThemeChoice::Dark,
                }
            ),
            got => panic!("appearance resolved to {got:?}"),
        }
    }

    #[test]
    fn stroke_paints_the_foreground_along_the_selection_border() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        let before = pixels::read_layer(ed.active().unwrap(), layer);
        select_rect(&mut ed, (4, 4), (12, 10));
        ed.set_foreground([0.0, 0.0, 1.0, 1.0]);
        assert!(invoke(&mut ed, MenuAction::StrokeDialog).unwrap());
        let after = pixels::read_layer(ed.active().unwrap(), layer);
        let w = ed.active().unwrap().document.width() as usize;
        let border = (4 * w + 4) * 4; // a corner of the selection border
        let interior = (7 * w + 8) * 4; // well inside the selection
                                        // The band is anti-aliased (the erosion it is built from is soft), so
                                        // the rim reads as blue-tinted rather than pure blue at the corner.
        assert!(
            after[border + 2] > before[border + 2] && after[border] < before[border],
            "the border was stroked toward the foreground colour, got {:?} from {:?}",
            &after[border..border + 4],
            &before[border..border + 4]
        );
        assert_eq!(
            &after[interior..interior + 4],
            &before[interior..interior + 4],
            "the interior was left untouched"
        );
    }

    #[test]
    fn save_deselect_reselect_uses_the_selection_store() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        select_rect(&mut ed, (2, 2), (6, 6));
        assert!(invoke(&mut ed, MenuAction::SaveSelection).unwrap());
        assert_eq!(
            ed.active().unwrap().document.saved_selections.len(),
            1,
            "one selection saved"
        );
        assert!(invoke(&mut ed, MenuAction::Deselect).unwrap());
        assert!(ed.active().unwrap().document.selection.is_none());
        // Reselect's enablement depends on the workspace's has_stored_selection
        // flag (populated live from the document by the chrome); the headless
        // invoke context starts with it false, so call the command directly.
        perform(MenuAction::Reselect, &mut ed).unwrap();
        // The stored selection came back as the live one.
        let bounds = ed.active().unwrap().document.selection.bounds().unwrap();
        assert_eq!(bounds.0, glam::IVec2::new(2, 2));
        assert_eq!(bounds.1, glam::IVec2::new(6, 6));
    }

    /// Card 026: the Layers panel's double-click intent routes to the shell's
    /// enter-text-session output.
    #[test]
    fn the_enter_text_layer_intent_routes_to_the_shell() {
        use layer_model::{Layer, LayerKind, TextLayer};
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("canvas.png"),
            raster::encode(raster::ExportFormat::Png, 32, 32, &[255u8; 32 * 32 * 4]).unwrap(),
        )
        .unwrap();
        let mut ed = editor(dir.path());
        let layer = Layer::with_kind(
            "Headline",
            LayerKind::Text(TextLayer {
                text: "THUMBNAILS".to_string(),
                font_family: "DejaVu Sans".to_string(),
                size_px: 32.0,
                ..TextLayer::default()
            }),
        );
        let id = layer.id;
        ed.apply_command(Command::create_layer(layer));

        let intent = ui::Intent::EnterTextLayer { layer: id };
        let pick = crate::menu_bridge::pick(&intent, &ed).expect("the intent routes");
        let mut out = crate::chrome::ChromeOutput::default();
        crate::menu_bridge::record(pick, &mut out);
        assert_eq!(out.enter_text_layer, Some(id), "the pick carries the layer");
    }

    // =======================================================================
    // W2-F: the menu gaps the audit found, driven through the menu bar's own
    // click handler (`Chrome::menu_click`) and the dialog host.
    // =======================================================================

    /// A 40x30 PNG that is transparent except for an opaque 12x12 block at
    /// (10, 8): the margins Image > Trim... has to find.
    fn margin_png(dir: &std::path::Path) -> std::path::PathBuf {
        let (w, h) = (40u32, 30u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 8..20 {
            for x in 10..22 {
                let i = ((y * w + x) * 4) as usize;
                rgba[i..i + 4].copy_from_slice(&[200, 30, 30, 255]);
            }
        }
        let path = dir.join("margin.png");
        std::fs::write(
            &path,
            raster::encode(raster::ExportFormat::Png, w, h, &rgba).unwrap(),
        )
        .unwrap();
        path
    }

    /// The `suggested` path every save/export picker was opened at, shared
    /// with the test because the editor owns the dialogs box.
    type Suggestions = std::rc::Rc<std::cell::RefCell<Vec<std::path::PathBuf>>>;

    /// A [`FileDialogs`] that answers like the [`ScriptedDialogs`] it wraps
    /// and copies every picker's `suggested` path out to a shared list — so
    /// a test can see *which* picker a menu row opened (the PSD save's
    /// `.psd` suggestion against the plain export's raster one), not only
    /// what the scripted answer was.
    struct SuggestionRecorder {
        inner: ScriptedDialogs,
        seen: Suggestions,
    }

    impl SuggestionRecorder {
        fn share(&mut self) {
            self.seen
                .borrow_mut()
                .extend(self.inner.suggested.drain(..));
        }
    }

    impl FileDialogs for SuggestionRecorder {
        fn pick_open_file(&mut self) -> Option<std::path::PathBuf> {
            self.inner.pick_open_file()
        }
        fn pick_place_file(&mut self) -> Option<std::path::PathBuf> {
            self.inner.pick_place_file()
        }
        fn pick_replace_file(&mut self) -> Option<std::path::PathBuf> {
            self.inner.pick_replace_file()
        }
        fn pick_open_project(&mut self) -> Option<std::path::PathBuf> {
            self.inner.pick_open_project()
        }
        fn pick_save_path(&mut self, suggested: &std::path::Path) -> Option<std::path::PathBuf> {
            let answer = self.inner.pick_save_path(suggested);
            self.share();
            answer
        }
        fn pick_export_path(&mut self, suggested: &std::path::Path) -> Option<std::path::PathBuf> {
            let answer = self.inner.pick_export_path(suggested);
            self.share();
            answer
        }
        fn pick_export_folder(&mut self) -> Option<std::path::PathBuf> {
            self.inner.pick_export_folder()
        }
        fn confirm_close(&mut self, document: &str) -> CloseChoice {
            self.inner.confirm_close(document)
        }
        fn confirm_recover(&mut self, document: &str) -> bool {
            self.inner.confirm_recover(document)
        }
        fn report_error(&mut self, title: &str, message: &str) {
            self.inner.report_error(title, message);
        }
        fn report_notice(&mut self, title: &str, message: &str) {
            self.inner.report_notice(title, message);
        }
    }

    /// `with_two_layers`, but the export picker answers `path` once and the
    /// paths every picker was opened at come back with the editor.
    fn with_two_layers_exporting_to(
        dir: &std::path::Path,
        path: &std::path::Path,
    ) -> (Editor, Suggestions) {
        let seen = Suggestions::default();
        let mut ed = Editor::with_state(
            AppPaths::rooted(dir.join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(SuggestionRecorder {
                inner: ScriptedDialogs::new().exporting_to(path),
                seen: seen.clone(),
            }),
        );
        ed.set_image_clipboard(Box::new(crate::clipboard::FakeClipboard::new()));
        ed.open_path(&probe_png(dir, 48, 32))
            .expect("the probe opens");
        let extra = layer_model::Layer::raster("Second");
        let id = extra.id;
        ed.apply_command(Command::create_layer(extra));
        let rgba = vec![90u8; 48 * 32 * 4];
        let paint = {
            let doc = ed.active_mut().unwrap();
            pixels::write_layer(doc, id, &rgba, "Second").unwrap()
        };
        ed.apply_command(paint);
        ed.set_active_layer(id);
        (ed, seen)
    }

    /// Route `action` through the menu bar's click handler exactly as `draw`
    /// does. The chrome comes back so a dialog it opened can be driven.
    fn click(ed: &mut Editor, action: MenuAction) -> (crate::chrome::Chrome, ChromeOutput) {
        let mut chrome = crate::chrome::Chrome::new();
        let menu_ctx = context(ed, chrome.workspace());
        let intent = resolve_intent(action, &menu_ctx, ed)
            .unwrap_or_else(|reason| panic!("{action:?} is disabled: {reason}"));
        let mut out = ChromeOutput::default();
        chrome.menu_click(intent, ed, &mut out);
        (chrome, out)
    }

    /// Apply what a click (or a confirmed dialog) put in the output, the way
    /// the shell does: commands through history, menu picks through
    /// `perform`, application actions through `dispatch`.
    fn apply_output(ed: &mut Editor, out: ChromeOutput) -> Result<(), String> {
        for command in out.commands {
            ed.apply_command(command);
        }
        for action in out.menu {
            perform(action, ed)?;
        }
        for action in out.actions {
            ed.dispatch(action).map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Press `pressed` in the chrome's open dialog: a settle frame, then the
    /// key, through the host's own `ui`. Returns what that frame produced.
    fn press_in_dialog(chrome: &mut crate::chrome::Chrome, pressed: egui::Key) -> ChromeOutput {
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let mut out = ChromeOutput::default();
        let host = chrome.dialogs_for_test();
        let _ = ctx.run(raw_input(Vec::new()), |ctx| host.ui(ctx, None, &mut out));
        let _ = ctx.run(raw_input(vec![key(pressed)]), |ctx| {
            host.ui(ctx, None, &mut out)
        });
        out
    }

    /// A raster layer holding one opaque block, painted and made active.
    fn block_layer(ed: &mut Editor, name: &str, x: u32, y: u32, size: u32) -> LayerId {
        let (w, h) = canvas_of(ed).unwrap();
        let layer = layer_model::Layer::raster(name);
        let id = layer.id;
        ed.apply_command(Command::create_layer(layer));
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for yy in y..(y + size).min(h) {
            for xx in x..(x + size).min(w) {
                let i = ((yy * w + xx) * 4) as usize;
                rgba[i..i + 4].copy_from_slice(&[255, 0, 0, 255]);
            }
        }
        let paint = {
            let doc = ed.active_mut().unwrap();
            pixels::write_layer(doc, id, &rgba, name).unwrap()
        };
        ed.apply_command(paint);
        ed.set_active_layer(id);
        id
    }

    fn ink(ed: &Editor, id: LayerId) -> raster::PixelRect {
        let doc = ed.active().unwrap();
        crate::tool_input::tight_document_bounds(&doc.document, &doc.tiles, id)
            .expect("the block layer has ink")
    }

    fn depth(ed: &Editor) -> usize {
        ed.active().unwrap().history_depth()
    }

    /// W9-A: Photopea's Select Pixels, driven through the REAL chrome — a
    /// Ctrl+click on the drawn Layers-panel thumbnail, routed to `perform`
    /// exactly as the shell routes it.
    mod select_layer_pixels {
        use super::*;
        use ui::dialogs::LoadOperation as Op;

        fn frame(
            ctx: &egui::Context,
            chrome: &mut crate::chrome::Chrome,
            ed: &mut Editor,
            events: Vec<egui::Event>,
            modifiers: egui::Modifiers,
        ) -> ChromeOutput {
            let mut input = raw_input(events);
            input.modifiers = modifiers;
            let mut out = ChromeOutput::default();
            let _ = ctx.run(input, |ctx| {
                out = chrome.ui(ctx, ed);
            });
            out
        }

        /// Settle the layout, then press and release on the thumbnail of
        /// `layer` with `modifiers` held; return what the click meant.
        fn click_thumb(
            ctx: &egui::Context,
            chrome: &mut crate::chrome::Chrome,
            ed: &mut Editor,
            id: egui::Id,
            modifiers: egui::Modifiers,
        ) -> ChromeOutput {
            for _ in 0..4 {
                let _ = frame(ctx, chrome, ed, Vec::new(), egui::Modifiers::NONE);
            }
            let pos = ctx
                .read_response(id)
                .unwrap_or_else(|| panic!("{id:?} was never drawn"))
                .rect
                .center();
            let button = |pressed: bool| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers,
            };
            let mut out = frame(
                ctx,
                chrome,
                ed,
                vec![egui::Event::PointerMoved(pos), button(true)],
                modifiers,
            );
            let release = frame(ctx, chrome, ed, vec![button(false)], modifiers);
            out.menu.extend(release.menu);
            out.commands.extend(release.commands);
            out.actions.extend(release.actions);
            out
        }

        fn coverage(ed: &Editor, x: i32, y: i32) -> u8 {
            match &ed.active().unwrap().document.selection {
                editor_core::Selection::Mask(m) => m.coverage_at(glam::IVec2::new(x, y)),
                other => panic!("expected a mask selection, got {other:?}"),
            }
        }

        #[test]
        fn a_ctrl_click_on_a_thumbnail_selects_exactly_the_layers_ink_and_shift_adds() {
            let dir = tempfile::tempdir().unwrap();
            let mut ed = editor_with_a_document(dir.path());
            let square = block_layer(&mut ed, "Square", 20, 30, 10);
            let other = block_layer(&mut ed, "Other", 50, 50, 10);
            assert_eq!(ed.active().unwrap().document.active_layer(), Some(other));
            let ctx = egui::Context::default();
            crate::chrome::install_theme(&ctx, design::Theme::Dark);
            let mut chrome = crate::chrome::Chrome::new();

            // Ctrl+click the Square's thumbnail: the chrome routes one
            // Select Pixels for that layer.
            let out = click_thumb(
                &ctx,
                &mut chrome,
                &mut ed,
                ui::view::ids::layer_content_thumb(square),
                egui::Modifiers::COMMAND,
            );
            assert_eq!(
                out.menu,
                vec![MenuAction::SelectLayerPixels {
                    layer: Some(square),
                    mask: false,
                    op: Op::New,
                }],
                "{out:?}"
            );
            let before = depth(&ed);
            apply_output(&mut ed, out).unwrap();
            assert_eq!(depth(&ed), before + 1, "one undo step");
            assert_eq!(
                ed.active().unwrap().document.selection.bounds(),
                Some((glam::IVec2::new(20, 30), glam::IVec2::new(30, 40))),
                "exactly the 10x10 square"
            );
            for (x, y) in [(20, 30), (29, 39), (25, 35)] {
                assert_eq!(coverage(&ed, x, y), 255, "inside at ({x},{y})");
            }
            for (x, y) in [(19, 30), (30, 39), (25, 40), (55, 55)] {
                assert_eq!(coverage(&ed, x, y), 0, "outside at ({x},{y})");
            }
            // Photopea's Ctrl+click does not change the active layer.
            assert_eq!(ed.active().unwrap().document.active_layer(), Some(other));

            // Ctrl+Shift+click the Other thumbnail: added to the selection.
            let out = click_thumb(
                &ctx,
                &mut chrome,
                &mut ed,
                ui::view::ids::layer_content_thumb(other),
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
            );
            assert_eq!(
                out.menu,
                vec![MenuAction::SelectLayerPixels {
                    layer: Some(other),
                    mask: false,
                    op: Op::Add,
                }],
                "{out:?}"
            );
            let before = depth(&ed);
            apply_output(&mut ed, out).unwrap();
            assert_eq!(depth(&ed), before + 1, "one undo step");
            assert_eq!(
                ed.active().unwrap().document.selection.bounds(),
                Some((glam::IVec2::new(20, 30), glam::IVec2::new(60, 60)))
            );
            assert_eq!(coverage(&ed, 25, 35), 255, "the square is kept");
            assert_eq!(coverage(&ed, 55, 55), 255, "the other block is added");
            assert_eq!(coverage(&ed, 40, 45), 0, "the gap is not selected");

            // One undo takes the add back and leaves the square.
            ed.dispatch(Action::Undo).unwrap();
            assert_eq!(
                ed.active().unwrap().document.selection.bounds(),
                Some((glam::IVec2::new(20, 30), glam::IVec2::new(30, 40)))
            );
        }

        #[test]
        fn subtract_intersect_and_the_row_menu_act_on_the_layers_alpha() {
            let dir = tempfile::tempdir().unwrap();
            let mut ed = editor_with_a_document(dir.path());
            let square = block_layer(&mut ed, "Square", 20, 20, 10);
            // A live rect selection overlapping the square's right half.
            ed.apply_command(Command::SetSelection {
                selection: editor_core::Selection::Rect {
                    min: glam::IVec2::new(25, 20),
                    max: glam::IVec2::new(40, 30),
                },
            });
            let sub = MenuAction::SelectLayerPixels {
                layer: Some(square),
                mask: false,
                op: Op::Subtract,
            };
            perform(sub, &mut ed).unwrap();
            assert_eq!(
                ed.active().unwrap().document.selection.bounds(),
                Some((glam::IVec2::new(30, 20), glam::IVec2::new(40, 30))),
                "the square is taken out of the rect"
            );
            ed.dispatch(Action::Undo).unwrap();
            let inter = MenuAction::SelectLayerPixels {
                layer: Some(square),
                mask: false,
                op: Op::Intersect,
            };
            perform(inter, &mut ed).unwrap();
            assert_eq!(
                ed.active().unwrap().document.selection.bounds(),
                Some((glam::IVec2::new(25, 20), glam::IVec2::new(30, 30))),
                "only the overlap is kept"
            );

            // The layer-row menu's row (the active layer) resolves and
            // performs through the same bridge.
            ed.apply_command(Command::SetSelection {
                selection: editor_core::Selection::None,
            });
            let chrome = crate::chrome::Chrome::new();
            let menu_ctx = context(&mut ed, chrome.workspace());
            let row = ui::context_menu::layer_items(&menu_ctx)
                .into_iter()
                .find(|i| i.label == "Select Pixels")
                .expect("the row menu carries Select Pixels");
            let intent = resolve_intent(row.action, &menu_ctx, &ed).unwrap();
            assert_eq!(intent, ui::Intent::Action(row.action));
            perform(row.action, &mut ed).unwrap();
            assert_eq!(
                ed.active().unwrap().document.selection.bounds(),
                Some((glam::IVec2::new(20, 20), glam::IVec2::new(30, 30)))
            );
        }

        #[test]
        fn a_mask_thumbnail_selects_the_mask_coverage() {
            let dir = tempfile::tempdir().unwrap();
            let mut ed = editor_with_a_document(dir.path());
            let layer = block_layer(&mut ed, "Masked", 0, 0, 64);
            ed.apply_command(Command::SetSelection {
                selection: editor_core::Selection::Rect {
                    min: glam::IVec2::new(8, 12),
                    max: glam::IVec2::new(18, 22),
                },
            });
            perform(MenuAction::Mask(ui::menu::MaskOp::RevealSelection), &mut ed).unwrap();
            ed.apply_command(Command::SetSelection {
                selection: editor_core::Selection::None,
            });
            assert!(ed
                .active()
                .unwrap()
                .document
                .layers
                .get(layer)
                .unwrap()
                .mask
                .is_some());
            perform(
                MenuAction::SelectLayerPixels {
                    layer: Some(layer),
                    mask: true,
                    op: Op::New,
                },
                &mut ed,
            )
            .unwrap();
            assert_eq!(
                ed.active().unwrap().document.selection.bounds(),
                Some((glam::IVec2::new(8, 12), glam::IVec2::new(18, 22))),
                "the mask's revealed rect, not the layer's 64x64 ink"
            );
        }
    }

    #[test]
    fn save_as_psd_writes_a_layered_file_the_psd_crate_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("stacked.psd");
        let (mut ed, suggested) = with_two_layers_exporting_to(dir.path(), &target);
        let (chrome, out) = click(&mut ed, MenuAction::SaveAsPsd);
        assert!(
            !chrome.dialog_open(),
            "Save as PSD is a file picker, not a modal"
        );
        assert_eq!(out.menu, vec![MenuAction::SaveAsPsd]);
        apply_output(&mut ed, out).expect("the export ran");
        // The row opened a *PSD* picker — the one that leads with the PSD
        // filter, is titled "Save as PSD" and suggests a `.psd` — not the
        // plain export picker suggesting a raster. The scripted picker
        // answers `target` either way, so the suggestion is what tells them
        // apart; without the arming it would end in `.png`.
        let extension = |path: &std::path::PathBuf| {
            path.extension().and_then(|e| e.to_str()).map(str::to_owned)
        };
        {
            let seen = suggested.borrow();
            assert_eq!(seen.len(), 1, "exactly one picker opened: {seen:?}");
            assert_eq!(
                extension(&seen[0]).as_deref(),
                Some(crate::dialogs::PSD_EXTENSION),
                "File > Save as PSD opened the PSD picker: {seen:?}"
            );
        }
        let bytes = std::fs::read(&target).expect("the .psd was written");
        let file = psd::read(&bytes).expect("the psd crate reads it back");
        assert_eq!(file.layers.len(), 2, "both layers survived the round trip");
        assert_eq!(file.header.width, 48);
        assert_eq!(file.header.height, 32);
        assert!(
            ed.status().unwrap().contains("PSD"),
            "the status names the PSD: {:?}",
            ed.status()
        );
        // Control: the plain Export from the same editor opens the plain
        // picker, whose suggestion is a raster — so the assertion above is
        // not one every picker satisfies, and one arming affects exactly one
        // picker. (The scripted answers are spent, so this picker cancels.)
        let _ = ed.dispatch(Action::Export);
        {
            let seen = suggested.borrow();
            assert_eq!(seen.len(), 2, "the plain export opened a picker: {seen:?}");
            assert_ne!(
                extension(&seen[1]).as_deref(),
                Some(crate::dialogs::PSD_EXTENSION),
                "the plain Export picker does not suggest a .psd: {seen:?}"
            );
            assert_eq!(
                seen[0].with_extension(""),
                seen[1].with_extension(""),
                "the PSD picker suggests the export's own name, in .psd: {seen:?}"
            );
        }
        // And a cancelled picker is a loud refusal, not a silent nothing.
        let mut cancelled = with_two_layers(dir.path());
        let reason = perform(MenuAction::SaveAsPsd, &mut cancelled).unwrap_err();
        assert!(reason.contains("Save as PSD"), "{reason}");
    }

    #[test]
    fn stamp_visible_lands_above_the_active_layer_as_one_undoable_step() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = with_two_layers(dir.path());
        let before = ed.active().unwrap().document.layers.root().to_vec();
        assert_eq!(before.len(), 2);
        let (top, bottom) = (before[0], before[1]);
        // Stamp above the BOTTOM layer, so "above the active one" is
        // distinguishable from "at the top".
        ed.set_active_layer(bottom);
        let expected = composite(&mut ed);
        let steps = depth(&ed);

        let (chrome, out) = click(&mut ed, MenuAction::StampVisible);
        assert!(!chrome.dialog_open());
        assert_eq!(out.menu, vec![MenuAction::StampVisible]);
        apply_output(&mut ed, out).expect("the stamp applied");

        let root = ed.active().unwrap().document.layers.root().to_vec();
        assert_eq!(root.len(), 3, "one new layer");
        assert_eq!(
            (root[0], root[2]),
            (top, bottom),
            "the stack around it is untouched"
        );
        let stamp = root[1];
        assert_eq!(
            ed.active().unwrap().document.active_layer(),
            Some(stamp),
            "the stamp becomes the active layer"
        );
        assert_eq!(
            pixels::read_layer(ed.active().unwrap(), stamp),
            expected,
            "the stamp holds exactly what the canvas showed"
        );
        assert_eq!(depth(&ed), steps + 1, "one undo step");
        ed.dispatch(Action::Undo).unwrap();
        assert_eq!(
            ed.active().unwrap().document.layers.root(),
            &[top, bottom],
            "undo removes the stamp"
        );
    }

    #[test]
    fn align_moves_the_layer_to_the_canvas_or_the_selection_as_one_step() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path()); // 48 x 32
        let id = block_layer(&mut ed, "Block", 10, 10, 4);
        assert_eq!((ink(&ed, id).x, ink(&ed, id).y), (10, 10));
        let steps = depth(&ed);

        // To the canvas: no selection.
        let (_, out) = click(&mut ed, MenuAction::AlignLayers(ui::menu::AlignEdge::Right));
        assert_eq!(
            out.menu,
            vec![MenuAction::AlignLayers(ui::menu::AlignEdge::Right)]
        );
        apply_output(&mut ed, out).unwrap();
        let b = ink(&ed, id);
        assert_eq!(
            b.x + b.width as i64,
            48,
            "the right edge meets the canvas edge"
        );
        assert_eq!(b.y, 10, "a horizontal align leaves y alone");
        assert_eq!(depth(&ed), steps + 1, "one undo step");

        // Already there: a loud refusal, and no undo step.
        let reason =
            perform(MenuAction::AlignLayers(ui::menu::AlignEdge::Right), &mut ed).unwrap_err();
        assert!(reason.contains("Already aligned"), "{reason}");
        assert_eq!(depth(&ed), steps + 1);

        ed.dispatch(Action::Undo).unwrap();
        assert_eq!(ink(&ed, id).x, 10, "undo puts the layer back");

        // To the selection when there is one.
        select_rect(&mut ed, (20, 4), (30, 20));
        let (_, out) = click(&mut ed, MenuAction::AlignLayers(ui::menu::AlignEdge::Left));
        apply_output(&mut ed, out).unwrap();
        assert_eq!(ink(&ed, id).x, 20, "the left edge meets the selection's");
        let (_, out) = click(
            &mut ed,
            MenuAction::AlignLayers(ui::menu::AlignEdge::Bottom),
        );
        apply_output(&mut ed, out).unwrap();
        let b = ink(&ed, id);
        assert_eq!(
            b.y + b.height as i64,
            20,
            "the bottom edge meets the selection's"
        );
        let (_, out) = click(
            &mut ed,
            MenuAction::AlignLayers(ui::menu::AlignEdge::HorizontalCenter),
        );
        apply_output(&mut ed, out).unwrap();
        assert_eq!(ink(&ed, id).x, 23, "centred on the selection's 25");
    }

    #[test]
    fn distribute_spaces_three_selected_layers_evenly_as_one_step() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let a = block_layer(&mut ed, "A", 0, 0, 4);
        let b = block_layer(&mut ed, "B", 4, 0, 4);
        let c = block_layer(&mut ed, "C", 40, 0, 4);
        // Two layers is not enough, and the menu says so.
        ed.set_layer_selection(vec![a, c], Some(c));
        let menu_ctx = context(&mut ed, &Workspace::new());
        assert_eq!(
            resolve_intent(
                MenuAction::DistributeLayers(ui::menu::DistributeAxis::Horizontal),
                &menu_ctx,
                &ed
            ),
            Err("Select three or more layers".to_string())
        );
        ed.set_layer_selection(vec![a, b, c], Some(c));
        let steps = depth(&ed);
        let (_, out) = click(
            &mut ed,
            MenuAction::DistributeLayers(ui::menu::DistributeAxis::Horizontal),
        );
        apply_output(&mut ed, out).unwrap();
        // Centres were 2, 6, 42: the outer two stay, the middle lands on 22.
        assert_eq!(ink(&ed, a).x, 0);
        assert_eq!(ink(&ed, b).x, 20);
        assert_eq!(ink(&ed, c).x, 40);
        assert_eq!(depth(&ed), steps + 1, "one undo step");
        ed.dispatch(Action::Undo).unwrap();
        assert_eq!(ink(&ed, b).x, 4, "undo puts the middle one back");
    }

    #[test]
    fn refine_edge_changes_the_selection_coverage_through_its_dialog_as_one_step() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        // Disabled without a selection, and the reason is the selection's.
        let menu_ctx = context(&mut ed, &Workspace::new());
        assert_eq!(
            resolve_intent(MenuAction::RefineEdge, &menu_ctx, &ed),
            Err("There is no selection".to_string())
        );
        select_rect(&mut ed, (8, 8), (24, 24));
        let before = format!("{:?}", ed.active().unwrap().document.selection);
        let steps = depth(&ed);

        let (mut chrome, out) = click(&mut ed, MenuAction::RefineEdge);
        assert!(chrome.dialog_open(), "Refine Edge opens its dialog");
        assert!(out.is_empty(), "opening changed nothing");
        chrome
            .dialogs_for_test()
            .active_refine_edge_dialog_for_test()
            .set_spec_for_test(ui::dialogs::refine_mask::RefineMaskSpec {
                shift_px: 4,
                ..Default::default()
            });
        let out = press_in_dialog(&mut chrome, egui::Key::Enter);
        assert!(!chrome.dialog_open(), "Enter closed the dialog");
        assert_eq!(out.menu, vec![MenuAction::RefineEdge]);
        assert!(
            out.commands.is_empty() && out.dialog.is_none(),
            "the confirmation must not take the layer-mask road: {out:?}"
        );
        apply_output(&mut ed, out).expect("the refinement applied");

        let after = &ed.active().unwrap().document.selection;
        assert!(
            matches!(after, editor_core::Selection::Mask(_)),
            "{after:?}"
        );
        // The expansion is a disk of radius 4: a pixel 3 outside the left
        // edge (level with the middle) is inside it, one 6 outside is not.
        assert_eq!(
            after.coverage_at(glam::IVec2::new(5, 16)),
            1.0,
            "expanding by 4 selects the pixel 3 outside the old edge"
        );
        assert_eq!(
            after.coverage_at(glam::IVec2::new(2, 16)),
            0.0,
            "and not the pixel 6 outside"
        );
        assert_eq!(depth(&ed), steps + 1, "one undo step");
        ed.dispatch(Action::Undo).unwrap();
        assert_eq!(
            format!("{:?}", ed.active().unwrap().document.selection),
            before,
            "undo restores the rectangle"
        );
    }

    #[test]
    fn the_trim_dialogs_options_reach_the_canvas_as_one_step() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        ed.set_image_clipboard(Box::new(crate::clipboard::FakeClipboard::new()));
        ed.open_path(&margin_png(dir.path())).unwrap();
        let steps = depth(&ed);

        let (mut chrome, out) = click(&mut ed, MenuAction::Trim);
        assert!(chrome.dialog_open(), "Trim... opens its options");
        assert!(out.is_empty());
        chrome
            .dialogs_for_test()
            .active_trim_dialog_for_test()
            .set_spec(ui::dialogs::TrimSpec {
                right: false,
                bottom: false,
                ..Default::default()
            });
        let out = press_in_dialog(&mut chrome, egui::Key::Enter);
        assert_eq!(out.menu, vec![MenuAction::Trim]);
        apply_output(&mut ed, out).expect("the trim applied");
        let doc = &ed.active().unwrap().document;
        assert_eq!(
            (doc.width(), doc.height()),
            (30, 22),
            "only the top and left margins went"
        );
        assert_eq!(depth(&ed), steps + 1, "one undo step");
        ed.dispatch(Action::Undo).unwrap();
        let doc = &ed.active().unwrap().document;
        assert_eq!((doc.width(), doc.height()), (40, 30));

        // Every side, the default: the block alone remains.
        let (mut chrome, _) = click(&mut ed, MenuAction::Trim);
        let out = press_in_dialog(&mut chrome, egui::Key::Enter);
        apply_output(&mut ed, out).unwrap();
        let doc = &ed.active().unwrap().document;
        assert_eq!((doc.width(), doc.height()), (12, 12));
    }

    #[test]
    fn about_opens_a_real_window_from_the_menu_bar() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = with_two_layers(dir.path());
        let (mut chrome, out) = click(&mut ed, MenuAction::About);
        assert!(chrome.dialog_open(), "About opens a window");
        assert!(out.is_empty(), "opening it produces no edit");
        let about_line = crate::version::about_line();
        {
            let host = chrome.dialogs_for_test();
            let about = host.active_about_dialog_for_test();
            assert_eq!(about.version_line(), about_line);
            assert_eq!(about.notices(), crate::dialog_host::THIRD_PARTY_NOTICES);
            assert_eq!(about.licence(), crate::dialog_host::LICENCE_POINTER);
            // The drawn window says the version: read back off the paint list.
            let ctx = egui::Context::default();
            crate::chrome::install_theme(&ctx, design::Theme::Dark);
            let mut drawn = ChromeOutput::default();
            // Two frames, as the nine-menus test runs: egui lays a window
            // out on its first frame and paints it on the second.
            let mut painted = Vec::new();
            for _ in 0..2 {
                let output = ctx.run(raw_input(Vec::new()), |ctx| host.ui(ctx, None, &mut drawn));
                painted = painted_text(&ctx, &output);
            }
            assert!(drawn.is_empty(), "drawing the window produced {drawn:?}");
            assert!(
                painted.iter().any(|t| t.contains(&about_line)),
                "the window never drew {about_line:?}; it drew {painted:?}"
            );
            assert!(painted.iter().any(|t| t == "About Raster Studio"));
        }
        let out = press_in_dialog(&mut chrome, egui::Key::Escape);
        assert!(!chrome.dialog_open(), "Escape closes it");
        assert!(out.is_empty());
        // The notices file the window points at is real.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        assert!(
            root.join(crate::dialog_host::THIRD_PARTY_NOTICES).is_file(),
            "the About window points at a file that does not exist"
        );
    }

    #[test]
    fn blending_options_opens_the_layer_style_dialog_not_the_properties_panel() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = with_two_layers(dir.path());
        let (mut chrome, out) = click(&mut ed, MenuAction::BlendingOptions);
        assert!(chrome.dialog_open(), "Blending Options opens a dialog");
        assert!(
            chrome.dialogs_for_test().layer_style_is_open_for_test(),
            "and it is the Layer Style dialog"
        );
        assert!(
            out.workspace.is_empty(),
            "no panel was revealed instead: {:?}",
            out.workspace
        );
    }

    #[test]
    fn the_guide_rows_ride_history_from_the_menu_bar() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = with_two_layers(dir.path());
        let menu_ctx = context(&mut ed, &Workspace::new());
        assert_eq!(
            resolve_intent(MenuAction::ClearGuides, &menu_ctx, &ed),
            Err("There are no guides to clear".to_string())
        );
        let steps = depth(&ed);

        // New Guide... through its dialog.
        let (mut chrome, out) = click(&mut ed, MenuAction::NewGuide);
        assert!(chrome.dialog_open());
        assert!(out.is_empty());
        {
            let dialog = chrome.dialogs_for_test().active_new_guide_dialog_for_test();
            dialog.set_orientation(ui::dialogs::new_guide::Orientation::Vertical);
            dialog.set_position(12.0);
        }
        let out = press_in_dialog(&mut chrome, egui::Key::Enter);
        assert!(!chrome.dialog_open());
        assert_eq!(out.commands.len(), 1, "one SetGuides command: {out:?}");
        apply_output(&mut ed, out).unwrap();
        let guides = &ed.active().unwrap().document.guides;
        assert_eq!(guides.list.len(), 1);
        assert_eq!(guides.list[0].axis, editor_core::GuideAxis::Vertical);
        assert_eq!(guides.list[0].doc, 12.0);
        assert_eq!(depth(&ed), steps + 1);

        // Lock Guides flips the document flag and ticks.
        assert!(invoke(&mut ed, MenuAction::LockGuides).unwrap());
        assert!(ed.active().unwrap().document.guides.locked);
        let menu_ctx = context(&mut ed, &Workspace::new());
        assert_eq!(MenuAction::LockGuides.checked(&menu_ctx), Some(true));
        assert_eq!(depth(&ed), steps + 2);

        // Clear Guides empties the list and keeps the lock flag.
        assert!(invoke(&mut ed, MenuAction::ClearGuides).unwrap());
        let guides = &ed.active().unwrap().document.guides;
        assert!(guides.list.is_empty());
        assert!(guides.locked);
        assert_eq!(depth(&ed), steps + 3);

        for _ in 0..3 {
            ed.dispatch(Action::Undo).unwrap();
        }
        assert_eq!(
            ed.active().unwrap().document.guides,
            editor_core::Guides::default(),
            "three undos put the guides back to nothing"
        );
    }

    #[test]
    fn rename_layer_renames_the_active_layer_through_its_dialog_as_one_step() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = with_two_layers(dir.path());
        let id = ed.active().unwrap().document.active_layer().unwrap();
        let steps = depth(&ed);
        let (mut chrome, out) = click(&mut ed, MenuAction::RenameLayer);
        assert!(chrome.dialog_open(), "Rename Layer... opens its dialog");
        assert!(out.is_empty());
        chrome
            .dialogs_for_test()
            .active_rename_dialog_for_test()
            .set_name("Sky");
        let out = press_in_dialog(&mut chrome, egui::Key::Enter);
        assert!(!chrome.dialog_open());
        assert_eq!(out.commands.len(), 1, "{out:?}");
        apply_output(&mut ed, out).unwrap();
        let name = |ed: &Editor| {
            ed.active()
                .unwrap()
                .document
                .layers
                .get(id)
                .unwrap()
                .name
                .clone()
        };
        assert_eq!(name(&ed), "Sky");
        assert_eq!(depth(&ed), steps + 1, "one undo step");
        ed.dispatch(Action::Undo).unwrap();
        assert_eq!(name(&ed), "Second", "undo restores the name");
    }

    #[test]
    fn duplicate_layer_asks_for_a_name_and_copies_above_its_source_as_one_step() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = with_two_layers(dir.path());
        let before = ed.active().unwrap().document.layers.root().to_vec();
        assert_eq!(before.len(), 2);
        let (top, bottom) = (before[0], before[1]);
        // Copy the BOTTOM layer, so "directly above its source" is
        // distinguishable from "at the top of the stack".
        ed.set_active_layer(bottom);
        let source_name = ed
            .active()
            .unwrap()
            .document
            .layers
            .get(bottom)
            .unwrap()
            .name
            .clone();
        let source_pixels = pixels::read_layer(ed.active().unwrap(), bottom);
        let steps = depth(&ed);

        let (mut chrome, out) = click(&mut ed, MenuAction::DuplicateLayer);
        assert!(chrome.dialog_open(), "Duplicate Layer... asks for a name");
        assert!(out.is_empty(), "opening the dialog copies nothing: {out:?}");
        {
            let dialog = chrome.dialogs_for_test().active_duplicate_dialog_for_test();
            assert_eq!(dialog.name(), format!("{source_name} copy"));
            dialog.set_name("Twin");
        }
        let out = press_in_dialog(&mut chrome, egui::Key::Enter);
        assert!(!chrome.dialog_open());
        assert_eq!(out.menu, vec![MenuAction::DuplicateLayer]);
        assert!(out.commands.is_empty());
        apply_output(&mut ed, out).expect("the duplicate applied");

        let doc = &ed.active().unwrap().document;
        let root = doc.layers.root().to_vec();
        assert_eq!(root.len(), 3, "one new layer");
        assert_eq!(
            (root[0], root[2]),
            (top, bottom),
            "the copy sits directly above its source, not on top of the stack"
        );
        let copy = root[1];
        assert_eq!(doc.layers.get(copy).unwrap().name, "Twin", "the typed name");
        assert_eq!(
            doc.active_layer(),
            Some(copy),
            "the copy is the active layer"
        );
        assert_eq!(
            pixels::read_layer(ed.active().unwrap(), copy),
            source_pixels,
            "the copy carries its source's pixels"
        );
        assert_eq!(depth(&ed), steps + 1, "one undo step");
        ed.dispatch(Action::Undo).unwrap();
        assert_eq!(
            ed.active().unwrap().document.layers.root().to_vec(),
            before,
            "undo removes the copy"
        );

        // Escape copies nothing. (Undo lifted the copy, which was the active
        // layer, so the source is made active again first.)
        ed.set_active_layer(bottom);
        let (mut chrome, _) = click(&mut ed, MenuAction::DuplicateLayer);
        let out = press_in_dialog(&mut chrome, egui::Key::Escape);
        assert!(!chrome.dialog_open() && out.is_empty(), "{out:?}");
        assert_eq!(ed.active().unwrap().document.layers.root().len(), 2);
    }

    #[test]
    fn layer_via_copy_and_cut_lift_only_the_selected_pixels() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = opened(dir.path());
        let source = ed.active().unwrap().document.active_layer().unwrap();
        let before = pixels::read_layer(ed.active().unwrap(), source);
        select_rect(&mut ed, (8, 8), (16, 16));
        assert!(invoke(&mut ed, MenuAction::LayerViaCopy).unwrap());
        let copy = ed.active().unwrap().document.layers.root()[0];
        assert_ne!(copy, source);
        let lifted = pixels::read_layer(ed.active().unwrap(), copy);
        let w = 48usize;
        let px =
            |buf: &[u8], x: usize, y: usize| buf[(y * w + x) * 4..(y * w + x) * 4 + 4].to_vec();
        assert_eq!(
            px(&lifted, 10, 10),
            px(&before, 10, 10),
            "inside: the source pixel"
        );
        assert_eq!(px(&lifted, 2, 2)[3], 0, "outside: nothing");
        assert_eq!(px(&lifted, 30, 20)[3], 0, "outside: nothing");
        assert_eq!(
            pixels::read_layer(ed.active().unwrap(), source),
            before,
            "a copy leaves the source alone"
        );
        // Cut takes the same pixels and clears them below.
        ed.set_active_layer(source);
        assert!(invoke(&mut ed, MenuAction::LayerViaCut).unwrap());
        let after = pixels::read_layer(ed.active().unwrap(), source);
        assert_eq!(px(&after, 10, 10)[3], 0, "the hole");
        assert_eq!(
            px(&after, 2, 2),
            px(&before, 2, 2),
            "outside the hole, untouched"
        );
    }

    #[test]
    fn step_rows_lock_rows_and_zoom_200_route_through_the_shell() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = with_two_layers(dir.path());
        let menu_ctx = context(&mut ed, &Workspace::new());
        assert_eq!(
            resolve(MenuAction::StepBackward, &menu_ctx, &ed),
            Ok(Pick::Action(Action::Undo))
        );
        assert_eq!(
            resolve(MenuAction::StepForward, &menu_ctx, &ed),
            Err("Nothing to redo".to_string())
        );
        ed.dispatch(Action::Undo).unwrap();
        let menu_ctx = context(&mut ed, &Workspace::new());
        assert_eq!(
            resolve(MenuAction::StepForward, &menu_ctx, &ed),
            Ok(Pick::Action(Action::Redo))
        );
        assert_eq!(
            resolve(
                MenuAction::Zoom(ui::menu::ZoomCommand::Double),
                &menu_ctx,
                &ed
            ),
            Ok(Pick::Zoom(2.0))
        );
        ed.dispatch(Action::Redo).unwrap();

        // The lock rows flip one flag each, as one undo step, and tick.
        let id = ed.active().unwrap().document.active_layer().unwrap();
        let locked = |ed: &Editor| ed.active().unwrap().document.layers.get(id).unwrap().locked;
        assert!(!locked(&ed).position);
        let steps = depth(&ed);
        assert!(invoke(
            &mut ed,
            MenuAction::LockLayer(ui::menu::LayerLock::Position)
        )
        .unwrap());
        assert!(locked(&ed).position);
        assert_eq!(depth(&ed), steps + 1);
        let menu_ctx = context(&mut ed, &Workspace::new());
        assert_eq!(
            MenuAction::LockLayer(ui::menu::LayerLock::Position).checked(&menu_ctx),
            Some(true)
        );
        assert_eq!(
            resolve_intent(
                MenuAction::AlignLayers(ui::menu::AlignEdge::Left),
                &menu_ctx,
                &ed
            ),
            Err("The layer's position is locked".to_string()),
            "a position lock refuses Align, as it refuses Free Transform"
        );
        assert!(invoke(
            &mut ed,
            MenuAction::LockLayer(ui::menu::LayerLock::Position)
        )
        .unwrap());
        assert!(!locked(&ed).position, "the second click releases it");
        // Lock All, then release it: the one patch the blanket lock allows.
        assert!(invoke(&mut ed, MenuAction::LockLayer(ui::menu::LayerLock::All)).unwrap());
        assert!(locked(&ed).all);
        let menu_ctx = context(&mut ed, &Workspace::new());
        assert_eq!(
            resolve_intent(MenuAction::RenameLayer, &menu_ctx, &ed),
            Err("The layer is locked".to_string())
        );
        assert!(invoke(&mut ed, MenuAction::LockLayer(ui::menu::LayerLock::All)).unwrap());
        assert!(!locked(&ed).all);
    }

    /// W7-E: Filter > Convert for Smart Filters, then filters on the smart
    /// object, through the same routes the shell drives: the menu's
    /// resolve/perform, the Filter dialog host, the confirmed invocation the
    /// shell hands `run_filter_invocation`, and the `SetLayerKind` command
    /// the Layers panel's eye/delete rows emit.
    mod smart_filters {
        use super::*;

        const W: u32 = 48;
        const H: u32 = 32;

        fn whole() -> raster::PixelRect {
            raster::PixelRect::new(0, 0, W, H)
        }

        /// Opaque black left of x = 24, opaque white from it.
        fn edge_rgba() -> Vec<u8> {
            let mut rgba = vec![0u8; (W * H * 4) as usize];
            for y in 0..H {
                for x in 0..W {
                    let i = ((y * W + x) * 4) as usize;
                    let v = if x < 24 { 0 } else { 255 };
                    rgba[i..i + 4].copy_from_slice(&[v, v, v, 255]);
                }
            }
            rgba
        }

        /// The probe document, its layer repainted as a hard edge, then
        /// converted for smart filters through the menu.
        fn converted(dir: &std::path::Path) -> (Editor, LayerId) {
            let mut ed = opened(dir);
            let layer = ed.active().unwrap().document.active_layer().unwrap();
            let paint =
                pixels::write_layer(ed.active_mut().unwrap(), layer, &edge_rgba(), "Edge").unwrap();
            ed.apply_command(paint);
            let ctx = context(&mut ed, &Workspace::new());
            assert_eq!(
                resolve_intent(MenuAction::ConvertForSmartFilters, &ctx, &ed),
                Ok(Intent::Action(MenuAction::ConvertForSmartFilters))
            );
            perform(MenuAction::ConvertForSmartFilters, &mut ed).unwrap();
            let id = active_smart_object(&ed).expect("the active layer is a smart object now");
            // Converting twice is refused with its reason.
            let ctx = context(&mut ed, &Workspace::new());
            assert_eq!(
                resolve_intent(MenuAction::ConvertForSmartFilters, &ctx, &ed),
                Err(ui::menu::SMART_FILTERS_ALREADY.to_string())
            );
            (ed, id)
        }

        fn composite(ed: &mut Editor) -> Vec<u8> {
            ed.active_mut().unwrap().composite(whole()).unwrap()
        }

        /// Red channel of the composite at `(x, 10)`.
        fn red(rgba: &[u8], x: u32) -> u8 {
            rgba[((10 * W + x) * 4) as usize]
        }

        fn stack(ed: &Editor, id: LayerId) -> Vec<layer_model::SmartFilter> {
            smart_filters_of(ed, id).expect("still a smart object")
        }

        fn radius(f: &layer_model::SmartFilter) -> f32 {
            match f.params.get("radius") {
                Some(layer_model::SmartParam::Float(r)) => *r,
                other => panic!("no float radius: {other:?}"),
            }
        }

        /// Open Gaussian Blur's dialog the way the menu does, move its radius
        /// to `r`, and confirm it the way the shell does.
        fn blur_through_the_dialog(ed: &mut Editor, r: f32) -> (f32, String) {
            let mut host = crate::dialog_host::DialogHost::default();
            assert!(host
                .open_for_menu_action(&MenuAction::Filter(ui::menu::FilterId::GaussianBlur), ed));
            let crate::dialog_host::ActiveDialog::Filter(dialog) = host.active_for_test() else {
                panic!("not the filter dialog");
            };
            let opened_at = dialog.params().float("radius");
            assert!(dialog.set_param("radius", ui::dialogs::ParamValue::Float(r)));
            let invocation = dialog.invocation();
            let message = run_filter_invocation(ed, &invocation).unwrap();
            (opened_at, message)
        }

        fn set_kind(ed: &mut Editor, id: LayerId, filters: Vec<layer_model::SmartFilter>) {
            // Exactly the command the Layers panel's sub-rows emit.
            let mut so = match &ed.active().unwrap().document.layers.get(id).unwrap().kind {
                layer_model::LayerKind::SmartObject(so) => so.clone(),
                _ => unreachable!(),
            };
            so.filters = filters;
            ed.apply_command(Command::SetLayerKind {
                layer_id: id,
                kind: Box::new(layer_model::LayerKind::SmartObject(so)),
            });
        }

        #[test]
        fn a_gaussian_blur_on_a_smart_object_is_a_live_editable_undoable_saved_smart_filter() {
            let dir = tempfile::tempdir().unwrap();
            let (mut ed, id) = converted(dir.path());
            let source = ed.active().unwrap().document.layer_tiles(id).cloned();
            let sharp = composite(&mut ed);
            assert_eq!(red(&sharp, 23), 0);
            assert_eq!(red(&sharp, 24), 255);

            // Apply: one stack entry, the source untouched, the composite soft.
            let (_, message) = blur_through_the_dialog(&mut ed, 4.0);
            assert!(message.contains("smart filter"), "{message}");
            let filters = stack(&ed, id);
            assert_eq!(filters.len(), 1);
            assert_eq!(filters[0].filter, "GaussianBlur");
            assert_eq!(radius(&filters[0]), 4.0);
            assert_eq!(
                ed.active().unwrap().document.layer_tiles(id).cloned(),
                source,
                "the smart object's source pixels were rewritten"
            );
            let soft4 = composite(&mut ed);
            assert!(
                red(&soft4, 23) > 20 && red(&soft4, 23) < 235,
                "{}",
                red(&soft4, 23)
            );

            // The eye off restores the unblurred composite; on again, soft.
            let mut off = filters.clone();
            off[0].enabled = false;
            set_kind(&mut ed, id, off);
            assert_eq!(composite(&mut ed), sharp);
            set_kind(&mut ed, id, filters.clone());
            assert_eq!(composite(&mut ed), soft4);

            // Re-edit: the dialog re-opens at the stored radius, and the
            // confirm replaces the entry rather than adding one.
            compositor::smart::request_edit(compositor::smart::EditRequest {
                layer: id,
                index: 0,
            });
            let (opened_at, message) = blur_through_the_dialog(&mut ed, 10.0);
            assert_eq!(opened_at, 4.0, "the dialog opened at the stored radius");
            assert!(message.contains("updated"), "{message}");
            let edited = stack(&ed, id);
            assert_eq!(edited.len(), 1);
            assert_eq!(radius(&edited[0]), 10.0);
            let soft10 = composite(&mut ed);
            assert_ne!(soft10, soft4, "a larger radius changes the composite");
            assert!(red(&soft10, 17) > red(&soft4, 17));

            // Each change is one undo step.
            let doc = ed.active_mut().unwrap();
            assert!(doc.undo().unwrap());
            assert_eq!(stack(&ed, id), filters);
            assert_eq!(composite(&mut ed), soft4);
            assert!(ed.active_mut().unwrap().redo().unwrap());
            assert_eq!(composite(&mut ed), soft10);

            // Delete, then undo the delete.
            set_kind(&mut ed, id, Vec::new());
            assert_eq!(composite(&mut ed), sharp);
            assert!(ed.active_mut().unwrap().undo().unwrap());
            assert_eq!(composite(&mut ed), soft10);

            // Save and reopen: the stack and its look come back.
            let path = dir.path().join("smart.rstudio");
            ed.active_mut().unwrap().save_to(&path, "test").unwrap();
            let mut again = editor(dir.path());
            again.open_path(&path).unwrap();
            let reopened = again
                .active()
                .unwrap()
                .document
                .layers
                .iter_depth_first()
                .into_iter()
                .find(|l| {
                    matches!(
                        again
                            .active()
                            .unwrap()
                            .document
                            .layers
                            .get(*l)
                            .map(|l| &l.kind),
                        Some(layer_model::LayerKind::SmartObject(_))
                    )
                })
                .expect("the smart object survived the round trip");
            assert_eq!(stack(&again, reopened), stack(&ed, id));
            assert_eq!(composite(&mut again), soft10);
        }

        /// The reviewer's route: the smart object is NOT the active layer, and
        /// its filter's name is double-clicked in the real Layers panel. The
        /// chrome routes the panel's Filter intent in the same frame as its
        /// selection, against the pre-selection editor; the dialog must still
        /// open at the stored parameters over the object's pixels, and the
        /// confirm must replace that entry, not append a second one.
        #[test]
        fn double_clicking_a_non_active_smart_objects_filter_reopens_it_at_its_params() {
            let dir = tempfile::tempdir().unwrap();
            let (mut ed, id) = converted(dir.path());
            let (_, message) = blur_through_the_dialog(&mut ed, 7.0);
            assert!(message.contains("added"), "{message}");
            // A new, empty layer on top becomes the active one.
            ed.dispatch(Action::NewLayer).unwrap();
            let other = ed.active().unwrap().document.active_layer().unwrap();
            assert_ne!(other, id, "setup: the smart object is not active");
            let soft7 = composite(&mut ed);

            let ctx = egui::Context::default();
            crate::chrome::install_theme(&ctx, design::Theme::Dark);
            let mut chrome = crate::chrome::Chrome::new();
            let input = |time: f64, events: Vec<egui::Event>| egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1400.0, 900.0),
                )),
                time: Some(time),
                events,
                ..Default::default()
            };
            // What `Shell::apply_chrome` does first with a frame's output:
            // the selection lands, then a confirmed dialog runs.
            let apply = |ed: &mut Editor, out: crate::chrome::ChromeOutput| -> Option<String> {
                if let Some((layers, active)) = out.select_layers {
                    ed.set_layer_selection(layers, active);
                }
                match out.dialog {
                    Some(ui::dialogs::DialogAction::RunFilter(invocation)) => {
                        Some(run_filter_invocation(ed, &invocation).unwrap())
                    }
                    _ => None,
                }
            };
            for frame in 0..3 {
                let mut out = crate::chrome::ChromeOutput::default();
                let _ = ctx.run(input(1.0 + frame as f64 * 0.05, Vec::new()), |ctx| {
                    out = chrome.ui(ctx, &mut ed);
                });
                assert_eq!(apply(&mut ed, out), None);
            }
            // The same id `ui::view::docks::smart_filter_part_id` gives the
            // name label of filter 0 of `id`.
            let name_id = egui::Id::new(("raster-smart-filter", id, 0usize, "name"));
            let name = ctx
                .read_response(name_id)
                .expect("the Layers panel drew the smart filter's row")
                .rect
                .center();
            let press = |pressed: bool| egui::Event::PointerButton {
                pos: name,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            for (frame, events) in [
                vec![egui::Event::PointerMoved(name)],
                vec![press(true)],
                vec![press(false)],
                vec![press(true)],
                vec![press(false)],
                Vec::new(),
            ]
            .into_iter()
            .enumerate()
            {
                let mut out = crate::chrome::ChromeOutput::default();
                let _ = ctx.run(input(2.0 + frame as f64 * 0.05, events), |ctx| {
                    out = chrome.ui(ctx, &mut ed);
                });
                assert_eq!(apply(&mut ed, out), None);
            }
            assert_eq!(
                ed.active().unwrap().document.active_layer(),
                Some(id),
                "the double-click selected the smart object"
            );
            let crate::dialog_host::ActiveDialog::Filter(dialog) =
                chrome.dialogs_for_test().active_for_test()
            else {
                panic!("the double-click did not open the filter dialog");
            };
            assert_eq!(
                dialog.params().float("radius"),
                7.0,
                "the dialog opened at the stored radius"
            );
            // It previews over the object's source, not the empty layer that
            // was active when the dialog was routed.
            assert!(
                dialog.preview_buffer().get(2, 10)[3] > 0.9,
                "the dialog previews the previously active (empty) layer"
            );
            assert!(dialog.set_param("radius", ui::dialogs::ParamValue::Float(3.0)));

            // Enter confirms; the shell runs the confirmed invocation.
            let enter = egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            };
            let mut out = crate::chrome::ChromeOutput::default();
            let _ = ctx.run(input(3.0, vec![enter]), |ctx| {
                out = chrome.ui(ctx, &mut ed);
            });
            let message = apply(&mut ed, out).expect("Enter did not confirm the filter dialog");
            assert!(message.contains("updated"), "{message}");
            let edited = stack(&ed, id);
            assert_eq!(edited.len(), 1, "the confirm appended instead of replacing");
            assert_eq!(radius(&edited[0]), 3.0);
            assert_ne!(composite(&mut ed), soft7, "the new radius changed nothing");
            // One undo step brings radius 7 back.
            assert!(ed.active_mut().unwrap().undo().unwrap());
            assert_eq!(radius(&stack(&ed, id)[0]), 7.0);
            assert_eq!(composite(&mut ed), soft7);
        }

        /// Open Gaussian Blur's dialog armed to re-edit entry 0 of `id` (the
        /// panel's double-click), as the host does.
        fn arm_reedit(ed: &Editor, id: LayerId) -> crate::dialog_host::DialogHost {
            compositor::smart::request_edit(compositor::smart::EditRequest {
                layer: id,
                index: 0,
            });
            let mut host = crate::dialog_host::DialogHost::default();
            assert!(host
                .open_for_menu_action(&MenuAction::Filter(ui::menu::FilterId::GaussianBlur), ed));
            assert_eq!(
                armed_smart_filter_layer(ed, ui::menu::FilterId::GaussianBlur),
                Some(id),
                "setup: the re-edit is armed"
            );
            host
        }

        /// The Filter Gallery's confirm for Gaussian Blur: its defaults.
        fn gallery_blur(ed: &mut Editor, host: &mut crate::dialog_host::DialogHost) -> String {
            assert!(host.open_for_menu_action(&MenuAction::FilterGallery, ed));
            assert!(matches!(
                host.active_for_test(),
                crate::dialog_host::ActiveDialog::FilterGallery(_)
            ));
            let spec = ui::dialogs::filter_by_id(ui::menu::FilterId::GaussianBlur).unwrap();
            let invocation = ui::dialogs::FilterInvocation {
                filter: spec,
                params: ui::dialogs::FilterParams::defaults(spec.params),
            };
            run_filter_invocation(ed, &invocation).unwrap()
        }

        /// Round-3 review route: a re-edit dialog is cancelled (Escape through
        /// the real host), a raster layer is selected, and the Filter Gallery
        /// runs Gaussian Blur. The raster layer is the one filtered; the smart
        /// object's stored entry keeps its radius.
        #[test]
        fn a_cancelled_reedit_never_captures_a_later_gallery_run_on_another_layer() {
            let dir = tempfile::tempdir().unwrap();
            let (mut ed, id) = converted(dir.path());
            blur_through_the_dialog(&mut ed, 7.0);
            let mut host = arm_reedit(&ed, id);

            // Escape cancels the dialog through the host's generic route.
            let ctx = egui::Context::default();
            crate::chrome::install_theme(&ctx, design::Theme::Dark);
            let escape = egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            };
            let mut out = crate::chrome::ChromeOutput::default();
            let _ = ctx.run(
                egui::RawInput {
                    events: vec![escape],
                    ..Default::default()
                },
                |ctx| host.ui(ctx, None, &mut out),
            );
            assert!(out.dialog.is_none(), "Escape confirmed the dialog");
            assert!(!host.is_open(), "Escape did not close the dialog");
            assert_eq!(
                armed_smart_filter_layer(&ed, ui::menu::FilterId::GaussianBlur),
                None,
                "cancelling left the re-edit armed"
            );

            // A painted raster layer on top becomes active; the Gallery
            // blurs it.
            ed.dispatch(Action::NewLayer).unwrap();
            let raster = ed.active().unwrap().document.active_layer().unwrap();
            assert_ne!(raster, id);
            let paint = pixels::write_layer(ed.active_mut().unwrap(), raster, &edge_rgba(), "Edge")
                .unwrap();
            ed.apply_command(paint);
            let message = gallery_blur(&mut ed, &mut host);
            assert_eq!(message, "Gaussian Blur applied", "{message}");
            assert_ne!(
                pixels::read_layer(ed.active().unwrap(), raster),
                edge_rgba(),
                "the raster layer was not filtered"
            );
            let kept = stack(&ed, id);
            assert_eq!(kept.len(), 1);
            assert_eq!(radius(&kept[0]), 7.0, "the stored filter was overwritten");
        }

        /// Even with no cancel in between, the Gallery never finishes an armed
        /// re-edit: over the same smart object it appends a new entry.
        #[test]
        fn the_filter_gallery_over_an_armed_smart_object_appends_not_replaces() {
            let dir = tempfile::tempdir().unwrap();
            let (mut ed, id) = converted(dir.path());
            blur_through_the_dialog(&mut ed, 7.0);
            let mut host = arm_reedit(&ed, id);
            let message = gallery_blur(&mut ed, &mut host);
            assert!(message.contains("added"), "{message}");
            let filters = stack(&ed, id);
            assert_eq!(filters.len(), 2, "the Gallery replaced the armed entry");
            assert_eq!(radius(&filters[0]), 7.0);
        }

        /// A filter run while a re-edit is armed but ANOTHER layer is active
        /// filters that layer: the arm never redirects a run to its object.
        #[test]
        fn an_armed_reedit_never_redirects_a_run_to_an_inactive_smart_object() {
            let dir = tempfile::tempdir().unwrap();
            let (mut ed, id) = converted(dir.path());
            blur_through_the_dialog(&mut ed, 7.0);
            let _host = arm_reedit(&ed, id);
            ed.dispatch(Action::NewLayer).unwrap();
            let raster = ed.active().unwrap().document.active_layer().unwrap();
            let paint = pixels::write_layer(ed.active_mut().unwrap(), raster, &edge_rgba(), "Edge")
                .unwrap();
            ed.apply_command(paint);
            let spec = ui::dialogs::filter_by_id(ui::menu::FilterId::GaussianBlur).unwrap();
            let invocation = ui::dialogs::FilterInvocation {
                filter: spec,
                params: ui::dialogs::FilterParams::defaults(spec.params),
            };
            let message = run_filter_invocation(&mut ed, &invocation).unwrap();
            assert_eq!(message, "Gaussian Blur applied", "{message}");
            assert_eq!(radius(&stack(&ed, id)[0]), 7.0);
        }

        /// `DialogHost::close` ends an armed re-edit too.
        #[test]
        fn closing_the_host_disarms_a_smart_filter_reedit() {
            let dir = tempfile::tempdir().unwrap();
            let (mut ed, id) = converted(dir.path());
            blur_through_the_dialog(&mut ed, 7.0);
            let mut host = arm_reedit(&ed, id);
            host.close();
            assert_eq!(
                armed_smart_filter_layer(&ed, ui::menu::FilterId::GaussianBlur),
                None
            );
        }

        #[test]
        fn the_filter_menu_on_a_smart_object_appends_and_never_paints() {
            let dir = tempfile::tempdir().unwrap();
            let (mut ed, id) = converted(dir.path());
            let source = ed.active().unwrap().document.layer_tiles(id).cloned();
            // A stale re-edit note must not turn a plain menu run into an edit.
            compositor::smart::request_edit(compositor::smart::EditRequest {
                layer: id,
                index: 0,
            });
            assert!(invoke(
                &mut ed,
                MenuAction::Filter(ui::menu::FilterId::GaussianBlur)
            )
            .unwrap());
            assert!(invoke(&mut ed, MenuAction::Filter(ui::menu::FilterId::Median)).unwrap());
            let keys: Vec<String> = stack(&ed, id).into_iter().map(|f| f.filter).collect();
            assert_eq!(keys, ["GaussianBlur", "Median"]);
            assert_eq!(
                ed.active().unwrap().document.layer_tiles(id).cloned(),
                source
            );
        }

        #[test]
        fn a_psd_export_writes_the_filtered_look() {
            let dir = tempfile::tempdir().unwrap();
            let (mut ed, _) = converted(dir.path());
            blur_through_the_dialog(&mut ed, 4.0);
            let soft = composite(&mut ed);
            let path = dir.path().join("smart.psd");
            ed.active_mut().unwrap().export_psd_to(&path).unwrap();
            let mut back = editor(dir.path());
            back.open_path(&path).unwrap();
            let flat = composite(&mut back);
            for x in [20, 22, 23, 24, 25, 27] {
                let (a, b) = (i32::from(red(&soft, x)), i32::from(red(&flat, x)));
                assert!((a - b).abs() <= 2, "x = {x}: {a} vs {b}");
            }
            assert!(red(&flat, 23) > 20, "the PSD holds the blurred pixels");
        }

        /// W10-C: each new filter, opened from its Filter-menu row through the
        /// real dialog host, confirms as exactly one undo step on a pixel
        /// layer, and over a smart object joins its smart-filter stack under
        /// its `FilterId` key, re-renders the composite, and undoes in one.
        #[test]
        fn w10c_filters_open_from_the_menu_and_land_as_one_step_or_a_smart_filter() {
            use ui::dialogs::ParamValue as P;
            use ui::menu::FilterId as F;
            let cases: [(F, &str, P); 4] = [
                (F::CameraRaw, "exposure", P::Float(-1.0)),
                (F::LensCorrection, "vignette_amount", P::Float(-100.0)),
                (F::LightingEffects, "intensity", P::Float(80.0)),
                (F::HsbHsl, "output", P::Choice(2)),
            ];
            let confirm = |ed: &mut Editor, id: F, key: &str, value: P| -> String {
                let mut host = crate::dialog_host::DialogHost::default();
                assert!(
                    host.open_for_menu_action(&MenuAction::Filter(id), ed),
                    "{id:?} opened no dialog"
                );
                let crate::dialog_host::ActiveDialog::Filter(dialog) = host.active_for_test()
                else {
                    panic!("{id:?} did not open the filter dialog");
                };
                assert_eq!(dialog.spec().id, id);
                assert!(dialog.set_param(key, value), "{id:?}/{key}");
                let invocation = dialog.invocation();
                run_filter_invocation(ed, &invocation).unwrap()
            };
            for (id, key, value) in cases {
                // A pixel layer: one undo step that changes the pixels.
                let dir = tempfile::tempdir().unwrap();
                let mut ed = opened(dir.path());
                let layer = ed.active().unwrap().document.active_layer().unwrap();
                let paint =
                    pixels::write_layer(ed.active_mut().unwrap(), layer, &edge_rgba(), "Edge")
                        .unwrap();
                ed.apply_command(paint);
                let before = composite(&mut ed);
                let depth = ed.active().unwrap().history.undo_depth();
                confirm(&mut ed, id, key, value);
                assert_ne!(composite(&mut ed), before, "{id:?} changed nothing");
                assert_eq!(
                    ed.active().unwrap().history.undo_depth(),
                    depth + 1,
                    "{id:?} was not exactly one undo step"
                );
                assert!(ed.active_mut().unwrap().undo().unwrap());
                assert_eq!(composite(&mut ed), before, "{id:?}: undo did not restore");

                // A smart object: a smart filter, not a pixel rewrite.
                let dir = tempfile::tempdir().unwrap();
                let (mut ed, so) = converted(dir.path());
                let source = ed.active().unwrap().document.layer_tiles(so).cloned();
                let before = composite(&mut ed);
                confirm(&mut ed, id, key, value);
                let filters = stack(&ed, so);
                assert_eq!(filters.len(), 1, "{id:?} did not join the stack");
                assert_eq!(filters[0].filter, format!("{id:?}"));
                assert_ne!(
                    composite(&mut ed),
                    before,
                    "{id:?}: the smart filter renders nothing"
                );
                assert_eq!(
                    ed.active().unwrap().document.layer_tiles(so).cloned(),
                    source,
                    "{id:?} rewrote the smart object's source"
                );
                assert!(ed.active_mut().unwrap().undo().unwrap());
                assert!(stack(&ed, so).is_empty(), "{id:?}: undo left the filter");
                assert_eq!(composite(&mut ed), before);
            }
        }
    }
}

#[cfg(test)]
mod w9k_text_menu_tests {
    use super::*;
    use crate::dialogs::ScriptedDialogs;
    use crate::prefs::AppPaths;
    use crate::recent::RecentFiles;
    use layer_model::{Layer, LayerKind, TextLayer};

    fn editor_with(dir: &std::path::Path, dialogs: ScriptedDialogs) -> Editor {
        Editor::with_state(
            AppPaths::rooted(dir.join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(dialogs),
        )
    }

    /// A document holding one DejaVu Sans text layer at `at`, active.
    fn with_text(
        dir: &std::path::Path,
        text: &str,
        size: f32,
        at: (f32, f32),
    ) -> (Editor, LayerId) {
        let bytes = dejavu::sans::regular().to_vec();
        compositor::load_font(bytes.clone());
        text_engine::register_session_font(bytes);
        let mut ed = editor_with(dir, ScriptedDialogs::new());
        ed.dispatch(Action::NewDocument).expect("a document");
        let mut layer = Layer::with_kind(
            "Type",
            LayerKind::Text(TextLayer::legacy(text, "DejaVu Sans", size)),
        );
        layer.transform = glam::Affine2::from_translation(glam::vec2(at.0, at.1));
        let id = layer.id;
        ed.apply_command(Command::create_layer(layer));
        ed.set_active_layer(id);
        (ed, id)
    }

    /// The composite as RGBA8.
    fn pixels(ed: &Editor) -> (u32, u32, Vec<u8>) {
        let open = ed.active().unwrap();
        let rect = open.canvas_rect();
        let canvas = compositor::composite_region(
            &open.document,
            &open.tiles,
            rect,
            0,
            compositor::CompositeOptions::default(),
        )
        .unwrap();
        (
            open.document.width(),
            open.document.height(),
            canvas.to_rgba8(&open.document.meta.color_space),
        )
    }

    /// Ink blobs (dark columns) left to right, as centroids.
    fn dark_blobs(w: u32, h: u32, rgba: &[u8]) -> Vec<(f32, f32)> {
        let mut blobs = Vec::new();
        let mut acc: Option<(f64, f64, f64)> = None;
        for x in 0..w {
            let mut col = (0.0, 0.0, 0.0);
            for y in 0..h {
                let i = ((y * w + x) * 4) as usize;
                // Dark AND opaque: black text over a transparent or a white
                // background alike.
                let a = f64::from(rgba[i + 3]);
                let ink = a - f64::from(rgba[i]) * a / 255.0;
                if ink > 32.0 {
                    col.0 += ink * f64::from(x);
                    col.1 += ink * f64::from(y);
                    col.2 += ink;
                }
            }
            if col.2 > 0.0 {
                let a = acc.get_or_insert((0.0, 0.0, 0.0));
                a.0 += col.0;
                a.1 += col.1;
                a.2 += col.2;
            } else if let Some(a) = acc.take() {
                blobs.push(((a.0 / a.2) as f32, (a.1 / a.2) as f32));
            }
        }
        if let Some(a) = acc {
            blobs.push(((a.0 / a.2) as f32, (a.1 / a.2) as f32));
        }
        blobs
    }

    fn warp_of(ed: &Editor, id: LayerId) -> layer_model::text::TextWarp {
        match &ed.active().unwrap().document.layers.get(id).unwrap().kind {
            LayerKind::Text(t) => t.warp,
            other => panic!("not text: {other:?}"),
        }
    }

    /// W9-K: Layer > Text > Warp Style > Arc is a live menu row over a text
    /// layer (and greyed with a reason over a raster one); performing it
    /// stores the warp on the layer as one undo step, and the composited
    /// canvas shows the bars bent onto an arc. The canvas is composited
    /// BEFORE the warp (a text layer is always on screen before the user
    /// warps it), so a cache that keyed the text without its warp would
    /// serve the flat bars - round-2 review defect 1.
    #[test]
    fn warp_text_arc_from_the_layer_menu_bends_the_composited_text() {
        let dir = tempfile::tempdir().unwrap();
        let (mut ed, id) = with_text(dir.path(), "I   I   I   I   I", 48.0, (40.0, 200.0));
        let arc = MenuAction::WarpText(ui::menu::WarpTextItem::Style(
            layer_model::text::WarpStyle::Arc,
        ));
        assert!(ui::menu::menu_bar(0)
            .iter()
            .any(|m| m.title == "Layer"
                && format!("{:?}", m.entries).contains("WarpText(Style(Arc))")));
        let ctx = context(&mut ed, &Workspace::new());
        assert!(resolve_intent(arc, &ctx, &ed).is_ok(), "live over text");

        let flat = |ed: &Editor| {
            let (w, h, rgba) = pixels(ed);
            let blobs = dark_blobs(w, h, &rgba);
            assert_eq!(blobs.len(), 5, "five bars: {blobs:?}");
            blobs.iter().all(|b| (b.1 - blobs[0].1).abs() < 2.0)
        };
        assert!(flat(&ed), "the bars start on one baseline");

        let status = perform(arc, &mut ed).expect("warped");
        assert!(status.contains("Arc"), "{status}");
        let doc = &ed.active().unwrap().document;
        let LayerKind::Text(text) = &doc.layers.get(id).unwrap().kind else {
            panic!("still a text layer");
        };
        assert_eq!(text.warp.style, layer_model::text::WarpStyle::Arc);
        assert_eq!(text.text, "I   I   I   I   I", "the text stays editable");

        let (w, h, rgba) = pixels(&ed);
        let blobs = dark_blobs(w, h, &rgba);
        assert_eq!(blobs.len(), 5, "five bars: {blobs:?}");
        assert!(
            blobs[0].1 - blobs[2].1 > 12.0 && blobs[4].1 - blobs[2].1 > 12.0,
            "the middle bar rides above the ends on the canvas: {blobs:?}"
        );

        // One Undo takes the warp off, and the canvas follows it back.
        ed.dispatch(Action::Undo).unwrap();
        assert!(!warp_of(&ed, id).is_active(), "one undo takes the warp off");
        assert!(flat(&ed), "the undone warp leaves the canvas flat again");

        // Over a raster layer the row is greyed with a reason.
        let raster = Layer::raster("Pixels");
        let rid = raster.id;
        ed.apply_command(Command::create_layer(raster));
        ed.set_active_layer(rid);
        let ctx = context(&mut ed, &Workspace::new());
        assert_eq!(
            resolve_intent(arc, &ctx, &ed).unwrap_err(),
            "The active layer is not a text layer"
        );
    }

    /// W9-K: Layer > Text > Warp Text... opens the Warp Text dialog over the
    /// active text layer (round-2 defect 2: the bend and both distortions
    /// are fields, not fixed menu steps); its confirmation stores the style
    /// and all three numbers as one undo step, and the composited canvas -
    /// already drawn once before the warp - moves.
    #[test]
    fn warp_text_dialog_sets_style_bend_and_distortions_as_one_step() {
        let dir = tempfile::tempdir().unwrap();
        let (mut ed, id) = with_text(dir.path(), "I   I   I   I   I", 48.0, (40.0, 200.0));
        let dialog_row = MenuAction::WarpText(ui::menu::WarpTextItem::Dialog);
        assert_eq!(dialog_row.label(), "Warp Text…");
        assert!(
            ui::menu::menu_bar(0)
                .iter()
                .any(|m| m.title == "Layer"
                    && format!("{:?}", m.entries).contains("WarpText(Dialog)"))
        );
        let (_, _, before) = pixels(&ed);
        let steps = ed.active().unwrap().history_depth();

        let mut chrome = crate::chrome::Chrome::new();
        let menu_ctx = context(&mut ed, chrome.workspace());
        let intent = resolve_intent(dialog_row, &menu_ctx, &ed).expect("live over text");
        let mut out = crate::chrome::ChromeOutput::default();
        chrome.menu_click(intent, &ed, &mut out);
        assert!(chrome.dialog_open(), "Warp Text... opens its dialog");
        assert!(out.is_empty());
        {
            let dialog = chrome.dialogs_for_test().active_warp_text_dialog_for_test();
            dialog.set_style(layer_model::text::WarpStyle::Flag);
            dialog.set_amounts(80, -40, 30);
        }
        // A settle frame, then Enter, through the host's own `ui`.
        let out = {
            let ctx = egui::Context::default();
            design::apply_theme(&ctx, design::Theme::Dark);
            let mut out = crate::chrome::ChromeOutput::default();
            let host = chrome.dialogs_for_test();
            let input = |events: Vec<egui::Event>| egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1400.0, 900.0),
                )),
                events,
                ..Default::default()
            };
            let _ = ctx.run(input(Vec::new()), |ctx| host.ui(ctx, None, &mut out));
            let enter = egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
            };
            let _ = ctx.run(input(vec![enter]), |ctx| host.ui(ctx, None, &mut out));
            out
        };
        assert!(!chrome.dialog_open());
        assert_eq!(out.commands.len(), 1, "{out:?}");
        assert!(out.menu.is_empty() && out.actions.is_empty(), "{out:?}");
        for command in out.commands {
            ed.apply_command(command);
        }

        let warp = warp_of(&ed, id);
        assert_eq!(warp.style, layer_model::text::WarpStyle::Flag);
        assert!((warp.bend - 0.8).abs() < 1e-6, "{warp:?}");
        assert!((warp.horizontal + 0.4).abs() < 1e-6, "{warp:?}");
        assert!((warp.vertical - 0.3).abs() < 1e-6, "{warp:?}");
        assert_eq!(
            ed.active().unwrap().history_depth(),
            steps + 1,
            "one undo step"
        );
        let (_, _, after) = pixels(&ed);
        assert_ne!(before, after, "the confirmed warp reaches the canvas");

        ed.dispatch(Action::Undo).unwrap();
        assert!(!warp_of(&ed, id).is_active());
        let (_, _, undone) = pixels(&ed);
        assert_eq!(before, undone, "undo restores the unwarped canvas");

        // With no text layer the row is refused with the reason.
        let raster = Layer::raster("Pixels");
        let rid = raster.id;
        ed.apply_command(Command::create_layer(raster));
        ed.set_active_layer(rid);
        assert_eq!(
            perform(dialog_row, &mut ed).unwrap_err(),
            "The active layer is not a text layer"
        );
    }

    /// W9-K: Layer > Text > Convert to Shape replaces the text layer with a
    /// shape layer in its place whose composited coverage matches the text's.
    #[test]
    fn convert_to_shape_makes_a_shape_layer_whose_coverage_matches_the_text() {
        let dir = tempfile::tempdir().unwrap();
        let (mut ed, id) = with_text(dir.path(), "Convert me", 72.0, (30.0, 60.0));
        let (w, h, before) = pixels(&ed);
        let index = ed
            .active()
            .unwrap()
            .document
            .layers
            .index_in_parent(id)
            .unwrap();

        perform(MenuAction::ConvertTextToShape, &mut ed).expect("converted");
        let doc = &ed.active().unwrap().document;
        assert!(!doc.layers.contains(id), "the text layer is replaced");
        let shape_id = doc
            .layers
            .iter_depth_first()
            .into_iter()
            .find(|l| matches!(doc.layers.get(*l).unwrap().kind, LayerKind::Shape(_)))
            .expect("a shape layer");
        let shape = doc.layers.get(shape_id).unwrap();
        assert_eq!(doc.layers.index_in_parent(shape_id), Some(index));
        assert_eq!(
            shape.transform.translation,
            glam::vec2(30.0, 60.0),
            "the shape keeps the text's placement"
        );

        let (_, _, after) = pixels(&ed);
        // Dark AND opaque, as in `dark_blobs`.
        let ink = |p: &[u8], i: usize| {
            let a = i32::from(p[i * 4 + 3]);
            a - i32::from(p[i * 4]) * a / 255
        };
        let (mut inked, mut differ) = (0usize, 0usize);
        for i in 0..(w * h) as usize {
            let (a, b) = (ink(&before, i), ink(&after, i));
            if a > 128 || b > 128 {
                inked += 1;
                if (a > 128) != (b > 128) {
                    differ += 1;
                }
            }
        }
        assert!(inked > 1000, "the text drew ink: {inked}");
        assert!(
            (differ as f64) < inked as f64 * 0.03,
            "the shape covers what the text covered: {differ} of {inked} ink pixels differ"
        );

        ed.dispatch(Action::Undo).unwrap();
        let doc = &ed.active().unwrap().document;
        assert!(doc.layers.contains(id) && !doc.layers.contains(shape_id));
    }

    /// W9-K: Type on a Path through the shell's real pointer route: with the
    /// Type tool current, a click on a shape layer's circle outline creates a
    /// text layer whose baseline is that circle (the shell hands the tool the
    /// document's shape paths), and typing then draws glyphs on the circle.
    #[test]
    fn a_type_click_on_a_circle_shape_through_the_shell_makes_path_text() {
        use ui::canvas::{PointerInput, PointerPhase};
        let dir = tempfile::tempdir().unwrap();
        compositor::load_font(dejavu::sans::regular().to_vec());
        let png = dir.path().join("canvas.png");
        let (w, h) = (300u32, 300u32);
        std::fs::write(
            &png,
            raster::encode(
                raster::ExportFormat::Png,
                w,
                h,
                &vec![255u8; (w * h * 4) as usize],
            )
            .unwrap(),
        )
        .unwrap();
        let mut ed = editor_with(dir.path(), ScriptedDialogs::new());
        ed.open_path(&png).unwrap();
        let viewport = glam::vec2(400.0, 400.0);
        {
            let doc = ed.active_mut().unwrap();
            doc.set_viewport(viewport);
            doc.camera.zoom = 1.0;
            doc.camera.center = glam::vec2(w as f32 / 2.0, h as f32 / 2.0);
        }
        let circle = vector::shapes::circle(vector::point(150.0, 150.0), 80.0);
        ed.apply_command(Command::create_layer(Layer::with_kind(
            "Circle",
            LayerKind::Shape(layer_model::ShapeLayer::from_svg(vector::to_svg(&circle))),
        )));
        ed.set_tool(ToolId::Type);
        let screen = |x: f32, y: f32| viewport * 0.5 + glam::vec2(x - 150.0, y - 150.0);
        let mut pointer = crate::ToolPointer::new();
        // The top of the circle.
        for phase in [PointerPhase::Down, PointerPhase::Up] {
            pointer.handle(
                &mut ed,
                PointerInput::at(phase, screen(150.0, 70.0)),
                false,
                &[],
            );
        }
        let doc = &ed.active().unwrap().document;
        let text = doc
            .layers
            .iter_depth_first()
            .into_iter()
            .find_map(|id| match &doc.layers.get(id)?.kind {
                LayerKind::Text(t) => Some(t.clone()),
                _ => None,
            })
            .expect("the click made a text layer");
        let path = text.path.expect("the click on the outline made path text");
        assert!(path.closed);
        for p in &path.points {
            let (x, y) = (p[0] + 150.0, p[1] + 70.0);
            let r = ((x - 150.0).powi(2) + (y - 150.0).powi(2)).sqrt();
            assert!((r - 80.0).abs() < 0.5, "the baseline is the circle: {r}");
        }
    }

    /// W9-K (round 2, defect 3): Type on a Path follows the outline the
    /// user SEES. A moved shape layer's outline is hit where it is drawn (its
    /// transform applies) and the baseline is that drawn outline; and the
    /// Paths panel's Work Path is offered too, through the real pointer route.
    #[test]
    fn type_on_a_path_follows_a_moved_shape_and_the_work_path() {
        use ui::canvas::{PointerInput, PointerPhase};
        compositor::load_font(dejavu::sans::regular().to_vec());
        let (w, h) = (300u32, 300u32);
        let viewport = glam::vec2(400.0, 400.0);
        let open = |dir: &std::path::Path| {
            let png = dir.join("canvas.png");
            std::fs::write(
                &png,
                raster::encode(
                    raster::ExportFormat::Png,
                    w,
                    h,
                    &vec![255u8; (w * h * 4) as usize],
                )
                .unwrap(),
            )
            .unwrap();
            let mut ed = editor_with(dir, ScriptedDialogs::new());
            ed.open_path(&png).unwrap();
            let doc = ed.active_mut().unwrap();
            doc.set_viewport(viewport);
            doc.camera.zoom = 1.0;
            doc.camera.center = glam::vec2(w as f32 / 2.0, h as f32 / 2.0);
            ed.set_tool(ToolId::Type);
            ed
        };
        let screen = |x: f32, y: f32| viewport * 0.5 + glam::vec2(x - 150.0, y - 150.0);
        let click = |ed: &mut Editor, x: f32, y: f32| {
            let mut pointer = crate::ToolPointer::new();
            for phase in [PointerPhase::Down, PointerPhase::Up] {
                pointer.handle(ed, PointerInput::at(phase, screen(x, y)), false, &[]);
            }
        };
        let path_text = |ed: &Editor| {
            let doc = &ed.active().unwrap().document;
            doc.layers
                .iter_depth_first()
                .into_iter()
                .find_map(|id| {
                    let layer = doc.layers.get(id)?;
                    match &layer.kind {
                        LayerKind::Text(t) => Some((layer.transform, t.clone())),
                        _ => None,
                    }
                })
                .expect("the click made a text layer")
        };

        // A circle shape (centre 150,150, r 60) moved down by 40: the user
        // sees it centred at (150,190). Its drawn top is (150,130), 40 px
        // from where the stored outline's top is.
        let dir = tempfile::tempdir().unwrap();
        let mut ed = open(dir.path());
        let circle = vector::shapes::circle(vector::point(150.0, 150.0), 60.0);
        let mut shape = Layer::with_kind(
            "Circle",
            LayerKind::Shape(layer_model::ShapeLayer::from_svg(vector::to_svg(&circle))),
        );
        shape.transform = glam::Affine2::from_translation(glam::vec2(0.0, 40.0));
        ed.apply_command(Command::create_layer(shape));
        click(&mut ed, 150.0, 130.0);
        let (pose, text) = path_text(&ed);
        let path = text
            .path
            .expect("the click on the drawn outline made path text");
        for p in &path.points {
            let q = pose.transform_point2(glam::vec2(p[0], p[1]));
            let r = ((q.x - 150.0).powi(2) + (q.y - 190.0).powi(2)).sqrt();
            assert!(
                (r - 60.0).abs() < 0.5,
                "the baseline is the drawn circle: {q}"
            );
        }

        // The Work Path (a circle, centre 150,150, r 50), no shape layer at
        // all: the menu context parks it the way every frame does, and a
        // Type click on it makes path text along it.
        let dir = tempfile::tempdir().unwrap();
        let mut ed = open(dir.path());
        let mut ws = Workspace::new();
        ws.paths.work_path = Some(vector::shapes::circle(vector::point(150.0, 150.0), 50.0));
        let _ = context(&mut ed, &ws);
        click(&mut ed, 150.0, 100.0);
        let (pose, text) = path_text(&ed);
        let path = text
            .path
            .expect("the click on the Work Path made path text");
        assert!(path.closed);
        for p in &path.points {
            let q = pose.transform_point2(glam::vec2(p[0], p[1]));
            let r = ((q.x - 150.0).powi(2) + (q.y - 150.0).powi(2)).sqrt();
            assert!((r - 50.0).abs() < 0.5, "the baseline is the Work Path: {q}");
        }
    }

    /// W9-K: File > Open of a .ttf loads the font for the session: no
    /// document opens, the compositor can shape the family, and the Type
    /// tool's Font list offers it. A file that is not a font is refused.
    #[test]
    fn file_open_of_a_font_file_makes_its_family_available() {
        let dir = tempfile::tempdir().unwrap();
        let font = dir.path().join("Extra.ttf");
        std::fs::write(&font, dejavu::serif_condensed::regular()).unwrap();
        let mut probe = text_engine::FontLibrary::empty();
        probe.load_bytes(dejavu::serif_condensed::regular().to_vec());
        let family = probe.family_names()[0].clone();
        assert!(
            crate::dialogs::open_file_filters()
                .iter()
                .any(|(label, ext)| *label == "Fonts" && ext.contains(&"ttf")),
            "File > Open offers font files"
        );

        let junk = dir.path().join("junk.otf");
        std::fs::write(&junk, b"not a font").unwrap();
        let mut ed = editor_with(dir.path(), ScriptedDialogs::new().opening(&junk));
        assert!(ed.dispatch(Action::Open).is_err(), "not a font");

        let mut ed = editor_with(dir.path(), ScriptedDialogs::new().opening(&font));
        let docs_before = ed.documents().len();
        ed.dispatch(Action::Open).expect("the font loads");
        assert_eq!(
            ed.documents().len(),
            docs_before,
            "a font is not a document"
        );
        assert!(
            compositor::font_families().contains(&family),
            "the compositor shapes {family}"
        );
        assert!(
            tools::registry::type_font_choices().contains(&family.as_str()),
            "the Type tool's Font list offers {family}"
        );
    }
}
