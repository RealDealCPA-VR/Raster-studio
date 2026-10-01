//! The Channels and Paths panels.
//!
//! Both are *inventories*: a list of named things belonging to the document,
//! each with a visibility toggle and a selection. Neither owns pixel data, so
//! both are small — and both are honest about what this build can do.
//!
//! # Channels
//!
//! The composite plus one row per component of the document's colour mode,
//! derived from [`color::ColorSpace`] rather than hard-coded, plus one row per
//! layer mask in the document (Photoshop's alpha channels).
//!
//! Toggling a component's visibility is a *view* setting, not a document edit:
//! it emits [`crate::Intent::SetChannelVisible`] rather than a
//! [`editor_core::Command`], and the application applies it to the composite on
//! its way to the screen — in this workspace, `app_shell::presenter::
//! ChannelMask`, which the canvas texture is uploaded through. So hiding the
//! red channel changes pixels and changes no file. A **mask** row is the
//! exception and says so below: a mask's visibility is the mask's own
//! `enabled` flag, which is document state, so that row emits a command.
//!
//! What this build still does not have is per-channel *editing* of a colour
//! component — painting into the red channel alone. A component row's
//! selection is therefore an isolation target, not a paint target;
//! `docs/parity-matrix.md` carries that gap.
//!
//! W10-B: the *alpha* rows are editable. Selecting a **mask** row makes its
//! layer active, aims painting at the mask and shows the mask alone in
//! grayscale ([`crate::MaskViewMode::Grayscale`]); selecting a colour row
//! again puts the composite back. A **saved selection** row's eye
//! ([`alpha_eye_id`]) opens the selection as a channel the same way — the
//! application builds a hidden scratch layer whose mask is the saved
//! coverage (`MenuAction::EditAlphaChannel`) — and a second click stores the
//! painted coverage back into the saved selection
//! (`MenuAction::CloseAlphaChannel`).
//!
//! # Paths
//!
//! One row per shape layer, since a shape layer is where this build keeps a
//! path. There is no free-standing path store yet, so a path cannot exist
//! without a layer — and the panel says so when there are none rather than
//! showing an empty box.

use color::ColorSpace;
use editor_core::Document;
use layer_model::{LayerId, LayerKind, MaskId};

use crate::shortcut::Shortcut;

/// One row of the Channels panel.
#[derive(Clone, PartialEq, Debug)]
pub struct ChannelRow {
    pub name: String,
    pub kind: ChannelKind,
    pub visible: bool,
    /// The chord that isolates this channel, e.g. `Ctrl+3` for red.
    pub shortcut_digit: Option<u8>,
}

impl ChannelRow {
    /// The chord hint the panel prints beside the row, or `None` for a row
    /// with no chord.
    ///
    /// Derived from the same [`Shortcut`] value the key handler matches, so a
    /// hint painted here is a promise [`ChannelsState::kind_for_digit`] keeps —
    /// see `every_channel_shortcut_the_panel_prints_selects_that_channel`.
    pub fn shortcut(&self) -> Option<Shortcut> {
        let digit = self.shortcut_digit?;
        Some(Shortcut::ctrl(char::from_digit(u32::from(digit), 10)?))
    }

    /// The chord hint as the panel writes it.
    pub fn shortcut_label(&self) -> Option<String> {
        self.shortcut().map(|s| s.to_string())
    }
}

/// What a channel row stands for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChannelKind {
    /// All components at once.
    Composite,
    /// One colour component, by index into the mode's components.
    Component(usize),
    /// A layer mask, shown as an alpha channel.
    Mask { layer: LayerId, mask: MaskId },
    /// W18-C: a set of colour components chosen together (Shift-click adds a
    /// component to the selection, Photopea's gesture) or one component of a
    /// CMYK / Lab document. `mask` has bit `i` set for component `i` of
    /// `model`'s list ([`model_component_names`]).
    ///
    /// A single RGB component stays [`ChannelKind::Component`], so the chord
    /// hints, the Load Channel route and every existing caller keep the kind
    /// they always had; this variant is what the row selection becomes when
    /// it names anything else.
    Components { mask: u8, model: ChannelModel },
}

/// W18-C: the colour model a document's component rows describe, read from
/// `DocumentMeta::color_mode` (the tiles are RGBA in every mode; the mode
/// decides which components the panel lists and edits).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum ChannelModel {
    /// Red, green and blue (every mode that is not CMYK or Lab).
    Rgb,
    /// Cyan, magenta, yellow and black.
    Cmyk,
    /// Lightness, a and b.
    Lab,
}

impl ChannelModel {
    /// The model of a document, from its colour mode.
    pub fn of(doc: &Document) -> Self {
        match doc.meta.color_mode {
            editor_core::color_mode::mode::CMYK => ChannelModel::Cmyk,
            editor_core::color_mode::mode::LAB => ChannelModel::Lab,
            _ => ChannelModel::Rgb,
        }
    }

    /// How many colour components the model has.
    pub fn component_count(self) -> usize {
        match self {
            ChannelModel::Cmyk => 4,
            ChannelModel::Rgb | ChannelModel::Lab => 3,
        }
    }

    /// The mask naming every component: selecting all of them is the
    /// composite.
    pub fn all_mask(self) -> u8 {
        (1u8 << self.component_count()) - 1
    }
}

/// W18-C: the component names of a model, through the catalogue. The RGB
/// list is [`component_names`]'s (the colour-space-derived names).
pub fn model_component_names(model: ChannelModel, mode: &ColorSpace) -> Vec<String> {
    use crate::strings::tr;
    let keys: &[&str] = match model {
        ChannelModel::Rgb => {
            return component_names(mode)
                .iter()
                .map(|n| (*n).to_string())
                .collect()
        }
        ChannelModel::Cmyk => &[
            "ui.w18.channels.cyan",
            "ui.w18.channels.magenta",
            "ui.w18.channels.yellow",
            "ui.w18.channels.black",
        ],
        ChannelModel::Lab => &[
            "ui.w18.channels.lightness",
            "ui.w18.channels.a",
            "ui.w18.channels.b",
        ],
    };
    keys.iter().map(|k| tr(k).to_string()).collect()
}

/// Channel visibility, which is a view setting rather than document state.
#[derive(Clone, PartialEq, Debug)]
pub struct ChannelsState {
    /// One flag per colour component; `true` means the component contributes.
    components: [bool; 4],
    /// The row the user has selected for editing.
    pub selected: ChannelKind,
    /// W13X-4: the open New Spot Channel dialog, if any.
    pub spot_dialog: Option<SpotChannelDialog>,
}

impl Default for ChannelsState {
    fn default() -> Self {
        Self {
            components: [true; 4],
            selected: ChannelKind::Composite,
            spot_dialog: None,
        }
    }
}

impl ChannelsState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn component_visible(&self, index: usize) -> bool {
        self.components.get(index).copied().unwrap_or(true)
    }

    pub fn set_component_visible(&mut self, index: usize, visible: bool) {
        if let Some(slot) = self.components.get_mut(index) {
            *slot = visible;
        }
    }

    /// `true` when every component contributes, which is what the composite
    /// row's own toggle shows.
    pub fn composite_visible(&self, mode: &ColorSpace) -> bool {
        (0..component_names(mode).len()).all(|i| self.component_visible(i))
    }

    /// Show or hide every component at once.
    pub fn set_composite_visible(&mut self, mode: &ColorSpace, visible: bool) {
        for i in 0..component_names(mode).len() {
            self.set_component_visible(i, visible);
        }
    }

    /// The rows to draw, composite first.
    pub fn rows(&self, doc: &Document) -> Vec<ChannelRow> {
        let mode = &doc.meta.color_space;
        // W18-C: a CMYK or Lab document lists its own components.
        let model = ChannelModel::of(doc);
        let names = model_component_names(model, mode);
        let composite = match model {
            ChannelModel::Rgb => composite_name(mode),
            ChannelModel::Cmyk => crate::strings::tr("ui.w18.channels.cmyk"),
            ChannelModel::Lab => crate::strings::tr("ui.w18.channels.lab"),
        };
        // The canvas can hide RGB components only, so a CMYK / Lab row is
        // always shown (and the panel draws no eye on it).
        let previewable = model == ChannelModel::Rgb;
        let mut rows = vec![ChannelRow {
            name: composite.to_string(),
            kind: ChannelKind::Composite,
            visible: !previewable || self.composite_visible(mode),
            shortcut_digit: Some(2),
        }];
        for (i, name) in names.into_iter().enumerate() {
            rows.push(ChannelRow {
                name,
                kind: component_kind(model, i),
                visible: !previewable || self.component_visible(i),
                // Ctrl+3 is the first component, matching every other editor.
                shortcut_digit: u8::try_from(i + 3).ok().filter(|d| *d <= 9),
            });
        }
        for id in doc.layers.iter_depth_first() {
            let Some(layer) = doc.layers.get(id) else {
                continue;
            };
            let Some(mask) = layer.mask.as_ref() else {
                continue;
            };
            rows.push(ChannelRow {
                name: format!("{} Mask", layer.name),
                kind: ChannelKind::Mask {
                    layer: id,
                    mask: mask.id,
                },
                visible: mask.enabled,
                shortcut_digit: None,
            });
        }
        rows
    }

    /// The channel `Ctrl+<digit>` names, or `None` when no row wears that
    /// digit in this document.
    ///
    /// Answered from [`ChannelsState::rows`] rather than from arithmetic, so
    /// the chord and the hint painted beside it cannot drift apart: a
    /// grayscale document that grows a fourth component gets `Ctrl+6` in both
    /// places or in neither.
    pub fn kind_for_digit(&self, doc: &Document, digit: u8) -> Option<ChannelKind> {
        self.rows(doc)
            .into_iter()
            .find(|r| r.shortcut_digit == Some(digit))
            .map(|r| r.kind)
    }

    /// Isolate one channel: make it the selection, and show only it.
    ///
    /// Selecting the composite shows every component again, which is what
    /// `Ctrl+2` does in every editor that has this chord.
    pub fn isolate(&mut self, mode: &ColorSpace, kind: ChannelKind) {
        self.selected = kind;
        match kind {
            ChannelKind::Composite => self.set_composite_visible(mode, true),
            ChannelKind::Component(index) => {
                for i in 0..component_names(mode).len() {
                    self.set_component_visible(i, i == index);
                }
            }
            // W18-C: several RGB components show together; a CMYK / Lab
            // component cannot be previewed alone (the canvas upload masks
            // RGB components only), so the composite stays on screen.
            ChannelKind::Components {
                mask,
                model: ChannelModel::Rgb,
            } => {
                for i in 0..component_names(mode).len() {
                    self.set_component_visible(i, mask & (1 << i) != 0);
                }
            }
            ChannelKind::Components { .. } => self.set_composite_visible(mode, true),
            // A mask channel's visibility is the mask's own `enabled` flag,
            // which is document state; isolating one is a selection only.
            ChannelKind::Mask { .. } => {}
        }
    }
}

// ---------------------------------------------------------------------------
// W18-C: per-channel editing: the selected colour components
// ---------------------------------------------------------------------------

/// W18-C: the row kind of component `index` of `model`: an RGB component is
/// [`ChannelKind::Component`], a CMYK / Lab one a one-bit
/// [`ChannelKind::Components`].
pub fn component_kind(model: ChannelModel, index: usize) -> ChannelKind {
    match model {
        ChannelModel::Rgb => ChannelKind::Component(index),
        ChannelModel::Cmyk | ChannelModel::Lab => ChannelKind::Components {
            mask: 1 << index,
            model,
        },
    }
}

impl ChannelsState {
    /// W18-C: the colour components pixel edits write, as a bit mask over
    /// `model`'s components, or `None` when every component is written (the
    /// composite, a mask row, or a selection made for another model: a tab
    /// switch from a CMYK document to an RGB one, say).
    pub fn edit_mask(&self, model: ChannelModel) -> Option<u8> {
        let mask = match self.selected {
            ChannelKind::Component(i) if model == ChannelModel::Rgb && i < 3 => 1u8 << i,
            ChannelKind::Components { mask, model: m } if m == model => mask & model.all_mask(),
            _ => return None,
        };
        (mask != 0 && mask != model.all_mask()).then_some(mask)
    }

    /// W18-C: whether the row of kind `row` is part of the selection: a
    /// component row is when its component is in the selected set.
    pub fn row_selected(&self, row: ChannelKind) -> bool {
        let bits = |kind: ChannelKind| match kind {
            ChannelKind::Component(i) if i < 8 => Some((ChannelModel::Rgb, 1u8 << i)),
            ChannelKind::Components { mask, model } => Some((model, mask)),
            _ => None,
        };
        match (bits(self.selected), bits(row)) {
            (Some((sm, sel)), Some((rm, r))) => sm == rm && r != 0 && sel & r == r,
            _ => self.selected == row,
        }
    }

    /// W18-C: a click on the row of kind `row`, Photopea's gesture. A plain
    /// click selects that channel alone (and shows it alone); a Shift-click
    /// on a colour component adds it to the selected components (or takes
    /// it out again, never leaving none). Selecting every component is the
    /// composite, and the composite row restores all. Answers the new
    /// selection.
    pub fn click(&mut self, doc: &Document, row: ChannelKind, shift: bool) -> ChannelKind {
        let model = ChannelModel::of(doc);
        let mode = doc.meta.color_space.clone();
        let row_bits = match row {
            ChannelKind::Component(i) if model == ChannelModel::Rgb && i < 3 => Some(1u8 << i),
            ChannelKind::Components { mask, model: m } if m == model => Some(mask),
            _ => None,
        };
        let Some(row_bits) = row_bits else {
            // The composite or a mask row: selected as it is.
            self.isolate(&mode, row);
            return self.selected;
        };
        let current = match self.selected {
            ChannelKind::Composite => Some(model.all_mask()),
            _ => self.edit_mask(model),
        };
        let bits = match (shift, current) {
            (true, Some(cur)) if cur ^ row_bits != 0 => cur ^ row_bits,
            (true, Some(cur)) => cur,
            _ => row_bits,
        };
        let kind = if bits == model.all_mask() {
            ChannelKind::Composite
        } else if model == ChannelModel::Rgb && bits.count_ones() == 1 {
            ChannelKind::Component(bits.trailing_zeros() as usize)
        } else {
            ChannelKind::Components { mask: bits, model }
        };
        self.isolate(&mode, kind);
        self.selected
    }
}

// ---------------------------------------------------------------------------
// W13X-4: the Channels panel menu and New Spot Channel
// ---------------------------------------------------------------------------

/// Stable ids for the Channels panel menu's rows, the spot rows and the New
/// Spot Channel dialog, for a headless test.
pub mod spot_ids {
    /// A row of the Channels panel menu (`"new-spot"`, `"merge"`).
    pub fn menu_row(name: &'static str) -> egui::Id {
        egui::Id::new(("raster-channels-menu", name))
    }

    /// The `index`th spot channel's ink swatch.
    pub fn spot_swatch(index: usize) -> egui::Id {
        egui::Id::new(("raster-channels-spot-swatch", index))
    }

    /// The dialog's ink swatch.
    pub fn dialog_swatch() -> egui::Id {
        egui::Id::new("raster-spot-channel-ink")
    }

    /// Holds the keyboard while the dialog is up and none of its fields has
    /// it, so a chord cannot act on the document behind it.
    pub fn keyboard_sink() -> egui::Id {
        egui::Id::new("raster-spot-channel-keyboard")
    }
}

pub use crate::dialogs::spot_channel::{SpotChannelDialog, SpotChannelSpec, DEFAULT_SPOT_INK};

/// W13X-4: the Channels panel menu, driven through the real workspace.
#[cfg(test)]
#[path = "w13x4_channels_tests.rs"]
mod w13x4_channels_tests;

/// W18-C: per-channel selection, driven through the real workspace.
#[cfg(test)]
#[path = "w18c_channels_tests.rs"]
mod w18c_channels_tests;

/// W10-B: the id of the `index`th saved-selection (alpha) row's eye in the
/// Channels panel.
pub fn alpha_eye_id(index: usize) -> egui::Id {
    egui::Id::new(("channels-alpha-eye", index))
}

/// W18-I: the eye of the `index`th spot channel row: it shows or hides that
/// channel's ink (`editor_core::spot::set_spot_visible`, one undo step).
pub fn spot_eye_id(index: usize) -> egui::Id {
    egui::Id::new(("channels-spot-eye", index))
}

// ---------------------------------------------------------------------------
// W18-I: renaming an alpha channel in place
// ---------------------------------------------------------------------------

/// W18-I: the name of the `index`th saved-selection (alpha) row; a
/// double-click on it opens [`alpha_rename_id`] in its place.
pub fn alpha_name_id(index: usize) -> egui::Id {
    egui::Id::new(("channels-alpha-name", index))
}

/// W18-I: the field an alpha row's name becomes while it is renamed.
pub fn alpha_rename_id(index: usize) -> egui::Id {
    egui::Id::new(("channels-alpha-rename", index))
}

fn alpha_renaming_key() -> egui::Id {
    egui::Id::new("channels-alpha-renaming")
}

fn alpha_pending_click_key() -> egui::Id {
    egui::Id::new("channels-alpha-pending-click")
}

/// W18-I: Photopea's "double-click the name of an independent channel to
/// rename it": saved selection `index` renamed to `name` (trimmed), its
/// coverage kept, as one undo step. `None` for an empty name, the name it
/// already has, or a slot that is gone.
pub fn rename_alpha(doc: &Document, index: usize, name: &str) -> Option<editor_core::Command> {
    let name = name.trim();
    let (old, selection) = doc.saved_selections.get(index)?;
    if name.is_empty() || old == name {
        return None;
    }
    Some(editor_core::Command::SetSavedSelection {
        index,
        name: name.to_string(),
        selection: selection.clone(),
    })
}

/// W18-I: the name cell of alpha row `index`: its label, or, while the row
/// is being renamed, the field. Answers the cell's rect (for
/// [`alpha_name_click`]) and, on the frame the field commits, the rename.
pub(crate) fn alpha_name_ui(
    ui: &mut egui::Ui,
    doc: &Document,
    index: usize,
    name: &str,
) -> (egui::Rect, Option<editor_core::Command>) {
    use crate::panels::layer_comps::{inline_rename_field, InlineRename};
    let renaming: Option<usize> = ui.data(|d| d.get_temp(alpha_renaming_key()));
    if renaming != Some(index) {
        let label = ui.label(crate::view::body(ui, name.to_string()));
        return (label.rect, None);
    }
    match inline_rename_field(ui, alpha_rename_id(index), name) {
        InlineRename::Editing => (egui::Rect::NOTHING, None),
        outcome => {
            ui.data_mut(|d| d.remove::<usize>(alpha_renaming_key()));
            let command = match outcome {
                InlineRename::Commit(text) => rename_alpha(doc, index, &text),
                _ => None,
            };
            (egui::Rect::NOTHING, command)
        }
    }
}

/// W18-I: the name cell's clicks, registered after the row's own click
/// region so the name answers first. A double-click opens the rename field.
/// A single click is the row's click (Load Selection), handed back as
/// `Some(direct)` once a double-click can no longer follow it, so the
/// dialog it opens does not swallow the second press of a double-click.
pub(crate) fn alpha_name_click(ui: &egui::Ui, index: usize, rect: egui::Rect) -> Option<bool> {
    let now = ui.input(|i| i.time);
    let delay = ui.ctx().options(|o| o.input_options.max_double_click_delay);
    let pending: Option<(usize, f64, bool)> = ui.data(|d| d.get_temp(alpha_pending_click_key()));
    if rect.is_positive() {
        let response = ui.interact(rect, alpha_name_id(index), egui::Sense::click());
        // egui reports a double-click for any second click in its window:
        // both clicks must have landed on this name.
        let first_here = matches!(pending, Some((i, _, _)) if i == index);
        if response.double_clicked() && first_here {
            ui.data_mut(|d| {
                d.remove::<(usize, f64, bool)>(alpha_pending_click_key());
                d.insert_temp(alpha_renaming_key(), index);
            });
            return None;
        }
        if response.clicked() {
            let direct = ui.input(|i| i.modifiers.command);
            if direct {
                // Ctrl+click loads at once: no rename follows a Ctrl+click.
                return Some(true);
            }
            ui.data_mut(|d| d.insert_temp(alpha_pending_click_key(), (index, now, false)));
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs_f64(delay));
            return None;
        }
    }
    match pending {
        Some((i, at, direct)) if i == index => {
            if now - at > delay {
                ui.data_mut(|d| d.remove::<(usize, f64, bool)>(alpha_pending_click_key()));
                Some(direct)
            } else {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_secs_f64(delay));
                None
            }
        }
        _ => None,
    }
}

/// The component names of a colour mode.
///
/// Derived from the mode rather than assumed to be RGB, so a grayscale document
/// does not show three identical rows.
pub fn component_names(mode: &ColorSpace) -> &'static [&'static str] {
    match mode {
        // Every space this build supports has RGB primaries; an ICC profile
        // could have any number of channels, and since nothing can transform
        // one (`ColorSpace::is_transform_supported`) the panel names the
        // components generically rather than claiming they are red and green.
        ColorSpace::Srgb | ColorSpace::LinearSrgb | ColorSpace::DisplayP3 => {
            &["Red", "Green", "Blue"]
        }
        ColorSpace::IccProfile { .. } => &["Channel 1", "Channel 2", "Channel 3"],
    }
}

/// The composite row's name for a mode.
pub fn composite_name(mode: &ColorSpace) -> &'static str {
    match mode {
        ColorSpace::Srgb | ColorSpace::LinearSrgb => "RGB",
        ColorSpace::DisplayP3 => "Display P3",
        ColorSpace::IccProfile { .. } => "Composite",
    }
}

/// One row of the Paths panel.
#[derive(Clone, PartialEq, Debug)]
pub struct PathRow {
    pub layer: LayerId,
    pub name: String,
    /// `true` when the layer's path string is non-empty; an empty shape layer
    /// is drawn as a placeholder rather than as a working path.
    pub has_geometry: bool,
    pub visible: bool,
}

/// The Paths panel.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct PathsState {
    pub selected: Option<LayerId>,
    /// W4-I: Photoshop's Work Path — a temporary path that belongs to no
    /// layer, in document pixels. Fed by the pen's uncommitted path while
    /// the pen is authoring one ([`PathsState::follow_pen`]), or made by the
    /// footer's "work path from selection"; saved to a path layer by the
    /// footer's New.
    pub work_path: Option<vector::Path>,
    /// The Work Path row is the selected one (rather than a layer's path).
    pub work_selected: bool,
    /// The Work Path is the pen's live path (so it goes when the pen's
    /// session ends), not one the footer made.
    pub work_from_pen: bool,
}

impl PathsState {
    pub fn new() -> Self {
        Self::default()
    }

    /// One row per shape layer, in document order.
    pub fn rows(doc: &Document) -> Vec<PathRow> {
        doc.layers
            .iter_depth_first()
            .into_iter()
            .filter_map(|id| {
                let layer = doc.layers.get(id)?;
                let LayerKind::Shape(shape) = &layer.kind else {
                    return None;
                };
                Some(PathRow {
                    layer: id,
                    name: layer.name.clone(),
                    has_geometry: !shape.path_svg.trim().is_empty(),
                    visible: layer.visible,
                })
            })
            .collect()
    }

    /// The sentence shown when there is nothing to list.
    pub const fn empty_message() -> &'static str {
        "Draw with a shape or pen tool to create a path"
    }

    /// The selected path in document pixels: the Work Path when its row is
    /// selected, otherwise the selected shape layer's path mapped through
    /// that layer's transform. `None` when nothing is selected or the layer's
    /// path data does not parse.
    pub fn selected_path(&self, doc: &Document) -> Option<vector::Path> {
        if self.work_selected {
            return self.work_path.clone();
        }
        let id = self.selected?;
        let layer = doc.layers.get(id)?;
        let LayerKind::Shape(shape) = &layer.kind else {
            return None;
        };
        let path = vector::parse_svg(&shape.path_svg).ok()?;
        Some(path.transform(&crate::panels::paths::affine_of(layer.transform)))
    }

    /// W4-I: follow the pen's live session, as the shell publishes it
    /// ([`tools::Tool::live_geometry`]). While the pen is authoring a path
    /// the Work Path *is* that path, anchor for anchor. When the session
    /// ends the Work Path it fed goes with it: a committed pen path has its
    /// own row (the path layer the commit made), and a cancelled one is
    /// gone. A Work Path the footer made from a selection is not touched.
    pub fn follow_pen(&mut self, live: Option<&tools::SessionGeometry>) {
        if let Some(tools::SessionGeometry::Path {
            anchors, handles, ..
        }) = live
        {
            if !anchors.is_empty() {
                self.work_path = Some(pen_path(anchors, handles));
                self.work_from_pen = true;
                return;
            }
        }
        if self.work_from_pen {
            self.work_path = None;
            self.work_selected = false;
            self.work_from_pen = false;
        }
    }

    /// Drop a selection whose layer has left the document.
    pub fn prune(&mut self, doc: &Document) {
        if let Some(id) = self.selected {
            if !doc.layers.contains(id) {
                self.selected = None;
            }
        }
    }
}

/// The path a pen session's anchors describe: `handles[i]` are anchor `i`'s
/// absolute `[in, out]` control points, equal to the anchor when straight,
/// so a segment is a line when both of its inner handles sit on their
/// anchors and a cubic otherwise (the same rule as `tools::pen`).
fn pen_path(anchors: &[glam::Vec2], handles: &[[glam::Vec2; 2]]) -> vector::Path {
    let pt = |v: glam::Vec2| vector::Point::new(f64::from(v.x), f64::from(v.y));
    let mut path = vector::Path::new();
    let Some(first) = anchors.first() else {
        return path;
    };
    path.move_to(pt(*first));
    for i in 1..anchors.len() {
        let (a, b) = (anchors[i - 1], anchors[i]);
        let out = handles.get(i - 1).map_or(a, |h| h[1]);
        let into = handles.get(i).map_or(b, |h| h[0]);
        if out == a && into == b {
            path.line_to(pt(b));
        } else {
            path.curve_to(pt(out), pt(into), pt(b));
        }
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use layer_model::{Layer, LayerMask, ShapeLayer};

    fn document() -> Document {
        Document::new(64, 64, "Test")
    }

    #[test]
    fn the_channel_list_starts_with_the_composite_then_the_components() {
        let doc = document();
        let rows = ChannelsState::new().rows(&doc);
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[0].kind, ChannelKind::Composite);
        assert_eq!(rows[0].name, "RGB");
        let names: Vec<&str> = rows[1..].iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["Red", "Green", "Blue"]);
        assert_eq!(rows[1].kind, ChannelKind::Component(0));
    }

    #[test]
    fn component_shortcuts_start_at_three() {
        let doc = document();
        let rows = ChannelsState::new().rows(&doc);
        assert_eq!(rows[0].shortcut_digit, Some(2));
        assert_eq!(rows[1].shortcut_digit, Some(3));
        assert_eq!(rows[2].shortcut_digit, Some(4));
        assert_eq!(rows[3].shortcut_digit, Some(5));
    }

    #[test]
    fn every_digit_the_panel_prints_names_a_channel() {
        let doc = document();
        let state = ChannelsState::new();
        for row in state.rows(&doc) {
            let Some(digit) = row.shortcut_digit else {
                assert_eq!(row.shortcut_label(), None, "{} printed a chord", row.name);
                continue;
            };
            let label = row.shortcut_label().expect("a digit prints a chord");
            assert!(label.contains(&digit.to_string()), "{label}");
            assert_eq!(
                state.kind_for_digit(&doc, digit),
                Some(row.kind),
                "{label} does not select {}",
                row.name
            );
        }
        // A digit no row wears is not a chord this panel claims.
        assert_eq!(state.kind_for_digit(&doc, 9), None);
    }

    #[test]
    fn isolating_a_component_hides_the_others_and_the_composite_brings_them_back() {
        let doc = document();
        let mode = doc.meta.color_space.clone();
        let mut state = ChannelsState::new();
        state.isolate(&mode, ChannelKind::Component(1));
        assert_eq!(state.selected, ChannelKind::Component(1));
        assert!(!state.component_visible(0));
        assert!(state.component_visible(1));
        assert!(!state.component_visible(2));

        state.isolate(&mode, ChannelKind::Composite);
        assert_eq!(state.selected, ChannelKind::Composite);
        assert!(state.composite_visible(&mode));
    }

    #[test]
    fn hiding_one_component_clears_the_composite_toggle() {
        let doc = document();
        let mut state = ChannelsState::new();
        assert!(state.composite_visible(&doc.meta.color_space));
        state.set_component_visible(1, false);
        assert!(!state.composite_visible(&doc.meta.color_space));
        let rows = state.rows(&doc);
        assert!(!rows[0].visible);
        assert!(rows[1].visible);
        assert!(!rows[2].visible);
    }

    #[test]
    fn the_composite_toggle_moves_every_component() {
        let doc = document();
        let mut state = ChannelsState::new();
        state.set_composite_visible(&doc.meta.color_space, false);
        for i in 0..3 {
            assert!(!state.component_visible(i), "component {i}");
        }
        state.set_composite_visible(&doc.meta.color_space, true);
        assert!(state.composite_visible(&doc.meta.color_space));
    }

    #[test]
    fn a_component_index_past_the_end_is_ignored_rather_than_panicking() {
        let mut state = ChannelsState::new();
        state.set_component_visible(99, false);
        assert!(state.component_visible(99));
    }

    #[test]
    fn every_layer_mask_becomes_an_alpha_channel_row() {
        let mut doc = document();
        let a = doc.layers.insert_at(Layer::raster("Sky"), None, 0).unwrap();
        let _b = doc
            .layers
            .insert_at(Layer::raster("Ground"), None, 1)
            .unwrap();
        let mask = LayerMask::new(MaskId::new());
        let mask_id = mask.id;
        doc.layers.get_mut(a).unwrap().mask = Some(mask);

        let rows = ChannelsState::new().rows(&doc);
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[4].name, "Sky Mask");
        assert_eq!(
            rows[4].kind,
            ChannelKind::Mask {
                layer: a,
                mask: mask_id
            }
        );
        assert!(rows[4].visible);
        assert_eq!(rows[4].shortcut_digit, None);
    }

    #[test]
    fn a_disabled_mask_shows_as_a_hidden_channel() {
        let mut doc = document();
        let a = doc.layers.push_root(Layer::raster("Sky")).unwrap();
        let mut mask = LayerMask::new(MaskId::new());
        mask.enabled = false;
        doc.layers.get_mut(a).unwrap().mask = Some(mask);
        let rows = ChannelsState::new().rows(&doc);
        assert!(!rows.last().unwrap().visible);
    }

    #[test]
    fn the_paths_panel_lists_shape_layers_and_nothing_else() {
        let mut doc = document();
        doc.layers
            .insert_at(Layer::raster("Not a path"), None, 0)
            .unwrap();
        let star = doc
            .layers
            .insert_at(
                Layer::with_kind(
                    "Star",
                    LayerKind::Shape(ShapeLayer::from_svg("M0 0 L10 10 Z")),
                ),
                None,
                1,
            )
            .unwrap();
        let empty = doc
            .layers
            .insert_at(
                Layer::with_kind("Empty", LayerKind::Shape(ShapeLayer::default())),
                None,
                2,
            )
            .unwrap();

        let rows = PathsState::rows(&doc);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].layer, star);
        assert!(rows[0].has_geometry);
        assert_eq!(rows[1].layer, empty);
        assert!(!rows[1].has_geometry);
    }

    #[test]
    fn a_document_with_no_shapes_has_a_sentence_rather_than_an_empty_box() {
        let doc = document();
        assert!(PathsState::rows(&doc).is_empty());
        assert!(!PathsState::empty_message().is_empty());
    }

    #[test]
    fn a_path_selection_drops_a_layer_that_left_the_document() {
        let mut doc = document();
        let id = doc
            .layers
            .push_root(Layer::with_kind(
                "Star",
                LayerKind::Shape(ShapeLayer::default()),
            ))
            .unwrap();
        let mut state = PathsState::new();
        state.selected = Some(id);
        state.prune(&doc);
        assert_eq!(state.selected, Some(id));
        doc.layers.remove(id).unwrap();
        state.prune(&doc);
        assert_eq!(state.selected, None);
    }

    #[test]
    fn every_colour_mode_names_its_components() {
        for mode in [
            ColorSpace::Srgb,
            ColorSpace::LinearSrgb,
            ColorSpace::DisplayP3,
        ] {
            assert!(!component_names(&mode).is_empty(), "{mode:?}");
            assert!(!composite_name(&mode).is_empty(), "{mode:?}");
            assert!(component_names(&mode).iter().all(|n| !n.is_empty()));
        }
    }
}
