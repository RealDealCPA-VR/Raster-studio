//! The Color panel: one colour, five ways of saying it.
//!
//! # One value, many spellings
//!
//! The panel holds a single straight-alpha sRGB triple and derives HSB, 8-bit
//! RGB, hex and Lab from it on demand. Storing each notation separately is the
//! classic colour-picker bug: dragging hue on a fully desaturated colour throws
//! the hue away, because the round trip through RGB has nothing to preserve it.
//! [`ColorState`] therefore keeps hue and saturation as the *authority* while a
//! gesture is in flight — see [`ColorState::set_hsv`] — and reconstitutes RGB
//! from them.
//!
//! # Parsing is forgiving, formatting is not
//!
//! [`parse_hex`] accepts `#RGB`, `#RGBA`, `#RRGGBB`, `#RRGGBBAA`, with or
//! without the hash and in any case, because that is what lands on a clipboard.
//! [`format_hex`] emits exactly one spelling, uppercase and six digits, so two
//! equal colours always read as equal.

use color::{hsv_to_rgb, lab_to_rgb, rgb_to_hsv, rgb_to_lab};

/// Which numeric fields the panel is showing.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, PartialOrd, Ord)]
pub enum ColorNotation {
    #[default]
    Hsb,
    Rgb,
    Hex,
    Lab,
}

impl ColorNotation {
    pub const ALL: &'static [ColorNotation] = &[
        ColorNotation::Hsb,
        ColorNotation::Rgb,
        ColorNotation::Hex,
        ColorNotation::Lab,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            ColorNotation::Hsb => "HSB",
            ColorNotation::Rgb => "RGB",
            ColorNotation::Hex => "Hex",
            ColorNotation::Lab => "Lab",
        }
    }
}

/// Which well the panel is editing.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum ColorWell {
    #[default]
    Foreground,
    Background,
}

impl ColorWell {
    pub const fn label(self) -> &'static str {
        match self {
            ColorWell::Foreground => "Foreground",
            ColorWell::Background => "Background",
        }
    }

    pub const fn other(self) -> Self {
        match self {
            ColorWell::Foreground => ColorWell::Background,
            ColorWell::Background => ColorWell::Foreground,
        }
    }
}

/// Photoshop's default pair: black over white.
pub const DEFAULT_FOREGROUND: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
pub const DEFAULT_BACKGROUND: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

/// The panel's state.
#[derive(Clone, PartialEq, Debug)]
pub struct ColorState {
    foreground: [f32; 4],
    background: [f32; 4],
    /// Which well the spectrum and the fields edit.
    pub editing: ColorWell,
    pub notation: ColorNotation,
    /// Hue and saturation carried across a gesture so a trip through a grey or
    /// a black does not lose them. See the module note.
    hue_sat: (f32, f32),
    /// `true` while the eyedropper is armed and the next canvas click samples
    /// a colour instead of painting.
    pub eyedropper_armed: bool,
}

impl Default for ColorState {
    fn default() -> Self {
        Self {
            foreground: DEFAULT_FOREGROUND,
            background: DEFAULT_BACKGROUND,
            editing: ColorWell::Foreground,
            notation: ColorNotation::default(),
            hue_sat: (0.0, 0.0),
            eyedropper_armed: false,
        }
    }
}

impl ColorState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn foreground(&self) -> [f32; 4] {
        self.foreground
    }

    pub fn background(&self) -> [f32; 4] {
        self.background
    }

    pub fn well(&self, well: ColorWell) -> [f32; 4] {
        match well {
            ColorWell::Foreground => self.foreground,
            ColorWell::Background => self.background,
        }
    }

    /// The colour the spectrum and the fields are editing.
    pub fn current(&self) -> [f32; 4] {
        self.well(self.editing)
    }

    /// Write a well. Non-finite components are dropped rather than stored — a
    /// NaN here would travel into a brush and stop the stroke drawing at all.
    pub fn set_well(&mut self, well: ColorWell, rgba: [f32; 4]) -> bool {
        let Some(clean) = sanitise(rgba) else {
            return false;
        };
        let slot = match well {
            ColorWell::Foreground => &mut self.foreground,
            ColorWell::Background => &mut self.background,
        };
        if *slot == clean {
            return false;
        }
        *slot = clean;
        if well == self.editing {
            let hsv = rgb_to_hsv([clean[0], clean[1], clean[2]]);
            // A fully desaturated or black colour reports hue 0; keep the hue
            // the user was on rather than snapping the wheel to red.
            if hsv[1] > 0.0 && hsv[2] > 0.0 {
                self.hue_sat = (hsv[0], hsv[1]);
            }
        }
        true
    }

    /// Write the well currently being edited.
    pub fn set_current(&mut self, rgba: [f32; 4]) -> bool {
        self.set_well(self.editing, rgba)
    }

    /// Swap foreground and background — the `X` key.
    pub fn swap(&mut self) {
        std::mem::swap(&mut self.foreground, &mut self.background);
    }

    /// Reset to black over white — the `D` key.
    pub fn reset(&mut self) {
        self.foreground = DEFAULT_FOREGROUND;
        self.background = DEFAULT_BACKGROUND;
        self.hue_sat = (0.0, 0.0);
    }

    /// The current colour as HSB — hue in degrees `0.0..360.0`, saturation and
    /// brightness in `0.0..=1.0` — with hue and saturation carried over from
    /// the gesture rather than re-derived when they are undefined.
    pub fn hsv(&self) -> [f32; 3] {
        let c = self.current();
        let derived = rgb_to_hsv([c[0], c[1], c[2]]);
        let hue = if derived[1] > 0.0 && derived[2] > 0.0 {
            derived[0]
        } else {
            self.hue_sat.0
        };
        let sat = if derived[2] > 0.0 {
            derived[1]
        } else {
            self.hue_sat.1
        };
        [hue, sat, derived[2]]
    }

    /// Set the current colour from HSB. Hue wraps into `0.0..360.0`; the other
    /// two clamp into `0.0..=1.0`.
    pub fn set_hsv(&mut self, hsv: [f32; 3]) -> bool {
        if !hsv.iter().all(|v| v.is_finite()) {
            return false;
        }
        let h = hsv[0].rem_euclid(360.0);
        let s = hsv[1].clamp(0.0, 1.0);
        let v = hsv[2].clamp(0.0, 1.0);
        self.hue_sat = (h, s);
        let rgb = hsv_to_rgb([h, s, v]);
        let alpha = self.current()[3];
        let changed = self.set_well(self.editing, [rgb[0], rgb[1], rgb[2], alpha]);
        // `set_well` re-derives hue/sat from RGB; put the authoritative pair
        // back, or dragging saturation to zero would forget the hue.
        self.hue_sat = (h, s);
        changed
    }

    /// The current colour as CIE Lab.
    pub fn lab(&self) -> [f32; 3] {
        let c = self.current();
        rgb_to_lab([c[0], c[1], c[2]])
    }

    /// Set the current colour from CIE Lab.
    pub fn set_lab(&mut self, lab: [f32; 3]) -> bool {
        if !lab.iter().all(|v| v.is_finite()) {
            return false;
        }
        let rgb = lab_to_rgb(lab);
        let alpha = self.current()[3];
        self.set_current([
            rgb[0].clamp(0.0, 1.0),
            rgb[1].clamp(0.0, 1.0),
            rgb[2].clamp(0.0, 1.0),
            alpha,
        ])
    }

    /// The current colour as 8-bit RGB.
    pub fn rgb8(&self) -> [u8; 3] {
        let c = self.current();
        [to_u8(c[0]), to_u8(c[1]), to_u8(c[2])]
    }

    /// Set the current colour from 8-bit RGB.
    pub fn set_rgb8(&mut self, rgb: [u8; 3]) -> bool {
        let alpha = self.current()[3];
        self.set_current([
            f32::from(rgb[0]) / 255.0,
            f32::from(rgb[1]) / 255.0,
            f32::from(rgb[2]) / 255.0,
            alpha,
        ])
    }

    /// The current colour as `#RRGGBB`.
    pub fn hex(&self) -> String {
        format_hex(self.current())
    }

    /// Set the current colour from a hex string. Returns `false` on a string
    /// that is not a colour, leaving the state alone.
    pub fn set_hex(&mut self, text: &str) -> bool {
        match parse_hex(text) {
            Some(rgba) => self.set_current(rgba),
            None => false,
        }
    }

    /// Is the colour outside what sRGB can show? Lab and the spectrum can both
    /// name colours the display cannot reproduce.
    pub fn is_out_of_gamut(&self) -> bool {
        let c = self.current();
        c[..3].iter().any(|v| *v < 0.0 || *v > 1.0)
    }
}

fn sanitise(rgba: [f32; 4]) -> Option<[f32; 4]> {
    rgba.iter()
        .all(|v| v.is_finite())
        .then(|| [rgba[0], rgba[1], rgba[2], rgba[3].clamp(0.0, 1.0)])
}

fn to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Format a colour as `#RRGGBB`, always six uppercase digits.
///
/// Alpha is deliberately not written: the hex field is a colour field, and an
/// eight-digit value pasted into a design tool that expects six is worse than
/// no alpha at all. The alpha slider is beside it.
pub fn format_hex(rgba: [f32; 4]) -> String {
    format!(
        "#{:02X}{:02X}{:02X}",
        to_u8(rgba[0]),
        to_u8(rgba[1]),
        to_u8(rgba[2])
    )
}

/// Whether the Hex field should show its "enter a colour like #3366CC" hint.
///
/// A hint is a correction, and a correction shown against text the panel itself
/// put in the field reads as the user's mistake. So it waits for an edit to be
/// under way — which is also why the field's buffer has to survive the frame at
/// all. See `crate::view::text_field`.
pub fn hex_hint_is_warranted(editing: bool, text: &str) -> bool {
    editing && parse_hex(text).is_none()
}

/// Parse `#RGB`, `#RGBA`, `#RRGGBB` or `#RRGGBBAA`, hash optional, any case.
///
/// Returns straight-alpha sRGB in `0.0..=1.0`. Anything else — a wrong length,
/// a non-hex digit, an empty string — is `None`.
pub fn parse_hex(text: &str) -> Option<[f32; 4]> {
    let t = text.trim().trim_start_matches('#');
    if !t.chars().all(|c| c.is_ascii_hexdigit()) || t.is_empty() {
        return None;
    }
    let nibble = |i: usize| -> Option<f32> {
        let c = t.as_bytes().get(i)?;
        let v = (*c as char).to_digit(16)?;
        Some(v as f32 / 15.0)
    };
    let byte = |i: usize| -> Option<f32> {
        let hi = (t.as_bytes().get(i * 2).copied()? as char).to_digit(16)?;
        let lo = (t.as_bytes().get(i * 2 + 1).copied()? as char).to_digit(16)?;
        Some((hi * 16 + lo) as f32 / 255.0)
    };
    match t.len() {
        3 => Some([nibble(0)?, nibble(1)?, nibble(2)?, 1.0]),
        4 => Some([nibble(0)?, nibble(1)?, nibble(2)?, nibble(3)?]),
        6 => Some([byte(0)?, byte(1)?, byte(2)?, 1.0]),
        8 => Some([byte(0)?, byte(1)?, byte(2)?, byte(3)?]),
        _ => None,
    }
}

/// One entry in the Swatches panel.
#[derive(Clone, PartialEq, Debug)]
pub struct Swatch {
    pub name: String,
    pub rgba: [f32; 4],
}

/// The Swatches panel's state: a named, reorderable palette.
#[derive(Clone, PartialEq, Debug)]
pub struct SwatchesState {
    swatches: Vec<Swatch>,
}

impl Default for SwatchesState {
    fn default() -> Self {
        Self {
            swatches: default_swatches(),
        }
    }
}

impl SwatchesState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn swatches(&self) -> &[Swatch] {
        &self.swatches
    }

    pub fn len(&self) -> usize {
        self.swatches.len()
    }

    pub fn is_empty(&self) -> bool {
        self.swatches.is_empty()
    }

    /// Add a colour to the end of the palette. A colour already in the palette
    /// is not added twice — a palette of forty identical greys helps nobody.
    pub fn add(&mut self, name: impl Into<String>, rgba: [f32; 4]) -> bool {
        let Some(clean) = sanitise(rgba) else {
            return false;
        };
        if self.index_of(clean).is_some() {
            return false;
        }
        self.swatches.push(Swatch {
            name: name.into(),
            rgba: clean,
        });
        true
    }

    /// The index of a colour already in the palette, compared at 8-bit
    /// precision — which is the precision the user can actually see.
    pub fn index_of(&self, rgba: [f32; 4]) -> Option<usize> {
        let key = format_hex(rgba);
        self.swatches
            .iter()
            .position(|s| format_hex(s.rgba) == key && to_u8(s.rgba[3]) == to_u8(rgba[3]))
    }

    pub fn remove(&mut self, index: usize) -> Option<Swatch> {
        (index < self.swatches.len()).then(|| self.swatches.remove(index))
    }

    /// Move a swatch. Both indices are clamped, so a drag past the end lands on
    /// the end rather than doing nothing.
    pub fn reorder(&mut self, from: usize, to: usize) -> bool {
        if self.swatches.is_empty() || from >= self.swatches.len() || from == to {
            return false;
        }
        let to = to.min(self.swatches.len() - 1);
        if from == to {
            return false;
        }
        let s = self.swatches.remove(from);
        self.swatches.insert(to, s);
        true
    }

    pub fn get(&self, index: usize) -> Option<&Swatch> {
        self.swatches.get(index)
    }
}

/// The palette a new install starts with: a neutral ramp and the primary and
/// secondary hues, which is enough to be useful without pretending to be a
/// curated brand palette.
fn default_swatches() -> Vec<Swatch> {
    let mut out = Vec::with_capacity(19);
    for (i, name) in ["Black", "Grey 20", "Grey 40", "Grey 60", "Grey 80", "White"]
        .into_iter()
        .enumerate()
    {
        let v = i as f32 / 5.0;
        out.push(Swatch {
            name: name.to_string(),
            rgba: [v, v, v, 1.0],
        });
    }
    for (name, hue) in [
        ("Red", 0.0),
        ("Orange", 30.0),
        ("Yellow", 60.0),
        ("Chartreuse", 90.0),
        ("Green", 120.0),
        ("Spring", 150.0),
        ("Cyan", 180.0),
        ("Azure", 210.0),
        ("Blue", 240.0),
        ("Violet", 270.0),
        ("Magenta", 300.0),
        ("Rose", 330.0),
    ] {
        let rgb = hsv_to_rgb([hue, 1.0, 1.0]);
        out.push(Swatch {
            name: name.to_string(),
            rgba: [rgb[0], rgb[1], rgb[2], 1.0],
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_pair_is_black_over_white() {
        let s = ColorState::new();
        assert_eq!(s.foreground(), DEFAULT_FOREGROUND);
        assert_eq!(s.background(), DEFAULT_BACKGROUND);
        assert_eq!(s.current(), DEFAULT_FOREGROUND);
    }

    #[test]
    fn swapping_exchanges_the_wells_and_swapping_twice_is_the_identity() {
        let mut s = ColorState::new();
        s.set_well(ColorWell::Foreground, [1.0, 0.0, 0.0, 1.0]);
        let (fg, bg) = (s.foreground(), s.background());
        s.swap();
        assert_eq!(s.foreground(), bg);
        assert_eq!(s.background(), fg);
        s.swap();
        assert_eq!(s.foreground(), fg);
        assert_eq!(s.background(), bg);
    }

    #[test]
    fn resetting_returns_to_black_over_white() {
        let mut s = ColorState::new();
        s.set_well(ColorWell::Foreground, [0.2, 0.4, 0.6, 0.5]);
        s.set_well(ColorWell::Background, [0.9, 0.1, 0.1, 1.0]);
        s.reset();
        assert_eq!(s.foreground(), DEFAULT_FOREGROUND);
        assert_eq!(s.background(), DEFAULT_BACKGROUND);
    }

    #[test]
    fn the_fields_edit_whichever_well_is_selected() {
        let mut s = ColorState::new();
        s.editing = ColorWell::Background;
        assert!(s.set_hex("#FF0000"));
        assert_eq!(s.rgb8(), [255, 0, 0]);
        assert_eq!(s.background()[0], 1.0);
        assert_eq!(s.foreground(), DEFAULT_FOREGROUND);
    }

    #[test]
    fn hex_parses_every_shape_a_clipboard_produces() {
        assert_eq!(parse_hex("#FF0000"), Some([1.0, 0.0, 0.0, 1.0]));
        assert_eq!(parse_hex("ff0000"), Some([1.0, 0.0, 0.0, 1.0]));
        assert_eq!(parse_hex("  #Ff0000  "), Some([1.0, 0.0, 0.0, 1.0]));
        assert_eq!(parse_hex("#F00"), Some([1.0, 0.0, 0.0, 1.0]));
        let rgba = parse_hex("#F00F").expect("four-digit form");
        assert_eq!(rgba, [1.0, 0.0, 0.0, 1.0]);
        let with_alpha = parse_hex("#FF000080").expect("eight-digit form");
        assert_eq!(with_alpha[3], 128.0 / 255.0);
    }

    #[test]
    fn the_hex_hint_waits_for_the_user_to_type_something_wrong() {
        // Not editing: the field shows what the panel put there, so there is
        // nothing to correct — even if that string somehow did not parse.
        assert!(!hex_hint_is_warranted(false, "#3366CC"));
        assert!(!hex_hint_is_warranted(false, "nonsense"));
        // Editing and valid: still nothing to say.
        assert!(!hex_hint_is_warranted(true, "#3366CC"));
        assert!(!hex_hint_is_warranted(true, "F00"));
        // Editing and half-typed: this is the one case worth a hint. Note
        // "336" is *not* one of them — three digits is a whole colour.
        assert!(hex_hint_is_warranted(true, "3366C"));
        assert!(hex_hint_is_warranted(true, "#33"));
        assert!(hex_hint_is_warranted(true, ""));
    }

    #[test]
    fn hex_refuses_anything_that_is_not_a_colour() {
        for bad in ["", "#", "#12", "#12345", "#GGGGGG", "red", "#1234567"] {
            assert_eq!(parse_hex(bad), None, "{bad:?} parsed");
        }
    }

    #[test]
    fn a_bad_hex_leaves_the_colour_alone() {
        let mut s = ColorState::new();
        s.set_hex("#3366CC");
        let before = s.current();
        assert!(!s.set_hex("not a colour"));
        assert_eq!(s.current(), before);
    }

    #[test]
    fn hex_formats_to_exactly_one_spelling() {
        assert_eq!(format_hex([1.0, 0.0, 0.0, 1.0]), "#FF0000");
        assert_eq!(format_hex([0.0, 0.0, 0.0, 1.0]), "#000000");
        assert_eq!(format_hex([1.0, 1.0, 1.0, 0.5]), "#FFFFFF");
        // Out of range in either direction still produces six digits.
        assert_eq!(format_hex([-1.0, 2.0, 0.5, 1.0]), "#00FF80");
    }

    #[test]
    fn hex_round_trips_through_the_state() {
        let mut s = ColorState::new();
        for hex in ["#3366CC", "#FFFFFF", "#000000", "#7F7F7F", "#12AB34"] {
            assert!(s.set_hex(hex), "{hex} was refused");
            assert_eq!(s.hex(), hex);
        }
    }

    #[test]
    fn eight_bit_rgb_round_trips() {
        let mut s = ColorState::new();
        for rgb in [[0, 0, 0], [255, 255, 255], [12, 200, 77], [1, 2, 3]] {
            assert!(s.set_rgb8(rgb) || s.rgb8() == rgb);
            assert_eq!(s.rgb8(), rgb);
        }
    }

    #[test]
    fn hsb_round_trips_for_saturated_colours() {
        let mut s = ColorState::new();
        for hsv in [
            [0.0, 1.0, 1.0],
            [90.0, 0.5, 0.75],
            [180.0, 1.0, 0.5],
            [356.4, 0.8, 0.9],
        ] {
            assert!(s.set_hsv(hsv));
            let back = s.hsv();
            assert!((back[0] - hsv[0]).abs() < 0.2, "hue: {hsv:?} -> {back:?}");
            for i in 1..3 {
                assert!(
                    (back[i] - hsv[i]).abs() < 1e-3,
                    "component {i}: {hsv:?} -> {back:?}"
                );
            }
        }
    }

    #[test]
    fn dragging_saturation_to_zero_does_not_forget_the_hue() {
        // The bug this exists to prevent: rgb_to_hsv reports hue 0 for any
        // grey, so a naive picker snaps the wheel back to red.
        let mut s = ColorState::new();
        s.set_hsv([252.0, 0.9, 0.9]);
        s.set_hsv([252.0, 0.0, 0.9]);
        assert!(
            (s.hsv()[0] - 252.0).abs() < 1e-3,
            "hue was lost: {:?}",
            s.hsv()
        );
        // ...and it comes back when saturation is raised again.
        s.set_hsv([s.hsv()[0], 1.0, 0.9]);
        assert!((s.hsv()[0] - 252.0).abs() < 0.2);
    }

    #[test]
    fn dragging_brightness_to_zero_does_not_forget_the_hue_or_saturation() {
        let mut s = ColorState::new();
        s.set_hsv([120.0, 0.8, 1.0]);
        s.set_hsv([120.0, 0.8, 0.0]);
        assert_eq!(s.rgb8(), [0, 0, 0]);
        let hsv = s.hsv();
        assert!((hsv[0] - 120.0).abs() < 1e-3, "hue lost: {hsv:?}");
        assert!((hsv[1] - 0.8).abs() < 1e-4, "saturation lost: {hsv:?}");
    }

    #[test]
    fn hue_wraps_and_the_other_components_clamp() {
        let mut s = ColorState::new();
        s.set_hsv([450.0, 2.0, -1.0]);
        let hsv = s.hsv();
        assert!((hsv[0] - 90.0).abs() < 1e-3, "hue did not wrap: {hsv:?}");
        assert_eq!(hsv[2], 0.0);
        s.set_hsv([-90.0, 0.5, 0.5]);
        assert!((s.hsv()[0] - 270.0).abs() < 0.2, "{:?}", s.hsv());
    }

    #[test]
    fn lab_round_trips_within_a_perceptual_hair() {
        let mut s = ColorState::new();
        for hex in ["#3366CC", "#FF8800", "#204020", "#EEEEEE"] {
            s.set_hex(hex);
            let lab = s.lab();
            let mut other = ColorState::new();
            assert!(other.set_lab(lab));
            assert_eq!(other.hex(), hex, "Lab round trip changed {hex}");
        }
    }

    #[test]
    fn lab_lightness_climbs_with_brightness() {
        let mut s = ColorState::new();
        s.set_hex("#000000");
        let dark = s.lab()[0];
        s.set_hex("#808080");
        let mid = s.lab()[0];
        s.set_hex("#FFFFFF");
        let light = s.lab()[0];
        assert!(dark < mid && mid < light, "{dark} {mid} {light}");
        assert!(dark.abs() < 1e-3);
        assert!((light - 100.0).abs() < 0.5);
    }

    #[test]
    fn a_non_finite_colour_is_refused_everywhere() {
        let mut s = ColorState::new();
        let before = s.current();
        assert!(!s.set_current([f32::NAN, 0.0, 0.0, 1.0]));
        assert!(!s.set_hsv([f32::NAN, 0.5, 0.5]));
        assert!(!s.set_lab([f32::INFINITY, 0.0, 0.0]));
        assert_eq!(s.current(), before);
    }

    #[test]
    fn out_of_gamut_is_reported_rather_than_silently_clipped() {
        let mut s = ColorState::new();
        assert!(!s.is_out_of_gamut());
        s.set_current([1.4, -0.2, 0.5, 1.0]);
        assert!(s.is_out_of_gamut());
        // ...but it is still shown as a legal hex.
        assert_eq!(s.hex(), "#FF0080");
    }

    #[test]
    fn writing_the_colour_it_already_holds_reports_no_change() {
        let mut s = ColorState::new();
        assert!(!s.set_current(DEFAULT_FOREGROUND));
        assert!(s.set_current([0.5, 0.5, 0.5, 1.0]));
        assert!(!s.set_current([0.5, 0.5, 0.5, 1.0]));
    }

    #[test]
    fn a_well_knows_its_opposite() {
        assert_eq!(ColorWell::Foreground.other(), ColorWell::Background);
        assert_eq!(ColorWell::Background.other(), ColorWell::Foreground);
        assert_eq!(ColorWell::Foreground.other().other(), ColorWell::Foreground);
    }

    #[test]
    fn every_notation_has_a_label() {
        for n in ColorNotation::ALL {
            assert!(!n.label().is_empty(), "{n:?}");
        }
        assert_eq!(ColorNotation::default(), ColorNotation::Hsb);
    }

    // ---- swatches ---------------------------------------------------------

    #[test]
    fn the_default_palette_is_a_ramp_plus_the_hue_wheel() {
        let s = SwatchesState::new();
        assert_eq!(s.len(), 18);
        assert_eq!(s.get(0).unwrap().rgba, [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(s.get(5).unwrap().rgba, [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(format_hex(s.get(6).unwrap().rgba), "#FF0000");
        assert!(s.swatches().iter().all(|w| !w.name.is_empty()));
    }

    #[test]
    fn the_same_colour_is_not_added_twice() {
        let mut s = SwatchesState::new();
        let before = s.len();
        assert!(!s.add("Black again", [0.0, 0.0, 0.0, 1.0]));
        assert_eq!(s.len(), before);
        assert!(s.add("Brand", [0.2, 0.4, 0.6, 1.0]));
        assert_eq!(s.len(), before + 1);
        assert!(!s.add("Brand copy", [0.2, 0.4, 0.6, 1.0]));
    }

    #[test]
    fn a_non_finite_swatch_is_refused() {
        let mut s = SwatchesState::new();
        let before = s.len();
        assert!(!s.add("Broken", [f32::NAN, 0.0, 0.0, 1.0]));
        assert_eq!(s.len(), before);
    }

    #[test]
    fn swatches_reorder_and_remove() {
        let mut s = SwatchesState::new();
        let first = s.get(0).unwrap().clone();
        assert!(s.reorder(0, 3));
        assert_eq!(s.get(3).unwrap(), &first);
        assert!(!s.reorder(0, 0));
        assert!(!s.reorder(99, 0));
        let removed = s.remove(3).expect("in range");
        assert_eq!(removed, first);
        assert_eq!(s.remove(999), None);
    }

    #[test]
    fn reordering_past_the_end_lands_on_the_end() {
        let mut s = SwatchesState::new();
        let first = s.get(0).unwrap().clone();
        let last = s.len() - 1;
        assert!(s.reorder(0, 9_999));
        assert_eq!(s.get(last).unwrap(), &first);
    }
}
