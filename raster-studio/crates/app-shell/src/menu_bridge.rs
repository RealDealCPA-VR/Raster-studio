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
pub const NOT_WIRED: &str = "This build cannot do that yet";

/// What the shell should do about a menu click.
#[derive(Debug, Clone, PartialEq)]
pub enum Pick {
    /// A named application action, routed through [`Editor::dispatch`].
    Action(Action),
    /// A document edit, routed through history.
    Command(Command),
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
    /// Stand on this many applied commands — [`Editor::jump_history`]'s
    /// absolute depth, converted here from the panel's relative step count.
    History(usize),
    /// The active document's zoom, as a scale factor.
    Zoom(f32),
    /// The active document's camera centre, in image pixels.
    ViewCenter((f32, f32)),
    Foreground([f32; 4]),
    Background([f32; 4]),
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
        Intent::SelectLayers { active, .. } => active.map(Pick::SelectLayer),
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
        Intent::Action(action) => format!("{}: {NOT_WIRED}", action.label()),
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
    use ui::menu::ZoomCommand as Z;
    if is_workspace_camera_action(action) {
        return Some(Pick::Workspace(Box::new(Intent::Action(action))));
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
        _ => return None,
    };
    Some(Pick::Action(mapped))
}

/// Fold a pick into the frame's output.
pub fn record(pick: Pick, out: &mut ChromeOutput) {
    match pick {
        Pick::Action(action) => out.actions.push(action),
        Pick::Command(command) => out.commands.push(command),
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
        Pick::SelectLayer(layer) => out.select_layer = Some(layer),
        Pick::History(depth) => out.history_jump = Some(depth),
        Pick::Zoom(zoom) => out.set_zoom = Some(zoom),
        Pick::ViewCenter(center) => out.set_view_center = Some(center),
        Pick::Foreground(rgba) => out.set_foreground = Some(rgba),
        Pick::Background(rgba) => out.set_background = Some(rgba),
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
        Resolution::Enabled(intent) => pick(&intent, editor).ok_or_else(|| NOT_WIRED.to_string()),
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
    fn tally(ed: &Editor, ws: &Workspace) -> Tally {
        let context = context(ed, ws);
        let mut tally = Tally::default();
        for menu in menus(ed) {
            for action in menu.actions() {
                match resolve(action, &context, ed) {
                    Ok(_) => tally.performable.push(action),
                    Err(reason) if reason == NOT_WIRED => tally.unwired.push(action),
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
    const MAX_UNWIRED_WITH_A_DOCUMENT: usize = 126;
    const MIN_PERFORMABLE_WITH_A_DOCUMENT: usize = 79;
    const MAX_UNWIRED_WITH_NOTHING_OPEN: usize = 4;
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
        // The menu model allows it; this shell has no gallery for it.
        assert_eq!(
            resolve(MenuAction::FileInfo, &context, &ed),
            Err(NOT_WIRED.to_string())
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
        assert!(message.contains(NOT_WIRED), "{message}");
        assert!(
            message.contains(&MenuAction::FileInfo.label()),
            "the message must name the item: {message}"
        );
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
}
