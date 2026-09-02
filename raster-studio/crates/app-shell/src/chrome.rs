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

use design::{Space, SurfaceRole, TextRole, TypeRole};
use editor_core::Command;
use layer_model::LayerId;
use tools::ToolId;

use crate::action::Action;
use crate::dialog_host::{ActiveDialog, CanvasSampler};
use crate::editor::Editor;
use crate::keymap::{Chord, Key};
use crate::prefs::Preferences;

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

/// One edit of a layer's kind payload, and the pointer gesture it belongs to.
///
/// # Why the gesture travels with the edit
///
/// A slider in the Properties panel emits the value it now holds on *every*
/// frame the pointer moves. Applied naively that is two hundred history entries
/// for one sweep of the Brightness knob, and an undo that walks back through
/// them one thousandth at a time. So consecutive edits to the same layer that
/// share a gesture are folded into a single entry by
/// [`crate::Editor::apply_kind_edit`].
///
/// `None` means "this edit stands alone": a keyboard nudge, or a value typed
/// into the field. Only the window knows whether a button is still down, which
/// is why [`crate::menu_bridge::record`] leaves this `None` and
/// [`Chrome::harvest`] stamps it.
/// One Actions-panel transport request, in panel-click order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionsTransport {
    StartRecording,
    StopRecording,
    ReplayRecording,
}

#[derive(Debug, Clone, PartialEq)]
pub struct KindEdit {
    pub layer: LayerId,
    pub kind: Box<layer_model::LayerKind>,
    pub gesture: Option<u64>,
}

/// What the user asked the application to do this frame.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChromeOutput {
    /// Menu items and buttons that name an [`Action`].
    pub actions: Vec<Action>,
    /// Document edits the layers dock emitted.
    pub commands: Vec<Command>,
    /// Menu items performed against the live document by
    /// [`crate::menu_bridge::perform`]: the Filter menu, Image ▸ Adjustments,
    /// the Select menu, the merges and the fixed transforms.
    ///
    /// Their own channel because they need `&mut Editor` — a filter's result
    /// has to be *hashed into the tile store* before the command that
    /// references it can be applied, and the selection is a document field with
    /// no command behind it at all. Building either during enablement would
    /// mean doing it for 256 items every frame the menu is open; see
    /// [`crate::menu_bridge::Pick::Menu`].
    pub menu: Vec<ui::MenuAction>,
    /// Edits to a layer's kind payload: the Properties panel's adjustment
    /// sliders, the Text panel's fields.
    ///
    /// Separate from [`ChromeOutput::commands`] only because a drag emits one
    /// per frame and they must land as a single undo step — see
    /// [`KindEdit::gesture`] and [`crate::Editor::apply_kind_edit`].
    pub layer_kind: Vec<KindEdit>,
    /// The Actions panel's transport, performed in order by the shell:
    /// start/stop the recording, replay the last capture.
    pub actions_transport: Vec<ActionsTransport>,
    /// Intents the bridge had no answer for.
    ///
    /// **Not an error path that can be left empty and forgotten.** Before this
    /// field existed [`Chrome::harvest`] dropped such an intent on the floor
    /// with no status message and no log line, and that silence is why every
    /// adjustment slider in the Properties panel was inert for a whole wave: a
    /// control that does nothing looks exactly like a control that works. The
    /// shell turns each of these into a status message through
    /// [`crate::menu_bridge::unrouted_message`], so the *next* unwired control
    /// announces itself the first time anybody clicks it.
    pub unrouted: Vec<ui::Intent>,
    /// The single colour component (0..=2) the Channels panel has selected as
    /// the edit target, or `None` to edit all. Applied to the editor each
    /// frame; the paint path masks tile edits to it.
    pub paint_channel: Option<usize>,
    /// A tab was clicked.
    pub activate: Option<usize>,
    /// A tab's close button was clicked.
    pub close: Option<usize>,
    /// The tab strip's drag: move the document at `.0` to index `.1`.
    pub move_document: Option<(usize, usize)>,
    /// A layer row was clicked **this frame**. Never a mirror of the current
    /// selection; see the module note.
    pub select_layer: Option<LayerId>,
    /// Photopea's multi-selection: the whole set, in click order, plus the
    /// layer the click landed on.
    pub select_layers: Option<(Vec<LayerId>, Option<LayerId>)>,
    /// A recent-files entry was chosen.
    pub open_recent: Option<PathBuf>,
    /// A history row was clicked: walk the timeline to this many applied
    /// commands. See [`crate::Editor::jump_history`].
    pub history_jump: Option<usize>,
    /// A tool button was clicked.
    pub select_tool: Option<ToolId>,
    /// The named choice a transform menu item made, as (tool, key, index).
    pub tool_choice: Option<(ToolId, String, usize)>,
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
    /// A settings change that maps straight onto the app's own preferences —
    /// the view menu's SetTheme intent, for instance.
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
    /// A modal dialog confirmed with a value that has no channel of its own
    /// yet: creating a document, resampling or re-framing one, export,
    /// running a filter, replacing the preferences. Each is consumed by the
    /// menu-item wiring that opens the dialog which produces it —
    /// [`crate::dialog_host::DialogHost::open_for_menu_action`] routes the
    /// click, this carries the answer.
    pub dialog: Option<ui::dialogs::DialogAction>,
    /// Whether a modal dialog is open this frame. The shell reads it to
    /// suppress the keymap and refuse new canvas gestures; a modal that lets
    /// either through is not modal.
    pub dialog_open: bool,
    /// A colour well's double-click asked for the picker. The chrome opens
    /// the dialog and clears this; the target rides in the host so the
    /// confirmed colour lands in the right well.
    pub color_picker: Option<ui::panels::color::ColorWell>,
    /// The options bar's ramp swatch asked for the gradient editor. The
    /// chrome opens the dialog and clears this.
    pub gradient_editor: bool,
    /// The Brushes panel asked for the brush editor. The chrome opens the
    /// dialog and clears this.
    pub brush_editor: bool,
    /// The Preferences dialog confirmed a new [`ui::dialogs::UiPreferences`].
    /// The shell maps it onto the app's own preferences and applies it.
    pub set_ui_preferences: Option<Box<ui::dialogs::UiPreferences>>,
    /// The Fill dialog's confirmed contents.
    pub fill_spec: Option<Box<ui::dialogs::FillSpec>>,
    /// The Stroke dialog's confirmed geometry.
    pub stroke_spec: Option<Box<ui::dialogs::StrokeSpec>>,
    /// The gradient dialog confirmed with a ramp for one tool. The chrome
    /// writes it into the workspace's options (the options bar reads them
    /// back) and reports the ramp to the editor for the next stroke.
    pub set_tool_gradient: Option<(ToolId, layer_model::Gradient)>,
    /// The ramp the gradient tools paint with, read back to the editor.
    pub set_gradient_ramp: Option<layer_model::Gradient>,
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

/// Read `options` back into a brush, keeping every field `tool`'s schema does
/// not declare.
///
/// The mirror of [`push_brush`], and it exists for the same reason: a key the
/// tool never had must not travel in either direction.
/// [`ui::ToolOptions::brush_settings`] falls back to `BrushSettings::default()`
/// for an undeclared key, so reading the Pencil back through it turned
/// `aliased` off and `size_pressure` on — the two fields that *are* the Pencil,
/// and neither of them a control the Pencil's options bar draws. `base` is the
/// brush that tool already has, so an undeclared field survives the round trip
/// untouched.
fn brush_from_options(
    options: &ui::ToolOptions,
    tool: tools::ToolId,
    base: tools::BrushSettings,
) -> tools::BrushSettings {
    use ui::OptionValue;
    let float = |key: &str, was: f32| {
        options
            .get(tool, key)
            .and_then(OptionValue::as_float)
            .unwrap_or(was)
    };
    let flag = |key: &str, was: bool| {
        options
            .get(tool, key)
            .and_then(OptionValue::as_bool)
            .unwrap_or(was)
    };
    tools::BrushSettings {
        size: float("size", base.size),
        hardness: float("hardness", base.hardness),
        spacing: float("spacing", base.spacing),
        angle: float("angle", base.angle),
        roundness: float("roundness", base.roundness),
        opacity: float("opacity", base.opacity),
        flow: float("flow", base.flow),
        smoothing: float("smoothing", base.smoothing),
        size_pressure: flag("size_pressure", base.size_pressure),
        flow_pressure: flag("flow_pressure", base.flow_pressure),
        // Neither is a control any tool's schema declares, so both are the
        // tool's own and are carried through rather than defaulted.
        min_size_ratio: base.min_size_ratio,
        aliased: base.aliased,
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
    /// The `ui` crate's workspace: the dock, the panels, the tool palette's
    /// fly-outs, the tool options, the view overlays.
    workspace: ui::Workspace,
    /// Which tab a drag started on, if one is in flight.
    tab_drag: Option<usize>,
    /// The status bar's readouts popup is open.
    readouts_open: bool,
    /// How many times a pointer button has gone down since the window opened.
    ///
    /// The identity of the drag in progress, and nothing more: two sweeps of
    /// the same slider get different numbers, so they land as two undo steps
    /// while one sweep lands as one. See [`KindEdit::gesture`].
    gesture: u64,
    /// Whether a pointer button is down right now. A release ends the run of
    /// edits that coalesce, which is the whole reason the counter alone is not
    /// enough: without it, a value nudged from the keyboard would be folded
    /// into whatever drag happened last.
    pointer_down: bool,
    /// The two rectangles the last drawn frame settled on. `None` until a frame
    /// has been drawn, which is the only honest answer before one has.
    frame_geometry: Option<FrameGeometry>,
    /// The modal dialog host: at most one [`ui::dialogs`] surface open, drawn
    /// after the docks. See [`crate::dialog_host`].
    dialogs: crate::dialog_host::DialogHost,
}

/// Where this frame's window is, and where the part of it the user can see the
/// image in is. **They are not the same rectangle**, and confusing them is what
/// made Fill Screen smaller than Fit on Screen.
///
/// Both are in logical points, as egui reports them; `ppp` converts either to
/// the physical pixels [`render::Camera`] measures in.
#[derive(Debug, Clone, Copy)]
struct FrameGeometry {
    /// The whole window — `Context::screen_rect`. This is the rectangle the
    /// shell renders the image across: [`crate::shell::Shell::redraw`] gives
    /// `OpenDocument::camera` the entire surface as its `viewport_size` and
    /// composites with no scissor, and the panels are painted on top of the
    /// result. [`crate::tool_input::canvas_viewport`] says the same thing on
    /// the way in, for pointer coordinates.
    surface: egui::Rect,
    /// What the docks, the strips and the tool rail left — the part of the
    /// image the user can actually see, and the rectangle Zoom to Selection
    /// has to land inside.
    content: egui::Rect,
    /// Physical pixels per logical point, for this frame.
    ppp: f32,
    /// The canvas appearance this frame is drawn with. Only the ruler gutter
    /// depth matters here, and this shell switches that off — see
    /// [`Chrome::sync_canvas_host`].
    style: ui::canvas::CanvasStyle,
}

impl Chrome {
    pub fn new() -> Self {
        Self::default()
    }

    /// The workspace this chrome draws, for tests and for the shell's own
    /// read-back of view state.
    pub fn workspace(&self) -> &ui::Workspace {
        &self.workspace
    }

    /// The choice index the options bar holds for the active tool's named
    /// mode — the transform tool's Scale/Rotate/Skew/… — fed to the live tool
    /// at each press.
    /// Adopt a named choice on the workspace's options, as the options bar
    /// would have written it.
    pub fn set_tool_choice(&mut self, tool: tools::ToolId, key: &str, index: usize) {
        self.workspace
            .options
            .set(tool, key, ui::OptionValue::Choice(index));
    }

    /// Every choice option the options bar holds for `tool`, as (key, index)
    /// pairs — the seed the live tool is fed at each press, so the transform
    /// tool's mode and target are both what the options bar shows.
    pub fn tool_choices(&self, tool: tools::ToolId) -> Vec<(String, usize)> {
        let Some(info) = tools::registry::info(tool) else {
            return Vec::new();
        };
        info.options
            .iter()
            .filter_map(|spec| match spec.kind {
                tools::registry::OptionKind::Choice { .. } => {
                    let index = match self.workspace.options.get(tool, spec.key)? {
                        ui::OptionValue::Choice(index) => index,
                        _ => return None,
                    };
                    Some((spec.key.to_string(), index))
                }
                _ => None,
            })
            .collect()
    }

    /// Whether a modal dialog is open this frame. The shell suppresses the
    /// keymap and refuses new canvas gestures while it is, so Escape and Enter
    /// belong to the dialog alone and a click that dismisses it can never
    /// start a stroke underneath.
    /// Whether the open dialog is waiting for a chord (the Preferences
    /// dialog's keymap section), for the status bar.
    pub fn is_recording(&self) -> bool {
        self.dialogs.is_recording()
    }

    pub fn dialog_open(&self) -> bool {
        self.dialogs.is_open()
    }

    /// Open the New Document dialog — File ▸ New, and the Ctrl+N that means
    /// the same thing. The shell performs [`Action::NewDocument`] by asking
    /// this question; the confirmed spec comes back through
    /// [`ChromeOutput::dialog`].
    pub fn open_new_document_dialog(&mut self) {
        self.dialogs.open(ActiveDialog::NewDocument(Box::<
            ui::dialogs::NewDocumentDialog,
        >::default()));
    }

    /// Post an intent exactly as a clicked control would, for tests.
    #[cfg(test)]
    pub(crate) fn workspace_for_test(&mut self) -> &mut ui::Workspace {
        &mut self.workspace
    }

    /// The open colour picker, for tests that drive its state directly — the
    /// headless equivalent of typing a hex code into it.
    #[cfg(test)]
    pub(crate) fn active_color_picker_for_test(&mut self) -> &mut ui::dialogs::ColorPickerDialog {
        match self.dialogs.active_for_test() {
            crate::dialog_host::ActiveDialog::ColorPicker(dialog) => dialog,
            other => panic!("the active dialog is {other:?}, not the colour picker"),
        }
    }

    /// The chrome's dialog host, for tests that drive a dialog's state.
    #[cfg(test)]
    pub(crate) fn dialogs_for_test(&mut self) -> &mut crate::dialog_host::DialogHost {
        &mut self.dialogs
    }

    /// Upload a fitted thumbnail per layer into the workspace, so the Layers
    /// panel draws real pixels instead of a kind glyph. The composite is read
    /// through the immutable [`Editor`] (free compositor) and uploaded as an
    /// egui texture; the small set is rebuilt each frame, so a layer edit shows
    /// on the next repaint.
    fn refresh_layer_thumbs(&mut self, ctx: &egui::Context, editor: &Editor) {
        self.workspace.layer_thumbs.clear();
        let Some(open) = editor.active() else {
            return;
        };
        for id in open.document.layers.iter_depth_first() {
            if let Ok((w, h, rgba)) = open.layer_thumbnail(id, 64) {
                let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
                let tex = ctx.load_texture(
                    format!("layer-thumb-{}", id),
                    img,
                    egui::TextureOptions::NEAREST,
                );
                self.workspace.layer_thumbs.insert(id, tex);
            }
        }
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
        self.read_gesture(ctx);
        self.sync_workspace(editor);
        self.refresh_layer_thumbs(ctx, editor);

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
        self.status_bar(ctx, editor, &mut out);
        if editor.panels_visible() {
            ui::view::tool_palette(&mut self.workspace, ctx);
            if let Some(open) = editor.active() {
                ui::view::docks(&mut self.workspace, ctx, &open.document, &open.history);
            }
        }
        self.start_screen(ctx, editor, &mut out);
        // The modal dialog host, after the docks: a dialog floats over
        // everything and, opened by a click this frame, draws from the next
        // one — so the click that opened it is never the click that lands on
        // it.
        self.dialogs.refresh_preview(editor);
        let sampler = self.screen_sampler(editor);
        self.dialogs.ui(
            ctx,
            sampler
                .as_ref()
                .map(|s| s as &dyn ui::dialogs::ScreenSampler),
            &mut out,
        );
        out.dialog_open = self.dialogs.is_open();
        // The gradient dialog confirmed: the ramp lands in the workspace's
        // options (the swatch reads them back next frame) and in the editor
        // (the next gradient stroke paints it, through the tool context).
        if let Some((tool, gradient)) = out.set_tool_gradient.take() {
            if self.workspace.options.set_gradient(tool, gradient.clone()) {
                out.workspace.push(ui::Intent::SetToolGradient {
                    tool,
                    gradient: Box::new(gradient.clone()),
                });
                out.set_gradient_ramp = Some(gradient);
            }
        }
        if editor.preferences_open() {
            // The flag is the intent signal; the dialog host owns the surface
            // now. ShowPreferences is a toggle, so pushing it clears the flag
            // and the dialog opens exactly once.
            self.dialogs.open_preferences(editor.ui_preferences());
            out.actions.push(crate::action::Action::ShowPreferences);
        }
        if editor.file_info_open() {
            self.file_info_window(ctx, editor, &mut out);
        }
        // The Channels panel's selected row is an *edit target*: when it is one
        // colour component, painting lands on that channel only.
        out.paint_channel = match self.workspace.channels.selected {
            ui::panels::channels::ChannelKind::Component(i) => Some(i),
            _ => None,
        };
        // Guides: the canvas view was seeded from the document in `observe`;
        // a guide edited on the canvas this frame diverges, and this converges
        // the document back as one undoable `SetGuides`. When nothing was
        // edited they agree and no command is emitted.
        self.sync_guides(editor, &mut out);
        // Read *after* the chrome is drawn: this is the room the image actually
        // has once every panel has taken its share, and it is what the
        // Navigator's rectangle and Fit on Screen are computed against.
        self.record_viewport(ctx);
        // The right-click menu floats above everything; its rows post intents
        // the harvest below turns into actions the same frame.
        let menu_ctx = crate::menu_bridge::context(editor, &self.workspace);
        ui::context_menu::draw_open(&mut self.workspace, ctx, &menu_ctx);
        self.channel_chords(ctx, editor);
        self.harvest(editor, &mut out);
        // A colour well's double-click asked for the picker: open it now (the
        // harvest that delivered the intent ran after this frame's draw), and
        // the dialog draws from the next frame with the target remembered.
        if let Some(target) = out.color_picker.take() {
            self.dialogs.open_color_picker(editor, target);
            out.dialog_open = self.dialogs.is_open();
        }
        if out.gradient_editor {
            let tool = editor.effective_tool();
            let gradient = self.workspace.options.gradient(tool);
            self.dialogs.open_gradient_editor(tool, gradient);
            out.dialog_open = self.dialogs.is_open();
        }
        if out.brush_editor {
            let tool = editor.effective_tool();
            self.dialogs.open_brush_editor(editor.brush_for(tool));
            out.dialog_open = self.dialogs.is_open();
        }
        out
    }

    /// The eyedropper's read of the live composite, when there is a composite
    /// and a frame to read it through. `None` draws the dialogs' eyedropper
    /// disabled with a reason rather than pretending.
    fn screen_sampler<'a>(&self, editor: &'a Editor) -> Option<CanvasSampler<'a>> {
        // Copy the frame values out: the sampler borrows only the editor, so
        // it can live across the dialogs' `&mut` draw.
        let frame = self.frame_geometry?;
        let doc = editor.active()?;
        Some(CanvasSampler::new(doc, frame.surface.size(), frame.ppp))
    }

    /// Converge the document's guides to what the canvas view currently holds,
    /// when they differ. The reverse of `CanvasHost::observe`'s seed: the
    /// document is the persisted, undoable record, so a guide movement made on
    /// the canvas lands here as one `SetGuides` step.
    fn sync_guides(&mut self, editor: &Editor, out: &mut ChromeOutput) {
        let Some(open) = editor.active() else {
            return;
        };
        let canvas = self.workspace.canvas.view.guides.to_document();
        if canvas != open.document.guides {
            out.commands
                .push(editor_core::Command::SetGuides { guides: canvas });
        }
    }

    /// Note which press-and-drag, if any, this frame's edits belong to.
    ///
    /// Read before anything is drawn, so every control in the frame agrees
    /// about the gesture it is part of.
    fn read_gesture(&mut self, ctx: &egui::Context) {
        let (pressed, down) = ctx.input(|i| (i.pointer.any_pressed(), i.pointer.any_down()));
        if pressed {
            self.gesture = self.gesture.wrapping_add(1);
        }
        self.pointer_down = down;
    }

    /// The gesture an edit raised this frame belongs to, or `None` when no
    /// button is down and the edit therefore stands alone.
    fn gesture(&self) -> Option<u64> {
        self.pointer_down.then_some(self.gesture)
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
        // `Editor::brush` is the *active tool's* brush — it keeps one per tool,
        // see `Editor::set_tool` — so what lands in the options bar is the
        // Pencil's 1px when the Pencil is selected and the Clone Stamp's 40px
        // when it is, rather than one application-wide number pushed into every
        // tool's sliders in turn. The reverse direction is `brush_from_options`,
        // which is careful to bring back only the keys `tool` declares.
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
            // Select ▸ Reselect / Save / Load enablement reads the stored
            // selection; publish it from the document so the menu is truthful.
            w.has_stored_selection = open.document.stored_selection.is_some()
                || !open.document.saved_selections.is_empty();
            w.saved_selections = open.document.saved_selections.len();
            // The `ui` canvas host is never *drawn* by this shell — the image
            // is composited onto the surface behind egui, and
            // `CanvasHost::central_panel` is never called — so nothing used to
            // tell it what document it was looking at or where the camera was.
            // The View menu's Fill Screen, Zoom to Selection and Print Size are
            // performed by that host, against exactly those two facts; without
            // this they framed a zero-sized document from a camera at the
            // origin and moved nothing.
            w.canvas.observe(&open.document);
            w.canvas.view.camera.set_zoom(open.camera.zoom);
            w.canvas.view.camera.center =
                glam::Vec2::new(open.camera.center.x, open.camera.center.y);
        }
    }

    /// Remember this frame's two rectangles: the window, and what the docks
    /// left of it.
    ///
    /// [`Workspace::viewport`](ui::Workspace::viewport) is the leftover — the
    /// visible canvas area, which is what the Navigator draws its proxy from.
    ///
    /// The `ui` canvas host, though, is given the **whole window**, because
    /// that is the rectangle the shell renders from: `OpenDocument::camera`'s
    /// `viewport_size` is the entire surface and the composite is drawn across
    /// all of it, with the panels painted over the top. Every zoom command this
    /// chrome routes to that host divides by its viewport, so the host and
    /// `render::Camera` have to be looking at the same rectangle or the zoom
    /// they compute is for a window that does not exist.
    ///
    /// That mismatch was not a rounding error. Given the host the content
    /// rectangle instead, a 400x300 document in a 1400x900 window came out at
    /// Fit 3.0 and Fill 2.4565 — Fill *smaller* than Fit — and the image, sized
    /// for a 732x752 rectangle but centred on the window, left a strip of bare
    /// backdrop along the bottom of the canvas area.
    ///
    /// The one command that genuinely belongs to the smaller rectangle is Zoom
    /// to Selection, and it asks for it by name: see [`Chrome::frame_selection`].
    fn record_viewport(&mut self, ctx: &egui::Context) {
        let content = ctx.available_rect();
        let (w, h) = (content.width(), content.height());
        if w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0 {
            self.workspace.viewport = (w, h);
        }
        let geometry = FrameGeometry {
            surface: ctx.screen_rect(),
            content,
            ppp: ctx.pixels_per_point(),
            style: ui::canvas::CanvasStyle::from_context(ctx),
        };
        // The host is never drawn by this shell and therefore never learned any
        // of this on its own: left alone it frames documents against its default
        // 1280x720 viewport, whatever window it is really in.
        self.sync_canvas_host(geometry.surface, &geometry);
        self.frame_geometry = Some(geometry);
    }

    /// Point the `ui` canvas host at `rect`, measured in the window `geometry`
    /// describes.
    ///
    /// The rulers are switched off first. They are the host's own gutter, and
    /// this shell never draws the host — so leaving them on insets the viewport
    /// by a strip of window that nothing has actually reserved, and every zoom
    /// computed from it comes out short.
    fn sync_canvas_host(&mut self, rect: egui::Rect, geometry: &FrameGeometry) {
        self.workspace.canvas.view.rulers_visible = false;
        let surface = geometry.surface.size();
        self.workspace.canvas.view.sync_viewport(
            glam::Vec2::new(surface.x, surface.y),
            rect,
            geometry.ppp,
            &geometry.style,
        );
    }

    /// View ▸ Zoom to Selection, framed where the user can see it.
    ///
    /// Every other camera command this chrome routes is about the whole
    /// picture, so the window is the right rectangle for all of them. This one
    /// is about *showing the user something*, and the shell paints its docks
    /// over the window: measured against the surface, a selection is centred on
    /// the window and its leading edges end up behind the tool rail and the
    /// options bar. With every dock open in a 1400x900 window that hid ~27
    /// points of a 40x40 selection's left edge and ~29 of its top;
    /// `zoom_to_selection_frames_the_selection_where_the_docks_are_not`
    /// measures the same thing from the camera the shell renders with.
    ///
    /// So the zoom is measured against the content rectangle, and the centre is
    /// then translated back into the surface-centred camera the shell renders
    /// from. [`render::Camera::screen_to_image`] puts `center` at the middle of
    /// the *surface*, so to land a document point `p` at the middle of the
    /// content rectangle the camera has to be centred at
    /// `p - (content_centre - surface_centre) / zoom`.
    fn frame_selection(&mut self) {
        let intent = ui::Intent::Action(ui::menu::MenuAction::Zoom(
            ui::menu::ZoomCommand::ToSelection,
        ));
        let Some(geometry) = self.frame_geometry else {
            // Nothing has been drawn yet, so there is no content rectangle to
            // frame against. Perform it plainly rather than drop it.
            self.workspace.absorb(&intent);
            return;
        };
        self.sync_canvas_host(geometry.content, &geometry);
        let moved = self.workspace.absorb(&intent);
        self.sync_canvas_host(geometry.surface, &geometry);
        // Nothing selected: the camera did not move, and shifting it by the
        // panel offset would pan the image for a command that refused.
        if !moved {
            return;
        }
        let zoom = self.workspace.canvas.view.camera.zoom;
        if !(zoom.is_finite() && zoom > 0.0) {
            return;
        }
        let offset = ((geometry.content.center() - geometry.surface.min) * geometry.ppp
            - geometry.surface.size() * (geometry.ppp * 0.5))
            / zoom;
        self.workspace.canvas.view.camera.center -= glam::Vec2::new(offset.x, offset.y);
        // `Workspace::absorb_action` read the camera back into `view_center`
        // before this correction, and `harvest` reports *that* to the shell.
        let center = self.workspace.canvas.view.camera.center;
        self.workspace.view_center = (center.x, center.y);
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
        // Nor while a modal is up: the dialog owns the keyboard, and a digit
        // typed into one of its fields must not isolate a channel behind it.
        if self.dialogs.is_open() {
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
            // A menu action the dialog host answers opens its dialog instead of
            // being performed. The intent is consumed here — the confirmed
            // value arrives through [`ChromeOutput::dialog`] or a dedicated
            // channel once the dialog is confirmed, one or more frames later.
            if let ui::Intent::Action(action) = &intent {
                if self.dialogs.open_for_menu_action(action, editor) {
                    continue;
                }
            }
            match crate::menu_bridge::pick(&intent, editor) {
                Some(pick) => crate::menu_bridge::record(pick, out),
                // Loud, not silent. This `else` used to be absent, so a control
                // whose intent the bridge could not answer produced no edit, no
                // status message and no log line — indistinguishable from a
                // control that worked. See `ChromeOutput::unrouted`.
                None => out.unrouted.push(intent),
            }
        }
        // Which drag an edit belongs to is the *window's* knowledge: a slider
        // emits the value it now holds and has no idea whether the button is
        // still down. Stamped here so `Editor::apply_kind_edit` can fold one
        // sweep into one undo step.
        for edit in &mut out.layer_kind {
            edit.gesture = self.gesture();
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
            match intent {
                // The one camera command that is measured against the rectangle
                // the docks left rather than the whole window, because it exists
                // to put something in front of the user. See `frame_selection`.
                ui::Intent::Action(ui::menu::MenuAction::Zoom(
                    ui::menu::ZoomCommand::ToSelection,
                )) => self.frame_selection(),
                _ => {
                    self.workspace.absorb(intent);
                }
            }
        }
        // The other half of the brush's single source of truth: an options-bar
        // edit is absorbed above, so read the result back and hand it to the
        // shell for `Editor::set_brush`.
        if out.workspace.iter().any(touches_brush) {
            let tool = editor.effective_tool();
            out.set_brush = Some(brush_from_options(
                &self.workspace.options,
                tool,
                editor.brush_for(tool),
            ));
        }
        // The other half of the four View items this bridge routes to the
        // workspace. `absorb_action` moves the *workspace's* canvas camera, and
        // the camera the user is looking at is the document's — the shell
        // composites against `OpenDocument::camera`. So the result is read back
        // and handed out the same way the Navigator's own pan is. Without this,
        // Fill Screen moved a camera nothing renders from and `sync_workspace`
        // put the old zoom back on the very next frame.
        if out
            .workspace
            .iter()
            .any(|i| matches!(i, ui::Intent::Action(a) if crate::menu_bridge::is_workspace_camera_action(*a)))
        {
            out.set_zoom = Some(self.workspace.status.zoom);
            out.set_view_center = Some(self.workspace.view_center);
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

    /// The id of the close control on tab `index`.
    ///
    /// A stable id so a headless test can click the real button, the way
    /// `ui::view::ids` does for the panels. It became worth having when the
    /// control stopped being a text button and started being a drawing:
    /// `read_response` is the only way to prove the thing on screen is still
    /// wired to `ChromeOutput::close`.
    pub fn tab_close_id(index: usize) -> egui::Id {
        egui::Id::new(("raster-tab-close", index))
    }

    /// Widest a document tab may grow before its title truncates.
    const TAB_MAX_WIDTH_PT: f32 = 160.0;

    /// The tab button itself, for drag targeting in tests and a11y.
    pub fn tab_id(index: usize) -> egui::Id {
        egui::Id::new(("raster-tab", index))
    }

    /// The status bar's editable zoom field.
    pub fn status_zoom_id() -> egui::Id {
        egui::Id::new("raster-status-zoom")
    }

    /// The status bar's readouts-menu button.
    pub fn status_readouts_id() -> egui::Id {
        egui::Id::new("raster-status-readouts")
    }

    fn tab_strip(&mut self, ctx: &egui::Context, editor: &Editor, out: &mut ChromeOutput) {
        egui::TopBottomPanel::top("raster-tabs")
            .frame(panel_frame(ctx, SurfaceRole::Panel, Space::Hair))
            .show(ctx, |ui| {
                // With no document the start screen (drawn over the canvas)
                // is the empty state; the strip itself stays empty.
                if editor.documents().is_empty() {
                    return;
                }
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = Space::XSmall.pt();
                    for (index, doc) in editor.documents().iter().enumerate() {
                        let tooltip = match doc.project_path() {
                            Some(p) => p.display().to_string(),
                            None => String::new(),
                        };
                        self.document_tab(ui, editor, index, doc.tab_label(), tooltip, out);
                    }
                    // A strip wider than the bar hides tabs; the chevron
                    // offers the first hidden one, which is a thing the click
                    // can complete.
                    let overflowed = ui.available_width() < 0.0;
                    let _ = overflowed;
                });
            });
    }

    /// One Photopea document tab: capped width, truncated title (the dirty
    /// dot rides in `tab_label`), close button, middle-click close, drag to
    /// reorder.
    fn document_tab(
        &mut self,
        ui: &mut egui::Ui,
        editor: &Editor,
        index: usize,
        label: String,
        tooltip: String,
        out: &mut ChromeOutput,
    ) -> egui::Rect {
        let selected = editor.active_index() == Some(index);
        let tokens = design::current_tokens(ui);
        let height = tokens.metrics.control_height;
        let tab_width = Self::TAB_MAX_WIDTH_PT;
        // The interaction carries a deterministic id so tests (and the drag
        // bookkeeping) can name the tab: `ui::interact` with an explicit id.
        let (rect, _) = ui.allocate_exact_size(egui::vec2(tab_width, height), egui::Sense::hover());
        let response = ui.interact(rect, Self::tab_id(index), egui::Sense::click_and_drag());
        if selected {
            ui.painter().rect_filled(
                rect,
                design::egui_theme::rounding(design::Radius::Small.resolve(&tokens.radii, height)),
                design::color32(tokens.palette.color(design::ColorRole::SurfaceElevated)),
            );
        }
        // The title truncates to the tab, with an ellipsis when it had to:
        // Photopea never lets one long name widen the strip.
        let font = egui::TextStyle::Small.resolve(ui.style());
        let color = design::color32(if selected {
            tokens.palette.text(design::TextRole::Primary)
        } else {
            tokens.palette.text(design::TextRole::Secondary)
        });
        let mut shown = label.clone();
        let mut galley = ui
            .painter()
            .layout_no_wrap(shown.clone(), font.clone(), color);
        if galley.size().x > tab_width - 12.0 {
            let ellipsis = '\u{2026}';
            while shown.chars().count() > 1 {
                shown.pop();
                let candidate = format!("{shown}{ellipsis}");
                let g = ui
                    .painter()
                    .layout_no_wrap(candidate.clone(), font.clone(), color);
                if g.size().x <= tab_width - 12.0 {
                    galley = g;
                    break;
                }
            }
        }
        let pos = egui::pos2(rect.left() + 6.0, rect.center().y - galley.size().y * 0.5);
        ui.painter().galley(pos, galley, color);
        let tip = if tooltip.is_empty() {
            "not saved yet".to_string()
        } else {
            tooltip
        };
        let response = response.on_hover_text(tip);
        if response.clicked() {
            out.activate = Some(index);
        }
        // Right-click offers the close family through the shared context-menu
        // drawer; its items resolve against this document's context.
        if response.secondary_clicked() {
            let open = &editor.documents()[index];
            let _ctx = ui::MenuContext {
                open_documents: editor.documents().len(),
                ..ui::MenuContext::from_document(&open.document, &open.history)
            };
            let pos = response
                .interact_pointer_pos()
                .unwrap_or_else(|| response.rect.center());
            self.workspace.context_menu = Some((ui::context_menu::ContextTarget::DocumentTab, pos));
            self.workspace.context_menu_fresh = true;
        }
        // Middle-click closes, like every browser tab. `clicked` only covers
        // the primary button, so the middle button is read off the hovered
        // tab's own input.
        if response.hovered() && ui.input(|i| i.pointer.button_clicked(egui::PointerButton::Middle))
        {
            out.close = Some(index);
        }
        // Drag to reorder: pressing one tab and dragging across another moves
        // it there live, the way a browser tab strip does. The emission is
        // absolute (from -> to), so absorbing it twice cannot move it twice.
        if response.drag_started() {
            self.tab_drag = Some(index);
        }
        if let Some(from) = self.tab_drag {
            // Geometry, not `hovered()`: egui suppresses hover on every widget
            // but the dragged one, and the whole point is which OTHER tab the
            // pointer is over.
            if ui.rect_contains_pointer(rect) && index != from {
                out.move_document = Some((from, index));
                self.tab_drag = Some(index);
            }
            if response.drag_stopped() || !ui.input(|i| i.pointer.any_down()) {
                self.tab_drag = None;
            }
        }
        // Drawn, not typed. The panel headers' close is `ui::icons`' drawing,
        // and a tab close built the other way — a "×" handed to a text button
        // — is one font change away from being an empty square again.
        if ui::icons::ui_icon_button_id(
            ui,
            "close",
            "Close",
            design::TextRole::Secondary,
            Some(Self::tab_close_id(index)),
        )
        .clicked()
        {
            out.close = Some(index);
        }
        rect
    }

    /// Photopea's start screen: New / Open buttons and the recent-files list,
    /// drawn over the canvas area while no document is open.
    ///
    /// Every emission is one the shell already performs — `Action::NewDocument`,
    /// `Action::Open`, `ChromeOutput::open_recent` — so a headless click
    /// exercises the same path the real click does.
    fn start_screen(&self, ctx: &egui::Context, editor: &Editor, out: &mut ChromeOutput) {
        if !editor.documents().is_empty() {
            return;
        }
        egui::Area::new(egui::Id::new("raster-start-screen"))
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .order(egui::Order::Middle)
            .show(ctx, |ui| {
                let tokens = design::current_tokens(ui);
                ui.vertical_centered(|ui| {
                    ui.add_space(design::Space::XXLarge.pt());
                    ui.label(
                        egui::RichText::new("Raster Studio")
                            .color(design::color32(
                                tokens.palette.text(design::TextRole::Primary),
                            ))
                            .font(design::egui_theme::font_id(tokens, design::TypeRole::Title)),
                    );
                    ui.add_space(design::Space::Medium.pt());
                    ui.horizontal(|ui| {
                        for (label, id, action) in [
                            ("New", "raster-start-new", Action::NewDocument),
                            ("Open…", "raster-start-open", Action::Open),
                        ] {
                            let (rect, _) = ui.allocate_exact_size(
                                egui::vec2(96.0, tokens.metrics.control_height),
                                egui::Sense::hover(),
                            );
                            let response =
                                ui.interact(rect, egui::Id::new(id), egui::Sense::click());
                            if response.hovered() {
                                ui.painter().rect_filled(
                                    rect,
                                    design::egui_theme::rounding(
                                        design::Radius::Medium
                                            .resolve(&tokens.radii, rect.height()),
                                    ),
                                    design::color32(
                                        tokens.palette.color(design::ColorRole::AccentSubtle),
                                    ),
                                );
                            }
                            let font = design::egui_theme::font_id(tokens, design::TypeRole::Body);
                            let galley = ui.painter().layout_no_wrap(
                                label.to_string(),
                                font,
                                design::color32(tokens.palette.text(design::TextRole::Primary)),
                            );
                            ui.painter().galley(
                                egui::pos2(
                                    rect.center().x - galley.size().x * 0.5,
                                    rect.center().y - galley.size().y * 0.5,
                                ),
                                galley,
                                egui::Color32::WHITE,
                            );
                            if response.clicked() {
                                out.actions.push(action);
                            }
                        }
                    });
                    ui.add_space(design::Space::Medium.pt());
                    ui.label(
                        egui::RichText::new("Recent")
                            .color(design::color32(
                                tokens.palette.text(design::TextRole::Tertiary),
                            ))
                            .font(design::egui_theme::font_id(
                                tokens,
                                design::TypeRole::Footnote,
                            )),
                    );
                    for (index, path) in editor.recent().entries().iter().enumerate().take(8) {
                        let name = path
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| path.display().to_string());
                        let height = tokens.metrics.list_row_height;
                        let width = ui.available_width().min(280.0);
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
                        let response =
                            ui.interact(rect, Self::start_recent_id(index), egui::Sense::click());
                        if response.hovered() {
                            ui.painter().rect_filled(
                                rect,
                                egui::Rounding::ZERO,
                                design::color32(
                                    tokens.palette.color(design::ColorRole::ControlFillHovered),
                                ),
                            );
                        }
                        let font = design::egui_theme::font_id(tokens, design::TypeRole::Body);
                        let galley = ui.painter().layout_no_wrap(
                            name,
                            font,
                            design::color32(tokens.palette.text(design::TextRole::Secondary)),
                        );
                        let pos =
                            egui::pos2(rect.left() + 6.0, rect.center().y - galley.size().y * 0.5);
                        ui.painter().galley(pos, galley, egui::Color32::WHITE);
                        let response = response.on_hover_text(path.display().to_string());
                        if response.clicked() {
                            out.open_recent = Some(path.clone());
                        }
                    }
                    if editor.recent().is_empty() {
                        ui.colored_label(
                            design::color32(tokens.palette.text(design::TextRole::Tertiary)),
                            "No recent files yet",
                        );
                    }
                });
            });
    }

    /// The recent row's id, so a headless test can click entry `index`.
    pub fn start_recent_id(index: usize) -> egui::Id {
        egui::Id::new(("raster-start-recent", index))
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
    fn status_bar(&mut self, ctx: &egui::Context, editor: &Editor, out: &mut ChromeOutput) {
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
                            // Photopea's bottom-left: the zoom is editable, the
                            // dimensions are not. Committing parses the
                            // Navigator's grammar and hands the camera the
                            // result through `ChromeOutput::set_zoom`.
                            if let Some(text) = self.zoom_field(ui) {
                                if let Some(zoom) = ui::panels::navigator::parse_zoom(&text) {
                                    self.workspace.status.zoom = zoom;
                                    out.set_zoom = Some(zoom);
                                }
                            }
                            ui.colored_label(dim, ui::status::format_dimensions(&doc.document));
                        }
                        None => {
                            ui.colored_label(dim, "No document");
                        }
                    }
                    // The readouts chevron: the fields that would crowd the
                    // bar live in this menu.
                    if ui::icons::ui_icon_button_id(
                        ui,
                        "chevron-right",
                        "More readouts",
                        design::TextRole::Secondary,
                        Some(Self::status_readouts_id()),
                    )
                    .clicked()
                    {
                        self.readouts_open = !self.readouts_open;
                    }
                    if self.readouts_open {
                        self.readouts_menu(ui, editor, dim);
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

    /// The editable zoom percentage, sharing the Navigator's grammar through
    /// `parse_zoom`. Commits on focus loss; Escape puts the old value back.
    fn zoom_field(&mut self, ui: &mut egui::Ui) -> Option<String> {
        let id = Self::status_zoom_id();
        let key = id.with("in-progress");
        let stored = ui.memory(|m| m.data.get_temp::<String>(key));
        let was_editing = stored.is_some();
        let mut buffer = stored
            .unwrap_or_else(|| ui::panels::navigator::format_zoom(self.workspace.status.zoom));
        let tokens = design::current_tokens(ui);
        let response = ui.add_sized(
            egui::Vec2::new(64.0, tokens.metrics.control_height),
            egui::TextEdit::singleline(&mut buffer).id(id),
        );
        let cancelled = ui.input(|i| i.key_pressed(egui::Key::Escape));
        let finished = response.lost_focus();
        let editing = was_editing || response.has_focus() || response.changed();
        if finished || cancelled {
            ui.memory_mut(|m| m.data.remove::<String>(key));
        } else if editing {
            ui.memory_mut(|m| m.data.insert_temp(key, buffer.clone()));
        }
        (finished && !cancelled).then_some(buffer)
    }

    /// The readouts popup: the fields Photopea keeps behind its bar chevron,
    /// drawn from the same derived model so it cannot drift from the bar.
    fn readouts_menu(&mut self, ui: &mut egui::Ui, editor: &Editor, dim: egui::Color32) {
        let Some(doc) = editor.active() else {
            return;
        };
        let fields: Vec<ui::status::StatusField> = self
            .workspace
            .status
            .fields(&doc.document)
            .into_iter()
            .skip(2) // zoom is the editable field, size is inline
            .collect();
        egui::Area::new(egui::Id::new("raster-status-readouts-popup"))
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_min_width(120.0);
                    for field in fields {
                        ui.colored_label(dim, format!("{}: {}", field.label, field.value));
                    }
                    ui.colored_label(dim, self.workspace.status.tool_hint());
                });
            });
    }

    /// The File ▸ File Info… window: the facts about the open document that
    /// already exist in the model — title, canvas size, colour space, origin
    /// path and source depth. Display-only, because `DocumentMeta` holds no XMP
    /// fields to edit; the item's `unavailable_reason` says so.
    fn file_info_window(&mut self, ctx: &egui::Context, editor: &Editor, out: &mut ChromeOutput) {
        let mut open = true;
        egui::Window::new("File Info")
            .open(&mut open)
            .resizable(true)
            .default_width(dock_width(ctx))
            .frame(overlay_frame(ctx))
            .show(ctx, |ui| {
                let Some(doc) = editor.active() else {
                    ui.label("No document is open");
                    return;
                };
                design::section_header(ui, "DOCUMENT");
                design::inspector_field(ui, "Name", |ui| {
                    ui.label(doc.title().to_string());
                });
                design::inspector_field(ui, "Size", |ui| {
                    ui.label(format!(
                        "{} × {} px",
                        doc.document.width(),
                        doc.document.height()
                    ))
                });
                design::inspector_field(ui, "Colour space", |ui| {
                    ui.label(doc.document.meta.color_space.name().to_string())
                });
                let origin = doc
                    .source_path()
                    .or(doc.project_path())
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "Not saved yet".to_string());
                design::inspector_field(ui, "Source", |ui| ui.label(origin));
                design::inspector_field(ui, "Source depth", |ui| {
                    ui.label(if doc.is_sixteen_bit() {
                        "16-bit"
                    } else {
                        "8-bit"
                    })
                });
            });
        if !open {
            // The window's own close button. Toggling the action keeps the
            // editor the one place that knows whether the window is up.
            out.actions.push(Action::ShowFileInfo);
        }
    }
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

        // ...and clicking the row itself is what selects — through the
        // multi-selection route now: the whole set, click order, the clicked
        // row active.
        let out = run_chrome(&ed, Some(ui::view::ids::layer_row(other)));
        assert_eq!(
            out.select_layers,
            Some((vec![other], Some(other))),
            "{out:?}"
        );
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

    /// Every character the chrome actually laid out, paired with the font it
    /// asked for it in.
    ///
    /// Read off the emitted shapes rather than off the source, because this is
    /// the last thing before pixels: whatever is in here is what the texture
    /// atlas will be asked to draw.
    fn painted_characters(
        ctx: &egui::Context,
        chrome: &mut Chrome,
        editor: &Editor,
    ) -> Vec<(egui::FontId, char)> {
        fn walk(shape: &egui::Shape, out: &mut Vec<(egui::FontId, char)>) {
            match shape {
                egui::Shape::Text(text) => {
                    let job = &text.galley.job;
                    for section in &job.sections {
                        let Some(run) = job.text.get(section.byte_range.clone()) else {
                            continue;
                        };
                        for ch in run.chars() {
                            out.push((section.format.font_id.clone(), ch));
                        }
                    }
                }
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        walk(shape, out);
                    }
                }
                _ => {}
            }
        }

        let output = ctx.run(raw_input(Vec::new()), |ctx| {
            chrome.ui(ctx, editor);
        });
        let mut out = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut out);
        }
        out
    }

    #[test]
    fn nothing_the_chrome_paints_comes_out_as_a_tofu_box() {
        // The bug this whole exercise is about, checked at the last possible
        // moment. epaint's own replacement glyph is U+25FB WHITE MEDIUM SQUARE
        // — literally the empty box in the screenshot — and it is substituted
        // silently for any character none of the loaded fonts has. So: draw the
        // real chrome with every panel open, collect every character it laid
        // out, and ask the fonts whether they can draw it.
        //
        // A source scan cannot make this claim, because it cannot see what a
        // widget composed at run time; this can, and it is the claim that
        // matters ("a screenshot contains no empty squares").
        let dir = tempfile::tempdir().unwrap();
        let p = png(dir.path(), "a.png");
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&p).unwrap();

        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut chrome = Chrome::new();
        // Everything on screen at once: a panel that is closed paints nothing,
        // and the surfaces this bug lived on were spread across all of them.
        let _ = ctx.run(raw_input(Vec::new()), |ctx| {
            chrome.ui(ctx, &ed);
        });
        for panel in ui::PanelId::ALL.iter().copied() {
            chrome
                .workspace
                .emit(ui::Intent::SetPanelOpen { panel, open: true });
        }
        for _ in 0..4 {
            let _ = painted_characters(&ctx, &mut chrome, &ed);
        }

        // Every tool, not just the one that happens to be active. The options
        // bar is redrawn from the selected tool's own `OptionSpec` labels, so a
        // single frame only ever sees one tool's captions — which is how
        // "Pressure \u{2192} Size" on the Brush and the Eraser survived the
        // first pass of this fix with the suite green. `set_tool` is what a
        // click on the palette ends up calling, and `Chrome::sync_workspace`
        // pushes it into the workspace before the frame is laid out.
        let mut painted: Vec<(egui::FontId, char)> = Vec::new();
        let mut tools_drawn = 0usize;
        for info in tools::registry::all() {
            ed.set_tool(info.id);
            // Two frames: the first settles the new options bar's layout, the
            // second is the one a user would be looking at.
            let _ = painted_characters(&ctx, &mut chrome, &ed);
            painted.extend(painted_characters(&ctx, &mut chrome, &ed));
            tools_drawn += 1;
        }
        assert!(
            tools_drawn >= 10,
            "only {tools_drawn} tools were drawn; the registry sweep is not \
             reaching the options bar any more"
        );
        assert!(
            painted.len() > 200,
            "the chrome painted almost nothing ({} characters); this test would \
             pass without looking at anything",
            painted.len()
        );

        let mut missing: Vec<String> = Vec::new();
        ctx.fonts(|f| {
            for (font, ch) in &painted {
                // Whitespace is laid out, never drawn; `has_glyph` reports
                // `false` for '\n' by design.
                if ch.is_whitespace() || ch.is_control() {
                    continue;
                }
                if !f.has_glyph(font, *ch) {
                    let note = format!("U+{:04X} {ch:?} at {font:?}", *ch as u32);
                    if !missing.contains(&note) {
                        missing.push(note);
                    }
                }
            }
        });
        assert!(
            missing.is_empty(),
            "the chrome asked for {} character(s) no loaded font has. Each one \
             is painted as U+25FB, the empty square. Draw it through \
             `ui::icons` instead:\n{}",
            missing.len(),
            missing.join("\n")
        );
    }

    #[test]
    fn the_tofu_check_can_tell_a_missing_glyph_from_a_present_one() {
        // The other half: a gate that only ever passes proves nothing. These
        // are the very symbols the panels used to type, put to the same fonts
        // the test above uses.
        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let _ = ctx.run(raw_input(Vec::new()), |_| {});
        let font = egui::FontId::new(13.0, egui::FontFamily::Proportional);
        ctx.fonts(|f| {
            for ch in ['\u{25B8}', '\u{2715}', '\u{22EF}', '\u{25D0}', '\u{2713}'] {
                assert!(
                    !f.has_glyph(&font, ch),
                    "U+{:04X} was expected to be missing from egui's fonts",
                    ch as u32
                );
            }
            for ch in ['A', 'z', '0', '\u{2014}', '\u{00B7}'] {
                assert!(f.has_glyph(&font, ch), "{ch:?} should be present");
            }
        });
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

        /// The screen rect a drawn widget occupies, for width assertions.
        fn read_rect(&self, id: egui::Id) -> Option<egui::Rect> {
            self.ctx.read_response(id).map(|r| r.rect)
        }

        /// Press on `from_id`, drag across `to_id`, release: the tab-strip
        /// drag gesture, one frame per phase the way a real drag spans them.
        fn drag(&mut self, editor: &Editor, from_id: egui::Id, to_id: egui::Id) -> ChromeOutput {
            let from = self
                .ctx
                .read_response(from_id)
                .unwrap_or_else(|| panic!("{from_id:?} was never drawn"))
                .rect
                .center();
            let to = self
                .ctx
                .read_response(to_id)
                .unwrap_or_else(|| panic!("{to_id:?} was never drawn"))
                .rect
                .center();
            let phases: Vec<Vec<egui::Event>> = vec![
                vec![
                    egui::Event::PointerMoved(from),
                    egui::Event::PointerButton {
                        pos: from,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::default(),
                    },
                ],
                vec![egui::Event::PointerMoved(to)],
                vec![egui::Event::PointerButton {
                    pos: to,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::default(),
                }],
            ];
            let mut merged = ChromeOutput::default();
            let chrome = &mut self.chrome;
            for events in phases {
                let mut out = ChromeOutput::default();
                let _ = self.ctx.run(raw_input(events), |ctx| {
                    out = chrome.ui(ctx, editor);
                });
                // Each phase contributes what it meant: a drag's move lands in
                // the middle frame, and the last frame is just the release.
                merged.move_document = merged.move_document.or(out.move_document);
                merged.activate = merged.activate.or(out.activate);
                merged.close = merged.close.or(out.close);
            }
            merged
        }

        /// Click a field, select its content, and type over it — one Text
        /// event per character, the way a keyboard delivers them.
        fn type_into(&mut self, editor: &Editor, id: egui::Id, text: &str) -> ChromeOutput {
            let mut merged = self.click(editor, id);
            merged.set_zoom = merged.set_zoom.or(None);
            let select_all = egui::Event::Key {
                key: egui::Key::A,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers {
                    command: true,
                    ..Default::default()
                },
            };
            let chrome = &mut self.chrome;
            let _ = self.ctx.run(raw_input(vec![select_all]), |ctx| {
                merged = chrome.ui(ctx, editor);
            });
            for ch in text.chars() {
                let mut out = ChromeOutput::default();
                let chrome = &mut self.chrome;
                let _ = self
                    .ctx
                    .run(raw_input(vec![egui::Event::Text(ch.to_string())]), |ctx| {
                        out = chrome.ui(ctx, editor);
                    });
                merged.set_zoom = out.set_zoom.or(merged.set_zoom);
            }
            merged
        }

        /// Right-click at a widget's centre: the gesture that opens a context
        /// menu.
        fn right_click(&mut self, editor: &Editor, id: egui::Id) -> ChromeOutput {
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
                    button: egui::PointerButton::Secondary,
                    pressed: true,
                    modifiers: egui::Modifiers::default(),
                },
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Secondary,
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

        /// Middle-click at a widget's centre.
        fn middle_click(&mut self, editor: &Editor, id: egui::Id) -> ChromeOutput {
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
                    button: egui::PointerButton::Middle,
                    pressed: true,
                    modifiers: egui::Modifiers::default(),
                },
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Middle,
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
    }

    #[test]
    fn clicking_the_first_recent_entry_on_the_start_screen_opens_it() {
        // The start screen is the empty state: the editor has no documents,
        // and the recent list it shows is the one the config dir persisted.
        let dir = tempfile::tempdir().unwrap();
        let target = png(dir.path(), "recent.png");
        let config = dir.path().join("config");
        std::fs::create_dir_all(&config).unwrap();
        let recents = AppPaths::rooted(&config).recent_file();
        let mut recent = crate::recent::RecentFiles::new();
        recent.record(&target);
        recent.save(&recents).unwrap();

        let mut ed = Editor::with_state(
            AppPaths::rooted(&config),
            Preferences::default(),
            // The editor loads recents from the config dir in ;
            //  takes the list, so load the same file here.
            crate::recent::RecentFiles::load(&recents),
            Box::new(ScriptedDialogs::new()),
        );
        assert!(ed.documents().is_empty());
        assert_eq!(ed.recent().entries(), std::slice::from_ref(&target));

        let mut window = Window::new(&ed);
        let out = window.click(&ed, Chrome::start_recent_id(0));
        assert_eq!(
            out.open_recent,
            Some(target.clone()),
            "the click meant {out:?}"
        );

        // Through the shell's apply path the file opens.
        let _ = ed.open_path(&target).unwrap();
        assert_eq!(ed.documents().len(), 1);
        assert_eq!(ed.documents()[0].tab_label(), "recent.png");
    }

    #[test]
    fn the_start_screen_offers_new_and_open() {
        let dir = tempfile::tempdir().unwrap();
        let ed = editor(dir.path());
        assert!(ed.documents().is_empty());
        let mut window = Window::new(&ed);
        let out = window.click(&ed, egui::Id::new("raster-start-new"));
        assert!(
            out.actions.contains(&Action::NewDocument),
            "the New button meant {out:?}"
        );
        let out = window.click(&ed, egui::Id::new("raster-start-open"));
        assert!(
            out.actions.contains(&Action::Open),
            "the Open button meant {out:?}"
        );
    }

    #[test]
    fn a_middle_click_on_a_tab_closes_its_document() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&png(dir.path(), "one.png")).unwrap();
        ed.open_path(&png(dir.path(), "two.png")).unwrap();
        assert_eq!(ed.documents().len(), 2);

        let mut window = Window::new(&ed);
        let out = window.middle_click(&ed, Chrome::tab_id(1));
        assert_eq!(out.close, Some(1), "middle-click meant {out:?}");

        // Through the shell's apply path the document is gone.
        ed.close_document(1).unwrap();
        assert_eq!(ed.documents().len(), 1);
    }

    #[test]
    fn a_right_click_on_a_tab_offers_close_close_others_and_close_all() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&png(dir.path(), "one.png")).unwrap();
        ed.open_path(&png(dir.path(), "two.png")).unwrap();

        let mut window = Window::new(&ed);
        let _ = window.right_click(&ed, Chrome::tab_id(1));
        // The shared drawer draws the menu inside the next frame's chrome;
        // one quiet frame lets it appear.
        let _ = window.frame(&ed);

        // Exactly three rows: the File menu's close family.
        let items = ui::context_menu::tab_items(&ui::MenuContext {
            open_documents: 2,
            has_document: true,
            ..ui::MenuContext::from_document(
                &ed.documents()[1].document,
                &ed.documents()[1].history,
            )
        });
        let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, ["Close", "Close Others", "Close All"]);
        for item in &items {
            if let ui::menu::Resolution::Disabled(reason) = &item.resolution {
                assert!(!reason.trim().is_empty(), "{:?} says why", item.label);
            }
        }
        for i in 0..3 {
            assert!(
                window
                    .ctx
                    .read_response(ui::context_menu::ids::context_item(i))
                    .is_some(),
                "row {i} of the tab menu was drawn"
            );
        }
        assert!(
            window
                .ctx
                .read_response(ui::context_menu::ids::context_item(3))
                .is_none(),
            "the tab menu has exactly three rows"
        );

        // "Close Others" routes through the menu bridge to the action.
        let out = window.click(&ed, ui::context_menu::ids::context_item(1));
        assert!(
            out.actions.contains(&Action::CloseOthers),
            "the tab menu's Close Others meant {out:?}"
        );
        // And through the shell's apply path the others are gone.
        ed.dispatch(Action::CloseOthers).unwrap();
        assert_eq!(ed.documents().len(), 1);
        assert_eq!(ed.documents()[0].tab_label(), "two.png");
    }

    #[test]
    fn the_transform_menu_items_route_to_the_canvas_gizmo() {
        // The Validate for P2.1, availability half: the gizmo exists now, so
        // Free Transform and its five interactive modes have no reason.
        use ui::menu::{MenuAction, TransformOp as T};
        for action in [
            MenuAction::FreeTransform,
            MenuAction::Transform(T::Scale),
            MenuAction::Transform(T::Rotate),
            MenuAction::Transform(T::Skew),
            MenuAction::Transform(T::Distort),
            MenuAction::Transform(T::Perspective),
            MenuAction::TransformSelection,
        ] {
            assert_eq!(
                crate::menu_bridge::unavailable_reason(action),
                None,
                "{action:?} is wired"
            );
        }
        // The mode items are a tool pick carrying the mode index — the shell
        // sets both the tool and the option from one click. The item is
        // gated on a document, so open one.
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&png(dir.path(), "a.png")).unwrap();
        let pick = crate::menu_bridge::resolve(
            MenuAction::Transform(T::Rotate),
            &crate::menu_bridge::context(&ed, &ui::Workspace::new()),
            &ed,
        )
        .unwrap();
        assert_eq!(
            pick,
            crate::menu_bridge::Pick::ToolChoice(tools::ToolId::FreeTransform, "mode", 1),
            "{pick:?}"
        );
    }

    #[test]
    fn select_all_layers_is_no_longer_unavailable() {
        // The Validate for P1.17: the item that used to name the one-active-
        // layer store as its reason now performs.
        assert_eq!(
            crate::menu_bridge::unavailable_reason(ui::menu::MenuAction::SelectAllLayers),
            None
        );
    }

    #[test]
    fn select_all_layers_fills_the_documents_selection_set() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&png(dir.path(), "one.png")).unwrap();
        ed.dispatch(Action::NewLayer).unwrap();
        ed.dispatch(Action::NewLayer).unwrap();
        assert_eq!(ed.active().unwrap().document.layers.len(), 3);

        let out = crate::menu_bridge::perform(ui::menu::MenuAction::SelectAllLayers, &mut ed);
        assert!(out.is_ok(), "{out:?}");
        let doc = &ed.active().unwrap().document;
        assert_eq!(doc.layer_selection().len(), 3, "every layer is in the set");
    }

    #[test]
    fn delete_removes_two_selected_layers_as_one_undo_step() {
        // Shift-click two rows (the selection set lands in the document), then
        // the footer's delete — the Transaction delete_selection builds —
        // removes both, and ONE undo puts them both back.
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&png(dir.path(), "one.png")).unwrap();
        ed.dispatch(Action::NewLayer).unwrap();
        ed.dispatch(Action::NewLayer).unwrap();
        let ids = ed.active().unwrap().document.layers.iter_depth_first();
        assert_eq!(ids.len(), 3, "the base layer plus two");
        let (a, b, _c) = (ids[0], ids[1], ids[2]);

        ed.set_layer_selection(vec![a, b], Some(b));
        assert_eq!(ed.active().unwrap().document.layer_selection(), vec![a, b]);

        // The layers footer's delete, driven through the real path: the
        // command the panel emits is what the shell applies through history.
        let doc = ed.active().unwrap();
        let command = ui::panels::layers::LayersModel::delete_selection(
            &doc.document,
            &doc.document.layer_selection(),
        )
        .expect("two layers delete as one step");
        assert!(
            matches!(&command, Command::Transaction { .. }),
            "two layers delete as one Transaction, not two entries: {command:?}"
        );
        ed.apply_command(command);
        assert_eq!(ed.active().unwrap().document.layers.len(), 1);

        {
            let open = ed.active_mut().unwrap();
            let (history, document) = (&mut open.history, &mut open.document);
            history.undo(document).unwrap();
        }
        assert_eq!(
            ed.active().unwrap().document.layers.len(),
            3,
            "one undo put both layers back"
        );
    }

    #[test]
    fn typing_200_into_the_status_bar_zoom_sets_the_camera_to_two() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&png(dir.path(), "one.png")).unwrap();

        let mut window = Window::new(&ed);
        window.type_into(&ed, Chrome::status_zoom_id(), "200");
        // Commit by clicking elsewhere: the field loses focus, the value lands
        // in `ChromeOutput::set_zoom`.
        let out = window.click(&ed, Chrome::status_readouts_id());
        assert_eq!(out.set_zoom, Some(2.0), "typing 200 meant {out:?}");

        // Through the shell's apply path the camera follows, and the canvas
        // redraws at the new zoom.
        if let Some(zoom) = out.set_zoom {
            ed.active_mut().unwrap().camera.zoom = zoom;
        }
        assert_eq!(ed.active().unwrap().camera.zoom, 2.0);
    }

    #[test]
    fn dragging_a_tab_reorders_the_documents() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&png(dir.path(), "one.png")).unwrap();
        ed.open_path(&png(dir.path(), "two.png")).unwrap();
        let names =
            |ed: &Editor| -> Vec<String> { ed.documents().iter().map(|d| d.tab_label()).collect() };
        assert_eq!(names(&ed), ["one.png", "two.png"]);

        let mut window = Window::new(&ed);
        let out = window.drag(&ed, Chrome::tab_id(0), Chrome::tab_id(1));
        assert_eq!(out.move_document, Some((0, 1)), "the drag meant {out:?}");
        ed.move_document(0, 1);
        assert_eq!(names(&ed), ["two.png", "one.png"]);
        // The active tab followed its document rather than staying at the
        // index.
        assert_eq!(ed.active_index(), Some(0));
    }

    #[test]
    fn a_long_title_truncates_without_widening_the_strip() {
        let dir = tempfile::tempdir().unwrap();
        let long = format!("{}.png", "a".repeat(30));
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&png(dir.path(), &long)).unwrap();

        let mut window = Window::new(&ed);
        window.settle(&ed);
        let tab = window
            .read_rect(Chrome::tab_id(0))
            .expect("the tab was drawn");
        assert!(
            tab.width() <= Chrome::TAB_MAX_WIDTH_PT + 1.0,
            "a {}-character title widened the tab to {}",
            long.len(),
            tab.width()
        );
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
        // The reorder control belongs to the ACTIVE tab of the bottom group
        // (History), and groups travel whole: one click moves History+Color
        // one slot up the rail's stack.
        let panel = *before.last().unwrap();
        let panel = if window.chrome.workspace().dock.is_active(panel) {
            panel
        } else {
            *before
                .iter()
                .rev()
                .find(|p| window.chrome.workspace().dock.is_active(**p))
                .unwrap()
        };
        let from = before.len() - 1;

        window.click(&ed, ui::view::ids::panel_menu(panel));
        let out = window.click(&ed, ui::view::ids::panel_reorder(panel, true));

        // One click on the up chevron moved the group one slot up: History
        // now sits between Adjustments and Layers instead of after Layers.
        let after = window.panels_on(ui::DockSide::Right);
        assert_eq!(
            after.iter().position(|q| *q == panel),
            Some(from - 2),
            "one click on the up chevron moved {panel:?} from {from} to {after:?}"
        );
        // The other panels are otherwise untouched, still tabbed together.
        let mut expected = before.clone();
        expected.remove(from);
        expected.remove(from - 1);
        expected.insert(from - 2, panel);
        let partner = if panel == ui::PanelId::History {
            ui::PanelId::Color
        } else {
            ui::PanelId::History
        };
        expected.insert(from - 1, partner);
        assert_eq!(after, expected);
        assert_eq!(
            out.workspace,
            vec![ui::Intent::ReorderPanel { panel, to: 1 }],
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
    fn clicking_a_tabs_drawn_close_control_closes_that_document() {
        // The tab close used to be `ghost_button(ui, "×")`. It is a drawing
        // now, for the same reason the panel headers are, and swapping the
        // widget under a control that had no test is how a working button
        // becomes a decorative one. So: click the real rect, by id.
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&png(dir.path(), "a.png")).unwrap();
        ed.open_path(&png(dir.path(), "b.png")).unwrap();
        assert_eq!(ed.documents().len(), 2);

        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut chrome = Chrome::new();
        // Settle first: the strip's height changes once the drawn button has
        // claimed its hit target, and a rect read before that has moved by the
        // time the pointer arrives.
        let mut out = ChromeOutput::default();
        for _ in 0..3 {
            let _ = ctx.run(raw_input(Vec::new()), |ctx| {
                out = chrome.ui(ctx, &ed);
            });
        }
        let id = Chrome::tab_close_id(0);
        // `interact_rect`, not `rect`: `design::list_row` takes the whole
        // available width, so the first tab's label pushes its close control
        // hard against the right edge of the window and part of it is clipped.
        // The pointer has to land on the part that is actually there.
        let at = ctx
            .read_response(id)
            .unwrap_or_else(|| panic!("{id:?} was never drawn"))
            .interact_rect
            .center();
        let events = vec![
            egui::Event::PointerMoved(at),
            egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            },
            egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::default(),
            },
        ];
        let _ = ctx.run(raw_input(events), |ctx| {
            out = chrome.ui(ctx, &ed);
        });
        assert_eq!(out.close, Some(0), "{out:?}");
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

    /// The options bar is per tool, and it may only move the fields that tool
    /// actually draws a control for.
    #[test]
    fn the_options_bar_shows_the_selected_tools_own_brush_and_keeps_what_it_cannot_draw() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(tools::ToolId::Pencil);
        let mut chrome = Chrome::new();
        one_frame(&mut chrome, &editor);

        // Editor -> options bar: the Pencil's slider reads 1, not the
        // application default of 24.
        assert_eq!(
            chrome
                .workspace()
                .options
                .get(tools::ToolId::Pencil, "size")
                .and_then(ui::OptionValue::as_float),
            Some(1.0),
            "the Pencil's size slider shows another tool's brush"
        );

        // Options bar -> editor: the Pencil's schema declares size, opacity and
        // spacing. `aliased`, `hardness` and the pressure switches are not
        // controls it draws, so moving the size slider may not touch them —
        // and they are what make a pencil a pencil.
        let mut out = ChromeOutput::default();
        out.workspace.push(ui::Intent::SetToolOption {
            tool: tools::ToolId::Pencil,
            key: "size",
            value: ui::OptionValue::Float(9.0),
        });
        chrome.harvest_workspace_for_test(&mut out, &editor);
        let back = out.set_brush.expect("a brush edit came back");
        assert_eq!(back.size, 9.0);
        assert!(back.aliased, "the size slider un-aliased the Pencil");
        assert!(
            !back.size_pressure,
            "the size slider gave the Pencil size-from-pressure"
        );
        assert_eq!(back.hardness, 1.0, "the size slider softened the Pencil");
    }

    #[test]
    fn an_intent_the_bridge_cannot_answer_is_reported_rather_than_dropped() {
        // `harvest` used to be `if let Some(pick) = pick(..)` with no `else`,
        // so an intent nothing could perform produced no edit, no status line
        // and no log record. That silence is why an entirely inert Properties
        // panel survived a whole wave of review: on screen, a control that does
        // nothing looks exactly like a control that works.
        let dir = tempfile::tempdir().unwrap();
        let ed = editor(dir.path());
        let mut chrome = Chrome::new();

        let orphan = ui::Intent::Action(ui::menu::MenuAction::PlaceEmbedded);
        chrome.workspace.emit(orphan.clone());
        let mut out = ChromeOutput::default();
        chrome.harvest_workspace_for_test(&mut out, &ed);

        assert_eq!(
            out.unrouted,
            vec![orphan.clone()],
            "the intent went nowhere and said nothing"
        );
        let said = crate::menu_bridge::unrouted_message(&orphan);
        // `Place Embedded…` is an action this build deliberately cannot answer
        // (nothing places an embedded document), and its refusal names the
        // missing piece rather than the generic fallback. What matters here is
        // that the user is *told* — the reporting path this test guards — so
        // assert the message is the real, specific one and not an empty or
        // dropped it.
        assert!(
            said.contains("Place"),
            "the refusal named nothing actionable: {said}"
        );
    }

    /// An editor with a real image open and a Posterize adjustment layer on it.
    fn editor_with_adjustment(
        dir: &std::path::Path,
    ) -> (Editor, layer_model::LayerId, layer_model::LayerKind) {
        let p = png(dir, "adj.png");
        let mut ed = editor(&dir.join("config"));
        ed.open_path(&p).unwrap();
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
        (ed, id, next)
    }

    #[test]
    fn a_slider_edit_carries_the_drag_it_belongs_to() {
        // The panel emits the value it now holds and knows nothing about the
        // pointer; only the window does. Without this stamp `Editor` cannot
        // tell one sweep of a slider from two hundred separate edits.
        let dir = tempfile::tempdir().unwrap();
        let (ed, layer, kind) = editor_with_adjustment(dir.path());
        let intent = ui::Intent::EditLayerKind {
            layer,
            kind: Box::new(kind),
        };

        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut chrome = Chrome::new();

        let at = egui::pos2(700.0, 450.0);
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };

        let mut gestures = Vec::new();
        let mut frame = |events: Vec<egui::Event>| {
            chrome.workspace.emit(intent.clone());
            let mut out = ChromeOutput::default();
            let _ = ctx.run(raw_input(events), |ctx| {
                out = chrome.ui(ctx, &ed);
            });
            assert_eq!(out.layer_kind.len(), 1, "the edit was dropped");
            out.layer_kind[0].gesture
        };

        // Press, drag, drag: one gesture throughout.
        gestures.push(frame(vec![egui::Event::PointerMoved(at), button(true)]));
        gestures.push(frame(vec![egui::Event::PointerMoved(egui::pos2(
            710.0, 450.0,
        ))]));
        // Release, then a value typed with no pointer down at all.
        gestures.push(frame(vec![button(false)]));
        // A second press is a second gesture.
        gestures.push(frame(vec![button(true)]));

        assert_eq!(
            gestures[0], gestures[1],
            "two frames of one drag were given different identities: {gestures:?}"
        );
        assert!(
            gestures[0].is_some(),
            "an edit made with the button down carried no gesture: {gestures:?}"
        );
        assert_eq!(
            gestures[2], None,
            "an edit with no button down must stand alone: {gestures:?}"
        );
        assert!(
            gestures[3].is_some() && gestures[3] != gestures[0],
            "a second press must start a second undo step: {gestures:?}"
        );
    }

    #[test]
    fn the_workspaces_canvas_learns_the_window_it_is_drawn_in() {
        // This shell never draws the `ui` canvas — the image is composited onto
        // the surface behind egui — so the canvas host's viewport was whatever
        // its default said (1280x720, no panel insets) for the whole session.
        // Every zoom command the View menu routes to the workspace divides by
        // it, so a stale one puts the image at the zoom some other window would
        // have needed.
        let dir = tempfile::tempdir().unwrap();
        let ed = editor(dir.path());
        let mut chrome = Chrome::new();
        one_frame(&mut chrome, &ed);

        let viewport = chrome.workspace().canvas.view.viewport();
        // `raw_input` gives the frame a 1400x900 window.
        assert!(
            (viewport.surface_pt().x - 1400.0).abs() < 1.0
                && (viewport.surface_pt().y - 900.0).abs() < 1.0,
            "the canvas host still thinks the window is {:?}",
            viewport.surface_pt()
        );
        // ...and its viewport is the *whole* window, because that is the
        // rectangle `render::Camera` centres the image on and spans with it.
        // Handing the host the smaller content rectangle instead is what made
        // Fill Screen come out smaller than Fit on Screen.
        assert_eq!(
            viewport.size_pt(),
            glam::Vec2::new(1400.0, 900.0),
            "the canvas host is framing against a rectangle the shell does not \
             render from: insets {:?}",
            viewport.insets()
        );
        // The chrome's own strips are still measured — they are what the
        // Navigator draws and what Zoom to Selection frames against — they are
        // just not the same rectangle.
        let geometry = chrome.frame_geometry.expect("a frame was drawn");
        assert!(
            geometry.content.height() < 900.0 && geometry.content.width() < 1400.0,
            "the menu, status strips and docks reserved nothing: {:?}",
            geometry.content
        );
        assert_eq!(
            chrome.workspace().viewport,
            (geometry.content.width(), geometry.content.height()),
            "the Navigator's viewport is no longer what the docks left"
        );
    }

    /// The document camera the shell renders from, as `Shell::redraw` builds
    /// it: the whole surface, in physical pixels.
    fn render_camera(editor: &Editor, geometry: FrameGeometry) -> render::Camera {
        let open = editor.active().expect("a document is open");
        let mut camera = open.camera.clone();
        camera.viewport_size =
            glam::Vec2::new(geometry.surface.width(), geometry.surface.height()) * geometry.ppp;
        camera
    }

    /// A 400x300 document — deliberately not the window's 14:9 — with a 40x40
    /// selection in it.
    fn wide_document(dir: &std::path::Path) -> Editor {
        let path = dir.join("wide.png");
        std::fs::write(
            &path,
            raster::encode(raster::ExportFormat::Png, 400, 300, &[9u8; 400 * 300 * 4]).unwrap(),
        )
        .unwrap();
        let mut ed = editor(&dir.join("config"));
        ed.open_path(&path).unwrap();
        let open = ed.active_mut().unwrap();
        // What `Shell::redraw` does on the first frame, and the only thing that
        // gives `OpenDocument::camera` a real viewport in a headless test.
        open.set_viewport(glam::Vec2::new(1400.0, 900.0));
        open.document.selection = editor_core::Selection::Rect {
            min: glam::IVec2::new(100, 120),
            max: glam::IVec2::new(140, 160),
        };
        ed
    }

    #[test]
    fn fill_screen_is_never_smaller_than_the_fit_the_application_performs() {
        // Fill and Fit are the same command with `min` swapped for `max`, so
        // Fill can only be smaller than Fit if the two are dividing by
        // different rectangles — which is exactly what happened while the `ui`
        // canvas host was given the content rect and `render::Camera` the whole
        // window. Measured: Fit 3.0, Fill 2.4565, and a strip of backdrop left
        // along the bottom of the canvas area.
        use ui::menu::MenuAction as M;
        use ui::menu::ZoomCommand as Z;
        let dir = tempfile::tempdir().unwrap();
        let mut ed = wide_document(dir.path());

        let (fill, chrome) = view_item(&ed, M::Zoom(Z::FillScreen));
        let fill_zoom = fill.set_zoom.expect("Fill Screen reports a zoom");
        // The Fit this application actually performs, on this same editor.
        ed.dispatch(crate::Action::ZoomFit).unwrap();
        let fit_zoom = ed.active().unwrap().camera.zoom;
        assert!(
            fill_zoom >= fit_zoom,
            "Fill Screen ({fill_zoom}) is smaller than Fit on Screen ({fit_zoom})"
        );

        // ...and it fills: at that zoom the image covers every point of the
        // canvas area the user can see, with nothing of the backdrop left.
        let geometry = chrome.frame_geometry.expect("a frame was drawn");
        let mut camera = render_camera(&ed, geometry);
        camera.zoom = fill_zoom;
        let (cx, cy) = fill.set_view_center.expect("Fill Screen reports a centre");
        camera.center = glam::Vec2::new(cx, cy);
        let ppp = geometry.ppp;
        let top_left = camera.screen_to_image(glam::Vec2::new(
            (geometry.content.min.x - geometry.surface.min.x) * ppp,
            (geometry.content.min.y - geometry.surface.min.y) * ppp,
        ));
        let bottom_right = camera.screen_to_image(glam::Vec2::new(
            (geometry.content.max.x - geometry.surface.min.x) * ppp,
            (geometry.content.max.y - geometry.surface.min.y) * ppp,
        ));
        assert!(
            top_left.x >= 0.0
                && top_left.y >= 0.0
                && bottom_right.x <= 400.0
                && bottom_right.y <= 300.0,
            "Fill Screen left backdrop showing: the canvas area spans document \
             {top_left:?}..{bottom_right:?}, outside the 400x300 image"
        );
    }

    #[test]
    fn zoom_to_selection_frames_the_selection_where_the_docks_are_not() {
        // The camera the shell renders from is centred on the *window*, and the
        // docks are painted over it. Framing the selection against the window
        // therefore hides its leading edges behind the tool rail and the
        // options bar — measured at ~27 points on the left and ~29 on the top
        // for this very selection.
        use ui::menu::MenuAction as M;
        use ui::menu::ZoomCommand as Z;
        let dir = tempfile::tempdir().unwrap();
        let ed = wide_document(dir.path());

        let (out, chrome) = view_item(&ed, M::Zoom(Z::ToSelection));
        let geometry = chrome.frame_geometry.expect("a frame was drawn");
        let mut camera = render_camera(&ed, geometry);
        camera.zoom = out.set_zoom.expect("Zoom to Selection reports a zoom");
        let (cx, cy) = out
            .set_view_center
            .expect("Zoom to Selection reports a centre");
        camera.center = glam::Vec2::new(cx, cy);

        // What the *visible* canvas rectangle shows, in document pixels.
        let ppp = geometry.ppp;
        let top_left = camera.screen_to_image(glam::Vec2::new(
            (geometry.content.min.x - geometry.surface.min.x) * ppp,
            (geometry.content.min.y - geometry.surface.min.y) * ppp,
        ));
        let bottom_right = camera.screen_to_image(glam::Vec2::new(
            (geometry.content.max.x - geometry.surface.min.x) * ppp,
            (geometry.content.max.y - geometry.surface.min.y) * ppp,
        ));
        assert!(
            top_left.x <= 100.0
                && top_left.y <= 120.0
                && bottom_right.x >= 140.0
                && bottom_right.y >= 160.0,
            "the selection (100,120)-(140,160) is not inside what the user can \
             see: the canvas area shows {top_left:?}..{bottom_right:?}"
        );
        // ...and it is framed, not merely somewhere on screen: a view showing
        // the whole document would satisfy the containment above.
        assert!(
            bottom_right.x - top_left.x < 60.0 && bottom_right.y - top_left.y < 60.0,
            "Zoom to Selection did not zoom: the canvas area shows \
             {top_left:?}..{bottom_right:?} of a 40x40 selection"
        );

        // The framing borrows the host's viewport for the length of the one
        // command and has to give it back. A Fill Screen later in the *same*
        // batch of intents would otherwise be measured against the content
        // rectangle — which is exactly the defect this distinction exists to
        // prevent, just moved one intent along.
        let mut both = Chrome::new();
        one_frame(&mut both, &ed);
        let mut batch = ChromeOutput::default();
        batch
            .workspace
            .push(ui::Intent::Action(M::Zoom(Z::ToSelection)));
        batch
            .workspace
            .push(ui::Intent::Action(M::Zoom(Z::FillScreen)));
        both.harvest_workspace_for_test(&mut batch, &ed);
        let after = batch.set_zoom.expect("Fill Screen reports a zoom");
        let alone = view_item(&ed, M::Zoom(Z::FillScreen))
            .0
            .set_zoom
            .expect("Fill Screen reports a zoom");
        assert!(
            (after - alone).abs() < 1e-3,
            "Fill Screen after Zoom to Selection gave {after}, not {alone}"
        );
    }

    #[test]
    fn zoom_to_selection_with_nothing_selected_leaves_the_camera_alone() {
        // The command refuses when there is no selection, and a refusal must
        // not be paid for with the panel-offset shift the framing applies:
        // clicking it on an empty selection would pan the image sideways.
        use ui::menu::MenuAction as M;
        use ui::menu::ZoomCommand as Z;
        let dir = tempfile::tempdir().unwrap();
        let mut ed = wide_document(dir.path());
        ed.active_mut().unwrap().document.selection = editor_core::Selection::None;
        let before = ed.active().unwrap().camera.center;

        let (out, _) = view_item(&ed, M::Zoom(Z::ToSelection));
        let (cx, cy) = out.set_view_center.expect("the read-back still reports");
        assert!(
            (cx - before.x).abs() < 1e-3 && (cy - before.y).abs() < 1e-3,
            "Zoom to Selection panned to ({cx}, {cy}) with nothing selected; \
             the camera was at {before:?}"
        );
    }

    /// Run one frame, then absorb `action` as the menu bar would have.
    fn view_item(editor: &Editor, action: ui::menu::MenuAction) -> (ChromeOutput, Chrome) {
        let mut chrome = Chrome::new();
        // The first frame is what tells the workspace's canvas host how big the
        // window and the document are.
        one_frame(&mut chrome, editor);
        let mut out = ChromeOutput::default();
        out.workspace.push(ui::Intent::Action(action));
        chrome.harvest_workspace_for_test(&mut out, editor);
        (out, chrome)
    }

    #[test]
    fn the_three_zoom_view_items_move_the_camera_the_shell_actually_renders_from() {
        // `Workspace::absorb_action` moves the *workspace's* canvas camera. The
        // camera the user sees is `OpenDocument::camera` — this shell
        // composites the image onto the surface itself and never draws the `ui`
        // canvas — so the result has to come back out as `set_zoom` /
        // `set_view_center`, which the shell writes to the document. Without
        // the read-back, Fill Screen moved a number nothing renders from and
        // `sync_workspace` overwrote it on the very next frame.
        //
        // Three items, not four: Reset View Rotation's effect lands on the
        // workspace camera's rotation and stops there, because `render::Camera`
        // is axis-aligned and has no rotation to be written back to. It is
        // checked at the bottom against the workspace camera, and it is not
        // user-reachable in this build at all — see
        // `menu_bridge::is_workspace_camera_action`'s doc.
        use ui::menu::MenuAction as M;
        use ui::menu::ZoomCommand as Z;
        let dir = tempfile::tempdir().unwrap();
        // Deliberately not the window's shape: Fit and Fill only differ on a
        // document whose aspect ratio is not the viewport's.
        let ed = wide_document(dir.path());
        let started = ed.active().unwrap().camera.zoom;

        // *That* Fill is larger than Fit is
        // `fill_screen_is_never_smaller_than_the_fit_the_application_performs`,
        // which compares against the Fit the application performs rather than
        // against another guess made on the same host. Here the claim is only
        // that the number reaches the document's camera at all.
        let (fill, _) = view_item(&ed, M::Zoom(Z::FillScreen));
        assert!(
            fill.set_zoom.is_some_and(|z| (z - started).abs() > 1e-3),
            "Fill Screen reported {:?}, which is the zoom the document already \
             had ({started})",
            fill.set_zoom
        );

        let (print, _) = view_item(&ed, M::Zoom(Z::PrintSize));
        let want = ui::canvas::workspace::POINTS_PER_INCH / ui::canvas::workspace::DEFAULT_PPI;
        assert!(
            print.set_zoom.is_some_and(|z| (z - want).abs() < 1e-3),
            "Print Size reported {:?}, wanted {want}",
            print.set_zoom
        );

        // *Where* Zoom to Selection puts the selection is
        // `zoom_to_selection_frames_the_selection_where_the_docks_are_not`; the
        // claim here is that it reports a centre near the selection at all.
        let (selection, _) = view_item(&ed, M::Zoom(Z::ToSelection));
        let center = selection
            .set_view_center
            .expect("Zoom to Selection reports a centre");
        assert!(
            (center.0 - 120.0).abs() < 20.0 && (center.1 - 140.0).abs() < 20.0,
            "Zoom to Selection framed {center:?}, nowhere near the selection"
        );

        // Rotation is the workspace canvas's own — this shell's document camera
        // is axis aligned — so this one is asserted *on* the workspace camera,
        // and it is rotated by hand first because no code path in this shell
        // can rotate it. That makes this an assertion about the routing, not
        // about anything a user can do here today.
        let mut chrome = Chrome::new();
        one_frame(&mut chrome, &ed);
        chrome.workspace.canvas.view.camera.rotation = 0.7;
        let mut out = ChromeOutput::default();
        out.workspace.push(ui::Intent::Action(M::ResetViewRotation));
        chrome.harvest_workspace_for_test(&mut out, &ed);
        assert_eq!(
            chrome.workspace.canvas.view.camera.rotation, 0.0,
            "Reset View Rotation left the view rotated"
        );
    }

    /// Open a document and queue a Layer Style intent the way a clicked menu
    /// row would, so the dialog-host tests below drive the real interception
    /// path in [`Chrome::harvest`].
    fn chrome_with_dialog() -> Chrome {
        let mut chrome = Chrome::new();
        chrome
            .workspace_for_test()
            .emit(ui::Intent::Action(ui::menu::MenuAction::LayerStyle(
                ui::menu::EffectSlot::DropShadow,
            )));
        chrome
    }

    /// Every string one drawn frame painted (see [`painted_text`], but for a
    /// chrome the caller keeps driving between frames).
    fn painted_in(full: &egui::FullOutput) -> Vec<String> {
        full.shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Text(text) => Some(text.galley.text().to_string()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_menu_action_dialog_opens_draws_and_escape_cancels_without_producing_anything() {
        let dir = tempfile::tempdir().unwrap();
        let p = png(dir.path(), "a.png");
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&p).unwrap();
        let history_before = ed.active().unwrap().history.journal().count();
        let effects_before = {
            let doc = &ed.active().unwrap().document;
            let id = doc.active_layer().unwrap();
            doc.layers.get(id).unwrap().effects.clone()
        };

        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut chrome = chrome_with_dialog();

        // Pass one opens the host (the intent is harvested); the modal draws
        // on the frames after that — egui learns the surface's height on its
        // first drawn frame, so give it two before asserting on the paint.
        let mut out = ChromeOutput::default();
        let mut painted = Vec::new();
        for _ in 0..3 {
            let full = ctx.run(raw_input(Vec::new()), |ctx| {
                out = chrome.ui(ctx, &ed);
            });
            painted = painted_in(&full);
        }
        assert!(out.dialog_open, "the dialog host is open");
        assert!(
            painted.iter().any(|t| t.contains("Layer Style")),
            "the modal was never drawn; painted: {painted:?}"
        );
        assert!(
            out.commands.is_empty() && out.actions.is_empty() && out.menu.is_empty(),
            "opening a dialog produced {out:?}"
        );

        // Escape cancels: the dialog closes, and nothing was produced — no
        // command, no action, no menu item, no parked dialog value.
        let mut out = ChromeOutput::default();
        let _ = ctx.run(
            raw_input(vec![egui::Event::Key {
                key: egui::Key::Escape,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
                physical_key: None,
            }]),
            |ctx| {
                out = chrome.ui(ctx, &ed);
            },
        );
        assert!(!out.dialog_open, "Escape did not close the dialog");
        assert!(out.dialog.is_none());
        assert!(
            out.commands.is_empty() && out.actions.is_empty() && out.menu.is_empty(),
            "cancelling a dialog produced {out:?}"
        );
        let doc = &ed.active().unwrap().document;
        let id = doc.active_layer().unwrap();
        assert_eq!(
            effects_before,
            doc.layers.get(id).unwrap().effects,
            "the document was edited by a dialog the user cancelled"
        );
        assert_eq!(
            history_before,
            ed.active().unwrap().history.journal().count(),
        );
    }

    #[test]
    fn a_canvas_click_while_a_dialog_is_open_lands_on_the_modal_and_produces_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let p = png(dir.path(), "a.png");
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&p).unwrap();

        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut chrome = chrome_with_dialog();
        let mut out = ChromeOutput::default();
        for _ in 0..3 {
            let _ = ctx.run(raw_input(Vec::new()), |ctx| {
                out = chrome.ui(ctx, &ed);
            });
        }
        assert!(out.dialog_open, "setup: the dialog is open");

        // A click below the centred dialog — over the status bar were it not
        // for the scrim, which is the only interactive surface there while a
        // modal is up.
        let click = egui::pos2(700.0, 850.0);
        let events = vec![
            egui::Event::PointerMoved(click),
            egui::Event::PointerButton {
                pos: click,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            },
            egui::Event::PointerButton {
                pos: click,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::default(),
            },
        ];
        let mut out = ChromeOutput::default();
        let mut pointer_wanted = false;
        let _ = ctx.run(raw_input(events), |ctx| {
            out = chrome.ui(ctx, &ed);
            pointer_wanted = ctx.wants_pointer_input();
        });
        // egui did receive and process the press: it registered a click.
        assert!(
            ctx.input(|i| i.pointer.any_click()),
            "the press and release were never delivered"
        );
        // And the click belongs to the chrome, not the canvas:
        // `wants_pointer_input` is what egui-winit turns into the `consumed`
        // flag the shell's gesture-claim veto reads, so a press the modal
        // layer owns can never become a tool gesture.
        assert!(
            pointer_wanted,
            "egui did not claim the pointer while a modal is open"
        );
        // Nothing was routed: no document edit, no action, no menu item, no
        // parked dialog value, and no canvas pointer event.
        assert!(
            out.commands.is_empty() && out.actions.is_empty() && out.menu.is_empty(),
            "a click under a modal produced {out:?}"
        );
        assert!(out.dialog.is_none());
        assert!(
            chrome.workspace_for_test().drain_canvas_events().is_empty(),
            "a RoutedPointer escaped while a dialog was open"
        );
        // And the dialog is still open: the click did not fall through to
        // anything that would have confirmed or cancelled it.
        assert!(out.dialog_open);
    }

    #[test]
    fn a_confirmed_preferences_dialog_changes_the_editor_s_preferences() {
        // The chrome window used to edit the app's preferences directly; the
        // dialog now owns the surface, and its confirmed schema has to reach
        // `Editor::preferences` through the shell's apply path.
        let dir = tempfile::tempdir().unwrap();
        let p = png(dir.path(), "a.png");
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&p).unwrap();
        let before = ed.preferences().clone();

        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut chrome = Chrome::new();
        ed.dispatch(crate::action::Action::ShowPreferences).unwrap();
        let mut out = ChromeOutput::default();
        for _ in 0..3 {
            let _ = ctx.run(raw_input(Vec::new()), |ctx| {
                out = chrome.ui(ctx, &ed);
            });
            // The shell performs the chrome's actions; the toggle-off has to
            // land or the intent re-fires every frame.
            for action in std::mem::take(&mut out.actions) {
                ed.dispatch(action).unwrap();
            }
        }
        assert!(out.dialog_open, "the preferences dialog opened");
        assert!(!ed.preferences_open(), "the intent was a one-shot");

        // Change theme, UI scale, autosave and history depth the way the
        // dialog's own controls do.
        chrome
            .dialogs_for_test()
            .active_preferences_for_test()
            .prefs_mut()
            .interface
            .theme = ui::dialogs::ThemeChoice::Light;
        chrome
            .dialogs_for_test()
            .active_preferences_for_test()
            .prefs_mut()
            .interface
            .ui_scale = 1.5;
        chrome
            .dialogs_for_test()
            .active_preferences_for_test()
            .prefs_mut()
            .general
            .autosave_minutes = 3;
        chrome
            .dialogs_for_test()
            .active_preferences_for_test()
            .prefs_mut()
            .history
            .states = 250;
        let mut out = ChromeOutput::default();
        let _ = ctx.run(
            raw_input(vec![egui::Event::Key {
                key: egui::Key::Enter,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
                physical_key: None,
            }]),
            |ctx| {
                out = chrome.ui(ctx, &ed);
            },
        );
        assert!(!out.dialog_open, "Enter did not close the dialog");
        let confirmed = out
            .set_ui_preferences
            .expect("the confirmed prefs travelled");

        // Through the shell's apply path, the editor's preferences are the
        // edited ones (the keymap is the documented bridge gap).
        ed.apply_ui_preferences(&confirmed);
        assert_eq!(ed.preferences().theme, crate::prefs::ThemeChoice::Light);
        assert_eq!(ed.preferences().ui_scale, 1.5);
        assert_eq!(ed.preferences().autosave_interval_secs, 180);
        assert_eq!(ed.preferences().history_depth, 250);
        assert_eq!(
            ed.preferences().keymap_overrides,
            before.keymap_overrides,
            "the live keymap wins while the bridge is unported"
        );
    }

    #[test]
    fn the_fill_menu_item_opens_its_dialog_and_confirmation_travels() {
        use ui::dialogs::Dialog as _;
        let dir = tempfile::tempdir().unwrap();
        let p = png(dir.path(), "a.png");
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&p).unwrap();

        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut chrome = Chrome::new();
        chrome
            .workspace_for_test()
            .emit(ui::Intent::Action(ui::menu::MenuAction::FillDialog));
        let mut out = ChromeOutput::default();
        for _ in 0..3 {
            let _ = ctx.run(raw_input(Vec::new()), |ctx| {
                out = chrome.ui(ctx, &ed);
            });
        }
        assert!(out.dialog_open, "the fill dialog opened");
        assert!(
            matches!(
                chrome
                    .dialogs_for_test()
                    .active_fill_dialog_for_test()
                    .confirm(),
                Some(ui::dialogs::DialogAction::Fill(_))
            ),
            "confirming produces a Fill action"
        );
    }

    #[test]
    fn a_brush_editor_dialog_confirm_changes_the_editor_s_brush() {
        let dir = tempfile::tempdir().unwrap();
        let p = png(dir.path(), "a.png");
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&p).unwrap();
        ed.set_tool(tools::ToolId::Brush);
        let tool = tools::ToolId::Brush;

        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut chrome = Chrome::new();
        chrome
            .workspace_for_test()
            .emit(ui::Intent::OpenBrushEditor);
        let mut out = ChromeOutput::default();
        for _ in 0..3 {
            let _ = ctx.run(raw_input(Vec::new()), |ctx| {
                out = chrome.ui(ctx, &ed);
            });
        }
        assert!(out.dialog_open, "the brush editor opened");

        // Change the hardness the way the dialog's own controls do.
        chrome
            .dialogs_for_test()
            .active_brush_editor_for_test()
            .settings_mut()
            .hardness = 0.25;
        let mut out = ChromeOutput::default();
        let _ = ctx.run(
            raw_input(vec![egui::Event::Key {
                key: egui::Key::Enter,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
                physical_key: None,
            }]),
            |ctx| {
                out = chrome.ui(ctx, &ed);
            },
        );
        assert!(!out.dialog_open, "Enter did not close the editor");
        let applied = out.set_brush.expect("the confirmed brush travelled");
        assert_eq!(applied.hardness, 0.25, "the hardness edit was lost");

        // Through the shell's apply path, the editor's brush is the edited one.
        ed.set_brush(applied);
        assert_eq!(ed.brush_for(tool).hardness, 0.25);
    }

    #[test]
    fn a_confirmed_gradient_editor_round_trips_into_the_workspace_and_the_editor() {
        let dir = tempfile::tempdir().unwrap();
        let p = png(dir.path(), "a.png");
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&p).unwrap();
        ed.set_tool(tools::ToolId::Gradient);

        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut chrome = Chrome::new();
        chrome
            .workspace_for_test()
            .emit(ui::Intent::OpenGradientEditor);
        let mut out = ChromeOutput::default();
        let mut painted = Vec::new();
        for _ in 0..3 {
            let full = ctx.run(raw_input(Vec::new()), |ctx| {
                out = chrome.ui(ctx, &ed);
            });
            painted = painted_in(&full);
        }
        assert!(out.dialog_open, "the gradient editor opened");
        assert!(
            painted.iter().any(|t| t.contains("Gradient")),
            "the gradient editor was never drawn"
        );

        // Edit the ramp the way the dialog's own controls do, then confirm.
        chrome
            .dialogs_for_test()
            .active_gradient_editor_for_test()
            .set_stop_color(ui::dialogs::StopKind::Color, 0, [1.0, 0.0, 0.0, 1.0]);
        chrome
            .dialogs_for_test()
            .active_gradient_editor_for_test()
            .set_stop_color(ui::dialogs::StopKind::Color, 1, [0.0, 0.0, 1.0, 1.0]);
        // What the dialog will commit: its own normalised copy of the stops.
        let confirmed = chrome
            .dialogs_for_test()
            .active_gradient_editor_for_test()
            .gradient()
            .clone();
        let mut out = ChromeOutput::default();
        let _ = ctx.run(
            raw_input(vec![egui::Event::Key {
                key: egui::Key::Enter,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
                physical_key: None,
            }]),
            |ctx| {
                out = chrome.ui(ctx, &ed);
            },
        );
        assert!(!out.dialog_open, "Enter did not close the editor");
        // The ramp round-trips: the options bar reads the same stops back.
        assert_eq!(
            chrome.workspace().options.gradient(tools::ToolId::Gradient),
            confirmed,
            "the workspace did not take the confirmed ramp"
        );
        assert_eq!(
            out.set_gradient_ramp,
            Some(confirmed),
            "the editor's stroke ramp was not read back"
        );
        // The edited stops really are in it (the dialog may add its own
        // opacity ramps beside them).
        let stops = &out.set_gradient_ramp.as_ref().unwrap().stops;
        assert_eq!(stops[0].color, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(stops[1].color, [0.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn a_foreground_swatch_double_click_opens_the_picker_and_its_confirm_lands_in_the_well() {
        let dir = tempfile::tempdir().unwrap();
        let p = png(dir.path(), "a.png");
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&p).unwrap();
        // A known colour the test sets into the picker.
        let known = [0.25, 0.5, 0.75, 1.0];

        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut chrome = Chrome::new();
        chrome
            .workspace_for_test()
            .emit(ui::Intent::OpenColorPicker(
                ui::panels::color::ColorWell::Foreground,
            ));
        let mut out = ChromeOutput::default();
        let mut painted = Vec::new();
        for _ in 0..3 {
            let full = ctx.run(raw_input(Vec::new()), |ctx| {
                out = chrome.ui(ctx, &ed);
            });
            painted = painted_in(&full);
        }
        assert!(out.dialog_open, "the picker opened");
        assert!(
            painted.iter().any(|t| t.contains("Color Picker")),
            "the picker was never drawn"
        );

        // Type a known colour into the picker and confirm it.
        chrome
            .active_color_picker_for_test()
            .set_color(ui::dialogs::ColorValue::new(known));
        let mut out = ChromeOutput::default();
        let _ = ctx.run(
            raw_input(vec![egui::Event::Key {
                key: egui::Key::Enter,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
                physical_key: None,
            }]),
            |ctx| {
                out = chrome.ui(ctx, &ed);
            },
        );
        assert!(!out.dialog_open, "Enter did not close the picker");
        assert_eq!(
            out.set_foreground,
            Some(known),
            "the confirmed colour did not land in the foreground well"
        );
        assert_eq!(out.set_background, None, "the background moved too");
    }

    #[test]
    fn cancelling_the_new_document_dialog_creates_no_document() {
        // No document open to begin with — the point is that nothing appears.
        let dir = tempfile::tempdir().unwrap();
        let ed = editor(&dir.path().join("config"));
        assert!(ed.documents().is_empty());

        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut chrome = Chrome::new();
        chrome
            .workspace_for_test()
            .emit(ui::Intent::Action(ui::menu::MenuAction::NewDocument));
        let mut out = ChromeOutput::default();
        let mut painted = Vec::new();
        for _ in 0..3 {
            let full = ctx.run(raw_input(Vec::new()), |ctx| {
                out = chrome.ui(ctx, &ed);
            });
            painted = painted_in(&full);
        }
        assert!(out.dialog_open, "File ▸ New opened its dialog");
        assert!(
            painted.iter().any(|t| t.contains("New Document")),
            "the New Document dialog was never drawn"
        );

        // Escape cancels: no document, no command, nothing.
        let mut out = ChromeOutput::default();
        let _ = ctx.run(
            raw_input(vec![egui::Event::Key {
                key: egui::Key::Escape,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
                physical_key: None,
            }]),
            |ctx| {
                out = chrome.ui(ctx, &ed);
            },
        );
        assert!(!out.dialog_open);
        assert!(ed.documents().is_empty(), "cancel created a document");
        assert!(
            out.commands.is_empty() && out.actions.is_empty() && out.dialog.is_none(),
            "cancelling produced {out:?}"
        );
    }
}
