//! The workspace: menu bar, tool palette, options bar, docked panels, status
//! bar.
//!
//! # The UI is a view
//!
//! Nothing in this crate holds a `&mut Document`. Every control resolves to an
//! [`Intent`], the [`Workspace`] collects them, and the application performs
//! them — document edits through [`editor_core::History`], so undo and redo
//! stay uniform no matter which control produced the edit.
//!
//! That is not architecture for its own sake. It is what makes the interesting
//! half of a UI testable with no window, no GPU and no event loop: "Bring
//! Forward is disabled on the top layer", "dragging a group into its own child
//! is refused", "clicking history row 2 undoes three steps" are all assertions
//! about values, and they are all in this crate's test suite.
//!
//! Each module leads with that testable half — a `*Model` or `*State` type with
//! no `egui` in its signature — and follows it with the drawing that reads it.
//!
//! # Nothing here names a colour
//!
//! Every colour, size, radius, gap and font comes from `design`, which is what
//! makes a re-skin an edit to one crate. The rule is enforced mechanically
//! rather than trusted: `tests/no_hardcoded_style.rs` reads this crate's own
//! shipping source and fails on a literal `Color32`, a bare `FontId`, or a raw
//! pixel gap; `tests/dialogs_style_gate.rs` does the same for `src/dialogs`.
//!
//! Two exemptions, both narrow and both named by the gates themselves:
//!
//! * The user's own foreground and background are not the design system's to
//!   choose, so converting them goes through
//!   `Color32::from_rgba_unmultiplied`, which the gate sanctions by name.
//! * `src/canvas` and `src/dialogs` are scanned by their own gate rather than
//!   by `no_hardcoded_style.rs`, and `src/dialogs` carries five annotated
//!   literals for exactly the case above.
//!
//! ```no_run
//! # use ui::Workspace;
//! # let ctx = egui::Context::default();
//! # let doc = editor_core::Document::new(64, 64, "Untitled");
//! # let history = editor_core::History::new();
//! let mut workspace = Workspace::new();
//! workspace.ui(&ctx, &doc, &history);
//! for intent in workspace.drain_intents() {
//!     // apply document commands through History; perform the rest
//! }
//! ```

#![forbid(unsafe_code)]

use editor_core::{Command, Document, History};

pub mod canvas;
pub mod dialogs;
pub mod dock;
pub mod icons;
pub mod intent;
pub mod keys;
pub mod menu;
pub mod palette;
pub mod panels;
pub mod shortcut;
pub mod status;
pub mod tool_options;
pub mod view;

pub use dock::{DockSide, DockState, LayoutId, PanelId};
pub use intent::{ClipboardState, Intent, Progress, ViewFlag, ViewFlags};
pub use menu::{MenuAction, MenuContext, Resolution};
pub use palette::{PaletteModel, PaletteState};
pub use shortcut::{Key, Shortcut};
pub use status::StatusBar;
pub use tool_options::{OptionValue, ToolOptions};

/// The whole workspace: every panel, and the state they keep between frames.
///
/// Held by the application across frames. [`Workspace::ui`] draws one frame and
/// queues intents; [`Workspace::drain_intents`] takes them.
pub struct Workspace {
    /// Where the panels are and which are open.
    pub dock: DockState,
    /// The panel whose header disclosure is showing its move controls, if any.
    /// At most one at a time, so the rail never grows two of them.
    pub panel_menu: Option<PanelId>,
    /// The tool palette's selection and fly-out state.
    pub palette: PaletteState,
    /// Per-tool option values.
    pub options: ToolOptions,
    /// Layers panel state: selection, expansion, drag.
    pub layers: panels::layers::LayersState,
    /// History panel snapshots.
    pub snapshots: Vec<panels::history::Snapshot>,
    /// Colour wells and the picker.
    pub color: panels::color::ColorState,
    pub swatches: panels::color::SwatchesState,
    pub brushes: panels::brushes::BrushesState,
    pub channels: panels::channels::ChannelsState,
    pub paths: panels::channels::PathsState,
    pub info: panels::navigator::InfoState,
    /// What the Properties panel is looking at.
    pub property_focus: panels::properties::PropertyFocus,
    /// The status bar's derived content.
    pub status: StatusBar,
    /// View overlays.
    pub view_flags: ViewFlags,
    /// The clipboard's state, as far as menu enablement is concerned. The
    /// application owns the clipboard itself and writes this.
    pub clipboard: ClipboardState,
    /// The recently opened files, most recent first, as the File menu should
    /// label them. Names rather than a count: an "Open Recent" submenu of
    /// numbered slots tells the user nothing.
    pub recent: Vec<String>,
    /// Appearance, so the Window menu can show which one is in use.
    pub theme: design::Theme,
    /// The last filter run, for Filter ▸ Last Filter.
    pub last_filter: Option<menu::FilterId>,
    /// The viewport rectangle in points, for the Navigator and Fit on Screen.
    pub viewport: (f32, f32),
    /// Where the viewport is centred, in document pixels.
    pub view_center: (f32, f32),
    /// A selection was deselected and can be brought back.
    pub has_stored_selection: bool,
    /// Named selections saved into the document.
    pub saved_selections: usize,
    /// The canvas: the central region the document is drawn into, its camera,
    /// guides, grid and input routing. Drawn last by [`Workspace::ui`], because
    /// egui's central panel takes what the docks left — which is exactly the
    /// rectangle the camera has to be measured against.
    pub canvas: canvas::CanvasHost,

    /// Pointer samples the canvas routed to the active tool this frame, in
    /// document space, waiting for [`Workspace::drain_canvas_events`].
    canvas_events: Vec<canvas::RoutedPointer>,
    /// The grid is on but too dense to draw at this zoom.
    grid_suppressed: bool,
    /// The zoom and centre the last read-back copied off the camera, so a
    /// panel's write to either is distinguishable from the camera's own move.
    view_readback: (f32, (f32, f32)),
    outbox: Vec<Intent>,
    /// What the layout engine reported for each rail's extent on the previous
    /// frame, indexed by [`DockSide::ALL`]. Only a *change* under the pointer
    /// counts as a resize — see [`dock::is_resize`].
    rail_measure: [Option<f32>; 3],
}

impl Default for Workspace {
    fn default() -> Self {
        Self::new()
    }
}

/// Index of a side in [`Workspace::rail_measure`].
fn rail_slot(side: DockSide) -> usize {
    DockSide::ALL.iter().position(|s| *s == side).unwrap_or(0)
}

impl Workspace {
    pub fn new() -> Self {
        Self {
            dock: DockState::default(),
            panel_menu: None,
            palette: PaletteState::new(),
            options: ToolOptions::new(),
            layers: panels::layers::LayersState::new(),
            snapshots: Vec::new(),
            color: panels::color::ColorState::new(),
            swatches: panels::color::SwatchesState::new(),
            brushes: panels::brushes::BrushesState::new(),
            channels: panels::channels::ChannelsState::new(),
            paths: panels::channels::PathsState::new(),
            info: panels::navigator::InfoState::default(),
            property_focus: panels::properties::PropertyFocus::default(),
            status: StatusBar::new(),
            view_flags: ViewFlags::defaults(),
            clipboard: ClipboardState::EMPTY,
            recent: Vec::new(),
            theme: design::Theme::default(),
            last_filter: None,
            viewport: (1280.0, 720.0),
            view_center: (0.0, 0.0),
            has_stored_selection: false,
            saved_selections: 0,
            canvas: canvas::CanvasHost::default(),
            canvas_events: Vec::new(),
            grid_suppressed: false,
            view_readback: (1.0, (0.0, 0.0)),
            outbox: Vec::new(),
            rail_measure: [None; 3],
        }
    }

    /// The extent the layout engine reported for one rail last frame.
    pub(crate) fn rail_measure(&self, side: DockSide) -> Option<f32> {
        self.rail_measure[rail_slot(side)]
    }

    pub(crate) fn set_rail_measure(&mut self, side: DockSide, measured: f32) {
        self.rail_measure[rail_slot(side)] = Some(measured);
    }

    /// The context every menu item is resolved against this frame.
    pub fn menu_context(&self, doc: &Document, history: &History) -> MenuContext {
        MenuContext {
            clipboard: self.clipboard,
            recent_files: self.recent.clone(),
            has_stored_selection: self.has_stored_selection,
            saved_selections: self.saved_selections,
            selected_layers: self
                .layers
                .selection()
                .len()
                .max(usize::from(doc.active_layer().is_some())),
            last_filter: self.last_filter,
            view: self.view_flags,
            view_rotated: self.canvas.view.camera.rotation != 0.0,
            ruler_unit: self.canvas.unit,
            dock: self.dock.clone(),
            theme: self.theme,
            ..MenuContext::from_document(doc, history)
        }
    }

    /// Queue an intent. Public so the drawing code in submodules can post from
    /// wherever the control lives.
    pub fn emit(&mut self, intent: Intent) {
        self.outbox.push(intent);
    }

    /// Take everything queued this frame.
    pub fn drain_intents(&mut self) -> Vec<Intent> {
        std::mem::take(&mut self.outbox)
    }

    /// Take only the document edits queued this frame, dropping the rest.
    ///
    /// A convenience for an application that performs commands and nothing
    /// else; anything that also handles menus wants [`Workspace::drain_intents`].
    pub fn drain_commands(&mut self) -> Vec<Command> {
        self.drain_intents()
            .into_iter()
            .filter_map(|i| match i {
                Intent::Document(c) => Some(c),
                _ => None,
            })
            .collect()
    }

    /// Apply the effects an intent has on the *workspace's own* state — panel
    /// visibility, the active tool, the theme, view flags.
    ///
    /// Document edits are not this function's business and are ignored: the
    /// application runs those through [`History`] and the next frame reads the
    /// result. Returns `true` when something in the workspace changed.
    ///
    /// # Absorbing twice is a no-op
    ///
    /// The controls in `view::` apply their own effect as they draw and *then*
    /// emit, so an application that absorbs what it drained is re-applying
    /// something that already landed. Every intent handled here is therefore an
    /// absolute set — see the idempotency rule on [`Intent`], and
    /// `every_workspace_intent_is_idempotent_under_absorb`, which enforces it
    /// by applying each one twice and comparing the workspace.
    pub fn absorb(&mut self, intent: &Intent) -> bool {
        match intent {
            Intent::SetPanelOpen { panel, open } => {
                let before = self.dock.is_open(*panel);
                self.dock.set_open(*panel, *open);
                before != self.dock.is_open(*panel)
            }
            Intent::DockPanel { panel, side } => self.dock.dock(*panel, *side),
            Intent::ReorderPanel { panel, to } => self.dock.reorder_to(*panel, *to),
            Intent::ApplyLayout(layout) => {
                let before = self.dock.clone();
                self.dock.apply_layout(*layout);
                before != self.dock
            }
            Intent::SetTheme(theme) => {
                let changed = self.theme != *theme;
                self.theme = *theme;
                changed
            }
            Intent::SetViewFlag { flag, on } => {
                let changed = self.view_flags.get(*flag) != *on;
                self.view_flags.set(*flag, *on);
                changed
            }
            Intent::SelectTool(tool) => {
                let model = PaletteModel::build();
                // Leaving the eyedropper puts the sampler away. Without this
                // the Color panel's crosshair stays lit for the rest of the
                // session, claiming an armed tool the user has moved off.
                if *tool != tools::ToolId::Eyedropper {
                    self.color.eyedropper_armed = false;
                }
                // Choosing a tool answers whatever the fly-out was asking, so
                // it goes away — `activate` no longer does this itself, since
                // doing it there made the fly-out's own button unable to
                // close it. See `PaletteState::click_slot`.
                let closed = self.palette.close_flyout();
                self.palette.activate(&model, *tool) || closed
            }
            // The canvas camera is the view. `status.zoom` and `view_center`
            // are readouts *of* it, written here so a caller that absorbs an
            // intent without drawing a frame still sees the new number.
            Intent::SetZoom(zoom) => {
                let next = panels::navigator::clamp_zoom(*zoom);
                let changed = self.status.zoom != next || self.canvas.view.camera.zoom != next;
                // Zooming about the middle of the viewport leaves the centre
                // where it is, which is what a typed zoom should do.
                self.canvas.view.camera.set_zoom(next);
                self.read_back_camera();
                changed
            }
            Intent::SetViewCenter(center) => {
                let next = (
                    if center.0.is_finite() { center.0 } else { 0.0 },
                    if center.1.is_finite() { center.1 } else { 0.0 },
                );
                let changed = self.view_center != next;
                self.canvas.view.camera.center = glam::Vec2::new(next.0, next.1);
                self.read_back_camera();
                changed
            }
            Intent::Action(action) => self.absorb_action(*action),
            Intent::SetRulerUnit(unit) => {
                let changed = self.canvas.unit != *unit;
                self.canvas.unit = *unit;
                changed
            }
            Intent::SetChannelVisible { channel, visible } => {
                use panels::channels::ChannelKind;
                match channel {
                    ChannelKind::Component(i) => {
                        let before = self.channels.component_visible(*i);
                        self.channels.set_component_visible(*i, *visible);
                        before != self.channels.component_visible(*i)
                    }
                    // The composite and a mask channel both need the document
                    // (for the component count, and for the mask's own flag),
                    // which `absorb` does not have; the drawing side has
                    // already applied those.
                    _ => false,
                }
            }
            Intent::SelectChannel(channel) => {
                let changed = self.channels.selected != *channel;
                self.channels.selected = *channel;
                changed
            }
            Intent::ResetToolOptions(tool) => self.options.reset(*tool),
            Intent::SetToolGradient { tool, gradient } => {
                self.options.set_gradient(*tool, (**gradient).clone())
            }
            Intent::SetForeground(rgba) => self
                .color
                .set_well(panels::color::ColorWell::Foreground, *rgba),
            Intent::SetBackground(rgba) => self
                .color
                .set_well(panels::color::ColorWell::Background, *rgba),
            Intent::SetGroupExpanded { layer, expanded } => {
                self.layers.set_expanded(*layer, *expanded)
            }
            Intent::SetToolOption { tool, key, value } => self.options.set(*tool, key, *value),
            _ => false,
        }
    }

    /// Perform the named actions that are the *view's* own.
    ///
    /// The zoom commands and the view-rotation reset move the canvas camera and
    /// nothing else, so the workspace can do them itself — and must, because
    /// otherwise Fit on Screen and 100% move a number in the status bar while
    /// the image stays where it was. Every other [`MenuAction`] is the
    /// application's and is reported as unhandled here.
    fn absorb_action(&mut self, action: MenuAction) -> bool {
        use menu::ZoomCommand;
        let before = self.canvas.view.camera;
        match action {
            MenuAction::Zoom(ZoomCommand::In) => self.canvas.view.zoom_in(),
            MenuAction::Zoom(ZoomCommand::Out) => self.canvas.view.zoom_out(),
            MenuAction::Zoom(ZoomCommand::FitOnScreen) => self.canvas.zoom_to_fit(),
            MenuAction::Zoom(ZoomCommand::FillScreen) => self.canvas.zoom_to_fill(),
            MenuAction::Zoom(ZoomCommand::ActualPixels) => self.canvas.view.zoom_to_actual_pixels(),
            // The return value is deliberately dropped: the camera comparison
            // below is the answer to "did anything change?", and a refusal here
            // means it did not.
            MenuAction::Zoom(ZoomCommand::ToSelection) => {
                let _ = self.canvas.zoom_to_selection();
            }
            MenuAction::Zoom(ZoomCommand::PrintSize) => self.canvas.zoom_to_print_size(),
            MenuAction::ResetViewRotation => self.canvas.view.camera.reset_rotation(),
            _ => return false,
        }
        self.read_back_camera();
        self.canvas.view.camera != before
    }

    /// Drop panel state naming layers the document no longer has.
    pub fn prune(&mut self, doc: &Document) {
        self.layers.prune(doc);
        self.paths.prune(doc);
    }

    /// Draw every enabled surface for one frame.
    pub fn ui(&mut self, ctx: &egui::Context, doc: &Document, history: &History) {
        self.prune(doc);
        self.status.tool = Some(self.palette.active());
        let context = self.menu_context(doc, history);
        self.handle_keys(ctx, doc, &context);
        view::menu_bar(self, ctx, &context);
        view::tool_options(self, ctx);
        view::status_bar(self, ctx, doc);
        view::tool_palette(self, ctx);
        view::docks(self, ctx, doc, history);
        self.record_viewport(ctx);
        // Last, and only last: the canvas gets what the chrome left.
        let tool = self.palette.active();
        let brush = self.options.brush_settings(tool);
        self.sync_canvas_view();
        let out = self.canvas.central_panel(ctx, doc, tool, &brush);
        self.consume_canvas(out);
    }

    /// Put the View menu's toggles onto the canvas.
    ///
    /// [`ViewFlags`] is what the menu draws its checkmarks from; this is what
    /// makes ticking one *do* something. Without it the canvas toggles were a
    /// checkmark and nothing else.
    ///
    /// Called immediately before the canvas draws, so a flag flipped by a menu
    /// click or a chord this frame is on screen in the same frame.
    fn sync_canvas_view(&mut self) {
        // A panel that wrote the zoom or the centre *since the last read-back*
        // is asking the camera to move; anything else leaves the camera alone,
        // so code that drives the camera directly is not clobbered by a stale
        // readout on the next frame.
        if self.status.zoom != self.view_readback.0 {
            self.canvas.view.camera.set_zoom(self.status.zoom);
        }
        if self.view_center != self.view_readback.1 {
            self.canvas.view.camera.center =
                glam::Vec2::new(self.view_center.0, self.view_center.1);
        }
        let view = &mut self.canvas.view;
        view.rulers_visible = self.view_flags.get(ViewFlag::Rulers);
        view.guides.visible = self.view_flags.get(ViewFlag::Guides);
        view.grid.visible = self.view_flags.get(ViewFlag::Grid);
        view.grid.pixel_grid = self.view_flags.get(ViewFlag::PixelGrid);
        view.snap.enabled = self.view_flags.get(ViewFlag::Snap);
        // Smart guides *are* the layer-alignment snap: the lines only appear
        // because a layer edge or centre caught, so the two are one setting.
        view.snap.to_layers = self.view_flags.get(ViewFlag::SmartGuides);
        view.selection_edges_visible = self.view_flags.get(ViewFlag::SelectionEdges);
        view.layer_edges_visible = self.view_flags.get(ViewFlag::LayerEdges);
        view.precise_cursor = self.view_flags.get(ViewFlag::PreciseCursor);
        view.camera.flip_x = self.view_flags.get(ViewFlag::FlipHorizontal);
        view.camera.flip_y = self.view_flags.get(ViewFlag::FlipVertical);
    }

    /// Read what the canvas frame produced.
    ///
    /// The canvas is the one camera in this crate, so the zoom readout and the
    /// Navigator's centre are *derived* from it rather than kept alongside it —
    /// before this they were separate numbers, and a wheel zoom moved the image
    /// while the status bar went on claiming 100%.
    fn consume_canvas(&mut self, out: canvas::CanvasOutput) {
        self.read_back_camera();
        self.info.pointer = out.pointer_doc.map(|p| (p.x, p.y));
        self.grid_suppressed = out.grid_suppressed;
        self.canvas_events.extend(out.tool_events);
    }

    /// Copy the canvas camera into the numbers the chrome reads.
    fn read_back_camera(&mut self) {
        let camera = &self.canvas.view.camera;
        self.status.zoom = camera.zoom;
        self.view_center = (camera.center.x, camera.center.y);
        self.view_readback = (self.status.zoom, self.view_center);
    }

    /// Take the pointer samples the canvas routed to the active tool.
    ///
    /// The canvas converts to document space, applies snapping and rejects
    /// anything a panel owns; running the tool is the application's job, so
    /// this is where the two meet.
    pub fn drain_canvas_events(&mut self) -> Vec<canvas::RoutedPointer> {
        std::mem::take(&mut self.canvas_events)
    }

    /// The grid is switched on but too dense at this zoom to be legible, so it
    /// was not drawn. Worth *saying*: a user who turns the grid on and sees
    /// nothing has been told the setting was ignored, which it was not.
    pub fn grid_is_suppressed(&self) -> bool {
        self.grid_suppressed
    }

    /// Everything a renderer needs to put the image where the user can see it.
    ///
    /// [`canvas::RenderCamera::viewport_origin_px`] carries the panel inset in
    /// *physical pixels*, which is the fix this whole module tree exists for: a
    /// renderer handed only a surface size centres the image on the middle of
    /// the window, so a left dock pushes it out from under the empty space the
    /// user is looking at. A renderer that cannot take a viewport offset uses
    /// [`canvas::RenderCamera::center_for_full_surface`] instead.
    pub fn render_camera(&self) -> canvas::RenderCamera {
        self.canvas.render_camera()
    }

    /// Remember how much room the canvas actually has, once every panel has
    /// taken its share.
    ///
    /// Read *after* the chrome is drawn, because that is when
    /// `Context::available_rect` describes what is left for the image. Fit on
    /// Screen and the Navigator's rectangle are both computed from this, and
    /// before it existed both used the fabricated default the constructor set —
    /// so "Fit" fitted the document to a viewport that was not on screen.
    fn record_viewport(&mut self, ctx: &egui::Context) {
        let rect = ctx.available_rect();
        let (w, h) = (rect.width(), rect.height());
        if w.is_finite() && h.is_finite() && w > 0.0 && h > 0.0 {
            self.viewport = (w, h);
        }
    }

    /// Record a colour the application sampled from the canvas, and put the
    /// eyedropper away.
    ///
    /// The UI arms the sampler; only the application knows when a sample
    /// actually happened, because only it can read a pixel. Returns `true` when
    /// the well changed.
    pub fn color_sampled(&mut self, rgba: [f32; 4]) -> bool {
        self.color.eyedropper_armed = false;
        self.color.set_current(rgba)
    }

    /// Run this frame's key presses through the same table the menu draws.
    ///
    /// Skipped entirely while a text field has focus: typing `v` into a layer
    /// name must not switch to the Move tool. The decision itself lives in
    /// [`crate::keys`], which is testable without a context; this is only the
    /// plumbing that feeds it.
    fn handle_keys(&mut self, ctx: &egui::Context, doc: &Document, context: &MenuContext) {
        if ctx.wants_keyboard_input() {
            return;
        }
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
            // A menu chord always wins, and consumes the key even when its item
            // is unavailable — otherwise Ctrl+Z on an empty history would fall
            // through to whatever else wanted Z.
            if let Some(resolution) = keys::resolve_key(key, modifiers, context, self.recent.len())
            {
                if let Resolution::Enabled(intent) = resolution {
                    self.emit(intent);
                }
                continue;
            }
            // The Channels panel prints `Ctrl+2`..`Ctrl+9` beside its rows, so
            // those chords have to work; they are looked up in the same row
            // list the panel drew, which is what keeps hint and behaviour in
            // step. Menu chords are matched first, so `Ctrl+0` and `Ctrl+1`
            // stay the zoom commands they are in the View menu.
            if let Some(channel) = keys::channel_for_key(key, modifiers, doc, &self.channels) {
                self.channels.isolate(&doc.meta.color_space, channel);
                self.emit(Intent::SelectChannel(channel));
                continue;
            }
            if let Some(tool) = keys::tool_for_key(key, modifiers, Some(self.palette.active())) {
                let model = PaletteModel::build();
                self.palette.close_flyout();
                if self.palette.activate(&model, tool) {
                    self.emit(Intent::SelectTool(tool));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use layer_model::Layer;
    use tools::ToolId;

    #[test]
    fn a_fresh_workspace_has_the_essentials_layout_and_the_default_theme() {
        let w = Workspace::new();
        assert_eq!(w.dock.layout(), Some(LayoutId::Essentials));
        assert!(w.dock.is_open(PanelId::Layers));
        assert_eq!(w.theme, design::Theme::Dark);
        assert!(w.clipboard.is_empty());
    }

    #[test]
    fn the_outbox_is_drained_not_replayed() {
        let mut w = Workspace::new();
        w.emit(Intent::SelectTool(ToolId::Eraser));
        assert_eq!(w.drain_intents().len(), 1);
        assert!(w.drain_intents().is_empty());
    }

    #[test]
    fn draining_commands_keeps_only_the_document_edits() {
        let mut w = Workspace::new();
        w.emit(Intent::SelectTool(ToolId::Eraser));
        w.emit(Intent::Document(Command::create_layer(Layer::raster("A"))));
        w.emit(Intent::SetTheme(design::Theme::Light));
        let commands = w.drain_commands();
        assert_eq!(commands.len(), 1);
        assert!(matches!(commands[0], Command::CreateLayer { .. }));
    }

    #[test]
    fn absorbing_a_panel_toggle_moves_the_dock() {
        let mut w = Workspace::new();
        assert!(w.absorb(&Intent::SetPanelOpen {
            panel: PanelId::Paths,
            open: true
        }));
        assert!(w.dock.is_open(PanelId::Paths));
        // Absorbing it again changes nothing and says so.
        assert!(!w.absorb(&Intent::SetPanelOpen {
            panel: PanelId::Paths,
            open: true
        }));
    }

    #[test]
    fn absorbing_a_layout_replaces_the_whole_dock() {
        let mut w = Workspace::new();
        assert!(w.absorb(&Intent::ApplyLayout(LayoutId::Painting)));
        assert!(w.dock.is_open(PanelId::Brushes));
        assert!(!w.dock.is_open(PanelId::History));
    }

    #[test]
    fn absorbing_a_panel_move_puts_the_panel_on_the_other_side() {
        let mut w = Workspace::new();
        assert_eq!(w.dock.placement(PanelId::Layers).side, DockSide::Right);
        assert!(w.absorb(&Intent::DockPanel {
            panel: PanelId::Layers,
            side: DockSide::Bottom,
        }));
        assert_eq!(w.dock.placement(PanelId::Layers).side, DockSide::Bottom);
        // Docking it where it already is says so rather than churning.
        assert!(!w.absorb(&Intent::DockPanel {
            panel: PanelId::Layers,
            side: DockSide::Bottom,
        }));
    }

    #[test]
    fn absorbing_a_reorder_moves_the_panel_within_its_side() {
        let mut w = Workspace::new();
        let before = w.dock.panels_on(DockSide::Right);
        let last = *before.last().expect("Essentials fills the right rail");
        let to = u8::try_from(before.len() - 2).unwrap();
        assert!(w.absorb(&Intent::ReorderPanel { panel: last, to }));
        assert_ne!(w.dock.panels_on(DockSide::Right), before);
        assert_eq!(w.dock.panels_on(DockSide::Right)[usize::from(to)], last);
        // Absorbing the very same intent again leaves it exactly there: the
        // destination is absolute, so a second application is a no-op.
        assert!(!w.absorb(&Intent::ReorderPanel { panel: last, to }));
        assert_eq!(w.dock.panels_on(DockSide::Right)[usize::from(to)], last);
        // Index 0 is where the first panel already is.
        let first = w.dock.panels_on(DockSide::Right)[0];
        assert!(!w.absorb(&Intent::ReorderPanel {
            panel: first,
            to: 0
        }));
    }

    /// Everything an application routes back into the workspace, applied twice.
    ///
    /// The invariant is on [`Intent`]: the drawing side has already applied
    /// what it emits, so absorbing a drained intent is the *second*
    /// application. A relative intent — `ReorderPanel { up }`, as it was
    /// written — moves the panel one more place, and every other variant in
    /// this set happened to be an absolute set, which is why nothing else
    /// caught it.
    #[test]
    fn every_workspace_intent_is_idempotent_under_absorb() {
        use dialogs::units::Unit;
        use panels::channels::ChannelKind;

        // The whole of the state `absorb` can touch for this set, as a value a
        // test can compare. `ToolOptions` has no `PartialEq`, so this is the
        // Debug rendering rather than the struct.
        fn probe(w: &Workspace) -> String {
            format!(
                "{:?}|{:?}|{:?}|{:?}|{:?}|{:?}",
                w.dock, w.view_flags, w.channels, w.canvas.unit, w.options, w.layers
            )
        }

        let doc = Document::new(32, 32, "Probe");
        let group = doc.layers.iter_depth_first().first().copied();
        let intents = vec![
            Intent::SetPanelOpen {
                panel: PanelId::Navigator,
                open: true,
            },
            Intent::DockPanel {
                panel: PanelId::Layers,
                side: DockSide::Left,
            },
            Intent::ReorderPanel {
                panel: PanelId::Layers,
                to: 0,
            },
            Intent::ApplyLayout(dock::LayoutId::Painting),
            Intent::SetViewFlag {
                flag: ViewFlag::Grid,
                on: true,
            },
            Intent::SetRulerUnit(Unit::Centimeters),
            Intent::SetChannelVisible {
                channel: ChannelKind::Component(0),
                visible: false,
            },
            Intent::SelectChannel(ChannelKind::Component(1)),
            Intent::SetToolOption {
                tool: tools::ToolId::Brush,
                key: "size",
                value: OptionValue::Float(48.0),
            },
            Intent::SetToolGradient {
                tool: tools::ToolId::Gradient,
                gradient: Box::new(layer_model::Gradient {
                    smoothness: 0.25,
                    ..Default::default()
                }),
            },
            Intent::ResetToolOptions(tools::ToolId::Brush),
        ]
        .into_iter()
        .chain(group.map(|layer| Intent::SetGroupExpanded {
            layer,
            expanded: false,
        }))
        .collect::<Vec<_>>();

        for intent in intents {
            let mut w = Workspace::new();
            // An override for `ResetToolOptions` to have something to clear —
            // without it that intent would pass the check by doing nothing.
            w.options
                .set(tools::ToolId::Brush, "size", OptionValue::Float(11.0));
            let fresh = probe(&w);
            assert!(
                w.absorb(&intent),
                "{intent:?} changed nothing at all, so applying it twice proves \
                 nothing"
            );
            let once = probe(&w);
            assert_ne!(once, fresh, "{intent:?} reported a change it did not make");
            let again = w.absorb(&intent);
            assert_eq!(
                probe(&w),
                once,
                "absorbing {intent:?} twice moved the workspace twice"
            );
            assert!(
                !again,
                "{intent:?} reported a change on its second absorb, so an \
                 application would repaint (and keep repainting) for nothing"
            );
        }
    }

    #[test]
    fn absorbing_a_pan_moves_the_view_centre_and_refuses_a_nonsense_one() {
        let mut w = Workspace::new();
        assert!(w.absorb(&Intent::SetViewCenter((120.0, 80.0))));
        assert_eq!(w.view_center, (120.0, 80.0));
        assert!(!w.absorb(&Intent::SetViewCenter((120.0, 80.0))));
        w.absorb(&Intent::SetViewCenter((f32::NAN, f32::INFINITY)));
        assert_eq!(w.view_center, (0.0, 0.0));
    }

    #[test]
    fn absorbing_a_channel_toggle_writes_the_component_flag() {
        use panels::channels::ChannelKind;
        let mut w = Workspace::new();
        assert!(w.absorb(&Intent::SetChannelVisible {
            channel: ChannelKind::Component(1),
            visible: false,
        }));
        assert!(!w.channels.component_visible(1));
        assert!(w.channels.component_visible(0));
        assert!(!w.absorb(&Intent::SetChannelVisible {
            channel: ChannelKind::Component(1),
            visible: false,
        }));
    }

    #[test]
    fn leaving_the_eyedropper_puts_the_sampler_away() {
        let mut w = Workspace::new();
        w.color.eyedropper_armed = true;
        w.absorb(&Intent::SelectTool(ToolId::Eyedropper));
        assert!(w.color.eyedropper_armed, "the eyedropper disarmed itself");
        w.absorb(&Intent::SelectTool(ToolId::Brush));
        assert!(!w.color.eyedropper_armed);
    }

    #[test]
    fn a_sampled_colour_lands_in_the_well_and_disarms_the_sampler() {
        let mut w = Workspace::new();
        w.color.eyedropper_armed = true;
        assert!(w.color_sampled([0.25, 0.5, 0.75, 1.0]));
        assert!(!w.color.eyedropper_armed);
        assert_eq!(w.color.current(), [0.25, 0.5, 0.75, 1.0]);
    }

    #[test]
    fn absorbing_a_document_command_changes_nothing_in_the_workspace() {
        let mut w = Workspace::new();
        assert!(!w.absorb(&Intent::Document(Command::create_layer(Layer::raster("A")))));
    }

    #[test]
    fn absorbing_a_zoom_clamps_it() {
        let mut w = Workspace::new();
        w.absorb(&Intent::SetZoom(1e9));
        assert_eq!(w.status.zoom, panels::navigator::MAX_ZOOM);
        w.absorb(&Intent::SetZoom(f32::NAN));
        assert_eq!(w.status.zoom, 1.0);
    }

    #[test]
    fn absorbing_a_tool_option_writes_it_through_the_registry() {
        let mut w = Workspace::new();
        assert!(w.absorb(&Intent::SetToolOption {
            tool: ToolId::Brush,
            key: "size",
            value: OptionValue::Float(64.0),
        }));
        assert_eq!(w.options.brush_settings(ToolId::Brush).size, 64.0);
        // A key the tool does not have is refused rather than stored.
        assert!(!w.absorb(&Intent::SetToolOption {
            tool: ToolId::Hand,
            key: "size",
            value: OptionValue::Float(64.0),
        }));
    }

    #[test]
    fn the_menu_context_carries_both_halves_of_the_state() {
        let mut doc = Document::new(64, 64, "Test");
        let id = doc.layers.push_root(Layer::raster("A")).unwrap();
        doc.set_active_layer(Some(id)).unwrap();
        let history = History::new();

        let mut w = Workspace::new();
        w.clipboard = ClipboardState {
            pixels: true,
            layers: false,
        };
        w.recent = vec!["one.png".into(), "two.psd".into()];
        w.theme = design::Theme::Light;
        let ctx = w.menu_context(&doc, &history);
        assert!(ctx.has_document);
        assert_eq!(ctx.active.map(|l| l.id), Some(id));
        assert!(ctx.clipboard.pixels);
        assert_eq!(ctx.recent_files, vec!["one.png", "two.psd"]);
        assert_eq!(ctx.theme, design::Theme::Light);
        assert!(ctx.dock.is_open(PanelId::Layers));
    }

    #[test]
    fn pruning_drops_panel_state_for_layers_that_are_gone() {
        let mut doc = Document::new(64, 64, "Test");
        let id = doc.layers.push_root(Layer::raster("A")).unwrap();
        let mut w = Workspace::new();
        w.layers.select_only(id);
        doc.layers.remove(id).unwrap();
        w.prune(&doc);
        assert!(w.layers.selection().is_empty());
    }

    const TEST_W: f32 = 1400.0;
    const TEST_H: f32 = 900.0;

    /// One frame of raw input at a chosen display scale.
    ///
    /// The scale has to be repeated on **every** frame: egui reads it out of
    /// the viewport info, and a frame that omits it silently falls back to 1x.
    fn raw_input(ppp: f32, events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(TEST_W, TEST_H),
            )),
            viewport_id: egui::ViewportId::ROOT,
            viewports: std::iter::once((
                egui::ViewportId::ROOT,
                egui::ViewportInfo {
                    native_pixels_per_point: Some(ppp),
                    ..Default::default()
                },
            ))
            .collect(),
            events,
            ..Default::default()
        }
    }

    /// A themed context, run once so its layout is settled.
    fn workspace_ctx(ppp: f32) -> egui::Context {
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let style = design::style_for(design::Theme::Dark);
        ctx.set_style_of(egui::Theme::Dark, style.clone());
        ctx.set_style_of(egui::Theme::Light, style);
        let _ = ctx.run(raw_input(ppp, Vec::new()), |_| {});
        ctx
    }

    fn workspace_frame(
        w: &mut Workspace,
        ctx: &egui::Context,
        doc: &Document,
        history: &History,
        ppp: f32,
        events: Vec<egui::Event>,
    ) -> Vec<egui::epaint::ClippedShape> {
        let full = ctx.run(raw_input(ppp, events), |ctx| w.ui(ctx, doc, history));
        let shapes = full.shapes.clone();
        let _ = ctx.tessellate(full.shapes, full.pixels_per_point);
        shapes
    }

    /// The headline bug, end to end and inside the application: a dock of known
    /// width has to move the renderer's viewport origin by that width times the
    /// display scale. Before the canvas was wired into the workspace this held
    /// only inside a library nothing called.
    #[test]
    fn a_dock_moves_the_render_cameras_viewport_origin_by_its_own_width() {
        let doc = Document::new(320, 240, "Test");
        let history = History::new();
        for requested in [1.0_f32, 2.0] {
            let ctx = workspace_ctx(requested);
            let mut w = Workspace::new();
            // Every dock shut: the canvas gets everything the bars leave.
            for panel in PanelId::ALL {
                w.dock.set_open(*panel, false);
            }
            workspace_frame(&mut w, &ctx, &doc, &history, requested, Vec::new());
            workspace_frame(&mut w, &ctx, &doc, &history, requested, Vec::new());
            let scale = ctx.pixels_per_point();
            assert_eq!(
                scale, requested,
                "the context did not take the display scale, so this proves nothing"
            );
            let bare = w.render_camera();

            // One panel on the left. Its width is read back off the dock after
            // the frame — egui owns it, because the user can drag it.
            w.dock.dock(PanelId::Layers, DockSide::Left);
            w.dock.set_open(PanelId::Layers, true);
            workspace_frame(&mut w, &ctx, &doc, &history, requested, Vec::new());
            workspace_frame(&mut w, &ctx, &doc, &history, requested, Vec::new());
            let docked = w.render_camera();
            let dock_pt = w.dock.left_width();
            assert!(dock_pt > 0.0, "the dock reported no width");

            let moved = docked.viewport_origin_px.x - bare.viewport_origin_px.x;
            assert!(
                (moved - dock_pt * scale).abs() < 2.0 * scale,
                "at {scale}x a {dock_pt}pt dock moved the render viewport by \
                 {moved}px, not {}px",
                dock_pt * scale
            );
            assert!(docked.viewport_size_px.x < bare.viewport_size_px.x);
            assert!(docked.surface_size_px.x >= docked.viewport_size_px.x);
            // The origin is in *physical* pixels, which is the half that used
            // to be lost: at 2x the same dock has to move it twice as far.
            assert!(
                (docked.viewport_origin_px.x - w.canvas.view.viewport().origin_pt().x * scale)
                    .abs()
                    < 1e-3
            );
        }
    }

    /// …and the image survives the frame that draws over it. The central panel
    /// is the one surface that must not fill: the renderer has already put the
    /// document on the surface underneath.
    #[test]
    fn nothing_the_workspace_draws_fills_over_the_image() {
        let ctx = workspace_ctx(1.0);
        let doc = Document::new(320, 240, "Test");
        let history = History::new();
        let mut w = Workspace::new();
        workspace_frame(&mut w, &ctx, &doc, &history, 1.0, Vec::new());
        w.canvas.view.zoom_to_fit(canvas::workspace::doc_size(&doc));
        let shapes = workspace_frame(&mut w, &ctx, &doc, &history, 1.0, Vec::new());

        let image = canvas::paint::document_bounds_pt(
            &w.canvas.view.camera,
            w.canvas.view.viewport(),
            canvas::workspace::doc_size(&doc),
        )
        .expect("the document did not project")
        .intersect(&w.canvas.view.viewport().content_bounds_pt());
        assert!(!image.is_empty(), "the image is not on screen");
        let middle = egui::pos2(image.center().x, image.center().y);

        let covering: Vec<egui::Rect> = shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Rect(r) if r.fill.a() == u8::MAX && r.rect.contains(middle) => {
                    Some(r.rect)
                }
                _ => None,
            })
            .collect();
        assert!(
            covering.is_empty(),
            "an opaque fill covers the middle of the document at {middle:?}: {covering:?}"
        );
    }

    /// Resolve a menu item against the live workspace and absorb what it
    /// emits, exactly as the application does. Panics if the item is disabled,
    /// because a test that silently exercised nothing proves nothing.
    fn invoke(w: &mut Workspace, doc: &Document, history: &History, action: MenuAction) {
        let context = w.menu_context(doc, history);
        match action.resolve(&context) {
            Resolution::Enabled(intent) => {
                w.absorb(&intent);
            }
            Resolution::Disabled(why) => panic!("{action:?} is disabled: {why}"),
        }
    }

    /// The View menu's canvas toggles were checkmarks and nothing else: nothing
    /// copied a flag onto the canvas, so ticking Rulers, Guides, Grid, Pixel
    /// Grid, Smart Guides or Snap changed nothing on screen.
    #[test]
    fn every_view_toggle_reaches_the_canvas() {
        let ctx = workspace_ctx(1.0);
        let doc = Document::new(320, 240, "Test");
        let history = History::new();
        let mut w = Workspace::new();
        workspace_frame(&mut w, &ctx, &doc, &history, 1.0, Vec::new());

        /// How to read the canvas field one flag drives.
        type Read = fn(&Workspace) -> bool;
        // Each flag, beside the canvas field it has to move.
        let read: [(ViewFlag, Read); 10] = [
            (ViewFlag::Rulers, |w| w.canvas.view.rulers_visible),
            (ViewFlag::Guides, |w| w.canvas.view.guides.visible),
            (ViewFlag::Grid, |w| w.canvas.view.grid.visible),
            (ViewFlag::PixelGrid, |w| w.canvas.view.grid.pixel_grid),
            (ViewFlag::Snap, |w| w.canvas.view.snap.enabled),
            (ViewFlag::SmartGuides, |w| w.canvas.view.snap.to_layers),
            (ViewFlag::SelectionEdges, |w| {
                w.canvas.view.selection_edges_visible
            }),
            (ViewFlag::LayerEdges, |w| w.canvas.view.layer_edges_visible),
            (ViewFlag::FlipHorizontal, |w| w.canvas.view.camera.flip_x),
            (ViewFlag::FlipVertical, |w| w.canvas.view.camera.flip_y),
        ];

        for (flag, read_it) in read {
            for on in [true, false, true] {
                w.absorb(&Intent::SetViewFlag { flag, on });
                workspace_frame(&mut w, &ctx, &doc, &history, 1.0, Vec::new());
                assert_eq!(
                    read_it(&w),
                    on,
                    "{flag:?} was ticked in the menu and the canvas never heard about it"
                );
            }
        }

        // Rulers also give the image the gutter back, which is the visible half
        // of that toggle.
        w.absorb(&Intent::SetViewFlag {
            flag: ViewFlag::Rulers,
            on: true,
        });
        workspace_frame(&mut w, &ctx, &doc, &history, 1.0, Vec::new());
        let with = w.canvas.view.viewport().size_pt();
        w.absorb(&Intent::SetViewFlag {
            flag: ViewFlag::Rulers,
            on: false,
        });
        workspace_frame(&mut w, &ctx, &doc, &history, 1.0, Vec::new());
        let without = w.canvas.view.viewport().size_pt();
        assert!(without.x > with.x && without.y > with.y, "{without:?}");
    }

    /// The precise-cursor toggle had no control anywhere in the tree. Now it is
    /// a View item, and it reaches the cursor.
    #[test]
    fn the_precise_cursor_item_changes_the_cursor() {
        let ctx = workspace_ctx(1.0);
        let doc = Document::new(320, 240, "Test");
        let history = History::new();
        let mut w = Workspace::new();
        w.palette.activate(&PaletteModel::build(), ToolId::Brush);
        workspace_frame(&mut w, &ctx, &doc, &history, 1.0, Vec::new());
        assert!(!w.canvas.view.precise_cursor);

        invoke(
            &mut w,
            &doc,
            &history,
            MenuAction::ToggleView(ViewFlag::PreciseCursor),
        );
        workspace_frame(&mut w, &ctx, &doc, &history, 1.0, Vec::new());
        assert!(w.canvas.view.precise_cursor, "the menu item did nothing");

        let at = w.canvas.view.viewport().center_pt();
        let content = canvas::CanvasContent {
            active_tool: ToolId::Brush,
            ..canvas::CanvasContent::default()
        };
        assert_eq!(
            w.canvas.view.resolve_cursor(&content, Some(at)),
            canvas::CanvasCursor::PreciseCross
        );

        // …and off again.
        invoke(
            &mut w,
            &doc,
            &history,
            MenuAction::ToggleView(ViewFlag::PreciseCursor),
        );
        workspace_frame(&mut w, &ctx, &doc, &history, 1.0, Vec::new());
        assert!(!w.canvas.view.precise_cursor);
        assert_eq!(
            w.canvas.view.resolve_cursor(&content, Some(at)),
            canvas::CanvasCursor::BrushOutline
        );
    }

    /// Flip Horizontal, Flip Vertical and Reset View Rotation had no control
    /// either: the camera methods existed and nothing called them.
    #[test]
    fn the_view_flip_and_rotation_items_move_the_camera() {
        let ctx = workspace_ctx(1.0);
        let doc = Document::new(320, 240, "Test");
        let history = History::new();
        let mut w = Workspace::new();
        workspace_frame(&mut w, &ctx, &doc, &history, 1.0, Vec::new());

        for (flag, read_it) in [(ViewFlag::FlipHorizontal, 0), (ViewFlag::FlipVertical, 1)] {
            invoke(&mut w, &doc, &history, MenuAction::ToggleView(flag));
            workspace_frame(&mut w, &ctx, &doc, &history, 1.0, Vec::new());
            let camera = &w.canvas.view.camera;
            let flipped = if read_it == 0 {
                camera.flip_x
            } else {
                camera.flip_y
            };
            assert!(flipped, "{flag:?} did not reach the camera");
        }

        // Reset View Rotation is disabled while the view is already upright,
        // with a reason, rather than being a dead item.
        let context = w.menu_context(&doc, &history);
        assert_eq!(
            MenuAction::ResetViewRotation.resolve(&context).reason(),
            Some("The view is already upright")
        );

        w.canvas
            .view
            .camera
            .set_rotation(std::f32::consts::FRAC_PI_4);
        invoke(&mut w, &doc, &history, MenuAction::ResetViewRotation);
        assert_eq!(w.canvas.view.camera.rotation, 0.0);
    }

    /// The rulers have a unit, it is reachable from a menu, and choosing one
    /// changes what the ticks say.
    #[test]
    fn the_ruler_unit_item_changes_what_the_rulers_measure() {
        use dialogs::units::Unit as Chosen;
        let ctx = workspace_ctx(1.0);
        let doc = Document::new(1200, 900, "Test");
        let history = History::new();
        let mut w = Workspace::new();
        w.canvas.resolution_ppi = 300.0;
        workspace_frame(&mut w, &ctx, &doc, &history, 1.0, Vec::new());
        assert_eq!(w.canvas.view.ruler_spec.unit, canvas::Unit::Pixels);

        // Pixels is already in use, so its item says so rather than pretending.
        let context = w.menu_context(&doc, &history);
        assert_eq!(
            MenuAction::SetRulerUnit(Chosen::Pixels)
                .resolve(&context)
                .reason(),
            Some("The rulers already read in this unit")
        );
        assert_eq!(
            MenuAction::SetRulerUnit(Chosen::Pixels).checked(&context),
            Some(true)
        );

        invoke(
            &mut w,
            &doc,
            &history,
            MenuAction::SetRulerUnit(Chosen::Inches),
        );
        workspace_frame(&mut w, &ctx, &doc, &history, 1.0, Vec::new());
        assert_eq!(w.canvas.view.ruler_spec.unit, canvas::Unit::Inches);
        assert_eq!(w.canvas.view.ruler_spec.dpi, 300.0);

        w.canvas.view.camera.set_zoom(1.0);
        let ticks = canvas::rulers::ruler_ticks(
            &w.canvas.view.camera,
            w.canvas.view.viewport(),
            canvas::Axis::X,
            &w.canvas.view.ruler_spec,
        );
        assert!(!ticks.is_empty());
        for t in &ticks {
            // An inch is 300 document pixels on this document, not 72.
            assert!((t.doc - t.value * 300.0).abs() < 1e-2, "{t:?}");
        }
    }

    /// Zoom to fit / 100% / in / out are menu items, and they move the camera
    /// the canvas actually draws with — not a second number beside it.
    #[test]
    fn the_zoom_commands_move_the_canvas_camera() {
        let ctx = workspace_ctx(1.0);
        let doc = Document::new(4000, 3000, "Big");
        let history = History::new();
        let mut w = Workspace::new();
        workspace_frame(&mut w, &ctx, &doc, &history, 1.0, Vec::new());

        invoke(
            &mut w,
            &doc,
            &history,
            MenuAction::Zoom(menu::ZoomCommand::FitOnScreen),
        );
        let visible = w
            .canvas
            .view
            .camera
            .visible_doc_rect(w.canvas.view.viewport());
        assert!(
            visible.min.x <= 0.0
                && visible.min.y <= 0.0
                && visible.max.x >= 4000.0
                && visible.max.y >= 3000.0,
            "Fit on Screen left part of the document off screen: {visible:?}"
        );
        assert!(w.canvas.view.camera.zoom < 1.0);
        // The readout followed the camera rather than being set separately.
        assert_eq!(w.status.zoom, w.canvas.view.camera.zoom);

        invoke(
            &mut w,
            &doc,
            &history,
            MenuAction::Zoom(menu::ZoomCommand::ActualPixels),
        );
        assert_eq!(w.canvas.view.camera.zoom, 1.0);
        assert_eq!(w.status.zoom, 1.0);

        invoke(
            &mut w,
            &doc,
            &history,
            MenuAction::Zoom(menu::ZoomCommand::In),
        );
        assert!(w.canvas.view.camera.zoom > 1.0);
        assert_eq!(w.status.zoom, w.canvas.view.camera.zoom);
        invoke(
            &mut w,
            &doc,
            &history,
            MenuAction::Zoom(menu::ZoomCommand::Out),
        );
        assert_eq!(w.canvas.view.camera.zoom, 1.0);

        // Print Size is a real command too, not a decoration: at 72 ppi and 1x
        // it is 96/72 of actual size.
        invoke(
            &mut w,
            &doc,
            &history,
            MenuAction::Zoom(menu::ZoomCommand::PrintSize),
        );
        assert!(
            (w.canvas.view.camera.zoom - canvas::workspace::POINTS_PER_INCH / 72.0).abs() < 1e-3,
            "{}",
            w.canvas.view.camera.zoom
        );
    }

    /// Fill Screen and Zoom to Selection were implemented on the canvas,
    /// tested, and reachable from no control in the running application: the
    /// View menu listed five zoom commands and `absorb_action` handled exactly
    /// those five.
    #[test]
    fn fill_screen_and_zoom_to_selection_are_reachable_and_do_something() {
        use editor_core::Selection;
        use glam::IVec2;
        use menu::ZoomCommand;

        let ctx = workspace_ctx(1.0);
        let mut doc = Document::new(4000, 3000, "Big");
        let history = History::new();
        let mut w = Workspace::new();
        workspace_frame(&mut w, &ctx, &doc, &history, 1.0, Vec::new());

        // Both are in the View menu at all, which is where this started.
        let in_the_menu: Vec<MenuAction> = menu::menu_bar(0)
            .iter()
            .flat_map(menu::Menu::actions)
            .collect();
        assert!(in_the_menu.contains(&MenuAction::Zoom(ZoomCommand::FillScreen)));
        assert!(in_the_menu.contains(&MenuAction::Zoom(ZoomCommand::ToSelection)));

        // Fill Screen goes past Fit on Screen, which is the whole difference
        // between them on a document that is not the viewport's shape.
        invoke(
            &mut w,
            &doc,
            &history,
            MenuAction::Zoom(ZoomCommand::FitOnScreen),
        );
        let fit = w.canvas.view.camera.zoom;
        invoke(
            &mut w,
            &doc,
            &history,
            MenuAction::Zoom(ZoomCommand::FillScreen),
        );
        assert!(
            w.canvas.view.camera.zoom > fit,
            "Fill Screen left the view fitted ({} vs {fit})",
            w.canvas.view.camera.zoom
        );
        assert_eq!(w.status.zoom, w.canvas.view.camera.zoom);

        // With nothing selected, Zoom to Selection is disabled *and says why* —
        // never an item that looks live and quietly does nothing.
        let context = w.menu_context(&doc, &history);
        let resolved = MenuAction::Zoom(ZoomCommand::ToSelection).resolve(&context);
        assert!(!resolved.is_enabled());
        assert_eq!(resolved.reason(), Some("Nothing is selected"));

        // …and with one, it frames it.
        doc.selection = Selection::Rect {
            min: IVec2::new(1000, 1200),
            max: IVec2::new(1100, 1260),
        };
        workspace_frame(&mut w, &ctx, &doc, &history, 1.0, Vec::new());
        let before = w.canvas.view.camera.center;
        invoke(
            &mut w,
            &doc,
            &history,
            MenuAction::Zoom(ZoomCommand::ToSelection),
        );
        assert_ne!(w.canvas.view.camera.center, before);
        assert!(
            (w.canvas.view.camera.center - glam::Vec2::new(1050.0, 1230.0)).length() < 1e-3,
            "{:?}",
            w.canvas.view.camera.center
        );
        assert_eq!(w.status.zoom, w.canvas.view.camera.zoom);
    }

    /// A zoom typed into the status bar reaches the camera, and a zoom the
    /// *canvas* performs reaches the readout.
    #[test]
    fn the_zoom_readout_and_the_camera_are_one_number() {
        let ctx = workspace_ctx(1.0);
        let doc = Document::new(320, 240, "Test");
        let history = History::new();
        let mut w = Workspace::new();
        workspace_frame(&mut w, &ctx, &doc, &history, 1.0, Vec::new());

        assert!(w.absorb(&Intent::SetZoom(2.0)));
        assert_eq!(w.canvas.view.camera.zoom, 2.0, "SetZoom missed the camera");
        assert_eq!(w.status.zoom, 2.0);

        // …and the wheel, which is the direction that used to be broken: the
        // canvas zoomed and the status bar went on claiming the old figure.
        let at = w.canvas.view.viewport().center_pt();
        workspace_frame(
            &mut w,
            &ctx,
            &doc,
            &history,
            1.0,
            vec![
                egui::Event::PointerMoved(egui::pos2(at.x, at.y)),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Line,
                    delta: egui::vec2(0.0, 3.0),
                    modifiers: egui::Modifiers::COMMAND,
                },
            ],
        );
        assert!(
            w.canvas.view.camera.zoom > 2.0,
            "the wheel did not zoom the canvas"
        );
        assert_eq!(
            w.status.zoom, w.canvas.view.camera.zoom,
            "the status bar still shows a zoom the canvas is not using"
        );
    }

    /// What one canvas frame produced is read, not dropped: the pointer readout
    /// the Info panel shows, and the tool samples the application runs.
    #[test]
    fn the_workspace_consumes_what_the_canvas_frame_produced() {
        let ctx = workspace_ctx(1.0);
        let doc = Document::new(320, 240, "Test");
        let history = History::new();
        let mut w = Workspace::new();
        w.palette.activate(&PaletteModel::build(), ToolId::Brush);
        workspace_frame(&mut w, &ctx, &doc, &history, 1.0, Vec::new());

        let at = w.canvas.view.viewport().center_pt();
        workspace_frame(
            &mut w,
            &ctx,
            &doc,
            &history,
            1.0,
            vec![
                egui::Event::PointerMoved(egui::pos2(at.x, at.y)),
                egui::Event::PointerButton {
                    pos: egui::pos2(at.x, at.y),
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::default(),
                },
            ],
        );

        let pointer = w.info.pointer.expect("the Info panel readout was not fed");
        let want = w
            .canvas
            .view
            .camera
            .doc_of_screen_pt(w.canvas.view.viewport(), at);
        assert!((pointer.0 - want.x).abs() < 1e-3 && (pointer.1 - want.y).abs() < 1e-3);

        // The hover that positioned the pointer and the press that followed it.
        let events = w.drain_canvas_events();
        assert_eq!(events.len(), 2, "{events:?}");
        assert!(events
            .iter()
            .all(|e| e.route == canvas::Route::Tool(ToolId::Brush)));
        assert_eq!(
            events.last().map(|e| e.phase),
            Some(canvas::PointerPhase::Down)
        );
        // Drained, not replayed.
        assert!(w.drain_canvas_events().is_empty());
        assert!(!w.grid_is_suppressed());
    }

    /// The overlay sessions the application owns are drawn by the workspace's
    /// own frame — the whole path from `Workspace::ui` to a handle on screen.
    #[test]
    fn a_live_transform_session_draws_its_handles_through_the_workspace() {
        let ctx = workspace_ctx(1.0);
        let doc = Document::new(320, 240, "Test");
        let history = History::new();
        let mut w = Workspace::new();
        w.palette
            .activate(&PaletteModel::build(), ToolId::FreeTransform);
        workspace_frame(&mut w, &ctx, &doc, &history, 1.0, Vec::new());
        let bare = workspace_frame(&mut w, &ctx, &doc, &history, 1.0, Vec::new()).len();

        w.canvas.sessions.transform = Some((
            tools::transform::TransformState::new(raster::PixelRect::new(20, 20, 200, 160)),
            tools::transform::TransformMode::Scale,
        ));
        let drawn = workspace_frame(&mut w, &ctx, &doc, &history, 1.0, Vec::new()).len();
        assert!(
            drawn > bare,
            "a live transform drew no handles in the application ({drawn} vs {bare})"
        );

        // …and the corner is grabbable: the cursor over it is the scale arrow.
        let corner = w
            .canvas
            .view
            .camera
            .screen_pt_of(w.canvas.view.viewport(), glam::Vec2::new(20.0, 20.0));
        let (state, mode) = w.canvas.sessions.transform.clone().unwrap();
        let content = canvas::CanvasContent {
            doc_size: glam::Vec2::new(320.0, 240.0),
            active_tool: ToolId::FreeTransform,
            transform: Some((&state, mode)),
            ..canvas::CanvasContent::default()
        };
        assert_eq!(
            w.canvas.view.resolve_cursor(&content, Some(corner)),
            canvas::CanvasCursor::ResizeNwSe
        );

        w.canvas.sessions.clear();
        assert_eq!(
            workspace_frame(&mut w, &ctx, &doc, &history, 1.0, Vec::new()).len(),
            bare
        );
    }

    /// The canvas is reachable, and a press inside it reaches the active tool.
    #[test]
    fn a_press_on_the_workspace_canvas_reaches_the_active_tool() {
        let ctx = workspace_ctx(1.0);
        let doc = Document::new(320, 240, "Test");
        let history = History::new();
        let mut w = Workspace::new();
        w.palette.activate(&PaletteModel::build(), ToolId::Brush);
        workspace_frame(&mut w, &ctx, &doc, &history, 1.0, Vec::new());

        let at = w.canvas.view.viewport().center_pt();
        workspace_frame(
            &mut w,
            &ctx,
            &doc,
            &history,
            1.0,
            vec![egui::Event::PointerButton {
                pos: egui::pos2(at.x, at.y),
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            }],
        );
        assert_eq!(
            w.canvas.last.tool_events.len(),
            1,
            "the canvas is drawn but nothing reaches the tool: {:?}",
            w.canvas.last
        );
        assert_eq!(
            w.canvas.last.tool_events[0].route,
            canvas::Route::Tool(ToolId::Brush)
        );
    }
}
