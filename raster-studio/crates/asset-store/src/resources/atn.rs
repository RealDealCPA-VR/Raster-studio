//! W13-E: Photoshop / Photopea action sets (`.atn`, version 16).
//!
//! An `.atn` file is one action *set*: a name and a list of actions, each a
//! list of *steps*. A step is an event id (a four-character code such as
//! `GsnB`, or a string id such as `gaussianBlur`), the name Photoshop shows
//! for it, and usually an action descriptor carrying its parameters
//! ("Radius: 4 px"). The layout, all big-endian:
//!
//! ```text
//! set    := u32 version (16)  unicode name  u8 expanded  u32 action-count  action*
//! action := u16 function-key  u8 shift  u8 command  u16 colour
//!           unicode name  u8 expanded  u32 step-count  step*
//! step   := u8 expanded  u8 enabled  u8 with-dialog  u8 dialog-options
//!           ('TEXT' u32 len ascii | 'long' fourcc)   event id
//!           u32 len ascii                            display name
//!           i32 flag (-1: u32 descriptor version 16 + descriptor; 0: none)
//! ```
//!
//! [`parse`] reads that into an [`AtnSet`]; [`write`] writes one back, and
//! [`parse`] of [`write`] is the identity. [`interpret`] turns a step into a
//! [`StepOp`] — the operations this application can perform — or says why it
//! cannot, so a player can show an unmapped step as skipped instead of
//! dropping it. [`StepOp::to_step`] is the inverse, used to export recorded
//! edits.
//!
//! Untrusted input: counts are checked against [`MAX_ENTRIES`] before
//! anything is reserved, strings against [`MAX_ATN_STRING`], and descriptors
//! go through the `psd` crate's depth- and count-limited reader. A damaged
//! file is a [`ResourceError`], never a panic.

use psd::bytes::{Cursor, Sink};
use psd::{Descriptor, RefItem, Value};
use serde::{Deserialize, Serialize};

use super::{check_count, ResourceError, MAX_ENTRIES};

// W16-H: the events beyond the first eighteen, and their descriptors.
#[path = "atn_more.rs"]
mod more;
pub use more::{
    blend_from_blnm, blnm, lab_to_rgb, rgb_to_lab, CurvesEntry, LayerRef, LevelsEntry, ModeTarget,
    Pivot, StrokeAt, ToneChannel, TransformTarget, TrimBasis,
};

/// The only `.atn` layout this reader accepts (Photoshop 6 and later).
pub const ATN_VERSION: u32 = 16;

/// Longest ASCII string (event string id, display name) a step may carry.
pub const MAX_ATN_STRING: usize = 4096;

/// Longest Unicode name (set or action), in UTF-16 code units.
const MAX_ATN_NAME: usize = 4096;

/// One action set: what an `.atn` file holds.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AtnSet {
    pub name: String,
    pub expanded: bool,
    pub actions: Vec<AtnAction>,
}

/// One action of a set.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AtnAction {
    pub name: String,
    /// The F-key assigned to it (0 = none).
    pub function_key: u16,
    pub shift: bool,
    pub command: bool,
    /// Photoshop's button-mode colour index.
    pub color: u16,
    pub expanded: bool,
    pub steps: Vec<AtnStep>,
}

/// One step of an action: an event and its parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(into = "StepWire", try_from = "StepWire")]
pub struct AtnStep {
    pub expanded: bool,
    /// The step's check box in the Actions panel: an unchecked step is not
    /// played.
    pub enabled: bool,
    pub with_dialog: bool,
    pub dialog_options: u8,
    /// The event id: a four-character code when `char_id`, else a string id.
    pub event: String,
    pub char_id: bool,
    /// The name Photoshop shows for the step ("Gaussian Blur").
    pub name: String,
    pub descriptor: Option<Descriptor>,
}

impl AtnStep {
    /// A step for `event`, enabled, with `descriptor`.
    pub fn new(event: &str, name: &str, descriptor: Option<Descriptor>) -> Self {
        Self {
            expanded: false,
            enabled: true,
            with_dialog: false,
            dialog_options: 0,
            event: event.to_string(),
            char_id: event.len() == 4,
            name: name.to_string(),
            descriptor,
        }
    }
}

/// How an [`AtnStep`] is kept in the application's own actions file: the
/// descriptor as its ATN bytes, so nothing is lost across a save.
#[derive(Serialize, Deserialize)]
struct StepWire {
    expanded: bool,
    enabled: bool,
    with_dialog: bool,
    dialog_options: u8,
    event: String,
    char_id: bool,
    name: String,
    descriptor: Option<Vec<u8>>,
}

impl From<AtnStep> for StepWire {
    fn from(s: AtnStep) -> Self {
        let descriptor = s.descriptor.as_ref().and_then(|d| {
            let mut sink = Sink::new();
            d.write(&mut sink).ok()?;
            Some(sink.into_inner())
        });
        Self {
            expanded: s.expanded,
            enabled: s.enabled,
            with_dialog: s.with_dialog,
            dialog_options: s.dialog_options,
            event: s.event,
            char_id: s.char_id,
            name: s.name,
            descriptor,
        }
    }
}

impl TryFrom<StepWire> for AtnStep {
    type Error = String;
    fn try_from(w: StepWire) -> Result<Self, String> {
        let descriptor = match w.descriptor {
            Some(bytes) => Some(
                Descriptor::read(&mut Cursor::new(&bytes), &psd::ReadOptions::default())
                    .map_err(|e| format!("step {}: {e}", w.name))?,
            ),
            None => None,
        };
        Ok(Self {
            expanded: w.expanded,
            enabled: w.enabled,
            with_dialog: w.with_dialog,
            dialog_options: w.dialog_options,
            event: w.event,
            char_id: w.char_id,
            name: w.name,
            descriptor,
        })
    }
}

// ------------------------------------------------------------------ reading

fn ascii(cur: &mut Cursor<'_>, what: &'static str) -> Result<String, ResourceError> {
    let len = cur.u32()? as usize;
    check_count(what, len, MAX_ATN_STRING)?;
    Ok(String::from_utf8_lossy(cur.take(len)?).into_owned())
}

/// Parse an `.atn` file.
pub fn parse(bytes: &[u8]) -> Result<AtnSet, ResourceError> {
    let mut cur = Cursor::new(bytes);
    let version = cur.u32()?;
    if version != ATN_VERSION {
        return Err(ResourceError::Unsupported {
            what: "actions file version",
            detail: format!("{version} (only version 16, Photoshop 6 and later, is read)"),
        });
    }
    let name = cur.unicode_string(MAX_ATN_NAME)?;
    let expanded = cur.u8()? != 0;
    let count = cur.u32()? as usize;
    check_count("action count", count, MAX_ENTRIES)?;
    let mut actions = Vec::with_capacity(count);
    for index in 0..count {
        actions.push(
            read_action(&mut cur)
                .map_err(|e| ResourceError::Malformed(format!("action {}: {e}", index + 1)))?,
        );
    }
    if !cur.is_empty() {
        return Err(ResourceError::Malformed(format!(
            "{} bytes after the last action",
            cur.remaining()
        )));
    }
    Ok(AtnSet {
        name,
        expanded,
        actions,
    })
}

fn read_action(cur: &mut Cursor<'_>) -> Result<AtnAction, ResourceError> {
    let function_key = cur.u16()?;
    let shift = cur.u8()? != 0;
    let command = cur.u8()? != 0;
    let color = cur.u16()?;
    let name = cur.unicode_string(MAX_ATN_NAME)?;
    let expanded = cur.u8()? != 0;
    let count = cur.u32()? as usize;
    check_count("step count", count, MAX_ENTRIES)?;
    let mut steps = Vec::with_capacity(count);
    for index in 0..count {
        steps.push(
            read_step(cur).map_err(|e| {
                ResourceError::Malformed(format!("{name}, step {}: {e}", index + 1))
            })?,
        );
    }
    Ok(AtnAction {
        name,
        function_key,
        shift,
        command,
        color,
        expanded,
        steps,
    })
}

fn read_step(cur: &mut Cursor<'_>) -> Result<AtnStep, ResourceError> {
    let expanded = cur.u8()? != 0;
    let enabled = cur.u8()? != 0;
    let with_dialog = cur.u8()? != 0;
    let dialog_options = cur.u8()?;
    let (event, char_id) = match &cur.tag()? {
        b"TEXT" => (ascii(cur, "event id length")?, false),
        b"long" => (String::from_utf8_lossy(&cur.tag()?).into_owned(), true),
        other => {
            return Err(ResourceError::Malformed(format!(
                "event id type {:?} is neither TEXT nor long",
                String::from_utf8_lossy(other)
            )))
        }
    };
    let name = ascii(cur, "step name length")?;
    let descriptor = match cur.i32()? {
        0 => None,
        -1 => {
            let version = cur.u32()?;
            if version != 16 {
                return Err(ResourceError::Unsupported {
                    what: "descriptor version",
                    detail: version.to_string(),
                });
            }
            Some(Descriptor::read(cur, &psd::ReadOptions::default())?)
        }
        other => {
            return Err(ResourceError::Malformed(format!(
                "descriptor flag {other} is neither -1 nor 0"
            )))
        }
    };
    Ok(AtnStep {
        expanded,
        enabled,
        with_dialog,
        dialog_options,
        event,
        char_id,
        name,
        descriptor,
    })
}

// ------------------------------------------------------------------ writing

/// Write `set` as an `.atn` file that [`parse`] (and Photoshop) reads.
pub fn write(set: &AtnSet) -> Result<Vec<u8>, ResourceError> {
    let mut s = Sink::new();
    s.u32(ATN_VERSION);
    s.unicode_string(&set.name);
    s.u8(u8::from(set.expanded));
    s.u32(set.actions.len() as u32);
    for action in &set.actions {
        s.u16(action.function_key);
        s.u8(u8::from(action.shift));
        s.u8(u8::from(action.command));
        s.u16(action.color);
        s.unicode_string(&action.name);
        s.u8(u8::from(action.expanded));
        s.u32(action.steps.len() as u32);
        for step in &action.steps {
            write_step(&mut s, step)?;
        }
    }
    Ok(s.into_inner())
}

fn write_step(s: &mut Sink, step: &AtnStep) -> Result<(), ResourceError> {
    s.u8(u8::from(step.expanded));
    s.u8(u8::from(step.enabled));
    s.u8(u8::from(step.with_dialog));
    s.u8(step.dialog_options);
    let fourcc: Option<[u8; 4]> = step.event.as_bytes().try_into().ok();
    match (step.char_id, fourcc) {
        (true, Some(code)) => {
            s.tag(b"long");
            s.tag(&code);
        }
        (true, None) => {
            return Err(ResourceError::Malformed(format!(
                "event id {:?} is not four characters",
                step.event
            )))
        }
        (false, _) => {
            s.tag(b"TEXT");
            s.u32(step.event.len() as u32);
            s.bytes(step.event.as_bytes());
        }
    }
    s.u32(step.name.len() as u32);
    s.bytes(step.name.as_bytes());
    match &step.descriptor {
        None => s.i32(0),
        Some(d) => {
            s.i32(-1);
            s.u32(16);
            d.write(s)?;
        }
    }
    Ok(())
}

// ------------------------------------------------------------ interpreting

/// A length a step gives in pixels or as a percentage of the current one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Length {
    Pixels(f64),
    Percent(f64),
}

impl Length {
    /// Resolve against the current length `of`, rounded, at least 1.
    pub fn resolve(self, of: u32) -> u32 {
        let v = match self {
            Length::Pixels(p) => p,
            Length::Percent(p) => f64::from(of) * p / 100.0,
        };
        if v.is_finite() {
            v.round().clamp(1.0, f64::from(u32::MAX)) as u32
        } else {
            of
        }
    }
}

/// What a Fill step fills with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FillWith {
    Foreground,
    Background,
    /// sRGB, `0.0..=1.0`.
    Rgb([f32; 3]),
}

/// A step this application can perform, with its parameters in Photoshop's
/// own units.
#[derive(Debug, Clone, PartialEq)]
pub enum StepOp {
    /// Layer ▸ New ▸ Layer.
    MakeLayer,
    SelectAll,
    Deselect,
    InverseSelection,
    /// A rectangular selection, pixel edges.
    SelectRect {
        left: f64,
        top: f64,
        right: f64,
        bottom: f64,
    },
    /// Edit ▸ Fill; `opacity` in `0.0..=1.0`.
    Fill {
        with: FillWith,
        opacity: f32,
    },
    /// Image ▸ Image Size. A missing side follows the other in proportion.
    ImageSize {
        width: Option<Length>,
        height: Option<Length>,
    },
    /// Image ▸ Canvas Size, anchored by `horizontal` / `vertical`
    /// (`0` start, `1` centre, `2` end); `relative` adds to the current size.
    CanvasSize {
        width: Option<Length>,
        height: Option<Length>,
        relative: bool,
        horizontal: u8,
        vertical: u8,
    },
    Invert,
    Desaturate,
    Equalize,
    /// Photoshop's ranges: brightness `-150..=150`, contrast `-50..=100`.
    BrightnessContrast {
        brightness: f64,
        contrast: f64,
    },
    /// Radius in pixels.
    GaussianBlur {
        radius: f64,
    },
    /// Amount in percent, radius in pixels, threshold in levels (`0..=255`).
    UnsharpMask {
        amount: f64,
        radius: f64,
        threshold: f64,
    },
    /// Radius in pixels.
    Median {
        radius: f64,
    },
    /// Image ▸ Image Rotation, degrees clockwise.
    RotateCanvas {
        degrees: f64,
    },
    FlipCanvas {
        horizontal: bool,
    },
    /// Edit ▸ Transform ▸ Rotate on the active layer, degrees clockwise.
    RotateLayer {
        degrees: f64,
    },
    FlipLayer {
        horizontal: bool,
    },
    /// File ▸ Save.
    Save,
    // ---- W16-H: appended; units are Photoshop's (see `atn_more`) ----------
    Levels(Vec<LevelsEntry>),
    Curves(Vec<CurvesEntry>),
    /// Hue `-180..=180` (colorize `0..=360`), saturation and lightness
    /// `-100..=100` (colorize saturation `0..=100`).
    HueSaturation {
        hue: f64,
        saturation: f64,
        lightness: f64,
        colorize: bool,
    },
    /// Cyan–red, magenta–green, yellow–blue, each `-100..=100`.
    ColorBalance {
        shadows: [f64; 3],
        midtones: [f64; 3],
        highlights: [f64; 3],
        preserve_luminosity: bool,
    },
    /// Reds, yellows, greens, cyans, blues, magentas in percent; the tint
    /// as `[r, g, b]` `0..=255`.
    BlackAndWhite {
        weights: [f64; 6],
        tint: Option<[f64; 3]>,
    },
    /// Both `-100..=100`.
    Vibrance {
        vibrance: f64,
        saturation: f64,
    },
    /// Stops, offset, gamma.
    Exposure {
        exposure: f64,
        offset: f64,
        gamma: f64,
    },
    /// Level `1..=255`.
    Threshold {
        level: f64,
    },
    Posterize {
        levels: u32,
    },
    /// Stops as `(location 0..=1, [r, g, b] 0..=255)`.
    GradientMap {
        stops: Vec<(f64, [f64; 3])>,
        reverse: bool,
    },
    /// Colour `[r, g, b]` `0..=255`, density in percent.
    PhotoFilter {
        color: [f64; 3],
        density: f64,
        preserve_luminosity: bool,
    },
    /// Each output row: red, green, blue, constant, in percent.
    ChannelMixer {
        red: [f64; 4],
        green: [f64; 4],
        blue: [f64; 4],
        monochrome: bool,
    },
    /// Image ▸ Crop to `[left, top, right, bottom]` of the current canvas
    /// (it may reach past it), or to the selection when `None`.
    Crop {
        rect: Option<[f64; 4]>,
    },
    Trim {
        basis: TrimBasis,
        top: bool,
        left: bool,
        bottom: bool,
        right: bool,
    },
    ConvertMode {
        mode: ModeTarget,
    },
    /// Image ▸ Mode ▸ 8 / 16 / 32 Bits/Channel.
    BitDepth {
        bits: u8,
    },
    /// Edit ▸ Free Transform (and the Move tool): offset in pixels, scale in
    /// percent, angle and skew in degrees clockwise, about `pivot`.
    Transform {
        target: TransformTarget,
        pivot: Pivot,
        offset: [f64; 2],
        scale: [f64; 2],
        angle: f64,
        skew: [f64; 2],
    },
    /// Layer ▸ Arrange (or a move to an index).
    ArrangeLayer(LayerRef),
    DuplicateLayer {
        name: Option<String>,
    },
    DeleteLayer,
    MergeDown,
    MergeVisible,
    Flatten,
    /// Layer ▸ New ▸ Group.
    MakeGroup,
    /// Layer ▸ Group Layers.
    GroupLayers,
    /// The active layer's name, opacity (percent) and blend mode.
    SetLayer {
        name: Option<String>,
        opacity: Option<f64>,
        blend: Option<layer_model::BlendMode>,
    },
    SetVisibility {
        visible: bool,
    },
    SelectLayer(LayerRef),
    /// Select ▸ Color Range over a sampled colour `[r, g, b]` `0..=255`.
    ColorRange {
        color: [f64; 3],
        fuzziness: f64,
        invert: bool,
    },
    Feather {
        radius: f64,
    },
    Expand {
        by: f64,
    },
    Contract {
        by: f64,
    },
    Border {
        width: f64,
    },
    Smooth {
        radius: f64,
    },
    /// Edit ▸ Stroke; `color` `None` strokes with the foreground colour.
    Stroke {
        width: f64,
        location: StrokeAt,
        opacity: f64,
        color: Option<[f64; 3]>,
        blend: layer_model::BlendMode,
    },
    Copy,
    CopyMerged,
    Paste,
    Cut,
    /// Layer ▸ New ▸ Layer via Copy / Cut.
    LayerVia {
        cut: bool,
    },
    /// Amount in percent.
    AddNoise {
        amount: f64,
        gaussian: bool,
        monochromatic: bool,
    },
    MotionBlur {
        angle: f64,
        distance: f64,
    },
    HighPass {
        radius: f64,
    },
    /// Amount and noise reduction in percent, radius in pixels.
    SmartSharpen {
        amount: f64,
        radius: f64,
        noise_reduction: f64,
    },
    /// File ▸ Save As / Export: the export dialog.
    Export,
}

/// The four-character code and the string id of each event [`interpret`]
/// knows; Photoshop writes either.
const EVENTS: &[(&str, &str)] = &[
    ("Mk  ", "make"),
    ("setd", "set"),
    ("Invs", "inverse"),
    ("Fl  ", "fill"),
    ("ImgS", "imageSize"),
    ("CnvS", "canvasSize"),
    ("Invr", "invert"),
    ("Dstt", "desaturate"),
    ("Eqlz", "equalize"),
    ("BrgC", "brightnessEvent"),
    ("GsnB", "gaussianBlur"),
    ("UnsM", "unsharpMask"),
    ("Mdn ", "median"),
    ("Rtte", "rotateEventEnum"),
    ("Flip", "flip"),
    ("save", "save"),
];

/// The step's event as its four-character code, when it is one [`EVENTS`]
/// knows under either spelling.
fn event_code(step: &AtnStep) -> Option<&'static str> {
    EVENTS
        .iter()
        .find(|(code, id)| {
            if step.char_id {
                *code == step.event
            } else {
                *id == step.event
            }
        })
        .map(|(code, _)| *code)
}

fn target_class(d: &Descriptor) -> Option<&str> {
    match d.get("null")? {
        Value::Reference(items) => items.first().map(|item| match item {
            RefItem::Property { class_id, .. }
            | RefItem::Class { class_id, .. }
            | RefItem::Enumerated { class_id, .. }
            | RefItem::Offset { class_id, .. }
            | RefItem::Name { class_id, .. } => class_id.as_str(),
            _ => "",
        }),
        _ => None,
    }
}

fn enum_value<'a>(d: &'a Descriptor, key: &str) -> Option<&'a str> {
    match d.get(key)? {
        Value::Enumerated { value, .. } => Some(value.as_str()),
        _ => None,
    }
}

fn length(d: &Descriptor, key: &str) -> Option<Length> {
    match d.get(key)? {
        Value::UnitFloat {
            unit: [b'#', b'P', b'r', b'c'],
            value,
        } => Some(Length::Percent(*value)),
        Value::UnitFloat { value, .. } | Value::Double(value) => Some(Length::Pixels(*value)),
        Value::Integer(v) => Some(Length::Pixels(f64::from(*v))),
        _ => None,
    }
}

fn anchor(d: &Descriptor, key: &str, start: &str, end: &str) -> u8 {
    match enum_value(d, key) {
        Some(v) if v == start => 0,
        Some(v) if v == end => 2,
        _ => 1,
    }
}

/// What `step` does here, or why it cannot be played.
pub fn interpret(step: &AtnStep) -> Result<StepOp, String> {
    if let Some(op) = more::interpret(step) {
        return op;
    }
    let unmapped = || {
        format!(
            "“{}” ({}) has no equivalent in this application",
            step.name, step.event
        )
    };
    let code = event_code(step).ok_or_else(unmapped)?;
    let empty = Descriptor::default();
    let d = step.descriptor.as_ref().unwrap_or(&empty);
    let target = target_class(d);
    let num = |key: &str| d.number(key);
    Ok(match code {
        "Mk  " => match target {
            Some("Lyr ") => StepOp::MakeLayer,
            _ => {
                return Err(format!(
                    "“{}” makes something other than a layer",
                    step.name
                ))
            }
        },
        "setd" => {
            if target != Some("Chnl") {
                return Err(format!(
                    "“{}” sets something other than the selection",
                    step.name
                ));
            }
            match d.get("T   ") {
                Some(Value::Enumerated { value, .. }) if value == "Al  " => StepOp::SelectAll,
                Some(Value::Enumerated { value, .. }) if value == "None" => StepOp::Deselect,
                Some(Value::Descriptor(r)) if r.class_id == "Rctn" => {
                    let edge = |k: &str| r.number(k).ok_or_else(|| format!("no {k} edge"));
                    StepOp::SelectRect {
                        left: edge("Left")?,
                        top: edge("Top ")?,
                        right: edge("Rght")?,
                        bottom: edge("Btom")?,
                    }
                }
                _ => {
                    return Err(format!(
                        "“{}” sets a selection shape other than all, none or a rectangle",
                        step.name
                    ))
                }
            }
        }
        "Invs" => StepOp::InverseSelection,
        "Fl  " => {
            let opacity = num("Opct").map_or(1.0, |p| (p / 100.0).clamp(0.0, 1.0) as f32);
            let with = match enum_value(d, "Usng") {
                None | Some("FrgC") => FillWith::Foreground,
                Some("BckC") => FillWith::Background,
                Some("Blck") => FillWith::Rgb([0.0; 3]),
                Some("Wht ") => FillWith::Rgb([1.0; 3]),
                Some("Gry ") => FillWith::Rgb([0.5; 3]),
                Some("Clr ") => {
                    let c = d
                        .descriptor("Clr ")
                        .filter(|c| c.class_id == "RGBC")
                        .ok_or_else(|| format!("“{}” fills with a non-RGB colour", step.name))?;
                    let ch = |k: &str| (c.number(k).unwrap_or(0.0) / 255.0).clamp(0.0, 1.0) as f32;
                    FillWith::Rgb([ch("Rd  "), ch("Grn "), ch("Bl  ")])
                }
                Some(other) => {
                    return Err(format!(
                        "“{}” fills with {other}, which has no equivalent here",
                        step.name
                    ))
                }
            };
            StepOp::Fill { with, opacity }
        }
        "ImgS" => {
            let (width, height) = (length(d, "Wdth"), length(d, "Hght"));
            if width.is_none() && height.is_none() {
                return Err(format!("“{}” changes only the resolution", step.name));
            }
            StepOp::ImageSize { width, height }
        }
        "CnvS" => StepOp::CanvasSize {
            width: length(d, "Wdth"),
            height: length(d, "Hght"),
            relative: matches!(d.get("Rltv"), Some(Value::Bool(true))),
            horizontal: anchor(d, "Hrzn", "Left", "Rght"),
            vertical: anchor(d, "Vrtc", "Top ", "Btom"),
        },
        "Invr" => StepOp::Invert,
        "Dstt" => StepOp::Desaturate,
        "Eqlz" => StepOp::Equalize,
        "BrgC" => StepOp::BrightnessContrast {
            brightness: num("Brgh").unwrap_or(0.0),
            contrast: num("Cntr").unwrap_or(0.0),
        },
        "GsnB" => StepOp::GaussianBlur {
            radius: num("Rds ").unwrap_or(1.0),
        },
        "UnsM" => StepOp::UnsharpMask {
            amount: num("Amnt").unwrap_or(50.0),
            radius: num("Rds ").unwrap_or(1.0),
            threshold: num("Thsh").unwrap_or(0.0),
        },
        "Mdn " => StepOp::Median {
            radius: num("Rds ").unwrap_or(1.0),
        },
        "Rtte" => {
            let degrees = num("Angl").ok_or_else(|| format!("“{}” has no angle", step.name))?;
            match target {
                Some("Dcmn") => StepOp::RotateCanvas { degrees },
                Some("Lyr ") => StepOp::RotateLayer { degrees },
                _ => {
                    return Err(format!(
                        "“{}” rotates something other than the canvas or a layer",
                        step.name
                    ))
                }
            }
        }
        "Flip" => {
            let horizontal = match enum_value(d, "Axis") {
                Some("Hrzn") => true,
                Some("Vrtc") => false,
                _ => return Err(format!("“{}” has no axis", step.name)),
            };
            match target {
                Some("Dcmn") => StepOp::FlipCanvas { horizontal },
                Some("Lyr ") => StepOp::FlipLayer { horizontal },
                _ => {
                    return Err(format!(
                        "“{}” flips something other than the canvas or a layer",
                        step.name
                    ))
                }
            }
        }
        "save" => StepOp::Save,
        _ => return Err(unmapped()),
    })
}

fn reference(class_id: &str, target: bool) -> Value {
    Value::Reference(vec![if target {
        RefItem::Enumerated {
            name: String::new(),
            class_id: class_id.to_string(),
            type_id: "Ordn".to_string(),
            value: "Trgt".to_string(),
        }
    } else {
        RefItem::Class {
            name: String::new(),
            class_id: class_id.to_string(),
        }
    }])
}

fn unit(unit: &[u8; 4], value: f64) -> Value {
    Value::UnitFloat { unit: *unit, value }
}

fn len_value(l: Length) -> Value {
    match l {
        Length::Pixels(v) => unit(b"#Pxl", v),
        Length::Percent(v) => unit(b"#Prc", v),
    }
}

fn enumerated(type_id: &str, value: &str) -> Value {
    Value::Enumerated {
        type_id: type_id.to_string(),
        value: value.to_string(),
    }
}

/// Build a descriptor from items whose keys are all four characters, which
/// [`Descriptor::push`] always accepts.
fn desc(class_id: &str, items: Vec<(&str, Value)>) -> Descriptor {
    let mut d = Descriptor::new(class_id);
    d.items = items.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    d
}

impl StepOp {
    /// This operation as the ATN step Photoshop would record for it;
    /// [`interpret`] of the result is `self`.
    pub fn to_step(&self) -> AtnStep {
        if let Some(step) = more::to_step(self) {
            return step;
        }
        let selection = || {
            Value::Reference(vec![RefItem::Property {
                name: String::new(),
                class_id: "Chnl".to_string(),
                key_id: "fsel".to_string(),
            }])
        };
        let (event, name, items): (&str, &str, Vec<(&str, Value)>) = match *self {
            StepOp::MakeLayer => ("Mk  ", "Make", vec![("null", reference("Lyr ", false))]),
            StepOp::SelectAll => (
                "setd",
                "Set Selection",
                vec![("null", selection()), ("T   ", enumerated("Ordn", "Al  "))],
            ),
            StepOp::Deselect => (
                "setd",
                "Set Selection",
                vec![("null", selection()), ("T   ", enumerated("Ordn", "None"))],
            ),
            StepOp::InverseSelection => ("Invs", "Inverse", vec![]),
            StepOp::SelectRect {
                left,
                top,
                right,
                bottom,
            } => (
                "setd",
                "Set Selection",
                vec![
                    ("null", selection()),
                    (
                        "T   ",
                        Value::Descriptor(desc(
                            "Rctn",
                            vec![
                                ("Top ", unit(b"#Pxl", top)),
                                ("Left", unit(b"#Pxl", left)),
                                ("Btom", unit(b"#Pxl", bottom)),
                                ("Rght", unit(b"#Pxl", right)),
                            ],
                        )),
                    ),
                ],
            ),
            StepOp::Fill { with, opacity } => {
                let mut items = Vec::new();
                match with {
                    FillWith::Foreground => items.push(("Usng", enumerated("FlCn", "FrgC"))),
                    FillWith::Background => items.push(("Usng", enumerated("FlCn", "BckC"))),
                    FillWith::Rgb(rgb) => {
                        items.push(("Usng", enumerated("FlCn", "Clr ")));
                        let ch = |v: f32| Value::Double(f64::from(v) * 255.0);
                        items.push((
                            "Clr ",
                            Value::Descriptor(desc(
                                "RGBC",
                                vec![
                                    ("Rd  ", ch(rgb[0])),
                                    ("Grn ", ch(rgb[1])),
                                    ("Bl  ", ch(rgb[2])),
                                ],
                            )),
                        ));
                    }
                }
                items.push(("Opct", unit(b"#Prc", f64::from(opacity) * 100.0)));
                items.push(("Md  ", enumerated("BlnM", "Nrml")));
                ("Fl  ", "Fill", items)
            }
            StepOp::ImageSize { width, height } => {
                let mut items = Vec::new();
                if let Some(w) = width {
                    items.push(("Wdth", len_value(w)));
                }
                if let Some(h) = height {
                    items.push(("Hght", len_value(h)));
                }
                ("ImgS", "Image Size", items)
            }
            StepOp::CanvasSize {
                width,
                height,
                relative,
                horizontal,
                vertical,
            } => {
                let mut items = vec![("Rltv", Value::Bool(relative))];
                if let Some(w) = width {
                    items.push(("Wdth", len_value(w)));
                }
                if let Some(h) = height {
                    items.push(("Hght", len_value(h)));
                }
                let h = ["Left", "Cntr", "Rght"][usize::from(horizontal.min(2))];
                let v = ["Top ", "Cntr", "Btom"][usize::from(vertical.min(2))];
                items.push(("Hrzn", enumerated("HrzL", h)));
                items.push(("Vrtc", enumerated("VrtL", v)));
                ("CnvS", "Canvas Size", items)
            }
            StepOp::Invert => ("Invr", "Invert", vec![]),
            StepOp::Desaturate => ("Dstt", "Desaturate", vec![]),
            StepOp::Equalize => ("Eqlz", "Equalize", vec![]),
            StepOp::BrightnessContrast {
                brightness,
                contrast,
            } => (
                "BrgC",
                "Brightness/Contrast",
                vec![
                    ("Brgh", Value::Double(brightness)),
                    ("Cntr", Value::Double(contrast)),
                ],
            ),
            StepOp::GaussianBlur { radius } => (
                "GsnB",
                "Gaussian Blur",
                vec![("Rds ", unit(b"#Pxl", radius))],
            ),
            StepOp::UnsharpMask {
                amount,
                radius,
                threshold,
            } => (
                "UnsM",
                "Unsharp Mask",
                vec![
                    ("Amnt", unit(b"#Prc", amount)),
                    ("Rds ", unit(b"#Pxl", radius)),
                    ("Thsh", Value::Double(threshold)),
                ],
            ),
            StepOp::Median { radius } => ("Mdn ", "Median", vec![("Rds ", unit(b"#Pxl", radius))]),
            StepOp::RotateCanvas { degrees } => (
                "Rtte",
                "Rotate",
                vec![
                    ("null", reference("Dcmn", true)),
                    ("Angl", unit(b"#Ang", degrees)),
                ],
            ),
            StepOp::RotateLayer { degrees } => (
                "Rtte",
                "Rotate",
                vec![
                    ("null", reference("Lyr ", true)),
                    ("Angl", unit(b"#Ang", degrees)),
                ],
            ),
            StepOp::FlipCanvas { horizontal } => (
                "Flip",
                "Flip",
                vec![
                    ("null", reference("Dcmn", true)),
                    (
                        "Axis",
                        enumerated("Ornt", if horizontal { "Hrzn" } else { "Vrtc" }),
                    ),
                ],
            ),
            StepOp::FlipLayer { horizontal } => (
                "Flip",
                "Flip",
                vec![
                    ("null", reference("Lyr ", true)),
                    (
                        "Axis",
                        enumerated("Ornt", if horizontal { "Hrzn" } else { "Vrtc" }),
                    ),
                ],
            ),
            StepOp::Save => ("save", "Save", vec![]),
            _ => unreachable!("every W16-H operation is written by atn_more::to_step"),
        };
        let descriptor = (!items.is_empty()).then(|| desc("null", items));
        AtnStep::new(event, name, descriptor)
    }
}

#[cfg(test)]
#[path = "atn_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "atn_w16_tests.rs"]
mod w16_tests;
