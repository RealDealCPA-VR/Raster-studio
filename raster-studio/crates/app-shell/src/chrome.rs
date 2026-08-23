//! The application chrome: menu bar, document tabs, tool palette, the layers
//! and history docks, the preferences window, and the status bar.
//!
//! # It is a view
//!
//! [`Chrome::ui`] takes `&Editor`, never `&mut Editor`. Everything the user
//! asks for comes back as a [`ChromeOutput`] the shell then performs, so the UI
//! cannot mutate a document behind history's back — and so the whole of "what
//! did that click mean" is a value a test can inspect.
//!
//! A field of [`ChromeOutput`] is set **only when the user did something this
//! frame**. Mirroring current state into it (which `select_layer` used to do)
//! turns every frame into a replay of the state the frame started with: an
//! action performed in the same frame is then immediately undone by the mirror
//! that was captured before it. See `a_new_layer_stays_active_when_the_menu_
//! creates_it`.
//!
//! # It names no colours
//!
//! Every colour, radius, gap and text size *this module chooses* comes from
//! `design`: the palette through [`design::current_tokens`], the widgets
//! through [`design::toolbar_icon_button`] and friends. There is no literal
//! `Color32` and no bare pixel gap anywhere below. The one class of colour that
//! is not the design system's to choose is the user's own foreground and
//! background, which the colour wells display and the picker edits.
//!
//! # One menu, not two
//!
//! The menu bar is **not** built here. [`crate::menu_bridge`] draws
//! `ui::menu::menu_bar` — the nine-menu Photopea-shaped model with its own
//! enablement rules and shortcut hints — and maps what it resolves to onto
//! this crate's [`Action`] catalogue. A second menu in this file would be a
//! second vocabulary to keep in step, which is exactly how the two drifted
//! apart before.
//!
//! The remaining surfaces below — tabs, tool palette, the layers and history
//! docks, the colour wells, preferences and the status bar — are still drawn
//! here rather than by `ui::Workspace`. That duplication is real and is
//! recorded as an open item; it is not hidden behind a claim that it does not
//! exist.

use std::path::PathBuf;

use design::{ColorRole, Space, SurfaceRole, TextRole, TypeRole};
use editor_core::{Command, Document, LayerPatch};
use layer_model::LayerId;
use tools::{registry, ToolId};

use crate::action::{Action, ToolKey};
use crate::doc::OpenDocument;
use crate::editor::Editor;
use crate::keymap::{Chord, Key};
use crate::prefs::{Preferences, ThemeChoice};

/// Widget ids the chrome pins down.
///
/// A themed affordance is painted by hand rather than by an `egui::Button`, so
/// it needs an id of its own — and a stable one lets a headless test find the
/// control and click it, which is how "this button emits that command" is
/// proved rather than asserted.
pub mod ids {
    pub const ADD_LAYER: &str = "raster-add-layer";
    pub const DELETE_LAYER: &str = "raster-delete-layer";
    pub const DUPLICATE_LAYER: &str = "raster-duplicate-layer";
    pub const SWAP_COLORS: &str = "raster-swap-colors";
    pub const RESET_COLORS: &str = "raster-reset-colors";

    /// The visibility toggle of one layer row.
    pub fn layer_eye(layer: layer_model::LayerId) -> egui::Id {
        egui::Id::new(("raster-layer-eye", layer))
    }

    /// One row of the history dock, by its index in [`super::history_rows`].
    pub fn history_row(index: usize) -> egui::Id {
        egui::Id::new(("raster-history-row", index))
    }
}

/// Install `theme` on an egui context so it survives the platform changing its
/// mind about light and dark.
///
/// # The bug this exists to prevent
///
/// `egui::Context::set_style` — which is what [`design::apply_theme`] calls —
/// writes only the slot for egui's *currently active* theme. egui keeps two
/// (`dark_style` and `light_style`) and swaps between them when the platform
/// reports a system theme through `RawInput`. So installing the design style
/// while egui happened to be dark leaves the light slot at egui's defaults, and
/// the first frame after the swap panics on the first `Footnote` label: the
/// design type scale registers "footnote" as a *named* text style, and
/// `TextStyle::resolve` panics rather than falling back when a name is missing.
/// It is a hard crash on a machine whose OS is set to light mode — which is how
/// this was found.
///
/// So: write **both** slots, and pin the preference so only the theme the user
/// actually chose is ever in play.
pub fn install_theme(ctx: &egui::Context, theme: design::Theme) {
    design::apply_theme(ctx, theme);
    let style = design::style_for(theme);
    ctx.set_style_of(egui::Theme::Dark, style.clone());
    ctx.set_style_of(egui::Theme::Light, style);
    ctx.set_theme(match theme {
        design::Theme::Dark => egui::ThemePreference::Dark,
        design::Theme::Light => egui::ThemePreference::Light,
    });
}

/// A shortcut the user asked to change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rebind {
    pub chord: Chord,
    pub action: Action,
    /// `true` when the user has already been shown the conflict and said
    /// "replace anyway".
    pub force: bool,
}

/// What the user asked the application to do this frame.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChromeOutput {
    /// Menu items and buttons that name an [`Action`].
    pub actions: Vec<Action>,
    /// Document edits the layers dock emitted.
    pub commands: Vec<Command>,
    /// A tab was clicked.
    pub activate: Option<usize>,
    /// A tab's close button was clicked.
    pub close: Option<usize>,
    /// A layer row was clicked **this frame**. Never a mirror of the current
    /// selection; see the module note.
    pub select_layer: Option<LayerId>,
    /// A recent-files entry was chosen.
    pub open_recent: Option<PathBuf>,
    /// A history row was clicked: walk the timeline to this many applied
    /// commands. See [`crate::Editor::jump_history`].
    pub history_jump: Option<usize>,
    /// A tool button was clicked.
    pub select_tool: Option<ToolId>,
    /// The foreground colour was edited in the colour well.
    pub set_foreground: Option<[f32; 4]>,
    /// The background colour was edited in the colour well.
    pub set_background: Option<[f32; 4]>,
    /// The preferences window changed a setting.
    pub preferences: Option<Preferences>,
    /// A shortcut was recorded in the shortcut editor.
    pub rebind: Option<Rebind>,
    /// A shortcut was cleared.
    pub unbind: Option<Chord>,
    /// "Restore defaults" in the shortcut editor.
    pub reset_keymap: bool,
    /// The conflict prompt was dismissed without replacing anything.
    pub dismiss_conflict: bool,
}

impl ChromeOutput {
    pub fn is_empty(&self) -> bool {
        *self == ChromeOutput::default()
    }
}

/// One button of the tool palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaletteEntry {
    pub key: ToolKey,
    /// The tool the button selects and draws — the active member of the group
    /// when one is active, otherwise the group's first tool.
    pub tool: ToolId,
    pub selected: bool,
}

/// The tool palette: one button per cycle group, in registry order.
///
/// A button per [`ToolId`] would be forty-five squares down the side of the
/// window; the registry already groups them behind one letter, and the button
/// shows whichever member of its group is active.
pub fn palette_entries(active: ToolId) -> Vec<PaletteEntry> {
    ToolKey::all()
        .into_iter()
        .filter_map(|key| {
            let group = registry::by_shortcut(key.char());
            let first = *group.first()?;
            let selected = group.contains(&active);
            Some(PaletteEntry {
                key,
                tool: if selected { active } else { first },
                selected,
            })
        })
        .collect()
}

/// The label for one document tab, with a bullet while it has unsaved changes.
pub fn tab_labels(editor: &Editor) -> Vec<String> {
    editor.documents().iter().map(|d| d.tab_label()).collect()
}

/// The two colours the wells in the tool strip paint.
///
/// A pure function so "the swatch shows what the editor holds" is a test rather
/// than a claim: `Action::SwapColors` and `Action::ResetColors` used to change
/// state that nothing on screen displayed.
pub fn color_wells(editor: &Editor) -> (egui::Color32, egui::Color32) {
    (
        rgba_to_color32(editor.foreground()),
        rgba_to_color32(editor.background()),
    )
}

fn rgba_to_color32(rgba: [f32; 4]) -> egui::Color32 {
    let c = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    egui::Color32::from_rgba_unmultiplied(c(rgba[0]), c(rgba[1]), c(rgba[2]), c(rgba[3]))
}

fn color32_to_rgba(c: egui::Color32) -> [f32; 4] {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    [
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0,
        a as f32 / 255.0,
    ]
}

/// One row of the layers dock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerRow {
    pub id: LayerId,
    /// Nesting depth; group children are indented by it. The Wave-0 panel
    /// walked only `layers.root()`, so everything inside a group was invisible.
    pub depth: usize,
    pub name: String,
    pub visible: bool,
    pub is_group: bool,
    pub selected: bool,
    /// Opacity as a whole percentage, for the trailing hint.
    pub opacity_percent: u32,
}

/// Every layer in composite order, top-most first, groups descended into.
pub fn layer_rows(doc: &Document, active: Option<LayerId>) -> Vec<LayerRow> {
    doc.layers
        .iter_depth_first()
        .into_iter()
        .filter_map(|id| {
            let layer = doc.layers.get(id)?;
            Some(LayerRow {
                id,
                depth: doc.layers.depth_of(id).unwrap_or(0),
                name: layer.name.clone(),
                visible: layer.visible,
                is_group: layer.is_group(),
                selected: active == Some(id),
                opacity_percent: (layer.opacity.clamp(0.0, 1.0) * 100.0).round() as u32,
            })
        })
        .collect()
}

/// The command a layer row's eye emits.
pub fn toggle_visibility_command(row: &LayerRow) -> Command {
    Command::SetLayerProperties {
        layer_id: row.id,
        patch: LayerPatch {
            visible: Some(!row.visible),
            ..Default::default()
        },
    }
}

/// One row of the history dock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryRow {
    pub label: String,
    /// The state the document is standing on right now.
    pub current: bool,
    /// How many commands are applied while standing on this row — what
    /// [`Editor::jump_history`] is asked for when the row is clicked.
    pub depth: usize,
    /// This step has been undone: it is ahead of where the document stands, and
    /// clicking it redoes forward to it.
    pub undone: bool,
}

/// The whole timeline of the active document, oldest first.
///
/// Row 0 is the state *before* any command, so the panel can walk all the way
/// back; each later row is the state after one more step. Undone steps stay in
/// the list rather than vanishing, because a history panel a user cannot click
/// forward in is only half of one.
pub fn history_rows(doc: &OpenDocument) -> Vec<HistoryRow> {
    let here = doc.history_depth();
    let mut rows = vec![HistoryRow {
        label: "Original".to_string(),
        current: here == 0,
        depth: 0,
        undone: false,
    }];
    rows.extend(
        doc.history_timeline()
            .into_iter()
            .enumerate()
            .map(|(i, label)| HistoryRow {
                label,
                current: i + 1 == here,
                depth: i + 1,
                undone: i + 1 > here,
            }),
    );
    rows
}

/// One row of the shortcut editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortcutRow {
    pub action: Action,
    pub label: String,
    pub chord: Option<Chord>,
}

/// Every action with the chord that performs it, in menu order.
pub fn shortcut_rows(editor: &Editor) -> Vec<ShortcutRow> {
    Action::all()
        .into_iter()
        .map(|action| ShortcutRow {
            action,
            label: format!("{} · {}", action.category().title(), action.label()),
            chord: editor.keymap().shortcut_for(action),
        })
        .collect()
}

/// Turn an egui key press into a [`Chord`] the keymap can hold.
///
/// `None` for keys the keymap has no spelling for, so recording one leaves the
/// existing binding alone rather than storing something that can never match.
pub fn chord_from_egui(key: egui::Key, mods: egui::Modifiers) -> Option<Chord> {
    use egui::Key as K;
    let mapped = match key {
        K::Tab => Key::Tab,
        K::Space => Key::Space,
        K::Enter => Key::Enter,
        K::Escape => Key::Escape,
        K::Backspace => Key::Backspace,
        K::Delete => Key::Delete,
        K::ArrowLeft => Key::ArrowLeft,
        K::ArrowRight => Key::ArrowRight,
        K::ArrowUp => Key::ArrowUp,
        K::ArrowDown => Key::ArrowDown,
        K::Minus => Key::Char('-'),
        K::Plus => Key::Char('+'),
        K::Equals => Key::Char('='),
        K::Comma => Key::Char(','),
        K::Period => Key::Char('.'),
        K::Semicolon => Key::Char(';'),
        K::Colon => Key::Char(':'),
        K::Slash => Key::Char('/'),
        K::Backslash => Key::Char('\\'),
        K::Pipe => Key::Char('|'),
        K::Questionmark => Key::Char('?'),
        K::OpenBracket => Key::Char('['),
        K::CloseBracket => Key::Char(']'),
        K::Backtick => Key::Char('`'),
        K::Quote => Key::Char('\''),
        other => {
            // `egui::Key::name` already spells the letters and digits as single
            // characters ("A", "0"); everything else is a word, and the only
            // words the keymap can hold are the function keys.
            let name = other.name();
            let mut chars = name.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_alphanumeric() => Key::character(c),
                _ => match name.strip_prefix('F').map(str::parse::<u8>) {
                    Some(Ok(n)) if (1..=24).contains(&n) => Key::Function(n),
                    _ => return None,
                },
            }
        }
    };
    Some(Chord {
        ctrl_or_cmd: mods.ctrl || mods.mac_cmd || mods.command,
        alt: mods.alt,
        shift: mods.shift,
        key: mapped,
    })
}

/// The chrome's own view state. Nothing here is document or editor state — it
/// is only "which row of the shortcut editor is listening for a key press".
#[derive(Debug, Default)]
pub struct Chrome {
    /// The action whose shortcut is being recorded, if any.
    capturing: Option<Action>,
}

impl Chrome {
    pub fn new() -> Self {
        Self::default()
    }

    /// The action currently listening for a chord, for tests and for the shell.
    pub fn capturing(&self) -> Option<Action> {
        self.capturing
    }

    /// Draw one frame of chrome.
    pub fn ui(&mut self, ctx: &egui::Context, editor: &Editor) -> ChromeOutput {
        let mut out = ChromeOutput::default();

        self.menu_bar(ctx, editor, &mut out);
        if editor.documents().len() > 1 || editor.panels_visible() {
            self.tab_strip(ctx, editor, &mut out);
        }
        if editor.panels_visible() {
            self.tool_palette(ctx, editor, &mut out);
            self.side_panels(ctx, editor, &mut out);
        }
        self.status_bar(ctx, editor);
        if editor.preferences_open() {
            self.preferences_window(ctx, editor, &mut out);
        } else {
            self.capturing = None;
        }
        out
    }

    /// The menu bar, drawn by [`crate::menu_bridge`] from `ui::menu::menu_bar`.
    ///
    /// There is deliberately no menu structure in this file any more. The nine
    /// menus, their labels, their shortcut hints and their enablement all come
    /// from the shared model in the `ui` crate; the bridge is the one place
    /// that says which of them this build can actually perform.
    fn menu_bar(&self, ctx: &egui::Context, editor: &Editor, out: &mut ChromeOutput) {
        crate::menu_bridge::draw(ctx, editor, out);
    }

    fn tab_strip(&self, ctx: &egui::Context, editor: &Editor, out: &mut ChromeOutput) {
        egui::TopBottomPanel::top("raster-tabs")
            .frame(panel_frame(ctx, SurfaceRole::Panel, Space::Hair))
            .show(ctx, |ui| {
                if editor.documents().is_empty() {
                    let tokens = design::current_tokens(ui);
                    ui.colored_label(
                        design::color32(tokens.palette.text(TextRole::Tertiary)),
                        "No document open — File ▸ Open, or drop an image here",
                    );
                    return;
                }
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = Space::XSmall.pt();
                    for (index, doc) in editor.documents().iter().enumerate() {
                        let selected = editor.active_index() == Some(index);
                        let response = design::list_row(ui, &doc.tab_label(), selected);
                        let response = match doc.project_path() {
                            Some(p) => response.on_hover_text(p.display().to_string()),
                            None => response.on_hover_text("not saved yet"),
                        };
                        if response.clicked() {
                            out.activate = Some(index);
                        }
                        if design::ghost_button(ui, "×")
                            .on_hover_text("Close")
                            .clicked()
                        {
                            out.close = Some(index);
                        }
                    }
                });
            });
    }

    fn tool_palette(&self, ctx: &egui::Context, editor: &Editor, out: &mut ChromeOutput) {
        egui::SidePanel::left("raster-tools")
            .resizable(false)
            .exact_width(tool_strip_width(ctx))
            .frame(panel_frame(ctx, SurfaceRole::Panel, Space::XSmall))
            .show(ctx, |ui| {
                ui.spacing_mut().item_spacing.y = Space::Hair.pt();
                for entry in palette_entries(editor.effective_tool()) {
                    let info = registry::info(entry.tool);
                    // The registry's `icon` is a KEY, not a glyph. Passing it
                    // to a text button paints the words "marquee-rect" wrapped
                    // across the strip; `ui::icons` resolves it to a drawing.
                    let icon_key = info.map(|i| i.icon).unwrap_or("");
                    let name = info.map(|i| i.name).unwrap_or("Tool");
                    let tooltip = format!("{name}  ({})", entry.key);
                    if ui::icons::icon_button(ui, icon_key, &tooltip, entry.selected).clicked() {
                        out.select_tool = Some(entry.tool);
                    }
                }
                ui.add_space(Space::Medium.pt());
                self.color_wells_ui(ui, editor, out);
            });
    }

    /// The foreground / background wells, and the two affordances that act on
    /// them. This is the surface `Action::SwapColors` and `Action::ResetColors`
    /// change: before it existed, both were menu items with no visible effect.
    fn color_wells_ui(&self, ui: &mut egui::Ui, editor: &Editor, out: &mut ChromeOutput) {
        let tokens = design::current_tokens(ui);
        let side = tokens.metrics.toolbar_button;
        let (mut foreground, mut background) = color_wells(editor);

        ui.scope(|ui| {
            ui.spacing_mut().interact_size = egui::vec2(side, side);
            ui.spacing_mut().item_spacing.y = Space::Hair.pt();
            if egui::color_picker::color_edit_button_srgba(
                ui,
                &mut foreground,
                egui::color_picker::Alpha::Opaque,
            )
            .on_hover_text("Foreground colour")
            .changed()
            {
                out.set_foreground = Some(color32_to_rgba(foreground));
            }
            if egui::color_picker::color_edit_button_srgba(
                ui,
                &mut background,
                egui::color_picker::Alpha::Opaque,
            )
            .on_hover_text("Background colour")
            .changed()
            {
                out.set_background = Some(color32_to_rgba(background));
            }
        });

        if icon_affordance(ui, ids::SWAP_COLORS, "⇄", "Swap colours  (X)", None).clicked() {
            out.actions.push(Action::SwapColors);
        }
        if icon_affordance(ui, ids::RESET_COLORS, "◨", "Default colours  (D)", None).clicked() {
            out.actions.push(Action::ResetColors);
        }
    }

    fn side_panels(&self, ctx: &egui::Context, editor: &Editor, out: &mut ChromeOutput) {
        let Some(doc) = editor.active() else {
            return;
        };
        self.layers_dock(ctx, editor, out);
        self.history_dock(ctx, doc, out);
    }

    /// The layers dock — themed, nested, and the only place a layer row can put
    /// a selection into [`ChromeOutput::select_layer`].
    fn layers_dock(&self, ctx: &egui::Context, editor: &Editor, out: &mut ChromeOutput) {
        let Some(open) = editor.active() else {
            return;
        };
        let doc = &open.document;
        let rows = layer_rows(doc, doc.active_layer());
        let can_delete = editor.can(Action::DeleteLayer).is_ok();
        let can_duplicate = editor.can(Action::DuplicateLayer);

        egui::SidePanel::left("raster-layers")
            .resizable(true)
            .default_width(dock_width(ctx))
            .frame(panel_frame(ctx, SurfaceRole::Panel, Space::XSmall))
            .show(ctx, |ui| {
                let tokens = design::current_tokens(ui);
                let dim = design::color32(tokens.palette.text(TextRole::Tertiary));
                design::section_header(ui, "LAYERS");

                // Leave room for the button row below, but never ask for a
                // negative height: a very short window would otherwise hand
                // the scroll area a nonsense budget.
                let list_height =
                    (ui.available_height() - tokens.metrics.toolbar_button - Space::Small.pt())
                        .max(tokens.metrics.list_row_height);
                egui::ScrollArea::vertical()
                    .auto_shrink([false, true])
                    .max_height(list_height)
                    .show(ui, |ui| {
                        if rows.is_empty() {
                            ui.colored_label(dim, "No layers yet — add one with +");
                        }
                        for row in &rows {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = Space::Hair.pt();
                                if row.depth > 0 {
                                    ui.add_space(row.depth as f32 * Space::Small.pt());
                                }
                                let eye = if row.visible { "◉" } else { "○" };
                                let tip = if row.visible { "Hide" } else { "Show" };
                                if eye_affordance(ui, ids::layer_eye(row.id), eye, tip).clicked() {
                                    out.commands.push(toggle_visibility_command(row));
                                }
                                let label = if row.is_group {
                                    format!("▸ {}", row.name)
                                } else if row.opacity_percent == 100 {
                                    row.name.clone()
                                } else {
                                    format!("{}  {}%", row.name, row.opacity_percent)
                                };
                                if design::list_row(ui, &label, row.selected).clicked() {
                                    out.select_layer = Some(row.id);
                                }
                            });
                        }
                    });

                ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = Space::Hair.pt();
                        // The *action*, not a bare `Command::CreateLayer`.
                        // `Command` never touches the active layer, so the
                        // button used to leave the selection on the old layer
                        // while the menu's New Layer moved it to the new one —
                        // and the “−” beside it then deleted a different layer
                        // than the one just created.
                        if icon_affordance(ui, ids::ADD_LAYER, "+", "New layer", None).clicked() {
                            out.actions.push(Action::NewLayer);
                        }
                        // A greyed-out control that will not say why is only
                        // half an answer — the same rule the menu bar keeps.
                        if icon_affordance(
                            ui,
                            ids::DUPLICATE_LAYER,
                            "⧉",
                            "Duplicate layer",
                            can_duplicate
                                .as_ref()
                                .err()
                                .map(|e| e.to_string())
                                .as_deref(),
                        )
                        .clicked()
                        {
                            out.actions.push(Action::DuplicateLayer);
                        }
                        if icon_affordance(
                            ui,
                            ids::DELETE_LAYER,
                            "−",
                            "Delete layer",
                            (!can_delete).then_some("select a layer first"),
                        )
                        .clicked()
                        {
                            // Also the action: it deletes whatever is active
                            // when the frame is *applied*, which after a click
                            // on “+” in the same frame is the new layer.
                            out.actions.push(Action::DeleteLayer);
                        }
                    });
                });
            });
    }

    /// The history dock: the whole timeline, and every row is a place to stand.
    ///
    /// Each row is a real control. It used to be drawn with
    /// [`design::list_row`], whose response was discarded — so every entry
    /// highlighted under the pointer, advertised itself as clickable, and did
    /// nothing at all. Now a click walks [`editor_core::History`] to that step
    /// (through [`crate::Editor::jump_history`], so it is exactly the steps
    /// Ctrl+Z and Ctrl+Shift+Z would have taken), and the one row that has
    /// nowhere to go — the state the document is already on — is rendered as a
    /// selected, non-clickable row that says so on hover.
    fn history_dock(&self, ctx: &egui::Context, doc: &OpenDocument, out: &mut ChromeOutput) {
        let rows = history_rows(doc);
        egui::SidePanel::right("raster-history")
            .resizable(true)
            .default_width(dock_width(ctx))
            .frame(panel_frame(ctx, SurfaceRole::Panel, Space::XSmall))
            .show(ctx, |ui| {
                design::section_header(ui, "HISTORY");
                egui::ScrollArea::vertical()
                    .auto_shrink([false, true])
                    .show(ui, |ui| {
                        for (index, row) in rows.iter().enumerate() {
                            let tooltip = if row.current {
                                "The step the document is on".to_string()
                            } else if row.undone {
                                format!("Redo forward to “{}”", row.label)
                            } else {
                                format!("Undo back to “{}”", row.label)
                            };
                            if history_row_affordance(ui, ids::history_row(index), row, &tooltip)
                                .clicked()
                            {
                                out.history_jump = Some(row.depth);
                            }
                        }
                    });
            });
    }

    fn status_bar(&self, ctx: &egui::Context, editor: &Editor) {
        egui::TopBottomPanel::bottom("raster-status")
            .frame(panel_frame(ctx, SurfaceRole::Panel, Space::Hair))
            .show(ctx, |ui| {
                let tokens = design::current_tokens(ui);
                let dim = design::color32(tokens.palette.text(TextRole::Secondary));
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = Space::Small.pt();
                    match editor.active() {
                        Some(doc) => {
                            ui.label(
                                egui::RichText::new(doc.title())
                                    .text_style(design::egui_theme::text_style(TypeRole::Footnote)),
                            );
                            ui.colored_label(
                                dim,
                                format!("{} × {}", doc.document.width(), doc.document.height()),
                            );
                            ui.colored_label(dim, format!("{:.0}%", doc.camera.zoom * 100.0));
                            ui.colored_label(dim, format!("{} layers", doc.document.layers.len()));
                        }
                        None => {
                            ui.colored_label(dim, "No document");
                        }
                    }
                    let tool = registry::info(editor.effective_tool())
                        .map(|i| i.name)
                        .unwrap_or("Tool");
                    ui.colored_label(dim, tool);
                    ui.colored_label(dim, format!("{} px", editor.brush().size as i32));
                    if let Some(status) = editor.status() {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.colored_label(dim, status);
                        });
                    }
                });
            });
    }

    /// Preferences, including the shortcut editor.
    ///
    /// This is what makes task item 9 reachable: theme, UI scale, autosave
    /// interval, history depth and the keymap were all persisted long before
    /// anything but a text editor could change them.
    fn preferences_window(&mut self, ctx: &egui::Context, editor: &Editor, out: &mut ChromeOutput) {
        let mut prefs = editor.preferences().clone();
        let original = prefs.clone();
        let mut open = true;

        egui::Window::new("Preferences")
            .open(&mut open)
            .resizable(true)
            .default_width(dock_width(ctx) * 2.0)
            .frame(overlay_frame(ctx))
            .show(ctx, |ui| {
                design::section_header(ui, "APPEARANCE");
                let mut theme_index = ThemeChoice::ALL
                    .iter()
                    .position(|t| *t == prefs.theme)
                    .unwrap_or(0);
                let labels: Vec<&str> = ThemeChoice::ALL.iter().map(|t| t.label()).collect();
                design::inspector_field(ui, "Theme", |ui| {
                    if design::segmented_control(ui, "raster-theme", &mut theme_index, &labels) {
                        prefs.theme = ThemeChoice::ALL[theme_index];
                    }
                });
                design::slider_row(
                    ui,
                    "UI scale",
                    &mut prefs.ui_scale,
                    Preferences::MIN_UI_SCALE..=Preferences::MAX_UI_SCALE,
                );

                design::section_header(ui, "DOCUMENTS");
                let mut autosave = prefs.autosave_interval_secs as f32;
                if design::slider_row(
                    ui,
                    "Autosave (s)",
                    &mut autosave,
                    0.0..=Preferences::MAX_AUTOSAVE_SECS as f32,
                )
                .changed()
                {
                    prefs.autosave_interval_secs = autosave.round().max(0.0) as u64;
                }
                let mut depth = prefs.history_depth as f32;
                if design::slider_row(
                    ui,
                    "History depth",
                    &mut depth,
                    Preferences::MIN_HISTORY_DEPTH as f32..=Preferences::MAX_HISTORY_DEPTH as f32,
                )
                .changed()
                {
                    prefs.history_depth = depth.round().max(1.0) as usize;
                }
                let tokens = design::current_tokens(ui);
                let dim = design::color32(tokens.palette.text(TextRole::Tertiary));
                design::inspector_field(ui, "Scratch", |ui| {
                    let path = prefs.scratch_dir(editor.paths());
                    ui.colored_label(dim, path.display().to_string())
                        .on_hover_text(
                            "Autosaves of documents that have never been saved live here",
                        );
                });

                self.shortcut_editor(ui, editor, out);
            });

        if !open {
            // The window's own close button. Toggling the action keeps the
            // editor the one place that knows whether the window is up.
            out.actions.push(Action::ShowPreferences);
        }
        let changed = prefs.clone().sanitized() != original.clone().sanitized();
        if changed {
            out.preferences = Some(prefs);
        }
    }

    fn shortcut_editor(&mut self, ui: &mut egui::Ui, editor: &Editor, out: &mut ChromeOutput) {
        design::section_header(ui, "KEYBOARD SHORTCUTS");
        let tokens = design::current_tokens(ui);
        let dim = design::color32(tokens.palette.text(TextRole::Tertiary));
        let warn = design::color32(tokens.palette.color(ColorRole::Warning));

        if let Some(conflict) = editor.pending_conflict() {
            ui.horizontal(|ui| {
                ui.colored_label(warn, conflict.to_string());
                if design::primary_button(ui, "Replace").clicked() {
                    if let Some(&action) = conflict.actions.last() {
                        out.rebind = Some(Rebind {
                            chord: conflict.chord,
                            action,
                            force: true,
                        });
                    }
                }
                if design::secondary_button(ui, "Keep").clicked() {
                    out.dismiss_conflict = true;
                }
            });
        }

        // While a row is listening, the next key press becomes its chord.
        if let Some(action) = self.capturing {
            if let Some(chord) = recorded_chord(ui.ctx()) {
                self.capturing = None;
                if chord.key != Key::Escape {
                    out.rebind = Some(Rebind {
                        chord,
                        action,
                        force: false,
                    });
                }
            }
        }

        egui::ScrollArea::vertical()
            .id_salt("raster-shortcuts")
            .max_height(tokens.metrics.list_row_height * 8.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for row in shortcut_rows(editor) {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = Space::XSmall.pt();
                        let listening = self.capturing == Some(row.action);
                        design::inspector_field(ui, &row.label, |ui| {
                            let text = if listening {
                                "press a chord…".to_string()
                            } else {
                                row.chord.map(|c| c.to_string()).unwrap_or_default()
                            };
                            if design::ghost_button(ui, if text.is_empty() { "—" } else { &text })
                                .on_hover_text("Click, then press the chord you want")
                                .clicked()
                            {
                                self.capturing = Some(row.action);
                            }
                            let clear = ui.add_enabled(
                                row.chord.is_some(),
                                egui::Button::new("Clear").frame(false),
                            );
                            if clear.clicked() {
                                if let Some(chord) = row.chord {
                                    out.unbind = Some(chord);
                                }
                            }
                        });
                    });
                }
            });

        ui.horizontal(|ui| {
            if design::secondary_button(ui, "Restore defaults").clicked() {
                out.reset_keymap = true;
                self.capturing = None;
            }
            ui.colored_label(dim, "Overrides are saved with your preferences");
        });
    }
}

/// The first key press of this frame, as a chord.
fn recorded_chord(ctx: &egui::Context) -> Option<Chord> {
    ctx.input(|i| {
        i.events.iter().find_map(|e| match e {
            egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => chord_from_egui(*key, *modifiers),
            _ => None,
        })
    })
}

/// Width of the tool strip: one square button plus the panel's own padding.
fn tool_strip_width(ctx: &egui::Context) -> f32 {
    let tokens = design::current_theme(ctx).tokens();
    tokens.metrics.toolbar_button + 2.0 * Space::XSmall.pt() + Space::Small.pt()
}

/// Default width of the layers and history docks, on the 4pt grid.
fn dock_width(ctx: &egui::Context) -> f32 {
    let m = &design::current_theme(ctx).tokens().metrics;
    2.0 * m.inspector_label_width + 2.0 * m.panel_padding
}

/// A panel frame in one of the design surfaces, with a hairline edge.
pub(crate) fn panel_frame(ctx: &egui::Context, surface: SurfaceRole, pad: Space) -> egui::Frame {
    let tokens = design::current_theme(ctx).tokens();
    egui::Frame::none()
        .fill(design::color32(tokens.palette.surface(surface)))
        .inner_margin(egui::Margin::symmetric(Space::Small.pt(), pad.pt()))
        .stroke(egui::Stroke::new(
            tokens.borders.hairline,
            design::color32(tokens.palette.color(design::ColorRole::SeparatorHairline)),
        ))
}

/// A floating surface: the overlay fill, a soft shadow, and the overlay radius.
fn overlay_frame(ctx: &egui::Context) -> egui::Frame {
    let theme = design::current_theme(ctx);
    let tokens = theme.tokens();
    let radius = design::Radius::Large.resolve(&tokens.radii, tokens.metrics.control_height);
    egui::Frame::none()
        .fill(design::color32(
            tokens.palette.surface(SurfaceRole::Overlay),
        ))
        .rounding(design::egui_theme::rounding(radius))
        .inner_margin(egui::Margin::same(tokens.metrics.panel_padding))
        .shadow(design::egui_theme::shadow(
            &tokens.palette,
            design::Elevation::Overlay,
        ))
}

/// A quiet square affordance with an explicit id.
///
/// Painted from tokens rather than by an `egui::Button` for two reasons: the
/// hover / pressed / disabled states are the design system's, and the stable id
/// lets a headless test find the control and click it.
///
/// `disabled_reason` is both the switch and the tooltip: `Some` greys the
/// control out *and* is what the hover says, so a control can never be off
/// without saying why. (`Response::on_disabled_hover_text` would be silent
/// here — it keys off `Ui::is_enabled`, which a hand-painted widget does not
/// change.)
fn icon_affordance(
    ui: &mut egui::Ui,
    id: &str,
    glyph: &str,
    tooltip: &str,
    disabled_reason: Option<&str>,
) -> egui::Response {
    affordance(ui, egui::Id::new(id), glyph, tooltip, disabled_reason)
}

fn eye_affordance(ui: &mut egui::Ui, id: egui::Id, glyph: &str, tooltip: &str) -> egui::Response {
    affordance(ui, id, glyph, tooltip, None)
}

/// One history row: a full-width list row with an id of its own.
///
/// Same shape and the same tokens as [`design::list_row`] — the row height, the
/// selection fill, the hover fill, the radius and the two text pairings all
/// come from the design system, nothing here is a literal. What it adds is a
/// *stable id*, so a headless test can find row N and click it, and a
/// non-interactive mode for the row that is already where the document stands:
/// that one senses hover only, so it neither highlights as a control nor
/// pretends a click would do something.
fn history_row_affordance(
    ui: &mut egui::Ui,
    id: egui::Id,
    row: &HistoryRow,
    tooltip: &str,
) -> egui::Response {
    let tokens = design::current_tokens(ui);
    let palette = &tokens.palette;
    let height = tokens.metrics.list_row_height;
    let width = ui.available_width().max(tokens.metrics.min_hit_target);
    let (_auto, rect) = ui.allocate_space(egui::vec2(width, height));
    // `interact` rather than the allocation's own response, so the row is
    // registered under the id the caller chose.
    let sense = if row.current {
        egui::Sense::hover()
    } else {
        egui::Sense::click()
    };
    let response = ui.interact(rect, id, sense);

    if ui.is_rect_visible(rect) {
        let fill = if row.current {
            design::color32(palette.color(ColorRole::SelectionFill))
        } else if response.is_pointer_button_down_on() {
            design::color32(palette.color(ColorRole::ControlFillActive))
        } else if response.hovered() {
            design::color32(palette.color(ColorRole::ControlFillHovered))
        } else {
            egui::Color32::TRANSPARENT
        };
        let radius = design::Radius::Medium.resolve(&tokens.radii, height);
        if fill != egui::Color32::TRANSPARENT {
            ui.painter()
                .rect_filled(rect, design::egui_theme::rounding(radius), fill);
        }
        if response.has_focus() {
            ui.painter().rect_stroke(
                rect,
                design::egui_theme::rounding(radius),
                egui::Stroke::new(
                    tokens.borders.focus_ring,
                    design::color32(palette.color(ColorRole::FocusRing)),
                ),
            );
        }
        // An undone step is ahead of where we stand: quieter, like any other
        // inactive thing in this palette.
        let text = if row.current {
            palette.text(TextRole::Primary)
        } else if row.undone {
            palette.text(TextRole::Tertiary)
        } else {
            palette.text(TextRole::Secondary)
        };
        ui.painter().text(
            egui::pos2(rect.left() + Space::Small.pt(), rect.center().y),
            egui::Align2::LEFT_CENTER,
            &row.label,
            design::egui_theme::font_id(tokens, TypeRole::Body),
            design::color32(text),
        );
    }
    response.on_hover_text(tooltip)
}

fn affordance(
    ui: &mut egui::Ui,
    id: egui::Id,
    glyph: &str,
    tooltip: &str,
    disabled_reason: Option<&str>,
) -> egui::Response {
    let enabled = disabled_reason.is_none();
    let tokens = design::current_tokens(ui);
    let palette = &tokens.palette;
    let side = tokens.metrics.min_hit_target;
    let (_auto, rect) = ui.allocate_space(egui::vec2(side, side));
    // `interact` rather than `allocate_exact_size` so the widget is registered
    // under the id the caller chose: that is what a headless test looks up.
    let response = ui.interact(
        rect,
        id,
        if enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        },
    );
    if ui.is_rect_visible(rect) {
        let fill = if !enabled {
            egui::Color32::TRANSPARENT
        } else if response.is_pointer_button_down_on() {
            design::color32(palette.color(ColorRole::ControlFillActive))
        } else if response.hovered() {
            design::color32(palette.color(ColorRole::ControlFillHovered))
        } else {
            egui::Color32::TRANSPARENT
        };
        let radius = design::Radius::Medium.resolve(&tokens.radii, side);
        if fill != egui::Color32::TRANSPARENT {
            ui.painter()
                .rect_filled(rect, design::egui_theme::rounding(radius), fill);
        }
        if response.has_focus() {
            ui.painter().rect_stroke(
                rect,
                design::egui_theme::rounding(radius),
                egui::Stroke::new(
                    tokens.borders.focus_ring,
                    design::color32(palette.color(ColorRole::FocusRing)),
                ),
            );
        }
        let text = if !enabled {
            palette.text(TextRole::Disabled)
        } else if response.hovered() {
            palette.text(TextRole::Primary)
        } else {
            palette.text(TextRole::Secondary)
        };
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            glyph,
            design::egui_theme::font_id(tokens, TypeRole::Body),
            design::color32(text),
        );
    }
    let hover = disabled_reason.unwrap_or(tooltip);
    if hover.is_empty() {
        response
    } else {
        response.on_hover_text(hover)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialogs::ScriptedDialogs;
    use crate::prefs::{AppPaths, Preferences};
    use crate::recent::RecentFiles;
    use layer_model::Layer;

    fn editor(dir: &std::path::Path) -> Editor {
        Editor::with_state(
            AppPaths::rooted(dir),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        )
    }

    fn png(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(
            &path,
            raster::encode(raster::ExportFormat::Png, 8, 8, &[9u8; 8 * 8 * 4]).unwrap(),
        )
        .unwrap();
        path
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

    /// Run the chrome headlessly, optionally clicking one widget by id.
    ///
    /// Two passes: the first registers every widget's rectangle, the second
    /// delivers a press and a release at the target's centre. What comes back
    /// is the second pass's [`ChromeOutput`].
    fn run_chrome(editor: &Editor, click: Option<egui::Id>) -> ChromeOutput {
        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut chrome = Chrome::new();
        let mut out = ChromeOutput::default();
        let _ = ctx.run(raw_input(Vec::new()), |ctx| {
            out = chrome.ui(ctx, editor);
        });
        let Some(id) = click else { return out };
        let rect = ctx
            .read_response(id)
            .unwrap_or_else(|| panic!("{id:?} was never drawn"))
            .rect;
        let pos = rect.center();
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
            out = chrome.ui(ctx, editor);
        });
        out
    }

    #[test]
    fn the_palette_has_one_button_per_tool_letter() {
        let entries = palette_entries(ToolId::Move);
        assert_eq!(entries.len(), ToolKey::all().len());
        assert!(!entries.is_empty());
        // Every button names a real tool with an icon and a name.
        for entry in &entries {
            let info = registry::info(entry.tool).expect("a registry tool");
            assert!(!info.icon.is_empty());
            assert!(!info.name.is_empty());
        }
        // Exactly one is selected, and it is the group holding the active tool.
        let selected: Vec<_> = entries.iter().filter(|e| e.selected).collect();
        assert_eq!(selected.len(), 1, "{selected:?}");
        assert_eq!(selected[0].tool, ToolId::Move);
    }

    #[test]
    fn a_group_button_shows_whichever_member_is_active() {
        let (key, group) = ToolKey::all()
            .into_iter()
            .map(|k| (k, registry::by_shortcut(k.char())))
            .find(|(_, g)| g.len() > 1)
            .expect("the registry has a cycle group");
        let second = group[1];
        let entry = palette_entries(second)
            .into_iter()
            .find(|e| e.key == key)
            .unwrap();
        assert!(entry.selected);
        assert_eq!(entry.tool, second, "the button follows the active member");

        // ...and falls back to the group's first tool when none is active.
        let outside = *ToolId::ALL
            .iter()
            .find(|t| !group.contains(t))
            .expect("some tool is outside this group");
        let entry = palette_entries(outside)
            .into_iter()
            .find(|e| e.key == key)
            .unwrap();
        assert!(!entry.selected);
        assert_eq!(entry.tool, group[0]);
    }

    #[test]
    fn tab_labels_mark_unsaved_documents() {
        let dir = tempfile::tempdir().unwrap();
        let p = png(dir.path(), "a.png");
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&p).unwrap();
        assert_eq!(tab_labels(&ed), ["a.png"]);
        ed.dispatch(Action::NewLayer).unwrap();
        assert_eq!(tab_labels(&ed), ["• a.png"]);
    }

    #[test]
    fn an_empty_output_asks_for_nothing() {
        let out = ChromeOutput::default();
        assert!(out.is_empty());
        let mut out = out;
        out.actions.push(Action::ZoomFit);
        assert!(!out.is_empty());
    }

    #[test]
    fn a_frame_in_which_nothing_was_clicked_asks_for_nothing() {
        // The defect this pins: `select_layer` used to mirror the active layer
        // into the output every frame, so `is_empty()` was false whenever a
        // document was open and the shell re-applied a stale selection over
        // whatever the same frame's actions had just done.
        let dir = tempfile::tempdir().unwrap();
        let p = png(dir.path(), "a.png");
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&p).unwrap();
        assert!(ed.active().unwrap().document.active_layer().is_some());

        let out = run_chrome(&ed, None);
        assert_eq!(out.select_layer, None, "nothing was clicked: {out:?}");
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn clicking_a_layer_row_is_the_only_thing_that_selects_a_layer() {
        let dir = tempfile::tempdir().unwrap();
        let p = png(dir.path(), "a.png");
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&p).unwrap();
        ed.dispatch(Action::NewLayer).unwrap();
        let rows = layer_rows(
            &ed.active().unwrap().document,
            ed.active().unwrap().document.active_layer(),
        );
        assert_eq!(rows.len(), 2);
        // The eye of the *unselected* row is a stable target that sits on the
        // same line as its list row.
        let other = rows.iter().find(|r| !r.selected).unwrap();
        let out = run_chrome(&ed, Some(ids::layer_eye(other.id)));
        assert_eq!(
            out.commands,
            vec![toggle_visibility_command(other)],
            "the eye emits a visibility command and nothing else"
        );
        assert_eq!(out.select_layer, None, "and does not move the selection");
    }

    #[test]
    fn the_add_button_takes_the_same_route_as_the_menu_and_activates_what_it_made() {
        // The defect: the "+" emitted a bare `Command::CreateLayer`, and
        // `Command` never touches the active layer — while `Action::NewLayer`
        // explicitly does. So after a click on "+" the selection stayed on the
        // old layer, and the "−" beside it then deleted a *different* layer
        // than the one just created.
        let dir = tempfile::tempdir().unwrap();
        let p = png(dir.path(), "a.png");
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&p).unwrap();
        let original = ed.active().unwrap().document.active_layer().unwrap();

        let out = run_chrome(&ed, Some(egui::Id::new(ids::ADD_LAYER)));
        assert_eq!(out.actions, vec![Action::NewLayer], "{out:?}");
        assert!(out.commands.is_empty(), "{out:?}");

        for action in out.actions {
            ed.dispatch(action).unwrap();
        }
        let doc = &ed.active().unwrap().document;
        assert_eq!(doc.layers.len(), 2);
        let created = doc.active_layer().expect("something is active");
        assert_ne!(created, original, "the new layer is the one to paint on");

        // ...and the very next "−" click therefore targets the new layer.
        let out = run_chrome(&ed, Some(egui::Id::new(ids::DELETE_LAYER)));
        assert_eq!(out.actions, vec![Action::DeleteLayer], "{out:?}");
        for action in out.actions {
            ed.dispatch(action).unwrap();
        }
        let doc = &ed.active().unwrap().document;
        assert_eq!(doc.layers.len(), 1);
        assert!(
            doc.layers.get(created).is_none(),
            "the layer “+” created is the one “−” removed"
        );
        assert!(doc.layers.get(original).is_some(), "and only that one");
    }

    #[test]
    fn a_new_layer_never_repeats_a_name_that_is_already_taken() {
        // Both routes to "add a layer" now share one naming rule, so this holds
        // whichever one made the layer.
        let dir = tempfile::tempdir().unwrap();
        let p = png(dir.path(), "a.png");
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&p).unwrap();
        for _ in 0..3 {
            ed.dispatch(Action::NewLayer).unwrap();
        }
        let doc = &ed.active().unwrap().document;
        let mut names: Vec<String> = doc
            .layers
            .iter_depth_first()
            .into_iter()
            .filter_map(|id| doc.layers.get(id).map(|l| l.name.clone()))
            .collect();
        let before = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), before, "two layers share a name: {names:?}");
    }

    #[test]
    fn the_delete_button_greys_out_without_a_layer_to_delete() {
        let dir = tempfile::tempdir().unwrap();
        let p = png(dir.path(), "a.png");
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&p).unwrap();

        // With the last layer gone the button is disabled, so a click on it
        // emits nothing at all.
        ed.dispatch(Action::DeleteLayer).unwrap();
        assert_eq!(ed.active().unwrap().document.layers.len(), 0);
        let out = run_chrome(&ed, Some(egui::Id::new(ids::DELETE_LAYER)));
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn the_layers_dock_shows_layers_nested_inside_groups() {
        // The Wave-0 panel walked `layers.root()` only, so a grouped layer
        // simply vanished from the dock.
        let mut doc = Document::new(32, 32, "d");
        let group = Layer::group("Group");
        let group_id = group.id;
        let child = Layer::raster("Inside");
        let child_id = child.id;
        doc.layers.push_root(group).unwrap();
        doc.layers.insert_at(child, Some(group_id), 0).unwrap();

        let rows = layer_rows(&doc, Some(child_id));
        let ids: Vec<LayerId> = rows.iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![group_id, child_id]);
        assert_eq!(rows[0].depth, 0);
        assert_eq!(rows[1].depth, 1, "a group child is indented, not hidden");
        assert!(rows[0].is_group);
        assert!(rows[1].selected);
    }

    /// A document with `steps` layers added, as one open tab.
    fn doc_with_history(steps: usize) -> OpenDocument {
        let imported = crate::import::document_from_image(
            &crate::import::DecodedImage {
                width: 8,
                height: 8,
                rgba8: vec![4u8; 8 * 8 * 4],
            },
            "d.png",
            100,
        )
        .unwrap();
        let mut doc = OpenDocument::from_import(crate::doc::DocumentId(1), imported);
        for i in 0..steps {
            doc.apply(Command::create_layer(Layer::raster(format!("step {i}"))))
                .unwrap();
        }
        doc
    }

    #[test]
    fn history_rows_are_the_whole_timeline_with_a_place_to_stand_on_each() {
        let fresh = doc_with_history(0);
        let rows = history_rows(&fresh);
        assert_eq!(rows.len(), 1, "the state before anything happened");
        assert_eq!(rows[0].depth, 0);
        assert!(rows[0].current);

        let mut doc = doc_with_history(2);
        let rows = history_rows(&doc);
        assert_eq!(rows.len(), 3, "Original + two steps");
        assert_eq!(
            rows.iter().map(|r| r.depth).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert!(rows[2].current, "the newest step is where we stand");
        assert!(rows.iter().all(|r| !r.label.is_empty()));
        assert!(rows.iter().all(|r| !r.undone));

        // An undone step stays in the list rather than vanishing, so it can be
        // clicked back — `History` has no label for its redo stack, which is
        // why `OpenDocument` keeps one.
        doc.undo().unwrap();
        let rows = history_rows(&doc);
        assert_eq!(rows.len(), 3, "{rows:?}");
        assert!(rows[1].current);
        assert!(rows[2].undone, "and is marked as ahead of us");
        assert_eq!(
            rows[2].label,
            Command::create_layer(Layer::raster("step 1")).label(),
            "an undone step keeps the label its command had"
        );

        // Redoing puts it back where it was.
        doc.redo().unwrap();
        let rows = history_rows(&doc);
        assert!(rows[2].current);
        assert!(!rows[2].undone);
    }

    #[test]
    fn clicking_a_history_row_asks_to_walk_to_that_step() {
        // The defect: every row was drawn with `design::list_row`, whose
        // response was thrown away. The rows highlighted under the pointer,
        // advertised themselves as clickable, and did nothing whatsoever.
        let dir = tempfile::tempdir().unwrap();
        let p = png(dir.path(), "a.png");
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&p).unwrap();
        for _ in 0..3 {
            ed.dispatch(Action::NewLayer).unwrap();
        }
        let rows = history_rows(ed.active().unwrap());
        assert_eq!(rows.len(), 4, "Original + three layers: {rows:?}");

        // Row 1 = "the state after the first command".
        let out = run_chrome(&ed, Some(ids::history_row(1)));
        assert_eq!(out.history_jump, Some(1), "{out:?}");

        // ...and performing it really moves the document there.
        let moved = ed.jump_history(out.history_jump.unwrap());
        assert_eq!(moved, 2, "two steps undone");
        assert_eq!(ed.active().unwrap().history_depth(), 1);
        assert_eq!(ed.active().unwrap().document.layers.len(), 2);

        // The row we are standing on is not a control: it senses hover only,
        // so clicking it asks for nothing.
        let out = run_chrome(&ed, Some(ids::history_row(1)));
        assert_eq!(out.history_jump, None, "{out:?}");
        assert!(out.is_empty(), "{out:?}");

        // A row ahead of us walks forward again, through History's redo.
        let out = run_chrome(&ed, Some(ids::history_row(3)));
        assert_eq!(out.history_jump, Some(3), "{out:?}");
        assert_eq!(ed.jump_history(3), 2);
        assert_eq!(ed.active().unwrap().document.layers.len(), 4);
    }

    #[test]
    fn a_history_jump_to_where_we_already_are_moves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let p = png(dir.path(), "a.png");
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&p).unwrap();
        ed.dispatch(Action::NewLayer).unwrap();
        let before = ed.revision();
        assert_eq!(ed.jump_history(1), 0);
        assert_eq!(ed.revision(), before, "nothing happened, nothing changed");
        // And a target beyond the timeline stops rather than spinning.
        assert_eq!(ed.jump_history(99), 0);
        assert_eq!(ed.active().unwrap().history_depth(), 1);
    }

    #[test]
    fn the_colour_wells_show_what_the_editor_holds_and_swap_emits_the_action() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(&dir.path().join("config"));
        ed.set_foreground([1.0, 0.0, 0.0, 1.0]);
        ed.set_background([0.0, 0.0, 1.0, 1.0]);
        let (fg, bg) = color_wells(&ed);
        assert_eq!(fg, egui::Color32::from_rgb(255, 0, 0));
        assert_eq!(bg, egui::Color32::from_rgb(0, 0, 255));

        let out = run_chrome(&ed, Some(egui::Id::new(ids::SWAP_COLORS)));
        assert_eq!(out.actions, vec![Action::SwapColors], "{out:?}");
        let out = run_chrome(&ed, Some(egui::Id::new(ids::RESET_COLORS)));
        assert_eq!(out.actions, vec![Action::ResetColors], "{out:?}");

        // ...and performing the action moves what the wells will show.
        ed.dispatch(Action::SwapColors).unwrap();
        assert_eq!(color_wells(&ed).0, egui::Color32::from_rgb(0, 0, 255));
    }

    #[test]
    fn an_egui_key_press_becomes_a_chord_the_keymap_can_hold() {
        let ctrl = egui::Modifiers {
            ctrl: true,
            ..Default::default()
        };
        assert_eq!(
            chord_from_egui(egui::Key::S, ctrl),
            Some(Chord::ctrl(Key::character('s')))
        );
        assert_eq!(
            chord_from_egui(egui::Key::Tab, egui::Modifiers::default()),
            Some(Chord::plain(Key::Tab))
        );
        assert_eq!(
            chord_from_egui(egui::Key::Num5, egui::Modifiers::default()),
            Some(Chord::plain(Key::Char('5')))
        );
        assert_eq!(
            chord_from_egui(egui::Key::F7, egui::Modifiers::default()),
            Some(Chord::plain(Key::Function(7)))
        );
        assert_eq!(
            chord_from_egui(egui::Key::OpenBracket, egui::Modifiers::default()),
            Some(Chord::plain(Key::Char('[')))
        );
        // Everything the recorder produces must survive the text round trip the
        // preferences file uses.
        for key in [egui::Key::A, egui::Key::Minus, egui::Key::Delete] {
            let chord = chord_from_egui(key, ctrl).unwrap();
            assert_eq!(chord.to_string().parse::<Chord>().unwrap(), chord);
        }
        // A key the keymap has no spelling for records nothing rather than
        // storing a chord that can never match.
        assert_eq!(chord_from_egui(egui::Key::Home, ctrl), None);
        assert_eq!(chord_from_egui(egui::Key::Copy, ctrl), None);
    }

    #[test]
    fn the_shortcut_editor_lists_every_action_with_its_chord() {
        let dir = tempfile::tempdir().unwrap();
        let ed = editor(&dir.path().join("config"));
        let rows = shortcut_rows(&ed);
        assert_eq!(rows.len(), Action::all().len());
        for row in &rows {
            assert!(!row.label.is_empty());
            assert!(
                row.chord.is_some(),
                "{} is listed with no chord",
                row.action.id()
            );
        }
        let save = rows.iter().find(|r| r.action == Action::Save).unwrap();
        assert_eq!(save.chord.unwrap().to_string(), "Ctrl+S");
    }

    #[test]
    fn the_named_text_styles_survive_the_platform_switching_theme() {
        // egui keeps a dark style and a light style and swaps between them when
        // the platform reports a system theme. `set_style` writes only the
        // active slot, so the design type scale's *named* styles were missing
        // from the other one — and `TextStyle::resolve` panics on a missing
        // name rather than falling back. A light-mode desktop crashed on the
        // first status-bar label.
        let footnote = design::egui_theme::text_style(TypeRole::Footnote);
        for theme in design::Theme::ALL {
            let ctx = egui::Context::default();
            install_theme(&ctx, *theme);
            assert_eq!(design::current_theme(&ctx), *theme);
            for egui_theme in [egui::Theme::Dark, egui::Theme::Light] {
                ctx.set_theme(egui::ThemePreference::from(egui_theme));
                assert!(
                    ctx.style().text_styles.contains_key(&footnote),
                    "{theme:?} loses the footnote style when egui is {egui_theme:?}"
                );
            }
        }

        // A plain `apply_theme` is what does not survive it: this is the
        // regression, spelled out.
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        ctx.set_theme(egui::ThemePreference::Light);
        assert!(
            !ctx.style().text_styles.contains_key(&footnote),
            "if this passes, egui no longer keeps per-theme styles and              `install_theme` can go back to a single `apply_theme`"
        );
    }

    #[test]
    fn the_tool_strip_is_wide_enough_for_its_buttons() {
        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let tokens = design::Theme::Dark.tokens();
        assert!(tool_strip_width(&ctx) >= tokens.metrics.toolbar_button);
        // ...and it is still on the 4pt grid.
        let extra = tool_strip_width(&ctx) - tokens.metrics.toolbar_button;
        assert_eq!(extra % design::UNIT_PT, 0.0, "off-grid by {extra}");
    }

    #[test]
    fn the_docks_are_on_the_grid_in_both_themes() {
        for theme in design::Theme::ALL {
            let ctx = egui::Context::default();
            install_theme(&ctx, *theme);
            let width = dock_width(&ctx);
            assert!(width > 0.0);
            assert_eq!(width % design::UNIT_PT, 0.0, "{theme:?} off-grid: {width}");
        }
    }

    #[test]
    fn every_surface_draws_in_both_themes_without_panicking() {
        // The docks and the preferences window are new surfaces; a missing
        // named text style or a bad layout shows up here rather than on a
        // user's screen.
        let dir = tempfile::tempdir().unwrap();
        let p = png(dir.path(), "a.png");
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&p).unwrap();
        ed.dispatch(Action::NewLayer).unwrap();
        ed.dispatch(Action::ShowPreferences).unwrap();
        assert!(ed.preferences_open());
        for theme in design::Theme::ALL {
            let ctx = egui::Context::default();
            install_theme(&ctx, *theme);
            let mut chrome = Chrome::new();
            for _ in 0..2 {
                let _ = ctx.run(raw_input(Vec::new()), |ctx| {
                    let _ = chrome.ui(ctx, &ed);
                });
            }
        }
    }
}
