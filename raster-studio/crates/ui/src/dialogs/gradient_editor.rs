//! The gradient editor.
//!
//! Two independent ramps share one position axis: colour stops below the bar,
//! opacity stops above it. Both are kept **sorted by position at all times** and
//! both are kept at **two stops or more**, because a ramp with one stop is not a
//! ramp and a ramp whose stops are out of order draws backwards. Those two
//! invariants are enforced by every mutator here rather than checked at the end,
//! which is what lets a stop be dragged straight through its neighbour.
//!
//! # Why a stop has a key as well as an index
//!
//! Sorting on every move means an index is a *position in the list*, not a
//! stop. The moment a dragged stop crosses a neighbour, index 0 and index 1
//! swap contents. `egui` routes an in-flight pointer drag by widget id, so a
//! handle whose id contains its index goes on receiving the drag after the
//! swap — and now addresses the neighbour, which follows the pointer as well.
//! Both stops move; the user grabbed one. That is exactly the bug this module
//! shipped with.
//!
//! So every stop also carries a [`StopKey`], minted once and carried through
//! every re-sort, and the handle's widget id is keyed off that. The index still
//! names a stop *within a frame* — the inspector, the selection and the
//! mutators all take one — but nothing that has to survive a frame boundary
//! does.

use design::{
    color32, current_tokens,
    egui_theme::rounding,
    tokens::palette::ColorRole,
    tokens::{Radius, Space},
};
use egui::{pos2, vec2, Context, Mesh, Rect, Sense, Shape};
use layer_model::{Gradient, GradientStop, Rgba};

use super::action::DialogAction;
use super::chrome::{
    action_row, caption, hairline, modal_with, Dialog, DialogButton, DialogKeys, DialogOutcome,
    DialogWidth, ModalStyle,
};
use super::color_edit::ColorEdit;
use super::color_picker::{ColorValue, ScreenSampler};
use super::controls::{color_of, numeric, swatch};
use super::{ids, sizes};

/// The fewest stops a ramp may have.
pub const MIN_STOPS: usize = 2;

/// Which of the two ramps a stop belongs to.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum StopKind {
    /// The colour ramp, drawn under the bar.
    #[default]
    Color,
    /// The opacity ramp, drawn over the bar. Only the alpha channel of its
    /// stops is meaningful.
    Opacity,
}

impl StopKind {
    /// Both ramps.
    pub const ALL: &'static [StopKind] = &[Self::Color, Self::Opacity];

    /// Label for the inspector.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Color => "Color",
            Self::Opacity => "Opacity",
        }
    }
}

/// Which stop the inspector is editing.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct StopRef {
    pub kind: StopKind,
    pub index: usize,
}

/// A stop's identity, unchanged by the re-sorts that dragging causes.
///
/// Unique within one dialog and meaningless outside it: it exists so a widget
/// id, which must be the same on the next frame as it was on this one, can name
/// a stop rather than a slot. See the module docs.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct StopKey(u64);

/// A named starting ramp.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct GradientPreset {
    pub name: &'static str,
    /// `(position, rgba)` pairs, already in order.
    pub stops: &'static [(f32, Rgba)],
}

/// The built-in ramps.
pub const PRESETS: &[GradientPreset] = &[
    GradientPreset {
        name: "Black to White",
        stops: &[(0.0, [0.0, 0.0, 0.0, 1.0]), (1.0, [1.0, 1.0, 1.0, 1.0])],
    },
    GradientPreset {
        name: "White to Black",
        stops: &[(0.0, [1.0, 1.0, 1.0, 1.0]), (1.0, [0.0, 0.0, 0.0, 1.0])],
    },
    GradientPreset {
        name: "Black to Transparent",
        stops: &[(0.0, [0.0, 0.0, 0.0, 1.0]), (1.0, [0.0, 0.0, 0.0, 0.0])],
    },
    GradientPreset {
        name: "Spectrum",
        stops: &[
            (0.0, [1.0, 0.0, 0.0, 1.0]),
            (0.17, [1.0, 1.0, 0.0, 1.0]),
            (0.33, [0.0, 1.0, 0.0, 1.0]),
            (0.5, [0.0, 1.0, 1.0, 1.0]),
            (0.67, [0.0, 0.0, 1.0, 1.0]),
            (0.83, [1.0, 0.0, 1.0, 1.0]),
            (1.0, [1.0, 0.0, 0.0, 1.0]),
        ],
    },
    GradientPreset {
        name: "Sunset",
        stops: &[
            (0.0, [0.15, 0.09, 0.30, 1.0]),
            (0.45, [0.85, 0.29, 0.31, 1.0]),
            (0.75, [0.98, 0.62, 0.24, 1.0]),
            (1.0, [1.0, 0.90, 0.62, 1.0]),
        ],
    },
    GradientPreset {
        name: "Copper",
        stops: &[
            (0.0, [0.08, 0.04, 0.02, 1.0]),
            (0.5, [0.72, 0.45, 0.20, 1.0]),
            (1.0, [1.0, 0.87, 0.72, 1.0]),
        ],
    },
];

/// The gradient editor dialog.
#[derive(Clone, Debug)]
pub struct GradientEditorDialog {
    gradient: Gradient,
    /// Identities for `gradient.stops`, same length and same order.
    color_keys: Vec<StopKey>,
    /// Identities for `gradient.alpha_stops`, same length and same order.
    alpha_keys: Vec<StopKey>,
    /// Next identity to hand out. Never reused, so a key that has been dropped
    /// can never come back attached to a different stop.
    next_key: u64,
    selected: StopRef,
    preset: Option<usize>,
    /// The nested colour picker, when a stop's swatch is clicked.
    color_edit: ColorEdit<StopRef>,
}

impl Default for GradientEditorDialog {
    fn default() -> Self {
        Self::new(Gradient::default())
    }
}

impl GradientEditorDialog {
    /// Open on `gradient`, repairing it first.
    ///
    /// A gradient read from a document may have unsorted stops, out-of-range
    /// positions, or an empty alpha ramp (which `layer_model` defines as "use
    /// the colour stops' own alpha"). The editor needs both ramps materialised
    /// and ordered, so that normalisation happens once, here.
    pub fn new(gradient: Gradient) -> Self {
        let mut dialog = Self {
            gradient,
            color_keys: Vec::new(),
            alpha_keys: Vec::new(),
            next_key: 0,
            selected: StopRef::default(),
            preset: None,
            color_edit: ColorEdit::new(),
        };
        if dialog.gradient.alpha_stops.is_empty() {
            dialog.gradient.alpha_stops = dialog
                .gradient
                .stops
                .iter()
                .map(|s| GradientStop {
                    position: s.position,
                    color: [0.0, 0.0, 0.0, s.color[3]],
                    midpoint: s.midpoint,
                })
                .collect();
        }
        for kind in StopKind::ALL {
            let stops = dialog.stops_mut(*kind);
            while stops.len() < MIN_STOPS {
                let fill = stops.last().cloned().unwrap_or_default();
                stops.push(GradientStop {
                    position: 1.0,
                    ..fill
                });
            }
            for stop in stops.iter_mut() {
                stop.position = clamp01(stop.position);
                stop.midpoint = clamp01(stop.midpoint);
            }
            sort_stops(stops);
            dialog.reseed_keys(*kind);
        }
        dialog
    }

    /// Mint a key nothing has held before.
    fn fresh_key(&mut self) -> StopKey {
        let key = StopKey(self.next_key);
        self.next_key += 1;
        key
    }

    /// Give every stop of one ramp a brand-new identity.
    ///
    /// Only for the two moments a ramp is *replaced* wholesale — opening, and
    /// loading a preset. Every other mutator carries the existing keys along,
    /// because a stop the user is dragging must not change identity underneath
    /// them.
    fn reseed_keys(&mut self, kind: StopKind) {
        let count = self.stops(kind).len();
        let fresh: Vec<StopKey> = (0..count).map(|_| self.fresh_key()).collect();
        *self.keys_mut(kind) = fresh;
    }

    /// The identities of one ramp's stops, in list order.
    pub fn keys(&self, kind: StopKind) -> &[StopKey] {
        match kind {
            StopKind::Color => &self.color_keys,
            StopKind::Opacity => &self.alpha_keys,
        }
    }

    fn keys_mut(&mut self, kind: StopKind) -> &mut Vec<StopKey> {
        match kind {
            StopKind::Color => &mut self.color_keys,
            StopKind::Opacity => &mut self.alpha_keys,
        }
    }

    /// The identity of the stop currently at `index`.
    pub fn stop_key(&self, kind: StopKind, index: usize) -> Option<StopKey> {
        self.keys(kind).get(index).copied()
    }

    /// Where the stop with `key` sits right now, if it is still in the ramp.
    pub fn index_of_key(&self, kind: StopKind, key: StopKey) -> Option<usize> {
        self.keys(kind).iter().position(|k| *k == key)
    }

    /// The ramp being edited.
    pub fn gradient(&self) -> &Gradient {
        &self.gradient
    }

    /// The stops of one ramp.
    pub fn stops(&self, kind: StopKind) -> &[GradientStop] {
        match kind {
            StopKind::Color => &self.gradient.stops,
            StopKind::Opacity => &self.gradient.alpha_stops,
        }
    }

    fn stops_mut(&mut self, kind: StopKind) -> &mut Vec<GradientStop> {
        match kind {
            StopKind::Color => &mut self.gradient.stops,
            StopKind::Opacity => &mut self.gradient.alpha_stops,
        }
    }

    /// The stop the inspector is editing.
    pub fn selected(&self) -> StopRef {
        self.selected
    }

    /// Select a stop. Out-of-range indices are clamped to the last stop.
    pub fn select(&mut self, kind: StopKind, index: usize) {
        let last = self.stops(kind).len().saturating_sub(1);
        self.selected = StopRef {
            kind,
            index: index.min(last),
        };
    }

    /// Add a stop at `position`, taking its colour from the ramp there.
    ///
    /// Returns the index it landed at, which is *not* the end of the list —
    /// the ramp stays sorted.
    pub fn add_stop(&mut self, kind: StopKind, position: f32) -> usize {
        let position = clamp01(position);
        let color = match kind {
            StopKind::Color => self.sample_color(position),
            StopKind::Opacity => [0.0, 0.0, 0.0, self.sample_alpha(position)],
        };
        let stop = GradientStop {
            position,
            color,
            midpoint: 0.5,
        };
        let stops = self.stops_mut(kind);
        let index = insertion_index(stops, position);
        stops.insert(index, stop);
        let key = self.fresh_key();
        self.keys_mut(kind).insert(index, key);
        self.preset = None;
        self.selected = StopRef { kind, index };
        index
    }

    /// Remove a stop.
    ///
    /// Refused — returning `false` and changing nothing — when it would leave
    /// fewer than [`MIN_STOPS`], or when the index does not exist.
    pub fn remove_stop(&mut self, kind: StopKind, index: usize) -> bool {
        let stops = self.stops_mut(kind);
        if index >= stops.len() || stops.len() <= MIN_STOPS {
            return false;
        }
        stops.remove(index);
        self.keys_mut(kind).remove(index);
        self.preset = None;
        let last = self.stops(kind).len() - 1;
        if self.selected.kind == kind && self.selected.index > last {
            self.selected.index = last;
        }
        true
    }

    /// Whether a stop can be removed right now, and why not if it cannot.
    pub fn removal_blocked(&self, kind: StopKind) -> Option<String> {
        (self.stops(kind).len() <= MIN_STOPS).then(|| {
            format!(
                "A {} ramp needs at least {MIN_STOPS} stops",
                kind.label().to_lowercase()
            )
        })
    }

    /// Move a stop. The position is clamped to `0..=1` and the ramp is re-sorted,
    /// so dragging one stop past another reorders them; the stop's **new index**
    /// is returned and the selection follows it.
    pub fn set_stop_position(&mut self, kind: StopKind, index: usize, position: f32) -> usize {
        let position = clamp01(position);
        let stops = self.stops_mut(kind);
        if index >= stops.len() {
            return index;
        }
        // Lift the stop out, move it, put it back where it now belongs. Doing
        // it this way keeps the moved stop's *identity* — a re-sort would have
        // to find it again, and two stops that happen to be identical are
        // indistinguishable once they are back in the list.
        let mut stop = stops.remove(index);
        stop.position = position;
        let new_index = insertion_index(stops, position);
        stops.insert(new_index, stop);
        // The key travels with the stop, which is what keeps a widget id — and
        // therefore an in-flight drag — pointing at the stop the user grabbed.
        let keys = self.keys_mut(kind);
        let key = keys.remove(index);
        keys.insert(new_index, key);
        self.preset = None;
        if self.selected.kind == kind {
            self.selected.index = remap_index(self.selected.index, index, new_index);
        }
        new_index
    }

    /// Set a colour stop's colour. On the opacity ramp only alpha is used.
    pub fn set_stop_color(&mut self, kind: StopKind, index: usize, color: Rgba) {
        let stops = self.stops_mut(kind);
        let Some(stop) = stops.get_mut(index) else {
            return;
        };
        stop.color = match kind {
            StopKind::Color => color,
            StopKind::Opacity => [0.0, 0.0, 0.0, clamp01(color[3])],
        };
        self.preset = None;
    }

    /// Set the interpolation midpoint between stop `index` and the next one.
    /// `0.5` is linear; the value is clamped to `0..=1`.
    pub fn set_midpoint(&mut self, kind: StopKind, index: usize, midpoint: f32) {
        let stops = self.stops_mut(kind);
        if let Some(stop) = stops.get_mut(index) {
            stop.midpoint = clamp01(midpoint);
            self.preset = None;
        }
    }

    /// Reverse the ramp end for end, both ramps together.
    ///
    /// A midpoint belongs to the segment *after* its stop, so reversing does
    /// not simply mirror each stop's own midpoint — the value has to move one
    /// stop along as well as being mirrored, or a ramp with a biased segment
    /// comes back different from what it went in as.
    pub fn reverse(&mut self) {
        for kind in StopKind::ALL {
            let stops = self.stops_mut(*kind);
            let count = stops.len();
            let reversed: Vec<GradientStop> = (0..count)
                .map(|j| {
                    let source = &stops[count - 1 - j];
                    GradientStop {
                        position: clamp01(1.0 - source.position),
                        color: source.color,
                        midpoint: if j + 1 < count {
                            clamp01(1.0 - stops[count - 2 - j].midpoint)
                        } else {
                            0.5
                        },
                    }
                })
                .collect();
            *stops = reversed;
            sort_stops(stops);
            // Mirroring positions reverses the list, so the identities reverse
            // with it: the stop that was first is the same stop, now last.
            self.keys_mut(*kind).reverse();
        }
        self.preset = None;
    }

    /// Load preset `index`.
    pub fn apply_preset(&mut self, index: usize) {
        let Some(preset) = PRESETS.get(index) else {
            return;
        };
        self.gradient.stops = preset
            .stops
            .iter()
            .map(|(position, color)| GradientStop {
                position: clamp01(*position),
                color: *color,
                midpoint: 0.5,
            })
            .collect();
        self.gradient.alpha_stops = preset
            .stops
            .iter()
            .map(|(position, color)| GradientStop {
                position: clamp01(*position),
                color: [0.0, 0.0, 0.0, color[3]],
                midpoint: 0.5,
            })
            .collect();
        sort_stops(&mut self.gradient.stops);
        sort_stops(&mut self.gradient.alpha_stops);
        // A preset replaces both ramps outright: these are different stops, so
        // they get different identities.
        for kind in StopKind::ALL {
            self.reseed_keys(*kind);
        }
        self.selected = StopRef::default();
        self.preset = Some(index);
    }

    /// The preset currently loaded, if the ramp has not been edited since.
    pub fn preset(&self) -> Option<usize> {
        self.preset
    }

    /// The ramp's colour at `t`, alpha taken from the colour ramp only.
    pub fn sample_color(&self, t: f32) -> Rgba {
        sample_ramp(&self.gradient.stops, t)
    }

    /// The opacity ramp's alpha at `t`.
    pub fn sample_alpha(&self, t: f32) -> f32 {
        sample_ramp(&self.gradient.alpha_stops, t)[3]
    }

    /// The colour the gradient actually paints at `t`: the colour ramp with the
    /// opacity ramp's alpha applied on top of its own.
    pub fn sample(&self, t: f32) -> Rgba {
        sample_gradient(&self.gradient, t)
    }

    /// The nested colour picker, when a colour stop's swatch has been clicked.
    pub fn color_edit(&self) -> &ColorEdit<StopRef> {
        &self.color_edit
    }

    /// Mutable access to it.
    pub fn color_edit_mut(&mut self) -> &mut ColorEdit<StopRef> {
        &mut self.color_edit
    }

    /// Draw the dialog for one frame.
    ///
    /// `sampler` reaches the nested colour picker's eyedropper; `None` draws
    /// that button disabled with its reason.
    pub fn show(
        &mut self,
        ctx: &Context,
        sampler: Option<&dyn ScreenSampler>,
    ) -> DialogOutcome<DialogAction> {
        self.show_impl(
            ctx,
            "gradient-editor",
            ModalStyle::centered(DialogWidth::Medium),
            sampler,
        )
    }

    /// Draw the editor as a surface opened from another dialog.
    ///
    /// Offset and without a second scrim, exactly like the nested colour
    /// picker: the Layer Style dialog opens one of these on its gradient
    /// overlay's ramp. `id_salt` names the host so two hosts cannot share
    /// window state.
    pub fn show_nested(
        &mut self,
        ctx: &Context,
        id_salt: &'static str,
        sampler: Option<&dyn ScreenSampler>,
    ) -> DialogOutcome<DialogAction> {
        self.show_impl(
            ctx,
            id_salt,
            ModalStyle::nested(DialogWidth::Medium),
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
        let nested = self.color_edit.is_open();
        let keys = if nested {
            DialogKeys::NONE
        } else {
            DialogKeys::read(ctx)
        };
        let mut outcome = super::chrome::resolve(self, keys);
        let drawn = modal_with(
            ctx,
            id_salt,
            self.title(),
            Some("Opacity stops sit above the bar, colour stops below it."),
            style,
            |ui| self.body(ui),
        );
        if let Some((stop, rgba)) = self.color_edit.show(ctx, "gradient-stop-color", sampler) {
            self.set_stop_color(stop.kind, stop.index, rgba);
        }
        if nested {
            return DialogOutcome::Open;
        }
        if let Some(Some(button)) = drawn {
            outcome = match button {
                DialogButton::Cancel => DialogOutcome::Cancelled,
                DialogButton::Confirm => self
                    .confirm()
                    .map_or(DialogOutcome::Open, DialogOutcome::Confirmed),
                DialogButton::Extra(0) => {
                    self.reverse();
                    DialogOutcome::Open
                }
                DialogButton::Extra(_) => DialogOutcome::Open,
            };
        }
        outcome
    }

    fn body(&mut self, ui: &mut egui::Ui) -> Option<DialogButton> {
        design::section_header(ui, "Presets");
        ui.horizontal_wrapped(|ui| {
            for (index, preset) in PRESETS.iter().enumerate() {
                let selected = self.preset == Some(index);
                let (rect, response) = ui.allocate_exact_size(sizes::preset_chip(), Sense::click());
                if ui.is_rect_visible(rect) {
                    let ramp: Vec<Rgba> = (0..=16)
                        .map(|i| sample_preset(preset, i as f32 / 16.0))
                        .collect();
                    paint_ramp(ui, rect, &ramp, selected);
                }
                if response.on_hover_text(preset.name).clicked() {
                    self.apply_preset(index);
                }
            }
        });

        hairline(ui);
        design::section_header(ui, "Ramp");
        self.stop_strip(ui);

        hairline(ui);
        self.inspector(ui);

        ui.add_space(Space::Small.pt());
        action_row(
            ui,
            self.confirm_label(),
            self.blocked_reason().as_deref(),
            &["Reverse"],
        )
    }

    fn stop_strip(&mut self, ui: &mut egui::Ui) {
        // A stop handle is dragged, so it is held to the design system's
        // minimum pointer target rather than to whatever `egui` happens to be
        // laying controls out at.
        let handle = ui
            .spacing()
            .interact_size
            .y
            .max(current_tokens(ui).metrics.min_hit_target);
        let width = ui.available_width();
        let bar_height = sizes::gradient_bar_height();
        let (rect, _) =
            ui.allocate_exact_size(vec2(width, bar_height + 2.0 * handle), Sense::hover());
        let bar = Rect::from_min_max(
            pos2(rect.left() + handle * 0.5, rect.top() + handle),
            pos2(
                rect.right() - handle * 0.5,
                rect.top() + handle + bar_height,
            ),
        );
        if ui.is_rect_visible(rect) {
            let ramp: Vec<Rgba> = (0..=64).map(|i| self.sample(i as f32 / 64.0)).collect();
            paint_ramp(ui, bar, &ramp, false);
        }
        // Decided while drawing, applied after the pass. `set_stop_position`
        // re-sorts the ramp, and re-sorting halfway through would draw one stop
        // twice — allocating the same widget id twice in a frame — and skip
        // another.
        let mut select: Option<StopRef> = None;
        let mut moved: Option<(StopKind, StopKey, f32)> = None;
        for kind in StopKind::ALL {
            let stops: Vec<(StopKey, GradientStop)> = self
                .keys(*kind)
                .iter()
                .copied()
                .zip(self.stops(*kind).iter().cloned())
                .collect();
            for (index, (key, stop)) in stops.into_iter().enumerate() {
                let x = bar.left() + stop.position * bar.width();
                let y = match kind {
                    StopKind::Opacity => bar.top() - handle * 0.5,
                    StopKind::Color => bar.bottom() + handle * 0.5,
                };
                let handle_rect = Rect::from_center_size(pos2(x, y), vec2(handle, handle));
                let response = ui.interact(
                    handle_rect,
                    ids::gradient_stop_handle(*kind, key),
                    Sense::click_and_drag(),
                );
                let selected = self.selected == StopRef { kind: *kind, index };
                if ui.is_rect_visible(handle_rect) {
                    let color = match kind {
                        StopKind::Color => stop.color,
                        StopKind::Opacity => {
                            let a = stop.color[3];
                            [a, a, a, 1.0]
                        }
                    };
                    paint_handle(ui, handle_rect, color, selected);
                }
                // Grabbing a stop selects it, so the inspector below is about
                // the stop under the pointer for the whole drag.
                if response.clicked() || response.drag_started() {
                    select = Some(StopRef { kind: *kind, index });
                }
                if response.dragged() {
                    if let Some(pos) = response.interact_pointer_pos() {
                        let t = (pos.x - bar.left()) / bar.width().max(1.0);
                        moved = Some((*kind, key, t));
                    }
                }
            }
        }
        if let Some(stop) = select {
            self.select(stop.kind, stop.index);
        }
        if let Some((kind, key, t)) = moved {
            // By key, not by the index it was drawn at: between the draw and
            // here the ramp may have re-sorted under an earlier decision.
            if let Some(index) = self.index_of_key(kind, key) {
                self.set_stop_position(kind, index, t);
            }
        }
        ui.horizontal(|ui| {
            for kind in StopKind::ALL {
                if design::ghost_button(ui, &format!("Add {} stop", kind.label().to_lowercase()))
                    .clicked()
                {
                    self.add_stop(*kind, 0.5);
                }
            }
        });
    }

    fn inspector(&mut self, ui: &mut egui::Ui) {
        let selection = self.selected;
        let Some(stop) = self.stops(selection.kind).get(selection.index).cloned() else {
            caption(ui, "No stop selected");
            return;
        };
        design::section_header(ui, &format!("{} stop", selection.kind.label()));
        design::inspector_field(ui, "Location", |ui| {
            let mut position = f64::from(stop.position) * 100.0;
            if numeric(ui, &mut position, 0.0..=100.0, 1, "%").changed() {
                self.set_stop_position(selection.kind, selection.index, (position / 100.0) as f32);
            }
        });
        let mut open_picker = false;
        match selection.kind {
            StopKind::Color => {
                design::inspector_field(ui, "Color", |ui| {
                    open_picker = swatch(
                        ui,
                        ids::gradient_stop_color(selection),
                        stop.color,
                        sizes::swatch(),
                    )
                    .clicked();
                    caption(ui, ColorValue::new(stop.color).to_hex(false));
                });
            }
            StopKind::Opacity => {
                design::inspector_field(ui, "Opacity", |ui| {
                    let mut alpha = f64::from(stop.color[3]) * 100.0;
                    if numeric(ui, &mut alpha, 0.0..=100.0, 1, "%").changed() {
                        self.set_stop_color(
                            selection.kind,
                            selection.index,
                            [0.0, 0.0, 0.0, (alpha / 100.0) as f32],
                        );
                    }
                });
            }
        }
        if open_picker {
            self.color_edit.open(selection, stop.color);
        }
        let is_last = selection.index + 1 >= self.stops(selection.kind).len();
        ui.add_enabled_ui(!is_last, |ui| {
            design::inspector_field(ui, "Midpoint", |ui| {
                let mut midpoint = f64::from(stop.midpoint) * 100.0;
                if numeric(ui, &mut midpoint, 0.0..=100.0, 1, "%").changed() {
                    self.set_midpoint(selection.kind, selection.index, (midpoint / 100.0) as f32);
                }
            });
        });
        if is_last {
            caption(ui, "The last stop has no segment after it.");
        }
        let blocked = self.removal_blocked(selection.kind);
        ui.horizontal(|ui| {
            let response = ui
                .add_enabled_ui(blocked.is_none(), |ui| {
                    design::secondary_button(ui, "Delete stop")
                })
                .inner;
            match &blocked {
                Some(reason) => {
                    response.on_disabled_hover_text(reason.clone());
                }
                None => {
                    if response.clicked() {
                        self.remove_stop(selection.kind, selection.index);
                    }
                }
            }
        });
    }
}

impl Dialog for GradientEditorDialog {
    fn title(&self) -> &'static str {
        "Gradient Editor"
    }

    fn confirm_label(&self) -> &'static str {
        "Apply"
    }

    fn confirm(&self) -> Option<DialogAction> {
        (self.gradient.stops.len() >= MIN_STOPS && self.gradient.alpha_stops.len() >= MIN_STOPS)
            .then(|| DialogAction::SetGradient(Box::new(self.gradient.clone())))
    }
}

/// The colour a whole gradient paints at `t`.
///
/// The colour ramp with the opacity ramp's alpha folded into its own. An empty
/// opacity ramp means "use the colour stops' alpha", which is how
/// [`layer_model`] stores a gradient that has never had one.
pub fn sample_gradient(gradient: &Gradient, t: f32) -> Rgba {
    let color = sample_ramp(&gradient.stops, t);
    if gradient.alpha_stops.is_empty() {
        return color;
    }
    let alpha = sample_ramp(&gradient.alpha_stops, t)[3];
    [color[0], color[1], color[2], clamp01(color[3] * alpha)]
}

/// A clickable preview of a whole ramp, for a dialog that *owns* a gradient but
/// is not this one.
///
/// The seam is [`super::controls::swatch`]'s, one step up: the host passes a
/// stable id from [`ids`], gets the `Response` back, and opens a nested
/// [`GradientEditorDialog`] with it. A ramp drawn without that is a picture of a
/// gradient nobody can edit, which is precisely what the Layer Style dialog's
/// Gradient Overlay panel used to be.
#[must_use = "a ramp that ignores its Response is a control wired to nothing"]
pub fn gradient_swatch(
    ui: &mut egui::Ui,
    id: egui::Id,
    gradient: &Gradient,
    size: egui::Vec2,
) -> egui::Response {
    let (_, rect) = ui.allocate_space(size);
    let response = ui.interact(rect, id, Sense::click());
    if ui.is_rect_visible(rect) {
        let ramp: Vec<Rgba> = (0..=32)
            .map(|i| sample_gradient(gradient, i as f32 / 32.0))
            .collect();
        paint_ramp(ui, rect, &ramp, false);
    }
    response
}

/// Evaluate a ramp at `t`, honouring each segment's midpoint.
///
/// Outside the outermost stops the ramp holds their colour, which is what every
/// gradient tool does at the ends of its line.
pub fn sample_ramp(stops: &[GradientStop], t: f32) -> Rgba {
    match stops {
        [] => [0.0, 0.0, 0.0, 0.0],
        [only] => only.color,
        _ => {
            let t = clamp01(t);
            if t <= stops[0].position {
                return stops[0].color;
            }
            if t >= stops[stops.len() - 1].position {
                return stops[stops.len() - 1].color;
            }
            for pair in stops.windows(2) {
                let (a, b) = (&pair[0], &pair[1]);
                if t >= a.position && t <= b.position {
                    let span = b.position - a.position;
                    let local = if span <= f32::EPSILON {
                        0.0
                    } else {
                        (t - a.position) / span
                    };
                    let eased = ease_midpoint(local, a.midpoint);
                    return [
                        lerp(a.color[0], b.color[0], eased),
                        lerp(a.color[1], b.color[1], eased),
                        lerp(a.color[2], b.color[2], eased),
                        lerp(a.color[3], b.color[3], eased),
                    ];
                }
            }
            stops[stops.len() - 1].color
        }
    }
}

/// Bend `t` so that the segment reaches its halfway colour at `midpoint`.
///
/// Piecewise linear: `0..m` is stretched onto `0..0.5` and `m..1` onto
/// `0.5..1`. `0.5` is the identity.
///
/// The obvious alternative — the power curve `t^(ln 0.5 / ln m)` — is smoother
/// but is **not closed under reversal**: no exponent reproduces
/// `1 - (1 - u)^p`, so reversing a ramp with a biased segment and reversing it
/// back would not give the ramp you started with. This family does, with
/// `m' = 1 - m`, which is what [`GradientEditorDialog::reverse`] relies on and
/// what `reversing_mirrors_a_biased_segment_too` pins.
///
/// The midpoint is held away from the ends so neither half collapses.
pub fn ease_midpoint(t: f32, midpoint: f32) -> f32 {
    let m = clamp01(midpoint).clamp(0.01, 0.99);
    let t = clamp01(t);
    if t < m {
        0.5 * t / m
    } else {
        0.5 + 0.5 * (t - m) / (1.0 - m)
    }
}

fn sample_preset(preset: &GradientPreset, t: f32) -> Rgba {
    let stops: Vec<GradientStop> = preset
        .stops
        .iter()
        .map(|(position, color)| GradientStop {
            position: *position,
            color: *color,
            midpoint: 0.5,
        })
        .collect();
    sample_ramp(&stops, t)
}

fn paint_ramp(ui: &egui::Ui, rect: Rect, ramp: &[Rgba], selected: bool) {
    let t = current_tokens(ui);
    let radius = Radius::Small.resolve(&t.radii, rect.height());
    super::controls::checkerboard(ui, rect, radius);
    if ramp.len() >= 2 {
        let mut mesh = Mesh::default();
        for index in 0..ramp.len() - 1 {
            let x0 = rect.left() + rect.width() * index as f32 / (ramp.len() - 1) as f32;
            let x1 = rect.left() + rect.width() * (index + 1) as f32 / (ramp.len() - 1) as f32;
            let band = Rect::from_min_max(pos2(x0, rect.top()), pos2(x1, rect.bottom()));
            let a = color_of(ramp[index]);
            let b = color_of(ramp[index + 1]);
            let base = mesh.vertices.len() as u32;
            mesh.colored_vertex(band.left_top(), a);
            mesh.colored_vertex(band.right_top(), b);
            mesh.colored_vertex(band.left_bottom(), a);
            mesh.colored_vertex(band.right_bottom(), b);
            mesh.add_triangle(base, base + 1, base + 2);
            mesh.add_triangle(base + 1, base + 3, base + 2);
        }
        ui.painter().add(Shape::mesh(mesh));
    }
    let stroke_color = if selected {
        t.palette.color(ColorRole::Accent)
    } else {
        t.palette.color(ColorRole::ControlStroke)
    };
    let width = if selected {
        t.borders.thick
    } else {
        t.borders.hairline
    };
    ui.painter().rect_stroke(
        rect,
        rounding(radius),
        egui::Stroke::new(width, color32(stroke_color)),
    );
}

fn paint_handle(ui: &egui::Ui, rect: Rect, color: Rgba, selected: bool) {
    let t = current_tokens(ui);
    let radius = Radius::Small.resolve(&t.radii, rect.height());
    ui.painter()
        .rect_filled(rect, rounding(radius), color_of(color));
    let stroke = if selected {
        egui::Stroke::new(t.borders.thick, color32(t.palette.color(ColorRole::Accent)))
    } else {
        egui::Stroke::new(
            t.borders.hairline,
            color32(t.palette.color(ColorRole::ControlStrokeStrong)),
        )
    };
    ui.painter().rect_stroke(rect, rounding(radius), stroke);
}

/// Where a stop at `position` belongs in an already-sorted ramp.
///
/// After every stop whose position is less than or equal to it, so a stop
/// dragged exactly onto another lands on that other stop's right.
fn insertion_index(stops: &[GradientStop], position: f32) -> usize {
    stops
        .iter()
        .position(|s| s.position > position)
        .unwrap_or(stops.len())
}

/// Follow an index through a "remove at `from`, insert at `to`" move.
fn remap_index(index: usize, from: usize, to: usize) -> usize {
    if index == from {
        return to;
    }
    let after_remove = if index > from { index - 1 } else { index };
    if after_remove >= to {
        after_remove + 1
    } else {
        after_remove
    }
}

fn sort_stops(stops: &mut [GradientStop]) {
    stops.sort_by(|a, b| {
        a.position
            .partial_cmp(&b.position)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn clamp01(value: f32) -> f32 {
    if value.is_nan() {
        0.0
    } else {
        value.clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialogs::chrome::test_support::{frame_both_themes, Harness};

    fn is_sorted(stops: &[GradientStop]) -> bool {
        stops.windows(2).all(|w| w[0].position <= w[1].position)
    }

    fn assert_invariants(dialog: &GradientEditorDialog) {
        for kind in StopKind::ALL {
            let stops = dialog.stops(*kind);
            let keys = dialog.keys(*kind);
            assert_eq!(
                keys.len(),
                stops.len(),
                "{kind:?} has {} keys for {} stops",
                keys.len(),
                stops.len()
            );
            let unique: std::collections::HashSet<StopKey> = keys.iter().copied().collect();
            assert_eq!(unique.len(), keys.len(), "{kind:?} reused a stop key");
            assert!(stops.len() >= MIN_STOPS, "{kind:?} fell below {MIN_STOPS}");
            assert!(is_sorted(stops), "{kind:?} is out of order: {stops:?}");
            for stop in stops {
                assert!(
                    (0.0..=1.0).contains(&stop.position),
                    "{kind:?} position {} escaped 0..=1",
                    stop.position
                );
                assert!((0.0..=1.0).contains(&stop.midpoint));
            }
        }
    }

    #[test]
    fn a_default_gradient_is_already_normalised() {
        let dialog = GradientEditorDialog::default();
        assert_invariants(&dialog);
        assert_eq!(dialog.stops(StopKind::Color).len(), 2);
        assert_eq!(dialog.stops(StopKind::Opacity).len(), 2);
    }

    #[test]
    fn a_gradient_with_unsorted_out_of_range_stops_is_repaired_on_open() {
        let gradient = Gradient {
            stops: vec![
                GradientStop {
                    position: 4.0,
                    color: [1.0, 0.0, 0.0, 1.0],
                    midpoint: 9.0,
                },
                GradientStop {
                    position: -2.0,
                    color: [0.0, 0.0, 1.0, 1.0],
                    midpoint: -1.0,
                },
            ],
            alpha_stops: Vec::new(),
            smoothness: 1.0,
        };
        let dialog = GradientEditorDialog::new(gradient);
        assert_invariants(&dialog);
        assert_eq!(dialog.stops(StopKind::Color)[0].position, 0.0);
        assert_eq!(dialog.stops(StopKind::Color)[1].position, 1.0);
    }

    #[test]
    fn a_gradient_with_too_few_stops_is_padded() {
        let gradient = Gradient {
            stops: vec![GradientStop {
                position: 0.25,
                color: [1.0, 0.0, 0.0, 1.0],
                midpoint: 0.5,
            }],
            alpha_stops: Vec::new(),
            smoothness: 1.0,
        };
        let dialog = GradientEditorDialog::new(gradient);
        assert_invariants(&dialog);
    }

    #[test]
    fn adding_a_stop_keeps_the_ramp_sorted() {
        let mut dialog = GradientEditorDialog::default();
        for position in [0.9, 0.1, 0.5, 0.05, 0.95, 0.37] {
            dialog.add_stop(StopKind::Color, position);
            assert_invariants(&dialog);
        }
        assert_eq!(dialog.stops(StopKind::Color).len(), 8);
    }

    #[test]
    fn an_added_stop_takes_the_colour_already_at_that_position() {
        let mut dialog = GradientEditorDialog::default();
        let index = dialog.add_stop(StopKind::Color, 0.5);
        let stop = dialog.stops(StopKind::Color)[index].clone();
        // Default ramp is black to white, so the midpoint is mid grey.
        assert!((stop.color[0] - 0.5).abs() < 1e-5, "{:?}", stop.color);
    }

    #[test]
    fn positions_are_clamped_to_the_unit_range() {
        let mut dialog = GradientEditorDialog::default();
        dialog.add_stop(StopKind::Color, 5.0);
        dialog.add_stop(StopKind::Color, -5.0);
        dialog.add_stop(StopKind::Color, f32::NAN);
        assert_invariants(&dialog);
        dialog.set_stop_position(StopKind::Color, 0, 42.0);
        dialog.set_stop_position(StopKind::Color, 1, -42.0);
        assert_invariants(&dialog);
    }

    #[test]
    fn dragging_a_stop_past_its_neighbour_reorders_the_ramp() {
        let mut dialog = GradientEditorDialog::default();
        dialog.add_stop(StopKind::Color, 0.25);
        dialog.set_stop_color(StopKind::Color, 1, [1.0, 0.0, 0.0, 1.0]);
        // The red stop is at index 1; drag it to the end.
        let new_index = dialog.set_stop_position(StopKind::Color, 1, 1.0);
        assert_eq!(new_index, 2);
        assert_invariants(&dialog);
        assert_eq!(dialog.stops(StopKind::Color)[2].color, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(
            dialog.selected(),
            StopRef {
                kind: StopKind::Color,
                index: 2
            }
        );
    }

    #[test]
    fn removing_the_last_two_stops_is_prevented() {
        let mut dialog = GradientEditorDialog::default();
        for kind in StopKind::ALL {
            assert_eq!(dialog.stops(*kind).len(), MIN_STOPS);
            assert!(!dialog.remove_stop(*kind, 0));
            assert!(!dialog.remove_stop(*kind, 1));
            assert_eq!(dialog.stops(*kind).len(), MIN_STOPS);
            assert!(dialog.removal_blocked(*kind).is_some());
        }
    }

    #[test]
    fn a_third_stop_can_be_removed_and_then_the_ramp_locks_again() {
        let mut dialog = GradientEditorDialog::default();
        dialog.add_stop(StopKind::Color, 0.5);
        assert!(dialog.removal_blocked(StopKind::Color).is_none());
        assert!(dialog.remove_stop(StopKind::Color, 1));
        assert_eq!(dialog.stops(StopKind::Color).len(), 2);
        assert!(!dialog.remove_stop(StopKind::Color, 0));
        assert_invariants(&dialog);
    }

    #[test]
    fn removing_a_nonexistent_stop_changes_nothing() {
        let mut dialog = GradientEditorDialog::default();
        dialog.add_stop(StopKind::Color, 0.5);
        let before = dialog.gradient().clone();
        assert!(!dialog.remove_stop(StopKind::Color, 99));
        assert_eq!(dialog.gradient(), &before);
    }

    #[test]
    fn removing_a_stop_pulls_the_selection_back_into_range() {
        let mut dialog = GradientEditorDialog::default();
        dialog.add_stop(StopKind::Color, 0.5);
        dialog.select(StopKind::Color, 2);
        assert!(dialog.remove_stop(StopKind::Color, 2));
        assert_eq!(dialog.selected().index, 1);
    }

    #[test]
    fn the_midpoint_bends_the_segment_and_half_is_the_identity() {
        assert!((ease_midpoint(0.25, 0.5) - 0.25).abs() < 1e-6);
        assert!((ease_midpoint(0.75, 0.5) - 0.75).abs() < 1e-6);
        // A midpoint at 0.25 reaches the halfway colour a quarter of the way in.
        assert!((ease_midpoint(0.25, 0.25) - 0.5).abs() < 1e-5);
        assert!((ease_midpoint(0.75, 0.75) - 0.5).abs() < 1e-5);
        // Still monotone and still bounded.
        for m in [0.0, 0.01, 0.25, 0.5, 0.75, 0.99, 1.0] {
            let mut previous = -1.0;
            for step in 0..=20 {
                let value = ease_midpoint(step as f32 / 20.0, m);
                assert!((0.0..=1.0).contains(&value), "{value} escaped for m={m}");
                assert!(value >= previous, "not monotone at m={m}");
                previous = value;
            }
        }
    }

    #[test]
    fn sampling_holds_the_end_colours_outside_the_ramp() {
        let dialog = GradientEditorDialog::default();
        assert_eq!(dialog.sample_color(-1.0), [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(dialog.sample_color(2.0), [1.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn the_opacity_ramp_multiplies_into_the_preview() {
        let mut dialog = GradientEditorDialog::default();
        dialog.set_stop_color(StopKind::Opacity, 0, [0.0, 0.0, 0.0, 0.0]);
        dialog.set_stop_color(StopKind::Opacity, 1, [0.0, 0.0, 0.0, 1.0]);
        assert!(dialog.sample(0.0)[3] < 1e-6);
        assert!((dialog.sample(1.0)[3] - 1.0).abs() < 1e-6);
        assert!((dialog.sample(0.5)[3] - 0.5).abs() < 1e-5);
    }

    #[test]
    fn every_preset_loads_into_a_valid_ramp() {
        for index in 0..PRESETS.len() {
            let mut dialog = GradientEditorDialog::default();
            dialog.apply_preset(index);
            assert_invariants(&dialog);
            assert_eq!(dialog.preset(), Some(index));
            assert!(dialog.confirm().is_some());
        }
    }

    #[test]
    fn editing_drops_the_preset_highlight() {
        let mut dialog = GradientEditorDialog::default();
        dialog.apply_preset(3);
        assert!(dialog.preset().is_some());
        dialog.add_stop(StopKind::Color, 0.5);
        assert!(dialog.preset().is_none());
    }

    #[test]
    fn reversing_twice_returns_the_original_ramp() {
        let mut dialog = GradientEditorDialog::default();
        dialog.apply_preset(4);
        let before = dialog.gradient().clone();
        dialog.reverse();
        assert_invariants(&dialog);
        assert_ne!(dialog.gradient(), &before);
        dialog.reverse();
        assert_eq!(dialog.gradient(), &before);
    }

    #[test]
    fn reversing_mirrors_what_the_ramp_paints() {
        let mut dialog = GradientEditorDialog::default();
        let before: Vec<Rgba> = (0..=10).map(|i| dialog.sample(i as f32 / 10.0)).collect();
        dialog.reverse();
        for (index, sample) in before.iter().enumerate() {
            let mirrored = dialog.sample(1.0 - index as f32 / 10.0);
            for channel in 0..4 {
                assert!(
                    (mirrored[channel] - sample[channel]).abs() < 1e-5,
                    "sample {index} channel {channel}: {mirrored:?} vs {sample:?}"
                );
            }
        }
    }

    #[test]
    fn reversing_mirrors_a_biased_segment_too() {
        // The bug this catches: mirroring each stop's own midpoint in place.
        // A midpoint describes the segment *after* its stop, so it has to move
        // one stop along as well as being flipped.
        let mut dialog = GradientEditorDialog::default();
        dialog.add_stop(StopKind::Color, 0.5);
        dialog.set_midpoint(StopKind::Color, 0, 0.2);
        dialog.set_midpoint(StopKind::Color, 1, 0.8);
        let before: Vec<Rgba> = (0..=20).map(|i| dialog.sample(i as f32 / 20.0)).collect();
        dialog.reverse();
        for (index, sample) in before.iter().enumerate() {
            let mirrored = dialog.sample(1.0 - index as f32 / 20.0);
            for channel in 0..4 {
                assert!(
                    (mirrored[channel] - sample[channel]).abs() < 1e-4,
                    "sample {index} channel {channel}: {mirrored:?} vs {sample:?}"
                );
            }
        }
    }

    #[test]
    fn index_remapping_follows_a_move_exactly() {
        // Moving item 3 to slot 1 in [0,1,2,3,4] gives [0,3,1,2,4].
        let expected = [0usize, 2, 3, 1, 4];
        for (index, want) in expected.into_iter().enumerate() {
            assert_eq!(remap_index(index, 3, 1), want, "index {index}");
        }
        // A move that changes nothing changes no index.
        for index in 0..5 {
            assert_eq!(remap_index(index, 2, 2), index);
        }
    }

    #[test]
    fn confirm_carries_the_ramp_and_cancel_carries_nothing() {
        let dialog = GradientEditorDialog::default();
        assert!(dialog.confirm().unwrap().is_valid());
        assert_eq!(
            super::super::chrome::resolve(&dialog, DialogKeys::CANCEL),
            DialogOutcome::Cancelled
        );
    }

    #[test]
    fn it_draws_in_both_appearances() {
        frame_both_themes(|ctx| {
            let mut dialog = GradientEditorDialog::default();
            dialog.apply_preset(3);
            dialog.add_stop(StopKind::Opacity, 0.5);
            assert!(dialog.show(ctx, None).is_open());
        });
    }

    #[test]
    fn clicking_a_colour_stops_swatch_recolours_that_stop() {
        // The defect this pins: the Color-stop inspector drew a swatch and
        // dropped its Response, so `set_stop_color`'s colour branch had no
        // production caller. A stop could be added, dragged, reordered and
        // deleted but never given a colour — every new stop was stuck on
        // whatever `sample_color` already returned there, which by
        // construction is invisible in the ramp.
        let h = Harness::new();
        let mut dialog = GradientEditorDialog::default();
        let index = dialog.add_stop(StopKind::Color, 0.5);
        dialog.select(StopKind::Color, index);
        let before = dialog.stops(StopKind::Color)[index].color;

        let selection = dialog.selected();
        h.click_widget(ids::gradient_stop_color(selection), |ctx| {
            dialog.show(ctx, None);
        });
        assert_eq!(
            dialog.color_edit().target(),
            Some(selection),
            "the swatch opened nothing"
        );

        let chosen = ColorValue::new([1.0, 0.0, 0.25, 1.0]);
        dialog
            .color_edit_mut()
            .picker_mut()
            .expect("the picker is up")
            .set_color(chosen);
        h.frame(Harness::key_events(egui::Key::Enter), |ctx| {
            assert!(dialog.show(ctx, None).is_open());
        });

        let after = dialog.stops(StopKind::Color)[index].color;
        assert_ne!(after, before, "the stop kept its sampled colour");
        assert_eq!(ColorValue::new(after).to_bytes(), chosen.to_bytes());
        assert_invariants(&dialog);
    }

    #[test]
    fn an_opacity_stop_has_a_number_rather_than_a_swatch() {
        // Only the alpha channel of an opacity stop means anything, so it gets
        // a percentage field. Drawing a colour swatch that could only ever set
        // alpha would be the same lie in the other direction.
        let h = Harness::new();
        let mut dialog = GradientEditorDialog::default();
        dialog.select(StopKind::Opacity, 0);
        let selection = dialog.selected();
        h.frame(Vec::new(), |ctx| {
            dialog.show(ctx, None);
        });
        assert!(!h.was_drawn(ids::gradient_stop_color(selection)));
    }

    #[test]
    fn a_moved_stop_carries_its_identity_and_leaves_the_others_theirs() {
        let mut dialog = GradientEditorDialog::default();
        dialog.add_stop(StopKind::Color, 0.5);
        let keys: Vec<StopKey> = dialog.keys(StopKind::Color).to_vec();
        // Drag the first stop to the end. Every key must still be present, and
        // each must still be attached to the stop it started on.
        dialog.set_stop_position(StopKind::Color, 0, 1.0);
        assert_invariants(&dialog);
        for key in &keys {
            assert!(
                dialog.index_of_key(StopKind::Color, *key).is_some(),
                "a key vanished across a move"
            );
        }
        let moved = dialog
            .index_of_key(StopKind::Color, keys[0])
            .expect("the moved stop");
        assert_eq!(dialog.stops(StopKind::Color)[moved].position, 1.0);
        let untouched = dialog
            .index_of_key(StopKind::Color, keys[1])
            .expect("the stop that was not moved");
        assert_eq!(dialog.stops(StopKind::Color)[untouched].position, 0.5);
    }

    #[test]
    fn a_preset_mints_new_identities_but_a_reversal_keeps_them() {
        let mut dialog = GradientEditorDialog::default();
        let before: Vec<StopKey> = dialog.keys(StopKind::Color).to_vec();
        dialog.reverse();
        let reversed: Vec<StopKey> = dialog.keys(StopKind::Color).to_vec();
        assert_eq!(
            reversed,
            before.iter().rev().copied().collect::<Vec<_>>(),
            "reversing changed which stop is which"
        );
        dialog.apply_preset(3);
        for key in &before {
            assert!(
                dialog.index_of_key(StopKind::Color, *key).is_none(),
                "a replaced ramp kept an old identity"
            );
        }
        assert_invariants(&dialog);
    }

    /// One frame of pointer movement.
    fn move_to(at: egui::Pos2) -> Vec<egui::Event> {
        vec![egui::Event::PointerMoved(at)]
    }

    /// Letting go at `at`.
    fn release_at(at: egui::Pos2) -> Vec<egui::Event> {
        vec![egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        }]
    }

    #[test]
    fn dragging_a_stop_past_its_neighbour_leaves_the_neighbour_alone() {
        // The defect this pins, reproduced through the drawn control: the stop
        // handles were allocated with index-keyed widget ids, so the instant a
        // dragged stop crossed a neighbour and the ramp re-sorted, `egui` went
        // on delivering the drag to the id it started on — which now addressed
        // the *neighbour*. Both stops followed the pointer: a ramp of
        // 0.00/0.50/1.00 came out as 0.6875/0.75/1.00, with the middle stop
        // moved by a drag that never touched it.
        let h = Harness::new();
        let mut dialog = GradientEditorDialog::default();
        dialog.add_stop(StopKind::Color, 0.5);
        let black = dialog.stop_key(StopKind::Color, 0).expect("first stop");
        let middle = dialog.stop_key(StopKind::Color, 1).expect("middle stop");
        let white = dialog.stop_key(StopKind::Color, 2).expect("last stop");

        let mut draw = |ctx: &Context| {
            dialog.show(ctx, None);
        };
        let from = h
            .settle(ids::gradient_stop_handle(StopKind::Color, black), &mut draw)
            .center();
        let to_end = h
            .ctx
            .read_response(ids::gradient_stop_handle(StopKind::Color, white))
            .expect("the last stop's handle is on screen")
            .rect
            .center();
        // Handle centres are laid out linearly along the bar, so the black stop
        // at 0.0 and the white one at 1.0 fix the mapping: this is 0.75.
        let target = egui::pos2(from.x + 0.75 * (to_end.x - from.x), from.y);

        h.frame(Harness::press_events(from), &mut draw);
        for step in 1..=6 {
            let at = egui::pos2(from.x + (target.x - from.x) * step as f32 / 6.0, from.y);
            h.frame(move_to(at), &mut draw);
        }
        h.frame(release_at(target), &mut draw);

        let middle_at = dialog
            .index_of_key(StopKind::Color, middle)
            .expect("the middle stop is still there");
        assert_eq!(
            dialog.stops(StopKind::Color)[middle_at].position,
            0.5,
            "a stop nobody dragged moved"
        );
        let black_at = dialog
            .index_of_key(StopKind::Color, black)
            .expect("the dragged stop is still there");
        let dragged_to = dialog.stops(StopKind::Color)[black_at].position;
        assert!(
            (dragged_to - 0.75).abs() < 0.02,
            "the dragged stop landed at {dragged_to}, not where the pointer was"
        );
        assert!(black_at > middle_at, "the ramp did not re-sort");
        assert_invariants(&dialog);
    }

    #[test]
    fn grabbing_a_stop_selects_it() {
        let h = Harness::new();
        let mut dialog = GradientEditorDialog::default();
        dialog.add_stop(StopKind::Color, 0.5);
        dialog.select(StopKind::Color, 0);
        let last = dialog.stop_key(StopKind::Color, 2).expect("last stop");

        let mut draw = |ctx: &Context| {
            dialog.show(ctx, None);
        };
        h.click_widget(ids::gradient_stop_handle(StopKind::Color, last), &mut draw);
        assert_eq!(
            dialog.selected(),
            StopRef {
                kind: StopKind::Color,
                index: 2
            },
            "clicking a handle did not select its stop"
        );
    }

    #[test]
    fn cancelling_the_picker_leaves_the_stop_alone() {
        let h = Harness::new();
        let mut dialog = GradientEditorDialog::default();
        dialog.select(StopKind::Color, 0);
        let before = dialog.stops(StopKind::Color)[0].color;
        let selection = dialog.selected();

        h.click_widget(ids::gradient_stop_color(selection), |ctx| {
            dialog.show(ctx, None);
        });
        dialog
            .color_edit_mut()
            .picker_mut()
            .unwrap()
            .set_color(ColorValue::new([1.0, 1.0, 0.0, 1.0]));
        h.frame(Harness::key_events(egui::Key::Escape), |ctx| {
            assert!(
                dialog.show(ctx, None).is_open(),
                "Escape closed the editor under the picker"
            );
        });
        assert!(!dialog.color_edit().is_open());
        assert_eq!(dialog.stops(StopKind::Color)[0].color, before);
    }
}
