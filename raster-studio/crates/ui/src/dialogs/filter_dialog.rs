//! Filter dialogs, generated from a parameter schema.
//!
//! There is exactly one filter dialog. It is built from a [`FilterSpec`] —
//! a name, a list of [`OptionSpec`]s, and the function that runs the filter —
//! so adding a filter to [`FILTERS`] gives it a dialog, a live preview, a
//! preview toggle, range-checked fields and a confirm path with no UI code at
//! all. The schema type is `tools::OptionSpec`, the same one the tool options
//! bar is generated from, so the app has one vocabulary for "a parameter" and
//! not two.
//!
//! The preview runs the *real* filter function on a proxy buffer. There is no
//! second, approximate implementation to drift.

use std::collections::BTreeMap;

use design::tokens::{Radius, Space};
use egui::{vec2, Context, TextureHandle};
use filters::{blur, distort, noise, other, pixelate, sharpen, stylize};
use filters::{EdgeMode, FilterBuffer, Sampling};
use tools::{OptionKind, OptionSpec};

use super::action::DialogAction;
use super::chrome::{
    action_row, caption, hairline, modal, Dialog, DialogButton, DialogKeys, DialogOutcome,
    DialogWidth,
};
use super::color_edit::ColorEdit;
use super::color_picker::ScreenSampler;
use super::controls::{checkbox_row, combo, integer, numeric};
use super::{ids, sizes};

/// Which menu a filter lives under.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum FilterGroup {
    Blur,
    Sharpen,
    Noise,
    Distort,
    Stylize,
    Pixelate,
    Other,
}

impl FilterGroup {
    /// Every group, in menu order.
    pub const ALL: &'static [FilterGroup] = &[
        Self::Blur,
        Self::Sharpen,
        Self::Noise,
        Self::Distort,
        Self::Stylize,
        Self::Pixelate,
        Self::Other,
    ];

    /// Menu label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Blur => "Blur",
            Self::Sharpen => "Sharpen",
            Self::Noise => "Noise",
            Self::Distort => "Distort",
            Self::Stylize => "Stylize",
            Self::Pixelate => "Pixelate",
            Self::Other => "Other",
        }
    }
}

/// One value a filter parameter can hold.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ParamValue {
    Float(f32),
    Int(i32),
    Bool(bool),
    /// Index into the option's `choices`.
    Choice(usize),
    Color([f32; 4]),
}

/// A filter's parameters, always complete and always in range.
///
/// Built from a schema, and every write goes back through that schema, so a
/// value out of range or of the wrong kind cannot get in. That is what lets the
/// dialog hand the parameters straight to the filter without re-checking.
#[derive(Clone, PartialEq, Debug)]
pub struct FilterParams {
    schema: &'static [OptionSpec],
    values: BTreeMap<&'static str, ParamValue>,
}

impl FilterParams {
    /// Every parameter at its schema default.
    pub fn defaults(schema: &'static [OptionSpec]) -> Self {
        let values = schema
            .iter()
            .map(|spec| (spec.key, default_value(spec.kind)))
            .collect();
        Self { schema, values }
    }

    /// The schema these parameters belong to.
    pub fn schema(&self) -> &'static [OptionSpec] {
        self.schema
    }

    /// The specification for one key.
    pub fn spec(&self, key: &str) -> Option<&'static OptionSpec> {
        self.schema.iter().find(|s| s.key == key)
    }

    /// The raw value for `key`.
    pub fn get(&self, key: &str) -> Option<ParamValue> {
        self.values.get(key).copied()
    }

    /// Write `value` for `key`, clamped to the schema.
    ///
    /// Returns `false` — changing nothing — when the key is not in the schema
    /// or the value is of the wrong kind for it.
    pub fn set(&mut self, key: &str, value: ParamValue) -> bool {
        let Some(spec) = self.spec(key) else {
            return false;
        };
        let Some(clamped) = clamp_value(spec.kind, value) else {
            return false;
        };
        self.values.insert(spec.key, clamped);
        true
    }

    /// A float parameter, or the schema default when the key is missing.
    pub fn float(&self, key: &str) -> f32 {
        match self.get(key) {
            Some(ParamValue::Float(v)) => v,
            _ => 0.0,
        }
    }

    /// An integer parameter.
    pub fn int(&self, key: &str) -> i32 {
        match self.get(key) {
            Some(ParamValue::Int(v)) => v,
            _ => 0,
        }
    }

    /// An integer parameter as a `u32`, negatives folded to zero.
    pub fn uint(&self, key: &str) -> u32 {
        self.int(key).max(0) as u32
    }

    /// A boolean parameter.
    pub fn flag(&self, key: &str) -> bool {
        matches!(self.get(key), Some(ParamValue::Bool(true)))
    }

    /// A choice parameter, as its index.
    pub fn choice(&self, key: &str) -> usize {
        match self.get(key) {
            Some(ParamValue::Choice(v)) => v,
            _ => 0,
        }
    }

    /// A choice parameter mapped through `options`, saturating at the end.
    pub fn choose<T: Copy>(&self, key: &str, options: &[T]) -> Option<T> {
        options
            .get(self.choice(key).min(options.len().saturating_sub(1)))
            .copied()
    }
}

/// Everything the generated dialog needs to know about a filter.
#[derive(Clone, Copy)]
pub struct FilterSpec {
    /// Stable identifier, used by the command the dialog emits.
    pub id: &'static str,
    pub name: &'static str,
    pub group: FilterGroup,
    /// One line saying what the filter does, shown under the title.
    pub summary: &'static str,
    pub params: &'static [OptionSpec],
    /// The real filter, applied to a buffer with these parameters.
    pub apply: fn(&FilterBuffer, &FilterParams) -> FilterBuffer,
}

impl std::fmt::Debug for FilterSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilterSpec")
            .field("id", &self.id)
            .field("group", &self.group)
            .finish_non_exhaustive()
    }
}

impl PartialEq for FilterSpec {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

const EDGE_MODES: &[&str] = &["Clamp", "Wrap", "Mirror"];

fn edge_mode(params: &FilterParams, key: &str) -> EdgeMode {
    params
        .choose(key, &[EdgeMode::Clamp, EdgeMode::Wrap, EdgeMode::Mirror])
        .unwrap_or(EdgeMode::Clamp)
}

const fn float(
    key: &'static str,
    label: &'static str,
    min: f32,
    max: f32,
    default: f32,
) -> OptionSpec {
    OptionSpec {
        key,
        label,
        kind: OptionKind::Float { min, max, default },
    }
}

const fn int(
    key: &'static str,
    label: &'static str,
    min: i32,
    max: i32,
    default: i32,
) -> OptionSpec {
    OptionSpec {
        key,
        label,
        kind: OptionKind::Int { min, max, default },
    }
}

const fn flag(key: &'static str, label: &'static str, default: bool) -> OptionSpec {
    OptionSpec {
        key,
        label,
        kind: OptionKind::Bool { default },
    }
}

const fn choice(
    key: &'static str,
    label: &'static str,
    choices: &'static [&'static str],
    default: usize,
) -> OptionSpec {
    OptionSpec {
        key,
        label,
        kind: OptionKind::Choice { choices, default },
    }
}

const GAUSSIAN: &[OptionSpec] = &[
    float("radius", "Radius", 0.0, 250.0, 4.0),
    choice("edge", "Edges", EDGE_MODES, 0),
];
const BOX_BLUR: &[OptionSpec] = &[
    int("radius", "Radius", 0, 250, 4),
    choice("edge", "Edges", EDGE_MODES, 0),
];
const UNSHARP: &[OptionSpec] = &[
    float("amount", "Amount", 0.0, 5.0, 1.0),
    float("radius", "Radius", 0.1, 100.0, 2.0),
    float("threshold", "Threshold", 0.0, 1.0, 0.0),
    choice("edge", "Edges", EDGE_MODES, 0),
];
const MEDIAN: &[OptionSpec] = &[
    int("radius", "Radius", 1, 64, 2),
    choice("edge", "Edges", EDGE_MODES, 0),
];
const ADD_NOISE: &[OptionSpec] = &[
    float("amount", "Amount", 0.0, 1.0, 0.1),
    choice("distribution", "Distribution", &["Uniform", "Gaussian"], 0),
    flag("monochromatic", "Monochromatic", false),
    int("seed", "Seed", 0, 9999, 1),
];
const TWIRL: &[OptionSpec] = &[
    float("radius", "Radius", 1.0, 4096.0, 128.0),
    float("angle", "Angle", -720.0, 720.0, 90.0),
];
const PINCH: &[OptionSpec] = &[
    float("radius", "Radius", 1.0, 4096.0, 128.0),
    float("amount", "Amount", -0.99, 0.99, 0.5),
];
const SPHERIZE: &[OptionSpec] = &[
    float("radius", "Radius", 1.0, 4096.0, 128.0),
    float("amount", "Amount", -1.0, 1.0, 0.5),
];
const EMBOSS: &[OptionSpec] = &[
    float("angle", "Angle", -180.0, 180.0, 135.0),
    float("height", "Height", 0.1, 100.0, 3.0),
    float("amount", "Amount", 0.0, 10.0, 1.0),
];
const OIL_PAINT: &[OptionSpec] = &[
    int("radius", "Radius", 1, 64, 4),
    int("levels", "Levels", 2, 256, 32),
    choice("edge", "Edges", EDGE_MODES, 0),
];
const MOSAIC: &[OptionSpec] = &[int("cell", "Cell size", 1, 512, 8)];
const HIGH_PASS: &[OptionSpec] = &[
    float("radius", "Radius", 0.1, 250.0, 10.0),
    choice("edge", "Edges", EDGE_MODES, 0),
];
const NO_PARAMS: &[OptionSpec] = &[];

/// Every filter that has a dialog.
///
/// This list *is* the menu, the dialog set and the parameter contract. Adding a
/// row here is the whole cost of shipping a new filter dialog.
pub const FILTERS: &[FilterSpec] = &[
    FilterSpec {
        id: "blur.gaussian",
        name: "Gaussian Blur",
        group: FilterGroup::Blur,
        summary: "A smooth, isotropic blur.",
        params: GAUSSIAN,
        apply: |src, p| blur::gaussian_blur(src, p.float("radius"), edge_mode(p, "edge")),
    },
    FilterSpec {
        id: "blur.box",
        name: "Box Blur",
        group: FilterGroup::Blur,
        summary: "A flat average over a square window.",
        params: BOX_BLUR,
        apply: |src, p| blur::box_blur(src, p.uint("radius"), edge_mode(p, "edge")),
    },
    FilterSpec {
        id: "sharpen.unsharp",
        name: "Unsharp Mask",
        group: FilterGroup::Sharpen,
        summary: "Adds back the difference between the image and a blur of it.",
        params: UNSHARP,
        apply: |src, p| {
            sharpen::unsharp_mask(
                src,
                p.float("amount"),
                p.float("radius"),
                p.float("threshold"),
                edge_mode(p, "edge"),
            )
        },
    },
    FilterSpec {
        id: "noise.median",
        name: "Median",
        group: FilterGroup::Noise,
        summary: "Replaces each pixel with the median of its neighbourhood.",
        params: MEDIAN,
        apply: |src, p| noise::median(src, p.uint("radius"), edge_mode(p, "edge")),
    },
    FilterSpec {
        id: "noise.add",
        name: "Add Noise",
        group: FilterGroup::Noise,
        summary: "Deterministic grain: the same seed gives the same grain.",
        params: ADD_NOISE,
        apply: |src, p| {
            noise::add_noise(
                src,
                p.float("amount"),
                p.choose(
                    "distribution",
                    &[
                        noise::NoiseDistribution::Uniform,
                        noise::NoiseDistribution::Gaussian,
                    ],
                )
                .unwrap_or_default(),
                p.flag("monochromatic"),
                p.uint("seed") as u64,
            )
        },
    },
    FilterSpec {
        id: "noise.despeckle",
        name: "Despeckle",
        group: FilterGroup::Noise,
        summary: "A fixed small median that removes isolated speckles.",
        params: NO_PARAMS,
        apply: |src, _| noise::despeckle(src, EdgeMode::Clamp),
    },
    FilterSpec {
        id: "distort.twirl",
        name: "Twirl",
        group: FilterGroup::Distort,
        summary: "Rotates the image about its centre, falling off with radius.",
        params: TWIRL,
        apply: |src, p| {
            distort::twirl(
                src,
                distort::center_of(src.dimensions().0, src.dimensions().1),
                p.float("radius"),
                p.float("angle"),
                Sampling::clamped(),
            )
        },
    },
    FilterSpec {
        id: "distort.pinch",
        name: "Pinch",
        group: FilterGroup::Distort,
        summary: "Pulls the image toward the centre, or pushes it out.",
        params: PINCH,
        apply: |src, p| {
            distort::pinch(
                src,
                distort::center_of(src.dimensions().0, src.dimensions().1),
                p.float("radius"),
                p.float("amount"),
                Sampling::clamped(),
            )
        },
    },
    FilterSpec {
        id: "distort.spherize",
        name: "Spherize",
        group: FilterGroup::Distort,
        summary: "Wraps the image onto a sphere.",
        params: SPHERIZE,
        apply: |src, p| {
            distort::spherize(
                src,
                distort::center_of(src.dimensions().0, src.dimensions().1),
                p.float("radius"),
                p.float("amount"),
                Sampling::clamped(),
            )
        },
    },
    FilterSpec {
        id: "stylize.emboss",
        name: "Emboss",
        group: FilterGroup::Stylize,
        summary: "Lights the image from one side and keeps only the relief.",
        params: EMBOSS,
        apply: |src, p| {
            stylize::emboss(
                src,
                p.float("angle"),
                p.float("height"),
                p.float("amount"),
                Sampling::clamped(),
            )
        },
    },
    FilterSpec {
        id: "stylize.find_edges",
        name: "Find Edges",
        group: FilterGroup::Stylize,
        summary: "Keeps the gradient magnitude and throws away the flat areas.",
        params: NO_PARAMS,
        apply: |src, _| stylize::find_edges(src, EdgeMode::Clamp),
    },
    FilterSpec {
        id: "stylize.solarize",
        name: "Solarize",
        group: FilterGroup::Stylize,
        summary: "Inverts the tones above mid grey.",
        params: NO_PARAMS,
        apply: |src, _| stylize::solarize(src),
    },
    FilterSpec {
        id: "stylize.oil_paint",
        name: "Oil Paint",
        group: FilterGroup::Stylize,
        summary: "Replaces each pixel with the most common tone nearby.",
        params: OIL_PAINT,
        apply: |src, p| {
            stylize::oil_paint(
                src,
                p.uint("radius"),
                p.uint("levels"),
                edge_mode(p, "edge"),
            )
        },
    },
    FilterSpec {
        id: "pixelate.mosaic",
        name: "Mosaic",
        group: FilterGroup::Pixelate,
        summary: "Averages the image into square cells.",
        params: MOSAIC,
        apply: |src, p| pixelate::mosaic(src, p.uint("cell")),
    },
    FilterSpec {
        id: "other.high_pass",
        name: "High Pass",
        group: FilterGroup::Other,
        summary: "Keeps only the detail finer than the radius.",
        params: HIGH_PASS,
        apply: |src, p| other::high_pass(src, p.float("radius"), edge_mode(p, "edge")),
    },
];

/// Look a filter up by its stable id.
pub fn filter_by_id(id: &str) -> Option<&'static FilterSpec> {
    FILTERS.iter().find(|f| f.id == id)
}

/// What the dialog commits to: which filter, with which parameters.
#[derive(Clone, PartialEq, Debug)]
pub struct FilterInvocation {
    pub filter: &'static FilterSpec,
    pub params: FilterParams,
}

impl FilterInvocation {
    /// Whether the invocation's parameters match its filter's schema.
    pub fn is_valid(&self) -> bool {
        self.params.schema().len() == self.filter.params.len()
            && self
                .filter
                .params
                .iter()
                .all(|spec| self.params.get(spec.key).is_some())
    }

    /// Run the filter.
    pub fn run(&self, src: &FilterBuffer) -> FilterBuffer {
        (self.filter.apply)(src, &self.params)
    }
}

/// Largest side of the buffer the live preview filters.
pub const MAX_PREVIEW_SIDE: u32 = 192;

/// The one filter dialog.
pub struct FilterDialog {
    spec: &'static FilterSpec,
    params: FilterParams,
    preview_enabled: bool,
    source: FilterBuffer,
    texture: Option<TextureHandle>,
    cached_for: Option<FilterParams>,
    /// The nested colour picker, keyed by the parameter it edits.
    color_edit: ColorEdit<&'static str>,
}

impl std::fmt::Debug for FilterDialog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FilterDialog")
            .field("spec", &self.spec)
            .field("params", &self.params)
            .field("preview_enabled", &self.preview_enabled)
            .finish_non_exhaustive()
    }
}

impl FilterDialog {
    /// Open `spec`'s dialog over `source`, a small proxy of the layer.
    pub fn new(spec: &'static FilterSpec, source: FilterBuffer) -> Self {
        Self {
            spec,
            params: FilterParams::defaults(spec.params),
            preview_enabled: true,
            source,
            texture: None,
            cached_for: None,
            color_edit: ColorEdit::new(),
        }
    }

    /// Open `spec`'s dialog over a generated proxy, for a caller that has no
    /// pixels yet.
    pub fn with_placeholder(spec: &'static FilterSpec) -> Self {
        Self::new(spec, placeholder_buffer(96, 96))
    }

    /// The filter this dialog is for.
    pub fn spec(&self) -> &'static FilterSpec {
        self.spec
    }

    /// The parameters as edited.
    pub fn params(&self) -> &FilterParams {
        &self.params
    }

    /// Write one parameter, clamped to the schema. Returns `false` when the
    /// key or the kind does not belong to this filter.
    pub fn set_param(&mut self, key: &str, value: ParamValue) -> bool {
        let changed = self.params.set(key, value);
        if changed {
            self.cached_for = None;
        }
        changed
    }

    /// Put every parameter back to its schema default.
    pub fn reset(&mut self) {
        self.params = FilterParams::defaults(self.spec.params);
        self.cached_for = None;
    }

    /// Whether the live preview is on.
    pub fn preview_enabled(&self) -> bool {
        self.preview_enabled
    }

    /// Turn the live preview on or off. Off means the filter is not run at all
    /// per frame, which is the point of the toggle on a large image.
    pub fn set_preview_enabled(&mut self, on: bool) {
        self.preview_enabled = on;
        if !on {
            self.texture = None;
            self.cached_for = None;
        }
    }

    /// Run the filter on the proxy and return the result.
    pub fn preview_buffer(&self) -> FilterBuffer {
        (self.spec.apply)(&self.source, &self.params)
    }

    /// The invocation the dialog would commit.
    pub fn invocation(&self) -> FilterInvocation {
        FilterInvocation {
            filter: self.spec,
            params: self.params.clone(),
        }
    }

    /// The nested colour picker, when a colour parameter's swatch is clicked.
    pub fn color_edit(&self) -> &ColorEdit<&'static str> {
        &self.color_edit
    }

    /// Mutable access to it.
    pub fn color_edit_mut(&mut self) -> &mut ColorEdit<&'static str> {
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
        let nested = self.color_edit.is_open();
        let keys = if nested {
            DialogKeys::NONE
        } else {
            DialogKeys::read(ctx)
        };
        let mut outcome = super::chrome::resolve(self, keys);
        self.refresh_preview(ctx);
        let drawn = modal(
            ctx,
            ("filter", self.spec.id),
            self.spec.name,
            Some(self.spec.summary),
            DialogWidth::Standard,
            |ui| self.body(ui),
        );
        if let Some((key, rgba)) = self.color_edit.show(ctx, "filter-param-color", sampler) {
            self.set_param(key, ParamValue::Color(rgba));
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
                DialogButton::Extra(_) => {
                    self.reset();
                    DialogOutcome::Open
                }
            };
        }
        outcome
    }

    fn refresh_preview(&mut self, ctx: &Context) {
        if !self.preview_enabled {
            return;
        }
        if self.cached_for.as_ref() == Some(&self.params) && self.texture.is_some() {
            return;
        }
        let filtered = self.preview_buffer();
        let (width, height) = filtered.dimensions();
        if width == 0 || height == 0 {
            self.texture = None;
            return;
        }
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [width as usize, height as usize],
            &filtered.to_rgba8(),
        );
        self.texture =
            Some(ctx.load_texture("filter-preview", image, egui::TextureOptions::LINEAR));
        self.cached_for = Some(self.params.clone());
    }

    fn body(&mut self, ui: &mut egui::Ui) -> Option<DialogButton> {
        let mut preview = self.preview_enabled;
        if checkbox_row(ui, "Preview", &mut preview).changed() {
            self.set_preview_enabled(preview);
        }
        match (&self.texture, self.preview_enabled) {
            (Some(texture), true) => {
                let size = texture.size_vec2();
                let scale = (sizes::filter_preview_width() / size.x.max(1.0)).min(2.0);
                ui.image((texture.id(), size * scale));
            }
            (_, true) => {
                caption(ui, "Nothing to preview.");
            }
            (_, false) => {
                let (w, h) = self.source.dimensions();
                let width = sizes::filter_preview_width();
                let (rect, _) = ui.allocate_exact_size(
                    vec2(width, width * h as f32 / w.max(1) as f32),
                    egui::Sense::hover(),
                );
                let radius = {
                    let t = design::current_tokens(ui);
                    Radius::Small.resolve(&t.radii, rect.height())
                };
                super::controls::checkerboard(ui, rect, radius);
                caption(ui, "Preview is off.");
            }
        }

        hairline(ui);
        if self.spec.params.is_empty() {
            caption(ui, "This filter has no settings.");
        }
        for spec in self.spec.params {
            self.param_row(ui, spec);
        }
        ui.add_space(Space::Small.pt());
        action_row(
            ui,
            self.confirm_label(),
            self.blocked_reason().as_deref(),
            &["Reset"],
        )
    }

    /// One row of the generated form. This match *is* the code generator.
    fn param_row(&mut self, ui: &mut egui::Ui, spec: &'static OptionSpec) {
        match spec.kind {
            OptionKind::Float { min, max, .. } => {
                let mut value = f64::from(self.params.float(spec.key));
                design::inspector_field(ui, spec.label, |ui| {
                    if numeric(ui, &mut value, f64::from(min)..=f64::from(max), 3, "").changed() {
                        self.set_param(spec.key, ParamValue::Float(value as f32));
                    }
                });
            }
            OptionKind::Int { min, max, .. } => {
                let mut value = i64::from(self.params.int(spec.key));
                design::inspector_field(ui, spec.label, |ui| {
                    if integer(ui, &mut value, i64::from(min)..=i64::from(max)).changed() {
                        self.set_param(spec.key, ParamValue::Int(value as i32));
                    }
                });
            }
            OptionKind::Bool { .. } => {
                let mut value = self.params.flag(spec.key);
                if checkbox_row(ui, spec.label, &mut value).changed() {
                    self.set_param(spec.key, ParamValue::Bool(value));
                }
            }
            OptionKind::Choice { choices, .. } => {
                let mut index = self.params.choice(spec.key);
                let options: Vec<usize> = (0..choices.len()).collect();
                design::inspector_field(ui, spec.label, |ui| {
                    if combo(
                        ui,
                        ("filter-choice", spec.key),
                        &mut index,
                        &options,
                        |i| choices.get(i).copied().unwrap_or("").to_string(),
                        |_| None,
                    ) {
                        self.set_param(spec.key, ParamValue::Choice(index));
                    }
                });
            }
            OptionKind::Color { .. } => {
                let value = match self.params.get(spec.key) {
                    Some(ParamValue::Color(c)) => c,
                    _ => [0.0, 0.0, 0.0, 1.0],
                };
                let mut open_picker = false;
                design::inspector_field(ui, spec.label, |ui| {
                    open_picker = super::controls::swatch(
                        ui,
                        ids::filter_param_color(spec.key),
                        value,
                        sizes::swatch(),
                    )
                    .clicked();
                });
                if open_picker {
                    self.color_edit.open(spec.key, value);
                }
            }
        }
    }
}

/// A proxy buffer for a caller with no pixels: a ramp with a hard checker, so
/// blurs, edges and pixelation all have something to act on.
pub fn placeholder_buffer(width: u32, height: u32) -> FilterBuffer {
    let width = width.clamp(1, MAX_PREVIEW_SIDE);
    let height = height.clamp(1, MAX_PREVIEW_SIDE);
    let mut buffer = FilterBuffer::transparent(width, height).expect("a non-empty proxy");
    for y in 0..height {
        for x in 0..width {
            let ramp = x as f32 / width as f32;
            let checker = if ((x / 8) + (y / 8)) % 2 == 0 {
                1.0
            } else {
                0.2
            };
            buffer.set(x, y, [ramp, checker, 1.0 - ramp, 1.0]);
        }
    }
    buffer
}

impl Dialog for FilterDialog {
    fn title(&self) -> &'static str {
        "Filter"
    }

    fn confirm_label(&self) -> &'static str {
        "Apply"
    }

    fn confirm(&self) -> Option<DialogAction> {
        let invocation = self.invocation();
        invocation
            .is_valid()
            .then(|| DialogAction::RunFilter(Box::new(invocation)))
    }
}

fn default_value(kind: OptionKind) -> ParamValue {
    match kind {
        OptionKind::Float { default, min, max } => ParamValue::Float(default.clamp(min, max)),
        OptionKind::Int { default, min, max } => ParamValue::Int(default.clamp(min, max)),
        OptionKind::Bool { default } => ParamValue::Bool(default),
        OptionKind::Choice { choices, default } => {
            ParamValue::Choice(default.min(choices.len().saturating_sub(1)))
        }
        OptionKind::Color { default } => ParamValue::Color(default),
    }
}

/// Fit a value to a schema entry, or reject it as the wrong kind.
fn clamp_value(kind: OptionKind, value: ParamValue) -> Option<ParamValue> {
    match (kind, value) {
        (OptionKind::Float { min, max, .. }, ParamValue::Float(v)) => {
            Some(ParamValue::Float(if v.is_nan() {
                min
            } else {
                v.clamp(min, max)
            }))
        }
        (OptionKind::Int { min, max, .. }, ParamValue::Int(v)) => {
            Some(ParamValue::Int(v.clamp(min, max)))
        }
        (OptionKind::Bool { .. }, ParamValue::Bool(v)) => Some(ParamValue::Bool(v)),
        (OptionKind::Choice { choices, .. }, ParamValue::Choice(v)) => {
            Some(ParamValue::Choice(v.min(choices.len().saturating_sub(1))))
        }
        (OptionKind::Color { .. }, ParamValue::Color(c)) => Some(ParamValue::Color([
            c[0].clamp(0.0, 1.0),
            c[1].clamp(0.0, 1.0),
            c[2].clamp(0.0, 1.0),
            c[3].clamp(0.0, 1.0),
        ])),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialogs::chrome::test_support::{frame_both_themes, Harness};

    #[test]
    fn every_filter_has_a_unique_id_a_name_and_a_summary() {
        let mut ids: Vec<&str> = FILTERS.iter().map(|f| f.id).collect();
        ids.sort_unstable();
        let unique = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), unique, "two filters share an id");
        for filter in FILTERS {
            assert!(!filter.name.is_empty(), "{} has no name", filter.id);
            assert!(!filter.summary.is_empty(), "{} has no summary", filter.id);
            assert!(filter_by_id(filter.id).is_some());
        }
        assert!(filter_by_id("nope.nothing").is_none());
    }

    #[test]
    fn every_schema_default_is_inside_its_own_range() {
        for filter in FILTERS {
            for spec in filter.params {
                match spec.kind {
                    OptionKind::Float { min, max, default } => {
                        assert!(min <= max, "{}/{}: {min} > {max}", filter.id, spec.key);
                        assert!(
                            (min..=max).contains(&default),
                            "{}/{}: default {default} outside {min}..={max}",
                            filter.id,
                            spec.key
                        );
                    }
                    OptionKind::Int { min, max, default } => {
                        assert!(min <= max, "{}/{}: {min} > {max}", filter.id, spec.key);
                        assert!(
                            (min..=max).contains(&default),
                            "{}/{}: default {default} outside {min}..={max}",
                            filter.id,
                            spec.key
                        );
                    }
                    OptionKind::Choice { choices, default } => {
                        assert!(
                            !choices.is_empty(),
                            "{}/{} has no choices",
                            filter.id,
                            spec.key
                        );
                        assert!(default < choices.len(), "{}/{}", filter.id, spec.key);
                    }
                    OptionKind::Bool { .. } | OptionKind::Color { .. } => {}
                }
                assert!(
                    !spec.label.is_empty(),
                    "{}/{} has no label",
                    filter.id,
                    spec.key
                );
            }
        }
    }

    #[test]
    fn a_new_filter_gets_a_complete_parameter_set_for_free() {
        for filter in FILTERS {
            let params = FilterParams::defaults(filter.params);
            for spec in filter.params {
                assert!(
                    params.get(spec.key).is_some(),
                    "{}/{} was not populated",
                    filter.id,
                    spec.key
                );
            }
        }
    }

    #[test]
    fn every_filter_runs_on_a_proxy_and_returns_the_same_size() {
        let source = placeholder_buffer(48, 48);
        for filter in FILTERS {
            let dialog = FilterDialog::new(filter, source.clone());
            let out = dialog.preview_buffer();
            assert_eq!(
                out.dimensions(),
                (48, 48),
                "{} changed the buffer size",
                filter.id
            );
            assert!(
                out.pixels().iter().all(|p| p.iter().all(|v| v.is_finite())),
                "{} produced a non-finite pixel",
                filter.id
            );
        }
    }

    #[test]
    fn every_filter_survives_a_one_pixel_buffer() {
        let source = placeholder_buffer(1, 1);
        for filter in FILTERS {
            let dialog = FilterDialog::new(filter, source.clone());
            assert_eq!(
                dialog.preview_buffer().dimensions(),
                (1, 1),
                "{}",
                filter.id
            );
        }
    }

    #[test]
    fn parameters_clamp_to_the_schema() {
        let spec = filter_by_id("blur.gaussian").unwrap();
        let mut dialog = FilterDialog::new(spec, placeholder_buffer(16, 16));
        assert!(dialog.set_param("radius", ParamValue::Float(1.0e9)));
        assert_eq!(dialog.params().float("radius"), 250.0);
        assert!(dialog.set_param("radius", ParamValue::Float(-5.0)));
        assert_eq!(dialog.params().float("radius"), 0.0);
        assert!(dialog.set_param("radius", ParamValue::Float(f32::NAN)));
        assert_eq!(dialog.params().float("radius"), 0.0);
    }

    #[test]
    fn a_choice_never_escapes_its_option_list() {
        let spec = filter_by_id("blur.gaussian").unwrap();
        let mut dialog = FilterDialog::new(spec, placeholder_buffer(16, 16));
        assert!(dialog.set_param("edge", ParamValue::Choice(99)));
        assert_eq!(dialog.params().choice("edge"), EDGE_MODES.len() - 1);
        assert_eq!(edge_mode(dialog.params(), "edge"), EdgeMode::Mirror);
    }

    #[test]
    fn a_parameter_of_the_wrong_kind_is_refused() {
        let spec = filter_by_id("blur.gaussian").unwrap();
        let mut dialog = FilterDialog::new(spec, placeholder_buffer(16, 16));
        let before = dialog.params().clone();
        assert!(!dialog.set_param("radius", ParamValue::Bool(true)));
        assert!(!dialog.set_param("no-such-key", ParamValue::Float(1.0)));
        assert_eq!(dialog.params(), &before);
    }

    #[test]
    fn reset_restores_every_default() {
        let spec = filter_by_id("sharpen.unsharp").unwrap();
        let mut dialog = FilterDialog::new(spec, placeholder_buffer(16, 16));
        let defaults = dialog.params().clone();
        dialog.set_param("amount", ParamValue::Float(4.0));
        dialog.set_param("radius", ParamValue::Float(9.0));
        assert_ne!(dialog.params(), &defaults);
        dialog.reset();
        assert_eq!(dialog.params(), &defaults);
    }

    #[test]
    fn a_parameter_change_actually_changes_the_preview() {
        let spec = filter_by_id("blur.gaussian").unwrap();
        let mut dialog = FilterDialog::new(spec, placeholder_buffer(48, 48));
        let sharp = dialog.preview_buffer().to_rgba8();
        dialog.set_param("radius", ParamValue::Float(12.0));
        let blurred = dialog.preview_buffer().to_rgba8();
        assert_ne!(sharp, blurred, "the radius did not reach the filter");
    }

    #[test]
    fn the_preview_toggle_stops_the_work() {
        let spec = filter_by_id("blur.gaussian").unwrap();
        let mut dialog = FilterDialog::new(spec, placeholder_buffer(16, 16));
        assert!(dialog.preview_enabled());
        dialog.set_preview_enabled(false);
        assert!(!dialog.preview_enabled());
        assert!(dialog.texture.is_none());
        assert!(dialog.cached_for.is_none());
    }

    #[test]
    fn an_invocation_carries_the_filter_and_its_own_schema() {
        for filter in FILTERS {
            let dialog = FilterDialog::new(filter, placeholder_buffer(16, 16));
            let invocation = dialog.invocation();
            assert!(invocation.is_valid(), "{}", filter.id);
            assert_eq!(invocation.filter.id, filter.id);
            // And it can actually run.
            let out = invocation.run(&placeholder_buffer(16, 16));
            assert_eq!(out.dimensions(), (16, 16));
        }
    }

    #[test]
    fn confirm_produces_a_valid_invocation_and_cancel_produces_nothing() {
        for filter in FILTERS {
            let dialog = FilterDialog::new(filter, placeholder_buffer(16, 16));
            let action = dialog.confirm().unwrap_or_else(|| panic!("{}", filter.id));
            assert!(action.is_valid(), "{}", filter.id);
            assert_eq!(
                super::super::chrome::resolve(&dialog, DialogKeys::CANCEL),
                DialogOutcome::Cancelled
            );
        }
    }

    #[test]
    fn every_generated_dialog_draws_in_both_appearances() {
        for filter in FILTERS {
            frame_both_themes(|ctx| {
                let mut dialog = FilterDialog::new(filter, placeholder_buffer(32, 32));
                assert!(dialog.show(ctx, None).is_open(), "{}", filter.id);
                dialog.set_preview_enabled(false);
                assert!(dialog.show(ctx, None).is_open(), "{}", filter.id);
            });
        }
    }

    /// A filter that exists only here, carrying one parameter of every kind.
    ///
    /// No shipping filter takes a colour yet, so without this the generated
    /// form's `Color` arm is never drawn and the "adding a `FilterSpec` is the
    /// entire cost of a new filter's UI" claim is untested for the one kind
    /// that used to be a dead control. It is a `FilterSpec` like any other —
    /// which is the point: nothing about it is special-cased.
    const EVERY_KIND: &[OptionSpec] = &[
        float("amount", "Amount", 0.0, 1.0, 0.5),
        int("count", "Count", 1, 8, 2),
        flag("invert", "Invert", false),
        choice("edge", "Edges", EDGE_MODES, 0),
        OptionSpec {
            key: "tint",
            label: "Tint",
            kind: OptionKind::Color {
                default: [0.0, 0.0, 0.0, 1.0],
            },
        },
    ];

    static COLOR_FILTER: FilterSpec = FilterSpec {
        id: "test.every_kind",
        name: "Every Kind",
        group: FilterGroup::Other,
        summary: "A test filter carrying one parameter of every kind.",
        params: EVERY_KIND,
        apply: |src, _| src.clone(),
    };

    #[test]
    fn the_generated_form_covers_every_option_kind() {
        let params = FilterParams::defaults(EVERY_KIND);
        let kinds: Vec<&str> = EVERY_KIND
            .iter()
            .map(|spec| match spec.kind {
                OptionKind::Float { .. } => "float",
                OptionKind::Int { .. } => "int",
                OptionKind::Bool { .. } => "bool",
                OptionKind::Choice { .. } => "choice",
                OptionKind::Color { .. } => "color",
            })
            .collect();
        assert_eq!(kinds, ["float", "int", "bool", "choice", "color"]);
        for spec in EVERY_KIND {
            assert!(params.get(spec.key).is_some(), "{}", spec.key);
        }
    }

    #[test]
    fn clicking_a_colour_parameters_swatch_sets_that_parameter() {
        // The defect this pins: the generated form's Color arm drew a swatch
        // and dropped its Response, so a filter with a colour parameter got a
        // dialog in which that parameter could never leave its schema default.
        let h = Harness::new();
        let mut dialog = FilterDialog::new(&COLOR_FILTER, placeholder_buffer(16, 16));
        assert_eq!(
            dialog.params().get("tint"),
            Some(ParamValue::Color([0.0, 0.0, 0.0, 1.0]))
        );

        h.click_widget(ids::filter_param_color("tint"), |ctx| {
            dialog.show(ctx, None);
        });
        assert_eq!(
            dialog.color_edit().target(),
            Some("tint"),
            "the swatch opened nothing"
        );

        let chosen = crate::dialogs::ColorValue::new([0.9, 0.1, 0.4, 1.0]);
        dialog
            .color_edit_mut()
            .picker_mut()
            .expect("the picker is up")
            .set_color(chosen);
        h.frame(Harness::key_events(egui::Key::Enter), |ctx| {
            assert!(dialog.show(ctx, None).is_open());
        });

        match dialog.params().get("tint") {
            Some(ParamValue::Color(rgba)) => {
                assert_eq!(
                    crate::dialogs::ColorValue::new(rgba).to_bytes(),
                    chosen.to_bytes()
                );
            }
            other => panic!("expected a colour, got {other:?}"),
        }
        // And it reaches the invocation the dialog commits.
        match dialog.confirm() {
            Some(DialogAction::RunFilter(invocation)) => {
                assert_eq!(invocation.params.get("tint"), dialog.params().get("tint"));
            }
            other => panic!("expected an invocation, got {other:?}"),
        }
    }

    #[test]
    fn a_filter_without_a_colour_parameter_draws_no_swatch() {
        let h = Harness::new();
        let spec = filter_by_id("blur.gaussian").unwrap();
        let mut dialog = FilterDialog::new(spec, placeholder_buffer(16, 16));
        h.frame(Vec::new(), |ctx| {
            dialog.show(ctx, None);
        });
        assert!(!h.was_drawn(ids::filter_param_color("tint")));
    }

    #[test]
    fn the_colour_filter_dialog_draws_in_both_appearances() {
        frame_both_themes(|ctx| {
            let mut dialog = FilterDialog::new(&COLOR_FILTER, placeholder_buffer(16, 16));
            assert!(dialog.show(ctx, None).is_open());
        });
    }
}
