//! The colour picker.
//!
//! Four numeric models of the same colour are on screen at once (HSB, RGB, Lab,
//! hex) plus a two-dimensional field and two strips, and every one of them can
//! be edited. That only works if the conversions are exact inverses, so they
//! are pure functions over `color`'s conversions and are round-trip tested
//! across the whole cube rather than at a couple of sample points.
//!
//! # Hue is authoritative, not derived
//!
//! The dialog stores **HSBA**, not RGBA. Dragging the saturation/value field to
//! pure black and back has to return the hue the user started on, and black has
//! no hue to recover — a picker that derives H from RGB every frame snaps to
//! red the moment the value hits zero. Editing an RGB, Lab or hex field writes
//! HSB back through [`ColorValue::to_hsb`], which preserves the field's own hue
//! wherever the conversion is ambiguous.

use design::{
    color32, current_tokens,
    egui_theme::rounding,
    tokens::palette::ColorRole,
    tokens::{Radius, Space},
};
use egui::{pos2, vec2, Context, Mesh, Rect, Sense, Shape};

use super::action::DialogAction;
use super::chrome::{
    action_row_with_extras, caption, hairline, modal_with, Dialog, DialogButton, DialogKeys,
    DialogOutcome, DialogWidth, ModalStyle,
};
use super::controls::{checkbox_row, from_byte, numeric, swatch, to_byte};
use super::{ids, sizes};

/// The saturation/value square's fixed corners.
///
/// These are **not** theme colours and must not become palette entries: the
/// square *is* "white at zero saturation, the hue at full, black at zero
/// brightness". That is the definition of the HSB model the numeric fields
/// beside it report, and re-skinning it would make the picker show a colour
/// other than the one it hands back.
const SV_UNSATURATED: egui::Color32 = egui::Color32::WHITE; // design-exempt: defines the HSB square
const SV_DARK: egui::Color32 = egui::Color32::BLACK; // design-exempt: defines the HSB square

/// The two rings of the draggable marker.
///
/// Also outside the palette, and for the same kind of reason: the marker sits
/// on top of an arbitrary user-chosen colour, so it cannot be *any* fixed
/// theme colour and stay visible. A dark ring under a light one reads against
/// every background there is.
const MARKER_UNDER: egui::Color32 = egui::Color32::BLACK; // design-exempt: must read on any colour
const MARKER_OVER: egui::Color32 = egui::Color32::WHITE; // design-exempt: must read on any colour

/// A straight-alpha colour in encoded sRGB, all channels `0..=1`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ColorValue {
    pub rgba: [f32; 4],
}

impl Default for ColorValue {
    fn default() -> Self {
        Self::BLACK
    }
}

impl ColorValue {
    pub const BLACK: Self = Self {
        rgba: [0.0, 0.0, 0.0, 1.0],
    };
    pub const WHITE: Self = Self {
        rgba: [1.0, 1.0, 1.0, 1.0],
    };

    /// From RGBA in `0..=1`, clamped.
    pub fn new(rgba: [f32; 4]) -> Self {
        Self {
            rgba: [
                clamp01(rgba[0]),
                clamp01(rgba[1]),
                clamp01(rgba[2]),
                clamp01(rgba[3]),
            ],
        }
    }

    /// From 8-bit channels.
    pub fn from_bytes(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self::new([from_byte(r), from_byte(g), from_byte(b), from_byte(a)])
    }

    /// The 8-bit channels a file would store.
    pub fn to_bytes(self) -> [u8; 4] {
        [
            to_byte(self.rgba[0]),
            to_byte(self.rgba[1]),
            to_byte(self.rgba[2]),
            to_byte(self.rgba[3]),
        ]
    }

    /// Straight alpha, `0..=1`.
    pub fn alpha(self) -> f32 {
        self.rgba[3]
    }

    /// From hue (degrees), saturation, brightness and alpha, all `0..=1`
    /// except the hue.
    pub fn from_hsb(hsb: [f32; 3], alpha: f32) -> Self {
        let rgb = color::hsv_to_rgb([hsb[0], clamp01(hsb[1]), clamp01(hsb[2])]);
        Self::new([rgb[0], rgb[1], rgb[2], alpha])
    }

    /// Hue in `[0, 360)`, saturation and brightness in `0..=1`.
    ///
    /// Achromatic colours have no hue; `previous_hue` is returned for them so a
    /// slider does not jump to red when the user drags into the grey axis.
    pub fn to_hsb(self, previous_hue: f32) -> [f32; 3] {
        let hsb = color::rgb_to_hsv([self.rgba[0], self.rgba[1], self.rgba[2]]);
        if hsb[1] <= f32::EPSILON {
            [previous_hue, hsb[1], hsb[2]]
        } else {
            hsb
        }
    }

    /// CIELAB, `L*` in `0..=100`.
    pub fn to_lab(self) -> [f32; 3] {
        color::rgb_to_lab([self.rgba[0], self.rgba[1], self.rgba[2]])
    }

    /// From CIELAB. Out-of-gamut values are clipped into sRGB, which is what
    /// the numeric field has to do to stay a colour the document can hold.
    pub fn from_lab(lab: [f32; 3], alpha: f32) -> Self {
        let rgb = color::lab_to_rgb(lab);
        Self::new([rgb[0], rgb[1], rgb[2], alpha])
    }

    /// `RRGGBB`, or `RRGGBBAA` when `with_alpha`, without a leading `#`.
    pub fn to_hex(self, with_alpha: bool) -> String {
        let [r, g, b, a] = self.to_bytes();
        if with_alpha {
            format!("{r:02X}{g:02X}{b:02X}{a:02X}")
        } else {
            format!("{r:02X}{g:02X}{b:02X}")
        }
    }

    /// Parse `#RGB`, `#RGBA`, `#RRGGBB` or `#RRGGBBAA`; the `#` is optional and
    /// case does not matter. Anything else is `None`.
    ///
    /// A three-digit form expands by *repeating* the nibble (`F` -> `FF`), not
    /// by shifting, so `#FFF` is white rather than `#F0F0F0`.
    pub fn from_hex(text: &str) -> Option<Self> {
        let text = text.trim().trim_start_matches('#');
        if !text.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let nibble = |i: usize| u8::from_str_radix(&text[i..=i], 16).ok();
        let byte = |i: usize| u8::from_str_radix(&text[i..i + 2], 16).ok();
        match text.len() {
            3 | 4 => {
                let expand = |i: usize| nibble(i).map(|v| v * 17);
                Some(Self::from_bytes(
                    expand(0)?,
                    expand(1)?,
                    expand(2)?,
                    if text.len() == 4 { expand(3)? } else { 255 },
                ))
            }
            6 | 8 => Some(Self::from_bytes(
                byte(0)?,
                byte(2)?,
                byte(4)?,
                if text.len() == 8 { byte(6)? } else { 255 },
            )),
            _ => None,
        }
    }

    /// The nearest colour on the 6x6x6 web-safe cube, alpha untouched.
    pub fn web_safe(self) -> Self {
        let snap = |v: f32| {
            let step = 255.0 / 5.0;
            (f32::from(to_byte(v)) / step).round() * step / 255.0
        };
        Self::new([
            snap(self.rgba[0]),
            snap(self.rgba[1]),
            snap(self.rgba[2]),
            self.rgba[3],
        ])
    }

    /// Whether this colour is already on the web-safe cube.
    pub fn is_web_safe(self) -> bool {
        self.to_bytes()[..3].iter().all(|b| u16::from(*b) % 51 == 0)
    }
}

/// A bounded most-recent-first list of colours.
#[derive(Clone, Debug)]
pub struct RecentColors {
    colors: Vec<ColorValue>,
    limit: usize,
}

impl Default for RecentColors {
    fn default() -> Self {
        Self::with_limit(16)
    }
}

impl RecentColors {
    /// An empty list holding at most `limit` colours (at least one).
    pub fn with_limit(limit: usize) -> Self {
        Self {
            colors: Vec::new(),
            limit: limit.max(1),
        }
    }

    /// Most recent first.
    pub fn as_slice(&self) -> &[ColorValue] {
        &self.colors
    }

    /// Record a colour. A repeat moves to the front rather than appearing
    /// twice, and the oldest entry falls off the end.
    pub fn push(&mut self, color: ColorValue) {
        self.colors.retain(|c| c.to_bytes() != color.to_bytes());
        self.colors.insert(0, color);
        self.colors.truncate(self.limit);
    }
}

/// Something that can read a pixel off the physical screen.
///
/// The picker owns the *interaction* — arm, follow the pointer, commit — but
/// not the platform call, which needs a window handle the `ui` crate does not
/// have. The shell supplies one of these; the tests supply a fake, which is
/// what makes the eyedropper's behaviour checkable at all.
pub trait ScreenSampler {
    /// The colour at `screen_pos`, in physical screen points, or `None` when
    /// the point cannot be read.
    fn sample(&self, screen_pos: [f32; 2]) -> Option<[f32; 4]>;
}

/// Whether the eyedropper is waiting for a click.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Eyedropper {
    #[default]
    Idle,
    /// Armed: the next click anywhere picks up a colour.
    Armed,
}

/// Which numeric model a field belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum ColorModel {
    #[default]
    Hsb,
    Rgb,
    Lab,
}

impl ColorModel {
    /// All three, in tab order.
    pub const ALL: &'static [ColorModel] = &[Self::Hsb, Self::Rgb, Self::Lab];

    /// Tab label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Hsb => "HSB",
            Self::Rgb => "RGB",
            Self::Lab => "Lab",
        }
    }
}

/// The colour picker dialog.
#[derive(Clone, Debug)]
pub struct ColorPickerDialog {
    /// Authoritative hue in degrees; see the module docs.
    hue: f32,
    saturation: f32,
    brightness: f32,
    alpha: f32,
    /// The colour the dialog opened on, for the before/after swatch.
    original: ColorValue,
    model: ColorModel,
    hex: String,
    web_safe_only: bool,
    eyedropper: Eyedropper,
    recents: RecentColors,
}

impl Default for ColorPickerDialog {
    fn default() -> Self {
        Self::new(ColorValue::BLACK)
    }
}

impl ColorPickerDialog {
    /// Open on `color`.
    pub fn new(color: ColorValue) -> Self {
        let hsb = color.to_hsb(0.0);
        Self {
            hue: hsb[0],
            saturation: hsb[1],
            brightness: hsb[2],
            alpha: color.alpha(),
            original: color,
            model: ColorModel::Hsb,
            hex: color.to_hex(false),
            web_safe_only: false,
            eyedropper: Eyedropper::Idle,
            recents: RecentColors::default(),
        }
    }

    /// The colour the dialog opened on.
    pub fn original(&self) -> ColorValue {
        self.original
    }

    /// The colour currently chosen.
    pub fn color(&self) -> ColorValue {
        let raw = ColorValue::from_hsb([self.hue, self.saturation, self.brightness], self.alpha);
        if self.web_safe_only {
            raw.web_safe()
        } else {
            raw
        }
    }

    /// Hue in degrees, saturation and brightness in `0..=1`.
    pub fn hsb(&self) -> [f32; 3] {
        [self.hue, self.saturation, self.brightness]
    }

    /// Set the colour from any RGBA, keeping the current hue where the new
    /// colour is achromatic.
    pub fn set_color(&mut self, color: ColorValue) {
        let hsb = color.to_hsb(self.hue);
        self.hue = hsb[0];
        self.saturation = hsb[1];
        self.brightness = hsb[2];
        self.alpha = color.alpha();
        self.hex = self.color().to_hex(false);
    }

    /// Set hue/saturation/brightness directly. Hue wraps into `[0, 360)`.
    pub fn set_hsb(&mut self, hsb: [f32; 3]) {
        self.hue = hsb[0].rem_euclid(360.0);
        self.saturation = clamp01(hsb[1]);
        self.brightness = clamp01(hsb[2]);
        self.hex = self.color().to_hex(false);
    }

    /// Set the alpha channel.
    pub fn set_alpha(&mut self, alpha: f32) {
        self.alpha = clamp01(alpha);
    }

    /// Whether the picker snaps to the web-safe cube.
    pub fn web_safe_only(&self) -> bool {
        self.web_safe_only
    }

    /// Turn web-safe snapping on or off.
    pub fn set_web_safe_only(&mut self, on: bool) {
        self.web_safe_only = on;
        if on {
            // Fold the snap back into the stored HSB, so the strips and the
            // field show the colour the dialog will actually hand back.
            let snapped = self.color().web_safe();
            let hsb = snapped.to_hsb(self.hue);
            self.hue = hsb[0];
            self.saturation = hsb[1];
            self.brightness = hsb[2];
        }
        self.hex = self.color().to_hex(false);
    }

    /// The hex text as typed. Kept as a string so a half-typed value does not
    /// get rewritten under the cursor.
    pub fn hex_text(&self) -> &str {
        &self.hex
    }

    /// Apply hex text. Returns `false` when it does not parse, leaving the
    /// colour alone so the user can keep typing.
    pub fn set_hex_text(&mut self, text: impl Into<String>) -> bool {
        self.hex = text.into();
        match ColorValue::from_hex(&self.hex) {
            Some(color) => {
                let alpha = self.alpha;
                let hsb = color.to_hsb(self.hue);
                self.hue = hsb[0];
                self.saturation = hsb[1];
                self.brightness = hsb[2];
                self.alpha = alpha;
                true
            }
            None => false,
        }
    }

    /// Colours the user has committed to before, most recent first.
    pub fn recents(&self) -> &[ColorValue] {
        self.recents.as_slice()
    }

    /// Seed the recent list, e.g. from the persisted swatch history or from the
    /// last picker the same host opened.
    pub fn set_recents(&mut self, recents: RecentColors) {
        self.recents = recents;
    }

    /// The recent list itself, so a host can carry it into the next picker it
    /// opens. Without this the strip would reset every time the picker closes,
    /// which is the same as not having one.
    pub fn recent_colors(&self) -> &RecentColors {
        &self.recents
    }

    /// The eyedropper's state.
    pub fn eyedropper(&self) -> Eyedropper {
        self.eyedropper
    }

    /// Arm the eyedropper: the next [`ColorPickerDialog::sample_screen`] picks.
    pub fn arm_eyedropper(&mut self) {
        self.eyedropper = Eyedropper::Armed;
    }

    /// Disarm without picking (Escape while the eyedropper is up).
    pub fn cancel_eyedropper(&mut self) {
        self.eyedropper = Eyedropper::Idle;
    }

    /// Take a sample at `screen_pos`.
    ///
    /// A no-op unless the eyedropper is armed, and it disarms itself whether or
    /// not the sample succeeded — an eyedropper that stays up after a failed
    /// read traps every subsequent click.
    pub fn sample_screen(&mut self, screen_pos: [f32; 2], sampler: &dyn ScreenSampler) -> bool {
        if self.eyedropper != Eyedropper::Armed {
            return false;
        }
        self.eyedropper = Eyedropper::Idle;
        match sampler.sample(screen_pos) {
            Some(rgba) => {
                self.set_color(ColorValue::new(rgba));
                true
            }
            None => false,
        }
    }

    /// Why the eyedropper cannot be used, or `None` when it can.
    ///
    /// The picker owns the interaction but not the platform call, so without a
    /// [`ScreenSampler`] there is nothing for a click to read. Arming a mode
    /// that can never complete — telling the user to click, then doing nothing
    /// when they do — is worse than a disabled button, so the button is
    /// disabled and says this instead.
    pub fn eyedropper_unavailable(has_sampler: bool) -> Option<&'static str> {
        if has_sampler {
            None
        } else {
            Some(crate::strings::tr(
                "ui.color_picker.this.window.cannot.read.screen.pixels",
            ))
        }
    }

    /// Draw the dialog for one frame.
    ///
    /// `sampler` is how the eyedropper reads the screen; the shell supplies one
    /// if it can, and the button is drawn disabled when it cannot.
    pub fn show(
        &mut self,
        ctx: &Context,
        sampler: Option<&dyn ScreenSampler>,
    ) -> DialogOutcome<DialogAction> {
        self.show_impl(
            ctx,
            "color-picker",
            ModalStyle::centered(DialogWidth::Standard),
            sampler,
        )
    }

    /// Draw the picker as a surface opened from another dialog.
    ///
    /// Offset from the centre and without a second scrim, because the dialog
    /// that opened it already put one up. `id_salt` names the host so two hosts
    /// cannot share window state.
    pub fn show_nested(
        &mut self,
        ctx: &Context,
        id_salt: &'static str,
        sampler: Option<&dyn ScreenSampler>,
    ) -> DialogOutcome<DialogAction> {
        self.show_impl(
            ctx,
            id_salt,
            ModalStyle::nested(DialogWidth::Standard),
            sampler,
        )
    }

    fn show_impl(
        &mut self,
        ctx: &Context,
        id_salt: &'static str,
        style: ModalStyle,
        sampler: Option<&dyn ScreenSampler>,
    ) -> DialogOutcome<DialogAction> {
        let keys = DialogKeys::read(ctx);
        if keys.cancel && self.eyedropper == Eyedropper::Armed {
            // Escape gets out of the eyedropper first, not out of the dialog.
            self.cancel_eyedropper();
            return DialogOutcome::Open;
        }
        // The armed eyedropper takes this frame's click wherever it landed —
        // on the canvas, on a panel, on the dialog itself. `interact_pos` is
        // the pointer, not a widget, so the scrim swallowing the click for
        // every *widget* underneath does not hide it from here.
        if self.eyedropper == Eyedropper::Armed {
            match sampler {
                Some(sampler) => {
                    let pressed = ctx.input(|i| {
                        i.pointer
                            .primary_pressed()
                            .then(|| i.pointer.interact_pos())
                            .flatten()
                    });
                    if let Some(pos) = pressed {
                        self.sample_screen([pos.x, pos.y], sampler);
                    }
                }
                // Nothing can complete the mode; do not leave the user stuck
                // in it. The button that arms it is disabled, so this is a
                // sampler that went away mid-pick rather than a normal path.
                None => self.cancel_eyedropper(),
            }
        }
        let mut outcome = match super::chrome::resolve(self, keys) {
            DialogOutcome::Confirmed(action) => self.commit(action),
            other => other,
        };
        let has_sampler = sampler.is_some();
        let drawn = modal_with(ctx, id_salt, self.title(), None, style, |ui| {
            self.body(ui, has_sampler)
        });
        if let Some(Some(button)) = drawn {
            outcome = match button {
                DialogButton::Cancel => DialogOutcome::Cancelled,
                DialogButton::Confirm => match self.confirm() {
                    Some(action) => self.commit(action),
                    None => DialogOutcome::Open,
                },
                DialogButton::Extra(_) => {
                    self.arm_eyedropper();
                    DialogOutcome::Open
                }
            };
        }
        outcome
    }

    /// Close on `action`, recording the colour in the recent strip.
    ///
    /// Both confirm paths go through here — Enter, resolved before the dialog
    /// is even drawn, and the primary button, resolved after. They used not to:
    /// the recent colour was recorded in the button's arm only, so confirming
    /// with the keyboard produced a different dialog state from confirming with
    /// the mouse, in a module whose whole grammar is "Enter confirms exactly as
    /// the primary button does".
    fn commit(&mut self, action: DialogAction) -> DialogOutcome<DialogAction> {
        self.recents.push(self.color());
        DialogOutcome::Confirmed(action)
    }

    fn body(&mut self, ui: &mut egui::Ui, has_sampler: bool) -> Option<DialogButton> {
        let field = sizes::saturation_value_field();
        let strip = vec2(sizes::color_strip_width(), field.y);
        ui.horizontal_top(|ui| {
            self.saturation_value_field(ui, field);
            ui.add_space(Space::Small.pt());
            self.hue_strip(ui, strip);
            ui.add_space(Space::Small.pt());
            self.alpha_strip(ui, strip);
            ui.add_space(Space::Medium.pt());
            ui.vertical(|ui| self.numeric_fields(ui));
        });

        hairline(ui);
        ui.horizontal(|ui| {
            // "Before" is a control: clicking it puts the colour the dialog
            // opened on back, which is how a picker lets you compare and then
            // change your mind. "After" is a readout of what Choose will hand
            // back, so it does not sense clicks at all.
            if swatch(
                ui,
                ids::compare_swatch(false),
                self.original.rgba,
                sizes::swatch_compare(),
            )
            .on_hover_text(crate::strings::tr(
                "ui.color_picker.back.to.the.colour.this.opened",
            ))
            .clicked()
            {
                self.set_color(self.original);
            }
            super::controls::swatch_readonly(
                ui,
                ids::compare_swatch(true),
                self.color().rgba,
                sizes::swatch_compare(),
            );
            caption(ui, crate::strings::tr("ui.color_picker.before.after"));
        });

        let mut web_safe = self.web_safe_only;
        if checkbox_row(
            ui,
            crate::strings::tr("ui.color_picker.only.web.safe.colours"),
            &mut web_safe,
        )
        .changed()
        {
            self.set_web_safe_only(web_safe);
        }

        if !self.recents.as_slice().is_empty() {
            design::section_header(ui, "Recent");
            let recents: Vec<ColorValue> = self.recents.as_slice().to_vec();
            ui.horizontal_wrapped(|ui| {
                for (index, color) in recents.into_iter().enumerate() {
                    if swatch(
                        ui,
                        ids::recent_color(index),
                        color.rgba,
                        sizes::swatch_recent(),
                    )
                    .clicked()
                    {
                        self.set_color(color);
                    }
                }
            });
        }

        if self.eyedropper == Eyedropper::Armed {
            caption(
                ui,
                crate::strings::tr("ui.color_picker.click.anywhere.to.sample.a.colour"),
            );
        }

        ui.add_space(Space::Small.pt());
        action_row_with_extras(
            ui,
            self.confirm_label(),
            self.blocked_reason().as_deref(),
            &[("Eyedropper", Self::eyedropper_unavailable(has_sampler))],
        )
    }

    fn saturation_value_field(&mut self, ui: &mut egui::Ui, size: egui::Vec2) {
        let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
        if ui.is_rect_visible(rect) {
            let t = current_tokens(ui);
            let radius = Radius::Small.resolve(&t.radii, size.y);
            let hue_color = ColorValue::from_hsb([self.hue, 1.0, 1.0], 1.0);
            let mut mesh = Mesh::default();
            quad(
                &mut mesh,
                rect,
                [
                    SV_UNSATURATED,
                    super::controls::color_of(hue_color.rgba),
                    SV_DARK,
                    SV_DARK,
                ],
            );
            ui.painter().add(Shape::mesh(mesh));
            let marker = pos2(
                rect.left() + self.saturation * rect.width(),
                rect.top() + (1.0 - self.brightness) * rect.height(),
            );
            ring(ui, marker);
            ui.painter().rect_stroke(
                rect,
                rounding(radius),
                egui::Stroke::new(
                    t.borders.hairline,
                    color32(t.palette.color(ColorRole::ControlStroke)),
                ),
            );
        }
        if let Some(pos) = drag_position(&response) {
            let s = ((pos.x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0);
            let v = 1.0 - ((pos.y - rect.top()) / rect.height().max(1.0)).clamp(0.0, 1.0);
            self.set_hsb([self.hue, s, v]);
        }
    }

    fn hue_strip(&mut self, ui: &mut egui::Ui, size: egui::Vec2) {
        let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
        if ui.is_rect_visible(rect) {
            const STEPS: usize = 12;
            let mut mesh = Mesh::default();
            for step in 0..STEPS {
                let t0 = step as f32 / STEPS as f32;
                let t1 = (step + 1) as f32 / STEPS as f32;
                let band = Rect::from_min_max(
                    pos2(rect.left(), rect.top() + t0 * rect.height()),
                    pos2(rect.right(), rect.top() + t1 * rect.height()),
                );
                let top = super::controls::color_of(
                    ColorValue::from_hsb([t0 * 360.0, 1.0, 1.0], 1.0).rgba,
                );
                let bottom = super::controls::color_of(
                    ColorValue::from_hsb([t1 * 360.0, 1.0, 1.0], 1.0).rgba,
                );
                quad(&mut mesh, band, [top, top, bottom, bottom]);
            }
            ui.painter().add(Shape::mesh(mesh));
            let y = rect.top() + (self.hue / 360.0) * rect.height();
            ring(ui, pos2(rect.center().x, y));
        }
        if let Some(pos) = drag_position(&response) {
            let t = ((pos.y - rect.top()) / rect.height().max(1.0)).clamp(0.0, 0.999_9);
            self.set_hsb([t * 360.0, self.saturation, self.brightness]);
        }
    }

    fn alpha_strip(&mut self, ui: &mut egui::Ui, size: egui::Vec2) {
        let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
        if ui.is_rect_visible(rect) {
            let t = current_tokens(ui);
            let radius = Radius::Small.resolve(&t.radii, size.x);
            super::controls::checkerboard(ui, rect, radius);
            let opaque = super::controls::color_of([
                self.color().rgba[0],
                self.color().rgba[1],
                self.color().rgba[2],
                1.0,
            ]);
            let clear =
                egui::Color32::from_rgba_unmultiplied(opaque.r(), opaque.g(), opaque.b(), 0);
            let mut mesh = Mesh::default();
            quad(&mut mesh, rect, [opaque, opaque, clear, clear]);
            ui.painter().add(Shape::mesh(mesh));
            let y = rect.top() + (1.0 - self.alpha) * rect.height();
            ring(ui, pos2(rect.center().x, y));
        }
        if let Some(pos) = drag_position(&response) {
            let t = ((pos.y - rect.top()) / rect.height().max(1.0)).clamp(0.0, 1.0);
            self.set_alpha(1.0 - t);
        }
    }

    fn numeric_fields(&mut self, ui: &mut egui::Ui) {
        let mut model = self.model;
        let mut index = ColorModel::ALL
            .iter()
            .position(|m| *m == model)
            .unwrap_or(0);
        if design::segmented_control(ui, "cp-model", &mut index, &["HSB", "RGB", "Lab"]) {
            model = ColorModel::ALL[index];
            self.model = model;
        }
        ui.add_space(Space::Small.pt());
        match self.model {
            ColorModel::Hsb => {
                let mut hsb = [
                    f64::from(self.hue),
                    f64::from(self.saturation) * 100.0,
                    f64::from(self.brightness) * 100.0,
                ];
                let mut changed = false;
                design::inspector_field(ui, "H", |ui| {
                    changed |= numeric(ui, &mut hsb[0], 0.0..=360.0, 1, "\u{b0}").changed();
                });
                design::inspector_field(ui, "S", |ui| {
                    changed |= numeric(ui, &mut hsb[1], 0.0..=100.0, 1, "%").changed();
                });
                design::inspector_field(ui, "B", |ui| {
                    changed |= numeric(ui, &mut hsb[2], 0.0..=100.0, 1, "%").changed();
                });
                if changed {
                    self.set_hsb([
                        hsb[0] as f32,
                        (hsb[1] / 100.0) as f32,
                        (hsb[2] / 100.0) as f32,
                    ]);
                }
            }
            ColorModel::Rgb => {
                let bytes = self.color().to_bytes();
                let mut values = [
                    i64::from(bytes[0]),
                    i64::from(bytes[1]),
                    i64::from(bytes[2]),
                ];
                let mut changed = false;
                for (index, label) in ["R", "G", "B"].into_iter().enumerate() {
                    design::inspector_field(ui, label, |ui| {
                        changed |=
                            super::controls::integer(ui, &mut values[index], 0..=255).changed();
                    });
                }
                if changed {
                    self.set_color(ColorValue::from_bytes(
                        values[0] as u8,
                        values[1] as u8,
                        values[2] as u8,
                        bytes[3],
                    ));
                }
            }
            ColorModel::Lab => {
                let lab = self.color().to_lab();
                let mut values = [f64::from(lab[0]), f64::from(lab[1]), f64::from(lab[2])];
                let mut changed = false;
                design::inspector_field(ui, "L", |ui| {
                    changed |= numeric(ui, &mut values[0], 0.0..=100.0, 2, "").changed();
                });
                design::inspector_field(ui, "a", |ui| {
                    changed |= numeric(ui, &mut values[1], -128.0..=127.0, 2, "").changed();
                });
                design::inspector_field(ui, "b", |ui| {
                    changed |= numeric(ui, &mut values[2], -128.0..=127.0, 2, "").changed();
                });
                if changed {
                    self.set_color(ColorValue::from_lab(
                        [values[0] as f32, values[1] as f32, values[2] as f32],
                        self.alpha,
                    ));
                }
            }
        }
        design::inspector_field(ui, "Alpha", |ui| {
            let mut alpha = f64::from(self.alpha) * 100.0;
            if numeric(ui, &mut alpha, 0.0..=100.0, 1, "%").changed() {
                self.set_alpha((alpha / 100.0) as f32);
            }
        });
        design::inspector_field(ui, "Hex", |ui| {
            let mut text = self.hex.clone();
            if ui
                .add(egui::TextEdit::singleline(&mut text).desired_width(sizes::text_field_short()))
                .changed()
            {
                self.set_hex_text(text);
            }
        });
        if ColorValue::from_hex(&self.hex).is_none() {
            caption(ui, crate::strings::tr("ui.color_picker.not.a.hex.colour"));
        }
    }
}

impl Dialog for ColorPickerDialog {
    fn title(&self) -> &'static str {
        crate::strings::tr("ui.color_picker.color.picker")
    }

    fn confirm_label(&self) -> &'static str {
        "Choose"
    }

    fn confirm(&self) -> Option<DialogAction> {
        Some(DialogAction::SetColor(self.color()))
    }
}

fn clamp01(value: f32) -> f32 {
    if value.is_nan() {
        0.0
    } else {
        value.clamp(0.0, 1.0)
    }
}

/// Two triangles with a colour per corner: top-left, top-right, bottom-left,
/// bottom-right.
fn quad(mesh: &mut Mesh, rect: Rect, corners: [egui::Color32; 4]) {
    let base = mesh.vertices.len() as u32;
    for (pos, color) in [
        (rect.left_top(), corners[0]),
        (rect.right_top(), corners[1]),
        (rect.left_bottom(), corners[2]),
        (rect.right_bottom(), corners[3]),
    ] {
        mesh.colored_vertex(pos, color);
    }
    mesh.add_triangle(base, base + 1, base + 2);
    mesh.add_triangle(base + 1, base + 3, base + 2);
}

/// The draggable marker: a light ring over a dark ring, so it stays visible on
/// any colour underneath without needing to know what that colour is.
///
/// Its two colours are deliberately outside the palette (see [`MARKER_UNDER`]);
/// its two widths are not — they are the design system's own border weights, so
/// the marker thickens with the rest of the chrome if those ever move.
fn ring(ui: &egui::Ui, center: egui::Pos2) {
    let t = current_tokens(ui);
    let radius = Space::Small.pt();
    let painter = ui.painter();
    painter.circle_stroke(
        center,
        radius,
        egui::Stroke::new(t.borders.thick, MARKER_UNDER),
    );
    painter.circle_stroke(
        center,
        radius,
        egui::Stroke::new(t.borders.hairline, MARKER_OVER),
    );
}

fn drag_position(response: &egui::Response) -> Option<egui::Pos2> {
    if response.is_pointer_button_down_on() || response.clicked() || response.dragged() {
        response.interact_pointer_pos()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialogs::chrome::test_support::{frame_both_themes, Harness};

    /// A spread of colours: the greys, the primaries, and a lattice through the
    /// interior of the cube.
    fn sample_colors() -> Vec<ColorValue> {
        let mut out = vec![
            ColorValue::BLACK,
            ColorValue::WHITE,
            ColorValue::from_bytes(255, 0, 0, 255),
            ColorValue::from_bytes(0, 255, 0, 255),
            ColorValue::from_bytes(0, 0, 255, 255),
            ColorValue::from_bytes(128, 128, 128, 255),
        ];
        for r in (0..=255).step_by(37) {
            for g in (0..=255).step_by(53) {
                for b in (0..=255).step_by(61) {
                    out.push(ColorValue::from_bytes(r, g, b, 255));
                }
            }
        }
        out
    }

    #[test]
    fn hsb_round_trips_every_sample_colour() {
        for color in sample_colors() {
            let hsb = color.to_hsb(0.0);
            let back = ColorValue::from_hsb(hsb, color.alpha());
            assert_eq!(
                back.to_bytes(),
                color.to_bytes(),
                "{:?} -> hsb {hsb:?} -> {:?}",
                color.to_bytes(),
                back.to_bytes()
            );
        }
    }

    #[test]
    fn lab_round_trips_every_sample_colour_within_a_code() {
        for color in sample_colors() {
            let lab = color.to_lab();
            let back = ColorValue::from_lab(lab, color.alpha());
            let (a, b) = (color.to_bytes(), back.to_bytes());
            for channel in 0..3 {
                let delta = i16::from(a[channel]) - i16::from(b[channel]);
                assert!(
                    delta.abs() <= 1,
                    "{a:?} -> lab {lab:?} -> {b:?} differs by {delta} on channel {channel}"
                );
            }
        }
    }

    #[test]
    fn hex_round_trips_exactly_with_and_without_alpha() {
        for color in sample_colors() {
            for with_alpha in [false, true] {
                let text = color.to_hex(with_alpha);
                let back = ColorValue::from_hex(&text).expect("our own hex parses");
                assert_eq!(back.to_bytes()[..3], color.to_bytes()[..3], "{text}");
                if with_alpha {
                    assert_eq!(back.to_bytes()[3], color.to_bytes()[3], "{text}");
                }
            }
        }
    }

    #[test]
    fn hex_accepts_the_short_forms_and_the_hash() {
        assert_eq!(
            ColorValue::from_hex("#FFF").unwrap().to_bytes(),
            [255, 255, 255, 255]
        );
        assert_eq!(
            ColorValue::from_hex("abc").unwrap().to_bytes(),
            [0xAA, 0xBB, 0xCC, 255]
        );
        assert_eq!(
            ColorValue::from_hex("  #1a2b3c  ").unwrap().to_bytes(),
            [0x1A, 0x2B, 0x3C, 255]
        );
        assert_eq!(
            ColorValue::from_hex("#1234").unwrap().to_bytes(),
            [0x11, 0x22, 0x33, 0x44]
        );
    }

    #[test]
    fn hex_rejects_anything_that_is_not_a_colour() {
        for bad in [
            "",
            "#",
            "12",
            "12345",
            "1234567",
            "#GGGGGG",
            "hello",
            "#12 34 56",
        ] {
            assert!(ColorValue::from_hex(bad).is_none(), "{bad:?} parsed");
        }
    }

    #[test]
    fn the_conversions_agree_across_all_four_models() {
        // The point of the picker: type into any field and the others follow.
        for color in sample_colors() {
            let hsb = color.to_hsb(0.0);
            let via_hsb = ColorValue::from_hsb(hsb, 1.0);
            let via_hex = ColorValue::from_hex(&color.to_hex(false)).unwrap();
            assert_eq!(via_hsb.to_bytes(), via_hex.to_bytes());
            let via_lab = ColorValue::from_lab(color.to_lab(), 1.0);
            for channel in 0..3 {
                let delta =
                    i16::from(via_lab.to_bytes()[channel]) - i16::from(via_hex.to_bytes()[channel]);
                assert!(delta.abs() <= 1);
            }
        }
    }

    #[test]
    fn dragging_through_black_and_back_keeps_the_hue() {
        let mut dialog = ColorPickerDialog::new(ColorValue::from_bytes(0, 128, 255, 255));
        let hue = dialog.hsb()[0];
        dialog.set_hsb([hue, dialog.hsb()[1], 0.0]);
        assert_eq!(dialog.color().to_bytes()[..3], [0, 0, 0]);
        dialog.set_hsb([dialog.hsb()[0], dialog.hsb()[1], 1.0]);
        assert!(
            (dialog.hsb()[0] - hue).abs() < 1e-3,
            "hue drifted to {}",
            dialog.hsb()[0]
        );
    }

    #[test]
    fn setting_an_achromatic_colour_keeps_the_slider_where_it_was() {
        let mut dialog = ColorPickerDialog::new(ColorValue::from_bytes(0, 128, 255, 255));
        let hue = dialog.hsb()[0];
        dialog.set_color(ColorValue::from_bytes(80, 80, 80, 255));
        assert_eq!(dialog.hsb()[0], hue);
    }

    #[test]
    fn the_hue_wraps_rather_than_clamping() {
        let mut dialog = ColorPickerDialog::default();
        dialog.set_hsb([420.0, 1.0, 1.0]);
        assert!((dialog.hsb()[0] - 60.0).abs() < 1e-4);
        dialog.set_hsb([-30.0, 1.0, 1.0]);
        assert!((dialog.hsb()[0] - 330.0).abs() < 1e-4);
    }

    #[test]
    fn web_safe_snapping_lands_on_the_cube() {
        for color in sample_colors() {
            let snapped = color.web_safe();
            assert!(snapped.is_web_safe(), "{:?}", snapped.to_bytes());
            for channel in 0..3 {
                let delta =
                    i16::from(snapped.to_bytes()[channel]) - i16::from(color.to_bytes()[channel]);
                assert!(delta.abs() <= 26, "snapped further than half a step");
            }
        }
    }

    #[test]
    fn web_safe_mode_changes_what_the_dialog_hands_back() {
        let mut dialog = ColorPickerDialog::new(ColorValue::from_bytes(200, 100, 50, 255));
        assert_eq!(dialog.color().to_bytes()[..3], [200, 100, 50]);
        dialog.set_web_safe_only(true);
        assert!(dialog.color().is_web_safe());
        assert_eq!(dialog.color().to_bytes()[..3], [204, 102, 51]);
    }

    #[test]
    fn alpha_survives_a_hex_edit() {
        let mut dialog = ColorPickerDialog::new(ColorValue::new([1.0, 0.0, 0.0, 0.5]));
        assert!(dialog.set_hex_text("00FF00"));
        assert!((dialog.color().alpha() - 0.5).abs() < 1e-6);
        assert_eq!(dialog.color().to_bytes()[..3], [0, 255, 0]);
    }

    #[test]
    fn a_half_typed_hex_does_not_change_the_colour() {
        // Five digits is no colour at all: three, four, six and eight are the
        // legal lengths, so a value mid-typing between six and eight is exactly
        // the case the field must sit still for.
        let mut dialog = ColorPickerDialog::new(ColorValue::WHITE);
        assert!(!dialog.set_hex_text("00FF0"));
        assert_eq!(dialog.color().to_bytes(), [255, 255, 255, 255]);
        assert_eq!(dialog.hex_text(), "00FF0");
    }

    #[test]
    fn recents_are_most_recent_first_deduplicated_and_bounded() {
        let mut recents = RecentColors::with_limit(3);
        recents.push(ColorValue::from_bytes(1, 0, 0, 255));
        recents.push(ColorValue::from_bytes(2, 0, 0, 255));
        recents.push(ColorValue::from_bytes(1, 0, 0, 255));
        assert_eq!(recents.as_slice().len(), 2);
        assert_eq!(recents.as_slice()[0].to_bytes()[0], 1);
        recents.push(ColorValue::from_bytes(3, 0, 0, 255));
        recents.push(ColorValue::from_bytes(4, 0, 0, 255));
        assert_eq!(recents.as_slice().len(), 3);
        assert_eq!(
            recents
                .as_slice()
                .iter()
                .map(|c| c.to_bytes()[0])
                .collect::<Vec<_>>(),
            vec![4, 3, 1]
        );
    }

    struct FakeScreen(Option<[f32; 4]>);
    impl ScreenSampler for FakeScreen {
        fn sample(&self, _pos: [f32; 2]) -> Option<[f32; 4]> {
            self.0
        }
    }

    #[test]
    fn the_eyedropper_only_samples_while_armed() {
        let mut dialog = ColorPickerDialog::new(ColorValue::WHITE);
        let screen = FakeScreen(Some([0.0, 0.0, 1.0, 1.0]));
        assert!(!dialog.sample_screen([10.0, 10.0], &screen));
        assert_eq!(dialog.color().to_bytes(), [255, 255, 255, 255]);

        dialog.arm_eyedropper();
        assert_eq!(dialog.eyedropper(), Eyedropper::Armed);
        assert!(dialog.sample_screen([10.0, 10.0], &screen));
        assert_eq!(dialog.color().to_bytes()[..3], [0, 0, 255]);
        assert_eq!(dialog.eyedropper(), Eyedropper::Idle);
    }

    #[test]
    fn a_failed_sample_still_disarms_the_eyedropper() {
        let mut dialog = ColorPickerDialog::new(ColorValue::WHITE);
        dialog.arm_eyedropper();
        assert!(!dialog.sample_screen([0.0, 0.0], &FakeScreen(None)));
        assert_eq!(dialog.eyedropper(), Eyedropper::Idle);
        assert_eq!(dialog.color().to_bytes(), [255, 255, 255, 255]);
    }

    #[test]
    fn cancelling_the_eyedropper_leaves_the_colour_alone() {
        let mut dialog = ColorPickerDialog::new(ColorValue::WHITE);
        dialog.arm_eyedropper();
        dialog.cancel_eyedropper();
        assert!(!dialog.sample_screen([0.0, 0.0], &FakeScreen(Some([0.0; 4]))));
        assert_eq!(dialog.color().to_bytes(), [255, 255, 255, 255]);
    }

    #[test]
    fn confirm_carries_the_colour_and_cancel_carries_nothing() {
        let dialog = ColorPickerDialog::new(ColorValue::from_bytes(10, 20, 30, 40));
        match dialog.confirm() {
            Some(DialogAction::SetColor(color)) => {
                assert_eq!(color.to_bytes(), [10, 20, 30, 40]);
            }
            other => panic!("expected a colour, got {other:?}"),
        }
        assert_eq!(
            super::super::chrome::resolve(&dialog, DialogKeys::CANCEL),
            DialogOutcome::Cancelled
        );
    }

    #[test]
    fn confirming_with_the_keyboard_records_a_recent_colour_too() {
        // The defect this pins: the chosen colour was pushed onto the recent
        // list inside the primary button's arm only. `resolve` answers Enter
        // before the dialog is drawn and the action row then reports no press,
        // so Enter confirmed the same colour and recorded nothing — two
        // different results for one user action, in a module whose stated
        // grammar is that Enter confirms exactly as the button does.
        let h = Harness::new();
        let chosen = ColorValue::from_bytes(12, 200, 90, 255);
        let mut dialog = ColorPickerDialog::new(chosen);
        assert!(dialog.recents().is_empty());

        let mut outcome = DialogOutcome::Open;
        h.frame(Harness::key_events(egui::Key::Enter), |ctx| {
            outcome = dialog.show(ctx, None);
        });
        assert!(
            matches!(outcome, DialogOutcome::Confirmed(_)),
            "Enter did not confirm"
        );
        assert_eq!(
            dialog.recents().iter().map(|c| c.to_bytes()).next(),
            Some(chosen.to_bytes()),
            "the keyboard path skipped the recent list"
        );
    }

    #[test]
    fn confirming_with_the_button_records_the_same_recent_colour() {
        let h = Harness::new();
        let chosen = ColorValue::from_bytes(12, 200, 90, 255);
        let mut keyboard = ColorPickerDialog::new(chosen);
        h.frame(Harness::key_events(egui::Key::Enter), |ctx| {
            keyboard.show(ctx, None);
        });

        let mut mouse = ColorPickerDialog::new(chosen);
        let mut confirmed = false;
        // The action row's primary button is the trailing control; drive it the
        // way the shell would, by resolving the same action the row reports.
        h.frame(Vec::new(), |ctx| {
            mouse.show(ctx, None);
        });
        if let Some(action) = mouse.confirm() {
            confirmed = matches!(mouse.commit(action), DialogOutcome::Confirmed(_));
        }
        assert!(confirmed, "the button path produced no action");
        assert_eq!(
            keyboard
                .recents()
                .iter()
                .map(|c| c.to_bytes())
                .collect::<Vec<_>>(),
            mouse
                .recents()
                .iter()
                .map(|c| c.to_bytes())
                .collect::<Vec<_>>(),
            "the keyboard and the button leave different state behind"
        );
    }

    #[test]
    fn every_numeric_model_draws_in_both_appearances() {
        for model in ColorModel::ALL {
            frame_both_themes(|ctx| {
                let mut dialog = ColorPickerDialog::new(ColorValue::from_bytes(30, 90, 200, 128));
                dialog.model = *model;
                dialog.arm_eyedropper();
                dialog.recents.push(ColorValue::WHITE);
                assert!(dialog.show(ctx, None).is_open());
            });
        }
    }

    #[test]
    fn a_drawn_frame_samples_the_screen_where_the_user_clicked() {
        // The defect this pins: `show` had no parameter to pass a sampler
        // through, so `sample_screen` had no caller outside the tests. The
        // Eyedropper button armed a mode that nothing could ever complete.
        let h = Harness::new();
        let screen = FakeScreen(Some([0.0, 0.0, 1.0, 1.0]));
        let mut dialog = ColorPickerDialog::new(ColorValue::WHITE);
        dialog.arm_eyedropper();

        h.frame(Vec::new(), |ctx| {
            assert!(dialog.show(ctx, Some(&screen)).is_open());
        });
        assert_eq!(dialog.eyedropper(), Eyedropper::Armed, "it disarmed itself");

        let at = egui::pos2(900.0, 700.0);
        h.frame(Harness::press_events(at), |ctx| {
            assert!(dialog.show(ctx, Some(&screen)).is_open());
        });
        assert_eq!(dialog.color().to_bytes()[..3], [0, 0, 255]);
        assert_eq!(dialog.eyedropper(), Eyedropper::Idle);
    }

    #[test]
    fn an_unarmed_picker_ignores_clicks_even_with_a_sampler() {
        let h = Harness::new();
        let screen = FakeScreen(Some([0.0, 0.0, 1.0, 1.0]));
        let mut dialog = ColorPickerDialog::new(ColorValue::WHITE);
        h.frame(Vec::new(), |ctx| {
            dialog.show(ctx, Some(&screen));
        });
        h.frame(Harness::press_events(egui::pos2(900.0, 700.0)), |ctx| {
            dialog.show(ctx, Some(&screen));
        });
        assert_eq!(dialog.color().to_bytes(), [255, 255, 255, 255]);
    }

    #[test]
    fn without_a_sampler_the_eyedropper_says_why_and_never_arms() {
        assert!(ColorPickerDialog::eyedropper_unavailable(true).is_none());
        let reason = ColorPickerDialog::eyedropper_unavailable(false)
            .expect("a disabled control must say why");
        assert!(reason.len() > 12, "{reason:?} explains nothing");

        // Even if something armed it, a frame with no sampler gets the user out
        // of the mode instead of leaving them clicking at nothing.
        let h = Harness::new();
        let mut dialog = ColorPickerDialog::new(ColorValue::WHITE);
        dialog.arm_eyedropper();
        h.frame(Vec::new(), |ctx| {
            dialog.show(ctx, None);
        });
        assert_eq!(dialog.eyedropper(), Eyedropper::Idle);
    }

    #[test]
    fn clicking_a_recent_colour_adopts_it() {
        let h = Harness::new();
        let mut dialog = ColorPickerDialog::new(ColorValue::WHITE);
        dialog
            .recents
            .push(ColorValue::from_bytes(10, 200, 30, 255));
        h.click_widget(ids::recent_color(0), |ctx| {
            dialog.show(ctx, None);
        });
        assert_eq!(dialog.color().to_bytes()[..3], [10, 200, 30]);
    }

    #[test]
    fn clicking_the_before_swatch_restores_the_colour_it_opened_on() {
        let h = Harness::new();
        let mut dialog = ColorPickerDialog::new(ColorValue::from_bytes(200, 30, 60, 255));
        dialog.set_color(ColorValue::from_bytes(10, 220, 90, 255));
        assert_eq!(dialog.color().to_bytes()[..3], [10, 220, 90]);
        h.click_widget(ids::compare_swatch(false), |ctx| {
            dialog.show(ctx, None);
        });
        assert_eq!(dialog.color().to_bytes()[..3], [200, 30, 60]);
    }

    #[test]
    fn the_after_swatch_is_a_readout_and_does_not_sense_clicks() {
        // It shows what Choose will hand back. Sensing a click it cannot act on
        // is the failure this whole module is trying not to repeat.
        let h = Harness::new();
        let mut dialog = ColorPickerDialog::new(ColorValue::from_bytes(200, 30, 60, 255));
        dialog.set_color(ColorValue::from_bytes(10, 220, 90, 255));
        h.click_widget(ids::compare_swatch(true), |ctx| {
            dialog.show(ctx, None);
        });
        assert_eq!(
            dialog.color().to_bytes()[..3],
            [10, 220, 90],
            "the readout acted like a control"
        );
        let response = h
            .ctx
            .read_response(ids::compare_swatch(true))
            .expect("it is drawn");
        assert!(!response.sense.click, "the readout senses clicks");
    }

    #[test]
    fn the_nested_picker_draws_without_a_second_scrim() {
        frame_both_themes(|ctx| {
            let mut dialog = ColorPickerDialog::new(ColorValue::from_bytes(1, 2, 3, 255));
            assert!(dialog.show_nested(ctx, "host", None).is_open());
        });
    }
}
