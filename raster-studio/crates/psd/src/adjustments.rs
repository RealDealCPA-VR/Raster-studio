//! W11-A: adjustment-layer payloads, read into and written from
//! [`layer_model::AdjustmentKind`].
//!
//! Every layout here follows the "Additional Layer Information" section of
//! Adobe's Photoshop File Format Specification. Integers are big-endian;
//! percentages are stored as whole numbers and become `value / 100` in the
//! model; 8-bit levels become `value / 255`.
//!
//! | key    | layout read and written |
//! |--------|-------------------------|
//! | `nvrt` | no data |
//! | `levl` | `u16` version 2, then up to 29 records of five `i16`: input floor, input ceiling, output floor, output ceiling, gamma × 100. Record 0 is the composite, 1–3 red, green, blue |
//! | `curv` | `u8` map flag (0), `u16` version (1 or 4), `u32` channel bitmap (version 1) or curve count (version 4, curves in channel order), then per curve a `u16` point count and that many (`i16` output, `i16` input) pairs. Channel 0 is the composite, 1–3 red, green, blue. Written as version 1 |
//! | `brit` | `i16` brightness, `i16` contrast, `i16` mean, `u8` Lab-only |
//! | `hue2` | `u16` version 2, `u8` colorize, `u8` pad, colorize hue/sat/light, master hue/sat/light (`i16`), six ranges of four `i16` bounds and three `i16` settings |
//! | `blnc` | shadows, midtones, highlights as three `i16` each (cyan–red, magenta–green, yellow–blue), `u8` preserve luminosity |
//! | `blwh` | `u32` 16 + descriptor: `Rd  ` `Yllw` `Grn ` `Cyn ` `Bl  ` `Mgnt` (percent), `useTint`, `tintColor` (`RGBC`) |
//! | `phfl` | `u16` version 2, colour (`u16` space + four `u16`), `u32` density percent, `u8` preserve luminosity |
//! | `mixr` | `u16` version 1, `u16` monochrome, per output channel five `i16` (red, green, blue, unused, constant) percent |
//! | `post` | `u16` levels (2–255), `u16` unused |
//! | `thrs` | `u16` level (1–255), `u16` unused |
//! | `grdm` | `u16` version 1, `u8` reverse, `u8` dither, Unicode name, `u16` colour-stop count and stops (`u32` location 0–4096, `u32` midpoint, colour, `u16` pad), `u16` transparency-stop count and stops, then the noise-gradient tail |
//! | `selc` | `u16` version 1, `u16` method (0 relative, 1 absolute), ten records of four `i16` (the first reserved) |
//! | `expA` | `u16` version 1, `f32` exposure, `f32` offset, `f32` gamma |
//! | `vibA` | `u32` 16 + descriptor: `vibrance`, `Strt` (percent) |
//! | `clrL` | `u16` version 1, `u32` 16 + descriptor; the embedded `.cube` text in `LUT3DFileData` |
//!
//! Decoding is bounded: every read goes through [`Cursor`], which returns an
//! error rather than panicking at the end of the payload, every count is
//! checked against the bytes that remain before anything is reserved, and a
//! value outside its documented range is refused by name. A caller gets
//! either a [`Decoded`] (with the settings the model has no room for listed in
//! [`Decoded::unmapped`]) or a [`PayloadError`] to put in its report.
//!
//! Encoding never clamps a setting into range. A value the layout cannot
//! spell (non-finite, outside the range the decoder accepts, or curve inputs
//! that collide once quantised to 1/255) makes [`encode`] return a
//! [`PayloadError`] naming it, so the caller reports it instead of saving a
//! different value. Every payload [`encode`] returns decodes again.
//!
//! These layouts are written from Adobe's published specification and are
//! verified only by round trips through this module; they have not been
//! checked against files written by Photoshop or Photopea.

use layer_model::AdjustmentKind;

use crate::bytes::{Cursor, Sink};
use crate::descriptor::{Descriptor, Value};
use crate::error::tag_name;
use crate::limits::ReadOptions;
use crate::model::Adjustment;

/// An adjustment payload decoded into the document model.
#[derive(Debug, Clone, PartialEq)]
pub struct Decoded {
    pub kind: AdjustmentKind,
    /// Settings the file carries that the model cannot hold, by name, e.g.
    /// `"per-colour-range settings"`. Empty when the mapping is exact.
    pub unmapped: Vec<String>,
}

/// Why a payload could not be decoded (or a kind could not be encoded).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadError {
    /// The four-character key, printable.
    pub key: String,
    pub reason: String,
}

impl std::fmt::Display for PayloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.key, self.reason)
    }
}

impl std::error::Error for PayloadError {}

type R<T> = Result<T, String>;

fn fail<T>(reason: impl Into<String>) -> R<T> {
    Err(reason.into())
}

/// Read one field; a truncated payload is reported by what was being read.
fn trunc(what: &str) -> impl FnOnce(crate::error::PsdError) -> String + '_ {
    move |e| format!("{what}: {e}")
}

fn ranged(what: &str, v: i32, min: i32, max: i32) -> R<i32> {
    if v < min || v > max {
        fail(format!("{what} is {v}, outside {min}..={max}"))
    } else {
        Ok(v)
    }
}

/// Decode an adjustment layer's payload.
///
/// Fill keys (`SoCo`, `GdFl`, `PtFl`) are not adjustments and are refused
/// here; [`crate::fill`] reads them.
pub fn decode(adjustment: &Adjustment, opts: &ReadOptions) -> Result<Decoded, PayloadError> {
    let data = adjustment.data.as_slice();
    let mut unmapped = Vec::new();
    let result = match &adjustment.key {
        b"nvrt" => Ok(AdjustmentKind::Invert),
        b"levl" => levels(data),
        b"curv" => curves(data, &mut unmapped),
        b"brit" => brightness_contrast(data),
        b"hue2" => hue_saturation(data, &mut unmapped),
        b"blnc" => color_balance(data),
        b"blwh" => black_and_white(data, opts),
        b"phfl" => photo_filter(data),
        b"mixr" => channel_mixer(data),
        b"post" => posterize(data),
        b"thrs" => threshold(data),
        b"grdm" => gradient_map(data, &mut unmapped),
        b"selc" => selective_color(data),
        b"expA" => exposure(data),
        b"vibA" => vibrance(data, opts),
        b"clrL" => color_lookup(data, opts),
        _ => fail("not an adjustment this build decodes"),
    };
    result
        .map(|kind| Decoded { kind, unmapped })
        .map_err(|reason| PayloadError {
            key: tag_name(adjustment.key),
            reason,
        })
}

/// Encode a model adjustment as its `.psd` payload.
///
/// Refuses the kinds a `.psd` has no adjustment layer for (Auto, Desaturate,
/// Equalize, Shadows/Highlights, Replace Color, HDR Toning, Match Color) and
/// settings its layout cannot spell.
pub fn encode(kind: &AdjustmentKind) -> Result<Adjustment, PayloadError> {
    let (key, data): ([u8; 4], R<Vec<u8>>) = match kind {
        AdjustmentKind::Invert => (*b"nvrt", Ok(Vec::new())),
        AdjustmentKind::Levels {
            black,
            white,
            gamma,
        } => (
            *b"levl",
            write_levels(&[
                [*black, *white, *gamma, 0.0, 1.0],
                IDENTITY_LEVELS,
                IDENTITY_LEVELS,
                IDENTITY_LEVELS,
            ]),
        ),
        AdjustmentKind::LevelsFull {
            composite,
            red,
            green,
            blue,
        } => (*b"levl", write_levels(&[*composite, *red, *green, *blue])),
        AdjustmentKind::Curves { points } => (*b"curv", write_curves([points, &[], &[], &[]])),
        AdjustmentKind::CurvesFull {
            composite,
            red,
            green,
            blue,
        } => (*b"curv", write_curves([composite, red, green, blue])),
        AdjustmentKind::BrightnessContrast {
            brightness,
            contrast,
        } => (*b"brit", write_brightness_contrast(*brightness, *contrast)),
        AdjustmentKind::HueSaturation {
            hue,
            saturation,
            lightness,
        } => (
            *b"hue2",
            write_hue_saturation(*hue, *saturation, *lightness, None),
        ),
        AdjustmentKind::HueSaturationFull {
            hue,
            saturation,
            lightness,
            colorize,
        } => (
            *b"hue2",
            write_hue_saturation(*hue, *saturation, *lightness, *colorize),
        ),
        AdjustmentKind::ColorBalance {
            shadows,
            midtones,
            highlights,
        } => (
            *b"blnc",
            write_color_balance([shadows, midtones, highlights], false),
        ),
        AdjustmentKind::ColorBalanceFull {
            shadows,
            midtones,
            highlights,
            preserve_luminosity,
        } => (
            *b"blnc",
            write_color_balance([shadows, midtones, highlights], *preserve_luminosity),
        ),
        AdjustmentKind::BlackAndWhite { weights, tint } => {
            (*b"blwh", write_black_and_white(weights, *tint))
        }
        AdjustmentKind::PhotoFilter {
            color_srgb,
            density,
            preserve_luminosity,
        } => (
            *b"phfl",
            write_photo_filter(*color_srgb, *density, *preserve_luminosity),
        ),
        AdjustmentKind::ChannelMixer { rows, monochrome } => {
            (*b"mixr", write_channel_mixer(rows, *monochrome))
        }
        AdjustmentKind::Posterize { levels } => {
            let data = if (2..=255).contains(levels) {
                let mut s = Sink::new();
                s.u16(*levels as u16);
                s.u16(0);
                Ok(s.into_inner())
            } else {
                fail(format!(
                    "posterize levels {levels} is outside what a .psd stores (2..=255)"
                ))
            };
            (*b"post", data)
        }
        AdjustmentKind::Threshold { level } => {
            let data = spell("threshold level", *level, 255.0, 1, 255).map(|v| {
                let mut s = Sink::new();
                s.u16(v as u16);
                s.u16(0);
                s.into_inner()
            });
            (*b"thrs", data)
        }
        AdjustmentKind::GradientMap { stops, reverse } => {
            (*b"grdm", write_gradient_map(stops, *reverse))
        }
        AdjustmentKind::SelectiveColor { ranges, relative } => {
            (*b"selc", write_selective_color(ranges, *relative))
        }
        AdjustmentKind::Exposure { stops } => (*b"expA", write_exposure(*stops, 0.0, 1.0)),
        AdjustmentKind::ExposureFull {
            stops,
            offset,
            gamma,
        } => (*b"expA", write_exposure(*stops, *offset, *gamma)),
        AdjustmentKind::Vibrance {
            vibrance,
            saturation,
        } => (*b"vibA", write_vibrance(*vibrance, *saturation)),
        AdjustmentKind::ColorLookup { name, size, table } => {
            (*b"clrL", write_color_lookup(name, *size, table))
        }
        AdjustmentKind::Auto { .. } => return Err(no_equivalent("Auto Tone/Contrast/Color")),
        AdjustmentKind::Desaturate => return Err(no_equivalent("Desaturate")),
        AdjustmentKind::Equalize => return Err(no_equivalent("Equalize")),
        AdjustmentKind::ShadowsHighlights { .. } => {
            return Err(no_equivalent("Shadows/Highlights"))
        }
        AdjustmentKind::ReplaceColor { .. } => return Err(no_equivalent("Replace Color")),
        AdjustmentKind::HdrToning { .. } => return Err(no_equivalent("HDR Toning")),
        AdjustmentKind::MatchColor { .. } => return Err(no_equivalent("Match Color")),
    };
    data.map(|data| Adjustment { key, data })
        .map_err(|reason| PayloadError {
            key: tag_name(key),
            reason,
        })
}

fn no_equivalent(name: &str) -> PayloadError {
    PayloadError {
        key: name.to_string(),
        reason: "a .psd has no adjustment layer for it".into(),
    }
}

/// `round(v * scale)`, or an error naming `what` when `v` is not finite or
/// lands outside `min..=max`: an encoder never clamps a value it cannot
/// store.
fn spell(what: &str, v: f32, scale: f32, min: i32, max: i32) -> R<i32> {
    if !v.is_finite() {
        return fail(format!("{what} is not a finite number"));
    }
    let q = (f64::from(v) * f64::from(scale)).round();
    if q < f64::from(min) || q > f64::from(max) {
        return fail(format!(
            "{what} {v} is outside what a .psd stores ({min}..={max} at 1/{scale})"
        ));
    }
    Ok(q as i32)
}

/// A percentage as a whole number within `min..=max`.
fn pct(what: &str, v: f32, min: i32, max: i32) -> R<i16> {
    spell(what, v, 100.0, min, max).map(|v| v as i16)
}

// ---------------------------------------------------------------- levels

const IDENTITY_LEVELS: [f32; 5] = [0.0, 1.0, 1.0, 0.0, 1.0];
const LEVEL_RECORDS: usize = 29;

fn levels(data: &[u8]) -> R<AdjustmentKind> {
    let mut c = Cursor::new(data);
    let version = c.u16().map_err(trunc("levels version"))?;
    if version != 2 {
        return fail(format!("levels version {version} (expected 2)"));
    }
    let available = (c.remaining() / 10).min(LEVEL_RECORDS);
    if available < 4 {
        return fail(format!(
            "levels carries {available} channel records; RGB needs 4"
        ));
    }
    let mut records = [IDENTITY_LEVELS; 4];
    for record in &mut records {
        let floor = ranged(
            "input floor",
            c.i16().map_err(trunc("levels"))?.into(),
            0,
            255,
        )?;
        let ceiling = ranged(
            "input ceiling",
            c.i16().map_err(trunc("levels"))?.into(),
            0,
            255,
        )?;
        let out_floor = ranged(
            "output floor",
            c.i16().map_err(trunc("levels"))?.into(),
            0,
            255,
        )?;
        let out_ceiling = ranged(
            "output ceiling",
            c.i16().map_err(trunc("levels"))?.into(),
            0,
            255,
        )?;
        let gamma = ranged("gamma", c.i16().map_err(trunc("levels"))?.into(), 1, 999)?;
        if floor >= ceiling {
            return fail(format!(
                "input floor {floor} is not below ceiling {ceiling}"
            ));
        }
        *record = [
            floor as f32 / 255.0,
            ceiling as f32 / 255.0,
            gamma as f32 / 100.0,
            out_floor as f32 / 255.0,
            out_ceiling as f32 / 255.0,
        ];
    }
    let [composite, red, green, blue] = records;
    let narrow = [red, green, blue].iter().all(|r| *r == IDENTITY_LEVELS)
        && composite[3] == 0.0
        && composite[4] == 1.0;
    Ok(if narrow {
        AdjustmentKind::Levels {
            black: composite[0],
            white: composite[1],
            gamma: composite[2],
        }
    } else {
        AdjustmentKind::LevelsFull {
            composite,
            red,
            green,
            blue,
        }
    })
}

fn write_levels(records: &[[f32; 5]; 4]) -> R<Vec<u8>> {
    let mut s = Sink::new();
    s.u16(2);
    for i in 0..LEVEL_RECORDS {
        match records.get(i) {
            Some(r) => {
                let floor = spell("levels input floor", r[0], 255.0, 0, 255)?;
                let ceiling = spell("levels input ceiling", r[1], 255.0, 0, 255)?;
                if floor >= ceiling {
                    return fail(format!(
                        "levels input floor {floor} is not below ceiling {ceiling}"
                    ));
                }
                s.i16(floor as i16);
                s.i16(ceiling as i16);
                s.i16(spell("levels output floor", r[3], 255.0, 0, 255)? as i16);
                s.i16(spell("levels output ceiling", r[4], 255.0, 0, 255)? as i16);
                s.i16(spell("levels gamma", r[2], 100.0, 1, 999)? as i16);
            }
            // The specification reserves the last two sets as zeros.
            None if i >= LEVEL_RECORDS - 2 => s.zeros(10),
            None => {
                for v in [0, 255, 0, 255, 100] {
                    s.i16(v);
                }
            }
        }
    }
    Ok(s.into_inner())
}

// ---------------------------------------------------------------- curves

const MAX_CURVE_POINTS: usize = 19;

fn identity_curve(points: &[[f32; 2]]) -> bool {
    points.is_empty() || points == [[0.0, 0.0], [1.0, 1.0]]
}

fn curves(data: &[u8], unmapped: &mut Vec<String>) -> R<AdjustmentKind> {
    let mut c = Cursor::new(data);
    let map = c.u8().map_err(trunc("curves"))?;
    let version = c.u16().map_err(trunc("curves version"))?;
    if version != 1 && version != 4 {
        return fail(format!("curves version {version} (expected 1 or 4)"));
    }
    if map != 0 {
        return fail("curves stored as 256-entry maps are not decoded");
    }
    // Version 1 carries a channel bitmap; version 4 a curve count, the
    // curves then following in channel order (as psd-tools reads it).
    let header = c.u32().map_err(trunc("curves channel bitmap"))?;
    let present: Vec<usize> = if version == 1 {
        (0..32).filter(|ch| header & (1 << ch) != 0).collect()
    } else {
        // Every curve needs at least its two-byte point count.
        if header as usize > c.remaining() / 2 {
            return fail(format!(
                "curves declares {header} curves in {} bytes",
                c.remaining()
            ));
        }
        (0..header as usize).collect()
    };
    let mut channels: [Vec<[f32; 2]>; 4] = Default::default();
    let mut ignored = false;
    for channel in present {
        let count = usize::from(c.u16().map_err(trunc("curve point count"))?);
        if !(2..=MAX_CURVE_POINTS).contains(&count) {
            return fail(format!(
                "a curve has {count} points (expected 2..={MAX_CURVE_POINTS})"
            ));
        }
        let mut points = Vec::with_capacity(count);
        for _ in 0..count {
            let output = ranged(
                "curve output",
                c.i16().map_err(trunc("curve point"))?.into(),
                0,
                255,
            )?;
            let input = ranged(
                "curve input",
                c.i16().map_err(trunc("curve point"))?.into(),
                0,
                255,
            )?;
            points.push([input as f32 / 255.0, output as f32 / 255.0]);
        }
        if points.windows(2).any(|w| w[1][0] <= w[0][0]) {
            return fail("curve inputs are not strictly increasing");
        }
        match channels.get_mut(channel) {
            Some(slot) => *slot = points,
            None => ignored = true,
        }
    }
    if ignored {
        unmapped.push("curves for channels beyond red, green and blue".into());
    }
    let [composite, red, green, blue] = channels.map(|p| {
        if p.is_empty() {
            vec![[0.0, 0.0], [1.0, 1.0]]
        } else {
            p
        }
    });
    Ok(
        if identity_curve(&red) && identity_curve(&green) && identity_curve(&blue) {
            AdjustmentKind::Curves { points: composite }
        } else {
            AdjustmentKind::CurvesFull {
                composite,
                red,
                green,
                blue,
            }
        },
    )
}

fn write_curves(channels: [&[[f32; 2]]; 4]) -> R<Vec<u8>> {
    let mut bitmap = 0u32;
    let mut body = Sink::new();
    for (i, points) in channels.iter().enumerate() {
        // The composite always goes out; a channel only when it bends.
        if i > 0 && identity_curve(points) {
            continue;
        }
        let points: Vec<[f32; 2]> = if points.is_empty() {
            vec![[0.0, 0.0], [1.0, 1.0]]
        } else {
            points.to_vec()
        };
        if points.len() > MAX_CURVE_POINTS {
            return fail(format!(
                "a curve has {} points; a .psd holds at most {MAX_CURVE_POINTS}",
                points.len()
            ));
        }
        if points.len() < 2 {
            return fail("a curve needs at least two points");
        }
        bitmap |= 1 << i;
        body.u16(points.len() as u16);
        let mut last_input = -1;
        for [x, y] in points {
            let input = spell("curve input", x, 255.0, 0, 255)?;
            let output = spell("curve output", y, 255.0, 0, 255)?;
            // The decoder refuses inputs that are not strictly increasing;
            // points closer than 1/255 collide once quantised.
            if input <= last_input {
                return fail(format!(
                    "curve inputs collide at {input}/255 once stored in 1/255 steps"
                ));
            }
            last_input = input;
            body.i16(output as i16);
            body.i16(input as i16);
        }
    }
    let mut s = Sink::new();
    s.u8(0);
    s.u16(1);
    s.u32(bitmap);
    s.bytes(body.as_slice());
    Ok(s.into_inner())
}

// ---------------------------------------------------- brightness/contrast

fn brightness_contrast(data: &[u8]) -> R<AdjustmentKind> {
    let mut c = Cursor::new(data);
    let b = ranged(
        "brightness",
        c.i16().map_err(trunc("brightness"))?.into(),
        -150,
        150,
    )?;
    let k = ranged(
        "contrast",
        c.i16().map_err(trunc("contrast"))?.into(),
        -100,
        100,
    )?;
    Ok(AdjustmentKind::BrightnessContrast {
        brightness: b as f32 / 255.0,
        contrast: k as f32 / 100.0,
    })
}

fn write_brightness_contrast(brightness: f32, contrast: f32) -> R<Vec<u8>> {
    let mut s = Sink::new();
    s.i16(spell("brightness", brightness, 255.0, -150, 150)? as i16);
    s.i16(pct("contrast", contrast, -100, 100)?);
    s.i16(127);
    s.u8(0);
    Ok(s.into_inner())
}

// ---------------------------------------------------------- hue/saturation

/// Photoshop's default range bounds for reds, yellows, greens, cyans, blues
/// and magentas.
const HUE_RANGES: [[i16; 4]; 6] = [
    [315, 345, 15, 45],
    [15, 45, 75, 105],
    [75, 105, 135, 165],
    [135, 165, 195, 225],
    [195, 225, 255, 285],
    [255, 285, 315, 345],
];

fn hue_saturation(data: &[u8], unmapped: &mut Vec<String>) -> R<AdjustmentKind> {
    let mut c = Cursor::new(data);
    let version = c.u16().map_err(trunc("hue/saturation version"))?;
    if version != 2 {
        return fail(format!("hue/saturation version {version} (expected 2)"));
    }
    let colorize = c.u8().map_err(trunc("hue/saturation"))? != 0;
    c.skip(1).map_err(trunc("hue/saturation"))?;
    let mut rd = |what: &str, min: i32, max: i32| -> R<i32> {
        ranged(
            what,
            c.i16().map_err(trunc("hue/saturation"))?.into(),
            min,
            max,
        )
    };
    let ch = rd("colorize hue", -360, 360)?;
    let cs = rd("colorize saturation", 0, 100)?;
    let cl = rd("colorize lightness", -100, 100)?;
    let h = rd("hue", -180, 180)?;
    let s = rd("saturation", -100, 100)?;
    let l = rd("lightness", -100, 100)?;
    // The six per-range records are optional here: the master settings are
    // what the model holds, and a nonzero range setting is reported.
    let mut ranged_settings = false;
    for _ in 0..6 {
        if c.remaining() < 14 {
            break;
        }
        c.skip(8).map_err(trunc("hue/saturation range"))?;
        for _ in 0..3 {
            if c.i16().map_err(trunc("hue/saturation range"))? != 0 {
                ranged_settings = true;
            }
        }
    }
    if ranged_settings && !colorize {
        unmapped.push("per-colour-range hue/saturation settings".into());
    }
    let (hue, saturation, lightness) = (h as f32, s as f32 / 100.0, l as f32 / 100.0);
    Ok(if colorize {
        AdjustmentKind::HueSaturationFull {
            hue,
            saturation,
            lightness,
            colorize: Some([
                (ch as f32).rem_euclid(360.0),
                cs as f32 / 100.0,
                cl as f32 / 100.0,
            ]),
        }
    } else {
        AdjustmentKind::HueSaturation {
            hue,
            saturation,
            lightness,
        }
    })
}

fn write_hue_saturation(
    hue: f32,
    saturation: f32,
    lightness: f32,
    colorize: Option<[f32; 3]>,
) -> R<Vec<u8>> {
    let mut s = Sink::new();
    s.u16(2);
    s.u8(u8::from(colorize.is_some()));
    s.u8(0);
    let [ch, cs, cl] = colorize.unwrap_or([0.0, 0.25, 0.0]);
    // Hues are angles: wrapping one changes nothing, so it is not refused.
    let ch = spell("colorize hue", ch, 1.0, -100_000, 100_000)?.rem_euclid(360);
    s.i16(ch as i16);
    s.i16(pct("colorize saturation", cs, 0, 100)?);
    s.i16(pct("colorize lightness", cl, -100, 100)?);
    // Photoshop's hue slider runs -180..=180: wrap rather than clamp.
    let wrapped = (spell("hue", hue, 1.0, -100_000, 100_000)? + 180).rem_euclid(360) - 180;
    s.i16(wrapped as i16);
    s.i16(pct("saturation", saturation, -100, 100)?);
    s.i16(pct("lightness", lightness, -100, 100)?);
    for range in HUE_RANGES {
        for v in range {
            s.i16(v);
        }
        s.zeros(6);
    }
    Ok(s.into_inner())
}

// ----------------------------------------------------------- colour balance

fn color_balance(data: &[u8]) -> R<AdjustmentKind> {
    let mut c = Cursor::new(data);
    let mut bands = [[0.0f32; 3]; 3];
    for band in &mut bands {
        for v in band.iter_mut() {
            *v = ranged(
                "colour balance",
                c.i16().map_err(trunc("colour balance"))?.into(),
                -100,
                100,
            )? as f32
                / 100.0;
        }
    }
    // The preserve-luminosity byte is optional in old files; Photoshop's
    // default is on.
    let preserve = c.u8().map(|b| b != 0).unwrap_or(true);
    let [shadows, midtones, highlights] = bands;
    Ok(if preserve {
        AdjustmentKind::ColorBalanceFull {
            shadows,
            midtones,
            highlights,
            preserve_luminosity: true,
        }
    } else {
        AdjustmentKind::ColorBalance {
            shadows,
            midtones,
            highlights,
        }
    })
}

fn write_color_balance(bands: [&[f32; 3]; 3], preserve: bool) -> R<Vec<u8>> {
    let mut s = Sink::new();
    for band in bands {
        for v in band {
            s.i16(pct("colour balance", *v, -100, 100)?);
        }
    }
    s.u8(u8::from(preserve));
    Ok(s.into_inner())
}

// ---------------------------------------------------------- HSL helpers

fn rgb_to_hsl([r, g, b]: [f64; 3]) -> [f64; 3] {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d <= f64::EPSILON {
        return [0.0, 0.0, l];
    }
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    [h * 60.0, s, l]
}

fn hsl_to_rgb([h, s, l]: [f64; 3]) -> [f64; 3] {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (hp.rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    [r + m, g + m, b + m]
}

// ---------------------------------------------------------- black & white

const BW_KEYS: [&str; 6] = ["Rd  ", "Yllw", "Grn ", "Cyn ", "Bl  ", "Mgnt"];

fn versioned_descriptor(data: &[u8], opts: &ReadOptions, what: &str) -> R<Descriptor> {
    let mut c = Cursor::new(data);
    let version = c.u32().map_err(trunc(what))?;
    if version != 16 {
        return fail(format!("{what} descriptor version {version} (expected 16)"));
    }
    Descriptor::read(&mut c, opts).map_err(|e| format!("{what}: {e}"))
}

fn finite_number(d: &Descriptor, key: &str) -> R<Option<f64>> {
    match d.number(key) {
        Some(v) if !v.is_finite() => fail(format!("{} is not a finite number", key.trim_end())),
        other => Ok(other),
    }
}

fn black_and_white(data: &[u8], opts: &ReadOptions) -> R<AdjustmentKind> {
    let d = versioned_descriptor(data, opts, "black & white")?;
    let mut weights = [0.0f32; 6];
    for (w, key) in weights.iter_mut().zip(BW_KEYS) {
        let v = finite_number(&d, key)?
            .ok_or_else(|| format!("the {} weight is missing", key.trim_end()))?;
        if !(-300.0..=300.0).contains(&v) {
            return fail(format!("the {} weight is {v}%", key.trim_end()));
        }
        *w = (v / 100.0) as f32;
    }
    let use_tint = matches!(d.get("useTint"), Some(Value::Bool(true)));
    let tint = if use_tint {
        let c = d
            .descriptor("tintColor")
            .ok_or("the tint colour is missing")?;
        let mut rgb = [0.0f64; 3];
        for (v, key) in rgb.iter_mut().zip(["Rd  ", "Grn ", "Bl  "]) {
            *v = (finite_number(c, key)?.ok_or("the tint colour is incomplete")? / 255.0)
                .clamp(0.0, 1.0);
        }
        let [h, s, _] = rgb_to_hsl(rgb);
        Some([h as f32, s as f32])
    } else {
        None
    };
    Ok(AdjustmentKind::BlackAndWhite { weights, tint })
}

fn write_descriptor(version_u16: Option<u16>, d: &Descriptor) -> R<Vec<u8>> {
    let mut s = Sink::new();
    if let Some(v) = version_u16 {
        s.u16(v);
    }
    s.u32(16);
    d.write(&mut s).map_err(|e| e.to_string())?;
    Ok(s.into_inner())
}

fn push(d: &mut Descriptor, key: &str, value: Value) -> R<()> {
    d.push(key, value).map_err(|e| e.to_string())
}

fn write_black_and_white(weights: &[f32; 6], tint: Option<[f32; 2]>) -> R<Vec<u8>> {
    let mut d = Descriptor::new("null");
    for (w, key) in weights.iter().zip(BW_KEYS) {
        push(
            &mut d,
            key,
            Value::Integer(spell(
                &format!("the {} weight", key.trim_end()),
                *w,
                100.0,
                -200,
                300,
            )?),
        )?;
    }
    push(&mut d, "useTint", Value::Bool(tint.is_some()))?;
    let [h, s] = tint.unwrap_or([42.0, 0.2]);
    if !h.is_finite() || !(0.0..=1.0).contains(&s) {
        return fail(format!(
            "the tint hue {h} / saturation {s} is not a colour a .psd stores"
        ));
    }
    let rgb = hsl_to_rgb([f64::from(h), f64::from(s), 0.5]);
    let mut color = Descriptor::new("RGBC");
    for (v, key) in rgb.iter().zip(["Rd  ", "Grn ", "Bl  "]) {
        push(&mut color, key, Value::Double(v * 255.0))?;
    }
    push(&mut d, "tintColor", Value::Descriptor(color))?;
    push(&mut d, "bwPresetKind", Value::Integer(1))?;
    write_descriptor(None, &d)
}

// ------------------------------------------------------------ photo filter

/// A `.psd` colour structure (`u16` space + four `u16`) as sRGB `0..=1`.
fn read_color(c: &mut Cursor<'_>) -> R<[f32; 3]> {
    let space = c.u16().map_err(trunc("colour space"))?;
    let mut v = [0u16; 4];
    for x in &mut v {
        *x = c.u16().map_err(trunc("colour"))?;
    }
    let unit = |x: u16| f64::from(x) / 65535.0;
    let rgb = match space {
        0 => [unit(v[0]), unit(v[1]), unit(v[2])],
        1 => hsb_to_rgb(unit(v[0]) * 360.0, unit(v[1]), unit(v[2])),
        8 => {
            let g = 1.0 - (f64::from(v[0]) / 10000.0).clamp(0.0, 1.0);
            [g, g, g]
        }
        other => {
            return fail(format!(
                "colour space {other} is not decoded (RGB, HSB and grey are)"
            ))
        }
    };
    Ok(rgb.map(|x| x.clamp(0.0, 1.0) as f32))
}

fn hsb_to_rgb(h: f64, s: f64, v: f64) -> [f64; 3] {
    let c = v * s;
    let hp = h.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (hp.rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [r + m, g + m, b + m]
}

fn write_rgb(s: &mut Sink, rgb: [f32; 3]) -> R<()> {
    s.u16(0);
    for v in rgb {
        s.u16(spell("colour component", v, 65535.0, 0, 65535)? as u16);
    }
    s.u16(0);
    Ok(())
}

fn photo_filter(data: &[u8]) -> R<AdjustmentKind> {
    let mut c = Cursor::new(data);
    let version = c.u16().map_err(trunc("photo filter version"))?;
    let color_srgb = match version {
        2 => read_color(&mut c)?,
        3 => return fail("photo filter version 3 stores its colour as XYZ, which is not decoded"),
        other => return fail(format!("photo filter version {other} (expected 2)")),
    };
    let density = c.u32().map_err(trunc("photo filter density"))?;
    if density > 100 {
        return fail(format!("photo filter density is {density}%"));
    }
    let preserve = c.u8().map_err(trunc("photo filter"))? != 0;
    Ok(AdjustmentKind::PhotoFilter {
        color_srgb,
        density: density as f32 / 100.0,
        preserve_luminosity: preserve,
    })
}

fn write_photo_filter(color: [f32; 3], density: f32, preserve: bool) -> R<Vec<u8>> {
    let mut s = Sink::new();
    s.u16(2);
    write_rgb(&mut s, color)?;
    s.u32(spell("photo filter density", density, 100.0, 0, 100)? as u32);
    s.u8(u8::from(preserve));
    Ok(s.into_inner())
}

// ----------------------------------------------------------- channel mixer

fn channel_mixer(data: &[u8]) -> R<AdjustmentKind> {
    let mut c = Cursor::new(data);
    let version = c.u16().map_err(trunc("channel mixer version"))?;
    if version != 1 {
        return fail(format!("channel mixer version {version} (expected 1)"));
    }
    let monochrome = c.u16().map_err(trunc("channel mixer"))? != 0;
    let mut rows = [[0.0f32; 4]; 3];
    for row in &mut rows {
        let mut v = [0i32; 5];
        for x in &mut v {
            *x = ranged(
                "mixer value",
                c.i16().map_err(trunc("channel mixer"))?.into(),
                -200,
                200,
            )?;
        }
        *row = [
            v[0] as f32 / 100.0,
            v[1] as f32 / 100.0,
            v[2] as f32 / 100.0,
            v[4] as f32 / 100.0,
        ];
    }
    Ok(AdjustmentKind::ChannelMixer { rows, monochrome })
}

fn write_channel_mixer(rows: &[[f32; 4]; 3], monochrome: bool) -> R<Vec<u8>> {
    let mut s = Sink::new();
    s.u16(1);
    s.u16(u16::from(monochrome));
    // Red, green, blue outputs, then the fourth (CMYK black) record unused.
    for row in rows.iter().chain(std::iter::once(&[0.0; 4])) {
        s.i16(pct("mixer value", row[0], -200, 200)?);
        s.i16(pct("mixer value", row[1], -200, 200)?);
        s.i16(pct("mixer value", row[2], -200, 200)?);
        s.i16(0);
        s.i16(pct("mixer value", row[3], -200, 200)?);
    }
    Ok(s.into_inner())
}

// ------------------------------------------------- posterize / threshold

fn posterize(data: &[u8]) -> R<AdjustmentKind> {
    let mut c = Cursor::new(data);
    let levels = ranged(
        "posterize levels",
        c.u16().map_err(trunc("posterize"))?.into(),
        2,
        255,
    )?;
    Ok(AdjustmentKind::Posterize {
        levels: levels as u32,
    })
}

fn threshold(data: &[u8]) -> R<AdjustmentKind> {
    let mut c = Cursor::new(data);
    let level = ranged(
        "threshold level",
        c.u16().map_err(trunc("threshold"))?.into(),
        1,
        255,
    )?;
    Ok(AdjustmentKind::Threshold {
        level: level as f32 / 255.0,
    })
}

// ------------------------------------------------------------ gradient map

const LOCATION_SCALE: f32 = 4096.0;
const MAX_GRADIENT_NAME: usize = 1024;

fn gradient_map(data: &[u8], unmapped: &mut Vec<String>) -> R<AdjustmentKind> {
    let mut c = Cursor::new(data);
    let version = c.u16().map_err(trunc("gradient map version"))?;
    if version != 1 {
        return fail(format!("gradient map version {version} (expected 1)"));
    }
    let reverse = c.u8().map_err(trunc("gradient map"))? != 0;
    let _dither = c.u8().map_err(trunc("gradient map"))?;
    c.unicode_string(MAX_GRADIENT_NAME)
        .map_err(trunc("gradient name"))?;
    let count = usize::from(c.u16().map_err(trunc("gradient stop count"))?);
    // Each colour stop is 20 bytes: refuse a count the payload cannot hold
    // before reserving for it.
    if count < 2 || count.saturating_mul(20) > c.remaining() {
        return fail(format!(
            "gradient map declares {count} colour stops in {} bytes",
            c.remaining()
        ));
    }
    let mut stops = Vec::with_capacity(count);
    let mut midpoints = false;
    for _ in 0..count {
        let location = c.u32().map_err(trunc("gradient stop"))?;
        if location > 4096 {
            return fail(format!(
                "a gradient stop sits at {location} (expected 0..=4096)"
            ));
        }
        let midpoint = c.u32().map_err(trunc("gradient stop"))?;
        if midpoint != 50 {
            midpoints = true;
        }
        let color = read_color(&mut c)?;
        c.skip(2).map_err(trunc("gradient stop"))?;
        stops.push((location as f32 / LOCATION_SCALE, color));
    }
    if midpoints {
        unmapped.push("gradient colour midpoints".into());
    }
    Ok(AdjustmentKind::GradientMap { stops, reverse })
}

fn write_gradient_map(stops: &[(f32, [f32; 3])], reverse: bool) -> R<Vec<u8>> {
    if stops.len() < 2 || stops.len() > usize::from(u16::MAX) {
        return fail(format!(
            "a gradient map with {} stops cannot be written",
            stops.len()
        ));
    }
    let mut s = Sink::new();
    s.u16(1);
    s.u8(u8::from(reverse));
    s.u8(0);
    s.unicode_string("Custom");
    s.u16(stops.len() as u16);
    for (position, color) in stops {
        s.u32(spell("gradient stop position", *position, LOCATION_SCALE, 0, 4096)? as u32);
        s.u32(50);
        write_rgb(&mut s, *color)?;
        s.u16(0);
    }
    // Opaque from end to end: a gradient map has no use for transparency.
    s.u16(2);
    for location in [0u32, 4096] {
        s.u32(location);
        s.u32(50);
        s.u16(100);
    }
    s.u16(2); // expansion count
    s.u16(4096); // interpolation (smoothness 100%)
    s.u16(32); // length
    s.u16(0); // mode
    s.u32(0); // random seed
    s.u16(0); // show transparency
    s.u16(0); // use vector colour
    s.u32(0); // roughness
    s.u16(0); // colour model
    s.zeros(8); // minimum colour
    for _ in 0..4 {
        s.u16(u16::MAX); // maximum colour
    }
    s.zeros(2);
    Ok(s.into_inner())
}

// ---------------------------------------------------------- selective colour

fn selective_color(data: &[u8]) -> R<AdjustmentKind> {
    let mut c = Cursor::new(data);
    let version = c.u16().map_err(trunc("selective colour version"))?;
    if version != 1 {
        return fail(format!("selective colour version {version} (expected 1)"));
    }
    let method = c.u16().map_err(trunc("selective colour"))?;
    let relative = match method {
        0 => true,
        1 => false,
        other => return fail(format!("selective colour method {other}")),
    };
    // The first record is reserved.
    c.skip(8).map_err(trunc("selective colour"))?;
    let mut ranges = [[0.0f32; 4]; 9];
    for range in &mut ranges {
        for v in range.iter_mut() {
            *v = ranged(
                "selective colour",
                c.i16().map_err(trunc("selective colour"))?.into(),
                -100,
                100,
            )? as f32
                / 100.0;
        }
    }
    Ok(AdjustmentKind::SelectiveColor { ranges, relative })
}

fn write_selective_color(ranges: &[[f32; 4]; 9], relative: bool) -> R<Vec<u8>> {
    let mut s = Sink::new();
    s.u16(1);
    s.u16(if relative { 0 } else { 1 });
    s.zeros(8);
    for range in ranges {
        for v in range {
            s.i16(pct("selective colour", *v, -100, 100)?);
        }
    }
    Ok(s.into_inner())
}

// ----------------------------------------------------------------- exposure

fn exposure(data: &[u8]) -> R<AdjustmentKind> {
    let mut c = Cursor::new(data);
    let version = c.u16().map_err(trunc("exposure version"))?;
    if version != 1 {
        return fail(format!("exposure version {version} (expected 1)"));
    }
    let mut f = |what: &str| -> R<f32> {
        let v = f32::from_bits(c.u32().map_err(trunc("exposure"))?);
        if v.is_finite() {
            Ok(v)
        } else {
            fail(format!("exposure {what} is not finite"))
        }
    };
    let stops = f("stops")?;
    let offset = f("offset")?;
    let gamma = f("gamma")?;
    if !(-20.0..=20.0).contains(&stops) || !(-1.0..=1.0).contains(&offset) {
        return fail(format!("exposure {stops} / offset {offset} out of range"));
    }
    if !(0.01..=10.0).contains(&gamma) {
        return fail(format!("exposure gamma {gamma} (expected 0.01..=10)"));
    }
    Ok(if offset == 0.0 && gamma == 1.0 {
        AdjustmentKind::Exposure { stops }
    } else {
        AdjustmentKind::ExposureFull {
            stops,
            offset,
            gamma,
        }
    })
}

fn write_exposure(stops: f32, offset: f32, gamma: f32) -> R<Vec<u8>> {
    // The same ranges the decoder accepts, so what is written reads back.
    if !(-20.0..=20.0).contains(&stops) || !(-1.0..=1.0).contains(&offset) {
        return fail(format!("exposure {stops} / offset {offset} out of range"));
    }
    if !(0.01..=10.0).contains(&gamma) {
        return fail(format!("exposure gamma {gamma} (expected 0.01..=10)"));
    }
    let mut s = Sink::new();
    s.u16(1);
    for v in [stops, offset, gamma] {
        s.u32(v.to_bits());
    }
    Ok(s.into_inner())
}

// ----------------------------------------------------------------- vibrance

fn vibrance(data: &[u8], opts: &ReadOptions) -> R<AdjustmentKind> {
    let d = versioned_descriptor(data, opts, "vibrance")?;
    let get = |key: &str| -> R<f32> {
        let v = finite_number(&d, key)?.unwrap_or(0.0);
        if !(-100.0..=100.0).contains(&v) {
            return fail(format!("{key} is {v}%"));
        }
        Ok((v / 100.0) as f32)
    };
    Ok(AdjustmentKind::Vibrance {
        vibrance: get("vibrance")?,
        saturation: get("Strt")?,
    })
}

fn write_vibrance(vibrance: f32, saturation: f32) -> R<Vec<u8>> {
    let mut d = Descriptor::new("null");
    push(
        &mut d,
        "vibrance",
        Value::Integer(spell("vibrance", vibrance, 100.0, -100, 100)?),
    )?;
    push(
        &mut d,
        "Strt",
        Value::Integer(spell("vibrance saturation", saturation, 100.0, -100, 100)?),
    )?;
    write_descriptor(None, &d)
}

// ------------------------------------------------------------ colour lookup

const MIN_LUT: u32 = 2;
const MAX_LUT: u32 = 65;

fn enumerated<'a>(d: &'a Descriptor, key: &str) -> Option<&'a str> {
    match d.get(key)? {
        Value::Enumerated { value, .. } => Some(value),
        _ => None,
    }
}

fn color_lookup(data: &[u8], opts: &ReadOptions) -> R<AdjustmentKind> {
    let mut c = Cursor::new(data);
    let version = c.u16().map_err(trunc("colour lookup version"))?;
    if version != 1 {
        return fail(format!("colour lookup version {version} (expected 1)"));
    }
    let d = versioned_descriptor(c.peek_rest(), opts, "colour lookup")?;
    let format = enumerated(&d, "LUTFormat");
    if let Some(format) = format {
        if format != "LUTFormatCUBE" {
            return fail(format!(
                "a {format} lookup is not decoded (an embedded .cube is)"
            ));
        }
    }
    let Some(Value::RawData(cube)) = d.get("LUT3DFileData") else {
        return fail("the colour lookup embeds no .cube LUT");
    };
    let name = d
        .text("Nm  ")
        .or_else(|| d.text("LUT3DFileName"))
        .unwrap_or("Color Lookup")
        .to_string();
    let (size, table) = parse_cube(cube)?;
    Ok(AdjustmentKind::ColorLookup { name, size, table })
}

/// A `.cube` 3D LUT: `LUT_3D_SIZE n` then `n³` lines of three numbers, red
/// fastest. Bounded by the text it is handed.
pub fn parse_cube(bytes: &[u8]) -> R<(u32, Vec<[f32; 3]>)> {
    let text = std::str::from_utf8(bytes).map_err(|_| "the .cube text is not UTF-8".to_string())?;
    let mut size: Option<u32> = None;
    let mut table: Vec<[f32; 3]> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut words = line.split_whitespace();
        let first = words.next().unwrap_or_default();
        match first {
            "LUT_3D_SIZE" => {
                let n: u32 = words
                    .next()
                    .and_then(|w| w.parse().ok())
                    .ok_or("LUT_3D_SIZE has no size")?;
                if !(MIN_LUT..=MAX_LUT).contains(&n) {
                    return fail(format!("LUT_3D_SIZE {n} (expected {MIN_LUT}..={MAX_LUT})"));
                }
                if size.is_some() || !table.is_empty() {
                    return fail("LUT_3D_SIZE appears twice or after the table");
                }
                size = Some(n);
                table.reserve((n * n * n) as usize);
            }
            "LUT_1D_SIZE" => return fail("a 1D .cube LUT is not decoded"),
            "DOMAIN_MIN" | "DOMAIN_MAX" => {
                let want = if first == "DOMAIN_MIN" { 0.0 } else { 1.0 };
                if words.any(|w| w.parse::<f32>().ok() != Some(want)) {
                    return fail("a .cube domain other than 0..1 is not decoded");
                }
            }
            "TITLE" | "LUT_3D_INPUT_RANGE" => {}
            _ => {
                let Some(n) = size else {
                    return fail("the .cube table starts before LUT_3D_SIZE");
                };
                let mut rgb = [0.0f32; 3];
                let mut parts = line.split_whitespace();
                for v in &mut rgb {
                    *v = parts
                        .next()
                        .and_then(|w| w.parse::<f32>().ok())
                        .filter(|v| v.is_finite())
                        .ok_or_else(|| format!("unreadable .cube line {line:?}"))?;
                }
                if table.len() as u64 >= u64::from(n).pow(3) {
                    return fail("the .cube table has more entries than LUT_3D_SIZE cubed");
                }
                table.push(rgb);
            }
        }
    }
    let size = size.ok_or("the .cube has no LUT_3D_SIZE")?;
    if table.len() as u64 != u64::from(size).pow(3) {
        return fail(format!(
            "the .cube table has {} entries, not {}",
            table.len(),
            u64::from(size).pow(3)
        ));
    }
    Ok((size, table))
}

fn write_cube(name: &str, size: u32, table: &[[f32; 3]]) -> String {
    let mut out = String::new();
    let title: String = name
        .chars()
        .filter(|c| *c != '"' && !c.is_control())
        .collect();
    out.push_str(&format!("TITLE \"{title}\"\nLUT_3D_SIZE {size}\n"));
    for [r, g, b] in table {
        out.push_str(&format!("{r} {g} {b}\n"));
    }
    out
}

fn write_color_lookup(name: &str, size: u32, table: &[[f32; 3]]) -> R<Vec<u8>> {
    if !(MIN_LUT..=MAX_LUT).contains(&size) || table.len() as u64 != u64::from(size).pow(3) {
        return fail(format!(
            "a {size}-point LUT with {} entries cannot be written",
            table.len()
        ));
    }
    // W11-A: a non-finite entry would be written as `NaN`/`inf`, which
    // `parse_cube` refuses on reopen; refuse it here, by name, instead.
    for (i, entry) in table.iter().enumerate() {
        if let Some(v) = entry.iter().find(|v| !v.is_finite()) {
            return fail(format!(
                "LUT entry {i} component {v} is not a finite number"
            ));
        }
    }
    let mut d = Descriptor::new("null");
    let en = |type_id: &str, value: &str| Value::Enumerated {
        type_id: type_id.into(),
        value: value.into(),
    };
    push(&mut d, "lookupType", en("colorLookupType", "3DLUT"))?;
    push(&mut d, "Nm  ", Value::Text(name.to_string()))?;
    push(&mut d, "Dthr", Value::Bool(true))?;
    push(&mut d, "LUTFormat", en("LUTFormatType", "LUTFormatCUBE"))?;
    push(&mut d, "dataOrder", en("colorDataOrder", "rgbOrder"))?;
    push(&mut d, "tableOrder", en("colorDataOrder", "bgrOrder"))?;
    push(
        &mut d,
        "LUT3DFileData",
        Value::RawData(write_cube(name, size, table).into_bytes()),
    )?;
    push(&mut d, "LUT3DFileName", Value::Text(format!("{name}.cube")))?;
    write_descriptor(Some(1), &d)
}

#[cfg(test)]
#[path = "adjustments_tests.rs"]
mod tests;
