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
//! * the delay field renames the layer ([`set_delay`]) — once per drag, on
//!   the release, so one drag of the field is one undo step; a typed value
//!   lands once, when the field is confirmed (Enter or focus leaves it), not
//!   on every keystroke;
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
//! Every word the panel shows resolves through the localization catalogue
//! ([`crate::strings::tr`]), under the `ui.animation.*` keys.

use design::{color32, current_tokens, egui_theme::rounding, ColorRole, Radius, Space, TextRole};
use editor_core::{Command, Document, LayerPatch, PixelTarget, TileEdit};
use egui::{Align, Layout, Sense, Ui, Vec2};
use layer_model::{Layer, LayerId};
use raster::animation::{frame_layer_name, parse_frame_layer_name, DEFAULT_FRAME_DELAY_MS};

// W13-L: Timeline mode (layer bars, opacity / position keyframes, the
// playhead), behind the panel's Frames / Timeline switch.
#[path = "animation_timeline.rs"]
pub mod timeline;

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

/// The catalogue keys of the panel's words (see [`crate::strings`]).
const PLAY: &str = "ui.animation.play";
const STOP: &str = "ui.animation.stop";
const ONION: &str = "ui.animation.onion";
const ADD: &str = "ui.animation.add";
const DUPLICATE: &str = "ui.animation.duplicate";
const DELETE: &str = "ui.animation.delete";
const NO_DOCUMENT: &str = "ui.animation.no_document";
const NO_FRAMES: &str = "ui.animation.no_frames";
const MS: &str = "ui.animation.ms";

fn tr(key: &str) -> &'static str {
    crate::strings::tr(key)
}

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
        label: tr(DUPLICATE).to_string(),
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
        label: format!(
            "Show {}",
            doc.layers.get(layer).map(|l| l.name.as_str()).unwrap_or("")
        ),
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
        empty_state(ui, tr(NO_DOCUMENT));
        return;
    }
    // W13-L: the Frames / Timeline switch; Timeline mode draws its own body.
    if timeline::mode_toggle(w, ui, doc) {
        timeline::timeline_body(w, ui, doc);
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
        let label = tr(if state.playing { STOP } else { PLAY });
        let play = ui.add_enabled(frames.len() > 1, egui::Button::new(body(ui, label)));
        crate::view::mark(ui, play.rect, ids::play());
        if play.clicked() {
            state.playing = !state.playing;
            state.shown_at = now;
        }
        let onion = ui.checkbox(&mut state.onion, body(ui, tr(ONION)));
        crate::view::mark(ui, onion.rect, ids::onion());
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let has_frame = if current.is_some() {
                ActionState::Idle
            } else {
                ActionState::Disabled
            };
            if icon_action_id(ui, "trash", tr(DELETE), has_frame, Some(ids::delete())).clicked() {
                if let Some(c) = current.and_then(|id| delete_frame(doc, id)) {
                    w.emit(Intent::Document(c));
                }
                state.playing = false;
            }
            if icon_action_id(
                ui,
                "layer-raster",
                tr(DUPLICATE),
                has_frame,
                Some(ids::duplicate()),
            )
            .clicked()
            {
                if let Some(c) = current.and_then(|id| duplicate_frame(doc, id)) {
                    w.emit(Intent::Document(c));
                }
                state.current += 1;
                state.playing = false;
            }
            if icon_action_id(ui, "plus", tr(ADD), ActionState::Idle, Some(ids::add())).clicked() {
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
        ui.label(hint(ui, tr(NO_FRAMES)));
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
                        // While the field is dragged its value is held here,
                        // not written: the rename lands once, on the release,
                        // so one drag is one undo step. A typed value is
                        // held by the text edit until Enter / focus loss
                        // (`update_while_editing(false)`), so typing a
                        // multi-digit delay is one rename, not one per key.
                        let pending_key =
                            egui::Id::new(("raster-animation-delay-drag", frame.layer));
                        let pending: Option<u32> = ui.data(|d| d.get_temp(pending_key));
                        let mut delay = pending.unwrap_or(frame.delay_ms);
                        let field = ui.add_sized(
                            Vec2::new(cell.x, t.metrics.control_height),
                            egui::DragValue::new(&mut delay)
                                .range(MIN_DELAY_MS..=MAX_DELAY_MS)
                                .update_while_editing(false)
                                .suffix(tr(MS)),
                        );
                        crate::view::mark(ui, field.rect, ids::delay(index));
                        let commit = if field.drag_stopped() {
                            ui.data_mut(|d| d.remove::<u32>(pending_key));
                            Some(delay)
                        } else if field.dragged() {
                            if field.changed() {
                                ui.data_mut(|d| d.insert_temp(pending_key, delay));
                            }
                            None
                        } else {
                            field.changed().then_some(delay)
                        };
                        if let Some(c) = commit.and_then(|v| set_delay(doc, frame.layer, v)) {
                            w.emit(Intent::Document(c));
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

#[cfg(test)]
mod tests {
    //! The panel driven the way a user drives it: the whole [`Workspace`]
    //! draws its dock with only the Animation panel open (the panel's real
    //! route, `docks::body_of`), clicks land by the controls' ids, and the
    //! emitted commands are applied to the document as the shell applies
    //! them.

    use super::*;
    use crate::dock::{LayoutId, PanelId};
    use editor_core::History;

    struct Live {
        ctx: egui::Context,
        w: Workspace,
        doc: Document,
        history: History,
        time: f64,
    }

    impl Live {
        fn new(doc: Document) -> Self {
            let ctx = egui::Context::default();
            design::apply_theme(&ctx, design::Theme::Dark);
            let mut w = Workspace::new();
            w.dock.apply_layout(LayoutId::Minimal);
            w.dock.set_open(PanelId::Animation, true);
            let mut live = Self {
                ctx,
                w,
                doc,
                history: History::new(),
                time: 0.0,
            };
            live.thumbs();
            for _ in 0..3 {
                live.frame(Vec::new());
            }
            live
        }

        /// A distinct one-pixel texture per layer, standing in for the
        /// thumbnails the shell uploads, so a painted image names its frame.
        fn thumbs(&mut self) {
            for id in self.doc.layers.iter_depth_first() {
                if !self.w.layer_thumbs.contains_key(&id) {
                    let tex = self.ctx.load_texture(
                        format!("thumb-{id:?}"),
                        egui::ColorImage::new([1, 1], egui::Color32::WHITE),
                        egui::TextureOptions::NEAREST,
                    );
                    self.w.layer_thumbs.insert(id, tex);
                }
            }
        }

        fn frame(&mut self, events: Vec<egui::Event>) -> (Vec<Intent>, egui::FullOutput) {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1400.0, 900.0),
                )),
                time: Some(self.time),
                events,
                ..Default::default()
            };
            let (w, doc, history) = (&mut self.w, &self.doc, &self.history);
            let out = self.ctx.run(input, |ctx| w.ui(ctx, doc, history));
            (self.w.drain_intents(), out)
        }

        fn rect(&self, id: egui::Id) -> egui::Rect {
            self.ctx
                .read_response(id)
                .unwrap_or_else(|| panic!("{id:?} was not drawn"))
                .rect
        }

        fn click(&mut self, id: egui::Id) -> Vec<Intent> {
            let at = self.rect(id).center();
            let button = |pressed| egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            };
            let (mut intents, _) = self.frame(vec![egui::Event::PointerMoved(at), button(true)]);
            intents.extend(self.frame(vec![button(false)]).0);
            intents
        }

        /// Apply every document command in `intents` through the history, as
        /// the shell does, so each is one undo step.
        fn apply(&mut self, intents: &[Intent]) -> usize {
            let mut applied = 0;
            for intent in intents {
                if let Intent::Document(c) = intent {
                    self.history
                        .apply(&mut self.doc, c.clone())
                        .expect("the command applies");
                    applied += 1;
                }
            }
            self.thumbs();
            for _ in 0..2 {
                self.frame(Vec::new());
            }
            applied
        }

        /// The textures painted inside the preview well, in paint order,
        /// with the tint each was painted with.
        fn preview_images(&mut self) -> Vec<(egui::TextureId, egui::Color32)> {
            let well = self.rect(ids::preview());
            let (_, out) = self.frame(Vec::new());
            let mut found = Vec::new();
            for clipped in &out.shapes {
                collect_images(&clipped.shape, well, &mut found);
            }
            found
        }
    }

    fn collect_images(
        shape: &egui::Shape,
        well: egui::Rect,
        out: &mut Vec<(egui::TextureId, egui::Color32)>,
    ) {
        match shape {
            egui::Shape::Vec(shapes) => {
                for s in shapes {
                    collect_images(s, well, out);
                }
            }
            egui::Shape::Mesh(mesh) if mesh.texture_id != egui::TextureId::default() => {
                let inside = mesh
                    .vertices
                    .iter()
                    .all(|v| well.expand(1.0).contains(v.pos));
                if inside && !mesh.vertices.is_empty() {
                    out.push((mesh.texture_id, mesh.vertices[0].color));
                }
            }
            _ => {}
        }
    }

    /// Three frames, 100 / 200 / 300 ms, pushed bottom first.
    fn three_frames() -> (Document, [LayerId; 3]) {
        let mut doc = Document::new(64, 48, "anim");
        let mut ids = [LayerId::new(); 3];
        for (i, delay) in [100u32, 200, 300].into_iter().enumerate() {
            let name = frame_layer_name(&format!("Frame {}", i + 1), delay);
            ids[i] = doc.layers.push_root(Layer::raster(name)).unwrap();
        }
        (doc, ids)
    }

    #[test]
    fn frames_are_the_top_level_a_layers_bottom_first_with_their_delays() {
        let (mut doc, ids) = three_frames();
        doc.layers.push_root(Layer::raster("Not a frame")).unwrap();
        let got = frames(&doc);
        assert_eq!(
            got.iter()
                .map(|f| (f.layer, f.delay_ms))
                .collect::<Vec<_>>(),
            vec![(ids[0], 100), (ids[1], 200), (ids[2], 300)]
        );
        assert_eq!(got[0].label, "Frame 1");
    }

    /// The Window-menu panel draws one thumbnail cell per frame in play
    /// order, each with its delay field under it; clicking a cell selects
    /// that frame's layer and shows it alone among the frames, one command,
    /// and the preview well draws that frame.
    #[test]
    fn the_strip_draws_each_frame_and_a_click_shows_that_frame_alone() {
        let (doc, ids) = three_frames();
        let mut live = Live::new(doc);
        let cells: Vec<egui::Rect> = (0..3).map(|i| live.rect(ids::frame(i))).collect();
        assert!(cells[0].right() <= cells[1].left() && cells[1].right() <= cells[2].left());
        for (i, cell) in cells.iter().enumerate() {
            let delay = live.rect(ids::delay(i));
            assert!(delay.top() >= cell.bottom(), "delay {i} is under its cell");
        }
        let intents = live.click(ids::frame(1));
        assert!(
            intents.contains(&Intent::SelectLayers {
                layers: vec![ids[1]],
                active: Some(ids[1]),
            }),
            "{intents:?}"
        );
        assert_eq!(live.apply(&intents), 1, "one show command: {intents:?}");
        let visible: Vec<bool> = ids
            .iter()
            .map(|id| live.doc.layers.get(*id).unwrap().visible)
            .collect();
        assert_eq!(visible, vec![false, true, false]);
        assert_eq!(playback(&live.ctx).current, 1);
        let want = live.w.layer_thumbs[&ids[1]].id();
        let images = live.preview_images();
        assert_eq!(images.last().map(|i| i.0), Some(want), "{images:?}");
    }

    /// Add, Duplicate and Delete each emit one command that, applied, makes
    /// the frame list what the button says.
    #[test]
    fn add_duplicate_and_delete_frame_edit_the_frame_layers() {
        let (doc, ids) = three_frames();
        let mut live = Live::new(doc);

        let intents = live.click(ids::add());
        assert_eq!(live.apply(&intents), 1, "{intents:?}");
        let after_add = frames(&live.doc);
        assert_eq!(after_add.len(), 4);
        assert_eq!(after_add[3].label, "Frame 4");
        assert_eq!(
            after_add[3].delay_ms, 300,
            "the delay of the frame before it"
        );

        // Pick frame 1 (index 0), then duplicate it: the copy comes right
        // after it in play order, with its delay.
        let intents = live.click(ids::frame(0));
        live.apply(&intents);
        let intents = live.click(ids::duplicate());
        assert_eq!(live.apply(&intents), 1, "{intents:?}");
        let after_dup = frames(&live.doc);
        assert_eq!(after_dup.len(), 5);
        assert_eq!(after_dup[0].layer, ids[0]);
        assert_eq!(after_dup[1].delay_ms, 100, "the copy keeps the delay");
        assert_ne!(after_dup[1].layer, ids[0]);
        assert_eq!(
            playback(&live.ctx).current,
            1,
            "the copy is the current frame"
        );

        let doomed = frames(&live.doc)[playback(&live.ctx).current].layer;
        let intents = live.click(ids::delete());
        let deletes = intents
            .iter()
            .filter(|i| {
                matches!(i, Intent::Document(Command::DeleteLayer { layer_id }) if *layer_id == doomed)
            })
            .count();
        assert_eq!(deletes, 1, "{intents:?}");
        live.apply(&intents);
        assert_eq!(frames(&live.doc).len(), 4);
        assert!(!live.doc.layers.contains(doomed));
    }

    /// Dragging a frame's delay field renames its layer to the new delay:
    /// the `_a_<label>,<ms>` form Export As reads. Every frame's commands are
    /// applied as they come, through the history, the way the shell applies
    /// them — and the whole drag is ONE undo step, which one undo takes back.
    #[test]
    fn dragging_a_delay_field_renames_the_frame_to_the_new_delay() {
        let (doc, ids) = three_frames();
        let mut live = Live::new(doc);
        let field = live.rect(ids::delay(0)).center();
        let button = |pressed, pos| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        let intents = live
            .frame(vec![egui::Event::PointerMoved(field), button(true, field)])
            .0;
        live.apply(&intents);
        let mut shown = Vec::new();
        for step in 1..=6 {
            let at = field + egui::vec2(10.0 * step as f32, 0.0);
            let intents = live.frame(vec![egui::Event::PointerMoved(at)]).0;
            live.apply(&intents);
            // What the field shows mid-drag: its painted value.
            let (_, out) = live.frame(Vec::new());
            let rect = live.rect(ids::delay(0));
            shown.extend(painted_texts_in(&out, rect));
        }
        assert_eq!(
            live.history.undo_depth(),
            0,
            "nothing lands while the field is dragged"
        );
        shown.dedup();
        assert!(
            shown.len() >= 3,
            "the field showed the value moving mid-drag: {shown:?}"
        );
        let end = field + egui::vec2(60.0, 0.0);
        let intents = live.frame(vec![button(false, end)]).0;
        assert_eq!(live.apply(&intents), 1, "the release lands the rename");
        assert_eq!(live.history.undo_depth(), 1, "one drag, one undo step");
        let frame = frames(&live.doc)[0].clone();
        assert_eq!(frame.layer, ids[0]);
        assert!(
            frame.delay_ms > 100,
            "the drag right raised the delay: {frame:?}"
        );
        assert_eq!(
            live.doc.layers.get(ids[0]).unwrap().name,
            frame_layer_name("Frame 1", frame.delay_ms)
        );
        // One undo takes the whole drag back.
        live.history.undo(&mut live.doc).unwrap();
        assert_eq!(frames(&live.doc)[0].delay_ms, 100);
        // The model refuses a delay below GIF's floor.
        let c = set_delay(&live.doc, ids[0], 0).expect("a change");
        c.apply(&mut live.doc).unwrap();
        assert_eq!(frames(&live.doc)[0].delay_ms, MIN_DELAY_MS);
    }

    /// Typing a delay is ONE edit: click the field (which opens its text
    /// edit), select all, type `2`, `5`, `0` one key per frame, press Enter.
    /// Nothing lands while the keys go in (no rename to a clamped `2` or to
    /// `25`), and Enter lands exactly one rename to 250 ms, one undo step.
    #[test]
    fn typing_a_multi_digit_delay_is_one_rename_and_one_undo_step() {
        let (doc, ids) = three_frames();
        let mut live = Live::new(doc);
        let intents = live.click(ids::delay(0));
        assert_eq!(live.apply(&intents), 0, "a click only opens the text edit");
        let key = |key, modifiers| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        };
        let intents = live
            .frame(vec![key(egui::Key::A, egui::Modifiers::COMMAND)])
            .0;
        live.apply(&intents);
        let mut names = Vec::new();
        for ch in ["2", "5", "0"] {
            let intents = live.frame(vec![egui::Event::Text(ch.into())]).0;
            assert_eq!(live.apply(&intents), 0, "the key {ch} lands nothing");
            names.push(live.doc.layers.get(ids[0]).unwrap().name.clone());
        }
        assert_eq!(live.history.undo_depth(), 0, "nothing lands mid-typing");
        assert!(
            names.iter().all(|n| *n == frame_layer_name("Frame 1", 100)),
            "no intermediate rename while typing: {names:?}"
        );
        let intents = live
            .frame(vec![key(egui::Key::Enter, egui::Modifiers::NONE)])
            .0;
        let mut applied = live.apply(&intents);
        for _ in 0..2 {
            let intents = live.frame(Vec::new()).0;
            applied += live.apply(&intents);
        }
        assert_eq!(applied, 1, "Enter lands the typed delay once");
        assert_eq!(
            live.history.undo_depth(),
            1,
            "one typed edit, one undo step"
        );
        assert_eq!(frames(&live.doc)[0].delay_ms, 250);
        assert_eq!(
            live.doc.layers.get(ids[0]).unwrap().name,
            frame_layer_name("Frame 1", 250)
        );
        live.history.undo(&mut live.doc).unwrap();
        assert_eq!(frames(&live.doc)[0].delay_ms, 100);
    }

    /// The painted strings whose top-left lies inside `rect`.
    fn painted_texts_in(out: &egui::FullOutput, rect: egui::Rect) -> Vec<String> {
        out.shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Text(t) if rect.expand(1.0).contains(t.pos) => {
                    Some(t.galley.text().to_string())
                }
                _ => None,
            })
            .collect()
    }

    /// The panel's words come from the localization catalogue: the painted
    /// transport and empty-state strings are exactly the `ui.animation.*`
    /// rows.
    #[test]
    fn the_panels_words_are_the_catalogues() {
        let (doc, _) = three_frames();
        let mut live = Live::new(doc);
        let (_, out) = live.frame(Vec::new());
        let texts = painted_texts_in(&out, egui::Rect::EVERYTHING);
        for key in [PLAY, ONION] {
            let word = crate::strings::tr(key);
            assert!(!word.is_empty(), "{key} has no catalogue row");
            assert!(
                texts.iter().any(|t| t == word),
                "{word:?} not drawn: {texts:?}"
            );
        }
        assert!(
            texts.iter().any(|t| t.ends_with(crate::strings::tr(MS))),
            "the delay suffix is the catalogue's: {texts:?}"
        );
        let mut empty = Live::new(Document::new(64, 48, "empty"));
        let (_, out) = empty.frame(Vec::new());
        let texts = painted_texts_in(&out, egui::Rect::EVERYTHING);
        let hint = crate::strings::tr(NO_FRAMES);
        assert!(!hint.is_empty());
        assert!(texts.iter().any(|t| t == hint), "{texts:?}");
    }

    /// Play steps the preview through the frames at their own delays and
    /// Stop holds it; neither touches the document. The onion skin draws the
    /// previous frame faintly under the current one.
    #[test]
    fn play_steps_the_preview_at_each_frames_delay_and_onion_skin_shows_the_previous() {
        let (doc, ids) = three_frames();
        let mut live = Live::new(doc);
        let intents = live.click(ids::play());
        assert!(
            !intents.iter().any(|i| matches!(i, Intent::Document(_))),
            "playback is view state: {intents:?}"
        );
        assert!(playback(&live.ctx).playing);
        let started = live.time;
        let mut seen = Vec::new();
        for ms in [50, 150, 320, 650] {
            live.time = started + f64::from(ms) / 1000.0;
            let (intents, _) = live.frame(Vec::new());
            assert!(!intents.iter().any(|i| matches!(i, Intent::Document(_))));
            seen.push(playback(&live.ctx).current);
        }
        // 0-100 ms frame 1, 100-300 frame 2, 300-600 frame 3, then it loops.
        assert_eq!(seen, vec![0, 1, 2, 0]);
        let showing = live.w.layer_thumbs[&ids[0]].id();
        assert_eq!(live.preview_images().last().map(|i| i.0), Some(showing));

        // Stop holds the frame.
        live.click(ids::play());
        assert!(!playback(&live.ctx).playing);
        let held = playback(&live.ctx).current;
        live.time += 5.0;
        live.frame(Vec::new());
        assert_eq!(playback(&live.ctx).current, held);

        // Onion skin: the frame before the current one, painted first and
        // fainter, under the current one.
        live.click(ids::onion());
        assert!(playback(&live.ctx).onion);
        let current = playback(&live.ctx).current;
        let before = (current + 2) % 3;
        let images = live.preview_images();
        assert_eq!(images.len(), 2, "{images:?}");
        assert_eq!(images[0].0, live.w.layer_thumbs[&ids[before]].id());
        assert_eq!(images[1].0, live.w.layer_thumbs[&ids[current]].id());
        assert!(
            images[0].1.a() < images[1].1.a(),
            "the onion frame is fainter: {images:?}"
        );
    }
}
