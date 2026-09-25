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

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use design::{Space, SurfaceRole, TextRole, TypeRole};
use editor_core::Command;
use layer_model::LayerId;
use tools::ToolId;

use crate::action::Action;
use crate::dialog_host::{ActiveDialog, CanvasSampler};
use crate::editor::Editor;
use crate::keymap::{Chord, Key};
use crate::prefs::Preferences;

/// The Layers panel thumbnail's longest edge, in texels.
const THUMB_EDGE: u32 = 64;
/// W2-D/W2-X: long edge of the composite preview the Navigator and the
/// Histogram share.
const PREVIEW_EDGE: u32 = 256;
/// How many canvas rows one band of the composite preview reads at a time —
/// the peak buffer is one band, not the whole canvas, on a 3628x2041 scene.
const PREVIEW_BAND_ROWS: u32 = 256;

/// How many layer/mask thumbnails one frame may recomposite. The rest wait
/// for the next frame (which [`Chrome::refresh_layer_thumbs`] requests), so
/// a tall stack whose every layer changed — an import, a canvas resize —
/// spreads its cost over frames instead of freezing one.
const THUMBS_PER_FRAME: usize = 2;

/// The start screen's recent-file thumbnails' longest edge, in texels: a
/// texel budget for the decode, not a size on screen (the cell is sized from
/// tokens and the image is fitted into it).
const RECENT_THUMB_EDGE: u32 = 256;

/// How many recent files the start screen's grid shows.
const START_RECENT_MAX: usize = 8;

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
    /// The Properties Layer/Mask focus was set this frame (card 007). The
    /// shell stores the validated target per document; the tools read it back
    /// through `Editor::edit_target`.
    pub edit_target: Option<crate::edit_target::EditTargetKind>,
    /// Card 026: a text layer's row was double-clicked — the shell opens a
    /// live text session on that existing layer.
    pub enter_text_layer: Option<LayerId>,
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
    /// W4-G: an options-bar button (the Ruler's Straighten Layer) asked the
    /// shell to confirm the gesture the live tool is holding — exactly what
    /// Enter does.
    pub confirm_tool: bool,
    /// The Preferences dialog confirmed a new [`ui::dialogs::UiPreferences`].
    /// The shell maps it onto the app's own preferences and applies it.
    pub set_ui_preferences: Option<Box<ui::dialogs::UiPreferences>>,
    /// The gradient dialog confirmed with a ramp for one tool. The chrome
    /// writes it into the workspace's options (the options bar reads them
    /// back) and reports the ramp to the editor for the next stroke.
    pub set_tool_gradient: Option<(ToolId, layer_model::Gradient)>,
    /// The ramp the gradient tools paint with, read back to the editor.
    pub set_gradient_ramp: Option<layer_model::Gradient>,
    /// W10-B: the Glyphs panel's picks, as (text layer, characters), in
    /// order. The shell inserts each at the live typing session's caret, or
    /// appends it to the layer's text when no session is open
    /// ([`crate::menu_bridge::glyph_insert::insert_glyph`]).
    pub insert_glyphs: Vec<(LayerId, String)>,
    /// W13-L: the Animation timeline's playhead, moved this frame (a ruler
    /// scrub, a playback frame, Stop): the shell seeks the active document
    /// with no history step ([`Editor::seek_timeline`]). The last one wins.
    pub seek_timeline: Option<u32>,
}

/// The option keys that make up a [`tools::BrushSettings`].
///
/// Kept beside [`push_brush`] so the two cannot drift: a key written out but
/// never read back — or the reverse — is how the two copies disagreed before.
/// The brush-shared option keys, from the registry (the single source the
/// options bar, the brush fold, and the forward-to-tool filter all read).
pub(crate) const BRUSH_KEYS: &[&str] = tools::registry::BRUSH_OPTION_KEYS;

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
    let pairs: [(&str, OptionValue); 11] = [
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
        (
            "opacity_pressure",
            OptionValue::Bool(brush.opacity_pressure),
        ),
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
        opacity_pressure: flag("opacity_pressure", base.opacity_pressure),
        // Neither is a control any tool's schema declares, so both are the
        // tool's own and are carried through rather than defaulted.
        min_size_ratio: base.min_size_ratio,
        aliased: base.aliased,
        // W9-E: the tip and the dynamics are no options-bar control either;
        // they ride through from the brush the edit started from.
        ..base
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

/// W4-A: how many points a live elliptical marquee's rubber band is drawn with.
const LIVE_ELLIPSE_STEPS: usize = 64;
/// W4-A: how many chords a live pen curve segment is flattened into.
const LIVE_CURVE_STEPS: usize = 16;
/// W8-C: the Perspective Crop grid — lines each way inside the quad.
const PERSPECTIVE_GRID_LINES: usize = 3;
/// W8-C: the quick-mask overlay's opacity over unmasked pixels — Photoshop
/// and Photopea's default Quick Mask opacity, 50%. A data value of the mode,
/// not a chrome colour: the red itself is the theme's `ChannelRed` token.
const QUICK_MASK_ALPHA: f32 = 0.5;
/// W8-C: the longest edge the Type Mask overlay texture is built at; a
/// larger canvas is sampled down to it (the overlay is a guide, the confirm
/// reads the glyphs at full resolution).
const TYPE_MASK_OVERLAY_MAX_PX: u32 = 1024;

/// W8-C: the Type Mask overlay texture, keyed by what it was built from.
struct TypeMaskOverlay {
    key: (
        crate::doc::DocumentId,
        LayerId,
        layer_model::TextLayer,
        raster::PixelRect,
    ),
    texture: egui::TextureHandle,
    /// The overlay's pixels, for the headless tests that read what it shows.
    #[cfg(test)]
    image: egui::ColorImage,
}

/// W5-F: one hash of everything in `doc` that can change a composited pixel
/// — canvas size and colour space, the layer tree's order, each layer's
/// pixel-affecting properties ([`compositor::composite::layer_signature`])
/// and every layer and mask tile hash. Taken every frame by the Layers
/// thumbnails and the Colour Samplers to decide whether to do any work; it
/// reads no pixel and serialises nothing, and unlike
/// [`Editor::content_revision`] it also sees the routes that write the
/// document without going through the editor (a live text draft).
fn document_signature(doc: &crate::doc::OpenDocument) -> u64 {
    use std::hash::{Hash, Hasher};
    let d = &doc.document;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    d.width().hash(&mut h);
    d.height().hash(&mut h);
    d.meta.bit_depth.hash(&mut h);
    d.meta.color_mode.hash(&mut h);
    std::mem::discriminant(&d.meta.color_space).hash(&mut h);
    if let color::ColorSpace::IccProfile { asset_hash, .. } = &d.meta.color_space {
        asset_hash.hash(&mut h);
    }
    d.layers.root().hash(&mut h);
    let tiles = |map: Option<&editor_core::TileMap>,
                 h: &mut std::collections::hash_map::DefaultHasher| {
        match map {
            None => 0usize.hash(h),
            Some(map) => {
                map.len().hash(h);
                for (coord, hash) in map.iter() {
                    (coord.x, coord.y, coord.level, hash.0).hash(h);
                }
            }
        }
    };
    for id in d.layers.iter_depth_first() {
        id.hash(&mut h);
        if let Some(layer) = d.layers.get(id) {
            compositor::composite::layer_signature(layer).hash(&mut h);
        }
        tiles(d.layer_tiles(id), &mut h);
        tiles(d.mask_tiles(id), &mut h);
        // W10-I: a smart object's filter-mask coverage (its thumbnail).
        let filter_mask = d
            .layers
            .get(id)
            .and_then(crate::edit_target::filter_mask_of)
            .map(|m| m.id);
        tiles(
            filter_mask.and_then(|m| d.pixels.tiles(editor_core::PixelKey::Mask(m))),
            &mut h,
        );
    }
    h.finish()
}

/// W4-G: the composited colour of one document pixel, straight-alpha sRGB in
/// `0..=1` — what the Info panel's Colour Sampler rows show. `None` off the
/// image or when the composite fails.
fn document_colour_at(doc: &crate::doc::OpenDocument, x: i64, y: i64) -> Option<[f32; 4]> {
    let (w, h) = (
        i64::from(doc.document.width()),
        i64::from(doc.document.height()),
    );
    if x < 0 || y < 0 || x >= w || y >= h {
        return None;
    }
    let canvas = compositor::composite_region(
        &doc.document,
        &doc.tiles,
        raster::PixelRect::new(x, y, 1, 1),
        0,
        compositor::CompositeOptions::default(),
    )
    .ok()?;
    let rgba = canvas.to_rgba8(&doc.document.meta.color_space);
    Some([
        f32::from(rgba[0]) / 255.0,
        f32::from(rgba[1]) / 255.0,
        f32::from(rgba[2]) / 255.0,
        f32::from(rgba[3]) / 255.0,
    ])
}

/// W4-A: what a live session is painted with — the overlay painter and the
/// camera, viewport, style and handle sizes the canvas was drawn with.
#[derive(Clone, Copy)]
struct LiveFrame<'a> {
    painter: &'a egui::Painter,
    camera: &'a ui::canvas::CanvasCamera,
    viewport: &'a ui::canvas::Viewport,
    style: &'a ui::canvas::CanvasStyle,
    layout: &'a ui::canvas::HandleLayout,
}

/// W4-A: a screen-space polyline as marching ants — the unbroken base run,
/// then the "on" halves of the dash pattern, shifted by `phase` so the band
/// marches the way the selection's ants do.
fn marching(
    outline: Vec<glam::Vec2>,
    style: &ui::canvas::AntsStyle,
    phase: f32,
) -> ui::canvas::AntsGeometry {
    let mut out = ui::canvas::AntsGeometry::default();
    if outline.len() < 2 || !outline.iter().all(|p| p.is_finite()) {
        return out;
    }
    let dash = style.dash();
    let period = dash * 2.0;
    let mut walked = phase.rem_euclid(period);
    for pair in outline.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let length = (b - a).length();
        if length <= f32::EPSILON {
            continue;
        }
        let dir = (b - a) / length;
        let mut t = 0.0;
        while t < length && out.dashes.len() < ui::canvas::ants::MAX_SEGMENTS {
            let into = walked.rem_euclid(period);
            let on = into < dash;
            let step = if on { dash - into } else { period - into }
                .min(length - t)
                .max(f32::EPSILON);
            if on {
                out.dashes
                    .push([a + dir * t, a + dir * (t + step).min(length)]);
            }
            t += step;
            walked += step;
        }
    }
    out.outlines.push(outline);
    out
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
    /// XB: the live tool's pointer readout (a shape drag's W/H), published by
    /// the shell's pointer handler (`Shell::on_pointer`, after each tool
    /// sample) through [`Chrome::publish_tool_readout`].
    live_readout: Option<(crate::doc::DocumentId, tools::tool::LiveReadout)>,
    /// W4-A: the live session that is not a transform — a crop box, a
    /// marquee's rubber band, a lasso outline, a pen path, the slice set —
    /// published by [`Chrome::publish_tool_geometry`] and painted by
    /// `paint_live_tool_geometry`. `None` when no such session is live.
    live_session: Option<tools::SessionGeometry>,
    /// W8-C: the quick-mask red a Type Mask session shows outside its glyphs,
    /// rebuilt only when the typed run changes. `None` outside such a session.
    type_mask_overlay: Option<TypeMaskOverlay>,
    /// W3-A: View ▸ Extras — rulers, guides, grid, layer edges, precise
    /// cursor — painted over the composite. See [`crate::canvas_extras`].
    extras: crate::canvas_extras::CanvasExtras,
    /// W3-A: the reason the last View toggle was refused this frame, for the
    /// status line. See [`view_flag_refusal`].
    refused_view_flag: Option<&'static str>,
    /// Card 059: the document id the mask-well popup state belongs to —
    /// switching tabs closes the popup (it anchors a layer of that stack).
    showing_document: Option<crate::doc::DocumentId>,
    /// The `ui` crate's workspace: the dock, the panels, the tool palette's
    /// fly-outs, the tool options, the view overlays.
    workspace: ui::Workspace,
    /// Which tab a drag started on, if one is in flight.
    tab_drag: Option<usize>,
    /// W9-I: a Layers-panel row released over another document's tab this
    /// frame, as (layer, tab index), for [`Chrome::copy_layers_across`].
    layer_tab_drop: Option<(layer_model::LayerId, usize)>,
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
    /// Which documents were fitted before a canvas area was measured — see
    /// [`Chrome::place_canvas`].
    canvas_placement: crate::interaction_geometry::CanvasPlacement,
    /// The modal dialog host: at most one [`ui::dialogs`] surface open, drawn
    /// after the docks. See [`crate::dialog_host`].
    dialogs: crate::dialog_host::DialogHost,
    /// The Layers panel's thumbnail cache — see [`Chrome::refresh_layer_thumbs`].
    thumbs: crate::doc::LayerThumbCache,
    /// The document `thumbs` and the workspace's thumbnail textures belong
    /// to; a switch of active document drops both.
    thumbs_document: Option<crate::doc::DocumentId>,
    /// W2-X: the editor revision the Navigator/Histogram composite preview
    /// was built at. `None` until one is built, and again once the last
    /// document closes. See [`Chrome::refresh_composite_preview`].
    /// W5-F: keyed on [`Editor::content_revision`] and the document, so a
    /// colour-well or brush change (which moves only [`Editor::revision`])
    /// does not recomposite the canvas.
    preview_revision: Option<(u64, crate::doc::DocumentId)>,
    /// W5-F: how many composite previews were built — the counter the
    /// colour-slider gate is tested by.
    preview_builds: usize,
    /// W5-F: the (document signature, document, sampler points) the Colour
    /// Sampler rows were composited at. See
    /// [`Chrome::refresh_sampler_colours`] and [`document_signature`].
    samplers_read_at: Option<(u64, crate::doc::DocumentId, Vec<glam::Vec2>)>,
    /// W5-F: how many 1x1 sampler composites ran — the counter its gate is
    /// tested by.
    sampler_composites: usize,
    /// W5-F: the (document signature, document) the Layers thumbnails were
    /// last checked against with nothing left owed. The per-layer
    /// fingerprint pass is skipped while it holds. See
    /// [`Chrome::refresh_layer_thumbs`] and [`document_signature`].
    /// The two texture counts are part of the key, so a texture dropped by
    /// anything else is re-uploaded on the next frame.
    thumbs_checked_at: Option<(u64, crate::doc::DocumentId, usize, usize)>,
    /// W5-F: how many per-layer fingerprint passes ran.
    thumb_passes: usize,
    /// W2-X: the revision and pointer position the Info panel's colour
    /// sample was read at, so the 1x1 composite runs when either moves and
    /// not per frame. See [`Chrome::refresh_info_sample`].
    info_sample_at: Option<(u64, egui::Pos2)>,
    /// W3-X: the (revision, active tab, active layer) the raster inks were
    /// last measured at. See [`Chrome::publish_raster_inks`].
    inks_published_at: Option<(u64, Option<usize>, Option<LayerId>)>,
    /// W3-X: how many times [`Chrome::publish_raster_inks`] measured — the
    /// counter its gate is tested by.
    ink_publishes: usize,
    /// What the docks are drawn against while no document is open: a
    /// document with no layers and an empty history, so every panel shows
    /// its header over an empty body — Photopea's start state — rather than
    /// the right half of the window going blank. Built once, on first use.
    empty_dock: Option<(editor_core::Document, editor_core::History)>,
    /// The tab strip's overflow list is open.
    tab_overflow_open: bool,
    /// The start screen's Templates card has its preset list unfolded.
    start_templates_open: bool,
    /// The start screen's recent-file thumbnails, uploaded once per path.
    /// `None` records a file that could not be read, so it is not retried
    /// every frame. See [`Chrome::refresh_recent_thumbs`].
    recent_thumbs: HashMap<PathBuf, Option<egui::TextureHandle>>,
}

/// W3-A: why a View toggle cannot be turned on in *this application*, or
/// `None` when it can.
///
/// Exactly the `ui` crate's own reasons ([`ui::view_flag_unavailable`];
/// none since W7-D made Proof Colors and Gamut Warning real). Flip View is
/// honoured: `render::Camera` mirrors, and [`Chrome::ui`] copies the checkmarks onto the active
/// document's camera every frame through [`crate::tool_input::apply_view_flips`].
pub fn view_flag_refusal(flag: ui::ViewFlag) -> Option<&'static str> {
    ui::view_flag_unavailable(flag)
}

/// Where this frame's window is, and where the canvas area inside it is.
/// **They are not the same rectangle**: the docks, the options bar, the tab
/// strip, the status bar and the tool column take the edges of the window,
/// and the document is fitted, centred and drawn in what they leave.
///
/// Both are in logical points, as egui reports them; `ppp` converts either to
/// the physical pixels [`render::Camera`] measures in.
#[derive(Debug, Clone, Copy)]
struct FrameGeometry {
    /// The whole window — `Context::screen_rect`. Screen coordinates (the
    /// pointer, the overlays) are measured from its corner.
    surface: egui::Rect,
    /// What the docks, the strips and the tool rail left — the canvas area.
    /// It is the document camera's viewport: [`Chrome::canvas_area_px`] hands
    /// it (in physical pixels) to the shell, which gives every document's
    /// `render::Camera` this rectangle as its `viewport_origin` /
    /// `viewport_size` ([`crate::chrome::Chrome::place_canvas`]),
    /// renders the composite into it and nowhere else
    /// ([`render::Canvas::render_in`]), and maps the pointer and every overlay
    /// through it ([`crate::tool_input::canvas_viewport`]). Fit, Fill, 100%
    /// and Zoom to Selection all frame against it — the `ui` canvas host is
    /// synced to it by [`Chrome::sync_canvas_host`]. The rectangle is known
    /// only once the chrome has laid out, so the shell uses the previous
    /// frame's.
    content: egui::Rect,
    /// `content` less the ruler gutters while View > Rulers is on (the rulers
    /// are painted over its top and left edges by `crate::canvas_extras`), so
    /// no part of the document is fitted or centred under a ruler. This is the
    /// rectangle [`Chrome::canvas_area_px`] reports; equal to `content` with
    /// the rulers off.
    canvas: egui::Rect,
    /// Physical pixels per logical point, for this frame.
    ppp: f32,
    /// The canvas appearance this frame is drawn with. Only the ruler gutter
    /// depth matters here, and this shell switches that off — see
    /// [`Chrome::sync_canvas_host`].
    style: ui::canvas::CanvasStyle,
}

impl Chrome {
    pub fn new() -> Self {
        let mut chrome = Self::default();
        // W3-A: a `--shot` run can ask for View toggles (see
        // `crate::SHOT_VIEW_ENV`); they go through the same intent a menu
        // click posts, so the first frame's harvest applies them.
        for &flag in crate::shot_view_flags() {
            chrome.emit(ui::Intent::SetViewFlag { flag, on: true });
        }
        chrome
    }

    /// The workspace this chrome draws, for tests and for the shell's own
    /// read-back of view state.
    pub fn workspace(&self) -> &ui::Workspace {
        &self.workspace
    }

    /// Post an intent exactly as a clicked control would.
    ///
    /// The shell's door for a key chord only the menu bar paints
    /// ([`crate::keymap::Resolved::Menu`]): the intent joins this frame's
    /// outbox and [`Chrome::harvest`] decides dialog-versus-perform for it, so
    /// the chord and the menu item cannot mean two different things.
    pub fn emit(&mut self, intent: ui::Intent) {
        self.workspace.emit(intent);
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

    /// Write one option value for `tool` — the options bar's write half for
    /// the non-choice kinds (the intents route here in production; tests
    /// call it directly to drive the forward boundary).
    pub fn set_tool_option(&mut self, tool: tools::ToolId, key: &str, value: ui::OptionValue) {
        self.workspace.options.set(tool, key, value);
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

    /// Every option value the options bar holds for `tool`, as (key, value)
    /// pairs — the typed seed the live tool is fed at each press (card 010).
    /// Only options the workspace actually holds are forwarded, so a tool
    /// rebuilt from the registry keeps its defaults for everything the user
    /// never touched. The values are the UI crate's here; the shell converts
    /// them at the boundary before a tool sees them.
    /// What the tool is told at pointer-down: only options the USER has
    /// actually touched. The options bar renders every declared option (its
    /// schema default when untouched) for display, but forwarding untouched
    /// defaults to the tool would demand `set_setting` answers for keys no
    /// tool implements — the refusals would flood the status bar on every
    /// press and drown genuine ones. A tool's untouched options are its
    /// registry defaults by construction.
    ///
    /// W3-A: View ▸ Snap and View ▸ Smart Guides ride along under the two
    /// reserved keys of [`crate::SnapPolicy`] when either is off — the pointer
    /// route strips them before a tool sees anything — so the flags reach
    /// every sample the shell routes. Both on (the default) adds nothing.
    pub fn tool_options(&self, tool: tools::ToolId) -> Vec<(String, ui::OptionValue)> {
        let mut held = self.workspace.options.held(tool);
        held.extend(self.snap_policy().to_settings(ui::OptionValue::Bool));
        held
    }

    /// W3-A: what View ▸ Snap and View ▸ Smart Guides say right now.
    pub fn snap_policy(&self) -> crate::SnapPolicy {
        crate::SnapPolicy::from_view_flags(self.workspace.view_flags)
    }

    /// W3-A: View ▸ Selection Edges — whether the marching ants are shown.
    pub fn selection_edges_visible(&self) -> bool {
        // W10-J: `shows`: View > Extras (Ctrl+H) off hides the ants too.
        self.workspace
            .view_flags
            .shows(ui::ViewFlag::SelectionEdges)
    }

    /// W3-A: the marching ants the shell strokes over the canvas this frame,
    /// gated on View ▸ Selection Edges. The shell's redraw calls this rather
    /// than [`crate::presenter::selection_ants`] directly, so the flag the
    /// menu ticks is the one that decides whether the outline is drawn. Off
    /// yields empty geometry: the selection itself is untouched, only hidden.
    pub fn selection_ants(
        &self,
        outline: &mut crate::presenter::SelectionOutline,
        doc: &crate::doc::OpenDocument,
        time_secs: f64,
        style: &ui::canvas::AntsStyle,
    ) -> ui::canvas::AntsGeometry {
        if !self.selection_edges_visible() {
            return ui::canvas::AntsGeometry::default();
        }
        crate::presenter::selection_ants(outline, doc, time_secs, style)
    }

    /// W3-A: what the View ▸ Extras pass drew on the last frame.
    pub fn extras_report(&self) -> crate::ExtrasReport {
        self.extras.last_report()
    }

    /// W8-A: the brush ring's position source. `Some` is a pen's window
    /// position in physical pixels (hovering in range, or in contact), which
    /// the ring follows; `None` follows egui's pointer (the mouse).
    pub fn set_pen_hover(&mut self, at: Option<glam::Vec2>) {
        self.extras.set_pen_hover(at);
    }

    /// Whether a modal dialog is open this frame. The shell suppresses the
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

    /// The thumbnail cache, for tests that count what one frame composited.
    #[cfg(test)]
    pub(crate) fn thumb_cache_for_test(&self) -> &crate::doc::LayerThumbCache {
        &self.thumbs
    }

    /// Keep one fitted thumbnail per layer (and per mask) uploaded in the
    /// workspace, so the Layers panel draws real pixels instead of a kind
    /// glyph.
    ///
    /// Called every frame, and cheap on a frame where nothing changed: each
    /// layer's [`crate::doc::LayerThumbCache::layer_fingerprint`] is compared
    /// with the stored thumbnail's and the compositor runs only for the
    /// layers whose fingerprint moved (a paint, a parameter, an effect, a
    /// mask). At most [`THUMBS_PER_FRAME`] thumbnails are recomposited per
    /// frame — a ten-layer stack never stalls one frame, the rest catch up
    /// over the next frames, and a repaint is requested while any is owed.
    /// A changed thumbnail is written into its existing egui texture with
    /// `set`; a texture is created only for a layer that has none yet.
    fn refresh_layer_thumbs(&mut self, ctx: &egui::Context, editor: &Editor) {
        let Some(open) = editor.active() else {
            self.workspace.layer_thumbs.clear();
            self.workspace.mask_thumbs.clear();
            self.thumbs.clear();
            self.thumbs_document = None;
            return;
        };
        if self.thumbs_document != Some(open.id()) {
            self.workspace.layer_thumbs.clear();
            self.workspace.mask_thumbs.clear();
            self.workspace.filter_mask_thumbs.clear();
            self.thumbs.clear();
            self.thumbs_document = Some(open.id());
        }
        // W5-F: the per-layer fingerprint pass serialises every layer (and a
        // group's whole subtree) — skip it outright while nothing that can
        // change a thumbnail has moved since a pass that left nothing owed.
        let signature = document_signature(open);
        let checked = (
            signature,
            open.id(),
            self.workspace.layer_thumbs.len(),
            self.workspace.mask_thumbs.len(),
        );
        if self.thumbs_checked_at == Some(checked) {
            return;
        }
        self.thumb_passes += 1;
        let ids = open.document.layers.iter_depth_first();
        let live: std::collections::HashSet<LayerId> = ids.iter().copied().collect();
        self.workspace
            .layer_thumbs
            .retain(|id, _| live.contains(id));
        self.workspace.mask_thumbs.retain(|id, _| live.contains(id));
        self.workspace
            .filter_mask_thumbs
            .retain(|id, _| live.contains(id));
        self.thumbs.retain(&live);

        let mut budget = THUMBS_PER_FRAME;
        let mut owed = false;
        for id in ids {
            // The layer's own pixels.
            let current = self.thumbs.layer_is_current(open, id, THUMB_EDGE)
                && self.workspace.layer_thumbs.contains_key(&id);
            if !current {
                if budget == 0 {
                    owed = true;
                } else if let Ok(thumb) = self.thumbs.layer_thumbnail(open, id, THUMB_EDGE) {
                    if thumb.fresh {
                        budget -= 1;
                    }
                    let img = egui::ColorImage::from_rgba_unmultiplied(
                        [thumb.width as usize, thumb.height as usize],
                        thumb.rgba,
                    );
                    Self::upload_thumb(
                        ctx,
                        &mut self.workspace.layer_thumbs,
                        id,
                        "layer-thumb",
                        img,
                    );
                }
            }
            // W10-I: a smart object's filter-mask thumbnail, rebuilt only
            // when its fingerprint moved.
            crate::menu_bridge::layer_extras::refresh_filter_mask_thumb(
                ctx,
                &mut self.workspace.filter_mask_thumbs,
                open,
                id,
                THUMB_EDGE,
            );
            // Card 059: the mask's real coverage thumbnail, for layers that
            // have a mask. The well falls back to the glyph without it.
            let has_mask = open
                .document
                .layers
                .get(id)
                .is_some_and(|l| l.mask.is_some());
            if !has_mask {
                self.workspace.mask_thumbs.remove(&id);
                self.thumbs.forget_mask(id);
                continue;
            }
            let current = self.thumbs.mask_is_current(open, id, THUMB_EDGE)
                && self.workspace.mask_thumbs.contains_key(&id);
            if !current {
                if budget == 0 {
                    owed = true;
                } else if let Ok(thumb) = self.thumbs.mask_thumbnail(open, id, THUMB_EDGE) {
                    if thumb.fresh {
                        budget -= 1;
                    }
                    let img = egui::ColorImage::from_rgba_unmultiplied(
                        [thumb.width as usize, thumb.height as usize],
                        thumb.rgba,
                    );
                    Self::upload_thumb(ctx, &mut self.workspace.mask_thumbs, id, "mask-thumb", img);
                }
            }
        }
        self.thumbs_checked_at = (!owed).then(|| {
            (
                signature,
                open.id(),
                self.workspace.layer_thumbs.len(),
                self.workspace.mask_thumbs.len(),
            )
        });
        if owed {
            // Thumbnails still to recomposite: the next frame takes the next
            // batch, without waiting for the user to move the pointer.
            ctx.request_repaint();
        }
    }

    /// W3-J: the Properties panel's Transform block measures a raster
    /// layer by its alpha ink, which only the tile bytes held here can
    /// answer. Measure the active layer (and, for a group, the layers under
    /// it) through the compositor's hash-cached `alpha_bounds` and hand the
    /// result to the panel.
    ///
    /// W3-X: only the Transform block reads it, and the block is closed by
    /// default, so this runs only when the block was drawn last frame
    /// ([`ui::Workspace::take_transform_block_drawn`]) AND the editor
    /// revision or the active layer moved since the last measurement — never
    /// per frame. The block asks for a repaint when it finds no current
    /// measurement, so opening it costs one frame, not a pointer move.
    fn publish_raster_inks(&mut self, ctx: &egui::Context, editor: &Editor) {
        if !self.workspace.take_transform_block_drawn() {
            return;
        }
        let key = (
            editor.revision(),
            editor.active_index(),
            editor
                .active()
                .and_then(|open| open.document.active_layer()),
        );
        if self.inks_published_at == Some(key) {
            return;
        }
        self.inks_published_at = Some(key);
        self.ink_publishes += 1;
        editor
            .active()
            .and_then(|open| {
                let id = open.document.active_layer()?;
                Some(ui::panels::properties::RasterInks::measure(
                    &open.document,
                    &open.tiles,
                    id,
                ))
            })
            .unwrap_or_default()
            .publish(ctx);
    }

    /// W2-X: the bounded downsample of the active composite the Navigator
    /// draws under its view box and the Histogram counts, rebuilt only when
    /// [`Editor::revision`] moves — never per frame — and read from the
    /// canvas in bands (see [`composite_preview`]) so the peak buffer is one
    /// band rather than the whole canvas.
    ///
    /// W5-F: keyed on [`Editor::content_revision`] (and the document), not
    /// [`Editor::revision`]: the colour wells, the brush and the status line
    /// move the latter on every frame of a slider drag without changing a
    /// pixel, and each of those frames used to read the whole canvas.
    fn refresh_composite_preview(&mut self, ctx: &egui::Context, editor: &mut Editor) {
        let content = editor.content_revision();
        let Some(open) = editor.active_mut() else {
            self.workspace.clear_composite_preview();
            self.preview_revision = None;
            ui::panels::history::HistoryThumbs::clear(ctx);
            return;
        };
        let key = (content, open.id());
        if self.preview_revision == Some(key) && self.workspace.navigator_texture.is_some() {
            return;
        }
        self.preview_builds += 1;
        // The Histogram's generation: unique per build, so a switch between
        // two documents at the same content revision still recounts.
        let revision = self.preview_builds as u64;
        let Some((w, h, small)) = composite_preview(open, PREVIEW_EDGE) else {
            self.workspace.clear_composite_preview();
            self.preview_revision = Some(key);
            return;
        };
        // W4-I: the same picture becomes the History panel's thumbnail of
        // the row the document is at now (the stored ones follow their
        // states when the stack is rewritten or compacted).
        ui::panels::history::HistoryThumbs::capture(
            ctx,
            open.id().0,
            &open.history,
            [w as usize, h as usize],
            &small,
        );
        self.workspace
            .set_composite_preview(ctx, revision, w as usize, h as usize, &small);
        self.preview_revision = Some(key);
    }

    /// W2-X: the colour under the pointer for the Info panel, read through
    /// the same sampler the dialogs' eyedropper uses — a 1x1 composite, run
    /// only when the pointer or the revision moved. `None` with the pointer
    /// off the canvas area (over a dock, outside the window), which the rows
    /// draw as a dash. Called after [`Chrome::record_viewport`], because the
    /// sampler maps through this frame's geometry.
    fn refresh_info_sample(&mut self, ctx: &egui::Context, editor: &Editor) {
        use ui::dialogs::ScreenSampler as _;
        let pos = match self.frame_geometry {
            Some(frame) => ctx
                .pointer_latest_pos()
                .filter(|p| frame.content.contains(*p)),
            None => None,
        };
        let at = pos.map(|p| (editor.revision(), p));
        if at == self.info_sample_at {
            return;
        }
        let sampler = pos.and_then(|_| self.screen_sampler(editor));
        let sample = match (pos, &sampler) {
            (Some(p), Some(s)) => s.sample([p.x, p.y]),
            _ => None,
        };
        self.workspace.info.pointer = pos.and_then(|p| self.pointer_document_point(editor, p));
        self.workspace.set_info_sample(sample);
        self.info_sample_at = at;
    }

    /// Where the pointer is on the document, in document pixels, for the
    /// Info panel's Pointer row: the same window-to-document mapping the
    /// sampler and the tool route use. `None` off the image.
    fn pointer_document_point(&self, editor: &Editor, pos: egui::Pos2) -> Option<(f32, f32)> {
        let frame = self.frame_geometry?;
        let doc = editor.active()?;
        let viewport = crate::tool_input::canvas_viewport(&doc.camera);
        let mirror = crate::tool_input::canvas_camera_of(&doc.camera);
        let pt = mirror.doc_of_screen_pt(
            &viewport,
            glam::Vec2::new(pos.x * frame.ppp, pos.y * frame.ppp),
        );
        let (w, h) = (doc.document.width() as f32, doc.document.height() as f32);
        (pt.x >= 0.0 && pt.y >= 0.0 && pt.x < w && pt.y < h).then_some((pt.x, pt.y))
    }

    /// Put `img` in the layer's texture: written in place when the layer
    /// already has one (the panel keeps drawing the same texture id), created
    /// otherwise.
    fn upload_thumb(
        ctx: &egui::Context,
        slot: &mut std::collections::HashMap<LayerId, egui::TextureHandle>,
        id: LayerId,
        kind: &str,
        img: egui::ColorImage,
    ) {
        match slot.get_mut(&id) {
            Some(tex) => tex.set(img, egui::TextureOptions::NEAREST),
            None => {
                let tex =
                    ctx.load_texture(format!("{kind}-{id}"), img, egui::TextureOptions::NEAREST);
                slot.insert(id, tex);
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

    /// Card 059: how the active layer's mask should reach the canvas.
    ///
    /// Like channel isolation, a VIEW setting: the Layers panel owns it (the
    /// mask well's popup writes it), this chrome reads it out every frame,
    /// and the presenter applies it on the composite's way to the GPU —
    /// never to the document, which is what keeps a mask-only view out of
    /// exports.
    pub fn mask_view(&self) -> ui::MaskViewMode {
        self.workspace.mask_view
    }

    /// W13-M: set the mask view from the keyboard (Photopea's `\` and `` ` ``,
    /// [`crate::shell`]'s `mask_view_key`); the same field the mask well's
    /// popup and its Alt+click write.
    pub fn set_mask_view(&mut self, mode: ui::MaskViewMode) {
        self.workspace.mask_view = mode;
    }

    /// Card 059: the overlay tint, from the theme's accent token — the same
    /// palette the chrome paints with, resolved for the theme the user is
    /// in, not a hard-coded colour in the painter.
    pub fn mask_overlay_tint(&self) -> [u8; 3] {
        let tokens = self.workspace.theme.tokens();
        let c = tokens.palette.color(design::ColorRole::Accent);
        [c.r, c.g, c.b]
    }

    /// W7-D: View > Proof Colors / Gamut Warning, as the presenter applies
    /// them. Like [`Self::mask_view`], a VIEW setting: the View menu's
    /// checkmarks are the authority, and the gamut warning paints with the
    /// theme's Warning token rather than a colour hard-coded in the painter.
    pub fn proof_view(&self) -> crate::presenter::ProofView {
        let flags = &self.workspace.view_flags;
        let tokens = self.workspace.theme.tokens();
        let c = tokens.palette.color(design::ColorRole::Warning);
        crate::presenter::ProofView {
            proof_colors: flags.get(ui::ViewFlag::ProofColors),
            gamut_warning: flags.get(ui::ViewFlag::GamutWarning),
            warning: [c.r, c.g, c.b],
        }
    }

    /// Draw one frame of chrome.
    pub fn ui(&mut self, ctx: &egui::Context, editor: &mut Editor) -> ChromeOutput {
        let mut out = ChromeOutput::default();
        self.read_gesture(ctx);
        self.sync_workspace(editor);
        self.refresh_layer_thumbs(ctx, editor);
        // W4-I: the Actions panel's clicks and library, and the Swatches /
        // Brushes lists kept in the preferences file (restored on start).
        editor.sync_actions_panel(ctx);
        editor.sync_panel_presets(&mut self.workspace);
        // View > Flip Horizontal / Vertical: the checkmarks are the
        // authority, the document camera is what the renderer and the pointer
        // read. A change owes one more frame so the picture follows at once.
        if let Some(doc) = editor.active_mut() {
            if crate::tool_input::apply_view_flips(self.workspace.view_flags, &mut doc.camera) {
                ctx.request_repaint();
            }
        }
        self.refresh_composite_preview(ctx, editor);
        self.publish_raster_inks(ctx, editor);
        // W2-X: Photopea's F. Both full-screen modes drop the options bar,
        // the tool column and the docks; the last drops the menu bar too. The
        // editor's Tab flag still hides the panels on its own in Standard.
        let mode = editor.screen_mode();
        let panels = editor.panels_visible() && mode.panels_visible();

        // Order matters, and it is egui's: each panel gets what the previously
        // added ones left. Photopea's chrome, top to bottom: the menu bar and
        // the options bar span the full width; the tool column and the docks
        // take the sides; the tab strip sits *inside* what is left, over the
        // canvas alone — never across the tool column or the docks; the
        // status strip is the bottom band. The `ui` crate's own surfaces are
        // driven from the workspace this chrome owns, and every control in
        // them posts an intent `harvest` translates below.
        if mode.menu_visible() {
            self.menu_bar(ctx, editor, &mut out);
        }
        if panels {
            ui::view::tool_options(&mut self.workspace, ctx);
        }
        self.status_bar(ctx, editor, &mut out);
        if panels {
            ui::view::tool_palette(&mut self.workspace, ctx);
            match editor.active() {
                Some(open) => {
                    ui::view::docks(&mut self.workspace, ctx, &open.document, &open.history);
                }
                None => {
                    // Photopea keeps its panels up with nothing open: the
                    // headers over an empty body, not an empty right half.
                    // Drawn against a placeholder with no layers, so the
                    // Layers panel has no rows — and nothing a control emits
                    // can reach a document; see `drop_document_intents`.
                    let (doc, history) = self.empty_dock.get_or_insert_with(|| {
                        (
                            editor_core::Document::new(0, 0, ""),
                            editor_core::History::new(),
                        )
                    });
                    ui::view::docks(&mut self.workspace, ctx, doc, history);
                    self.drop_document_intents();
                }
            }
        }
        // The tab strip, in the room the docks left: over the canvas only, and
        // only when there is a document to name. With none, the start screen
        // is the empty state and there is no dead band above it.
        if !editor.documents().is_empty() {
            self.tab_strip(ctx, editor, &mut out);
        }
        self.start_screen(ctx, editor, &mut out);
        // W3-A: View ▸ Extras over the composite, in the rectangle the docks
        // left and under the live session's handles. Painted on a background
        // layer, so an open dialog and its scrim sit over them; with one up
        // they take no pointer (`CanvasExtras::paint`, *Layering*).
        if !editor.documents().is_empty() {
            // W13-F: View > Pattern Preview, on the same layer, under them.
            crate::menu_bridge::menu_w13f::paint_pattern_preview(
                ctx,
                self.workspace.view_flags,
                editor,
            );
            let modal_open = self.dialogs.is_open();
            self.extras
                .paint(ctx, &mut self.workspace, editor, modal_open);
        }
        // The live tool session's overlays, over the canvas the surface shows
        // (card 012). After the docks, so the canvas rectangle is what the
        // docks left; clipped to it, because a panel must never grow handles.
        self.paint_live_tool_geometry(ctx, editor);
        self.paint_live_readout(ctx, editor);
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
        self.copy_layers_across(editor);
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
        // The Info panel's colour under the pointer, read through the frame
        // just recorded — the same route the dialogs' eyedropper takes.
        self.refresh_info_sample(ctx, editor);
        // W4-G: and the colour under each Colour Sampler point.
        self.refresh_sampler_colours(editor);
        // The right-click menu floats above everything; its rows post intents
        // the harvest below turns into actions the same frame.
        let menu_ctx = crate::menu_bridge::context(editor, &self.workspace);
        ui::context_menu::draw_open(&mut self.workspace, ctx, &menu_ctx);
        // W11-G: Help > Search Commands resolves its rows against this.
        self.dialogs.set_menu_context(&menu_ctx);
        self.channel_chords(ctx, editor);
        self.harvest(editor, &mut out);
        // W3-A: a View toggle this build cannot honour was refused rather
        // than ticked; say why, where the user is looking.
        if let Some(reason) = self.refused_view_flag.take() {
            editor.set_status(reason);
        }
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
        // W3-A: a guide being dragged on the canvas converges once, on the
        // drop — one drag is one undo step, not one per frame it moved.
        if self.extras.is_dragging_guide() {
            return;
        }
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
        // Card 059: a mask-well popup belongs to the document it opened on —
        // switching tabs must not leave it anchored over a layer that is not
        // on this document's stack.
        let active_doc = editor.active().map(|d| d.id());
        if self.showing_document != active_doc {
            self.showing_document = active_doc;
            w.layers.mask_menu = None;
            w.layers.mask_menu_fresh = false;
        }
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
        // W2-X: the footer's Q and F draw the editor's modes — whichever
        // route moved them (the footer, the chord, the Select menu) — and
        // never their own last click.
        w.palette.quick_mask = editor.quick_mask();
        w.palette.screen_mode = editor.screen_mode();
        // W5-D round 2: the Layer|Mask toggle, the thumbnail target border
        // and the Properties subject draw the editor's validated edit target
        // — whichever route moved it (Layers mask button, Layer > Layer Mask,
        // a thumbnail click, undo removing the mask) — never a stale focus.
        w.property_focus = if editor.edit_target_is_mask() {
            ui::panels::properties::PropertyFocus::Mask
        } else {
            ui::panels::properties::PropertyFocus::Layer
        };
        // W10-I: the smart object whose filter mask edits aim at.
        w.filter_mask_target = editor
            .edit_target_filter_mask()
            .and_then(|_| editor.edit_target().map(|t| t.layer));
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

    /// Publish the live tool session's geometry into the canvas sessions the
    /// overlays are drawn from (card 012).
    ///
    /// Called by the shell every frame after the pointer route ran. `None` —
    /// or a geometry whose document is no longer in front — clears the
    /// published state, so a committed or cancelled gesture and a tab switch
    /// both remove the overlays with no second mechanism to forget. The
    /// publication is the shell's, never a test helper's: a test that wants
    /// geometry on screen drives a real gesture and calls this.
    pub fn publish_tool_geometry(
        &mut self,
        geometry: Option<(crate::doc::DocumentId, tools::SessionGeometry)>,
        active_document: Option<crate::doc::DocumentId>,
    ) {
        let sessions = &mut self.workspace.canvas.sessions;
        let geometry = geometry
            .filter(|(doc, _)| Some(*doc) == active_document)
            .map(|(_, geometry)| geometry);
        Self::publish_tool_info(&mut self.workspace.info, geometry.as_ref());
        match geometry {
            Some(tools::SessionGeometry::Transform {
                state,
                mode,
                active,
                layer: _,
            }) => {
                sessions.transform = Some((state, mode));
                sessions.active_handle = active;
                self.live_session = None;
            }
            other => {
                if sessions.transform.is_some() {
                    sessions.transform = None;
                    sessions.active_handle = None;
                }
                // W4-A: a crop box and a pen path also go into the canvas
                // sessions, so the guide grab bands stand down under their
                // handles exactly as they do under a transform's.
                let crop = match &other {
                    Some(tools::SessionGeometry::Crop { rect, .. }) => {
                        Some(ui::canvas::DocRect::from_corners(rect[0], rect[1]))
                    }
                    _ => None,
                };
                let path = match &other {
                    Some(tools::SessionGeometry::Path {
                        anchors, handles, ..
                    }) => Some((Self::live_path_topology(anchors, handles), Vec::new())),
                    _ => None,
                };
                if sessions.crop != crop {
                    sessions.crop = crop;
                }
                if sessions.path != path {
                    sessions.path = path;
                }
                self.live_session = other;
            }
        }
        // W4-I: the Paths panel's Work Path row follows the pen's
        // uncommitted path through the same publication.
        self.workspace.paths.follow_pen(self.live_session.as_ref());
    }

    /// W4-A: the non-transform session geometry the overlay paints this frame
    /// — what a shell-level test reads to prove Escape and Enter take a crop
    /// box, pen path, slice set or lasso outline down with no pointer sample.
    #[cfg(test)]
    pub(crate) fn live_session(&self) -> Option<&tools::SessionGeometry> {
        self.live_session.as_ref()
    }

    /// W4-G: perform an options-bar confirm ([`ChromeOutput::confirm_tool`],
    /// the Ruler's Straighten Layer button) — Enter by another door: the live
    /// tool's held gesture is committed through
    /// [`crate::tool_input::ToolPointer::commit`], the preview settles, and
    /// the consumed geometry is un-published now rather than on the next
    /// pointer sample. The shell calls this from its chrome-output step.
    pub fn confirm_tool(
        &mut self,
        pointer: &mut crate::tool_input::ToolPointer,
        editor: &mut Editor,
    ) -> crate::tool_input::CommitOutcome {
        let outcome = pointer.commit(editor);
        pointer.settle_preview(editor);
        let geometry = pointer.live_geometry();
        self.publish_tool_geometry(geometry, editor.active().map(|d| d.id()));
        outcome
    }

    /// W4-G: the Ruler's line, from the same published geometry the canvas
    /// draws, into the Info panel's Distance and Angle rows. (The sampler rows
    /// come from the document itself: see [`Chrome::refresh_sampler_colours`].)
    fn publish_tool_info(
        info: &mut ui::panels::navigator::InfoState,
        geometry: Option<&tools::SessionGeometry>,
    ) {
        info.measure = match geometry {
            Some(tools::SessionGeometry::Measure { start, end }) => {
                Some(tools::measure::Measurement {
                    start: *start,
                    end: *end,
                })
            }
            _ => None,
        };
    }

    /// W4-G: the Info panel's #1..#4 rows, from the active document's Colour
    /// Sampler points whatever tool is selected (the points are the
    /// document's, so they outlive a tool switch), each with the colour of a
    /// 1x1 composite at it — an edit under a point shows in its row on the
    /// next frame. No document, no rows.
    ///
    /// W5-F: each readout is a 1x1 composite, which the compositor quantises
    /// to a whole tile — so the rows are re-read only when the document's
    /// content ([`document_signature`]), the document or a point moved,
    /// never on an idle frame or a colour-well drag.
    fn refresh_sampler_colours(&mut self, editor: &Editor) {
        let Some(doc) = editor.active() else {
            self.workspace.info.samplers.clear();
            self.samplers_read_at = None;
            return;
        };
        if doc.samplers().is_empty() && self.workspace.info.samplers.is_empty() {
            self.samplers_read_at = None;
            return;
        }
        let key = (document_signature(doc), doc.id(), doc.samplers().to_vec());
        if self.samplers_read_at.as_ref() == Some(&key)
            && self.workspace.info.samplers.len() == key.2.len()
        {
            return;
        }
        self.sampler_composites += doc.samplers().len();
        self.workspace.info.samplers = doc
            .samplers()
            .iter()
            .map(|p| ui::panels::navigator::SamplerReadout {
                position: (p.x, p.y),
                color: document_colour_at(doc, p.x.floor() as i64, p.y.floor() as i64),
            })
            .collect();
        self.samplers_read_at = Some(key);
    }

    /// XB: publish (or clear, with `None`) the live tool's pointer readout —
    /// the shape tools' W/H. The shell calls this with
    /// [`crate::tool_input::ToolPointer::live_readout`] after every pointer
    /// sample it hands the tools (so a release clears it) and after
    /// abandoning a gesture on Escape or focus loss (so a cancel clears it).
    pub fn publish_tool_readout(
        &mut self,
        readout: Option<(crate::doc::DocumentId, tools::tool::LiveReadout)>,
    ) {
        self.live_readout = readout;
    }

    /// XB: the W/H label beside the pointer while a shape is dragged
    /// (Photopea's cursor readout), in the Units preference, token-styled, on
    /// the overlay layer over the canvas. Nothing is painted without a
    /// published readout or when it belongs to a document not in front.
    fn paint_live_readout(&self, ctx: &egui::Context, editor: &Editor) {
        let Some((doc_id, readout)) = self.live_readout else {
            return;
        };
        let Some(doc) = editor.active().filter(|d| d.id() == doc_id) else {
            return;
        };
        let camera = crate::tool_input::canvas_camera_of(&doc.camera);
        let viewport = crate::tool_input::canvas_viewport(&doc.camera);
        let pointer =
            crate::interaction_geometry::document_to_screen(&camera, &viewport, readout.anchor);
        if !pointer.is_finite() {
            return;
        }
        let unit = match editor.display_unit() {
            ui::dialogs::Unit::Percent => ui::dialogs::Unit::Pixels,
            other => other,
        };
        let ppi = ui::dialogs::units::DEFAULT_PPI;
        let decimals = unit.decimals();
        let line = |key: &str, px: f32| {
            let px = f64::from(px);
            let value = unit.from_pixels(px, ppi, px);
            format!(
                "{}: {value:.decimals$} {}",
                ui::strings::tr(key),
                unit.short()
            )
        };
        let text = format!(
            "{}\n{}",
            line("ui.chrome.readout.width", readout.width_px),
            line("ui.chrome.readout.height", readout.height_px)
        );
        let tokens = design::current_theme(ctx).tokens();
        let painter = ctx
            .layer_painter(crate::canvas_extras::overlay_layer())
            .with_clip_rect(ctx.available_rect());
        let galley = painter.layout_no_wrap(
            text,
            design::egui_theme::font_id(tokens, TypeRole::Caption),
            design::color32(tokens.palette.text(TextRole::Primary)),
        );
        let pad = Space::Small.pt();
        let offset = Space::Large.pt();
        let rect = egui::Rect::from_min_size(
            egui::pos2(pointer.x + offset, pointer.y + offset),
            galley.size() + egui::vec2(pad * 2.0, pad * 2.0),
        );
        painter.rect(
            rect,
            design::egui_theme::rounding(
                design::Radius::Small.resolve(&tokens.radii, rect.height()),
            ),
            design::color32(tokens.palette.color(design::ColorRole::SurfaceOverlay)),
            egui::Stroke::new(
                tokens.borders.hairline,
                design::color32(tokens.palette.color(design::ColorRole::SeparatorHairline)),
            ),
        );
        let color = design::color32(tokens.palette.text(TextRole::Primary));
        painter.galley(rect.min + egui::vec2(pad, pad), galley, color);
    }

    /// Paint the published live geometry over the canvas (card 012).
    ///
    /// The image is a wgpu composite behind egui and `CanvasHost::
    /// central_panel` is never drawn by this shell, so this is the route the
    /// overlays actually appear through: the same painters the canvas view
    /// uses (`ui::canvas::paint`), against the same camera the surface was
    /// rendered with, clipped to the rectangle the docks left.
    fn paint_live_tool_geometry(&mut self, ctx: &egui::Context, editor: &Editor) {
        use ui::canvas::{handles::HandleLayout, paint, style::CanvasStyle};

        let transform = self.workspace.canvas.sessions.transform.clone();
        // W4-G: the document's Colour Sampler points are marked whatever tool
        // is selected; they are not a tool session.
        let samplers: Vec<glam::Vec2> = editor
            .active()
            .map(|d| d.samplers().to_vec())
            .unwrap_or_default();
        // W8-C: the Type Mask overlay lives exactly as long as its session.
        let type_mask_layer = match &self.live_session {
            Some(tools::SessionGeometry::TypeMask { layer }) => Some(*layer),
            _ => None,
        };
        if type_mask_layer.is_none() {
            self.type_mask_overlay = None;
        }
        if transform.is_none() && self.live_session.is_none() && samplers.is_empty() {
            return;
        }
        let Some(doc) = editor.active() else {
            return;
        };
        let camera = crate::tool_input::canvas_camera_of(&doc.camera);
        let viewport = crate::tool_input::canvas_viewport(&doc.camera);
        let style = CanvasStyle::from_context(ctx);
        let layout = HandleLayout::default();
        // The extras' layer, painted after them: the handles sit over the
        // grid and guides deterministically (one layer draws in order), and
        // an open dialog and its scrim sit over both.
        let mut painter = ctx.layer_painter(crate::canvas_extras::overlay_layer());
        painter.set_clip_rect(ctx.available_rect());
        if let Some((state, mode)) = transform {
            let active = self.workspace.canvas.sessions.active_handle;
            let session = ui::canvas::paint::TransformPaint {
                state: &state,
                mode,
                layout: &layout,
                active,
            };
            paint::transform(&painter, &camera, &viewport, &session, &style);
        }
        if let Some(layer) = type_mask_layer {
            self.refresh_type_mask_overlay(ctx, doc, layer);
            if let Some(overlay) = &self.type_mask_overlay {
                let rect = overlay.key.3;
                let (x0, y0) = (rect.x as f32, rect.y as f32);
                let (x1, y1) = (x0 + rect.width as f32, y0 + rect.height as f32);
                let mut mesh = egui::Mesh::with_texture(overlay.texture.id());
                for (p, uv) in [
                    (glam::Vec2::new(x0, y0), egui::pos2(0.0, 0.0)),
                    (glam::Vec2::new(x1, y0), egui::pos2(1.0, 0.0)),
                    (glam::Vec2::new(x1, y1), egui::pos2(1.0, 1.0)),
                    (glam::Vec2::new(x0, y1), egui::pos2(0.0, 1.0)),
                ] {
                    let s = camera.screen_pt_of(&viewport, p);
                    mesh.vertices.push(egui::epaint::Vertex {
                        pos: egui::pos2(s.x, s.y),
                        uv,
                        color: egui::Color32::WHITE,
                    });
                }
                mesh.add_triangle(0, 1, 2);
                mesh.add_triangle(0, 2, 3);
                painter.add(egui::Shape::mesh(mesh));
            }
        }
        if let Some(session) = self.live_session.as_ref() {
            let frame = LiveFrame {
                painter: &painter,
                camera: &camera,
                viewport: &viewport,
                style: &style,
                layout: &layout,
            };
            Self::paint_live_session(ctx, &frame, session);
        }
        if !samplers.is_empty() {
            let frame = LiveFrame {
                painter: &painter,
                camera: &camera,
                viewport: &viewport,
                style: &style,
                layout: &layout,
            };
            Self::paint_samplers(ctx, &frame, &samplers);
        }
    }

    /// W8-C: (re)build the Type Mask overlay for `layer` when its text or the
    /// canvas changed: the layer composited alone (the draft the canvas is
    /// showing), and the theme's red at [`QUICK_MASK_ALPHA`] everywhere its
    /// glyph coverage is not — Photoshop's quick-mask view of the selection
    /// the confirm will make.
    fn refresh_type_mask_overlay(
        &mut self,
        ctx: &egui::Context,
        doc: &crate::doc::OpenDocument,
        layer: LayerId,
    ) {
        let Some(layer_model::LayerKind::Text(text)) =
            doc.document.layers.get(layer).map(|l| &l.kind)
        else {
            self.type_mask_overlay = None;
            return;
        };
        let rect = doc.canvas_rect();
        let key = (doc.id(), layer, text.clone(), rect);
        if self
            .type_mask_overlay
            .as_ref()
            .is_some_and(|o| o.key == key)
        {
            return;
        }
        let Ok(rgba) = doc.layer_pixels(layer) else {
            self.type_mask_overlay = None;
            return;
        };
        let (w, h) = (rect.width.max(1), rect.height.max(1));
        let step = w.max(h).div_ceil(TYPE_MASK_OVERLAY_MAX_PX).max(1);
        let (ow, oh) = (w.div_ceil(step), h.div_ceil(step));
        let red = design::current_theme(ctx)
            .tokens()
            .palette
            .color(design::ColorRole::ChannelRed);
        let mut pixels = Vec::with_capacity((ow * oh) as usize);
        for oy in 0..oh {
            for ox in 0..ow {
                let (x, y) = ((ox * step).min(w - 1), (oy * step).min(h - 1));
                let a = rgba
                    .get(((y * w + x) * 4 + 3) as usize)
                    .copied()
                    .unwrap_or(0);
                let alpha = QUICK_MASK_ALPHA * (1.0 - f32::from(a) / 255.0);
                pixels.push(egui::Color32::from_rgba_unmultiplied(
                    red.r,
                    red.g,
                    red.b,
                    (alpha * 255.0).round() as u8,
                ));
            }
        }
        let image = egui::ColorImage {
            size: [ow as usize, oh as usize],
            pixels,
        };
        #[cfg(test)]
        let kept = image.clone();
        match &mut self.type_mask_overlay {
            Some(overlay) => {
                overlay.texture.set(image, egui::TextureOptions::NEAREST);
                overlay.key = key;
                #[cfg(test)]
                {
                    overlay.image = kept;
                }
            }
            None => {
                let texture =
                    ctx.load_texture("type-mask-overlay", image, egui::TextureOptions::NEAREST);
                self.type_mask_overlay = Some(TypeMaskOverlay {
                    key,
                    texture,
                    #[cfg(test)]
                    image: kept,
                });
            }
        }
    }

    /// W8-C: the Type Mask overlay's pixels as last built — what a headless
    /// test reads to prove the red sits outside the glyphs.
    #[cfg(test)]
    pub(crate) fn type_mask_overlay_image(&self) -> Option<&egui::ColorImage> {
        self.type_mask_overlay.as_ref().map(|o| &o.image)
    }

    /// W4-G: each Colour Sampler point as a numbered crosshair.
    fn paint_samplers(ctx: &egui::Context, frame: &LiveFrame<'_>, points: &[glam::Vec2]) {
        let LiveFrame {
            painter,
            camera,
            viewport,
            style,
            ..
        } = *frame;
        let tokens = design::current_theme(ctx).tokens();
        let font = design::egui_theme::font_id(tokens, TypeRole::Caption);
        let stroke = style.hairline(style.path_stroke);
        let arm = Space::Small.pt();
        for (i, p) in points.iter().enumerate() {
            let at = camera.screen_pt_of(viewport, *p);
            if !at.is_finite() {
                continue;
            }
            let c = egui::pos2(at.x, at.y);
            for d in [egui::vec2(arm, 0.0), egui::vec2(0.0, arm)] {
                painter.line_segment([c - d, c + d], stroke);
            }
            painter.circle_stroke(c, arm * 0.5, stroke);
            painter.text(
                c + egui::vec2(arm, arm),
                egui::Align2::LEFT_TOP,
                format!("{}", i + 1),
                font.clone(),
                style.path_stroke,
            );
        }
    }

    /// W4-A: paint a live session that is not a transform, with the `ui`
    /// canvas painters: the crop box with its shaded surround and thirds
    /// guide, a marquee's marching rubber band, a lasso's outline, a pen
    /// path's segments, anchors and handles (plus the rubber segment to the
    /// pointer between presses), and the numbered slice regions.
    fn paint_live_session(
        ctx: &egui::Context,
        frame: &LiveFrame<'_>,
        session: &tools::SessionGeometry,
    ) {
        use ui::canvas::paint;
        let LiveFrame {
            painter,
            camera,
            viewport,
            style,
            layout,
        } = *frame;
        let to_screen = |p: glam::Vec2| camera.screen_pt_of(viewport, p);
        let pos = |v: glam::Vec2| egui::pos2(v.x, v.y);
        // The rubber segment follows the pointer only between presses: while
        // the button is held the gesture itself is at the pointer.
        let hover = ctx.input(|i| {
            if i.pointer.primary_down() {
                None
            } else {
                i.pointer.hover_pos()
            }
        });
        let ants = ui::canvas::AntsStyle::default();
        let phase = ui::canvas::ants_phase(ctx.input(|i| i.time), &ants);
        match session {
            tools::SessionGeometry::Transform { .. } => {}
            tools::SessionGeometry::Crop {
                rect,
                guide,
                straighten,
            } => {
                // W4-D: the Overlay the options bar chose, drawn as asked.
                let guide = ui::canvas::CropGuide::from(*guide);
                let overlay = ui::canvas::crop::build(
                    ui::canvas::DocRect::from_corners(rect[0], rect[1]),
                    camera,
                    viewport,
                    guide,
                    layout.handle_pt,
                );
                paint::crop(painter, &overlay, style);
                // W4-D round 2: the Straighten line as it is dragged, and the
                // rotation it asks for, by its far end.
                if let Some([from, to]) = straighten {
                    let (a, b) = (to_screen(*from), to_screen(*to));
                    if a.is_finite() && b.is_finite() {
                        painter.line_segment([pos(a), pos(b)], style.hairline(style.path_stroke));
                        if let Some(angle) = tools::edit::straighten_angle(*from, *to) {
                            let tokens = design::current_theme(ctx).tokens();
                            let font = design::egui_theme::font_id(tokens, TypeRole::Caption);
                            let pad = Space::Small.pt();
                            painter.text(
                                pos(b) + egui::vec2(pad, pad),
                                egui::Align2::LEFT_TOP,
                                format!("{:.1}°", angle.to_degrees()),
                                font,
                                style.path_stroke,
                            );
                        }
                    }
                }
            }
            tools::SessionGeometry::Marquee { shape, rect } => {
                let mut outline: Vec<glam::Vec2> = match shape {
                    tools::select::MarqueeShape::Ellipse => {
                        let centre = (rect[0] + rect[1]) * 0.5;
                        let radii = (rect[1] - rect[0]) * 0.5;
                        (0..LIVE_ELLIPSE_STEPS)
                            .map(|i| {
                                let t =
                                    std::f32::consts::TAU * i as f32 / LIVE_ELLIPSE_STEPS as f32;
                                to_screen(centre + radii * glam::Vec2::new(t.cos(), t.sin()))
                            })
                            .collect()
                    }
                    _ => ui::canvas::DocRect::from_corners(rect[0], rect[1])
                        .corners()
                        .into_iter()
                        .map(to_screen)
                        .collect(),
                };
                if let Some(first) = outline.first().copied() {
                    outline.push(first);
                }
                paint::ants(painter, &marching(outline, &ants, phase), style);
            }
            tools::SessionGeometry::Lasso { points, closed } => {
                let mut outline: Vec<glam::Vec2> = points.iter().copied().map(to_screen).collect();
                if *closed {
                    if let Some(first) = outline.first().copied() {
                        outline.push(first);
                    }
                } else if let Some(h) = hover {
                    outline.push(glam::Vec2::new(h.x, h.y));
                }
                paint::ants(painter, &marching(outline, &ants, phase), style);
            }
            tools::SessionGeometry::Path {
                anchors,
                handles,
                closing,
            } => {
                let n = anchors.len().min(handles.len());
                if n == 0 {
                    return;
                }
                let mut line: Vec<egui::Pos2> = vec![pos(to_screen(anchors[0]))];
                let mut join = |i: usize, j: usize| {
                    let (a, b) = (anchors[i], anchors[j]);
                    let (c1, c2) = (handles[i][1], handles[j][0]);
                    if c1 == a && c2 == b {
                        line.push(pos(to_screen(b)));
                        return;
                    }
                    for k in 1..=LIVE_CURVE_STEPS {
                        let t = k as f32 / LIVE_CURVE_STEPS as f32;
                        let u = 1.0 - t;
                        let p = a * (u * u * u)
                            + c1 * (3.0 * u * u * t)
                            + c2 * (3.0 * u * t * t)
                            + b * (t * t * t);
                        line.push(pos(to_screen(p)));
                    }
                };
                for i in 1..n {
                    join(i - 1, i);
                }
                if *closing && n > 2 {
                    join(n - 1, 0);
                } else if let Some(h) = hover {
                    line.push(h);
                }
                if line.len() > 1 && line.iter().all(|p| p.x.is_finite() && p.y.is_finite()) {
                    painter.add(egui::Shape::line(line, style.hairline(style.path_stroke)));
                }
                let topology = Self::live_path_topology(&anchors[..n], &handles[..n]);
                let projected = ui::canvas::paths::project(&topology, &[n - 1], camera, viewport);
                let control = Space::XSmall.pt();
                paint::path(painter, &projected, control * 1.5, control, style);
            }
            tools::SessionGeometry::Slices { rects } => {
                let tokens = design::current_theme(ctx).tokens();
                let font = design::egui_theme::font_id(tokens, TypeRole::Caption);
                let stroke = style.hairline(style.guide);
                let pad = Space::XSmall.pt();
                for (i, r) in rects.iter().enumerate() {
                    let (a, b) = (to_screen(r[0]), to_screen(r[1]));
                    if !a.is_finite() || !b.is_finite() {
                        continue;
                    }
                    let rect = egui::Rect::from_two_pos(pos(a), pos(b));
                    painter.rect_stroke(rect, egui::Rounding::ZERO, stroke);
                    painter.text(
                        rect.min + egui::vec2(pad, pad),
                        egui::Align2::LEFT_TOP,
                        format!("{:02}", i + 1),
                        font.clone(),
                        style.guide,
                    );
                }
            }
            // W10-A: the Slice Select tool's view of the committed set: each
            // slice labelled by its name (not its place in the set), the
            // picked one — what Delete and Slice Options act on — drawn with
            // the thick selected-handle stroke.
            tools::SessionGeometry::SliceSelect {
                rects,
                labels,
                picked,
            } => {
                let tokens = design::current_theme(ctx).tokens();
                let font = design::egui_theme::font_id(tokens, TypeRole::Caption);
                let pad = Space::XSmall.pt();
                for (i, r) in rects.iter().enumerate() {
                    let (a, b) = (to_screen(r[0]), to_screen(r[1]));
                    if !a.is_finite() || !b.is_finite() {
                        continue;
                    }
                    let rect = egui::Rect::from_two_pos(pos(a), pos(b));
                    let (stroke, ink) = if *picked == Some(i) {
                        (style.thick(style.handle_selected), style.handle_selected)
                    } else {
                        (style.hairline(style.guide), style.guide)
                    };
                    painter.rect_stroke(rect, egui::Rounding::ZERO, stroke);
                    painter.text(
                        rect.min + egui::vec2(pad, pad),
                        egui::Align2::LEFT_TOP,
                        labels.get(i).cloned().unwrap_or_default(),
                        font.clone(),
                        ink,
                    );
                }
            }
            // W8-C: the Perspective Crop quad — its outline, a perspective
            // grid through it and a handle square on each corner, the one
            // being dragged filled with the selected-handle colour.
            tools::SessionGeometry::PerspectiveCrop { quad, active } => {
                let corners: Vec<glam::Vec2> = quad.iter().copied().map(to_screen).collect();
                if !corners.iter().all(|c| c.is_finite()) {
                    return;
                }
                let mut outline: Vec<egui::Pos2> = corners.iter().map(|c| pos(*c)).collect();
                outline.push(outline[0]);
                painter.add(egui::Shape::line(
                    outline,
                    style.hairline(style.crop_outline),
                ));
                // Lines between matching points of opposite edges, in
                // document space so they follow the quad's perspective.
                let grid = style.hairline(style.crop_guide);
                let lerp = |a: glam::Vec2, b: glam::Vec2, t: f32| a + (b - a) * t;
                let n = PERSPECTIVE_GRID_LINES + 1;
                for k in 1..n {
                    let t = k as f32 / n as f32;
                    for (a0, a1, b0, b1) in [
                        (quad[0], quad[1], quad[3], quad[2]),
                        (quad[0], quad[3], quad[1], quad[2]),
                    ] {
                        let (p, q) = (to_screen(lerp(a0, a1, t)), to_screen(lerp(b0, b1, t)));
                        painter.line_segment([pos(p), pos(q)], grid);
                    }
                }
                let half = layout.handle_pt * 0.5;
                for (i, c) in corners.iter().enumerate() {
                    let square =
                        egui::Rect::from_center_size(pos(*c), egui::vec2(half * 2.0, half * 2.0));
                    let fill = if *active == Some(i) {
                        style.handle_selected
                    } else {
                        style.handle_fill
                    };
                    let rounding = egui::Rounding::same(style.handle_radius_pt);
                    painter.rect_filled(square, rounding, fill);
                    painter.rect_stroke(square, rounding, style.hairline(style.handle_stroke));
                }
            }
            // W8-C: painted by `paint_live_tool_geometry`, which holds the
            // document the overlay is built from.
            tools::SessionGeometry::TypeMask { .. } => {}
            // W4-G: the Ruler's line, with a cross at each end.
            tools::SessionGeometry::Measure { start, end } => {
                let (a, b) = (to_screen(*start), to_screen(*end));
                if !a.is_finite() || !b.is_finite() {
                    return;
                }
                let stroke = style.hairline(style.path_stroke);
                painter.line_segment([pos(a), pos(b)], stroke);
                let arm = Space::XSmall.pt();
                for p in [pos(a), pos(b)] {
                    for d in [egui::vec2(arm, 0.0), egui::vec2(0.0, arm)] {
                        painter.line_segment([p - d, p + d], stroke);
                    }
                }
            }
        }
    }

    /// W4-A: a pen path's anchors and handles as the `ui` path painter's
    /// topology — one open subpath, a control only where a handle is pulled.
    fn live_path_topology(
        anchors: &[glam::Vec2],
        handles: &[[glam::Vec2; 2]],
    ) -> ui::canvas::PathTopology {
        use ui::canvas::paths::{Anchor, ControlHandle, ControlSide};
        let mut topology = ui::canvas::PathTopology::default();
        for (index, (&doc, [h_in, h_out])) in anchors.iter().zip(handles).enumerate() {
            topology.anchors.push(Anchor {
                index,
                subpath: 0,
                doc,
                closes: false,
            });
            for (side, h) in [
                (ControlSide::Incoming, *h_in),
                (ControlSide::Outgoing, *h_out),
            ] {
                if h != doc {
                    topology.controls.push(ControlHandle {
                        anchor: index,
                        side,
                        doc: h,
                    });
                }
            }
        }
        topology
    }

    fn record_viewport(&mut self, ctx: &egui::Context) {
        let content = ctx.available_rect();
        let (w, h) = (content.width(), content.height());
        if w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0 {
            self.workspace.viewport = (w, h);
        }
        let style = ui::canvas::CanvasStyle::from_context(ctx);
        let canvas = if self.workspace.view_flags.get(ui::ViewFlag::Rulers) {
            let t = style.ruler_thickness_pt;
            let t = if t.is_finite() { t.max(0.0) } else { 0.0 };
            egui::Rect::from_min_max(content.min + egui::vec2(t, t), content.max)
        } else {
            content
        };
        let geometry = FrameGeometry {
            surface: ctx.screen_rect(),
            content,
            canvas,
            ppp: ctx.pixels_per_point(),
            style,
        };
        // The host is never drawn by this shell and therefore never learned any
        // of this on its own: left alone it frames documents against its default
        // 1280x720 viewport, whatever window it is really in. It is given the
        // canvas area, the same rectangle the document camera draws into.
        self.sync_canvas_host(geometry.canvas, &geometry);
        self.frame_geometry = Some(geometry);
    }

    /// Give `doc`'s camera the canvas area to draw into, on a `surface_px`
    /// surface: the last laid-out frame's area ([`Chrome::canvas_area_px`],
    /// cut back to the surface), or the whole surface before any frame has
    /// been laid out. The shell calls this every frame for the active document
    /// and for every document on a resize; see
    /// [`crate::interaction_geometry::CanvasPlacement`] for the fit rules.
    pub fn place_canvas(&mut self, doc: &mut crate::doc::OpenDocument, surface_px: glam::Vec2) {
        let measured = self.canvas_area_px().and_then(|a| a.within(surface_px));
        let (area, measured) = match measured {
            Some(area) => (area, true),
            None => (
                crate::interaction_geometry::CanvasArea::whole(surface_px),
                false,
            ),
        };
        self.canvas_placement.place(doc, area, measured);
    }

    /// The canvas area the last laid-out frame left between the panels, in
    /// physical surface pixels — the document camera's viewport. `None` until a
    /// frame has been drawn (the shell then falls back to the whole surface
    /// and keeps a new document's fit pending), or when the panels left no
    /// area at all.
    pub fn canvas_area_px(&self) -> Option<crate::interaction_geometry::CanvasArea> {
        let frame = self.frame_geometry?;
        let origin = (frame.canvas.min - frame.surface.min) * frame.ppp;
        let size = frame.canvas.size() * frame.ppp;
        (origin.x.is_finite()
            && origin.y.is_finite()
            && size.x.is_finite()
            && size.y.is_finite()
            && size.x >= 1.0
            && size.y >= 1.0)
            .then(|| crate::interaction_geometry::CanvasArea {
                origin: glam::Vec2::new(origin.x, origin.y),
                size: glam::Vec2::new(size.x, size.y),
            })
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
    /// The `ui` canvas host is synced to the canvas area every frame
    /// ([`Chrome::record_viewport`]) and the document camera the shell renders
    /// from is centred in that same area, so the host's framing lands the
    /// selection between the docks with no correction. The camera that the
    /// host moved is read back into `view_center` by `Workspace::absorb`, and
    /// `harvest` reports that to the shell; a refusal (nothing selected)
    /// moves nothing.
    fn frame_selection(&mut self) {
        let intent = ui::Intent::Action(ui::menu::MenuAction::Zoom(
            ui::menu::ZoomCommand::ToSelection,
        ));
        self.workspace.absorb(&intent);
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

    /// Route one intent into the frame's output — the one road every intent
    /// travels, whether a docked panel posted it or a menu row was clicked.
    ///
    /// A menu action the dialog host answers opens its dialog instead of
    /// being performed. The intent is consumed here — the confirmed value
    /// arrives through [`ChromeOutput::dialog`] or a dedicated channel once
    /// the dialog is confirmed, one or more frames later. Everything else is
    /// picked by the bridge and recorded; what the bridge cannot answer is
    /// reported, never dropped.
    fn route(&mut self, intent: ui::Intent, editor: &Editor, out: &mut ChromeOutput) {
        if let ui::Intent::Action(action) = &intent {
            // W3-X: a Channels saved-selection row names the row it loads.
            let load = match action {
                ui::menu::MenuAction::LoadSelection => self.workspace.take_pending_selection_load(),
                _ => None,
            };
            // W11-G: a blend-mode chord with a painting tool active sets the
            // tool's Mode, as Photoshop does; otherwise the layer's (below).
            let tool = editor.effective_tool();
            if let Some(pick) =
                crate::dialog_host::paint_chord_pick(*action, tool, &self.workspace.options)
            {
                pick.into_iter()
                    .for_each(|p| crate::menu_bridge::record(p, out));
                return;
            }
            if self.dialogs.open_for_menu_action_at(action, editor, load) {
                return;
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

    /// What a click on an enabled menu-bar row does: [`Self::route`] its
    /// intent, exactly as a panel control's intent is routed.
    ///
    /// This used to be `menu_bridge::record` straight into the output, which
    /// skipped the dialog host: Image Size, Canvas Size, Arbitrary rotation,
    /// Layer Style, the Filter Gallery and every Filter row went to
    /// `menu_bridge::perform` — which has no arm for a question only a dialog
    /// can answer — while the same rows in the context menu, arriving as
    /// workspace intents, opened their dialogs. One handler, so the two
    /// surfaces cannot disagree again;
    /// `menu_bridge::tests::every_enabled_menu_item_really_does_something`
    /// drives every enabled row through it.
    pub(crate) fn menu_click(
        &mut self,
        intent: ui::Intent,
        editor: &Editor,
        out: &mut ChromeOutput,
    ) {
        self.route(intent, editor, out);
    }

    /// The empty state's "controls disabled": the docks were just drawn
    /// against the placeholder document, so anything they emitted *at a
    /// document* — a layer command, a selection, a history jump, a camera
    /// move — has nowhere to land and is dropped here. Everything else (a
    /// panel opened or moved, a tool option, a colour) is the workspace's own
    /// and is put back for `harvest`, in order.
    fn drop_document_intents(&mut self) {
        let kept: Vec<ui::Intent> = self
            .workspace
            .drain_intents()
            .into_iter()
            .filter(|intent| !is_document_directed(intent))
            .collect();
        for intent in kept {
            self.workspace.emit(intent);
        }
    }

    fn harvest(&mut self, editor: &Editor, out: &mut ChromeOutput) {
        for intent in self.workspace.drain_intents() {
            self.route(intent, editor, out);
        }
        // W2-X: the palette footer's F has no menu item to raise an intent
        // for, so its click is a request the chrome answers with the
        // application's own action — performed by the shell against the
        // editor, and mirrored back next frame by `sync_workspace`.
        if self.workspace.palette.take_screen_mode_cycle() {
            out.actions.push(Action::CycleScreenMode);
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
                // W3-A: a flag this build cannot honour is refused with its
                // reason instead of ticked and ignored.
                ui::Intent::SetViewFlag { flag, on: true }
                    if view_flag_refusal(*flag).is_some() =>
                {
                    self.refused_view_flag = view_flag_refusal(*flag);
                }
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
                // W9-E: a Brushes-panel preset's tip and dynamics (no
                // options-bar keys) ride in on the brush it applies over.
                self.workspace
                    .brushes
                    .take_extras_over(tool, editor.brush_for(tool)),
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
    fn menu_bar(&mut self, ctx: &egui::Context, editor: &mut Editor, out: &mut ChromeOutput) {
        let context = crate::menu_bridge::context(editor, &self.workspace);
        // W11-G: a Help > Search Commands click routes inside `draw`.
        self.dialogs.set_menu_context(&context);
        let editor: &Editor = editor;
        crate::menu_bridge::draw(ctx, editor, &context, &mut |intent| {
            self.menu_click(intent, editor, out)
        });
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

    /// Widest a document tab may grow before its title truncates: two
    /// inspector labels, on the grid.
    pub fn tab_width(tokens: &design::Tokens) -> f32 {
        tokens.metrics.inspector_label_width * 2.0
    }

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
        let tokens = design::current_theme(ctx).tokens();
        let tab_width = Self::tab_width(tokens);
        let gap = Space::XSmall.pt();
        // Added after the tool column and the docks, so egui hands it only the
        // room they left: the strip runs over the canvas, not the window.
        egui::TopBottomPanel::top("raster-tabs")
            .frame(panel_frame(ctx, SurfaceRole::Header, Space::Hair))
            .show(ctx, |ui| {
                let total = editor.documents().len();
                // The chevron's room is always reserved, so the count of tabs
                // that fit cannot flip between frames as the chevron appears.
                let chevron = tokens.metrics.min_hit_target + gap;
                let room = (ui.available_width() - chevron).max(0.0);
                let fits = ((room + gap) / (tab_width + gap)).floor().max(1.0) as usize;
                let (first, last) = visible_tab_range(total, fits, editor.active_index());
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    for index in first..last {
                        let doc = &editor.documents()[index];
                        let tooltip = match doc.project_path() {
                            Some(p) => p.display().to_string(),
                            None => String::new(),
                        };
                        self.document_tab(ui, editor, index, doc.tab_label(), tooltip, out);
                    }
                    // A strip wider than the bar hides tabs; the chevron lists
                    // the hidden ones, and a row in that list activates it.
                    let hidden: Vec<usize> =
                        (0..total).filter(|i| !(first..last).contains(i)).collect();
                    if hidden.is_empty() {
                        self.tab_overflow_open = false;
                        return;
                    }
                    let chevron = ui::icons::ui_icon_button_id(
                        ui,
                        "chevron-down",
                        ui::strings::tr("ui.chrome.more.tabs"),
                        design::TextRole::Secondary,
                        Some(Self::tab_overflow_id()),
                    );
                    if chevron.clicked() {
                        self.tab_overflow_open = !self.tab_overflow_open;
                    }
                    if self.tab_overflow_open {
                        self.tab_overflow_menu(ui, editor, &hidden, chevron.rect, out);
                    }
                });
            });
    }

    /// The list the tab strip's chevron opens: one row per hidden document,
    /// in strip order. A click activates that document and closes the list.
    fn tab_overflow_menu(
        &mut self,
        ui: &mut egui::Ui,
        editor: &Editor,
        hidden: &[usize],
        anchor: egui::Rect,
        out: &mut ChromeOutput,
    ) {
        let tokens = design::current_tokens(ui);
        let width = Self::tab_width(tokens);
        let area = egui::Area::new(egui::Id::new("raster-tabs-overflow"))
            .order(egui::Order::Foreground)
            .fixed_pos(egui::pos2(
                anchor.right() - width,
                anchor.bottom() + Space::Hair.pt(),
            ))
            .show(ui.ctx(), |ui| {
                overlay_frame(ui.ctx()).show(ui, |ui| {
                    ui.set_min_width(width);
                    ui.set_max_width(width);
                    for &index in hidden {
                        let label = editor.documents()[index].tab_label();
                        let row = design::list_row(ui, &label, false);
                        // Marked with a stable id (hover only, so it never
                        // takes the click) so a test can name the row.
                        ui.interact(
                            row.rect,
                            Self::tab_overflow_item_id(index),
                            egui::Sense::hover(),
                        );
                        if row.clicked() {
                            out.activate = Some(index);
                            self.tab_overflow_open = false;
                        }
                    }
                });
            });
        // A press anywhere else dismisses it, like every popup.
        let pressed_outside = ui.input(|i| {
            i.pointer.any_pressed()
                && i.pointer
                    .interact_pos()
                    .is_some_and(|p| !area.response.rect.contains(p) && !anchor.contains(p))
        });
        if pressed_outside {
            self.tab_overflow_open = false;
        }
    }

    /// W9-I: the two roads a layer takes into another open document — a
    /// Layers-panel row released on that document's tab, and Duplicate
    /// Layer…'s Destination naming it — both land here, where the editor is
    /// in hand, as [`Editor::duplicate_layer_into_document`]: one undo step
    /// in the target, which becomes the active document. A refusal is the
    /// status line's.
    fn copy_layers_across(&mut self, editor: &mut Editor) {
        let mut copies = Vec::new();
        if let Some((layer, index)) = self.layer_tab_drop.take() {
            if let Some(target) = editor.documents().get(index).map(|d| d.id()) {
                copies.push((layer, target, None));
            }
        }
        if let Some((layer, target, name)) = crate::dialog_host::take_confirmed_duplicate_into() {
            copies.push((layer, target, Some(name)));
        }
        for (layer, target, name) in copies {
            if let Err(reason) = editor.duplicate_layer_into_document(layer, target, name) {
                editor.set_status(reason);
            }
        }
    }

    /// One Photopea document tab: capped width, truncated title (the dirty
    /// dot rides in `tab_label`), the close control *inside* the tab,
    /// middle-click close, drag to reorder.
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
        let height = tokens
            .metrics
            .control_height
            .max(tokens.metrics.min_hit_target + Space::XSmall.pt());
        let tab_width = Self::tab_width(tokens);
        // The interaction carries a deterministic id so tests (and the drag
        // bookkeeping) can name the tab: `ui::interact` with an explicit id.
        let (rect, _) = ui.allocate_exact_size(egui::vec2(tab_width, height), egui::Sense::hover());
        let response = ui.interact(rect, Self::tab_id(index), egui::Sense::click_and_drag());
        let rounding =
            design::egui_theme::rounding(design::Radius::Small.resolve(&tokens.radii, height));
        if selected {
            // The active tab is the panel surface: it reads as the sheet the
            // canvas below belongs to, lifted off the header band.
            ui.painter().rect_filled(
                rect,
                rounding,
                design::color32(tokens.palette.color(design::ColorRole::SurfacePanel)),
            );
        } else if response.hovered() {
            ui.painter().rect_filled(
                rect,
                rounding,
                design::color32(tokens.palette.color(design::ColorRole::ControlFillHovered)),
            );
        }
        // The close control's well, inside the tab's right edge. The title
        // gets what is left of the tab to its left.
        let well = tokens.metrics.min_hit_target;
        let close_rect = egui::Rect::from_center_size(
            egui::pos2(
                rect.right() - Space::XSmall.pt() - well * 0.5,
                rect.center().y,
            ),
            egui::Vec2::splat(well),
        );
        let pad = Space::Small.pt();
        let title_room = (close_rect.left() - rect.left() - pad * 2.0).max(0.0);
        // The title truncates to the tab, with an ellipsis when it had to:
        // Photopea never lets one long name widen the strip.
        let color = design::color32(if selected {
            tokens.palette.text(design::TextRole::Primary)
        } else {
            tokens.palette.text(design::TextRole::Secondary)
        });
        let mut job = egui::text::LayoutJob::single_section(
            label,
            egui::TextFormat {
                font_id: design::egui_theme::font_id(tokens, TypeRole::Body),
                color,
                ..Default::default()
            },
        );
        job.wrap = egui::text::TextWrapping::truncate_at_width(title_room);
        let galley = ui.painter().layout_job(job);
        let pos = egui::pos2(rect.left() + pad, rect.center().y - galley.size().y * 0.5);
        ui.painter().galley(pos, galley, color);
        let tip = if tooltip.is_empty() {
            ui::strings::tr("ui.chrome.not.saved.yet").to_string()
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
        // W9-I: a Layers-panel row dragged over ANOTHER document's tab is a
        // copy into that document (Photopea's drag-to-tab). The tab shows
        // the drop cue while the row is over it; the release parks the drop
        // for `copy_layers_across`, which makes the copy after the dialogs.
        if let Some(row) =
            egui::DragAndDrop::payload::<ui::dialogs::duplicate_layer::LayerRowDrag>(ui.ctx())
        {
            if !selected && ui.rect_contains_pointer(rect) {
                ui.painter().rect_stroke(
                    rect,
                    rounding,
                    egui::Stroke::new(
                        tokens.borders.thick,
                        design::color32(tokens.palette.color(design::ColorRole::SelectionStroke)),
                    ),
                );
                if ui.input(|i| i.pointer.any_released()) {
                    self.layer_tab_drop = Some((row.layer, index));
                }
            }
        }
        // Drawn, not typed. The panel headers' close is `ui::icons`' drawing,
        // and a tab close built the other way — a "×" handed to a text button
        // — is one font change away from being an empty square again. Drawn
        // in a child laid over the well, so its rect is inside the tab's; it
        // is registered after the tab, so it wins the hit test over it.
        let mut well_ui = ui.new_child(egui::UiBuilder::new().max_rect(close_rect).layout(
            egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
        ));
        if ui::icons::ui_icon_button_id(
            &mut well_ui,
            "close",
            ui::strings::tr("ui.chrome.close.tab"),
            design::TextRole::Secondary,
            Some(Self::tab_close_id(index)),
        )
        .clicked()
        {
            out.close = Some(index);
        }
        rect
    }

    /// The tab strip's overflow chevron.
    pub fn tab_overflow_id() -> egui::Id {
        egui::Id::new("raster-tabs-more")
    }

    /// One row of the overflow list: the hidden document at `index`.
    pub fn tab_overflow_item_id(index: usize) -> egui::Id {
        egui::Id::new(("raster-tabs-more-item", index))
    }

    /// Photopea's start screen, drawn over the canvas area while no document
    /// is open: a title, the New / Open / Templates cards and a grid of recent
    /// files with their thumbnails.
    ///
    /// Every emission is one the shell already performs — `Action::NewDocument`,
    /// `Action::Open`, `ChromeOutput::open_recent` — so a headless click
    /// exercises the same path the real click does. The Templates card is the
    /// exception in *shape* only: a preset row opens the same New Document
    /// dialog File ▸ New does, seeded with that preset, through the dialog
    /// host this chrome owns.
    fn start_screen(&mut self, ctx: &egui::Context, editor: &Editor, out: &mut ChromeOutput) {
        if !editor.documents().is_empty() {
            self.start_templates_open = false;
            return;
        }
        let tokens = design::current_theme(ctx).tokens();
        self.refresh_recent_thumbs(ctx, editor);
        // Centred in the room the docks and bands left — the canvas area —
        // not in the window, or the docks would cover its right-hand third.
        let room = ctx.available_rect();
        let offset = room.center() - ctx.screen_rect().center();
        let card_gap = Space::Medium.pt();
        let card_size = Self::start_card_size(tokens);
        let columns = 3.0;
        let width = card_size.x * columns + card_gap * (columns - 1.0);
        egui::Area::new(egui::Id::new("raster-start-screen"))
            .anchor(egui::Align2::CENTER_CENTER, offset)
            .order(egui::Order::Middle)
            .show(ctx, |ui| {
                ui.set_width(width);
                ui.vertical_centered(|ui| {
                    ui.label(
                        egui::RichText::new("Raster Studio")
                            .color(design::color32(
                                tokens.palette.text(design::TextRole::Primary),
                            ))
                            .font(design::egui_theme::font_id(tokens, design::TypeRole::Title)),
                    );
                    ui.add_space(design::Space::Large.pt());
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = card_gap;
                        if Self::start_card(
                            ui,
                            "raster-start-new",
                            ui::strings::tr("ui.chrome.start.new"),
                            ui::strings::tr("ui.chrome.start.new.hint"),
                            false,
                        )
                        .clicked()
                        {
                            out.actions.push(Action::NewDocument);
                        }
                        if Self::start_card(
                            ui,
                            "raster-start-open",
                            ui::strings::tr("ui.chrome.start.open"),
                            ui::strings::tr("ui.chrome.start.open.hint"),
                            false,
                        )
                        .clicked()
                        {
                            out.actions.push(Action::Open);
                        }
                        if Self::start_card(
                            ui,
                            "raster-start-templates",
                            ui::strings::tr("ui.chrome.start.templates"),
                            ui::strings::tr("ui.chrome.start.templates.hint"),
                            self.start_templates_open,
                        )
                        .clicked()
                        {
                            self.start_templates_open = !self.start_templates_open;
                        }
                    });
                    if self.start_templates_open {
                        ui.add_space(design::Space::Small.pt());
                        self.start_templates(ui, width);
                    }
                    ui.add_space(design::Space::Large.pt());
                    ui.label(
                        egui::RichText::new(ui::strings::tr("ui.chrome.start.recent"))
                            .color(design::color32(
                                tokens.palette.text(design::TextRole::Tertiary),
                            ))
                            .font(design::egui_theme::font_id(
                                tokens,
                                design::TypeRole::Footnote,
                            )),
                    );
                    ui.add_space(design::Space::Small.pt());
                    if editor.recent().is_empty() {
                        ui.colored_label(
                            design::color32(tokens.palette.text(design::TextRole::Tertiary)),
                            ui::strings::tr("ui.chrome.start.no.recent"),
                        );
                    } else {
                        self.start_recent_grid(ui, editor, width, card_gap, out);
                    }
                });
            });
    }

    /// A start-screen card: a bordered, rounded tile with a headline and a
    /// one-line hint. Painted by hand so the label is stated for the
    /// accessibility tree (C14) and the card's rect is the thing a test reads.
    fn start_card(
        ui: &mut egui::Ui,
        id: &'static str,
        label: &str,
        hint: &str,
        selected: bool,
    ) -> egui::Response {
        let tokens = design::current_tokens(ui);
        let (rect, _) = ui.allocate_exact_size(Self::start_card_size(tokens), egui::Sense::hover());
        let response = ui.interact(rect, egui::Id::new(id), egui::Sense::click());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
        let radius = design::egui_theme::rounding(
            design::Radius::Medium.resolve(&tokens.radii, rect.height()),
        );
        let fill = if selected {
            design::ColorRole::AccentSubtle
        } else if response.hovered() {
            design::ColorRole::ControlFillHovered
        } else {
            design::ColorRole::SurfaceElevated
        };
        ui.painter().rect(
            rect,
            radius,
            design::color32(tokens.palette.color(fill)),
            egui::Stroke::new(
                tokens.borders.hairline,
                design::color32(tokens.palette.color(design::ColorRole::SeparatorHairline)),
            ),
        );
        let pad = tokens.metrics.panel_padding + Space::Small.pt();
        let title = ui.painter().layout_no_wrap(
            label.to_string(),
            design::egui_theme::font_id(tokens, design::TypeRole::Headline),
            design::color32(tokens.palette.text(design::TextRole::Primary)),
        );
        let title_pos = egui::pos2(rect.left() + pad, rect.top() + pad);
        let mut job = egui::text::LayoutJob::single_section(
            hint.to_string(),
            egui::TextFormat {
                font_id: design::egui_theme::font_id(tokens, design::TypeRole::Footnote),
                color: design::color32(tokens.palette.text(design::TextRole::Secondary)),
                ..Default::default()
            },
        );
        job.wrap = egui::text::TextWrapping::wrap_at_width(rect.width() - pad * 2.0);
        let hint_galley = ui.painter().layout_job(job);
        let hint_pos = egui::pos2(
            rect.left() + pad,
            rect.bottom() - pad - hint_galley.size().y,
        );
        // The galleys carry their own colours; the tint passed here is egui's
        // fallback for a section without one, so it is the same colour.
        let primary = design::color32(tokens.palette.text(design::TextRole::Primary));
        ui.painter().galley(title_pos, title, primary);
        ui.painter().galley(hint_pos, hint_galley, primary);
        response
    }

    /// One card's footprint, on the grid: two inspector labels wide, a
    /// headline plus a hint plus the padding around them tall.
    fn start_card_size(tokens: &design::Tokens) -> egui::Vec2 {
        egui::vec2(
            tokens.metrics.inspector_label_width * 2.0,
            tokens.metrics.control_height * 2.0
                + tokens.metrics.panel_padding * 2.0
                + Space::Small.pt() * 2.0,
        )
    }

    /// The Templates card's list: the New dialog's own presets, grouped as
    /// that dialog groups them. A row opens that dialog seeded with the preset.
    fn start_templates(&mut self, ui: &mut egui::Ui, width: f32) {
        use ui::dialogs::new_document::{PresetGroup, PRESETS};
        let tokens = design::current_tokens(ui);
        let column = tokens.metrics.inspector_label_width * 2.0;
        let gap = Space::Medium.pt();
        let mut chosen: Option<usize> = None;
        overlay_frame(ui.ctx()).show(ui, |ui| {
            ui.set_width(width);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for group in PresetGroup::ALL {
                    ui.vertical(|ui| {
                        ui.set_width(column);
                        design::section_header(ui, group.label());
                        for (index, preset) in PRESETS.iter().enumerate() {
                            if preset.group != *group {
                                continue;
                            }
                            let row = design::list_row(ui, preset.name, false);
                            ui.interact(
                                row.rect,
                                Self::start_template_id(index),
                                egui::Sense::hover(),
                            );
                            if row.clicked() {
                                chosen = Some(index);
                            }
                        }
                    });
                }
            });
        });
        if let Some(index) = chosen {
            let mut dialog = ui::dialogs::NewDocumentDialog::default();
            dialog.apply_preset(index);
            self.dialogs
                .open(ActiveDialog::NewDocument(Box::new(dialog)));
            self.start_templates_open = false;
        }
    }

    /// The recent-files grid: a thumbnail over a name per file, as many
    /// columns as the start screen's width allows.
    fn start_recent_grid(
        &mut self,
        ui: &mut egui::Ui,
        editor: &Editor,
        width: f32,
        gap: f32,
        out: &mut ChromeOutput,
    ) {
        let tokens = design::current_tokens(ui);
        let cell = Self::start_recent_cell_size(tokens);
        let columns = ((width + gap) / (cell.x + gap)).floor().max(1.0) as usize;
        let entries: Vec<(usize, &PathBuf)> = editor
            .recent()
            .entries()
            .iter()
            .enumerate()
            .take(START_RECENT_MAX)
            .collect();
        for row in entries.chunks(columns) {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                for (index, path) in row {
                    self.start_recent_cell(ui, *index, path, cell, out);
                }
            });
            ui.add_space(Space::Small.pt());
        }
    }

    /// A recent-file cell: thumbnail (or a plain well while it has none),
    /// then the file name, truncated to the cell.
    fn start_recent_cell(
        &self,
        ui: &mut egui::Ui,
        index: usize,
        path: &Path,
        cell: egui::Vec2,
        out: &mut ChromeOutput,
    ) {
        let tokens = design::current_tokens(ui);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());
        let (rect, _) = ui.allocate_exact_size(cell, egui::Sense::hover());
        let response = ui
            .interact(rect, Self::start_recent_id(index), egui::Sense::click())
            .on_hover_text(path.display().to_string());
        // Painted by hand; the cell's accessible name is the file's name
        // (C14).
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name.clone())
        });
        let radius = design::egui_theme::rounding(
            design::Radius::Small.resolve(&tokens.radii, tokens.metrics.control_height),
        );
        if response.hovered() {
            ui.painter().rect_filled(
                rect.expand(Space::XSmall.pt()),
                radius,
                design::color32(tokens.palette.color(design::ColorRole::ControlFillHovered)),
            );
        }
        let thumb_rect = egui::Rect::from_min_size(
            rect.min,
            egui::vec2(cell.x, cell.y - tokens.metrics.list_row_height),
        );
        ui.painter().rect_filled(
            thumb_rect,
            radius,
            design::color32(tokens.palette.color(design::ColorRole::SurfaceSunken)),
        );
        if let Some(Some(tex)) = self.recent_thumbs.get(path) {
            // Fit the image inside the well, centred, aspect kept.
            let size = tex.size_vec2();
            let scale = (thumb_rect.width() / size.x).min(thumb_rect.height() / size.y);
            let fitted = egui::Rect::from_center_size(thumb_rect.center(), size * scale);
            // A texture is painted through a tint; white is "as uploaded", the
            // only tint that is not a design decision.
            ui.painter().image(
                tex.id(),
                fitted,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
        let color = design::color32(tokens.palette.text(design::TextRole::Secondary));
        let mut job = egui::text::LayoutJob::single_section(
            name,
            egui::TextFormat {
                font_id: design::egui_theme::font_id(tokens, design::TypeRole::Footnote),
                color,
                ..Default::default()
            },
        );
        job.wrap = egui::text::TextWrapping::truncate_at_width(cell.x);
        let galley = ui.painter().layout_job(job);
        let label_rect =
            egui::Rect::from_min_max(egui::pos2(rect.left(), thumb_rect.bottom()), rect.max);
        ui.painter().galley(
            egui::pos2(
                label_rect.center().x - galley.size().x * 0.5,
                label_rect.center().y - galley.size().y * 0.5,
            ),
            galley,
            color,
        );
        if response.clicked() {
            out.open_recent = Some(path.to_path_buf());
        }
    }

    /// A recent cell's footprint: the card width, a 3:2 thumbnail well and a
    /// list row for the name.
    fn start_recent_cell_size(tokens: &design::Tokens) -> egui::Vec2 {
        let width = tokens.metrics.inspector_label_width * 2.0;
        egui::vec2(
            width,
            Space::XXLarge.pt() * 3.0 + tokens.metrics.list_row_height,
        )
    }

    /// Keep one uploaded thumbnail per recent file, decoded once — never per
    /// frame. A `.rstudio` package's own `previews/preview.png` is read; a
    /// flat image is decoded through `raster` and box-filtered down. One
    /// decode per frame, so a long list spreads its cost over frames instead
    /// of freezing the first one. `None` in the map records a file that could
    /// not be read, so it is not retried every frame.
    fn refresh_recent_thumbs(&mut self, ctx: &egui::Context, editor: &Editor) {
        let wanted: Vec<PathBuf> = editor
            .recent()
            .entries()
            .iter()
            .take(START_RECENT_MAX)
            .cloned()
            .collect();
        self.recent_thumbs.retain(|path, _| wanted.contains(path));
        let Some(path) = wanted
            .into_iter()
            .find(|p| !self.recent_thumbs.contains_key(p))
        else {
            return;
        };
        let texture = recent_thumb_image(&path, RECENT_THUMB_EDGE).map(|img| {
            ctx.load_texture(
                format!("recent-{}", path.display()),
                img,
                egui::TextureOptions::LINEAR,
            )
        });
        self.recent_thumbs.insert(path, texture);
        // Another entry may still be waiting; the next frame takes it.
        ctx.request_repaint();
    }

    /// The uploaded thumbnails, for tests that ask what the start screen has.
    #[cfg(test)]
    pub(crate) fn recent_thumbs_for_test(&self) -> &HashMap<PathBuf, Option<egui::TextureHandle>> {
        &self.recent_thumbs
    }

    /// The recent cell's id, so a headless test can click entry `index`.
    pub fn start_recent_id(index: usize) -> egui::Id {
        egui::Id::new(("raster-start-recent", index))
    }

    /// The Templates list's row for preset `index` (into
    /// `ui::dialogs::new_document::PRESETS`).
    pub fn start_template_id(index: usize) -> egui::Id {
        egui::Id::new(("raster-start-template", index))
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
    ///
    /// The document's title is *not* here: the tab strip carries it, and a
    /// strip that repeats the tab is one more thing to read for nothing.
    fn status_bar(&mut self, ctx: &egui::Context, editor: &Editor, out: &mut ChromeOutput) {
        egui::TopBottomPanel::bottom("raster-status")
            .frame(panel_frame(ctx, SurfaceRole::Header, Space::Hair))
            .show(ctx, |ui| {
                let tokens = design::current_tokens(ui);
                let dim = design::color32(tokens.palette.text(TextRole::Secondary));
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = Space::Small.pt();
                    match editor.active() {
                        Some(doc) => {
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
                            // W3-G: the Size field reads in the Units preference.
                            ui.colored_label(
                                dim,
                                editor.size_readout(doc.document.width(), doc.document.height()),
                            );
                        }
                        None => {
                            ui.colored_label(dim, ui::strings::tr("ui.chrome.no.document"));
                        }
                    }
                    // The readouts chevron: the fields that would crowd the
                    // bar live in this menu.
                    if ui::icons::ui_icon_button_id(
                        ui,
                        "chevron-right",
                        ui::strings::tr("ui.chrome.more.readouts"),
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
                    // The size readout belongs to the tools that *have* a size
                    // — decided from the options schema, so "Move (V) 24 px"
                    // cannot come back with the next tool that is added.
                    let tool = editor.effective_tool();
                    if tool_has_size(tool) {
                        ui.colored_label(dim, format!("{} px", editor.brush().size as i32));
                    }
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

/// Which tabs the strip shows: `[first, last)` into the document list.
///
/// Everything, when `fits` is enough; otherwise a window of `fits` tabs that
/// keeps the active document in view, sliding only as far as it has to — the
/// first tabs stay put while the active one is among them, the way a browser
/// strip does.
fn visible_tab_range(total: usize, fits: usize, active: Option<usize>) -> (usize, usize) {
    if total == 0 {
        return (0, 0);
    }
    let fits = fits.max(1);
    if total <= fits {
        return (0, total);
    }
    let active = active.unwrap_or(0).min(total - 1);
    let first = if active < fits { 0 } else { active + 1 - fits };
    (first, first + fits)
}

/// Whether the status strip's size readout applies to `tool`: true when the
/// tool's options schema carries a `size` key, the same schema the options bar
/// draws its slider from. Asked of the registry, never of a list of tools.
fn tool_has_size(tool: ToolId) -> bool {
    tools::registry::info(tool).is_some_and(|info| {
        ui::tool_options::schema_for(info)
            .iter()
            .any(|spec| spec.key == "size")
    })
}

/// An intent that can only mean something *to a document*. With no document
/// open there is nothing for it to land on, so the empty-state docks drop it.
fn is_document_directed(intent: &ui::Intent) -> bool {
    matches!(
        intent,
        ui::Intent::Document(_)
            | ui::Intent::EditLayerKind { .. }
            | ui::Intent::SelectLayers { .. }
            | ui::Intent::SetGroupExpanded { .. }
            | ui::Intent::EnterTextLayer { .. }
            | ui::Intent::InsertGlyph { .. }
            | ui::Intent::SeekTimeline { .. }
            | ui::Intent::SetEditTarget { .. }
            | ui::Intent::HistoryJump(_)
            | ui::Intent::SetZoom(_)
            | ui::Intent::SetViewCenter(_)
    )
}

/// A recent file's thumbnail, decoded and fitted under `max_edge` texels.
///
/// A `.rstudio` package carries its own composite preview at
/// `project_format::PREVIEW_FILE`; that PNG is read and decoded. Anything
/// else is decoded whole through `raster`'s codec facade and box-filtered
/// down. `None` for a file that is gone, unreadable, or not an image.
fn recent_thumb_image(path: &Path, max_edge: u32) -> Option<egui::ColorImage> {
    let preview = path.join(project_format::PREVIEW_FILE);
    let decoded = if preview.is_file() {
        raster::decode_bytes(&std::fs::read(preview).ok()?).ok()?
    } else if path.is_file() {
        raster::decode_path(path).ok()?
    } else {
        return None;
    };
    let (width, height, rgba8) =
        box_downscale(decoded.width, decoded.height, &decoded.rgba8, max_edge);
    if width == 0 || height == 0 {
        return None;
    }
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [width as usize, height as usize],
        &rgba8,
    ))
}

/// W2-X: the whole canvas composited and box-averaged so its long edge is at
/// most `max_edge`, read in bands of [`PREVIEW_BAND_ROWS`] source rows.
///
/// One integer factor for the whole image (so every output pixel averages the
/// same block), each band a whole number of output rows, and the band is the
/// only full-resolution buffer alive: a 3628x2041 scene is read as eight
/// bands of 3.7 MB rather than one of 30 MB. `None` for an empty canvas or a
/// composite that failed.
fn composite_preview(
    open: &mut crate::doc::OpenDocument,
    max_edge: u32,
) -> Option<(u32, u32, Vec<u8>)> {
    let rect = open.canvas_rect();
    let (width, height) = (rect.width, rect.height);
    if width == 0 || height == 0 {
        return None;
    }
    let factor = width.max(height).div_ceil(max_edge.max(1)).max(1);
    let out_w = (width / factor).max(1);
    let out_h = (height / factor).max(1);
    let mut out = vec![0u8; (out_w as usize) * (out_h as usize) * 4];
    // Output rows per band: a whole number, so a block never straddles two
    // bands.
    let rows_per_band = (PREVIEW_BAND_ROWS / factor).max(1);
    let block = u64::from(factor) * u64::from(factor);
    let mut oy = 0u32;
    while oy < out_h {
        let band_rows = rows_per_band.min(out_h - oy);
        let y0 = oy * factor;
        let band_h = (band_rows * factor).min(height - y0);
        let band = open
            .composite(raster::PixelRect::new(
                rect.x,
                rect.y + i64::from(y0),
                width,
                band_h,
            ))
            .ok()?;
        let stride = width as usize * 4;
        for by in 0..band_rows {
            for ox in 0..out_w {
                let mut sum = [0u64; 4];
                for y in by * factor..((by + 1) * factor).min(band_h) {
                    let row = y as usize * stride;
                    for x in ox * factor..((ox + 1) * factor).min(width) {
                        let i = row + x as usize * 4;
                        for (c, acc) in sum.iter_mut().enumerate() {
                            *acc += u64::from(band[i + c]);
                        }
                    }
                }
                let o = ((oy + by) as usize * out_w as usize + ox as usize) * 4;
                for (c, acc) in sum.iter().enumerate() {
                    out[o + c] = (acc / block) as u8;
                }
            }
        }
        oy += band_rows;
    }
    Some((out_w, out_h, out))
}

/// Shrink straight-alpha RGBA8 by an integer factor so the longest edge is at
/// most `max_edge`, averaging each factor-by-factor block. An image already
/// small enough comes back untouched.
fn box_downscale(width: u32, height: u32, rgba8: &[u8], max_edge: u32) -> (u32, u32, Vec<u8>) {
    let long = width.max(height);
    if long == 0 || rgba8.len() < (width as usize) * (height as usize) * 4 {
        return (0, 0, Vec::new());
    }
    let factor = long.div_ceil(max_edge.max(1)).max(1);
    if factor == 1 {
        return (width, height, rgba8.to_vec());
    }
    let out_w = (width / factor).max(1);
    let out_h = (height / factor).max(1);
    let mut out = vec![0u8; (out_w as usize) * (out_h as usize) * 4];
    let stride = width as usize * 4;
    for oy in 0..out_h {
        for ox in 0..out_w {
            let mut sum = [0u64; 4];
            let mut n = 0u64;
            for y in oy * factor..((oy + 1) * factor).min(height) {
                let row = y as usize * stride;
                for x in ox * factor..((ox + 1) * factor).min(width) {
                    let i = row + x as usize * 4;
                    for (c, acc) in sum.iter_mut().enumerate() {
                        *acc += u64::from(rgba8[i + c]);
                    }
                    n += 1;
                }
            }
            let o = (oy as usize * out_w as usize + ox as usize) * 4;
            for (c, acc) in sum.iter().enumerate() {
                out[o + c] = (acc / n.max(1)) as u8;
            }
        }
    }
    (out_w, out_h, out)
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
    use crate::tool_input::ToolPointer;

    fn editor(dir: &std::path::Path) -> Editor {
        // Card 052: hermetic — chrome tests must not read the real OS
        // clipboard through the menu-context probe.
        let mut ed = Editor::with_state(
            AppPaths::rooted(dir),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        );
        ed.set_image_clipboard(Box::new(crate::clipboard::FakeClipboard::new()));
        ed
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
    /// Every shape one drawn frame painted (card 012's visibility check).
    /// Like [`painted_text`], but the whole shape list: the transform quad is
    /// a closed path, not text, so its presence in the paint output is the
    /// only honest answer to "did the published geometry appear".
    fn painted_shapes(chrome: &mut Chrome, editor: &mut Editor) -> Vec<egui::Shape> {
        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut shapes = Vec::new();
        // Two passes: the first frame is where egui learns the sizes.
        for _ in 0..2 {
            let output = ctx.run(raw_input(Vec::new()), |ctx| {
                chrome.ui(ctx, editor);
            });
            shapes = output
                .shapes
                .iter()
                .map(|clipped| clipped.shape.clone())
                .collect();
        }
        shapes
    }

    fn painted_text(editor: &mut Editor) -> Vec<(String, egui::Rect)> {
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
    fn run_chrome(editor: &mut Editor, click: Option<egui::Id>) -> ChromeOutput {
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

        // The status bar's own shapes — not a dock row whose galley runs
        // past its clip into the bar's band (egui clips at draw time, not in
        // the shape list), which is what the bottom of the Brushes panel is.
        let mut window = Window::new(&mut ed);
        let painted = window.status_bar_texts(&mut ed);
        let row: Vec<&(String, egui::Rect)> = painted.iter().collect();
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

    /// Card 061 (review round 4): push_brush's key list and the registry's
    /// brush-shared option set are ONE list — a new BrushSettings field with
    /// an option key must update both or the two copies drift again.
    #[test]
    fn push_brush_writes_exactly_the_brush_option_keys() {
        let mut w = ui::Workspace::default();
        // The written set: keys the workspace's held values carry for Brush.
        // push_brush only writes TOUCHED values, so seed the whole brush
        // family by hand first — each key moved off its schema default
        // (range's far end, flipped when the default sits at the max, as
        // Opacity's 1.0 does) so set() actually stores it.
        for key in tools::registry::BRUSH_OPTION_KEYS {
            let spec = ui::ToolOptions::spec_for_test(tools::ToolId::Brush, key)
                .unwrap_or_else(|| panic!("{key} is not a registry option"));
            let changed = match spec.kind {
                tools::OptionKind::Float { default, min, max } => {
                    let far = if (default - max).abs() < f32::EPSILON {
                        min
                    } else {
                        max
                    };
                    ui::OptionValue::Float(far)
                }
                tools::OptionKind::Bool { default, .. } => ui::OptionValue::Bool(!default),
                other => unreachable!("brush keys are floats/bools, got {other:?}"),
            };
            w.options.set(tools::ToolId::Brush, key, changed);
        }
        let held = w.options.held(tools::ToolId::Brush);
        let mut written: Vec<&str> = held.iter().map(|(k, _)| k.as_str()).collect();
        written.sort_unstable();
        let mut expected: Vec<&str> = tools::registry::BRUSH_OPTION_KEYS.to_vec();
        expected.sort_unstable();
        assert_eq!(
            written, expected,
            "push_brush keys drifted from the registry"
        );
    }

    #[test]
    fn tool_options_forwards_every_value_the_options_bar_holds() {
        // Card 010's read half: the workspace's option values reach the
        // boundary channel as (key, value) pairs, whatever their kind — not
        // the choice-only subset the old seam forwarded.
        let dir = tempfile::tempdir().unwrap();
        let p = png(dir.path(), "a.png");
        let mut chrome = Chrome::new();
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&p).unwrap();

        // Card 061 (review round 3): NOTHING set yet forwards NOTHING — the
        // forward channel carries only TOUCHED options, because an untouched
        // option is its registry default and demanding `set_setting` answers
        // for keys no tool implements flooded the status bar on every press
        // (the round-2 critical). The options BAR renders the declared
        // defaults; this channel is the forward-to-tool set.
        let defaults = chrome.tool_options(tools::ToolId::Move);
        assert!(
            defaults.is_empty(),
            "untouched defaults do not forward: {defaults:?}"
        );

        // The options bar writes the workspace; the boundary reads it back.
        chrome.set_tool_choice(tools::ToolId::FreeTransform, "mode", 2);
        chrome.workspace.options.set(
            tools::ToolId::Move,
            "auto_select",
            ui::OptionValue::Bool(true),
        );
        let options = chrome.tool_options(tools::ToolId::Move);
        assert!(
            options.contains(&("auto_select".to_string(), ui::OptionValue::Bool(true))),
            "the set value is forwarded: {options:?}"
        );
    }

    /// W3-A: an 8x8 document at `zoom`, centred in the 1400x900 test window,
    /// with exactly the View ▸ Extras in `on` ticked — every other extra is
    /// unticked, through the same `SetViewFlag` intents the menu posts — and
    /// the shapes the next frame paints.
    struct ExtrasFrame {
        _dir: tempfile::TempDir,
        editor: Editor,
        chrome: Chrome,
        shapes: Vec<egui::Shape>,
        content: egui::Rect,
        style: ui::canvas::CanvasStyle,
    }

    const EXTRAS: [ui::ViewFlag; 7] = [
        ui::ViewFlag::Rulers,
        ui::ViewFlag::Guides,
        ui::ViewFlag::Grid,
        ui::ViewFlag::PixelGrid,
        ui::ViewFlag::LayerEdges,
        ui::ViewFlag::PreciseCursor,
        ui::ViewFlag::SmartGuides,
    ];

    fn extras_frame(on: &[ui::ViewFlag], zoom: f32) -> ExtrasFrame {
        extras_frame_of(on, zoom, &[9u8; 8 * 8 * 4])
    }

    /// [`extras_frame`] over an 8x8 image of the given RGBA bytes.
    fn extras_frame_of(on: &[ui::ViewFlag], zoom: f32, rgba: &[u8]) -> ExtrasFrame {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.png");
        // A square document whose side follows the pixels handed in (8x8 for
        // the default fixture; the grid tests pass a larger one, because the
        // grid is drawn over the document only).
        let side = ((rgba.len() / 4) as f64).sqrt() as u32;
        assert_eq!(
            (side * side * 4) as usize,
            rgba.len(),
            "square RGBA fixture"
        );
        std::fs::write(
            &p,
            raster::encode(raster::ExportFormat::Png, side, side, rgba).unwrap(),
        )
        .unwrap();
        let mut editor = editor(&dir.path().join("config"));
        editor.open_path(&p).unwrap();
        {
            let doc = editor.active_mut().unwrap();
            doc.set_viewport(glam::Vec2::new(1400.0, 900.0));
            doc.camera.zoom = zoom;
            doc.camera.center = glam::Vec2::splat(side as f32 / 2.0);
        }
        let mut chrome = Chrome::new();
        for flag in EXTRAS {
            chrome.emit(ui::Intent::SetViewFlag {
                flag,
                on: on.contains(&flag),
            });
        }
        // Frame one harvests the toggles; frame two paints with them.
        let shapes = painted_shapes(&mut chrome, &mut editor);
        let content = chrome.frame_geometry.expect("a frame was drawn").content;
        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let style = ui::canvas::CanvasStyle::from_context(&ctx);
        ExtrasFrame {
            _dir: dir,
            editor,
            chrome,
            shapes,
            content,
            style,
        }
    }

    /// Every line segment painted in `colour` that crosses `within` (the
    /// painter clips to it, so a grid line spanning the window is still one
    /// line on the canvas).
    fn segments_in(
        shapes: &[egui::Shape],
        colour: egui::Color32,
        within: egui::Rect,
    ) -> Vec<[egui::Pos2; 2]> {
        shapes
            .iter()
            .filter_map(|s| match s {
                egui::Shape::LineSegment { points, stroke }
                    if stroke.color == egui::epaint::ColorMode::Solid(colour)
                        && egui::Rect::from_two_pos(points[0], points[1])
                            .expand(0.5)
                            .intersects(within) =>
                {
                    Some(*points)
                }
                _ => None,
            })
            .collect()
    }

    /// Every filled rectangle painted in `fill`.
    fn rects_filled(shapes: &[egui::Shape], fill: egui::Color32) -> Vec<egui::Rect> {
        shapes
            .iter()
            .filter_map(|s| match s {
                egui::Shape::Rect(r) if r.fill == fill => Some(r.rect),
                _ => None,
            })
            .collect()
    }

    /// W3-A: View ▸ Rulers paints the two gutter bands over the top and left
    /// of the canvas area the docks left; unticked, neither band is there.
    #[test]
    fn view_rulers_paints_the_two_ruler_bands_over_the_canvas() {
        let on = extras_frame(&[ui::ViewFlag::Rulers], 1.0);
        let [top, left] = ui::canvas::rulers::gutters(on.content, on.style.ruler_thickness_pt);
        let bands = rects_filled(&on.shapes, on.style.ruler_fill);
        let near = |a: egui::Rect, b: egui::Rect| {
            (a.min - b.min).length() < 0.5 && (a.max - b.max).length() < 0.5
        };
        assert!(
            bands.iter().any(|r| near(*r, top)),
            "no top ruler band at {top:?}: {bands:?}"
        );
        assert!(
            bands.iter().any(|r| near(*r, left)),
            "no left ruler band at {left:?}: {bands:?}"
        );
        assert!(on.chrome.extras_report().rulers);

        let off = extras_frame(&[], 1.0);
        let bands = rects_filled(&off.shapes, off.style.ruler_fill);
        assert!(
            !bands.iter().any(|r| near(*r, top) || near(*r, left)),
            "Rulers unticked still painted a band: {bands:?}"
        );
        assert!(!off.chrome.extras_report().rulers);

        // The pointer is marked on both rulers: a hairline across the top
        // gutter at its x, and across the left gutter at its y.
        let mut frame = extras_frame(&[ui::ViewFlag::Rulers], 1.0);
        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let at = egui::pos2(400.0, 300.0);
        let mut shapes = Vec::new();
        for _ in 0..2 {
            shapes = ctx
                .run(raw_input(vec![egui::Event::PointerMoved(at)]), |ctx| {
                    frame.chrome.ui(ctx, &mut frame.editor);
                })
                .shapes
                .into_iter()
                .map(|c| c.shape)
                .collect();
        }
        let marks = segments_in(&shapes, frame.style.guide, top);
        assert!(
            marks.iter().any(|[a, b]| a.x == at.x && b.x == at.x),
            "no pointer mark on the top ruler at x = {}: {marks:?}",
            at.x
        );
        let marks = segments_in(&shapes, frame.style.guide, left);
        assert!(
            marks.iter().any(|[a, b]| a.y == at.y && b.y == at.y),
            "no pointer mark on the left ruler at y = {}: {marks:?}",
            at.y
        );
    }

    /// W3-A: View ▸ Grid at 100% paints the document grid — at the default
    /// 64 px spacing a 640x640 document crosses well over a dozen major
    /// lines — and unticked paints none. The grid covers the document only
    /// (Photopea), so the fixture is a document, not the 8x8 swatch.
    #[test]
    fn view_grid_paints_grid_lines_at_100_percent() {
        let on = extras_frame_of(&[ui::ViewFlag::Grid], 1.0, &vec![9u8; 640 * 640 * 4]);
        let major = segments_in(&on.shapes, on.style.grid_major, on.content);
        assert!(major.len() >= 12, "only {} major grid lines", major.len());
        let off = extras_frame(&[], 1.0);
        assert!(segments_in(&off.shapes, off.style.grid_major, off.content).is_empty());
    }

    /// W3-A: View ▸ Pixel Grid paints nothing at 100% and one line per pixel
    /// boundary of the 8x8 document at 800%: nine each way, on the pixel
    /// edges (the document spans x 668..732 on screen at 800%).
    #[test]
    fn view_pixel_grid_paints_nothing_at_100_percent_and_the_pixel_lines_at_800() {
        let low = extras_frame(&[ui::ViewFlag::PixelGrid], 1.0);
        assert!(
            segments_in(&low.shapes, low.style.pixel_grid, low.content).is_empty(),
            "the pixel grid drew at 100%"
        );
        let high = extras_frame(&[ui::ViewFlag::PixelGrid], 8.0);
        let lines = segments_in(&high.shapes, high.style.pixel_grid, high.content);
        let verticals: std::collections::BTreeSet<i32> = lines
            .iter()
            .filter(|[a, b]| (a.x - b.x).abs() < 1e-3)
            .map(|[a, _]| a.x.round() as i32)
            .collect();
        let expected: std::collections::BTreeSet<i32> = (0..=8).map(|i| 668 + 8 * i).collect();
        assert_eq!(verticals, expected, "vertical pixel-grid lines");
        assert!(
            lines.len() >= 18,
            "{} pixel-grid lines at 800%",
            lines.len()
        );
        let off = extras_frame(&[], 8.0);
        assert!(segments_in(&off.shapes, off.style.pixel_grid, off.content).is_empty());
    }

    /// W3-A: View ▸ Layer Edges outlines the active layer's ink where it is
    /// on screen - not the canvas: the layer is inked over only part of it.
    #[test]
    fn view_layer_edges_outlines_the_active_layer_at_its_screen_bounds() {
        let outline = |frame: &ExtrasFrame| -> Vec<Vec<egui::Pos2>> {
            frame
                .shapes
                .iter()
                .filter_map(|s| match s {
                    egui::Shape::Path(p)
                        if p.closed
                            && p.stroke.color
                                == egui::epaint::ColorMode::Solid(frame.style.layer_edge) =>
                    {
                        Some(p.points.clone())
                    }
                    _ => None,
                })
                .collect()
        };
        // Ink only at x 2..6, y 1..4 of the 8x8 canvas; the rest is clear.
        // The canvas spans (696, 446)..(704, 454) on screen, so the layer's
        // ink spans (698, 447)..(702, 450) - a box the canvas border is not.
        let mut rgba = [0u8; 8 * 8 * 4];
        for y in 1..4 {
            for x in 2..6 {
                let i = (y * 8 + x) * 4;
                rgba[i..i + 4].copy_from_slice(&[200, 40, 40, 255]);
            }
        }
        let on = extras_frame_of(&[ui::ViewFlag::LayerEdges], 1.0, &rgba);
        let quads = outline(&on);
        let is_quad = |q: &Vec<egui::Pos2>, corners: [egui::Pos2; 4]| {
            q.len() == 4
                && corners
                    .iter()
                    .all(|e| q.iter().any(|p| (*p - *e).length() < 0.01))
        };
        let ink = [
            egui::pos2(698.0, 447.0),
            egui::pos2(702.0, 447.0),
            egui::pos2(702.0, 450.0),
            egui::pos2(698.0, 450.0),
        ];
        let canvas = [
            egui::pos2(696.0, 446.0),
            egui::pos2(704.0, 446.0),
            egui::pos2(704.0, 454.0),
            egui::pos2(696.0, 454.0),
        ];
        assert!(
            quads.iter().any(|q| is_quad(q, ink)),
            "no outline at the layer's ink bounds {ink:?}: {quads:?}"
        );
        assert!(
            !quads.iter().any(|q| is_quad(q, canvas)),
            "Layer Edges outlined the canvas, not the layer: {quads:?}"
        );
        assert_eq!(on.chrome.extras_report().layer_edges, 1);
        let off = extras_frame(&[], 1.0);
        assert!(outline(&off).is_empty(), "Layer Edges unticked still drew");
    }

    /// W3-A: View ▸ Precise Cursor draws a crosshair at the pointer over the
    /// canvas.
    #[test]
    fn view_precise_cursor_draws_a_crosshair_at_the_pointer() {
        let mut frame = extras_frame(&[ui::ViewFlag::PreciseCursor], 1.0);
        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let at = egui::pos2(700.0, 450.0);
        let crosshair = |frame: &mut ExtrasFrame| -> Vec<[egui::Pos2; 2]> {
            let mut shapes: Vec<egui::Shape> = Vec::new();
            for _ in 0..2 {
                shapes = ctx
                    .run(raw_input(vec![egui::Event::PointerMoved(at)]), |ctx| {
                        frame.chrome.ui(ctx, &mut frame.editor);
                    })
                    .shapes
                    .into_iter()
                    .map(|c| c.shape)
                    .collect();
            }
            // The two arms, each centred on the pointer: one horizontal, one
            // vertical, in the cursor's contrasting stroke.
            segments_in(&shapes, frame.style.brush_ring_over, frame.content)
                .into_iter()
                .filter(|[a, b]| {
                    (egui::pos2((a.x + b.x) * 0.5, (a.y + b.y) * 0.5) - at).length() < 0.01
                })
                .collect()
        };
        let arms = crosshair(&mut frame);
        assert!(
            arms.iter()
                .any(|[a, b]| (a.y - b.y).abs() < 1e-3 && (a.x - b.x).abs() > 1.0),
            "no horizontal crosshair arm through {at:?}: {arms:?}"
        );
        assert!(
            arms.iter()
                .any(|[a, b]| (a.x - b.x).abs() < 1e-3 && (a.y - b.y).abs() > 1.0),
            "no vertical crosshair arm through {at:?}: {arms:?}"
        );
        assert!(frame.chrome.extras_report().precise_cursor);
        let mut off = extras_frame(&[], 1.0);
        assert!(
            crosshair(&mut off).is_empty(),
            "Precise Cursor unticked still drew a crosshair"
        );
        assert!(!off.chrome.extras_report().precise_cursor);
    }

    /// Drive one frame of `frame` with `events` and apply the commands the
    /// chrome emitted, as the shell does. Returns the painted shapes.
    fn extras_step(
        ctx: &egui::Context,
        frame: &mut ExtrasFrame,
        events: Vec<egui::Event>,
    ) -> Vec<egui::Shape> {
        let mut commands = Vec::new();
        let shapes = ctx
            .run(raw_input(events), |ctx| {
                commands = frame.chrome.ui(ctx, &mut frame.editor).commands;
            })
            .shapes
            .into_iter()
            .map(|c| c.shape)
            .collect();
        for command in commands {
            frame.editor.apply_command(command);
        }
        shapes
    }

    /// A press at `from`, a drag through `to`, and the release there - the
    /// frames a real mouse produces.
    fn extras_drag(ctx: &egui::Context, frame: &mut ExtrasFrame, from: egui::Pos2, to: egui::Pos2) {
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        let mid = from + (to - from) * 0.5;
        for events in [
            vec![egui::Event::PointerMoved(from)],
            vec![egui::Event::PointerMoved(from)],
            vec![button(from, true)],
            vec![egui::Event::PointerMoved(mid)],
            vec![egui::Event::PointerMoved(to)],
            vec![button(to, false)],
            Vec::new(),
        ] {
            let _ = extras_step(ctx, frame, events);
        }
    }

    /// The long horizontal guide-coloured lines across the image area (the
    /// ruler's pointer mark is the same colour but only a gutter deep).
    fn horizontal_guides(frame: &ExtrasFrame, shapes: &[egui::Shape]) -> Vec<f32> {
        segments_in(shapes, frame.style.guide, frame.content)
            .into_iter()
            .filter(|[a, b]| (a.y - b.y).abs() < 1e-3 && (a.x - b.x).abs() > 100.0)
            .map(|[a, _]| a.y)
            .collect()
    }

    /// W3-A: View ▸ Guides on an ordinary opened image (whose document guide
    /// set is the default, `visible: false`): a guide pulled out of the top
    /// ruler lands in the document as one `SetGuides`, is painted where it was
    /// dropped on the next frame, and can be grabbed and moved again.
    /// Unticking Guides hides it without editing the document.
    #[test]
    fn view_guides_drags_a_guide_out_of_the_ruler_paints_it_and_moves_it_again() {
        let mut frame = extras_frame(&[ui::ViewFlag::Rulers, ui::ViewFlag::Guides], 1.0);
        assert!(
            !frame.editor.active().unwrap().document.guides.visible,
            "the fixture is the ordinary case: the document's own flag is off"
        );
        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let [top, _] = ui::canvas::rulers::gutters(frame.content, frame.style.ruler_thickness_pt);
        let x = 600.0;
        // Dropped at screen y 452: document y 4 + (452 - 450) = 6 at 100%.
        extras_drag(
            &ctx,
            &mut frame,
            egui::pos2(x, top.center().y),
            egui::pos2(x, 452.0),
        );
        let guides = frame.editor.active().unwrap().document.guides.clone();
        assert_eq!(guides.list.len(), 1, "one guide landed: {guides:?}");
        assert_eq!(guides.list[0].axis, editor_core::GuideAxis::Horizontal);
        assert!(
            (guides.list[0].doc - 6.0).abs() <= 1.0,
            "the guide is where it was dropped: {guides:?}"
        );
        assert!(
            !guides.visible,
            "toggling a view must not edit the document's persisted flag"
        );
        let screen_y = 450.0 + guides.list[0].doc - 4.0;
        let shapes = extras_step(&ctx, &mut frame, Vec::new());
        let painted = horizontal_guides(&frame, &shapes);
        assert!(
            painted.iter().any(|y| (y - screen_y).abs() < 0.5),
            "the dropped guide is not painted at y {screen_y}: {painted:?}"
        );
        assert_eq!(frame.chrome.extras_report().guides, 1);

        // Grab it again and move it 10 points down.
        extras_drag(
            &ctx,
            &mut frame,
            egui::pos2(x, screen_y),
            egui::pos2(x, screen_y + 10.0),
        );
        let moved = frame.editor.active().unwrap().document.guides.clone();
        assert_eq!(moved.list.len(), 1, "{moved:?}");
        assert!(
            (moved.list[0].doc - (guides.list[0].doc + 10.0)).abs() <= 1.0,
            "the guide was grabbed and moved: {guides:?} -> {moved:?}"
        );

        // Guides unticked: nothing painted, and the document keeps its guide.
        frame.chrome.emit(ui::Intent::SetViewFlag {
            flag: ui::ViewFlag::Guides,
            on: false,
        });
        let _ = extras_step(&ctx, &mut frame, Vec::new());
        let shapes = extras_step(&ctx, &mut frame, Vec::new());
        assert!(horizontal_guides(&frame, &shapes).is_empty());
        assert_eq!(frame.chrome.extras_report().guides, 0);
        assert_eq!(frame.editor.active().unwrap().document.guides, moved);
    }

    /// W3-A (round 3): the extras paint *under* an open modal dialog and its
    /// scrim. They used to be a free `Order::Middle` layer, which egui draws
    /// after every ordered middle area — over the dialog and undimmed — so
    /// with Grid or the default Layer Edges on, lines crossed Image Size or
    /// Levels. Here every grid line and the layer outline come before the
    /// scrim in paint order, and the scrim before the dialog's text.
    #[test]
    fn view_extras_paint_under_an_open_dialog_and_its_scrim() {
        let mut frame = extras_frame_of(
            &[ui::ViewFlag::Grid, ui::ViewFlag::LayerEdges],
            1.0,
            &vec![9u8; 640 * 640 * 4],
        );
        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        frame.chrome.open_new_document_dialog();
        let _ = extras_step(&ctx, &mut frame, Vec::new());
        let shapes = extras_step(&ctx, &mut frame, Vec::new());
        assert!(frame.chrome.dialog_open(), "the dialog is up");
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1400.0, 900.0));
        // The scrim: the one translucent wash over the whole window.
        let scrim = shapes
            .iter()
            .position(|s| {
                matches!(s, egui::Shape::Rect(r)
                    if r.rect == screen && r.fill.a() > 0 && r.fill.a() < 255)
            })
            .expect("the scrim is painted");
        let last_text = shapes
            .iter()
            .enumerate()
            .filter(|(_, s)| matches!(s, egui::Shape::Text(_)))
            .map(|(i, _)| i)
            .max()
            .expect("the dialog paints text");
        let grid: Vec<usize> = shapes
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                matches!(s, egui::Shape::LineSegment { stroke, .. }
                    if stroke.color == egui::epaint::ColorMode::Solid(frame.style.grid_major))
            })
            .map(|(i, _)| i)
            .collect();
        assert!(grid.len() >= 12, "the grid is painted: {}", grid.len());
        assert_eq!(frame.chrome.extras_report().layer_edges, 1);
        let last_grid = *grid.iter().max().expect("grid lines");
        assert!(
            last_grid < scrim,
            "a grid line (shape {last_grid}) is painted over the scrim (shape {scrim})"
        );
        assert!(scrim < last_text, "the dialog is over its scrim");
        let layer_edge = frame.style.layer_edge;
        let outlines_over_scrim = shapes
            .iter()
            .enumerate()
            .skip(scrim)
            .filter(|(_, s)| {
                matches!(s, egui::Shape::Path(p)
                    if p.closed && p.stroke.color == egui::epaint::ColorMode::Solid(layer_edge))
            })
            .count();
        assert_eq!(
            outlines_over_scrim, 0,
            "the layer outline is over the scrim"
        );
    }

    /// W3-A (round 3): with a modal up, a press in the ruler gutter belongs to
    /// the scrim, not to a guide gesture: no guide is pulled out. The gutter
    /// areas used to be raised at `Order::Middle`, above the scrim.
    #[test]
    fn a_ruler_press_under_an_open_dialog_pulls_no_guide() {
        let mut frame = extras_frame(&[ui::ViewFlag::Rulers, ui::ViewFlag::Guides], 1.0);
        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        frame.chrome.open_new_document_dialog();
        let _ = extras_step(&ctx, &mut frame, Vec::new());
        assert!(frame.chrome.dialog_open());
        let [top, _] = ui::canvas::rulers::gutters(frame.content, frame.style.ruler_thickness_pt);
        extras_drag(
            &ctx,
            &mut frame,
            egui::pos2(600.0, top.center().y),
            egui::pos2(600.0, 452.0),
        );
        assert!(frame.chrome.dialog_open(), "the dialog stayed up");
        assert!(
            frame
                .editor
                .active()
                .unwrap()
                .document
                .guides
                .list
                .is_empty(),
            "a press the scrim owns pulled a guide: {:?}",
            frame.editor.active().unwrap().document.guides
        );
    }

    /// W3-A (round 3): a guide's grab band stands down while a transform
    /// session is live — the session's handles have the higher claim (the
    /// `ui` host's `may_grab` rule) — and grabs again once it ends.
    #[test]
    fn guide_grab_bands_yield_to_a_live_transform_session() {
        let mut frame = extras_frame(&[ui::ViewFlag::Rulers, ui::ViewFlag::Guides], 1.0);
        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let [top, _] = ui::canvas::rulers::gutters(frame.content, frame.style.ruler_thickness_pt);
        let x = 600.0;
        extras_drag(
            &ctx,
            &mut frame,
            egui::pos2(x, top.center().y),
            egui::pos2(x, 452.0),
        );
        let placed = frame.editor.active().unwrap().document.guides.clone();
        assert_eq!(placed.list.len(), 1, "{placed:?}");
        let screen_y = 450.0 + placed.list[0].doc - 4.0;

        let doc_id = frame.editor.active().unwrap().id();
        let session = tools::SessionGeometry::Transform {
            state: tools::transform::TransformState::new(raster::PixelRect::new(0, 0, 8, 8)),
            mode: tools::transform::TransformMode::Scale,
            active: None,
            layer: None,
        };
        frame
            .chrome
            .publish_tool_geometry(Some((doc_id, session)), Some(doc_id));
        extras_drag(
            &ctx,
            &mut frame,
            egui::pos2(x, screen_y),
            egui::pos2(x, screen_y + 10.0),
        );
        assert_eq!(
            frame.editor.active().unwrap().document.guides,
            placed,
            "the guide was grabbed through a live transform session"
        );

        // The session ends: the same press grabs the guide again.
        frame.chrome.publish_tool_geometry(None, Some(doc_id));
        extras_drag(
            &ctx,
            &mut frame,
            egui::pos2(x, screen_y),
            egui::pos2(x, screen_y + 10.0),
        );
        let moved = frame.editor.active().unwrap().document.guides.clone();
        assert!(
            (moved.list[0].doc - (placed.list[0].doc + 10.0)).abs() <= 1.0,
            "with no session the guide moves: {placed:?} -> {moved:?}"
        );
    }

    /// W3-A (round 3): Smart Guides while an ordinary Move-tool drag is held
    /// (no transform session is published for one). A 2x2 block of ink in
    /// the 8x8 image, dragged 3.2 px right at 800%, snaps its centre onto the
    /// canvas centre (x 4), and the canvas-centre smart guide is drawn there
    /// (screen x 700) while the button is down; released, it is gone. With
    /// Smart Guides off the same drag draws none.
    #[test]
    fn a_move_drag_paints_the_smart_guide_it_snapped_to() {
        let mut rgba = vec![0u8; 8 * 8 * 4];
        for y in 0..2 {
            for x in 0..2 {
                let i = (y * 8 + x) * 4;
                rgba[i..i + 4].copy_from_slice(&[200, 30, 30, 255]);
            }
        }
        let held = |smart: bool| -> (Vec<egui::Shape>, usize, usize, ExtrasFrame) {
            let on: &[ui::ViewFlag] = if smart {
                &[ui::ViewFlag::SmartGuides]
            } else {
                &[]
            };
            let mut frame = extras_frame_of(on, 8.0, &rgba);
            frame.editor.set_tool(tools::ToolId::Move);
            let ctx = egui::Context::default();
            install_theme(&ctx, design::Theme::Dark);
            // Doc (1, 1) is screen (676, 426) at 800% about (700, 450).
            let from = egui::pos2(676.0, 426.0);
            let to = egui::pos2(676.0 + 3.2 * 8.0, 426.0);
            let button = |pos, pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::default(),
            };
            let _ = extras_step(&ctx, &mut frame, vec![egui::Event::PointerMoved(from)]);
            let _ = extras_step(&ctx, &mut frame, vec![egui::Event::PointerMoved(from)]);
            let _ = extras_step(&ctx, &mut frame, vec![button(from, true)]);
            let shapes = extras_step(&ctx, &mut frame, vec![egui::Event::PointerMoved(to)]);
            let during = frame.chrome.extras_report().smart_guides;
            let _ = extras_step(&ctx, &mut frame, vec![button(to, false)]);
            let _ = extras_step(&ctx, &mut frame, Vec::new());
            let after = frame.chrome.extras_report().smart_guides;
            (shapes, during, after, frame)
        };
        let (shapes, during, after, frame) = held(true);
        let lines = segments_in(&shapes, frame.style.smart_guide, frame.content);
        assert!(
            lines
                .iter()
                .any(|[a, b]| (a.x - 700.0).abs() < 0.5 && (b.x - 700.0).abs() < 0.5),
            "no smart guide at the canvas centre x 700: {lines:?}"
        );
        // The dragged layer never catches on its own pre-drag edges (its
        // top edge at y 0 is where the zero-y drag leaves it): no horizontal
        // smart guide at screen y 418, 426 or 434.
        assert!(
            !lines.iter().any(|[a, b]| (a.y - b.y).abs() < 1e-3),
            "the moving layer snapped to itself: {lines:?}"
        );
        assert_eq!(during, 1, "one smart guide: {lines:?}");
        assert_eq!(after, 0, "released, the smart guide is gone");

        let (shapes, during, _, frame) = held(false);
        assert!(segments_in(&shapes, frame.style.smart_guide, frame.content).is_empty());
        assert_eq!(during, 0, "Smart Guides off draws none");
    }

    /// W3-A: the `--shot` fixture's View toggles parse from their variant
    /// names, case-insensitively, skipping unknown ones.
    #[test]
    fn shot_view_flags_parse_from_variant_names() {
        assert_eq!(
            crate::parse_view_flags("Rulers, grid,nope,PIXELGRID"),
            vec![
                ui::ViewFlag::Rulers,
                ui::ViewFlag::Grid,
                ui::ViewFlag::PixelGrid
            ]
        );
    }

    /// W3-A: View ▸ Selection Edges decides whether the shell's marching ants
    /// are drawn: the geometry the shell strokes ([`Chrome::selection_ants`],
    /// what `Shell::redraw` calls) is the outline with the flag on and empty
    /// with it off, over the same selection.
    #[test]
    fn view_selection_edges_gates_the_marching_ants_the_shell_draws() {
        let ants = |on: bool| {
            let mut frame = extras_frame(&[], 1.0);
            frame.chrome.emit(ui::Intent::SetViewFlag {
                flag: ui::ViewFlag::SelectionEdges,
                on,
            });
            let _ = painted_shapes(&mut frame.chrome, &mut frame.editor);
            assert_eq!(frame.chrome.selection_edges_visible(), on);
            let doc = frame.editor.active_mut().unwrap();
            doc.document.selection = editor_core::Selection::Rect {
                min: glam::IVec2::new(1, 1),
                max: glam::IVec2::new(6, 6),
            };
            let doc = frame.editor.active().unwrap();
            let mut outline = crate::presenter::SelectionOutline::new();
            frame
                .chrome
                .selection_ants(&mut outline, doc, 0.0, &Default::default())
        };
        assert!(!ants(true).is_empty(), "Selection Edges on drew no ants");
        assert!(
            ants(false).is_empty(),
            "Selection Edges off still drew ants"
        );
    }

    /// View > Flip Horizontal, through the real toggle intent and a real
    /// chrome frame: the item ticks, the active document's camera mirrors, a
    /// point on screen maps to the mirrored image column, and unticking
    /// restores the upright mapping.
    #[test]
    fn view_flip_ticks_and_mirrors_the_document_camera() {
        let mut frame = extras_frame(&[], 1.0);
        let camera = frame.editor.active().unwrap().camera.clone();
        // A point off the vertical centre line of the viewport, so a mirror
        // about that line must move it to another column.
        let probe = camera.viewport_size * 0.5 + glam::Vec2::new(-20.0, 3.0);
        let upright = camera.screen_to_image(probe);

        frame.chrome.emit(ui::Intent::SetViewFlag {
            flag: ui::ViewFlag::FlipHorizontal,
            on: true,
        });
        let _ = painted_shapes(&mut frame.chrome, &mut frame.editor);
        assert!(frame
            .chrome
            .workspace()
            .view_flags
            .get(ui::ViewFlag::FlipHorizontal));
        assert!(frame.editor.status().is_none_or(|s| !s.contains("Flip")));
        let flipped = frame.editor.active().unwrap().camera.clone();
        assert!(
            flipped.flip_x && !flipped.flip_y,
            "the checkmark never reached the camera"
        );
        let mirrored = flipped.screen_to_image(probe);
        assert!(
            (mirrored.x - upright.x).abs() > 1.0 && (mirrored.y - upright.y).abs() < 1e-3,
            "a mirrored view maps the same screen point to another column: {upright:?} vs {mirrored:?}"
        );

        frame.chrome.emit(ui::Intent::SetViewFlag {
            flag: ui::ViewFlag::FlipHorizontal,
            on: false,
        });
        let _ = painted_shapes(&mut frame.chrome, &mut frame.editor);
        let back = frame.editor.active().unwrap().camera.clone();
        assert!(!back.flip_x);
        assert!((back.screen_to_image(probe) - upright).length() < 1e-3);
    }

    /// W7-D (was W3-A's refusal): View > Proof Colors and View > Gamut
    /// Warning are enabled menu rows, a tick lands on the workspace, and the
    /// presenter's per-frame read ([`crate::presenter::CanvasPresenter::
    /// read_view_settings`], the one call the shell makes) turns it into
    /// pixels: saturated green shows as its CMYK round trip, then as the
    /// theme's Warning token, while mid grey is left alone.
    #[test]
    fn proof_colors_and_gamut_warning_tick_from_the_menu_and_change_the_canvas_texture() {
        let mut rgba = Vec::new();
        for i in 0..64 {
            if i % 8 < 4 {
                rgba.extend_from_slice(&[0, 255, 0, 255]);
            } else {
                rgba.extend_from_slice(&[128, 128, 128, 255]);
            }
        }
        let mut frame = extras_frame_of(&[], 1.0, &rgba);
        let whole = raster::PixelRect::new(0, 0, 8, 8);
        let mut presenter = crate::presenter::CanvasPresenter::new();
        presenter.read_view_settings(&frame.chrome);
        let plain = presenter
            .composite_masked(frame.editor.active_mut().unwrap(), whole)
            .unwrap();
        assert_eq!(&plain[0..4], &[0, 255, 0, 255]);

        let warning = frame.chrome.proof_view().warning;
        let proofed_green = color::cmyk::ProofLut::shared().proof([0, 255, 0]);
        assert_ne!(proofed_green, [0, 255, 0], "green is not printable");
        for (flag, green) in [
            (ui::ViewFlag::ProofColors, proofed_green),
            (ui::ViewFlag::GamutWarning, warning),
        ] {
            // The menu row is enabled before any click.
            let context = crate::menu_bridge::context(&mut frame.editor, frame.chrome.workspace());
            let intent = crate::menu_bridge::resolve_intent(
                ui::MenuAction::ToggleView(flag),
                &context,
                &frame.editor,
            )
            .unwrap_or_else(|e| panic!("{flag:?} is greyed: {e}"));
            frame.chrome.emit(intent);
            let _ = painted_shapes(&mut frame.chrome, &mut frame.editor);
            assert!(
                frame.chrome.workspace().view_flags.get(flag),
                "{flag:?} never ticked"
            );
            assert!(
                presenter.read_view_settings(&frame.chrome),
                "{flag:?} never reached the presenter"
            );
            let shown = presenter
                .composite_masked(frame.editor.active_mut().unwrap(), whole)
                .unwrap();
            assert_eq!(&shown[0..3], &green, "{flag:?}: green");
            assert_eq!(
                &shown[16..20],
                &[128, 128, 128, 255],
                "{flag:?}: grey prints"
            );
            // Off again: the plain composite.
            frame
                .chrome
                .emit(ui::Intent::SetViewFlag { flag, on: false });
            let _ = painted_shapes(&mut frame.chrome, &mut frame.editor);
            assert!(presenter.read_view_settings(&frame.chrome));
            assert_eq!(
                presenter
                    .composite_masked(frame.editor.active_mut().unwrap(), whole)
                    .unwrap(),
                plain
            );
        }
        // The document itself never moved: a proof is a view.
        assert_eq!(
            frame.editor.active_mut().unwrap().composite(whole).unwrap(),
            plain
        );
    }

    /// Card 012's check: a shell-published transform session is *visible* —
    /// the same frame paints the quad and handles, and an ended session paints
    /// none. Publication happens through the production publisher fed by a
    /// real gesture's geometry, never by writing the sessions field by hand.
    #[test]
    fn a_published_transform_session_paints_handles_until_the_session_ends() {
        let dir = tempfile::tempdir().unwrap();
        let p = png(dir.path(), "a.png");
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&p).unwrap();
        // The camera the surface is rendered with: 100%, image centred, over
        // the full test window.
        {
            let doc = ed.active_mut().unwrap();
            doc.set_viewport(glam::Vec2::new(1400.0, 900.0));
            doc.camera.zoom = 1.0;
            doc.camera.center = glam::Vec2::new(4.0, 4.0);
        }
        ed.set_tool(tools::ToolId::FreeTransform);

        // A real gesture: press on the document's corner, drag it.
        let mut pointer = ToolPointer::new();
        let doc_to_screen = |x: f32, y: f32| egui::pos2(700.0 + x - 4.0, 450.0 + y - 4.0);
        let at = |phase: ui::canvas::PointerPhase, pos: egui::Pos2| {
            ui::canvas::PointerInput::at(phase, glam::Vec2::new(pos.x, pos.y))
        };
        pointer.handle(
            &mut ed,
            at(ui::canvas::PointerPhase::Down, doc_to_screen(0.0, 0.0)),
            false,
            &[],
        );
        pointer.handle(
            &mut ed,
            at(ui::canvas::PointerPhase::Move, doc_to_screen(6.0, 0.0)),
            false,
            &[],
        );

        // Publish through the production publisher, then look at what the
        // chrome actually painted.
        let geometry = pointer.live_geometry();
        assert!(geometry.is_some(), "the session is live");
        let mut chrome = Chrome::new();
        // W3-A: View ▸ Layer Edges is on by default and outlines the 8x8
        // layer at (696, 446)..(704, 454) — a closed path within this test's
        // 3pt tolerance of the quad corner it looks for. Unticked, so the
        // only closed path near there is the transform's.
        chrome.emit(ui::Intent::SetViewFlag {
            flag: ui::ViewFlag::LayerEdges,
            on: false,
        });
        chrome.publish_tool_geometry(geometry, ed.active().map(|d| d.id()));
        let painted = painted_shapes(&mut chrome, &mut ed);
        let quad = painted
            .iter()
            .filter(|s| matches!(s, egui::Shape::Path(path) if path.closed))
            .any(|s| {
                let points = match s {
                    egui::Shape::Path(path) => &path.points,
                    _ => unreachable!(),
                };
                points
                    .iter()
                    .any(|p| (p.x - 702.0).abs() < 3.0 && (p.y - 446.0).abs() < 3.0)
                    || points
                        .iter()
                        .any(|p| (p.x - 702.0).abs() < 3.0 && (p.y - 450.0).abs() < 3.0)
            });
        assert!(
            quad,
            "the transform quad is painted at the dragged corner: {painted:?}"
        );

        // End the session: the same publication route clears, and the next
        // frame paints no quad there.
        assert!(pointer.cancel(&mut ed));
        let geometry = pointer.live_geometry();
        assert!(geometry.is_none());
        chrome.publish_tool_geometry(geometry, ed.active().map(|d| d.id()));
        assert!(chrome.workspace.canvas.sessions.transform.is_none());
        let painted = painted_shapes(&mut chrome, &mut ed);
        let quad = painted
            .iter()
            .filter(|s| matches!(s, egui::Shape::Path(path) if path.closed))
            .any(|s| match s {
                egui::Shape::Path(path) => path
                    .points
                    .iter()
                    .any(|p| (p.x - 702.0).abs() < 3.0 && (p.y - 446.0).abs() < 3.0),
                _ => false,
            });
        assert!(!quad, "the ended session paints no quad: {painted:?}");
    }

    /// W4-A: a live-gesture rig — one 8x8 document at 2000% centred in the
    /// 1400x900 test window (doc `(x, y)` lands at screen
    /// `(700 + 20(x-4), 450 + 20(y-4))`), a real [`ToolPointer`] with `tool`
    /// selected, and a chrome with Layer Edges off so the only closed paths
    /// and rectangles near the document are the session's.
    struct LiveRig {
        _dir: tempfile::TempDir,
        ed: Editor,
        pointer: ToolPointer,
        chrome: Chrome,
    }

    const LIVE_ZOOM: f32 = 20.0;

    fn live_screen(x: f32, y: f32) -> egui::Pos2 {
        egui::pos2(700.0 + (x - 4.0) * LIVE_ZOOM, 450.0 + (y - 4.0) * LIVE_ZOOM)
    }

    impl LiveRig {
        fn new(tool: tools::ToolId) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let p = png(dir.path(), "a.png");
            let mut ed = editor(&dir.path().join("config"));
            ed.open_path(&p).unwrap();
            {
                let doc = ed.active_mut().unwrap();
                doc.set_viewport(glam::Vec2::new(1400.0, 900.0));
                doc.camera.zoom = LIVE_ZOOM;
                doc.camera.center = glam::Vec2::new(4.0, 4.0);
            }
            ed.set_tool(tool);
            let mut chrome = Chrome::new();
            chrome.emit(ui::Intent::SetViewFlag {
                flag: ui::ViewFlag::LayerEdges,
                on: false,
            });
            Self {
                _dir: dir,
                ed,
                pointer: ToolPointer::new(),
                chrome,
            }
        }

        /// One pointer sample at document `(x, y)`, through the real route.
        fn send(&mut self, phase: ui::canvas::PointerPhase, x: f32, y: f32) {
            let at = live_screen(x, y);
            self.pointer.handle(
                &mut self.ed,
                ui::canvas::PointerInput::at(phase, glam::Vec2::new(at.x, at.y)),
                false,
                &[],
            );
        }

        fn down(&mut self, x: f32, y: f32) {
            self.send(ui::canvas::PointerPhase::Down, x, y);
        }
        fn drag(&mut self, x: f32, y: f32) {
            self.send(ui::canvas::PointerPhase::Move, x, y);
        }
        fn up(&mut self, x: f32, y: f32) {
            self.send(ui::canvas::PointerPhase::Up, x, y);
        }

        /// Escape: the pointer's own cancel route.
        fn escape(&mut self) {
            self.pointer.cancel(&mut self.ed);
        }

        /// Publish through the production publisher, then draw a frame and
        /// return what the chrome painted.
        fn frame(&mut self) -> Vec<egui::Shape> {
            let geometry = self.pointer.live_geometry();
            let active = self.ed.active().map(|d| d.id());
            self.chrome.publish_tool_geometry(geometry, active);
            painted_shapes(&mut self.chrome, &mut self.ed)
        }
    }

    fn near(p: egui::Pos2, q: egui::Pos2) -> bool {
        (p.x - q.x).abs() < 1.5 && (p.y - q.y).abs() < 1.5
    }

    /// Every egui shape, with `Shape::Vec`s flattened.
    fn flat(shapes: &[egui::Shape]) -> Vec<egui::Shape> {
        let mut out = Vec::new();
        for s in shapes {
            match s {
                egui::Shape::Vec(inner) => out.extend(flat(inner)),
                other => out.push(other.clone()),
            }
        }
        out
    }

    /// A painted rectangle whose corners are the two screen points.
    fn has_rect(shapes: &[egui::Shape], min: egui::Pos2, max: egui::Pos2) -> bool {
        flat(shapes).iter().any(|s| match s {
            egui::Shape::Rect(r) => near(r.rect.min, min) && near(r.rect.max, max),
            _ => false,
        })
    }

    /// A painted polyline passing through every one of `points`.
    fn has_polyline_through(shapes: &[egui::Shape], points: &[egui::Pos2]) -> bool {
        flat(shapes).iter().any(|s| match s {
            egui::Shape::Path(path) => points
                .iter()
                .all(|want| path.points.iter().any(|p| near(*p, *want))),
            _ => false,
        })
    }

    /// A painted square (an anchor) centred on `centre`.
    fn has_square_at(shapes: &[egui::Shape], centre: egui::Pos2) -> bool {
        flat(shapes).iter().any(|s| match s {
            egui::Shape::Rect(r) => near(r.rect.center(), centre) && r.rect.width() < 20.0,
            _ => false,
        })
    }

    fn painted_texts(shapes: &[egui::Shape]) -> Vec<String> {
        flat(shapes)
            .iter()
            .filter_map(|s| match s {
                egui::Shape::Text(t) => Some(t.galley.text().to_owned()),
                _ => None,
            })
            .collect()
    }

    /// W4-A: a crop drag paints its box (outline and shaded surround) while
    /// the button is down; released, the box stays up waiting for Enter;
    /// Escape takes it down.
    #[test]
    fn a_crop_drag_paints_its_box_until_escape() {
        let mut rig = LiveRig::new(tools::ToolId::Crop);
        // Mid-drag the box follows the pointer; released it snaps outward
        // to whole pixels, (1, 1)..(6, 5).
        let (min, max) = (live_screen(1.25, 1.25), live_screen(5.75, 4.75));
        assert!(
            !has_rect(&rig.frame(), min, max),
            "nothing before the press"
        );
        rig.down(1.25, 1.25);
        rig.drag(5.75, 4.75);
        let painted = rig.frame();
        assert!(
            has_rect(&painted, min, max),
            "mid-drag the crop box is painted at {min:?}..{max:?}: {painted:?}"
        );
        // The shaded surround: a scrim band above the box, full width.
        assert!(
            flat(&painted).iter().any(|s| matches!(
                s,
                egui::Shape::Rect(r) if (r.rect.max.y - min.y).abs() < 1.5
                    && r.rect.width() > 1000.0
            )),
            "the outside of the crop is shaded: {painted:?}"
        );
        rig.up(5.75, 4.75);
        let (min, max) = (live_screen(1.0, 1.0), live_screen(6.0, 5.0));
        let painted = rig.frame();
        assert!(
            has_rect(&painted, min, max),
            "released, the crop box waits for Enter: {painted:?}"
        );
        rig.escape();
        let painted = rig.frame();
        assert!(
            !has_rect(&painted, min, max),
            "Escape takes the crop box down: {painted:?}"
        );
    }

    /// W4-D round 2: one pointer sample at document `(x, y)` with the Crop
    /// options the chrome's own options bar holds — the settings a real
    /// press is seeded with.
    fn crop_send(rig: &mut LiveRig, phase: ui::canvas::PointerPhase, x: f32, y: f32) {
        let settings: Vec<(String, tools::ToolSetting)> = rig
            .chrome
            .tool_options(tools::ToolId::Crop)
            .into_iter()
            .map(|(key, value)| {
                let setting = match value {
                    ui::OptionValue::Float(v) => tools::ToolSetting::Float(v),
                    ui::OptionValue::Int(v) => tools::ToolSetting::Int(v),
                    ui::OptionValue::Bool(v) => tools::ToolSetting::Bool(v),
                    ui::OptionValue::Choice(v) => tools::ToolSetting::Choice(v),
                    ui::OptionValue::Color(v) => tools::ToolSetting::Color(v),
                };
                (key, setting)
            })
            .collect();
        let at = live_screen(x, y);
        let out = rig.pointer.handle(
            &mut rig.ed,
            ui::canvas::PointerInput::at(phase, glam::Vec2::new(at.x, at.y)),
            false,
            &settings,
        );
        assert!(out.failed.is_none(), "the press refused: {:?}", out.failed);
    }

    /// The crop-guide lines a frame painted inside the screen box `keep`.
    fn crop_guide_lines(shapes: &[egui::Shape], keep: egui::Rect) -> usize {
        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let style = ui::canvas::CanvasStyle::from_context(&ctx);
        segments_in(&flat(shapes), style.crop_guide, keep.shrink(0.5)).len()
    }

    /// W4-D round 2: the Overlay chosen in the options bar is the guide the
    /// chrome paints inside the crop box — through the chrome's real
    /// painter, not a rebuilt overlay: Grid's fourteen lines, Diagonal's two,
    /// None's none, and the default Rule of Thirds' four.
    #[test]
    fn the_crop_overlay_choice_is_the_guide_the_chrome_paints() {
        use ui::canvas::PointerPhase::{Down, Move, Up};
        let index = |label: &str| {
            tools::edit::CROP_OVERLAY_LABELS
                .iter()
                .position(|l| *l == label)
                .unwrap()
        };
        let keep = egui::Rect::from_two_pos(live_screen(1.0, 1.0), live_screen(7.0, 7.0));
        for (choice, want) in [
            (Some("Grid"), 14usize),
            (Some("Diagonal"), 2),
            (Some("None"), 0),
            (None, 4),
        ] {
            let mut rig = LiveRig::new(tools::ToolId::Crop);
            if let Some(label) = choice {
                rig.chrome.set_tool_option(
                    tools::ToolId::Crop,
                    "overlay",
                    ui::OptionValue::Choice(index(label)),
                );
            }
            // Off the pixel grid, as a real hand is: the release snaps the
            // box outward to (1, 1)..(7, 7).
            crop_send(&mut rig, Down, 1.25, 1.25);
            crop_send(&mut rig, Move, 6.75, 6.75);
            let held = rig.frame();
            let dragged =
                egui::Rect::from_two_pos(live_screen(1.25, 1.25), live_screen(6.75, 6.75));
            assert!(has_rect(&held, dragged.min, dragged.max), "{held:?}");
            assert_eq!(
                crop_guide_lines(&held, dragged),
                want,
                "{choice:?} mid-drag: {held:?}"
            );
            crop_send(&mut rig, Up, 6.75, 6.75);
            let released = rig.frame();
            assert!(has_rect(&released, keep.min, keep.max), "{released:?}");
            assert_eq!(
                crop_guide_lines(&released, keep),
                want,
                "{choice:?} released"
            );
        }
    }

    /// W8-C: a Perspective Crop quad is painted through the chrome's real
    /// painter with a handle square on each corner and a 3 x 3 grid inside;
    /// dragging a corner moves its handle, drawn as the selected one.
    #[test]
    fn a_perspective_crop_quad_is_painted_with_corner_handles_and_a_grid() {
        let mut rig = LiveRig::new(tools::ToolId::PerspectiveCrop);
        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let style = ui::canvas::CanvasStyle::from_context(&ctx);
        let layout = ui::canvas::HandleLayout::default();
        let handle = |shapes: &[egui::Shape], at: egui::Pos2, fill: egui::Color32| {
            flat(shapes).iter().any(|s| match s {
                egui::Shape::Rect(r) => {
                    near(r.rect.center(), at)
                        && (r.rect.width() - layout.handle_pt).abs() < 0.5
                        && r.fill == fill
                }
                _ => false,
            })
        };
        rig.down(1.0, 1.0);
        rig.drag(7.0, 7.0);
        rig.up(7.0, 7.0);
        let held = rig.frame();
        for (x, y) in [(1.0, 1.0), (7.0, 1.0), (7.0, 7.0), (1.0, 7.0)] {
            assert!(
                handle(&held, live_screen(x, y), style.handle_fill),
                "no handle on corner ({x}, {y}): {held:?}"
            );
        }
        let inside = egui::Rect::from_two_pos(live_screen(1.0, 1.0), live_screen(7.0, 7.0));
        assert_eq!(
            segments_in(&flat(&held), style.crop_guide, inside).len(),
            6,
            "three grid lines each way"
        );
        // Grab the top-right corner and pull it in: its handle follows and
        // is the emphasised one while it is held.
        rig.down(7.0, 1.0);
        rig.drag(6.0, 2.0);
        let dragging = rig.frame();
        assert!(
            handle(&dragging, live_screen(6.0, 2.0), style.handle_selected),
            "the dragged corner's handle: {dragging:?}"
        );
        assert!(!handle(&dragging, live_screen(7.0, 1.0), style.handle_fill));
        rig.up(6.0, 2.0);
        rig.escape();
        let gone = rig.frame();
        assert!(!handle(&gone, live_screen(1.0, 1.0), style.handle_fill));
    }

    /// W8-C: while a Type Mask run is typed the quick-mask red covers the
    /// canvas everywhere but the glyphs — a textured quad over the document,
    /// built from the draft the canvas shows — and it goes with the session.
    #[test]
    fn a_type_mask_session_shows_the_quick_mask_red_outside_its_glyphs() {
        compositor::load_font(dejavu::sans::regular().to_vec());
        let mut rig = LiveRig::new(tools::ToolId::HorizontalTypeMask);
        let textured = |shapes: &[egui::Shape]| {
            flat(shapes).iter().any(|s| match s {
                egui::Shape::Mesh(m) => {
                    m.texture_id != egui::TextureId::default()
                        && m.vertices
                            .iter()
                            .any(|v| near(v.pos, live_screen(0.0, 0.0)))
                        && m.vertices
                            .iter()
                            .any(|v| near(v.pos, live_screen(8.0, 8.0)))
                }
                _ => false,
            })
        };
        rig.down(1.0, 1.0);
        rig.up(1.0, 1.0);
        rig.pointer
            .text_edit(&mut rig.ed, tools::TextEdit::Insert("WW"));
        let typing = rig.frame();
        assert!(textured(&typing), "no overlay over the canvas: {typing:?}");
        let image = rig.chrome.type_mask_overlay_image().expect("an overlay");
        assert_eq!(image.size, [8, 8], "one texel per canvas pixel");
        let alphas: Vec<u8> = image.pixels.iter().map(|p| p.a()).collect();
        let (lo, hi) = (*alphas.iter().min().unwrap(), *alphas.iter().max().unwrap());
        assert!(hi >= 120, "the red shows outside the glyphs: {alphas:?}");
        assert!(lo < hi / 2, "the glyphs are cut out of the red: {alphas:?}");
        // Above-left of the click is outside every glyph: full quick-mask red.
        assert!(alphas[0] >= 120, "{alphas:?}");
        // The theme's ChannelRed (premultiplied by egui): red-dominant.
        let p = image.pixels[0];
        let (r, g, b) = (p.r() as i32, p.g() as i32, p.b() as i32);
        assert!(
            r > g + 60 && (g - b).abs() <= 2,
            "the overlay is not the channel red: {p:?}"
        );
        rig.pointer.text_edit(&mut rig.ed, tools::TextEdit::Confirm);
        let done = rig.frame();
        assert!(!textured(&done), "the overlay outlived its session");
        assert!(rig.chrome.type_mask_overlay_image().is_none());
    }

    /// W4-D round 2: in Straighten mode the line is painted while it is
    /// dragged, with the rotation it asks for printed by its far end, and it
    /// stays up with the whole-canvas box once released, until Escape.
    #[test]
    fn a_straighten_drag_paints_its_line_and_angle_until_escape() {
        use ui::canvas::PointerPhase::{Down, Move, Up};
        let mut rig = LiveRig::new(tools::ToolId::Crop);
        rig.chrome.set_tool_option(
            tools::ToolId::Crop,
            "straighten_line",
            ui::OptionValue::Bool(true),
        );
        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let style = ui::canvas::CanvasStyle::from_context(&ctx);
        // A 10 degree line from doc (1, 2).
        let (x0, y0) = (1.0f32, 2.0f32);
        let (x1, y1) = (7.0f32, 2.0 + 6.0 * 10f32.to_radians().tan());
        let (a, b) = (live_screen(x0, y0), live_screen(x1, y1));
        let has_line = |shapes: &[egui::Shape]| {
            segments_in(&flat(shapes), style.path_stroke, egui::Rect::EVERYTHING)
                .iter()
                .any(|[p, q]| near(*p, a) && near(*q, b))
        };
        let angle_label =
            |shapes: &[egui::Shape]| painted_texts(shapes).into_iter().any(|t| t == "10.0\u{b0}");
        crop_send(&mut rig, Down, x0, y0);
        crop_send(&mut rig, Move, x1, y1);
        let held = rig.frame();
        assert!(has_line(&held), "no straighten line mid-drag: {held:?}");
        assert!(
            angle_label(&held),
            "no angle mid-drag: {:?}",
            painted_texts(&held)
        );
        // The box shown is the whole canvas: what Enter will keep.
        let canvas = (live_screen(0.0, 0.0), live_screen(8.0, 8.0));
        assert!(has_rect(&held, canvas.0, canvas.1), "{held:?}");
        crop_send(&mut rig, Up, x1, y1);
        let released = rig.frame();
        assert!(has_line(&released), "released, the line waits for Enter");
        assert!(angle_label(&released));
        rig.escape();
        let gone = rig.frame();
        assert!(
            !has_line(&gone) && !angle_label(&gone),
            "Escape takes it down"
        );
    }

    /// W4-A: a rectangular marquee drag paints its marching rubber band while
    /// the button is down, and nothing once the release has made the
    /// selection.
    #[test]
    fn a_marquee_drag_paints_its_rubber_band_until_release() {
        let mut rig = LiveRig::new(tools::ToolId::RectMarquee);
        let corners = [
            live_screen(1.0, 1.0),
            live_screen(6.0, 1.0),
            live_screen(6.0, 5.0),
            live_screen(1.0, 5.0),
        ];
        rig.down(1.0, 1.0);
        rig.drag(6.0, 5.0);
        let painted = rig.frame();
        assert!(
            has_polyline_through(&painted, &corners),
            "mid-drag the rubber band is painted through {corners:?}: {painted:?}"
        );
        assert!(
            flat(&painted)
                .iter()
                .any(|s| matches!(s, egui::Shape::LineSegment { .. })),
            "the band marches (dashes over the base run)"
        );
        rig.up(6.0, 5.0);
        let painted = rig.frame();
        assert!(
            !has_polyline_through(&painted, &corners),
            "released, no rubber band remains: {painted:?}"
        );
    }

    /// W4-A: an elliptical marquee's band is the ellipse, not its box.
    #[test]
    fn an_elliptical_marquee_drag_paints_an_ellipse() {
        let mut rig = LiveRig::new(tools::ToolId::EllipseMarquee);
        rig.down(1.0, 1.0);
        rig.drag(7.0, 5.0);
        let painted = rig.frame();
        // The ellipse's right and bottom extremes, and not the box's corner.
        let (right, bottom) = (live_screen(7.0, 3.0), live_screen(4.0, 5.0));
        assert!(
            has_polyline_through(&painted, &[right, bottom]),
            "the ellipse is painted: {painted:?}"
        );
        assert!(!has_polyline_through(&painted, &[live_screen(7.0, 5.0)]));
        rig.up(7.0, 5.0);
        assert!(!has_polyline_through(&rig.frame(), &[right, bottom]));
    }

    /// W4-A: a freehand lasso paints the path it has traced while the
    /// button is down; the release closes it into a selection and the path
    /// is gone.
    #[test]
    fn a_lasso_drag_paints_its_path_until_release() {
        let mut rig = LiveRig::new(tools::ToolId::Lasso);
        let traced = [
            live_screen(1.0, 1.0),
            live_screen(6.0, 1.0),
            live_screen(6.0, 6.0),
        ];
        rig.down(1.0, 1.0);
        rig.drag(6.0, 1.0);
        rig.drag(6.0, 6.0);
        let painted = rig.frame();
        assert!(
            has_polyline_through(&painted, &traced),
            "mid-drag the lasso path is painted through {traced:?}: {painted:?}"
        );
        rig.up(6.0, 6.0);
        let painted = rig.frame();
        assert!(
            !has_polyline_through(&painted, &traced),
            "released, no lasso path remains: {painted:?}"
        );
    }

    /// W4-A: a polygonal lasso's vertices stay painted between clicks (its
    /// release is not the end of the gesture); Escape takes them down.
    #[test]
    fn a_polygonal_lasso_paints_its_vertices_until_escape() {
        let mut rig = LiveRig::new(tools::ToolId::PolygonalLasso);
        let placed = [live_screen(1.0, 1.0), live_screen(6.0, 2.0)];
        rig.down(1.0, 1.0);
        rig.up(1.0, 1.0);
        rig.down(6.0, 2.0);
        rig.up(6.0, 2.0);
        let painted = rig.frame();
        assert!(
            has_polyline_through(&painted, &placed),
            "the polygon so far is painted: {painted:?}"
        );
        rig.escape();
        assert!(!has_polyline_through(&rig.frame(), &placed));
    }

    /// W4-A: the pen paints its anchors and the path between them as they
    /// are placed — not only after Enter — plus the handles a drag pulls;
    /// Escape takes them down.
    #[test]
    fn a_pen_path_paints_anchors_and_handles_until_escape() {
        let mut rig = LiveRig::new(tools::ToolId::Pen);
        let (a, b, c) = (
            live_screen(1.0, 1.0),
            live_screen(6.0, 1.0),
            live_screen(6.0, 6.0),
        );
        rig.down(1.0, 1.0);
        rig.up(1.0, 1.0);
        rig.down(6.0, 1.0);
        rig.up(6.0, 1.0);
        // The third press is dragged: it pulls the anchor's handles out.
        rig.down(6.0, 6.0);
        rig.drag(6.0, 3.0);
        let painted = rig.frame();
        for (name, at) in [("first", a), ("second", b), ("third", c)] {
            assert!(
                has_square_at(&painted, at),
                "the {name} anchor is painted at {at:?}: {painted:?}"
            );
        }
        assert!(
            has_polyline_through(&painted, &[a, b, c]),
            "the path between the anchors is painted: {painted:?}"
        );
        // The dragged anchor's handles: out at (6, 3), in mirrored at (6, 9).
        let handle = live_screen(6.0, 3.0);
        assert!(
            flat(&painted).iter().any(|s| matches!(
                s,
                egui::Shape::LineSegment { points, .. }
                    if near(points[0], c) && near(points[1], handle)
            )),
            "the pulled handle's direction line is painted: {painted:?}"
        );
        rig.up(6.0, 3.0);
        assert!(has_square_at(&rig.frame(), a), "released, still authoring");
        rig.escape();
        let painted = rig.frame();
        assert!(
            !has_square_at(&painted, a) && !has_square_at(&painted, c),
            "Escape takes the anchors down: {painted:?}"
        );
    }

    /// W4-I: the pen's uncommitted path is the Paths panel's Work Path,
    /// through the production publisher: the row appears with the first
    /// anchors, holds exactly the segments placed so far, and goes with
    /// Escape.
    #[test]
    fn the_pens_uncommitted_path_is_the_paths_panels_work_path() {
        use vector::{PathEl, Point};
        let mut rig = LiveRig::new(tools::ToolId::Pen);
        rig.chrome
            .workspace
            .dock
            .set_open(ui::dock::PanelId::Paths, true);
        rig.chrome.workspace.dock.raise(ui::dock::PanelId::Paths);
        let work = ui::strings::tr("ui.docks.paths.work.path");
        let has_row = |shapes: &[egui::Shape]| {
            flat(shapes)
                .iter()
                .any(|s| matches!(s, egui::Shape::Text(t) if t.galley.text() == work))
        };
        assert!(!has_row(&rig.frame()), "no pen path, no Work Path row");

        rig.down(1.0, 1.0);
        rig.up(1.0, 1.0);
        rig.down(6.0, 1.0);
        rig.up(6.0, 1.0);
        let painted = rig.frame();
        assert!(
            has_row(&painted),
            "the Paths panel lists the pen's path as the Work Path"
        );
        let path = rig
            .chrome
            .workspace
            .paths
            .work_path
            .clone()
            .expect("the pen's path is the Work Path");
        let close = |p: &Point, x: f64, y: f64| (p.x - x).abs() < 1e-3 && (p.y - y).abs() < 1e-3;
        assert!(
            matches!(
                path.elements(),
                [PathEl::MoveTo(a), PathEl::LineTo(b)] if close(a, 1.0, 1.0) && close(b, 6.0, 1.0)
            ),
            "the Work Path is the pen's anchors, in document pixels: {:?}",
            path.elements()
        );

        rig.escape();
        assert!(!has_row(&rig.frame()), "Escape takes the Work Path down");
        assert!(rig.chrome.workspace.paths.work_path.is_none());
    }

    /// W4-A: the slice tool paints each drawn slice with its number, and the
    /// one being dragged; Escape drops the set and its regions.
    #[test]
    fn slices_paint_numbered_regions_until_escape() {
        let mut rig = LiveRig::new(tools::ToolId::Slice);
        let dragged = (live_screen(1.25, 1.25), live_screen(2.75, 2.75));
        // Released, the slice snaps outward to whole pixels.
        let first = (live_screen(1.0, 1.0), live_screen(3.0, 3.0));
        let second = (live_screen(4.0, 4.0), live_screen(7.0, 6.0));
        rig.down(1.25, 1.25);
        rig.drag(2.75, 2.75);
        let painted = rig.frame();
        assert!(
            has_rect(&painted, dragged.0, dragged.1),
            "the slice being dragged is painted: {painted:?}"
        );
        rig.up(2.75, 2.75);
        rig.down(4.0, 4.0);
        rig.drag(7.0, 6.0);
        let painted = rig.frame();
        assert!(
            has_rect(&painted, first.0, first.1),
            "the drawn slice stays"
        );
        assert!(
            has_rect(&painted, second.0, second.1),
            "the second slice, mid-drag"
        );
        let texts = painted_texts(&painted);
        assert!(
            texts.iter().any(|t| t == "01") && texts.iter().any(|t| t == "02"),
            "the slices are numbered: {texts:?}"
        );
        rig.escape();
        let painted = rig.frame();
        assert!(
            !has_rect(&painted, first.0, first.1) && !has_rect(&painted, second.0, second.1),
            "Escape takes the slices down: {painted:?}"
        );
    }

    /// XB: a real shape drag, published through the production publisher,
    /// paints a W/H label beside the pointer carrying both dragged numbers;
    /// the release takes it down through the same route.
    #[test]
    fn a_shape_drag_paints_a_w_h_readout_by_the_pointer_until_release() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wide.png");
        std::fs::write(
            &path,
            raster::encode(raster::ExportFormat::Png, 200, 100, &[9u8; 200 * 100 * 4]).unwrap(),
        )
        .unwrap();
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&path).unwrap();
        {
            let doc = ed.active_mut().unwrap();
            doc.set_viewport(glam::Vec2::new(1400.0, 900.0));
            doc.camera.zoom = 1.0;
            doc.camera.center = glam::Vec2::new(100.0, 50.0);
        }
        ed.set_tool(tools::ToolId::Rectangle);
        let doc_to_screen = |x: f32, y: f32| egui::pos2(700.0 + x - 100.0, 450.0 + y - 50.0);
        let at = |phase: ui::canvas::PointerPhase, pos: egui::Pos2| {
            ui::canvas::PointerInput::at(phase, glam::Vec2::new(pos.x, pos.y))
        };
        let mut pointer = ToolPointer::new();
        assert!(pointer.live_readout().is_none(), "no gesture, no readout");
        pointer.handle(
            &mut ed,
            at(ui::canvas::PointerPhase::Down, doc_to_screen(10.0, 20.0)),
            false,
            &[],
        );
        pointer.handle(
            &mut ed,
            at(ui::canvas::PointerPhase::Move, doc_to_screen(133.0, 77.0)),
            false,
            &[],
        );
        let readout = pointer.live_readout();
        let (doc_id, live) = readout.expect("a shape drag publishes a readout");
        assert_eq!(Some(doc_id), ed.active().map(|d| d.id()));
        assert!((live.width_px - 123.0).abs() < 1e-3, "{live:?}");
        assert!((live.height_px - 57.0).abs() < 1e-3, "{live:?}");

        let mut chrome = Chrome::new();
        chrome.publish_tool_readout(readout);
        let labels = |painted: &[egui::Shape]| -> Vec<(String, egui::Pos2)> {
            painted
                .iter()
                .filter_map(|s| match s {
                    egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.pos)),
                    _ => None,
                })
                .collect()
        };
        let painted = labels(&painted_shapes(&mut chrome, &mut ed));
        let pointer_at = doc_to_screen(133.0, 77.0);
        let label = painted
            .iter()
            .find(|(text, _)| text.contains("W: 123 px") && text.contains("H: 57 px"));
        let (_, pos) = label.unwrap_or_else(|| panic!("no W/H readout painted: {painted:?}"));
        assert!(
            pos.x >= pointer_at.x && pos.y >= pointer_at.y && pos.distance(pointer_at) < 60.0,
            "the readout sits beside the pointer at {pointer_at:?}, not at {pos:?}"
        );

        // Release: the readout is gone, and the next frame paints none.
        pointer.handle(
            &mut ed,
            at(ui::canvas::PointerPhase::Up, doc_to_screen(133.0, 77.0)),
            false,
            &[],
        );
        let readout = pointer.live_readout();
        assert!(readout.is_none(), "a released drag has no readout");
        chrome.publish_tool_readout(readout);
        let painted = labels(&painted_shapes(&mut chrome, &mut ed));
        assert!(
            !painted.iter().any(|(text, _)| text.contains("W: 123")),
            "the released drag still paints a readout: {painted:?}"
        );
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

        let out = run_chrome(&mut ed, None);
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

        let out = run_chrome(&mut ed, Some(ui::view::ids::layer_eye(other)));
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
        let out = run_chrome(&mut ed, Some(ui::view::ids::layer_row(other)));
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

        let out = run_chrome(&mut ed, Some(ui::view::ids::new_layer()));
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

        let out = run_chrome(&mut ed, Some(ui::view::ids::history_row(1)));
        assert_eq!(out.history_jump, Some(1), "{out:?}");

        // ...and performing it really moves the document there.
        let moved = ed.jump_history(out.history_jump.unwrap());
        assert_eq!(moved, 2, "two steps undone");
        assert_eq!(ed.active().unwrap().history_depth(), 1);
        assert_eq!(ed.active().unwrap().document.layers.len(), 2);

        // A row ahead of us walks forward again, through History's redo.
        let out = run_chrome(&mut ed, Some(ui::view::ids::history_row(3)));
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
            chrome.ui(ctx, &mut ed);
        });
        assert_eq!(chrome.workspace().color.foreground(), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(chrome.workspace().color.background(), [0.0, 0.0, 1.0, 1.0]);

        // ...and a colour the panel emits comes back out as a request the shell
        // performs, rather than being written straight into the workspace.
        ed.dispatch(Action::SwapColors).unwrap();
        let _ = ctx.run(raw_input(Vec::new()), |ctx| {
            chrome.ui(ctx, &mut ed);
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
                    let _ = chrome.ui(ctx, &mut ed);
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

        let painted: Vec<String> = painted_text(&mut ed).into_iter().map(|(t, _)| t).collect();
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
                chrome.ui(ctx, &mut ed);
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
            chrome.ui(ctx, &mut ed);
        });
        // Actions is the one panel the default (Essentials) layout leaves
        // closed — W2-D opened Channels and Paths with Layers, and Navigator
        // sits in the narrow right column — so it is the one whose opening
        // this test can watch.
        let panel = ui::PanelId::Actions;
        assert!(
            !chrome.workspace().dock.is_open(panel),
            "the default layout opens {panel:?}, so opening it proves nothing"
        );

        chrome
            .workspace
            .emit(ui::Intent::SetPanelOpen { panel, open: true });
        let mut out = ChromeOutput::default();
        let _ = ctx.run(raw_input(Vec::new()), |ctx| {
            out = chrome.ui(ctx, &mut ed);
        });
        assert_eq!(
            out.workspace,
            vec![ui::Intent::SetPanelOpen { panel, open: true }],
            "{out:?}"
        );
        assert!(
            chrome.workspace().dock.is_open(panel),
            "the panel was reported but never opened"
        );

        // ...and the next frame really draws it.
        let painted: Vec<String> = painted_text_with(&ctx, &mut chrome, &mut ed);
        assert!(
            painted.iter().any(|t| t == panel.title()),
            "the {panel:?} panel never appeared: {painted:?}"
        );
    }

    /// Draw two more frames on an existing chrome and read back what they said.
    fn painted_text_with(
        ctx: &egui::Context,
        chrome: &mut Chrome,
        editor: &mut Editor,
    ) -> Vec<String> {
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
        editor: &mut Editor,
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
            chrome.ui(ctx, &mut ed);
        });
        for panel in ui::PanelId::ALL.iter().copied() {
            chrome
                .workspace
                .emit(ui::Intent::SetPanelOpen { panel, open: true });
        }
        for _ in 0..4 {
            let _ = painted_characters(&ctx, &mut chrome, &mut ed);
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
            let _ = painted_characters(&ctx, &mut chrome, &mut ed);
            painted.extend(painted_characters(&ctx, &mut chrome, &mut ed));
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
            chrome.ui(ctx, &mut ed);
        });
        chrome
            .workspace
            .emit(ui::Intent::SetViewCenter((12.0, 34.0)));
        chrome.workspace.emit(ui::Intent::SetZoom(2.5));
        let mut out = ChromeOutput::default();
        let _ = ctx.run(raw_input(Vec::new()), |ctx| {
            out = chrome.ui(ctx, &mut ed);
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
        fn new(editor: &mut Editor) -> Self {
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
        fn settle(&mut self, editor: &mut Editor) {
            for _ in 0..4 {
                self.frame(editor);
            }
        }

        fn frame(&mut self, editor: &mut Editor) -> ChromeOutput {
            let mut out = ChromeOutput::default();
            let chrome = &mut self.chrome;
            let _ = self.ctx.run(raw_input(Vec::new()), |ctx| {
                out = chrome.ui(ctx, editor);
            });
            out
        }

        /// The strings this layout paints as text.
        ///
        /// C3's validate: the start screen must actually be *painted*, not
        /// merely laid out — galleys are still readable from the shape list
        /// before tessellation, so this walks `FullOutput::shapes`.
        fn painted_texts(&mut self, editor: &mut Editor) -> Vec<String> {
            let mut out = ChromeOutput::default();
            let chrome = &mut self.chrome;
            let full = self.ctx.run(raw_input(Vec::new()), |ctx| {
                out = chrome.ui(ctx, editor);
            });
            drop(out);
            full.shapes
                .iter()
                .filter_map(|clipped| match &clipped.shape {
                    egui::Shape::Text(text) => Some(text.galley.text().to_string()),
                    _ => None,
                })
                .collect()
        }

        /// Click the centre of the first galley painted with exactly `label`
        /// — a menu-bar title or a menu row, which have no stable ids — and
        /// return what that frame meant. One frame is drawn first so egui
        /// knows the rectangle the press lands in.
        fn click_text(&mut self, editor: &mut Editor, label: &str) -> ChromeOutput {
            let mut out = ChromeOutput::default();
            let chrome = &mut self.chrome;
            let full = self.ctx.run(raw_input(Vec::new()), |ctx| {
                out = chrome.ui(ctx, editor);
            });
            let rect = full
                .shapes
                .iter()
                .find_map(|clipped| match &clipped.shape {
                    egui::Shape::Text(text) if text.galley.text() == label => {
                        Some(egui::Rect::from_min_size(text.pos, text.galley.size()))
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{label:?} was never painted"));
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
            let mut out = ChromeOutput::default();
            let chrome = &mut self.chrome;
            let _ = self.ctx.run(raw_input(events), |ctx| {
                out = chrome.ui(ctx, editor);
            });
            out
        }

        /// Click a widget by id and return what that frame meant.
        fn click(&mut self, editor: &mut Editor, id: egui::Id) -> ChromeOutput {
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

        /// Every shape one more frame painted, pre-tessellation.
        fn shapes(&mut self, editor: &mut Editor) -> Vec<egui::Shape> {
            let chrome = &mut self.chrome;
            let full = self.ctx.run(raw_input(Vec::new()), |ctx| {
                let _ = chrome.ui(ctx, editor);
            });
            full.shapes
                .iter()
                .map(|clipped| clipped.shape.clone())
                .collect()
        }

        /// Every string one more frame painted, with where it landed.
        fn painted_text_rects(&mut self, editor: &mut Editor) -> Vec<(String, egui::Rect)> {
            self.shapes(editor)
                .into_iter()
                .filter_map(|shape| match shape {
                    egui::Shape::Text(text) => Some((
                        text.galley.text().to_string(),
                        egui::Rect::from_min_size(text.pos, text.galley.size()),
                    )),
                    _ => None,
                })
                .collect()
        }

        /// Every string the status bar itself painted on one more frame,
        /// with where it landed.
        ///
        /// The bar's shapes are the ones clipped to the bar: egui clips a
        /// galley at draw time, not in the shape list, so a dock row laid
        /// out just above the bar keeps its full rectangle in
        /// `FullOutput::shapes` even where the bar is painted over it. A
        /// band-of-the-window filter caught those rows; this does not.
        fn status_bar_texts(&mut self, editor: &mut Editor) -> Vec<(String, egui::Rect)> {
            let chrome = &mut self.chrome;
            let full = self.ctx.run(raw_input(Vec::new()), |ctx| {
                let _ = chrome.ui(ctx, editor);
            });
            let bar = self
                .panel_rect("raster-status")
                .expect("the status bar was drawn");
            full.shapes
                .iter()
                .filter_map(|clipped| match &clipped.shape {
                    egui::Shape::Text(text) if clipped.clip_rect.min.y >= bar.top() - 0.5 => {
                        Some((
                            text.galley.text().to_string(),
                            egui::Rect::from_min_size(text.pos, text.galley.size()),
                        ))
                    }
                    _ => None,
                })
                .collect()
        }

        /// The rectangle egui gave a named panel on the last frame, or
        /// `None` when no such panel was ever drawn on this context.
        fn panel_rect(&self, id: &str) -> Option<egui::Rect> {
            egui::containers::panel::PanelState::load(&self.ctx, egui::Id::new(id))
                .map(|state| state.rect)
        }

        /// The screen rect a drawn widget occupies, for width assertions.
        fn read_rect(&self, id: egui::Id) -> Option<egui::Rect> {
            self.ctx.read_response(id).map(|r| r.rect)
        }

        /// Press on `from_id`, drag across `to_id`, release: the tab-strip
        /// drag gesture, one frame per phase the way a real drag spans them.
        fn drag(
            &mut self,
            editor: &mut Editor,
            from_id: egui::Id,
            to_id: egui::Id,
        ) -> ChromeOutput {
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
        fn type_into(&mut self, editor: &mut Editor, id: egui::Id, text: &str) -> ChromeOutput {
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
        fn right_click(&mut self, editor: &mut Editor, id: egui::Id) -> ChromeOutput {
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
        fn middle_click(&mut self, editor: &mut Editor, id: egui::Id) -> ChromeOutput {
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

        let mut window = Window::new(&mut ed);
        let out = window.click(&mut ed, Chrome::start_recent_id(0));
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
        let mut ed = editor(dir.path());
        assert!(ed.documents().is_empty());
        let mut window = Window::new(&mut ed);
        let out = window.click(&mut ed, egui::Id::new("raster-start-new"));
        assert!(
            out.actions.contains(&Action::NewDocument),
            "the New button meant {out:?}"
        );
        let out = window.click(&mut ed, egui::Id::new("raster-start-open"));
        assert!(
            out.actions.contains(&Action::Open),
            "the Open button meant {out:?}"
        );
    }

    /// C3: the start screen must be *painted* on the empty state — the audit
    /// found a build where its shapes were emitted but never reached the
    /// screen (the palette's ScrollArea batch poisoned the egui pass). The
    /// galley strings are asserted in the shape list when no document is
    /// open, and absent when one is.
    #[test]
    fn the_start_screen_title_and_buttons_are_painted_only_when_empty() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(&dir.path().join("config"));
        assert!(ed.documents().is_empty());

        let mut window = Window::new(&mut ed);
        let texts = window.painted_texts(&mut ed);
        let joined = texts.join("\n");
        assert!(
            joined.contains("Raster Studio"),
            "the start-screen title must be painted; got {joined:?}"
        );
        assert!(
            texts.iter().any(|t| t == "New"),
            "the New button must be painted; got {texts:?}"
        );
        assert!(
            texts.iter().any(|t| t == "Open\u{2026}"),
            "the Open button must be painted; got {texts:?}"
        );

        // A document open means the start screen is gone entirely.
        let mut ed = {
            let mut ed = editor(&dir.path().join("config"));
            ed.open_path(&png(dir.path(), "shown.png")).unwrap();
            ed
        };
        assert_eq!(ed.documents().len(), 1);
        let texts = window.painted_texts(&mut ed);
        let joined = texts.join("\n");
        assert!(
            !joined.contains("Raster Studio"),
            "the start screen must not paint over a document; got {joined:?}"
        );
        assert!(
            !texts.iter().any(|t| t == "Open\u{2026}"),
            "the start screen must not paint over a document; got {texts:?}"
        );
    }

    #[test]
    fn a_middle_click_on_a_tab_closes_its_document() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&png(dir.path(), "one.png")).unwrap();
        ed.open_path(&png(dir.path(), "two.png")).unwrap();
        assert_eq!(ed.documents().len(), 2);

        let mut window = Window::new(&mut ed);
        let out = window.middle_click(&mut ed, Chrome::tab_id(1));
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

        let mut window = Window::new(&mut ed);
        let _ = window.right_click(&mut ed, Chrome::tab_id(1));
        // The shared drawer draws the menu inside the next frame's chrome;
        // one quiet frame lets it appear.
        let _ = window.frame(&mut ed);

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
        let out = window.click(&mut ed, ui::context_menu::ids::context_item(1));
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
    fn a_real_click_on_image_size_in_the_menu_bar_opens_its_dialog() {
        // The route a user takes, with a real pointer: press "Image" in the
        // drawn menu bar, then press the "Image Size…" row in the popup it
        // opened. The row's click hands its intent to `Chrome::menu_click`,
        // which asks the dialog host first — so the dialog is open at the end
        // of that frame and *nothing* was recorded for `perform`, which has
        // no arm for Image Size and used to answer "no implementation".
        let dir = tempfile::tempdir().unwrap();
        let p = png(dir.path(), "a.png");
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&p).unwrap();
        let mut window = Window::new(&mut ed);
        assert!(!window.chrome.dialog_open());

        let out = window.click_text(&mut ed, "Image");
        assert!(
            out.menu.is_empty(),
            "opening the menu performed something: {out:?}"
        );
        assert!(
            !window.chrome.dialog_open(),
            "opening the menu opened a dialog"
        );

        let out = window.click_text(&mut ed, "Image Size…");
        assert!(
            window.chrome.dialog_open(),
            "the Image Size row was clicked and no dialog opened: {out:?}"
        );
        assert!(
            out.dialog_open,
            "the frame did not report the modal to the shell"
        );
        assert!(
            out.menu.is_empty(),
            "the row was also sent to perform, which has no arm for it: {:?}",
            out.menu
        );
        assert!(out.unrouted.is_empty(), "{:?}", out.unrouted);

        // ...and the next frame really draws it.
        let painted = window.painted_texts(&mut ed);
        assert!(
            painted.iter().any(|t| t.contains("Image Size")),
            "the dialog never appeared: {painted:?}"
        );
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
            &crate::menu_bridge::context(&mut ed, &ui::Workspace::new()),
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

        let mut window = Window::new(&mut ed);
        window.type_into(&mut ed, Chrome::status_zoom_id(), "200");
        // Commit by clicking elsewhere: the field loses focus, the value lands
        // in `ChromeOutput::set_zoom`.
        let out = window.click(&mut ed, Chrome::status_readouts_id());
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

        let mut window = Window::new(&mut ed);
        let out = window.drag(&mut ed, Chrome::tab_id(0), Chrome::tab_id(1));
        assert_eq!(out.move_document, Some((0, 1)), "the drag meant {out:?}");
        ed.move_document(0, 1);
        assert_eq!(names(&ed), ["two.png", "one.png"]);
        // The active tab followed its document rather than staying at the
        // index.
        assert_eq!(ed.active_index(), Some(0));
    }

    // ---- W9-I: a layer into another open document ----------------------

    /// A PNG whose every pixel is `rgba`, so two documents' pixels differ.
    fn w9i_solid_png(dir: &std::path::Path, name: &str, rgba: [u8; 4]) -> std::path::PathBuf {
        let path = dir.join(name);
        let bytes: Vec<u8> = rgba.iter().copied().cycle().take(8 * 8 * 4).collect();
        std::fs::write(
            &path,
            raster::encode(raster::ExportFormat::Png, 8, 8, &bytes).unwrap(),
        )
        .unwrap();
        path
    }

    /// A layer's pixels as (tile, bytes), read through its own document's
    /// tile store — so a copy whose hashes never reached the target's store
    /// reads as missing, not as equal.
    fn w9i_layer_pixels(
        doc: &crate::doc::OpenDocument,
        id: layer_model::LayerId,
    ) -> Vec<(raster::TileCoord, Vec<u8>)> {
        doc.document
            .layer_tiles(id)
            .map(|m| {
                m.iter()
                    .map(|(coord, hash)| {
                        let bytes = compositor::TileSource::tile(&doc.tiles, hash)
                            .expect("every tile the layer names is in its document's store");
                        (coord, bytes.to_vec())
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Two documents: `a.png` (index 0, red) and `b.png` (index 1, green,
    /// active), and the id of `b`'s active layer.
    fn w9i_two_documents(dir: &std::path::Path) -> (Editor, layer_model::LayerId) {
        let mut ed = editor(&dir.join("config"));
        ed.open_path(&w9i_solid_png(dir, "a.png", [200, 10, 10, 255]))
            .unwrap();
        ed.open_path(&w9i_solid_png(dir, "b.png", [10, 200, 10, 255]))
            .unwrap();
        assert_eq!(ed.active_index(), Some(1));
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        (ed, layer)
    }

    /// The copy landed in `a` with `b`'s pixels, `b` is untouched, `a` is
    /// active, and one undo in `a` takes the copy back out.
    fn w9i_assert_copied_then_undo(
        ed: &mut Editor,
        source: layer_model::LayerId,
        name: &str,
        a_layers: usize,
        b_layers: usize,
        b_pixels: &[(raster::TileCoord, Vec<u8>)],
    ) {
        assert_eq!(
            ed.active_index(),
            Some(0),
            "the target document became active"
        );
        let a = &ed.documents()[0];
        assert_eq!(
            a.document.layers.len(),
            a_layers + 1,
            "one layer added to A"
        );
        let copy = a.document.active_layer().expect("the copy is active");
        assert_ne!(copy, source, "the copy has its own id");
        assert_eq!(a.document.layers.get(copy).unwrap().name, name);
        assert_eq!(
            w9i_layer_pixels(a, copy),
            b_pixels,
            "identical pixels at the same tiles, filed in A's own store"
        );
        assert_eq!(a.document.layers.root().first(), Some(&copy), "on top of A");
        let b = &ed.documents()[1];
        assert_eq!(b.document.layers.len(), b_layers, "nothing added to B");
        assert!(!b.history.can_undo(), "no undo step recorded in B");
        assert!(ed.documents()[0].history.can_undo());
        assert!(ed.active_mut().unwrap().undo().unwrap(), "one undo in A");
        let a = &ed.documents()[0];
        assert_eq!(a.document.layers.len(), a_layers, "undo removed the copy");
        assert!(!a.document.layers.contains(copy));
        assert!(!a.history.can_undo(), "the copy was ONE undo step");
    }

    #[test]
    fn dragging_a_layer_row_onto_another_documents_tab_copies_it_there() {
        let dir = tempfile::tempdir().unwrap();
        let (mut ed, source) = w9i_two_documents(dir.path());
        let a_layers = ed.documents()[0].document.layers.len();
        let b_layers = ed.documents()[1].document.layers.len();
        let b_pixels = w9i_layer_pixels(&ed.documents()[1], source);
        assert!(!b_pixels.is_empty());
        assert_ne!(
            b_pixels,
            w9i_layer_pixels(
                &ed.documents()[0],
                ed.documents()[0].document.active_layer().unwrap()
            ),
            "the fixture's documents differ in pixels"
        );
        let name = ed.documents()[1]
            .document
            .layers
            .get(source)
            .unwrap()
            .name
            .clone();
        let mut window = Window::new(&mut ed);
        // The real gesture: press on the Layers-panel row, move onto the
        // other document's tab, release.
        let out = window.drag(&mut ed, ui::view::ids::layer_row(source), Chrome::tab_id(0));
        assert_eq!(out.move_document, None, "a layer drag is not a tab reorder");
        w9i_assert_copied_then_undo(&mut ed, source, &name, a_layers, b_layers, &b_pixels);
    }

    #[test]
    fn a_group_copied_into_another_document_brings_its_children() {
        let dir = tempfile::tempdir().unwrap();
        let (mut ed, base) = w9i_two_documents(dir.path());
        // In B: a group holding B's pixel layer.
        let group = layer_model::Layer::group("Folder");
        let group_id = group.id;
        ed.apply_command(Command::create_layer(group));
        ed.apply_command(Command::MoveLayer {
            layer_id: base,
            parent: Some(group_id),
            index: 0,
        });
        let b_pixels = w9i_layer_pixels(&ed.documents()[1], base);
        let a_layers = ed.documents()[0].document.layers.len();
        let target = ed.documents()[0].id();
        ed.duplicate_layer_into_document(group_id, target, None)
            .unwrap();
        let a = &ed.documents()[0];
        assert_eq!(
            a.document.layers.len(),
            a_layers + 2,
            "the group and its child"
        );
        let copy = a.document.active_layer().unwrap();
        let layer_model::LayerKind::Group(g) = &a.document.layers.get(copy).unwrap().kind else {
            panic!("the copy is a group");
        };
        assert_eq!(g.children.len(), 1);
        assert_ne!(g.children[0], base);
        assert_eq!(w9i_layer_pixels(a, g.children[0]), b_pixels);
        assert!(ed.active_mut().unwrap().undo().unwrap());
        assert_eq!(
            ed.documents()[0].document.layers.len(),
            a_layers,
            "one undo step"
        );
    }

    #[test]
    fn duplicate_layer_with_another_destination_copies_into_that_document() {
        let dir = tempfile::tempdir().unwrap();
        let (mut ed, source) = w9i_two_documents(dir.path());
        let a_layers = ed.documents()[0].document.layers.len();
        let b_layers = ed.documents()[1].document.layers.len();
        let b_pixels = w9i_layer_pixels(&ed.documents()[1], source);
        let a_key = ed.documents()[0].id().0;
        let mut window = Window::new(&mut ed);
        assert!(window
            .chrome
            .dialogs
            .open_for_menu_action(&ui::menu::MenuAction::DuplicateLayer, &ed));
        let dialog = window.chrome.dialogs.active_duplicate_dialog_for_test();
        assert_eq!(
            dialog.destinations().len(),
            2,
            "every open document is listed"
        );
        assert!(dialog.set_destination(a_key));
        dialog.set_name("Twin");
        window.frame(&mut ed);
        let enter = egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        };
        let chrome = &mut window.chrome;
        let mut out = ChromeOutput::default();
        let _ = window.ctx.run(raw_input(vec![enter]), |ctx| {
            out = chrome.ui(ctx, &mut ed);
        });
        assert!(!window.chrome.dialogs.is_open(), "Enter confirmed");
        assert!(
            out.menu.is_empty(),
            "the in-document Duplicate arm is not also run: {:?}",
            out.menu
        );
        w9i_assert_copied_then_undo(&mut ed, source, "Twin", a_layers, b_layers, &b_pixels);
    }

    /// Click the centre of the LAST galley painted with exactly `label`: the
    /// topmost one, since egui paints layers bottom to top — so a document
    /// title in the open Destination combo wins over the same title on its
    /// tab. One frame is drawn first so egui knows where the press lands.
    fn w9i_click_topmost_text(window: &mut Window, ed: &mut Editor, label: &str) {
        let chrome = &mut window.chrome;
        let full = window.ctx.run(raw_input(Vec::new()), |ctx| {
            let _ = chrome.ui(ctx, ed);
        });
        let mut painted = Vec::new();
        fn walk(shape: &egui::Shape, out: &mut Vec<(String, egui::Rect)>) {
            match shape {
                egui::Shape::Text(t) => out.push((
                    t.galley.text().to_string(),
                    egui::Rect::from_min_size(t.pos, t.galley.size()),
                )),
                egui::Shape::Vec(inner) => inner.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        full.shapes
            .iter()
            .for_each(|c| walk(&c.shape, &mut painted));
        let pos = painted
            .iter()
            .rev()
            .find(|(text, _)| text == label)
            .map(|(_, rect)| rect.center())
            .unwrap_or_else(|| panic!("{label:?} was never painted"));
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
        let chrome = &mut window.chrome;
        let _ = window.ctx.run(raw_input(events), |ctx| {
            let _ = chrome.ui(ctx, ed);
        });
    }

    /// The whole Destination route with no test-only setter: open the combo
    /// by clicking it, click the other document's entry in its popup, press
    /// Enter — the copy lands in that document.
    #[test]
    fn picking_another_document_in_the_destination_combo_copies_into_it() {
        let dir = tempfile::tempdir().unwrap();
        let (mut ed, source) = w9i_two_documents(dir.path());
        let a_layers = ed.documents()[0].document.layers.len();
        let b_layers = ed.documents()[1].document.layers.len();
        let b_pixels = w9i_layer_pixels(&ed.documents()[1], source);
        let a_title = ed.documents()[0].title().to_string();
        let b_title = ed.documents()[1].title().to_string();
        let name = format!(
            "{} copy",
            ed.documents()[1].document.layers.get(source).unwrap().name
        );
        let mut window = Window::new(&mut ed);
        assert!(window
            .chrome
            .dialogs
            .open_for_menu_action(&ui::menu::MenuAction::DuplicateLayer, &ed));
        window.settle(&mut ed);
        // The combo opens on this document (B); a press on it opens the list.
        w9i_click_topmost_text(&mut window, &mut ed, &b_title);
        window.frame(&mut ed);
        // The popup's entry for A, drawn above A's tab.
        w9i_click_topmost_text(&mut window, &mut ed, &a_title);
        window.settle(&mut ed);
        assert!(
            window.chrome.dialogs.is_open(),
            "picking a destination does not confirm"
        );
        let enter = egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        };
        let chrome = &mut window.chrome;
        let mut out = ChromeOutput::default();
        let _ = window.ctx.run(raw_input(vec![enter]), |ctx| {
            out = chrome.ui(ctx, &mut ed);
        });
        assert!(!window.chrome.dialogs.is_open(), "Enter confirmed");
        assert!(
            out.menu.is_empty(),
            "the in-document Duplicate arm is not also run: {:?}",
            out.menu
        );
        w9i_assert_copied_then_undo(&mut ed, source, &name, a_layers, b_layers, &b_pixels);
    }

    #[test]
    fn a_long_title_truncates_without_widening_the_strip() {
        let dir = tempfile::tempdir().unwrap();
        let long = format!("{}.png", "a".repeat(30));
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&png(dir.path(), &long)).unwrap();

        let mut window = Window::new(&mut ed);
        window.settle(&mut ed);
        let tab = window
            .read_rect(Chrome::tab_id(0))
            .expect("the tab was drawn");
        let max_width = Chrome::tab_width(design::Theme::Dark.tokens());
        assert!(
            tab.width() <= max_width + 1.0,
            "a {}-character title widened the tab to {}",
            long.len(),
            tab.width()
        );
    }

    // ------------------------------------------------------------------
    // W2-A: Photopea's chrome layout — band order, the tab strip's extent,
    // the empty state's docks, the status readouts, the tab's close control
    // and the start screen's cards and thumbnails. Each of these is red on
    // the layout this replaced.
    // ------------------------------------------------------------------

    #[test]
    fn the_options_bar_sits_above_the_tab_strip_which_runs_only_over_the_canvas() {
        // Photopea: menu, then the options bar across the whole window, then
        // the tab strip over the canvas alone — right of the tool column,
        // left of the docks. The strip used to be a full-width band between
        // the menu and the options bar.
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&png(dir.path(), "a.png")).unwrap();
        let mut window = Window::new(&mut ed);
        window.settle(&mut ed);

        let menu = window.panel_rect("raster-menu-bar").expect("the menu bar");
        let options = window
            .panel_rect("raster-tool-options")
            .expect("the options bar");
        let tabs = window.panel_rect("raster-tabs").expect("the tab strip");
        let tools = window.panel_rect("raster-tools").expect("the tool column");
        let right = window
            .panel_rect("raster-dock-right")
            .expect("the default layout docks panels on the right");
        assert!(
            menu.bottom() <= options.top() + 0.5,
            "the menu bar {menu:?} is not above the options bar {options:?}"
        );
        assert!(
            options.bottom() <= tabs.top() + 0.5,
            "the options bar {options:?} is not above the tab strip {tabs:?}"
        );
        assert!(
            tabs.left() >= tools.right() - 0.5,
            "the tab strip {tabs:?} runs across the tool column {tools:?}"
        );
        assert!(
            tabs.right() <= right.left() + 0.5,
            "the tab strip {tabs:?} runs under the right dock {right:?}"
        );
        assert!(
            options.left() < tools.right() && options.width() > tabs.width(),
            "the options bar {options:?} is not the full-width band above the strip {tabs:?}"
        );
    }

    #[test]
    fn with_no_document_the_docks_are_drawn_and_the_tab_strip_is_not() {
        // Photopea keeps its panels up on the start screen. The docks used to
        // be drawn only for an active document, which left the right half of
        // the window empty; and the strip used to be an empty 20pt band.
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(&dir.path().join("config"));
        assert!(ed.documents().is_empty());
        let mut window = Window::new(&mut ed);
        let texts = window.painted_texts(&mut ed);

        let dock = ui::DockState::default();
        let open: Vec<ui::PanelId> = ui::DockSide::ALL
            .iter()
            .flat_map(|side| dock.panels_on(*side))
            .collect();
        assert!(open.len() >= 5, "the default layout opens {open:?}");
        for panel in &open {
            assert!(
                texts.iter().any(|t| t == panel.title()),
                "the {} panel header is not drawn on the start screen; drawn: {texts:?}",
                panel.title()
            );
        }
        assert!(
            window.panel_rect("raster-dock-right").is_some(),
            "the right dock is not drawn on the start screen"
        );
        assert!(
            window.panel_rect("raster-tabs").is_none(),
            "an empty tab strip is drawn with no document"
        );
        assert!(
            texts
                .iter()
                .any(|t| t == ui::strings::tr("ui.chrome.no.document")),
            "the status strip does not say there is no document: {texts:?}"
        );
    }

    #[test]
    fn the_empty_state_docks_draw_in_both_themes_without_panicking() {
        // The docks are drawn against a document with no layers and no size;
        // a panel that divides by the canvas size shows up here, not on a
        // user's first launch.
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(&dir.path().join("config"));
        for theme in design::Theme::ALL {
            let ctx = egui::Context::default();
            install_theme(&ctx, *theme);
            let mut chrome = Chrome::new();
            let _ = ctx.run(raw_input(Vec::new()), |ctx| {
                let _ = chrome.ui(ctx, &mut ed);
            });
            for panel in ui::PanelId::ALL.iter().copied() {
                chrome
                    .workspace
                    .emit(ui::Intent::SetPanelOpen { panel, open: true });
            }
            for _ in 0..3 {
                let _ = ctx.run(raw_input(Vec::new()), |ctx| {
                    let _ = chrome.ui(ctx, &mut ed);
                });
            }
        }
    }

    #[test]
    fn an_empty_state_panel_control_reaches_no_document() {
        // The empty state's docks are real panels with real buttons. A click
        // on the Layers footer's + must not come out as a document command
        // (there is no document) and must not be reported as unrouted either
        // — it is simply nothing, the way a disabled control is.
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(&dir.path().join("config"));
        assert!(ed.documents().is_empty());
        let mut window = Window::new(&mut ed);
        let out = window.click(&mut ed, ui::view::ids::new_layer());
        assert!(
            out.commands.is_empty() && out.layer_kind.is_empty() && out.select_layer.is_none(),
            "a click with no document open produced {out:?}"
        );
        assert!(
            out.unrouted.is_empty(),
            "the empty state reported a control as unrouted: {:?}",
            out.unrouted
        );
    }

    #[test]
    fn the_status_bar_sizes_only_the_tools_whose_schema_has_a_size() {
        // The strip used to say "Move (V) 24 px". The size readout follows
        // the options schema — the same one the options bar draws its slider
        // from — and the document title is the tab's, not the strip's.
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&png(dir.path(), "a.png")).unwrap();
        let mut window = Window::new(&mut ed);
        let is_size_readout = |t: &String| {
            t.strip_suffix(" px")
                .is_some_and(|n| n.parse::<i32>().is_ok())
        };

        // Only the status bar's own shapes: a panel prints pixel counts of
        // its own (the Brushes panel's "Pencil 1px" rows sit right above the
        // bar in the narrow column, and their galleys run into its band).
        let strip = |window: &mut Window, ed: &mut Editor| -> Vec<String> {
            window
                .status_bar_texts(ed)
                .into_iter()
                .map(|(t, _)| t)
                .collect()
        };

        ed.set_tool(tools::ToolId::Move);
        let texts = strip(&mut window, &mut ed);
        assert!(
            texts.iter().any(|t| t.starts_with("Move")),
            "the Move tool is not the one on screen: {texts:?}"
        );
        assert!(
            !texts.iter().any(is_size_readout),
            "Move has no size, but a size is painted: {texts:?}"
        );

        ed.set_tool(tools::ToolId::Brush);
        let texts = strip(&mut window, &mut ed);
        let size = ed.brush().size as i32;
        assert!(
            texts.iter().any(|t| *t == format!("{size} px")),
            "the Brush's {size} px is not painted: {texts:?}"
        );

        // The title once, on the tab; the status bar does not repeat it.
        let title = ed.active().unwrap().title().to_string();
        let in_status_strip: Vec<(String, egui::Rect)> = window
            .status_bar_texts(&mut ed)
            .into_iter()
            .filter(|(t, _)| *t == title)
            .collect();
        assert!(
            in_status_strip.is_empty(),
            "the status strip repeats the tab's title: {in_status_strip:?}"
        );
        let bar = window.panel_rect("raster-status").unwrap();
        let placed = window.painted_text_rects(&mut ed);
        assert!(
            placed
                .iter()
                .any(|(t, r)| *t == title && r.center().y < bar.top()),
            "the title is not on the tab either: {placed:?}"
        );
    }

    #[test]
    fn tool_has_size_follows_the_registry_schema() {
        assert!(tool_has_size(tools::ToolId::Brush));
        assert!(!tool_has_size(tools::ToolId::Move));
        // Every tool with a size key says so; every tool without does not —
        // the predicate is the schema, not a list.
        for info in tools::registry::all() {
            let in_schema = ui::tool_options::schema_for(info)
                .iter()
                .any(|o| o.key == "size");
            assert_eq!(tool_has_size(info.id), in_schema, "{:?}", info.id);
        }
    }

    #[test]
    fn a_tabs_close_control_sits_inside_its_tab() {
        // The close mark used to be allocated *after* the tab rect, outside
        // it, so the strip read as "title, gap, x, title, gap, x".
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&png(dir.path(), "a.png")).unwrap();
        ed.open_path(&png(dir.path(), "b.png")).unwrap();
        let mut window = Window::new(&mut ed);
        window.settle(&mut ed);
        for index in 0..2 {
            let tab = window
                .read_rect(Chrome::tab_id(index))
                .expect("the tab was drawn");
            let close = window
                .read_rect(Chrome::tab_close_id(index))
                .expect("the close control was drawn");
            assert!(
                tab.contains_rect(close),
                "tab {index}'s close control {close:?} is outside the tab {tab:?}"
            );
        }
    }

    #[test]
    fn a_tab_title_is_not_small_print() {
        // 9pt (`TextStyle::Small`) is a caption, not a document name.
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&png(dir.path(), "a.png")).unwrap();
        let mut window = Window::new(&mut ed);
        window.settle(&mut ed);
        let tokens = design::Theme::Dark.tokens();
        let want = design::egui_theme::font_id(tokens, TypeRole::Body).size;
        let shapes = window.shapes(&mut ed);
        let tab = window.read_rect(Chrome::tab_id(0)).expect("the tab");
        let sizes: Vec<f32> = shapes
            .iter()
            .filter_map(|s| match s {
                egui::Shape::Text(t) if tab.contains(t.pos) && t.galley.text() == "a.png" => {
                    t.galley.job.sections.first().map(|s| s.format.font_id.size)
                }
                _ => None,
            })
            .collect();
        assert!(!sizes.is_empty(), "no title painted inside the tab {tab:?}");
        assert!(
            sizes.iter().all(|s| (*s - want).abs() < 0.01),
            "the tab title is set at {sizes:?}pt, not the Body size {want}pt"
        );
    }

    #[test]
    fn visible_tab_range_keeps_the_active_tab_in_view() {
        assert_eq!(visible_tab_range(0, 4, None), (0, 0));
        assert_eq!(visible_tab_range(3, 4, Some(2)), (0, 3));
        assert_eq!(visible_tab_range(10, 4, Some(1)), (0, 4));
        assert_eq!(visible_tab_range(10, 4, Some(3)), (0, 4));
        assert_eq!(visible_tab_range(10, 4, Some(4)), (1, 5));
        assert_eq!(visible_tab_range(10, 4, Some(9)), (6, 10));
        assert_eq!(visible_tab_range(10, 0, Some(9)), (9, 10));
        assert_eq!(visible_tab_range(10, 4, None), (0, 4));
    }

    #[test]
    fn the_overflow_chevron_lists_the_hidden_tabs_and_a_row_activates_one() {
        // `let _ = overflowed;` used to be the whole overflow story.
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(&dir.path().join("config"));
        for i in 0..14 {
            ed.open_path(&png(dir.path(), &format!("doc-{i:02}.png")))
                .unwrap();
        }
        let mut window = Window::new(&mut ed);
        window.settle(&mut ed);
        let hidden = (0..14)
            .find(|i| window.read_rect(Chrome::tab_id(*i)).is_none())
            .expect("fourteen tabs fit in the strip; widen the list");
        let out = window.click(&mut ed, Chrome::tab_overflow_id());
        assert!(out.activate.is_none(), "{out:?}");
        // The list is drawn from the frame the chevron was clicked in; one
        // more frame registers its rows' rectangles.
        let _ = window.frame(&mut ed);
        let out = window.click(&mut ed, Chrome::tab_overflow_item_id(hidden));
        assert_eq!(out.activate, Some(hidden), "{out:?}");
    }

    #[test]
    fn the_start_screen_draws_cards_and_a_thumbnail_for_a_recent_project() {
        // Photopea's start screen: New / Open / Templates cards, and recents
        // with thumbnails. Ours was two borderless text buttons and a list of
        // names. The `.rstudio` package's own preview is the thumbnail.
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("scene.rstudio");
        project_format::save_project(&project, &editor_core::Document::new(16, 16, "scene"))
            .unwrap();
        assert!(
            project.join(project_format::PREVIEW_FILE).is_file(),
            "the fixture package carries no preview"
        );
        let mut recent = crate::recent::RecentFiles::new();
        recent.record(&project);
        let mut ed = Editor::with_state(
            AppPaths::rooted(dir.path().join("config")),
            Preferences::default(),
            recent,
            Box::new(ScriptedDialogs::new()),
        );
        ed.set_image_clipboard(Box::new(crate::clipboard::FakeClipboard::new()));
        assert!(ed.documents().is_empty());

        let mut window = Window::new(&mut ed);
        let shapes = window.shapes(&mut ed);
        let tokens = design::Theme::Dark.tokens();
        let mut cards = 0;
        for id in [
            "raster-start-new",
            "raster-start-open",
            "raster-start-templates",
        ] {
            let rect = window
                .read_rect(egui::Id::new(id))
                .unwrap_or_else(|| panic!("{id} was never drawn"));
            assert!(
                rect.height() >= tokens.metrics.control_height * 2.0
                    && rect.width() >= tokens.metrics.inspector_label_width,
                "{id} is a text button, not a card: {rect:?}"
            );
            let painted = shapes.iter().any(|s| match s {
                egui::Shape::Rect(r) => {
                    r.fill.a() > 0
                        && r.rect.expand(1.0).contains_rect(rect)
                        && rect.expand(1.0).contains_rect(r.rect)
                }
                _ => false,
            });
            assert!(painted, "no card rectangle is painted for {id} at {rect:?}");
            cards += 1;
        }
        assert!(cards >= 2);

        let texture = window
            .chrome
            .recent_thumbs_for_test()
            .get(&project)
            .cloned()
            .flatten()
            .expect("the recent project's preview was not decoded into a texture");
        assert!(
            painted_texture_ids(&shapes).contains(&texture.id()),
            "the thumbnail texture is not painted on the start screen"
        );
        // And the cell is still the thing a click opens.
        let out = window.click(&mut ed, Chrome::start_recent_id(0));
        assert_eq!(out.open_recent, Some(project.clone()), "{out:?}");
    }

    #[test]
    fn a_recent_thumbnail_is_decoded_once_not_per_frame() {
        let dir = tempfile::tempdir().unwrap();
        let image = png(dir.path(), "photo.png");
        let mut recent = crate::recent::RecentFiles::new();
        recent.record(&image);
        let mut ed = Editor::with_state(
            AppPaths::rooted(dir.path().join("config")),
            Preferences::default(),
            recent,
            Box::new(ScriptedDialogs::new()),
        );
        ed.set_image_clipboard(Box::new(crate::clipboard::FakeClipboard::new()));
        let mut window = Window::new(&mut ed);
        let first = window
            .chrome
            .recent_thumbs_for_test()
            .get(&image)
            .cloned()
            .flatten()
            .expect("the PNG was not decoded into a thumbnail")
            .id();
        for _ in 0..3 {
            let _ = window.frame(&mut ed);
        }
        let again = window
            .chrome
            .recent_thumbs_for_test()
            .get(&image)
            .cloned()
            .flatten()
            .expect("the thumbnail was dropped")
            .id();
        assert_eq!(
            first, again,
            "the thumbnail was re-uploaded on a later frame"
        );
    }

    #[test]
    fn box_downscale_fits_under_the_edge_and_averages() {
        // 8x4 of two colours side by side, down to an edge of 4: 4x2, each
        // output pixel the mean of a 2x2 block of one colour.
        let mut rgba = Vec::new();
        for _y in 0..4 {
            for x in 0..8 {
                let v = if x < 4 { 0u8 } else { 200u8 };
                rgba.extend_from_slice(&[v, v, v, 255]);
            }
        }
        let (w, h, out) = box_downscale(8, 4, &rgba, 4);
        assert_eq!((w, h), (4, 2));
        assert_eq!(&out[0..4], &[0, 0, 0, 255]);
        assert_eq!(&out[3 * 4..4 * 4], &[200, 200, 200, 255]);
        // Already small enough: untouched.
        let (w, h, same) = box_downscale(8, 4, &rgba, 8);
        assert_eq!((w, h), (4 * 2, 4));
        assert_eq!(same, rgba);
    }

    #[test]
    fn a_template_row_opens_the_new_document_dialog_seeded_with_that_preset() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(&dir.path().join("config"));
        assert!(ed.documents().is_empty());
        let mut window = Window::new(&mut ed);
        let out = window.click(&mut ed, egui::Id::new("raster-start-templates"));
        assert!(
            out.actions.is_empty() && !out.dialog_open,
            "the Templates card must unfold the list, not act: {out:?}"
        );
        window.settle(&mut ed);
        let preset = 3;
        let out = window.click(&mut ed, Chrome::start_template_id(preset));
        assert!(out.dialog_open, "no dialog opened: {out:?}");
        match window.chrome.dialogs_for_test().active_for_test() {
            ActiveDialog::NewDocument(dialog) => {
                assert_eq!(dialog.preset(), Some(preset), "the wrong preset is seeded");
                let want = &ui::dialogs::new_document::PRESETS[preset];
                assert_eq!(dialog.pixel_width(), want.width as u32);
            }
            other => panic!("the active dialog is {other:?}, not New Document"),
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

        let mut window = Window::new(&mut ed);
        let side = ui::DockSide::Right;
        let before = window.panels_on(side);
        let groups = window.chrome.workspace().dock.groups_on(side);
        assert!(groups.len() >= 2, "the right rail holds {groups:?}");
        // The reorder control belongs to the ACTIVE tab of the bottom group,
        // and groups travel whole: one click moves that group one slot up the
        // rail's stack, whatever the preset put there. Stated in terms of the
        // groups the dock reports rather than a named layout, so it holds
        // when the preset changes shape again.
        let from = groups.len() - 1;
        let bottom: Vec<ui::PanelId> = groups[from].1.clone();
        let above: Vec<ui::PanelId> = groups[from - 1].1.clone();
        let panel = bottom
            .iter()
            .copied()
            .find(|p| window.chrome.workspace().dock.is_active(*p))
            .expect("the bottom group shows a tab");
        let index_before = before.iter().position(|q| *q == panel).unwrap();

        window.click(&mut ed, ui::view::ids::panel_menu(panel));
        let out = window.click(&mut ed, ui::view::ids::panel_reorder(panel, true));

        // One click on the up chevron moved the group exactly one slot up:
        // the panel's index dropped by the size of the group it climbed over
        // — no more (the double-apply this test exists for), no less.
        let after = window.panels_on(side);
        assert_eq!(
            after.iter().position(|q| *q == panel),
            Some(index_before - above.len()),
            "one click on the up chevron moved {panel:?} from {index_before} to {after:?}"
        );
        // The group above followed it down, untouched inside; nothing else
        // moved.
        let at = before.iter().position(|p| *p == above[0]).unwrap();
        let mut expected: Vec<ui::PanelId> = before[..at].to_vec();
        expected.extend(bottom.iter().copied());
        expected.extend(above.iter().copied());
        assert_eq!(after, expected);
        let after_groups = window.chrome.workspace().dock.groups_on(side);
        assert_eq!(after_groups[from - 1].1, bottom);
        assert_eq!(after_groups[from].1, above);
        assert_eq!(
            out.workspace,
            vec![ui::Intent::ReorderPanel {
                panel,
                to: u8::try_from(from - 1).unwrap()
            }],
            "the click meant {out:?}"
        );
    }

    /// W2-X: the Navigator's thumbnail, the Histogram's bins and the Info
    /// panel's colour rows are fed by this chrome from the live composite —
    /// `Workspace::set_composite_preview` and `set_info_sample` had no caller
    /// after wave 2, so all three drew their empty state for the life of a
    /// session. One frame with a document is enough; a second frame with
    /// nothing changed rebuilds nothing; closing the document clears them.
    #[test]
    fn the_navigator_histogram_and_info_panel_are_fed_from_the_composite() {
        let dir = tempfile::tempdir().unwrap();
        let p = png(dir.path(), "a.png");
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&p).unwrap();

        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut chrome = Chrome::new();
        assert!(chrome.workspace().navigator_texture.is_none());
        assert_eq!(chrome.workspace().histogram.generation(), None);
        assert_eq!(chrome.workspace().info.sampled, None);

        // A layout frame, then the document placed in the canvas area it left,
        // as `Shell::redraw` does; then a frame with the pointer over the
        // middle of the canvas area — where the freshly opened image is
        // centred.
        let _ = ctx.run(raw_input(Vec::new()), |ctx| {
            chrome.ui(ctx, &mut ed);
        });
        chrome.place_canvas(ed.active_mut().unwrap(), glam::Vec2::new(1400.0, 900.0));
        let centre = chrome
            .frame_geometry
            .expect("a frame was drawn")
            .canvas
            .center();
        let full = ctx.run(raw_input(vec![egui::Event::PointerMoved(centre)]), |ctx| {
            chrome.ui(ctx, &mut ed);
        });
        let w = chrome.workspace();
        let tex = w
            .navigator_texture
            .as_ref()
            .expect("no composite preview after a frame with a document");
        // The 8x8 test image needs no downscale: the preview is the image.
        assert_eq!(tex.size(), [8, 8]);
        assert_eq!(
            w.histogram.generation(),
            Some(ed.revision()),
            "the histogram was not counted from this revision"
        );
        assert!(
            w.histogram.bins().is_some(),
            "the histogram has no bins to draw"
        );
        // ...and the Navigator really draws that texture, this frame.
        assert!(
            full.shapes.iter().any(|c| match &c.shape {
                egui::Shape::Mesh(m) => m.texture_id == tex.id(),
                _ => false,
            }),
            "the Navigator never painted the composite preview"
        );
        // The Info panel: the pixel under the pointer is the png's [9; 4].
        let sampled = w
            .info
            .sampled
            .expect("no Info sample with the pointer over the image");
        for c in sampled {
            assert!((c - 9.0 / 255.0).abs() < 1e-6, "sampled {sampled:?}");
        }
        let (px, py) = w.info.pointer.expect("no Info pointer position");
        assert!(
            (0.0..8.0).contains(&px) && (0.0..8.0).contains(&py),
            "{px}, {py}"
        );

        // Nothing changed: the same texture is kept, not re-uploaded.
        let id = tex.id();
        let revision = ed.revision();
        let _ = ctx.run(raw_input(Vec::new()), |ctx| {
            chrome.ui(ctx, &mut ed);
        });
        assert_eq!(ed.revision(), revision, "a frame alone moved the revision");
        assert_eq!(
            chrome.workspace().navigator_texture.as_ref().unwrap().id(),
            id
        );
        assert_eq!(chrome.workspace().info.sampled, Some(sampled));

        // The pointer off the image: the rows go back to their dash.
        let _ = ctx.run(
            raw_input(vec![egui::Event::PointerMoved(egui::pos2(-10.0, -10.0))]),
            |ctx| {
                chrome.ui(ctx, &mut ed);
            },
        );
        assert_eq!(chrome.workspace().info.sampled, None);
        assert_eq!(chrome.workspace().info.pointer, None);

        // The document closes: the preview goes with it.
        ed.dispatch(Action::CloseDocument).unwrap();
        let _ = ctx.run(raw_input(Vec::new()), |ctx| {
            chrome.ui(ctx, &mut ed);
        });
        assert!(chrome.workspace().navigator_texture.is_none());
        assert_eq!(chrome.workspace().histogram.generation(), None);
    }

    /// W2-X: the palette footer's Q shows the *editor's* quick-mask state,
    /// whichever route toggled it — the control itself, the `Q` chord or the
    /// Select menu all end in `Editor::toggle_quick_mask` — and never lights
    /// ahead of the editor. Driven through the real footer control and the
    /// real menu-bridge route.
    #[test]
    fn the_footer_quick_mask_control_lights_from_the_editor_and_not_its_own_click() {
        let dir = tempfile::tempdir().unwrap();
        let p = png(dir.path(), "a.png");
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&p).unwrap();
        let mut window = Window::new(&mut ed);
        let t = design::Theme::Dark.tokens();
        let accent = design::color32(t.palette.color(design::ColorRole::AccentSubtle));
        let engaged = |window: &mut Window, ed: &mut Editor| {
            let rect = window
                .read_rect(ui::palette::quick_mask_control())
                .expect("Q was not drawn");
            window.shapes(ed).iter().any(|s| match s {
                egui::Shape::Rect(r) => r.fill == accent && r.rect.contains_rect(rect.shrink(1.0)),
                _ => false,
            })
        };

        assert!(!ed.quick_mask());
        assert!(!window.chrome.workspace().palette.quick_mask);
        assert!(
            !engaged(&mut window, &mut ed),
            "Q lit before anything engaged it"
        );

        // A click on Q: the footer raises the Select menu's action and the
        // chrome routes it to the menu channel for the shell — it does not
        // flip the mirrored flag itself.
        let out = window.click(&mut ed, ui::palette::quick_mask_control());
        assert!(
            out.menu.contains(&ui::MenuAction::ToggleQuickMask),
            "the click meant {out:?}"
        );
        assert!(
            !window.chrome.workspace().palette.quick_mask,
            "the footer lit before the editor engaged"
        );
        // The shell performs it against the editor; the next frame mirrors it.
        crate::menu_bridge::perform(ui::MenuAction::ToggleQuickMask, &mut ed).unwrap();
        assert!(ed.quick_mask());
        window.frame(&mut ed);
        assert!(
            window.chrome.workspace().palette.quick_mask,
            "the engaged state never reached the footer"
        );
        assert!(
            engaged(&mut window, &mut ed),
            "the engaged Q has no accent fill"
        );

        // Off again through the same route (the Q chord and the Select menu
        // both end here): the light goes.
        crate::menu_bridge::perform(ui::MenuAction::ToggleQuickMask, &mut ed).unwrap();
        assert!(!ed.quick_mask());
        window.frame(&mut ed);
        assert!(!window.chrome.workspace().palette.quick_mask);
        assert!(
            !engaged(&mut window, &mut ed),
            "Q stays lit after the editor left quick mask"
        );
    }

    /// W2-X: Photopea's F. The footer's control asks for the cycle (the
    /// chrome answers with `Action::CycleScreenMode`, which the shell
    /// performs); the editor holds the mode; the chrome drops the tool
    /// column, the options bar and the docks in both full-screen modes and
    /// the menu bar in the last, and mirrors the mode back to the footer.
    #[test]
    fn f_cycles_the_screen_mode_and_full_screen_drops_the_docks_then_the_menu() {
        use ui::palette::ScreenMode;
        let dir = tempfile::tempdir().unwrap();
        let p = png(dir.path(), "a.png");
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&p).unwrap();
        let mut window = Window::new(&mut ed);
        let bands = |window: &mut Window, ed: &mut Editor| {
            let texts = window.painted_texts(ed);
            let menu = texts.iter().any(|t| t == "File");
            let docks = window
                .read_rect(ui::dock::ids::column(ui::DockSide::Right))
                .is_some();
            let tools = window.read_rect(ui::view::ids::tool_slot(0)).is_some();
            let options = window
                .read_rect(ui::view::ids::tool_options_reset(ed.effective_tool()))
                .is_some();
            (menu, docks, tools, options)
        };

        assert_eq!(ed.screen_mode(), ScreenMode::Standard);
        assert_eq!(bands(&mut window, &mut ed), (true, true, true, true));

        // The footer's F: a request the chrome turns into the action, not a
        // mode it changes itself.
        let out = window.click(&mut ed, ui::palette::screen_mode_control());
        assert_eq!(out.actions, vec![Action::CycleScreenMode], "{out:?}");
        assert_eq!(
            ed.screen_mode(),
            ScreenMode::Standard,
            "the chrome changed the mode instead of asking"
        );

        // The shell performs it: full screen with the menu bar.
        ed.dispatch(Action::CycleScreenMode).unwrap();
        assert_eq!(ed.screen_mode(), ScreenMode::FullScreenWithMenu);
        window.settle(&mut ed);
        assert_eq!(
            window.chrome.workspace().palette.screen_mode,
            ScreenMode::FullScreenWithMenu,
            "the footer's mirror did not follow the editor"
        );
        assert_eq!(
            bands(&mut window, &mut ed),
            (true, false, false, false),
            "(menu, docks, tools, options) in Full Screen With Menu Bar"
        );

        // Again: full screen, no menu bar either.
        ed.dispatch(Action::CycleScreenMode).unwrap();
        assert_eq!(ed.screen_mode(), ScreenMode::FullScreen);
        window.settle(&mut ed);
        assert_eq!(
            bands(&mut window, &mut ed),
            (false, false, false, false),
            "(menu, docks, tools, options) in Full Screen"
        );

        // And round to Standard: everything comes back.
        ed.dispatch(Action::CycleScreenMode).unwrap();
        assert_eq!(ed.screen_mode(), ScreenMode::Standard);
        window.settle(&mut ed);
        assert_eq!(bands(&mut window, &mut ed), (true, true, true, true));
        assert_eq!(
            window.chrome.workspace().palette.screen_mode,
            ScreenMode::Standard
        );
    }

    /// W2-X: the menu bar and the options bar are one header band across the
    /// top of the window — the header shade, not the panel shade the columns
    /// use. Read from the panel frames the chrome really paints.
    #[test]
    fn the_menu_and_options_bands_are_painted_in_the_header_shade() {
        let dir = tempfile::tempdir().unwrap();
        let p = png(dir.path(), "a.png");
        let mut ed = editor(&dir.path().join("config"));
        ed.open_path(&p).unwrap();
        let mut window = Window::new(&mut ed);
        let shapes = window.shapes(&mut ed);
        let t = design::Theme::Dark.tokens();
        let header = design::color32(t.palette.surface(design::SurfaceRole::Header));
        let panel = design::color32(t.palette.surface(design::SurfaceRole::Panel));
        assert_ne!(
            header, panel,
            "the theme cannot tell the header from a panel"
        );
        for name in ["raster-menu-bar", "raster-tool-options"] {
            let band = window
                .panel_rect(name)
                .unwrap_or_else(|| panic!("{name} was not drawn"));
            let fills: Vec<egui::Color32> = shapes
                .iter()
                .filter_map(|s| match s {
                    egui::Shape::Rect(r)
                        if (r.rect.min - band.min).length() < 1.0
                            && (r.rect.max - band.max).length() < 1.0 =>
                    {
                        Some(r.fill)
                    }
                    _ => None,
                })
                .collect();
            assert!(
                fills.contains(&header),
                "{name} {band:?} is not filled with the header shade: {fills:?}"
            );
            assert!(
                !fills.contains(&panel),
                "{name} is still filled with the panel shade: {fills:?}"
            );
        }
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

        let mut window = Window::new(&mut ed);
        let panel = ui::PanelId::History;
        assert!(!window.panels_on(ui::DockSide::Bottom).contains(&panel));
        let from = window.chrome.workspace().dock.placement(panel).side;
        assert_ne!(from, ui::DockSide::Bottom);

        window.click(&mut ed, ui::view::ids::panel_menu(panel));
        let out = window.click(
            &mut ed,
            ui::view::ids::panel_dock(panel, ui::DockSide::Bottom),
        );

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
        let painted = painted_text_with(&window.ctx, &mut window.chrome, &mut ed);
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

        let mut window = Window::new(&mut ed);
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
        window.settle(&mut ed);

        // Row 0 is the composite; row 1 is the first component.
        let out = window.click(&mut ed, ui::view::ids::channel_eye(1));
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
            chrome.ui(ctx, &mut ed);
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
            out = chrome.ui(ctx, &mut ed);
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
            out = chrome.ui(ctx, &mut ed);
        });
        assert_eq!(chrome.channel_mask(), crate::presenter::ChannelMask::ALL);

        // Ctrl+1 belongs to the application's keymap (100%), so the panel must
        // not steal it — there is no row wearing digit 1 either.
        let _ = ctx.run(raw_input(press(egui::Key::Num1)), |ctx| {
            out = chrome.ui(ctx, &mut ed);
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
                out = chrome.ui(ctx, &mut ed);
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
            out = chrome.ui(ctx, &mut ed);
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

        let out = run_chrome(&mut ed, Some(ui::view::ids::tool_slot(slot)));
        assert_eq!(out.select_tool, Some(ToolId::Brush), "{out:?}");
    }

    /// One frame of the chrome, returning what it emitted.
    fn one_frame(chrome: &mut Chrome, editor: &mut Editor) -> ChromeOutput {
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
        one_frame(&mut chrome, &mut editor);
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
        one_frame(&mut chrome, &mut editor);

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
    fn an_intent_nothing_can_perform_is_still_reported_to_the_user() {
        // `harvest` used to be `if let Some(pick) = pick(..)` with no `else`,
        // so an intent nothing could perform produced no edit, no status line
        // and no log record. That silence is why an entirely inert Properties
        // panel survived a whole wave of review: on screen, a control that does
        // nothing looks exactly like a control that works.
        //
        // Every menu action has a route now (the dialog host or `perform`), so
        // there is no intent `pick` answers with `None` any more; the property
        // this test guards is the same one, one step further down: an action
        // that cannot run *here* (no document is open) must still reach the
        // user as a specific refusal, never vanish. Brightness/Contrast with
        // no document is the case: the dialog host declines (nothing to
        // preview), the bridge hands the action to `perform`, and `perform`
        // refuses by name.
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        let mut chrome = Chrome::new();

        let action =
            ui::menu::MenuAction::ApplyAdjustment(ui::menu::AdjustmentId::BrightnessContrast);
        chrome.workspace.emit(ui::Intent::Action(action));
        let mut out = ChromeOutput::default();
        chrome.harvest_workspace_for_test(&mut out, &ed);

        assert!(
            out.unrouted.is_empty(),
            "the action has a route; it must not be reported as unrouted: {:?}",
            out.unrouted
        );
        assert_eq!(
            out.menu,
            vec![action],
            "with no document the dialog host declines and the bridge hands the action on"
        );
        let refusal = crate::menu_bridge::perform(action, &mut ed)
            .expect_err("nothing can apply an adjustment with no document open");
        // The refusal names the missing piece rather than a generic fallback.
        // What matters here is that the user is *told* — the reporting path
        // this test guards — so assert the message is the real, specific one.
        assert!(
            refusal.contains("Brightness/Contrast") && refusal.contains("Adjustments"),
            "the refusal named nothing actionable: {refusal}"
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
        let mut ed = ed;
        let mut frame = |events: Vec<egui::Event>| {
            chrome.workspace.emit(intent.clone());
            let mut out = ChromeOutput::default();
            let _ = ctx.run(raw_input(events), |ctx| {
                out = chrome.ui(ctx, &mut ed);
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
        let mut ed = editor(dir.path());
        let mut chrome = Chrome::new();
        one_frame(&mut chrome, &mut ed);

        let viewport = chrome.workspace().canvas.view.viewport();
        // `raw_input` gives the frame a 1400x900 window.
        assert!(
            (viewport.surface_pt().x - 1400.0).abs() < 1.0
                && (viewport.surface_pt().y - 900.0).abs() < 1.0,
            "the canvas host still thinks the window is {:?}",
            viewport.surface_pt()
        );
        // ...and its viewport is the canvas area the docks left, because that
        // is the rectangle `render::Camera` centres the image on and draws
        // into (`Chrome::place_canvas`). The host and the camera dividing by
        // different rectangles is what once made Fill Screen come out smaller
        // than Fit on Screen.
        let geometry = chrome.frame_geometry.expect("a frame was drawn");
        assert_eq!(
            viewport.size_pt(),
            glam::Vec2::new(geometry.canvas.width(), geometry.canvas.height()),
            "the canvas host is framing against a rectangle the shell does not \
             render into: insets {:?}",
            viewport.insets()
        );
        assert_eq!(
            viewport.center_pt(),
            glam::Vec2::new(geometry.canvas.center().x, geometry.canvas.center().y),
        );
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
    /// it: the canvas area the chrome's last frame left, in physical pixels.
    fn render_camera(editor: &Editor, chrome: &Chrome) -> render::Camera {
        let open = editor.active().expect("a document is open");
        let mut camera = open.camera.clone();
        let area = chrome.canvas_area_px().expect("a frame was drawn");
        camera.viewport_origin = area.origin;
        camera.viewport_size = area.size;
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

        let (fill, mut chrome) = view_item(&mut ed, M::Zoom(Z::FillScreen));
        let fill_zoom = fill.set_zoom.expect("Fill Screen reports a zoom");
        // The Fit this application actually performs, on this same editor,
        // with the document placed in the canvas area as `Shell::redraw`
        // places it.
        chrome.place_canvas(ed.active_mut().unwrap(), glam::Vec2::new(1400.0, 900.0));
        ed.dispatch(crate::Action::ZoomFit).unwrap();
        let fit_zoom = ed.active().unwrap().camera.zoom;
        assert!(
            fill_zoom >= fit_zoom,
            "Fill Screen ({fill_zoom}) is smaller than Fit on Screen ({fit_zoom})"
        );

        // ...and it fills: at that zoom the image covers every point of the
        // canvas area the user can see, with nothing of the backdrop left.
        let geometry = chrome.frame_geometry.expect("a frame was drawn");
        let mut camera = render_camera(&ed, &chrome);
        camera.zoom = fill_zoom;
        let (cx, cy) = fill.set_view_center.expect("Fill Screen reports a centre");
        camera.center = glam::Vec2::new(cx, cy);
        let ppp = geometry.ppp;
        let top_left = camera.screen_to_image(glam::Vec2::new(
            (geometry.canvas.min.x - geometry.surface.min.x) * ppp,
            (geometry.canvas.min.y - geometry.surface.min.y) * ppp,
        ));
        let bottom_right = camera.screen_to_image(glam::Vec2::new(
            (geometry.canvas.max.x - geometry.surface.min.x) * ppp,
            (geometry.canvas.max.y - geometry.surface.min.y) * ppp,
        ));
        // Within the host's framing margin: `ui::CanvasCamera` frames Fill with
        // the same `FIT_MARGIN` as Fit, which leaves a hairline (1% of a side)
        // of backdrop; the defect this pins was a whole strip of it.
        let tol = glam::Vec2::new(400.0, 300.0) * ui::canvas::CanvasCamera::FIT_MARGIN;
        assert!(
            top_left.x >= -tol.x
                && top_left.y >= -tol.y
                && bottom_right.x <= 400.0 + tol.x
                && bottom_right.y <= 300.0 + tol.y,
            "Fill Screen left backdrop showing: the canvas area spans document \
             {top_left:?}..{bottom_right:?}, outside the 400x300 image"
        );
    }

    #[test]
    fn zoom_to_selection_frames_the_selection_where_the_docks_are_not() {
        // The camera the shell renders from is centred in the canvas area the
        // docks leave. Framing the selection against the window instead hid
        // its leading edges behind the tool rail and the options bar —
        // measured at ~27 points on the left and ~29 on the top for this very
        // selection.
        use ui::menu::MenuAction as M;
        use ui::menu::ZoomCommand as Z;
        let dir = tempfile::tempdir().unwrap();
        let mut ed = wide_document(dir.path());

        let (out, chrome) = view_item(&mut ed, M::Zoom(Z::ToSelection));
        let geometry = chrome.frame_geometry.expect("a frame was drawn");
        let mut camera = render_camera(&ed, &chrome);
        camera.zoom = out.set_zoom.expect("Zoom to Selection reports a zoom");
        let (cx, cy) = out
            .set_view_center
            .expect("Zoom to Selection reports a centre");
        camera.center = glam::Vec2::new(cx, cy);

        // What the *visible* canvas rectangle shows, in document pixels.
        let ppp = geometry.ppp;
        let top_left = camera.screen_to_image(glam::Vec2::new(
            (geometry.canvas.min.x - geometry.surface.min.x) * ppp,
            (geometry.canvas.min.y - geometry.surface.min.y) * ppp,
        ));
        let bottom_right = camera.screen_to_image(glam::Vec2::new(
            (geometry.canvas.max.x - geometry.surface.min.x) * ppp,
            (geometry.canvas.max.y - geometry.surface.min.y) * ppp,
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
        one_frame(&mut both, &mut ed);
        let mut batch = ChromeOutput::default();
        batch
            .workspace
            .push(ui::Intent::Action(M::Zoom(Z::ToSelection)));
        batch
            .workspace
            .push(ui::Intent::Action(M::Zoom(Z::FillScreen)));
        both.harvest_workspace_for_test(&mut batch, &ed);
        let after = batch.set_zoom.expect("Fill Screen reports a zoom");
        let alone = view_item(&mut ed, M::Zoom(Z::FillScreen))
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

        let (out, _) = view_item(&mut ed, M::Zoom(Z::ToSelection));
        let (cx, cy) = out.set_view_center.expect("the read-back still reports");
        assert!(
            (cx - before.x).abs() < 1e-3 && (cy - before.y).abs() < 1e-3,
            "Zoom to Selection panned to ({cx}, {cy}) with nothing selected; \
             the camera was at {before:?}"
        );
    }

    /// A `w`x`h` PNG opened in a fresh editor, and a 1440x900 window's worth
    /// of frames driven exactly as `Shell::redraw` drives them: place the
    /// active document in the chrome's last canvas area (the whole surface
    /// before the first layout), then lay the chrome out. The default docks
    /// are open.
    fn opened_in_a_1440_window(dir: &std::path::Path, w: u32, h: u32) -> (Editor, Chrome) {
        let path = dir.join("scene.png");
        std::fs::write(
            &path,
            raster::encode(
                raster::ExportFormat::Png,
                w,
                h,
                &vec![200u8; (w * h * 4) as usize],
            )
            .unwrap(),
        )
        .unwrap();
        let mut ed = editor(&dir.join("config"));
        ed.open_path(&path).unwrap();
        let mut chrome = Chrome::new();
        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let surface = glam::Vec2::new(1440.0, 900.0);
        for _ in 0..3 {
            chrome.place_canvas(ed.active_mut().unwrap(), surface);
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(1440.0, 900.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    chrome.ui(ctx, &mut ed);
                },
            );
        }
        chrome.place_canvas(ed.active_mut().unwrap(), surface);
        (ed, chrome)
    }

    /// W5-A: a 320x180 image opened in a 1440x900 window is centred in the
    /// canvas area the docks leave — within a pixel of its centre — and not
    /// on the window's centre, where its right part went under the docks.
    /// Measured through the camera the shell renders with, the viewport every
    /// overlay maps through, and a real marquee gesture through the pointer
    /// route.
    #[test]
    fn an_opened_image_is_centred_in_the_canvas_area_not_the_window() {
        let dir = tempfile::tempdir().unwrap();
        let (mut ed, chrome) = opened_in_a_1440_window(dir.path(), 320, 180);
        let area = chrome.canvas_area_px().expect("a frame was laid out");
        // The default docks really take the right of the window: the canvas
        // area's centre is well left of the window's.
        assert!(
            area.center().x < 720.0 - 100.0,
            "the default docks left the canvas area {area:?}"
        );
        // View > Rulers is on by default, and the rulers are painted over the
        // top and left of what the docks left: the canvas area starts past
        // them, so no image pixel is fitted or centred under a ruler.
        let frame = chrome.frame_geometry.expect("a frame was laid out");
        assert!(chrome.workspace().view_flags.get(ui::ViewFlag::Rulers));
        let t = frame.style.ruler_thickness_pt;
        assert!(t > 0.0);
        assert_eq!(
            area.origin,
            glam::Vec2::new(frame.content.min.x + t, frame.content.min.y + t) * frame.ppp,
            "the canvas area does not start past the ruler gutters"
        );
        let doc = ed.active().unwrap();
        assert_eq!(doc.camera.zoom, 1.0, "a small image opens at 100%");
        assert_eq!(doc.camera.viewport_origin, area.origin);
        assert_eq!(doc.camera.viewport_size, area.size);

        // Where the renderer draws the image's corners and centre.
        let camera = crate::tool_input::canvas_camera_of(&doc.camera);
        let viewport = crate::tool_input::canvas_viewport(&doc.camera);
        let to_screen =
            |p: glam::Vec2| crate::interaction_geometry::document_to_screen(&camera, &viewport, p);
        let (tl, br) = (
            to_screen(glam::Vec2::ZERO),
            to_screen(glam::Vec2::new(320.0, 180.0)),
        );
        let centre = (tl + br) * 0.5;
        assert!(
            (centre - area.center()).length() <= 1.0,
            "the image is centred on {centre:?}, not the canvas area's {:?}",
            area.center()
        );
        assert!(
            (centre - glam::Vec2::new(720.0, 450.0)).length() > 50.0,
            "the image is still centred on the window"
        );
        // Nothing of it under a dock: it lies inside the canvas area.
        let far = area.origin + area.size;
        assert!(
            tl.x >= area.origin.x && tl.y >= area.origin.y && br.x <= far.x && br.y <= far.y,
            "the image {tl:?}..{br:?} leaves the canvas area {area:?}"
        );
        // The renderer's own mapping agrees: the area's centre is the image's.
        let under = doc.camera.screen_to_image(area.center());
        assert!(
            (under - glam::Vec2::new(160.0, 90.0)).length() <= 1.0,
            "{under:?}"
        );

        // A pointer press at the canvas area's centre lands on the image's
        // centre: a marquee dragged from there starts at (160, 90).
        ed.set_tool(tools::ToolId::RectMarquee);
        let mut pointer = crate::tool_input::ToolPointer::new();
        let start = area.center();
        for (phase, at) in [
            (ui::canvas::PointerPhase::Down, start),
            (
                ui::canvas::PointerPhase::Move,
                start + glam::Vec2::new(10.0, 10.0),
            ),
            (
                ui::canvas::PointerPhase::Up,
                start + glam::Vec2::new(20.0, 20.0),
            ),
        ] {
            pointer.handle(&mut ed, ui::canvas::PointerInput::at(phase, at), false, &[]);
        }
        let (min, max) = ed
            .active()
            .unwrap()
            .document
            .selection
            .bounds()
            .expect("the marquee selected");
        assert_eq!(
            (min.x, min.y, max.x, max.y),
            (160, 90, 180, 110),
            "a press at the canvas area's centre did not land on the image's centre"
        );
    }

    /// W5-A: View > Fit on Screen fits the image inside the canvas area — a
    /// 3628x2041 scene, which fitted against the window ran under the docks.
    /// And Tab (panels hidden, so the area grows) keeps the image centre in the
    /// middle of the new area without re-fitting.
    #[test]
    fn fit_on_screen_fits_the_canvas_area_and_a_panel_toggle_keeps_the_centre() {
        let dir = tempfile::tempdir().unwrap();
        let (mut ed, mut chrome) = opened_in_a_1440_window(dir.path(), 3628, 2041);
        let area = chrome.canvas_area_px().expect("a frame was laid out");
        ed.dispatch(crate::Action::ZoomFit).unwrap();
        let doc = ed.active().unwrap();
        let want = (area.size.x / 3628.0).min(area.size.y / 2041.0);
        assert!(
            (doc.camera.zoom - want).abs() < 1e-5,
            "Fit on Screen zoomed to {}, the canvas area fits {want}",
            doc.camera.zoom
        );
        let camera = crate::tool_input::canvas_camera_of(&doc.camera);
        let viewport = crate::tool_input::canvas_viewport(&doc.camera);
        let tl =
            crate::interaction_geometry::document_to_screen(&camera, &viewport, glam::Vec2::ZERO);
        let br = crate::interaction_geometry::document_to_screen(
            &camera,
            &viewport,
            glam::Vec2::new(3628.0, 2041.0),
        );
        let far = area.origin + area.size;
        assert!(
            tl.x >= area.origin.x - 0.5
                && tl.y >= area.origin.y - 0.5
                && br.x <= far.x + 0.5
                && br.y <= far.y + 0.5,
            "the fitted image {tl:?}..{br:?} leaves the canvas area {area:?}"
        );

        // The area moves (as Tab hiding the docks moves it): no re-fit, and the
        // document point that was in the middle of the area is in the middle
        // of the new one.
        let (zoom, center) = (doc.camera.zoom, doc.camera.center);
        let wider = crate::interaction_geometry::CanvasArea {
            origin: glam::Vec2::ZERO,
            size: glam::Vec2::new(1440.0, 900.0),
        };
        chrome
            .canvas_placement
            .place(ed.active_mut().unwrap(), wider, true);
        let doc = ed.active().unwrap();
        assert_eq!(doc.camera.zoom, zoom, "a panel toggle re-fitted");
        let middle = doc.camera.screen_to_image(wider.center());
        assert!(
            (middle - center).length() < 1e-3,
            "{middle:?} vs {center:?}"
        );
    }

    /// W5-A: View > Zoom In / Zoom Out (and Ctrl+= / Ctrl+-, the same
    /// actions) zoom about the middle of the canvas area, where the image is
    /// centred. The canvas area starts past the tool column, the bars and the
    /// rulers, so anchoring at half its size (a point up and left of its
    /// middle) walked the image off centre on every step.
    #[test]
    fn view_zoom_in_and_out_keep_the_image_centred_in_the_canvas_area() {
        use ui::menu::MenuAction as M;
        use ui::menu::ZoomCommand as Z;
        let dir = tempfile::tempdir().unwrap();
        let (mut ed, chrome) = opened_in_a_1440_window(dir.path(), 320, 180);
        let area = chrome.canvas_area_px().expect("a frame was laid out");
        assert!(
            area.origin.x > 1.0 && area.origin.y > 1.0,
            "the canvas area {area:?} does not start past the chrome"
        );
        let image_centre = |ed: &Editor| {
            let doc = ed.active().unwrap();
            let camera = crate::tool_input::canvas_camera_of(&doc.camera);
            let viewport = crate::tool_input::canvas_viewport(&doc.camera);
            let tl = crate::interaction_geometry::document_to_screen(
                &camera,
                &viewport,
                glam::Vec2::ZERO,
            );
            let br = crate::interaction_geometry::document_to_screen(
                &camera,
                &viewport,
                glam::Vec2::new(320.0, 180.0),
            );
            (tl + br) * 0.5
        };
        assert!((image_centre(&ed) - area.center()).length() <= 1.0);
        for (step, zoom) in [Z::In, Z::In, Z::In, Z::Out, Z::Out, Z::Out, Z::Out]
            .into_iter()
            .enumerate()
        {
            // The menu route: the pick the menu bar makes, then the dispatch.
            let intent = ui::Intent::Action(M::Zoom(zoom));
            let Some(crate::menu_bridge::Pick::Action(action)) =
                crate::menu_bridge::pick(&intent, &ed)
            else {
                panic!("View > Zoom {zoom:?} is not an application action");
            };
            let before = ed.active().unwrap().camera.zoom;
            ed.dispatch(action).unwrap();
            assert_ne!(ed.active().unwrap().camera.zoom, before, "step {step}");
            let centre = image_centre(&ed);
            assert!(
                (centre - area.center()).length() <= 1.0,
                "after step {step} ({zoom:?}) the image is centred on {centre:?}, not the canvas area's {:?}",
                area.center()
            );
        }
    }

    /// Run one frame, then absorb `action` as the menu bar would have.
    fn view_item(editor: &mut Editor, action: ui::menu::MenuAction) -> (ChromeOutput, Chrome) {
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
        let mut ed = wide_document(dir.path());
        let started = ed.active().unwrap().camera.zoom;

        // *That* Fill is larger than Fit is
        // `fill_screen_is_never_smaller_than_the_fit_the_application_performs`,
        // which compares against the Fit the application performs rather than
        // against another guess made on the same host. Here the claim is only
        // that the number reaches the document's camera at all.
        let (fill, _) = view_item(&mut ed, M::Zoom(Z::FillScreen));
        assert!(
            fill.set_zoom.is_some_and(|z| (z - started).abs() > 1e-3),
            "Fill Screen reported {:?}, which is the zoom the document already \
             had ({started})",
            fill.set_zoom
        );

        let (print, _) = view_item(&mut ed, M::Zoom(Z::PrintSize));
        let want = ui::canvas::workspace::POINTS_PER_INCH / ui::canvas::workspace::DEFAULT_PPI;
        assert!(
            print.set_zoom.is_some_and(|z| (z - want).abs() < 1e-3),
            "Print Size reported {:?}, wanted {want}",
            print.set_zoom
        );

        // *Where* Zoom to Selection puts the selection is
        // `zoom_to_selection_frames_the_selection_where_the_docks_are_not`; the
        // claim here is that it reports a centre near the selection at all.
        let (selection, _) = view_item(&mut ed, M::Zoom(Z::ToSelection));
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
        one_frame(&mut chrome, &mut ed);
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
                out = chrome.ui(ctx, &mut ed);
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
                out = chrome.ui(ctx, &mut ed);
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
                out = chrome.ui(ctx, &mut ed);
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
            out = chrome.ui(ctx, &mut ed);
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
                out = chrome.ui(ctx, &mut ed);
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
                out = chrome.ui(ctx, &mut ed);
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
                out = chrome.ui(ctx, &mut ed);
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
                out = chrome.ui(ctx, &mut ed);
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
                out = chrome.ui(ctx, &mut ed);
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
                out = chrome.ui(ctx, &mut ed);
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
                out = chrome.ui(ctx, &mut ed);
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
                out = chrome.ui(ctx, &mut ed);
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
                out = chrome.ui(ctx, &mut ed);
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
        let mut ed = editor(&dir.path().join("config"));
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
                out = chrome.ui(ctx, &mut ed);
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
                out = chrome.ui(ctx, &mut ed);
            },
        );
        assert!(!out.dialog_open);
        assert!(ed.documents().is_empty(), "cancel created a document");
        assert!(
            out.commands.is_empty() && out.actions.is_empty() && out.dialog.is_none(),
            "cancelling produced {out:?}"
        );
    }

    // -----------------------------------------------------------------------
    // W1-A: Layers-panel thumbnails are cached, throttled and updated in place
    // -----------------------------------------------------------------------

    /// One opaque grey tile at the canvas origin of `id`, through the real
    /// command route — the edit that must (and the only edit that must)
    /// recomposite that layer's thumbnail.
    fn paint_grey(open: &mut crate::doc::OpenDocument, id: LayerId, v: u8) {
        let ts = raster::TILE_SIZE;
        let mut bytes = Vec::with_capacity((ts * ts * 4) as usize);
        for _ in 0..ts * ts {
            bytes.extend_from_slice(&[v, v, v, 255]);
        }
        let hash = open.tiles.insert_bytes(bytes);
        open.apply(
            Command::paint_tiles(
                editor_core::PixelTarget::Layer(id),
                vec![editor_core::TileEdit::set(
                    raster::TileCoord::new(0, 0, 0),
                    hash,
                )],
            )
            .unwrap(),
        )
        .unwrap();
    }

    /// [`paint_grey`] through [`Editor::apply_command`], the choke point
    /// every user edit goes through, so the editor's revision moves.
    fn paint_grey_through_editor(ed: &mut Editor, id: LayerId, v: u8) {
        let ts = raster::TILE_SIZE;
        let mut bytes = Vec::with_capacity((ts * ts * 4) as usize);
        for _ in 0..ts * ts {
            bytes.extend_from_slice(&[v, v, v, 255]);
        }
        let hash = ed.active_mut().unwrap().tiles.insert_bytes(bytes);
        ed.apply_command(
            Command::paint_tiles(
                editor_core::PixelTarget::Layer(id),
                vec![editor_core::TileEdit::set(
                    raster::TileCoord::new(0, 0, 0),
                    hash,
                )],
            )
            .unwrap(),
        );
    }

    /// W3-J: the Properties Transform block measures a raster layer by the
    /// ink the chrome publishes, not by its stored tiles. A 300x200 image is
    /// stored as 512x256 of tiles; the published frame is the image. W3-X:
    /// the block is opened through its drawn disclosure first, because the
    /// chrome measures only for a block on screen.
    #[test]
    fn the_chrome_publishes_the_active_raster_layer_s_alpha_ink_to_properties() {
        let dir = tempfile::tempdir().unwrap();
        let (mut ed, ids) = editor_with_layers(dir.path(), 1);
        let mut window = Window::new(&mut ed);
        let _ = window.click(&mut ed, ui::panels::properties::ids::transform_toggle());
        window.settle(&mut ed);
        let inks = ui::panels::properties::RasterInks::published(&window.ctx);
        let doc = &ed.active().unwrap().document;
        let frame = ui::panels::properties::Transform::frame(doc, ids[0], &inks)
            .expect("the chrome measured the active raster layer");
        assert_eq!(
            (frame.x, frame.y, frame.width, frame.height),
            (0.0, 0.0, 300.0, 200.0)
        );
    }

    /// W3-X: the alpha-ink measurement only the Transform block reads is made
    /// while that block is on screen and the layer changed — never per frame,
    /// and never with the block closed (its default).
    #[test]
    fn the_raster_ink_is_measured_only_for_an_open_transform_block_after_an_edit() {
        let dir = tempfile::tempdir().unwrap();
        let (mut ed, ids) = editor_with_layers(dir.path(), 1);
        let mut window = Window::new(&mut ed);
        window.settle(&mut ed);
        assert_eq!(
            window.chrome.ink_publishes, 0,
            "measured with the Transform block closed"
        );
        paint_grey_through_editor(&mut ed, ids[0], 40);
        window.settle(&mut ed);
        assert_eq!(
            window.chrome.ink_publishes, 0,
            "an edit under a closed block was measured"
        );

        // Open the block through the disclosure the Properties panel draws.
        let _ = window.click(&mut ed, ui::panels::properties::ids::transform_toggle());
        window.settle(&mut ed);
        assert_eq!(window.chrome.ink_publishes, 1, "opening measures once");
        {
            let inks = ui::panels::properties::RasterInks::published(&window.ctx);
            let doc = &ed.active().unwrap().document;
            assert!(
                ui::panels::properties::Transform::frame(doc, ids[0], &inks).is_some(),
                "the open block has a current measurement"
            );
        }
        window.settle(&mut ed);
        assert_eq!(
            window.chrome.ink_publishes, 1,
            "idle frames re-measured an unchanged layer"
        );

        // One edit with the block open: exactly one more measurement.
        paint_grey_through_editor(&mut ed, ids[0], 90);
        window.settle(&mut ed);
        assert_eq!(window.chrome.ink_publishes, 2, "one edit, one measurement");

        // Closed again: an edit measures nothing.
        let _ = window.click(&mut ed, ui::panels::properties::ids::transform_toggle());
        window.settle(&mut ed);
        paint_grey_through_editor(&mut ed, ids[0], 130);
        window.settle(&mut ed);
        assert_eq!(window.chrome.ink_publishes, 2, "measured a closed block");
    }

    /// W3-X: a Channels saved-selection row loads the row that was clicked.
    /// A plain click opens Load Selection with that row chosen (not the
    /// newest entry); a Ctrl+click loads it as the selection without asking.
    #[test]
    fn a_channels_saved_selection_row_loads_the_row_that_was_clicked() {
        let dir = tempfile::tempdir().unwrap();
        let (mut ed, _ids) = editor_with_layers(dir.path(), 1);
        let first = editor_core::Selection::Rect {
            min: glam::IVec2::new(0, 0),
            max: glam::IVec2::new(10, 10),
        };
        let second = editor_core::Selection::Rect {
            min: glam::IVec2::new(20, 20),
            max: glam::IVec2::new(40, 40),
        };
        let third = editor_core::Selection::Rect {
            min: glam::IVec2::new(50, 50),
            max: glam::IVec2::new(60, 60),
        };
        {
            let doc = &mut ed.active_mut().unwrap().document;
            doc.saved_selections
                .push(("Alpha 1".to_string(), first.clone()));
            doc.saved_selections.push(("Keep".to_string(), second));
            doc.saved_selections.push(("Newest".to_string(), third));
        }
        let mut window = Window::new(&mut ed);
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
        window.settle(&mut ed);

        // Click the second row: the dialog opens on "Keep", not "Newest".
        let _ = window.click_text(&mut ed, "Keep");
        let dialog = window
            .chrome
            .dialogs
            .active_load_selection_dialog_for_test();
        assert_eq!(dialog.selected(), 1, "the dialog opened on another row");
        assert_eq!(
            dialog.confirm().map(|spec| spec.name),
            Some("Keep".to_string())
        );
        window.chrome.dialogs.close();
        window.settle(&mut ed);

        // Ctrl+click the first row: no dialog, and performing the routed
        // action loads that row.
        let rect = window
            .painted_text_rects(&mut ed)
            .into_iter()
            .find(|(text, _)| text == "Alpha 1")
            .map(|(_, rect)| rect)
            .expect("the first saved selection row was painted");
        let pos = rect.center();
        let press = |pressed: bool| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::COMMAND,
        };
        let mut input = raw_input(vec![
            egui::Event::PointerMoved(pos),
            press(true),
            press(false),
        ]);
        input.modifiers = egui::Modifiers::COMMAND;
        let mut out = ChromeOutput::default();
        let chrome = &mut window.chrome;
        let _ = window.ctx.run(input, |ctx| {
            out = chrome.ui(ctx, &mut ed);
        });
        assert!(
            !window.chrome.dialogs.is_open(),
            "a Ctrl+click asked instead of loading"
        );
        let status =
            crate::menu_bridge::perform(ui::menu::MenuAction::LoadSelection, &mut ed).unwrap();
        assert!(status.contains("Alpha 1"), "{status} / {out:?}");
        assert_eq!(ed.active().unwrap().document.selection, first);
    }

    /// An editor whose active document is a 300x200 image under `n - 1`
    /// further raster layers, every layer holding real pixels.
    fn editor_with_layers(dir: &std::path::Path, n: usize) -> (Editor, Vec<LayerId>) {
        let mut ed = editor(dir);
        let path = dir.join("stack.png");
        std::fs::write(
            &path,
            raster::encode(raster::ExportFormat::Png, 300, 200, &[120u8; 300 * 200 * 4]).unwrap(),
        )
        .unwrap();
        ed.open_path(&path).unwrap();
        let open = ed.active_mut().unwrap();
        let mut ids = vec![open.document.active_layer().unwrap()];
        for i in 1..n {
            let layer = layer_model::Layer::raster(format!("layer {i}"));
            let id = layer.id;
            open.apply(Command::create_layer(layer)).unwrap();
            paint_grey(open, id, 20 * i as u8);
            ids.push(id);
        }
        (ed, ids)
    }

    /// Draw one frame through the real chrome and hand back what it painted.
    fn thumb_frame(
        ctx: &egui::Context,
        chrome: &mut Chrome,
        editor: &mut Editor,
    ) -> Vec<egui::Shape> {
        ctx.run(raw_input(Vec::new()), |ctx| {
            chrome.ui(ctx, editor);
        })
        .shapes
        .into_iter()
        .map(|clipped| clipped.shape)
        .collect()
    }

    /// The texture ids of every textured mesh one frame painted (an image is
    /// a textured quad mesh in epaint).
    fn painted_texture_ids(shapes: &[egui::Shape]) -> Vec<egui::TextureId> {
        fn walk(shape: &egui::Shape, out: &mut Vec<egui::TextureId>) {
            match shape {
                egui::Shape::Mesh(mesh) => out.push(mesh.texture_id),
                egui::Shape::Vec(inner) => inner.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        shapes.iter().for_each(|s| walk(s, &mut out));
        out
    }

    /// W4-I: through the real chrome, the composite preview of each state
    /// the document reaches is uploaded as that History row's thumbnail, and
    /// the History panel paints it.
    #[test]
    fn history_rows_get_the_composite_of_each_state_they_were_at() {
        use ui::panels::history::HistoryThumbs;
        let dir = tempfile::tempdir().unwrap();
        let (mut ed, _) = editor_with_layers(dir.path(), 1);
        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut chrome = Chrome::new();
        chrome
            .workspace
            .dock
            .set_open(ui::dock::PanelId::History, true);
        for _ in 0..3 {
            thumb_frame(&ctx, &mut chrome, &mut ed);
        }
        let opened = HistoryThumbs::texture(&ctx, 0).expect("row 0 was captured");

        ed.apply_command(Command::create_layer(layer_model::Layer::raster("next")));
        let mut shapes = Vec::new();
        for _ in 0..3 {
            shapes = thumb_frame(&ctx, &mut chrome, &mut ed);
        }
        let edited = HistoryThumbs::texture(&ctx, 1).expect("row 1 was captured");
        assert_ne!(opened.id(), edited.id());
        let painted = painted_texture_ids(&shapes);
        assert!(
            painted.contains(&opened.id()) && painted.contains(&edited.id()),
            "the History panel did not paint both rows' thumbnails"
        );
    }

    /// W4-I: the Actions panel through the real chrome: a two-step
    /// recording is listed by name, its twirl shows both steps, and
    /// selecting it and pressing Play replays both on the active document.
    #[test]
    fn the_actions_panel_lists_expands_and_plays_a_recording_through_the_chrome() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        ed.open_path(&png(dir.path(), "a.png")).unwrap();
        ed.open_path(&png(dir.path(), "b.png")).unwrap();
        ed.activate(0).unwrap();
        let rect = editor_core::Selection::Rect {
            min: glam::IVec2::new(1, 1),
            max: glam::IVec2::new(3, 3),
        };
        let make = Command::create_layer(layer_model::Layer::raster("Recorded"));
        let select = Command::SetSelection {
            selection: rect.clone(),
        };
        let labels = [make.label(), select.label()];
        ed.start_recording();
        ed.apply_command(make);
        ed.apply_command(select);
        assert_eq!(ed.stop_recording().map(|e| e.len()), Some(2));
        ed.activate(1).unwrap();
        let before = ed.active().unwrap().document.layers.len();

        let mut win = Window::new(&mut ed);
        win.chrome
            .workspace
            .dock
            .set_open(ui::dock::PanelId::Actions, true);
        win.chrome.workspace.dock.raise(ui::dock::PanelId::Actions);
        win.settle(&mut ed);
        let texts = win.painted_texts(&mut ed);
        assert!(
            texts.iter().any(|t| t == "Action 1"),
            "the recording is not listed: {texts:?}"
        );
        // A step's label can also be painted elsewhere (a menu title, the
        // History panel), so the twirl is judged by what it adds.
        let count = |texts: &[String], label: &str| texts.iter().filter(|t| *t == label).count();
        let collapsed = labels.clone().map(|l| count(&texts, &l));

        win.click(&mut ed, egui::Id::new(("raster-actions-twirl", 0usize)));
        win.settle(&mut ed);
        let texts = win.painted_texts(&mut ed);
        for (label, was) in labels.iter().zip(collapsed) {
            assert!(
                count(&texts, label) > was,
                "the expanded action does not show the step {label:?}: {texts:?}"
            );
        }

        win.click(&mut ed, egui::Id::new(("raster-actions-row", 0usize)));
        win.frame(&mut ed);
        win.click(&mut ed, egui::Id::new("raster-actions-play"));
        win.frame(&mut ed);
        let doc = &ed.active().unwrap().document;
        assert_eq!(doc.layers.len(), before + 1, "the layer step replayed");
        assert_eq!(doc.selection, rect, "the selection step replayed");
    }

    /// W4-I: a swatch added in the Swatches panel through the real chrome is
    /// written to the preferences file, and a fresh editor built the way the
    /// application starts ([`Editor::new`] reads that file) shows it again.
    #[test]
    fn a_swatch_added_in_the_panel_survives_a_restart_through_the_chrome() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        ed.open_path(&png(dir.path(), "a.png")).unwrap();
        let mut win = Window::new(&mut ed);
        win.chrome
            .workspace
            .dock
            .set_open(ui::dock::PanelId::Swatches, true);
        win.chrome.workspace.dock.raise(ui::dock::PanelId::Swatches);
        let odd = [0.125, 0.75, 0.375, 1.0];
        ed.set_foreground(odd);
        win.settle(&mut ed);
        let before = win.chrome.workspace.swatches.len();
        assert!(win.chrome.workspace.swatches.index_of(odd).is_none());
        win.click_text(&mut ed, ui::strings::tr("ui.docks.add.current.colour"));
        win.frame(&mut ed);
        assert_eq!(win.chrome.workspace.swatches.len(), before + 1);

        let paths = AppPaths::rooted(dir.path());
        let saved = Preferences::load(&paths.preferences_file());
        assert!(
            saved
                .swatches
                .as_ref()
                .is_some_and(|s| s.iter().any(|w| w.rgba == odd)),
            "the added swatch was not written to the preferences file"
        );

        let mut fresh = Editor::new(paths, Box::new(ScriptedDialogs::new()));
        fresh.set_image_clipboard(Box::new(crate::clipboard::FakeClipboard::new()));
        let mut win = Window::new(&mut fresh);
        win.frame(&mut fresh);
        let swatches = &win.chrome.workspace.swatches;
        assert_eq!(swatches.len(), before + 1, "{:?}", swatches.swatches());
        assert!(
            swatches.index_of(odd).is_some(),
            "the added swatch did not survive the restart"
        );
    }

    /// W4-I: at the history limit each edit compacts the oldest entry off
    /// the stack and every surviving state moves down one row; the pictures
    /// move with their states instead of staying on row numbers that now
    /// name other states, and the opened state stops being marked as the
    /// History Brush's source row.
    #[test]
    fn history_thumbnails_follow_their_states_when_the_limit_compacts_the_stack() {
        use ui::panels::history::HistoryThumbs;
        let dir = tempfile::tempdir().unwrap();
        let (mut ed, _) = editor_with_layers(dir.path(), 1);
        ed.active_mut().unwrap().history.set_limit(3);
        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut chrome = Chrome::new();
        let settle = |chrome: &mut Chrome, ed: &mut Editor| {
            for _ in 0..3 {
                thumb_frame(&ctx, chrome, ed);
            }
        };
        settle(&mut chrome, &mut ed);
        let mut pictures = vec![HistoryThumbs::texture(&ctx, 0).unwrap().id()];
        for name in ["a", "b", "c"] {
            ed.apply_command(Command::create_layer(layer_model::Layer::raster(name)));
            settle(&mut chrome, &mut ed);
            let row = ed.active().unwrap().history.undo_depth();
            pictures.push(HistoryThumbs::texture(&ctx, row).unwrap().id());
        }
        assert_eq!(ed.active().unwrap().history.undo_depth(), 3);
        assert_eq!(HistoryThumbs::opened_row(&ctx), Some(0));

        // One more edit: the entry for "a" is compacted away, so row 0 is
        // now the state after "a", row 1 after "b", row 2 after "c".
        ed.apply_command(Command::create_layer(layer_model::Layer::raster("d")));
        settle(&mut chrome, &mut ed);
        assert_eq!(ed.active().unwrap().history.undo_depth(), 3);
        for row in 0..3 {
            assert_eq!(
                HistoryThumbs::texture(&ctx, row).map(|t| t.id()),
                Some(pictures[row + 1]),
                "row {row} does not show the picture of the state it now is"
            );
        }
        let newest = HistoryThumbs::texture(&ctx, 3).unwrap().id();
        assert!(!pictures.contains(&newest), "row 3 is the new state");
        assert_eq!(
            HistoryThumbs::opened_row(&ctx),
            None,
            "the opened state is no longer in the list"
        );
    }

    #[test]
    fn layer_thumbnails_are_recomposited_only_for_the_layer_that_changed() {
        let dir = tempfile::tempdir().unwrap();
        let (mut ed, ids) = editor_with_layers(dir.path(), 6);
        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut chrome = Chrome::new();

        // The first pass is throttled: THUMBS_PER_FRAME per frame, so six
        // layers take three frames and no frame composites more than two.
        let mut frames = 0;
        while chrome.workspace().layer_thumbs.len() < ids.len() {
            let before = chrome.thumb_cache_for_test().recomposites();
            thumb_frame(&ctx, &mut chrome, &mut ed);
            frames += 1;
            let done = chrome.thumb_cache_for_test().recomposites() - before;
            assert!(
                done <= THUMBS_PER_FRAME as u64,
                "frame {frames} recomposited {done} thumbnails"
            );
            assert!(frames <= ids.len(), "the pass never completes");
        }
        assert_eq!(frames, ids.len().div_ceil(THUMBS_PER_FRAME));
        let full_pass = chrome.thumb_cache_for_test().recomposites();
        assert_eq!(full_pass, ids.len() as u64, "one composite per layer");
        let texture_ids: Vec<egui::TextureId> = ids
            .iter()
            .map(|id| chrome.workspace().layer_thumbs[id].id())
            .collect();

        // Three more frames with nothing changed: zero recomposites.
        let mut shapes = Vec::new();
        for _ in 0..3 {
            shapes = thumb_frame(&ctx, &mut chrome, &mut ed);
        }
        assert_eq!(
            chrome.thumb_cache_for_test().recomposites(),
            full_pass,
            "an unchanged stack recomposites nothing"
        );

        // The thumbnails reach the screen: the Layers panel painted every
        // layer's texture this frame.
        let painted = painted_texture_ids(&shapes);
        for (id, tex) in ids.iter().zip(&texture_ids) {
            assert!(
                painted.contains(tex),
                "layer {id}'s thumbnail texture was not painted; painted {painted:?}"
            );
        }

        // Edit one layer: exactly one recompute, written into the SAME
        // texture (the panel keeps drawing the id it had).
        paint_grey(ed.active_mut().unwrap(), ids[3], 240);
        thumb_frame(&ctx, &mut chrome, &mut ed);
        assert_eq!(
            chrome.thumb_cache_for_test().recomposites(),
            full_pass + 1,
            "one edited layer is one recompute"
        );
        for (id, tex) in ids.iter().zip(&texture_ids) {
            assert_eq!(
                chrome.workspace().layer_thumbs[id].id(),
                *tex,
                "the texture handle is updated in place, never recreated"
            );
        }
        // And nothing more on the frame after.
        thumb_frame(&ctx, &mut chrome, &mut ed);
        assert_eq!(chrome.thumb_cache_for_test().recomposites(), full_pass + 1);
    }

    #[test]
    fn cached_thumbnails_cost_a_fraction_of_the_compositor_calls_of_the_per_frame_rebuild() {
        // A ratio on the same machine and the same fixture, in compositor
        // calls — never wall clock. The old policy recomposited every layer
        // every frame; the cache composites each layer once.
        let dir = tempfile::tempdir().unwrap();
        let (mut ed, ids) = editor_with_layers(dir.path(), 6);
        const FRAMES: usize = 30;

        // Baseline: the pre-cache policy, one `layer_thumbnail` per layer
        // per frame, counted in compositor calls.
        let before = crate::doc::thumbnail_composites();
        for _ in 0..FRAMES {
            let open = ed.active().unwrap();
            for &id in &ids {
                open.layer_thumbnail(id, THUMB_EDGE).unwrap();
            }
        }
        let uncached = crate::doc::thumbnail_composites() - before;
        assert!(uncached >= (FRAMES * ids.len()) as u64, "{uncached}");

        // The real route: thirty frames of the chrome.
        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut chrome = Chrome::new();
        let before = crate::doc::thumbnail_composites();
        for _ in 0..FRAMES {
            thumb_frame(&ctx, &mut chrome, &mut ed);
        }
        let cached = crate::doc::thumbnail_composites() - before;
        assert!(cached > 0, "the cached path composited nothing at all");
        assert_eq!(
            chrome.workspace().layer_thumbs.len(),
            ids.len(),
            "every layer has a thumbnail by frame {FRAMES}"
        );
        assert!(
            uncached >= 5 * cached,
            "cached path is not 5x cheaper: {uncached} vs {cached} compositor calls"
        );
    }

    #[test]
    fn switching_documents_drops_the_other_documents_thumbnails() {
        let dir = tempfile::tempdir().unwrap();
        let (mut ed, ids) = editor_with_layers(dir.path(), 3);
        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut chrome = Chrome::new();
        for _ in 0..3 {
            thumb_frame(&ctx, &mut chrome, &mut ed);
        }
        assert_eq!(chrome.workspace().layer_thumbs.len(), ids.len());

        // A second document becomes active: the first stack's textures go,
        // the second's arrive.
        let p = png(dir.path(), "second.png");
        ed.open_path(&p).unwrap();
        thumb_frame(&ctx, &mut chrome, &mut ed);
        let second = ed.active().unwrap().document.active_layer().unwrap();
        assert!(chrome.workspace().layer_thumbs.contains_key(&second));
        for id in &ids {
            assert!(
                !chrome.workspace().layer_thumbs.contains_key(id),
                "layer {id} of the first document is still uploaded"
            );
        }
    }

    /// W5-F: thirty frames of a colour-well drag — the shell hands each
    /// frame's `ChromeOutput::set_foreground` to `Editor::set_foreground`,
    /// and a brush-size drag lands in `set_brush` — build ZERO composite
    /// previews (each one reads the whole canvas), capture no History
    /// thumbnail and run no Layers fingerprint pass; one real edit
    /// afterwards builds exactly one.
    #[test]
    fn a_colour_slider_drag_recomposites_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (mut ed, ids) = editor_with_layers(dir.path(), 2);
        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut chrome = Chrome::new();
        for _ in 0..4 {
            thumb_frame(&ctx, &mut chrome, &mut ed);
        }
        let builds = chrome.preview_builds;
        let passes = chrome.thumb_passes;
        assert!(builds >= 1, "fixture: the preview was never built");
        let revision = ed.revision();
        for i in 0..30 {
            let v = i as f32 / 30.0;
            ed.set_foreground([v, 1.0 - v, 0.5, 1.0]);
            let mut brush = *ed.brush();
            brush.size = 10.0 + i as f32;
            ed.set_brush(brush);
            thumb_frame(&ctx, &mut chrome, &mut ed);
        }
        assert!(
            ed.revision() > revision,
            "the drag moved the editor revision"
        );
        assert_eq!(
            chrome.preview_builds, builds,
            "a colour drag recomposited the canvas preview"
        );
        assert_eq!(
            chrome.thumb_passes, passes,
            "a colour drag re-fingerprinted the layers"
        );

        paint_grey_through_editor(&mut ed, ids[1], 77);
        thumb_frame(&ctx, &mut chrome, &mut ed);
        thumb_frame(&ctx, &mut chrome, &mut ed);
        assert_eq!(chrome.preview_builds, builds + 1, "one edit, one preview");
        assert_eq!(
            chrome.thumb_passes,
            passes + 1,
            "one edit, one fingerprint pass"
        );
    }

    /// W5-F: the Colour Sampler rows composite (one 1x1, tile-quantised
    /// composite per point) only when the document's pixels, the document or
    /// a point moved: idle frames and a colour drag composite zero; a paint
    /// under the points — even one written straight into the document,
    /// bypassing the editor — re-reads them on the next frame.
    #[test]
    fn colour_samplers_composite_nothing_while_nothing_moved() {
        let dir = tempfile::tempdir().unwrap();
        let (mut ed, ids) = editor_with_layers(dir.path(), 2);
        ed.active_mut().unwrap().samplers = vec![
            glam::Vec2::new(10.5, 10.5),
            glam::Vec2::new(200.5, 20.5),
            glam::Vec2::new(40.5, 150.5),
            glam::Vec2::new(290.5, 190.5),
        ];
        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut chrome = Chrome::new();
        thumb_frame(&ctx, &mut chrome, &mut ed);
        assert_eq!(
            chrome.sampler_composites, 4,
            "the first frame reads each point"
        );
        assert_eq!(chrome.workspace.info.samplers.len(), 4);
        for i in 0..30 {
            ed.set_foreground([i as f32 / 30.0, 0.0, 0.0, 1.0]);
            thumb_frame(&ctx, &mut chrome, &mut ed);
        }
        assert_eq!(
            chrome.sampler_composites, 4,
            "idle frames composited samplers"
        );

        let before = chrome.workspace.info.samplers[0].color;
        paint_grey(ed.active_mut().unwrap(), ids[1], 250);
        thumb_frame(&ctx, &mut chrome, &mut ed);
        assert_eq!(
            chrome.sampler_composites, 8,
            "an edit re-reads every point once"
        );
        assert_ne!(
            chrome.workspace.info.samplers[0].color, before,
            "the row under the painted tile did not change"
        );

        // A moved point is a re-read too.
        ed.active_mut().unwrap().samplers[3] = glam::Vec2::new(5.5, 5.5);
        thumb_frame(&ctx, &mut chrome, &mut ed);
        assert_eq!(chrome.sampler_composites, 12);
    }
}
