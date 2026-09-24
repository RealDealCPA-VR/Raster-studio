//! W10-I: the Animation panel — Photopea's frame timeline
//! (photopea.com/learn/animations).
//!
//! # A frame is a layer
//!
//! Photopea keeps no separate timeline: an animation frame is a top-level
//! layer whose name starts with `_a_`, and the delay rides in the name after
//! the last comma (`_a_Frame 1,100` shows for 100 ms). This build already
//! opens animated GIF / APNG / WebP files as exactly those layers and exports
//! them back (W9-J, [`raster::animation`]); this panel is the editor over
//! them. Play order is the stack from the bottom up, the same order Export
//! As writes.
//!
//! Every edit is an ordinary document command, so each is one undo step and
//! the frames stay plain layers the Layers panel shows too:
//!
//! * the delay field renames the layer ([`set_delay`]);
//! * Add Frame creates an empty `_a_` layer on top — the new last frame
//!   ([`add_frame`]);
//! * Duplicate Frame copies the current frame's layer, pixels included (the
//!   tile hashes are content addresses, so the copy shares the stored tiles
//!   and costs nothing), right after it in play order ([`duplicate_frame`]);
//! * Delete Frame deletes the layer ([`delete_frame`]);
//! * clicking a frame selects its layer and shows it alone among the frames
//!   on the canvas ([`show_frame`]), which is how Photopea lets you look at
//!   one frame.
//!
//! # Playback is view state
//!
//! Play / Stop and the onion skin are not document state and never touch
//! the history: [`Playback`] lives in egui's memory, and the preview well
//! draws the current frame's layer thumbnail, stepping through the frames at
//! their own delays ([`Playback::tick`]). With the onion skin on, the frame
//! before the current one is drawn faintly under it.
//!
//! # Strings
//!
//! The panel's words are the constants below. They are English only: the
//! localization catalogue (`crate::strings`) belongs to another part of this
//! wave, and moving these into it is a key-per-constant follow-up.

use design::{color32, current_tokens, egui_theme::rounding, ColorRole, Radius, Space, TextRole};
use editor_core::{Command, Document, LayerPatch, PixelTarget, TileEdit};
use egui::{Align, Layout, Sense, Ui, Vec2};
use layer_model::{Layer, LayerId};
use raster::animation::{frame_layer_name, parse_frame_layer_name, DEFAULT_FRAME_DELAY_MS};

use crate::intent::Intent;
use crate::view::{body, empty_state, hint, icon_action_id, text, ActionState};
use crate::Workspace;

/// The shortest delay the panel plays or stores: GIF's own floor (1/100 s),
/// so a zero in a name cannot spin the preview.
pub const MIN_DELAY_MS: u32 = 10;

/// The longest delay the delay field accepts: one minute.
pub const MAX_DELAY_MS: u32 = 60_000;

/// How strongly the onion skin draws the previous frame under the current
/// one — a fraction of full opacity, not a colour.
const ONION_STRENGTH: f32 = 0.35;

const PLAY: &str = "Play";
const STOP: &str = "Stop";
const ONION: &str = "Onion skin";
const ADD: &str = "Add frame";
const DUPLICATE: &str = "Duplicate frame";
const DELETE: &str = "Delete frame";
const NO_DOCUMENT: &str = "Open a document to animate it.";
const NO_FRAMES: &str =
    "No frames yet. Add frame makes an _a_ layer: each one is a frame, bottom first.";
const MS: &str = " ms";

/// One frame of the document's animation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    /// The `_a_` layer the frame is.
    pub layer: LayerId,
    /// The name without the `_a_` prefix and the delay.
    pub label: String,
    /// How long the frame shows, in milliseconds.
    pub delay_ms: u32,
}

/// The document's frames in play order: its top-level `_a_` layers, bottom
/// of the stack first — the order Export As writes them (the application's
/// `import::animation_frame_layers` reads the same layers the same way).
pub fn frames(doc: &Document) -> Vec<Frame> {
    doc.layers
        .root()
        .iter()
        .rev()
        .filter_map(|id| {
            let layer = doc.layers.get(*id)?;
            let (label, delay_ms) = parse_frame_layer_name(&layer.name)?;
            Some(Frame {
                layer: *id,
                label: label.to_string(),
                delay_ms,
            })
        })
        .collect()
}

/// The panel's playback state: which frame the preview shows, whether it is
/// stepping, when the current frame came up, and the onion-skin switch.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Playback {
    pub playing: bool,
    /// Index into [`frames`].
    pub current: usize,
    /// `egui` input time (seconds) at which the current frame came up.
    pub shown_at: f64,
    pub onion: bool,
}

impl Playback {
    /// The state at time `now`: while playing, every frame whose delay has
    /// run out is stepped past (looping, as an exported GIF loops); a stale
    /// `current` past the end is clamped. A preview that fell more than one
    /// loop behind (the window was hidden) resynchronises to `now` rather
    /// than replaying every missed frame.
    pub fn tick(mut self, frames: &[Frame], now: f64) -> Self {
        if frames.is_empty() {
            self.current = 0;
            self.playing = false;
            return self;
        }
        self.current = self.current.min(frames.len() - 1);
        if !self.playing {
            return self;
        }
        let mut stepped = 0usize;
        loop {
            let delay = delay_seconds(&frames[self.current]);
            if now - self.shown_at < delay {
                break;
            }
            if stepped >= frames.len() {
                self.shown_at = now;
                break;
            }
            self.shown_at += delay;
            self.current = (self.current + 1) % frames.len();
            stepped += 1;
        }
        self
    }

    /// Seconds until the next frame is due, while playing.
    pub fn due_in(&self, frames: &[Frame], now: f64) -> Option<f64> {
        let frame = frames.get(self.current)?;
        self.playing
            .then(|| (self.shown_at + delay_seconds(frame) - now).max(0.0))
    }
}

fn delay_seconds(frame: &Frame) -> f64 {
    f64::from(frame.delay_ms.max(MIN_DELAY_MS)) / 1000.0
}

fn playback_key() -> egui::Id {
    egui::Id::new("raster-animation-playback")
}

/// The panel's [`Playback`], as the last frame left it.
pub fn playback(ctx: &egui::Context) -> Playback {
    ctx.data(|d| d.get_temp(playback_key())).unwrap_or_default()
}

fn store_playback(ctx: &egui::Context, state: Playback) {
    ctx.data_mut(|d| d.insert_temp(playback_key(), state));
}

/// The command that gives `frame` a new delay: a rename to
/// `_a_<label>,<delay>`. `None` when the layer is not a frame or the delay
/// is already that.
pub fn set_delay(doc: &Document, layer: LayerId, delay_ms: u32) -> Option<Command> {
    let current = doc.layers.get(layer)?;
    let (label, old) = parse_frame_layer_name(&current.name)?;
    let delay_ms = delay_ms.clamp(MIN_DELAY_MS, MAX_DELAY_MS);
    let name = frame_layer_name(label, delay_ms);
    (old != delay_ms || name != current.name).then(|| Command::SetLayerProperties {
        layer_id: layer,
        patch: LayerPatch {
            name: Some(name),
            ..LayerPatch::default()
        },
    })
}

/// `Frame <n>` for the lowest `n` from one past the frame count that no
/// frame is already called.
fn next_label(frames: &[Frame]) -> String {
    let mut n = frames.len() + 1;
    loop {
        let label = format!("Frame {n}");
        if !frames.iter().any(|f| f.label == label) {
            return label;
        }
        n += 1;
    }
}

/// Add Frame: an empty frame layer on top of the stack — the new last frame
/// in play order — with the delay of the frame before it.
pub fn add_frame(doc: &Document) -> Command {
    let frames = frames(doc);
    let delay = frames
        .last()
        .map(|f| f.delay_ms)
        .unwrap_or(DEFAULT_FRAME_DELAY_MS);
    Command::create_layer(Layer::raster(frame_layer_name(&next_label(&frames), delay)))
}

/// Duplicate Frame: a copy of `layer` — its pixels, opacity, blend and
/// style, under a fresh frame name with the same delay — placed right after
/// it in play order (directly above it in the stack), as one undo step.
/// The copy's tiles are the original's content hashes, so no pixel is
/// re-stored. A mask is not copied: its coverage lives under the original's
/// mask id.
pub fn duplicate_frame(doc: &Document, layer: LayerId) -> Option<Command> {
    let source = doc.layers.get(layer)?;
    let (_, delay) = parse_frame_layer_name(&source.name)?;
    let index = doc.layers.index_in_parent(layer)?;
    let mut copy = source.clone();
    copy.id = LayerId::new();
    copy.mask = None;
    copy.name = frame_layer_name(&next_label(&frames(doc)), delay);
    let new_id = copy.id;
    let mut commands = vec![Command::create_layer(copy)];
    let edits: Vec<TileEdit> = doc
        .layer_tiles(layer)
        .map(|m| m.iter().map(|(c, h)| TileEdit::set(c, h)).collect())
        .unwrap_or_default();
    if !edits.is_empty() {
        commands.push(Command::paint_tiles(PixelTarget::Layer(new_id), edits).ok()?);
    }
    commands.push(Command::MoveLayer {
        layer_id: new_id,
        parent: doc.layers.parent_of(layer),
        index,
    });
    Some(Command::Transaction {
        label: DUPLICATE.to_string(),
        commands,
    })
}

/// Delete Frame: the frame's layer goes. `None` when `layer` is not a frame.
pub fn delete_frame(doc: &Document, layer: LayerId) -> Option<Command> {
    parse_frame_layer_name(&doc.layers.get(layer)?.name)?;
    Some(Command::DeleteLayer { layer_id: layer })
}

/// Show `layer` alone among the frames: its eye on, every other frame's
/// off, other layers untouched. `None` when that is already so.
pub fn show_frame(doc: &Document, layer: LayerId) -> Option<Command> {
    let commands: Vec<Command> = frames(doc)
        .iter()
        .filter_map(|f| {
            let visible = f.layer == layer;
            let now = doc.layers.get(f.layer)?.visible;
            (now != visible).then(|| Command::SetLayerProperties {
                layer_id: f.layer,
                patch: LayerPatch {
                    visible: Some(visible),
                    ..LayerPatch::default()
                },
            })
        })
        .collect();
    (!commands.is_empty()).then(|| Command::Transaction {
        label: format!("Show {}", doc.layers.get(layer).map(|l| l.name.as_str()).unwrap_or("")),
        commands,
    })
}

/// Stable ids for the panel's controls, so a headless test can click them.
pub mod ids {
    pub fn play() -> egui::Id {
        egui::Id::new("raster-animation-play")
    }
    pub fn onion() -> egui::Id {
        egui::Id::new("raster-animation-onion")
    }
    pub fn add() -> egui::Id {
        egui::Id::new("raster-animation-add")
    }
    pub fn duplicate() -> egui::Id {
        egui::Id::new("raster-animation-duplicate")
    }
    pub fn delete() -> egui::Id {
        egui::Id::new("raster-animation-delete")
    }
    pub fn preview() -> egui::Id {
        egui::Id::new("raster-animation-preview")
    }
    /// The thumbnail cell of frame `index` (play order).
    pub fn frame(index: usize) -> egui::Id {
        egui::Id::new(("raster-animation-frame", index))
    }
    /// The delay field of frame `index`.
    pub fn delay(index: usize) -> egui::Id {
        egui::Id::new(("raster-animation-delay", index))
    }
}

/// Draw the panel.
pub(crate) fn animation_body(w: &mut Workspace, ui: &mut Ui, doc: &Document) {
    if doc.width() == 0 || doc.height() == 0 {
        empty_state(ui, NO_DOCUMENT);
        return;
    }
    let frames = frames(doc);
    let now = ui.input(|i| i.time);
    let mut state = playback(ui.ctx()).tick(&frames, now);
    let t = current_tokens(ui);
    let current = frames.get(state.current).map(|f| f.layer);

    // Transport: Play / Stop and the onion skin on the left, the frame
    // buttons on the right.
    ui.horizontal(|ui| {
        let label = if state.playing { STOP } else { PLAY };
        let play = ui.add_enabled(frames.len() > 1, egui::Button::new(body(ui, label)));
        crate::view::mark(ui, play.rect, ids::play());
        if play.clicked() {
            state.playing = !state.playing;
            state.shown_at = now;
        }
        let onion = ui.checkbox(&mut state.onion, body(ui, ONION));
        crate::view::mark(ui, onion.rect, ids::onion());
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let has_frame = if current.is_some() {
                ActionState::Idle
            } else {
                ActionState::Disabled
            };
            if icon_action_id(ui, "trash", DELETE, has_frame, Some(ids::delete())).clicked() {
                if let Some(c) = current.and_then(|id| delete_frame(doc, id)) {
                    w.emit(Intent::Document(c));
                }
                state.playing = false;
            }
            if icon_action_id(ui, "layer-raster", DUPLICATE, has_frame, Some(ids::duplicate()))
                .clicked()
            {
                if let Some(c) = current.and_then(|id| duplicate_frame(doc, id)) {
                    w.emit(Intent::Document(c));
                }
                state.current += 1;
                state.playing = false;
            }
            if icon_action_id(ui, "plus", ADD, ActionState::Idle, Some(ids::add())).clicked() {
                w.emit(Intent::Document(add_frame(doc)));
                state.current = frames.len();
                state.playing = false;
            }
        });
    });
    ui.add_space(Space::XSmall.pt());

    // The preview well: the current frame, the one before it faintly under
    // it when the onion skin is on.
    let height = t.metrics.control_height * 5.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::hover());
    crate::view::mark(ui, rect, ids::preview());
    if ui.is_rect_visible(rect) {
        let radius = Radius::Small.resolve(&t.radii, height);
        crate::view::checkerboard(ui.painter(), rect, Space::XSmall.pt());
        let fit = fitted(rect, doc.width(), doc.height());
        let full = egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0));
        if state.onion && frames.len() > 1 {
            let before = (state.current + frames.len() - 1) % frames.len();
            if let Some(tex) = w.layer_thumbs.get(&frames[before].layer) {
                ui.painter().image(
                    tex.id(),
                    fit,
                    full,
                    crate::dialogs::controls::UNTINTED.gamma_multiply(ONION_STRENGTH),
                );
            }
        }
        if let Some(tex) = current.and_then(|id| w.layer_thumbs.get(&id)) {
            ui.painter()
                .image(tex.id(), fit, full, crate::dialogs::controls::UNTINTED);
        }
        ui.painter().rect_stroke(
            rect,
            rounding(radius),
            egui::Stroke::new(
                t.borders.hairline,
                color32(t.palette.color(ColorRole::ControlStroke)),
            ),
        );
    }
    ui.add_space(Space::XSmall.pt());

    if frames.is_empty() {
        ui.label(hint(ui, NO_FRAMES));
        store_playback(ui.ctx(), state);
        return;
    }

    // The strip: one cell per frame, in play order.
    let cell_h = t.metrics.list_row_height * 1.5;
    let cell = Vec2::new(cell_h * 4.0 / 3.0, cell_h);
    egui::ScrollArea::horizontal()
        .id_salt("raster-animation-strip")
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                for (index, frame) in frames.iter().enumerate() {
                    ui.vertical(|ui| {
                        let (rect, _) = ui.allocate_exact_size(cell, Sense::hover());
                        let response = ui.interact(rect, ids::frame(index), Sense::click());
                        response.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::WidgetType::Button,
                                true,
                                frame.label.clone(),
                            )
                        });
                        if ui.is_rect_visible(rect) {
                            let radius = Radius::Small.resolve(&t.radii, cell.y);
                            crate::view::checkerboard(ui.painter(), rect, Space::XSmall.pt());
                            if let Some(tex) = w.layer_thumbs.get(&frame.layer) {
                                ui.painter().image(
                                    tex.id(),
                                    fitted(rect, doc.width(), doc.height()),
                                    egui::Rect::from_min_max(
                                        egui::Pos2::ZERO,
                                        egui::Pos2::new(1.0, 1.0),
                                    ),
                                    crate::dialogs::controls::UNTINTED,
                                );
                            }
                            let (width, role) = if index == state.current {
                                (t.borders.thick, ColorRole::SelectionStroke)
                            } else {
                                (t.borders.hairline, ColorRole::ControlStroke)
                            };
                            ui.painter().rect_stroke(
                                rect,
                                rounding(radius),
                                egui::Stroke::new(width, color32(t.palette.color(role))),
                            );
                        }
                        if response.clicked() {
                            state.current = index;
                            state.playing = false;
                            w.emit(Intent::SelectLayers {
                                layers: vec![frame.layer],
                                active: Some(frame.layer),
                            });
                            if let Some(c) = show_frame(doc, frame.layer) {
                                w.emit(Intent::Document(c));
                            }
                        }
                        ui.label(text(
                            ui,
                            format!("{}", index + 1),
                            TextRole::Secondary,
                            design::TypeRole::Caption,
                        ));
                        let mut delay = frame.delay_ms;
                        let field = ui.add_sized(
                            Vec2::new(cell.x, t.metrics.control_height),
                            egui::DragValue::new(&mut delay)
                                .range(MIN_DELAY_MS..=MAX_DELAY_MS)
                                .suffix(MS),
                        );
                        crate::view::mark(ui, field.rect, ids::delay(index));
                        if field.changed() {
                            if let Some(c) = set_delay(doc, frame.layer, delay) {
                                w.emit(Intent::Document(c));
                            }
                        }
                    });
                }
            });
        });

    if let Some(due) = state.due_in(&frames, now) {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs_f64(due));
    }
    store_playback(ui.ctx(), state);
}

/// The largest rect of the canvas's aspect centred in `rect`.
fn fitted(rect: egui::Rect, w: u32, h: u32) -> egui::Rect {
    let (w, h) = (w.max(1) as f32, h.max(1) as f32);
    let scale = (rect.width() / w).min(rect.height() / h);
    egui::Rect::from_center_size(rect.center(), Vec2::new(w * scale, h * scale))
}
