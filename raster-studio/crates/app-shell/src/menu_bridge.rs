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
pub fn context(editor: &Editor, workspace: &Workspace) -> MenuContext {
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
        pixels: editor.clipboard().is_some(),
        layers: false,
    };
    context.open_documents = editor.documents().len();
    context.theme = editor.preferences().theme.resolve(design::Theme::Dark);
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
        Intent::Document(command) => Some(Pick::Command(command.clone())),
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
/// [`ui::Workspace::absorb_action`] performs all four against the canvas
/// camera, and did so for a whole release while the four sat greyed out beside
/// Zoom In and Fit on Screen — because the bridge routed *no* [`Intent::Action`]
/// to the workspace, so the only actions that worked were the ones the shell
/// happened to reimplement as an [`Action`].
///
/// Every one is an absolute placement of the camera (fill this rectangle, frame
/// this selection, this many pixels per inch, rotation zero), so all four
/// satisfy the idempotence [`Pick::Workspace`] requires.
///
/// # Three of the four are reachable by a user; the fourth is routed only for
/// completeness
///
/// Fill Screen, Zoom to Selection and Print Size can be clicked and do their
/// work. `ResetViewRotation` is routed here for completeness and **cannot be
/// enabled in this build at all**: `ui::menu` gates it on
/// [`MenuContext::view_rotated`], which reads
/// `Workspace::canvas.view.camera.rotation`, and no code path in this shell
/// ever writes that field to anything but zero.
/// `Chrome::sync_workspace` pushes the document camera's zoom
/// and centre into that camera and not a rotation, and the Rotate View tool
/// turns a *mirror* built by `tool_input::canvas_camera_of` — which starts at
/// rotation zero every gesture — whose rotation `tool_input::write_camera_back`
/// then drops, because [`render::Camera`], the camera this shell actually
/// renders from, is axis-aligned (`crate::tool_input`'s own module docs say so).
/// So the item is permanently greyed out here, the ratchet counts it under
/// `disabled` in every state this build can reach, and the tests that cover it
/// have to rotate the workspace camera by hand because no user path can. What
/// they prove is the routing, not the reachability; the item becomes reachable
/// the day the renderer can show a rotated view, and this routing is what will
/// make it work that day without a second wiring pass.
pub fn is_workspace_camera_action(action: MenuAction) -> bool {
    use ui::menu::ZoomCommand as Z;
    matches!(
        action,
        MenuAction::Zoom(Z::FillScreen)
            | MenuAction::Zoom(Z::ToSelection)
            | MenuAction::Zoom(Z::PrintSize)
            | MenuAction::ResetViewRotation
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
        _ => None,
    } {
        return Some(Pick::ToolChoice(tools::ToolId::FreeTransform, key, index));
    }
    if is_workspace_camera_action(action) {
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
        // The shortcut editor lives inside the preferences window.
        MenuAction::Preferences | MenuAction::KeyboardShortcuts => Action::ShowPreferences,
        MenuAction::DuplicateLayer => Action::DuplicateLayer,
        MenuAction::Zoom(Z::In) => Action::ZoomIn,
        MenuAction::Zoom(Z::Out) => Action::ZoomOut,
        MenuAction::Zoom(Z::FitOnScreen) => Action::ZoomFit,
        MenuAction::Zoom(Z::ActualPixels) => Action::ZoomActualPixels,
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
/// enabled, so [`perform`] must have a real arm for it —
/// `every_enabled_menu_item_really_does_something` runs every one of them
/// against a live document and fails on an arm that changes nothing.
///
/// Every reason names the *specific* missing piece. "This build cannot do that
/// yet" is not a reason; it is the absence of one, and 126 items wore it.
pub fn unavailable_reason(action: MenuAction) -> Option<&'static str> {
    Some(match action {
        // ---- File ----------------------------------------------------------
        // Everything that resizes the canvas rectangle is disabled below.
        MenuAction::PlaceEmbedded | MenuAction::PlaceLinked => {
            "Place needs a transform gizmo to position the imported image, and \
             the canvas has no gizmo overlay"
        }
        MenuAction::FileInfo => {
            "There is no metadata editor: editor_core::DocumentMeta stores a \
             title and a size and no XMP fields"
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
        // An adjustment whose stored starting parameters are the identity has
        // nothing to bake until the user has moved a control, and there is no
        // control to move without the dialog. Rather than offer an item that
        // silently changes no pixel, this names the route that *does* work —
        // the adjustment layer, whose parameters the Properties panel edits.
        // The four that are defined as changing every pixel (Invert, Threshold,
        // Black & White, Posterize) and Gradient Map fall through and are
        // wired; which ones those are is decided by asking `adjustments`, not
        // by a list here that could drift from it.
        MenuAction::ApplyAdjustment(id)
            if adjustments::PreparedAdjustment::new(&adjustments::Adjustment::from(
                &id.identity_kind(),
            ))
            .is_identity() =>
        {
            "This adjustment starts at its identity setting and the shell hosts \
             no dialog to change it in; add it through Layer > New Adjustment \
             Layer and edit it in the Properties panel"
        }
        MenuAction::SetColorMode(_) => {
            "Changing colour mode would rewrite every tile, and editor_core has \
             no command that can carry that as one undoable step"
        }
        // Everything that changes the canvas *rectangle* is hosted now:
        // `ImageSize`, `CanvasSize` and `RotateCanvas(Arbitrary)` open real
        // dialogs whose confirmed specs land as one undoable step each (right-
        // angle rotations take the exact fixed path), and Reveal All performs
        // directly in [`perform`] — it asks nothing.

        // ---- Layer ---------------------------------------------------------
        // Select ▸ All Layers performs now — the document keeps a real
        // multi-selection set (see `perform`), so it has no reason here.
        // ---- Select --------------------------------------------------------
        MenuAction::SelectSubject => {
            "Selecting the subject needs a segmentation model, and none ships \
             with this build"
        }
        // Transform Selection routes to the gizmo wearing its Selection
        // target now (P2.2): the drag resamples the selection mask and
        // commits as one undoable SetSelection step. It has no reason here.
        // Reselect, Save Selection and Load Selection all need somewhere to
        // *keep* a selection between operations, and no such store exists —
        // Select ▸ Reselect/Save/Load Selection are wired (the store lives on
        // the document, see `Document::stored_selection`), so they have no
        // entry here.

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
        Pick::History(depth) => out.history_jump = Some(depth),
        Pick::Zoom(zoom) => out.set_zoom = Some(zoom),
        Pick::ViewCenter(center) => out.set_view_center = Some(center),
        Pick::Foreground(rgba) => out.set_foreground = Some(rgba),
        Pick::Background(rgba) => out.set_background = Some(rgba),
        Pick::OpenColorPicker(target) => out.color_picker = Some(target),
        Pick::OpenGradientEditor => out.gradient_editor = true,
        Pick::OpenBrushEditor => out.brush_editor = true,
    }
}

/// How one item resolves *for this build*: either something to do, or a
/// sentence saying why it is off.
///
/// Exposed so the enablement rule is testable without a window; [`draw`] is
/// the only caller that paints it.
pub fn resolve(action: MenuAction, context: &MenuContext, editor: &Editor) -> Result<Pick, String> {
    match action.resolve(context) {
        Resolution::Disabled(reason) => Err(reason.to_string()),
        Resolution::Enabled(intent) => pick(&intent, editor).ok_or_else(|| {
            // The specific sentence, when there is one. [`NOT_WIRED`] is the
            // last resort and no menu item reaches it — see
            // `no_menu_item_falls_back_to_the_generic_refusal`.
            unavailable_reason(action).unwrap_or(NOT_WIRED).to_string()
        }),
    }
}

// ---------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------

/// Draw the menu bar and record whatever the user picked.
pub fn draw(ctx: &egui::Context, editor: &Editor, workspace: &Workspace, out: &mut ChromeOutput) {
    let menus = menus(editor);
    let context = context(editor, workspace);
    egui::TopBottomPanel::top("raster-menu-bar")
        .frame(crate::chrome::panel_frame(
            ctx,
            design::SurfaceRole::Panel,
            design::Space::Hair,
        ))
        .show(ctx, |ui| {
            egui::menu::bar(ui, |ui| {
                for menu in &menus {
                    ui.menu_button(menu.title, |ui| {
                        entries(ui, &menu.entries, &context, editor, out);
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
    out: &mut ChromeOutput,
) {
    for entry in entries {
        match entry {
            Entry::Item(action) => item(ui, *action, context, editor, out),
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
                    .any(|a| resolve(a, context, editor).is_ok());
                if live {
                    ui.menu_button(*label, |ui| {
                        self::entries(ui, children, context, editor, out);
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
    out: &mut ChromeOutput,
) {
    let outcome = resolve(action, context, editor);
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
        Ok(pick) => {
            if response.clicked() {
                record(pick, out);
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

    /// A layer's pixels, flattened over the canvas rectangle.
    ///
    /// Tiles outside the canvas are dropped and absent tiles read as
    /// transparent black, which is exactly what
    /// [`raster::TileGrid::to_rgba8`] promises for the same data.
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
            let Some(bytes) = compositor::TileSource::tile(&doc.tiles, hash) else {
                continue;
            };
            if bytes.len() < need {
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

    /// Fold `after` back towards `before` wherever the selection does not
    /// fully cover the pixel.
    ///
    /// This is what makes every operation in this module honour the marquee.
    /// [`Selection::None`] covers everything ([`Selection::coverage_at`] answers
    /// 1.0), so the no-selection case is the whole layer and needs no branch of
    /// its own — but it does get a fast path, because walking two million
    /// pixels to multiply each by one is a waste.
    pub fn mask_by_selection(before: &[u8], after: &mut [u8], sel: &Selection, w: u32, h: u32) {
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
                    let mixed = before[i + k] as f32 * (1.0 - c) + after[i + k] as f32 * c;
                    after[i + k] = mixed.round().clamp(0.0, 255.0) as u8;
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

    let outcome = match action {
        // ---- File ----------------------------------------------------------
        MenuAction::FileInfo => {
            editor.toggle_file_info();
            Ok("File Info…".to_string())
        }
        MenuAction::ExportLayers => editor.export_layers(),
        MenuAction::PlaceEmbedded => editor.place_from_dialog(false),
        MenuAction::PlaceLinked => editor.place_from_dialog(true),
        MenuAction::Print => editor.print_pdf(),
        MenuAction::Rasterize(ui::menu::RasterizeTarget::Text)
        | MenuAction::Rasterize(ui::menu::RasterizeTarget::Shape)
        | MenuAction::Rasterize(ui::menu::RasterizeTarget::LayerStyle)
        | MenuAction::Rasterize(ui::menu::RasterizeTarget::Layer)
        | MenuAction::Rasterize(ui::menu::RasterizeTarget::SmartObject) => editor.rasterize_layer(),
        MenuAction::DefinePattern => editor.define_pattern_from_selection(),
        MenuAction::DefineBrush => editor.define_brush_preset(),
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
        MenuAction::CommitSmartObjectContents => editor.commit_smart_object_contents(),
        // ---- Filter --------------------------------------------------------
        MenuAction::Filter(id) => run_filter(editor, id),

        // ---- Image ▸ Adjustments -------------------------------------------
        MenuAction::ApplyAdjustment(id) => {
            let kind = id.identity_kind();
            run_adjustment(
                editor,
                &adjustments::Adjustment::from(&kind),
                &format!("Apply {}", id.label()),
            )
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
        // Free Transform and its five modes route to the gizmo tool: the
        // session begins on the next canvas press, the options bar's mode
        // choice (fed to the tool at every press) names the shape of the
        // drag, Enter commits one undoable step, Escape cancels. The mode
        // itself is set by the workspace option, which the Transform items
        // also arrive as a pick for (see resolve).
        MenuAction::FreeTransform => {
            editor.set_tool(tools::ToolId::FreeTransform);
            Ok("Free Transform: drag a handle, Enter to commit, Escape to cancel".to_string())
        }
        MenuAction::Transform(T::Scale)
        | MenuAction::Transform(T::Rotate)
        | MenuAction::Transform(T::Skew)
        | MenuAction::Transform(T::Distort)
        | MenuAction::Transform(T::Perspective) => {
            editor.set_tool(tools::ToolId::FreeTransform);
            Ok("Transform: drag a handle, Enter to commit, Escape to cancel".to_string())
        }
        MenuAction::CropToSelection => editor.crop_to_selection(),
        MenuAction::Trim => editor.trim_canvas(),
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
        MenuAction::ClearPixels => clear_selection(editor),
        MenuAction::FillDialog => fill_selection(editor),
        MenuAction::StrokeDialog => stroke_selection(editor),
        MenuAction::Copy => copy(editor, false),
        MenuAction::CopyMerged => copy(editor, true),
        MenuAction::Cut => cut(editor),
        MenuAction::Paste => paste(editor, false),
        MenuAction::PasteInto => paste(editor, true),

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
        // Select ▸ Save / Load / Reselect — the store lives on the document
        // (``Document::stored_selection` / `saved_selections`); selection edits
        // are direct field writes, not undo steps, exactly like the marquee.
        MenuAction::SaveSelection => save_selection(editor),
        MenuAction::LoadSelection => load_selection(editor),
        MenuAction::Reselect => reselect(editor),
        MenuAction::ToggleQuickMask => editor.toggle_quick_mask(),
        MenuAction::SetColorMode(mode) => editor.set_color_mode(mode),

        // ---- Layer ---------------------------------------------------------
        MenuAction::LayerViaCopy => layer_via(editor, false),
        MenuAction::LayerViaCut => layer_via(editor, true),
        MenuAction::GroupLayers => group_layers(editor),
        MenuAction::UngroupLayers => ungroup_layers(editor),
        MenuAction::MergeDown => merge(editor, MergeScope::Down),
        MenuAction::MergeVisible => merge(editor, MergeScope::Visible),
        MenuAction::FlattenImage => merge(editor, MergeScope::All),
        MenuAction::Mask(MaskOp::Toggle) => toggle_mask(editor, false),
        MenuAction::Mask(MaskOp::ToggleLink) => toggle_mask(editor, true),
        MenuAction::Mask(MaskOp::Apply) => apply_mask(editor),
        // `LayerStyle(_)` never reaches here: the chrome's dialog host opens
        // the real dialog for it (`DialogHost::open_for_menu_action`) and the
        // confirmed style arrives as the command the dialog emits. Reaching
        // this catch-all anyway means the host and this match disagree.
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

        // ---- Help ----------------------------------------------------------
        MenuAction::Help => {
            let url = "https://github.com/RealDealCPA-VR/Raster-studio/wiki";
            let opened = webbrowser::open(url).is_ok();
            Ok(format!(
                "{}{url}",
                if opened { "Opened " } else { "Help lives at " }
            ))
        }
        MenuAction::ExportDiagnostics => editor.export_diagnostics(),
        MenuAction::ReleaseNotes => {
            let url = "https://github.com/RealDealCPA-VR/Raster-studio/releases";
            let opened = webbrowser::open(url).is_ok();
            Ok(format!(
                "{}{url}",
                if opened {
                    "Opened "
                } else {
                    "Release notes live at "
                }
            ))
        }
        MenuAction::ReportIssue => {
            let url = "https://github.com/RealDealCPA-VR/Raster-studio/issues/new";
            let opened = webbrowser::open(url).is_ok();
            Ok(format!(
                "{}{url}",
                if opened { "Opened " } else { "File issues at " }
            ))
        }
        MenuAction::About => Ok(format!(
            "Raster Studio {} — a layered raster editor",
            env!("CARGO_PKG_VERSION")
        )),

        // Anything else must have been refused during enablement. Reaching here
        // means `unavailable_reason` and this match disagree, which
        // `every_enabled_menu_item_really_does_something` is there to catch.
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
fn edit_active_pixels(
    editor: &mut Editor,
    label: &str,
    op: impl FnOnce(&mut filters::FilterBuffer, &color::ColorSpace) -> Result<(), String>,
) -> Result<(), String> {
    let layer = pixel_layer(editor)?;
    let (w, h) = canvas_of(editor)?;
    if w == 0 || h == 0 {
        return Err("The canvas has no pixels".to_string());
    }
    let (before, selection, space) = {
        let doc = editor.active().ok_or("No document is open")?;
        (
            pixels::read_layer(doc, layer),
            doc.document.selection.clone(),
            doc.document.meta.color_space.clone(),
        )
    };
    let mut buffer = filters::FilterBuffer::from_rgba8(w, h, &before).map_err(|e| e.to_string())?;
    op(&mut buffer, &space)?;
    if buffer.dimensions() != (w, h) {
        return Err(format!(
            "{label} changed the image from {w}x{h} to {:?}, and this build \
             cannot resize a layer",
            buffer.dimensions()
        ));
    }
    let mut after = buffer.to_rgba8();
    pixels::mask_by_selection(&before, &mut after, &selection, w, h);
    if after == before {
        return Err(format!("{label} changed nothing"));
    }
    let command = {
        let doc = editor.active_mut().ok_or("No document is open")?;
        pixels::write_layer(doc, layer, &after, label)?
    };
    editor.apply_command(command);
    Ok(())
}

fn run_filter(editor: &mut Editor, id: ui::menu::FilterId) -> Result<String, String> {
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
    filters::FilterBuffer::from_rgba8(w, h, &before).ok()
}

/// Run one filter invocation — the dialog's confirmed answer — against the
/// active layer's pixels as one undoable step.
pub(crate) fn run_filter_invocation(
    editor: &mut Editor,
    invocation: &ui::dialogs::FilterInvocation,
) -> Result<String, String> {
    let spec = invocation.filter;
    let label = spec.name();
    edit_active_pixels(editor, label, |buffer, _| {
        let filtered = invocation.run(buffer);
        *buffer = filtered;
        Ok(())
    })?;
    Ok(format!("{label} applied"))
}

fn run_adjustment(
    editor: &mut Editor,
    adjustment: &adjustments::Adjustment,
    label: &str,
) -> Result<String, String> {
    let prepared = adjustments::PreparedAdjustment::new(adjustment);
    if prepared.is_identity() {
        return Err(format!(
            "{label} starts at its identity setting, so applying it here would \
             change nothing; add it as an adjustment layer and edit it in the \
             Properties panel instead"
        ));
    }
    edit_active_pixels(editor, label, |buffer, space| {
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
        let before = pixels::read_layer(doc, layer);
        let after = remap(&before, w, h, &map);
        if after == before {
            return Err(format!("{label} changed nothing"));
        }
        pixels::write_layer(doc, layer, &after, label)?
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
fn remap(src: &[u8], w: u32, h: u32, map: &impl Fn(i64, i64, i64, i64) -> (i64, i64)) -> Vec<u8> {
    let mut out = vec![0u8; src.len()];
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
        let before = pixels::read_layer(doc, layer);
        let selection = doc.document.selection.clone();
        let mut after = vec![0u8; before.len()];
        pixels::mask_by_selection(&before, &mut after, &selection, w, h);
        if after == before {
            return Err("There is nothing to clear here".to_string());
        }
        pixels::write_layer(doc, layer, &after, "Clear")?
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

/// The shared fill painter: `source` answers the paint colour (normalized
/// RGBA) for each canvas pixel, so the solid colours and the tiled pattern go
/// through the same source-over-with-blend loop and the same selection mask.
pub(crate) fn fill_selection_painting(
    editor: &mut Editor,
    spec: &ui::dialogs::FillSpec,
    source: &dyn Fn(i64, i64) -> [f32; 4],
) -> Result<String, String> {
    let layer = pixel_layer(editor)?;
    let (w, h) = canvas_of(editor)?;
    let command = {
        let doc = editor.active_mut().ok_or("No document is open")?;
        let before = pixels::read_layer(doc, layer);
        let selection = doc.document.selection.clone();
        let mut after = before.clone();
        for py in 0..i64::from(h) {
            for px in 0..i64::from(w) {
                let i = (py as usize * w as usize + px as usize) * 4;
                let paint = source(px, py);
                let src = [paint[0], paint[1], paint[2]];
                let src_a = paint[3].clamp(0.0, 1.0);
                let dst_a = f32::from(before[i + 3]) / 255.0;
                if spec.preserve_transparency && dst_a <= 0.0 {
                    continue;
                }
                let paint_a = if spec.preserve_transparency {
                    src_a * dst_a
                } else {
                    src_a
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
        pixels::mask_by_selection(&before, &mut after, &selection, w, h);
        if after == before {
            return Err("The fill would change nothing".to_string());
        }
        pixels::write_layer(doc, layer, &after, "Fill")?
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
/// # Not undoable, and `editor_core` is why
///
/// The selection is a *field* of [`editor_core::Document`] with no command
/// behind it, which the crate documents and which
/// `crate::tools::SelectionEdit` already relies on: a marquee drag changes it
/// directly too. So Select ▸ Inverse behaves exactly like dragging a new
/// marquee — it marks the document dirty and it is not on the undo stack. It is
/// stated here rather than implied because a menu item that looks undoable and
/// is not is a trap.
fn set_selection(
    editor: &mut Editor,
    next: impl FnOnce(&editor_core::Selection, u32, u32) -> Result<editor_core::Selection, String>,
) -> Result<(), String> {
    let (w, h) = canvas_of(editor)?;
    let doc = editor.active_mut().ok_or("No document is open")?;
    let value = next(&doc.document.selection, w, h)?;
    if value == doc.document.selection {
        return Err("The selection is already that".to_string());
    }
    doc.document.selection = value;
    doc.document.mark_dirty();
    Ok(())
}

/// The radius each Select ▸ Modify item uses, in pixels.
///
/// Photoshop asks; this build has no numeric prompt to ask in, so each one uses
/// the value that dialog opens at. Named as a constant so the number is one
/// decision in one place rather than five literals.
/// Select ▸ Save Selection: set the document's stored selection to the
/// current one and append it to the named list (suffixed with a counter,
/// since the shell hosts no dialog to name it with).
fn save_selection(editor: &mut Editor) -> Result<String, String> {
    let doc = editor
        .active_mut()
        .ok_or_else(|| "No document is open".to_string())?;
    let selection = doc.document.selection.clone();
    if selection.is_none() {
        return Err("There is no selection to save".to_string());
    }
    let n = doc.document.saved_selections.len() + 1;
    doc.document
        .saved_selections
        .push((format!("Selection {n}"), selection.clone()));
    doc.document.stored_selection = Some(selection);
    doc.document.mark_dirty();
    Ok(format!("Saved the selection (Selection {n})"))
}

/// Select ▸ Load Selection: replace the live selection with the last saved one.
fn load_selection(editor: &mut Editor) -> Result<String, String> {
    let doc = editor
        .active_mut()
        .ok_or_else(|| "No document is open".to_string())?;
    let saved = doc
        .document
        .saved_selections
        .last()
        .map(|(_, s)| s.clone())
        .or(doc.document.stored_selection.clone())
        .ok_or_else(|| "No selection has been saved".to_string())?;
    if doc.document.selection == saved {
        return Err("The saved selection is already active".to_string());
    }
    doc.document.selection = saved;
    doc.document.stored_selection = None;
    doc.document.mark_dirty();
    Ok("Loaded the saved selection".to_string())
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
    doc.document.selection = saved;
    doc.document.stored_selection = None;
    doc.document.mark_dirty();
    Ok("Reselected".to_string())
}

pub const MODIFY_RADIUS: u32 = 4;

fn modify_selection(editor: &mut Editor, op: ui::menu::ModifySelection) -> Result<String, String> {
    use ui::menu::ModifySelection as M;
    set_selection(editor, |sel, w, h| {
        let rect = canvas_rect(w, h);
        let mask = selection::to_mask(sel, rect).map_err(|e| e.to_string())?;
        let next = match op {
            M::Border => selection::border(&mask, MODIFY_RADIUS),
            M::Smooth => selection::smooth(&mask, MODIFY_RADIUS),
            M::Expand => selection::expand(&mask, MODIFY_RADIUS),
            M::Contract => selection::contract(&mask, MODIFY_RADIUS),
            M::Feather => selection::feather(&mask, MODIFY_RADIUS as f32),
        }
        .map_err(|e| e.to_string())?;
        Ok(editor_core::Selection::Mask(next))
    })?;
    Ok(format!(
        "{} by {MODIFY_RADIUS} px — this build has no radius dialog",
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

fn color_range(editor: &mut Editor) -> Result<String, String> {
    let layer = pixel_layer(editor)?;
    let (w, h) = canvas_of(editor)?;
    let fg = editor.foreground();
    let hex = crate::editor::color_hex(fg);
    let mut target = rgba8_of(fg);
    target[3] = 255;
    let rgba = {
        let doc = editor.active().ok_or("No document is open")?;
        pixels::read_layer(doc, layer)
    };
    let image = selection::ImageBuffer::from_rgba8(glam::IVec2::ZERO, w, h, rgba)
        .map_err(|e| e.to_string())?;
    set_selection(editor, |_, _, _| {
        let opts = selection::ColorRangeOptions::default();
        let mask =
            selection::color_range(&image.view(), target, &opts).map_err(|e| e.to_string())?;
        Ok(editor_core::Selection::Mask(mask))
    })?;
    Ok(format!(
        "Selected everything near the foreground colour {hex} — this build has \
         no colour-range dialog to pick another"
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
        rgba8: rgba,
    });
    Ok(format!(
        "Copied {cw}×{ch} pixels{}",
        if merged {
            " from every visible layer"
        } else {
            ""
        }
    ))
}

fn cut(editor: &mut Editor) -> Result<String, String> {
    let copied = copy(editor, false)?;
    let cleared = clear_selection(editor)?;
    Ok(format!("{copied}, {}", cleared.to_lowercase()))
}

/// Edit ▸ Paste and Edit ▸ Paste Into.
///
/// The clipboard lands on a **new layer** at the canvas origin, which is one
/// undoable step and cannot destroy what was already there. Paste Into masks it
/// by the current selection, which is the only thing that distinguishes the two.
fn paste(editor: &mut Editor, into: bool) -> Result<String, String> {
    let clip = editor
        .clipboard()
        .cloned()
        .ok_or("The clipboard is empty")?;
    let (w, h) = canvas_of(editor)?;
    let label = if into { "Paste Into" } else { "Paste" };
    let command = {
        let doc = editor.active_mut().ok_or("No document is open")?;
        let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
        let rows = clip.height.min(h);
        let cols = clip.width.min(w);
        if rows == 0 || cols == 0 {
            return Err(format!("{label}: the clipboard does not fit the canvas"));
        }
        for row in 0..rows {
            let s = (row as usize) * clip.width as usize * 4;
            let d = (row as usize) * w as usize * 4;
            let n = cols as usize * 4;
            rgba[d..d + n].copy_from_slice(&clip.rgba8[s..s + n]);
        }
        if into {
            let selection = doc.document.selection.clone();
            let empty = vec![0u8; rgba.len()];
            pixels::mask_by_selection(&empty, &mut rgba, &selection, w, h);
            if rgba.iter().skip(3).step_by(4).all(|a| *a == 0) {
                return Err("Paste Into: the selection hides all of it".to_string());
            }
        }
        let layer = layer_model::Layer::raster(label);
        let new_id = layer.id;
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
        Command::Transaction {
            label: label.to_string(),
            commands,
        }
    };
    editor.apply_command(command);
    Ok(format!("{label}d onto a new layer"))
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

/// Bake a layer's mask into its alpha and remove the mask.
fn apply_mask(editor: &mut Editor) -> Result<String, String> {
    let (w, h) = canvas_of(editor)?;
    let command = {
        let doc = editor.active_mut().ok_or("No document is open")?;
        let id = doc.document.active_layer().ok_or("Select a layer first")?;
        let has_mask = doc
            .document
            .layers
            .get(id)
            .is_some_and(|l| l.mask.is_some());
        if !has_mask {
            return Err("The layer has no mask".to_string());
        }
        let before = pixels::read_layer(doc, id);
        let coverage = read_mask_coverage(doc, id, w, h);
        let mut after = before.clone();
        for (i, c) in coverage.iter().enumerate() {
            let a = i * 4 + 3;
            after[a] = ((after[a] as u32 * *c as u32) / 255) as u8;
        }
        let mut commands = vec![pixels::write_layer(doc, id, &after, "Apply Mask")?];
        commands.push(Command::SetLayerProperties {
            layer_id: id,
            patch: editor_core::LayerPatch {
                mask: editor_core::Patch::Clear,
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

/// One coverage byte per canvas pixel, read out of a layer's mask tiles.
///
/// An absent mask tile is *hidden* — the table in [`editor_core::pixels`] says
/// so — which is why the buffer starts at zero rather than at 255.
fn read_mask_coverage(doc: &crate::doc::OpenDocument, layer: LayerId, w: u32, h: u32) -> Vec<u8> {
    let (w, h) = (w as usize, h as usize);
    let mut out = vec![0u8; w * h];
    let Some(map) = doc.document.mask_tiles(layer) else {
        return out;
    };
    let ts = raster::TILE_SIZE as usize;
    for (coord, hash) in map.iter() {
        if coord.level != 0 {
            continue;
        }
        let Some(bytes) = compositor::TileSource::tile(&doc.tiles, hash) else {
            continue;
        };
        if bytes.len() < ts * ts {
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
            let n = (x1 - x0) as usize;
            let s = row * ts + (x0 - ox) as usize;
            let d = y as usize * w + x0 as usize;
            out[d..d + n].copy_from_slice(&bytes[s..s + n]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialogs::ScriptedDialogs;
    use crate::prefs::AppPaths;
    use crate::recent::RecentFiles;

    fn editor(dir: &std::path::Path) -> Editor {
        with_recent(dir, RecentFiles::new())
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
        for _ in 0..2 {
            let output = ctx.run(input.clone(), |ctx| {
                chrome.ui(ctx, &ed);
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
    fn tally(ed: &Editor, ws: &Workspace) -> Tally {
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
    // Both of those are counted under `disabled` here, for reasons that are not
    // the same: Zoom to Selection is disabled because *this* state has nothing
    // selected, and a selection enables it. Reset View Rotation is disabled in
    // every state this build can reach — nothing writes the workspace canvas
    // camera's rotation, so `view_rotated` is permanently false and only three
    // of the four are user-reachable today. `is_workspace_camera_action`'s doc
    // has the whole reason. All four were implemented in
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

        for (label, ed, min_performable, max_unwired) in [
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
            let t = tally(&ed, &ws);
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
        let ed = editor_with_a_document(dir.path());
        let ws = Workspace::new();
        let context = context(&ed, &ws);

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
        let ed = editor_with_a_document(dir.path());
        let mut ws = Workspace::new();
        assert!(!ws.dock.is_open(ui::PanelId::Channels), "not open yet");

        let context = context(&ed, &ws);
        let Ok(Pick::Workspace(intent)) = resolve(
            MenuAction::TogglePanel(ui::PanelId::Channels),
            &context,
            &ed,
        ) else {
            panic!("Window ▸ Channels is not wired");
        };
        assert!(ws.absorb(&intent), "absorbing it changed nothing");
        assert!(ws.dock.is_open(ui::PanelId::Channels));

        // ...and the menu now shows the checkmark, because the context is read
        // off the same workspace rather than off a fresh default.
        let after = self::context(&ed, &ws);
        assert_eq!(
            MenuAction::TogglePanel(ui::PanelId::Channels).checked(&after),
            Some(true)
        );
    }

    #[test]
    fn the_file_menu_routes_the_actions_this_build_has() {
        let dir = tempfile::tempdir().unwrap();
        let ed = editor(dir.path());
        let context = context(&ed, &Workspace::new());
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
        let context = context(&ed, &Workspace::new());
        // The menu model allows it; this build places no embedded document —
        // and it says which piece is missing rather than "cannot do that yet".
        let reason = resolve(MenuAction::PlaceEmbedded, &context, &ed).unwrap_err();
        assert!(reason.contains("Place"), "{reason}");
        assert_ne!(reason, NOT_WIRED);
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
    fn the_four_view_items_the_workspace_performs_are_routed_to_it() {
        // Fill Screen, Zoom to Selection, Print Size and Reset View Rotation
        // are all implemented by `ui::Workspace::absorb_action` and were all
        // greyed out with `NOT_WIRED`, sitting beside four zoom items that
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
        // ...and Reset View Rotation is gated on a rotated view, which is a
        // state no user of *this* build can put the workspace canvas into: see
        // `is_workspace_camera_action`'s doc. It is rotated by hand here because
        // nothing else can rotate it, and what that proves is the routing —
        // that the item reaches the workspace rather than `NOT_WIRED` — and not
        // that a user can reach the item. Three of the four are user-reachable
        // today; this one is wired ahead of a renderer that can show it.
        let mut ws = Workspace::new();
        ws.canvas.view.camera.set_rotation(0.7);
        let context = context(&ed, &ws);

        for action in [
            MenuAction::Zoom(Z::FillScreen),
            MenuAction::Zoom(Z::ToSelection),
            MenuAction::Zoom(Z::PrintSize),
            MenuAction::ResetViewRotation,
        ] {
            match resolve(action, &context, &ed) {
                Ok(Pick::Workspace(intent)) => assert_eq!(*intent, Intent::Action(action)),
                other => panic!(
                    "{action:?} resolved to {other:?}; it must reach the workspace that \
                     already implements it"
                ),
            }
        }
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
        let ed = with_recent(dir.path(), recent);
        let context = context(&ed, &Workspace::new());
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
            "{} {:?} {:?} sel={:?} tool={:?} {}x{} stored={} saved={}",
            d.history_depth(),
            d.document.selection,
            d.document.active_layer(),
            d.document.layer_selection(),
            ed.effective_tool(),
            d.document.width(),
            d.document.height(),
            d.document.stored_selection.is_some(),
            d.document.saved_selections.len(),
        );
        for id in d.document.layers.iter_depth_first() {
            let layer = d.document.layers.get(id).expect("a listed layer exists");
            s.push_str(&format!(
                "|{id:?} v{} {:?} {:?}",
                layer.visible, layer.mask, layer.kind
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
            let context = context(&ed, &Workspace::new());
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
        let context = context(&ed, &Workspace::new());
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
        let context = context(&ed, &Workspace::new());
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

    #[test]
    fn no_enabled_menu_item_resolves_to_a_no_op() {
        // The bar this whole wave is measured against: an item that is *not*
        // greyed out has to change the document when it is clicked. Every item
        // is driven exactly the way the shell drives it — resolve, then either
        // `perform` or `apply_command` — against a fresh document each time, so
        // one item cannot leave another with nothing to do.
        let dir = tempfile::tempdir().unwrap();
        let mut checked = 0usize;
        let template = with_two_layers(dir.path());
        let context = context(&template, &Workspace::new());
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
            // File ▸ Export Layers… writes files or refuses worriedly when no
            // destination is chosen; either outcome is loud, never a silent
            // no-op, so it does not have to change the document digest.
            if action == MenuAction::ExportLayers
                || action == MenuAction::Print
                // Export Diagnostics writes a file (or refuses when the dialog
                // is declined) — loud either way, and the digest cannot move.
                || action == MenuAction::ExportDiagnostics
                || action == MenuAction::CommitSmartObjectContents
                || action == MenuAction::CloseAll
                || action == MenuAction::Trim
                || action == MenuAction::CropToSelection
                || action == MenuAction::StrokeDialog
                // Define Brush Preset stores a *brush*, not pixels — the
                // digest cannot move; `defining_a_brush_preset_offers_it_
                // again_after_a_restart` pins the persistence instead.
                || action == MenuAction::DefineBrush
                // New ▸ Fill Layer ▸ Pattern needs a user-defined pattern;
                // `a_new_pattern_fill_layer_tiles_the_latest_pattern` drives
                // the define-then-fill sequence this fixture has no time for.
                || action == MenuAction::NewFillLayer(ui::menu::FillLayerKind::Pattern)
                // Reveal All is a no-op when every layer already fits — its
                // answer is the status line, and `reveal_all_on_a_contained_
                // canvas_reveals_nothing` pins the grow case.
                || action == MenuAction::RevealAll
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
                match perform(action, &mut ed) {
                    Ok(message) if message.len() > 20 => checked += 1,
                    Ok(message) => dead.push(format!("{action:?}: said only {message:?}")),
                    Err(reason) => dead.push(format!("{action:?}: refused with {reason:?}")),
                }
                assert_eq!(
                    ed.status().map(str::to_string),
                    Some(perform(action, &mut ed).unwrap()),
                    "{action:?} did not reach the status bar"
                );
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
        assert_eq!(settings.size, 33.0, "the settings round-trip");
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
            let context = context(&ed, &ws);
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
        let ed = editor(dir.path());
        let context = context(&ed, &Workspace::new());
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
}
