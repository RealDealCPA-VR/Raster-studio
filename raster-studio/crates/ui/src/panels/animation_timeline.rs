//! W13-L: the Animation panel's Timeline mode (photopea.com/learn/video).
//!
//! A Frames / Timeline switch sits at the top of the panel; it is document
//! state ([`model::set_mode`]), because Export As writes the timeline's frames
//! while Timeline mode is on and the `_a_` frame layers otherwise.
//!
//! In Timeline mode the panel shows, under the transport:
//!
//! * the **ruler** — click or drag it to move the playhead. Every frame the
//!   time under the pointer changes, the panel raises
//!   [`Intent::SeekTimeline`]; the application answers with
//!   [`model::seek`], which puts each tracked layer's visibility, opacity and
//!   position at that time on the layers, so the canvas (which draws the
//!   compositor's output of the document) follows the scrub live;
//! * one **row per top-level layer**, top of the stack first: a bar from the
//!   layer's in point to its out point (drag either end handle to move it),
//!   with the layer's keys on it in four lanes, top to bottom: opacity,
//!   position, scale, rotation — click a key to select it, drag it to move
//!   it; a Hold key is drawn square, the others as diamonds;
//! * **Opacity key / Position key / Scale key / Rotation key** add a key at
//!   the playhead to the active layer holding its current value (scale and
//!   rotation turn about the layer centre), the trash button deletes the
//!   selected key, and **Linear / Ease In / Ease Out / Hold** set the
//!   selected key's interpolation (W13X-9).
//!
//! Every edit is one document command (one undo step), landed once on the
//! release of a drag, never per pointer move. Moving the playhead is not an
//! edit (as in Photopea): a scrub, each playback frame and Stop raise
//! [`Intent::SeekTimeline`], which is no history step and does not make the
//! document dirty. Play advances the playhead at the timeline's frame rate
//! and seeks the canvas to each frame; the preview well beside the transport
//! draws each layer's thumbnail at its interpolated opacity and transform
//! (position, scale and rotation, as a textured quad).

use design::{color32, current_tokens, egui_theme::rounding, ColorRole, Radius, Space};
use editor_core::timeline::{self as model, KeyProperty};
use editor_core::Document;
use egui::{Align, Layout, Pos2, Rect, Sense, Ui, Vec2};
use layer_model::LayerId;

use super::{tr, MS, PLAY, STOP};
use crate::intent::Intent;
use crate::view::{body, hint, icon_action_id, ActionState};
use crate::Workspace;

pub(super) const MODE_FRAMES: &str = "ui.animation.mode.frames";
pub(super) const MODE_TIMELINE: &str = "ui.animation.mode.timeline";
const FPS: &str = "ui.animation.fps";
const LENGTH: &str = "ui.animation.length";
const KEY_OPACITY: &str = "ui.animation.key.opacity";
const KEY_POSITION: &str = "ui.animation.key.position";
const KEY_DELETE: &str = "ui.animation.key.delete";
// W13X-9: scale / rotation keys and per-key interpolation.
const KEY_SCALE: &str = "ui.animation.key.scale";
const KEY_ROTATION: &str = "ui.animation.key.rotation";
const INTERP_LINEAR: &str = "ui.animation.interp.linear";
const INTERP_EASE_IN: &str = "ui.animation.interp.ease_in";
const INTERP_EASE_OUT: &str = "ui.animation.interp.ease_out";
const INTERP_HOLD: &str = "ui.animation.interp.hold";

/// The catalogue key naming `how`.
fn interp_label(how: model::Interpolation) -> &'static str {
    match how {
        model::Interpolation::Linear => INTERP_LINEAR,
        model::Interpolation::EaseIn => INTERP_EASE_IN,
        model::Interpolation::EaseOut => INTERP_EASE_OUT,
        model::Interpolation::Hold => INTERP_HOLD,
    }
}
const NO_LAYERS: &str = "ui.animation.no_layers";

/// A drag in flight: what is being dragged and the time it is over. Held in
/// view state and landed as one command on the release.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Drag {
    In(LayerId, u32),
    Out(LayerId, u32),
    Key(LayerId, KeyProperty, usize, u32),
}

/// The Timeline view's state: playback, a ruler scrub, the selected key and
/// a drag in flight. Not document state; lives in egui's memory.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct TimelineView {
    pub playing: bool,
    /// egui input time (seconds) when Play was pressed.
    pub play_origin: f64,
    /// The playhead when Play was pressed.
    pub play_from_ms: u32,
    /// The time under a ruler press / drag, before its release lands it.
    pub scrub_ms: Option<u32>,
    pub selected: Option<(LayerId, KeyProperty, usize)>,
    pub drag: Option<Drag>,
}

impl TimelineView {
    /// Where the playhead is drawn at egui time `now`.
    pub fn playhead(&self, timeline: &model::DocumentTimeline, now: f64) -> u32 {
        let duration = timeline.duration_ms.max(1);
        if self.playing {
            let elapsed = ((now - self.play_origin).max(0.0) * 1000.0) as u64;
            let t = (u64::from(self.play_from_ms) + elapsed) % u64::from(duration);
            return snap(t as u32, timeline);
        }
        self.scrub_ms.unwrap_or(timeline.current_ms).min(duration)
    }
}

fn view_key() -> egui::Id {
    egui::Id::new("raster-animation-timeline-view")
}

/// The Timeline view's state as the last frame left it.
pub fn timeline_view(ctx: &egui::Context) -> TimelineView {
    ctx.data(|d| d.get_temp(view_key())).unwrap_or_default()
}

fn store_view(ctx: &egui::Context, view: TimelineView) {
    ctx.data_mut(|d| d.insert_temp(view_key(), view));
}

/// Stable ids for the Timeline mode's controls, so a headless test can
/// click them.
pub mod ids {
    use editor_core::timeline::KeyProperty;

    pub fn mode_frames() -> egui::Id {
        egui::Id::new("raster-animation-mode-frames")
    }
    pub fn mode_timeline() -> egui::Id {
        egui::Id::new("raster-animation-mode-timeline")
    }
    pub fn fps() -> egui::Id {
        egui::Id::new("raster-animation-fps")
    }
    pub fn length() -> egui::Id {
        egui::Id::new("raster-animation-length")
    }
    pub fn key_opacity() -> egui::Id {
        egui::Id::new("raster-animation-key-opacity")
    }
    pub fn key_position() -> egui::Id {
        egui::Id::new("raster-animation-key-position")
    }
    pub fn key_delete() -> egui::Id {
        egui::Id::new("raster-animation-key-delete")
    }
    pub fn key_scale() -> egui::Id {
        egui::Id::new("raster-animation-key-scale")
    }
    pub fn key_rotation() -> egui::Id {
        egui::Id::new("raster-animation-key-rotation")
    }
    /// The interpolation button for `how` (applies to the selected key).
    pub fn interp(how: editor_core::timeline::Interpolation) -> egui::Id {
        egui::Id::new(("raster-animation-interp", how))
    }
    pub fn preview() -> egui::Id {
        egui::Id::new("raster-animation-timeline-preview")
    }
    /// The ruler: exactly the time axis (0 at its left edge, the length at
    /// its right), which the rows' bars share.
    pub fn ruler() -> egui::Id {
        egui::Id::new("raster-animation-ruler")
    }
    /// The bar of row `row` (top of the stack first).
    pub fn bar(row: usize) -> egui::Id {
        egui::Id::new(("raster-animation-bar", row))
    }
    pub fn bar_in(row: usize) -> egui::Id {
        egui::Id::new(("raster-animation-bar-in", row))
    }
    pub fn bar_out(row: usize) -> egui::Id {
        egui::Id::new(("raster-animation-bar-out", row))
    }
    /// Key `index` (time order) of `property` on row `row`.
    pub fn key(row: usize, property: KeyProperty, index: usize) -> egui::Id {
        egui::Id::new(("raster-animation-key", row, property, index))
    }
}

/// `t` snapped to the nearest frame of the timeline's frame rate.
fn snap(t: u32, timeline: &model::DocumentTimeline) -> u32 {
    let fps = u64::from(timeline.fps.max(1));
    let frame = (u64::from(t) * fps * 2 + 1000) / 2000;
    ((frame * 1000 + fps / 2) / fps).min(u64::from(timeline.duration_ms)) as u32
}

/// The time under `x` on an axis spanning `axis`.
fn time_at(x: f32, axis: Rect, timeline: &model::DocumentTimeline) -> u32 {
    let f = ((x - axis.left()) / axis.width().max(1.0)).clamp(0.0, 1.0);
    snap(
        (f * timeline.duration_ms.max(1) as f32).round() as u32,
        timeline,
    )
}

/// Where time `t` lies on an axis spanning `axis`.
fn x_at(t: u32, axis: Rect, timeline: &model::DocumentTimeline) -> f32 {
    axis.left() + axis.width() * (t as f32 / timeline.duration_ms.max(1) as f32)
}

/// The Frames / Timeline switch. Answers whether Timeline mode is on.
pub(super) fn mode_toggle(w: &mut Workspace, ui: &mut Ui, doc: &Document) -> bool {
    let on = doc.timeline.enabled;
    ui.horizontal(|ui| {
        let frames = ui.selectable_label(!on, body(ui, tr(MODE_FRAMES)));
        crate::view::mark(ui, frames.rect, ids::mode_frames());
        if frames.clicked() && on {
            if let Some(c) = model::set_mode(doc, false) {
                w.emit(Intent::Document(c));
            }
        }
        let timeline = ui.selectable_label(on, body(ui, tr(MODE_TIMELINE)));
        crate::view::mark(ui, timeline.rect, ids::mode_timeline());
        if timeline.clicked() && !on {
            if let Some(c) = model::set_mode(doc, true) {
                w.emit(Intent::Document(c));
            }
        }
    });
    ui.add_space(Space::XSmall.pt());
    on
}

/// A number field whose drag is held in view state and lands once, on the
/// release; a typed value lands on Enter / focus loss.
fn held_field(
    ui: &mut Ui,
    id: egui::Id,
    current: u32,
    range: std::ops::RangeInclusive<u32>,
    suffix: &str,
) -> Option<u32> {
    let t = current_tokens(ui);
    let pending_key = id.with("pending");
    let pending: Option<u32> = ui.data(|d| d.get_temp(pending_key));
    let mut value = pending.unwrap_or(current);
    let field = ui.add_sized(
        Vec2::new(t.metrics.numeric_field_width, t.metrics.control_height),
        egui::DragValue::new(&mut value)
            .range(range)
            .update_while_editing(false)
            .suffix(suffix),
    );
    crate::view::mark(ui, field.rect, id);
    let commit = if field.drag_stopped() {
        ui.data_mut(|d| d.remove::<u32>(pending_key));
        Some(value)
    } else if field.dragged() {
        if field.changed() {
            ui.data_mut(|d| d.insert_temp(pending_key, value));
        }
        None
    } else {
        field.changed().then_some(value)
    };
    commit.filter(|v| *v != current)
}

/// Draw Timeline mode.
pub(super) fn timeline_body(w: &mut Workspace, ui: &mut Ui, doc: &Document) {
    let t = current_tokens(ui);
    let now = ui.input(|i| i.time);
    let mut view = timeline_view(ui.ctx());
    let tl = &doc.timeline;
    let playhead = view.playhead(tl, now);
    let active = doc
        .active_layer()
        .filter(|id| doc.layers.root().contains(id));

    // Transport: Play / Stop, the playhead, frame rate and length; the key
    // buttons on the right.
    ui.horizontal(|ui| {
        let label = tr(if view.playing { STOP } else { PLAY });
        let play = ui.button(body(ui, label));
        crate::view::mark(ui, play.rect, super::ids::play());
        if play.clicked() {
            if view.playing {
                view.playing = false;
                w.emit(Intent::SeekTimeline { t_ms: playhead });
            } else {
                view.playing = true;
                view.play_origin = now;
                view.play_from_ms = playhead;
                view.scrub_ms = None;
            }
        }
        ui.label(body(ui, format!("{playhead}{}", tr(MS))));
        if let Some(fps) = held_field(ui, ids::fps(), tl.fps, model::FPS_RANGE, tr(FPS)) {
            let mut next = tl.clone();
            next.fps = fps;
            if let Some(c) = model::set_timeline(doc, tr(FPS).trim(), next) {
                w.emit(Intent::Document(c));
            }
        }
        ui.label(body(ui, tr(LENGTH)));
        if let Some(len) = held_field(
            ui,
            ids::length(),
            tl.duration_ms,
            model::DURATION_RANGE_MS,
            tr(MS),
        ) {
            let mut next = tl.clone();
            next.duration_ms = len;
            if let Some(c) = model::set_timeline(doc, tr(LENGTH), next) {
                w.emit(Intent::Document(c));
            }
        }
    });
    // W13X-9: four key buttons wrap rather than widen the panel.
    ui.horizontal_wrapped(|ui| {
        let can_key = active.is_some() && !view.playing;
        let opacity = ui.add_enabled(can_key, egui::Button::new(body(ui, tr(KEY_OPACITY))));
        crate::view::mark(ui, opacity.rect, ids::key_opacity());
        let position = ui.add_enabled(can_key, egui::Button::new(body(ui, tr(KEY_POSITION))));
        crate::view::mark(ui, position.rect, ids::key_position());
        let scale = ui.add_enabled(can_key, egui::Button::new(body(ui, tr(KEY_SCALE))));
        crate::view::mark(ui, scale.rect, ids::key_scale());
        let rotation = ui.add_enabled(can_key, egui::Button::new(body(ui, tr(KEY_ROTATION))));
        crate::view::mark(ui, rotation.rect, ids::key_rotation());
        for (response, property) in [
            (opacity, KeyProperty::Opacity),
            (position, KeyProperty::Position),
            (scale, KeyProperty::Scale),
            (rotation, KeyProperty::Rotation),
        ] {
            if response.clicked() {
                if let Some(c) = active.and_then(|id| model::add_key(doc, id, property, playhead)) {
                    w.emit(Intent::Document(c));
                }
            }
        }
    });
    // W13X-9: the selected key's interpolation, and the trash button. Always drawn (disabled with
    // no key selected), so the rows below never jump when a key is picked.
    let selected_interp = view.selected.and_then(|(layer, property, index)| {
        tl.track(layer)
            .and_then(|tr| tr.interpolation(property, index))
            .map(|how| (layer, property, index, how))
    });
    ui.horizontal(|ui| {
        for how in model::Interpolation::ALL {
            let on = selected_interp.is_some_and(|s| s.3 == how);
            let r = ui.add_enabled(
                selected_interp.is_some() && !view.playing,
                egui::SelectableLabel::new(on, body(ui, tr(interp_label(how)))),
            );
            crate::view::mark(ui, r.rect, ids::interp(how));
            if r.clicked() {
                if let Some((layer, property, index, _)) = selected_interp {
                    if let Some(c) = model::set_interpolation(doc, layer, property, index, how) {
                        w.emit(Intent::Document(c));
                    }
                }
            }
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let state = if view.selected.is_some() {
                ActionState::Idle
            } else {
                ActionState::Disabled
            };
            if icon_action_id(ui, "trash", tr(KEY_DELETE), state, Some(ids::key_delete())).clicked()
            {
                if let Some((layer, property, index)) = view.selected.take() {
                    if let Some(c) = model::delete_key(doc, layer, property, index) {
                        w.emit(Intent::Document(c));
                    }
                }
            }
        });
    });
    ui.add_space(Space::XSmall.pt());

    preview(w, ui, doc, playhead);
    ui.add_space(Space::XSmall.pt());

    let rows: Vec<LayerId> = doc.layers.root().to_vec();
    if rows.is_empty() {
        ui.label(hint(ui, tr(NO_LAYERS)));
        store_view(ui.ctx(), view);
        return;
    }

    // The ruler: the time axis, right of the name column.
    let label_w = t.metrics.inspector_label_width;
    let row_h = t.metrics.list_row_height;
    let (strip, _) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), t.metrics.control_height),
        Sense::hover(),
    );
    let axis = Rect::from_min_max(Pos2::new(strip.left() + label_w, strip.top()), strip.max);
    let ruler = ui.interact(axis, ids::ruler(), Sense::click_and_drag());
    if ui.is_rect_visible(axis) {
        ui.painter().rect_filled(
            axis,
            rounding(Radius::Small.resolve(&t.radii, axis.height())),
            color32(t.palette.color(ColorRole::SurfaceSunken)),
        );
        let tick = egui::Stroke::new(
            t.borders.hairline,
            color32(t.palette.color(ColorRole::SeparatorStrong)),
        );
        let mut s = 0u32;
        while s <= tl.duration_ms {
            let x = x_at(s, axis, tl);
            ui.painter().line_segment(
                [Pos2::new(x, axis.center().y), Pos2::new(x, axis.bottom())],
                tick,
            );
            s += 1000;
        }
    }
    // A scrub seeks the canvas on every frame the time under the pointer
    // differs from the document's playhead: live, and never a history step.
    if ruler.is_pointer_button_down_on() || ruler.dragged() {
        if let Some(p) = ruler.interact_pointer_pos() {
            let at = time_at(p.x, axis, tl);
            view.scrub_ms = Some(at);
            view.playing = false;
            if at != tl.current_ms {
                w.emit(Intent::SeekTimeline { t_ms: at });
            }
        }
    }
    if ruler.drag_stopped() || ruler.clicked() {
        let at = ruler
            .interact_pointer_pos()
            .map(|p| time_at(p.x, axis, tl))
            .or(view.scrub_ms);
        if let Some(at) = at.filter(|at| *at != tl.current_ms) {
            w.emit(Intent::SeekTimeline { t_ms: at });
        }
        view.scrub_ms = None;
    }

    // One row per top-level layer, top of the stack first.
    let handle_w = Space::Small.pt();
    let key_r = Space::XSmall.pt();
    let mut rows_bottom = strip.bottom();
    for (row, id) in rows.iter().enumerate() {
        let Some(layer) = doc.layers.get(*id) else {
            continue;
        };
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), row_h), Sense::hover());
        rows_bottom = rect.bottom();
        let name_rect = Rect::from_min_max(rect.min, Pos2::new(axis.left(), rect.bottom()));
        ui.put(
            name_rect,
            egui::Label::new(body(ui, layer.name.clone())).truncate(),
        );
        let lane = Rect::from_min_max(
            Pos2::new(axis.left(), rect.top() + Space::Hair.pt()),
            Pos2::new(axis.right(), rect.bottom() - Space::Hair.pt()),
        );
        let track = tl.track(*id);
        let (mut in_ms, mut out_ms) = track.map_or((0, tl.duration_ms), |tr| (tr.in_ms, tr.out_ms));
        match view.drag {
            Some(Drag::In(l, at)) if l == *id => in_ms = at.min(out_ms),
            Some(Drag::Out(l, at)) if l == *id => out_ms = at.max(in_ms),
            _ => {}
        }
        let bar = Rect::from_min_max(
            Pos2::new(x_at(in_ms, lane, tl), lane.top()),
            Pos2::new(x_at(out_ms, lane, tl), lane.bottom()),
        );
        let bar_response = ui.interact(bar, ids::bar(row), Sense::click());
        if bar_response.clicked() {
            w.emit(Intent::SelectLayers {
                layers: vec![*id],
                active: Some(*id),
            });
        }
        if ui.is_rect_visible(lane) {
            let radius = rounding(Radius::Small.resolve(&t.radii, lane.height()));
            ui.painter().rect_filled(
                bar,
                radius,
                color32(t.palette.color(ColorRole::AccentSubtle)),
            );
            let (width, role) = if active == Some(*id) {
                (t.borders.thick, ColorRole::Accent)
            } else {
                (t.borders.hairline, ColorRole::ControlStroke)
            };
            ui.painter().rect_stroke(
                bar,
                radius,
                egui::Stroke::new(width, color32(t.palette.color(role))),
            );
        }

        // The end handles: drag to move the in / out point.
        for (edge_ms, handle_id, is_in) in [
            (in_ms, ids::bar_in(row), true),
            (out_ms, ids::bar_out(row), false),
        ] {
            let x = x_at(edge_ms, lane, tl);
            let handle = Rect::from_center_size(
                Pos2::new(x, lane.center().y),
                Vec2::new(handle_w, lane.height()),
            );
            let r = ui.interact(handle, handle_id, Sense::drag());
            if r.dragged() {
                if let Some(p) = r.interact_pointer_pos() {
                    let at = time_at(p.x, lane, tl);
                    view.drag = Some(if is_in {
                        Drag::In(*id, at)
                    } else {
                        Drag::Out(*id, at)
                    });
                }
            }
            if r.drag_stopped() {
                if let Some(c) = model::set_in_out(doc, *id, in_ms, out_ms) {
                    w.emit(Intent::Document(c));
                }
                view.drag = None;
            }
            if ui.is_rect_visible(handle) {
                ui.painter().rect_filled(
                    handle.shrink2(Vec2::new(0.0, Space::XSmall.pt())),
                    rounding(Radius::Small.resolve(&t.radii, handle.width())),
                    color32(t.palette.color(ColorRole::ControlStrokeStrong)),
                );
            }
        }

        // The keys, in four lanes top to bottom: opacity, position, scale,
        // rotation (W13X-9). Each hit box is no taller than its lane, so
        // the lanes' keys never cover one another.
        let Some(track) = track else {
            continue;
        };
        let lanes = KeyProperty::ALL.len() as f32;
        let lane_step = lane.height() / lanes;
        for (slot, property) in KeyProperty::ALL.into_iter().enumerate() {
            let y = lane.top() + lane_step * (slot as f32 + 0.5);
            for (index, key_ms) in track.key_times(property).into_iter().enumerate() {
                let shown_ms = match view.drag {
                    Some(Drag::Key(l, p, i, at)) if l == *id && p == property && i == index => at,
                    _ => key_ms,
                };
                let centre = Pos2::new(x_at(shown_ms, lane, tl), y);
                let hit = Rect::from_center_size(
                    centre,
                    Vec2::new(key_r * 3.0, (key_r * 3.0).min(lane_step)),
                );
                let r = ui.interact(hit, ids::key(row, property, index), Sense::click_and_drag());
                if r.clicked() || r.drag_started() {
                    view.selected = Some((*id, property, index));
                }
                if r.dragged() {
                    if let Some(p) = r.interact_pointer_pos() {
                        view.drag = Some(Drag::Key(*id, property, index, time_at(p.x, lane, tl)));
                    }
                }
                if r.drag_stopped() {
                    if shown_ms != key_ms {
                        if let Some(c) = model::move_key(doc, *id, property, index, shown_ms) {
                            w.emit(Intent::Document(c));
                        }
                    }
                    view.drag = None;
                    view.selected = None;
                }
                if ui.is_rect_visible(hit) {
                    let selected = view.selected == Some((*id, property, index));
                    let fill = if selected {
                        ColorRole::SelectionStroke
                    } else {
                        match property {
                            KeyProperty::Opacity => ColorRole::Accent,
                            KeyProperty::Position => ColorRole::TextPrimary,
                            KeyProperty::Scale => ColorRole::Success,
                            KeyProperty::Rotation => ColorRole::Warning,
                        }
                    };
                    // Half a lane at most, so neighbouring lanes' keys do
                    // not overlap; a Hold key is a square.
                    let kr = key_r.min(lane_step * 0.5);
                    let hold =
                        track.interpolation(property, index) == Some(model::Interpolation::Hold);
                    let shape = if hold {
                        vec![
                            centre + Vec2::new(-kr, -kr),
                            centre + Vec2::new(kr, -kr),
                            centre + Vec2::new(kr, kr),
                            centre + Vec2::new(-kr, kr),
                        ]
                    } else {
                        vec![
                            centre + Vec2::new(0.0, -kr),
                            centre + Vec2::new(kr, 0.0),
                            centre + Vec2::new(0.0, kr),
                            centre + Vec2::new(-kr, 0.0),
                        ]
                    };
                    ui.painter().add(egui::Shape::convex_polygon(
                        shape,
                        color32(t.palette.color(fill)),
                        egui::Stroke::new(
                            t.borders.hairline,
                            color32(t.palette.color(ColorRole::SurfacePanel)),
                        ),
                    ));
                }
            }
        }
    }

    // The playhead line, over the ruler and every row.
    let x = x_at(playhead, axis, tl);
    ui.painter().line_segment(
        [Pos2::new(x, axis.top()), Pos2::new(x, rows_bottom)],
        egui::Stroke::new(
            t.borders.thick,
            color32(t.palette.color(ColorRole::SelectionStroke)),
        ),
    );

    if view.playing {
        // Playback scrubs the canvas: each frame the playhead has moved on,
        // the document is sought to it (no history step).
        if playhead != tl.current_ms {
            w.emit(Intent::SeekTimeline { t_ms: playhead });
        }
        let step = 1.0 / f64::from(tl.fps.max(1));
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs_f64(step));
    }
    store_view(ui.ctx(), view);
}

/// The preview well at `playhead`: each visible top-level layer's thumbnail,
/// bottom of the stack first, at its opacity and transform at that time.
/// The thumbnail shows the layer as it stands now (canvas-sized), so it is
/// drawn as a quad moved by the change from the layer's current transform
/// to its transform at `playhead` (position, scale and rotation about the
/// layer centre, [`model::transform_at`]).
fn preview(w: &Workspace, ui: &mut Ui, doc: &Document, playhead: u32) {
    let t = current_tokens(ui);
    let height = t.metrics.control_height * 5.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::hover());
    crate::view::mark(ui, rect, ids::preview());
    if !ui.is_rect_visible(rect) {
        return;
    }
    let tl = &doc.timeline;
    crate::view::checkerboard(ui.painter(), rect, Space::XSmall.pt());
    let fit = super::fitted(rect, doc.width(), doc.height());
    let scale = fit.width() / doc.width().max(1) as f32;
    let full = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
    let painter = ui.painter().with_clip_rect(rect);
    for id in doc.layers.root().iter().rev() {
        let Some(layer) = doc.layers.get(*id) else {
            continue;
        };
        let track = tl.track(*id);
        let visible = track.map_or(layer.visible, |tr| tr.shows_at(playhead));
        if !visible {
            continue;
        }
        let opacity = track
            .and_then(|tr| tr.opacity_at(playhead))
            .unwrap_or(layer.opacity);
        let change = model::transform_at(doc, *id, playhead)
            .filter(|_| layer.transform.matrix2.determinant().abs() > f32::EPSILON)
            .map_or(glam::Affine2::IDENTITY, |at| at * layer.transform.inverse());
        if let Some(tex) = w.layer_thumbs.get(id) {
            let tint = crate::dialogs::controls::UNTINTED.gamma_multiply(opacity);
            let (cw, ch) = (doc.width() as f32, doc.height() as f32);
            let mut mesh = egui::Mesh::with_texture(tex.id());
            for (corner, uv) in [
                (glam::Vec2::ZERO, full.left_top()),
                (glam::Vec2::new(cw, 0.0), full.right_top()),
                (glam::Vec2::new(cw, ch), full.right_bottom()),
                (glam::Vec2::new(0.0, ch), full.left_bottom()),
            ] {
                let p = change.transform_point2(corner);
                mesh.vertices.push(egui::epaint::Vertex {
                    pos: fit.min + Vec2::new(p.x, p.y) * scale,
                    uv,
                    color: tint,
                });
            }
            mesh.add_triangle(0, 1, 2);
            mesh.add_triangle(0, 2, 3);
            painter.add(egui::Shape::mesh(mesh));
        }
    }
    ui.painter().rect_stroke(
        rect,
        rounding(Radius::Small.resolve(&t.radii, height)),
        egui::Stroke::new(
            t.borders.hairline,
            color32(t.palette.color(ColorRole::ControlStroke)),
        ),
    );
}

#[cfg(test)]
mod tests {
    //! Timeline mode driven the way a user drives it: the whole
    //! [`Workspace`] draws its dock with only the Animation panel open (the
    //! panel's real route), clicks and drags land by the controls' ids, and
    //! the emitted commands are applied through the history as the shell
    //! applies them.

    use super::*;
    use crate::dock::{LayoutId, PanelId};
    use editor_core::History;
    use layer_model::Layer;

    struct Live {
        ctx: egui::Context,
        w: Workspace,
        doc: Document,
        history: History,
        time: f64,
    }

    fn button(pos: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
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
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1400.0, 900.0))),
                time: Some(self.time),
                events,
                ..Default::default()
            };
            let (w, doc, history) = (&mut self.w, &self.doc, &self.history);
            let out = self.ctx.run(input, |ctx| w.ui(ctx, doc, history));
            (self.w.drain_intents(), out)
        }

        fn rect(&self, id: egui::Id) -> Rect {
            self.ctx
                .read_response(id)
                .unwrap_or_else(|| panic!("{id:?} was not drawn"))
                .rect
        }

        fn click_at(&mut self, at: Pos2) -> Vec<Intent> {
            let (mut intents, _) =
                self.frame(vec![egui::Event::PointerMoved(at), button(at, true)]);
            intents.extend(self.frame(vec![button(at, false)]).0);
            intents
        }

        fn click(&mut self, id: egui::Id) -> Vec<Intent> {
            let at = self.rect(id).center();
            self.click_at(at)
        }

        /// Press at `from`, move to `to` in steps (one frame each), release.
        /// Answers every intent, and how many document commands came before
        /// the release.
        fn drag(&mut self, from: Pos2, to: Pos2) -> (Vec<Intent>, usize) {
            let mut intents = self
                .frame(vec![egui::Event::PointerMoved(from), button(from, true)])
                .0;
            for step in 1..=6 {
                let at = from + (to - from) * (step as f32 / 6.0);
                intents.extend(self.frame(vec![egui::Event::PointerMoved(at)]).0);
            }
            let before = documents(&intents);
            intents.extend(self.frame(vec![button(to, false)]).0);
            (intents, before)
        }

        fn apply(&mut self, intents: &[Intent]) -> usize {
            let mut applied = 0;
            for intent in intents {
                match intent {
                    Intent::Document(c) => {
                        self.history
                            .apply(&mut self.doc, c.clone())
                            .expect("the command applies");
                        applied += 1;
                    }
                    Intent::SelectLayers {
                        active: Some(id), ..
                    } => {
                        let _ = self.doc.set_active_layer(Some(*id));
                    }
                    // What the shell does with a seek: no history.
                    Intent::SeekTimeline { t_ms } => {
                        model::seek(&mut self.doc, *t_ms);
                    }
                    _ => {}
                }
            }
            self.thumbs();
            for _ in 0..2 {
                self.frame(Vec::new());
            }
            applied
        }

        /// The textures painted inside the preview well, with their tints.
        fn preview_images(&mut self) -> Vec<(egui::TextureId, egui::Color32)> {
            let (_, out) = self.frame(Vec::new());
            let well = self.rect(ids::preview());
            let mut found = Vec::new();
            for clipped in &out.shapes {
                collect_images(&clipped.shape, well, &mut found);
            }
            found
        }
    }

    fn documents(intents: &[Intent]) -> usize {
        intents
            .iter()
            .filter(|i| matches!(i, Intent::Document(_)))
            .count()
    }

    fn seeks(intents: &[Intent]) -> Vec<u32> {
        intents
            .iter()
            .filter_map(|i| match i {
                Intent::SeekTimeline { t_ms } => Some(*t_ms),
                _ => None,
            })
            .collect()
    }

    fn collect_images(
        shape: &egui::Shape,
        well: Rect,
        out: &mut Vec<(egui::TextureId, egui::Color32)>,
    ) {
        match shape {
            egui::Shape::Vec(shapes) => {
                for s in shapes {
                    collect_images(s, well, out);
                }
            }
            egui::Shape::Mesh(mesh)
                if mesh.texture_id != egui::TextureId::default()
                    && !mesh.vertices.is_empty()
                    && mesh
                        .vertices
                        .iter()
                        .all(|v| well.expand(1.0).contains(v.pos)) =>
            {
                out.push((mesh.texture_id, mesh.vertices[0].color));
            }
            _ => {}
        }
    }

    /// Two layers ("Back" at the bottom, "Clip" on top); Clip active.
    fn two_layers() -> (Document, LayerId, LayerId) {
        let mut doc = Document::new(64, 48, "video");
        let back = doc.layers.push_root(Layer::raster("Back")).unwrap();
        let clip = doc.layers.push_root(Layer::raster("Clip")).unwrap();
        doc.set_active_layer(Some(clip)).unwrap();
        (doc, back, clip)
    }

    /// Timeline mode on and Clip fading in: opacity 0 at 0 ms, 1 at 3000 ms.
    fn timeline_doc() -> (Document, LayerId, LayerId) {
        let (mut doc, back, clip) = two_layers();
        model::set_mode(&doc, true)
            .unwrap()
            .apply(&mut doc)
            .unwrap();
        model::add_key(&doc, clip, KeyProperty::Opacity, 3000)
            .unwrap()
            .apply(&mut doc)
            .unwrap();
        doc.layers.get_mut(clip).unwrap().opacity = 0.0;
        model::add_key(&doc, clip, KeyProperty::Opacity, 0)
            .unwrap()
            .apply(&mut doc)
            .unwrap();
        (doc, back, clip)
    }

    #[test]
    fn the_mode_switch_turns_on_the_timeline_with_a_bar_per_layer_and_its_keys() {
        let (doc, _, clip) = two_layers();
        let mut live = Live::new(doc);
        let intents = live.click(ids::mode_timeline());
        assert_eq!(live.apply(&intents), 1, "{intents:?}");
        assert!(live.doc.timeline.enabled, "Timeline mode is document state");
        assert_eq!(live.history.undo_depth(), 1, "one undo step");

        // One bar per top-level layer, top of the stack first, each
        // spanning the whole axis while it has no track.
        let axis = live.rect(ids::ruler());
        let bars = [live.rect(ids::bar(0)), live.rect(ids::bar(1))];
        assert!(bars[0].bottom() <= bars[1].top(), "rows stack: {bars:?}");
        for bar in bars {
            assert!(
                (bar.left() - axis.left()).abs() < 1.0,
                "{bar:?} vs {axis:?}"
            );
            assert!(
                (bar.right() - axis.right()).abs() < 1.0,
                "{bar:?} vs {axis:?}"
            );
        }

        // A key at 1000 ms is drawn a third of the way along Clip's row.
        let c = model::add_key(&live.doc, clip, KeyProperty::Opacity, 1000).unwrap();
        live.apply(&[Intent::Document(c)]);
        let key = live.rect(ids::key(0, KeyProperty::Opacity, 0));
        let want = axis.left() + axis.width() / 3.0;
        assert!(
            (key.center().x - want).abs() < 1.0,
            "{key:?}, want x {want}"
        );

        // The words are the catalogue's.
        let (_, out) = live.frame(Vec::new());
        let texts: Vec<String> = out
            .shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Text(t) => Some(t.galley.text().to_string()),
                _ => None,
            })
            .collect();
        for key in [
            MODE_FRAMES,
            MODE_TIMELINE,
            KEY_OPACITY,
            KEY_POSITION,
            LENGTH,
        ] {
            let word = tr(key);
            assert!(!word.is_empty(), "{key} has no catalogue row");
            assert!(
                texts.iter().any(|t| t == word),
                "{word:?} not drawn: {texts:?}"
            );
        }

        // And back to Frames.
        let intents = live.click(ids::mode_frames());
        live.apply(&intents);
        assert!(!live.doc.timeline.enabled);
    }

    /// Clicking the ruler halfway moves the playhead there and puts the
    /// layer values at that time on the layers (what the canvas draws), with
    /// no history step and no dirty flag; a drag seeks on every frame, so the
    /// layers follow the pointer before the release.
    #[test]
    fn scrubbing_the_ruler_seeks_the_canvas_layers_live_with_no_history() {
        let (mut doc, _, clip) = timeline_doc();
        doc.mark_saved();
        let mut live = Live::new(doc);
        let axis = live.rect(ids::ruler());
        let intents = live.click_at(axis.center());
        assert_eq!(live.apply(&intents), 0, "no document command: {intents:?}");
        assert_eq!(seeks(&intents).last(), Some(&1500), "{intents:?}");
        assert_eq!(live.doc.timeline.current_ms, 1500);
        let opacity = live.doc.layers.get(clip).unwrap().opacity;
        assert!((opacity - 0.5).abs() < 1e-3, "{opacity}");
        assert_eq!(live.history.undo_depth(), 0, "a scrub is no undo step");
        assert!(!live.doc.is_dirty(), "a scrub does not dirty the document");

        // Drag from a quarter to three quarters, applying what each frame
        // raised as the shell does: the layer follows the pointer mid-drag.
        let y = axis.center().y;
        let from = Pos2::new(axis.left() + axis.width() * 0.25, y);
        let to = Pos2::new(axis.left() + axis.width() * 0.75, y);
        let pressed = live
            .frame(vec![egui::Event::PointerMoved(from), button(from, true)])
            .0;
        live.apply(&pressed);
        let mut mid = Vec::new();
        for step in 1..=6 {
            let at = from + (to - from) * (step as f32 / 6.0);
            let raised = live.frame(vec![egui::Event::PointerMoved(at)]).0;
            assert_eq!(documents(&raised), 0, "{raised:?}");
            live.apply(&raised);
            mid.push(live.doc.layers.get(clip).unwrap().opacity);
        }
        assert!(
            mid.windows(2).all(|w| w[1] >= w[0]) && mid[0] < mid[5] - 0.3,
            "the layer fades in as the pointer moves: {mid:?}"
        );
        let released = live.frame(vec![button(to, false)]).0;
        assert_eq!(documents(&released), 0);
        live.apply(&released);
        // 2250 ms snapped to the nearest 30 fps frame (frame 68).
        assert_eq!(live.doc.timeline.current_ms, 2267);
        let opacity = live.doc.layers.get(clip).unwrap().opacity;
        assert!((opacity - 2267.0 / 3000.0).abs() < 1e-3, "{opacity}");
        assert_eq!(live.history.undo_depth(), 0);
        assert!(!live.doc.is_dirty());
    }

    /// Opacity key adds a key at the playhead on the active layer; dragging
    /// the key moves it (one command, on the release); selecting it and
    /// pressing the trash button deletes it.
    #[test]
    fn keyframes_are_added_moved_and_deleted_from_the_panel() {
        let (mut doc, _, clip) = two_layers();
        model::set_mode(&doc, true)
            .unwrap()
            .apply(&mut doc)
            .unwrap();
        let mut live = Live::new(doc);
        let intents = live.click(ids::key_position());
        assert_eq!(live.apply(&intents), 1, "{intents:?}");
        let track = live.doc.timeline.track(clip).unwrap().clone();
        assert_eq!(track.key_times(KeyProperty::Position), vec![0]);

        let axis = live.rect(ids::ruler());
        let key = live.rect(ids::key(0, KeyProperty::Position, 0)).center();
        let to = Pos2::new(axis.left() + axis.width() * 0.5, key.y);
        let (intents, before) = live.drag(key, to);
        assert_eq!(before, 0, "nothing lands mid-drag");
        assert_eq!(live.apply(&intents), 1, "{intents:?}");
        assert_eq!(
            live.doc
                .timeline
                .track(clip)
                .unwrap()
                .key_times(KeyProperty::Position),
            vec![1500]
        );

        let intents = live.click(ids::key(0, KeyProperty::Position, 0));
        assert_eq!(live.apply(&intents), 0, "a click selects");
        assert!(timeline_view(&live.ctx).selected.is_some());
        let intents = live.click(ids::key_delete());
        assert_eq!(live.apply(&intents), 1, "{intents:?}");
        assert!(live.doc.timeline.track(clip).unwrap().position.is_empty());
        assert_eq!(live.history.undo_depth(), 3, "add, move, delete");
    }

    /// `linear` turned about the 64 x 48 canvas centre, as a Free
    /// Transform about the centre leaves the layer.
    fn about_centre(linear: glam::Mat2) -> glam::Affine2 {
        let c = glam::Vec2::new(32.0, 24.0);
        glam::Affine2::from_mat2_translation(linear, c - linear * c)
    }

    /// Hold the ruler down at `at` and read the clip thumbnail's quad (its
    /// four corners) from the preview well the frame after the press, while
    /// the seek it raised has not landed: the well draws the playhead's
    /// transform relative to the layer as it stands. Answers the quad and
    /// every intent raised through the release.
    fn held_quad(live: &mut Live, clip: LayerId, at: Pos2) -> (Vec<Pos2>, Vec<Intent>) {
        let tex = live.w.layer_thumbs[&clip].id();
        let mut intents = live
            .frame(vec![egui::Event::PointerMoved(at), button(at, true)])
            .0;
        let (raised, out) = live.frame(Vec::new());
        intents.extend(raised);
        fn find(shape: &egui::Shape, tex: egui::TextureId) -> Option<Vec<Pos2>> {
            match shape {
                egui::Shape::Vec(shapes) => shapes.iter().find_map(|s| find(s, tex)),
                egui::Shape::Mesh(m) if m.texture_id == tex => {
                    Some(m.vertices.iter().map(|v| v.pos).collect())
                }
                _ => None,
            }
        }
        let quad = out
            .shapes
            .iter()
            .find_map(|c| find(&c.shape, tex))
            .expect("the clip is painted in the well");
        intents.extend(live.frame(vec![button(at, false)]).0);
        (quad, intents)
    }

    /// W13X-9: Rotation key from the panel, keyed at 0 and 3000 ms (0 and
    /// 90 degrees); the canvas layer and the preview's quad turn about the
    /// centre as the playhead moves; picking the first key and pressing
    /// Ease In / Hold (one command each) changes the value at the playhead,
    /// and a Hold key is drawn square.
    #[test]
    fn rotation_keys_and_interpolation_from_the_panel_turn_the_layer_and_its_preview() {
        let (mut doc, _, clip) = two_layers();
        model::set_mode(&doc, true)
            .unwrap()
            .apply(&mut doc)
            .unwrap();
        let mut live = Live::new(doc);
        let intents = live.click(ids::key_rotation());
        assert_eq!(live.apply(&intents), 1, "{intents:?}");
        let axis = live.rect(ids::ruler());
        let y = axis.center().y;
        let intents = live.click_at(Pos2::new(axis.right() - 0.5, y));
        live.apply(&intents);
        assert_eq!(live.doc.timeline.current_ms, 3000);
        live.doc.layers.get_mut(clip).unwrap().transform =
            about_centre(glam::Mat2::from_angle(90f32.to_radians()));
        let intents = live.click(ids::key_rotation());
        assert_eq!(live.apply(&intents), 1, "{intents:?}");
        let track = live.doc.timeline.track(clip).unwrap();
        assert_eq!(track.key_times(KeyProperty::Rotation), vec![0, 3000]);
        assert!((track.rotation[1].degrees - 90.0).abs() < 1e-3);

        // Back to 0 ms (the layer unturned), then hold the ruler halfway:
        // the preview's quad turns 45 degrees before the seek lands, and
        // once it lands the canvas layer is at 45 degrees about the centre.
        let start = Pos2::new(axis.left() + 0.5, y);
        let intents = live.click_at(start);
        live.apply(&intents);
        let angle = |live: &Live| {
            let m = live.doc.layers.get(clip).unwrap().transform;
            let centre = glam::Vec2::new(32.0, 24.0);
            assert!((m.transform_point2(centre) - centre).length() < 1e-3);
            m.to_scale_angle_translation().1.to_degrees()
        };
        assert!(angle(&live).abs() < 1e-3, "{}", angle(&live));
        let (quad, intents) = held_quad(&mut live, clip, axis.center());
        let edge = quad[1] - quad[0];
        assert!(
            (edge.angle().to_degrees() - 45.0).abs() < 0.5,
            "the top edge turned 45 degrees: {quad:?}"
        );
        live.apply(&intents);
        assert_eq!(live.doc.timeline.current_ms, 1500);
        assert!((angle(&live) - 45.0).abs() < 1e-3, "{}", angle(&live));

        // Pick the first key; Ease In: a quarter of the way at halfway.
        let intents = live.click(ids::key(0, KeyProperty::Rotation, 0));
        assert_eq!(live.apply(&intents), 0, "a click selects");
        let intents = live.click(ids::interp(model::Interpolation::EaseIn));
        assert_eq!(live.apply(&intents), 1, "{intents:?}");
        let track = live.doc.timeline.track(clip).unwrap();
        assert_eq!(
            track.interpolation(KeyProperty::Rotation, 0),
            Some(model::Interpolation::EaseIn)
        );
        assert!((angle(&live) - 22.5).abs() < 1e-3, "{}", angle(&live));
        let intents = live.click_at(start);
        live.apply(&intents);
        let (quad, intents) = held_quad(&mut live, clip, axis.center());
        let edge = quad[1] - quad[0];
        assert!((edge.angle().to_degrees() - 22.5).abs() < 0.5, "{quad:?}");
        live.apply(&intents);

        // Hold: the first key's value until the next key; drawn square.
        let intents = live.click(ids::interp(model::Interpolation::Hold));
        assert_eq!(live.apply(&intents), 1, "{intents:?}");
        assert!(angle(&live).abs() < 1e-3, "{}", angle(&live));
        let key = live.rect(ids::key(0, KeyProperty::Rotation, 0));
        let (_, out) = live.frame(Vec::new());
        let square = out.shapes.iter().any(|c| match &c.shape {
            egui::Shape::Path(p) if p.points.len() == 4 && p.closed => {
                let mid = p.points.iter().fold(Vec2::ZERO, |a, q| a + q.to_vec2()) / 4.0;
                key.contains(mid.to_pos2())
                    && (p.points[0].y - p.points[1].y).abs() < 1e-3
                    && (p.points[0].x - p.points[1].x).abs() > 1.0
            }
            _ => false,
        });
        assert!(square, "the Hold key is drawn as a square");
        assert_eq!(live.history.undo_depth(), 4, "two keys, two interpolations");

        // The words are the catalogue's.
        let texts: Vec<String> = out
            .shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Text(t) => Some(t.galley.text().to_string()),
                _ => None,
            })
            .collect();
        for key in [
            KEY_SCALE,
            KEY_ROTATION,
            INTERP_LINEAR,
            INTERP_EASE_IN,
            INTERP_EASE_OUT,
            INTERP_HOLD,
        ] {
            let word = tr(key);
            assert!(
                !word.is_empty() && word != key,
                "{key} has no catalogue row"
            );
            assert!(
                texts.iter().any(|t| t == word),
                "{word:?} not drawn: {texts:?}"
            );
        }
    }

    /// W13X-9: Scale key from the panel holds the layer's scale; a second
    /// key at half size shrinks the canvas layer about its centre.
    #[test]
    fn scale_keys_from_the_panel_shrink_the_layer_about_its_centre() {
        let (mut doc, _, clip) = two_layers();
        model::set_mode(&doc, true)
            .unwrap()
            .apply(&mut doc)
            .unwrap();
        let mut live = Live::new(doc);
        let intents = live.click(ids::key_scale());
        assert_eq!(live.apply(&intents), 1, "{intents:?}");
        let axis = live.rect(ids::ruler());
        let intents = live.click_at(Pos2::new(axis.right() - 0.5, axis.center().y));
        live.apply(&intents);
        live.doc.layers.get_mut(clip).unwrap().transform =
            about_centre(glam::Mat2::from_diagonal(glam::Vec2::splat(0.5)));
        let intents = live.click(ids::key_scale());
        assert_eq!(live.apply(&intents), 1, "{intents:?}");
        let intents = live.click_at(Pos2::new(axis.left() + 0.5, axis.center().y));
        live.apply(&intents);
        let (quad, intents) = held_quad(&mut live, clip, axis.center());
        let well = live.rect(ids::preview());
        let fit = super::super::fitted(well, 64, 48);
        assert!(
            ((quad[1].x - quad[0].x) - fit.width() * 0.75).abs() < 0.5,
            "the preview quad is three quarters wide: {quad:?} vs {fit:?}"
        );
        live.apply(&intents);
        let m = live.doc.layers.get(clip).unwrap().transform;
        let centre = glam::Vec2::new(32.0, 24.0);
        assert!((m.transform_point2(centre) - centre).length() < 1e-3);
        assert!((m.matrix2.x_axis.x - 0.75).abs() < 1e-4, "{m:?}");
    }

    /// Dragging a bar's out handle sets the layer's out point, and past it
    /// the layer is hidden.
    #[test]
    fn dragging_a_bars_out_handle_sets_the_layers_out_point() {
        let (doc, back, _) = timeline_doc();
        let mut live = Live::new(doc);
        let axis = live.rect(ids::ruler());
        let handle = live.rect(ids::bar_out(1)).center();
        let to = Pos2::new(axis.left() + axis.width() / 3.0, handle.y);
        let (intents, before) = live.drag(handle, to);
        assert_eq!(before, 0);
        assert_eq!(live.apply(&intents), 1, "{intents:?}");
        let track = live.doc.timeline.track(back).unwrap();
        assert_eq!((track.in_ms, track.out_ms), (0, 1000));
        let bar = live.rect(ids::bar(1));
        assert!((bar.right() - to.x).abs() < 1.0, "{bar:?}");
        live.apply(&[Intent::SeekTimeline { t_ms: 2000 }]);
        assert!(!live.doc.layers.get(back).unwrap().visible);
    }

    /// Play advances the playhead and seeks the canvas layers to each frame
    /// (the preview well paints the same opacity); Stop seeks to the frame
    /// it stopped on. None of it is a history step.
    #[test]
    fn playback_seeks_the_canvas_layers_each_frame_and_stop_leaves_no_history() {
        let (doc, _, clip) = timeline_doc();
        let mut live = Live::new(doc);
        let intents = live.click(super::super::ids::play());
        assert_eq!(documents(&intents), 0, "playback is view state");
        assert!(timeline_view(&live.ctx).playing);
        let started = timeline_view(&live.ctx).play_origin;
        let clip_tex = live.w.layer_thumbs[&clip].id();
        let mut seen = Vec::new();
        for (i, secs) in [0.5, 1.0, 1.5].into_iter().enumerate() {
            live.time = started + secs;
            let raised = live.frame(Vec::new()).0;
            assert_eq!(documents(&raised), 0, "{raised:?}");
            assert!(!seeks(&raised).is_empty(), "frame {i} seeks: {raised:?}");
            live.apply(&raised);
            let at = live.doc.timeline.current_ms;
            let opacity = live.doc.layers.get(clip).unwrap().opacity;
            assert!(
                (opacity - at as f32 / 3000.0).abs() < 1e-3,
                "{at}: {opacity}"
            );
            seen.push(at);
        }
        assert!(seen.windows(2).all(|w| w[1] > w[0]), "{seen:?}");
        assert!((1450..=1600).contains(&seen[2]), "{seen:?}");
        // The preview well paints the clip at the same time's opacity.
        let images = live.preview_images();
        let tint = images
            .into_iter()
            .find(|i| i.0 == clip_tex)
            .expect("the clip is painted")
            .1;
        let alpha = f32::from(tint.a()) / 255.0;
        assert!((alpha - 0.5).abs() < 0.03, "half faded at 1.5 s: {tint:?}");

        let intents = live.click(super::super::ids::play());
        assert_eq!(live.apply(&intents), 0, "Stop is no edit: {intents:?}");
        assert!(!timeline_view(&live.ctx).playing);
        let at = live.doc.timeline.current_ms;
        assert!((1450..=1600).contains(&at), "{at}");
        let opacity = live.doc.layers.get(clip).unwrap().opacity;
        assert!((opacity - at as f32 / 3000.0).abs() < 1e-3);
        assert_eq!(live.history.undo_depth(), 0, "playback left no history");
    }
}
