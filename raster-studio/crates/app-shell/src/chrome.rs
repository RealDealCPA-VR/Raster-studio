//! The application chrome: the document tabs, the preferences window, the
//! status strip — and the seam that hands everything else to `ui::Workspace`.
//!
//! # One chrome, not two
//!
//! This file used to draw a second menu bar, tool palette, layers dock, history
//! dock and colour-well pair beside the ones the `ui` crate publishes. Nothing
//! in the binary reached `ui::Workspace`, so the thirteen docked panels, the
//! tool fly-outs, the options bar, the Navigator, the Channels panel and the
//! workspace layouts existed only in that crate's own tests.
//!
//! [`Chrome`] now **owns** a [`ui::Workspace`] and draws it:
//!
//! * the menu bar through [`crate::menu_bridge`], which paints
//!   `ui::menu::menu_bar` and gates each item on what this build can perform;
//! * the tool palette through [`ui::view::tool_palette`];
//! * the options bar through [`ui::view::tool_options`];
//! * every docked panel through [`ui::view::docks`].
//!
//! What is left here is what the `ui` crate has no model for: the document tab
//! strip (that crate knows one document, not a set of them), the preferences and
//! shortcut editor, and the status strip — which carries the shell's transient
//! message ("Opened C:\…\photo.png"), a string [`ui::StatusBar`] has no field
//! for. Its *readouts* are `ui::StatusBar`'s, so the zoom, size, colour mode and
//! tool name are formatted once for the whole application.
//!
//! # It is still a view
//!
//! [`Chrome::ui`] takes `&Editor`, never `&mut Editor`. Everything the user asks
//! for comes back as a [`ChromeOutput`] the shell then performs, so the UI
//! cannot mutate a document behind history's back — and so the whole of "what
//! did that click mean" is a value a test can inspect. The workspace's own
//! intents go through the same door: [`ui::Workspace::drain_intents`] is
//! translated by [`crate::menu_bridge::pick`], exactly as a menu click is.
//!
//! A field of [`ChromeOutput`] is set **only when the user did something this
//! frame**. Mirroring current state into it (which `select_layer` used to do)
//! turns every frame into a replay of the state the frame started with: an
//! action performed in the same frame is then immediately undone by the mirror
//! that was captured before it. See `a_new_layer_stays_active_when_the_menu_
//! creates_it`.
//!
//! # The editor is the source of truth, once per frame
//!
//! The workspace keeps its own copy of the things a panel has to draw — the
//! active tool, the two colour wells, the zoom, the recent files. Those belong
//! to the [`Editor`], so [`Chrome::sync_workspace`] pushes them in before the
//! frame is drawn and the intents the frame produced are what push back. One
//! direction each way; nothing is authoritative in two places.
//!
//! # It names no colours
//!
//! Every colour, radius, gap and text size *this module chooses* comes from
//! `design`. There is no literal `Color32` and no bare pixel gap anywhere below.

use std::path::PathBuf;

use design::{ColorRole, Space, SurfaceRole, TextRole, TypeRole};
use editor_core::Command;
use layer_model::LayerId;
use tools::ToolId;

use crate::action::Action;
use crate::editor::Editor;
use crate::keymap::{Chord, Key};
use crate::prefs::{Preferences, ThemeChoice};

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
    /// A control asked for a zoom level — the status bar's field, the
    /// Navigator's slider — as a scale factor.
    pub set_zoom: Option<f32>,
    /// The Navigator was panned: the camera's new centre, in image pixels.
    pub set_view_center: Option<(f32, f32)>,
    /// Intents whose whole effect is on the workspace — panel visibility, the
    /// dock layout, view overlays, channel isolation, tool options.
    ///
    /// [`Chrome::ui`] has already absorbed these into the workspace it owns by
    /// the time the shell sees them; they are reported so a test can read what
    /// a click meant, and so the shell can repaint knowing something moved.
    pub workspace: Vec<ui::Intent>,
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
    /// The options bar edited a brush parameter. [`crate::Editor`] owns the
    /// brush, so the edit travels back out to it rather than living on in the
    /// workspace as a second, disagreeing copy.
    pub set_brush: Option<tools::BrushSettings>,
}

/// The option keys that make up a [`tools::BrushSettings`].
///
/// Kept beside [`push_brush`] so the two cannot drift: a key written out but
/// never read back — or the reverse — is how the two copies disagreed before.
const BRUSH_KEYS: &[&str] = &[
    "size",
    "hardness",
    "spacing",
    "angle",
    "roundness",
    "opacity",
    "flow",
    "smoothing",
    "size_pressure",
    "flow_pressure",
];

/// Whether an intent could have changed the active tool's brush.
fn touches_brush(intent: &ui::Intent) -> bool {
    match intent {
        ui::Intent::SetToolOption { key, .. } => BRUSH_KEYS.contains(key),
        ui::Intent::ResetToolOptions { .. } => true,
        _ => false,
    }
}

/// Write `brush` into `w`'s options for `tool`.
///
/// Only keys the tool's schema actually declares are set, so a tool exposing
/// just `size` is not given a hardness slider it never had.
fn push_brush(w: &mut ui::Workspace, tool: tools::ToolId, brush: &tools::BrushSettings) {
    use ui::OptionValue;
    let pairs: [(&str, OptionValue); 10] = [
        ("size", OptionValue::Float(brush.size)),
        ("hardness", OptionValue::Float(brush.hardness)),
        ("spacing", OptionValue::Float(brush.spacing)),
        ("angle", OptionValue::Float(brush.angle)),
        ("roundness", OptionValue::Float(brush.roundness)),
        ("opacity", OptionValue::Float(brush.opacity)),
        ("flow", OptionValue::Float(brush.flow)),
        ("smoothing", OptionValue::Float(brush.smoothing)),
        ("size_pressure", OptionValue::Bool(brush.size_pressure)),
        ("flow_pressure", OptionValue::Bool(brush.flow_pressure)),
    ];
    for (key, value) in pairs {
        w.options.set(tool, key, value);
    }
}

impl ChromeOutput {
    pub fn is_empty(&self) -> bool {
        *self == ChromeOutput::default()
    }
}

/// The label for one document tab, with a bullet while it has unsaved changes.
pub fn tab_labels(editor: &Editor) -> Vec<String> {
    editor.documents().iter().map(|d| d.tab_label()).collect()
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

/// The chrome's view state: the whole `ui` workspace, plus "which row of the
/// shortcut editor is listening for a key press".
///
/// The workspace is *owned* here rather than by the shell because it is view
/// state — which panels are open, where they are docked, which channel is
/// isolated, what the gradient ramp looks like — and none of it belongs in a
/// document or in the editor.
#[derive(Default)]
pub struct Chrome {
    /// The action whose shortcut is being recorded, if any.
    capturing: Option<Action>,
    /// The `ui` crate's workspace: the dock, the panels, the tool palette's
    /// fly-outs, the tool options, the view overlays.
    workspace: ui::Workspace,
}

impl Chrome {
    pub fn new() -> Self {
        Self::default()
    }

    /// The action currently listening for a chord, for tests and for the shell.
    pub fn capturing(&self) -> Option<Action> {
        self.capturing
    }

    /// The workspace this chrome draws, for tests and for the shell's own
    /// read-back of view state.
    pub fn workspace(&self) -> &ui::Workspace {
        &self.workspace
    }

    /// Which colour components the canvas should show, as the Channels panel
    /// currently says.
    ///
    /// Channel isolation is a *view* setting, so it is not in the document and
    /// not in the [`Editor`]: the panel owns it, this chrome owns the panel,
    /// and [`crate::presenter::CanvasPresenter`] applies it on the composite's
    /// way to the GPU. `hiding_a_channel_in_the_panel_changes_what_the_canvas_
    /// is_asked_to_show` drives the real panel and reads this back.
    pub fn channel_mask(&self) -> crate::presenter::ChannelMask {
        crate::presenter::ChannelMask::from_channels(&self.workspace.channels)
    }

    /// Draw one frame of chrome.
    pub fn ui(&mut self, ctx: &egui::Context, editor: &Editor) -> ChromeOutput {
        let mut out = ChromeOutput::default();
        self.sync_workspace(editor);

        // Order matters, and it is `ui::Workspace::ui`'s: egui gives each panel
        // what the previously added ones left, so the full-width strips — menu,
        // tabs, options, status — are claimed before the vertical tool rail and
        // the docks, and the canvas gets the rectangle in the middle.
        self.menu_bar(ctx, editor, &mut out);
        if editor.documents().len() > 1 || editor.panels_visible() {
            self.tab_strip(ctx, editor, &mut out);
        }
        // The `ui` crate's own surfaces, driven from the workspace this chrome
        // owns. Every control in them posts an intent, which `harvest`
        // translates below.
        if editor.panels_visible() {
            ui::view::tool_options(&mut self.workspace, ctx);
        }
        self.status_bar(ctx, editor);
        if editor.panels_visible() {
            ui::view::tool_palette(&mut self.workspace, ctx);
            if let Some(open) = editor.active() {
                ui::view::docks(&mut self.workspace, ctx, &open.document, &open.history);
            }
        }
        if editor.preferences_open() {
            self.preferences_window(ctx, editor, &mut out);
        } else {
            self.capturing = None;
        }
        // Read *after* the chrome is drawn: this is the room the image actually
        // has once every panel has taken its share, and it is what the
        // Navigator's rectangle and Fit on Screen are computed against.
        self.record_viewport(ctx);
        self.channel_chords(ctx, editor);
        self.harvest(editor, &mut out);
        out
    }

    /// Push the editor's state into the workspace, once, before the frame.
    ///
    /// These are the values a panel draws that the [`Editor`] owns. Without
    /// this the Layers panel would show the workspace's idea of the active
    /// tool, the Colour panel its own wells, and the Navigator a zoom that no
    /// camera ever had.
    fn sync_workspace(&mut self, editor: &Editor) {
        let w = &mut self.workspace;
        w.theme = editor.preferences().theme.resolve(design::Theme::Dark);
        w.recent = editor
            .recent()
            .entries()
            .iter()
            .map(|p| {
                p.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| p.display().to_string())
            })
            .collect();
        let tool = editor.effective_tool();
        w.palette.activate(&ui::PaletteModel::build(), tool);
        w.status.tool = Some(tool);
        // The brush is [`Editor`]'s. Push it into the options bar every frame
        // so `[` and `]` move the slider the user is looking at — without this
        // the options bar and the status bar show different sizes in the same
        // window. The reverse direction is `ChromeOutput::set_brush`.
        push_brush(w, tool, editor.brush());
        w.color.set_well(
            ui::panels::color::ColorWell::Foreground,
            editor.foreground(),
        );
        w.color.set_well(
            ui::panels::color::ColorWell::Background,
            editor.background(),
        );
        if let Some(open) = editor.active() {
            w.status.zoom = open.camera.zoom;
            w.view_center = (open.camera.center.x, open.camera.center.y);
            w.prune(&open.document);
        }
    }

    /// Remember how much room the canvas has once the docks have taken theirs.
    fn record_viewport(&mut self, ctx: &egui::Context) {
        let rect = ctx.available_rect();
        let (w, h) = (rect.width(), rect.height());
        if w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0 {
            self.workspace.viewport = (w, h);
        }
    }

    /// `Ctrl+2`…`Ctrl+9`: isolate the channel the Channels panel prints that
    /// chord beside.
    ///
    /// # Why this is not `ui::Workspace::handle_keys`
    ///
    /// That function runs the *whole* `ui` shortcut table, and this application
    /// already has one: [`crate::keymap::Keymap`], routed from winit through
    /// [`crate::shell::Shell::on_key`]. Running both would perform Ctrl+Z
    /// twice. So only the chords the panel paints — and only those the
    /// application's own keymap does not claim — are read here, from the same
    /// [`ui::keys::channel_for_key`] table the hint is derived from. A chord
    /// hint painted beside a control is a promise, and until this existed it
    /// was a promise only the `ui` crate's own tests saw kept.
    fn channel_chords(&mut self, ctx: &egui::Context, editor: &Editor) {
        // Typing "3" into a layer name must not isolate the red channel.
        if ctx.wants_keyboard_input() {
            return;
        }
        let Some(open) = editor.active() else { return };
        let presses: Vec<(egui::Key, egui::Modifiers)> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        repeat: false,
                        ..
                    } => Some((*key, *modifiers)),
                    _ => None,
                })
                .collect()
        });
        for (key, modifiers) in presses {
            // The application's keymap wins: `Ctrl+0` and `Ctrl+1` are its zoom
            // commands, and a user who rebinds `Ctrl+3` gets what they bound.
            if chord_from_egui(key, modifiers)
                .and_then(|chord| editor.keymap().resolve(&chord))
                .is_some()
            {
                continue;
            }
            if let Some(channel) =
                ui::keys::channel_for_key(key, modifiers, &open.document, &self.workspace.channels)
            {
                self.workspace
                    .channels
                    .isolate(&open.document.meta.color_space, channel);
                self.workspace.emit(ui::Intent::SelectChannel(channel));
            }
        }
    }

    /// Translate what the workspace's controls asked for into the frame's
    /// output, and absorb the part of it that is the workspace's own.
    ///
    /// The same [`crate::menu_bridge::pick`] the menu bar goes through, so a
    /// panel button and the menu item beside it cannot mean two different
    /// things. An intent this build has no answer for is dropped here rather
    /// than silently half-applied — the menu bar's equivalent is the item that
    /// greys out carrying [`crate::menu_bridge::NOT_WIRED`].
    /// [`Chrome::harvest`], reachable from this crate's tests.
    #[cfg(test)]
    fn harvest_workspace_for_test(&mut self, out: &mut ChromeOutput, editor: &Editor) {
        self.harvest(editor, out);
    }

    fn harvest(&mut self, editor: &Editor, out: &mut ChromeOutput) {
        for intent in self.workspace.drain_intents() {
            if let Some(pick) = crate::menu_bridge::pick(&intent, editor) {
                crate::menu_bridge::record(pick, out);
            }
        }
        // Workspace-local intents are performed by the thing that owns the
        // state — this chrome — rather than travelling out to the shell and
        // back. They stay in the output so a test can read what a click meant.
        //
        // This is the *second* application for anything a drawn control raised:
        // `ui::view::docks` moves the panel as the header control is clicked
        // and then emits. That is safe only because every intent
        // `menu_bridge::pick` routes to `Pick::Workspace` is an absolute set,
        // which `ui::Intent` states as an invariant and
        // `every_workspace_intent_is_idempotent_under_absorb` enforces. It was
        // not always true: `ReorderPanel` carried a direction, so one click on
        // the ▲ moved the panel two places — see
        // `the_header_reorder_control_moves_a_panel_exactly_one_place`.
        for intent in &out.workspace {
            self.workspace.absorb(intent);
        }
        // The other half of the brush's single source of truth: an options-bar
        // edit is absorbed above, so read the result back and hand it to the
        // shell for `Editor::set_brush`.
        if out.workspace.iter().any(touches_brush) {
            let tool = editor.effective_tool();
            out.set_brush = Some(self.workspace.options.brush_settings(tool));
        }
    }

    /// The menu bar, drawn by [`crate::menu_bridge`] from `ui::menu::menu_bar`.
    ///
    /// There is deliberately no menu structure in this file any more. The nine
    /// menus, their labels, their shortcut hints and their enablement all come
    /// from the shared model in the `ui` crate; the bridge is the one place
    /// that says which of them this build can actually perform.
    fn menu_bar(&self, ctx: &egui::Context, editor: &Editor, out: &mut ChromeOutput) {
        crate::menu_bridge::draw(ctx, editor, &self.workspace, out);
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

    /// The status strip.
    ///
    /// Its readouts are `ui::StatusBar`'s — the zoom, the size, the colour mode
    /// and the memory figure are formatted by the shared model, so the strip
    /// and the panels showing the same number cannot disagree about how it is
    /// written. What is drawn here rather than by `ui::view::status_bar` is the
    /// **transient message**: "Opened C:\…\photo.png", "Restored 2
    /// document(s)", the reason an action refused. `ui::StatusBar` has no field
    /// for that string, and dropping it would take the only report a user gets
    /// of half the shell's work off the screen.
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
                            for field in self.workspace.status.fields(&doc.document) {
                                ui.colored_label(dim, field.value);
                            }
                        }
                        None => {
                            ui.colored_label(dim, "No document");
                        }
                    }
                    ui.colored_label(dim, self.workspace.status.tool_hint());
                    ui.colored_label(dim, format!("{} px", editor.brush().size as i32));
                    if let Some(status) = editor.status() {
                        // Laid out and placed by hand, because this is the one
                        // label whose length the application does not control:
                        // it is routinely a whole file path ("Opened
                        // C:\…\photo.png"). egui does not clip a label to
                        // the space it was given, so the message was painted
                        // straight across the tool name, the brush size and the
                        // layer count — the right-hand end of the bar was two
                        // sentences on top of each other.
                        //
                        // Elided to the room that is left and right-aligned
                        // inside exactly that room, so "it cannot cover its
                        // neighbours" is true by construction rather than by
                        // hoping a layout does the right thing.
                        let room = ui.available_width();
                        let (rect, _) = ui.allocate_exact_size(
                            egui::vec2(room, ui.spacing().interact_size.y),
                            egui::Sense::hover(),
                        );
                        let mut job = egui::text::LayoutJob::single_section(
                            status.to_string(),
                            egui::TextFormat {
                                font_id: egui::TextStyle::Body.resolve(ui.style()),
                                color: dim,
                                ..Default::default()
                            },
                        );
                        job.wrap = egui::text::TextWrapping::truncate_at_width(room);
                        let galley = ui.painter().layout_job(job);
                        let size = galley.size();
                        ui.painter().galley(
                            egui::pos2(rect.right() - size.x, rect.center().y - size.y * 0.5),
                            galley,
                            dim,
                        );
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialogs::ScriptedDialogs;
    use crate::prefs::{AppPaths, Preferences};
    use crate::recent::RecentFiles;

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

    /// Every string one drawn frame painted, with the rectangle it occupies.
    ///
    /// `FullOutput::shapes` is pre-tessellation, so a text shape still carries
    /// its galley — which knows both its text and its size. That is what lets a
    /// headless test assert on *where* the window put something, not only that
    /// it was drawn.
    fn painted_text(editor: &Editor) -> Vec<(String, egui::Rect)> {
        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut chrome = Chrome::new();
        let mut painted = Vec::new();
        // Two passes: the first frame is where egui learns the sizes.
        for _ in 0..2 {
            let output = ctx.run(raw_input(Vec::new()), |ctx| {
                chrome.ui(ctx, editor);
            });
            painted = output
                .shapes
                .iter()
                .filter_map(|clipped| match &clipped.shape {
                    egui::Shape::Text(text) => Some((
                        text.galley.text().to_string(),
                        egui::Rect::from_min_size(text.pos, text.galley.size()),
                    )),
                    _ => None,
                })
                .collect();
        }
        painted
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
    fn a_long_status_message_does_not_paint_over_the_rest_of_the_status_bar() {
        // Found by running the application: opening a file put "Opened
        // C:\…\big.png" in the status bar, and because that label is drawn
        // right-to-left from the panel's right edge and egui does not clip a
        // label to the space it was given, it grew leftwards straight across
        // the zoom, the layer count, the tool name and the brush size. The
        // whole right half of the bar was two sentences on top of each other.
        let dir = tempfile::tempdir().unwrap();
        let p = png(dir.path(), "a.png");
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&p).unwrap();
        // Longer than the window is wide, which is the whole point: the
        // message is a file path and paths are as long as the user's folders.
        ed.set_status(format!(
            "Opened C:{}\\photograph-of-the-whole-family-at-the-beach.png",
            "\\a directory with a long name".repeat(12)
        ));

        let painted = painted_text(&ed);
        // The status bar is the bottom-most panel of the window.
        let row: Vec<&(String, egui::Rect)> = painted
            .iter()
            .filter(|(_, r)| r.center().y > 900.0 - 40.0)
            .collect();
        assert!(
            row.len() >= 4,
            "the status bar drew {row:?}, so this test is not looking at it"
        );
        assert!(
            row.iter().any(|(t, _)| t.starts_with("Opened ")),
            "the status message is not in the row being checked: {row:?}"
        );

        for (i, (a, ra)) in row.iter().enumerate() {
            // Nothing may be painted outside the window either: a label egui
            // was never asked to elide runs off the edge instead, and whatever
            // is still on screen sits on top of its neighbours.
            assert!(
                ra.left() >= 0.0 && ra.right() <= 1400.0,
                "“{a}” is painted outside the window: {ra:?}"
            );
            for (b, rb) in row.iter().skip(i + 1) {
                // Half a pixel of slack: adjacent labels are separated by real
                // spacing, so anything that overlaps does so by a lot.
                let a_box = ra.shrink2(egui::vec2(0.5, 0.0));
                assert!(
                    !a_box.intersects(rb.shrink2(egui::vec2(0.5, 0.0))),
                    "“{a}” and “{b}” are painted on top of each other: {ra:?} vs {rb:?}"
                );
            }
        }
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
        // The row and the eye are the `ui` crate's, found by the id
        // `ui::view::ids` publishes for them: this is the shipped panel being
        // clicked, not a model being asked a question.
        let dir = tempfile::tempdir().unwrap();
        let p = png(dir.path(), "a.png");
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&p).unwrap();
        ed.dispatch(Action::NewLayer).unwrap();
        let doc = &ed.active().unwrap().document;
        let active = doc.active_layer().unwrap();
        let other = doc
            .layers
            .iter_depth_first()
            .into_iter()
            .find(|id| *id != active)
            .expect("two layers");

        let out = run_chrome(&ed, Some(ui::view::ids::layer_eye(other)));
        assert_eq!(out.commands.len(), 1, "the eye emits one command: {out:?}");
        assert!(
            matches!(
                &out.commands[0],
                Command::SetLayerProperties { layer_id, patch }
                    if *layer_id == other && patch.visible == Some(false)
            ),
            "the eye emits a visibility command: {out:?}"
        );
        assert_eq!(out.select_layer, None, "and does not move the selection");

        // ...and clicking the row itself is what selects.
        let out = run_chrome(&ed, Some(ui::view::ids::layer_row(other)));
        assert_eq!(out.select_layer, Some(other), "{out:?}");
    }

    #[test]
    fn the_layers_panel_footer_adds_a_layer_through_history() {
        // `ui::view::ids::new_layer()` is the "+" the shipped Layers panel
        // draws. Before this wave that panel was never instantiated by the
        // binary, so this click had nowhere to land.
        let dir = tempfile::tempdir().unwrap();
        let p = png(dir.path(), "a.png");
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&p).unwrap();
        assert_eq!(ed.active().unwrap().document.layers.len(), 1);

        let out = run_chrome(&ed, Some(ui::view::ids::new_layer()));
        assert_eq!(out.commands.len(), 1, "{out:?}");
        assert!(out.actions.is_empty(), "{out:?}");

        for command in out.commands {
            ed.apply_command(command);
        }
        assert_eq!(
            ed.active().unwrap().document.layers.len(),
            2,
            "the panel's + really added a layer"
        );
        // ...and it went through history, so Ctrl+Z takes it back.
        assert_eq!(ed.active().unwrap().history_depth(), 1);
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
    fn clicking_a_history_row_asks_to_walk_to_that_step() {
        // The History panel is the `ui` crate's, and it counts *steps* from
        // where the document stands. `menu_bridge::pick` turns that into the
        // absolute depth `Editor::jump_history` walks to — the conversion is
        // the seam this pins.
        let dir = tempfile::tempdir().unwrap();
        let p = png(dir.path(), "a.png");
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&p).unwrap();
        for _ in 0..3 {
            ed.dispatch(Action::NewLayer).unwrap();
        }
        assert_eq!(ed.active().unwrap().history_depth(), 3);

        let out = run_chrome(&ed, Some(ui::view::ids::history_row(1)));
        assert_eq!(out.history_jump, Some(1), "{out:?}");

        // ...and performing it really moves the document there.
        let moved = ed.jump_history(out.history_jump.unwrap());
        assert_eq!(moved, 2, "two steps undone");
        assert_eq!(ed.active().unwrap().history_depth(), 1);
        assert_eq!(ed.active().unwrap().document.layers.len(), 2);

        // A row ahead of us walks forward again, through History's redo.
        let out = run_chrome(&ed, Some(ui::view::ids::history_row(3)));
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
    fn the_colour_wells_show_what_the_editor_holds() {
        // The wells are the Colour panel's now, and the panel reads them out of
        // the workspace — so the editor's colours have to reach the workspace
        // every frame or the swatches show whatever the `ui` crate happened to
        // default to. `sync_workspace` is that push, and this is what proves it
        // happens.
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(&dir.path().join("config"));
        ed.set_foreground([1.0, 0.0, 0.0, 1.0]);
        ed.set_background([0.0, 0.0, 1.0, 1.0]);

        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut chrome = Chrome::new();
        let _ = ctx.run(raw_input(Vec::new()), |ctx| {
            chrome.ui(ctx, &ed);
        });
        assert_eq!(chrome.workspace().color.foreground(), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(chrome.workspace().color.background(), [0.0, 0.0, 1.0, 1.0]);

        // ...and a colour the panel emits comes back out as a request the shell
        // performs, rather than being written straight into the workspace.
        ed.dispatch(Action::SwapColors).unwrap();
        let _ = ctx.run(raw_input(Vec::new()), |ctx| {
            chrome.ui(ctx, &ed);
        });
        assert_eq!(chrome.workspace().color.foreground(), [0.0, 0.0, 1.0, 1.0]);
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
    #[test]
    fn the_docked_panels_the_window_draws_are_the_ui_crates() {
        // Defect 1, pinned. `ui::Workspace`'s panels used to be unreachable
        // from the binary: this file drew its own layers and history docks and
        // nothing ever constructed `ui::view::docks`. What is asserted is not
        // "the bridge would return them" but "the window says them" — the panel
        // headers read back off one real frame's paint list, and the layer row
        // found by the id only the `ui` crate's panel registers.
        let dir = tempfile::tempdir().unwrap();
        let p = png(dir.path(), "a.png");
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&p).unwrap();
        let layer = ed.active().unwrap().document.active_layer().unwrap();

        let painted: Vec<String> = painted_text(&ed).into_iter().map(|(t, _)| t).collect();
        let dock = ui::DockState::default();
        let open: Vec<ui::PanelId> = ui::DockSide::ALL
            .iter()
            .flat_map(|side| dock.panels_on(*side))
            .collect();
        assert!(open.len() >= 5, "the default layout opens {open:?}");
        for panel in open {
            assert!(
                painted.iter().any(|t| t == panel.title()),
                "the window never drew the {} panel; it drew {painted:?}",
                panel.title()
            );
        }

        // The row a user clicks is the `ui` crate's row.
        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut chrome = Chrome::new();
        for _ in 0..2 {
            let _ = ctx.run(raw_input(Vec::new()), |ctx| {
                chrome.ui(ctx, &ed);
            });
        }
        assert!(
            ctx.read_response(ui::view::ids::layer_row(layer)).is_some(),
            "the Layers panel drawn is not ui::view::docks's"
        );
        assert!(
            ctx.read_response(ui::view::ids::tool_slot(0)).is_some(),
            "the tool palette drawn is not ui::view::toolbar's"
        );
    }

    #[test]
    fn a_panel_the_window_menu_opens_is_absorbed_by_the_chrome_that_owns_the_dock() {
        // The other half of Defect 1: an intent the workspace raises has to be
        // performed by something. It is performed here, because this is where
        // the dock lives — and it is still reported, so the shell knows the
        // frame changed something and a test can read what the click meant.
        let dir = tempfile::tempdir().unwrap();
        let p = png(dir.path(), "a.png");
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&p).unwrap();

        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut chrome = Chrome::new();
        let _ = ctx.run(raw_input(Vec::new()), |ctx| {
            chrome.ui(ctx, &ed);
        });
        assert!(!chrome.workspace().dock.is_open(ui::PanelId::Navigator));

        chrome.workspace.emit(ui::Intent::SetPanelOpen {
            panel: ui::PanelId::Navigator,
            open: true,
        });
        let mut out = ChromeOutput::default();
        let _ = ctx.run(raw_input(Vec::new()), |ctx| {
            out = chrome.ui(ctx, &ed);
        });
        assert_eq!(
            out.workspace,
            vec![ui::Intent::SetPanelOpen {
                panel: ui::PanelId::Navigator,
                open: true
            }],
            "{out:?}"
        );
        assert!(
            chrome.workspace().dock.is_open(ui::PanelId::Navigator),
            "the panel was reported but never opened"
        );

        // ...and the next frame really draws it.
        let painted: Vec<String> = painted_text_with(&ctx, &mut chrome, &ed);
        assert!(
            painted.iter().any(|t| t == ui::PanelId::Navigator.title()),
            "the Navigator never appeared: {painted:?}"
        );
    }

    /// Draw two more frames on an existing chrome and read back what they said.
    fn painted_text_with(ctx: &egui::Context, chrome: &mut Chrome, editor: &Editor) -> Vec<String> {
        let mut painted = Vec::new();
        for _ in 0..2 {
            let output = ctx.run(raw_input(Vec::new()), |ctx| {
                chrome.ui(ctx, editor);
            });
            painted = output
                .shapes
                .iter()
                .filter_map(|clipped| match &clipped.shape {
                    egui::Shape::Text(text) => Some(text.galley.text().to_string()),
                    _ => None,
                })
                .collect();
        }
        painted
    }

    #[test]
    fn the_navigators_pan_and_the_zoom_field_come_out_as_camera_moves() {
        // Both used to be workspace-local writes the reviewer measured as dead:
        // `view_center` was read by nobody outside the Navigator's own panel.
        // They are now requests the shell performs on the document's camera.
        let dir = tempfile::tempdir().unwrap();
        let p = png(dir.path(), "a.png");
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&p).unwrap();

        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut chrome = Chrome::new();
        let _ = ctx.run(raw_input(Vec::new()), |ctx| {
            chrome.ui(ctx, &ed);
        });
        chrome
            .workspace
            .emit(ui::Intent::SetViewCenter((12.0, 34.0)));
        chrome.workspace.emit(ui::Intent::SetZoom(2.5));
        let mut out = ChromeOutput::default();
        let _ = ctx.run(raw_input(Vec::new()), |ctx| {
            out = chrome.ui(ctx, &ed);
        });
        assert_eq!(out.set_view_center, Some((12.0, 34.0)), "{out:?}");
        assert_eq!(out.set_zoom, Some(2.5), "{out:?}");
    }

    /// One window, many clicks: the real [`Chrome`] driven across frames.
    ///
    /// `run_chrome` builds a fresh chrome per call and can click once, which is
    /// enough for a control that is always on screen. A docking gesture is two
    /// clicks — open the panel's "⋯" disclosure, then hit the control inside
    /// it — and the second one only exists because the first one landed.
    struct Window {
        ctx: egui::Context,
        chrome: Chrome,
    }

    impl Window {
        fn new(editor: &Editor) -> Self {
            let ctx = egui::Context::default();
            install_theme(&ctx, design::Theme::Dark);
            let mut window = Self {
                ctx,
                chrome: Chrome::new(),
            };
            window.settle(editor);
            window
        }

        /// Draw until the layout stops moving.
        ///
        /// A rail whose panels overflow grows a scroll bar on the frame *after*
        /// the overflow, and that narrows every widget in it — so a rectangle
        /// read from an early frame is not where the click will land. The left
        /// rail of the default layout needs this; the right one happens not to.
        fn settle(&mut self, editor: &Editor) {
            for _ in 0..4 {
                self.frame(editor);
            }
        }

        fn frame(&mut self, editor: &Editor) -> ChromeOutput {
            let mut out = ChromeOutput::default();
            let chrome = &mut self.chrome;
            let _ = self.ctx.run(raw_input(Vec::new()), |ctx| {
                out = chrome.ui(ctx, editor);
            });
            out
        }

        /// Click a widget by id and return what that frame meant.
        fn click(&mut self, editor: &Editor, id: egui::Id) -> ChromeOutput {
            let pos = self
                .ctx
                .read_response(id)
                .unwrap_or_else(|| panic!("{id:?} was never drawn"))
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
            let mut out = ChromeOutput::default();
            let chrome = &mut self.chrome;
            let _ = self.ctx.run(raw_input(events), |ctx| {
                out = chrome.ui(ctx, editor);
            });
            out
        }

        fn panels_on(&self, side: ui::DockSide) -> Vec<ui::PanelId> {
            self.chrome.workspace().dock.panels_on(side)
        }
    }

    #[test]
    fn the_header_reorder_control_moves_a_panel_exactly_one_place() {
        // The seam defect: `ui::view::docks` reorders the panel as the ▲ is
        // clicked and *then* emits the intent, and `Chrome::harvest` absorbs
        // everything it drained. While the intent said "up" rather than "to
        // index 1", one click moved the panel twice — with the default
        // Essentials layout, Layers went from the bottom of the right rail
        // straight to the top. Nothing else caught it because every other
        // workspace intent is an absolute set.
        let dir = tempfile::tempdir().unwrap();
        let p = png(dir.path(), "a.png");
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&p).unwrap();

        let mut window = Window::new(&ed);
        let before = window.panels_on(ui::DockSide::Right);
        assert!(before.len() >= 3, "the right rail holds {before:?}");
        let panel = *before.last().unwrap();
        let from = before.len() - 1;

        window.click(&ed, ui::view::ids::panel_menu(panel));
        let out = window.click(&ed, ui::view::ids::panel_reorder(panel, true));

        let after = window.panels_on(ui::DockSide::Right);
        assert_eq!(
            after.iter().position(|q| *q == panel),
            Some(from - 1),
            "one click on ▲ moved {panel:?} from {from} to {after:?}"
        );
        // The order is otherwise untouched: exactly two panels traded places.
        let mut expected = before.clone();
        expected.swap(from - 1, from);
        assert_eq!(after, expected);
        assert_eq!(
            out.workspace,
            vec![ui::Intent::ReorderPanel {
                panel,
                to: u8::try_from(from - 1).unwrap()
            }],
            "the click meant {out:?}"
        );
    }

    #[test]
    fn the_header_move_control_docks_a_panel_on_the_other_side_once() {
        // The companion gesture, and the same double-apply risk: `DockPanel`
        // survives being absorbed twice only because it names a side rather
        // than "the next one round".
        let dir = tempfile::tempdir().unwrap();
        let p = png(dir.path(), "a.png");
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&p).unwrap();

        let mut window = Window::new(&ed);
        let panel = ui::PanelId::History;
        assert!(!window.panels_on(ui::DockSide::Bottom).contains(&panel));
        let from = window.chrome.workspace().dock.placement(panel).side;
        assert_ne!(from, ui::DockSide::Bottom);

        window.click(&ed, ui::view::ids::panel_menu(panel));
        let out = window.click(&ed, ui::view::ids::panel_dock(panel, ui::DockSide::Bottom));

        assert_eq!(
            out.workspace,
            vec![ui::Intent::DockPanel {
                panel,
                side: ui::DockSide::Bottom
            }],
            "the click meant {out:?}"
        );
        assert_eq!(window.panels_on(ui::DockSide::Bottom), vec![panel]);
        assert!(!window.panels_on(from).contains(&panel));
        // ...and the window really draws it down there on the next frame.
        let painted = painted_text_with(&window.ctx, &mut window.chrome, &ed);
        assert!(
            painted.iter().any(|t| t == panel.title()),
            "the bottom rail never drew {panel:?}: {painted:?}"
        );
    }

    #[test]
    fn hiding_a_channel_in_the_panel_changes_what_the_canvas_is_asked_to_show() {
        // Defect 6: the Channels panel's component toggles used to move a flag
        // nothing outside the panel read. The eye is clicked on the real
        // window here, and what comes back is the mask the presenter applies
        // to the composite before it reaches the GPU — see
        // `hiding_a_channel_changes_the_texture_the_canvas_samples`.
        let dir = tempfile::tempdir().unwrap();
        let p = png(dir.path(), "a.png");
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&p).unwrap();

        let mut window = Window::new(&ed);
        assert_eq!(
            window.chrome.channel_mask(),
            crate::presenter::ChannelMask::ALL
        );
        // One panel, so no rail overflows and every row is reachable.
        window
            .chrome
            .workspace
            .dock
            .apply_layout(ui::dock::LayoutId::Minimal);
        window
            .chrome
            .workspace
            .dock
            .set_open(ui::PanelId::Channels, true);
        window.settle(&ed);

        // Row 0 is the composite; row 1 is the first component.
        let out = window.click(&ed, ui::view::ids::channel_eye(1));
        assert_eq!(
            out.workspace,
            vec![ui::Intent::SetChannelVisible {
                channel: ui::panels::channels::ChannelKind::Component(0),
                visible: false,
            }],
            "the click meant {out:?}"
        );
        assert_eq!(
            window.chrome.channel_mask(),
            crate::presenter::ChannelMask {
                components: [false, true, true]
            },
            "the canvas was never told the red channel is off"
        );
    }

    #[test]
    fn the_channel_chord_the_panel_prints_isolates_that_channel_in_this_window() {
        // The hint beside the red row says "Ctrl+3". `ui::Workspace::ui` reads
        // that chord, but this application does not call `Workspace::ui` — it
        // draws the surfaces itself and routes keys through its own keymap — so
        // until `Chrome::channel_chords` existed the hint was a promise the
        // shipped window did not keep.
        let dir = tempfile::tempdir().unwrap();
        let p = png(dir.path(), "a.png");
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&p).unwrap();

        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut chrome = Chrome::new();
        let _ = ctx.run(raw_input(Vec::new()), |ctx| {
            chrome.ui(ctx, &ed);
        });

        let command = egui::Modifiers {
            command: true,
            ..Default::default()
        };
        let press = |key| {
            vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: command,
            }]
        };
        let mut out = ChromeOutput::default();
        let _ = ctx.run(raw_input(press(egui::Key::Num3)), |ctx| {
            out = chrome.ui(ctx, &ed);
        });
        assert_eq!(
            chrome.channel_mask(),
            crate::presenter::ChannelMask {
                components: [true, false, false]
            },
            "Ctrl+3 did not isolate the red channel: {out:?}"
        );
        assert_eq!(
            out.workspace,
            vec![ui::Intent::SelectChannel(
                ui::panels::channels::ChannelKind::Component(0)
            )],
            "{out:?}"
        );

        // Ctrl+2 is the composite, and puts every channel back.
        let _ = ctx.run(raw_input(press(egui::Key::Num2)), |ctx| {
            out = chrome.ui(ctx, &ed);
        });
        assert_eq!(chrome.channel_mask(), crate::presenter::ChannelMask::ALL);

        // Ctrl+1 belongs to the application's keymap (100%), so the panel must
        // not steal it — there is no row wearing digit 1 either.
        let _ = ctx.run(raw_input(press(egui::Key::Num1)), |ctx| {
            out = chrome.ui(ctx, &ed);
        });
        assert_eq!(chrome.channel_mask(), crate::presenter::ChannelMask::ALL);
    }

    #[test]
    fn clicking_a_tool_slot_selects_that_tool() {
        // `ui::view::toolbar`'s palette, driven from this chrome's workspace.
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(&dir.path().join("config"));
        ed.set_tool(ToolId::Move);
        let model = ui::PaletteModel::build();
        let slot = model.slot_of(ToolId::Brush).expect("the brush has a slot");

        let out = run_chrome(&ed, Some(ui::view::ids::tool_slot(slot)));
        assert_eq!(out.select_tool, Some(ToolId::Brush), "{out:?}");
    }

    /// One frame of the chrome, returning what it emitted.
    fn one_frame(chrome: &mut Chrome, editor: &Editor) -> ChromeOutput {
        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut out = ChromeOutput::default();
        let _ = ctx.run(raw_input(Vec::new()), |ctx| {
            out = chrome.ui(ctx, editor);
        });
        out
    }

    #[test]
    fn the_brush_size_is_one_number_in_both_directions() {
        // Two surfaces show the brush: the options bar reads the workspace's
        // tool options, the status bar reads Editor::brush(). Before this they
        // were separate copies, so `[` moved one and the slider moved the
        // other, and the window showed two different sizes at once.
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(tools::ToolId::Brush);
        let mut chrome = Chrome::new();

        // Editor -> options bar. A keymap change must reach the slider.
        let mut brush = *editor.brush();
        brush.size += 12.0;
        let expected = brush.size;
        editor.set_brush(brush);
        one_frame(&mut chrome, &editor);
        assert_eq!(
            chrome
                .workspace()
                .options
                .brush_settings(tools::ToolId::Brush)
                .size,
            expected,
            "the options bar did not follow Editor::brush()"
        );

        // Options bar -> editor. An intent from a drawn control must come back
        // out as `set_brush` so the shell can apply it.
        let mut out = ChromeOutput::default();
        out.workspace.push(ui::Intent::SetToolOption {
            tool: tools::ToolId::Brush,
            key: "size",
            value: ui::OptionValue::Float(77.0),
        });
        chrome.harvest_workspace_for_test(&mut out, &editor);
        assert_eq!(
            out.set_brush.map(|b| b.size),
            Some(77.0),
            "an options-bar edit did not travel back to the editor"
        );
    }
}
