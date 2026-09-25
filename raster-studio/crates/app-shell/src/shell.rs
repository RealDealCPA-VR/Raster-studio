//! The native shell: the winit event loop, the wgpu surface, and the egui
//! overlay.
//!
//! This is the thin layer. It turns platform events into [`Action`]s, hands
//! them to the [`Editor`], and draws what the editor holds — the composite of
//! the active document ([`CanvasPresenter`]) with the chrome on top
//! ([`Chrome`]). Every decision worth testing lives one layer down, without a
//! window.
//!
//! # Failure is a dialog *and* an exit code
//!
//! Nothing in start-up calls `.expect`. A machine that cannot give us an event
//! loop, an adapter, a surface, or a window gets a [`ShellError`] in a native
//! message box — see [`crate::error`] for why that matters under
//! `panic = "abort"` — **and** the same error comes back out of [`Shell::run`].
//!
//! Both halves were missing, in mirror-image ways:
//!
//! * The failure inside [`Shell::resumed`] showed its dialog and was then
//!   dropped, because `ApplicationHandler` has nowhere to return one. `run_app`
//!   reported the loop's own clean exit, `run` returned `Ok`, and a run that
//!   never opened a window exited 0 — while `studio-desktop`'s module doc
//!   promised a script or a CI job a non-zero status. The error is parked on
//!   [`Shell::startup_error`] and re-raised by [`Shell::finish`].
//! * `EventLoop::new` failing — no `DISPLAY`, an SSH session, a container — was
//!   returned but never shown, although [`ShellError::EventLoop`]'s advice text
//!   ("Raster Studio needs a desktop session…") is written for exactly that
//!   user. It now goes through [`Shell::report_startup_failure`] like every
//!   other start-up failure.
//!
//! # Who owns the keyboard
//!
//! Not `egui-winit`'s `consumed` flag — that is the seam this shell got wrong.
//! egui-winit 0.29 computes `consumed = wants_keyboard_input() || key == Tab`,
//! so **every** Tab press is consumed whatever the modifiers, and the Tab it
//! swallows moves egui's widget focus, after which `wants_keyboard_input()`
//! stays true and no shortcut works again until Escape.
//!
//! Two rules replace it, both pure functions with tests:
//!
//! * [`withhold_from_egui`] — Tab never reaches egui unless egui is recording a
//!   chord, so egui's focus navigation has nothing to steal. Nothing in this
//!   application needs Tab-to-focus; the panels are pointer-driven.
//! * [`route_key`] — the shell performs a chord unless a text field genuinely
//!   holds focus or the shortcut editor is listening for the next chord
//!   ([`KeyboardOwner`]). The second case is what stops recording a shortcut
//!   over Ctrl+Q from quitting the application while recording it.
//!
//! # Who owns the pointer
//!
//! [`crate::tool_input::ToolPointer`], which is where a canvas drag becomes a
//! [`tools::PointerEvent`] in document coordinates, reaches the tool the palette
//! says is selected, and leaves as one undoable command. This module's job is
//! only the winit half: turn `MouseInput`/`CursorMoved` into a
//! [`ui::canvas::PointerInput`], say whether the chrome is under the cursor,
//! refuse the buttons nothing is bound to ([`pointer_button`] — the right one
//! is among them, and its doc says why), and
//! ask for a repaint when the answer changed something. Everything else — which
//! gesture belongs to the camera, what a press over a panel means, how a stroke
//! becomes a history step — lives there, without a window, under test.
//!
//! # The backdrop is a token
//!
//! [`backdrop_srgb`] is the one place the colour around the image comes from,
//! and it is handed to both the empty-window clear and
//! [`render::Canvas::set_backdrop`]. They used to disagree, which made Light
//! mode jump to near-black the moment a document opened.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use glam::Vec2;
use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

use render::{Camera, Canvas, GpuContext, Overlay, MAX_ZOOM, MIN_ZOOM};
use ui::dialogs::DialogAction;

use crate::action::Action;
use crate::chrome::Chrome;
use crate::editor::{ActionError, Editor};
use crate::error::ShellError;
use crate::keymap::{Chord, Key, Resolved};
use crate::prefs::WindowGeometry;
use crate::presenter::{ants_segments, CanvasPresenter, SelectionOutline};
use crate::session::SessionMarker;
use crate::tool_input::ToolPointer;
use ui::canvas::{PointerButton, PointerInput, PointerPhase};

/// The key under a shifted glyph on the US layout, or `None` when the glyph is
/// not one Shift produces there.
///
/// winit's `logical_key` is the character the press *typed*, so with Shift
/// held the `0` key arrives as `)`, `6` as `^`, `;` as `:` and `]` as `}`. A
/// chord is named by the key, not by what the key printed — the menu bar
/// paints `Ctrl+Shift+0`, never `Ctrl+Shift+)` — so [`chord_from_key`] folds
/// the glyph back to its key whenever Shift is down. Letters need no table:
/// [`Key::character`] lowercases them.
///
/// This is the US layout's fold, the one the painted chords assume. On a
/// layout whose shifted glyphs differ the glyph is kept as typed, which is what
/// happened before for every key.
pub fn unshifted_us_glyph(c: char) -> Option<char> {
    Some(match c {
        ')' => '0',
        '!' => '1',
        '@' => '2',
        '#' => '3',
        '$' => '4',
        '%' => '5',
        '^' => '6',
        '&' => '7',
        '*' => '8',
        '(' => '9',
        '_' => '-',
        '+' => '=',
        '{' => '[',
        '}' => ']',
        '|' => '\\',
        ':' => ';',
        '"' => '\'',
        '<' => ',',
        '>' => '.',
        '?' => '/',
        '~' => '`',
        _ => return None,
    })
}

/// Translate a winit key event into a [`Chord`].
///
/// `None` for keys that cannot form a shortcut on their own (a bare modifier,
/// dead keys, IME composition). Letter case is normalised by [`Key::character`],
/// so `Shift+B` and `B` name the same key with the shift flag telling them
/// apart; a shifted punctuation or digit glyph is folded back to its key by
/// [`unshifted_us_glyph`] for the same reason, so `Ctrl+Shift+0` is the chord
/// the menu paints and not a `Ctrl+Shift+)` nothing binds.
pub fn chord_from_key(logical: &winit::keyboard::Key, mods: ModifiersState) -> Option<Chord> {
    let key = match logical {
        winit::keyboard::Key::Character(s) => {
            let mut chars = s.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => {
                    let c = if mods.shift_key() {
                        unshifted_us_glyph(c).unwrap_or(c)
                    } else {
                        c
                    };
                    Key::character(c)
                }
                _ => return None,
            }
        }
        winit::keyboard::Key::Named(named) => named_key(*named)?,
        _ => return None,
    };
    Some(Chord {
        ctrl_or_cmd: mods.control_key() || mods.super_key(),
        alt: mods.alt_key(),
        shift: mods.shift_key(),
        key,
    })
}

fn named_key(named: NamedKey) -> Option<Key> {
    Some(match named {
        NamedKey::Tab => Key::Tab,
        NamedKey::Space => Key::Space,
        NamedKey::Enter => Key::Enter,
        NamedKey::Escape => Key::Escape,
        NamedKey::Backspace => Key::Backspace,
        NamedKey::Delete => Key::Delete,
        NamedKey::ArrowLeft => Key::ArrowLeft,
        NamedKey::ArrowRight => Key::ArrowRight,
        NamedKey::ArrowUp => Key::ArrowUp,
        NamedKey::ArrowDown => Key::ArrowDown,
        NamedKey::F1 => Key::Function(1),
        NamedKey::F2 => Key::Function(2),
        NamedKey::F3 => Key::Function(3),
        NamedKey::F4 => Key::Function(4),
        NamedKey::F5 => Key::Function(5),
        NamedKey::F6 => Key::Function(6),
        NamedKey::F7 => Key::Function(7),
        NamedKey::F8 => Key::Function(8),
        NamedKey::F9 => Key::Function(9),
        NamedKey::F10 => Key::Function(10),
        NamedKey::F11 => Key::Function(11),
        NamedKey::F12 => Key::Function(12),
        _ => return None,
    })
}

/// `true` when releasing this key should give back the temporary hand tool.
pub fn is_temporary_hand_key(logical: &winit::keyboard::Key) -> bool {
    matches!(logical, winit::keyboard::Key::Named(NamedKey::Space))
}

/// W13X-1: the four arrow keys, whose held repeats keep nudging.
pub fn is_nudge_key(logical: &winit::keyboard::Key) -> bool {
    matches!(
        logical,
        winit::keyboard::Key::Named(
            NamedKey::ArrowLeft | NamedKey::ArrowRight | NamedKey::ArrowUp | NamedKey::ArrowDown
        )
    )
}

/// W11-F: the tool a held modifier lends while `tool` is selected, the
/// Photoshop/Photopea everyday gestures:
///
/// * Space held with Ctrl (Command) or Alt is the Zoom tool — a click zooms in
///   at the point, and with Alt it zooms out (the Zoom route reads Alt);
/// * Space alone is the hand, which [`Editor::effective_tool`] already answers
///   from the space bar, so nothing is lent;
/// * Ctrl alone is the Move tool, except on the tools whose own gestures read
///   Ctrl or that already move or navigate ([`ctrl_keeps_the_tool`]);
/// * Alt alone on a colour-painting tool is the Eyedropper, so an Alt-click
///   samples the composite into the foreground and paints nothing.
/// * W13-A: on the tools Ctrl lends the Move tool on, Ctrl+Alt lends it
///   too (Alt no longer blocks the lend), so the drag is the Move tool's
///   Alt+drag copy (`move_duplicate`); on the tools in [`ctrl_keeps_the_tool`]
///   Ctrl+Alt lends nothing. With the Move tool itself in hand Alt lends
///   nothing ([`alt_samples_colour`] is false for it), so its Alt+drag copies.
///
/// Shift never lends a tool: on a stroke tool it draws the straight line
/// (`tools::stroke`), on the selection tools it adds.
pub fn temporary_tool_for(
    tool: tools::ToolId,
    space_held: bool,
    mods: tools::Modifiers,
) -> Option<tools::ToolId> {
    use tools::ToolId as T;
    if space_held {
        return (mods.ctrl || mods.alt).then_some(T::Zoom);
    }
    if mods.ctrl && !ctrl_keeps_the_tool(tool) {
        return Some(T::Move);
    }
    if mods.alt && !mods.ctrl && alt_samples_colour(tool) {
        return Some(T::Eyedropper);
    }
    None
}

/// W11-F: the tools a held Ctrl does not turn into the Move tool — the ones
/// that already move or navigate, and the ones whose own gestures read Ctrl
/// or hold an open session (the path tools, text, transforms, crops, slices).
fn ctrl_keeps_the_tool(tool: tools::ToolId) -> bool {
    use tools::ToolId as T;
    matches!(
        tool,
        T::Move
            | T::Hand
            | T::Zoom
            | T::RotateView
            | T::Pen
            | T::FreeformPen
            | T::CurvaturePen
            | T::AddAnchor
            | T::DeleteAnchor
            | T::ConvertAnchor
            | T::PathSelect
            | T::DirectSelection
            | T::Type
            | T::VerticalType
            | T::HorizontalTypeMask
            | T::VerticalTypeMask
            | T::FreeTransform
            | T::Crop
            | T::PerspectiveCrop
            | T::Slice
            | T::SliceSelect
            | T::Artboard
    )
}

/// W11-F: the colour-painting tools on which Alt is the Eyedropper. The
/// source-reading stroke tools (Clone Stamp, the healing brushes) keep Alt
/// for their sample point, the selection tools for subtract, the Eraser and
/// the retouching tools have no colour to pick.
fn alt_samples_colour(tool: tools::ToolId) -> bool {
    use tools::ToolId as T;
    matches!(
        tool,
        T::Brush | T::Pencil | T::ColorReplacement | T::PaintBucket | T::Gradient | T::MixerBrush
    )
}

/// `true` for the key egui would use to move widget focus.
///
/// Tab, and only Tab. egui advances focus on any Tab press
/// (`egui::memory::Focus::begin_pass`), and `egui-winit` 0.29 reports **every**
/// Tab press as consumed whatever the modifiers are
/// (`consumed = wants_keyboard_input() || key == Tab`). Between them, the three
/// Tab chords this application ships — Tab, Ctrl+Tab, Ctrl+Shift+Tab — could
/// never fire, *and* the swallowed Tab left an egui button focused, which makes
/// `wants_keyboard_input()` true and killed every other shortcut until the user
/// happened to press Escape.
///
/// So Tab is not offered to egui at all unless a chord is being recorded (where
/// egui is the thing that reads it). Nothing in this application needs Tab
/// focus navigation; the panels are pointer-driven and the shortcut editor
/// records keys directly.
pub fn is_focus_navigation_key(logical: &winit::keyboard::Key) -> bool {
    matches!(logical, winit::keyboard::Key::Named(NamedKey::Tab))
}

/// `true` when this key press must not be handed to egui at all.
///
/// Only Tab, and only while nothing is recording a chord. egui's *own* use for
/// Tab is focus navigation, which this application does not want and which is
/// what poisons `wants_keyboard_input()` for every later key press; the one
/// time egui legitimately needs to see a Tab is when the shortcut editor is
/// listening for the next chord and Tab is the chord being pressed.
pub fn withhold_from_egui(owner: KeyboardOwner, logical: &winit::keyboard::Key) -> bool {
    !owner.recording_shortcut && is_focus_navigation_key(logical)
}

/// Who has a claim on the keyboard this frame.
///
/// The two things — and the *only* two things — that may take a key press away
/// from the shell's shortcut table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeyboardOwner {
    /// `egui::Context::wants_keyboard_input()`: a widget holds keyboard focus,
    /// which in this application means a text field or a drag value being typed
    /// into. Focus is never granted by a click in egui 0.29, and Tab never
    /// reaches egui (see [`is_focus_navigation_key`]), so this really does mean
    /// "the user is typing" rather than "some button caught focus".
    pub egui_text_focus: bool,
    /// The shortcut editor is listening for the next chord. The same key press
    /// must not *also* be performed as whatever it currently means — recording
    /// a new shortcut over Ctrl+Q used to quit the application.
    pub recording_shortcut: bool,
}

/// What the shell does with one key event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyOutcome {
    /// Resolve this chord in the keymap and perform what it names.
    Dispatch(Chord),
    /// Space came up: give back the tool the hand borrowed.
    ReleaseTemporaryHand,
    /// Nothing for the shell to do.
    Ignore,
}

/// Decide what a key event means, with no window and no egui context.
///
/// This is the routing decision the shell used to take from `egui-winit`'s
/// `consumed` flag, which is wrong twice over — see [`is_focus_navigation_key`]
/// for the Tab half and [`KeyboardOwner::recording_shortcut`] for the other.
/// Being a pure function, every one of those cases is a test.
pub fn route_key(
    owner: KeyboardOwner,
    logical: &winit::keyboard::Key,
    state: ElementState,
    repeat: bool,
    mods: ModifiersState,
) -> KeyOutcome {
    if state == ElementState::Released {
        // Unconditional, deliberately: if focus moved while Space was held, a
        // guarded release would leave the hand tool engaged for ever. Giving
        // back a hand that was never borrowed is a no-op.
        return if is_temporary_hand_key(logical) {
            KeyOutcome::ReleaseTemporaryHand
        } else {
            KeyOutcome::Ignore
        };
    }
    if owner.recording_shortcut || owner.egui_text_focus {
        return KeyOutcome::Ignore;
    }
    // A held key repeats. Only the temporary hand wants the repeats — it is
    // idempotent and they are what keep it engaged.
    // W13X-1: and the arrows, whose held repeats keep nudging (Photopea).
    if repeat && !is_temporary_hand_key(logical) && !is_nudge_key(logical) {
        return KeyOutcome::Ignore;
    }
    match chord_from_key(logical, mods) {
        Some(chord) => KeyOutcome::Dispatch(chord),
        None => KeyOutcome::Ignore,
    }
}

/// A winit mouse button as the pointer router names it.
///
/// `None` for the buttons nothing is bound to: routing them would claim a
/// gesture that no release ever ends, and the router would then refuse every
/// later press as somebody else's.
///
/// **The right button is one of them, deliberately.**
/// [`ui::canvas::InputRouter`] decides a route from the *tool*, and special-
/// cases only the middle button and the space bar — so a `Secondary` press
/// claims a `Route::Tool` gesture exactly as a `Primary` one does, and
/// [`tools::PointerEvent`] carries no button for a tool to tell the two apart.
/// Routing it would mean a right-drag on the canvas painting a full undoable
/// brush stroke the user never asked for. So the right button stops here,
/// where the platform event is named, rather than in the shared router that
/// `ui` also uses — and what it *does* on the canvas happens on egui's side of
/// the same event: egui-winit is handed every window event before this, and a
/// right-click over the bare canvas opens the canvas context menu at the
/// pointer (`canvas_extras::CanvasExtras::paint`, W4-C), whose rows route
/// through `Chrome::route`.
pub fn pointer_button(button: MouseButton) -> Option<PointerButton> {
    match button {
        MouseButton::Left => Some(PointerButton::Primary),
        MouseButton::Middle => Some(PointerButton::Middle),
        MouseButton::Right | MouseButton::Back | MouseButton::Forward | MouseButton::Other(_) => {
            None
        }
    }
}

/// Pixels one wheel notch pans the view by; also how a touchpad's pixel
/// delta is turned into notches.
const WHEEL_LINE_PX: f32 = 60.0;

/// What one wheel event does to the view.
#[derive(Debug, Clone, Copy, PartialEq)]
enum WheelGesture {
    /// Multiply the zoom by this, about the cursor.
    Zoom(f32),
    /// Move the picture by this many screen pixels.
    Pan(Vec2),
}

/// Decide what a wheel event of `lines` notches does.
///
/// W13-M: Photopea's rule, read from its canvas wheel handler. Alt inverts
/// the Scroll-wheel-zooms preference (`wheel_zooms`): with it off, a plain
/// wheel pans and Alt + wheel zooms; with it on, a plain wheel zooms and
/// Alt + wheel pans. Ctrl (Command) + wheel, when it does not zoom, pans
/// *horizontally* — Photopea's Hand swaps the axes while Ctrl is held — so
/// with the preference off Ctrl + wheel scrolls sideways rather than zooming.
/// Shift + wheel turns a vertical wheel horizontal (what the browser hands
/// Photopea for Shift + wheel) and always pans, so a Shift + wheel is never
/// a zoom by nothing.
fn wheel_gesture(
    lines: Vec2,
    ctrl: bool,
    alt: bool,
    shift: bool,
    wheel_zooms: bool,
) -> WheelGesture {
    if alt != wheel_zooms && !shift {
        return WheelGesture::Zoom((1.0 + lines.y * 0.1).clamp(0.2, 5.0));
    }
    let lines = if shift && lines.x == 0.0 {
        Vec2::new(lines.y, 0.0)
    } else {
        lines
    };
    let lines = if ctrl {
        Vec2::new(lines.y, lines.x)
    } else {
        lines
    };
    WheelGesture::Pan(lines * WHEEL_LINE_PX)
}

/// W13-M: what a key press does to the mask view, Photopea's way (its key
/// handler, `pp.js`, the `mskView` branch): only while the active layer has
/// a mask, a bare `\` toggles the rubylith overlay, a bare `` ` `` toggles
/// the mask shown alone, and Escape puts the image back when a mask view is
/// on. `None` for every other key, and when nothing would change.
fn mask_view_key(
    chord: &Chord,
    current: ui::MaskViewMode,
    has_mask: bool,
) -> Option<ui::MaskViewMode> {
    use ui::MaskViewMode::{Composite, Grayscale, Overlay};
    if !has_mask || chord.ctrl_or_cmd || chord.alt || chord.shift {
        return None;
    }
    let toggle = |mode| if current == mode { Composite } else { mode };
    let next = match chord.key {
        Key::Char('\\') => toggle(Overlay),
        Key::Char('`') => toggle(Grayscale),
        Key::Escape if current != Composite => Composite,
        _ => return None,
    };
    Some(next)
}

// W13-M: the Photopea wheel, tool-letter, mask-thumbnail and print routes,
// driven through the shell's own entry points.
#[cfg(test)]
#[path = "shell_w13m_tests.rs"]
mod w13m_tests;

// W13-L: the Animation timeline's scrub and playback reach the canvas.
#[cfg(test)]
#[path = "shell_w13l_tests.rs"]
mod w13l_tests;

// W13X-1: the arrow keys nudge, driven through `on_key`.
#[cfg(test)]
#[path = "shell_nudge_tests.rs"]
mod nudge_tests;

/// Held modifiers as the tools read them.
///
/// The platform modifier folds into `ctrl`, because a tool that checks `ctrl`
/// means "the key this platform modifies with" — Cmd on macOS. The same rule
/// [`chord_from_key`] applies to shortcuts.
pub fn modifiers_of(mods: ModifiersState) -> tools::Modifiers {
    tools::Modifiers {
        shift: mods.shift_key(),
        alt: mods.alt_key(),
        ctrl: mods.control_key() || mods.super_key(),
    }
}

/// Pick the surface format the canvas will draw to, preferring sRGB.
pub fn choose_surface_format(
    formats: &[wgpu::TextureFormat],
) -> Result<wgpu::TextureFormat, ShellError> {
    formats
        .iter()
        .copied()
        .find(|f| f.is_srgb() && Canvas::supports_target(*f))
        .or_else(|| {
            formats
                .iter()
                .copied()
                .find(|f| Canvas::supports_target(*f))
        })
        .ok_or_else(|| ShellError::UnsupportedSurfaceFormat {
            formats: formats
                .iter()
                .map(|f| format!("{f:?}"))
                .collect::<Vec<_>>()
                .join(", "),
        })
}

/// How often the marching ants are redrawn while a selection is on screen.
///
/// Thirty a second: the pattern moves three grid units a second, so this is
/// several frames per dash — enough to read as motion, and far short of asking
/// for a full repaint at the display's rate for an animation that is four
/// hairlines wide.
const ANTS_FRAME: Duration = Duration::from_millis(33);

/// C1: how many frames a `--shot` renders and discards before capturing.
/// egui learns layout over several passes — the first frame drew an empty tool
/// column and no start screen — and it also ANIMATES: widget fades ease out
/// over ~0.3 s, so a capture before the curves settle differs from a settled
/// one by a uniform one-step fade (measured: 1563 bytes, rows 415-517, every
/// pixel one level darker). 24 frames at the poll rate clears layout AND the
/// longest default animation; two consecutive captures then come out
/// byte-identical (the C1 validate, verified by running `--shot` twice and
/// hashing the PNGs).
const SHOT_WARMUP_FRAMES: u32 = 24;

/// W2-G: how often the loop wakes to look at jobs in flight when nothing else
/// is happening. A frame's worth: a completion is noticed within one frame,
/// and an idle window with a save running costs sixty polls a second rather
/// than a spinning core.
const JOB_POLL: Duration = Duration::from_millis(16);

/// W2-G: the geometry the window opens with.
///
/// A `--shot` capture ignores whatever the last session persisted — always
/// [`WindowGeometry::DEFAULT`], never maximized — so two runs on any machine
/// produce PNGs of the same size and evidence screenshots are reproducible
/// (before this, a persisted maximized 2560×1351 and a fresh 1440×900 profile
/// captured different images of the same build). An ordinary session restores
/// where it was, clamped into something a window manager can honour.
pub fn window_geometry_for(persisted: Option<WindowGeometry>, shot: bool) -> WindowGeometry {
    if shot {
        WindowGeometry::DEFAULT
    } else {
        persisted.unwrap_or(WindowGeometry::DEFAULT).sanitized()
    }
}

/// Read the rendered surface back to the CPU and write it as a PNG at the
/// `--shot` path (S2.3: a literal screenshot of the GUI). Reported, never
/// fatal: a failed capture logs and returns `false`, and the session carries
/// on rather than aborting every other document's unsaved work.
fn capture_shot(
    gpu: &render::context::GpuContext,
    texture: &wgpu::Texture,
    path: Option<&std::path::Path>,
) -> bool {
    let Some(path) = path else { return false };
    let readback = render::offscreen::read_texture_rgba8(gpu, texture, 0);
    match readback {
        Ok(pixels) => match raster::encode(
            raster::ExportFormat::Png,
            pixels.width(),
            pixels.height(),
            pixels.as_rgba8(),
        ) {
            Ok(png) => match std::fs::write(path, png) {
                Ok(()) => {
                    tracing::info!("captured screenshot to {}", path.display());
                    true
                }
                Err(e) => {
                    tracing::error!("could not write screenshot {}: {e}", path.display());
                    false
                }
            },
            Err(e) => {
                tracing::error!("could not encode screenshot: {e}");
                false
            }
        },
        Err(e) => {
            tracing::error!("could not read back the surface for the screenshot: {e}");
            false
        }
    }
}

struct WindowState {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    surface_config: wgpu::SurfaceConfiguration,
    gpu: GpuContext,
    canvas: Canvas,
    /// The marching-ants pass, drawn over the canvas and under the chrome.
    overlay: Overlay,
    /// The traced selection boundary the ants follow, cached across frames.
    outline: SelectionOutline,
    presenter: CanvasPresenter,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
    egui_depth: wgpu::TextureView,
    /// The title last pushed to the window, so it is only set when it changes.
    title: String,
    /// The theme last installed on the egui context.
    theme: design::Theme,
    /// W2-X: the screen mode the window was last put in. The two full-screen
    /// modes are borderless full screen; Standard restores the framed window.
    /// Applied only when the editor's mode moves, so a mode the platform
    /// dropped on its own is not forced back every frame.
    screen_mode: ui::palette::ScreenMode,
}

/// The application: an [`Editor`] plus the window it is shown in.
/// The event loop's user event: how AccessKit's adapter (screen readers,
/// assistive tooling) reaches the shell. `accesskit_winit::Event` is the only
/// variant today; more sources would widen the enum, not change the route.
#[derive(Debug)]
pub enum AppEvent {
    AccessKit(accesskit_winit::Event),
}

impl From<accesskit_winit::Event> for AppEvent {
    fn from(event: accesskit_winit::Event) -> Self {
        Self::AccessKit(event)
    }
}

pub struct Shell {
    editor: Editor,
    chrome: Chrome,
    state: Option<WindowState>,
    marker: Option<SessionMarker>,
    /// Files named on the command line, opened once the window exists.
    startup_files: Vec<PathBuf>,
    cursor: Vec2,
    /// The button currently held, so a `CursorMoved` can name the gesture it
    /// belongs to. winit reports the button on press and release but not on the
    /// moves in between.
    held: Option<PointerButton>,
    /// The Actions panel's last stopped recording, held between Stop and
    /// Replay so the capture survives the recording flag's reset.
    last_recording: Option<Vec<crate::editor::RecordedEdit>>,
    /// The proxy AccessKit's adapter sends its events through, created once
    /// the loop exists and handed to `init_accesskit` at window build.
    accesskit_proxy: Option<winit::event_loop::EventLoopProxy<crate::shell::AppEvent>>,
    /// Pointer input, routed to the active tool or to the camera.
    pointer: ToolPointer,
    modifiers: ModifiersState,
    /// The stylus pressure to stamp on the next pointer samples. A mouse is
    /// `1.0` (full), and egui 0.29's stream carries no pressure — so the native
    /// shell is what reads the winit tablet events and lands them here via
    /// [`Shell::set_pen_pressure`]. This is the S1.4 seam that turns the
    /// engine's pressure-aware stroke (verified in `tools`) into a working
    /// tablet stroke.
    pen_pressure: f32,
    /// W7-A: the touch/pen contact driving the pointer, and the gate that
    /// drops the OS's emulated mouse events for that same contact.
    pen: crate::pen_input::PenInput,
    /// When this shell started, which is the clock the marching ants crawl on.
    /// A wall-clock reading would jump when the system clock is adjusted; the
    /// dash phase is a pure function of this elapsed time, so a dropped frame
    /// catches up rather than making the ants stutter.
    started: Instant,
    /// Card 030: the last value handed to `set_ime_allowed` — winit does not
    /// dedupe the OS call, so per-frame toggling would hammer the IMM.
    ime_allowed_last: bool,
    repaint_at: Option<Instant>,
    /// A start-up failure that happened inside the event loop.
    ///
    /// `ApplicationHandler::resumed` returns `()`, so an error raised there has
    /// nowhere to go: it waits here until [`Shell::finish`] hands it back as
    /// the process's exit status.
    startup_error: Option<ShellError>,
    /// A literal GUI screenshot to capture (`--shot`): after the first frame
    /// is rendered and before it is presented, the surface is read back to
    /// this PNG path and the process exits (S2.3).
    shot: Option<PathBuf>,
    /// Whether the shot has already been taken, so capture happens once.
    shot_taken: bool,
    /// How many frames have rendered since the `--shot` was requested (C1):
    /// the capture waits out the warm-up so egui's layout is settled.
    shot_frames: u32,
    /// The ruler unit the chrome's workspace held when the shell last looked.
    /// A change the shell did not push itself is the user's View ▸ Rulers ▸
    /// <unit> choice, which [`Shell::adopt_ruler_unit`] writes back into the
    /// Units preference so the rulers and every size readout stay one setting.
    ruler_unit_seen: ui::dialogs::Unit,
}

impl Shell {
    /// The shell the desktop binary runs.
    pub fn new(editor: Editor, startup_files: Vec<PathBuf>) -> Self {
        Shell::with_shot(editor, startup_files, None)
    }

    /// As [`Shell::new`], but capture one rendered frame to `shot` (a literal
    /// GUI screenshot) and then exit — the S2.3 path, see [`Shell::run`].
    pub fn with_shot(editor: Editor, startup_files: Vec<PathBuf>, shot: Option<PathBuf>) -> Self {
        let mut shell = Shell {
            editor,
            chrome: Chrome::new(),
            state: None,
            marker: None,
            startup_files,
            cursor: Vec2::ZERO,
            held: None,
            last_recording: None,
            accesskit_proxy: None,
            pointer: ToolPointer::new(),
            modifiers: ModifiersState::empty(),
            pen_pressure: 1.0,
            pen: crate::pen_input::PenInput::new(),
            started: Instant::now(),
            ime_allowed_last: false,
            repaint_at: Some(Instant::now()),
            startup_error: None,
            shot,
            shot_taken: false,
            shot_frames: 0,
            ruler_unit_seen: ui::dialogs::Unit::default(),
        };
        shell.ruler_unit_seen = shell.chrome.workspace().canvas.unit;
        shell.sync_ruler_unit();
        // W5-D: a capture run's fixtures stay out of File > Open Recent.
        if shell.shot.is_some() {
            shell.editor.freeze_recent_files();
        }
        shell
    }

    pub fn editor(&self) -> &Editor {
        &self.editor
    }

    /// Run until the window closes.
    ///
    /// Returns the start-up failure that stopped the window from appearing, if
    /// there was one — the user has already seen it in a dialog by then, and
    /// this is what makes the process exit non-zero for whoever launched it
    /// from a terminal or a script.
    pub fn run(mut self) -> Result<(), ShellError> {
        // A typed user event so AccessKit's adapter can reach us (P3.11):
        // `EventLoop::new` is `EventLoop<()>`, and `()` cannot carry an
        // `accesskit_winit::Event`.
        let event_loop = match EventLoop::<crate::shell::AppEvent>::with_user_event().build() {
            Ok(event_loop) => event_loop,
            // The headless case: no display, an SSH session, a container.
            // Returned *and* shown — `ShellError::EventLoop`'s advice is
            // written for this user, and before this it reached nobody.
            Err(e) => return Err(self.report_startup_failure(ShellError::EventLoop(e))),
        };
        self.accesskit_proxy = Some(event_loop.create_proxy());
        let ran = event_loop.run_app(&mut self);
        self.finish(ran)
    }

    /// Tell the user about a start-up failure, and hand it back to the caller.
    ///
    /// One function for both halves of the promise in this module's doc: the
    /// dialog (the only report a user who double-clicked an icon will ever
    /// see) and the value (the exit code). It reports through the editor's own
    /// [`crate::dialogs::FileDialogs`], so the native build shows a real
    /// message box and a test can read back what was shown.
    fn report_startup_failure(&mut self, error: ShellError) -> ShellError {
        tracing::error!("{error}");
        self.editor
            .report_error(error.title(), &error.user_message());
        error
    }

    /// Start-up failed inside the event loop: tell the user, and keep the
    /// error for [`Shell::finish`].
    ///
    /// Split out of [`Shell::resumed`] because `resumed` needs an
    /// `ActiveEventLoop` no test can build, and "the failure is reported *and*
    /// survives to become the exit code" is the whole of what went wrong here.
    fn start_up_failed(&mut self, error: ShellError) {
        let error = self.report_startup_failure(error);
        self.startup_error = Some(error);
    }

    /// Turn the event loop's result into the shell's.
    ///
    /// A failure inside [`Shell::resumed`] wins over `run_app`'s own `Ok`:
    /// the loop exited cleanly *because* start-up failed, so reporting the
    /// clean exit would report the consequence and hide the cause. That is
    /// exactly what used to happen — the dialog was shown and the process still
    /// exited 0.
    fn finish(&mut self, ran: Result<(), winit::error::EventLoopError>) -> Result<(), ShellError> {
        if let Some(error) = self.startup_error.take() {
            return Err(error);
        }
        match ran {
            Ok(()) => Ok(()),
            Err(e) => Err(self.report_startup_failure(ShellError::EventLoop(e))),
        }
    }

    /// Claim this run's crash marker and offer whatever previous runs left.
    ///
    /// A list, not one record: every crashed run has a marker of its own, and a
    /// machine that lost two of them has two lots of work to offer. Markers
    /// belonging to instances that are *running* are not in this list at all —
    /// see [`crate::session`].
    fn begin_session(&mut self) {
        let (marker, previous) = SessionMarker::begin(self.editor.paths());
        self.marker = Some(marker);
        let mut restored = 0;
        for record in &previous {
            let report = self.editor.recover(record);
            restored += report.restored.len();
            for (project, reason) in &report.failed {
                tracing::warn!("could not recover {}: {reason}", project.display());
            }
        }
        if restored > 0 {
            // Documents, not commands: a scratch autosave replays nothing (the
            // package *is* the work), so counting commands would say
            // "Restored 0" for exactly the case that lost the most.
            self.editor
                .set_status(format!("Restored {restored} document(s)"));
        }
    }

    /// Keep the crash marker in step with what a crash would have to recover:
    /// the packages that are open, *and* the scratch autosaves of the documents
    /// that have no package at all.
    fn sync_marker(&mut self) {
        let projects = self.editor.open_project_paths();
        let autosaves = self.editor.autosave_paths();
        if let Some(marker) = &mut self.marker {
            marker.set_open_projects(projects);
            marker.set_autosaves(autosaves);
        }
    }

    /// Perform an action, turning a refusal into a status message (or a dialog
    /// when something actually failed).
    fn perform(&mut self, action: Action) {
        // File ▸ New is a question, not an edit: the dialog asks for size and
        // background before anything is created. The chrome's dialog host owns
        // the question, so the shell opens it and the confirmed spec comes
        // back through [`ChromeOutput::dialog`].
        if matches!(action, Action::NewDocument) {
            self.chrome.open_new_document_dialog();
            self.repaint_at = Some(Instant::now());
            return;
        }
        match self.editor.dispatch(action) {
            Ok(_) => {}
            Err(ActionError::Cancelled(_)) => {}
            Err(ActionError::Unavailable { reason, .. }) => self.editor.set_status(reason),
            Err(ActionError::Failed { action, reason }) => {
                let title = format!("{} failed", action.label());
                self.editor.report_error(&title, &reason);
            }
        }
        self.repaint_at = Some(Instant::now());
    }

    fn build_window(&mut self, event_loop: &ActiveEventLoop) -> Result<WindowState, ShellError> {
        let geometry = window_geometry_for(self.editor.preferences().window, self.shot.is_some());
        let attrs = Window::default_attributes()
            .with_title(self.editor.window_title())
            .with_inner_size(PhysicalSize::new(geometry.width, geometry.height))
            .with_position(PhysicalPosition::new(geometry.x, geometry.y))
            .with_maximized(geometry.maximized);
        let window = Arc::new(event_loop.create_window(attrs)?);
        // W13-M: File > Print's system dialog opens owned by this window.
        crate::editor::print::set_owner_window(&window);

        let instance = wgpu::Instance::default();
        let surface = instance.create_surface(window.clone())?;
        let gpu = pollster::block_on(GpuContext::for_surface(instance, &surface))
            .map_err(ShellError::Gpu)?;
        // The diagnostics bundle names the adapter the window ACTUALLY got.
        self.editor
            .set_gpu_adapter_name(gpu.adapter.get_info().name);

        let size = window.inner_size();
        let caps = surface.get_capabilities(&gpu.adapter);
        let format = choose_surface_format(&caps.formats)?;
        let surface_config = wgpu::SurfaceConfiguration {
            // The literal `--shot` screenshot reads the surface back to the CPU,
            // which needs `COPY_SRC`; an ordinary session does not pay for it.
            usage: if self.shot.is_some() {
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC
            } else {
                wgpu::TextureUsages::RENDER_ATTACHMENT
            },
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&gpu.device, &surface_config);

        let theme = self
            .editor
            .preferences()
            .theme
            .resolve(system_theme(&window));
        let mut canvas = Canvas::new(&gpu, format);
        canvas.set_backdrop(backdrop_srgb(theme));
        // `choose_surface_format` already refused anything `Canvas` cannot
        // draw into, and the overlay's rule is the same one, so this cannot
        // fail here.
        let overlay = Overlay::new(&gpu, format);

        let egui_ctx = egui::Context::default();
        crate::chrome::install_theme(&egui_ctx, theme);
        egui_ctx.set_zoom_factor(self.editor.preferences().ui_scale);
        let mut egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &*window,
            Some(window.scale_factor() as f32),
            window.theme(),
            Some(gpu.adapter.limits().max_texture_dimension_2d as usize),
        );
        // AccessKit (P3.11): screen readers and assistive tooling get a labelled
        // node per egui widget. egui-winit publishes the tree; the event side
        // arrives as `AppEvent::AccessKit` and is routed in `user_event`.
        if let Some(proxy) = self.accesskit_proxy.as_ref() {
            egui_state.init_accesskit(&window, proxy.clone());
        }
        let egui_renderer = egui_wgpu::Renderer::new(
            &gpu.device,
            format,
            Some(wgpu::TextureFormat::Depth32Float),
            1,
            true,
        );
        let egui_depth = create_depth_view(&gpu, size.width.max(1), size.height.max(1));

        Ok(WindowState {
            title: self.editor.window_title(),
            window,
            surface,
            surface_config,
            gpu,
            canvas,
            overlay,
            outline: SelectionOutline::new(),
            presenter: CanvasPresenter::new(),
            egui_ctx,
            egui_state,
            egui_renderer,
            egui_depth,
            theme,
            screen_mode: ui::palette::ScreenMode::Standard,
        })
    }

    /// W2-G: one frame's worth of job bookkeeping — apply every import, save
    /// and export that has landed, keep the crash marker current, and say
    /// whether anything is still running. Called once per loop iteration by
    /// `about_to_wait`; a test with no window calls it directly to step the
    /// loop.
    fn pump_jobs(&mut self) -> bool {
        if !self.editor.jobs_pending() {
            return false;
        }
        let autosaves_before = self.editor.autosave_paths();
        self.editor.poll_jobs();
        // A scratch autosave that has just landed is only recoverable once
        // the marker names it.
        if self.editor.autosave_paths() != autosaves_before {
            self.sync_marker();
        }
        self.editor.jobs_pending()
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        let Some(state) = &mut self.state else { return };
        if size.width == 0 || size.height == 0 {
            return;
        }
        state.surface_config.width = size.width;
        state.surface_config.height = size.height;
        state
            .surface
            .configure(&state.gpu.device, &state.surface_config);
        state.egui_depth = create_depth_view(&state.gpu, size.width, size.height);
        self.spread_viewport(Vec2::new(size.width as f32, size.height as f32));
    }

    /// Hand every open document the canvas area it is drawn in, on a
    /// `surface` of this many physical pixels.
    ///
    /// Through [`Chrome::place_canvas`] (and so
    /// [`OpenDocument::set_viewport`]) rather than by assigning
    /// `camera.viewport_size`, because a document that has never been drawn
    /// still owes the user a fit and this is the moment its size is known. A
    /// background tab opened while another was active gets fitted here too,
    /// rather than the first time it happens to be redrawn. The area is the
    /// last laid-out frame's canvas area cut back to the new surface, or the
    /// surface itself before any frame was laid out.
    fn spread_viewport(&mut self, surface: Vec2) {
        for doc in self.editor.documents_mut() {
            self.chrome.place_canvas(doc, surface);
        }
    }

    /// Store the window's geometry so the next session opens where this one was.
    ///
    /// Not for a `--shot` run: its window is the fixed capture geometry, not
    /// where the user left anything, and persisting it would overwrite the
    /// real session's record (W2-G).
    fn capture_geometry(&mut self) {
        if self.shot.is_some() {
            return;
        }
        let Some(state) = &self.state else { return };
        let size = state.window.inner_size();
        let position = state
            .window
            .outer_position()
            .unwrap_or(PhysicalPosition::new(
                WindowGeometry::DEFAULT.x,
                WindowGeometry::DEFAULT.y,
            ));
        let geometry = WindowGeometry {
            x: position.x,
            y: position.y,
            width: size.width,
            height: size.height,
            maximized: state.window.is_maximized(),
        }
        .sanitized();
        let mut prefs = self.editor.preferences().clone();
        prefs.window = Some(geometry);
        self.editor.set_preferences(prefs);
    }

    /// Clean exit: saves in flight, geometry, preferences, recent files, then
    /// the crash marker.
    fn shut_down(&mut self) {
        // A save still running on a worker — an autosave the timer started, a
        // Ctrl+S the user did not wait for — lands before the process goes.
        // The one place the interaction thread blocks on a save, and the one
        // where nothing else needs it.
        self.editor.wait_for_saves();
        self.capture_geometry();
        if let Err(e) = self.editor.persist() {
            tracing::warn!("could not save preferences: {e}");
        }
        if let Some(marker) = self.marker.take() {
            marker.finish();
        }
    }

    /// Re-install the theme and UI scale if the preferences moved.
    ///
    /// Applied every frame rather than only at start-up, so changing the theme
    /// or the scale takes effect without a restart.
    fn sync_appearance(&mut self) {
        let choice = self.editor.preferences().theme;
        let scale = self.editor.preferences().ui_scale;
        let Some(state) = &mut self.state else { return };
        // W13X-4: any of Photopea's themes, the moment the preference moves.
        if let Some(resolved) =
            crate::prefs::theme_to_install(choice, system_theme(&state.window), state.theme)
        {
            crate::chrome::install_theme(&state.egui_ctx, resolved);
            // The area around the image is a themed surface like any other.
            state.canvas.set_backdrop(backdrop_srgb(resolved));
            state.theme = resolved;
        }
        if (state.egui_ctx.zoom_factor() - scale).abs() > f32::EPSILON {
            state.egui_ctx.set_zoom_factor(scale);
        }
        // W2-X: the window follows the editor's screen mode (plain F, the
        // palette footer). The chrome hides its bands from the same value.
        let mode = self.editor.screen_mode();
        if state.screen_mode != mode {
            state.window.set_fullscreen(
                mode.fullscreen()
                    .then_some(winit::window::Fullscreen::Borderless(None)),
            );
            state.screen_mode = mode;
        }
    }

    fn redraw(&mut self) {
        self.sync_appearance();
        // Snapshot the shot target before the mutable borrow of `state` below,
        // so the capture (which only needs `&state.gpu`) does not compete for
        // `&mut self` mid-frame.
        let shot_target = self.shot.clone();
        // C1: a `--shot` must not capture frame one — egui learns layout over
        // several frames, and the first one drew an empty tool column and no
        // start screen (the committed main-window.png showed exactly that).
        // Warm up: count frames while the shot is pending and capture only
        // after SHOT_WARMUP_FRAMES.
        if shot_target.is_some() && !self.shot_taken {
            self.shot_frames += 1;
        }
        let shot_requested =
            self.shot.is_some() && !self.shot_taken && self.shot_frames >= SHOT_WARMUP_FRAMES;
        let Some(state) = &mut self.state else { return };
        let frame = match state.surface.get_current_texture() {
            Ok(f) => f,
            Err(wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated) => {
                state
                    .surface
                    .configure(&state.gpu.device, &state.surface_config);
                state.window.request_redraw();
                return;
            }
            Err(e) => {
                tracing::warn!("dropped frame: {e:?}");
                return;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        // ---- document -> compositor -> GPU texture ----
        let mut camera = Camera::new(Vec2::ONE, Vec2::ONE);
        let mut have_document = false;
        let surface_px = Vec2::new(
            state.surface_config.width as f32,
            state.surface_config.height as f32,
        );
        // The canvas area this frame renders into: the one the last laid-out
        // frame left between the panels. Compared after the chrome runs, so a
        // frame that moved the panels is followed by one drawn into the new
        // area.
        let area_used = self.chrome.canvas_area_px();
        if let Some(doc) = self.editor.active_mut() {
            // The first frame is where a freshly opened document learns how big
            // its canvas area is, and therefore where it is fitted to it —
            // between the docks, not across the window.
            self.chrome.place_canvas(doc, surface_px);
            camera = doc.camera.clone();
            have_document = true;
            // The Channels panel's component toggles are a view setting, so
            // they are applied on the way to the texture rather than to the
            // document. Read every frame: the panel is the authority, and the
            // presenter re-uploads only when the answer actually changes.
            // Card 059: the mask view rides the same per-frame read — the
            // panel owns it, the chrome exposes it, the presenter applies it.
            // W7-D: so do View > Proof Colors / Gamut Warning.
            state.presenter.read_view_settings(&self.chrome);
            match state.presenter.sync(&state.gpu, doc) {
                Ok(report) => {
                    if report.texture_replaced {
                        if let Some(texture) = state.presenter.texture() {
                            state.canvas.set_source(&state.gpu, texture);
                        }
                    }
                }
                // Reported, not fatal. This arm now also carries the GPU's
                // refusal of a texture it cannot make — which used to be an
                // uncaptured wgpu error, i.e. a panic, i.e. under
                // `panic = "abort"` the death of every open document's unsaved
                // work.
                Err(e) => tracing::error!("this document could not be presented: {e}"),
            }
        }
        if have_document {
            camera.image_size = Vec2::new(
                state.presenter.size().0 as f32,
                state.presenter.size().1 as f32,
            );
            state.canvas.update_camera(&state.gpu, &camera);
        }

        // ---- the selection, which is in no texture ----
        //
        // The canvas draws the document's composite and nothing else, so a
        // marquee used to change the document and not one pixel of the picture.
        // The ants are screen-space geometry over the top of it: traced from
        // the selection mask, projected through the same camera a click is
        // routed against, and cut into dashes whose phase is a pure function of
        // the clock.
        let ants = self
            .editor
            .active()
            .map(|doc| {
                // W3-A: gated on View ▸ Selection Edges by the chrome.
                let geometry = self.chrome.selection_ants(
                    &mut state.outline,
                    doc,
                    self.started.elapsed().as_secs_f64(),
                    &Default::default(),
                );
                ants_segments(&geometry)
            })
            .unwrap_or_default();
        // Card 039: a tab switch must not leave a stale preview lens on the
        // tab the user left.
        self.editor.clear_stale_previews();

        // ---- the text session's caret and selection, also in no texture ----
        //
        // Card 030: shaped from the document's own text layer (the draft rides
        // it), so the caret agrees with rendering after transform and zoom by
        // construction. Same screen mapping as a click route; the caret bar is
        // also the IME cursor window's anchor.
        let mut overlay_segments = ants;
        let text_geometry = self.pointer.text_overlay_geometry(&self.editor);
        let caret_screen = {
            if let (Some(doc), Some(bar)) = (
                self.editor.active(),
                text_geometry.iter().find(|segment| {
                    segment.kind == crate::tool_input::TextOverlayKind::Caret
                        && (segment.b - segment.a).length() > 0.5
                }),
            ) {
                let viewport = crate::tool_input::canvas_viewport(&doc.camera);
                let camera = crate::tool_input::canvas_camera_of(&doc.camera);
                let a = crate::interaction_geometry::document_to_screen(&camera, &viewport, bar.a);
                let b = crate::interaction_geometry::document_to_screen(&camera, &viewport, bar.b);
                Some((a, b))
            } else {
                None
            }
        };
        {
            let (viewport, camera) = self
                .editor
                .active()
                .map(|doc| {
                    (
                        crate::tool_input::canvas_viewport(&doc.camera),
                        crate::tool_input::canvas_camera_of(&doc.camera),
                    )
                })
                .unwrap_or_default();
            for segment in &text_geometry {
                let (a, b) = (
                    crate::interaction_geometry::document_to_screen(&camera, &viewport, segment.a),
                    crate::interaction_geometry::document_to_screen(&camera, &viewport, segment.b),
                );
                overlay_segments.push(render::Segment {
                    a,
                    b,
                    width_px: if segment.kind == crate::tool_input::TextOverlayKind::Caret {
                        1.5
                    } else {
                        1.0
                    },
                    color: match segment.kind {
                        crate::tool_input::TextOverlayKind::Caret => [0.1, 0.5, 1.0, 0.95],
                        crate::tool_input::TextOverlayKind::Selection => [0.1, 0.5, 1.0, 0.45],
                        crate::tool_input::TextOverlayKind::BoxFrame => [0.1, 0.5, 1.0, 0.3],
                    },
                });
            }
        }
        // Card 029/030: the IME composition window sits at the shaped caret,
        // mapped through the same camera — the OS popup lands exactly where
        // the text will appear, whatever the zoom.
        let ime_wanted = caret_screen.is_some();
        if self.ime_allowed_last != ime_wanted {
            state.window.set_ime_allowed(ime_wanted);
            self.ime_allowed_last = ime_wanted;
        }
        if let Some((a, b)) = caret_screen {
            let position = winit::dpi::PhysicalPosition::new(a.x.min(b.x) as f64, a.y as f64);
            let size = winit::dpi::PhysicalSize::new(1.0f64, (b.y - a.y).abs().max(1.0) as f64);
            state.window.set_ime_cursor_area(position, size);
        }
        // Card 039: a stable pivot marker for a live transform session — a
        // small cross at the recorded pivot, screen-constant in size, drawn
        // through the same camera as the handles.
        if let (Some(doc), Some((geometry_doc, geometry))) =
            (self.editor.active(), self.pointer.live_geometry())
        {
            // W4-A: only a transform has a pivot; the other sessions
            // (crop, marquee, lasso, path, slices) are painted by the chrome.
            if let (true, tools::SessionGeometry::Transform { state, .. }) =
                (geometry_doc == doc.id(), &geometry)
            {
                let viewport = crate::tool_input::canvas_viewport(&doc.camera);
                let camera = crate::tool_input::canvas_camera_of(&doc.camera);
                let pivot = crate::interaction_geometry::document_to_screen(
                    &camera,
                    &viewport,
                    state.pivot,
                );
                let arm = 5.0;
                for (a, b) in [
                    (
                        Vec2::new(pivot.x - arm, pivot.y),
                        Vec2::new(pivot.x + arm, pivot.y),
                    ),
                    (
                        Vec2::new(pivot.x, pivot.y - arm),
                        Vec2::new(pivot.x, pivot.y + arm),
                    ),
                ] {
                    overlay_segments.push(render::Segment {
                        a,
                        b,
                        width_px: 1.0,
                        color: [1.0, 0.4, 0.1, 0.9],
                    });
                }
            }
        }
        let has_ants = !overlay_segments.is_empty();
        state.overlay.set_viewport(
            &state.gpu,
            Vec2::new(
                state.surface_config.width as f32,
                state.surface_config.height as f32,
            ),
        );
        state.overlay.set_segments(&state.gpu, &overlay_segments);

        // ---- chrome ----
        let raw_input = state.egui_state.take_egui_input(&state.window);
        let (full_output, chrome_output) = {
            let chrome = &mut self.chrome;
            let editor = &mut self.editor;
            let mut captured = crate::chrome::ChromeOutput::default();
            let full = state.egui_ctx.run(raw_input, |ctx| {
                captured = chrome.ui(ctx, editor);
            });
            (full, captured)
        };
        // The panels moved (Tab, a screen mode, a dock resized, the first
        // layout): this frame was rendered into the old canvas area, so draw
        // one more into the new one rather than wait for the next input.
        if have_document && self.chrome.canvas_area_px() != area_used {
            state.window.request_redraw();
        }
        state
            .egui_state
            .handle_platform_output(&state.window, full_output.platform_output);

        let paint_jobs = state
            .egui_ctx
            .tessellate(full_output.shapes, full_output.pixels_per_point);
        let screen_descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [state.surface_config.width, state.surface_config.height],
            pixels_per_point: state.egui_ctx.pixels_per_point(),
        };

        let mut encoder =
            state
                .gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("frame"),
                });
        if have_document {
            // Into the canvas area only: the document is never drawn under a
            // dock (the pass still clears the whole surface to the backdrop).
            state.canvas.render_in(
                &mut encoder,
                &view,
                (state.surface_config.width, state.surface_config.height),
            );
            // Over the image, under the chrome — a panel must cover the ants,
            // not the other way round.
            state.overlay.render(&mut encoder, &view);
        } else {
            clear(
                &mut encoder,
                &view,
                state.theme,
                state.surface_config.format,
            );
        }

        for (id, delta) in &full_output.textures_delta.set {
            state
                .egui_renderer
                .update_texture(&state.gpu.device, &state.gpu.queue, *id, delta);
        }
        state.egui_renderer.update_buffers(
            &state.gpu.device,
            &state.gpu.queue,
            &mut encoder,
            &paint_jobs,
            &screen_descriptor,
        );
        {
            let rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("egui"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // MUST be Load: `Operations::default()` clears, which
                        // would wipe the canvas pass that just drew the image.
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &state.egui_depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            let mut rpass = rpass.forget_lifetime();
            state
                .egui_renderer
                .render(&mut rpass, &paint_jobs, &screen_descriptor);
        }
        for id in &full_output.textures_delta.free {
            state.egui_renderer.free_texture(id);
        }
        state.gpu.queue.submit(std::iter::once(encoder.finish()));

        // A literal GUI screenshot (`--shot`): read the just-rendered surface
        // back to the CPU and write it as a PNG before presenting. Queued work
        // has retired (the readback submits and polls with Wait), so the bytes
        // are the frame the user is about to see; `shot_taken` is set below,
        // once the borrow of `state` has ended, so `about_to_wait` clears the
        // window for us.
        let mut captured = false;
        if shot_requested {
            captured = capture_shot(&state.gpu, &frame.texture, shot_target.as_deref());
        }

        frame.present();
        self.shot_taken |= captured;

        let delay = full_output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map(|v| v.repaint_delay)
            .unwrap_or(Duration::ZERO);
        // Ants that are on screen have to keep crawling: nothing else in the
        // frame is changing, so without this the chrome's own repaint delay
        // (which is `Duration::MAX` for an idle window) would freeze them.
        let delay = if has_ants {
            delay.min(ANTS_FRAME)
        } else {
            delay
        };
        self.repaint_at = Instant::now().checked_add(delay);

        self.apply_chrome(chrome_output);
        self.refresh_title();
    }

    /// Perform whatever the chrome asked for this frame.
    ///
    /// # Order matters
    ///
    /// The selection lands **before** the actions. A frame can carry both — the
    /// user clicks a layer row and then picks Layer ▸ New Layer — and the click
    /// happened first, so it must be applied first. Doing it the other way
    /// round is what made a menu-invoked New Layer create the layer and then
    /// immediately point the cursor back at the previously active one; see
    /// `a_new_layer_stays_active_when_the_menu_creates_it`.
    fn apply_chrome(&mut self, output: crate::chrome::ChromeOutput) {
        self.adopt_ruler_unit();
        if let Some((layers, active)) = output.select_layers {
            self.editor.set_layer_selection(layers, active);
        } else if let Some(id) = output.select_layer {
            self.editor.set_active_layer(id);
        }
        // Card 007: the Properties Layer/Mask focus lands as shell-owned
        // state; tools read it back through `Editor::edit_target`.
        // Card 026: double-clicking a text row enters that layer.
        if let Some(layer) = output.enter_text_layer {
            self.pointer.enter_text_session(&mut self.editor, layer);
        }
        // W10-B: a Glyphs panel pick goes to the live session's caret, or to
        // the end of the layer's text when nobody is typing.
        for (layer, text) in &output.insert_glyphs {
            let said = crate::menu_bridge::glyph_insert::insert_glyph(
                &mut self.pointer,
                &mut self.editor,
                *layer,
                text,
            );
            self.editor.set_status(said.unwrap_or_else(|e| e));
        }
        // W13-L: the timeline's playhead moved; the canvas follows, with no
        // history step.
        if let Some(t_ms) = output.seek_timeline {
            self.editor.seek_timeline(t_ms);
        }
        if let Some(kind) = output.edit_target {
            self.editor.set_edit_target_kind(kind);
        }
        if let Some(depth) = output.history_jump {
            let moved = self.editor.jump_history(depth);
            if moved > 0 {
                self.editor
                    .set_status(format!("Stepped {moved} place(s) in history"));
            }
        }
        // W4-G: an options-bar confirm (the Ruler's Straighten Layer) is
        // Enter by another door: the same commit, the same preview settle
        // and the same un-publish of the consumed geometry.
        if output.confirm_tool {
            self.chrome
                .confirm_tool(&mut self.pointer, &mut self.editor);
        }
        for action in output.actions {
            self.perform(action);
        }
        for transport in output.actions_transport {
            match transport {
                crate::chrome::ActionsTransport::StartRecording => {
                    self.editor.start_recording();
                    self.last_recording = None;
                    self.editor.set_status("Recording actions");
                }
                crate::chrome::ActionsTransport::StopRecording => {
                    self.last_recording = self.editor.stop_recording();
                    self.editor.set_status("Stopped recording");
                }
                crate::chrome::ActionsTransport::ReplayRecording => {
                    // The capture the panel replay button uses: the last
                    // stopped recording, held by the shell between stop and
                    // replay.
                    if let Some(recording) = &self.last_recording {
                        let applied = self.editor.replay(recording);
                        self.editor
                            .set_status(format!("Replayed {applied} step(s)"));
                    } else {
                        self.editor.set_status("Nothing to replay: record first");
                    }
                }
            }
        }
        self.editor.set_paint_channel(output.paint_channel);
        if let Some(index) = output.activate {
            if let Err(e) = self.editor.activate(index) {
                self.editor.set_status(e.to_string());
            }
        }
        // W5-C: Edit > Transform > Scale/Rotate/... and Transform Selection
        // arrive as a tool pick carrying the options-bar choice. The choice
        // lands on the options bar, and a Free Transform pick parks a
        // session request like Ctrl+T does, so the handles are up before any
        // click and a commit hands the palette back to the tool it came from.
        if let Some((tool, key, index)) = output.tool_choice {
            self.chrome.set_tool_choice(tool, &key, index);
            if tool == tools::ToolId::FreeTransform {
                crate::tool_input::request_free_transform(self.editor.tool());
            }
        }
        if let Some(tool) = output.select_tool {
            // Card 025 / W5-D: switching tools ends a live text session
            // through its CONFIRM route, so the draft never strands on the
            // layer — clicking another tool is how a Photopea user finishes
            // typing, and cancelling here deleted the text they just typed.
            if self.pointer.is_text_editing() {
                self.pointer
                    .text_edit(&mut self.editor, tools::TextEdit::Confirm);
            }
            self.editor.set_tool(tool);
        }
        for command in output.commands {
            self.editor.apply_command(command);
        }
        // The Filter, Select, Adjustments and merge items. They cannot be a
        // `Command` built during enablement — the pixels have to be hashed into
        // the tile store first, and the selection is a document field with no
        // command behind it — so the bridge names the operation and performs it
        // here, once, with `&mut Editor`. Both halves of the answer reach the
        // status bar: `perform` sets it either way, so an operation that
        // refused says why instead of looking like it worked.
        for action in output.menu {
            // Card 025: a menu pick is a deliberate gesture — a live text
            // session confirms first (its draft lands as one history entry),
            // never silently stranded by a menu-driven tool switch.
            self.confirm_live_text_session();
            if let Err(reason) = crate::menu_bridge::perform(action, &mut self.editor) {
                tracing::warn!("{}: {reason}", action.label());
            }
        }
        // W5-C: a Ctrl+T (or Transform-menu) session and a ticked Show
        // Transform Controls box begin here, in the frame that asked for
        // them, and publish their handles at once: the keyboard route never
        // produces a pointer sample, so waiting for one left the canvas bare.
        self.begin_pending_tool_session();
        // The Properties panel's adjustment sliders and the Text panel's
        // fields. Their own path rather than `commands` because a drag emits
        // one per frame and `apply_kind_edit` folds the run into a single undo
        // step; see `KindEdit::gesture`.
        for edit in output.layer_kind {
            self.editor.apply_kind_edit(edit);
        }
        // A modal dialog confirmed with a value no existing channel carries.
        // Each variant is applied by the piece that owns its effect; a
        // variant with no application path yet says so rather than
        // disappearing — each remaining one is consumed by its own P0 task as
        // its dialog gets wired.
        if let Some(action) = output.dialog {
            match action {
                DialogAction::NewDocument(spec) => {
                    let background = match spec.background {
                        ui::dialogs::BackgroundContents::Transparent => {
                            crate::import::BlankBackground::Transparent
                        }
                        ui::dialogs::BackgroundContents::White => {
                            crate::import::BlankBackground::Solid {
                                rgba8: [255, 255, 255, 255],
                                depth: spec.bit_depth,
                            }
                        }
                        ui::dialogs::BackgroundContents::Black => {
                            crate::import::BlankBackground::Solid {
                                rgba8: [0, 0, 0, 255],
                                depth: spec.bit_depth,
                            }
                        }
                        ui::dialogs::BackgroundContents::Custom(rgba) => {
                            crate::import::BlankBackground::Solid {
                                rgba8: crate::menu_bridge::rgba8_of(rgba),
                                depth: spec.bit_depth,
                            }
                        }
                    };
                    if let Err(e) = self.editor.new_document_with(
                        spec.width,
                        spec.height,
                        &spec.title,
                        background,
                    ) {
                        self.editor.set_status(e.to_string());
                    } else if let Some(doc) = self.editor.active_mut() {
                        // W4-F: a transparent background has no tiles to
                        // carry the depth, so the spec's depth is recorded.
                        doc.set_initial_bit_depth(spec.bit_depth);
                        // W10-J: the Artboard box makes the canvas an
                        // artboard of the chosen background.
                        if spec.artboard {
                            let background = spec.background.fill().unwrap_or([0.0; 4]);
                            if let Err(e) =
                                crate::menu_bridge::artboard_doc::make_canvas_an_artboard(
                                    &mut self.editor,
                                    background,
                                )
                            {
                                self.editor.set_status(e);
                            }
                        }
                    }
                }
                DialogAction::Export(job) => {
                    // Photopea writes downloads straight away; this is the
                    // desktop equivalent — the folder picker asks once, then
                    // every enabled entry lands in it. The composite and the
                    // encodes run on a worker (W2-G); the status line reports
                    // the outcome when it lands.
                    if let Some(dir) = self.editor.pick_export_folder() {
                        // W4-H: the settings File > Export > Slices reuses —
                        // remembered only once the job is handed on.
                        ui::dialogs::export_as::remember_exported_job(&job);
                        self.editor.request_export(*job, dir);
                    }
                    // Cancelled at the folder picker: nothing written, nothing
                    // to report.
                }
                DialogAction::ResizeImage(spec) => {
                    // A spec with `resample: None` changes print metadata
                    // only, and nothing here stores a ppi — say so rather
                    // than silently doing nothing.
                    match (spec.resample, self.editor.active_mut()) {
                        (None, _) => {
                            self.editor.set_status(
                                "Print resolution is not stored yet — the pixels were left unchanged",
                            );
                        }
                        (Some(_), Some(doc)) => match doc.resample_command(&spec) {
                            Ok(command) => self.editor.apply_command(command),
                            Err(e) => {
                                tracing::warn!("image size failed: {e}");
                                self.editor.set_status(format!("Image Size failed: {e}"));
                            }
                        },
                        (Some(_), None) => {}
                    }
                }
                DialogAction::ResizeCanvas(spec) => {
                    if let Some(doc) = self.editor.active_mut() {
                        match doc.canvas_size_command(&spec) {
                            Ok(command) => self.editor.apply_command(command),
                            Err(e) => {
                                tracing::warn!("canvas size failed: {e}");
                                self.editor.set_status(format!("Canvas Size failed: {e}"));
                            }
                        }
                    }
                }
                DialogAction::Fill(spec) => {
                    if let Err(reason) =
                        crate::menu_bridge::fill_selection_with(&mut self.editor, &spec)
                    {
                        self.editor.set_status(reason);
                    }
                }
                DialogAction::RefineMask(spec) => {
                    if let Err(reason) =
                        crate::menu_bridge::refine_mask_with(&mut self.editor, &spec)
                    {
                        self.editor.set_status(reason);
                    }
                }
                DialogAction::Defringe(spec) => {
                    if let Err(reason) = crate::menu_bridge::defringe_with(&mut self.editor, &spec)
                    {
                        self.editor.set_status(reason);
                    }
                }
                DialogAction::Stroke(spec) => {
                    if let Err(reason) =
                        crate::menu_bridge::stroke_selection_with(&mut self.editor, &spec)
                    {
                        self.editor.set_status(reason);
                    }
                }
                DialogAction::RotateCanvas(degrees) => {
                    // Right angles take the exact index-copy path the fixed
                    // menu items use, so a 90° through the dialog is
                    // byte-identical to Image ▸ Rotation ▸ 90° Clockwise.
                    let turns = degrees.rem_euclid(360.0);
                    let orthogonal = [
                        (90.0, Some(ui::menu::CanvasRotation::Deg90Cw)),
                        (180.0, Some(ui::menu::CanvasRotation::Deg180)),
                        (270.0, Some(ui::menu::CanvasRotation::Deg90Ccw)),
                    ]
                    .into_iter()
                    .find(|(angle, _)| (turns - angle).abs() < 1e-9);
                    match orthogonal {
                        Some((_, fixed)) => {
                            self.confirm_live_text_session();
                            if let Err(reason) = crate::menu_bridge::perform(
                                ui::menu::MenuAction::RotateCanvas(fixed.unwrap()),
                                &mut self.editor,
                            ) {
                                tracing::warn!("rotate failed: {reason}");
                            }
                        }
                        None => {
                            if (turns).abs() < 1e-9 {
                                self.editor.set_status("The canvas is already at 0°");
                            } else if let Some(doc) = self.editor.active_mut() {
                                match doc.rotate_canvas_arbitrary(degrees) {
                                    Ok(command) => self.editor.apply_command(command),
                                    Err(e) => {
                                        tracing::warn!("rotate failed: {e}");
                                        self.editor.set_status(format!("Rotate failed: {e}"));
                                    }
                                }
                            }
                        }
                    }
                }
                DialogAction::RunFilter(invocation) => {
                    match crate::menu_bridge::run_filter_invocation(&mut self.editor, &invocation) {
                        Ok(message) => self.editor.set_status(message),
                        Err(reason) => {
                            tracing::warn!("filter failed: {reason}");
                            self.editor.set_status(reason);
                        }
                    }
                }
                other => {
                    let label = other.label();
                    tracing::warn!("dialog confirmed but not applied yet: {label}");
                    self.editor
                        .set_status(format!("{label} — applying it is not wired yet"));
                }
            }
        }
        // A control the bridge could not answer says so. Silence here is what
        // hid an entire inert Properties panel through a whole review: an
        // intent that reached nobody left no trace at all.
        for intent in &output.unrouted {
            let message = crate::menu_bridge::unrouted_message(intent);
            tracing::warn!("{message}");
            self.editor.set_status(message);
        }
        if let Some(rgba) = output.set_foreground {
            self.editor.set_foreground(rgba);
        }
        if let Some(gradient) = output.set_gradient_ramp {
            self.editor.set_gradient_ramp(gradient);
        }
        if let Some(rgba) = output.set_background {
            self.editor.set_background(rgba);
        }
        // The brush belongs to the editor, so an options-bar edit lands here.
        // Without this the slider moved and nothing else did, while `[` and `]`
        // moved the editor's brush and the slider stayed put — two numbers for
        // one setting, disagreeing in the same window.
        if let Some(brush) = output.set_brush {
            self.editor.set_brush(brush);
        }
        // The camera is the document's, so the Navigator's pan and the status
        // bar's zoom field land here rather than in the workspace. Before this
        // they were workspace-local writes nothing read: dragging the Navigator
        // moved the box inside the Navigator and the image stayed still.
        if let Some((x, y)) = output.set_view_center {
            if let Some(doc) = self.editor.active_mut() {
                if x.is_finite() && y.is_finite() {
                    doc.camera.center = Vec2::new(x, y);
                }
            }
        }
        if let Some(zoom) = output.set_zoom {
            if let Some(doc) = self.editor.active_mut() {
                if zoom.is_finite() && zoom > 0.0 {
                    doc.camera.zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
                }
            }
        }
        if let Some(prefs) = output.preferences {
            self.editor.set_preferences(prefs);
        }
        // The Preferences dialog's confirmed schema maps onto the app's own
        // preferences, keymap page included (`Editor::apply_ui_preferences`);
        // the Units preference then reaches the rulers through the workspace.
        if let Some(prefs) = output.set_ui_preferences {
            self.editor.apply_ui_preferences(&prefs);
            self.sync_ruler_unit();
        }
        if output.reset_keymap {
            self.editor.reset_keymap();
        }
        if let Some(chord) = output.unbind {
            self.editor.unbind_chord(chord);
        }
        if let Some(rebind) = output.rebind {
            if rebind.force {
                self.editor.force_rebind(rebind.chord, rebind.action);
            } else {
                // A refusal parks the conflict on the editor; the shortcut
                // editor renders it as the "…is already Save. Replace?" prompt.
                let _ = self.editor.rebind(rebind.chord, rebind.action);
            }
        }
        if output.dismiss_conflict {
            self.editor.clear_conflict();
        }
        if let Some(path) = output.open_recent {
            self.editor.open_paths(&[path]);
        }
        if let Some(index) = output.close {
            if let Err(ActionError::Failed { action, reason }) = self.editor.close_document(index) {
                let title = format!("{} failed", action.label());
                self.editor.report_error(&title, &reason);
            }
        }
        if let Some((from, to)) = output.move_document {
            self.editor.move_document(from, to);
        }
        self.sync_marker();
    }

    /// Who has a claim on the keyboard right now.
    fn keyboard_owner(&self) -> KeyboardOwner {
        KeyboardOwner {
            egui_text_focus: self
                .state
                .as_ref()
                .is_some_and(|s| s.egui_ctx.wants_keyboard_input()),
            // The guard `Chrome::capturing` was written for, and which nothing
            // ever called: while a chord is being recorded the shell must not
            // *also* perform it.
            recording_shortcut: self.chrome.is_recording(),
        }
    }

    /// Hand one key press to a text run open on the canvas, if there is one.
    ///
    /// Reports whether it was consumed. The rules are the narrow ones: only
    /// while [`ToolPointer::is_text_editing`], only when egui does not hold the
    /// keyboard, and never with Ctrl or Alt held (except AltGr, Ctrl+Alt with
    /// a character, which types) — so Ctrl+S still saves while the user is
    /// typing. Ctrl+Enter and Escape both confirm the run (the draft commits
    /// as one history entry, W5-D), plain Enter inserts a line break (card
    /// 031) — through the text-session routes (card 025).
    fn route_text_key(&mut self, owner: KeyboardOwner, logical: &winit::keyboard::Key) -> bool {
        use winit::keyboard::Key as WKey;
        if owner.egui_text_focus || owner.recording_shortcut || !self.pointer.is_text_editing() {
            return false;
        }
        // Card 028: while a session is live, Ctrl+A belongs to the text
        // (select-all); every other Ctrl/Alt/Super chord still reaches the
        // keymap — Ctrl+S keeps saving mid-typing.
        // W5-D: Ctrl+Alt together is how Windows reports AltGr, the key that
        // types `@`, `€` and `{` on non-US layouts — a character arriving
        // with both held is text, not a chord. Round 2: but Ctrl+Alt is also
        // a real chord family (Ctrl+Alt+Z Undo, Ctrl+Alt+J Duplicate Layer,
        // Ctrl+Alt+Shift+S Export), whose logical key is the plain letter.
        // AltGr never yields an ASCII letter or digit, so those stay chords,
        // and so does any Ctrl+Alt chord the keymap actually binds.
        let altgr_character = self.modifiers.control_key()
            && self.modifiers.alt_key()
            && !self.modifiers.super_key()
            && match logical {
                WKey::Character(s) => {
                    !s.chars().any(|c| c.is_ascii_alphanumeric())
                        && chord_from_key(logical, self.modifiers)
                            .is_none_or(|chord| self.editor.keymap().resolve_any(&chord).is_none())
                }
                _ => false,
            };
        if !altgr_character && (self.modifiers.control_key() || self.modifiers.super_key()) {
            if let winit::keyboard::Key::Character(c) = logical {
                // Card 028: Ctrl+C/X/V/A belong to the text while a session
                // is live; every other chord still reaches the keymap —
                // Ctrl+S keeps saving mid-typing.
                let key = c.to_lowercase().to_string();
                let clipboard_key = match key.as_str() {
                    "c" => (true, false, false, false),
                    "x" => (false, true, false, false),
                    "v" => (false, false, true, false),
                    "a" => (false, false, false, true),
                    _ => (false, false, false, false),
                };
                if clipboard_key != (false, false, false, false) {
                    let (copy, cut, paste, select_all) = clipboard_key;
                    return self.route_text_clipboard_key(copy, cut, paste, select_all);
                }
            }
        }
        // Card 031: Ctrl+Enter confirms the session — the only Ctrl chord
        // admitted past this guard (alongside card 028's clipboard keys in
        // the branch above); every other Ctrl/Alt/Super chord still reaches
        // the keymap — Ctrl+S keeps saving mid-typing.
        let enter_confirm_chord =
            self.modifiers.control_key() && matches!(logical, WKey::Named(NamedKey::Enter));
        if !enter_confirm_chord
            && !altgr_character
            && (self.modifiers.control_key()
                || self.modifiers.alt_key()
                || self.modifiers.super_key())
        {
            return false;
        }
        // Card 029: while an IME composition is live, the platform owns the
        // text — characters and editing keys alike arrive through
        // `Ime::Preedit`/`Ime::Commit`, and acting on them here would double
        // or lose the text (Backspace would strand the preedit as plain text;
        // Enter would end the session before the commit lands). Movement
        // keys are fine: their arms commit the preedit first, matching how
        // real IMEs behave on arrow keys.
        if self.pointer.text_composing(&self.editor)
            && matches!(
                logical,
                WKey::Character(_)
                    | WKey::Named(NamedKey::Space)
                    | WKey::Named(NamedKey::Backspace)
                    | WKey::Named(NamedKey::Delete)
                    | WKey::Named(NamedKey::Enter)
                    | WKey::Named(NamedKey::Escape)
            )
        {
            return true;
        }
        let edit = match logical {
            // The platform's own text for the key, so a shifted letter arrives
            // as a capital and a dead-key composition arrives composed. This is
            // what `Chord` cannot carry: it normalises case on purpose.
            WKey::Character(text) => tools::TextEdit::Insert(text.as_str()),
            WKey::Named(NamedKey::Space) => tools::TextEdit::Insert(" "),
            WKey::Named(NamedKey::Backspace) => tools::TextEdit::Backspace,
            WKey::Named(NamedKey::Delete) => tools::TextEdit::DeleteForward,
            // Card 031: Enter inserts a line break (paragraph text, card
            // 023); Ctrl+Enter confirms the session as one history entry for
            // the whole run; Escape confirms too (W5-D). None falls through
            // to the keymap while a
            // run is open — a composing IME is consumed earlier (card 029),
            // so Enter during composition belongs to the platform.
            WKey::Named(NamedKey::Enter) => {
                if self.modifiers.control_key() {
                    tools::TextEdit::Confirm
                } else {
                    tools::TextEdit::Insert("\n")
                }
            }
            // W5-D: Escape commits the run, as Photopea does — the typed
            // text is kept, never thrown away by a reflexive Escape.
            WKey::Named(NamedKey::Escape) => tools::TextEdit::Confirm,
            // Card 027: movement keys, with Shift extending the selection.
            WKey::Named(NamedKey::ArrowLeft) => tools::TextEdit::CaretStep {
                back: true,
                extend: self.modifiers.shift_key(),
            },
            WKey::Named(NamedKey::ArrowRight) => tools::TextEdit::CaretStep {
                back: false,
                extend: self.modifiers.shift_key(),
            },
            WKey::Named(NamedKey::Home) => tools::TextEdit::ParagraphEdge {
                end: false,
                extend: self.modifiers.shift_key(),
            },
            WKey::Named(NamedKey::End) => tools::TextEdit::ParagraphEdge {
                end: true,
                extend: self.modifiers.shift_key(),
            },
            _ => return false,
        };
        let outcome = self.pointer.text_edit(&mut self.editor, edit);
        if outcome.needs_repaint() {
            self.repaint_at = Some(Instant::now());
        }
        outcome.had_pending
    }

    /// Card 028: Ctrl+C copies the session's selected text to the OS
    /// clipboard, Ctrl+X copies then deletes the range, Ctrl+V pastes
    /// clipboard text over the selection (or at the caret), Ctrl+A selects
    /// the whole draft. Clipboard failures degrade to a no-op.
    fn route_text_clipboard_key(
        &mut self,
        copy: bool,
        cut: bool,
        paste: bool,
        select_all: bool,
    ) -> bool {
        use arboard::Clipboard;
        if select_all {
            let outcome = self
                .pointer
                .text_edit(&mut self.editor, tools::TextEdit::SelectAll);
            if outcome.needs_repaint() {
                self.repaint_at = Some(Instant::now());
            }
            return outcome.had_pending;
        }
        if copy || cut {
            if let Some(text) = self.pointer.text_selection_text(&self.editor) {
                // The same process-wide lock the image clipboard holds (card
                // 052): every clipboard access in the process serializes.
                crate::clipboard::with_os_clipboard_lock(|| {
                    if let Ok(mut clip) = Clipboard::new() {
                        let _ = clip.set_text(text);
                    }
                });
            }
        }
        if cut {
            // No selection means nothing to cut — the chord is consumed but
            // must not degrade into deleting one character.
            if self.pointer.text_selection_text(&self.editor).is_none() {
                return true;
            }
            let outcome = self
                .pointer
                .text_edit(&mut self.editor, tools::TextEdit::DeleteForward);
            if outcome.needs_repaint() {
                self.repaint_at = Some(Instant::now());
            }
            return outcome.had_pending;
        }
        if paste {
            // Card 028: an empty (or image-only) clipboard is consumed while
            // typing — falling through would let a user-bound paste chord
            // create a layer mid-typing.
            let pasted = crate::clipboard::with_os_clipboard_lock(|| {
                Clipboard::new()
                    .and_then(|mut clip| clip.get_text())
                    .unwrap_or_default()
            });
            if pasted.is_empty() {
                return true;
            }
            let outcome = self
                .pointer
                .text_edit(&mut self.editor, tools::TextEdit::PasteText(&pasted));
            if outcome.needs_repaint() {
                self.repaint_at = Some(Instant::now());
            }
            return outcome.had_pending;
        }
        false
    }

    /// Card 029: IME events route into the live text session. A non-empty
    /// preedit begins/replaces the composition; an empty one withdraws it; a
    /// commit turns the preedit into draft text plus the committed string.
    /// Without a session the events are ignored (the IME is only enabled
    /// while text editing, but platforms can be loose about ordering).
    /// Card 049: the dropped-file routing seam — the one function the
    /// window event calls, and the one tests drive.
    ///
    /// A native project (an `.rstudio` package — by name or by manifest —
    /// plus `.psd`/`.psb`) OPENS; a library file (W11-D: `.abr` `.asl`
    /// `.pat` `.grd` `.csh` `.aco` `.ase` `.icc` fonts `.cube`) reaches its
    /// importer through [`crate::editor::Editor::open_any`]; anything else PLACES into the active
    /// composition when one is open, and opens when none is. Multiple
    /// dropped files are processed in arrival order, each as its own
    /// documented per-file step: every failure is collected and reported in
    /// the status after the batch, so nothing blocks and no later success
    /// buries an earlier failure.
    pub fn on_dropped_files(&mut self, paths: &[std::path::PathBuf]) {
        let mut failures: Vec<String> = Vec::new();
        for path in paths {
            // The project half uses the Editor's own predicate: it also
            // recognizes a manifest-bearing package DIRECTORY without the
            // extension.
            let opens = crate::editor::Editor::is_project_path(path)
                || path
                    .extension()
                    .map(|e| e.eq_ignore_ascii_case("psd") || e.eq_ignore_ascii_case("psb"))
                    .unwrap_or(false);
            // W11-D: a library file (brushes, styles, patterns, gradients,
            // shapes, swatches, a profile, a font, a `.cube`) goes to its
            // importer whether or not a document is open - it is never
            // placed as a picture.
            let library = crate::editor::Editor::is_library_file(path);
            if opens || library || self.editor.active().is_none() {
                // open_any answers the failure instead of raising the
                // blocking modal open_paths routes through.
                if let Err(e) = self.editor.open_any(path) {
                    failures.push(format!(
                        "{}: {e}",
                        path.file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default()
                    ));
                }
            } else if let Err(e) = self.editor.place_path(path, false) {
                failures.push(format!(
                    "{}: {e}",
                    path.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default()
                ));
            }
        }
        if !failures.is_empty() {
            self.editor
                .set_status(format!("Drop failed ({})", failures.join("; ")));
        }
        self.sync_marker();
        self.repaint_at = Some(Instant::now());
    }

    fn on_ime(&mut self, ime: &winit::event::Ime) {
        use winit::event::Ime;
        match ime {
            Ime::Preedit(text, _) => {
                let edit = if text.is_empty() {
                    tools::TextEdit::ClearComposition
                } else {
                    tools::TextEdit::SetComposition(text.as_str())
                };
                let outcome = self.pointer.text_edit(&mut self.editor, edit);
                if outcome.needs_repaint() {
                    self.repaint_at = Some(Instant::now());
                }
            }
            Ime::Commit(text) => {
                let outcome = self
                    .pointer
                    .text_edit(&mut self.editor, tools::TextEdit::CommitIme(text.as_str()));
                if outcome.needs_repaint() {
                    self.repaint_at = Some(Instant::now());
                }
            }
            // The platform toggles the IME; the session state itself is
            // driven by preedit/commit events.
            Ime::Enabled | Ime::Disabled => {}
        }
    }

    /// Route one key press. Separated from `window_event` so it can be driven
    /// without an event loop, which is how [`route_key`]'s decisions are shown
    /// to reach [`Editor::dispatch`].
    fn on_key(
        &mut self,
        owner: KeyboardOwner,
        logical: &winit::keyboard::Key,
        state: ElementState,
        repeat: bool,
    ) {
        // A modal dialog owns the keyboard while it is open: Escape and Enter
        // are the dialog's, delivered through egui, and the keymap must not
        // act beside them — a chord fired under a modal would edit a document
        // the user cannot see.
        if self.chrome.dialog_open() {
            return;
        }
        // A Type-tool run open on the canvas owns the keyboard, the way a
        // focused egui field does. Without this the Type tool could create a
        // layer and never put a character in it: every letter would be a tool
        // shortcut and the space bar would grab the hand tool.
        if state == ElementState::Pressed && self.route_text_key(owner, logical) {
            return;
        }
        match route_key(owner, logical, state, repeat, self.modifiers) {
            KeyOutcome::Dispatch(chord) => {
                // Escape abandons whatever the pointer is in the middle of,
                // *then* means whatever the keymap says. Nothing binds it
                // today, and a stroke that could not be called off would be the
                // gap this route is here to close.
                if chord.key == Key::Escape {
                    self.abandon_gesture();
                }
                // Enter confirms the gesture the live tool is *holding*: the
                // crop box, the slice set, the free-transform quad. Those three
                // tools publish only from `Tool::commit`, so without this the
                // user drew a crop rectangle that could never become a crop.
                // Only when there is something to confirm — otherwise Enter
                // stays whatever the keymap says it is.
                if chord == Chord::plain(Key::Enter) {
                    let outcome = self.pointer.commit(&mut self.editor);
                    // Card 039: a commit ends the session — settle the
                    // preview NOW, or a keyboard undo right after Enter
                    // misrenders through the stale lens until the next
                    // cursor move heals it.
                    self.pointer.settle_preview(&mut self.editor);
                    // W4-A: and un-publish the committed crop box / slice
                    // set / pen path now, not on the next pointer sample.
                    let geometry = self.pointer.live_geometry();
                    self.chrome
                        .publish_tool_geometry(geometry, self.editor.active().map(|d| d.id()));
                    if outcome.needs_repaint() {
                        self.repaint_at = Some(Instant::now());
                    }
                    if outcome.had_pending {
                        return;
                    }
                }
                // W13-M: Photopea's mask-view keys, before the keymap — a
                // view toggle that works wherever the keyboard is the
                // canvas's, whether or not the Layers panel is showing.
                let has_mask = self.editor.active().is_some_and(|d| {
                    let doc = &d.document;
                    doc.active_layer()
                        .and_then(|id| doc.layers.get(id))
                        .is_some_and(|l| l.mask.is_some())
                });
                if let Some(mode) = mask_view_key(&chord, self.chrome.mask_view(), has_mask) {
                    self.chrome.set_mask_view(mode);
                    self.repaint_at = Some(Instant::now());
                    return;
                }
                match self.editor.keymap().resolve_any(&chord) {
                    // W13-M: Shift + a tool letter steps through the group
                    // (Photopea); the bare letter picks it and keeps the tool.
                    Some(Resolved::App(Action::SelectTool(key))) if chord.shift => {
                        if self.editor.select_tool_letter(key, true).is_some() {
                            self.repaint_at = Some(Instant::now());
                        }
                    }
                    // W13X-1: an arrow nudges through the shell's own pointer
                    // (a live gesture or text run refuses it): Shift is ten
                    // pixels, Alt moves a copy — made on the press only, a
                    // held key's repeats move the copy it made.
                    Some(Resolved::App(Action::Nudge(direction))) => {
                        let copy = chord.alt && !repeat;
                        match self
                            .pointer
                            .nudge(&mut self.editor, direction, chord.shift, copy)
                        {
                            Ok(0) => {}
                            Ok(_) => self.repaint_at = Some(Instant::now()),
                            Err(reason) => self.editor.set_status(reason),
                        }
                    }
                    Some(Resolved::App(action)) => self.perform(action),
                    Some(Resolved::Menu(action)) => self.perform_menu_chord(action),
                    // W11-F: Ctrl+Space and Alt+Space, unless the keymap
                    // binds them, hold the space bar's slot too — the tool
                    // they lend is the Zoom tool (`temporary_tool_for`).
                    None if chord.key == Key::Space && (chord.ctrl_or_cmd || chord.alt) => {
                        self.perform(Action::TemporaryHand)
                    }
                    None => {}
                }
                self.sync_temporary_tool();
            }
            KeyOutcome::ReleaseTemporaryHand => {
                self.editor.release_temporary_hand();
                self.sync_temporary_tool();
                self.repaint_at = Some(Instant::now());
            }
            KeyOutcome::Ignore => {}
        }
    }

    /// W11-F: the held modifiers changed. The tool a modifier lends
    /// (`temporary_tool_for`) follows at once, so releasing Ctrl or Alt gives
    /// the selected tool back.
    fn on_modifiers(&mut self, mods: ModifiersState) {
        self.modifiers = mods;
        self.sync_temporary_tool();
    }

    /// W11-F: lend the editor whatever tool the held keys now ask for.
    fn sync_temporary_tool(&mut self) {
        let lent = temporary_tool_for(
            self.editor.tool(),
            self.editor.temporary_hand(),
            modifiers_of(self.modifiers),
        );
        if self.editor.set_temporary_tool(lent) {
            self.repaint_at = Some(Instant::now());
        }
    }

    /// One wheel event over the canvas, `lines` in wheel notches (positive y
    /// is away from the user).
    ///
    /// Honours the Scroll-wheel-zooms preference and Photopea's modifiers
    /// ([`wheel_gesture`]): Alt inverts the preference, Ctrl pans sideways,
    /// Shift turns the vertical wheel horizontal.
    fn on_wheel(&mut self, lines: Vec2) {
        let gesture = wheel_gesture(
            lines,
            self.modifiers.control_key() || self.modifiers.super_key(),
            self.modifiers.alt_key(),
            self.modifiers.shift_key(),
            self.editor.preferences().scroll_wheel_zooms,
        );
        let anchor = self.cursor;
        if let Some(doc) = self.editor.active_mut() {
            match gesture {
                WheelGesture::Zoom(factor) => doc.camera.zoom_at(anchor, factor),
                WheelGesture::Pan(delta) => doc.camera.pan_screen(delta),
            }
        }
        self.repaint_at = Some(Instant::now());
    }

    /// A ruler unit the chrome's workspace took since the last look that the
    /// Units preference does not already hold came from View ▸ Rulers ▸
    /// <unit>: adopt it as the preference, so the status bar, the Info panel,
    /// the size dialogs and the Preferences dialog read what the rulers show,
    /// and a later Preferences OK re-applies it instead of reverting it.
    /// A change that only lands the preference the shell itself pushed
    /// ([`Shell::sync_ruler_unit`]) already agrees and is left alone.
    fn adopt_ruler_unit(&mut self) {
        let unit = self.chrome.workspace().canvas.unit;
        if unit != self.ruler_unit_seen {
            self.ruler_unit_seen = unit;
            self.editor.set_display_unit(unit);
        }
    }

    /// Push the Units preference into the chrome's workspace, where the rulers
    /// read it. Called at start-up and whenever the preferences are applied.
    fn sync_ruler_unit(&mut self) {
        let unit = self.editor.display_unit();
        if self.chrome.workspace().canvas.unit != unit {
            self.chrome.emit(ui::Intent::SetRulerUnit(unit));
        }
    }

    /// A chord only the menu bar paints: Ctrl+E Merge Down, Ctrl+A Select All,
    /// F7 the Layers panel. Posted into the chrome's workspace as
    /// `ui::Intent::Action`, which is the door a click on the menu item goes
    /// through — `Chrome::harvest` then decides whether the action opens a
    /// dialog or is performed, exactly as it would for the click. Before this
    /// the shell consulted only its own keymap, so every one of these chords
    /// was painted and dead.
    fn perform_menu_chord(&mut self, action: ui::MenuAction) {
        // Edit ▸ Keyboard Shortcuts' Ctrl+Alt+Shift+K rides this same door:
        // the chrome's dialog host answers the intent with the Preferences
        // dialog on its Keymap page, exactly as it answers the click.
        self.chrome.emit(ui::Intent::Action(action));
        self.repaint_at = Some(Instant::now());
    }

    /// Abandon whatever gesture is running: Escape, or the window losing focus.
    ///
    /// Reports whether there was one, and forgets the held button with it — a
    /// gesture the router still believes in refuses every later press as
    /// somebody else's, which is how a canvas goes permanently dead.
    /// Confirm a live text session, if one is open (card 025): the typed
    /// draft lands as one history entry before a menu-driven action —
    /// notably the Transform items' `set_tool` — can move the tool out from
    /// under it.
    fn confirm_live_text_session(&mut self) {
        if self.pointer.is_text_editing() {
            let outcome = self
                .pointer
                .text_edit(&mut self.editor, tools::TextEdit::Confirm);
            if outcome.needs_repaint() {
                self.repaint_at = Some(Instant::now());
            }
        }
    }

    fn abandon_gesture(&mut self) -> bool {
        // Card 025: a live text session is reconciled through its own cancel
        // route (restore-or-delete, history-free for entered layers) — the
        // bare Tool::cancel contract would strand the draft on the layer.
        if self.pointer.is_text_editing() {
            let outcome = self
                .pointer
                .text_edit(&mut self.editor, tools::TextEdit::Cancel);
            self.held = None;
            if outcome.needs_repaint() {
                self.repaint_at = Some(Instant::now());
            }
            return outcome.had_pending;
        }
        let cancelled = self.pointer.cancel(&mut self.editor);
        // XB: an abandoned gesture takes its W/H readout down with it, through
        // the same publisher the pointer route uses — Escape and focus loss
        // never produce a pointer sample, so nothing else would clear it.
        self.chrome
            .publish_tool_readout(self.pointer.live_readout());
        // W4-A: the same for the session's overlay (crop box, rubber band,
        // lasso, pen path, slices), which Escape must take down at once.
        let geometry = self.pointer.live_geometry();
        self.chrome
            .publish_tool_geometry(geometry, self.editor.active().map(|d| d.id()));
        if !cancelled {
            return false;
        }
        // Card 013: an abandoned gesture drops its preview with it.
        self.pointer.settle_preview(&mut self.editor);
        self.held = None;
        self.repaint_at = Some(Instant::now());
        true
    }

    /// Feed one pointer sample to [`ToolPointer`].
    ///
    /// Everything that decides *what happens* is one layer down; this is the
    /// translation from winit's shape to the router's, plus the two pieces of
    /// window state that go with it — which button is held, and whether the
    /// frame has to be drawn again.
    /// Supply the winit tablet pressure for subsequent pointer samples. A
    /// mouse path that never subscribes to tablet events stays full-pressure;
    /// a native tablet handler feeds real `0..=1` values here and the next
    /// stroke lands them on the brush. The value is clamped to `0..=1` and a
    /// non-finite reading falls back to full pressure, so a stale or bogus
    /// tablet sample can never veto a stroke.
    pub fn set_pen_pressure(&mut self, pressure: f32) {
        self.pen_pressure = if pressure.is_finite() {
            pressure.clamp(0.0, 1.0)
        } else {
            1.0
        };
    }

    /// Card 010: what the options bar holds for the effective tool, as the
    /// tool's own setting values. The conversion happens here, at the
    /// boundary — the UI crate's value types never reach a tool.
    fn tool_settings(&self) -> Vec<(String, tools::ToolSetting)> {
        self.chrome
            .tool_options(self.editor.effective_tool())
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
            .collect()
    }

    /// W5-C: begin whatever session must be on screen without a pointer
    /// sample (a parked Ctrl+T, a ticked Show Transform Controls) and, when
    /// one began or changed, publish its geometry through the production
    /// publisher and ask for a frame to draw it.
    fn begin_pending_tool_session(&mut self) {
        let (_, settings) = crate::tool_input::SnapPolicy::split_settings(&self.tool_settings());
        if self
            .pointer
            .begin_pending_session(&mut self.editor, &settings)
        {
            let geometry = self.pointer.live_geometry();
            self.chrome
                .publish_tool_geometry(geometry, self.editor.active().map(|doc| doc.id()));
            self.repaint_at = Some(Instant::now());
        }
    }

    /// W7-A: one winit touch/pen sample, routed exactly as the mouse is.
    /// The sample's pressure goes through [`Shell::set_pen_pressure`] before
    /// the pointer sample is built, so every stroke sample carries it; once
    /// the contact lifts the pointer returns to full (mouse) pressure.
    ///
    /// W8-A: a pen hovering in range is a hover move, routed as a mouse
    /// hover is; and a pen's position (hovering or in contact) is handed to
    /// the chrome as the brush ring's position. A finger's is not: a finger
    /// cannot hover, so its lift must not leave a ring behind.
    fn on_touch(
        &mut self,
        id: u64,
        phase: winit::event::TouchPhase,
        location: PhysicalPosition<f64>,
        force: Option<winit::event::Force>,
        over_panel: bool,
    ) {
        let pos = Vec2::new(location.x as f32, location.y as f32);
        let Some(sample) = self.pen.on_touch(id, phase, pos, force) else {
            return;
        };
        self.chrome.set_pen_hover(sample.pen.then_some(sample.pos));
        self.cursor = sample.pos;
        if sample.hover {
            let button = self.held.unwrap_or(PointerButton::Primary);
            self.on_pointer(PointerPhase::Move, button, over_panel);
            return;
        }
        self.set_pen_pressure(sample.pressure);
        self.on_pointer(sample.phase, PointerButton::Primary, over_panel);
        if sample.phase == PointerPhase::Up {
            self.set_pen_pressure(1.0);
        }
    }

    /// The pointer half of `window_event`, after egui has seen the event:
    /// mouse buttons, cursor moves, touch/pen contacts and focus changes.
    /// `consumed` is egui's "the chrome wants this pointer". Any other event
    /// is ignored here.
    fn on_pointer_window_event(&mut self, event: WindowEvent, consumed: bool) {
        match event {
            WindowEvent::MouseInput { state, button, .. } => {
                // `consumed` is only ever a veto on *claiming* a gesture — a
                // drag already running keeps running over a panel.
                self.on_mouse_button(state, button, consumed);
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.on_cursor_moved(position, consumed);
            }
            // W7-A: a pen or finger (winit's `Touch`, e.g. Windows
            // `WM_POINTER`) drives the same pointer route, with its force as
            // the stroke's pressure.
            WindowEvent::Touch(winit::event::Touch {
                id,
                phase,
                location,
                force,
                ..
            }) => {
                self.on_touch(id, phase, location, force, consumed);
            }
            // Card 052: a focus change is the moment the OS clipboard's
            // content can have changed (another application copied while we
            // were unfocused), so the menu-enablement probe is re-read.
            WindowEvent::Focused(true) => {
                self.editor.invalidate_os_image_probe();
            }
            WindowEvent::Focused(false) => {
                self.editor.invalidate_os_image_probe();
                self.on_focus_lost();
            }
            _ => {}
        }
    }

    /// A drag cannot outlive the window's focus, and a gesture left claimed
    /// would refuse every later press as somebody else's. Not `CursorLeft`:
    /// dragging past the edge of the window and back is a gesture, and winit
    /// keeps delivering its moves.
    ///
    /// W7-A: a touch/pen contact is dropped with it. winit 0.30 on Windows
    /// never reports `TouchPhase::Cancelled` and ignores
    /// `WM_POINTERCAPTURECHANGED`, so a contact whose lift was lost to Alt+Tab
    /// or a system prompt would otherwise stay down for good — swallowing
    /// every later mouse event and new contact — and leave its last pressure
    /// on the next stroke.
    fn on_focus_lost(&mut self) {
        self.abandon_gesture();
        self.pen.reset();
        self.set_pen_pressure(1.0);
        self.chrome.set_pen_hover(None);
    }

    /// A winit mouse button, unless it is the OS emulating an active
    /// touch/pen contact (W7-A), in which case it is dropped.
    fn on_mouse_button(&mut self, state: ElementState, button: MouseButton, over_panel: bool) {
        if self
            .pen
            .swallow_mouse_button(button, state == ElementState::Pressed)
        {
            return;
        }
        if let Some(button) = pointer_button(button) {
            let phase = match state {
                ElementState::Pressed => PointerPhase::Down,
                ElementState::Released => PointerPhase::Up,
            };
            self.on_pointer(phase, button, over_panel);
        }
    }

    /// A winit cursor move, unless a touch/pen contact is positioning the
    /// pointer itself (W7-A).
    fn on_cursor_moved(&mut self, position: PhysicalPosition<f64>, over_panel: bool) {
        if self.pen.swallow_cursor_move() {
            return;
        }
        // W8-A: the mouse moved, so the brush ring follows it again.
        self.chrome.set_pen_hover(None);
        self.cursor = Vec2::new(position.x as f32, position.y as f32);
        // The move belongs to whichever button went down, which winit
        // does not repeat here; with none held it is a hover.
        let button = self.held.unwrap_or(PointerButton::Primary);
        self.on_pointer(PointerPhase::Move, button, over_panel);
    }

    fn on_pointer(&mut self, phase: PointerPhase, button: PointerButton, over_panel: bool) {
        // A modal dialog owns the whole pointer while it is open: a press that
        // means "dismiss this modal" must never claim a canvas gesture. The
        // scrim already makes egui consume the click; this veto is the second
        // half, for a press that arrives before the next frame is drawn.
        let over_panel = over_panel || self.chrome.dialog_open();
        // W11-F: a press takes the tool the keys held right now lend, even
        // if a modifier change was never reported (focus came back with the
        // key already down); the router pins it for the gesture.
        if phase == PointerPhase::Down {
            self.sync_temporary_tool();
        }
        let input = PointerInput {
            phase,
            button,
            pos_pt: self.cursor,
            // A mouse is full pressure and egui 0.29 carries no pressure, so
            // the native winit tablet stream is what feeds a real value here
            // via [`Shell::set_pen_pressure`].
            pressure: self.pen_pressure,
            modifiers: modifiers_of(self.modifiers),
        };
        match phase {
            PointerPhase::Down => self.held = Some(button),
            PointerPhase::Up => self.held = None,
            PointerPhase::Move => {}
        }
        // Card 010: what the options bar holds is what the tool is. The
        // conversion happens here, at the boundary — the UI crate's value
        // types never reach a tool.
        let settings = self.tool_settings();
        let outcome = self
            .pointer
            .handle(&mut self.editor, input, over_panel, &settings);
        // Card 012: publish (or clear) the live session's overlay geometry
        // every frame, through the production publisher.
        // Card 012: publish (or clear) the live session's overlay geometry
        // every frame, through the production publisher.
        let geometry = self.pointer.live_geometry();
        self.chrome
            .publish_tool_geometry(geometry.clone(), self.editor.active().map(|doc| doc.id()));
        // XB: and the same session's pointer readout (a shape drag's W/H),
        // which the chrome paints beside the pointer; a release clears it.
        self.chrome
            .publish_tool_readout(self.pointer.live_readout());
        // Card 013: the same live session is what the compositor previews —
        // the object's pixels move as the handles move, through the one
        // compositor, with the committed document and its history untouched.
        self.pointer.settle_preview(&mut self.editor);
        if let Some(failed) = outcome.failed.as_deref() {
            // A refused setting is surfaced, not swallowed — the options
            // bar's contract (card 010's seam).
            self.editor.set_status(failed);
        }
        if outcome.needs_repaint() {
            self.repaint_at = Some(Instant::now());
        }
    }

    fn refresh_title(&mut self) {
        let title = self.editor.window_title();
        if let Some(state) = &mut self.state {
            if state.title != title {
                state.window.set_title(&title);
                state.title = title;
            }
        }
    }
}

fn system_theme(window: &Window) -> design::Theme {
    match window.theme() {
        Some(winit::window::Theme::Light) => design::Theme::Light,
        _ => design::Theme::Dark,
    }
}

/// The canvas backdrop for `theme`, as an 8-bit sRGB display value.
///
/// One function, two consumers: the empty-window clear below and
/// [`render::Canvas::set_backdrop`]. They used to disagree — the empty path
/// read `BackgroundCanvas` while the canvas cleared to a hardcoded grey — so in
/// Light mode the surround snapped from #E9E9EE to near-black the instant a
/// file was opened.
pub fn backdrop_srgb(theme: design::Theme) -> [u8; 3] {
    let c = theme
        .tokens()
        .palette
        .color(design::ColorRole::BackgroundCanvas);
    [c.r, c.g, c.b]
}

/// The clear value the window is filled with when there is no document.
///
/// Goes through the same [`render::backdrop_clear_color`] the canvas uses, so
/// the sRGB→linear conversion exists once rather than in two places that can
/// drift.
pub fn backdrop_clear(theme: design::Theme, format: wgpu::TextureFormat) -> wgpu::Color {
    render::backdrop_clear_color(backdrop_srgb(theme), format)
}

/// Fill the window with the canvas backdrop when there is no document to draw.
fn clear(
    encoder: &mut wgpu::CommandEncoder,
    view: &wgpu::TextureView,
    theme: design::Theme,
    format: wgpu::TextureFormat,
) {
    encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("empty-canvas"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(backdrop_clear(theme, format)),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
    });
}

fn create_depth_view(gpu: &GpuContext, width: u32, height: u32) -> wgpu::TextureView {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("egui-depth"),
        size: wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Depth32Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

impl ApplicationHandler<crate::shell::AppEvent> for Shell {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        match self.build_window(event_loop) {
            Ok(state) => {
                self.state = Some(state);
                self.begin_session();
                let files = std::mem::take(&mut self.startup_files);
                if !files.is_empty() {
                    self.editor.open_paths(&files);
                }
                self.sync_marker();
                self.refresh_title();
            }
            Err(e) => {
                // The whole point of `crate::error`: say what happened instead
                // of aborting with nothing on screen — and then *keep* it.
                // Nothing can be returned from here, so it is parked for
                // `finish`; dropping it is what made a run that never opened a
                // window exit 0.
                self.start_up_failed(e);
                event_loop.exit();
            }
        }
    }

    /// AccessKit's adapter talks back through the event-loop proxy: action
    /// requests (a screen reader clicked a node) route into egui-winit, which
    /// turns them into the widget responses the focused control would give.
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: crate::shell::AppEvent) {
        let crate::shell::AppEvent::AccessKit(event) = event;
        if let Some(state) = &mut self.state {
            match event.window_event {
                // C14: a screen reader asked for the tree. egui-winit 0.29
                // never calls `enable_accesskit` itself, so without this the
                // context keeps emitting an empty tree update and the AT
                // client sees nothing but the bare window.
                accesskit_winit::WindowEvent::InitialTreeRequested => {
                    state.egui_ctx.enable_accesskit();
                    // Enabling only flips a flag; the tree rides the next
                    // egui pass, and an idle document window would never
                    // repaint on its own. (C14: without this the screen
                    // reader's first poke is answered with silence.)
                    state.window.request_redraw();
                }
                accesskit_winit::WindowEvent::ActionRequested(request) => {
                    state.egui_state.on_accesskit_action_request(request);
                }
                _ => {}
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_none() {
            return;
        }
        // Card 087 / W2-G: apply finished jobs — imports, saves, exports. A
        // completion redraws; while jobs are in flight the loop wakes at the
        // job poll rate instead of sleeping, or a slow worker would never be
        // noticed — and instead of spinning, which a save that takes seconds
        // would turn into seconds of a busy core.
        if self.pump_jobs() {
            if let Some(state) = &self.state {
                state.window.request_redraw();
            }
            event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + JOB_POLL));
            return;
        }
        // C1: keep redrawing until the shot's warm-up frames have all rendered
        // — an idle window under `ControlFlow::Wait` would otherwise never
        // produce them.
        if self.shot.is_some() && !self.shot_taken && self.shot_frames < SHOT_WARMUP_FRAMES {
            if let Some(state) = &self.state {
                state.window.request_redraw();
            }
            event_loop.set_control_flow(ControlFlow::Poll);
            return;
        }
        if let Some(report) = self.editor.autosave_tick(Instant::now()) {
            tracing::info!("autosave started for {} document(s)", report.started.len());
            for (_, reason) in &report.failed {
                tracing::warn!("autosave failed: {reason}");
            }
            // A scratch autosave is only recoverable once the marker names it.
            // The marker is re-synced as each job lands too (`pump_jobs`).
            self.sync_marker();
        }
        let Some(state) = &self.state else { return };
        if self.editor.quit_requested() || self.shot_taken {
            self.shut_down();
            event_loop.exit();
            return;
        }
        // An armed autosave has to wake the loop, or a document sitting idle
        // would never be written.
        let deadline = match (self.repaint_at, self.editor.next_autosave()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        match deadline {
            None => event_loop.set_control_flow(ControlFlow::Wait),
            Some(at) if at <= Instant::now() => {
                self.repaint_at = None;
                state.window.request_redraw();
                event_loop.set_control_flow(ControlFlow::Wait);
            }
            Some(at) => event_loop.set_control_flow(ControlFlow::WaitUntil(at)),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        // Who owns the keyboard is read *before* the event reaches egui, so a
        // press cannot be judged against the focus it is about to create.
        let owner = self.keyboard_owner();
        // Tab is withheld from egui unless egui is the thing that wants it.
        // egui would otherwise move widget focus with it and then claim every
        // subsequent key press; see `is_focus_navigation_key`.
        let hide_from_egui = matches!(
            &event,
            WindowEvent::KeyboardInput { event, .. }
                if withhold_from_egui(owner, &event.logical_key)
        );
        let (consumed, wants_repaint) = match &mut self.state {
            Some(state) if !hide_from_egui => {
                let r = state.egui_state.on_window_event(&state.window, &event);
                (r.consumed, r.repaint)
            }
            Some(_) => (false, true),
            None => (false, false),
        };
        if wants_repaint {
            self.repaint_at = Some(Instant::now());
        }
        match event {
            WindowEvent::CloseRequested => {
                // Ask about unsaved work first; `Quit` reports Cancelled when
                // the user backs out, and the window stays.
                match self.editor.dispatch(Action::Quit) {
                    Ok(_) => {
                        self.shut_down();
                        event_loop.exit();
                    }
                    Err(e) => {
                        self.editor.set_status(e.to_string());
                        self.repaint_at = Some(Instant::now());
                    }
                }
            }
            WindowEvent::Resized(size) => {
                self.resize(size);
                self.repaint_at = Some(Instant::now());
            }
            WindowEvent::ThemeChanged(theme) => {
                let system = match theme {
                    winit::window::Theme::Light => design::Theme::Light,
                    winit::window::Theme::Dark => design::Theme::Dark,
                };
                let resolved = self.editor.preferences().theme.resolve(system);
                if let Some(state) = &mut self.state {
                    if state.theme != resolved {
                        crate::chrome::install_theme(&state.egui_ctx, resolved);
                        state.canvas.set_backdrop(backdrop_srgb(resolved));
                        state.theme = resolved;
                    }
                }
                self.repaint_at = Some(Instant::now());
            }
            WindowEvent::Ime(ime) => self.on_ime(&ime),
            WindowEvent::ModifiersChanged(mods) => self.on_modifiers(mods.state()),
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::DroppedFile(path) => {
                self.on_dropped_files(&[path]);
            }
            WindowEvent::KeyboardInput {
                event: key_event, ..
            } => {
                // Note what is *not* here: a guard on egui-winit's `consumed`.
                // That flag is true for every Tab press whatever the modifiers,
                // which made three shipped bindings unreachable. `route_key`
                // asks the two questions that actually matter instead.
                self.on_key(
                    owner,
                    &key_event.logical_key,
                    key_event.state,
                    key_event.repeat,
                );
            }
            // Mouse, touch/pen and focus changes: one route, testable
            // without an event loop (W7-A).
            event @ (WindowEvent::MouseInput { .. }
            | WindowEvent::CursorMoved { .. }
            | WindowEvent::Touch(_)
            | WindowEvent::Focused(_)) => {
                self.on_pointer_window_event(event, consumed);
            }
            WindowEvent::MouseWheel { delta, .. } if !consumed => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(x, y) => Vec2::new(x, y),
                    MouseScrollDelta::PixelDelta(p) => {
                        Vec2::new(p.x as f32, p.y as f32) / WHEEL_LINE_PX
                    }
                };
                self.on_wheel(lines);
            }
            _ => {}
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if self.marker.is_some() {
            self.shut_down();
        }
    }
}

#[cfg(test)]
#[path = "shell_w5d_tests.rs"]
mod w5d_tests;

#[cfg(test)]
#[path = "shell_w5c_tests.rs"]
mod w5c_tests;

#[cfg(test)]
#[path = "shell_w9l_tests.rs"]
mod w9l_tests;

#[cfg(test)]
#[path = "shell_w10j_tests.rs"]
mod w10j_tests;

#[cfg(test)]
#[path = "shell_pen_tests.rs"]
mod pen_tests;

#[cfg(test)]
#[path = "shell_w7i_tests.rs"]
mod w7i_tests;

#[cfg(test)]
#[path = "shell_w10k_tests.rs"]
mod w10k_tests;

#[cfg(test)]
#[path = "shell_w11f_tests.rs"]
mod w11f_tests;

// W11-D: Ctrl+V with no document open, through the real key route.
#[cfg(test)]
#[path = "shell_w11d_tests.rs"]
mod w11d_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::Key as WKey;

    use crate::chrome::ChromeOutput;
    use crate::dialogs::ScriptedDialogs;
    use crate::prefs::{AppPaths, Preferences};
    use crate::recent::RecentFiles;

    fn shell_with_one_image(dir: &std::path::Path) -> Shell {
        let png = dir.join("a.png");
        std::fs::write(
            &png,
            raster::encode(raster::ExportFormat::Png, 16, 16, &[7u8; 16 * 16 * 4]).unwrap(),
        )
        .unwrap();
        let mut editor = crate::editor::Editor::with_state(
            AppPaths::rooted(dir.join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        );
        editor.open_path(&png).unwrap();
        Shell::new(editor, Vec::new())
    }

    fn camera_of(shell: &Shell) -> (Vec2, f32) {
        let camera = &shell.editor.active().unwrap().camera;
        (camera.center, camera.zoom)
    }

    #[test]
    fn scroll_wheel_zooms_false_routes_a_plain_wheel_to_pan() {
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_with_one_image(dir.path());
        // The default: a plain wheel zooms.
        assert!(shell.editor.preferences().scroll_wheel_zooms);
        let (center, zoom) = camera_of(&shell);
        shell.on_wheel(Vec2::new(0.0, 1.0));
        let (c1, z1) = camera_of(&shell);
        assert_ne!(z1, zoom, "a plain wheel zooms by default");

        // Off: the same plain wheel pans and leaves the zoom alone.
        let mut prefs = shell.editor.ui_preferences();
        prefs.tools.scroll_wheel_zooms = false;
        shell.apply_chrome(ChromeOutput {
            set_ui_preferences: Some(Box::new(prefs)),
            ..ChromeOutput::default()
        });
        assert!(!shell.editor.preferences().scroll_wheel_zooms);
        shell.on_wheel(Vec2::new(0.0, -1.0));
        let (c2, z2) = camera_of(&shell);
        assert_eq!(z2, z1, "a plain wheel no longer zooms");
        assert_ne!(c2, c1, "a plain wheel pans");
        assert_eq!(c2.x, c1.x, "a vertical wheel pans vertically");

        // Shift turns it horizontal.
        shell.modifiers = ModifiersState::SHIFT;
        shell.on_wheel(Vec2::new(0.0, -1.0));
        let (c3, _) = camera_of(&shell);
        assert_ne!(c3.x, c2.x);
        assert_eq!(c3.y, c2.y);

        // W13-M: Ctrl+wheel scrolls sideways, as in Photopea; Alt+wheel zooms.
        shell.modifiers = ModifiersState::CONTROL;
        shell.on_wheel(Vec2::new(0.0, 1.0));
        let (c4, z4) = camera_of(&shell);
        assert_eq!(z4, z2, "Ctrl+wheel pans, it does not zoom");
        assert_ne!(c4.x, c3.x, "Ctrl+wheel pans horizontally");
        assert_eq!(c4.y, c3.y, "Ctrl+wheel pans horizontally");
        shell.modifiers = ModifiersState::ALT;
        shell.on_wheel(Vec2::new(0.0, 1.0));
        let (_, z5) = camera_of(&shell);
        assert_ne!(z5, z4, "Alt+wheel zooms");
        let _ = center;
    }

    #[test]
    fn the_wheel_gesture_follows_the_preference_and_the_modifiers() {
        let notch = Vec2::new(0.0, 1.0);
        assert!(matches!(
            wheel_gesture(notch, false, false, false, true),
            WheelGesture::Zoom(f) if f > 1.0
        ));
        assert_eq!(
            wheel_gesture(notch, false, false, false, false),
            WheelGesture::Pan(Vec2::new(0.0, WHEEL_LINE_PX))
        );
        assert_eq!(
            wheel_gesture(notch, false, false, true, false),
            WheelGesture::Pan(Vec2::new(WHEEL_LINE_PX, 0.0))
        );
        // W13-M: Ctrl + wheel pans sideways with wheel-pan configured
        // (Photopea), and zooms when the wheel zooms.
        assert_eq!(
            wheel_gesture(notch, true, false, false, false),
            WheelGesture::Pan(Vec2::new(WHEEL_LINE_PX, 0.0))
        );
        assert!(matches!(
            wheel_gesture(notch, true, false, false, true),
            WheelGesture::Zoom(_)
        ));
        // W13-M: with the wheel zooming, Alt + wheel pans instead.
        assert_eq!(
            wheel_gesture(notch, false, true, false, true),
            WheelGesture::Pan(Vec2::new(0.0, WHEEL_LINE_PX))
        );
        // W11-F: Alt + wheel zooms with wheel-pan configured.
        assert!(matches!(
            wheel_gesture(notch, false, true, false, false),
            WheelGesture::Zoom(f) if f > 1.0
        ));
    }

    /// Every control the Preferences dialog draws, against what it changes in
    /// the running application. The table is an exhaustive match, so a new
    /// control does not compile until it names its consumer; the loop proves
    /// each one's edit, confirmed through the shell's apply path, moves that
    /// consumer.
    #[test]
    fn every_preference_control_has_a_consumer() {
        use ui::dialogs::preferences::PrefControl;

        fn consumer(control: PrefControl, shell: &Shell) -> String {
            let ed = &shell.editor;
            match control {
                // The autosave scheduler's period.
                PrefControl::Autosave => {
                    format!("{:?}", crate::editor::autosave_period(ed.preferences()))
                }
                // The theme the frame installs.
                PrefControl::Theme => format!("{:?}", ed.preferences().theme),
                // egui's points-per-pixel multiplier.
                PrefControl::UiScale => format!("{}", ed.preferences().ui_scale),
                // The strings catalogue's active locale.
                PrefControl::Language => format!("{:?}", ui::strings::active()),
                // The size readout the status line carries.
                PrefControl::Units => ed.size_readout(72, 72),
                // What a plain wheel notch does to the camera.
                PrefControl::ScrollWheelZooms => format!(
                    "{:?}",
                    wheel_gesture(
                        Vec2::new(0.0, 1.0),
                        false,
                        false,
                        false,
                        ed.preferences().scroll_wheel_zooms
                    )
                ),
                // The open document's undo limit.
                PrefControl::HistoryStates => format!("{}", ed.active().unwrap().history.limit()),
                // Where a never-saved document's autosave is written.
                PrefControl::ScratchDir => ed
                    .preferences()
                    .scratch_dir(ed.paths())
                    .display()
                    .to_string(),
                // What the live keymap resolves.
                PrefControl::Keymap => format!("{:?}", ed.keymap().bindings()),
            }
        }

        for control in PrefControl::ALL {
            let dir = tempfile::tempdir().unwrap();
            let mut shell = shell_with_one_image(dir.path());
            let before = consumer(control, &shell);
            let mut prefs = shell.editor.ui_preferences();
            if !control.mutate(&mut prefs) {
                assert_eq!(control, PrefControl::Language, "{control:?} cannot change");
                assert_eq!(
                    ui::strings::Locale::ALL.len(),
                    1,
                    "a language list of more than one must be able to change"
                );
                continue;
            }
            shell.apply_chrome(ChromeOutput {
                set_ui_preferences: Some(Box::new(prefs)),
                ..ChromeOutput::default()
            });
            let after = consumer(control, &shell);
            assert_ne!(before, after, "{control:?} changed nothing that reads it");
        }
    }

    #[test]
    fn edit_keyboard_shortcuts_opens_preferences_on_the_keymap_page() {
        let dir = tempfile::tempdir().unwrap();
        // Three roads, each driven through the chrome the way a user drives
        // it, never by writing a `ChromeOutput` by hand:
        // - the menu bar's click handler (`Chrome::menu_click`, what
        //   `menu_bridge::draw` calls on a click);
        // - a workspace intent drained by the chrome's frame (the context
        //   menu and panel door);
        // - the chord (Ctrl+Alt+Shift+K through `perform_menu_chord`).
        // Each has to open the Preferences dialog on the Keymap page, and none
        // may emit the plain Preferences action that opens on General.
        let ctx = egui::Context::default();
        crate::chrome::install_theme(&ctx, design::Theme::Dark);
        let settle = |shell: &mut Shell| -> Vec<crate::action::Action> {
            let mut actions = Vec::new();
            for _ in 0..3 {
                let mut out = ChromeOutput::default();
                let _ = ctx.run(egui::RawInput::default(), |ctx| {
                    out = shell.chrome.ui(ctx, &mut shell.editor);
                });
                actions.extend(out.actions.iter().copied());
                shell.apply_chrome(out);
            }
            actions
        };
        for road in ["menu click", "workspace intent", "chord"] {
            let mut shell = shell_with_one_image(dir.path());
            let mut clicked = ChromeOutput::default();
            match road {
                "menu click" => {
                    let intent = ui::Intent::Action(ui::MenuAction::KeyboardShortcuts);
                    shell.chrome.menu_click(intent, &shell.editor, &mut clicked);
                }
                "workspace intent" => shell
                    .chrome
                    .emit(ui::Intent::Action(ui::MenuAction::KeyboardShortcuts)),
                _ => shell.perform_menu_chord(ui::MenuAction::KeyboardShortcuts),
            }
            assert!(
                !clicked
                    .actions
                    .contains(&crate::action::Action::ShowPreferences),
                "{road}: routed to the plain Preferences action"
            );
            shell.apply_chrome(clicked);
            let actions = settle(&mut shell);
            assert!(
                !actions.contains(&crate::action::Action::ShowPreferences),
                "{road}: routed to the plain Preferences action ({actions:?})"
            );
            let dialog = shell
                .chrome
                .dialogs_for_test()
                .active_preferences_for_test();
            assert_eq!(
                dialog.section(),
                ui::dialogs::PrefsSection::Keymap,
                "{road}"
            );
            assert!(!dialog.prefs().keymap.commands().is_empty());
        }
    }

    #[test]
    fn a_shortcut_bound_in_the_dialog_resolves_once_the_dialog_is_confirmed() {
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_with_one_image(dir.path());
        // Edit ▸ Keyboard Shortcuts…, through the door a menu click uses.
        shell
            .chrome
            .emit(ui::Intent::Action(ui::MenuAction::KeyboardShortcuts));
        let ctx = egui::Context::default();
        crate::chrome::install_theme(&ctx, design::Theme::Dark);
        let frame = |shell: &mut Shell, events: Vec<egui::Event>| {
            let mut out = ChromeOutput::default();
            let input = egui::RawInput {
                events,
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1440.0, 900.0),
                )),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                out = shell.chrome.ui(ctx, &mut shell.editor);
            });
            shell.apply_chrome(out);
        };
        for _ in 0..3 {
            frame(&mut shell, Vec::new());
        }
        // Capture a chord for Export with a real key press in a frame.
        shell
            .chrome
            .dialogs_for_test()
            .active_preferences_for_test()
            .begin_capture(&Action::Export.id());
        let press = |key: egui::Key, modifiers: egui::Modifiers| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        };
        let mods = egui::Modifiers {
            ctrl: true,
            alt: true,
            command: true,
            ..Default::default()
        };
        frame(&mut shell, vec![press(egui::Key::F9, mods)]);
        let chord = Chord {
            ctrl_or_cmd: true,
            alt: true,
            shift: false,
            key: Key::Function(9),
        };
        assert_eq!(shell.editor.keymap().resolve(&chord), None, "not before OK");
        {
            let dialog = shell
                .chrome
                .dialogs_for_test()
                .active_preferences_for_test();
            assert_eq!(dialog.capturing(), None, "the key press was captured");
            assert_eq!(
                dialog
                    .prefs()
                    .keymap
                    .shortcuts(&Action::Export.id())
                    .last()
                    .map(|s| s.display()),
                Some("Ctrl+Alt+F9".to_string()),
                "the capture bound the pressed chord on the page"
            );
        }
        // Enter confirms the dialog; the shell applies what it confirmed.
        frame(
            &mut shell,
            vec![press(egui::Key::Enter, egui::Modifiers::default())],
        );
        assert_eq!(shell.editor.keymap().resolve(&chord), Some(Action::Export));
    }

    #[test]
    fn the_units_preference_reaches_the_rulers() {
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_with_one_image(dir.path());
        let mut prefs = shell.editor.ui_preferences();
        prefs.interface.units = ui::dialogs::Unit::Centimeters;
        shell.apply_chrome(ChromeOutput {
            set_ui_preferences: Some(Box::new(prefs)),
            ..ChromeOutput::default()
        });
        let ctx = egui::Context::default();
        crate::chrome::install_theme(&ctx, design::Theme::Dark);
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            let _ = shell.chrome.ui(ctx, &mut shell.editor);
        });
        assert_eq!(
            shell.chrome.workspace().canvas.unit,
            ui::dialogs::Unit::Centimeters
        );
        // And a shell started on those preferences opens with them.
        let editor = crate::editor::Editor::with_state(
            AppPaths::rooted(dir.path().join("config2")),
            shell.editor.preferences().clone(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        );
        let mut fresh = Shell::new(editor, Vec::new());
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            let _ = fresh.chrome.ui(ctx, &mut fresh.editor);
        });
        assert_eq!(
            fresh.chrome.workspace().canvas.unit,
            ui::dialogs::Unit::Centimeters
        );
    }

    /// Round-3 review: View ▸ Rulers ▸ Inches changed only the workspace's
    /// ruler unit, so the status bar kept saying px while the Info panel said
    /// in, and a later Preferences OK that touched only the theme silently
    /// put the rulers back to pixels. The ruler menu and the Units preference
    /// are one setting.
    #[test]
    fn the_ruler_menu_is_the_units_preference_and_an_unrelated_save_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_with_one_image(dir.path());
        let ctx = egui::Context::default();
        crate::chrome::install_theme(&ctx, design::Theme::Dark);
        let frame = |shell: &mut Shell| {
            let mut out = ChromeOutput::default();
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                out = shell.chrome.ui(ctx, &mut shell.editor);
            });
            shell.apply_chrome(out);
        };
        frame(&mut shell);
        assert_eq!(shell.editor.display_unit(), ui::dialogs::Unit::Pixels);

        // The View menu's item: resolved against the live menu context and
        // its intent posted, which is what a click on View ▸ Rulers ▸ Inches
        // does.
        let context = crate::menu_bridge::context(&mut shell.editor, shell.chrome.workspace());
        let intent = ui::MenuAction::SetRulerUnit(ui::dialogs::Unit::Inches)
            .resolve(&context)
            .intent()
            .cloned()
            .expect("View > Rulers > Inches is enabled");
        shell.chrome.emit(intent);
        for _ in 0..2 {
            frame(&mut shell);
        }
        assert_eq!(
            shell.chrome.workspace().canvas.unit,
            ui::dialogs::Unit::Inches
        );
        assert_eq!(
            shell.editor.display_unit(),
            ui::dialogs::Unit::Inches,
            "the ruler menu did not reach the Units preference"
        );
        assert!(
            shell.editor.size_readout(72, 72).ends_with(" in"),
            "status readout: {}",
            shell.editor.size_readout(72, 72)
        );
        assert_eq!(
            shell.editor.ui_preferences().interface.units,
            ui::dialogs::Unit::Inches,
            "the Preferences dialog shows another unit than the rulers"
        );

        // Preferences… opened and saved with only the theme changed.
        let mut prefs = shell.editor.ui_preferences();
        prefs.interface.theme = match prefs.interface.theme {
            ui::dialogs::preferences::ThemeChoice::Dark => {
                ui::dialogs::preferences::ThemeChoice::Light
            }
            _ => ui::dialogs::preferences::ThemeChoice::Dark,
        };
        shell.apply_chrome(ChromeOutput {
            set_ui_preferences: Some(Box::new(prefs)),
            ..ChromeOutput::default()
        });
        for _ in 0..2 {
            frame(&mut shell);
        }
        assert_eq!(
            shell.chrome.workspace().canvas.unit,
            ui::dialogs::Unit::Inches,
            "an unrelated Preferences save reverted the rulers"
        );
        assert_eq!(shell.editor.display_unit(), ui::dialogs::Unit::Inches);
    }

    /// W3-X: View > Rulers > Picas used to persist as px, because the
    /// preference had no picas choice, so the next launch showed pixels. The
    /// ruler menu route, the saved file and a fresh load all keep Picas.
    #[test]
    fn picking_picas_on_the_ruler_menu_persists_picas_and_a_fresh_load_restores_them() {
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_with_one_image(dir.path());
        let ctx = egui::Context::default();
        crate::chrome::install_theme(&ctx, design::Theme::Dark);
        let frame = |shell: &mut Shell| {
            let mut out = ChromeOutput::default();
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                out = shell.chrome.ui(ctx, &mut shell.editor);
            });
            shell.apply_chrome(out);
        };
        frame(&mut shell);

        // View > Rulers > Picas, resolved against the live menu context and
        // its intent posted, as a click on the row does.
        let context = crate::menu_bridge::context(&mut shell.editor, shell.chrome.workspace());
        let intent = ui::MenuAction::SetRulerUnit(ui::dialogs::Unit::Picas)
            .resolve(&context)
            .intent()
            .cloned()
            .expect("View > Rulers > Picas is enabled");
        shell.chrome.emit(intent);
        for _ in 0..2 {
            frame(&mut shell);
        }
        assert_eq!(
            shell.chrome.workspace().canvas.unit,
            ui::dialogs::Unit::Picas
        );
        assert_eq!(
            shell.editor.display_unit(),
            ui::dialogs::Unit::Picas,
            "the ruler pick did not reach the Units preference as picas"
        );
        assert_eq!(
            shell.editor.preferences().units,
            crate::prefs::UnitChoice::Pc
        );

        // Saved, then read back from disk by a fresh load.
        shell.editor.persist().unwrap();
        let file = shell.editor.paths().preferences_file();
        let loaded = Preferences::load(&file);
        assert_eq!(loaded.units, crate::prefs::UnitChoice::Pc);

        // A shell started on the loaded file draws its rulers in picas.
        let editor = crate::editor::Editor::with_state(
            AppPaths::rooted(dir.path().join("config2")),
            loaded,
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        );
        let mut fresh = Shell::new(editor, Vec::new());
        frame(&mut fresh);
        assert_eq!(
            fresh.chrome.workspace().canvas.unit,
            ui::dialogs::Unit::Picas,
            "the next launch did not restore the ruler unit"
        );
    }

    /// W3-X: the Edit menu's Keyboard Shortcuts row, found in the menu the
    /// bar draws, enabled by the bridge's own resolution and clicked through
    /// the menu bar's handler, opens Preferences on the Keymap page.
    #[test]
    fn the_edit_menu_keyboard_shortcuts_row_is_live_and_opens_the_keymap_page() {
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_with_one_image(dir.path());
        let edit = crate::menu_bridge::menus(&shell.editor)
            .into_iter()
            .find(|m| m.actions().contains(&ui::MenuAction::Undo))
            .expect("the Edit menu");
        assert!(
            edit.actions().contains(&ui::MenuAction::KeyboardShortcuts),
            "the Edit menu lists Keyboard Shortcuts"
        );
        let context = crate::menu_bridge::context(&mut shell.editor, shell.chrome.workspace());
        let intent = crate::menu_bridge::resolve_intent(
            ui::MenuAction::KeyboardShortcuts,
            &context,
            &shell.editor,
        )
        .expect("the Keyboard Shortcuts row is enabled");
        let mut out = ChromeOutput::default();
        shell.chrome.menu_click(intent, &shell.editor, &mut out);
        assert!(
            out.actions.is_empty() && out.menu.is_empty() && out.unrouted.is_empty(),
            "the click was answered by the dialog host, not recorded: {:?} {:?}",
            out.actions,
            out.menu
        );
        let dialog = shell
            .chrome
            .dialogs_for_test()
            .active_preferences_for_test();
        assert_eq!(dialog.section(), ui::dialogs::PrefsSection::Keymap);
    }

    /// W3-G: every size readout the spec names follows the Units preference,
    /// read off what the real frame paints and what the real menu click
    /// opens — not off the helper that formats it.
    #[test]
    fn the_units_preference_reaches_the_status_bar_the_info_panel_and_the_size_dialogs() {
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_with_one_image(dir.path());
        let mut prefs = shell.editor.ui_preferences();
        prefs.interface.units = ui::dialogs::Unit::Centimeters;
        shell.apply_chrome(ChromeOutput {
            set_ui_preferences: Some(Box::new(prefs)),
            ..ChromeOutput::default()
        });
        // The Info panel on screen, alone in a minimal layout, as the ui
        // crate's own panel tests stage it (in the default layout it is a
        // background tab behind the Navigator).
        let dock = &mut shell.chrome.workspace_for_test().dock;
        dock.apply_layout(ui::LayoutId::Minimal);
        dock.set_open(ui::dock::PanelId::Info, true);
        let ctx = egui::Context::default();
        crate::chrome::install_theme(&ctx, design::Theme::Dark);
        let mut shapes = Vec::new();
        for _ in 0..3 {
            let mut out = ChromeOutput::default();
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1440.0, 900.0),
                )),
                ..Default::default()
            };
            let full = ctx.run(input, |ctx| {
                out = shell.chrome.ui(ctx, &mut shell.editor);
            });
            shapes = full.shapes;
            shell.apply_chrome(out);
        }
        // 16 px at the rulers' 72 ppi.
        let expected = "0.564 × 0.564 cm";
        assert_eq!(shell.editor.size_readout(16, 16), expected);
        let painted: Vec<egui::Pos2> = shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Text(t) if t.galley.text() == expected => Some(t.pos),
                _ => None,
            })
            .collect();
        let info_row = ctx
            .read_response(ui::dock::ids::info_value("Document"))
            .expect("the Info panel's Document row was drawn")
            .rect;
        assert!(
            painted.iter().any(|p| info_row.expand(1.0).contains(*p)),
            "the Info panel's Document row does not read {expected:?} (painted at {painted:?}, row {info_row:?})"
        );
        assert!(
            painted.iter().any(|p| p.y > 800.0),
            "the status bar's Size field does not read {expected:?} (painted at {painted:?})"
        );
        assert!(
            !shapes.iter().any(|c| matches!(
                &c.shape,
                egui::Shape::Text(t) if t.galley.text() == "16 × 16 px"
            )),
            "a size readout still reads in pixels"
        );

        // Image ▸ Image Size… and Canvas Size…, clicked through the menu bar's
        // handler, open with their fields in the preference.
        for action in [ui::MenuAction::ImageSize, ui::MenuAction::CanvasSize] {
            let mut out = ChromeOutput::default();
            shell
                .chrome
                .menu_click(ui::Intent::Action(action), &shell.editor, &mut out);
            let host = shell.chrome.dialogs_for_test();
            let unit = match host.active_for_test() {
                crate::dialog_host::ActiveDialog::ImageSize(d) => d.print_unit(),
                crate::dialog_host::ActiveDialog::CanvasSize(d) => d.unit(),
                other => panic!("{action:?} opened {other:?}"),
            };
            assert_eq!(unit, ui::dialogs::Unit::Centimeters, "{action:?}");
            host.close();
        }
    }

    #[test]
    fn pen_pressure_sets_the_sample_the_next_stroke_will_use() {
        let dir = std::env::temp_dir().join(format!("rs-pen-pressure-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut shell = shell_with_one_image(&dir);
        // Default is full pressure (the mouse case).
        assert_eq!(shell.pen_pressure, 1.0);
        shell.set_pen_pressure(0.3);
        assert_eq!(shell.pen_pressure, 0.3);
        // Clamped into 0..=1.
        shell.set_pen_pressure(-2.0);
        assert_eq!(shell.pen_pressure, 0.0);
        shell.set_pen_pressure(9.0);
        assert_eq!(shell.pen_pressure, 1.0);
        // A non-finite reading falls back to full pressure, not vetoing a stroke.
        shell.set_pen_pressure(f32::NAN);
        assert_eq!(shell.pen_pressure, 1.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn shell_with_two_images(dir: &std::path::Path) -> Shell {
        let mut shell = shell_with_one_image(dir);
        let png = dir.join("b.png");
        std::fs::write(
            &png,
            raster::encode(raster::ExportFormat::Png, 8, 8, &[3u8; 8 * 8 * 4]).unwrap(),
        )
        .unwrap();
        shell.editor.open_path(&png).unwrap();
        assert_eq!(shell.editor.documents().len(), 2);
        shell
    }

    /// Press a key on a shell that has no window, exactly as `window_event`
    /// would once `route_key` has spoken.
    fn press(shell: &mut Shell, owner: KeyboardOwner, key: WKey, mods: ModifiersState) {
        shell.modifiers = mods;
        shell.on_key(owner, &key, ElementState::Pressed, false);
    }

    /// A shell showing one 16x16 image in a 200x160 window, at 100% with the
    /// image centred — so screen `(100, 80)` is document `(8, 8)`.
    fn shell_ready_to_draw(dir: &std::path::Path) -> Shell {
        let mut shell = shell_with_one_image(dir);
        shell.spread_viewport(Vec2::new(200.0, 160.0));
        let doc = shell.editor.active_mut().unwrap();
        doc.camera.zoom = 1.0;
        doc.camera.center = Vec2::new(8.0, 8.0);
        // A freshly opened document owes the presenter the whole canvas. Taking
        // it is what the first frame does, and it is what makes "the gesture
        // invalidated these tiles" an observable claim rather than a tautology.
        doc.take_dirty();
        shell
    }

    /// Move the cursor to a document point and hand the shell one pointer
    /// sample, exactly as `window_event` does after winit has spoken.
    fn point(shell: &mut Shell, phase: PointerPhase, doc: Vec2, over_panel: bool) {
        shell.cursor = Vec2::new(100.0, 80.0) + doc - Vec2::new(8.0, 8.0);
        shell.on_pointer(phase, PointerButton::Primary, over_panel);
    }

    /// Press or release a *winit* button at document `x` on the row `y = 8`,
    /// through the same `pointer_button` gate `window_event` puts it through —
    /// so a button that routes to nothing reaches nothing here either.
    fn mouse(shell: &mut Shell, phase: PointerPhase, button: MouseButton, x: f32) {
        shell.cursor = Vec2::new(100.0, 80.0) + Vec2::new(x - 8.0, 0.0);
        if let Some(button) = pointer_button(button) {
            shell.on_pointer(phase, button, false);
        }
    }

    /// The defect this wave exists for: a left-drag on the canvas used to pan
    /// the view whatever tool was selected, so no tool could ever run.
    #[test]
    fn a_left_drag_paints_with_the_selected_tool_instead_of_panning() {
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_ready_to_draw(dir.path());
        shell.editor.set_tool(tools::ToolId::Brush);
        shell.editor.set_foreground([1.0, 0.0, 0.0, 1.0]);
        let center = shell.editor.active().unwrap().camera.center;
        // `Shell::new` starts owing a frame and only `about_to_wait` clears the
        // debt, which no test calls — so without this the assertion below is
        // true before the gesture begins and could never fail.
        shell.repaint_at = None;

        point(&mut shell, PointerPhase::Down, Vec2::new(6.0, 8.0), false);
        point(&mut shell, PointerPhase::Move, Vec2::new(8.0, 8.0), false);
        point(&mut shell, PointerPhase::Up, Vec2::new(10.0, 8.0), false);

        assert_eq!(
            shell.editor.active().unwrap().history_depth(),
            1,
            "the drag produced no undoable step"
        );
        assert_eq!(
            shell.editor.active().unwrap().camera.center,
            center,
            "a brush drag panned the view"
        );
        assert!(
            shell.repaint_at.is_some(),
            "the stroke asked for no repaint, so the canvas keeps showing the \
             frame from before it"
        );
        // The tiles it touched are outstanding — and only those, or a stroke
        // would re-upload the whole canvas.
        let dirty = shell.editor.active().unwrap().dirty();
        assert!(!dirty.is_all(), "a stroke invalidated the whole canvas");
        assert_eq!(
            dirty.tiles().collect::<Vec<_>>(),
            vec![raster::TileCoord::new(0, 0, 0)],
            "the canvas will not show the stroke"
        );
    }

    #[test]
    fn a_drag_with_the_hand_tool_still_pans_and_edits_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_ready_to_draw(dir.path());
        shell.editor.set_tool(tools::ToolId::Hand);
        let center = shell.editor.active().unwrap().camera.center;
        shell.repaint_at = None;

        point(&mut shell, PointerPhase::Down, Vec2::new(6.0, 8.0), false);
        point(&mut shell, PointerPhase::Move, Vec2::new(10.0, 8.0), false);
        point(&mut shell, PointerPhase::Up, Vec2::new(10.0, 8.0), false);

        assert_eq!(shell.editor.active().unwrap().history_depth(), 0);
        assert_ne!(shell.editor.active().unwrap().camera.center, center);
        // The camera path owes a frame too: the pixels are the same and the
        // view of them is not, so a pan that asks for no repaint is a pan the
        // user does not see until something else happens to redraw.
        assert!(
            shell.repaint_at.is_some(),
            "the pan asked for no repaint, so the view moved off-screen only"
        );
    }

    #[test]
    fn a_press_the_chrome_wanted_reaches_neither_the_tool_nor_the_camera() {
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_ready_to_draw(dir.path());
        shell.editor.set_tool(tools::ToolId::Brush);
        let center = shell.editor.active().unwrap().camera.center;

        shell.repaint_at = None;

        // `over_panel` is egui's `consumed`, which is what a press on a docked
        // panel or a menu comes back as.
        point(&mut shell, PointerPhase::Down, Vec2::new(6.0, 8.0), true);
        point(&mut shell, PointerPhase::Move, Vec2::new(10.0, 8.0), false);
        point(&mut shell, PointerPhase::Up, Vec2::new(10.0, 8.0), false);

        assert_eq!(shell.editor.active().unwrap().history_depth(), 0);
        assert_eq!(shell.editor.active().unwrap().camera.center, center);
        assert!(shell.editor.active().unwrap().dirty().is_empty());
        // Nothing changed, so nothing is owed: a gesture the chrome took must
        // not schedule a frame that would redraw an identical picture. This is
        // the other half of the repaint claim — the tests above prove it is
        // asked for when it is due, this proves it is not asked for otherwise.
        assert!(
            shell.repaint_at.is_none(),
            "a press the chrome consumed scheduled a repaint of the same frame"
        );
    }

    #[test]
    fn escape_abandons_the_stroke_the_shell_is_in_the_middle_of() {
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_ready_to_draw(dir.path());
        shell.editor.set_tool(tools::ToolId::Brush);
        point(&mut shell, PointerPhase::Down, Vec2::new(6.0, 8.0), false);
        point(&mut shell, PointerPhase::Move, Vec2::new(10.0, 8.0), false);
        assert!(shell.pointer.is_tool_active());

        press(
            &mut shell,
            KeyboardOwner::default(),
            WKey::Named(NamedKey::Escape),
            ModifiersState::empty(),
        );
        assert!(
            !shell.pointer.is_tool_active(),
            "Escape left the stroke live"
        );
        assert!(shell.held.is_none());

        point(&mut shell, PointerPhase::Up, Vec2::new(10.0, 8.0), false);
        assert_eq!(
            shell.editor.active().unwrap().history_depth(),
            0,
            "a cancelled stroke was committed anyway"
        );
        // ...and the canvas is not dead: the next press paints.
        point(&mut shell, PointerPhase::Down, Vec2::new(6.0, 8.0), false);
        point(&mut shell, PointerPhase::Up, Vec2::new(10.0, 8.0), false);
        assert_eq!(shell.editor.active().unwrap().history_depth(), 1);
    }

    #[test]
    fn losing_focus_abandons_the_gesture_rather_than_leaving_it_claimed() {
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_ready_to_draw(dir.path());
        shell.editor.set_tool(tools::ToolId::Brush);
        assert!(
            !shell.abandon_gesture(),
            "nothing is running, so there is nothing to abandon"
        );

        point(&mut shell, PointerPhase::Down, Vec2::new(6.0, 8.0), false);
        assert!(shell.abandon_gesture());
        assert!(!shell.pointer.is_gesture_active());
        assert!(!shell.abandon_gesture());
    }

    #[test]
    fn the_held_button_is_what_a_move_belongs_to() {
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_ready_to_draw(dir.path());
        assert!(shell.held.is_none());
        point(&mut shell, PointerPhase::Down, Vec2::new(6.0, 8.0), false);
        assert_eq!(shell.held, Some(PointerButton::Primary));
        point(&mut shell, PointerPhase::Up, Vec2::new(6.0, 8.0), false);
        assert!(shell.held.is_none());
    }

    #[test]
    fn only_the_two_routed_mouse_buttons_claim_a_gesture() {
        assert_eq!(
            pointer_button(MouseButton::Left),
            Some(PointerButton::Primary)
        );
        assert_eq!(
            pointer_button(MouseButton::Middle),
            Some(PointerButton::Middle)
        );
        // A press nothing routes must not claim a gesture no release ends...
        assert_eq!(pointer_button(MouseButton::Back), None);
        assert_eq!(pointer_button(MouseButton::Forward), None);
        assert_eq!(pointer_button(MouseButton::Other(9)), None);
        // ...and the right button is one of those, because the router would
        // give a `Secondary` press the active tool exactly as it gives it a
        // `Primary` one. See `pointer_button`.
        assert_eq!(pointer_button(MouseButton::Right), None);
    }

    /// The right button paints nothing, pans nothing, and claims nothing.
    ///
    /// It is not enough that `pointer_button` answers `None`: what matters is
    /// that the whole drag — the press, the moves winit reports while it is
    /// held, and the release — leaves the document and the camera exactly where
    /// they were.
    #[test]
    fn a_right_drag_on_the_canvas_neither_paints_nor_pans() {
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_ready_to_draw(dir.path());
        shell.editor.set_tool(tools::ToolId::Brush);
        shell.editor.set_foreground([1.0, 0.0, 0.0, 1.0]);
        let center = shell.editor.active().unwrap().camera.center;
        let before = shell
            .editor
            .active_mut()
            .unwrap()
            .composite(raster::PixelRect::new(0, 0, 16, 16))
            .unwrap();
        shell.repaint_at = None;

        mouse(&mut shell, PointerPhase::Down, MouseButton::Right, 6.0);
        // winit reports the moves of a right-drag as plain `CursorMoved`, which
        // the shell attributes to `held` — and nothing was held.
        for x in [8.0, 10.0] {
            shell.cursor = Vec2::new(100.0, 80.0) + Vec2::new(x - 8.0, 0.0);
            let button = shell.held.unwrap_or(PointerButton::Primary);
            shell.on_pointer(PointerPhase::Move, button, false);
        }
        mouse(&mut shell, PointerPhase::Up, MouseButton::Right, 10.0);

        assert!(shell.held.is_none(), "the right button claimed the pointer");
        assert!(!shell.pointer.is_gesture_active());
        assert_eq!(
            shell.editor.active().unwrap().history_depth(),
            0,
            "a right-drag painted an undoable stroke"
        );
        assert_eq!(
            shell
                .editor
                .active_mut()
                .unwrap()
                .composite(raster::PixelRect::new(0, 0, 16, 16))
                .unwrap(),
            before,
            "a right-drag changed the pixels"
        );
        assert_eq!(
            shell.editor.active().unwrap().camera.center,
            center,
            "a right-drag panned the view"
        );
        assert!(shell.editor.active().unwrap().dirty().is_empty());
        assert!(
            shell.repaint_at.is_none(),
            "a right-drag that changed nothing still scheduled a frame"
        );

        // ...and the canvas is not dead afterwards: the left button still
        // paints, so this refuses the button rather than the gesture.
        mouse(&mut shell, PointerPhase::Down, MouseButton::Left, 6.0);
        mouse(&mut shell, PointerPhase::Up, MouseButton::Left, 10.0);
        assert_eq!(shell.editor.active().unwrap().history_depth(), 1);
        assert!(shell.repaint_at.is_some(), "the left drag owed a frame");
    }

    #[test]
    fn the_platform_modifier_reaches_a_tool_as_ctrl() {
        // The same rule `chord_from_key` follows: a tool that checks `ctrl`
        // means the key this platform modifies with.
        let m = modifiers_of(ModifiersState::SHIFT | ModifiersState::ALT);
        assert!(m.shift && m.alt && !m.ctrl);
        assert!(modifiers_of(ModifiersState::CONTROL).ctrl);
        assert!(modifiers_of(ModifiersState::SUPER).ctrl);
        assert_eq!(
            modifiers_of(ModifiersState::empty()),
            tools::Modifiers::NONE
        );
    }

    /// W2-X: Photopea's plain `F` walks the three screen modes through the
    /// same key route Tab takes to the panels flag. The window's borderless
    /// full screen follows `Editor::screen_mode` in `sync_appearance`, which
    /// needs a real window; this proves the route up to the value it reads.
    #[test]
    fn plain_f_cycles_the_screen_mode_through_the_key_route() {
        use ui::palette::ScreenMode;
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_with_two_images(dir.path());
        let free = KeyboardOwner::default();
        let f = WKey::Character("f".into());

        assert_eq!(shell.editor().screen_mode(), ScreenMode::Standard);
        for expected in [
            ScreenMode::FullScreenWithMenu,
            ScreenMode::FullScreen,
            ScreenMode::Standard,
        ] {
            press(&mut shell, free, f.clone(), ModifiersState::empty());
            assert_eq!(shell.editor().screen_mode(), expected);
        }

        // Like every other chord, F is a letter while a field has the
        // keyboard, and nothing while a modal owns it.
        let typing = KeyboardOwner {
            egui_text_focus: true,
            recording_shortcut: false,
        };
        press(&mut shell, typing, f.clone(), ModifiersState::empty());
        assert_eq!(
            shell.editor().screen_mode(),
            ScreenMode::Standard,
            "F cycled the screen mode while a text field had the keyboard"
        );
        shell.chrome.open_new_document_dialog();
        assert!(shell.chrome.dialog_open());
        press(&mut shell, free, f, ModifiersState::empty());
        assert_eq!(
            shell.editor().screen_mode(),
            ScreenMode::Standard,
            "F cycled the screen mode under a modal"
        );
    }

    #[test]
    fn the_tab_chords_reach_the_editor_although_egui_calls_every_tab_consumed() {
        // The defect: the key handler only ran when `egui-winit` reported the
        // event as *not* consumed, and egui-winit 0.29 computes
        // `consumed = wants_keyboard_input() || key == Tab` — so Tab was always
        // consumed, whatever the modifiers, and the three Tab chords this
        // application ships could never fire in the running program even though
        // the keymap resolved them.
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_with_two_images(dir.path());
        let free = KeyboardOwner::default();

        assert!(shell.editor().panels_visible());
        press(
            &mut shell,
            free,
            WKey::Named(NamedKey::Tab),
            ModifiersState::empty(),
        );
        assert!(
            !shell.editor().panels_visible(),
            "Tab must toggle the panels"
        );

        shell.editor.activate(0).unwrap();
        press(
            &mut shell,
            free,
            WKey::Named(NamedKey::Tab),
            ModifiersState::CONTROL,
        );
        assert_eq!(
            shell.editor().active_index(),
            Some(1),
            "Ctrl+Tab must step to the next document"
        );
        press(
            &mut shell,
            free,
            WKey::Named(NamedKey::Tab),
            ModifiersState::CONTROL | ModifiersState::SHIFT,
        );
        assert_eq!(
            shell.editor().active_index(),
            Some(0),
            "Ctrl+Shift+Tab must step back"
        );
    }

    #[test]
    fn tab_is_never_handed_to_egui_unless_egui_is_recording_it() {
        // The other half of the same defect: the Tab egui swallowed moved egui's
        // widget focus, after which `wants_keyboard_input()` stayed true and
        // *every* shortcut was dead until the user pressed Escape.
        let tab = WKey::Named(NamedKey::Tab);
        let z = WKey::Character("z".into());
        assert!(withhold_from_egui(KeyboardOwner::default(), &tab));
        assert!(
            !withhold_from_egui(KeyboardOwner::default(), &z),
            "only Tab moves egui's focus"
        );
        assert!(
            !withhold_from_egui(
                KeyboardOwner {
                    recording_shortcut: true,
                    ..Default::default()
                },
                &tab
            ),
            "the shortcut editor reads its chord out of egui's events"
        );
        // Even a focused text field does not get Tab: nothing here needs it,
        // and letting it through is what gives focus somewhere to wander to.
        assert!(withhold_from_egui(
            KeyboardOwner {
                egui_text_focus: true,
                ..Default::default()
            },
            &tab
        ));
    }

    #[test]
    fn a_focused_text_field_wins_the_keyboard() {
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_with_two_images(dir.path());
        let typing = KeyboardOwner {
            egui_text_focus: true,
            ..Default::default()
        };
        let before = shell.editor().active_index();

        press(
            &mut shell,
            typing,
            WKey::Named(NamedKey::Tab),
            ModifiersState::CONTROL,
        );
        assert_eq!(
            shell.editor().active_index(),
            before,
            "Ctrl+Tab did not switch tabs"
        );
        press(
            &mut shell,
            typing,
            WKey::Character("b".into()),
            ModifiersState::empty(),
        );
        press(
            &mut shell,
            typing,
            WKey::Named(NamedKey::Tab),
            ModifiersState::empty(),
        );
        assert!(
            shell.editor().panels_visible(),
            "and a bare Tab did not hide the panels"
        );
        assert_eq!(
            shell.editor().tool(),
            tools::ToolId::Move,
            "nor did a letter select a tool"
        );
    }

    #[test]
    fn recording_a_shortcut_does_not_also_perform_it() {
        // The defect: egui 0.29 does not focus a clicked button, so while the
        // shortcut editor was listening the shell still saw the press as
        // unconsumed and performed it. Assigning a shortcut over Ctrl+Q quit
        // the application; over Ctrl+W it closed the document.
        let recording = KeyboardOwner {
            recording_shortcut: true,
            ..Default::default()
        };
        for (key, mods) in [
            (WKey::Character("q".into()), ModifiersState::CONTROL),
            (WKey::Character("w".into()), ModifiersState::CONTROL),
            (
                WKey::Named(NamedKey::Delete),
                ModifiersState::CONTROL | ModifiersState::SHIFT,
            ),
        ] {
            assert_eq!(
                route_key(recording, &key, ElementState::Pressed, false, mods),
                KeyOutcome::Ignore,
                "{key:?} was dispatched while it was being recorded"
            );
        }

        // ...and it really does not reach the editor.
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_with_two_images(dir.path());
        press(
            &mut shell,
            recording,
            WKey::Character("q".into()),
            ModifiersState::CONTROL,
        );
        assert!(!shell.editor().quit_requested(), "Ctrl+Q quit the app");
        assert_eq!(shell.editor().documents().len(), 2);

        // With nothing recording, the same press is the action it names.
        press(
            &mut shell,
            KeyboardOwner::default(),
            WKey::Character("q".into()),
            ModifiersState::CONTROL,
        );
        assert!(
            shell.editor().quit_requested(),
            "and otherwise it still quits"
        );
    }

    #[test]
    fn a_release_gives_back_the_hand_whoever_owns_the_keyboard() {
        // A focus change mid-hold must not strand the temporary hand tool, so
        // the release is unconditional. It is idempotent, so giving back a hand
        // that was never borrowed costs nothing.
        for owner in [
            KeyboardOwner::default(),
            KeyboardOwner {
                egui_text_focus: true,
                recording_shortcut: true,
            },
        ] {
            assert_eq!(
                route_key(
                    owner,
                    &WKey::Named(NamedKey::Space),
                    ElementState::Released,
                    false,
                    ModifiersState::empty()
                ),
                KeyOutcome::ReleaseTemporaryHand
            );
            assert_eq!(
                route_key(
                    owner,
                    &WKey::Character("b".into()),
                    ElementState::Released,
                    false,
                    ModifiersState::empty()
                ),
                KeyOutcome::Ignore
            );
        }
    }

    #[test]
    fn only_the_hand_wants_a_held_keys_repeats() {
        let free = KeyboardOwner::default();
        assert_eq!(
            route_key(
                free,
                &WKey::Named(NamedKey::Space),
                ElementState::Pressed,
                true,
                ModifiersState::empty()
            ),
            KeyOutcome::Dispatch(Chord::plain(Key::Space)),
            "a held Space must keep the hand engaged"
        );
        assert_eq!(
            route_key(
                free,
                &WKey::Character("z".into()),
                ElementState::Pressed,
                true,
                ModifiersState::CONTROL
            ),
            KeyOutcome::Ignore,
            "a held Ctrl+Z must not undo the whole session"
        );
        // A key that forms no chord at all is simply not ours.
        assert_eq!(
            route_key(
                free,
                &WKey::Named(NamedKey::Shift),
                ElementState::Pressed,
                false,
                ModifiersState::SHIFT
            ),
            KeyOutcome::Ignore
        );
    }

    #[test]
    fn a_start_up_failure_is_shown_and_still_ends_the_run_non_zero() {
        // The defect: `resumed` showed its dialog and then dropped the error,
        // because `ApplicationHandler` has nowhere to return one. `run_app`
        // reported the loop's own clean exit, `run` returned `Ok(())`, and a
        // run that never opened a window exited 0 — while
        // `studio-desktop`'s module doc promises a terminal or a CI script
        // exactly the opposite.
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_with_one_image(dir.path());
        assert!(
            shell.finish(Ok(())).is_ok(),
            "a run that started is still a clean run"
        );

        let mut shell = shell_with_one_image(dir.path());
        shell.start_up_failed(ShellError::Gpu(anyhow::anyhow!(
            "no suitable GPU adapter found"
        )));

        // The user was told. `Editor::report_error` is the call that puts the
        // native message box on screen, and the status line it writes at the
        // same time is how a windowless test reads back that it happened.
        let told = shell.editor().status().unwrap_or_default().to_string();
        assert!(
            told.contains("Raster Studio cannot start the graphics system"),
            "no dialog title: {told}"
        );
        assert!(
            told.contains("no suitable GPU adapter found"),
            "the dialog did not name what failed: {told}"
        );
        assert!(
            told.contains("graphics driver"),
            "the advice never reached the user: {told}"
        );

        // ...and the process still fails, which is the half that was missing.
        match shell.finish(Ok(())) {
            Err(ShellError::Gpu(e)) => {
                assert!(e.to_string().contains("no suitable GPU adapter found"))
            }
            other => panic!("a start-up failure exited cleanly: {other:?}"),
        }
    }

    #[test]
    fn a_windowing_system_that_never_answers_is_explained_rather_than_only_returned() {
        // The mirror case. `EventLoop::new` failing — no display, an SSH
        // session, a container — was returned but never shown, although
        // `ShellError::EventLoop`'s advice ("Raster Studio needs a desktop
        // session…") is written for precisely that user. `run` now hands it to
        // `report_startup_failure`, the same path every other start-up failure
        // takes. (`EventLoop::new` itself cannot be made to fail in a test; the
        // variant a headless box produces is not constructible outside winit,
        // so this drives the same `ShellError` through the same function.)
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_with_one_image(dir.path());
        let returned = shell.report_startup_failure(ShellError::EventLoop(
            winit::error::EventLoopError::RecreationAttempt,
        ));
        assert!(
            matches!(returned, ShellError::EventLoop(_)),
            "the error must come back for the exit code"
        );

        let told = shell.editor().status().unwrap_or_default().to_string();
        assert!(
            told.contains("Raster Studio cannot open a window"),
            "no dialog title: {told}"
        );
        assert!(
            told.contains("desktop session"),
            "the advice written for this case never reached the user: {told}"
        );
    }

    #[test]
    fn an_unroutable_intent_reaches_the_status_bar() {
        // The window's own admission that a click went nowhere. Before this,
        // `Chrome::harvest` dropped an intent the bridge could not answer with
        // no status line and no log record — which is exactly how a Properties
        // panel in which not one slider worked passed review.
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_with_one_image(dir.path());
        let orphan = ui::Intent::Action(ui::menu::MenuAction::PlaceEmbedded);

        shell.apply_chrome(ChromeOutput {
            unrouted: vec![orphan.clone()],
            ..Default::default()
        });

        let told = shell.editor().status().unwrap_or_default().to_string();
        assert_eq!(told, crate::menu_bridge::unrouted_message(&orphan));
        // `Place Embedded…` cannot be answered in this build; that refusal
        // names the missing piece (nothing places an embedded document) rather
        // than the generic fallback. This test protects the *reporting* — that
        // the window admits a click went somewhere it cannot perform — not the
        // specific reason, so assert the user was told something real.
        assert!(
            told.contains("Place"),
            "the user was told nothing actionable: {told}"
        );
    }

    #[test]
    fn a_slider_edit_from_the_chrome_reaches_the_document() {
        // The last wire of the three: the panel emits, the bridge routes, and
        // this is where it lands. `ChromeOutput::layer_kind` had no consumer at
        // all before, so an adjustment's parameters travelled as far as the
        // shell and stopped.
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_with_one_image(dir.path());
        let layer = layer_model::Layer::with_kind(
            "Posterize",
            layer_model::LayerKind::Adjustment(layer_model::AdjustmentLayer {
                kind: layer_model::AdjustmentKind::Posterize { levels: 8 },
            }),
        );
        let id = layer.id;
        shell
            .editor
            .apply_command(editor_core::Command::create_layer(layer));

        shell.apply_chrome(ChromeOutput {
            layer_kind: vec![crate::chrome::KindEdit {
                layer: id,
                kind: Box::new(layer_model::LayerKind::Adjustment(
                    layer_model::AdjustmentLayer {
                        kind: layer_model::AdjustmentKind::Posterize { levels: 3 },
                    },
                )),
                gesture: Some(1),
            }],
            ..Default::default()
        });

        let kind = &shell
            .editor()
            .active()
            .unwrap()
            .document
            .layers
            .get(id)
            .unwrap()
            .kind;
        assert_eq!(
            kind,
            &layer_model::LayerKind::Adjustment(layer_model::AdjustmentLayer {
                kind: layer_model::AdjustmentKind::Posterize { levels: 3 },
            }),
            "the slider's value never reached the document"
        );
    }

    #[test]
    fn the_navigators_pan_and_a_typed_zoom_move_the_documents_camera() {
        // The reviewer measured both of these as dead: the Navigator's drag
        // wrote `Workspace::view_center`, which `grep` found no reader for
        // outside the Navigator's own panel, and the status bar's zoom field
        // moved a number in the workspace while the image stayed where it was.
        // The camera belongs to the document, so this is where they land.
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_with_one_image(dir.path());
        let before = shell.editor().active().unwrap().camera.center;

        shell.apply_chrome(ChromeOutput {
            set_view_center: Some((3.0, 4.0)),
            set_zoom: Some(8.0),
            ..Default::default()
        });
        let camera = &shell.editor().active().unwrap().camera;
        assert_eq!(camera.center, Vec2::new(3.0, 4.0));
        assert_ne!(camera.center, before, "the pan really moved the view");
        assert_eq!(camera.zoom, 8.0);

        // A value that cannot be drawn is refused rather than making the
        // camera unusable: `screen_to_image` divides by the zoom, and a NaN
        // centre poisons every later pan.
        shell.apply_chrome(ChromeOutput {
            set_view_center: Some((f32::NAN, 0.0)),
            set_zoom: Some(0.0),
            ..Default::default()
        });
        let camera = &shell.editor().active().unwrap().camera;
        assert_eq!(camera.center, Vec2::new(3.0, 4.0));
        assert_eq!(camera.zoom, 8.0);

        // ...and a typed zoom is held to the same range a wheel gesture is,
        // so the two routes to a zoom level cannot reach different extremes.
        shell.apply_chrome(ChromeOutput {
            set_zoom: Some(10_000.0),
            ..Default::default()
        });
        assert_eq!(shell.editor().active().unwrap().camera.zoom, MAX_ZOOM);
        shell.apply_chrome(ChromeOutput {
            set_zoom: Some(1e-9),
            ..Default::default()
        });
        assert_eq!(shell.editor().active().unwrap().camera.zoom, MIN_ZOOM);
    }

    /// W4-F: the New Document dialog's 16-bit answer, through the shell's
    /// own dialog arm: a white background becomes RGBA16 tiles and the
    /// document says 16; a transparent one (no tiles) still says 16.
    #[test]
    fn a_sixteen_bit_new_document_is_created_at_sixteen_bits() {
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_with_one_image(dir.path());
        for background in [
            ui::dialogs::BackgroundContents::White,
            ui::dialogs::BackgroundContents::Transparent,
        ] {
            let spec = ui::dialogs::NewDocumentSpec {
                title: "Deep".to_string(),
                width: 300,
                height: 40,
                resolution_ppi: 72.0,
                color_mode: ui::dialogs::ColorMode::Rgb,
                color_space: color::ColorSpace::Srgb,
                bit_depth: raster::BitDepth::Sixteen,
                background,
                artboard: false,
            };
            shell.apply_chrome(ChromeOutput {
                dialog: Some(ui::dialogs::DialogAction::NewDocument(Box::new(spec))),
                ..Default::default()
            });
            let doc = shell.editor().active().unwrap();
            assert_eq!(doc.title(), "Deep");
            assert_eq!(doc.document.meta.bit_depth, 16, "{background:?}");
            let layer = doc.document.active_layer().unwrap();
            let lens: Vec<usize> = doc
                .document
                .layer_tiles(layer)
                .map(|m| {
                    m.iter()
                        .map(|(_, h)| compositor::TileSource::tile(&doc.tiles, h).unwrap().len())
                        .collect()
                })
                .unwrap_or_default();
            if background == ui::dialogs::BackgroundContents::White {
                assert_eq!(lens.len(), 2, "two tiles across 300 px");
            }
            assert!(
                lens.iter()
                    .all(|l| *l == raster::Tile::byte_len(raster::PixelFormat::Rgba16)),
                "{background:?}: {lens:?}"
            );
        }
    }

    #[test]
    fn the_windows_size_reaches_every_open_document_and_fits_it_once() {
        // The defect: the only `fit()` on the open path ran against a viewport
        // that was still the canvas's own size, so it was a no-op and a large
        // image opened as a 100% centre crop. The shell is where the real size
        // is known, so the shell is where the fit happens.
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_with_one_image(dir.path());
        // File ▸ New is the dialog now; what this test exercises is what
        // happens when its confirmed answer becomes a document, so the spec
        // travels the same road the dialog's confirm sends it.
        let spec = ui::dialogs::NewDocumentSpec {
            title: "Untitled".to_string(),
            width: crate::editor::NEW_DOCUMENT_SIZE.0,
            height: crate::editor::NEW_DOCUMENT_SIZE.1,
            resolution_ppi: 72.0,
            color_mode: ui::dialogs::ColorMode::Rgb,
            color_space: color::ColorSpace::Srgb,
            bit_depth: raster::BitDepth::Eight,
            background: ui::dialogs::BackgroundContents::Transparent,
            artboard: false,
        };
        shell.apply_chrome(ChromeOutput {
            dialog: Some(ui::dialogs::DialogAction::NewDocument(Box::new(spec))),
            ..Default::default()
        });
        let (w, h) = crate::editor::NEW_DOCUMENT_SIZE;
        assert!(
            shell.editor().documents().iter().all(|d| d.awaiting_fit()),
            "nothing has been drawn yet, so nothing can have been fitted"
        );

        shell.spread_viewport(Vec2::new(800.0, 600.0));

        let new_doc = shell.editor().documents().last().unwrap();
        let expected = (800.0 / w as f32).min(600.0 / h as f32);
        assert!(
            (new_doc.camera.zoom - expected).abs() < 1e-4,
            "{w}x{h} did not fit an 800x600 window: zoom {}",
            new_doc.camera.zoom
        );
        assert_eq!(
            new_doc.camera.viewport_size,
            Vec2::new(800.0, 600.0),
            "the camera is still working from a fabricated viewport"
        );
        assert!(
            !shell.editor().documents()[0].awaiting_fit(),
            "the background tab waits for a redraw that may never come"
        );
    }

    #[test]
    fn a_new_layer_stays_active_when_the_menu_creates_it() {
        // The defect: `apply_chrome` performed the actions first and *then*
        // applied `select_layer`, which the chrome had filled with the
        // selection as it stood before the click. So Layer ▸ New Layer created
        // the layer and immediately pointed the cursor back at the old one —
        // silently, with no error and nothing in the log.
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_with_one_image(dir.path());
        let original = shell
            .editor()
            .active()
            .unwrap()
            .document
            .active_layer()
            .unwrap();

        shell.apply_chrome(ChromeOutput {
            actions: vec![Action::NewLayer],
            select_layer: Some(original),
            ..Default::default()
        });

        let doc = &shell.editor().active().unwrap().document;
        assert_eq!(doc.layers.len(), 2, "the layer was created");
        let active = doc.active_layer().expect("something must be active");
        assert_ne!(
            active, original,
            "the new layer is the one you want to paint on"
        );
    }

    #[test]
    fn a_click_and_an_action_in_one_frame_compose_in_the_order_they_happened() {
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_with_one_image(dir.path());
        // Two layers, with the *lower* one selected.
        shell.perform(Action::NewLayer);
        let doc = &shell.editor().active().unwrap().document;
        let lower = *doc.layers.root().last().unwrap();
        let upper = doc.active_layer().unwrap();
        assert_ne!(lower, upper);

        // Click the lower row, then hide it from the menu, in one frame.
        shell.apply_chrome(ChromeOutput {
            actions: vec![Action::ToggleLayerVisibility],
            select_layer: Some(lower),
            ..Default::default()
        });
        let doc = &shell.editor().active().unwrap().document;
        assert!(
            !doc.layers.get(lower).unwrap().visible,
            "the row that was clicked is the one that was hidden"
        );
        assert!(doc.layers.get(upper).unwrap().visible);
    }

    #[test]
    fn the_canvas_backdrop_is_the_design_token_in_every_theme() {
        // The defect: `Shell::clear` used `BackgroundCanvas` but ran only while
        // the window was empty. With a document open the canvas cleared to a
        // hardcoded 0.1 grey, so Light mode snapped from #E9E9EE to near-black
        // the instant a file was opened.
        for theme in design::Theme::ALL {
            let token = theme
                .tokens()
                .palette
                .color(design::ColorRole::BackgroundCanvas);
            assert_eq!(
                backdrop_srgb(*theme),
                [token.r, token.g, token.b],
                "{theme:?} does not hand the canvas its own token"
            );
            for format in [
                wgpu::TextureFormat::Bgra8UnormSrgb,
                wgpu::TextureFormat::Bgra8Unorm,
                wgpu::TextureFormat::Rgba8UnormSrgb,
            ] {
                // What the empty window clears to and what the canvas clears to
                // are the same value, computed by the same function.
                assert_eq!(
                    backdrop_clear(*theme, format),
                    render::backdrop_clear_color(backdrop_srgb(*theme), format),
                    "{theme:?}/{format:?}"
                );
            }
        }
        // ...and the two themes really are different colours, so a test that
        // passed by handing both the same constant would fail here.
        assert_ne!(
            backdrop_srgb(design::Theme::Light),
            backdrop_srgb(design::Theme::Dark)
        );
        assert_ne!(
            backdrop_srgb(design::Theme::Dark),
            render::DEFAULT_BACKDROP_SRGB,
            "the token must not be the constant the render crate invented"
        );
    }

    #[test]
    fn a_character_key_becomes_the_chord_the_keymap_expects() {
        let chord = chord_from_key(&WKey::Character("z".into()), ModifiersState::CONTROL).unwrap();
        assert_eq!(chord, Chord::ctrl(Key::character('z')));
        assert_eq!(
            crate::keymap::Keymap::default().resolve(&chord),
            Some(Action::Undo)
        );

        // Shift reports the upper-case character; the key is the same one.
        let chord = chord_from_key(
            &WKey::Character("Z".into()),
            ModifiersState::CONTROL | ModifiersState::SHIFT,
        )
        .unwrap();
        assert_eq!(chord, Chord::ctrl_shift(Key::character('z')));
        assert_eq!(
            crate::keymap::Keymap::default().resolve(&chord),
            Some(Action::Redo)
        );
    }

    #[test]
    fn named_keys_map_to_the_keys_the_defaults_use() {
        let cases = [
            (NamedKey::Tab, Key::Tab, Some(Action::TogglePanels)),
            (NamedKey::Space, Key::Space, Some(Action::TemporaryHand)),
            (NamedKey::Escape, Key::Escape, None),
            (NamedKey::F5, Key::Function(5), None),
        ];
        for (named, expected, action) in cases {
            let chord = chord_from_key(&WKey::Named(named), ModifiersState::empty()).unwrap();
            assert_eq!(chord.key, expected);
            assert_eq!(crate::keymap::Keymap::default().resolve(&chord), action);
        }
    }

    #[test]
    fn the_delete_layer_chord_reaches_the_action_that_had_no_key_at_all() {
        // Wave 0 shipped `NewLayer` and `DeleteLayer` with no binding.
        let chord = chord_from_key(
            &WKey::Named(NamedKey::Delete),
            ModifiersState::CONTROL | ModifiersState::SHIFT,
        )
        .unwrap();
        assert_eq!(
            crate::keymap::Keymap::default().resolve(&chord),
            Some(Action::DeleteLayer)
        );
        let chord = chord_from_key(
            &WKey::Character("n".into()),
            ModifiersState::CONTROL | ModifiersState::SHIFT,
        )
        .unwrap();
        assert_eq!(
            crate::keymap::Keymap::default().resolve(&chord),
            Some(Action::NewLayer)
        );
    }

    /// The chords the menu bar paints and the application's own keymap does
    /// not claim reach the chrome's workspace as the very intent a click on
    /// the item would post — the door `Chrome::harvest` drains.
    ///
    /// RED before `Shell::on_key` consulted the menu table: every press here
    /// left the outbox empty, and Ctrl+Shift+I / Ctrl+Shift+E / Ctrl+J ran
    /// File Info / Export / Duplicate Layer instead.
    #[test]
    fn a_chord_only_the_menu_paints_goes_through_the_menu_door() {
        use ui::menu::MenuAction as M;
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_with_one_image(dir.path());
        let free = KeyboardOwner::default();
        shell.chrome.workspace_for_test().drain_intents();
        let ctrl_shift = ModifiersState::CONTROL | ModifiersState::SHIFT;
        let cases: [(WKey, ModifiersState, M); 9] = [
            (WKey::Character("I".into()), ctrl_shift, M::InverseSelection),
            (WKey::Character("E".into()), ctrl_shift, M::MergeVisible),
            (
                WKey::Character("j".into()),
                ModifiersState::CONTROL,
                M::LayerViaCopy,
            ),
            (WKey::Character("J".into()), ctrl_shift, M::LayerViaCut),
            (
                WKey::Character("e".into()),
                ModifiersState::CONTROL,
                M::MergeDown,
            ),
            (
                WKey::Character("a".into()),
                ModifiersState::CONTROL,
                M::SelectAll,
            ),
            (
                WKey::Character("t".into()),
                ModifiersState::CONTROL,
                M::FreeTransform,
            ),
            (
                WKey::Named(NamedKey::F7),
                ModifiersState::empty(),
                M::TogglePanel(ui::PanelId::Layers),
            ),
            (
                WKey::Named(NamedKey::Delete),
                ModifiersState::empty(),
                M::ClearPixels,
            ),
        ];
        for (key, mods, expected) in cases {
            shell.repaint_at = None;
            press(&mut shell, free, key.clone(), mods);
            let intents = shell.chrome.workspace_for_test().drain_intents();
            assert_eq!(
                intents,
                vec![ui::Intent::Action(expected)],
                "{key:?} with {mods:?}"
            );
            assert!(shell.repaint_at.is_some(), "{key:?} owed a frame");
        }
    }

    /// The character winit's `logical_key` carries for a chord on a US
    /// keyboard: the glyph the key *types* with those modifiers held, which
    /// with Shift is the upper glyph — `)` for the `0` key, `}` for `]`.
    fn us_logical_glyph(chord: &Chord) -> Option<WKey> {
        let Key::Char(base) = chord.key else {
            return None;
        };
        let typed = if chord.shift {
            if base.is_ascii_alphabetic() {
                base.to_ascii_uppercase()
            } else {
                // The inverse of `unshifted_us_glyph`, looked up rather than
                // written twice so the test cannot agree with itself by copy.
                (0x20u8..0x7f)
                    .map(char::from)
                    .find(|g| unshifted_us_glyph(*g) == Some(base))
                    .unwrap_or(base)
            }
        } else {
            base
        };
        Some(WKey::Character(typed.to_string().into()))
    }

    /// A shifted digit or punctuation glyph names the key under it, so the
    /// chord is the one the menu bar paints. Without Shift the glyph is kept:
    /// a layout where `)` is unshifted has a `)` key.
    #[test]
    fn a_shifted_glyph_folds_back_to_the_key_the_menu_names() {
        let ctrl_shift = ModifiersState::CONTROL | ModifiersState::SHIFT;
        assert_eq!(
            chord_from_key(&WKey::Character(")".into()), ctrl_shift),
            Some(Chord::ctrl_shift(Key::Char('0')))
        );
        assert_eq!(
            chord_from_key(&WKey::Character("^".into()), ModifiersState::SHIFT),
            Some(Chord {
                ctrl_or_cmd: false,
                alt: false,
                shift: true,
                key: Key::Char('6'),
            })
        );
        assert_eq!(
            chord_from_key(&WKey::Character(":".into()), ctrl_shift),
            Some(Chord::ctrl_shift(Key::Char(';')))
        );
        assert_eq!(
            chord_from_key(&WKey::Character("}".into()), ctrl_shift),
            Some(Chord::ctrl_shift(Key::Char(']')))
        );
        assert_eq!(
            chord_from_key(&WKey::Character("{".into()), ctrl_shift),
            Some(Chord::ctrl_shift(Key::Char('[')))
        );
        assert_eq!(
            chord_from_key(&WKey::Character("|".into()), ctrl_shift),
            Some(Chord::ctrl_shift(Key::Char('\\')))
        );
        // Zoom In on the `=`/`+` key: Ctrl+Shift+= types `+`, and the keymap
        // binds Ctrl+Shift+= — the fold must not lose it.
        assert_eq!(
            crate::keymap::Keymap::default()
                .resolve(&chord_from_key(&WKey::Character("+".into()), ctrl_shift).unwrap()),
            Some(Action::ZoomIn)
        );
        // No Shift, no fold.
        assert_eq!(
            chord_from_key(&WKey::Character(")".into()), ModifiersState::CONTROL),
            Some(Chord::ctrl(Key::Char(')')))
        );
    }

    /// The table gate the keymap's own test cannot be: every painted menu
    /// chord, spelled as winit's `logical_key` delivers it from a US keyboard
    /// with the chord's modifiers held, goes through [`chord_from_key`] and
    /// comes out as its own menu action.
    ///
    /// RED before the shifted-glyph fold: Ctrl+Shift+0 (Fill Screen), Shift+6
    /// (Feather), Ctrl+Shift+; (Snap), Ctrl+Shift+] (Bring to Front) and
    /// Ctrl+Shift+[ (Send to Back) arrived as `)` `^` `:` `}` `{` and
    /// resolved to nothing.
    #[test]
    fn every_painted_menu_chord_is_reachable_from_the_key_winit_reports() {
        use crate::keymap::{chord_of_shortcut, menu_twin, Keymap, Resolved};
        use ui::menu::MenuAction;
        let map = Keymap::default();
        let mut dead = Vec::new();
        let mut through_glyph = 0;
        for action in MenuAction::all() {
            let Some(shortcut) = action.shortcut() else {
                continue;
            };
            let painted = chord_of_shortcut(shortcut).unwrap();
            let Some(logical) = us_logical_glyph(&painted) else {
                // Named keys (F7, Delete, …) carry no glyph to fold.
                continue;
            };
            let mut mods = ModifiersState::empty();
            mods.set(ModifiersState::CONTROL, painted.ctrl_or_cmd);
            mods.set(ModifiersState::ALT, painted.alt);
            mods.set(ModifiersState::SHIFT, painted.shift);
            let got = chord_from_key(&logical, mods).and_then(|c| map.resolve_any(&c));
            let agrees = match got {
                Some(Resolved::Menu(menu)) => menu == action,
                Some(Resolved::App(app)) => menu_twin(app) == Some(action),
                None => false,
            };
            if !agrees {
                dead.push(format!(
                    "{painted} paints {action:?}; the key arrives as {logical:?} and resolves to {got:?}"
                ));
            }
            through_glyph += 1;
        }
        assert!(
            through_glyph > 40,
            "the menu bar lost its shortcuts ({through_glyph})"
        );
        assert!(
            dead.is_empty(),
            "{} painted chord(s) cannot be pressed on a US keyboard:\n{}",
            dead.len(),
            dead.join("\n")
        );
    }

    /// The five chords the audit's second pass found dead at the window, each
    /// pressed as the platform delivers it — the *shifted* glyph — and each
    /// arriving in the chrome's outbox as the menu item's own intent.
    ///
    /// RED before the shifted-glyph fold: every press left the outbox empty.
    #[test]
    fn the_shifted_punctuation_chords_the_menu_paints_reach_the_menu_door() {
        use ui::menu::{Arrange, MenuAction as M, ModifySelection, ZoomCommand};
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_with_one_image(dir.path());
        let free = KeyboardOwner::default();
        shell.chrome.workspace_for_test().drain_intents();
        let ctrl_shift = ModifiersState::CONTROL | ModifiersState::SHIFT;
        let cases: [(WKey, ModifiersState, M); 5] = [
            (
                WKey::Character(")".into()),
                ctrl_shift,
                M::Zoom(ZoomCommand::FillScreen),
            ),
            // Feather… is Photopea's Shift+F6 (W3-H), not Shift+6.
            (
                WKey::Named(NamedKey::F6),
                ModifiersState::SHIFT,
                M::Modify(ModifySelection::Feather),
            ),
            (
                WKey::Character(":".into()),
                ctrl_shift,
                M::ToggleView(ui::ViewFlag::Snap),
            ),
            (
                WKey::Character("}".into()),
                ctrl_shift,
                M::ArrangeLayer(Arrange::BringToFront),
            ),
            (
                WKey::Character("{".into()),
                ctrl_shift,
                M::ArrangeLayer(Arrange::SendToBack),
            ),
        ];
        for (key, mods, expected) in cases {
            shell.repaint_at = None;
            press(&mut shell, free, key.clone(), mods);
            let intents = shell.chrome.workspace_for_test().drain_intents();
            assert_eq!(
                intents,
                vec![ui::Intent::Action(expected)],
                "{key:?} with {mods:?}"
            );
            assert!(shell.repaint_at.is_some(), "{key:?} owed a frame");
        }
    }

    #[test]
    fn a_menu_chord_is_not_fired_while_typing_recording_or_under_a_modal() {
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_with_one_image(dir.path());
        shell.chrome.workspace_for_test().drain_intents();
        let ctrl_shift = ModifiersState::CONTROL | ModifiersState::SHIFT;

        let typing = KeyboardOwner {
            egui_text_focus: true,
            recording_shortcut: false,
        };
        press(&mut shell, typing, WKey::Character("I".into()), ctrl_shift);
        let recording = KeyboardOwner {
            egui_text_focus: false,
            recording_shortcut: true,
        };
        press(
            &mut shell,
            recording,
            WKey::Character("I".into()),
            ctrl_shift,
        );
        assert!(
            shell.chrome.workspace_for_test().drain_intents().is_empty(),
            "a chord typed into a field or recorded as a shortcut must not fire"
        );

        shell.chrome.open_new_document_dialog();
        assert!(shell.chrome.dialog_open());
        press(
            &mut shell,
            KeyboardOwner::default(),
            WKey::Character("I".into()),
            ctrl_shift,
        );
        assert!(
            shell.chrome.workspace_for_test().drain_intents().is_empty(),
            "a modal owns the keyboard"
        );
    }

    #[test]
    fn the_users_own_binding_wins_over_the_menu_table_at_the_key_handler() {
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_with_one_image(dir.path());
        shell.chrome.workspace_for_test().drain_intents();
        // Ctrl+E is Merge Down in the menu; the user makes it Fit on Screen.
        let ctrl_e = Chord::ctrl(Key::character('e'));
        shell
            .editor
            .keymap_mut()
            .force_bind(ctrl_e, Action::ZoomFit);
        shell.editor.active_mut().unwrap().camera.zoom = 4.0;
        press(
            &mut shell,
            KeyboardOwner::default(),
            WKey::Character("e".into()),
            ModifiersState::CONTROL,
        );
        assert!(
            shell.chrome.workspace_for_test().drain_intents().is_empty(),
            "the menu door must not also open"
        );
        assert_ne!(
            shell.editor.active().unwrap().camera.zoom,
            4.0,
            "the user's Fit on Screen ran"
        );
    }

    #[test]
    fn a_bare_modifier_forms_no_chord() {
        assert_eq!(
            chord_from_key(&WKey::Named(NamedKey::Shift), ModifiersState::SHIFT),
            None
        );
        // ...and neither does a multi-character IME commit.
        assert_eq!(
            chord_from_key(&WKey::Character("ab".into()), ModifiersState::empty()),
            None
        );
    }

    #[test]
    fn the_super_key_counts_as_ctrl_so_one_table_serves_macos() {
        let chord = chord_from_key(&WKey::Character("s".into()), ModifiersState::SUPER).unwrap();
        assert!(chord.ctrl_or_cmd);
        assert_eq!(
            crate::keymap::Keymap::default().resolve(&chord),
            Some(Action::Save)
        );
    }

    #[test]
    fn only_space_releases_the_temporary_hand() {
        assert!(is_temporary_hand_key(&WKey::Named(NamedKey::Space)));
        assert!(!is_temporary_hand_key(&WKey::Character("b".into())));
        assert!(!is_temporary_hand_key(&WKey::Named(NamedKey::Tab)));
    }

    #[test]
    fn the_surface_format_prefers_srgb_and_refuses_what_it_cannot_draw() {
        use wgpu::TextureFormat as F;
        assert_eq!(
            choose_surface_format(&[F::Bgra8Unorm, F::Bgra8UnormSrgb]).unwrap(),
            F::Bgra8UnormSrgb,
            "sRGB wins when both are offered"
        );
        assert_eq!(
            choose_surface_format(&[F::Bgra8Unorm]).unwrap(),
            F::Bgra8Unorm,
            "a plain 8-bit target is still drawable"
        );

        // An adapter offering only formats the canvas cannot draw to is a
        // dialog, not a panic — and the message names what was offered.
        let err = choose_surface_format(&[F::Rgba16Float]).unwrap_err();
        assert!(err.to_string().contains("Rgba16Float"), "{err}");
        let err = choose_surface_format(&[]).unwrap_err();
        assert!(matches!(err, ShellError::UnsupportedSurfaceFormat { .. }));
    }

    /// A confirmed New Document dialog builds exactly the document it
    /// confirmed — the size and the background the user chose, not a hardcoded
    /// default.
    #[test]
    fn a_confirmed_export_dialog_writes_through_the_folder_picker() {
        let dir = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let png = dir.path().join("noise.png");
        // Noise, so the JPEG quality assertions in the doc-level test carry
        // over: this one pins the *wiring* — confirm → folder picker → files.
        let mut state = 0x9e37_79b9u32;
        let mut px = Vec::with_capacity(16 * 16 * 4);
        for _ in 0..16 * 16 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            px.extend_from_slice(&[(state >> 16) as u8, (state >> 8) as u8, state as u8, 255]);
        }
        std::fs::write(
            &png,
            raster::encode(raster::ExportFormat::Png, 16, 16, &px).unwrap(),
        )
        .unwrap();

        let dialogs = ScriptedDialogs {
            export_folders: vec![out.path().to_path_buf()],
            ..Default::default()
        };
        let mut editor = crate::editor::Editor::with_state(
            AppPaths::rooted(dir.path().join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(dialogs),
        );
        editor.open_path(&png).unwrap();
        let mut shell = Shell::new(editor, Vec::new());

        let job = ui::dialogs::ExportJob {
            base_name: "shot".to_string(),
            entries: vec![ui::dialogs::ExportEntry::new(
                "",
                raster::ExportFormat::Png,
                1.0,
            )],
        };
        shell.apply_chrome(ChromeOutput {
            dialog: Some(DialogAction::Export(Box::new(job))),
            ..Default::default()
        });

        let written = out.path().join("shot.png");
        assert!(
            written.exists(),
            "the confirmed job was not written: {out:?}"
        );
        assert!(
            shell
                .editor()
                .status()
                .is_some_and(|s| s.starts_with("Exported 1 file")),
            "the status bar did not report the export"
        );
    }

    /// W4-H: File > Export > Slices writes with the settings of the last
    /// Export As that was handed to the writer. A job confirmed and then
    /// cancelled at the folder picker is not remembered.
    #[test]
    fn an_export_as_job_is_remembered_for_slices_only_once_a_folder_is_chosen() {
        use ui::dialogs::export_as::{forget_last_confirmed_entry, last_confirmed_entry};
        forget_last_confirmed_entry();
        let dir = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let png = dir.path().join("probe.png");
        std::fs::write(
            &png,
            raster::encode(raster::ExportFormat::Png, 8, 8, &[200u8; 8 * 8 * 4]).unwrap(),
        )
        .unwrap();
        // One folder answer: the first export gets it, the second is cancelled.
        let dialogs = ScriptedDialogs {
            export_folders: vec![out.path().to_path_buf()],
            ..Default::default()
        };
        let mut editor = crate::editor::Editor::with_state(
            AppPaths::rooted(dir.path().join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(dialogs),
        );
        editor.open_path(&png).unwrap();
        let mut shell = Shell::new(editor, Vec::new());
        let job = |format| ui::dialogs::ExportJob {
            base_name: "probe".to_string(),
            entries: vec![ui::dialogs::ExportEntry::new("", format, 0.5)],
        };

        shell.apply_chrome(ChromeOutput {
            dialog: Some(DialogAction::Export(Box::new(job(
                raster::ExportFormat::Jpeg(40),
            )))),
            ..Default::default()
        });
        let remembered = last_confirmed_entry();
        assert_eq!(remembered.preset.format, raster::ExportFormat::Jpeg(40));
        assert_eq!(remembered.preset.scale, 0.5);

        // The picker has no answer left: cancelled, nothing written, and the
        // remembered settings stay the JPEG ones.
        shell.apply_chrome(ChromeOutput {
            dialog: Some(DialogAction::Export(Box::new(job(
                raster::ExportFormat::Bmp,
            )))),
            ..Default::default()
        });
        assert_eq!(
            last_confirmed_entry().preset.format,
            raster::ExportFormat::Jpeg(40),
            "a job cancelled at the folder picker was remembered"
        );
        forget_last_confirmed_entry();
    }

    // ------------------------------------------------------------ W2-G

    thread_local! {
        /// Job bodies a queueing spawner has been handed and not yet run.
        static QUEUED_JOBS: std::cell::RefCell<Vec<Box<dyn FnOnce() + Send>>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }

    /// A spawner that holds every job until the test says so — the job is
    /// "in flight" for exactly as many frames as the test pumps.
    fn queue_job(_name: String, body: Box<dyn FnOnce() + Send>) -> std::io::Result<()> {
        QUEUED_JOBS.with(|q| q.borrow_mut().push(body));
        Ok(())
    }

    /// Run every queued job body on a real worker thread, joined: the body
    /// provably runs off the calling thread, and the test stays deterministic.
    fn run_queued_jobs_on_a_worker() -> usize {
        let bodies: Vec<_> = QUEUED_JOBS.with(|q| q.borrow_mut().drain(..).collect());
        let n = bodies.len();
        for body in bodies {
            std::thread::spawn(body)
                .join()
                .expect("the job body completes");
        }
        n
    }

    /// A `w`x`h` PNG of noise: every 256x256 tile distinct, so a save has as
    /// many blobs to write as the canvas has tiles.
    fn write_noise_png(path: &std::path::Path, w: u32, h: u32) {
        let mut state = 0x9e37_79b9u32;
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..w * h {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            px.extend_from_slice(&[(state >> 16) as u8, (state >> 8) as u8, state as u8, 255]);
        }
        std::fs::write(
            path,
            raster::encode(raster::ExportFormat::Png, w, h, &px).unwrap(),
        )
        .unwrap();
    }

    /// A shell over a 1024x1024 noise image (16 distinct tiles) whose jobs
    /// are queued rather than run, with the save picker primed to `target`.
    fn shell_with_a_large_document_and_queued_jobs(
        dir: &std::path::Path,
        target: &std::path::Path,
    ) -> Shell {
        let png = dir.join("large.png");
        write_noise_png(&png, 1024, 1024);
        let mut editor = crate::editor::Editor::with_state(
            AppPaths::rooted(dir.join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new().saving_to(target.to_path_buf())),
        );
        editor.open_path(&png).unwrap();
        editor.dispatch(Action::NewLayer).unwrap();
        assert!(editor.active().unwrap().is_dirty());
        editor.set_spawner(queue_job);
        Shell::new(editor, Vec::new())
    }

    /// W2-G: Ctrl+S hands the write to a worker and the shell keeps
    /// processing frames while it runs. Pinned through the job seam with no
    /// wall clock: the job is held in flight while frames are pumped, then run
    /// on a real thread, and the next pump applies the completion — path,
    /// dirty flag, title, recents, status — on the interaction thread.
    #[test]
    fn a_save_runs_on_the_worker_while_the_shell_keeps_processing_frames() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("large.rstudio");
        let mut shell = shell_with_a_large_document_and_queued_jobs(dir.path(), &target);

        // Ctrl+S through the shell's own action route.
        shell.perform(Action::Save);
        assert!(
            shell.editor.saves_pending(),
            "the save is a job, not a blocking call"
        );
        assert!(
            shell
                .editor
                .status()
                .is_some_and(|s| s.starts_with("Saving")),
            "{:?}",
            shell.editor.status()
        );
        assert!(
            !target.exists(),
            "nothing has been written: the worker has not run"
        );
        assert!(shell.editor.active().unwrap().is_dirty());
        assert!(shell.editor.window_title().starts_with("• "));

        // A second Ctrl+S while it runs is refused, and the refusal is on the
        // status line.
        shell.perform(Action::Save);
        assert!(
            shell
                .editor
                .status()
                .is_some_and(|s| s.contains("already being saved")),
            "{:?}",
            shell.editor.status()
        );
        assert_eq!(
            QUEUED_JOBS.with(|q| q.borrow().len()),
            1,
            "the refused save started no second job"
        );

        // Frames go by. The loop polls, finds nothing landed, carries on.
        for _ in 0..3 {
            assert!(shell.pump_jobs(), "still in flight");
            shell.apply_chrome(ChromeOutput::default());
        }
        assert!(shell.editor.active().unwrap().is_dirty());
        assert!(!target.exists());

        // The worker runs — on another thread — and the next frame applies
        // the completion here.
        assert_eq!(run_queued_jobs_on_a_worker(), 1);
        assert!(target.join(project_format::MANIFEST_FILE).is_file());
        assert!(
            shell.editor.active().unwrap().is_dirty(),
            "not clean until the completion is applied on this thread"
        );
        assert!(!shell.pump_jobs(), "nothing left in flight");
        let doc = shell.editor.active().unwrap();
        assert!(!doc.is_dirty(), "the completion cleared the dirty flag");
        assert_eq!(doc.project_path(), Some(target.as_path()));
        assert_eq!(
            shell.editor.window_title(),
            "large — Raster Studio",
            "the title lost its bullet and follows the saved file (W5-D)"
        );
        assert!(shell.editor.recent().entries().contains(&target));
        assert!(
            shell
                .editor
                .status()
                .is_some_and(|s| s.starts_with("Saved ")),
            "{:?}",
            shell.editor.status()
        );
        // And the package the worker wrote is the document, pixels included.
        let loaded = project_format::open_project(&target).unwrap();
        assert_eq!(loaded.document.layers.len(), 2);
        assert_eq!(loaded.tiles.len(), 16, "sixteen distinct noise tiles");
    }

    /// W2-G: an edit made while the worker is writing leaves the document
    /// dirty when the save lands — the package holds the snapshot, not the
    /// edit — and the status line says so.
    #[test]
    fn an_edit_during_a_save_keeps_the_document_dirty_when_it_lands() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("edited.rstudio");
        let mut shell = shell_with_a_large_document_and_queued_jobs(dir.path(), &target);
        shell.perform(Action::Save);
        shell.editor.dispatch(Action::NewLayer).unwrap();
        run_queued_jobs_on_a_worker();
        shell.pump_jobs();
        let doc = shell.editor.active().unwrap();
        assert!(doc.is_dirty(), "the third layer is not on disk");
        assert_eq!(doc.project_path(), Some(target.as_path()));
        assert!(
            shell
                .editor
                .status()
                .is_some_and(|s| s.contains("not in it yet")),
            "{:?}",
            shell.editor.status()
        );
        assert_eq!(
            project_format::load_project(&target).unwrap().layers.len(),
            2,
            "the snapshot had two layers"
        );
    }

    /// W2-G: the autosave timer takes the same worker route as Ctrl+S.
    #[test]
    fn an_autosave_takes_the_same_worker_route() {
        let dir = tempfile::tempdir().unwrap();
        let mut shell =
            shell_with_a_large_document_and_queued_jobs(dir.path(), &dir.path().join("x"));
        let t0 = Instant::now();
        assert!(
            shell.editor.autosave_tick(t0).is_none(),
            "the first tick arms"
        );
        let due = shell.editor.next_autosave().unwrap();
        let report = shell.editor.autosave_tick(due).expect("a dirty document");
        assert_eq!(report.started.len(), 1, "{report:?}");
        assert!(
            report.written.is_empty(),
            "started, not written: {report:?}"
        );
        let (_, path) = &report.started[0];
        assert!(!path.exists(), "the worker has not run");
        assert!(shell.editor.saves_pending());
        assert!(
            shell.editor.autosave_paths().is_empty(),
            "not recorded until it has landed"
        );

        assert!(shell.pump_jobs());
        assert_eq!(run_queued_jobs_on_a_worker(), 1);
        assert!(!shell.pump_jobs());
        assert!(path.join(project_format::MANIFEST_FILE).is_file());
        assert_eq!(shell.editor.autosave_paths(), vec![path.clone()]);
        assert!(
            shell.editor.active().unwrap().is_dirty(),
            "an autosave into scratch is not the save the user asked for"
        );
    }

    /// W2-G: a `.psd` is parsed on the worker; the interaction thread only
    /// wraps the result when the job lands.
    #[test]
    fn a_psd_is_parsed_on_the_worker() {
        let dir = tempfile::tempdir().unwrap();
        let psd = dir.path().join("layers.psd");
        let mut file = psd::PsdFile::new(psd::PsdHeader::rgba8(16, 16));
        let canvas = psd::Rect::sized(16, 16);
        let mut layer = psd::PsdLayer::raster("Base", canvas);
        layer
            .set_rgba8(&[10u8, 20, 30, 255].repeat(16 * 16))
            .unwrap();
        file.layers = vec![layer];
        std::fs::write(&psd, psd::write(&file).unwrap()).unwrap();

        let mut editor = crate::editor::Editor::with_state(
            AppPaths::rooted(dir.path().join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new().opening(psd.clone())),
        );
        editor.set_image_clipboard(Box::new(crate::clipboard::FakeClipboard::new()));
        editor.set_spawner(queue_job);
        let mut shell = Shell::new(editor, Vec::new());
        shell.perform(Action::Open);
        assert!(shell.editor.imports_pending());
        assert!(shell.editor.documents().is_empty(), "nothing parsed here");
        assert!(shell.pump_jobs());
        assert_eq!(run_queued_jobs_on_a_worker(), 1);
        assert!(!shell.pump_jobs());
        let doc = shell.editor.active().expect("the psd opened");
        assert_eq!(doc.title(), "layers.psd");
        assert_eq!(doc.source_path(), Some(psd.as_path()));
        assert_eq!(doc.document.layers.len(), 1);
        assert!(!doc.tiles.is_empty(), "the pixels came with the tree");
    }

    /// W2-G: a save that fails reaches the status line, leaves the document
    /// dirty, and leaves the previous package exactly as it was.
    #[test]
    fn a_failed_save_reaches_the_status_line_and_leaves_the_previous_package_intact() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.rstudio");
        // A "directory" that is a file: the worker cannot create the package
        // under it.
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, b"not a directory").unwrap();
        let bad = blocker.join("second.rstudio");
        let png = dir.path().join("a.png");
        write_noise_png(&png, 256, 256);
        let mut editor = crate::editor::Editor::with_state(
            AppPaths::rooted(dir.path().join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(
                ScriptedDialogs::new()
                    .saving_to(first.clone())
                    .saving_to(bad.clone()),
            ),
        );
        editor.open_path(&png).unwrap();
        editor.dispatch(Action::NewLayer).unwrap();
        let mut shell = Shell::new(editor, Vec::new());

        shell.perform(Action::Save);
        assert!(!shell.editor.active().unwrap().is_dirty());
        assert!(first.join(project_format::MANIFEST_FILE).is_file());

        shell.editor.dispatch(Action::NewLayer).unwrap();
        shell.perform(Action::SaveAs);
        let doc = shell.editor.active().unwrap();
        assert!(doc.is_dirty(), "a failed save clears nothing");
        assert_eq!(
            doc.project_path(),
            Some(first.as_path()),
            "and adopts nothing"
        );
        assert!(
            shell
                .editor
                .status()
                .is_some_and(|s| s.starts_with("Save failed:")),
            "{:?}",
            shell.editor.status()
        );
        let loaded = project_format::open_project(&first).unwrap();
        assert_eq!(loaded.document.layers.len(), 2, "the first save is intact");
    }

    /// W2-G round 2: File ▸ Export… (one file) composites and encodes on the
    /// worker; the shell keeps processing frames, nothing exists at the
    /// target until the worker has run, and the completion puts the file's
    /// name on the status line.
    #[test]
    fn a_file_export_runs_on_the_worker_while_the_shell_keeps_processing_frames() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("flat.png");
        let png = dir.path().join("large.png");
        write_noise_png(&png, 1024, 1024);
        let mut editor = crate::editor::Editor::with_state(
            AppPaths::rooted(dir.path().join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new().exporting_to(target.clone())),
        );
        editor.open_path(&png).unwrap();
        editor.set_spawner(queue_job);
        let mut shell = Shell::new(editor, Vec::new());

        shell.perform(Action::Export);
        assert!(
            !target.exists(),
            "nothing has been written: the export is a job and the worker has not run"
        );
        assert!(shell.editor.jobs_pending(), "the export is in flight");
        assert!(
            shell
                .editor
                .status()
                .is_some_and(|s| s.starts_with("Exporting ")),
            "{:?}",
            shell.editor.status()
        );
        for _ in 0..3 {
            assert!(shell.pump_jobs(), "still in flight");
            shell.apply_chrome(ChromeOutput::default());
        }
        assert!(!target.exists());

        assert_eq!(run_queued_jobs_on_a_worker(), 1);
        assert!(!shell.pump_jobs(), "nothing left in flight");
        assert_eq!(
            shell.editor.status(),
            Some(format!("Exported {}", target.display()).as_str())
        );
        let source = raster::decode_path(&png).unwrap();
        let exported = raster::decode_path(&target).unwrap();
        assert_eq!((exported.width, exported.height), (1024, 1024));
        let off = source
            .rgba8
            .iter()
            .zip(&exported.rgba8)
            .filter(|(a, b)| a.abs_diff(**b) > 1)
            .count();
        assert_eq!(off, 0, "the export carries the document's pixels");
    }

    /// W2-G round 2: a destination nothing can encode is refused on this
    /// thread, as the synchronous route refused it — an error dialog, no job.
    #[test]
    fn a_file_export_to_an_unknown_format_is_refused_before_any_job_starts() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("a.png");
        write_noise_png(&png, 256, 256);
        let mut editor = crate::editor::Editor::with_state(
            AppPaths::rooted(dir.path().join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new().exporting_to(dir.path().join("a.xyz"))),
        );
        editor.open_path(&png).unwrap();
        editor.set_spawner(queue_job);
        let mut shell = Shell::new(editor, Vec::new());
        let err = shell
            .editor
            .dispatch(Action::Export)
            .expect_err("no codec writes .xyz");
        assert!(matches!(err, ActionError::Failed { .. }), "{err:?}");
        assert!(err.to_string().contains("xyz"), "{err}");
        assert!(
            !shell.editor.jobs_pending(),
            "no job for a file nothing can write"
        );
        assert_eq!(QUEUED_JOBS.with(|q| q.borrow().len()), 0);
    }

    /// W2-G round 2: a command accepted while the worker writes a save is
    /// journaled aside, and lands in the new package's journal *after* the
    /// save marker once the save has landed — so crash recovery replays it.
    ///
    /// Before this, the record went into the old package's journal: either
    /// ahead of the marker (copied as "already in the snapshot") or into the
    /// directory the swap deleted. Recovery then replayed nothing of what
    /// followed the save, and stopped at the first later command that no
    /// longer applied.
    #[test]
    fn a_command_applied_during_a_save_is_replayed_by_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("held.rstudio");
        let mut shell = shell_with_a_large_document_and_queued_jobs(dir.path(), &target);

        // A first save, landed: the package exists and the document is clean.
        shell.perform(Action::Save);
        assert_eq!(run_queued_jobs_on_a_worker(), 1);
        assert!(!shell.pump_jobs());
        assert!(!shell.editor.active().unwrap().is_dirty());
        assert_eq!(
            shell.editor.active().unwrap().project_path(),
            Some(target.as_path())
        );

        // One edit before the save (in its snapshot), one during it (not).
        shell.editor.dispatch(Action::NewLayer).unwrap();
        shell.perform(Action::Save);
        assert!(shell.editor.saves_pending());
        shell.editor.dispatch(Action::NewLayer).unwrap();
        let hold = crate::editor::journal_hold_path(&target);
        assert!(
            hold.is_file(),
            "the command accepted during the save is held aside, not written into \
             a journal the swap is about to delete"
        );
        assert_eq!(
            shell.editor.active().unwrap().journal_hold(),
            Some(hold.as_path())
        );

        assert_eq!(run_queued_jobs_on_a_worker(), 1);
        assert!(!shell.pump_jobs());
        assert!(!hold.exists(), "absorbed into the new package's journal");
        assert!(shell.editor.active().unwrap().journal_hold().is_none());
        assert!(
            shell.editor.active().unwrap().is_dirty(),
            "the fourth layer is not on disk"
        );

        // Exactly the one command follows the marker, and it applies onto the
        // package as recovery would apply it.
        let rec = crate::session::recoverable(&target)
            .unwrap()
            .expect("the edit made during the save is recoverable");
        assert_eq!(rec.commands.len(), 1, "{rec:?}");
        let loaded = project_format::open_project(&target).unwrap();
        assert_eq!(loaded.document.layers.len(), 3, "the snapshot had three");
        let journal =
            project_format::CommandJournal::read(&target.join(project_format::JOURNAL_FILE))
                .unwrap();
        let mut recovered = loaded.document;
        assert_eq!(
            journal
                .replay_onto(&mut recovered, loaded.document_digest)
                .unwrap(),
            1
        );
        assert_eq!(recovered.layers.len(), 4);

        // And a command after the save goes to the package journal directly,
        // after the held one.
        shell.editor.dispatch(Action::NewLayer).unwrap();
        assert!(!hold.exists());
        assert_eq!(
            crate::session::recoverable(&target)
                .unwrap()
                .unwrap()
                .commands
                .len(),
            2
        );
    }

    /// W2-G round 2: the process dies after the worker swapped the new
    /// package in but before this thread applied the completion. The hold
    /// survives next to the package, and the next run's recovery absorbs it
    /// before it reads the journal — the command made during the save is
    /// restored along with the rest.
    #[test]
    fn a_crash_between_the_swap_and_the_completion_loses_no_command() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("crashed.rstudio");
        let mut shell = shell_with_a_large_document_and_queued_jobs(dir.path(), &target);
        shell.perform(Action::Save);
        assert_eq!(run_queued_jobs_on_a_worker(), 1);
        assert!(!shell.pump_jobs());

        shell.editor.dispatch(Action::NewLayer).unwrap();
        shell.perform(Action::Save);
        shell.editor.dispatch(Action::NewLayer).unwrap();
        // The worker finishes — the new package (three layers) is in place —
        // and then the process is gone before the next frame.
        assert_eq!(run_queued_jobs_on_a_worker(), 1);
        drop(shell);
        let hold = crate::editor::journal_hold_path(&target);
        assert!(hold.is_file(), "the hold outlives the process");
        assert_eq!(
            project_format::load_project(&target).unwrap().layers.len(),
            3
        );

        // The next run.
        let mut editor = crate::editor::Editor::with_state(
            AppPaths::rooted(dir.path().join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new().answering_recover(true)),
        );
        let report = editor.recover(&crate::session::SessionRecord {
            pid: 0,
            open_projects: vec![target.clone()],
            autosaves: Vec::new(),
        });
        assert_eq!(report.restored, vec![(target.clone(), 1)], "{report:?}");
        assert!(report.failed.is_empty(), "{report:?}");
        assert!(!hold.exists(), "absorbed exactly once");
        assert_eq!(editor.active().unwrap().document.layers.len(), 4);
    }

    /// W2-G round 2: Ctrl+S while the timer's autosave is writing the same
    /// document is not dropped — it is queued and starts the moment the
    /// autosave lands, and no second writer touches the package meanwhile.
    #[test]
    fn a_save_pressed_during_an_autosave_runs_after_it() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("queued.rstudio");
        let mut shell = shell_with_a_large_document_and_queued_jobs(dir.path(), &target);
        let t0 = Instant::now();
        assert!(shell.editor.autosave_tick(t0).is_none());
        let due = shell.editor.next_autosave().unwrap();
        let report = shell.editor.autosave_tick(due).expect("a dirty document");
        assert_eq!(report.started.len(), 1);
        assert_eq!(QUEUED_JOBS.with(|q| q.borrow().len()), 1);

        shell.perform(Action::Save);
        let status = shell.editor.status().unwrap_or_default().to_string();
        assert!(
            !status.contains("already being saved"),
            "the user's save is not refused behind an autosave: {status:?}"
        );
        assert!(status.contains("once the autosave finishes"), "{status:?}");
        assert_eq!(
            QUEUED_JOBS.with(|q| q.borrow().len()),
            1,
            "no second writer to the package while the autosave runs"
        );

        // The autosave lands; the same frame starts the queued save.
        assert_eq!(run_queued_jobs_on_a_worker(), 1);
        assert!(shell.pump_jobs(), "the queued save is now in flight");
        assert_eq!(QUEUED_JOBS.with(|q| q.borrow().len()), 1);
        assert!(shell.editor.active().unwrap().is_dirty());
        assert!(!target.exists());
        assert!(
            shell
                .editor
                .status()
                .is_some_and(|s| s.starts_with("Saving ")),
            "{:?}",
            shell.editor.status()
        );

        assert_eq!(run_queued_jobs_on_a_worker(), 1);
        assert!(!shell.pump_jobs());
        let doc = shell.editor.active().unwrap();
        assert!(!doc.is_dirty(), "the queued save landed");
        assert_eq!(doc.project_path(), Some(target.as_path()));
        assert!(target.join(project_format::MANIFEST_FILE).is_file());
    }

    /// W2-G round 2: a save's progress refreshes the status line only while
    /// the line still shows that save's own text; a newer message — the
    /// refusal of a second Ctrl+S, anything else — is not overwritten by the
    /// tile count moving.
    #[test]
    fn a_saves_progress_does_not_overwrite_a_newer_status_line() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("progress.rstudio");
        let mut shell = shell_with_a_large_document_and_queued_jobs(dir.path(), &target);
        shell.perform(Action::Save);
        let id = shell.editor.active().unwrap().id();
        let progress = shell.editor.save_progress_of(id).expect("a save in flight");
        assert!(
            shell
                .editor
                .status()
                .is_some_and(|s| s.starts_with("Saving ")),
            "{:?}",
            shell.editor.status()
        );

        // The worker counts its tiles while the line shows its own text: the
        // count is shown.
        progress.set_total(16);
        progress.bump();
        assert!(shell.pump_jobs());
        assert_eq!(
            shell.editor.status(),
            // W5-D: the document is named after the file it is saved to.
            Some("Saving progress… 1/16 tiles"),
            "progress refreshes its own line"
        );

        // Something newer claims the line; further progress leaves it alone.
        shell.perform(Action::Save);
        assert!(
            shell
                .editor
                .status()
                .is_some_and(|s| s.contains("already being saved")),
            "{:?}",
            shell.editor.status()
        );
        progress.bump();
        assert!(shell.pump_jobs());
        assert!(
            shell
                .editor
                .status()
                .is_some_and(|s| s.contains("already being saved")),
            "the refusal survived a progress tick: {:?}",
            shell.editor.status()
        );
        shell.editor.set_status("Layer added");
        progress.bump();
        assert!(shell.pump_jobs());
        assert_eq!(shell.editor.status(), Some("Layer added"));

        // The completion is news, and says its piece.
        assert_eq!(run_queued_jobs_on_a_worker(), 1);
        assert!(!shell.pump_jobs());
        assert!(
            shell
                .editor
                .status()
                .is_some_and(|s| s.starts_with("Saved ")),
            "{:?}",
            shell.editor.status()
        );
    }

    /// W2-G reachability: the editor the desktop binary runs
    /// (`Editor::native` → `Editor::new`) starts its jobs on worker threads —
    /// the inline mode belongs to `with_state` alone.
    #[test]
    fn the_desktop_editor_runs_jobs_on_worker_threads() {
        let dir = tempfile::tempdir().unwrap();
        let editor = crate::editor::Editor::new(
            AppPaths::rooted(dir.path().join("config")),
            Box::new(ScriptedDialogs::new()),
        );
        assert!(std::ptr::fn_addr_eq(
            editor.spawner(),
            crate::jobs::spawn_thread as crate::jobs::Spawner
        ));
        let inline = crate::editor::Editor::with_state(
            AppPaths::rooted(dir.path().join("config2")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        );
        assert!(std::ptr::fn_addr_eq(
            inline.spawner(),
            crate::jobs::run_inline as crate::jobs::Spawner
        ));
    }

    /// W2-G: `--shot` opens the fixed capture geometry whatever the last
    /// session persisted, and persists nothing on the way out.
    #[test]
    fn a_shot_window_ignores_persisted_geometry_and_never_stores_its_own() {
        let persisted = WindowGeometry {
            x: -3,
            y: 12,
            width: 2560,
            height: 1351,
            maximized: true,
        };
        assert_eq!(
            window_geometry_for(Some(persisted), true),
            WindowGeometry::DEFAULT
        );
        let shot = window_geometry_for(None, true);
        assert_eq!(shot, WindowGeometry::DEFAULT);
        assert!(!shot.maximized, "a capture window is never maximized");
        assert_eq!((shot.width, shot.height), (1440, 900));
        // An ordinary session restores where it was, sanitized.
        assert_eq!(
            window_geometry_for(Some(persisted), false),
            persisted.sanitized()
        );
        assert_eq!(window_geometry_for(None, false), WindowGeometry::DEFAULT);

        // And a shot shell's exit leaves the persisted record alone.
        let dir = tempfile::tempdir().unwrap();
        let prefs = Preferences {
            window: Some(persisted),
            ..Preferences::default()
        };
        let editor = crate::editor::Editor::with_state(
            AppPaths::rooted(dir.path().join("config")),
            prefs,
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        );
        let mut shell = Shell::with_shot(editor, Vec::new(), Some(dir.path().join("shot.png")));
        shell.capture_geometry();
        assert_eq!(
            shell.editor.preferences().window,
            Some(persisted.sanitized()),
            "a shot run must not overwrite the real session's geometry"
        );
    }

    #[test]
    fn a_ninety_degree_rotation_through_the_arbitrary_dialog_matches_the_fixed_one() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("in.png");
        let mut state = 0x51ed_270bu32;
        let mut px = Vec::with_capacity(40 * 30 * 4);
        for _ in 0..40 * 30 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            px.extend_from_slice(&[(state >> 16) as u8, (state >> 8) as u8, state as u8, 255]);
        }
        std::fs::write(
            &png,
            raster::encode(raster::ExportFormat::Png, 40, 30, &px).unwrap(),
        )
        .unwrap();

        // Doc A: through the dialog's confirmed answer.
        let mut a = crate::editor::Editor::with_state(
            AppPaths::rooted(dir.path().join("a")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        );
        a.open_path(&png).unwrap();
        let mut shell = Shell::new(a, Vec::new());
        shell.apply_chrome(ChromeOutput {
            dialog: Some(DialogAction::RotateCanvas(90.0)),
            ..Default::default()
        });

        // Doc B: the fixed menu item.
        let mut b = crate::editor::Editor::with_state(
            AppPaths::rooted(dir.path().join("b")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        );
        b.open_path(&png).unwrap();
        crate::menu_bridge::perform(
            ui::menu::MenuAction::RotateCanvas(ui::menu::CanvasRotation::Deg90Cw),
            &mut b,
        )
        .unwrap();

        let ca = shell.editor().active().unwrap();
        let cb = b.active().unwrap();
        assert_eq!((ca.document.width(), ca.document.height()), (30, 40));
        assert_eq!(
            ca.export_preview(512).unwrap(),
            cb.export_preview(512).unwrap(),
            "90° through the dialog differed from the fixed 90°"
        );
    }

    #[test]
    fn an_arbitrary_angle_grows_the_canvas_resamples_and_undoes() {
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_with_one_image(dir.path());
        let before = shell
            .editor()
            .active()
            .unwrap()
            .export_preview(512)
            .unwrap();

        shell.apply_chrome(ChromeOutput {
            dialog: Some(DialogAction::RotateCanvas(37.0)),
            ..Default::default()
        });
        let doc = shell.editor().active().unwrap();
        let (w, h) = (doc.document.width(), doc.document.height());
        assert!(
            w > 16 && h > 16,
            "the canvas grew to the rotated bbox: {w}x{h}"
        );
        assert_ne!(
            doc.export_preview(512).unwrap(),
            before,
            "rotating 37° changed no pixel"
        );
    }

    #[test]
    fn applying_from_the_filter_gallery_matches_the_menu_item() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("noise.png");
        let mut state = 0x2f6a_88c1u32;
        let mut px = Vec::with_capacity(16 * 16 * 4);
        for _ in 0..16 * 16 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            px.extend_from_slice(&[(state >> 16) as u8, (state >> 8) as u8, state as u8, 255]);
        }
        std::fs::write(
            &png,
            raster::encode(raster::ExportFormat::Png, 16, 16, &px).unwrap(),
        )
        .unwrap();

        // Doc A: confirm the gallery with its default selection (the first
        // catalogue entry at its default parameters).
        let mut a = crate::editor::Editor::with_state(
            AppPaths::rooted(dir.path().join("a")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        );
        a.open_path(&png).unwrap();
        let mut shell = Shell::new(a, Vec::new());
        let gallery_spec = {
            let source = crate::menu_bridge::filter_source(shell.editor()).unwrap();
            ui::dialogs::FilterGalleryDialog::new(source)
        };
        let confirmed = ui::dialogs::Dialog::confirm(&gallery_spec).unwrap();
        shell.apply_chrome(ChromeOutput {
            dialog: Some(confirmed),
            ..Default::default()
        });

        // Doc B: the menu item for the same filter, at its defaults.
        let mut b = crate::editor::Editor::with_state(
            AppPaths::rooted(dir.path().join("b")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        );
        b.open_path(&png).unwrap();
        crate::menu_bridge::perform(
            ui::menu::MenuAction::Filter(gallery_spec.selected_filter().id),
            &mut b,
        )
        .unwrap();

        let ca = shell.editor().active().unwrap();
        let cb = b.active().unwrap();
        assert_eq!(
            ca.export_preview(512).unwrap(),
            cb.export_preview(512).unwrap(),
            "the gallery and the menu item disagreed about the pixels"
        );
    }

    #[test]
    fn a_drop_shadow_set_through_the_layer_style_dialog_is_one_undoable_step() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.png");
        // An opaque block on a transparent canvas: a shadow behind a fully
        // opaque, edge-to-edge layer would never be visible.
        let mut state = 0x7f4a_c921u32;
        let mut px = Vec::with_capacity(32 * 32 * 4);
        for y in 0..32u32 {
            for x in 0..32u32 {
                let inside = (8..24).contains(&x) && (8..24).contains(&y);
                if inside {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    px.extend_from_slice(&[
                        (state >> 16) as u8,
                        (state >> 8) as u8,
                        state as u8,
                        255,
                    ]);
                } else {
                    px.extend_from_slice(&[0, 0, 0, 0]);
                }
            }
        }
        std::fs::write(
            &p,
            raster::encode(raster::ExportFormat::Png, 32, 32, &px).unwrap(),
        )
        .unwrap();
        let mut ed = crate::editor::Editor::with_state(
            AppPaths::rooted(dir.path().join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        );
        ed.open_path(&p).unwrap();
        let layer = ed.active().unwrap().document.active_layer().unwrap();
        let composite_before = ed.active().unwrap().export_preview(512).unwrap();
        let history_before = ed.active().unwrap().history.journal().count();

        // The dialog the menu row opens, with Drop Shadow switched on: the
        // same state mutation the dialog's effect list performs.
        let effects = ed
            .active()
            .unwrap()
            .document
            .layers
            .get(layer)
            .unwrap()
            .effects
            .clone();
        let mut dialog = ui::dialogs::LayerStyleDialog::new(layer, "a.png", effects);
        dialog.set_enabled(ui::dialogs::EffectKind::DropShadow, true);
        let action =
            ui::dialogs::Dialog::confirm(&dialog).expect("a dialog with an effect on can confirm");

        // Through the shell: the confirmed command is one history entry, the
        // composite changes, and undo takes the shadow off again.
        let mut shell = Shell::new(ed, Vec::new());
        shell.apply_chrome(ChromeOutput {
            commands: match action {
                ui::dialogs::DialogAction::Command(command) => vec![*command],
                other => panic!("the layer style dialog confirmed to {other:?}"),
            },
            ..Default::default()
        });
        let open = shell.editor().active().unwrap();
        assert_eq!(
            open.history.journal().count(),
            history_before + 1,
            "the shadow was not exactly one history entry"
        );
        assert_ne!(
            open.export_preview(512).unwrap(),
            composite_before,
            "the shadow changed no pixel"
        );

        // Undo — driven through the shell's own action channel, the same road
        // Ctrl+Z takes — removes the shadow entirely.
        shell.apply_chrome(ChromeOutput {
            actions: vec![Action::Undo],
            ..Default::default()
        });
        let open = shell.editor().active().unwrap();
        assert_eq!(
            open.history.journal().count(),
            history_before,
            "undo did not remove the history entry"
        );
        assert_eq!(
            open.export_preview(512).unwrap(),
            composite_before,
            "undo did not restore the pixels"
        );
    }

    #[test]
    fn a_confirmed_filter_dialog_runs_at_radius_zero_and_eight() {
        let dir = tempfile::tempdir().unwrap();
        // Noise, not a flat fill: a uniform image blurred is still uniform.
        let png = dir.path().join("noise.png");
        let mut state = 0x68e3_1a0fu32;
        let mut px = Vec::with_capacity(16 * 16 * 4);
        for _ in 0..16 * 16 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            px.extend_from_slice(&[(state >> 16) as u8, (state >> 8) as u8, state as u8, 255]);
        }
        std::fs::write(
            &png,
            raster::encode(raster::ExportFormat::Png, 16, 16, &px).unwrap(),
        )
        .unwrap();
        let mut editor = crate::editor::Editor::with_state(
            AppPaths::rooted(dir.path().join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        );
        editor.open_path(&png).unwrap();
        let mut shell = Shell::new(editor, Vec::new());
        let spec = ui::dialogs::filter_by_id(ui::menu::FilterId::GaussianBlur).unwrap();

        // Radius 0 is the identity: the pixels do not change, and the shell
        // says so instead of recording an undo step that does nothing.
        let mut identity = ui::dialogs::FilterParams::defaults(spec.params);
        identity.set("radius", ui::dialogs::ParamValue::Float(0.0));
        let before = shell
            .editor()
            .active()
            .unwrap()
            .export_preview(512)
            .unwrap();
        let undo_before = shell.editor().active().unwrap().history.undo_depth();
        shell.apply_chrome(ChromeOutput {
            dialog: Some(DialogAction::RunFilter(Box::new(
                ui::dialogs::FilterInvocation {
                    filter: spec,
                    params: identity,
                },
            ))),
            ..Default::default()
        });
        let open = shell.editor().active().unwrap();
        assert_eq!(
            open.export_preview(512).unwrap(),
            before,
            "radius 0 changed the pixels"
        );
        assert_eq!(
            open.history.undo_depth(),
            undo_before,
            "radius 0 recorded an undo step"
        );

        // Radius 8 blurs, as exactly one undoable step.
        let mut blurred = ui::dialogs::FilterParams::defaults(spec.params);
        blurred.set("radius", ui::dialogs::ParamValue::Float(8.0));
        shell.apply_chrome(ChromeOutput {
            dialog: Some(DialogAction::RunFilter(Box::new(
                ui::dialogs::FilterInvocation {
                    filter: spec,
                    params: blurred,
                },
            ))),
            ..Default::default()
        });
        let open = shell.editor().active().unwrap();
        assert_ne!(
            open.export_preview(512).unwrap(),
            before,
            "radius 8 changed nothing"
        );
        assert_eq!(
            open.history.undo_depth(),
            undo_before + 1,
            "the blur was not exactly one undo step"
        );
    }

    #[test]
    fn a_confirmed_canvas_size_dialog_reframes_through_history() {
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_with_one_image(dir.path());

        let spec = ui::dialogs::CanvasSizeSpec {
            width: 32,
            height: 32,
            offset: ui::dialogs::Anchor::TopLeft.offset((16, 16), (32, 32)),
            anchor: ui::dialogs::Anchor::TopLeft,
            background: ui::dialogs::BackgroundContents::Transparent,
        };
        shell.apply_chrome(ChromeOutput {
            dialog: Some(ui::dialogs::DialogAction::ResizeCanvas(spec)),
            ..Default::default()
        });
        let doc = shell.editor().active().unwrap();
        assert_eq!((doc.document.width(), doc.document.height()), (32, 32));
        assert!(
            doc.history.undo_depth() > 0,
            "the re-frame is one undoable step"
        );
    }

    #[test]
    fn a_confirmed_image_size_dialog_resamples_through_history() {
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_with_one_image(dir.path());
        assert_eq!(shell.editor().active().unwrap().document.width(), 16);

        let spec = ui::dialogs::ImageSizeSpec {
            width: 8,
            height: 8,
            resolution_ppi: 72.0,
            resample: Some(raster::ResampleFilter::Triangle),
        };
        shell.apply_chrome(ChromeOutput {
            dialog: Some(ui::dialogs::DialogAction::ResizeImage(spec)),
            ..Default::default()
        });
        let doc = shell.editor().active().unwrap();
        assert_eq!((doc.document.width(), doc.document.height()), (8, 8));
        assert!(
            doc.history.undo_depth() > 0,
            "the resample is one undoable step"
        );
    }

    #[test]
    fn a_confirmed_new_document_dialog_creates_the_document_it_confirmed() {
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_with_one_image(dir.path());
        let before = shell.editor().documents().len();

        let spec = ui::dialogs::NewDocumentSpec {
            title: "Poster".to_string(),
            width: 1920,
            height: 1080,
            resolution_ppi: 72.0,
            color_mode: ui::dialogs::ColorMode::Rgb,
            color_space: color::ColorSpace::Srgb,
            bit_depth: raster::BitDepth::Eight,
            background: ui::dialogs::BackgroundContents::Transparent,
            artboard: false,
        };
        shell.apply_chrome(ChromeOutput {
            dialog: Some(ui::dialogs::DialogAction::NewDocument(Box::new(spec))),
            ..Default::default()
        });

        let open = shell
            .editor()
            .active()
            .expect("the confirmed document is open");
        assert_eq!(open.document.width(), 1920);
        assert_eq!(open.document.height(), 1080);
        assert_eq!(shell.editor().documents().len(), before + 1);
        // A transparent background is *no* background: the base layer holds no
        // tiles, so every pixel composites fully transparent.
        let layer = open.document.active_layer().unwrap();
        // The thumbnail preserves the document's aspect ratio (16:9 here).
        let (_, _, rgba) = open.layer_thumbnail(layer, 32).unwrap();
        assert!(
            rgba.iter().all(|&b| b == 0),
            "the base layer was not transparent"
        );
    }

    #[test]
    fn a_confirmed_new_document_dialog_honours_a_solid_background() {
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_with_one_image(dir.path());

        let spec = ui::dialogs::NewDocumentSpec {
            title: "White".to_string(),
            width: 300,
            height: 300,
            resolution_ppi: 72.0,
            color_mode: ui::dialogs::ColorMode::Rgb,
            color_space: color::ColorSpace::Srgb,
            bit_depth: raster::BitDepth::Eight,
            background: ui::dialogs::BackgroundContents::White,
            artboard: false,
        };
        shell.apply_chrome(ChromeOutput {
            dialog: Some(ui::dialogs::DialogAction::NewDocument(Box::new(spec))),
            ..Default::default()
        });

        let open = shell.editor().active().unwrap();
        let layer = open.document.active_layer().unwrap();
        // 300×300 crosses one tile boundary, so the edge-tile zeroing ran too.
        let (_, _, rgba) = open.layer_thumbnail(layer, 32).unwrap();
        assert!(
            rgba.as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [255, 255, 255, 255]),
            "the white background did not composite as white"
        );
    }

    /// W10-B: the Glyphs panel's pick, performed by the shell: into the live
    /// session's draft at the caret (not the committed text the draft would
    /// overwrite), confirmed with the run as one step; with no session, onto
    /// the end of the layer's text as its own step.
    #[test]
    fn a_glyph_pick_lands_at_the_live_sessions_caret_through_the_shell() {
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_with_one_image(dir.path());
        let layer = layer_model::Layer::with_kind(
            "Glyph",
            layer_model::LayerKind::Text(layer_model::TextLayer {
                text: "ab".to_string(),
                font_family: "DejaVu Sans".to_string(),
                size_px: 32.0,
                ..layer_model::TextLayer::default()
            }),
        );
        let id = layer.id;
        shell
            .editor
            .apply_command(editor_core::Command::create_layer(layer));
        let text_of = |shell: &Shell| match &shell
            .editor
            .active()
            .unwrap()
            .document
            .layers
            .get(id)
            .unwrap()
            .kind
        {
            layer_model::LayerKind::Text(t) => t.text.clone(),
            _ => unreachable!(),
        };
        shell.pointer.enter_text_session(&mut shell.editor, id);
        assert!(shell.pointer.is_text_editing());
        shell.pointer.text_edit(
            &mut shell.editor,
            tools::TextEdit::CaretStep {
                back: true,
                extend: false,
            },
        );
        let depth = shell.editor.active().unwrap().history_depth();
        let pick = |text: &str| ChromeOutput {
            insert_glyphs: vec![(id, text.to_string())],
            ..Default::default()
        };
        shell.apply_chrome(pick("\u{2014}"));
        assert_eq!(text_of(&shell), "a\u{2014}b", "at the caret, in the draft");
        assert!(shell.pointer.is_text_editing(), "the session goes on");
        assert_eq!(shell.editor.active().unwrap().history_depth(), depth);
        let out = shell
            .pointer
            .text_edit(&mut shell.editor, tools::TextEdit::Confirm);
        assert_eq!(out.steps, 1, "{out:?}");
        assert_eq!(text_of(&shell), "a\u{2014}b", "the confirm kept the glyph");

        // No session: appended to the committed text, one undo step.
        shell.apply_chrome(pick("!"));
        assert_eq!(text_of(&shell), "a\u{2014}b!");
        shell.editor.dispatch(crate::Action::Undo).unwrap();
        assert_eq!(text_of(&shell), "a\u{2014}b");
    }

    #[test]
    fn ctrl_enter_confirms_and_plain_enter_breaks_the_line_in_a_text_session() {
        // Card 031's core keybinding, at the SHELL route level (the tool-level
        // arms cannot catch a routing regression).
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_with_one_image(dir.path());
        let layer = layer_model::Layer::with_kind(
            "Headline",
            layer_model::LayerKind::Text(layer_model::TextLayer {
                text: "top".to_string(),
                font_family: "DejaVu Sans".to_string(),
                size_px: 32.0,
                ..layer_model::TextLayer::default()
            }),
        );
        let id = layer.id;
        shell
            .editor
            .apply_command(editor_core::Command::create_layer(layer));
        shell.pointer.enter_text_session(&mut shell.editor, id);
        assert!(shell.pointer.is_text_editing(), "the session is live");
        let depth_before_session = shell.editor.active().unwrap().history_depth();

        let owner = KeyboardOwner::default();
        // Plain Enter: a line break, session still open.
        press(
            &mut shell,
            owner,
            WKey::Named(NamedKey::Enter),
            ModifiersState::empty(),
        );
        assert!(shell.pointer.is_text_editing(), "plain Enter keeps typing");
        let doc = shell.editor.active().unwrap();
        let text = match &doc.document.layers.get(id).unwrap().kind {
            layer_model::LayerKind::Text(t) => t.text.clone(),
            _ => panic!("still a text layer"),
        };
        assert!(
            text.contains('\n'),
            "plain Enter inserted a break: {text:?}"
        );

        // While an IME composition is live, Enter (with or without Ctrl)
        // belongs to the platform — consumed, session untouched.
        shell
            .pointer
            .text_edit(&mut shell.editor, tools::TextEdit::SetComposition("kanji"));
        assert!(shell.pointer.text_composing(&shell.editor));
        press(
            &mut shell,
            owner,
            WKey::Named(NamedKey::Enter),
            ModifiersState::CONTROL,
        );
        assert!(
            shell.pointer.is_text_editing(),
            "Ctrl+Enter during composition is the IME's"
        );
        shell
            .pointer
            .text_edit(&mut shell.editor, tools::TextEdit::CommitIme("kanji"));

        // Ctrl+Enter: confirm — the run commits as one transaction and the
        // session closes.
        press(
            &mut shell,
            owner,
            WKey::Named(NamedKey::Enter),
            ModifiersState::CONTROL,
        );
        assert!(
            !shell.pointer.is_text_editing(),
            "Ctrl+Enter confirms the session"
        );
        let doc = shell.editor.active().unwrap();
        let text = match &doc.document.layers.get(id).unwrap().kind {
            layer_model::LayerKind::Text(t) => t.text.clone(),
            _ => panic!("still a text layer"),
        };
        assert!(
            text.contains("kanji"),
            "the committed text landed: {text:?}"
        );
        assert!(text.contains('\n'), "the line break survived the commit");
        // Card 031's undo contract: the whole run is ONE history entry.
        assert_eq!(
            shell.editor.active().unwrap().history_depth(),
            depth_before_session + 1,
            "typing a sentence is one document undo"
        );
    }

    // ---------------------------------------------------------------- card 049

    fn shell_with_canvas_only(dir: &std::path::Path) -> Shell {
        let png = dir.join("canvas.png");
        std::fs::write(
            &png,
            raster::encode(raster::ExportFormat::Png, 32, 32, &[9u8; 32 * 32 * 4]).unwrap(),
        )
        .unwrap();
        let mut editor = crate::editor::Editor::with_state(
            AppPaths::rooted(dir.join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        );
        editor.open_path(&png).unwrap();
        Shell::new(editor, Vec::new())
    }

    fn write_source_png(dir: &std::path::Path, name: &str, value: u8) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(
            &path,
            raster::encode(raster::ExportFormat::Png, 16, 16, &[value; 16 * 16 * 4]).unwrap(),
        )
        .unwrap();
        path
    }

    /// Card 049's done-check: dropping a portrait into a composition adds a
    /// LAYER to the open document instead of opening a second image tab.
    #[test]
    fn dropping_an_image_on_an_open_canvas_places_it_as_a_layer() {
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_with_canvas_only(dir.path());
        let portrait = write_source_png(dir.path(), "portrait.png", 200);
        let docs_before = shell.editor.documents().len();
        let layers_before = shell
            .editor
            .active()
            .unwrap()
            .document
            .layers
            .iter_depth_first()
            .len();

        shell.on_dropped_files(&[portrait]);

        assert_eq!(
            shell.editor.documents().len(),
            docs_before,
            "the drop did not open a second tab"
        );
        let open = shell.editor.active().unwrap();
        assert_eq!(
            open.document.layers.iter_depth_first().len(),
            layers_before + 1,
            "the drop added one placed layer"
        );
        assert!(open
            .document
            .layers
            .iter_depth_first()
            .into_iter()
            .any(|id| matches!(
                open.document.layers.get(id).map(|l| &l.kind),
                Some(layer_model::LayerKind::SmartObject(_))
            )));
    }

    /// Card 049: dropping a native project still OPENS it.
    #[test]
    fn dropping_a_native_project_opens_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_with_canvas_only(dir.path());
        // A native package saved from a helper document.
        let source_png = write_source_png(dir.path(), "proj-src.png", 60);
        let mut proj = crate::editor::Editor::with_state(
            AppPaths::rooted(dir.path().join("cfg2")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        );
        proj.open_path(&source_png).unwrap();
        let package = dir.path().join("project.rstudio");
        proj.active_mut()
            .unwrap()
            .save_to(&package, "test")
            .unwrap();

        let docs_before = shell.editor.documents().len();
        shell.on_dropped_files(&[package]);

        assert_eq!(
            shell.editor.documents().len(),
            docs_before + 1,
            "the dropped project opened as its own document"
        );
    }

    /// Card 049: dropping with NO document open opens the image.
    #[test]
    fn dropping_with_no_document_open_opens_the_image() {
        let dir = tempfile::tempdir().unwrap();
        let editor = crate::editor::Editor::with_state(
            AppPaths::rooted(dir.path().join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        );
        assert!(editor.active().is_none(), "the fixture starts empty");
        let mut shell = Shell::new(editor, Vec::new());
        let portrait = write_source_png(dir.path(), "first.png", 40);

        shell.on_dropped_files(&[portrait]);

        assert_eq!(
            shell.editor.documents().len(),
            1,
            "the drop opened the image as a document"
        );
    }

    /// Card 049: multiple dropped files preserve order and a failure is
    /// reported without blocking the files after it.
    #[test]
    fn multiple_dropped_files_place_in_order_and_report_failures() {
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_with_canvas_only(dir.path());
        let first = write_source_png(dir.path(), "first.png", 40);
        let garbage = dir.path().join("broken.png");
        std::fs::write(&garbage, b"definitely not a png").unwrap();
        let second = write_source_png(dir.path(), "second.png", 80);

        shell.on_dropped_files(&[first, garbage, second]);

        let open = shell.editor.active().unwrap();
        // Order preserved: two placed layers exist (the garbage file reported,
        // not blocking the second image).
        let placed: Vec<_> = open
            .document
            .layers
            .iter_depth_first()
            .into_iter()
            .filter(|id| {
                matches!(
                    open.document.layers.get(*id).map(|l| &l.kind),
                    Some(layer_model::LayerKind::SmartObject(_))
                )
            })
            .collect();
        assert_eq!(placed.len(), 2, "both images placed despite the failure");
        // iter_depth_first is top-most first, so the FIRST dropped file's
        // layer sits at the END of the placed list.
        let last = placed.last().unwrap();
        let first_asset = match &open.document.layers.get(*last).unwrap().kind {
            layer_model::LayerKind::SmartObject(so) => so.asset,
            other => panic!("smart object: {other:?}"),
        };
        let origin = open.document.asset_origin(first_asset);
        assert!(
            matches!(
                origin,
                Some(layer_model::AssetOrigin::Embedded { ref name, .. }) if name == "first"
            ),
            "the order is preserved: the first dropped file placed first: {origin:?}"
        );
        // The failure was reported in the status.
        let status = shell.editor.status().unwrap_or("");
        assert!(
            status.contains("Drop failed"),
            "the failed file was reported: {status:?}"
        );
    }

    /// XB: the W/H readout reaches the running app. A shape drag fed through
    /// the shell's own pointer path (the one `window_event` uses) must leave
    /// the chrome painting both numbers on its next frame, and the release
    /// must take the label down — no hand-published readout anywhere.
    #[test]
    fn a_shape_drag_through_the_shell_paints_the_w_h_readout_until_release() {
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_ready_to_draw(dir.path());
        shell.editor.set_tool(tools::ToolId::Rectangle);
        let ctx = egui::Context::default();
        crate::chrome::install_theme(&ctx, design::Theme::Dark);
        let painted_text = |shell: &mut Shell| -> Vec<String> {
            let mut texts = Vec::new();
            // Two passes: the first frame is where egui learns the sizes.
            for _ in 0..2 {
                let output = ctx.run(egui::RawInput::default(), |ctx| {
                    let _ = shell.chrome.ui(ctx, &mut shell.editor);
                });
                texts = output
                    .shapes
                    .iter()
                    .filter_map(|clipped| match &clipped.shape {
                        egui::Shape::Text(t) => Some(t.galley.text().to_string()),
                        _ => None,
                    })
                    .collect();
            }
            texts
        };
        let has_readout = |texts: &[String]| {
            texts
                .iter()
                .any(|t| t.contains("W: 10 px") && t.contains("H: 7 px"))
        };

        point(&mut shell, PointerPhase::Down, Vec2::new(2.0, 3.0), false);
        point(&mut shell, PointerPhase::Move, Vec2::new(12.0, 10.0), false);
        let texts = painted_text(&mut shell);
        assert!(
            has_readout(&texts),
            "a shape drag through the shell painted no W/H readout: {texts:?}"
        );

        point(&mut shell, PointerPhase::Up, Vec2::new(12.0, 10.0), false);
        let texts = painted_text(&mut shell);
        assert!(
            !texts.iter().any(|t| t.contains("W: 10 px")),
            "the released drag still paints a readout: {texts:?}"
        );
    }

    #[test]
    fn escape_mid_shape_drag_takes_the_w_h_readout_down() {
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_ready_to_draw(dir.path());
        shell.editor.set_tool(tools::ToolId::Rectangle);
        let ctx = egui::Context::default();
        crate::chrome::install_theme(&ctx, design::Theme::Dark);
        let painted_text = |shell: &mut Shell| -> Vec<String> {
            let mut texts = Vec::new();
            for _ in 0..2 {
                let output = ctx.run(egui::RawInput::default(), |ctx| {
                    let _ = shell.chrome.ui(ctx, &mut shell.editor);
                });
                texts = output
                    .shapes
                    .iter()
                    .filter_map(|clipped| match &clipped.shape {
                        egui::Shape::Text(t) => Some(t.galley.text().to_string()),
                        _ => None,
                    })
                    .collect();
            }
            texts
        };

        point(&mut shell, PointerPhase::Down, Vec2::new(2.0, 3.0), false);
        point(&mut shell, PointerPhase::Move, Vec2::new(12.0, 10.0), false);
        let texts = painted_text(&mut shell);
        assert!(
            texts
                .iter()
                .any(|t| t.contains("W: 10 px") && t.contains("H: 7 px")),
            "precondition: the drag paints its W/H readout: {texts:?}"
        );

        // Escape (and focus loss) route through abandon_gesture, with no
        // pointer sample after it.
        assert!(shell.abandon_gesture(), "the live drag was cancelled");
        let texts = painted_text(&mut shell);
        assert!(
            !texts.iter().any(|t| t.contains("W: 10 px")),
            "a cancelled drag still paints its readout: {texts:?}"
        );
    }

    /// W4-A: how many shapes the chrome paints for the shell's current
    /// state, settled over two frames (the first lays panels out).
    fn painted_shape_count(ctx: &egui::Context, shell: &mut Shell) -> usize {
        let mut count = 0;
        for _ in 0..2 {
            let output = ctx.run(egui::RawInput::default(), |ctx| {
                let _ = shell.chrome.ui(ctx, &mut shell.editor);
            });
            count = output.shapes.len();
        }
        count
    }

    /// W4-A: put `tool` through `gesture` on the shell, then press `key`
    /// through the real key route (`on_key`), with NO pointer sample after
    /// it — and require the overlay the gesture published to be gone, both
    /// from the chrome's published session and from the painted frame.
    fn key_takes_the_overlay_down(
        tool: tools::ToolId,
        gesture: impl Fn(&mut Shell),
        key: NamedKey,
        expect: impl Fn(&tools::SessionGeometry) -> bool,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_ready_to_draw(dir.path());
        shell.editor.set_tool(tool);
        let ctx = egui::Context::default();
        crate::chrome::install_theme(&ctx, design::Theme::Dark);
        let idle = painted_shape_count(&ctx, &mut shell);

        gesture(&mut shell);
        let session = shell.chrome.live_session().cloned();
        assert!(
            session.as_ref().is_some_and(&expect),
            "precondition: {tool:?} published its overlay: {session:?}"
        );
        let live = painted_shape_count(&ctx, &mut shell);
        assert!(
            live > idle,
            "precondition: {tool:?}'s overlay paints ({live} shapes vs {idle} idle)"
        );

        press(
            &mut shell,
            KeyboardOwner::default(),
            WKey::Named(key),
            ModifiersState::empty(),
        );
        assert!(
            shell.chrome.live_session().is_none(),
            "{key:?} left {tool:?}'s overlay published: {:?}",
            shell.chrome.live_session()
        );
        let after = painted_shape_count(&ctx, &mut shell);
        // What the next pointer sample would paint: the shell's own pointer
        // route republishes, so this frame has no stale overlay by
        // construction. A commit changes the document (history row, crop
        // size, stored path), so that — not the idle frame — is the yardstick
        // for Enter; Escape must also match the untouched idle frame.
        point(&mut shell, PointerPhase::Move, Vec2::new(8.0, 8.0), false);
        assert!(shell.chrome.live_session().is_none());
        let settled = painted_shape_count(&ctx, &mut shell);
        assert_eq!(
            after, settled,
            "{key:?} left {tool:?}'s overlay painted until the next pointer sample"
        );
        if key == NamedKey::Escape {
            assert_eq!(after, idle, "Escape left {tool:?}'s overlay painted");
        }
    }

    fn drag(shell: &mut Shell, from: Vec2, to: Vec2) {
        point(shell, PointerPhase::Down, from, false);
        point(shell, PointerPhase::Move, (from + to) * 0.5, false);
        point(shell, PointerPhase::Move, to, false);
        point(shell, PointerPhase::Up, to, false);
    }

    fn click(shell: &mut Shell, at: Vec2) {
        point(shell, PointerPhase::Down, at, false);
        point(shell, PointerPhase::Up, at, false);
    }

    fn is_crop(g: &tools::SessionGeometry) -> bool {
        matches!(g, tools::SessionGeometry::Crop { .. })
    }

    fn is_path(g: &tools::SessionGeometry) -> bool {
        matches!(g, tools::SessionGeometry::Path { .. })
    }

    fn is_slices(g: &tools::SessionGeometry) -> bool {
        matches!(g, tools::SessionGeometry::Slices { .. })
    }

    fn is_lasso(g: &tools::SessionGeometry) -> bool {
        matches!(g, tools::SessionGeometry::Lasso { .. })
    }

    #[test]
    fn escape_mid_crop_drag_takes_the_crop_box_down_through_the_shell() {
        key_takes_the_overlay_down(
            tools::ToolId::Crop,
            |shell| {
                point(shell, PointerPhase::Down, Vec2::new(2.0, 2.0), false);
                point(shell, PointerPhase::Move, Vec2::new(12.0, 12.0), false);
            },
            NamedKey::Escape,
            is_crop,
        );
    }

    #[test]
    fn escape_takes_a_released_crop_box_down_through_the_shell() {
        key_takes_the_overlay_down(
            tools::ToolId::Crop,
            |shell| drag(shell, Vec2::new(2.0, 2.0), Vec2::new(12.0, 12.0)),
            NamedKey::Escape,
            is_crop,
        );
    }

    #[test]
    fn enter_commits_the_crop_and_takes_its_box_down_through_the_shell() {
        key_takes_the_overlay_down(
            tools::ToolId::Crop,
            |shell| drag(shell, Vec2::new(2.0, 2.0), Vec2::new(12.0, 12.0)),
            NamedKey::Enter,
            is_crop,
        );
    }

    #[test]
    fn escape_takes_the_pen_path_down_through_the_shell() {
        key_takes_the_overlay_down(
            tools::ToolId::Pen,
            |shell| {
                click(shell, Vec2::new(2.0, 2.0));
                click(shell, Vec2::new(12.0, 4.0));
                click(shell, Vec2::new(8.0, 12.0));
            },
            NamedKey::Escape,
            is_path,
        );
    }

    #[test]
    fn enter_commits_the_pen_path_and_takes_its_anchors_down_through_the_shell() {
        key_takes_the_overlay_down(
            tools::ToolId::Pen,
            |shell| {
                click(shell, Vec2::new(2.0, 2.0));
                click(shell, Vec2::new(12.0, 4.0));
                click(shell, Vec2::new(8.0, 12.0));
            },
            NamedKey::Enter,
            is_path,
        );
    }

    #[test]
    fn escape_takes_the_slice_set_down_through_the_shell() {
        key_takes_the_overlay_down(
            tools::ToolId::Slice,
            |shell| drag(shell, Vec2::new(2.0, 2.0), Vec2::new(10.0, 10.0)),
            NamedKey::Escape,
            is_slices,
        );
    }

    #[test]
    fn enter_commits_the_slices_and_takes_them_down_through_the_shell() {
        key_takes_the_overlay_down(
            tools::ToolId::Slice,
            |shell| drag(shell, Vec2::new(2.0, 2.0), Vec2::new(10.0, 10.0)),
            NamedKey::Enter,
            is_slices,
        );
    }

    #[test]
    fn escape_takes_the_polygonal_lasso_outline_down_through_the_shell() {
        key_takes_the_overlay_down(
            tools::ToolId::PolygonalLasso,
            |shell| {
                click(shell, Vec2::new(2.0, 2.0));
                click(shell, Vec2::new(12.0, 4.0));
                click(shell, Vec2::new(8.0, 12.0));
            },
            NamedKey::Escape,
            is_lasso,
        );
    }
}
