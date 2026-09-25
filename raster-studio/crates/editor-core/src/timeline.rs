//! W13-L: the document's video timeline — Photopea's Animation panel in
//! Timeline mode (photopea.com/learn/video): each layer has an in/out bar,
//! and opacity, position, scale and rotation keyframes that interpolate over
//! time, each key with its own interpolation (Linear, Ease In, Ease Out,
//! Hold).
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
//! layer opacity at that time; a scale key the scale factors and a rotation
//! key the angle (degrees) of the transform's linear part. What a position
//! key holds depends on the track's [`LayerTrack::centred`] marker:
//!
//! - **Not centred** (every track saved in wave 13, and any track until it
//!   keys scale or rotation): the transform's translation, exactly as wave
//!   13 wrote it; the linear part is left as the layer has it.
//! - **Centred** (set when a track first keys scale or rotation): where the
//!   layer's centre is, as `M(c) - c` (`M` the layer transform, `c` the
//!   centre of the layer's canvas-sized pixel box, `(width / 2, height /
//!   2)` in layer pixels; equal to the translation only when the linear
//!   part is the identity). [`set_timeline`] migrates a track's position
//!   keys the moment it first keys scale or rotation: each translation `p`
//!   becomes `L c + p - c`, `L` the layer's linear part then (constant over
//!   an uncentred track, which never keys it), so the frame does not move.
//!
//! Scale and rotation turn **about that centre**: the translation is
//! recomputed so the centre stays where the position key (or, with none,
//! the layer as it stands) puts it. The pivot
//! is the pixel box's centre, not the bounds of what is painted: the model
//! holds no pixels to measure. That is what makes [`seek`] idempotent:
//! writing the values at `t` into the layers and evaluating again at `t`
//! gives the same values. A tracked layer is visible exactly while the
//! playhead is inside its bar (`in_ms <= t < out_ms`).
//!
//! # Interpolation
//!
//! Each key carries an [`Interpolation`] that governs the stretch from it to
//! the next key of the same property: `Linear` (constant speed), `EaseIn`
//! (starts slow: `f * f`), `EaseOut` (ends slow: `1 - (1 - f)^2`) and `Hold`
//! (keeps its value until the next key, then jumps). Before the first key
//! the first key's value holds, after the last the last's.
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

/// Which keyframed property a key belongs to. Serde append-only: new
/// properties go at the end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KeyProperty {
    Opacity,
    Position,
    /// W13X-9: the transform's scale factors, about the layer centre.
    Scale,
    /// W13X-9: the transform's rotation (degrees), about the layer centre.
    Rotation,
}

impl KeyProperty {
    /// Every property, in the order the Timeline rows draw their keys.
    pub const ALL: [KeyProperty; 4] = [
        KeyProperty::Opacity,
        KeyProperty::Position,
        KeyProperty::Scale,
        KeyProperty::Rotation,
    ];
}

/// W13X-9: how a key's value runs to the next key of the same property
/// (Photopea's keyframe interpolation). Serde append-only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Interpolation {
    /// Constant speed.
    #[default]
    Linear,
    /// Starts slow and speeds up: `f * f`.
    EaseIn,
    /// Starts fast and slows down: `1 - (1 - f)^2`.
    EaseOut,
    /// Holds this key's value until the next key, then jumps to it.
    Hold,
}

impl Interpolation {
    /// Every interpolation, in the order the panel offers them.
    pub const ALL: [Interpolation; 4] = [
        Interpolation::Linear,
        Interpolation::EaseIn,
        Interpolation::EaseOut,
        Interpolation::Hold,
    ];

    /// `true` for the default, so a key saved before W13X-9 reads back the
    /// same and a linear key writes no field.
    pub fn is_linear(&self) -> bool {
        *self == Interpolation::Linear
    }

    /// The eased fraction for a linear fraction `f` in `0.0..=1.0`.
    pub fn ease(self, f: f32) -> f32 {
        let f = f.clamp(0.0, 1.0);
        match self {
            Interpolation::Linear => f,
            Interpolation::EaseIn => f * f,
            Interpolation::EaseOut => 1.0 - (1.0 - f) * (1.0 - f),
            Interpolation::Hold => 0.0,
        }
    }
}

/// An opacity key: the layer's opacity (`0.0..=1.0`) at `t_ms`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OpacityKey {
    pub t_ms: u32,
    pub opacity: f32,
    /// W13X-9: how the value runs to the next key.
    #[serde(default, skip_serializing_if = "Interpolation::is_linear")]
    pub interp: Interpolation,
}

/// A position key: on a track that is not [`LayerTrack::centred`], the
/// transform's translation at `t_ms` (the wave-13 meaning); on a centred
/// track, where the layer's centre is, as `M(c) - c`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PositionKey {
    pub t_ms: u32,
    pub x: f32,
    pub y: f32,
    /// W13X-9: how the value runs to the next key.
    #[serde(default, skip_serializing_if = "Interpolation::is_linear")]
    pub interp: Interpolation,
}

/// W13X-9: a scale key: the transform's horizontal and vertical scale
/// factors (1.0 = 100 %) at `t_ms`, applied about the layer centre.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ScaleKey {
    pub t_ms: u32,
    pub sx: f32,
    pub sy: f32,
    #[serde(default, skip_serializing_if = "Interpolation::is_linear")]
    pub interp: Interpolation,
}

/// W13X-9: a rotation key: the transform's angle in degrees (clockwise on
/// screen, y down) at `t_ms`, about the layer centre. Interpolated in
/// degrees, so 0 to 720 turns twice.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RotationKey {
    pub t_ms: u32,
    pub degrees: f32,
    #[serde(default, skip_serializing_if = "Interpolation::is_linear")]
    pub interp: Interpolation,
}

/// The smallest scale factor magnitude a key holds: a zero scale would
/// make the transform singular. The sign is kept: a mirrored layer's
/// factor is negative.
pub const MIN_SCALE: f32 = 0.001;

/// W13X-9: `v` with its magnitude at least [`MIN_SCALE`] and its sign kept,
/// so a flipped layer (Free Transform dragged past the opposite handle)
/// keys `-1`, not a sliver. Zero reads as positive.
pub fn clamp_scale(v: f32) -> f32 {
    v.signum() * v.abs().max(MIN_SCALE)
}

fn is_false(b: &bool) -> bool {
    !*b
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
    /// W13X-9: scale keys (appended; absent in older documents).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scale: Vec<ScaleKey>,
    /// W13X-9: rotation keys (appended; absent in older documents).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rotation: Vec<RotationKey>,
    /// W13X-9: position keys hold the centre (`M(c) - c`), not the
    /// translation. Absent (false) in every wave-13 document, so a wave-13
    /// position key keeps meaning the translation; [`set_timeline`] sets it
    /// (migrating the keys) when the track first keys scale or rotation.
    #[serde(default, skip_serializing_if = "is_false")]
    pub centred: bool,
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
            scale: Vec::new(),
            rotation: Vec::new(),
            centred: false,
        }
    }

    /// Whether the layer is on screen at `t_ms`: inside `[in_ms, out_ms)`.
    pub fn shows_at(&self, t_ms: u32) -> bool {
        self.in_ms <= t_ms && t_ms < self.out_ms
    }

    /// The opacity at `t_ms`, when the track has opacity keys: the first
    /// key's value before it, the last key's after it, each key's
    /// [`Interpolation`] between.
    pub fn opacity_at(&self, t_ms: u32) -> Option<f32> {
        let keys: Vec<Keyed> = self
            .opacity
            .iter()
            .map(|k| (k.t_ms, Vec2::new(k.opacity, 0.0), k.interp))
            .collect();
        interpolate(&keys, t_ms).map(|v| v.x.clamp(0.0, 1.0))
    }

    /// The position at `t_ms`, when the track has position keys: the
    /// translation, or on a [`centred`](Self::centred) track the centre
    /// (`M(c) - c`).
    pub fn position_at(&self, t_ms: u32) -> Option<Vec2> {
        let keys: Vec<Keyed> = self
            .position
            .iter()
            .map(|k| (k.t_ms, Vec2::new(k.x, k.y), k.interp))
            .collect();
        interpolate(&keys, t_ms)
    }

    /// The scale factors at `t_ms`, when the track has scale keys.
    pub fn scale_at(&self, t_ms: u32) -> Option<Vec2> {
        let keys: Vec<Keyed> = self
            .scale
            .iter()
            .map(|k| (k.t_ms, Vec2::new(k.sx, k.sy), k.interp))
            .collect();
        interpolate(&keys, t_ms).map(|v| Vec2::new(clamp_scale(v.x), clamp_scale(v.y)))
    }

    /// The rotation in degrees at `t_ms`, when the track has rotation keys.
    pub fn rotation_at(&self, t_ms: u32) -> Option<f32> {
        let keys: Vec<Keyed> = self
            .rotation
            .iter()
            .map(|k| (k.t_ms, Vec2::new(k.degrees, 0.0), k.interp))
            .collect();
        interpolate(&keys, t_ms).map(|v| v.x)
    }

    /// The key times of `property`, in order.
    pub fn key_times(&self, property: KeyProperty) -> Vec<u32> {
        match property {
            KeyProperty::Opacity => self.opacity.iter().map(|k| k.t_ms).collect(),
            KeyProperty::Position => self.position.iter().map(|k| k.t_ms).collect(),
            KeyProperty::Scale => self.scale.iter().map(|k| k.t_ms).collect(),
            KeyProperty::Rotation => self.rotation.iter().map(|k| k.t_ms).collect(),
        }
    }

    /// The interpolation of key `index` (time order) of `property`.
    pub fn interpolation(&self, property: KeyProperty, index: usize) -> Option<Interpolation> {
        match property {
            KeyProperty::Opacity => self.opacity.get(index).map(|k| k.interp),
            KeyProperty::Position => self.position.get(index).map(|k| k.interp),
            KeyProperty::Scale => self.scale.get(index).map(|k| k.interp),
            KeyProperty::Rotation => self.rotation.get(index).map(|k| k.interp),
        }
    }

    /// Whether scale or rotation is keyed: the transform's linear part is
    /// then the track's, turned about the layer centre.
    pub fn keys_linear_part(&self) -> bool {
        !self.scale.is_empty() || !self.rotation.is_empty()
    }

    fn sort(&mut self) {
        self.opacity.sort_by_key(|k| k.t_ms);
        self.position.sort_by_key(|k| k.t_ms);
        self.scale.sort_by_key(|k| k.t_ms);
        self.rotation.sort_by_key(|k| k.t_ms);
    }
}

/// A key as the interpolator reads it: time, value, interpolation.
type Keyed = (u32, Vec2, Interpolation);

/// Interpolation over time-ordered `keys`, held flat outside them; the
/// stretch from one key to the next follows the first key's
/// [`Interpolation`].
fn interpolate(keys: &[Keyed], t_ms: u32) -> Option<Vec2> {
    let first = keys.first()?;
    if t_ms <= first.0 {
        return Some(first.1);
    }
    for pair in keys.windows(2) {
        let ((t0, v0, how), (t1, v1, _)) = (pair[0], pair[1]);
        if t_ms < t1 {
            let span = (t1 - t0).max(1) as f32;
            let f = how.ease((t_ms - t0) as f32 / span);
            return Some(v0 + (v1 - v0) * f);
        }
    }
    keys.last().map(|k| k.1)
}

/// The pivot scale and rotation turn about: the centre of the layer's
/// canvas-sized pixel box, in layer pixels.
fn pivot(doc: &Document) -> Vec2 {
    Vec2::new(doc.width() as f32, doc.height() as f32) * 0.5
}

/// Where `m` puts the layer centre, as a position key holds it: `M(c) - c`.
fn centre_position(m: &glam::Affine2, c: Vec2) -> Vec2 {
    m.transform_point2(c) - c
}

/// The transform `track` gives a layer whose transform is now `current` at
/// `t_ms`, or `None` when the track keys none of position, scale and
/// rotation. A track that is not centred and keys no scale or rotation
/// (every wave-13 track) sets the translation to the position key and keeps
/// the linear part. Otherwise scale and rotation replace the linear part
/// and turn about the centre `c`; the centre lands where the position key
/// puts it, or where it is now when position is not keyed. (A track keying
/// scale or rotation that is not marked centred cannot come from
/// [`set_timeline`], which migrates it; a hand-edited file with one reads
/// its position keys as centres until the next edit migrates them.)
fn keyed_transform(
    track: &LayerTrack,
    current: &glam::Affine2,
    c: Vec2,
    t_ms: u32,
) -> Option<glam::Affine2> {
    let position = track.position_at(t_ms);
    if !track.keys_linear_part() && !track.centred {
        return position.map(|p| glam::Affine2::from_mat2_translation(current.matrix2, p));
    }
    if position.is_none() && !track.keys_linear_part() {
        return None;
    }
    let base = position.unwrap_or_else(|| centre_position(current, c));
    let linear = if track.keys_linear_part() {
        let (scale_now, angle_now, _) = current.to_scale_angle_translation();
        let scale = track.scale_at(t_ms).unwrap_or(scale_now);
        let angle = track
            .rotation_at(t_ms)
            .map_or(angle_now, |d| d.to_radians());
        glam::Mat2::from_scale_angle(scale, angle)
    } else {
        current.matrix2
    };
    Some(glam::Affine2::from_mat2_translation(
        linear,
        base + c - linear * c,
    ))
}

/// Whether two transforms differ by more than float noise: a decompose and
/// recompose of the same scale and angle does not come back bit-exact, and
/// a seek must stay idempotent.
fn transform_differs(a: &glam::Affine2, b: &glam::Affine2) -> bool {
    a.to_cols_array()
        .iter()
        .zip(b.to_cols_array())
        .any(|(x, y)| (x - y).abs() > 1e-4)
}

/// W13X-9: the transform layer `layer` has at `t_ms` by its track (position,
/// scale and rotation about the centre), or `None` when the track keys none
/// of them or the layer has no track. What the Animation panel's preview
/// draws each thumbnail with.
pub fn transform_at(doc: &Document, layer: LayerId, t_ms: u32) -> Option<glam::Affine2> {
    let current = doc.layers.get(layer)?;
    let track = doc.timeline.track(layer)?;
    keyed_transform(track, &current.transform, pivot(doc), t_ms)
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
        if let Some(m) = keyed_transform(track, &layer.transform, pivot(doc), t_ms) {
            if !(respect_locks && layer.locked.blocks_transform())
                && transform_differs(&layer.transform, &m)
            {
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
        for k in &mut track.scale {
            k.sx = clamp_scale(k.sx);
            k.sy = clamp_scale(k.sy);
        }
        // W13X-9: the first scale or rotation key turns the position keys
        // from translations into centres (`L c + p - c`, `L` the layer's
        // linear part now, which an uncentred track never keyed), so the
        // layer does not jump.
        if !track.centred && track.keys_linear_part() {
            let c = pivot(doc);
            let linear = doc
                .layers
                .get(track.layer)
                .map_or(glam::Mat2::IDENTITY, |l| l.transform.matrix2);
            let shift = linear * c - c;
            for k in &mut track.position {
                k.x += shift.x;
                k.y += shift.y;
            }
            track.centred = true;
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

/// Run `$body` with `$keys` bound to the key list of `$property` on
/// `$track`, whatever that list's key type (every key type has `t_ms` and
/// `interp`).
macro_rules! with_keys {
    ($track:expr, $property:expr, |$keys:ident| $body:expr) => {
        match $property {
            KeyProperty::Opacity => {
                let $keys = &mut $track.opacity;
                $body
            }
            KeyProperty::Position => {
                let $keys = &mut $track.position;
                $body
            }
            KeyProperty::Scale => {
                let $keys = &mut $track.scale;
                $body
            }
            KeyProperty::Rotation => {
                let $keys = &mut $track.rotation;
                $body
            }
        }
    };
}

/// Add (or replace) a key of `property` on `layer` at `t_ms`, holding the
/// layer's current value: its opacity; its translation, or on a centred
/// track where its centre is (`M(c) - c`); or its transform's scale factors
/// (sign kept, so a mirrored layer keys a negative factor) or angle. A key
/// replaced at the same time keeps its interpolation.
pub fn add_key(
    doc: &Document,
    layer: LayerId,
    property: KeyProperty,
    t_ms: u32,
) -> Option<Command> {
    let current = doc.layers.get(layer)?;
    let (scale, angle, _) = current.transform.to_scale_angle_translation();
    let centre = centre_position(&current.transform, pivot(doc));
    let mut t = doc.timeline.clone();
    let track = t.track_mut(layer);
    let interp = with_keys!(track, property, |keys| {
        let old = keys.iter().find(|k| k.t_ms == t_ms).map(|k| k.interp);
        keys.retain(|k| k.t_ms != t_ms);
        old.unwrap_or_default()
    });
    match property {
        KeyProperty::Opacity => track.opacity.push(OpacityKey {
            t_ms,
            opacity: current.opacity,
            interp,
        }),
        KeyProperty::Position => {
            let p = if track.centred {
                centre
            } else {
                current.transform.translation
            };
            track.position.push(PositionKey {
                t_ms,
                x: p.x,
                y: p.y,
                interp,
            })
        }
        KeyProperty::Scale => track.scale.push(ScaleKey {
            t_ms,
            sx: clamp_scale(scale.x),
            sy: clamp_scale(scale.y),
            interp,
        }),
        KeyProperty::Rotation => track.rotation.push(RotationKey {
            t_ms,
            degrees: angle.to_degrees(),
            interp,
        }),
    }
    set_timeline(doc, "Add Keyframe", t)
}

/// W13X-9: set the interpolation of key `index` (time order) of `property`
/// on `layer`: how its value runs to the next key. `None` when there is no
/// such key or it already has `interp`.
pub fn set_interpolation(
    doc: &Document,
    layer: LayerId,
    property: KeyProperty,
    index: usize,
    interp: Interpolation,
) -> Option<Command> {
    let mut t = doc.timeline.clone();
    let track = t.tracks.iter_mut().find(|tr| tr.layer == layer)?;
    with_keys!(track, property, |keys| {
        keys.get_mut(index)?.interp = interp;
    });
    set_timeline(doc, "Keyframe Interpolation", t)
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
    with_keys!(track, property, |keys| {
        let mut key = *keys.get(index)?;
        keys.remove(index);
        keys.retain(|k| k.t_ms != t_ms);
        key.t_ms = t_ms;
        keys.push(key);
    });
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
    with_keys!(track, property, |keys| {
        keys.get(index)?;
        keys.remove(index);
    });
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
                interp: Interpolation::Linear,
            },
            OpacityKey {
                t_ms: 2000,
                opacity: 1.0,
                interp: Interpolation::Linear,
            },
        ];
        track.position = vec![
            PositionKey {
                t_ms: 0,
                x: 0.0,
                y: 0.0,
                interp: Interpolation::Linear,
            },
            PositionKey {
                t_ms: 2000,
                x: 20.0,
                y: -10.0,
                interp: Interpolation::Linear,
            },
        ];
    }

    /// W13X-9: each key's interpolation shapes the stretch to the next key:
    /// Ease In is `f * f`, Ease Out `1 - (1 - f)^2`, Hold keeps the value
    /// until the next key and then jumps.
    #[test]
    fn eased_and_hold_keys_give_their_values_at_t() {
        let (mut doc, id) = doc_with_layer();
        keyed(&mut doc, id);
        let set = |doc: &Document, how| {
            let c = set_interpolation(doc, id, KeyProperty::Opacity, 0, how).unwrap();
            let mut next = doc.clone();
            c.apply(&mut next).unwrap();
            next
        };
        let at = |doc: &Document, t| doc.timeline.track(id).unwrap().opacity_at(t).unwrap();
        // Keys: opacity 0 at 1000 ms, 1 at 2000 ms.
        let ease_in = set(&doc, Interpolation::EaseIn);
        assert!(
            (at(&ease_in, 1250) - 0.0625).abs() < 1e-6,
            "{}",
            at(&ease_in, 1250)
        );
        assert!((at(&ease_in, 1500) - 0.25).abs() < 1e-6);
        assert_eq!(at(&ease_in, 2000), 1.0);
        let ease_out = set(&doc, Interpolation::EaseOut);
        assert!(
            (at(&ease_out, 1250) - 0.4375).abs() < 1e-6,
            "{}",
            at(&ease_out, 1250)
        );
        assert!((at(&ease_out, 1500) - 0.75).abs() < 1e-6);
        let hold = set(&doc, Interpolation::Hold);
        assert_eq!(at(&hold, 1000), 0.0);
        assert_eq!(at(&hold, 1999), 0.0, "held right up to the next key");
        assert_eq!(at(&hold, 2000), 1.0, "then the next key's value");
        // The document at t carries the eased value; the key saves its mode.
        let layer_at = document_at(&ease_in, 1500);
        assert!((layer_at.layers.get(id).unwrap().opacity - 0.25).abs() < 1e-6);
        let json = serde_json::to_string(&hold.timeline).unwrap();
        assert!(json.contains("\"Hold\""), "{json}");
        let back: DocumentTimeline = serde_json::from_str(&json).unwrap();
        assert_eq!(back, hold.timeline);
        // Unchanged or missing keys are no command.
        assert!(
            set_interpolation(&hold, id, KeyProperty::Opacity, 0, Interpolation::Hold).is_none()
        );
        assert!(
            set_interpolation(&hold, id, KeyProperty::Opacity, 9, Interpolation::Hold).is_none()
        );
        // A position key eases the same way.
        let c =
            set_interpolation(&doc, id, KeyProperty::Position, 0, Interpolation::EaseIn).unwrap();
        c.apply(&mut doc).unwrap();
        let p = doc.timeline.track(id).unwrap().position_at(1000).unwrap();
        assert!((p - Vec2::new(5.0, -2.5)).length() < 1e-4, "{p}");
    }

    /// W13X-9: scale and rotation keys turn the layer about its centre; the
    /// centre stays put and seeking again changes nothing.
    #[test]
    fn scale_and_rotation_keys_turn_the_layer_about_its_centre() {
        let (mut doc, id) = doc_with_layer();
        let c = set_mode(&doc, true).unwrap();
        c.apply(&mut doc).unwrap();
        add_key(&doc, id, KeyProperty::Scale, 0)
            .unwrap()
            .apply(&mut doc)
            .unwrap();
        add_key(&doc, id, KeyProperty::Rotation, 0)
            .unwrap()
            .apply(&mut doc)
            .unwrap();
        {
            let track = doc
                .timeline
                .tracks
                .iter_mut()
                .find(|t| t.layer == id)
                .unwrap();
            track.scale.push(ScaleKey {
                t_ms: 1000,
                sx: 0.5,
                sy: 0.5,
                interp: Interpolation::Linear,
            });
            track.rotation.push(RotationKey {
                t_ms: 1000,
                degrees: 90.0,
                interp: Interpolation::Linear,
            });
        }
        let track = doc.timeline.track(id).unwrap();
        assert_eq!(track.scale[0].sx, 1.0, "the key held the layer's scale");
        assert!(track.rotation[0].degrees.abs() < 1e-4);
        assert_eq!(track.scale_at(500), Some(Vec2::splat(0.75)));
        assert_eq!(track.rotation_at(500), Some(45.0));

        let at = document_at(&doc, 1000);
        let m = at.layers.get(id).unwrap().transform;
        let centre = Vec2::new(16.0, 8.0);
        assert!(
            (m.transform_point2(centre) - centre).length() < 1e-3,
            "the centre stays"
        );
        // The top-left corner, 16 left and 8 up of the centre, halved and
        // turned 90 degrees clockwise (y down): 4 right and 8 up of it.
        let corner = m.transform_point2(Vec2::ZERO);
        assert!((corner - Vec2::new(20.0, 0.0)).length() < 1e-3, "{corner}");
        assert!(seek(&mut doc, 1000));
        assert!(!seek(&mut doc, 1000), "idempotent at the same time");
        assert!(transform_at(&doc, id, 1000).is_some());
        assert!(
            transform_at(&doc, id, 0).is_some_and(|m| (m.matrix2 - glam::Mat2::IDENTITY)
                .to_cols_array()
                .iter()
                .all(|v| v.abs() < 1e-4))
        );
        // A position key moves the centre while it turns.
        seek(&mut doc, 0);
        add_key(&doc, id, KeyProperty::Position, 0)
            .unwrap()
            .apply(&mut doc)
            .unwrap();
        let track = doc
            .timeline
            .tracks
            .iter_mut()
            .find(|t| t.layer == id)
            .unwrap();
        track.position.push(PositionKey {
            t_ms: 1000,
            x: 10.0,
            y: 4.0,
            interp: Interpolation::Linear,
        });
        let m = document_at(&doc, 1000).layers.get(id).unwrap().transform;
        assert!((m.transform_point2(centre) - (centre + Vec2::new(10.0, 4.0))).length() < 1e-3);
        // Saved and read back.
        let json = serde_json::to_string(&doc).unwrap();
        let back: Document = serde_json::from_str(&json).unwrap();
        assert_eq!(back.timeline, doc.timeline);
    }

    /// W13X-9 (round 2): keying Scale on a mirrored layer (Free Transform
    /// dragged past the opposite handle gives a negative determinant) keys
    /// the negative factor and leaves the layer as it is, not a sliver.
    #[test]
    fn a_flipped_layer_keys_its_negative_scale_and_stays_mirrored() {
        let (mut doc, id) = doc_with_layer();
        set_mode(&doc, true).unwrap().apply(&mut doc).unwrap();
        // Mirrored about the vertical line through the centre (x = 16).
        let mirrored = glam::Affine2::from_mat2_translation(
            glam::Mat2::from_diagonal(Vec2::new(-1.0, 1.0)),
            Vec2::new(32.0, 0.0),
        );
        doc.layers.get_mut(id).unwrap().transform = mirrored;
        add_key(&doc, id, KeyProperty::Scale, 0)
            .unwrap()
            .apply(&mut doc)
            .unwrap();
        let track = doc.timeline.track(id).unwrap();
        assert_eq!(
            (track.scale[0].sx, track.scale[0].sy),
            (-1.0, 1.0),
            "{:?}",
            track.scale[0]
        );
        assert_eq!(track.scale_at(0), Some(Vec2::new(-1.0, 1.0)));
        let m = doc.layers.get(id).unwrap().transform;
        assert!(!transform_differs(&m, &mirrored), "{m:?}");
        let at = document_at(&doc, 700).layers.get(id).unwrap().transform;
        assert!(!transform_differs(&at, &mirrored), "{at:?}");
        assert!(!seek(&mut doc, 0), "nothing to change at the key");
        // The sign survives the normalising pass; the magnitude is floored.
        assert_eq!(clamp_scale(-0.0001), -MIN_SCALE);
        assert_eq!(clamp_scale(0.0), MIN_SCALE);
        assert_eq!(clamp_scale(-2.0), -2.0);
    }

    /// W13X-9 (round 2): a wave-13 position key is the transform's
    /// translation, and still is on a scaled layer; keying rotation later
    /// migrates it to a centre without moving the frame.
    #[test]
    fn a_wave13_position_key_on_a_scaled_layer_is_still_the_translation() {
        let (mut doc, id) = doc_with_layer();
        let half = glam::Mat2::from_diagonal(Vec2::splat(0.5));
        doc.layers.get_mut(id).unwrap().transform =
            glam::Affine2::from_mat2_translation(half, Vec2::ZERO);
        // Exactly what wave 13 (06abd74) wrote for one position key.
        let id_json = serde_json::to_string(&id).unwrap();
        let wave13 = format!(
            r#"{{"enabled":true,"duration_ms":3000,"fps":30,"current_ms":0,"tracks":[{{"layer":{id_json},"in_ms":0,"out_ms":3000,"position":[{{"t_ms":0,"x":3.0,"y":4.0}}]}}]}}"#
        );
        doc.timeline = serde_json::from_str(&wave13).unwrap();
        assert!(!doc.timeline.track(id).unwrap().centred);
        let m = document_at(&doc, 0).layers.get(id).unwrap().transform;
        assert_eq!(m.translation, Vec2::new(3.0, 4.0), "{m:?}");
        assert_eq!(m.matrix2, half, "the linear part is the layer's");
        // Saved again, the track writes no marker: byte-for-byte wave 13.
        let text = serde_json::to_string(&doc.timeline).unwrap();
        assert!(!text.contains("centred"), "{text}");

        seek(&mut doc, 0);
        add_key(&doc, id, KeyProperty::Rotation, 0)
            .unwrap()
            .apply(&mut doc)
            .unwrap();
        let track = doc.timeline.track(id).unwrap();
        assert!(track.centred, "migrated with the first rotation key");
        // The centre (16, 8) at half scale plus (3, 4) sits at (11, 8); as
        // a centre key that is (11, 8) - (16, 8) = (-5, 0).
        assert_eq!(track.position_at(0), Some(Vec2::new(-5.0, 0.0)));
        let m = document_at(&doc, 0).layers.get(id).unwrap().transform;
        assert!(
            (m.translation - Vec2::new(3.0, 4.0)).length() < 1e-4,
            "the frame did not move: {m:?}"
        );
        assert!(!seek(&mut doc, 0));
        let text = serde_json::to_string(&doc.timeline).unwrap();
        assert!(text.contains("\"centred\":true"), "{text}");
    }

    /// A timeline saved before W13X-9 (no interp, no scale / rotation
    /// fields) is exactly what linear keys with no scale / rotation write,
    /// so an old file reads back linear and unchanged.
    #[test]
    fn a_pre_w13x9_timeline_reads_back_linear() {
        let (mut doc, id) = doc_with_layer();
        keyed(&mut doc, id);
        let text = serde_json::to_string(&doc.timeline).unwrap();
        for field in ["interp", "scale", "rotation"] {
            assert!(!text.contains(field), "{field}: {text}");
        }
        let back: DocumentTimeline = serde_json::from_str(&text).unwrap();
        assert_eq!(back, doc.timeline);
        assert_eq!(back.tracks[0].opacity[0].interp, Interpolation::Linear);
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
                opacity: 0.25,
                interp: Interpolation::Linear,
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
