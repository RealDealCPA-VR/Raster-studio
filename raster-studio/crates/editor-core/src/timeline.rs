//! W13-L: the document's video timeline — Photopea's Animation panel in
//! Timeline mode (photopea.com/learn/video): each layer has an in/out bar,
//! and opacity and position keyframes that interpolate over time.
//!
//! # The record
//!
//! [`DocumentTimeline`] rides on the [`Document`] (saved with the `.rstudio`
//! document, omitted while it is the default) and changes through
//! [`Command::SetTimeline`], so every edit is one undo step. A layer with no
//! [`LayerTrack`] is on screen for the whole timeline exactly as the document
//! has it.
//!
//! # What a track governs
//!
//! Keyframed values are **absolute**, not offsets: an opacity key holds the
//! layer opacity at that time and a position key holds the translation part
//! of the layer's transform. That is what makes [`seek`] idempotent:
//! writing the values at `t` into the layers and evaluating again at `t`
//! gives the same values. A tracked layer is visible exactly while the
//! playhead is inside its bar (`in_ms <= t < out_ms`).
//!
//! # Time t
//!
//! [`document_at`] is the document as it stands at `t` (a clone with the
//! tracked layers' visibility, opacity and translation set), which is what
//! the compositor renders for an exported video frame. [`seek`] moves the
//! playhead and puts those same values on the real layers, so the canvas
//! shows the frame at `t`.
//!
//! # The playhead is not an edit
//!
//! As in Photopea, moving the playhead (a ruler scrub, playback, Stop) is
//! not a history step and does not make the document dirty: [`seek`] writes
//! the document directly, with no [`Command`]. [`Command::SetTimeline`]
//! likewise leaves the playhead where it is when it is applied or undone, so
//! Undo after a scrub takes back the last edit, not the scrub.

use glam::Vec2;
use layer_model::LayerId;
use serde::{Deserialize, Serialize};

use crate::command::{Command, LayerPatch};
use crate::document::Document;

/// Frames per second a new timeline plays and exports at.
pub const DEFAULT_FPS: u32 = 30;
/// Length of a new timeline, in milliseconds.
pub const DEFAULT_DURATION_MS: u32 = 3000;
/// The frame rates the timeline accepts.
pub const FPS_RANGE: std::ops::RangeInclusive<u32> = 1..=60;
/// The lengths the timeline accepts: 100 ms to ten minutes.
pub const DURATION_RANGE_MS: std::ops::RangeInclusive<u32> = 100..=600_000;

fn default_fps() -> u32 {
    DEFAULT_FPS
}

fn default_duration() -> u32 {
    DEFAULT_DURATION_MS
}

/// Which keyframed property a key belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KeyProperty {
    Opacity,
    Position,
}

/// An opacity key: the layer's opacity (`0.0..=1.0`) at `t_ms`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OpacityKey {
    pub t_ms: u32,
    pub opacity: f32,
}

/// A position key: the layer transform's translation at `t_ms`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PositionKey {
    pub t_ms: u32,
    pub x: f32,
    pub y: f32,
}

/// One layer's row on the timeline: its bar and its keys, each list kept in
/// time order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayerTrack {
    pub layer: LayerId,
    /// Where the bar starts (the layer appears), in milliseconds.
    #[serde(default)]
    pub in_ms: u32,
    /// Where the bar ends (the layer is gone from here on).
    pub out_ms: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub opacity: Vec<OpacityKey>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub position: Vec<PositionKey>,
}

impl LayerTrack {
    /// A bar covering the whole of a `duration_ms` timeline, with no keys.
    pub fn new(layer: LayerId, duration_ms: u32) -> Self {
        Self {
            layer,
            in_ms: 0,
            out_ms: duration_ms,
            opacity: Vec::new(),
            position: Vec::new(),
        }
    }

    /// Whether the layer is on screen at `t_ms`: inside `[in_ms, out_ms)`.
    pub fn shows_at(&self, t_ms: u32) -> bool {
        self.in_ms <= t_ms && t_ms < self.out_ms
    }

    /// The opacity at `t_ms`, when the track has opacity keys: the first
    /// key's value before it, the last key's after it, linear between.
    pub fn opacity_at(&self, t_ms: u32) -> Option<f32> {
        let keys: Vec<(u32, Vec2)> = self
            .opacity
            .iter()
            .map(|k| (k.t_ms, Vec2::new(k.opacity, 0.0)))
            .collect();
        interpolate(&keys, t_ms).map(|v| v.x.clamp(0.0, 1.0))
    }

    /// The translation at `t_ms`, when the track has position keys.
    pub fn position_at(&self, t_ms: u32) -> Option<Vec2> {
        let keys: Vec<(u32, Vec2)> = self
            .position
            .iter()
            .map(|k| (k.t_ms, Vec2::new(k.x, k.y)))
            .collect();
        interpolate(&keys, t_ms)
    }

    /// The key times of `property`, in order.
    pub fn key_times(&self, property: KeyProperty) -> Vec<u32> {
        match property {
            KeyProperty::Opacity => self.opacity.iter().map(|k| k.t_ms).collect(),
            KeyProperty::Position => self.position.iter().map(|k| k.t_ms).collect(),
        }
    }

    fn sort(&mut self) {
        self.opacity.sort_by_key(|k| k.t_ms);
        self.position.sort_by_key(|k| k.t_ms);
    }
}

/// Linear interpolation over time-ordered `keys`, held flat outside them.
fn interpolate(keys: &[(u32, Vec2)], t_ms: u32) -> Option<Vec2> {
    let first = keys.first()?;
    if t_ms <= first.0 {
        return Some(first.1);
    }
    for pair in keys.windows(2) {
        let ((t0, v0), (t1, v1)) = (pair[0], pair[1]);
        if t_ms < t1 {
            let span = (t1 - t0).max(1) as f32;
            let f = (t_ms - t0) as f32 / span;
            return Some(v0 + (v1 - v0) * f);
        }
    }
    keys.last().map(|k| k.1)
}

/// The document's timeline: Timeline mode on or off, its length and frame
/// rate, where the playhead is, and the layer tracks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocumentTimeline {
    /// Timeline mode: the Animation panel shows the timeline, and an animated
    /// export writes the timeline's frames instead of the `_a_` frame layers.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_duration")]
    pub duration_ms: u32,
    #[serde(default = "default_fps")]
    pub fps: u32,
    /// The playhead, in milliseconds.
    #[serde(default)]
    pub current_ms: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tracks: Vec<LayerTrack>,
}

impl Default for DocumentTimeline {
    fn default() -> Self {
        Self {
            enabled: false,
            duration_ms: DEFAULT_DURATION_MS,
            fps: DEFAULT_FPS,
            current_ms: 0,
            tracks: Vec::new(),
        }
    }
}

impl DocumentTimeline {
    /// `true` while nothing differs from a new document's timeline, so the
    /// document can omit it.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The track of `layer`, if it has one.
    pub fn track(&self, layer: LayerId) -> Option<&LayerTrack> {
        self.tracks.iter().find(|t| t.layer == layer)
    }

    /// The track of `layer`, created (covering the whole timeline) if missing.
    pub fn track_mut(&mut self, layer: LayerId) -> &mut LayerTrack {
        if let Some(i) = self.tracks.iter().position(|t| t.layer == layer) {
            return &mut self.tracks[i];
        }
        self.tracks.push(LayerTrack::new(layer, self.duration_ms));
        self.tracks.last_mut().expect("just pushed")
    }

    /// The frames an export writes: `(time, duration)` in milliseconds, one
    /// per `1/fps` second over the whole length. The durations add up to the
    /// length exactly (each frame starts at the rounded multiple of
    /// `1000 / fps`).
    pub fn frame_times(&self) -> Vec<(u32, u32)> {
        let fps = u64::from(self.fps.clamp(*FPS_RANGE.start(), *FPS_RANGE.end()));
        let duration = u64::from(self.duration_ms.max(1));
        let count = (duration * fps).div_ceil(1000).max(1);
        let start = |i: u64| ((i * 1000 + fps / 2) / fps).min(duration) as u32;
        (0..count)
            .map(|i| {
                let t = start(i);
                let end = if i + 1 == count {
                    duration as u32
                } else {
                    start(i + 1)
                };
                (t, end.saturating_sub(t).max(1))
            })
            .collect()
    }
}

/// The value of every tracked layer at `t_ms`, as the patches that would put
/// them on the document's layers. Layers that left the tree are skipped.
/// `respect_locks` leaves out what a lock refuses (a fully locked layer
/// entirely, a position-locked layer's transform), which is what a command
/// must do; a render ignores locks.
fn patches_at(doc: &Document, t_ms: u32, respect_locks: bool) -> Vec<(LayerId, LayerPatch)> {
    let mut out = Vec::new();
    for track in &doc.timeline.tracks {
        let Some(layer) = doc.layers.get(track.layer) else {
            continue;
        };
        if respect_locks && layer.locked.all {
            continue;
        }
        let mut patch = LayerPatch::default();
        let visible = track.shows_at(t_ms);
        if layer.visible != visible {
            patch.visible = Some(visible);
        }
        if let Some(o) = track.opacity_at(t_ms) {
            if (layer.opacity - o).abs() > f32::EPSILON {
                patch.opacity = Some(o);
            }
        }
        if let Some(p) = track.position_at(t_ms) {
            if !(respect_locks && layer.locked.blocks_transform())
                && layer.transform.translation != p
            {
                let mut m = layer.transform;
                m.translation = p;
                patch.transform = Some(m.to_cols_array());
            }
        }
        if patch != LayerPatch::default() {
            out.push((track.layer, patch));
        }
    }
    out
}

/// Write `patches` (visibility, opacity, transform only) onto `doc`'s layers.
fn put_patches(doc: &mut Document, patches: Vec<(LayerId, LayerPatch)>) {
    for (id, patch) in patches {
        if let Some(layer) = doc.layers.get_mut(id) {
            if let Some(v) = patch.visible {
                layer.visible = v;
            }
            if let Some(o) = patch.opacity {
                layer.opacity = o;
            }
            if let Some(m) = patch.transform {
                layer.transform = glam::Affine2::from_cols_array(&m);
            }
        }
    }
}

/// The document as it stands at `t_ms`: a copy whose tracked layers carry
/// their visibility, opacity and translation at that time. This is what a
/// video frame renders.
pub fn document_at(doc: &Document, t_ms: u32) -> Document {
    let patches = patches_at(doc, t_ms, false);
    let mut out = doc.clone();
    put_patches(&mut out, patches);
    out
}

/// Move the playhead to `t_ms` (clamped to the length) and, in Timeline
/// mode, put each tracked layer's visibility, opacity and translation at that
/// time on the layer (a lock is respected), so the canvas shows that frame.
///
/// History-free on purpose: no [`Command`], no undo step, no dirty flag. The
/// playhead is where the user is looking, not an edit (a scrub in Photopea
/// is not a history state). Answers whether anything changed.
pub fn seek(doc: &mut Document, t_ms: u32) -> bool {
    let t_ms = t_ms.min(doc.timeline.duration_ms);
    let patches = if doc.timeline.enabled {
        patches_at(doc, t_ms, true)
    } else {
        Vec::new()
    };
    if doc.timeline.current_ms == t_ms && patches.is_empty() {
        return false;
    }
    doc.timeline.current_ms = t_ms;
    put_patches(doc, patches);
    true
}

/// The command that makes `timeline` the document's and shows the frame at
/// its playhead on the layers: one undo step. `None` when nothing changes.
pub fn set_timeline(doc: &Document, label: &str, timeline: DocumentTimeline) -> Option<Command> {
    let mut timeline = timeline;
    timeline.duration_ms = timeline
        .duration_ms
        .clamp(*DURATION_RANGE_MS.start(), *DURATION_RANGE_MS.end());
    timeline.fps = timeline.fps.clamp(*FPS_RANGE.start(), *FPS_RANGE.end());
    timeline.current_ms = timeline.current_ms.min(timeline.duration_ms);
    for track in &mut timeline.tracks {
        track.out_ms = track.out_ms.min(timeline.duration_ms);
        track.in_ms = track.in_ms.min(track.out_ms);
        for k in &mut track.opacity {
            k.opacity = k.opacity.clamp(0.0, 1.0);
        }
        track.sort();
    }
    // Evaluate the new timeline against the layers as they are now.
    let mut probe = doc.clone();
    probe.timeline = timeline.clone();
    let patches = if timeline.enabled {
        patches_at(&probe, timeline.current_ms, true)
    } else {
        Vec::new()
    };
    if timeline == doc.timeline && patches.is_empty() {
        return None;
    }
    let mut commands = vec![Command::SetTimeline {
        timeline: Box::new(timeline),
    }];
    commands.extend(
        patches
            .into_iter()
            .map(|(layer_id, patch)| Command::SetLayerProperties { layer_id, patch }),
    );
    Some(Command::Transaction {
        label: label.to_string(),
        commands,
    })
}

/// Timeline mode on or off.
pub fn set_mode(doc: &Document, enabled: bool) -> Option<Command> {
    let mut t = doc.timeline.clone();
    t.enabled = enabled;
    set_timeline(
        doc,
        if enabled {
            "Timeline Mode"
        } else {
            "Frame Mode"
        },
        t,
    )
}

/// Set `layer`'s bar to `[in_ms, out_ms)`.
pub fn set_in_out(doc: &Document, layer: LayerId, in_ms: u32, out_ms: u32) -> Option<Command> {
    doc.layers.get(layer)?;
    let mut t = doc.timeline.clone();
    let track = t.track_mut(layer);
    track.in_ms = in_ms.min(out_ms);
    track.out_ms = out_ms.max(in_ms);
    set_timeline(doc, "Set Layer Duration", t)
}

/// Add (or replace) a key of `property` on `layer` at `t_ms`, holding the
/// layer's current value: its opacity, or its transform's translation.
pub fn add_key(
    doc: &Document,
    layer: LayerId,
    property: KeyProperty,
    t_ms: u32,
) -> Option<Command> {
    let current = doc.layers.get(layer)?;
    let mut t = doc.timeline.clone();
    let track = t.track_mut(layer);
    match property {
        KeyProperty::Opacity => {
            track.opacity.retain(|k| k.t_ms != t_ms);
            track.opacity.push(OpacityKey {
                t_ms,
                opacity: current.opacity,
            });
        }
        KeyProperty::Position => {
            track.position.retain(|k| k.t_ms != t_ms);
            let p = current.transform.translation;
            track.position.push(PositionKey {
                t_ms,
                x: p.x,
                y: p.y,
            });
        }
    }
    set_timeline(doc, "Add Keyframe", t)
}

/// Move key `index` (time order) of `property` on `layer` to `t_ms`. A key
/// already at `t_ms` is replaced by the moved one.
pub fn move_key(
    doc: &Document,
    layer: LayerId,
    property: KeyProperty,
    index: usize,
    t_ms: u32,
) -> Option<Command> {
    let mut t = doc.timeline.clone();
    let track = t.tracks.iter_mut().find(|tr| tr.layer == layer)?;
    match property {
        KeyProperty::Opacity => {
            let mut key = *track.opacity.get(index)?;
            track.opacity.remove(index);
            track.opacity.retain(|k| k.t_ms != t_ms);
            key.t_ms = t_ms;
            track.opacity.push(key);
        }
        KeyProperty::Position => {
            let mut key = *track.position.get(index)?;
            track.position.remove(index);
            track.position.retain(|k| k.t_ms != t_ms);
            key.t_ms = t_ms;
            track.position.push(key);
        }
    }
    set_timeline(doc, "Move Keyframe", t)
}

/// Delete key `index` (time order) of `property` on `layer`.
pub fn delete_key(
    doc: &Document,
    layer: LayerId,
    property: KeyProperty,
    index: usize,
) -> Option<Command> {
    let mut t = doc.timeline.clone();
    let track = t.tracks.iter_mut().find(|tr| tr.layer == layer)?;
    match property {
        KeyProperty::Opacity => {
            track.opacity.get(index)?;
            track.opacity.remove(index);
        }
        KeyProperty::Position => {
            track.position.get(index)?;
            track.position.remove(index);
        }
    }
    set_timeline(doc, "Delete Keyframe", t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use layer_model::Layer;

    fn doc_with_layer() -> (Document, LayerId) {
        let mut doc = Document::new(32, 16, "video");
        let id = doc.layers.push_root(Layer::raster("Clip")).unwrap();
        (doc, id)
    }

    fn keyed(doc: &mut Document, id: LayerId) {
        let t = &mut doc.timeline;
        t.enabled = true;
        let track = t.track_mut(id);
        track.in_ms = 500;
        track.out_ms = 2500;
        track.opacity = vec![
            OpacityKey {
                t_ms: 1000,
                opacity: 0.0,
            },
            OpacityKey {
                t_ms: 2000,
                opacity: 1.0,
            },
        ];
        track.position = vec![
            PositionKey {
                t_ms: 0,
                x: 0.0,
                y: 0.0,
            },
            PositionKey {
                t_ms: 2000,
                x: 20.0,
                y: -10.0,
            },
        ];
    }

    #[test]
    fn keyframes_interpolate_linearly_at_t_and_hold_outside_the_keys() {
        let (mut doc, id) = doc_with_layer();
        keyed(&mut doc, id);
        let track = doc.timeline.track(id).unwrap();
        assert_eq!(track.opacity_at(0), Some(0.0), "held before the first key");
        assert_eq!(track.opacity_at(1000), Some(0.0));
        assert!((track.opacity_at(1250).unwrap() - 0.25).abs() < 1e-6);
        assert!((track.opacity_at(1500).unwrap() - 0.5).abs() < 1e-6);
        assert_eq!(track.opacity_at(2000), Some(1.0));
        assert_eq!(track.opacity_at(2900), Some(1.0), "held after the last");
        assert_eq!(track.position_at(1000), Some(Vec2::new(10.0, -5.0)));
        assert_eq!(track.position_at(500), Some(Vec2::new(5.0, -2.5)));

        let at = document_at(&doc, 1500);
        let layer = at.layers.get(id).unwrap();
        assert!((layer.opacity - 0.5).abs() < 1e-6);
        assert_eq!(layer.transform.translation, Vec2::new(15.0, -7.5));
        assert!(layer.visible);
        assert!(!document_at(&doc, 400).layers.get(id).unwrap().visible);
        assert!(!document_at(&doc, 2500).layers.get(id).unwrap().visible);
        // The source document is untouched.
        assert_eq!(doc.layers.get(id).unwrap().opacity, 1.0);
    }

    #[test]
    fn seek_puts_the_values_at_t_on_the_layers_with_no_history_and_no_dirty_flag() {
        let (mut doc, id) = doc_with_layer();
        keyed(&mut doc, id);
        doc.mark_saved();
        assert!(seek(&mut doc, 1500), "a change");
        assert_eq!(doc.timeline.current_ms, 1500);
        let layer = doc.layers.get(id).unwrap();
        assert!((layer.opacity - 0.5).abs() < 1e-6);
        assert_eq!(layer.transform.translation, Vec2::new(15.0, -7.5));
        assert!(!doc.is_dirty(), "a scrub is not an edit");
        // Idempotent: at the same time again there is nothing to do.
        assert!(!seek(&mut doc, 1500));
        // Clamped to the length; past the bar the layer is hidden.
        assert!(seek(&mut doc, 99_999));
        assert_eq!(doc.timeline.current_ms, doc.timeline.duration_ms);
        assert!(!doc.layers.get(id).unwrap().visible);
    }

    /// Undo after a scrub takes back the last edit and leaves the playhead
    /// where the scrub put it.
    #[test]
    fn a_timeline_edit_undone_after_a_scrub_keeps_the_playhead() {
        let (mut doc, id) = doc_with_layer();
        let mut history = crate::History::new();
        let c = set_mode(&doc, true).unwrap();
        history.apply(&mut doc, c).unwrap();
        let c = add_key(&doc, id, KeyProperty::Opacity, 0).unwrap();
        history.apply(&mut doc, c).unwrap();
        assert_eq!(history.undo_depth(), 2);
        seek(&mut doc, 1200);
        assert_eq!(history.undo_depth(), 2, "the scrub is no step");
        history.undo(&mut doc).unwrap();
        assert!(doc.timeline.track(id).is_none_or(|t| t.opacity.is_empty()));
        assert_eq!(doc.timeline.current_ms, 1200, "undo left the playhead");
        history.redo(&mut doc).unwrap();
        assert_eq!(doc.timeline.current_ms, 1200, "and so did redo");
    }

    #[test]
    fn keys_are_added_moved_and_deleted_through_commands() {
        let (mut doc, id) = doc_with_layer();
        let c = set_mode(&doc, true).unwrap();
        c.apply(&mut doc).unwrap();
        doc.layers.get_mut(id).unwrap().opacity = 0.25;
        add_key(&doc, id, KeyProperty::Opacity, 2000)
            .unwrap()
            .apply(&mut doc)
            .unwrap();
        add_key(&doc, id, KeyProperty::Position, 100)
            .unwrap()
            .apply(&mut doc)
            .unwrap();
        let track = doc.timeline.track(id).unwrap();
        assert_eq!(
            track.opacity,
            vec![OpacityKey {
                t_ms: 2000,
                opacity: 0.25
            }]
        );
        assert_eq!(track.key_times(KeyProperty::Position), vec![100]);
        move_key(&doc, id, KeyProperty::Opacity, 0, 700)
            .unwrap()
            .apply(&mut doc)
            .unwrap();
        assert_eq!(
            doc.timeline
                .track(id)
                .unwrap()
                .key_times(KeyProperty::Opacity),
            vec![700]
        );
        delete_key(&doc, id, KeyProperty::Position, 0)
            .unwrap()
            .apply(&mut doc)
            .unwrap();
        assert!(doc.timeline.track(id).unwrap().position.is_empty());
        assert!(delete_key(&doc, id, KeyProperty::Position, 0).is_none());
    }

    #[test]
    fn the_timeline_survives_a_save_and_load_and_is_omitted_while_default() {
        let (mut doc, id) = doc_with_layer();
        let json = serde_json::to_string(&doc).unwrap();
        assert!(!json.contains("\"timeline\""), "{json}");
        keyed(&mut doc, id);
        doc.timeline.fps = 24;
        let json = serde_json::to_string(&doc).unwrap();
        assert!(json.contains("\"timeline\""), "{json}");
        let back: Document = serde_json::from_str(&json).unwrap();
        assert_eq!(back.timeline, doc.timeline);
        assert_eq!(back, doc);
        let named = rmp_serde::to_vec_named(&doc).unwrap();
        let back: Document = rmp_serde::from_slice(&named).unwrap();
        assert_eq!(back.timeline, doc.timeline);
    }

    #[test]
    fn frame_times_cover_the_length_at_the_frame_rate() {
        let t = DocumentTimeline {
            duration_ms: 1000,
            fps: 3,
            ..DocumentTimeline::default()
        };
        assert_eq!(t.frame_times(), vec![(0, 333), (333, 334), (667, 333)]);
        let t = DocumentTimeline::default();
        let frames = t.frame_times();
        assert_eq!(frames.len(), 90);
        assert_eq!(frames.iter().map(|f| f.1).sum::<u32>(), 3000);
    }
}
