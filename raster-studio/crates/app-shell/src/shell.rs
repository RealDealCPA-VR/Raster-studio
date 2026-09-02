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
use crate::keymap::{Chord, Key};
use crate::prefs::WindowGeometry;
use crate::presenter::{ants_segments, selection_ants, CanvasPresenter, SelectionOutline};
use crate::session::SessionMarker;
use crate::tool_input::ToolPointer;
use ui::canvas::{PointerButton, PointerInput, PointerPhase};

/// Translate a winit key event into a [`Chord`].
///
/// `None` for keys that cannot form a shortcut on their own (a bare modifier,
/// dead keys, IME composition). Letter case is normalised by [`Key::character`],
/// so `Shift+B` and `B` name the same key with the shift flag telling them
/// apart.
pub fn chord_from_key(logical: &winit::keyboard::Key, mods: ModifiersState) -> Option<Chord> {
    let key = match logical {
        winit::keyboard::Key::Character(s) => {
            let mut chars = s.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Key::character(c),
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
    if repeat && !is_temporary_hand_key(logical) {
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
/// brush stroke the user never asked for. Nothing in this application binds the
/// right button to anything else yet — there is no canvas context menu — so it
/// stops here, where the platform event is named, rather than in the shared
/// router that `ui` also uses.
pub fn pointer_button(button: MouseButton) -> Option<PointerButton> {
    match button {
        MouseButton::Left => Some(PointerButton::Primary),
        MouseButton::Middle => Some(PointerButton::Middle),
        MouseButton::Right | MouseButton::Back | MouseButton::Forward | MouseButton::Other(_) => {
            None
        }
    }
}

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
    /// When this shell started, which is the clock the marching ants crawl on.
    /// A wall-clock reading would jump when the system clock is adjusted; the
    /// phase is a pure function of this elapsed time, so a dropped frame catches
    /// up rather than making the ants stutter.
    started: Instant,
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
}

impl Shell {
    /// The shell the desktop binary runs.
    pub fn new(editor: Editor, startup_files: Vec<PathBuf>) -> Self {
        Shell::with_shot(editor, startup_files, None)
    }

    /// As [`Shell::new`], but capture one rendered frame to `shot` (a literal
    /// GUI screenshot) and then exit — the S2.3 path, see [`Shell::run`].
    pub fn with_shot(editor: Editor, startup_files: Vec<PathBuf>, shot: Option<PathBuf>) -> Self {
        Shell {
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
            started: Instant::now(),
            repaint_at: Some(Instant::now()),
            startup_error: None,
            shot,
            shot_taken: false,
            shot_frames: 0,
        }
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
        let geometry = self
            .editor
            .preferences()
            .window
            .unwrap_or(WindowGeometry::DEFAULT)
            .sanitized();
        let attrs = Window::default_attributes()
            .with_title(self.editor.window_title())
            .with_inner_size(PhysicalSize::new(geometry.width, geometry.height))
            .with_position(PhysicalPosition::new(geometry.x, geometry.y))
            .with_maximized(geometry.maximized);
        let window = Arc::new(event_loop.create_window(attrs)?);

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
        })
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

    /// Hand every open document the size of the area it is drawn in.
    ///
    /// Through [`OpenDocument::set_viewport`] rather than by assigning
    /// `camera.viewport_size`, because a document that has never been drawn
    /// still owes the user a fit and this is the moment its size is known. A
    /// background tab opened while another was active gets fitted here too,
    /// rather than the first time it happens to be redrawn.
    fn spread_viewport(&mut self, viewport: Vec2) {
        for doc in self.editor.documents_mut() {
            doc.set_viewport(viewport);
        }
    }

    /// Store the window's geometry so the next session opens where this one was.
    fn capture_geometry(&mut self) {
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

    /// Clean exit: geometry, preferences, recent files, then the crash marker.
    fn shut_down(&mut self) {
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
        let resolved = choice.resolve(system_theme(&state.window));
        if state.theme != resolved {
            crate::chrome::install_theme(&state.egui_ctx, resolved);
            // The area around the image is a themed surface like any other.
            state.canvas.set_backdrop(backdrop_srgb(resolved));
            state.theme = resolved;
        }
        if (state.egui_ctx.zoom_factor() - scale).abs() > f32::EPSILON {
            state.egui_ctx.set_zoom_factor(scale);
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
        if let Some(doc) = self.editor.active_mut() {
            // The first frame is where a freshly opened document learns how big
            // the window is, and therefore where it is fitted to it.
            doc.set_viewport(Vec2::new(
                state.surface_config.width as f32,
                state.surface_config.height as f32,
            ));
            camera = doc.camera.clone();
            have_document = true;
            // The Channels panel's component toggles are a view setting, so
            // they are applied on the way to the texture rather than to the
            // document. Read every frame: the panel is the authority, and the
            // presenter re-uploads only when the answer actually changes.
            state.presenter.set_channel_mask(self.chrome.channel_mask());
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
                let geometry = selection_ants(
                    &mut state.outline,
                    doc,
                    self.started.elapsed().as_secs_f64(),
                    &Default::default(),
                );
                ants_segments(&geometry)
            })
            .unwrap_or_default();
        let has_ants = !ants.is_empty();
        state.overlay.set_viewport(
            &state.gpu,
            Vec2::new(
                state.surface_config.width as f32,
                state.surface_config.height as f32,
            ),
        );
        state.overlay.set_segments(&state.gpu, &ants);

        // ---- chrome ----
        let raw_input = state.egui_state.take_egui_input(&state.window);
        let (full_output, chrome_output) = {
            let chrome = &mut self.chrome;
            let editor = &self.editor;
            let mut captured = crate::chrome::ChromeOutput::default();
            let full = state.egui_ctx.run(raw_input, |ctx| {
                captured = chrome.ui(ctx, editor);
            });
            (full, captured)
        };
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
            state.canvas.render(&mut encoder, &view);
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
        if let Some((layers, active)) = output.select_layers {
            self.editor.set_layer_selection(layers, active);
        } else if let Some(id) = output.select_layer {
            self.editor.set_active_layer(id);
        }
        if let Some(depth) = output.history_jump {
            let moved = self.editor.jump_history(depth);
            if moved > 0 {
                self.editor
                    .set_status(format!("Stepped {moved} place(s) in history"));
            }
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
        if let Some(tool) = output.select_tool {
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
            if let Err(reason) = crate::menu_bridge::perform(action, &mut self.editor) {
                tracing::warn!("{}: {reason}", action.label());
            }
        }
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
                    }
                }
                DialogAction::Export(job) => {
                    // Photopea writes downloads straight away; this is the
                    // desktop equivalent — the folder picker asks once, then
                    // every enabled entry lands in it.
                    if let Some(dir) = self.editor.pick_export_folder() {
                        match self
                            .editor
                            .active_mut()
                            .expect("the export dialog only opens with a document")
                            .export_job(&job, &dir)
                        {
                            Ok(paths) => {
                                let last = paths.last().map(|p| p.display().to_string());
                                self.editor.set_status(format!(
                                    "Exported {} file(s) to {}",
                                    paths.len(),
                                    last.unwrap_or_default()
                                ));
                            }
                            Err(e) => {
                                tracing::warn!("export failed: {e}");
                                self.editor.set_status(format!("Export failed: {e}"));
                            }
                        }
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
        // preferences; the keymap bridge is the preferences dedupe task's
        // documented gap, so the live keymap wins for now.
        if let Some(prefs) = output.set_ui_preferences {
            self.editor.apply_ui_preferences(&prefs);
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
    /// keyboard, and never with Ctrl or Alt held — so Ctrl+S still saves while
    /// the user is typing. Enter and Escape deliberately fall through: they end
    /// the run through the commit and cancel routes below, which is the only
    /// way out of a text session.
    fn route_text_key(&mut self, owner: KeyboardOwner, logical: &winit::keyboard::Key) -> bool {
        use winit::keyboard::Key as WKey;
        if owner.egui_text_focus || owner.recording_shortcut || !self.pointer.is_text_editing() {
            return false;
        }
        if self.modifiers.control_key() || self.modifiers.alt_key() || self.modifiers.super_key() {
            return false;
        }
        let edit = match logical {
            // The platform's own text for the key, so a shifted letter arrives
            // as a capital and a dead-key composition arrives composed. This is
            // what `Chord` cannot carry: it normalises case on purpose.
            WKey::Character(text) => tools::TextEdit::Insert(text.as_str()),
            WKey::Named(NamedKey::Space) => tools::TextEdit::Insert(" "),
            WKey::Named(NamedKey::Backspace) => tools::TextEdit::Backspace,
            _ => return false,
        };
        let outcome = self.pointer.text_edit(&mut self.editor, edit);
        if outcome.needs_repaint() {
            self.repaint_at = Some(Instant::now());
        }
        outcome.had_pending
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
                    if outcome.needs_repaint() {
                        self.repaint_at = Some(Instant::now());
                    }
                    if outcome.had_pending {
                        return;
                    }
                }
                if let Some(action) = self.editor.keymap().resolve(&chord) {
                    self.perform(action);
                }
            }
            KeyOutcome::ReleaseTemporaryHand => {
                self.editor.release_temporary_hand();
                self.repaint_at = Some(Instant::now());
            }
            KeyOutcome::Ignore => {}
        }
    }

    /// Abandon whatever gesture is running: Escape, or the window losing focus.
    ///
    /// Reports whether there was one, and forgets the held button with it — a
    /// gesture the router still believes in refuses every later press as
    /// somebody else's, which is how a canvas goes permanently dead.
    fn abandon_gesture(&mut self) -> bool {
        if !self.pointer.cancel(&mut self.editor) {
            return false;
        }
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

    fn on_pointer(&mut self, phase: PointerPhase, button: PointerButton, over_panel: bool) {
        // A modal dialog owns the whole pointer while it is open: a press that
        // means "dismiss this modal" must never claim a canvas gesture. The
        // scrim already makes egui consume the click; this veto is the second
        // half, for a press that arrives before the next frame is drawn.
        let over_panel = over_panel || self.chrome.dialog_open();
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
        let choices = self.chrome.tool_choices(self.editor.effective_tool());
        let outcome = self
            .pointer
            .handle(&mut self.editor, input, over_panel, &choices);
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
        if let accesskit_winit::WindowEvent::ActionRequested(request) = event.window_event {
            if let Some(state) = &mut self.state {
                state.egui_state.on_accesskit_action_request(request);
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_none() {
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
            tracing::info!("autosaved {} document(s)", report.written.len());
            for (_, reason) in &report.failed {
                tracing::warn!("autosave failed: {reason}");
            }
            // A scratch autosave is only recoverable once the marker names it.
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
            WindowEvent::ModifiersChanged(mods) => self.modifiers = mods.state(),
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::DroppedFile(path) => {
                self.editor.open_paths(&[path]);
                self.sync_marker();
                self.repaint_at = Some(Instant::now());
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
            WindowEvent::MouseInput { state, button, .. } => {
                if let Some(button) = pointer_button(button) {
                    let phase = match state {
                        ElementState::Pressed => PointerPhase::Down,
                        ElementState::Released => PointerPhase::Up,
                    };
                    // `consumed` is egui's "the chrome wants this pointer", and
                    // it is only ever a veto on *claiming* a gesture — a drag
                    // already running keeps running over a panel.
                    self.on_pointer(phase, button, consumed);
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = Vec2::new(position.x as f32, position.y as f32);
                // The move belongs to whichever button went down, which winit
                // does not repeat here; with none held it is a hover.
                let button = self.held.unwrap_or(PointerButton::Primary);
                self.on_pointer(PointerPhase::Move, button, consumed);
            }
            // A drag cannot outlive the window's focus, and a gesture left
            // claimed would refuse every later press as somebody else's. Not
            // `CursorLeft`: dragging past the edge of the window and back is a
            // gesture, and winit keeps delivering its moves.
            WindowEvent::Focused(false) => {
                self.abandon_gesture();
            }
            WindowEvent::MouseWheel { delta, .. } if !consumed => {
                let scroll = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => p.y as f32 / 60.0,
                };
                let factor = (1.0 + scroll * 0.1).clamp(0.2, 5.0);
                let anchor = self.cursor;
                if let Some(doc) = self.editor.active_mut() {
                    doc.camera.zoom_at(anchor, factor);
                }
                self.repaint_at = Some(Instant::now());
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
            rgba.chunks_exact(4).all(|p| p == [255, 255, 255, 255]),
            "the white background did not composite as white"
        );
    }
}
